//! Thin wrappers over the few libc calls the standard library does not
//! offer (Linux, glibc or musl): device nodes, xattrs, timestamps without
//! following symlinks, `syncfs`, `flock`, `fnmatch`, terminal queries and
//! `chroot`/`umask` for script children.

use std::ffi::CString;
use std::io;
use std::os::raw::{c_char, c_int, c_long, c_uint, c_ulong};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::AsRawFd;
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

extern "C" {
    fn mknod(path: *const c_char, mode: c_uint, dev: u64) -> c_int;
    fn fnmatch(pattern: *const c_char, string: *const c_char, flags: c_int) -> c_int;
    fn syncfs(fd: c_int) -> c_int;
    fn flock(fd: c_int, op: c_int) -> c_int;
    fn isatty(fd: c_int) -> c_int;
    fn ioctl(fd: c_int, req: c_ulong, ...) -> c_int;
    fn umask(mask: c_uint) -> c_uint;
    fn lsetxattr(path: *const c_char, name: *const c_char, value: *const u8, size: usize, flags: c_int) -> c_int;
    fn utimensat(dirfd: c_int, path: *const c_char, times: *const Timespec, flags: c_int) -> c_int;
    fn geteuid() -> c_uint;
    fn chroot(path: *const c_char) -> c_int;
    fn chdir(path: *const c_char) -> c_int;
}

const AT_FDCWD: c_int = -100;
const AT_SYMLINK_NOFOLLOW: c_int = 0x100;
const FNM_PATHNAME: c_int = 1;
const LOCK_EX: c_int = 2;
const LOCK_NB: c_int = 4;
const TIOCGWINSZ: c_ulong = 0x5413;

pub fn cstr(p: &Path) -> io::Result<CString> {
    CString::new(p.as_os_str().as_bytes()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
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
pub fn sync_fs(f: &std::fs::File) -> io::Result<()> {
    check(unsafe { syncfs(f.as_raw_fd()) })
}

/// Takes an exclusive lock without waiting; `Ok(false)` if it is held.
pub fn try_lock(f: &std::fs::File) -> io::Result<bool> {
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

/// Whether directory `path` has a default ACL (which a directory made in
/// it inherits, with a mode worked out from it instead of from the umask).
/// "Cannot tell" counts as yes.
pub fn has_default_acl(path: &Path) -> bool {
    let Ok(c) = cstr(path) else { return true };
    let r = unsafe { libc::lgetxattr(c.as_ptr(), b"system.posix_acl_default\0".as_ptr() as *const c_char, std::ptr::null_mut(), 0) };
    if r >= 0 {
        return true;
    }
    // no such attribute, or a file system without ACLs
    !matches!(io::Error::last_os_error().raw_os_error(), Some(libc::ENODATA) | Some(libc::EOPNOTSUPP))
}

/// The kind of work [`name_threads`] is asked about.
#[derive(Clone, Copy)]
pub enum NameWork {
    /// hard links and renames into a tree
    Link,
    /// unlinking names of files that keep another name
    Unlink,
    /// deleting whole trees: the last names of their files
    Trees,
}

const BTRFS_SUPER_MAGIC: u64 = 0x9123_683E;
const XFS_SUPER_MAGIC: u64 = 0x5846_5342;

/// The magic number of the file system that holds `path`.
fn fs_magic(path: &Path) -> Option<u64> {
    let c = cstr(path).ok()?;
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some(st.f_type as u64 & 0xffff_ffff)
}

/// How many threads at most are worth it for work that makes or removes
/// names on the file system that holds `path` (`usize::MAX`: as many as the
/// caller would use anyway).
///
/// It depends on the file system. Btrfs keeps all the names of a subvolume
/// in one tree, and threads that change it wait for each other; XFS frees
/// inodes no faster on several threads. Measured on Linux 6.18 with 12
/// CPUs, each file system on the same disk image, Cataclysm's 8,500 entries
/// (milliseconds on 1, 2, 4 and 16 threads):
///
/// |                         | ext4           | XFS               | Btrfs (zstd:1)    |
/// |-------------------------|----------------|-------------------|-------------------|
/// | linking them into place | 28, 17, 11, 11 | 33, 20, 15, 15    | 83, 67, 114, 173  |
/// | unlinking them there    | 41, 28, 21, 20 | 46, 31, 25, 24    | 121, 145, 314, 275|
/// | deleting the old tree   | 77, 57, 41, 40 | 137, 170, 162, 167| 120, 210, 474, 522|
///
/// So: as many threads as there are on ext4 and on what was not measured;
/// on XFS a tree is deleted on one; on Btrfs links are made on two and
/// everything is removed on one.
///
/// `CATALYST_LINK_THREADS`, `CATALYST_UNLINK_THREADS` and
/// `CATALYST_TREE_THREADS` set the number (for measuring on another file
/// system or kernel).
pub fn name_threads(path: &Path, work: NameWork) -> usize {
    let var = match work {
        NameWork::Link => "CATALYST_LINK_THREADS",
        NameWork::Unlink => "CATALYST_UNLINK_THREADS",
        NameWork::Trees => "CATALYST_TREE_THREADS",
    };
    if let Some(n) = std::env::var(var).ok().and_then(|v| v.parse::<usize>().ok()).filter(|&n| n >= 1) {
        return n;
    }
    match (fs_magic(path), work) {
        (Some(BTRFS_SUPER_MAGIC), NameWork::Link) => 2,
        (Some(BTRFS_SUPER_MAGIC), _) => 1,
        (Some(XFS_SUPER_MAGIC), NameWork::Trees) => 1,
        _ => usize::MAX,
    }
}

/// The umask the process was started with (0o777 while it is not known:
/// then nothing may be assumed about what `mkdir` leaves of a mode).
static UMASK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0o777);

/// Reads the process's umask (it can only be read by setting it, so this is
/// for the very start of the program, before any thread exists).
pub fn note_umask() {
    unsafe {
        let u = umask(0o022);
        umask(u);
        UMASK.store(u & 0o777, AtomicOrdering::Relaxed);
    }
}

pub fn umask_at_start() -> u32 {
    UMASK.load(AtomicOrdering::Relaxed)
}

/// The descriptor that holds the root's lock (-1: none). Package scripts
/// inherit it, so that a script left running after catalyst itself was killed
/// keeps the root locked until it ends (no second catalyst can start the same
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
/// (the `CATALYST_CRASH_AT` test hook; `abort()` would go through a core-dump
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
pub fn open_regular_nofollow(p: &Path) -> io::Result<std::fs::File> {
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
pub fn open_dir_nofollow(p: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(p)
}
