//! Paths inside a managed root, resolved the way the kernel would resolve
//! them after `chroot(ROOT)`.
//!
//! catalyst works on the root from outside. A path such as `bin/sh` in a root
//! where `bin` is a symlink must lead to `ROOT/usr/bin/sh`, and an absolute
//! symlink (`lib -> /usr/lib`) or a `..` must never lead out of the root
//! into the host. [`RootFs::resolve`] walks the components itself: it
//! follows symlinks with `ROOT` as `/`, clamps `..` at the root and returns
//! a host path whose directory part contains no symlinks.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{fchown, DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::{db, sys};

const MAX_SYMLINKS: usize = 40;

/// A resolved path: the host path, and the canonical root-relative path
/// (no symlinks in its directory part, no `.`/`..`).
#[derive(Debug, Clone)]
pub struct Resolved {
    pub host: PathBuf,
    pub canon: Vec<u8>,
}

pub struct RootFs {
    root: PathBuf,
    /// Root-relative directory path as written by a package -> its
    /// canonical form, for directories known to exist as real directories.
    dir_cache: HashMap<Vec<u8>, Vec<u8>>,
    /// Canonical directories created by this RootFs (for triggers).
    pub created_dirs: HashSet<Vec<u8>>,
    /// The device of catalyst's state directory (looked up once).
    home_dev: Option<Option<u64>>,
    /// A directory resolved so far is on another device than catalyst's
    /// state directory (a mounted `/boot`, a separate `/var/log`): what is
    /// done there is not ordered against the journal by the file system.
    pub off_device: bool,
    /// While set, [`RootFs::ensure_dir`] makes no directory once
    /// `off_device` is set and returns [`OffDevice`] instead (a light
    /// switch then puts its journal's name on disk first).
    pub hold: bool,
    /// One open directory of each other file system met (`None`: it could
    /// not be opened), for [`RootFs::sync_others`].
    others: Vec<(u64, Option<fs::File>)>,
    /// The directories of `dir_cache` that are on another device.
    off_dirs: HashSet<Vec<u8>>,
    /// A directory on another device was resolved, or made, since
    /// [`RootFs::sync_others`] last ran: something may have been done there.
    pub off_dirty: bool,
}

/// The error of [`RootFs::ensure_dir`] under [`RootFs::hold`].
#[derive(Debug)]
pub struct OffDevice;

impl std::fmt::Display for OffDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a directory on another file system than catalyst's state")
    }
}

impl std::error::Error for OffDevice {}

pub fn is_off_device(e: &io::Error) -> bool {
    e.get_ref().map_or(false, |r| r.is::<OffDevice>())
}

fn components(p: &[u8]) -> impl DoubleEndedIterator<Item = &[u8]> {
    p.split(|&c| c == b'/').filter(|c| !c.is_empty())
}

pub fn join_rel(a: &[u8], b: &[u8]) -> Vec<u8> {
    if a.is_empty() {
        return b.to_vec();
    }
    let mut v = Vec::with_capacity(a.len() + 1 + b.len());
    v.extend_from_slice(a);
    v.push(b'/');
    v.extend_from_slice(b);
    v
}

/// (directory part, base name) of a root-relative path.
pub fn split_rel(p: &[u8]) -> (&[u8], &[u8]) {
    match p.iter().rposition(|&c| c == b'/') {
        Some(i) => (&p[..i], &p[i + 1..]),
        None => (&p[..0], p),
    }
}

impl RootFs {
    pub fn new(root: &Path) -> RootFs {
        RootFs {
            root: root.to_path_buf(),
            dir_cache: HashMap::new(),
            created_dirs: HashSet::new(),
            home_dev: None,
            off_device: false,
            hold: false,
            others: Vec::new(),
            off_dirs: HashSet::new(),
            off_dirty: false,
        }
    }

    /// Notes the device of a directory in which something is, or may be,
    /// done (`host` is its path). Whether it is another device than the
    /// state directory's.
    fn note_dev(&mut self, dev: u64, host: &Path) -> bool {
        let root = &self.root;
        let home = *self.home_dev.get_or_insert_with(|| fs::metadata(root.join(db::STATE_DIR)).ok().map(|m| m.dev()));
        if home == Some(dev) {
            return false;
        }
        self.off_device = true;
        self.off_dirty = true;
        if !self.others.iter().any(|(d, _)| *d == dev) {
            self.others.push((dev, fs::File::open(host).ok()));
        }
        true
    }

    /// Notes the file system of the directory at `host` (not one of a
    /// package's entries: the root itself, `Programs`, a place a script
    /// writes to). Whether it is another one than the state directory's.
    pub fn note_dir(&mut self, host: &Path) -> bool {
        match fs::metadata(host) {
            Ok(m) if m.is_dir() => self.note_dev(m.dev(), host),
            _ => false,
        }
    }

    /// `syncfs` of every other file system noted so far: what was done
    /// there is on disk when this returns. (The file system of the state
    /// directory, with the journal, is not among them.)
    pub fn sync_others(&mut self) -> io::Result<usize> {
        let mut all = false;
        for (_, f) in &self.others {
            match f {
                Some(f) => sys::sync_fs(f)?,
                None => all = true,
            }
        }
        if all {
            // one of them could not be opened: every file system, then
            unsafe { libc::sync() };
        }
        self.off_dirty = false;
        Ok(self.others.len())
    }

    /// Something may have been done on the other file systems by someone
    /// else (a script).
    pub fn touch_others(&mut self) {
        if !self.others.is_empty() {
            self.off_dirty = true;
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn host(&self, canon: &[u8]) -> PathBuf {
        if canon.is_empty() {
            self.root.clone()
        } else {
            self.root.join(OsStr::from_bytes(canon))
        }
    }

    /// Resolves a root-relative path. Symlinks in every component but the
    /// last are followed (inside the root); the last one only with
    /// `follow_last`. Missing components are kept literally.
    pub fn resolve(&self, rel: &[u8], follow_last: bool) -> io::Result<Resolved> {
        let mut stack: Vec<Vec<u8>> = Vec::new();
        let mut pending: Vec<Vec<u8>> = components(rel).rev().map(|c| c.to_vec()).collect();
        let mut links = 0;
        let mut missing = false;
        while let Some(c) = pending.pop() {
            if c == b"." {
                continue;
            }
            if c == b".." {
                stack.pop();
                continue;
            }
            if !missing {
                let mut canon = stack.join(&b'/');
                if !canon.is_empty() {
                    canon.push(b'/');
                }
                canon.extend_from_slice(&c);
                match fs::symlink_metadata(self.host(&canon)) {
                    Ok(m) if m.file_type().is_symlink() && (!pending.is_empty() || follow_last) => {
                        links += 1;
                        if links > MAX_SYMLINKS {
                            return Err(io::Error::new(io::ErrorKind::Other, "too many levels of symbolic links"));
                        }
                        let t = fs::read_link(self.host(&canon))?;
                        let t = t.as_os_str().as_bytes();
                        if t.starts_with(b"/") {
                            stack.clear();
                        }
                        pending.extend(components(t).rev().map(|c| c.to_vec()));
                        continue;
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => missing = true,
                    Err(e) if e.raw_os_error() == Some(20) => missing = true, // ENOTDIR
                    Err(e) => return Err(e),
                }
            }
            stack.push(c);
        }
        let canon = stack.join(&b'/');
        Ok(Resolved { host: self.host(&canon), canon })
    }

    /// Canonical form of an existing directory path, cached.
    fn dir_canon(&mut self, rel_dir: &[u8]) -> io::Result<Option<Vec<u8>>> {
        if rel_dir.is_empty() {
            return Ok(Some(Vec::new()));
        }
        if let Some(c) = self.dir_cache.get(rel_dir) {
            let c = c.clone();
            // work may follow in a directory on another file system
            if self.off_device && self.off_dirs.contains(rel_dir) {
                self.off_dirty = true;
            }
            return Ok(Some(c));
        }
        // With the directory above known, one lstat settles the usual case
        // (a real directory there). Walking every component again for each
        // directory made the removal of a big package ask about the same
        // directories some 17,000 times. A symlink, or a path with `.`, `..`
        // or a doubled slash, goes the long way below.
        let (parent, name) = split_rel(rel_dir);
        if !name.is_empty() && name != b"." && name != b".." {
            let Some(pc) = self.dir_canon(parent)? else { return Ok(None) };
            let canon = join_rel(&pc, name);
            let host = self.host(&canon);
            match fs::symlink_metadata(&host) {
                Ok(m) if m.is_dir() => {
                    if self.note_dev(m.dev(), &host) {
                        self.off_dirs.insert(rel_dir.to_vec());
                    }
                    self.dir_cache.insert(rel_dir.to_vec(), canon.clone());
                    return Ok(Some(canon));
                }
                Ok(m) if m.file_type().is_symlink() => {}
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        format!("/{} exists and is not a directory", String::from_utf8_lossy(rel_dir)),
                    ))
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e),
            }
        }
        let r = self.resolve(rel_dir, true)?;
        match fs::symlink_metadata(&r.host) {
            Ok(m) if m.is_dir() => {
                if self.note_dev(m.dev(), &r.host) {
                    self.off_dirs.insert(rel_dir.to_vec());
                }
                self.dir_cache.insert(rel_dir.to_vec(), r.canon.clone());
                Ok(Some(r.canon))
            }
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::Other,
                format!("/{} exists and is not a directory", String::from_utf8_lossy(rel_dir)),
            )),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Makes sure a directory exists. Missing ancestors are made 0755
    /// root:root (as apk does); the directory itself, if it has to be made,
    /// gets `mode` and the owner. Existing directories are left alone.
    /// Returns the canonical path. A directory that is, or resolves to,
    /// one of catalyst's own directories is refused ([`refuse_reserved`]).
    pub fn ensure_dir(&mut self, rel_dir: &[u8], mode: u32, uid: u32, gid: u32) -> io::Result<Vec<u8>> {
        if let Some(c) = self.dir_canon(rel_dir)? {
            refuse_reserved(rel_dir, &c)?;
            return Ok(c);
        }
        let (parent, _) = split_rel(rel_dir);
        self.ensure_dir(parent, 0o755, 0, 0)?;
        // the parent now exists; resolve again (the last component may be
        // a dangling symlink, which resolve follows)
        let r = self.resolve(rel_dir, true)?;
        refuse_reserved(rel_dir, &r.canon)?;
        // The new directory is made on the file system of the directory
        // it is made in, which is not always one resolved before (a
        // dangling symlink may lead to another mount): noted before the
        // directory is made.
        let off = match r.host.parent().map(|p| (p, fs::metadata(p))) {
            Some((p, Ok(m))) => self.note_dev(m.dev(), p),
            _ => false,
        };
        if self.hold && self.off_device {
            return Err(io::Error::new(io::ErrorKind::Other, OffDevice));
        }
        if make_dir_atomic(&r.host, mode, uid, gid)? {
            self.created_dirs.insert(r.canon.clone());
        }
        if off {
            self.off_dirs.insert(rel_dir.to_vec());
        }
        self.dir_cache.insert(rel_dir.to_vec(), r.canon.clone());
        Ok(r.canon)
    }

    /// Resolves the directory part of `rel` (which must exist, see
    /// [`RootFs::ensure_dir`]) and returns the host path of the entry
    /// itself, not following a final symlink, with its canonical path.
    /// An entry that is, or resolves to a place in, one of catalyst's own
    /// directories is refused.
    pub fn entry(&mut self, rel: &[u8]) -> io::Result<Resolved> {
        let (dir, name) = split_rel(rel);
        let canon_dir = match self.dir_canon(dir)? {
            Some(c) => c,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("directory /{} does not exist", String::from_utf8_lossy(dir)),
                ))
            }
        };
        let canon = join_rel(&canon_dir, name);
        refuse_reserved(rel, &canon)?;
        Ok(Resolved { host: self.host(&canon), canon })
    }

    /// Like [`RootFs::entry`], but `None` when the directory is missing
    /// (or the entry is in one of catalyst's own directories).
    pub fn entry_if_dir_exists(&mut self, rel: &[u8]) -> io::Result<Option<Resolved>> {
        let (dir, name) = split_rel(rel);
        // a directory resolved before: its path is not copied once more
        let canon = match self.dir_cache.get(dir) {
            Some(canon_dir) => {
                let c = join_rel(canon_dir, name);
                if self.off_device && self.off_dirs.contains(dir) {
                    self.off_dirty = true;
                }
                c
            }
            None => match self.dir_canon(dir) {
                Ok(Some(canon_dir)) => join_rel(&canon_dir, name),
                Ok(None) | Err(_) => return Ok(None),
            },
        };
        if db::is_reserved(&canon) {
            return Ok(None);
        }
        Ok(Some(Resolved { host: self.host(&canon), canon }))
    }

    /// The canonical path of directory `rel_dir`, if it exists as one.
    pub fn dir_if_exists(&mut self, rel_dir: &[u8]) -> Option<Vec<u8>> {
        self.dir_canon(rel_dir).ok().flatten()
    }

    /// Where directory `rel_dir` is, or will be once [`RootFs::ensure_dir`]
    /// has made it: the canonical path of the part of it that exists, and
    /// the rest as written (a missing directory is made as a real one under
    /// the last existing one; a symlink on the way, dangling or not, is
    /// followed, as `ensure_dir` follows it). `planned` remembers the
    /// answers for the directories that do not exist yet.
    pub fn dir_planned(&mut self, rel_dir: &[u8], planned: &mut HashMap<Vec<u8>, Vec<u8>>) -> io::Result<Vec<u8>> {
        if let Some(c) = planned.get(rel_dir) {
            return Ok(c.clone());
        }
        if let Some(c) = self.dir_canon(rel_dir)? {
            return Ok(c);
        }
        let (parent, name) = split_rel(rel_dir);
        let canon = if name.is_empty() || name == b"." || name == b".." {
            self.resolve(rel_dir, true)?.canon
        } else {
            let pc = self.dir_planned(parent, planned)?;
            let here = join_rel(&pc, name);
            match fs::symlink_metadata(self.host(&here)) {
                // a symlink that leads nowhere yet: the directory is made where it points
                Ok(m) if m.file_type().is_symlink() => self.resolve(rel_dir, true)?.canon,
                _ => here,
            }
        };
        planned.insert(rel_dir.to_vec(), canon.clone());
        Ok(canon)
    }

    /// Forgets cached directories: after directories were removed, and
    /// after a package script ran (it may have replaced a directory by a
    /// symlink).
    pub fn forget(&mut self) {
        self.dir_cache.clear();
        self.off_dirs.clear();
    }

    /// Reads a small regular file of the root, resolved inside the root
    /// (an absolute symlink such as `/etc/apk/keys/k -> /usr/share/k`
    /// stays in the root). `Ok(None)` if it does not exist; an error if it
    /// is not a regular file or larger than `max` bytes.
    pub fn read_file(&self, rel: &[u8], max: u64) -> io::Result<Option<Vec<u8>>> {
        let r = self.resolve(rel, true)?;
        let f = match sys::open_regular_nofollow(&r.host) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut v = Vec::new();
        (&f).take(max + 1).read_to_end(&mut v)?;
        if v.len() as u64 > max {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("/{} is larger than {max} bytes", String::from_utf8_lossy(&r.canon)),
            ));
        }
        Ok(Some(v))
    }

    /// The names in a directory of the root (resolved inside the root),
    /// with the directory's canonical path; `None` if it does not exist.
    pub fn list_dir(&self, rel: &[u8]) -> io::Result<Option<(Vec<u8>, Vec<Vec<u8>>)>> {
        let r = self.resolve(rel, true)?;
        let rd = match fs::read_dir(&r.host) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut names = Vec::new();
        for e in rd {
            names.push(e?.file_name().as_bytes().to_vec());
        }
        Ok(Some((r.canon, names)))
    }
}

/// Makes the directory `host` with its final owner and mode, atomically:
/// it is made under a temporary name next to it, given owner and mode
/// through a descriptor (no symlink can be slipped in), and renamed into
/// place, so that a kill at any moment leaves either no directory or the
/// complete one (an existing directory is left alone, so one made with the
/// wrong mode would stay wrong). A temporary left by an interrupted run is
/// removed. Returns false if the directory was already there.
fn make_dir_atomic(host: &Path, mode: u32, uid: u32, gid: u32) -> io::Result<bool> {
    // The usual directory (root:root 0755 under a root:root directory) is
    // what one plain mkdir makes, and then that one call is the atomic way:
    // the temporary name, the descriptor and the rename below cost five
    // calls more, for each of the 1,250 directories of a big install.
    if plain_mkdir_is_exact(host, mode, uid, gid) {
        return match fs::DirBuilder::new().mode(mode).create(host) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if fs::symlink_metadata(host).map_or(false, |m| m.is_dir()) {
                    Ok(false)
                } else {
                    Err(e)
                }
            }
            Err(e) => Err(e),
        };
    }
    let tmp = tmp_name(host);
    match fs::symlink_metadata(&tmp) {
        Ok(m) if m.is_dir() => fs::remove_dir(&tmp)?,
        Ok(_) => fs::remove_file(&tmp)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    fs::DirBuilder::new().mode(0o700).create(&tmp)?;
    let res = (|| {
        let d = sys::open_dir_nofollow(&tmp)?;
        if sys::is_root() {
            fchown(&d, Some(uid), Some(gid))?;
        }
        // mkdir applies the umask; chown clears set-id bits
        d.set_permissions(fs::Permissions::from_mode(mode & 0o7777))?;
        drop(d);
        if fs::symlink_metadata(host).is_ok() {
            return Ok(false); // made meanwhile; rename would replace an empty one
        }
        fs::rename(&tmp, host).map(|_| true)
    })();
    match res {
        Ok(true) => Ok(true),
        Ok(false) => {
            fs::remove_dir(&tmp)?;
            Ok(false)
        }
        Err(e) => {
            let _ = fs::remove_dir(&tmp);
            if fs::symlink_metadata(host).map_or(false, |m| m.is_dir()) {
                Ok(false)
            } else {
                Err(e)
            }
        }
    }
}

/// Whether a plain `mkdir(host, mode)` gives exactly this mode and owner,
/// whatever the file system's rules for a new entry's group are: no bit of
/// the mode is taken by the umask or is a write bit for others or a set-id
/// or sticky bit; the directory above is a real one, not setgid (its bit
/// would be inherited) and without a default ACL (which the new directory
/// would inherit, mode included); and, for root, the owner wanted is the one the
/// process runs as and the group wanted is both the process's and that of
/// the directory above (so it comes out right whether the new entry takes
/// the one or the other). Otherwise the directory is made the long way.
fn plain_mkdir_is_exact(host: &Path, mode: u32, uid: u32, gid: u32) -> bool {
    if mode & !0o755 != 0 || sys::umask_at_start() & mode != 0 {
        return false;
    }
    let Some(parent) = host.parent() else { return false };
    let Ok(pm) = fs::symlink_metadata(parent) else { return false };
    if !pm.is_dir() || pm.mode() & 0o2000 != 0 || sys::has_default_acl(parent) {
        return false;
    }
    !sys::is_root() || (unsafe { libc::geteuid() == uid && libc::getegid() == gid } && pm.gid() == gid)
}

/// Refuses a path whose canonical form is in one of catalyst's own directories
/// (`Programs`, `var/lib/catalyst`, `var/cache/catalyst`): a package must not reach
/// them, not even through a symlink that another package (or a script)
/// put in the live tree.
fn refuse_reserved(rel: &[u8], canon: &[u8]) -> io::Result<()> {
    if db::is_reserved(canon) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "/{} resolves to /{}, inside catalyst's own directories; refused",
                String::from_utf8_lossy(rel),
                String::from_utf8_lossy(canon)
            ),
        ));
    }
    Ok(())
}

/// A temporary name next to `target`, unique per target name: `.catalyst.` and
/// 16 hex digits of SHA-256 of the name. A leftover from an interrupted
/// run is simply reused.
pub fn tmp_name(target: &Path) -> PathBuf {
    let name = target.file_name().map(|n| n.as_bytes()).unwrap_or(b"");
    let h = crate::digest::sha256(name);
    let t = format!(".catalyst.{}", crate::digest::hex(&h[..8]));
    match target.parent() {
        Some(p) => p.join(t),
        None => PathBuf::from(t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn resolve_in_root() {
        // scratch space: $CATALYST_TEST_TMP (default /root/catalyst-test); skipped if absent
        let tmp = PathBuf::from(std::env::var_os("CATALYST_TEST_TMP").unwrap_or_else(|| "/root/catalyst-test".into()));
        if !tmp.is_dir() {
            eprintln!("skipped: {} does not exist", tmp.display());
            return;
        }
        let base = tmp.join(format!("unit-rootfs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("usr/bin")).unwrap();
        symlink("usr/bin", base.join("bin")).unwrap();
        symlink("/usr/bin", base.join("abs")).unwrap();
        symlink("../../../..", base.join("usr/up")).unwrap();
        let r = RootFs::new(&base);
        assert_eq!(r.resolve(b"bin/sh", false).unwrap().canon, b"usr/bin/sh");
        assert_eq!(r.resolve(b"abs/sh", false).unwrap().canon, b"usr/bin/sh");
        assert_eq!(r.resolve(b"usr/up/etc/passwd", false).unwrap().canon, b"etc/passwd");
        assert_eq!(r.resolve(b"../../etc", false).unwrap().canon, b"etc");
        assert_eq!(r.resolve(b"bin", false).unwrap().canon, b"bin");
        assert_eq!(r.resolve(b"bin", true).unwrap().canon, b"usr/bin");
        assert_eq!(r.resolve(b"nope/x/y", true).unwrap().canon, b"nope/x/y");
        let mut r = r;
        assert_eq!(r.ensure_dir(b"abs/x/y", 0o700, 0, 0).unwrap(), b"usr/bin/x/y");
        assert!(base.join("usr/bin/x/y").is_dir());
        fs::remove_dir_all(&base).unwrap();
    }

    /// A plain mkdir is taken for exact only where it is: the umask takes
    /// no bit of the mode, the mode has none a mkdir is not trusted with,
    /// and the directory above passes on neither a set-group-id bit nor a
    /// default ACL.
    #[test]
    fn a_plain_mkdir_is_taken_only_where_it_is_exact() {
        sys::note_umask();
        let base = std::env::temp_dir().join(format!("catalyst-mkdir-exact-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::DirBuilder::new().mode(0o755).create(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o755)).unwrap();
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        let exact = |parent: &Path, mode: u32| plain_mkdir_is_exact(&parent.join("new"), mode, uid, gid);
        // under an ordinary directory: when the umask takes no bit of the
        // mode and, for root, the groups agree
        let plain = sys::umask_at_start() & 0o755 == 0 && (!sys::is_root() || fs::metadata(&base).unwrap().gid() == gid);
        assert_eq!(exact(&base, 0o755), plain);
        assert_eq!(exact(&base, 0o700), sys::umask_at_start() & 0o700 == 0 && plain);
        // modes a plain mkdir is not trusted with
        assert!(!exact(&base, 0o775) && !exact(&base, 0o757) && !exact(&base, 0o1755) && !exact(&base, 0o2755));
        // a set-group-id directory above: the new one would inherit the bit
        let sg = base.join("setgid");
        fs::DirBuilder::new().mode(0o755).create(&sg).unwrap();
        fs::set_permissions(&sg, fs::Permissions::from_mode(0o2755)).unwrap();
        assert!(!exact(&sg, 0o755));
        // a default ACL above (user::rwx, group::r-x, other::r-x): the new
        // one's mode would come from it; skipped where the file system has
        // no ACLs
        let acl = base.join("acl");
        fs::DirBuilder::new().mode(0o755).create(&acl).unwrap();
        let mut blob = vec![2u8, 0, 0, 0];
        for (tag, perm) in [(1u16, 7u16), (4, 5), (0x20, 5)] {
            blob.extend(tag.to_le_bytes());
            blob.extend(perm.to_le_bytes());
            blob.extend(u32::MAX.to_le_bytes());
        }
        assert!(!sys::has_default_acl(&base));
        if sys::set_xattr(&acl, b"system.posix_acl_default", &blob).is_ok() {
            assert!(sys::has_default_acl(&acl) && !exact(&acl, 0o755));
        } else {
            eprintln!("no default ACL could be set here: that part is not tested");
        }
        // nothing above, or a file
        assert!(!exact(&base.join("missing"), 0o755));
        fs::write(base.join("file"), b"").unwrap();
        assert!(!exact(&base.join("file"), 0o755));
        // fast path must fail when host already exists as a file or symlink, not return Ok(false)
        assert!(make_dir_atomic(&base.join("file"), 0o755, uid, gid).is_err());
        symlink(&base, base.join("link-to-dir")).unwrap();
        assert!(make_dir_atomic(&base.join("link-to-dir"), 0o755, uid, gid).is_err());
        // existing real directory still returns Ok(false)
        assert_eq!(make_dir_atomic(&sg, 0o755, uid, gid).unwrap(), false);
        fs::remove_dir_all(&base).unwrap();
    }
}
