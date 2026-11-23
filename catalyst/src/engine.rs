//! The commands `add`, `del`, `upgrade` and `list`: one transaction each.
//!
//! 1. open the root: take the lock, finish a leftover journal (roll
//!    forward), delete leftover `.partial` and never-current version
//!    directories;
//! 2. resolve the new world against the indexes and what is installed;
//! 3. fetch and unpack the new packages in parallel threads into
//!    `Programs/<name>/<version>` (nothing live is touched), while a helper
//!    thread keeps flushing the file system so that the sync before the
//!    journal has little left to write;
//! 4. check that no two packages claim the same file;
//! 5. `syncfs`, write the journal, run the switch ([`crate::commit`]).

use std::collections::{BTreeMap, HashSet};
use std::io::BufReader;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use crate::adb::Dependency;
use crate::commit::{self, PkgState, SwitchOptions};
use crate::db::{self, Db, Journal, JournalFile, Op};
use crate::error::{Error, Result};
use crate::fetch::{self, Http};
use crate::progress::{human, Phase, Progress};
use crate::repo::{self, Repo};
use crate::solver::{self, Mode, Universe};
use crate::sys;
use crate::unpack::{self, IdMap};
use crate::verify::{self, TrustedKeys, VerifyOptions};

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub root: PathBuf,
    pub repositories: Vec<String>,
    /// Additional repositories appended to `ROOT/etc/apk/repositories` (`-X` / `--add-repository`).
    pub extra_repositories: Vec<String>,
    pub keys_dir: Option<PathBuf>,
    pub arch: Option<String>,
    pub initdb: bool,
    pub plain: bool,
    pub allow_untrusted: bool,
    pub no_scripts: bool,
    pub no_commit_hooks: bool,
    pub simulate: bool,
    /// For `add`: upgrade named packages even if already installed (`-u`).
    pub upgrade: bool,
    /// Worker threads for fetch/unpack (0: one per CPU, at most 16).
    pub jobs: usize,
    /// Accept an index older than the one used before (see
    /// [`repo::Freshness`]).
    pub allow_rollback: bool,
    /// Test hook (`CATALYST_CRASH_AT`): kill the process at a named point of the
    /// switch.
    pub crash_at: Option<String>,
}

impl Options {
    fn switch_opts(&self) -> SwitchOptions {
        SwitchOptions { no_scripts: self.no_scripts, crash_at: self.crash_at.clone(), verbose: false, unsynced: false }
    }
    fn fancy(&self) -> bool {
        !self.plain && sys::is_tty(1)
    }
}

fn abs_root(o: &Options, create_if_initdb: bool) -> Result<PathBuf> {
    let r = if o.root.is_absolute() { o.root.clone() } else { std::env::current_dir()?.join(&o.root) };
    if create_if_initdb && o.initdb {
        fs::create_dir_all(&r)?;
    }
    Ok(r)
}

pub fn arches(o: &Options, root: &Path) -> Vec<String> {
    match &o.arch {
        Some(a) => vec![a.clone()],
        None => repo::root_arches(root),
    }
}

/// Opens a root: lock, recovery of an interrupted switch, cleanup.
pub fn open(o: &Options) -> Result<Db> {
    let mut db = open_locked(o)?;
    settle(o, &mut db)?;
    Ok(db)
}

/// Takes the lock and reads the database; a leftover journal is left as it is.
fn open_locked(o: &Options) -> Result<Db> {
    let root = abs_root(o, true)?;
    Db::open(&root, o.initdb)
}

/// Finishes a leftover journal, cleans up after an interrupted run, and
/// rebuilds a missing or outdated index.
fn settle(o: &Options, db: &mut Db) -> Result<()> {
    let root = db.root.clone();
    // the system went down since adds that were not synced: what was not
    // written to disk is found and removed before anything else
    if let Some(mark) = crate::unsynced::stale(&root) {
        after_unsynced_stop(db, &mark)?;
    }
    let mut recovered = false;
    if let Some(j) = JournalFile::open(&root)? {
        println!("oct: finishing an interrupted switch ({} package changes)", j.plan.ops.len());
        // a switch planned with the index is finished with the index as it
        // was before the switch, when that is still intact: then it reads the
        // same records, all checked before the journal was written; otherwise
        // it reads every record
        let before = match (&j.plan.index, &j.plan.reads) {
            (Some(st), Some(_)) => crate::index::open_before(&root, &j.plan.ops, st),
            _ => None,
        };
        let needs = |n: &str| match (&before, &j.plan.reads) {
            (Some(_), Some(r)) => r.iter().any(|x| x == n),
            _ => true,
        };
        let broken: Vec<(String, String)> = db
            .broken
            .iter()
            .filter(|(n, _, _)| needs(n.as_str()))
            .map(|(n, v, why)| (n.clone(), format!("{n}-{v}: {why}")))
            .collect();
        if !broken.is_empty() {
            return Err(Error::Msg(format!("{}\n(finishing the interrupted switch needs it)", broken_records(&broken))));
        }
        if j.plan.unsynced {
            // finished as a synced switch: what the journal names is on disk first
            sys::sync_fs(&fs::File::open(&root)?)?;
        }
        let rep = commit::switch(&root, &db.installed, j, &o.switch_opts(), before.as_ref()).map_err(|e| {
            Error::Msg(format!(
                "finishing the interrupted switch failed: {e}\n  if a record can't be read, `oct fix` rebuilds it from the signed package and then finishes the switch"
            ))
        })?;
        println!(
            "oct: switch finished: {} files placed, {} already in place, {} removed",
            rep.placed, rep.unchanged, rep.removed
        );
        db.reload()?;
        recovered = true;
    }
    // what an interrupted rebuild of the index left behind, and a journal
    // written but never put in effect (a crash before the switch)
    crate::index::clean_leftovers(&root);
    let _ = fs::remove_file(Db::journal_pending_path(&root));
    // the clean-up walk over Programs is only needed after an interrupted or
    // failed run: one whose journal was just finished, or that left the
    // dirty mark (set before unpacking, removed when a change succeeded)
    let dirty = dirty_mark(&root);
    if recovered || dirty.exists() {
        gc(&root, db, &mut |_| {})?;
        let _ = fs::remove_file(&dirty);
    }
    // the index, rebuilt from the records when it is missing or out of date
    if db.index.is_none() && db.broken.is_empty() && !o.simulate {
        if let Ok(states) = crate::index::load_all(&db.installed) {
            match crate::index::rebuild(&root, &states, true) {
                Ok(()) => db.reload()?,
                Err(e) => eprintln!("oct: warning: the index could not be written: {e}"),
            }
        }
    }
    Ok(())
}

/// The package in a version directory, if the directory holds what its
/// record says: the record readable, every entry there, no regular file
/// shorter than recorded, no script empty. What a power cut takes from
/// files that were written and not synced is their content: they come back
/// short or empty.
fn whole_version(dir: &Path) -> Option<PkgState> {
    use std::os::unix::ffi::OsStrExt;
    let st = PkgState::load(dir, false).ok()?;
    if let Ok(rd) = fs::read_dir(dir.join(db::META_DIR).join("scripts")) {
        if !rd.flatten().all(|e| e.metadata().map_or(false, |m| m.len() > 0)) {
            return None;
        }
    }
    let whole = st.recs.iter().all(|r| {
        let Ok(m) = fs::symlink_metadata(dir.join(std::ffi::OsStr::from_bytes(&r.path))) else { return false };
        match &r.kind {
            db::RecKind::Dir => m.is_dir(),
            db::RecKind::File(crate::adb::FileKind::Regular) => m.is_file() && m.len() >= r.size,
            db::RecKind::File(crate::adb::FileKind::Symlink(_)) => m.file_type().is_symlink(),
            _ => true,
        }
    });
    whole.then_some(st)
}

/// The canonical places of every entry of the installed packages, those
/// named in `except` left out.
fn live_claims(root: &Path, db: &Db, except: &HashSet<String>) -> HashSet<Vec<u8>> {
    let mut rfs = crate::rootfs::RootFs::new(root);
    let mut set = HashSet::new();
    for (n, ip) in &db.installed {
        if except.contains(n) {
            continue;
        }
        for r in db::read_files(&ip.dir).unwrap_or_default() {
            if let Ok(Some(e)) = rfs.entry_if_dir_exists(&r.path) {
                set.insert(e.canon);
            }
        }
    }
    set
}

/// Removes from the live tree what version directory `vdir` put there,
/// going by the directory itself and not by its record (which may be what
/// is damaged):
/// - the regular files that are its own inodes (they were hard-linked);
/// - where no other package has an entry (`others`; nothing of this kind
///   when that is not known): its symlinks, its copies under `etc/` that
///   are untouched or empty, and its directories once they are empty.
fn unlink_live(root: &Path, vdir: &Path, others: Option<&HashSet<Vec<u8>>>) {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let mut rfs = crate::rootfs::RootFs::new(root);
    let mut dirs: Vec<Vec<u8>> = Vec::new();
    let mut todo: Vec<Vec<u8>> = vec![Vec::new()];
    while let Some(rel) = todo.pop() {
        let Ok(rd) = fs::read_dir(vdir.join(std::ffi::OsStr::from_bytes(&rel))) else { continue };
        for e in rd.flatten() {
            let name = e.file_name();
            if rel.is_empty() && name.as_bytes() == db::META_DIR.as_bytes() {
                continue;
            }
            let mut r = rel.clone();
            if !r.is_empty() {
                r.push(b'/');
            }
            r.extend_from_slice(name.as_bytes());
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                dirs.push(r.clone());
                todo.push(r);
                continue;
            }
            let Ok(Some(live)) = rfs.entry_if_dir_exists(&r) else { continue };
            let Ok(lm) = fs::symlink_metadata(&live.host) else { continue };
            let free = others.map_or(false, |o| !o.contains(&live.canon));
            let ours = if m.is_file() && lm.dev() == m.dev() && lm.ino() == m.ino() {
                true
            } else if !free {
                false
            } else if m.file_type().is_symlink() {
                lm.file_type().is_symlink() && fs::read_link(&live.host).ok() == fs::read_link(e.path()).ok()
            } else {
                m.is_file()
                    && lm.is_file()
                    && commit::is_etc(&r)
                    && (lm.len() == 0 || (lm.len() == m.len() && fs::read(&live.host).ok() == fs::read(e.path()).ok()))
            };
            if ours {
                let _ = fs::remove_file(&live.host);
            }
        }
    }
    let Some(others) = others else { return };
    dirs.sort_by(|a, b| b.len().cmp(&a.len()));
    for d in dirs {
        if let Ok(Some(live)) = rfs.entry_if_dir_exists(&d) {
            let real = fs::symlink_metadata(&live.host).map_or(false, |m| m.is_dir());
            if real && !others.contains(&live.canon) {
                let _ = fs::remove_dir(&live.host);
            }
        }
    }
}

/// Whether the lookup index has every entry of package `st`.
fn index_knows(ix: &crate::index::Index, st: &PkgState) -> bool {
    let name = st.meta.name();
    st.recs.iter().all(|r| {
        let who = if r.is_dir() { ix.dir_claimants(&r.path) } else { ix.file_claimants(&r.path) };
        who.map_or(false, |v| v.iter().any(|n| n == name))
    })
}

/// What follows a stop of the system since adds that were not synced
/// ([`crate::unsynced`]): `mark` is of another boot. A clean shutdown wrote
/// everything out and this finds nothing to do. After a power cut:
/// - a package installed since the mark (not in its list) that is not
///   whole is removed: its record can't be read, or files of it are
///   missing or short. Nothing else can have been lost: an unsynced switch
///   makes only new names;
/// - the journal of an unsynced add is finished only when everything it
///   installs is whole (and nothing had to be removed); otherwise it is
///   deleted, and what it had unpacked and not yet made current goes too,
///   with whatever of it was placed;
/// - the lookup index is rebuilt unless it has the recent packages' entries.
///
/// A removed package that the world names is installed again by the next
/// change; a whole one that the world lost is, like any package nothing
/// asks for, removed by it.
fn after_unsynced_stop(db: &mut Db, mark: &crate::unsynced::Mark) -> Result<()> {
    let root = db.root.clone();
    let recent = |n: &str, v: &str| !mark.had.contains(&(n.to_string(), v.to_string()));
    let mut drops: Vec<(String, String)> =
        db.broken.iter().filter(|(n, v, _)| recent(n, v)).map(|(n, v, _)| (n.clone(), v.clone())).collect();
    let mut whole: Vec<PkgState> = Vec::new();
    for (n, ip) in &db.installed {
        let v = ip.meta.info.version.as_str();
        if recent(n, v) {
            match whole_version(&ip.dir) {
                Some(st) => whole.push(st),
                None => drops.push((n.clone(), v.to_string())),
            }
        }
    }
    let jp = Db::journal_path(&root);
    let journal = match fs::read(&jp) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let keep = drops.is_empty()
        && journal.as_ref().map_or(false, |b| {
            std::str::from_utf8(b).ok().and_then(|t| Journal::parse(t).ok()).map_or(false, |j| {
                !j.unsynced
                    || j.ops.iter().all(|op| {
                        op.new_version().map_or(true, |v| whole_version(&Db::pkg_dir(&root, op.name(), v)).is_some())
                    })
            })
        });
    let cut = journal.is_some() && !keep;
    if cut {
        fs::remove_file(&jp)?;
    }
    if !drops.is_empty() || cut {
        let names: HashSet<String> = drops.iter().map(|(n, _)| n.clone()).collect();
        // what the other packages have in the live tree; not known when
        // one of them has a record that can't be read
        let unknown = db.broken.iter().any(|(n, _, _)| !names.contains(n));
        let others = (!unknown).then(|| live_claims(&root, db, &names));
        for (n, v) in &drops {
            let pdir = db::checked_pkg_parent(&root, n)?;
            let vdir = pdir.join(v);
            if fs::symlink_metadata(&vdir).map_or(false, |m| m.is_dir()) {
                unlink_live(&root, &vdir, others.as_ref());
                fs::remove_dir_all(&vdir)?;
            }
            let _ = fs::remove_file(pdir.join(db::CURRENT));
            let _ = fs::remove_dir(&pdir);
        }
        db.reload()?;
        gc(&root, db, &mut |vdir| unlink_live(&root, vdir, others.as_ref()))?;
        crate::index::invalidate(&root);
        db.reload()?;
        if drops.is_empty() {
            println!("oct: an install was cut short when the system went down; what it had not finished was removed");
        } else {
            let list: Vec<String> = drops.iter().map(|(n, v)| format!("{n}-{v}")).collect();
            println!(
                "oct: the system went down before {} was written to disk completely; removed.\n     \
                 The next change installs again what the world names; `oct add` does it now.",
                list.join(", ")
            );
        }
    } else if keep {
        // finished below, from the records: the index may lack what earlier
        // unsynced adds put in it
        crate::index::invalidate(&root);
    } else if !db.index.as_ref().map_or(true, |ix| whole.iter().all(|st| index_knows(ix, st))) {
        crate::index::invalidate(&root);
        db.reload()?;
    }
    crate::unsynced::remove(&root);
    Ok(())
}

/// The mark a change leaves while it unpacks and switches.
fn dirty_mark(root: &Path) -> PathBuf {
    Db::state(root).join("dirty")
}

/// The error for records that can't be read: nothing is changed until
/// `oct fix` has rebuilt them from the signed packages (N-1).
fn broken_records(list: &[(String, String)]) -> Error {
    let mut names: Vec<&str> = list.iter().map(|(n, _)| n.as_str()).collect();
    names.sort();
    names.dedup();
    Error::Msg(format!(
        "the record of {} can't be read, so catalyst changes nothing on this system until it is repaired:\n  {}\n\
         run `oct fix {}` to rebuild it from the signed package",
        names.join(", "),
        list.iter().map(|(_, m)| m.as_str()).collect::<Vec<_>>().join("\n  "),
        names.join(" ")
    ))
}

/// Opens a root read-only without taking the lock or modifying anything.
pub fn open_readonly(o: &Options) -> Result<Db> {
    let root = abs_root(o, false)?;
    Db::open_readonly(&root, o.initdb)
}

/// Deletes `.partial` directories and version directories that are not
/// current (left by an interrupted run before its journal was written, or
/// after it was deleted), and interrupted downloads in the cache. Entries
/// of `Programs` that are not real directories, or whose `Current` is not
/// valid, are left alone (and reported by [`Db::reload`]). `cut` is called
/// with every version directory about to be deleted.
fn gc(root: &Path, db: &Db, cut: &mut dyn FnMut(&Path)) -> Result<()> {
    let programs = Db::programs(root);
    for e in fs::read_dir(&programs)? {
        let e = e?;
        if !e.file_type()?.is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        let current = match db.installed.get(&name) {
            Some(i) => Some(i.meta.info.version.clone()),
            // a Current link that is there but was not accepted: keep all
            None if fs::symlink_metadata(e.path().join(db::CURRENT)).is_ok() => continue,
            None => None,
        };
        for v in fs::read_dir(e.path())? {
            let v = v?;
            let vn = v.file_name().to_string_lossy().into_owned();
            if vn == db::CURRENT || Some(&vn) == current.as_ref() {
                continue;
            }
            let p = v.path();
            if v.file_type()?.is_dir() {
                if !vn.ends_with(db::PARTIAL_SUFFIX) {
                    cut(&p);
                }
                fs::remove_dir_all(&p)?;
            } else {
                fs::remove_file(&p)?;
            }
        }
        if current.is_none() {
            let _ = fs::remove_dir(e.path());
        }
    }
    if let Ok(rd) = fs::read_dir(root.join(db::CACHE_DIR)) {
        for e in rd.flatten() {
            let n = e.file_name();
            let n = n.as_encoded_bytes();
            if (n.ends_with(b".part") || n.ends_with(b".new")) && e.file_type().map_or(false, |t| t.is_file()) {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    Ok(())
}

fn load_keys(o: &Options, root: &Path) -> Result<TrustedKeys> {
    let keys = match &o.keys_dir {
        Some(d) => TrustedKeys::from_dir(d)?,
        None => TrustedKeys::from_root(root)?,
    };
    for (name, why) in &keys.skipped {
        eprintln!("oct: warning: key file {name} skipped: {why}");
    }
    if keys.is_empty() && !o.allow_untrusted {
        eprintln!("oct: warning: no trusted keys loaded");
    }
    Ok(keys)
}

fn repo_specs(o: &Options, root: &Path) -> Result<Vec<String>> {
    let mut specs = if o.repositories.is_empty() {
        repo::read_repositories_file(root)?
    } else {
        o.repositories.clone()
    };
    for extra in &o.extra_repositories {
        if !specs.contains(extra) {
            specs.push(extra.clone());
        }
    }
    if specs.is_empty() {
        return Err(Error::Msg(format!(
            "no repositories (give --repository, or list them in {}/etc/apk/repositories)",
            root.display()
        )));
    }
    Ok(specs)
}

fn load_repos(
    o: &Options,
    root: &Path,
    arch: &str,
    keys: &TrustedKeys,
    save_freshness: bool,
    cache_max_age: Option<std::time::Duration>,
) -> Result<(Vec<Repo>, Option<Http>, usize)> {
    let specs = repo_specs(o, root)?;
    let remote = specs.iter().any(|s| repo::is_remote(&repo::locate(s, arch).0));
    let http = if remote { Some(Http::new()?) } else { None };
    let vo = VerifyOptions { allow_untrusted: o.allow_untrusted, ..Default::default() };
    let cache = root.join(db::CACHE_DIR);
    let mut fresh = repo::Freshness::load(root, o.allow_rollback);
    crate::prof::mark("  repositories: keys, freshness record");
    let mut repos = Vec::new();
    let mut problems: usize = 0;
    let mut last_err: Option<Error> = None;
    for s in &specs {
        match repo::load(s, arch, keys, &vo, http.as_ref(), &cache, &fresh, cache_max_age) {
            Ok(r) => {
                if r.stale {
                    problems += 1;
                }
                repos.push(r);
            }
            Err(e @ (Error::Untrusted(_) | Error::Integrity(_))) => return Err(e),
            Err(e) => {
                eprintln!("oct: warning: {s}: {e}; skipped");
                problems += 1;
                last_err = Some(e);
            }
        }
    }
    if repos.is_empty() {
        return Err(last_err.unwrap_or_else(|| Error::Msg("no usable repositories".into())));
    }
    crate::prof::mark("  repositories: loaded and checked against the freshness record");
    if save_freshness {
        fresh.save(&repos)?;
    }
    crate::prof::mark("  repositories: freshness record compared");
    Ok((repos, http, problems))
}

struct JobSpec {
    job: unpack::Job,
    url: Option<String>,
    work: u64,
}

/// From how many bytes to unpack a helper thread syncs while the workers
/// write.
const HELPER_MIN_BYTES: u64 = 32 << 20;

/// Fetches and unpacks all jobs on worker threads.
fn run_jobs(
    o: &Options,
    root: &Path,
    jobs: &[JobSpec],
    keys: &TrustedKeys,
    http: Option<&Http>,
    ids: &IdMap,
) -> Result<Vec<String>> {
    let n = jobs.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(jobs[i].work));
    let workers = if o.jobs > 0 {
        o.jobs
    } else {
        std::thread::available_parallelism().map(|p| p.get()).unwrap_or(4).min(16)
    }
    .min(n)
    .max(1);
    let items = jobs.iter().map(|j| (format!("{}-{}", j.job.name, j.job.version), j.work)).collect();
    let progress = Progress::new(&format!("{n} packages to unpack, {workers} threads"), items, o.fancy());
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let errors: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let warnings: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let workers_done = AtomicUsize::new(0);
    let worker = || loop {
        if failed.load(Ordering::Relaxed) {
            break;
        }
        let k = next.fetch_add(1, Ordering::Relaxed);
        if k >= n {
            break;
        }
        let i = order[k];
        let js = &jobs[i];
        let nv = format!("{}-{}", js.job.name, js.job.version);
        let fail = |e: Error| {
            errors.lock().unwrap().push(format!("{nv}: {e}"));
            progress.finish_item(i, false, e.to_string());
            failed.store(true, Ordering::Relaxed);
        };
        if let Some(url) = &js.url {
            progress.set_phase(i, Phase::Fetching, None);
            let Some(http) = http else {
                fail(Error::Msg("no HTTP client".into()));
                continue;
            };
            let r = fetch::fetch(
                http,
                url,
                &js.job.src,
                &js.job.index_info,
                keys,
                o.allow_untrusted,
                &mut |b| progress.advance(i, b),
            );
            if let Err(e) = r {
                fail(e);
                continue;
            }
        }
        progress.set_phase(i, Phase::Unpacking, None);
        match unpack::unpack(root, &js.job, keys, ids, o.allow_untrusted, &mut |b| progress.advance(i, b)) {
            Ok(st) => {
                warnings.lock().unwrap().extend(st.warnings.iter().map(|w| format!("{nv}: {w}")));
                progress.finish_item(i, true, format!("{} entries, {}", st.entries, human(st.bytes)));
            }
            Err(e) => fail(e),
        }
    };
    // writeback overlapping the unpacking: a helper thread syncs the file
    // system over and over while the workers write, so that the syncfs before
    // the journal finds little left to do (the workers themselves never wait
    // for it). A small job's syncfs is cheap anyway; there the helper's
    // repeated syncs would only cost time.
    let helper = jobs.iter().map(|j| j.work).sum::<u64>() >= HELPER_MIN_BYTES;
    std::thread::scope(|sc| {
        let wd = &workers_done;
        if helper {
            sc.spawn(move || {
                let Ok(f) = fs::File::open(root) else { return };
                while wd.load(Ordering::Relaxed) < workers {
                    let _ = sys::sync_fs(&f);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            });
        }
        for _ in 0..workers {
            sc.spawn(|| {
                worker();
                workers_done.fetch_add(1, Ordering::Relaxed);
            });
        }
    });
    progress.finish();
    let errors = errors.into_inner().unwrap();
    if !errors.is_empty() {
        return Err(Error::Msg(errors.join("\n")));
    }
    Ok(warnings.into_inner().unwrap())
}

/// Removes the version directories a failed transaction unpacked.
fn cleanup_new(root: &Path, db: &Db, ops: &[Op]) {
    for op in ops {
        if let Some(v) = op.new_version() {
            if db.installed.get(op.name()).map(|i| i.meta.info.version.as_str()) == Some(v) {
                continue;
            }
            // only through real directories
            let Ok(pdir) = db::checked_pkg_parent(root, op.name()) else { continue };
            let d = pdir.join(v);
            if fs::symlink_metadata(&d).map_or(false, |m| m.is_dir()) {
                let _ = fs::remove_dir_all(&d);
            }
            let _ = fs::remove_dir(&pdir);
        }
    }
}

/// Warns about installed packages that every repository offers only in
/// older versions (a mirror serving an old index, or a downgrade).
fn warn_newer_installed(db: &Db, u: &Universe) {
    let mut best: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for c in u.cands.iter().filter(|c| c.repo.is_some()) {
        let e = best.entry(c.info.name.as_str()).or_insert(c.info.version.as_str());
        if crate::version::cmp(&c.info.version, e) == std::cmp::Ordering::Greater {
            *e = c.info.version.as_str();
        }
    }
    for (name, ip) in &db.installed {
        if let Some(v) = best.get(name.as_str()) {
            if crate::version::cmp(&ip.meta.info.version, v) == std::cmp::Ordering::Greater {
                eprintln!(
                    "oct: warning: {name} {} is installed, but the repositories offer only up to {v}",
                    ip.meta.info.version
                );
            }
        }
    }
}

fn world_strings(world: &[Dependency]) -> Vec<String> {
    db::format_world(world).lines().filter(|l| !l.is_empty()).map(String::from).collect()
}

/// Resolves `world` and carries out the changes.
fn transact(o: &Options, db: &mut Db, world: Vec<Dependency>, mode: Mode, fresh: HashSet<String>, use_repos: bool) -> Result<bool> {
    let t0 = Instant::now();
    crate::prof::mark("open: lock, database, leftovers");
    if !db.broken.is_empty() {
        let list: Vec<(String, String)> =
            db.broken.iter().map(|(n, v, why)| (n.clone(), format!("{n}-{v}: {why}"))).collect();
        return Err(broken_records(&list));
    }
    let root = db.root.clone();
    let arches = arches(o, &root);
    let keys = load_keys(o, &root)?;
    crate::prof::mark("  keys loaded");
    let (repos, http, _) =
        if use_repos { load_repos(o, &root, &arches[0], &keys, !o.simulate, None)? } else { (Vec::new(), None, 0) };
    crate::prof::mark("repositories: read and verified");
    let u = Universe::with_arches(&repos, &db.installed, &arches);
    crate::prof::mark("  resolve: universe built");
    if use_repos {
        warn_newer_installed(db, &u);
    }
    let sol = solver::solve(&u, &world, mode, &fresh)?;
    crate::prof::mark("resolve");
    let t_solve = t0.elapsed();

    let mut ops: Vec<Op> = Vec::new();
    let mut jobs: Vec<JobSpec> = Vec::new();
    let mut chosen: HashSet<&str> = HashSet::new();
    let cache = root.join(db::CACHE_DIR);
    for &c in &sol.order {
        let cand = &u.cands[c];
        let name = cand.info.name.as_str();
        chosen.insert(name);
        let new = cand.info.version.clone();
        let op = match db.installed.get(name) {
            Some(ip) if ip.meta.info.version == new => continue,
            Some(ip) => Op::Upgrade { name: name.into(), old: ip.meta.info.version.clone(), new: new.clone() },
            None => Op::Install { name: name.into(), new: new.clone() },
        };
        if !db::safe_component(name) || !db::safe_version(&new) {
            return Err(Error::Schema(format!("unsafe package name or version '{}'", cand.info.nv())));
        }
        let Some(ri) = cand.repo else {
            return Err(Error::Msg(format!("{}: not in any repository", cand.info.nv())));
        };
        let r = &repos[ri];
        let (src, url) = fetch::source_path(r, &cand.info, &cache)?;
        let work = cand.info.installed_size + if url.is_some() { cand.info.file_size } else { 0 };
        jobs.push(JobSpec {
            job: unpack::Job {
                name: name.into(),
                version: new,
                src,
                index_info: cand.info.clone(),
                source: r.package_url(&cand.info)?,
            },
            url,
            work,
        });
        ops.push(op);
    }
    for (name, ip) in &db.installed {
        if !chosen.contains(name.as_str()) {
            ops.push(Op::Remove { name: name.clone(), old: ip.meta.info.version.clone() });
        }
    }
    let new_world = world_strings(&world);
    if ops.is_empty() {
        if !o.simulate && new_world != world_strings(&db.world) {
            db::write_atomic(&Db::state(&root).join("world"), db::format_world(&world).as_bytes())?;
        }
        println!("OK: nothing to do; {} packages installed", db.installed.len());
        return Ok(true);
    }

    let (mut ni, mut nu, mut nr) = (0, 0, 0);
    for op in &ops {
        match op {
            Op::Install { name, new } => {
                ni += 1;
                println!("  install  {name} {new}");
            }
            Op::Upgrade { name, old, new } => {
                nu += 1;
                println!("  upgrade  {name} {old} -> {new}");
            }
            Op::Remove { name, old } => {
                nr += 1;
                println!("  remove   {name} {old}");
            }
        }
    }
    let unpack_bytes: u64 = jobs.iter().map(|j| j.job.index_info.installed_size).sum();
    println!(
        "{ni} to install, {nu} to upgrade, {nr} to remove ({} to unpack)",
        human(unpack_bytes)
    );
    if o.simulate {
        println!("OK: simulated (no changes made)");
        return Ok(true);
    }

    let run_hooks = !o.no_scripts && !o.no_commit_hooks;
    if run_hooks {
        commit::run_commit_hooks(&root, "pre-commit")?;
    }

    // An add of new packages only, and of little, is not synced
    // (crate::unsynced): nothing installed is replaced or removed, so a
    // power cut can cost only the packages being added, and the next run
    // finds and removes what is not whole. From 32 MiB on a change is
    // synced as before: there the flushes are a few percent, and the
    // helper below already overlaps them with the unpacking.
    let unsynced = !ops.is_empty()
        && ops.iter().all(|op| matches!(op, Op::Install { .. }))
        && jobs.iter().map(|j| j.work).sum::<u64>() < HELPER_MIN_BYTES
        && crate::unsynced::ensure(&root, &db.installed);
    crate::prof::mark(if unsynced { "this add is not synced" } else { "this change is synced" });

    // fetch and unpack; the dirty mark makes the next run clean up after a
    // crash before the journal exists
    let t1 = Instant::now();
    let dirty = dirty_mark(&root);
    fs::write(&dirty, b"")?;
    let ids = IdMap::load(&root);
    let warnings = match run_jobs(o, &root, &jobs, &keys, http.as_ref(), &ids) {
        Ok(w) => w,
        Err(e) => {
            cleanup_new(&root, db, &ops);
            if run_hooks {
                let _ = commit::run_commit_hooks(&root, "post-commit");
            }
            return Err(e);
        }
    };
    for w in &warnings {
        eprintln!("oct: warning: {w}");
    }
    crate::prof::mark("fetch and unpack");
    let t_unpack = t1.elapsed();

    // file ownership of the final set
    let changed: HashSet<&str> = ops.iter().map(|o| o.name()).collect();
    let mut finals: Vec<PkgState> = Vec::new();
    // which unchanged packages' records the switch reads (None: every one)
    let mut decided: Option<HashSet<String>> = None;
    let mut check = || -> Result<()> {
        // every record the switch reads must be readable before the journal
        // is written: a journal whose switch can't load a record would stop
        // every later run (N-1)
        let mut broken: Vec<(String, String)> = Vec::new();
        let mut olds_st: Vec<PkgState> = Vec::new();
        for op in &ops {
            if let Some(v) = op.old_version() {
                let d = Db::pkg_dir(&root, op.name(), v);
                if d.join(db::META_DIR).exists() {
                    match PkgState::load(&d, true) {
                        Ok(st) => olds_st.push(st),
                        Err(e) => broken.push((op.name().to_string(), format!("{}-{v}: {e}", op.name()))),
                    }
                }
            }
        }
        let mut news_st: Vec<PkgState> = Vec::new();
        for op in &ops {
            if let Some(v) = op.new_version() {
                news_st.push(PkgState::load(&Db::pkg_dir(&root, op.name(), v), true)?);
            }
        }
        // the unchanged packages whose records matter: with a matching index
        // only those the switch will read (crate::index::wanted, as the switch decides)
        let wanted: Option<HashSet<String>> = db.index.as_ref().and_then(|ix| {
            let o: Vec<&PkgState> = olds_st.iter().collect();
            let n: Vec<&PkgState> = news_st.iter().collect();
            let names: HashSet<String> = changed.iter().map(|s| s.to_string()).collect();
            crate::index::wanted(ix, &mut crate::rootfs::RootFs::new(&root), &db.installed, &names, &o, &n)
        });
        decided = wanted.clone();
        for (name, ip) in &db.installed {
            if !changed.contains(name.as_str()) && wanted.as_ref().map_or(true, |w| w.contains(name)) {
                match PkgState::load(&ip.dir, false) {
                    Ok(st) => finals.push(st),
                    Err(e) => broken.push((name.clone(), format!("{}: {e}", ip.meta.nv()))),
                }
            }
        }
        if !broken.is_empty() {
            return Err(broken_records(&broken));
        }
        finals.extend(news_st);
        let (_, conflicts) = commit::ownership(&finals);
        if !conflicts.is_empty() {
            return Err(Error::Msg(format!("file conflicts:\n  {}", conflicts.join("\n  "))));
        }
        Ok(())
    };
    if let Err(e) = check() {
        cleanup_new(&root, db, &ops);
        if run_hooks {
            let _ = commit::run_commit_hooks(&root, "post-commit");
        }
        return Err(e);
    }
    drop(finals);
    crate::prof::mark("check: records, index lookups, ownership");

    // the switch
    let t2 = Instant::now();
    // the journal names the records the switch reads and the index it was
    // planned with: finishing it after a crash reads the same ones
    let reads: Option<Vec<String>> = decided.map(|w| {
        let mut v: Vec<String> = w.into_iter().collect();
        v.sort();
        v
    });
    let index_stamp = reads.as_ref().map(|_| {
        crate::index::stamp(&db.installed.iter().map(|(n, ip)| (n.clone(), ip.meta.info.version.clone())).collect())
    });
    // written under its temporary name first, so that one syncfs makes it
    // durable together with everything unpacked; in effect only after that
    let pending = JournalFile::prepare(
        &root,
        Journal { ops: ops.clone(), world: new_world, reads, index: index_stamp, unsynced, done: Default::default() },
    )?;
    let jf = if unsynced {
        pending.commit_unsynced(&root)?
    } else {
        sys::sync_fs(&fs::File::open(&root)?)?;
        crate::prof::mark("syncfs before the switch (packages and journal)");
        pending.commit(&root)?
    };
    crate::prof::mark("journal in effect");
    if o.crash_at.as_deref() == Some("journal") {
        eprintln!("oct: CATALYST_CRASH_AT=journal: killed (test hook)");
        sys::kill_self();
    }
    let switch_res =
        commit::switch(&root, &db.installed, jf, &SwitchOptions { unsynced, ..o.switch_opts() }, db.index.as_ref());
    if run_hooks {
        let _ = commit::run_commit_hooks(&root, "post-commit");
    }
    crate::prof::mark("switch: old versions deleted; post-commit hooks");
    let rep = switch_res?;
    // the database in memory stays the one from before the switch (the
    // process ends here); the count for the summary comes from the plan
    let installed_now = db.installed.len() + ni - nr;
    let t_switch = t2.elapsed();

    let ok = rep.errors.is_empty();
    if ok {
        let _ = fs::remove_file(&dirty);
    }
    println!(
        "{}: {} packages installed ({ni} new, {nu} upgraded, {nr} removed); {} files placed, {} removed; \
         resolve {:.2}s, fetch+unpack {:.2}s, switch {:.2}s, total {:.2}s",
        if ok { "OK" } else { "ERRORS" },
        installed_now,
        rep.placed,
        rep.removed,
        t_solve.as_secs_f64(),
        t_unpack.as_secs_f64(),
        t_switch.as_secs_f64(),
        t0.elapsed().as_secs_f64()
    );
    if !rep.errors.is_empty() {
        eprintln!("oct: {} errors during the switch:", rep.errors.len());
        for e in &rep.errors {
            eprintln!("  {e}");
        }
    }
    Ok(ok)
}

fn parse_names(names: &[String]) -> Result<Vec<Dependency>> {
    if names.is_empty() {
        return Err(Error::Msg("no package names given".into()));
    }
    names.iter().map(|n| Dependency::parse_world(n)).collect()
}

/// `add`: the names go into the world. A name that is already installed
/// keeps its installed version when that satisfies the request (as apk
/// without `-u`); with `o.upgrade` (`-u`), named packages upgrade to the
/// newest version.
pub fn add(o: &Options, names: &[String]) -> Result<bool> {
    let deps = parse_names(names)?;
    let mut db = if o.simulate { open_readonly(o)? } else { open(o)? };
    let added: HashSet<String> = deps.iter().map(|d| d.name.clone()).collect();
    let fresh: HashSet<String> = if o.upgrade {
        added.clone()
    } else {
        added.iter().filter(|n| !db.installed.contains_key(*n)).cloned().collect()
    };
    let mut world: Vec<Dependency> = db.world.iter().filter(|d| !added.contains(&d.name)).cloned().collect();
    world.extend(deps);
    transact(o, &mut db, world, Mode::Keep, fresh, true)
}

pub fn del(o: &Options, names: &[String]) -> Result<bool> {
    let deps = parse_names(names)?;
    let mut db = if o.simulate { open_readonly(o)? } else { open(o)? };
    let gone: HashSet<String> = deps.iter().map(|d| d.name.clone()).collect();
    for n in &gone {
        if !db.world.iter().any(|d| &d.name == n) {
            eprintln!("oct: warning: {n} is not in the world");
        }
    }
    let world: Vec<Dependency> = db.world.iter().filter(|d| !gone.contains(&d.name)).cloned().collect();
    let ok = transact(o, &mut db, world, Mode::Keep, HashSet::new(), false)?;
    if !o.simulate {
        for n in &gone {
            // after the switch: what is installed is what has a Current
            if Db::programs(&db.root).join(n).join(db::CURRENT).exists() {
                println!("oct: {n} stays installed: other packages need it");
            }
        }
    }
    Ok(ok)
}

pub fn upgrade(o: &Options) -> Result<bool> {
    let mut db = if o.simulate { open_readonly(o)? } else { open(o)? };
    let world = db.world.clone();
    transact(o, &mut db, world, Mode::Upgrade, HashSet::new(), true)
}

pub fn list(o: &Options) -> Result<bool> {
    let db = open_readonly(o)?;
    let world: HashSet<&str> = db.world.iter().map(|d| d.name.as_str()).collect();
    for (name, ip) in &db.installed {
        let i = &ip.meta.info;
        println!(
            "{}-{} {} {{{}}} ({}){}",
            name,
            i.version,
            i.arch,
            i.origin,
            i.license,
            if world.contains(name.as_str()) { " [world]" } else { "" }
        );
    }
    Ok(true)
}

/// Refreshes all configured repository indexes and saves freshness state.
/// Returns `Ok(false)` (exit 1) when any repository failed or fell back to cache.
pub fn update(o: &Options) -> Result<bool> {
    if o.simulate {
        let root = abs_root(o, false)?;
        let specs = repo_specs(o, &root)?;
        for s in &specs {
            println!("  would fetch {s}");
        }
        println!("OK: simulated ({} repositories)", specs.len());
        return Ok(true);
    }
    let db = open(o)?;
    let root = &db.root;
    let arches = arches(o, root);
    let keys = load_keys(o, root)?;
    let (repos, _, problems) = load_repos(o, root, &arches[0], &keys, true, None)?;
    let total: usize = repos.iter().map(|r| r.index.packages.len()).sum();
    for r in &repos {
        println!(
            "  {} ({} packages{})",
            r.index_url,
            r.index.packages.len(),
            if r.stale { ", cached" } else { "" }
        );
    }
    if problems > 0 {
        println!("ERRORS: {problems} unavailable or stale repositories; {} packages available", total);
        return Ok(false);
    }
    println!("OK: {} repositories, {} packages available", repos.len(), total);
    Ok(true)
}

#[derive(Debug, Clone, Default)]
pub struct SearchOpts {
    pub exact: bool,
    pub description: bool,
}

fn match_text(pat: &str, text: &str, exact: bool) -> bool {
    if exact {
        return text == pat;
    }
    if pat.contains('*') || pat.contains('?') {
        return sys::glob_match(pat.as_bytes(), text.as_bytes());
    }
    text.to_ascii_lowercase().contains(&pat.to_ascii_lowercase())
}

fn pkg_matches(pat: &str, p: &crate::adb::PkgInfo, so: &SearchOpts) -> bool {
    if match_text(pat, &p.name, so.exact) {
        return true;
    }
    if p.provides.iter().any(|pr| match_text(pat, &pr.name, so.exact)) {
        return true;
    }
    !so.exact && so.description && match_text(pat, &p.description, false)
}

/// Searches configured repositories for packages matching `patterns` (by
/// package name or provides, or all packages if `patterns` is empty).
pub fn search(o: &Options, so: &SearchOpts, patterns: &[String]) -> Result<bool> {
    let root = abs_root(o, false)?;
    db::real_dir(&root.join(db::CACHE_DIR)).map_err(Error::Io)?;
    let arches = arches(o, &root);
    let keys = load_keys(o, &root)?;
    let (repos, _, _) = load_repos(o, &root, &arches[0], &keys, false, Some(std::time::Duration::from_secs(60)))?;
    let mut matches: std::collections::BTreeMap<(String, String), String> = std::collections::BTreeMap::new();
    for r in &repos {
        for p in &r.index.packages {
            if !p.arch.is_empty() && p.arch != "noarch" && !arches.iter().any(|a| a == &p.arch) {
                continue;
            }
            let ok = patterns.is_empty() || patterns.iter().any(|pat| pkg_matches(pat, p, so));
            if ok {
                matches.entry((p.name.clone(), p.version.clone())).or_insert_with(|| p.description.clone());
            }
        }
    }
    for ((name, ver), desc) in matches {
        if so.exact || desc.is_empty() {
            println!("{name}-{ver}");
        } else {
            println!("{name}-{ver} - {desc}");
        }
    }
    Ok(true)
}

#[derive(Debug, Clone, Default)]
pub struct InfoOpts {
    pub installed: bool,
    pub all: bool,
    pub description: bool,
    pub contents: bool,
    pub depends: bool,
    pub provides: bool,
    pub size: bool,
    pub who_owns: bool,
}

impl InfoOpts {
    fn any_section(&self) -> bool {
        self.all || self.description || self.contents || self.depends || self.provides || self.size
    }
}

fn print_pkg_info(
    info: &crate::adb::PkgInfo,
    dir: Option<&Path>,
    io: &InfoOpts,
) -> Result<()> {
    let nv = info.nv();
    let default_view = !io.any_section();
    if default_view || io.all || io.description {
        println!("{nv} description:\n{}", info.description);
        if !info.url.is_empty() {
            println!("{nv} webpage:\n{}", info.url);
        }
    }
    if default_view || io.all || io.size {
        println!("{nv} installed size:\n{} ({})", human(info.installed_size), info.installed_size);
    }
    if io.all {
        if !info.arch.is_empty() {
            println!("{nv} arch:\n{}", info.arch);
        }
        if !info.license.is_empty() {
            println!("{nv} license:\n{}", info.license);
        }
        if !info.origin.is_empty() {
            println!("{nv} origin:\n{}", info.origin);
        }
    }
    if io.all || io.depends {
        println!("{nv} depends on:");
        for d in &info.depends {
            println!("{d}");
        }
    }
    if io.all || io.provides {
        println!("{nv} provides:");
        for p in &info.provides {
            println!("{p}");
        }
    }
    if io.all || io.contents {
        if let Some(d) = dir {
            println!("{nv} contains:");
            for r in db::read_files(d)? {
                println!("{}", r.path_lossy());
            }
        }
    }
    Ok(())
}

/// Shows information about installed (or repository) packages, checks
/// whether packages are installed (`-e`), or looks up file ownership (`-W`).
pub fn info(o: &Options, io: &InfoOpts, args: &[String]) -> Result<bool> {
    let db = open_readonly(o)?;
    if io.who_owns {
        if args.is_empty() {
            return Err(Error::Msg("info -W needs at least one PATH".into()));
        }
        let rfs = crate::rootfs::RootFs::new(&db.root);
        let rels: Vec<String> = args
            .iter()
            .map(|arg| {
                let abs = if db.root == Path::new("/") && !arg.starts_with('/') {
                    std::env::current_dir().map(|d| d.join(arg)).unwrap_or_else(|_| PathBuf::from(arg))
                } else {
                    PathBuf::from(arg)
                };
                abs.to_string_lossy().trim_start_matches('/').to_string()
            })
            .collect();
        // with an index, only the packages that claim one of the paths (as
        // given, or with its last component followed) are read
        let wanted: Option<HashSet<String>> = db.index.as_ref().and_then(|ix| {
            let mut w = HashSet::new();
            for rel in &rels {
                w.extend(ix.file_claimants(rel.as_bytes()).ok()?);
                if let Ok(res) = rfs.resolve(rel.as_bytes(), true) {
                    w.extend(ix.file_claimants(&res.canon).ok()?);
                }
            }
            Some(w)
        });
        let mut pkgs: Vec<PkgState> = Vec::new();
        for (name, ip) in &db.installed {
            if wanted.as_ref().map_or(true, |w| w.contains(name)) {
                if let Ok(st) = PkgState::load(&ip.dir, false) {
                    pkgs.push(st);
                }
            }
        }
        let (own, _) = commit::ownership(&pkgs);
        let mut all_found = true;
        for (arg, rel) in args.iter().zip(rels.iter()) {
            let rel = rel.as_str();
            let found = own.owner.get(rel.as_bytes()).copied().or_else(|| {
                // Retry once following the final component if it is a symlink inside ROOT.
                let res = rfs.resolve(rel.as_bytes(), true).ok()?;
                own.owner.get(&res.canon).copied()
            });
            if let Some((pi, _)) = found {
                println!("{arg} is owned by {}", pkgs[pi].meta.nv());
            } else {
                eprintln!("oct: {arg}: not owned by any installed package");
                all_found = false;
            }
        }
        return Ok(all_found);
    }
    if args.is_empty() {
        for name in db.installed.keys() {
            println!("{name}");
        }
        return Ok(true);
    }
    if io.installed {
        let mut all_ok = true;
        for name in args {
            let found = db.installed.contains_key(name)
                || db.installed.values().any(|ip| ip.meta.info.provides.iter().any(|p| p.name == *name));
            if found {
                println!("{name}");
            } else {
                all_ok = false;
            }
        }
        return Ok(all_ok);
    }

    let mut repos_cache: Option<Vec<Repo>> = None;
    let mut all_ok = true;
    for (idx, name) in args.iter().enumerate() {
        if idx > 0 {
            println!();
        }
        if let Some(ip) = db.installed.get(name) {
            print_pkg_info(&ip.meta.info, Some(&ip.dir), io)?;
            continue;
        }
        // Fallback to repositories if not installed
        if repos_cache.is_none() {
            let root = &db.root;
            let arches = arches(o, root);
            let keys = load_keys(o, root)?;
            match load_repos(o, root, &arches[0], &keys, false, Some(std::time::Duration::from_secs(60))) {
                Ok((repos, _, _)) => repos_cache = Some(repos),
                Err(_) => repos_cache = Some(Vec::new()),
            }
        }
        let best = repos_cache
            .as_ref()
            .unwrap()
            .iter()
            .flat_map(|r| r.index.packages.iter())
            .filter(|p| p.name == *name)
            .max_by(|a, b| crate::version::cmp(&a.version, &b.version));
        if let Some(p) = best {
            print_pkg_info(p, None, io)?;
        } else {
            eprintln!("oct: {name}: package not found");
            all_ok = false;
        }
    }
    Ok(all_ok)
}

/// Removes downloaded `.apk` files from `ROOT/var/cache/catalyst/`, keeping
/// cached repository indexes.
pub fn cache_clean(o: &Options) -> Result<bool> {
    let db = if o.simulate { open_readonly(o)? } else { open(o)? };
    let cache = db.root.join(db::CACHE_DIR);
    let mut count: u64 = 0;
    let mut bytes: u64 = 0;
    if let Ok(rd) = fs::read_dir(&cache) {
        for e in rd.flatten() {
            let n = e.file_name();
            let nb = n.as_encoded_bytes();
            if nb.ends_with(b".apk") && e.file_type().map_or(false, |t| t.is_file()) {
                let sz = e.metadata().map(|m| m.len()).unwrap_or(0);
                if !o.simulate {
                    fs::remove_file(e.path())?;
                }
                count += 1;
                bytes += sz;
            }
        }
    }
    if o.simulate {
        println!("OK: would remove {count} cached packages ({})", human(bytes));
    } else {
        println!("OK: removed {count} cached packages ({} freed)", human(bytes));
    }
    Ok(true)
}

/// Verifies (and unless `o.simulate`, repairs) live-tree files of installed
/// packages against their copies in `Programs/<name>/<version>/`.
/// If `names` is empty, checks all installed packages.
pub fn fix(o: &Options, names: &[String]) -> Result<bool> {
    // a leftover journal is finished after step 1: finishing it may need a
    // record that only step 1 can rebuild
    let mut db = if o.simulate { open_readonly(o)? } else { open_locked(o)? };
    let filter: HashSet<&str> = names.iter().map(|s| s.as_str()).collect();
    for n in &filter {
        if !db.installed.contains_key(*n) && !db.broken.iter().any(|b| b.0 == *n) {
            return Err(Error::Msg(format!("{n}: package is not installed")));
        }
    }
    let wanted = |n: &str| filter.is_empty() || filter.contains(n);
    let run_hooks = !o.simulate && !o.no_scripts && !o.no_commit_hooks;
    let mut hooks_open = false;
    let mut errors: Vec<String> = Vec::new();

    // 1. records that can't be read are rebuilt from the signed packages
    // (all of them: one unreadable record stops every change)
    let mut rebuild: Vec<(String, String, String)> = db.broken.clone();
    for (name, ip) in &db.installed {
        // both files: with the index the package list comes from its copy of
        // every info, so an unreadable info is not in db.broken
        if let Err(e) = db::read_meta(&ip.dir).and_then(|_| db::read_files(&ip.dir)) {
            rebuild.push((name.clone(), ip.meta.info.version.clone(), e.to_string()));
        }
    }
    // and the versions a leftover journal switches between, which finishing it reads
    if let Ok(t) = fs::read_to_string(Db::journal_path(&db.root)) {
        if let Ok(j) = Journal::parse(&t) {
            for op in &j.ops {
                for v in [op.old_version(), op.new_version()].into_iter().flatten() {
                    let d = Db::pkg_dir(&db.root, op.name(), v);
                    if !d.join(db::META_DIR).exists() || rebuild.iter().any(|(n, x, _)| n == op.name() && x == v) {
                        continue;
                    }
                    if let Err(e) = db::read_meta(&d).and_then(|_| db::read_files(&d)) {
                        rebuild.push((op.name().to_string(), v.to_string(), e.to_string()));
                    }
                }
            }
        }
    }
    if !rebuild.is_empty() {
        if o.simulate {
            for (n, v, why) in &rebuild {
                println!("  would rebuild the record of {n}-{v} ({why})");
            }
        } else {
            let root = db.root.clone();
            let keys = load_keys(o, &root)?;
            let arches = arches(o, &root);
            match load_repos(o, &root, &arches[0], &keys, false, None) {
                Ok((repos, http, _)) => {
                    let nothing_installed = BTreeMap::new();
                    let u = Universe::with_arches(&repos, &nothing_installed, &arches);
                    if run_hooks {
                        commit::run_commit_hooks(&root, "pre-commit")?;
                        hooks_open = true;
                    }
                    for (n, v, why) in &rebuild {
                        match rebuild_record(o, &root, n, v, &u, &repos, http.as_ref(), &keys) {
                            Ok(src) => println!("  rebuilt the record of {n}-{v} from {src}"),
                            Err(e) => errors.push(format!("{n}-{v}: its record can't be read ({why}) and was not rebuilt: {e}")),
                        }
                    }
                }
                Err(e) => {
                    for (n, v, why) in &rebuild {
                        errors.push(format!("{n}-{v}: its record can't be read ({why}); no repository to rebuild it from: {e}"));
                    }
                }
            }
            db.reload()?;
        }
    }
    // a leftover journal finished, now that the records it reads are whole
    if !o.simulate {
        settle(o, &mut db)?;
    }

    // 2. the files, checked and repaired against the records
    let mut pkgs: Vec<PkgState> = Vec::with_capacity(db.installed.len());
    let mut targets: HashSet<usize> = HashSet::new();
    let mut unreadable: Vec<String> = db.broken.iter().map(|(n, v, why)| format!("{n}-{v}: {why}")).collect();
    for (name, ip) in &db.installed {
        match PkgState::load(&ip.dir, false) {
            Ok(st) => {
                if wanted(name) {
                    targets.insert(pkgs.len());
                }
                pkgs.push(st);
            }
            Err(e) => unreadable.push(format!("{}: {e}", ip.meta.nv())),
        }
    }
    // who owns a path can't be known while a record is unreadable: rather
    // than give its files to another package, nothing is repaired (N-4)
    let blocked = !unreadable.is_empty() && !o.simulate;
    let res = if blocked {
        commit::repair(&db.root, &pkgs, &targets, true)
    } else if run_hooks {
        // checked first, so that a pre-commit snapshot is taken only when
        // something will actually be repaired
        match commit::repair(&db.root, &pkgs, &targets, true) {
            Ok(dry) if dry.repaired > 0 => {
                let mut r = Ok(());
                if !hooks_open {
                    r = commit::run_commit_hooks(&db.root, "pre-commit");
                    hooks_open = r.is_ok();
                }
                r.and_then(|_| commit::repair(&db.root, &pkgs, &targets, false))
            }
            other => other,
        }
    } else {
        commit::repair(&db.root, &pkgs, &targets, o.simulate)
    };
    if hooks_open {
        let _ = commit::run_commit_hooks(&db.root, "post-commit");
    }
    let rep = res?;
    for u in &unreadable {
        errors.push(if blocked {
            format!("{u}; nothing is repaired while this record can't be read")
        } else {
            format!("unreadable record: {u}")
        });
    }
    errors.extend(rep.errors);
    // the index from the records as they are now: fix is where doubts end
    if !o.simulate && !blocked {
        if let Err(e) = crate::index::rebuild(&db.root, &pkgs, true) {
            errors.push(format!("the index could not be written: {e}"));
        }
    }
    let status = if errors.is_empty() { "OK" } else { "ERRORS" };
    println!(
        "{status}: checked {} entries ({} ok, {} {}, {} errors)",
        rep.checked,
        rep.ok,
        rep.repaired,
        if o.simulate || blocked { "would be repaired" } else { "repaired" },
        errors.len()
    );
    for e in &errors {
        eprintln!("oct: {e}");
    }
    Ok(errors.is_empty())
}

/// Rebuilds the record (`.catalyst/`) of an installed version from the signed
/// package in the repositories, the way unpacking writes it. The package
/// file is verified completely first; the files in Programs are not touched
/// (`fix` checks them against the rebuilt record afterwards). Returns where
/// the package came from.
fn rebuild_record(
    o: &Options,
    root: &Path,
    name: &str,
    version: &str,
    u: &Universe,
    repos: &[Repo],
    http: Option<&Http>,
    keys: &TrustedKeys,
) -> Result<String> {
    let dir = Db::pkg_dir(root, name, version);
    if !fs::symlink_metadata(&dir).map_or(false, |m| m.is_dir()) {
        return Err(Error::Msg(format!("{} is not a directory", dir.display())));
    }
    let Some(c) = u.cands.iter().find(|c| c.repo.is_some() && c.info.name == name && c.info.version == version) else {
        return Err(Error::Msg("no repository has this version".into()));
    };
    let r = &repos[c.repo.unwrap()];
    let (src, url) = fetch::source_path(r, &c.info, &root.join(db::CACHE_DIR))?;
    if let Some(url) = &url {
        let http = http.ok_or_else(|| Error::Msg("no HTTP client".into()))?;
        fetch::fetch(http, url, &src, &c.info, keys, o.allow_untrusted, &mut |_| {})?;
    }
    let opts = VerifyOptions { allow_untrusted: o.allow_untrusted, expected_identity: Some(c.info.hashes.clone()), require_signature: false };
    // the whole file: signature or identity, and every file's content
    verify::verify_file(&src, keys, &opts)?;
    let f = fs::File::open(&src).map_err(|e| Error::Msg(format!("{}: {e}", src.display())))?;
    let pr = crate::adb::PackageReader::open(BufReader::new(f), keys, &opts)?;
    let pkg = pr.package().clone();
    if pkg.info.name != name || pkg.info.version != version || pkg.info.arch != c.info.arch {
        return Err(Error::Integrity(format!("{}: contains {} instead of {name}-{version}", src.display(), pkg.info.nv())));
    }
    let source = r.package_url(&c.info)?;
    let meta = db::PkgMeta {
        info: crate::adb::PkgInfo { hashes: c.info.hashes.clone(), file_size: c.info.file_size, ..pkg.info.clone() },
        triggers: pkg.triggers.clone(),
        replaces_priority: pkg.replaces_priority,
        source: source.clone(),
    };
    let recs = db::records(&pkg);
    // the new record is written next to the old one and swapped in; a crash
    // in between leaves no record, which the next `oct fix` rebuilds again
    let work = dir.join(".catalyst-rebuild");
    if fs::symlink_metadata(&work).is_ok() {
        fs::remove_dir_all(&work)?;
    }
    fs::create_dir(&work)?;
    let res = (|| -> Result<()> {
        db::write_meta(&work, &meta, &recs, &pkg)?;
        for f in ["info", "files"] {
            fs::File::open(work.join(db::META_DIR).join(f))?.sync_all()?;
        }
        let live = dir.join(db::META_DIR);
        if fs::symlink_metadata(&live).is_ok() {
            fs::rename(&live, work.join("old"))?;
        }
        fs::rename(work.join(db::META_DIR), &live)?;
        fs::File::open(&dir)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_dir_all(&work);
    res?;
    Ok(source)
}


