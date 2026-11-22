//! The switch: the only step that touches the live tree.
//!
//! Everything it needs is complete on disk before it starts: the new
//! versions are unpacked under `Programs/<name>/<version>/` (with their
//! `.octet/` metadata), the old ones are still there, and the journal
//! (`var/lib/octet/journal`) says which package goes from which version to
//! which. The switch is written so that running it again from the start
//! finishes it (roll forward): every step is idempotent, and the steps
//! that are not (scripts) are marked in the journal when done.
//!
//! Steps:
//! 1. per installed or upgraded package, dependencies first (apk's order):
//!    - its pre-install/pre-upgrade script; owners re-resolved (the script
//!      may have added users);
//!    - each entry placed in the live tree: directories made with the
//!      package's mode and owner (existing ones are left alone); regular
//!      files outside `etc/` (and device nodes) are hard links to the
//!      Programs copy; files under `etc/` are copies; a symlink is
//!      recreated. A file that replaces another is made under a temporary
//!      name next to its target and renamed over it; a new name is linked
//!      (or symlinked) directly, which is atomic too; a new directory is
//!      made under a temporary name with its owner and mode and renamed
//!      into place. So every path is at any moment either the old or the
//!      new entry. An `etc/` file the admin changed (its content is neither
//!      the old package's nor the new one's) is kept and the new one is
//!      written as `<file>.octet-new`. No entry may resolve into octet's own
//!      directories (`Programs`, `var/lib/octet`, `var/cache/octet`);
//!    - the old version's entries that no package owns any more are
//!      removed (only if they are still what it installed), then its empty
//!      directories;
//!    - `Programs/<name>/Current` flipped (new symlink + rename); the old
//!      version directory made 0700 (its set-id files are no longer in the
//!      live tree);
//!    - its post-install/post-upgrade script (scripts run chrooted, with
//!      the root's lock inherited; directories are resolved afresh after
//!      every script);
//! 2. removed packages: pre-deinstall, their entries, `Current`,
//!    post-deinstall;
//! 3. the world written;
//! 4. owners re-resolved once more; triggers;
//! 5. `syncfs`, the journal deleted, old version directories deleted.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::{self, File, FileTimes, OpenOptions};
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{fchown, lchown, symlink, DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use crate::adb::FileKind;
use crate::db::{self, Db, FileRec, Installed, JournalFile, Op, PkgMeta, RecKind};
use crate::digest::{Alg, Hasher};
use crate::error::{Error, Result};
use crate::rootfs::{self, Resolved, RootFs};
use crate::sys;
use crate::unpack::{self, IdMap};

#[derive(Debug, Clone, Default)]
pub struct SwitchOptions {
    pub no_scripts: bool,
    /// Test hook: abort the process at this point (`OCTET_CRASH_AT`).
    pub crash_at: Option<String>,
    /// Print each script's output and the steps.
    pub verbose: bool,
}

#[derive(Debug, Default)]
pub struct Report {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub placed: u64,
    pub unchanged: u64,
    pub removed: u64,
    pub scripts: u64,
    pub new_configs: Vec<String>,
}

/// A package of the final set (or an old version), with its records.
pub struct PkgState {
    pub meta: PkgMeta,
    pub dir: PathBuf,
    pub recs: Vec<FileRec>,
    pub changed: bool,
}

impl PkgState {
    pub fn load(dir: &Path, changed: bool) -> Result<PkgState> {
        Ok(PkgState { meta: db::read_meta(dir)?, recs: db::read_files(dir)?, dir: dir.to_path_buf(), changed })
    }
}

/// Who owns each non-directory path of the final set, and its directories.
pub struct Ownership {
    /// path -> (package index, record index)
    pub owner: HashMap<Vec<u8>, (usize, usize)>,
    pub dirs: HashSet<Vec<u8>>,
}

fn same_entry(a: &FileRec, b: &FileRec) -> bool {
    match (&a.kind, &b.kind) {
        (RecKind::File(FileKind::Symlink(x)), RecKind::File(FileKind::Symlink(y))) => x == y,
        _ => a.is_regular() && b.is_regular() && a.hash == b.hash && !a.hash.is_empty() && a.mode == b.mode,
    }
}

fn replaces(a: &PkgMeta, b: &PkgMeta) -> bool {
    a.info.replaces.iter().any(|d| d.name == b.info.name && d.matches_version(&b.info.version))
}

/// Assigns every path to one package, as apk's `apk_pkg_replaces_file`
/// decides (§E8.1): a package that `replaces` the other wins (the higher
/// `replaces-priority` first), then the same origin lets the later one
/// take it. Identical entries are shared. Anything else is a conflict,
/// returned as a message; the earlier owner keeps the path.
pub fn ownership(pkgs: &[PkgState]) -> (Ownership, Vec<String>) {
    let mut own = Ownership { owner: HashMap::new(), dirs: HashSet::new() };
    let mut conflicts = Vec::new();
    for (i, p) in pkgs.iter().enumerate() {
        for (ri, r) in p.recs.iter().enumerate() {
            if r.is_dir() {
                own.dirs.insert(r.path.clone());
                continue;
            }
            let Some(&(j, rj)) = own.owner.get(&r.path) else {
                own.owner.insert(r.path.clone(), (i, ri));
                continue;
            };
            let (a, b) = (&pkgs[j].meta, &p.meta);
            let a_prio = if replaces(a, b) { a.replaces_priority as i64 } else { -1 };
            let b_prio = if replaces(b, a) { b.replaces_priority as i64 } else { -1 };
            if a_prio > b_prio {
                continue;
            }
            if b_prio >= 0 || (!a.info.origin.is_empty() && a.info.origin == b.info.origin) {
                own.owner.insert(r.path.clone(), (i, ri));
                continue;
            }
            if same_entry(&pkgs[j].recs[rj], r) {
                continue;
            }
            conflicts.push(format!("{} and {} both have /{}", a.nv(), b.nv(), r.path_lossy()));
        }
    }
    (own, conflicts)
}

fn is_etc(path: &[u8]) -> bool {
    path.starts_with(b"etc/")
}

/// `/boot` lives on its own FAT32 partition on installed systems, so files
/// there are always copied rather than hard-linked (keeping both upgrades
/// and squashfs unpacks free of cross-device link errors).
fn is_boot(path: &[u8]) -> bool {
    path.starts_with(b"boot/")
}

fn ignore_nonposix(r: io::Result<()>) -> io::Result<()> {
    match r {
        Ok(()) => Ok(()),
        Err(e)
            if matches!(
                e.raw_os_error(),
                Some(libc::EPERM | libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP)
            ) =>
        {
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// True when regular file `dst` has the same content as `r` (comparing
/// against `r.hash`, or against `src` if `r.hash` is empty).
fn same_content(dst: &Path, src: &Path, r: &FileRec) -> bool {
    if let Some(alg) = Alg::from_len(r.hash.len()) {
        return hash_file(dst, alg).map_or(false, |h| h == r.hash);
    }
    match (hash_file(dst, Alg::Sha256), hash_file(src, Alg::Sha256)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Copies regular file `src` (opened with `O_NOFOLLOW`) to `dst` via a
/// temporary file and `rename`. When `sync_file` is set, calls `sync_all`
/// before renaming (important when `dst` is on another filesystem such as
/// `/boot`, since `syncfs(root)` does not flush other mounts). When
/// `tolerate_nonposix` is set, `EPERM`/`EINVAL` from chown/chmod/utimens on
/// FAT32 are ignored.
fn copy_file_atomic(
    src: &Path,
    dst: &Path,
    r: &FileRec,
    chown: bool,
    owner: (u32, u32),
    sync_file: bool,
    tolerate_nonposix: bool,
) -> io::Result<()> {
    let tmp = rootfs::tmp_name(dst);
    let _ = fs::remove_file(&tmp);
    let res = (|| {
        let mut i = sys::open_regular_nofollow(src)?;
        let mut o = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        io::copy(&mut i, &mut o)?;
        if chown {
            let r_ch = fchown(&o, Some(owner.0), Some(owner.1));
            if tolerate_nonposix {
                ignore_nonposix(r_ch)?;
            } else {
                r_ch?;
            }
        }
        let r_perm = o.set_permissions(fs::Permissions::from_mode(r.mode & 0o7777));
        if tolerate_nonposix {
            ignore_nonposix(r_perm)?;
        } else {
            r_perm?;
        }
        let t = UNIX_EPOCH + Duration::from_secs(r.mtime);
        let r_time = o.set_times(FileTimes::new().set_accessed(t).set_modified(t));
        if tolerate_nonposix {
            ignore_nonposix(r_time)?;
        } else {
            r_time?;
        }
        if sync_file {
            o.sync_all()?;
        }
        fs::rename(&tmp, dst)?;
        if sync_file {
            // the rename must reach the disk as well: on another file system
            // (FAT32 /boot) neither syncfs(root) nor the journal covers it, and
            // the old version's files are deleted later in the switch (N-2)
            sync_parent_dir(dst)?;
        }
        Ok(())
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

/// Makes a rename or creation in `p`'s directory durable (an fsync of the
/// directory). A file system that cannot fsync a directory (`EINVAL`) has
/// nothing to sync.
fn sync_parent_dir(p: &Path) -> io::Result<()> {
    let dir = match p.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    match File::open(dir).and_then(|d| d.sync_all()) {
        Err(e) if e.raw_os_error() == Some(libc::EINVAL) => Ok(()),
        r => r,
    }
}

/// Hashes a live file. A final symlink is not followed and only a regular
/// file is read (a FIFO or device in its place is an error, not a hang).
fn hash_file(p: &Path, alg: Alg) -> io::Result<Vec<u8>> {
    hash_open(&mut sys::open_regular_nofollow(p)?, alg)
}

fn hash_open(f: &mut File, alg: Alg) -> io::Result<Vec<u8>> {
    let mut h = Hasher::new(alg);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finish())
}

fn other(m: String) -> io::Error {
    io::Error::new(io::ErrorKind::Other, m)
}

/// A script put off until all packages are placed (its interpreter was
/// not there yet).
struct Deferred {
    key: String,
    name: String,
    version: String,
    dir: PathBuf,
    typ: String,
    args: Vec<String>,
}

/// The interpreter named by a `#!` line.
fn interpreter(body: &[u8]) -> Option<&[u8]> {
    let line = body.strip_prefix(b"#!")?;
    let line = &line[..line.iter().position(|&c| c == b'\n').unwrap_or(line.len())];
    line.split(|&c| c == b' ' || c == b'\t').find(|w| !w.is_empty())
}

struct Switch<'a> {
    root: PathBuf,
    fs: RootFs,
    ids: IdMap,
    j: JournalFile,
    opts: &'a SwitchOptions,
    rep: Report,
    files_done: u64,
    chown: bool,
    dev_checked: bool,
    deferred: Vec<Deferred>,
}

impl<'a> Switch<'a> {
    fn crash(&self, point: &str) {
        if self.opts.crash_at.as_deref() == Some(point) {
            eprintln!("oct: OCTET_CRASH_AT={point}: killed (test hook)");
            sys::kill_self();
        }
    }

    fn err(&mut self, what: String) {
        eprintln!("oct: error: {what}");
        self.rep.errors.push(what);
    }

    fn owner_ids(&self, r: &FileRec) -> (u32, u32) {
        (self.ids.uid(r.user.as_deref()), self.ids.gid(r.group.as_deref()))
    }

    fn file_placed(&mut self) {
        self.rep.placed += 1;
        self.files_done += 1;
        if let Some(c) = &self.opts.crash_at {
            if c.strip_prefix("link:").and_then(|n| n.parse::<u64>().ok()) == Some(self.files_done) {
                eprintln!("oct: OCTET_CRASH_AT={c}: killed (test hook)");
                sys::kill_self();
            }
        }
    }

    /// Hard link `src` (in Programs) to `dst`; nothing to do if `dst` is
    /// already that inode (or, across filesystems, a regular file with the
    /// same content). Falls back to an atomic synced copy on `EXDEV`.
    fn place_link(&mut self, src: &Path, r: &FileRec, dst: &Resolved) -> io::Result<()> {
        match fs::symlink_metadata(&dst.host) {
            Ok(dm) => {
                let sm = fs::symlink_metadata(src)?;
                if dm.dev() == sm.dev() && dm.ino() == sm.ino() {
                    self.rep.unchanged += 1;
                    return Ok(());
                }
                if dm.is_dir() {
                    return Err(other(format!("/{} is a directory", String::from_utf8_lossy(&dst.canon))));
                }
                if dm.dev() != sm.dev() && dm.is_file() && r.is_regular() && same_content(&dst.host, src, r) {
                    self.rep.unchanged += 1;
                    return Ok(());
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => match fs::hard_link(src, &dst.host) {
                Ok(()) => {
                    self.file_placed();
                    return Ok(());
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) if e.raw_os_error() == Some(libc::EXDEV) && r.is_regular() => {
                    return self.copy_cross_dev(src, &dst.host, r);
                }
                Err(e) => return Err(e),
            },
            Err(e) => return Err(e),
        }
        let tmp = rootfs::tmp_name(&dst.host);
        let _ = fs::remove_file(&tmp);
        match fs::hard_link(src, &tmp) {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) && r.is_regular() => {
                return self.copy_cross_dev(src, &dst.host, r);
            }
            Err(e) => return Err(e),
        }
        if let Err(e) = fs::rename(&tmp, &dst.host) {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        self.file_placed();
        Ok(())
    }

    fn place_symlink(&mut self, target: &[u8], r: &FileRec, old: Option<&FileRec>, dst: &Resolved) -> io::Result<()> {
        let mut write_new = false;
        match fs::symlink_metadata(&dst.host) {
            Ok(m) if m.file_type().is_symlink() => {
                let cur = fs::read_link(&dst.host)?;
                if cur.as_os_str().as_bytes() == target {
                    self.rep.unchanged += 1;
                    return Ok(());
                }
                if is_etc(&r.path) {
                    let pristine = matches!(
                        old.map(|o| &o.kind),
                        Some(RecKind::File(FileKind::Symlink(ot))) if cur.as_os_str().as_bytes() == ot.as_slice()
                    );
                    if !pristine {
                        write_new = true;
                    }
                }
            }
            Ok(m) if m.is_dir() => {
                return Err(other(format!("/{} is a directory", String::from_utf8_lossy(&dst.canon))))
            }
            Ok(m) if is_etc(&r.path) => {
                // A regular file or other node where the new version has a
                // symlink: replaced when it is exactly the old version's file
                // (the package turned it into a symlink), kept when the admin
                // changed or replaced it (N-3).
                let pristine = m.file_type().is_file()
                    && old.map_or(false, |o| {
                        o.is_regular()
                            && Alg::from_len(o.hash.len())
                                .map_or(false, |a| hash_file(&dst.host, a).map_or(false, |h| h == o.hash))
                    });
                if !pristine {
                    write_new = true;
                }
            }
            // a new name: made directly when it needs no owner change (a
            // symlink appears atomically with its target; an lchown after it
            // could be lost to a crash)
            Err(e) if e.kind() == io::ErrorKind::NotFound && (!self.chown || self.owner_ids(r) == (0, 0)) => {
                match symlink(OsStr::from_bytes(target), &dst.host) {
                    Ok(()) => {
                        self.file_placed();
                        return Ok(());
                    }
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(e),
                }
            }
            _ => {}
        }
        if write_new {
            let mut n = dst.host.as_os_str().to_os_string();
            n.push(".octet-new");
            let newp = PathBuf::from(n);
            let already = fs::symlink_metadata(&newp).map_or(false, |m| {
                m.file_type().is_symlink()
                    && fs::read_link(&newp).map_or(false, |t| t.as_os_str().as_bytes() == target)
            });
            if !already {
                let tmp = rootfs::tmp_name(&newp);
                let _ = fs::remove_file(&tmp);
                symlink(OsStr::from_bytes(target), &tmp)?;
                if self.chown {
                    let (u, g) = self.owner_ids(r);
                    lchown(&tmp, Some(u), Some(g))?;
                }
                if let Err(e) = fs::rename(&tmp, &newp) {
                    let _ = fs::remove_file(&tmp);
                    return Err(e);
                }
                self.file_placed();
            }
            let msg = format!(
                "/{} was changed locally; kept, new version in /{}.octet-new",
                String::from_utf8_lossy(&dst.canon),
                String::from_utf8_lossy(&dst.canon)
            );
            println!("  {msg}");
            self.rep.new_configs.push(msg);
            return Ok(());
        }
        let tmp = rootfs::tmp_name(&dst.host);
        let _ = fs::remove_file(&tmp);
        symlink(OsStr::from_bytes(target), &tmp)?;
        if self.chown {
            let (u, g) = self.owner_ids(r);
            lchown(&tmp, Some(u), Some(g))?;
        }
        if let Err(e) = fs::rename(&tmp, &dst.host) {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        self.file_placed();
        Ok(())
    }

    /// Copies `src` to `dst` (through a temporary name) with the record's
    /// mode, owner and mtime.
    fn copy_to(&mut self, src: &Path, dst: &Path, r: &FileRec) -> io::Result<()> {
        let owner = self.owner_ids(r);
        copy_file_atomic(src, dst, r, self.chown, owner, false, false)?;
        self.file_placed();
        Ok(())
    }

    /// Copies `src` to `dst` on another filesystem (e.g. FAT32 `/boot`),
    /// syncing the file before rename and tolerating non-POSIX metadata errors.
    fn copy_cross_dev(&mut self, src: &Path, dst: &Path, r: &FileRec) -> io::Result<()> {
        let owner = self.owner_ids(r);
        copy_file_atomic(src, dst, r, self.chown, owner, true, true)?;
        self.file_placed();
        Ok(())
    }

    /// Places a file under `boot/` as a synced copy (never hard-linked).
    fn place_boot(&mut self, src: &Path, r: &FileRec, dst: &Resolved) -> io::Result<()> {
        match fs::symlink_metadata(&dst.host) {
            Ok(dm) if dm.is_dir() => {
                return Err(other(format!("/{} is a directory", String::from_utf8_lossy(&dst.canon))));
            }
            Ok(dm) if dm.is_file() && same_content(&dst.host, src, r) => {
                self.rep.unchanged += 1;
                return Ok(());
            }
            Ok(_) | Err(_) => {}
        }
        self.copy_cross_dev(src, &dst.host, r)
    }

    /// The `etc/` rule: a copy, unless the admin changed the live file.
    fn place_etc(&mut self, src: &Path, r: &FileRec, old: Option<&FileRec>, dst: &Resolved) -> io::Result<()> {
        let alg = Alg::from_len(r.hash.len());
        match fs::symlink_metadata(&dst.host) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => self.copy_to(src, &dst.host, r),
            Err(e) => Err(e),
            Ok(m) if m.is_dir() => Err(other(format!("/{} is a directory", String::from_utf8_lossy(&dst.canon)))),
            Ok(m) if m.is_file() => {
                let live = match alg {
                    Some(a) => Some(hash_file(&dst.host, a)?),
                    None => None,
                };
                if live.is_some() && live.as_deref() == Some(&r.hash[..]) {
                    self.rep.unchanged += 1;
                    return Ok(());
                }
                let pristine = old.map_or(false, |o| o.is_regular() && live.as_deref() == Some(&o.hash[..]));
                if pristine || alg.is_none() {
                    self.copy_to(src, &dst.host, r)
                } else {
                    let mut n = dst.host.as_os_str().to_os_string();
                    n.push(".octet-new");
                    let newp = PathBuf::from(n);
                    let already = alg.map_or(false, |a| hash_file(&newp, a).map_or(false, |h| h == r.hash));
                    if !already {
                        self.copy_to(src, &newp, r)?;
                    }
                    let msg = format!("/{} was changed locally; kept, new version in /{}.octet-new", String::from_utf8_lossy(&dst.canon), String::from_utf8_lossy(&dst.canon));
                    println!("  {msg}");
                    self.rep.new_configs.push(msg);
                    Ok(())
                }
            }
            // a symlink or other node where the package now has a file
            Ok(_) => self.copy_to(src, &dst.host, r),
        }
    }

    fn place(&mut self, p: &PkgState, r: &FileRec, old: Option<&FileRec>) -> io::Result<()> {
        if let RecKind::Dir = r.kind {
            let (u, g) = self.owner_ids(r);
            self.fs.ensure_dir(&r.path, r.mode, if self.chown { u } else { 0 }, if self.chown { g } else { 0 })?;
            return Ok(());
        }
        let (dir, _) = rootfs::split_rel(&r.path);
        self.fs.ensure_dir(dir, 0o755, 0, 0)?;
        let dst = self.fs.entry(&r.path)?;
        let src = p.dir.join(OsStr::from_bytes(&r.path));
        match &r.kind {
            RecKind::File(FileKind::Symlink(t)) => self.place_symlink(t, r, old, &dst),
            RecKind::File(FileKind::Regular) | RecKind::File(FileKind::Hardlink(_)) if is_etc(&r.path) => {
                self.place_etc(&src, r, old, &dst)
            }
            RecKind::File(FileKind::Regular) | RecKind::File(FileKind::Hardlink(_)) if is_boot(&r.path) => {
                self.place_boot(&src, r, &dst)
            }
            // regular files, hard links and device nodes: one inode (EXDEV falls back to copy)
            _ => self.place_link(&src, r, &dst),
        }
    }

    fn script(&mut self, key: &str, p: &PkgState, typ: &str, args: &[String]) {
        self.script_at(key, p.meta.name(), p.meta.version(), &p.dir, typ, args, false)
    }

    /// Like [`Switch::script`], but a script whose `#!` interpreter is not
    /// in the root yet (its package comes later in the order) is put off
    /// until all packages are placed, instead of failing as it would with
    /// apk.
    fn script_deferrable(&mut self, key: &str, p: &PkgState, typ: &str, args: &[String]) {
        self.script_at(key, p.meta.name(), p.meta.version(), &p.dir, typ, args, true)
    }

    #[allow(clippy::too_many_arguments)]
    fn script_at(&mut self, key: &str, name: &str, version: &str, dir: &Path, typ: &str, args: &[String], defer_ok: bool) {
        if self.j.is_done(key) {
            return;
        }
        let Some(body) = db::read_script(dir, typ) else { return };
        if !self.opts.no_scripts {
            if defer_ok {
                if let Some(interp) = interpreter(&body) {
                    let present = self
                        .fs
                        .resolve(interp, true)
                        .map_or(false, |r| fs::symlink_metadata(r.host).map_or(false, |m| m.is_file()));
                    if !present {
                        println!(
                            "  {name}-{version}.{typ}: {} is not installed yet; run after the other packages",
                            String::from_utf8_lossy(interp)
                        );
                        self.deferred.push(Deferred {
                            key: key.to_string(),
                            name: name.to_string(),
                            version: version.to_string(),
                            dir: dir.to_path_buf(),
                            typ: typ.to_string(),
                            args: args.to_vec(),
                        });
                        return;
                    }
                }
            }
            if !self.dev_checked {
                self.dev_checked = true;
                if let Err(e) = ensure_dev_nodes(&mut self.fs) {
                    eprintln!("oct: warning: cannot create device nodes in /dev: {e}");
                }
                self.fs.forget();
            }
            self.rep.scripts += 1;
            if let Err(e) = run_script(&self.root, name, version, typ, &body, args) {
                self.err(format!("{name}-{version}.{typ}: {e}"));
            }
            // the script may have changed the tree (e.g. replaced /var/run
            // by a symlink): directories are resolved afresh from now on
            self.fs.forget();
        }
        // a script that ran is marked durably; one skipped by --no-scripts
        // needs no fsync (losing that mark only repeats the skip)
        if let Err(e) = self.j.mark_with(key, !self.opts.no_scripts) {
            eprintln!("oct: warning: journal: {e}");
        }
        if !self.opts.no_scripts {
            // a script may have added users or groups
            self.ids = IdMap::load(&self.root);
        }
    }

    fn run_deferred(&mut self) {
        for d in std::mem::take(&mut self.deferred) {
            self.script_at(&d.key, &d.name, &d.version, &d.dir, &d.typ, &d.args, false);
        }
    }
}

/// Scripts run in the root without anything mounted, so a script's
/// `2>/dev/null` would create a regular file. Like apk when it cannot
/// bind-mount the host's /dev (`make_device_tree`), make the basic device
/// nodes if they are missing: null, zero, random, urandom, console. A
/// regular file in their place (left by such a redirection) is replaced.
fn ensure_dev_nodes(rfs: &mut RootFs) -> io::Result<()> {
    if !sys::is_root() {
        return Ok(());
    }
    rfs.ensure_dir(b"dev", 0o755, 0, 0)?;
    for (name, mode, major, minor) in
        [("null", 0o666, 1u64, 3u64), ("zero", 0o666, 1, 5), ("random", 0o666, 1, 8), ("urandom", 0o666, 1, 9), ("console", 0o600, 5, 1)]
    {
        let p = rfs.entry(format!("dev/{name}").as_bytes())?;
        match fs::symlink_metadata(&p.host) {
            Ok(m) if m.is_file() => fs::remove_file(&p.host)?,
            Ok(_) => continue,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        sys::make_node(&p.host, 0o020000 | mode, (major << 8) | minor)?;
        fs::set_permissions(&p.host, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// The directory scripts are written to (`var/lib/octet/exec`, mode 0700):
/// made or checked to be a real directory, never followed through a
/// symlink.
fn exec_dir(root: &Path) -> Result<PathBuf> {
    let state = root.join(db::STATE_DIR);
    if !db::real_dir(&state)? {
        return Err(Error::Msg(format!("{} is missing", state.display())));
    }
    let d = state.join("exec");
    match fs::symlink_metadata(&d) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => fs::remove_file(&d)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    match fs::DirBuilder::new().mode(0o700).create(&d) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let f = sys::open_dir_nofollow(&d)?;
    f.set_permissions(fs::Permissions::from_mode(0o700))?;
    Ok(d)
}

/// Runs one package script as apk does (§D9, `apk_ipkg_run_script`): the
/// script file is written into the root and executed after
/// `chroot(ROOT)` with the root as working directory, `argv[0]` its path
/// relative to the root and the versions as arguments, a clean
/// environment (`PATH`, `APK_SCRIPT`, `APK_PACKAGE`) and umask 022.
/// Nothing is mounted into the root. The script inherits the root's lock
/// (see [`sys::set_lock_fd`]).
pub fn run_script(root: &Path, name: &str, version: &str, typ: &str, body: &[u8], args: &[String]) -> Result<()> {
    let exec_dir = exec_dir(root)?;
    let fname = format!("{name}-{version}.{typ}");
    let host = exec_dir.join(&fname);
    match fs::remove_file(&host) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    {
        let mut f = OpenOptions::new().write(true).create_new(true).mode(0o700).open(&host)?;
        io::Write::write_all(&mut f, body)?;
        f.set_permissions(fs::Permissions::from_mode(0o755))?;
    }
    let inside = format!("/{}/exec/{fname}", db::STATE_DIR);
    let arg0 = format!("{}/exec/{fname}", db::STATE_DIR);
    println!("  executing {name}-{version}.{typ}{}", if args.is_empty() { String::new() } else { format!(" {}", args.join(" ")) });
    let prep = sys::ChrootPrep::new(root)?;
    let mut cmd = Command::new(&inside);
    cmd.arg0(&arg0)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("APK_SCRIPT", typ)
        .env("APK_PACKAGE", name)
        .stdin(std::process::Stdio::null());
    // SAFETY: ChrootPrep::apply only makes async-signal-safe calls with
    // strings prepared before the fork.
    unsafe {
        cmd.pre_exec(move || prep.apply());
    }
    let res = cmd.output();
    let _ = fs::remove_file(&host);
    let out = res.map_err(|e| Error::Msg(format!("cannot execute: {e}")))?;
    for l in String::from_utf8_lossy(&out.stdout).lines().chain(String::from_utf8_lossy(&out.stderr).lines()) {
        println!("  * {l}");
    }
    if !out.status.success() {
        return Err(Error::Msg(format!("script exited with {}", out.status)));
    }
    Ok(())
}

fn parent_dir(p: &[u8]) -> Vec<u8> {
    rootfs::split_rel(p).0.to_vec()
}

/// Canonical live paths of the final set's entries (where they exist): a
/// path of an old package may reach the same file through a symlinked
/// directory, and must then not be removed.
fn final_canon(rfs: &mut RootFs, pkgs: &[PkgState]) -> HashSet<Vec<u8>> {
    let mut keep = HashSet::new();
    for p in pkgs {
        for r in &p.recs {
            if let Ok(Some(e)) = rfs.entry_if_dir_exists(&r.path) {
                keep.insert(e.canon);
            }
        }
    }
    keep
}

impl<'a> Switch<'a> {
    /// Removes the entries of an old version (or a removed package) that
    /// no package of the final set owns, and only those that are still
    /// what the old version put there (same inode, content or link target);
    /// anything else was changed locally and is kept. Then its directories
    /// that no package has, if empty.
    fn remove_old(&mut self, o: &PkgState, own: &Ownership, keep: &HashSet<Vec<u8>>, modified: &mut HashSet<Vec<u8>>) {
        let mut dirs: BTreeSet<(usize, Vec<u8>)> = BTreeSet::new();
        for r in &o.recs {
            if r.is_dir() {
                if !own.dirs.contains(&r.path) {
                    dirs.insert((usize::MAX - r.path.len(), r.path.clone()));
                }
                continue;
            }
            if own.owner.contains_key(&r.path) {
                continue;
            }
            let Ok(Some(e)) = self.fs.entry_if_dir_exists(&r.path) else { continue };
            if keep.contains(&e.canon) {
                continue;
            }
            let Ok(m) = fs::symlink_metadata(&e.host) else { continue };
            let ours = match &r.kind {
                _ if m.is_dir() => false,
                RecKind::File(FileKind::Symlink(t)) => {
                    m.file_type().is_symlink()
                        && fs::read_link(&e.host).map_or(false, |l| l.as_os_str().as_bytes() == &t[..])
                }
                _ if m.file_type().is_symlink() => false,
                RecKind::File(FileKind::Regular) | RecKind::File(FileKind::Hardlink(_)) if m.is_file() => {
                    let same_inode = !is_etc(&r.path)
                        && fs::symlink_metadata(o.dir.join(OsStr::from_bytes(&r.path)))
                            .map_or(false, |pm| pm.dev() == m.dev() && pm.ino() == m.ino());
                    same_inode
                        || Alg::from_len(r.hash.len())
                            .map_or(false, |a| hash_file(&e.host, a).map_or(false, |h| h == r.hash))
                }
                RecKind::File(FileKind::Regular) | RecKind::File(FileKind::Hardlink(_)) => false,
                _ => !m.is_file(), // device node or fifo
            };
            if !ours {
                if !m.is_dir() {
                    let msg = format!("/{} was changed locally; kept", r.path_lossy());
                    println!("  {msg}");
                    self.rep.warnings.push(msg);
                }
                continue;
            }
            match fs::remove_file(&e.host) {
                Ok(()) => self.rep.removed += 1,
                Err(err) => self.err(format!("{}: removing /{}: {err}", o.meta.nv(), r.path_lossy())),
            }
        }
        for (_, d) in dirs {
            let Ok(Some(e)) = self.fs.entry_if_dir_exists(&d) else { continue };
            if keep.contains(&e.canon) {
                continue;
            }
            if let Ok(m) = fs::symlink_metadata(&e.host) {
                if m.is_dir() && fs::remove_dir(&e.host).is_ok() {
                    modified.insert(parent_dir(&d));
                }
            }
        }
        self.fs.forget();
    }

    fn sync(&self) -> Result<()> {
        sys::sync_fs(&File::open(&self.root)?)?;
        Ok(())
    }
}

/// Runs (or finishes) the switch described by the journal. `installed`
/// is the database as it is on disk now (some `Current` links may already
/// point at new versions after an interruption).
///
/// Scripts run in apk's order (`apk_db_install_pkg`): per package,
/// dependencies first, the pre-install/pre-upgrade script, then its files
/// (and the removal of its old version's leftovers), then the
/// post-install/post-upgrade script; removed packages after that
/// (pre-deinstall, files, post-deinstall); triggers last. One difference:
/// a pre/post script whose `#!` interpreter is not in the root yet (e.g.
/// bash's post-install before busybox provides /bin/sh, which fails under
/// apk) waits until all packages are placed. Once a package's files are
/// placed this is marked in the journal (`placed:NAME`, made durable with
/// a syncfs before a post script runs), so that a recovery does not place
/// them again over what its scripts did; every script run is marked too.
pub fn switch(root: &Path, installed: &BTreeMap<String, Installed>, j: JournalFile, opts: &SwitchOptions) -> Result<Report> {
    let plan = j.plan.clone();
    let programs = Db::programs(root);
    let changed: HashMap<&str, &Op> = plan.ops.iter().map(|o| (o.name(), o)).collect();

    // the final set: unchanged packages first, then new ones in order
    let mut pkgs: Vec<PkgState> = Vec::new();
    for (name, ip) in installed {
        if !changed.contains_key(name.as_str()) {
            pkgs.push(PkgState::load(&ip.dir, false)?);
        }
    }
    let first_new = pkgs.len();
    for op in &plan.ops {
        if let Some(v) = op.new_version() {
            pkgs.push(PkgState::load(&programs.join(op.name()).join(v), true)?);
        }
    }
    // old versions (absent if already deleted)
    let mut olds: HashMap<String, PkgState> = HashMap::new();
    for op in &plan.ops {
        if let Some(v) = op.old_version() {
            let d = programs.join(op.name()).join(v);
            if d.join(db::META_DIR).exists() {
                olds.insert(op.name().to_string(), PkgState::load(&d, true)?);
            }
        }
    }
    let (own, conflicts) = ownership(&pkgs);
    for c in conflicts {
        eprintln!("oct: warning: {c}");
    }

    let mut s = Switch {
        root: root.to_path_buf(),
        fs: RootFs::new(root),
        ids: IdMap::load(root),
        j,
        opts,
        rep: Report::default(),
        files_done: 0,
        chown: sys::is_root(),
        dev_checked: false,
        deferred: Vec::new(),
    };
    s.crash("start");

    // directories whose content changes (for triggers): as apk marks them
    // (apk_db_migrate_files, apk_db_purge_pkg), every directory of a new or
    // old version, and the directory of each of their files
    let mut modified: HashSet<Vec<u8>> = HashSet::new();
    for p in pkgs.iter().skip(first_new).chain(olds.values()) {
        for r in &p.recs {
            modified.insert(if r.is_dir() { r.path.clone() } else { parent_dir(&r.path) });
        }
    }
    let mut keep: Option<HashSet<Vec<u8>>> = None;

    // 1. installed and upgraded packages, dependencies first
    for (i, p) in pkgs.iter().enumerate().skip(first_new) {
        let name = p.meta.name().to_string();
        let (pre, post, args) = match changed[name.as_str()] {
            Op::Upgrade { old, new, .. } => ("pre-upgrade", "post-upgrade", vec![new.clone(), old.clone()]),
            _ => ("pre-install", "post-install", vec![p.meta.version().to_string()]),
        };
        let placed = format!("placed:{name}");
        let post_key = format!("{post}:{name}");
        s.script_deferrable(&format!("{pre}:{name}"), p, pre, &args);
        if !s.j.is_done(&placed) {
            unpack::fix_owners(&p.dir, &p.recs, &s.ids);
            let old_map: HashMap<&[u8], &FileRec> = olds
                .get(&name)
                .map(|o| o.recs.iter().map(|r| (r.path.as_slice(), r)).collect())
                .unwrap_or_default();
            for (ri, r) in p.recs.iter().enumerate() {
                if !r.is_dir() && own.owner.get(&r.path) != Some(&(i, ri)) {
                    continue; // another package owns this path
                }
                let old = old_map.get(r.path.as_slice()).copied();
                if let Err(e) = s.place(p, r, old) {
                    s.err(format!("{}: /{}: {e}", p.meta.nv(), r.path_lossy()));
                }
            }
            s.crash(&format!("placed:{name}"));
            if let Some(o) = olds.get(&name) {
                if keep.is_none() {
                    keep = Some(final_canon(&mut s.fs, &pkgs));
                }
                s.remove_old(o, &own, keep.as_ref().unwrap(), &mut modified);
            }
            db::set_current(root, &name, p.meta.version())?;
            let runs_post = !opts.no_scripts && !s.j.is_done(&post_key) && db::read_script(&p.dir, post).is_some();
            if runs_post {
                s.sync()?;
            }
            s.j.mark_with(&placed, false)?;
        }
        // the old version is no longer in the live tree: close its
        // directory, so that its set-id programs cannot be run through
        // Programs/<name>/<old>/... until it is deleted
        if let Some(o) = olds.get(&name) {
            lock_old_version(&o.dir);
        }
        s.script_deferrable(&post_key, p, post, &args);
    }
    s.run_deferred();
    s.crash("placed");

    // 2. removed packages
    for op in &plan.ops {
        let Op::Remove { name, old } = op else { continue };
        let key = format!("removed:{name}");
        let o = olds.get(name);
        if !s.j.is_done(&key) {
            if let Some(o) = o {
                s.script(&format!("pre-deinstall:{name}"), o, "pre-deinstall", &[old.clone()]);
                if keep.is_none() {
                    keep = Some(final_canon(&mut s.fs, &pkgs));
                }
                s.remove_old(o, &own, keep.as_ref().unwrap(), &mut modified);
            }
            match fs::remove_file(programs.join(name).join(db::CURRENT)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            if o.map_or(false, |o| db::read_script(&o.dir, "post-deinstall").is_some()) && !opts.no_scripts {
                s.sync()?;
            }
            s.j.mark_with(&key, false)?;
        }
        if let Some(o) = o {
            lock_old_version(&o.dir);
        }
        if let Some(o) = o {
            s.script(&format!("post-deinstall:{name}"), o, "post-deinstall", &[old.clone()]);
        }
    }
    s.crash("removed");

    // 3. the world
    let world: Vec<crate::adb::Dependency> =
        plan.world.iter().filter_map(|w| crate::adb::Dependency::parse(w).ok()).collect();
    db::write_atomic(&Db::state(root).join("world"), db::format_world(&world).as_bytes())?;
    s.crash("world");

    // 4. owners once more, now that all files (etc/passwd too) are in place
    s.ids = IdMap::load(root);
    for p in pkgs.iter().skip(first_new) {
        unpack::fix_owners(&p.dir, &p.recs, &s.ids);
        for r in p.recs.iter().filter(|r| is_etc(&r.path) && r.is_regular()) {
            let special = |x: &Option<String>| x.as_deref().map_or(false, |x| x != "root");
            if !special(&r.user) && !special(&r.group) {
                continue;
            }
            if let Ok(Some(e)) = s.fs.entry_if_dir_exists(&r.path) {
                // hashed and changed through one descriptor of the file
                // itself (no symlink followed, no FIFO read)
                let Ok(mut f) = sys::open_regular_nofollow(&e.host) else { continue };
                let ours = Alg::from_len(r.hash.len())
                    .map_or(false, |a| hash_open(&mut f, a).ok().as_ref() == Some(&r.hash));
                if ours && s.chown {
                    let (u, g) = s.owner_ids(r);
                    let _ = fchown(&f, Some(u), Some(g));
                    let _ = f.set_permissions(fs::Permissions::from_mode(r.mode & 0o7777));
                }
            }
        }
    }

    // 5. triggers (§D9, apk's fire_triggers): a package's trigger script
    // runs when it was just installed/upgraded or a directory matching one
    // of its globs was modified; it gets the matching directories
    // apk's installed directories: every directory of a package and all
    // their parents
    let mut installed_dirs: BTreeSet<Vec<u8>> = BTreeSet::new();
    // the root directory itself ("/") is one of them, as in apk
    installed_dirs.insert(Vec::new());
    for p in &pkgs {
        for r in &p.recs {
            let mut d = if r.is_dir() { r.path.clone() } else { parent_dir(&r.path) };
            while !d.is_empty() && installed_dirs.insert(d.clone()) {
                d = parent_dir(&d);
            }
        }
    }
    for c in &s.fs.created_dirs {
        modified.insert(parent_dir(c));
    }
    for p in &pkgs {
        if p.meta.triggers.is_empty() || db::read_script(&p.dir, "trigger").is_none() {
            continue;
        }
        let mut pending = false;
        let mut args: Vec<String> = Vec::new();
        for d in &installed_dirs {
            let is_mod = modified.contains(d);
            if !p.changed && !is_mod {
                continue;
            }
            let rooted = [b"/".as_slice(), d].concat();
            for t in &p.meta.triggers {
                let (only_changed, g) = match t.strip_prefix('+') {
                    Some(g) => (true, g),
                    None => (false, t.as_str()),
                };
                if !g.starts_with('/') || !sys::glob_match(g.as_bytes(), &rooted) {
                    continue;
                }
                pending = true;
                if !only_changed || is_mod {
                    args.push(String::from_utf8_lossy(&rooted).into_owned());
                }
                break;
            }
        }
        if pending {
            let key = format!("trigger:{}", p.meta.name());
            s.script(&key, p, "trigger", &args);
        }
    }
    s.crash("triggers");

    // 6. durable, then forget the journal and the old versions
    s.sync()?;
    let Switch { j, rep, .. } = s;
    j.remove(root)?;
    for op in &plan.ops {
        // only through real directories (Programs/<name> a symlink is
        // corruption, never followed)
        match op {
            Op::Upgrade { name, old, .. } => {
                let d = db::checked_pkg_parent(root, name)?.join(old);
                match fs::symlink_metadata(&d) {
                    Ok(m) if m.is_dir() => fs::remove_dir_all(&d)?,
                    Ok(_) => fs::remove_file(&d)?,
                    Err(_) => {}
                }
            }
            Op::Remove { name, .. } => {
                let d = db::checked_pkg_parent(root, name)?;
                if fs::symlink_metadata(d.join(db::CURRENT)).is_err() && d.exists() {
                    fs::remove_dir_all(&d)?;
                }
            }
            Op::Install { .. } => {}
        }
    }
    Ok(rep)
}

/// Makes an old version directory accessible to root only (mode 0700).
/// Its files are no longer linked into the live tree once its package was
/// switched; this keeps set-id programs among them from being run by other
/// users through their Programs path while the directory still exists.
fn lock_old_version(dir: &Path) {
    if let Ok(d) = sys::open_dir_nofollow(dir) {
        let _ = d.set_permissions(fs::Permissions::from_mode(0o700));
    }
}

/// Directories inside a root searched for commit hooks, in precedence order
/// (`etc/` admin overrides before `usr/lib/` package hooks, matching apk).
const COMMIT_HOOK_DIRS: &[&[u8]] = &[
    b"etc/octet/commit_hooks.d",
    b"etc/apk/commit_hooks.d",
    b"usr/lib/octet/commit_hooks.d",
    b"lib/octet/commit_hooks.d",
    b"usr/lib/apk/commit_hooks.d",
    b"lib/apk/commit_hooks.d",
];

/// Discovers executable commit hooks inside `root`, returning
/// `(name, inside_path)` pairs sorted by filename and deduplicated by
/// `(dev, ino)` (so merged `/lib -> usr/lib` never runs a hook twice).
pub fn discover_commit_hooks(root: &Path) -> Vec<(String, String)> {
    let rfs = RootFs::new(root);
    let mut seen_ino: HashSet<(u64, u64)> = HashSet::new();
    let mut seen_name: HashSet<String> = HashSet::new();
    let mut hooks: Vec<(String, String)> = Vec::new();
    for dir in COMMIT_HOOK_DIRS {
        let Ok(res) = rfs.resolve(dir, true) else { continue };
        let Ok(rd) = fs::read_dir(&res.host) else { continue };
        for e in rd.flatten() {
            let fname = e.file_name();
            let fb = fname.as_bytes();
            if fb.is_empty() || fb.starts_with(b".") {
                continue;
            }
            let Ok(name) = std::str::from_utf8(fb) else { continue };
            let mut rel = res.canon.clone();
            if !rel.is_empty() {
                rel.push(b'/');
            }
            rel.extend_from_slice(fb);
            let Ok(fres) = rfs.resolve(&rel, true) else { continue };
            let Ok(md) = fs::symlink_metadata(&fres.host) else { continue };
            if !md.is_file() || (md.mode() & 0o111) == 0 {
                continue;
            }
            if !seen_ino.insert((md.dev(), md.ino())) {
                continue;
            }
            if !seen_name.insert(name.to_string()) {
                continue;
            }
            let Ok(inside_rel) = std::str::from_utf8(&fres.canon) else { continue };
            hooks.push((name.to_string(), format!("/{inside_rel}")));
        }
    }
    hooks.sort_by(|a, b| a.0.cmp(&b.0));
    hooks
}

/// Runs commit hooks inside `root` for `stage` (`"pre-commit"` or
/// `"post-commit"`), matching apk's `commit_hooks.d` contract:
/// - `pre-commit`: a failing hook aborts immediately and returns an error.
/// - `post-commit`: all hooks run; failures are printed as warnings and do
///   not fail an already-committed transaction.
pub fn run_commit_hooks(root: &Path, stage: &str) -> Result<()> {
    let hooks = discover_commit_hooks(root);
    if hooks.is_empty() {
        return Ok(());
    }
    let is_pre = stage == "pre-commit";
    let snap_text = std::env::var("VALENCE_SNAPSHOT_TEXT").ok();
    for (name, inside) in hooks {
        println!("  executing hook {name} {stage}");
        let prep = sys::ChrootPrep::new_no_lock(root)?;
        let mut cmd = Command::new(&inside);
        cmd.arg(stage)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .env("APK_SCRIPT", stage)
            .stdin(std::process::Stdio::null());
        if let Some(ref t) = snap_text {
            cmd.env("VALENCE_SNAPSHOT_TEXT", t);
        }
        // SAFETY: ChrootPrep::apply only makes async-signal-safe calls with
        // strings prepared before the fork.
        unsafe {
            cmd.pre_exec(move || prep.apply());
        }
        let out = match cmd.output() {
            Ok(o) => o,
            Err(e) => {
                let msg = format!("commit hook {name} ({stage}): cannot execute: {e}");
                if is_pre {
                    return Err(Error::Msg(msg));
                }
                eprintln!("oct: warning: {msg}");
                continue;
            }
        };
        for l in String::from_utf8_lossy(&out.stdout)
            .lines()
            .chain(String::from_utf8_lossy(&out.stderr).lines())
        {
            println!("  * {l}");
        }
        if !out.status.success() {
            let msg = format!("commit hook {name} ({stage}) exited with {}", out.status);
            if is_pre {
                return Err(Error::Msg(msg));
            }
            eprintln!("oct: warning: {msg}");
        }
    }
    Ok(())
}

/// Summary of a `oct fix` pass.
#[derive(Debug, Default)]
pub struct RepairReport {
    pub checked: usize,
    pub ok: usize,
    pub repaired: usize,
    pub errors: Vec<String>,
}

/// Verifies (and unless `simulate`, repairs) live-tree entries for the
/// packages at `target_indices` inside `pkgs` against their copies in
/// `Programs/<name>/<version>/`.
///
/// - Regular files outside `etc/` and `boot/`: verifies the `Programs` copy
///   matches its recorded hash (detecting in-place corruption), then checks
///   `(dev, ino)` (or content hash across filesystems) and atomically relinks
///   (or copies on `EXDEV`) when missing or drifted.
/// - Files under `boot/`: verified by content hash and restored as synced
///   copies (never hard-linked, since `/boot` is FAT32).
/// - Files and symlinks under `etc/`: restored only when missing, preserving
///   admin edits and replacements.
/// - Symlinks outside `etc/`: restored when missing or pointing elsewhere.
/// - Directories: created when missing (following symlinked directory paths).
pub fn repair(
    root: &Path,
    pkgs: &[PkgState],
    target_indices: &HashSet<usize>,
    simulate: bool,
) -> Result<RepairReport> {
    let (own, _) = ownership(pkgs);
    let ids = IdMap::load(root);
    let chown = sys::is_root();
    let mut rfs = RootFs::new(root);
    let mut rep = RepairReport::default();

    let owner_ids = |r: &FileRec| -> (u32, u32) { (ids.uid(r.user.as_deref()), ids.gid(r.group.as_deref())) };

    let ensure_parents = |rfs: &mut RootFs, p: &[u8]| -> io::Result<()> {
        let par = parent_dir(p);
        if !par.is_empty() {
            rfs.ensure_dir(&par, 0o755, 0, 0)?;
        }
        Ok(())
    };

    for (idx, p) in pkgs.iter().enumerate() {
        if !target_indices.contains(&idx) {
            continue;
        }
        for (ri, r) in p.recs.iter().enumerate() {
            if r.is_dir() {
                rep.checked += 1;
                let dst = match rfs.resolve(&r.path, true) {
                    Ok(d) => d,
                    Err(e) => {
                        rep.errors.push(format!("{}: /{}: {e}", p.meta.nv(), r.path_lossy()));
                        continue;
                    }
                };
                if db::is_reserved(&dst.canon) {
                    continue;
                }
                if fs::symlink_metadata(&dst.host).map_or(false, |m| m.is_dir()) {
                    rep.ok += 1;
                    continue;
                }
                if simulate {
                    println!("  would restore dir /{}", r.path_lossy());
                    rep.repaired += 1;
                } else {
                    let (u, g) = owner_ids(r);
                    match rfs.ensure_dir(&r.path, r.mode, u, g) {
                        Ok(_) => {
                            println!("  repaired dir /{}", r.path_lossy());
                            rep.repaired += 1;
                        }
                        Err(e) => rep.errors.push(format!("{}: /{}: {e}", p.meta.nv(), r.path_lossy())),
                    }
                }
                continue;
            }

            if own.owner.get(&r.path) != Some(&(idx, ri)) {
                continue;
            }
            rep.checked += 1;
            let dst = match rfs.resolve(&r.path, false) {
                Ok(d) => d,
                Err(e) => {
                    rep.errors.push(format!("{}: /{}: {e}", p.meta.nv(), r.path_lossy()));
                    continue;
                }
            };
            if db::is_reserved(&dst.canon) {
                rep.errors.push(format!("{}: /{}: path resolves into octet's own directory", p.meta.nv(), r.path_lossy()));
                continue;
            }

            match &r.kind {
                RecKind::File(FileKind::Symlink(target)) => {
                    let live_md = fs::symlink_metadata(&dst.host);
                    let intact = live_md.as_ref().map_or(false, |m| {
                        m.file_type().is_symlink()
                            && fs::read_link(&dst.host)
                                .map_or(false, |t| t.as_os_str().as_bytes() == target.as_slice())
                    });
                    if intact {
                        rep.ok += 1;
                        continue;
                    }
                    if is_etc(&r.path) && live_md.is_ok() {
                        // Preserve admin-modified symlinks or files under etc/ (V-1).
                        println!("  kept locally changed /{}", r.path_lossy());
                        rep.ok += 1;
                        continue;
                    }
                    if simulate {
                        println!("  would restore symlink /{}", r.path_lossy());
                        rep.repaired += 1;
                        continue;
                    }
                    let res = (|| -> io::Result<()> {
                        ensure_parents(&mut rfs, &r.path)?;
                        let dst = rfs.resolve(&r.path, false)?;
                        let tmp = rootfs::tmp_name(&dst.host);
                        let _ = fs::remove_file(&tmp);
                        symlink(OsStr::from_bytes(target), &tmp)?;
                        if chown {
                            let (u, g) = owner_ids(r);
                            lchown(&tmp, Some(u), Some(g))?;
                        }
                        if let Err(e) = fs::rename(&tmp, &dst.host) {
                            let _ = fs::remove_file(&tmp);
                            return Err(e);
                        }
                        Ok(())
                    })();
                    match res {
                        Ok(()) => {
                            println!("  repaired symlink /{}", r.path_lossy());
                            rep.repaired += 1;
                        }
                        Err(e) => rep.errors.push(format!("{}: /{}: {e}", p.meta.nv(), r.path_lossy())),
                    }
                }
                RecKind::File(FileKind::Regular | FileKind::Hardlink(_)) if is_etc(&r.path) => {
                    if fs::symlink_metadata(&dst.host).is_ok() {
                        rep.ok += 1;
                        continue;
                    }
                    let src = p.dir.join(OsStr::from_bytes(&r.path));
                    if !fs::symlink_metadata(&src).map_or(false, |m| m.is_file()) {
                        rep.errors.push(format!("{}: /{}: missing in Programs", p.meta.nv(), r.path_lossy()));
                        continue;
                    }
                    if let Some(alg) = Alg::from_len(r.hash.len()) {
                        if hash_file(&src, alg).map_or(true, |h| h != r.hash) {
                            rep.errors.push(format!("{}: /{}: corrupt in Programs (hash mismatch)", p.meta.nv(), r.path_lossy()));
                            continue;
                        }
                    }
                    if simulate {
                        println!("  would restore etc /{}", r.path_lossy());
                        rep.repaired += 1;
                        continue;
                    }
                    let owner = owner_ids(r);
                    let res = (|| -> io::Result<()> {
                        ensure_parents(&mut rfs, &r.path)?;
                        let dst = rfs.resolve(&r.path, false)?;
                        copy_file_atomic(&src, &dst.host, r, chown, owner, false, false)
                    })();
                    match res {
                        Ok(()) => {
                            println!("  repaired etc /{}", r.path_lossy());
                            rep.repaired += 1;
                        }
                        Err(e) => rep.errors.push(format!("{}: /{}: {e}", p.meta.nv(), r.path_lossy())),
                    }
                }
                RecKind::File(FileKind::Regular | FileKind::Hardlink(_)) if is_boot(&r.path) => {
                    let src = p.dir.join(OsStr::from_bytes(&r.path));
                    if !fs::symlink_metadata(&src).map_or(false, |m| m.is_file()) {
                        rep.errors.push(format!("{}: /{}: missing in Programs", p.meta.nv(), r.path_lossy()));
                        continue;
                    }
                    if let Some(alg) = Alg::from_len(r.hash.len()) {
                        if hash_file(&src, alg).map_or(true, |h| h != r.hash) {
                            rep.errors.push(format!("{}: /{}: corrupt in Programs (hash mismatch)", p.meta.nv(), r.path_lossy()));
                            continue;
                        }
                    }
                    let intact = fs::symlink_metadata(&dst.host)
                        .map_or(false, |dm| dm.is_file() && same_content(&dst.host, &src, r));
                    if intact {
                        rep.ok += 1;
                        continue;
                    }
                    if simulate {
                        println!("  would copy /{}", r.path_lossy());
                        rep.repaired += 1;
                        continue;
                    }
                    let owner = owner_ids(r);
                    let res = (|| -> io::Result<()> {
                        ensure_parents(&mut rfs, &r.path)?;
                        let dst = rfs.resolve(&r.path, false)?;
                        copy_file_atomic(&src, &dst.host, r, chown, owner, true, true)
                    })();
                    match res {
                        Ok(()) => {
                            println!("  repaired /{}", r.path_lossy());
                            rep.repaired += 1;
                        }
                        Err(e) => rep.errors.push(format!("{}: /{}: {e}", p.meta.nv(), r.path_lossy())),
                    }
                }
                RecKind::File(_) => {
                    let src = p.dir.join(OsStr::from_bytes(&r.path));
                    let sm = match fs::symlink_metadata(&src) {
                        Ok(m) => m,
                        Err(e) => {
                            rep.errors.push(format!("{}: /{}: missing in Programs ({e})", p.meta.nv(), r.path_lossy()));
                            continue;
                        }
                    };
                    if r.is_regular() {
                        if let Some(alg) = Alg::from_len(r.hash.len()) {
                            if hash_file(&src, alg).map_or(true, |h| h != r.hash) {
                                rep.errors.push(format!("{}: /{}: corrupt in Programs (hash mismatch)", p.meta.nv(), r.path_lossy()));
                                continue;
                            }
                        }
                    }
                    let intact = fs::symlink_metadata(&dst.host).map_or(false, |dm| {
                        (dm.dev() == sm.dev() && dm.ino() == sm.ino())
                            || (dm.dev() != sm.dev() && dm.is_file() && r.is_regular() && same_content(&dst.host, &src, r))
                    });
                    if intact {
                        rep.ok += 1;
                        continue;
                    }
                    if simulate {
                        println!("  would relink /{}", r.path_lossy());
                        rep.repaired += 1;
                        continue;
                    }
                    let owner = owner_ids(r);
                    let res = (|| -> io::Result<()> {
                        ensure_parents(&mut rfs, &r.path)?;
                        let dst = rfs.resolve(&r.path, false)?;
                        let tmp = rootfs::tmp_name(&dst.host);
                        let _ = fs::remove_file(&tmp);
                        match fs::hard_link(&src, &tmp) {
                            Ok(()) => {
                                if let Err(e) = fs::rename(&tmp, &dst.host) {
                                    let _ = fs::remove_file(&tmp);
                                    return Err(e);
                                }
                                Ok(())
                            }
                            Err(e) if e.raw_os_error() == Some(libc::EXDEV) && r.is_regular() => {
                                copy_file_atomic(&src, &dst.host, r, chown, owner, true, true)
                            }
                            Err(e) => Err(e),
                        }
                    })();
                    match res {
                        Ok(()) => {
                            println!("  repaired /{}", r.path_lossy());
                            rep.repaired += 1;
                        }
                        Err(e) => rep.errors.push(format!("{}: /{}: {e}", p.meta.nv(), r.path_lossy())),
                    }
                }
                RecKind::Dir => {}
            }
        }
    }

    if !simulate && rep.repaired > 0 {
        sys::sync_fs(&File::open(root)?)?;
    }
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_hooks_discover_dedup_and_order() {
        let dir = std::env::temp_dir().join(format!("octet-hooks-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("usr/lib/apk/commit_hooks.d")).unwrap();
        fs::create_dir_all(dir.join("etc/octet/commit_hooks.d")).unwrap();
        // merged /usr: lib -> usr/lib
        symlink("usr/lib", dir.join("lib")).unwrap();

        // executable hook in usr/lib/apk/commit_hooks.d
        let h1 = dir.join("usr/lib/apk/commit_hooks.d/20-snap");
        fs::write(&h1, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&h1, fs::Permissions::from_mode(0o755)).unwrap();

        // non-executable file should be skipped
        let h_noexec = dir.join("usr/lib/apk/commit_hooks.d/15-disabled");
        fs::write(&h_noexec, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&h_noexec, fs::Permissions::from_mode(0o644)).unwrap();

        // dotfile should be skipped
        let h_dot = dir.join("usr/lib/apk/commit_hooks.d/.hidden");
        fs::write(&h_dot, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&h_dot, fs::Permissions::from_mode(0o755)).unwrap();

        // higher-precedence hook in etc/octet/commit_hooks.d
        let h2 = dir.join("etc/octet/commit_hooks.d/10-first");
        fs::write(&h2, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&h2, fs::Permissions::from_mode(0o755)).unwrap();

        // override of same name in etc/octet/commit_hooks.d
        let h_override = dir.join("etc/octet/commit_hooks.d/20-snap");
        fs::write(&h_override, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&h_override, fs::Permissions::from_mode(0o755)).unwrap();

        let hooks = discover_commit_hooks(&dir);
        assert_eq!(
            hooks,
            vec![
                ("10-first".into(), "/etc/octet/commit_hooks.d/10-first".into()),
                ("20-snap".into(), "/etc/octet/commit_hooks.d/20-snap".into()),
            ]
        );

        let _ = fs::remove_dir_all(&dir);
    }
}


