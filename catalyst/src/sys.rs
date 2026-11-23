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

/// Whether `name` in the directory `dir` is a mount point, a bind mount of
/// the same file system included, where the kernel says (`openat2` with
/// `RESOLVE_NO_XDEV`, Linux 5.6). `None`: it does not say.
pub fn is_mount_point(dir: &std::fs::File, name: &std::ffi::CStr) -> Option<bool> {
    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }
    const SYS_OPENAT2: c_long = 437;
    const RESOLVE_NO_XDEV: u64 = 0x01;
    let how = OpenHow { flags: (libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64, mode: 0, resolve: RESOLVE_NO_XDEV };
    let fd = unsafe {
        libc::syscall(SYS_OPENAT2, dir.as_raw_fd(), name.as_ptr(), &how as *const OpenHow, std::mem::size_of::<OpenHow>())
    };
    if fd >= 0 {
        unsafe { libc::close(fd as c_int) };
        return Some(false);
    }
    match io::Error::last_os_error().raw_os_error() {
        Some(libc::EXDEV) => Some(true),
        _ => None,
    }
}

/// A mount of this process's mount namespace, as `/proc/self/mountinfo` has
/// it: its id, the device of its file system, the directory of that file
/// system that is its root, and where it is mounted.
#[derive(Debug, PartialEq)]
pub struct Mount {
    pub id: u64,
    pub dev: Vec<u8>,
    pub root: std::path::PathBuf,
    pub at: std::path::PathBuf,
}

/// The mounts of this process's mount namespace, bind mounts of single
/// files included. `None`: they cannot be read (no `/proc`).
pub fn mounts() -> Option<Vec<Mount>> {
    Some(parse_mountinfo(&std::fs::read("/proc/self/mountinfo").ok()?))
}

/// The first, third, fourth and fifth field of every line of a
/// `mountinfo`, the octal escapes of the paths undone.
fn parse_mountinfo(text: &[u8]) -> Vec<Mount> {
    fn path(p: &[u8]) -> std::path::PathBuf {
        let mut out = Vec::with_capacity(p.len());
        let mut i = 0;
        while i < p.len() {
            let oct = |d: u8| (b'0'..=b'7').contains(&d);
            if p[i] == b'\\' && i + 3 < p.len() && oct(p[i + 1]) && oct(p[i + 2]) && oct(p[i + 3]) {
                out.push(((p[i + 1] - b'0') << 6) | ((p[i + 2] - b'0') << 3) | (p[i + 3] - b'0'));
                i += 4;
            } else {
                out.push(p[i]);
                i += 1;
            }
        }
        std::path::PathBuf::from(std::ffi::OsStr::from_bytes(&out))
    }
    let mut v = Vec::new();
    for line in text.split(|&c| c == b'\n') {
        let f: Vec<&[u8]> = line.split(|&c| c == b' ').take(5).collect();
        let id = f.first().and_then(|x| std::str::from_utf8(x).ok()).and_then(|x| x.parse().ok());
        if let (5, Some(id)) = (f.len(), id) {
            v.push(Mount { id, dev: f[2].to_vec(), root: path(f[3]), at: path(f[4]) });
        }
    }
    v
}

/// Whether something is mounted on the directory at `abs` (an absolute,
/// canonical path of something that is there) or anywhere in it, or the
/// directory, or one in it, is itself the root of a mount somewhere else (a
/// bind mount of it): then it is reached by another path than its own, and
/// must stay where it is. "Cannot tell" counts as yes.
pub fn mounted_in_or_from(mounts: &[Mount], abs: &Path) -> bool {
    match mount_id(abs) {
        Some(id) => in_or_from(mounts, abs, id),
        None => true,
    }
}

/// [`mounted_in_or_from`] for a directory that is in the mount with id `id`
/// (the kernel says which one that is: the path does not, where a mount is
/// hidden under a later one on a directory above it).
fn in_or_from(mounts: &[Mount], abs: &Path, id: u64) -> bool {
    if mounts.iter().any(|m| m.at.starts_with(abs)) {
        return true;
    }
    let Some(m) = mounts.iter().find(|m| m.id == id) else { return true };
    // where it is inside its file system
    let Ok(rest) = abs.strip_prefix(&m.at) else { return true };
    let inside = m.root.join(rest);
    mounts.iter().any(|x| x.dev == m.dev && x.id != m.id && x.root.starts_with(&inside))
}

/// What `statx` gives, as far as it is used here (256 bytes).
#[repr(C)]
struct Statx {
    mask: u32,
    blksize: u32,
    attributes: u64,
    nlink: u32,
    uid: u32,
    gid: u32,
    mode: u16,
    pad: u16,
    ino: u64,
    size: u64,
    blocks: u64,
    attributes_mask: u64,
    times: [u64; 8],
    rdev: [u32; 2],
    dev: [u32; 2],
    mnt_id: u64,
    rest: [u64; 13],
}

fn statx(path: &Path, mask: c_uint) -> Option<Statx> {
    let c = cstr(path).ok()?;
    let mut st: Statx = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::syscall(libc::SYS_statx, AT_FDCWD, c.as_ptr(), AT_SYMLINK_NOFOLLOW, mask, &mut st as *mut Statx) };
    (r == 0).then_some(st)
}

/// The id of the mount that the file or directory at `path` (not followed)
/// is in, as `mountinfo` numbers them. `None`: the kernel does not say
/// (before Linux 5.8).
pub fn mount_id(path: &Path) -> Option<u64> {
    const STATX_MNT_ID: c_uint = 0x1000;
    let st = statx(path, STATX_MNT_ID)?;
    (st.mask & STATX_MNT_ID != 0).then_some(st.mnt_id)
}

/// Whether the file or directory at `path` (not followed) is marked
/// immutable or append-only, so that it cannot be removed. "Cannot tell"
/// counts as yes: no answer, or a file system that does not report these
/// marks.
pub fn is_protected(path: &Path) -> bool {
    const MARKS: u64 = 0x10 | 0x20; // STATX_ATTR_IMMUTABLE, STATX_ATTR_APPEND
    match statx(path, 0) {
        Some(st) => st.attributes_mask & MARKS != MARKS || st.attributes & MARKS != 0,
        None => true,
    }
}

/// Takes an exclusive lock, waiting for whoever holds it.
pub fn lock_wait(f: &std::fs::File) -> io::Result<()> {
    loop {
        if unsafe { flock(f.as_raw_fd(), LOCK_EX) } == 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// `rename` that fails with `EEXIST` where something is at `to` already,
/// instead of replacing it (`renameat2` with `RENAME_NOREPLACE`).
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    const RENAME_NOREPLACE: c_uint = 1;
    let (f, t) = (cstr(from)?, cstr(to)?);
    let r = unsafe { libc::syscall(libc::SYS_renameat2, AT_FDCWD, f.as_ptr(), AT_FDCWD, t.as_ptr(), RENAME_NOREPLACE) };
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// For a child between fork and exec: every descriptor above `keep` is
/// closed at the exec. Only async-signal-safe calls.
pub fn cloexec_above(keep: c_int) {
    const SYS_CLOSE_RANGE: c_long = 436;
    const CLOSE_RANGE_CLOEXEC: c_uint = 4;
    let r = unsafe { libc::syscall(SYS_CLOSE_RANGE, (keep + 1) as c_uint, c_uint::MAX, CLOSE_RANGE_CLOEXEC) };
    if r != 0 {
        // a kernel before 5.11: one by one
        for fd in keep + 1..1024 {
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
    }
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

#[cfg(test)]
mod mount_tests {
    use super::*;

    /// The mounts of a `mountinfo`: id, device, root and mount point, with
    /// the escapes for space, tab, newline and backslash undone; and what
    /// that says about a directory.
    #[test]
    fn mounts_are_read_from_mountinfo() {
        let text = b"22 27 0:21 / /proc rw,nosuid shared:13 - proc proc rw\n\
            35 27 0:30 /@ / rw,noatime shared:1 - btrfs /dev/sda2 rw\n\
            90 35 0:30 /@/a\\040b /mnt/with\\040space\\011tab\\134x rw - btrfs /dev/sda2 rw\n\
            91 35 8:1 / /boot rw - vfat /dev/sda1 rw\n\
            92 35 0:30 /@/usr/share/shared/sub /srv/x rw - btrfs /dev/sda2 rw\n\
            93 35 0:30 /@/etc/hosts /usr/share/app/hosts rw - btrfs /dev/sda2 rw\n\
            94 35 0:31 /@/usr/share/other /srv/y rw - btrfs /dev/sdb1 rw\n\
            95 35 8:3 / /hidden/data rw - ext4 /dev/sdc1 rw\n\
            96 35 8:4 / /hidden rw - ext4 /dev/sdd1 rw\n\
            97 35 8:4 /data/pkg/share /mnt/view rw - ext4 /dev/sdd1 rw\n\
            short line\n";
        let m = parse_mountinfo(text);
        assert_eq!(m.len(), 10);
        assert_eq!(m[2], Mount { id: 90, dev: b"0:30".to_vec(), root: "/@/a b".into(), at: "/mnt/with space\ttab\\x".into() });
        // in the root mount (35)
        let no = |p: &str| !in_or_from(&m, Path::new(p), 35);
        // nothing on it, in it, or from it
        assert!(no("/usr/share/plain") && no("/usr/share/other"), "another device's root of the same name does not count");
        // a mount on it, and one in it (a single file)
        assert!(!no("/boot") && !no("/usr/share/app") && !no("/usr/share"));
        // it, or a directory in it, is the root of a mount somewhere else
        assert!(!no("/usr/share/shared/sub") && !no("/usr/share/shared") && no("/usr/share/shared/sub/deeper"));
        // A mount hidden under a later one on a directory above it (95
        // under 96): the directory is in 96, whatever the paths look like,
        // and there it is the root of the mount at /mnt/view.
        assert!(in_or_from(&m, Path::new("/hidden/data/pkg/share"), 96));
        assert!(!in_or_from(&m, Path::new("/hidden/data/pkg/share"), 95), "by the path alone it would have passed");
        assert!(!in_or_from(&m, Path::new("/hidden/data/pkg/other"), 96));
        // a mount that is not listed: nothing can be said
        assert!(in_or_from(&m, Path::new("/usr/share/plain"), 1234));
        // this process: its root is in a mount that is listed
        if let Some(m) = mounts() {
            if let Some(id) = mount_id(Path::new("/")) {
                assert!(m.iter().any(|x| x.id == id && x.at == Path::new("/")));
            }
        }
    }

    /// A file that is not marked is not protected; one marked immutable is
    /// (where this can be tried: as root, on a file system that has the
    /// mark); one that cannot be asked about counts as protected.
    #[test]
    fn what_is_marked_or_cannot_be_asked_about_counts_as_protected() {
        let p = std::env::temp_dir().join(format!("catalyst-protected-{}", std::process::id()));
        std::fs::write(&p, b"").unwrap();
        let reported = !is_protected(&p);
        let chattr = |flag: &str| std::process::Command::new("chattr").arg(flag).arg(&p).status().map_or(false, |s| s.success());
        if reported && chattr("+i") {
            assert!(is_protected(&p), "an immutable file");
            assert!(chattr("-i"));
            assert!(!is_protected(&p));
        } else {
            eprintln!("no immutable file could be made here: that part is not tested");
        }
        std::fs::remove_file(&p).unwrap();
        assert!(is_protected(&p), "nothing there to ask about");
    }
}
