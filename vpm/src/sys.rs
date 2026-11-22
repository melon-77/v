//! Thin wrappers over Linux libc/syscall calls the standard library does not
//! offer (glibc or musl): `openat2(RESOLVE_IN_ROOT)`, `*at()` directory-relative
//! operations, device nodes, xattrs, timestamps without following symlinks,
//! `syncfs`, `flock`, `fnmatch`, terminal queries and `chroot`/`umask` for
//! script children.

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::mem::size_of;
use std::os::raw::{c_char, c_int, c_long, c_uint, c_ulong};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::sync::atomic::{AtomicI32, Ordering as AtomicOrdering};

#[repr(C)]
struct Timespec {
    tv_sec: c_long,
    tv_nsec: c_long,
}

#[repr(C)]
#[derive(Default)]
struct Winsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

extern "C" {
    fn mknod(path: *const c_char, mode: c_uint, dev: u64) -> c_int;
    fn mknodat(dirfd: c_int, path: *const c_char, mode: c_uint, dev: u64) -> c_int;
    fn fnmatch(pattern: *const c_char, string: *const c_char, flags: c_int) -> c_int;
    fn syncfs(fd: c_int) -> c_int;
    fn flock(fd: c_int, op: c_int) -> c_int;
    fn isatty(fd: c_int) -> c_int;
    fn ioctl(fd: c_int, req: c_ulong, ...) -> c_int;
    fn umask(mask: c_uint) -> c_uint;
    fn lsetxattr(path: *const c_char, name: *const c_char, value: *const u8, size: usize, flags: c_int) -> c_int;
    fn fsetxattr(fd: c_int, name: *const c_char, value: *const u8, size: usize, flags: c_int) -> c_int;
    fn utimensat(dirfd: c_int, path: *const c_char, times: *const Timespec, flags: c_int) -> c_int;
    fn geteuid() -> c_uint;
    fn chroot(path: *const c_char) -> c_int;
    fn chdir(path: *const c_char) -> c_int;
}

const AT_FDCWD: c_int = -100;
const AT_SYMLINK_NOFOLLOW: c_int = 0x100;
const AT_REMOVEDIR: c_int = 0x200;
const FNM_PATHNAME: c_int = 1;
const LOCK_EX: c_int = 2;
const LOCK_NB: c_int = 4;
const TIOCGWINSZ: c_ulong = 0x5413;

/// Linux `openat2(2)` resolve flags (`<linux/openat2.h>`).
pub const RESOLVE_NO_SYMLINKS: u64 = 0x04;
pub const RESOLVE_IN_ROOT: u64 = 0x10;

pub fn cstr(p: &Path) -> io::Result<CString> {
    CString::new(p.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}

fn cbytes(b: &[u8]) -> io::Result<CString> {
    CString::new(b).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}

fn cleaf(leaf: &[u8]) -> io::Result<CString> {
    if leaf.is_empty() || leaf.contains(&b'/') || leaf == b"." || leaf == b".." {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid directory entry name '{}'", String::from_utf8_lossy(leaf)),
        ));
    }
    cbytes(leaf)
}

fn check(r: c_int) -> io::Result<()> {
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `mknod(path, mode, dev)`; `mode` includes the file type bits.
pub fn make_node(path: &Path, mode: u32, dev: u64) -> io::Result<()> {
    let c = cstr(path)?;
    check(unsafe { mknod(c.as_ptr(), mode, dev) })
}

/// `lsetxattr`.
pub fn set_xattr(path: &Path, name: &[u8], value: &[u8]) -> io::Result<()> {
    let c = cstr(path)?;
    let n = CString::new(name).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "xattr name contains NUL"))?;
    check(unsafe { lsetxattr(c.as_ptr(), n.as_ptr(), value.as_ptr(), value.len(), 0) })
}

/// `fsetxattr` on an open file descriptor (immune to symlink races).
pub fn fsetxattr_fd(fd: RawFd, name: &[u8], value: &[u8]) -> io::Result<()> {
    let n = CString::new(name).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "xattr name contains NUL"))?;
    check(unsafe { fsetxattr(fd, n.as_ptr(), value.as_ptr(), value.len(), 0) })
}

/// Sets atime and mtime of `path` (not following a final symlink).
pub fn set_mtime_nofollow(path: &Path, mtime: u64) -> io::Result<()> {
    let c = cstr(path)?;
    let t = [Timespec { tv_sec: mtime as c_long, tv_nsec: 0 }, Timespec { tv_sec: mtime as c_long, tv_nsec: 0 }];
    check(unsafe { utimensat(AT_FDCWD, c.as_ptr(), t.as_ptr(), AT_SYMLINK_NOFOLLOW) })
}

/// `fnmatch(pattern, s, FNM_PATHNAME) == 0`.
pub fn glob_match(pattern: &[u8], s: &[u8]) -> bool {
    let (Ok(p), Ok(s)) = (CString::new(pattern), CString::new(s)) else { return false };
    unsafe { fnmatch(p.as_ptr(), s.as_ptr(), FNM_PATHNAME) == 0 }
}

/// Flushes the whole file system that holds `f`.
pub fn sync_fs(f: &File) -> io::Result<()> {
    check(unsafe { syncfs(f.as_raw_fd()) })
}

/// Takes an exclusive lock without waiting; `Ok(false)` if it is held.
pub fn try_lock(f: &File) -> io::Result<bool> {
    if unsafe { flock(f.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0 {
        return Ok(true);
    }
    let e = io::Error::last_os_error();
    if e.kind() == io::ErrorKind::WouldBlock {
        Ok(false)
    } else {
        Err(e)
    }
}

pub fn is_tty(fd: i32) -> bool {
    unsafe { isatty(fd) == 1 }
}

/// (rows, columns) of the terminal on `fd`.
pub fn term_size(fd: i32) -> Option<(u16, u16)> {
    let mut ws = Winsize::default();
    let r = unsafe { ioctl(fd, TIOCGWINSZ, &mut ws as *mut Winsize) };
    if r == 0 && ws.ws_row > 0 && ws.ws_col > 0 {
        Some((ws.ws_row, ws.ws_col))
    } else {
        None
    }
}

pub fn is_root() -> bool {
    unsafe { geteuid() == 0 }
}

/// The descriptor that holds the root's lock (-1: none). Package scripts
/// inherit it, so that a script left running after vpm itself was killed
/// keeps the root locked until it ends (no second vpm can start the same
/// recovery, and the same script, beside it).
static LOCK_FD: AtomicI32 = AtomicI32::new(-1);

pub fn set_lock_fd(fd: i32) {
    LOCK_FD.store(fd, AtomicOrdering::Relaxed);
}

/// For a child between fork and exec: `umask(022)`, the lock descriptor
/// made inheritable, `chroot(root)`, `chdir("/")`. Only async-signal-safe
/// calls; the strings are made before the fork.
pub struct ChrootPrep {
    root: CString,
    slash: CString,
    lock_fd: i32,
}

impl ChrootPrep {
    pub fn new(root: &Path) -> io::Result<Self> {
        Ok(ChrootPrep { root: cstr(root)?, slash: CString::new("/").unwrap(), lock_fd: LOCK_FD.load(AtomicOrdering::Relaxed) })
    }

    /// Like [`ChrootPrep::new`], but keeps the root lock `O_CLOEXEC` (for
    /// commit hooks, which do not need to inherit the lock).
    pub fn new_no_lock(root: &Path) -> io::Result<Self> {
        Ok(ChrootPrep { root: cstr(root)?, slash: CString::new("/").unwrap(), lock_fd: -1 })
    }

    /// # Safety
    /// To be called only in a forked child before exec.
    pub unsafe fn apply(&self) -> io::Result<()> {
        umask(0o022);
        if self.lock_fd >= 0 {
            libc::fcntl(self.lock_fd, libc::F_SETFD, 0);
        }
        if self.root.as_bytes() != b"/" {
            check(chroot(self.root.as_ptr()))?;
        }
        check(chdir(self.slash.as_ptr()))
    }
}

/// Ends the process at once with SIGKILL, as a crash or `kill -9` would
/// (the `VPM_CRASH_AT` test hook; `abort()` would go through a core-dump
/// handler).
pub fn kill_self() -> ! {
    unsafe {
        libc::kill(libc::getpid(), libc::SIGKILL);
    }
    std::process::abort()
}

/// Opens an existing file for reading without following a final symlink
/// and without blocking on a FIFO or device; only regular files are
/// accepted (`ErrorKind::InvalidInput` otherwise).
pub fn open_regular_nofollow(p: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(p)
        .map_err(|e| {
            if e.raw_os_error() == Some(libc::ELOOP) {
                io::Error::new(io::ErrorKind::InvalidInput, format!("{}: is a symbolic link", p.display()))
            } else {
                e
            }
        })?;
    if !f.metadata()?.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{}: not a regular file", p.display())));
    }
    Ok(f)
}

/// Opens a directory without following a final symlink (for `fchown` and
/// `fchmod` on exactly the directory that was made).
pub fn open_dir_nofollow(p: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(p)
}

// ---------------------------------------------------------------- openat2 & *at()

/// Raw Linux `openat2(2)` syscall.
pub fn openat2(dirfd: RawFd, path: &[u8], flags: u64, mode: u64, resolve: u64) -> io::Result<OwnedFd> {
    let c = cbytes(path)?;
    let how = OpenHow { flags, mode, resolve };
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dirfd as c_long,
            c.as_ptr(),
            &how as *const OpenHow,
            size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
}

/// Opens the managed root directory itself (`O_RDONLY | O_DIRECTORY | O_CLOEXEC`).
pub fn open_root_dir(root: &Path) -> io::Result<OwnedFd> {
    let c = cstr(root)?;
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Opens an existing directory strictly inside `root_fd` via
/// `openat2(RESOLVE_IN_ROOT)`. Empty `rel_dir` opens `root_fd` itself (`.`).
pub fn open_dir_in_root(root_fd: RawFd, rel_dir: &[u8]) -> io::Result<OwnedFd> {
    let p = if rel_dir.is_empty() { b"." } else { rel_dir };
    openat2(
        root_fd,
        p,
        (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64,
        0,
        RESOLVE_IN_ROOT,
    )
}

/// Opens an existing regular file strictly inside `root_fd` via
/// `openat2(RESOLVE_IN_ROOT)` (follows symlinks inside `root_fd` only,
/// never escapes `root_fd`, never blocks on FIFOs).
pub fn open_regular_in_root(root_fd: RawFd, rel: &[u8]) -> io::Result<File> {
    let fd = openat2(
        root_fd,
        rel,
        (libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC) as u64,
        0,
        RESOLVE_IN_ROOT,
    )?;
    let f = File::from(fd);
    if !f.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("/{}: not a regular file", String::from_utf8_lossy(rel)),
        ));
    }
    Ok(f)
}

/// `fstatat(dirfd, leaf, AT_SYMLINK_NOFOLLOW)`.
pub fn fstatat_nofollow(dirfd: RawFd, leaf: &[u8]) -> io::Result<libc::stat> {
    let c = cleaf(leaf)?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    check(unsafe { libc::fstatat(dirfd, c.as_ptr(), &mut st as *mut libc::stat, AT_SYMLINK_NOFOLLOW) })?;
    Ok(st)
}

pub fn stat_is_dir(st: &libc::stat) -> bool {
    (st.st_mode & libc::S_IFMT) == libc::S_IFDIR
}

pub fn stat_is_file(st: &libc::stat) -> bool {
    (st.st_mode & libc::S_IFMT) == libc::S_IFREG
}

pub fn stat_is_symlink(st: &libc::stat) -> bool {
    (st.st_mode & libc::S_IFMT) == libc::S_IFLNK
}

/// Opens an existing regular file at `(dirfd, leaf)` without following a
/// final symlink and without blocking on a FIFO/device.
pub fn openat_regular_nofollow(dirfd: RawFd, leaf: &[u8]) -> io::Result<File> {
    let c = cleaf(leaf)?;
    let fd = unsafe {
        libc::openat(dirfd, c.as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
    };
    if fd < 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ELOOP) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{}: is a symbolic link", String::from_utf8_lossy(leaf)),
            ));
        }
        return Err(e);
    }
    let f = unsafe { File::from_raw_fd(fd) };
    if !f.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{}: not a regular file", String::from_utf8_lossy(leaf)),
        ));
    }
    Ok(f)
}

/// Atomically creates a new regular file at `(dirfd, leaf)` (`O_CREAT|O_EXCL|O_NOFOLLOW`).
pub fn openat_create_new(dirfd: RawFd, leaf: &[u8], mode: u32) -> io::Result<File> {
    let c = cleaf(leaf)?;
    let fd = unsafe {
        libc::openat(
            dirfd,
            c.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode as c_uint,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Opens an existing directory at `(dirfd, leaf)` without following a symlink.
pub fn openat_dir_nofollow(dirfd: RawFd, leaf: &[u8]) -> io::Result<File> {
    let c = cleaf(leaf)?;
    let fd = unsafe {
        libc::openat(dirfd, c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// `linkat(AT_FDCWD, src, dst_dirfd, dst_leaf, 0)`.
pub fn linkat_into(src: &Path, dst_dirfd: RawFd, dst_leaf: &[u8]) -> io::Result<()> {
    let s = cstr(src)?;
    let d = cleaf(dst_leaf)?;
    check(unsafe { libc::linkat(AT_FDCWD, s.as_ptr(), dst_dirfd, d.as_ptr(), 0) })
}

/// `symlinkat(target, dst_dirfd, dst_leaf)`.
pub fn symlinkat_into(target: &[u8], dst_dirfd: RawFd, dst_leaf: &[u8]) -> io::Result<()> {
    let t = cbytes(target)?;
    let d = cleaf(dst_leaf)?;
    check(unsafe { libc::symlinkat(t.as_ptr(), dst_dirfd, d.as_ptr()) })
}

/// `readlinkat(dirfd, leaf)`.
pub fn readlinkat_in(dirfd: RawFd, leaf: &[u8]) -> io::Result<Vec<u8>> {
    let c = cleaf(leaf)?;
    let mut cap = 256usize;
    loop {
        let mut buf = vec![0u8; cap];
        let n = unsafe { libc::readlinkat(dirfd, c.as_ptr(), buf.as_mut_ptr() as *mut c_char, buf.len()) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        let n = n as usize;
        if n < buf.len() {
            buf.truncate(n);
            return Ok(buf);
        }
        cap *= 2;
        if cap > 64 * 1024 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "symlink target too long"));
        }
    }
}

/// `renameat(dirfd, old_leaf, dirfd, new_leaf)`.
pub fn renameat_in(dirfd: RawFd, old_leaf: &[u8], new_leaf: &[u8]) -> io::Result<()> {
    let o = cleaf(old_leaf)?;
    let n = cleaf(new_leaf)?;
    check(unsafe { libc::renameat(dirfd, o.as_ptr(), dirfd, n.as_ptr()) })
}

/// `unlinkat(dirfd, leaf, AT_REMOVEDIR if remove_dir else 0)`.
pub fn unlinkat_in(dirfd: RawFd, leaf: &[u8], remove_dir: bool) -> io::Result<()> {
    let c = cleaf(leaf)?;
    let flags = if remove_dir { AT_REMOVEDIR } else { 0 };
    check(unsafe { libc::unlinkat(dirfd, c.as_ptr(), flags) })
}

/// `mkdirat(dirfd, leaf, mode)`.
pub fn mkdirat_in(dirfd: RawFd, leaf: &[u8], mode: u32) -> io::Result<()> {
    let c = cleaf(leaf)?;
    check(unsafe { libc::mkdirat(dirfd, c.as_ptr(), mode as c_uint) })
}

/// `mknodat(dirfd, leaf, mode, dev)`.
pub fn mknodat_in(dirfd: RawFd, leaf: &[u8], mode: u32, dev: u64) -> io::Result<()> {
    let c = cleaf(leaf)?;
    check(unsafe { mknodat(dirfd, c.as_ptr(), mode as c_uint, dev) })
}

/// `fchownat(dirfd, leaf, uid, gid, AT_SYMLINK_NOFOLLOW)`.
pub fn fchownat_nofollow(dirfd: RawFd, leaf: &[u8], uid: u32, gid: u32) -> io::Result<()> {
    let c = cleaf(leaf)?;
    check(unsafe { libc::fchownat(dirfd, c.as_ptr(), uid, gid, AT_SYMLINK_NOFOLLOW) })
}

/// `utimensat(dirfd, leaf, times, AT_SYMLINK_NOFOLLOW)`.
pub fn utimensat_nofollow(dirfd: RawFd, leaf: &[u8], mtime: u64) -> io::Result<()> {
    let c = cleaf(leaf)?;
    let t = [Timespec { tv_sec: mtime as c_long, tv_nsec: 0 }, Timespec { tv_sec: mtime as c_long, tv_nsec: 0 }];
    check(unsafe { utimensat(dirfd, c.as_ptr(), t.as_ptr(), AT_SYMLINK_NOFOLLOW) })
}
