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
    /// `--sync`: a change is on disk when the command returns (the journal's
    /// name is fsync'ed and the switch ends with a syncfs), as every change
    /// but an add was before 23 November 2026.
    pub sync: bool,
    /// `--clean-later` / `--clean-now`: whether the old versions of a big
    /// change are deleted by a process of its own after the command has
    /// returned ([`crate::trash`]). `None`: later when the root is `/`,
    /// now under `--root` (whoever set that root up may unmount it next).
    /// `--sync` means now.
    pub clean_later: Option<bool>,
}

impl Options {
    fn switch_opts(&self) -> SwitchOptions {
        SwitchOptions { no_scripts: self.no_scripts, crash_at: self.crash_at.clone(), verbose: false, light: false, clean_later: false }
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

/// The errors of an interrupted switch that this run finished first.
static RECOVERY_ERRORS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How many errors the interrupted switch had that this run finished before
/// its own work (0: there was none, or it went through). A command that
/// went well after such a recovery has still not left the system as it
/// reports: `main` makes it fail.
pub fn recovery_errors() -> usize {
    RECOVERY_ERRORS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Finishes a leftover journal, cleans up after an interrupted run, and
/// rebuilds a missing or outdated index.
fn settle(o: &Options, db: &mut Db) -> Result<usize> {
    let root = db.root.clone();
    let mut recovered = false;
    let mut errors = 0;
    // The catalyst of 23 November 2026 put the journal of an add in effect
    // without any sync, under its mark `unsynced`. A power cut can have
    // left that journal empty or cut short: it cannot be finished, and is
    // removed (while the mark that tells is still there). Some of what its
    // switch did can be on disk all the same: the packages it had begun
    // are named, because files of theirs may be left in the tree.
    if crate::unsynced::path(&root).exists() {
        if let Ok(b) = fs::read(Db::journal_path(&root)) {
            if Journal::parse(&String::from_utf8_lossy(&b)).is_err() {
                eprintln!("oct: note: a journal an earlier catalyst wrote without a sync is incomplete; removed");
                let mut begun: Vec<String> = Vec::new();
                if let Ok(rd) = fs::read_dir(Db::programs(&root)) {
                    for e in rd.flatten() {
                        if e.path().is_dir() && fs::symlink_metadata(e.path().join(crate::db::CURRENT)).is_err() {
                            begun.push(e.file_name().to_string_lossy().into_owned());
                        }
                    }
                }
                begun.sort();
                if !begun.is_empty() {
                    eprintln!(
                        "oct: note: the add it was for had begun with: {}. Files of these may be left in the tree, \
                         belonging to no package and possibly empty; `oct add` puts a package and its files right",
                        begun.join(", ")
                    );
                }
                fs::remove_file(Db::journal_path(&root))?;
                db.reload()?;
                recovered = true;
            }
        }
    }
    legacy_unsynced_notice(&root, db);
    if let Some(j) = JournalFile::open(&root)? {
        println!("oct: finishing an interrupted switch ({} package changes)", j.plan.ops.len());
        // directories that the interrupted switch had moved out of the live
        // tree and not yet looked through go back first: their entries are
        // then removed one by one with everything else
        crate::trash::settle_unchecked(&root, true).map_err(|e| {
            Error::Msg(format!(
                "the trash could not be gone through ({e}): a directory the interrupted switch moved out of the live tree may wait there to be put back, so the switch is not finished now"
            ))
        })?;
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
        // finished as a full switch: the journal's name, which a light
        // switch had not put on disk, goes there first with everything else,
        // and the switch ends with a syncfs
        commit::sync_root(&root)?;
        if j.plan.unsynced {
            eprintln!(
                "oct: note: this switch was begun by an earlier catalyst without a sync; \
                 if the system lost power then, `oct fix` checks its packages"
            );
        }
        let rep = commit::switch(&root, &db.installed, j, &o.switch_opts(), before.as_ref(), Vec::new(), None).map_err(|e| {
            Error::Msg(format!(
                "finishing the interrupted switch failed: {e}\n  if a record can't be read, `oct fix` rebuilds it from the signed package and then finishes the switch"
            ))
        })?;
        println!(
            "oct: switch finished: {} files placed, {} already in place, {} removed",
            rep.placed, rep.unchanged, rep.removed
        );
        errors = rep.errors.len();
        RECOVERY_ERRORS.store(errors, std::sync::atomic::Ordering::Relaxed);
        db.reload()?;
        recovered = true;
    }
    // what an interrupted rebuild of the index left behind, and a journal
    // written but never put in effect (a crash before the switch)
    crate::index::clean_leftovers(&root);
    // A directory moved out of the live tree and still unchecked now
    // belongs to no journal (the one above, if there was one, has put its
    // own back): it is set aside for good. Then the old versions that an
    // earlier run left to be deleted later and that nobody is deleting (its
    // process died).
    if let Err(e) = crate::trash::settle_unchecked(&root, false) {
        eprintln!("oct: warning: the trash of old versions: {e}");
    }
    if let Err(e) = crate::trash::empty(&root) {
        eprintln!("oct: warning: the trash of old versions: {e}");
    }
    let _ = fs::remove_file(Db::journal_pending_path(&root));
    let _ = fs::remove_file(Db::world_pending_path(&root));
    for t in stray_temporaries(&root) {
        let _ = fs::remove_file(t);
    }
    // the clean-up walk over Programs is only needed after an interrupted or
    // failed run: one whose journal was just finished, or that left the
    // dirty mark (set before unpacking, removed when a change succeeded)
    let dirty = dirty_mark(&root);
    if recovered || dirty.exists() {
        gc(&root, db)?;
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
    Ok(errors)
}

/// The temporary files of `db::write_atomic` that a power cut left in
/// catalyst's state directory and in its `indexes/`: such a file is fsync'ed
/// before it is renamed, so a cut between the two leaves it on disk beside
/// the file it was to replace (which is whole, and stays).
fn stray_temporaries(root: &Path) -> Vec<PathBuf> {
    let state = root.join(db::STATE_DIR);
    let mut out = Vec::new();
    for d in [state.clone(), state.join("indexes")] {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            if e.file_name().as_encoded_bytes().ends_with(b".catalyst-tmp") && e.file_type().map_or(false, |t| t.is_file()) {
                out.push(e.path());
            }
        }
    }
    out
}

/// Retires the mark of "unsynced adds" (see `unsynced.rs`): if one is
/// there it is removed, with a note naming the packages added under it.
/// Nothing is checked and nothing is uninstalled: telling a file damaged by
/// a power cut from one the admin changed took guesses, and a wrong guess
/// removed a healthy package.
fn legacy_unsynced_notice(root: &Path, db: &Db) {
    let Some(had) = crate::unsynced::take_legacy(root) else { return };
    let names: Vec<String> = db
        .installed
        .iter()
        .filter(|(n, ip)| !had.contains(&((*n).clone(), ip.meta.info.version.clone())))
        .map(|(n, ip)| format!("{n}-{}", ip.meta.info.version))
        .collect();
    if !names.is_empty() {
        eprintln!(
            "oct: note: added by an earlier catalyst without a sync: {}\n     \
             if the system lost power right after, `oct fix NAME` checks and repairs them",
            names.join(", ")
        );
    }
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
/// valid, are left alone (and reported by [`Db::reload`]).
fn gc(root: &Path, db: &Db) -> Result<()> {
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
    // always there: a local index may name packages on a server, and the
    // client costs nothing until a request is made (TLS is set up by the
    // first https one)
    let http = Some(Http::new()?);
    let vo = VerifyOptions { allow_untrusted: o.allow_untrusted, ..Default::default() };
    let cache = root.join(db::CACHE_DIR);
    let mut fresh = repo::Freshness::load(root, o.allow_rollback);
    crate::prof::mark("  repositories: keys, freshness record");
    let mut repos = Vec::new();
    let mut problems: usize = 0;
    let mut last_err: Option<Error> = None;
    for s in &specs {
        match repo::load_with_cache(s, arch, keys, &vo, http.as_ref(), &cache, &fresh, cache_max_age, save_freshness, !o.simulate) {
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
            Err(e) => {
                // a downloaded file that turns out to be bad must not be met
                // in the cache again by the next attempt
                if js.url.is_some() && matches!(e, Error::Integrity(_) | Error::Format(_) | Error::Schema(_) | Error::Untrusted(_)) {
                    let _ = fs::remove_file(&js.job.src);
                }
                fail(e)
            }
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
                // counted from a guard: a worker that panics is done as
                // well, and the helper must not go on waiting for it
                struct Done<'a>(&'a AtomicUsize);
                impl Drop for Done<'_> {
                    fn drop(&mut self) {
                        self.0.fetch_add(1, Ordering::Relaxed);
                    }
                }
                let _done = Done(&workers_done);
                worker();
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

/// How old a remote repository's cached index may be for an `add` to plan
/// with it without asking the server: four hours, as apk's is. The owner
/// decided this on 2026-11-23 ("Reuse for 4 hours"): fetching the index on
/// every `add` was a request apk does not make. `add -u`, `upgrade` and
/// `update` always ask the server.
const INDEX_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(4 * 3600);

/// Resolves `world` and carries out the changes.
fn transact(o: &Options, db: &mut Db, world: Vec<Dependency>, mode: Mode, fresh: HashSet<String>, use_repos: bool) -> Result<bool> {
    let max_age = (mode == Mode::Keep && !o.upgrade).then_some(INDEX_MAX_AGE);
    transact_at(o, db, world, mode, fresh, use_repos, max_age)
}

/// [`transact`] with the age up to which a cached index is used as it is
/// (`None`: the server is asked).
fn transact_at(
    o: &Options,
    db: &mut Db,
    world: Vec<Dependency>,
    mode: Mode,
    fresh: HashSet<String>,
    use_repos: bool,
    max_age: Option<std::time::Duration>,
) -> Result<bool> {
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
        if use_repos { load_repos(o, &root, &arches[0], &keys, !o.simulate, max_age)? } else { (Vec::new(), None, 0) };
    // Never freed: an index is some twenty thousand small allocations, and
    // giving each back took 0.6 ms at the end of every `add`, just before
    // the process ends and the system takes all of it back at once.
    let repos = std::mem::ManuallyDrop::new(repos);
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
            if o.sync {
                // the world's new name too, not only its content
                fs::File::open(Db::state(&root))?.sync_all()?;
            }
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

    // A change is light unless --sync asks otherwise (README, "One sync"):
    // one syncfs puts the journal and everything it names on disk, the
    // journal's rename into place is not fsync'ed, and the switch ends
    // without a syncfs. Nothing in the live tree is touched before that one
    // syncfs, and everything after it is done in an order the file system
    // keeps, so what a power cut leaves is the change not begun (discarded
    // by the next run) or begun (finished by it). A switch that runs a
    // script puts the journal's name on disk first and syncs after it
    // (`Switch::harden`).
    let light = !o.sync;
    // Old versions of a big change are deleted after the command has
    // returned where someone waits for it on the installed system; anywhere
    // else only when asked for.
    let clean_later = !o.sync && o.clean_later.unwrap_or_else(|| later_by_default(&root));

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
            // planned with a cached index, which may name a package the
            // server has replaced since: once more, with the index fetched
            if max_age.is_some() && repos.iter().any(|r| r.reused) {
                eprintln!("oct: {e}\noct: the repository index came from the cache; fetching it and trying again");
                return transact_at(o, db, world, mode, fresh, use_repos, None);
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
    let mut olds_read: Vec<PkgState> = Vec::new();
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
        crate::prof::mark("  check: records of the change read");
        // the unchanged packages whose records matter: with a matching index
        // only those the switch will read (crate::index::wanted, as the switch decides)
        let wanted: Option<HashSet<String>> = db.index.as_ref().and_then(|ix| {
            let o: Vec<&PkgState> = olds_st.iter().collect();
            let n: Vec<&PkgState> = news_st.iter().collect();
            let names: HashSet<String> = changed.iter().map(|s| s.to_string()).collect();
            crate::index::wanted(ix, &mut crate::rootfs::RootFs::new(&root), &db.installed, &names, &o, &n)
        });
        decided = wanted.clone();
        crate::prof::mark("  check: index lookups");
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
        olds_read = olds_st;
        crate::prof::mark("  check: other records read");
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
    // the records read for the check go to the switch, which needs the same
    let mut pool = finals;
    pool.extend(olds_read);
    // the copies under etc/ that the switch will rename into the live tree
    // are made now, so that the syncfs below puts them on disk whole
    {
        let new_dirs: HashSet<PathBuf> =
            ops.iter().filter_map(|op| op.new_version().map(|v| Db::pkg_dir(&root, op.name(), v))).collect();
        let news: Vec<&PkgState> = pool.iter().filter(|p| new_dirs.contains(&p.dir)).collect();
        if let Err(e) = commit::stage_copies(&root, &news) {
            cleanup_new(&root, db, &ops);
            if run_hooks {
                let _ = commit::run_commit_hooks(&root, "post-commit");
            }
            return Err(e.into());
        }
    }
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
    // The journal and the world it ends with are written under temporary
    // names; one syncfs puts them on disk with everything unpacked; only
    // then is the journal renamed into place: a journal is never there
    // before what it names. A full change fsyncs that rename; a light one
    // does not (PendingJournal::commit_named says why that is enough).
    // A light change through the index leaves nothing of the index to be
    // written after its one syncfs: the index of after the change is
    // written now, beside the current one, and the switch puts it in place
    // by renames (index::stage). If that can't be done the switch updates
    // the index its other way and ends with a syncfs.
    let staged = match (&db.index, &reads) {
        (Some(ix), Some(_)) if light => {
            let names: HashSet<String> = changed.iter().map(|s| s.to_string()).collect();
            let old_dirs: HashSet<PathBuf> =
                ops.iter().filter_map(|op| op.old_version().map(|v| Db::pkg_dir(&root, op.name(), v))).collect();
            let olds: Vec<&PkgState> = pool.iter().filter(|p| old_dirs.contains(&p.dir)).collect();
            // in the order of the change, as the switch places them
            let news: Vec<&PkgState> = ops
                .iter()
                .filter_map(|op| op.new_version().map(|v| Db::pkg_dir(&root, op.name(), v)))
                .filter_map(|d| pool.iter().find(|p| p.dir == d))
                .collect();
            let mut metas: BTreeMap<String, &db::PkgMeta> = db
                .installed
                .iter()
                .filter(|(n, _)| !changed.contains(n.as_str()))
                .map(|(n, ip)| (n.clone(), &ip.meta))
                .collect();
            for p in &news {
                metas.insert(p.meta.name().to_string(), &p.meta);
            }
            match crate::index::stage(&root, ix, &names, &olds, &news, &metas, &mut crate::rootfs::RootFs::new(&root)) {
                Ok(st) => Some(st),
                Err(e) => {
                    eprintln!("oct: warning: index: {e}");
                    crate::index::discard_staged(&root);
                    None
                }
            }
        }
        _ => None,
    };
    let pending = JournalFile::prepare(
        &root,
        Journal {
            ops: ops.clone(),
            world: new_world,
            reads,
            index: index_stamp,
            unsynced: false,
            no_scripts: o.no_scripts,
            closed_versions: true,
            done: Default::default(),
        },
    )?;
    commit::sync_root(&root)?;
    crate::prof::mark("syncfs before the switch (packages and journal)");
    if o.crash_at.as_deref() == Some("synced") {
        eprintln!("oct: CATALYST_CRASH_AT=synced: killed (test hook)");
        sys::kill_self();
    }
    let jf = if light { pending.commit_named(&root)? } else { pending.commit(&root)? };
    crate::prof::mark("journal in effect");
    if o.crash_at.as_deref() == Some("journal") {
        eprintln!("oct: CATALYST_CRASH_AT=journal: killed (test hook)");
        sys::kill_self();
    }
    let switch_res = commit::switch(
        &root,
        &db.installed,
        jf,
        &SwitchOptions { light, clean_later, ..o.switch_opts() },
        db.index.as_ref(),
        pool,
        staged,
    );
    if run_hooks && light && !commit::discover_commit_hooks(&root).is_empty() {
        // A hook may write, and sync, what it likes (a snapshot, a note of
        // the change): like a script it must not get ahead of the change.
        // So when there are hooks the change is put on disk before they
        // run, also after a switch that failed and left its journal.
        if let Err(e) = commit::sync_root(&root) {
            eprintln!("oct: warning: syncfs before the post-commit hooks: {e}");
        }
    }
    // Old versions that wait in the trash: from here on a process of its own
    // deletes them, or this one does where none comes to run. Before the
    // post-commit hooks, which may take their time, and so that a hook finds
    // as little of the old versions as can be (one that takes a snapshot
    // would keep what it finds; `--clean-now` is for such a system).
    if switch_res.as_ref().map_or(false, |r| r.trash) {
        if o.crash_at.as_deref() == Some("trash") {
            eprintln!("oct: CATALYST_CRASH_AT=trash: killed (test hook)");
            sys::kill_self();
        }
        if crate::trash::empty_later(&root) {
            crate::prof::mark("old versions left to a process of its own");
        } else {
            if let Err(e) = crate::trash::empty(&root) {
                eprintln!("oct: warning: deleting the old versions: {e}");
            }
            crate::prof::mark("old versions deleted here (no process came to run)");
        }
    }
    if run_hooks {
        let _ = commit::run_commit_hooks(&root, "post-commit");
    }
    crate::prof::mark("switch: old versions deleted (or left to the trash); post-commit hooks");
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

/// Whether the old versions of a big change are deleted after the command
/// when nothing was said: on the installed system (the root is `/` and is
/// the root of process 1, not a chroot that may be unmounted next) when the
/// command writes to a terminal, where someone waits for it. A script, a
/// build, a container's one command have everything done when the command
/// returns: nothing may be there to finish the rest.
fn later_by_default(root: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    if root != Path::new("/") || !sys::is_tty(1) {
        return false;
    }
    match (fs::metadata("/"), fs::metadata("/proc/1/root")) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
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

/// `empty-trash`: deletes the old versions that a change left to be deleted
/// after it had returned ([`crate::trash`]). oct starts this itself, as a
/// process of its own; it takes no lock and reads no database.
pub fn empty_trash(o: &Options) -> Result<bool> {
    use std::io::Write;
    let root = abs_root(o, false)?;
    // as before anything else is deleted: catalyst's own directories must
    // be real directories
    for d in Db::own_dirs(&root) {
        db::real_dir(&d).map_err(Error::Io)?;
    }
    if o.simulate {
        let e = crate::trash::entries(&root);
        println!("OK: simulated; would delete {} entries of the trash{}{}", e.len(), if e.is_empty() { "" } else { ": " }, e.join(", "));
        return Ok(true);
    }
    // whoever started this waits for this line (trash::empty_later)
    let mut out = std::io::stdout();
    let _ = out.write_all(crate::trash::STARTED).and_then(|()| out.flush());
    crate::trash::empty(&root)?;
    Ok(true)
}

/// `recover`: finishes what an interrupted run left behind (a switch whose
/// journal is there, what a run prepared and never began, an index half
/// rebuilt, old versions left in the trash) and does nothing else. It is what every other command does
/// first; as a command of its own it is for the start of the system, so
/// that a change cut by a power failure is finished before anyone logs in.
/// When nothing was left, the root is not even locked (it may still be
/// mounted read-only).
pub fn recover(o: &Options) -> Result<bool> {
    let root = abs_root(o, false)?;
    let state = root.join(db::STATE_DIR);
    let mut left: Vec<&str> = ["journal", "journal.new", "world.new", "dirty", "index.new", "index.old", "index/next", "trash"]
        .into_iter()
        .filter(|n| *n != "trash" || crate::trash::has_work(&root))
        .filter(|n| fs::symlink_metadata(state.join(n)).is_ok())
        .collect();
    if !stray_temporaries(&root).is_empty() {
        left.push("temporary files");
    }
    if left.is_empty() {
        println!("OK: nothing to finish");
        return Ok(true);
    }
    if o.simulate {
        println!("OK: simulated; would finish or clean up: {}", left.join(", "));
        return Ok(true);
    }
    let mut db = open_locked(o)?;
    let errors = settle(o, &mut db)?;
    if errors > 0 {
        println!("ERRORS: the interrupted switch was finished with {errors} errors; {} packages installed", db.installed.len());
        return Ok(false);
    }
    println!("OK: finished; {} packages installed", db.installed.len());
    Ok(true)
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
                Err(e) => {
                    eprintln!("oct: warning: the repositories could not be read: {e}");
                    repos_cache = Some(Vec::new())
                }
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
        match crate::index::rebuild(&db.root, &pkgs, true) {
            Ok(()) => {}
            Err(e) => errors.push(format!("the index could not be written: {e}")),
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
    if let Err(e) = verify::verify_file(&src, keys, &opts) {
        // a downloaded file that is bad does not stay in the cache
        if url.is_some() && matches!(e, Error::Integrity(_) | Error::Format(_) | Error::Schema(_) | Error::Untrusted(_)) {
            let _ = fs::remove_file(&src);
        }
        return Err(e);
    }
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


