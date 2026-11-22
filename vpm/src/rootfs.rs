//! Paths inside a managed root, resolved the way the kernel would resolve
//! them after `chroot(ROOT)` and opened through Linux `openat2(RESOLVE_IN_ROOT)`.
//!
//! vpm works on the root from outside. A path such as `bin/sh` in a root
//! where `bin` is a symlink must lead to `ROOT/usr/bin/sh`, and an absolute
//! symlink (`lib -> /usr/lib`) or a `..` must never lead out of the root
//! into the host.
//!
//! [`RootFs`] holds an open descriptor to `ROOT` and opens live-tree parent
//! directories and files through `openat2(RESOLVE_IN_ROOT)` so the kernel
//! itself enforces confinement inside `ROOT`; callers then perform `*at()`
//! syscalls on `(dirfd, leaf)` with no symlink-escape TOCTOU window.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{fchown, PermissionsExt};
use std::os::unix::io::{AsRawFd, OwnedFd, RawFd};
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

/// A live-tree entry anchored to its `openat2(RESOLVE_IN_ROOT)`-opened parent
/// directory descriptor plus validated single-component leaf name.
pub struct EntryAt {
    pub dirfd: OwnedFd,
    pub leaf: Vec<u8>,
    pub canon: Vec<u8>,
    pub host: PathBuf,
}

impl EntryAt {
    pub fn fd(&self) -> RawFd {
        self.dirfd.as_raw_fd()
    }
}

pub struct RootFs {
    root: PathBuf,
    /// Open directory descriptor for `root` (for `openat2(RESOLVE_IN_ROOT)`).
    root_fd: Option<OwnedFd>,
    /// Root-relative directory path as written by a package -> its
    /// canonical form, for directories known to exist as real directories.
    dir_cache: HashMap<Vec<u8>, Vec<u8>>,
    /// Canonical directories created by this RootFs (for triggers).
    pub created_dirs: HashSet<Vec<u8>>,
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
        let root_fd = sys::open_root_dir(root).ok();
        RootFs {
            root: root.to_path_buf(),
            root_fd,
            dir_cache: HashMap::new(),
            created_dirs: HashSet::new(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn root_fd(&self) -> io::Result<OwnedFd> {
        if let Some(fd) = &self.root_fd {
            fd.try_clone()
        } else {
            sys::open_root_dir(&self.root)
        }
    }

    fn root_raw_fd(&mut self) -> io::Result<RawFd> {
        if self.root_fd.is_none() {
            self.root_fd = Some(sys::open_root_dir(&self.root)?);
        }
        Ok(self.root_fd.as_ref().unwrap().as_raw_fd())
    }

    pub fn host(&self, canon: &[u8]) -> PathBuf {
        if canon.is_empty() {
            self.root.clone()
        } else {
            self.root.join(OsStr::from_bytes(canon))
        }
    }

    /// Opens an existing directory strictly inside `ROOT` using
    /// `openat2(RESOLVE_IN_ROOT)` (falls back to `open_dir_nofollow` only on
    /// `ENOSYS`).
    pub fn open_dir_confined(&self, canon_dir: &[u8]) -> io::Result<OwnedFd> {
        let rfd = self.root_fd()?;
        match sys::open_dir_in_root(rfd.as_raw_fd(), canon_dir) {
            Ok(fd) => Ok(fd),
            Err(e) if e.raw_os_error() == Some(libc::ENOSYS) => {
                let f = sys::open_dir_nofollow(&self.host(canon_dir))?;
                Ok(f.into())
            }
            Err(e) => Err(e),
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
                    Err(e) if e.raw_os_error() == Some(libc::ENOTDIR) => missing = true,
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
            return Ok(Some(c.clone()));
        }
        let r = self.resolve(rel_dir, true)?;
        match fs::symlink_metadata(&r.host) {
            Ok(m) if m.is_dir() => {
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
    /// gets `mode` and the owner through `openat2(RESOLVE_IN_ROOT)` + `*at()`.
    /// Existing directories are left alone. Returns the canonical path.
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
        let (canon_parent, leaf) = split_rel(&r.canon);
        if !leaf.is_empty() {
            let pfd = self.open_dir_confined(canon_parent)?;
            if make_dir_atomic_at(pfd.as_raw_fd(), leaf, mode, uid, gid)? {
                self.created_dirs.insert(r.canon.clone());
            }
        }
        self.dir_cache.insert(rel_dir.to_vec(), r.canon.clone());
        Ok(r.canon)
    }

    /// Resolves the directory part of `rel` and opens it strictly inside
    /// `ROOT` via `openat2(RESOLVE_IN_ROOT)`, returning an [`EntryAt`] for
    /// race-free `*at()` operations.
    pub fn open_parent(&mut self, rel: &[u8]) -> io::Result<EntryAt> {
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
        let rfd = self.root_raw_fd()?;
        let dirfd = match sys::open_dir_in_root(rfd, &canon_dir) {
            Ok(fd) => fd,
            Err(e) if e.raw_os_error() == Some(libc::ENOSYS) => {
                sys::open_dir_nofollow(&self.host(&canon_dir))?.into()
            }
            Err(e) => return Err(e),
        };
        Ok(EntryAt {
            dirfd,
            leaf: name.to_vec(),
            host: self.host(&canon),
            canon,
        })
    }

    /// Like [`RootFs::open_parent`], but returns `Ok(None)` when the parent
    /// directory does not exist (or the entry is in one of vpm's own
    /// directories).
    pub fn open_parent_if_dir_exists(&mut self, rel: &[u8]) -> io::Result<Option<EntryAt>> {
        let (dir, name) = split_rel(rel);
        let Some(canon_dir) = (match self.dir_canon(dir) {
            Ok(v) => v,
            Err(_) => return Ok(None),
        }) else {
            return Ok(None);
        };
        let canon = join_rel(&canon_dir, name);
        if db::is_reserved(&canon) {
            return Ok(None);
        }
        let rfd = self.root_raw_fd()?;
        let dirfd = match sys::open_dir_in_root(rfd, &canon_dir) {
            Ok(fd) => fd,
            Err(e) if e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ENOTDIR) => {
                return Ok(None);
            }
            Err(e) if e.raw_os_error() == Some(libc::ENOSYS) => match sys::open_dir_nofollow(&self.host(&canon_dir)) {
                Ok(f) => f.into(),
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e),
            },
            Err(e) => return Err(e),
        };
        Ok(Some(EntryAt {
            dirfd,
            leaf: name.to_vec(),
            host: self.host(&canon),
            canon,
        }))
    }

    /// Resolves the directory part of `rel` (which must exist, see
    /// [`RootFs::ensure_dir`]) and returns the host path of the entry
    /// itself, not following a final symlink, with its canonical path.
    /// An entry that is, or resolves to a place in, one of vpm's own
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
    /// (or the entry is in one of vpm's own directories).
    pub fn entry_if_dir_exists(&mut self, rel: &[u8]) -> io::Result<Option<Resolved>> {
        let (dir, name) = split_rel(rel);
        match self.dir_canon(dir) {
            Ok(Some(canon_dir)) => {
                let canon = join_rel(&canon_dir, name);
                if db::is_reserved(&canon) {
                    return Ok(None);
                }
                Ok(Some(Resolved { host: self.host(&canon), canon }))
            }
            Ok(None) => Ok(None),
            Err(_) => Ok(None),
        }
    }

    /// Forgets cached directories: after directories were removed, and
    /// after a package script ran (it may have replaced a directory by a
    /// symlink).
    pub fn forget(&mut self) {
        self.dir_cache.clear();
    }

    /// Reads a small regular file of the root, opened directly through
    /// `openat2(RESOLVE_IN_ROOT)` so the kernel guarantees symlinks never
    /// escape `ROOT`. `Ok(None)` if it does not exist; an error if it is not
    /// a regular file or larger than `max` bytes.
    pub fn read_file(&self, rel: &[u8], max: u64) -> io::Result<Option<Vec<u8>>> {
        let rel_trim = rel.strip_prefix(b"/").unwrap_or(rel);
        let f = if let Ok(rfd) = self.root_fd() {
            match sys::open_regular_in_root(rfd.as_raw_fd(), rel_trim) {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ENOTDIR) => {
                    return Ok(None);
                }
                Err(e) if e.raw_os_error() == Some(libc::ENOSYS) => {
                    let r = self.resolve(rel_trim, true)?;
                    match sys::open_regular_nofollow(&r.host) {
                        Ok(f) => f,
                        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                        Err(e) => return Err(e),
                    }
                }
                Err(e) => return Err(e),
            }
        } else {
            let r = self.resolve(rel_trim, true)?;
            match sys::open_regular_nofollow(&r.host) {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e),
            }
        };
        let mut v = Vec::new();
        (&f).take(max + 1).read_to_end(&mut v)?;
        if v.len() as u64 > max {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("/{} is larger than {max} bytes", String::from_utf8_lossy(rel_trim)),
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

/// Makes directory `(parent_fd, leaf)` with its final owner and mode,
/// atomically through `*at()` syscalls: it is made under a temporary leaf
/// name, given owner and mode through an open directory fd (no symlink can
/// be slipped in), and renamed into place. Returns false if the directory
/// was already there.
fn make_dir_atomic_at(parent_fd: RawFd, leaf: &[u8], mode: u32, uid: u32, gid: u32) -> io::Result<bool> {
    let tmp = tmp_leaf(leaf);
    match sys::fstatat_nofollow(parent_fd, &tmp) {
        Ok(st) if sys::stat_is_dir(&st) => sys::unlinkat_in(parent_fd, &tmp, true)?,
        Ok(_) => sys::unlinkat_in(parent_fd, &tmp, false)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    sys::mkdirat_in(parent_fd, &tmp, 0o700)?;
    let res = (|| {
        let d = sys::openat_dir_nofollow(parent_fd, &tmp)?;
        if sys::is_root() {
            fchown(&d, Some(uid), Some(gid))?;
        }
        // mkdir applies the umask; chown clears set-id bits
        d.set_permissions(fs::Permissions::from_mode(mode & 0o7777))?;
        drop(d);
        if sys::fstatat_nofollow(parent_fd, leaf).is_ok() {
            return Ok(false); // made meanwhile; rename would replace an empty one
        }
        sys::renameat_in(parent_fd, &tmp, leaf).map(|_| true)
    })();
    match res {
        Ok(true) => Ok(true),
        Ok(false) => {
            sys::unlinkat_in(parent_fd, &tmp, true)?;
            Ok(false)
        }
        Err(e) => {
            let _ = sys::unlinkat_in(parent_fd, &tmp, true);
            if sys::fstatat_nofollow(parent_fd, leaf).map_or(false, |st| sys::stat_is_dir(&st)) {
                Ok(false)
            } else {
                Err(e)
            }
        }
    }
}

/// Refuses a path whose canonical form is in one of vpm's own directories
/// (`Programs`, `var/lib/vpm`, `var/cache/vpm`): a package must not reach
/// them, not even through a symlink that another package (or a script)
/// put in the live tree.
fn refuse_reserved(rel: &[u8], canon: &[u8]) -> io::Result<()> {
    if db::is_reserved(canon) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "/{} resolves to /{}, inside vpm's own directories; refused",
                String::from_utf8_lossy(rel),
                String::from_utf8_lossy(canon)
            ),
        ));
    }
    Ok(())
}

/// A temporary leaf name for `leaf`, unique per entry name: `.vpm.` and
/// 16 hex digits of SHA-256 of the name.
pub fn tmp_leaf(leaf: &[u8]) -> Vec<u8> {
    let h = crate::digest::sha256(leaf);
    format!(".vpm.{}", crate::digest::hex(&h[..8])).into_bytes()
}

/// A temporary name next to `target`, unique per target name: `.vpm.` and
/// 16 hex digits of SHA-256 of the name. A leftover from an interrupted
/// run is simply reused.
pub fn tmp_name(target: &Path) -> PathBuf {
    let name = target.file_name().map(|n| n.as_bytes()).unwrap_or(b"");
    let t = tmp_leaf(name);
    match target.parent() {
        Some(p) => p.join(OsStr::from_bytes(&t)),
        None => PathBuf::from(OsStr::from_bytes(&t)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn resolve_and_openat2_in_root() {
        let base = std::env::temp_dir().join(format!("vpm-unit-rootfs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("usr/bin")).unwrap();
        fs::create_dir_all(base.join("etc")).unwrap();
        fs::write(base.join("etc/passwd"), b"root:x:0:0::/root:/bin/sh\n").unwrap();
        symlink("usr/bin", base.join("bin")).unwrap();
        symlink("/usr/bin", base.join("abs")).unwrap();
        symlink("../../../..", base.join("usr/up")).unwrap();
        symlink("/etc/passwd", base.join("usr/abs_passwd")).unwrap();

        let mut r = RootFs::new(&base);
        assert_eq!(r.resolve(b"bin/sh", false).unwrap().canon, b"usr/bin/sh");
        assert_eq!(r.resolve(b"abs/sh", false).unwrap().canon, b"usr/bin/sh");
        assert_eq!(r.resolve(b"usr/up/etc/passwd", false).unwrap().canon, b"etc/passwd");
        assert_eq!(r.resolve(b"../../etc", false).unwrap().canon, b"etc");
        assert_eq!(r.resolve(b"bin", false).unwrap().canon, b"bin");
        assert_eq!(r.resolve(b"bin", true).unwrap().canon, b"usr/bin");
        assert_eq!(r.resolve(b"nope/x/y", true).unwrap().canon, b"nope/x/y");

        // openat2(RESOLVE_IN_ROOT) keeps absolute symlinks & .. escapes strictly inside `base`
        // (must read `base/etc/passwd`, NOT the host's `/etc/passwd`!)
        assert_eq!(
            r.read_file(b"usr/abs_passwd", 4096).unwrap().as_deref(),
            Some(&b"root:x:0:0::/root:/bin/sh\n"[..])
        );
        assert_eq!(
            r.read_file(b"usr/up/etc/passwd", 4096).unwrap().as_deref(),
            Some(&b"root:x:0:0::/root:/bin/sh\n"[..])
        );

        // ensure_dir + open_parent + *at() file creation inside symlinked dir
        assert_eq!(r.ensure_dir(b"abs/x/y", 0o700, 0, 0).unwrap(), b"usr/bin/x/y");
        assert!(base.join("usr/bin/x/y").is_dir());
        let ent = r.open_parent(b"abs/x/y/hello").unwrap();
        assert_eq!(ent.canon, b"usr/bin/x/y/hello");
        assert_eq!(ent.leaf, b"hello");
        let _f = sys::openat_create_new(ent.fd(), &ent.leaf, 0o644).unwrap();
        assert!(base.join("usr/bin/x/y/hello").is_file());

        // Symlink pointing into vpm's own reserved directory is refused
        symlink("/Programs/evil", base.join("usr/escape")).unwrap();
        fs::create_dir_all(base.join("Programs/evil")).unwrap();
        assert!(r.open_parent(b"usr/escape/pwn").is_err());

        fs::remove_dir_all(&base).unwrap();
    }
}
