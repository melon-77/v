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

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use crate::adb::Dependency;
use crate::commit::{self, PkgState, SwitchOptions};
use crate::db::{self, Db, Journal, JournalFile, Op, StagedPkg};
use crate::error::{Error, Result};
use crate::fetch::{self, Http};
use crate::progress::{human, Phase, Progress};
use crate::repo::{self, Repo};
use crate::solver::{self, Mode, Universe};
use crate::sys;
use crate::unpack::{self, IdMap};
use crate::verify::{TrustedKeys, VerifyOptions};

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
    /// Optional maximum age in days for remote indexes (enforced even on a
    /// fresh root via the signed index timestamp).
    pub max_index_age_days: Option<u64>,
    /// Test hook (`VPM_CRASH_AT`): kill the process at a named point of the
    /// switch.
    pub crash_at: Option<String>,
}

impl Options {
    fn switch_opts(&self) -> SwitchOptions {
        SwitchOptions { no_scripts: self.no_scripts, crash_at: self.crash_at.clone(), verbose: false }
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
    let root = abs_root(o, true)?;
    let mut db = Db::open(&root, o.initdb)?;
    let mut need_gc = false;
    if let Some(j) = JournalFile::open(&root)? {
        println!("vpm: finishing an interrupted switch ({} package changes)", j.plan.ops.len());
        let rep = commit::switch(&db, j, &o.switch_opts(), None)?;
        println!(
            "vpm: switch finished: {} files placed, {} already in place, {} removed",
            rep.placed, rep.unchanged, rep.removed
        );
        db.reload()?;
        need_gc = true;
    }
    let dirty = Db::dirty_path(&root);
    if need_gc || dirty.exists() || db.has_non_installed()? {
        gc(&root, &db)?;
        let _ = fs::remove_file(&dirty);
    }
    Ok(db)
}

/// Opens a root read-only without taking the lock or modifying anything.
pub fn open_readonly(o: &Options) -> Result<Db> {
    let root = abs_root(o, false)?;
    Db::open_readonly(&root, o.initdb)
}

/// Deletes `.partial` directories, stale non-installed DB rows, version
/// directories that are not current (left by an interrupted run before its
/// journal was written, or after it was deleted), and interrupted downloads
/// in the cache.
fn gc(root: &Path, db: &Db) -> Result<()> {
    db.purge_non_installed()?;
    let programs = Db::programs(root);
    for e in fs::read_dir(&programs)? {
        let e = e?;
        if !e.file_type()?.is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        let current = match db.installed.get(&name) {
            Some(i) => Some(i.dir_name.clone()),
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
        eprintln!("vpm: warning: key file {name} skipped: {why}");
    }
    if keys.is_empty() && !o.allow_untrusted {
        eprintln!("vpm: warning: no trusted keys loaded");
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
    let max_age = o.max_index_age_days.map(|d| std::time::Duration::from_secs(d.saturating_mul(86_400)));
    let mut fresh = repo::Freshness::load_with_max_age(root, o.allow_rollback, max_age);
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
                eprintln!("vpm: warning: {s}: {e}; skipped");
                problems += 1;
                last_err = Some(e);
            }
        }
    }
    if repos.is_empty() {
        return Err(last_err.unwrap_or_else(|| Error::Msg("no usable repositories".into())));
    }
    if save_freshness {
        fresh.save(&repos)?;
    }
    Ok((repos, http, problems))
}

struct JobSpec {
    job: unpack::Job,
    url: Option<String>,
    work: u64,
}

/// Fetches and unpacks all jobs on worker threads, returning `(warnings, staged)`.
fn run_jobs(
    o: &Options,
    root: &Path,
    jobs: &[JobSpec],
    keys: &TrustedKeys,
    http: Option<&Http>,
    ids: &IdMap,
) -> Result<(Vec<String>, Vec<StagedPkg>)> {
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
    let staged: Mutex<Vec<StagedPkg>> = Mutex::new(Vec::with_capacity(n));
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
            Ok((st, sp)) => {
                warnings.lock().unwrap().extend(st.warnings.iter().map(|w| format!("{nv}: {w}")));
                staged.lock().unwrap().push(sp);
                progress.finish_item(i, true, format!("{} entries, {}", st.entries, human(st.bytes)));
            }
            Err(e) => fail(e),
        }
    };
    let total_work: u64 = jobs.iter().map(|j| j.work).sum();
    let spawn_syncer = jobs.len() >= 2 && total_work >= 16 * 1024 * 1024;
    std::thread::scope(|sc| {
        // writeback overlapping large multi-package unpacking: a helper thread
        // periodically flushes while the workers write, so that the syncfs
        // before the journal finds little left to do (skipped for small or
        // single-package unpacks where it adds latency and lock contention)
        if spawn_syncer {
            let wd = &workers_done;
            sc.spawn(move || {
                let Ok(f) = fs::File::open(root) else { return };
                while wd.load(Ordering::Relaxed) < workers {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    if wd.load(Ordering::Relaxed) >= workers {
                        break;
                    }
                    let _ = sys::sync_fs(&f);
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
    Ok((warnings.into_inner().unwrap(), staged.into_inner().unwrap()))
}

/// Removes the version directories and any staged DB rows a failed transaction unpacked.
fn cleanup_new(root: &Path, db: &Db, ops: &[Op]) {
    let _ = db.delete_staged(ops);
    for op in ops {
        if let Some(d_name) = op.new_dir() {
            if db.installed.get(op.name()).map(|i| i.dir_name.as_str()) == Some(d_name) {
                continue;
            }
            // only through real directories
            let Ok(pdir) = db::checked_pkg_parent(root, op.name()) else { continue };
            let d = pdir.join(d_name);
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
                    "vpm: warning: {name} {} is installed, but the repositories offer only up to {v}",
                    ip.meta.info.version
                );
            }
        }
    }
}

fn world_strings(world: &[Dependency]) -> Vec<String> {
    db::format_world(world).lines().filter(|l| !l.is_empty()).map(String::from).collect()
}

/// Durably flushes a small set of newly unpacked packages in `Programs/` and
/// the staged SQLite WAL before creating the switch journal, avoiding a
/// whole-filesystem `syncfs`.
fn sync_unpacked_small(root: &Path, staged_map: &HashMap<(String, String), StagedPkg>) -> Result<()> {
    let mut dirs: HashSet<PathBuf> = HashSet::new();
    let programs = Db::programs(root);
    dirs.insert(programs.clone());
    for sp in staged_map.values() {
        let pkg_dir = Db::pkg_dir(root, sp.meta.name(), &sp.dir_name);
        if let Some(par) = pkg_dir.parent() {
            dirs.insert(par.to_path_buf());
        }
        dirs.insert(pkg_dir.clone());
        for r in &sp.recs {
            let p = pkg_dir.join(OsStr::from_bytes(&r.path));
            if r.is_dir() {
                dirs.insert(p);
                continue;
            }
            if r.is_regular() {
                if let Ok(f) = sys::open_regular_nofollow(&p) {
                    f.sync_data()?;
                }
            }
            let mut cur = p.parent();
            while let Some(d) = cur {
                if !dirs.insert(d.to_path_buf()) || d == pkg_dir {
                    break;
                }
                cur = d.parent();
            }
        }
    }
    for d in &dirs {
        if let Ok(df) = sys::open_dir_nofollow(d) {
            df.sync_all()?;
        }
    }
    Db::sync_state_dir(root)?;
    Ok(())
}

/// Resolves `world` and carries out the changes.
fn transact(o: &Options, db: &mut Db, world: Vec<Dependency>, mode: Mode, fresh: HashSet<String>, use_repos: bool) -> Result<bool> {
    let t0 = Instant::now();
    let root = db.root.clone();
    let arches = arches(o, &root);
    let keys = load_keys(o, &root)?;
    let (repos, http, _) =
        if use_repos { load_repos(o, &root, &arches[0], &keys, !o.simulate, None)? } else { (Vec::new(), None, 0) };
    let u = Universe::with_arches(&repos, &db.installed, &arches);
    if use_repos {
        warn_newer_installed(db, &u);
    }
    let sol = solver::solve(&u, &world, mode, &fresh)?;
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
        let (op, new_dir) = match db.installed.get(name) {
            Some(ip)
                if ip.meta.info.version == new
                    && (ip.meta.info.hashes.is_empty() || ip.meta.info.hashes == cand.info.hashes) =>
            {
                continue
            }
            Some(ip) => {
                let new_dir = db::choose_dir_name(&new, &cand.info.hashes, Some(&ip.dir_name));
                let op = Op::Upgrade {
                    name: name.into(),
                    old: ip.meta.info.version.clone(),
                    old_dir: ip.dir_name.clone(),
                    new: new.clone(),
                    new_dir: new_dir.clone(),
                };
                (op, new_dir)
            }
            None => {
                let new_dir = db::choose_dir_name(&new, &cand.info.hashes, None);
                let op = Op::Install { name: name.into(), new: new.clone(), new_dir: new_dir.clone() };
                (op, new_dir)
            }
        };
        if !db::safe_component(name) || !db::safe_version(&new) || !db::safe_dir_name(&new_dir) {
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
                dir_name: new_dir,
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
            ops.push(Op::Remove {
                name: name.clone(),
                old: ip.meta.info.version.clone(),
                old_dir: ip.dir_name.clone(),
            });
        }
    }
    let new_world = world_strings(&world);
    if ops.is_empty() {
        if !o.simulate && new_world != world_strings(&db.world) {
            db.save_world(&world)?;
        }
        println!("OK: nothing to do; {} packages installed", db.installed.len());
        return Ok(true);
    }

    let (mut ni, mut nu, mut nr) = (0, 0, 0);
    for op in &ops {
        match op {
            Op::Install { name, new, .. } => {
                ni += 1;
                println!("  install  {name} {new}");
            }
            Op::Upgrade { name, old, new, .. } => {
                nu += 1;
                if old == new {
                    println!("  upgrade  {name} {old} -> {new} (rebuilt)");
                } else {
                    println!("  upgrade  {name} {old} -> {new}");
                }
            }
            Op::Remove { name, old, .. } => {
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

    // Mark root dirty before writing into Programs/ so a crash before
    // JournalFile::create still triggers gc() on the next open().
    let dirty_file = Db::dirty_path(&root);
    let _ = fs::write(&dirty_file, b"");

    // fetch and unpack
    let t1 = Instant::now();
    let ids = IdMap::load(&root);
    let (warnings, staged) = match run_jobs(o, &root, &jobs, &keys, http.as_ref(), &ids) {
        Ok(res) => res,
        Err(e) => {
            cleanup_new(&root, db, &ops);
            let _ = fs::remove_file(&dirty_file);
            if run_hooks {
                let _ = commit::run_commit_hooks(&root, "post-commit");
            }
            return Err(e);
        }
    };
    for w in &warnings {
        eprintln!("vpm: warning: {w}");
    }
    let t_unpack = t1.elapsed();

    let staged_idx: HashMap<(&str, &str), usize> = staged
        .iter()
        .enumerate()
        .map(|(i, sp)| ((sp.meta.name(), sp.effective_dir_name()), i))
        .collect();

    // file ownership of the final set (using SQLite index + in-memory staged records)
    let changed: HashSet<&str> = ops.iter().map(|o| o.name()).collect();
    let mut finals: Vec<PkgState> = Vec::new();
    let mut check = || -> Result<()> {
        if let Some(colliding) = db.find_colliding_installed_pkgs(&staged, &changed)? {
            for name in colliding {
                if let Some(ip) = db.installed.get(&name) {
                    match PkgState::load_from_db(db, &name, &ip.dir_name, false) {
                        Ok(st) => finals.push(st),
                        Err(e) => eprintln!("vpm: warning: {}: {e}; skipped in conflict check", ip.meta.nv()),
                    }
                }
            }
        } else {
            for (name, ip) in &db.installed {
                if !changed.contains(name.as_str()) {
                    match PkgState::load_from_db(db, name, &ip.dir_name, false) {
                        Ok(st) => finals.push(st),
                        Err(e) => eprintln!("vpm: warning: {}: {e}; skipped in conflict check", ip.meta.nv()),
                    }
                }
            }
        }
        for op in &ops {
            if let Some(d) = op.new_dir() {
                if let Some(&i) = staged_idx.get(&(op.name(), d)) {
                    finals.push(PkgState::from_staged(&root, &staged[i], true));
                } else {
                    finals.push(PkgState::load_from_db(db, op.name(), d, true)?);
                }
            }
        }
        let (_, conflicts) = commit::ownership(&finals);
        if !conflicts.is_empty() {
            return Err(Error::Msg(format!("file conflicts:\n  {}", conflicts.join("\n  "))));
        }
        Ok(())
    };
    if let Err(e) = check() {
        cleanup_new(&root, db, &ops);
        let _ = fs::remove_file(&dirty_file);
        if run_hooks {
            let _ = commit::run_commit_hooks(&root, "post-commit");
        }
        return Err(e);
    }
    drop(finals);
    drop(staged_idx);

    // stage unpacked metadata durably into SQLite in one ACID transaction
    if let Err(e) = db.stage_packages(&staged) {
        cleanup_new(&root, db, &ops);
        let _ = fs::remove_file(&dirty_file);
        if run_hooks {
            let _ = commit::run_commit_hooks(&root, "post-commit");
        }
        return Err(e);
    }
    let staged_recs: usize = staged.iter().map(|sp| sp.recs.len()).sum();
    let staged_map: HashMap<(String, String), StagedPkg> = staged
        .into_iter()
        .map(|sp| ((sp.meta.name().to_string(), sp.effective_dir_name().to_string()), sp))
        .collect();

    // the switch
    let t2 = Instant::now();
    if staged_map.is_empty() {
        // pure removal: nothing unpacked into Programs/
    } else if staged_recs <= commit::SMALL_SYNC_LIMIT {
        sync_unpacked_small(&root, &staged_map)?;
    } else {
        sys::sync_fs(&fs::File::open(&root)?)?;
    }
    let jf = JournalFile::create(&root, Journal { ops: ops.clone(), world: new_world, done: Default::default() })?;
    if o.crash_at.as_deref() == Some("journal") {
        eprintln!("vpm: VPM_CRASH_AT=journal: killed (test hook)");
        sys::kill_self();
    }
    let switch_res = commit::switch(db, jf, &o.switch_opts(), Some(&staged_map));
    if switch_res.is_ok() {
        let _ = fs::remove_file(&dirty_file);
    }
    if run_hooks {
        let _ = commit::run_commit_hooks(&root, "post-commit");
    }
    let rep = switch_res?;
    db.reload()?;
    let t_switch = t2.elapsed();

    let ok = rep.errors.is_empty();
    println!(
        "{}: {} packages installed ({ni} new, {nu} upgraded, {nr} removed); {} files placed, {} removed; \
         resolve {:.2}s, fetch+unpack {:.2}s, switch {:.2}s, total {:.2}s",
        if ok { "OK" } else { "ERRORS" },
        db.installed.len(),
        rep.placed,
        rep.removed,
        t_solve.as_secs_f64(),
        t_unpack.as_secs_f64(),
        t_switch.as_secs_f64(),
        t0.elapsed().as_secs_f64()
    );
    if !rep.errors.is_empty() {
        eprintln!("vpm: {} errors during the switch:", rep.errors.len());
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
            eprintln!("vpm: warning: {n} is not in the world");
        }
    }
    let world: Vec<Dependency> = db.world.iter().filter(|d| !gone.contains(&d.name)).cloned().collect();
    let ok = transact(o, &mut db, world, Mode::Keep, HashSet::new(), false)?;
    if !o.simulate {
        for n in &gone {
            if db.installed.contains_key(n) {
                println!("vpm: {n} stays installed: other packages need it");
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
    db: &Db,
    info: &crate::adb::PkgInfo,
    installed_dir: Option<&str>,
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
        if let Some(dir_name) = installed_dir {
            println!("{nv} contains:");
            for r in db.read_pkg_files(&info.name, dir_name)? {
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
        let mut all_found = true;

        if db.is_sqlite() {
            // O(log N) SQLite index lookup via idx_pkg_files_path
            for arg in args {
                let abs = if db.root == Path::new("/") && !arg.starts_with('/') {
                    std::env::current_dir().map(|d| d.join(arg)).unwrap_or_else(|_| PathBuf::from(arg))
                } else {
                    PathBuf::from(arg)
                };
                let rel_s = abs.to_string_lossy();
                let rel = rel_s.trim_start_matches('/');
                let mut names = db.find_path_owners(rel.as_bytes())?.unwrap_or_default();
                if names.is_empty() {
                    if let Ok(res) = rfs.resolve(rel.as_bytes(), true) {
                        names = db.find_path_owners(&res.canon)?.unwrap_or_default();
                    }
                }
                let metas: Vec<&db::PkgMeta> = names.iter().filter_map(|n| db.installed.get(n).map(|ip| &ip.meta)).collect();
                if let Some(winner) = commit::pick_owner(&metas) {
                    println!("{arg} is owned by {}", winner.nv());
                } else {
                    eprintln!("vpm: {arg}: not owned by any installed package");
                    all_found = false;
                }
            }
            return Ok(all_found);
        }

        // Legacy read-only fallback for unmigrated `.vpm/` roots
        let mut pkgs: Vec<PkgState> = Vec::with_capacity(db.installed.len());
        for (name, ip) in &db.installed {
            if let Ok(st) = PkgState::load_from_db(&db, name, &ip.dir_name, false) {
                pkgs.push(st);
            }
        }
        let (own, _) = commit::ownership(&pkgs);
        for arg in args {
            let abs = if db.root == Path::new("/") && !arg.starts_with('/') {
                std::env::current_dir().map(|d| d.join(arg)).unwrap_or_else(|_| PathBuf::from(arg))
            } else {
                PathBuf::from(arg)
            };
            let rel_s = abs.to_string_lossy();
            let rel = rel_s.trim_start_matches('/');
            let found = own.owner.get(rel.as_bytes()).copied().or_else(|| {
                let res = rfs.resolve(rel.as_bytes(), true).ok()?;
                own.owner.get(&res.canon).copied()
            });
            if let Some((pi, _)) = found {
                println!("{arg} is owned by {}", pkgs[pi].meta.nv());
            } else {
                eprintln!("vpm: {arg}: not owned by any installed package");
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
            if db.is_installed_or_provided(name) {
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
            print_pkg_info(&db, &ip.meta.info, Some(&ip.dir_name), io)?;
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
            print_pkg_info(&db, p, None, io)?;
        } else {
            eprintln!("vpm: {name}: package not found");
            all_ok = false;
        }
    }
    Ok(all_ok)
}

/// Removes downloaded `.apk` files from `ROOT/var/cache/vpm/`, keeping
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
/// packages against their copies in `Programs/<name>/<dir_name>/`.
/// If `names` is empty, checks all installed packages.
pub fn fix(o: &Options, names: &[String]) -> Result<bool> {
    let db = if o.simulate { open_readonly(o)? } else { open(o)? };
    let mut pkgs: Vec<PkgState> = Vec::with_capacity(db.installed.len());
    let mut targets: HashSet<usize> = HashSet::new();
    let mut load_errors: Vec<String> = Vec::new();
    let filter: HashSet<&str> = names.iter().map(|s| s.as_str()).collect();
    for n in &filter {
        if !db.installed.contains_key(*n) {
            return Err(Error::Msg(format!("{n}: package is not installed")));
        }
    }
    for (name, ip) in &db.installed {
        let wanted = filter.is_empty() || filter.contains(name.as_str());
        match PkgState::load_from_db(&db, name, &ip.dir_name, false) {
            Ok(st) => {
                let idx = pkgs.len();
                pkgs.push(st);
                if wanted {
                    targets.insert(idx);
                }
            }
            Err(e) => {
                let msg = format!("{}: unreadable package metadata: {e}", ip.meta.nv());
                if wanted {
                    load_errors.push(msg);
                } else {
                    eprintln!("vpm: warning: {msg}; skipped");
                }
            }
        }
    }
    let run_hooks = !o.simulate && !o.no_scripts && !o.no_commit_hooks;
    let rep = if run_hooks {
        // Check first so we only take a pre-commit snapshot when something will actually be repaired.
        let dry = commit::repair(&db.root, &pkgs, &targets, true)?;
        if dry.repaired > 0 {
            commit::run_commit_hooks(&db.root, "pre-commit")?;
            let res = commit::repair(&db.root, &pkgs, &targets, false);
            let _ = commit::run_commit_hooks(&db.root, "post-commit");
            res?
        } else {
            dry
        }
    } else {
        commit::repair(&db.root, &pkgs, &targets, o.simulate)?
    };
    let mut errors = load_errors;
    errors.extend(rep.errors);
    let status = if errors.is_empty() { "OK" } else { "ERRORS" };
    if o.simulate {
        println!(
            "{status}: checked {} entries ({} ok, {} would be repaired, {} errors)",
            rep.checked,
            rep.ok,
            rep.repaired,
            errors.len()
        );
    } else {
        println!(
            "{status}: checked {} entries ({} ok, {} repaired, {} errors)",
            rep.checked,
            rep.ok,
            rep.repaired,
            errors.len()
        );
    }
    for e in &errors {
        eprintln!("vpm: {e}");
    }
    Ok(errors.is_empty())
}


