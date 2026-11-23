//! Hints: files asked for before they are read.
//!
//! From a cold cache a run waits for the disk at each file it opens, one
//! file after another: the keys, a repository's index, the parsed copy of
//! it, the segments of the index of installed files. Most of them are known
//! well before they are read. Here they are named to the system early, on a
//! thread of its own, so that the disk fetches them while the run does
//! something else (README, "Files asked for ahead").
//!
//! A hint decides nothing. It is `posix_fadvise(POSIX_FADV_WILLNEED)` on a
//! regular file, or a read whose result is dropped, of a file that the run
//! reads itself a moment later and checks then as it always did. A hint
//! that fails, comes late, or names a file that is never read costs a
//! little time on another thread and nothing else; no error of a hint is
//! reported. Nothing is written, made or locked. A file asked for by its
//! path is opened without setting its time of last access where the
//! system allows that; the keys of a root are read as the run reads them a
//! moment later.
//!
//! The threads of hints make file system calls only, and none into a
//! library that tidies up when the process ends (the cryptographic one
//! does): the process may end while they run. What a hint needs of such a
//! library, a hash for a file's name, the run works out before it gives
//! the hint.
//!
//! `CATALYST_HINTS=0` turns the hints off, to measure a run without them.
//! `CATALYST_HINTS=show` gives every hint on the thread that asks for it,
//! in the order asked, and names each file that was asked for on stderr
//! (`hint: PATH`): for tests, and for seeing what a run asks for.

use std::collections::{HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

type Job = Box<dyn FnOnce() + Send>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Off,
    On,
    Show,
}

fn mode() -> Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    *MODE.get_or_init(|| match std::env::var_os("CATALYST_HINTS") {
        Some(v) if v == "0" => Mode::Off,
        Some(v) if v == "show" => Mode::Show,
        _ => Mode::On,
    })
}

/// Whether hints are given in this process (`CATALYST_HINTS=0`: none).
pub fn enabled() -> bool {
    mode() != Mode::Off
}

/// Says on stderr that `path` was asked for, when hints are shown.
pub fn named(path: &Path) {
    if mode() == Mode::Show {
        use std::io::Write;
        // not eprintln!: a stderr that cannot be written to must not stop
        // a run because its hints were to be shown
        let _ = writeln!(std::io::stderr(), "hint: {}", path.display());
    }
}

/// Runs a hint where it is: one that fails is a hint not given.
fn run_here(job: impl FnOnce()) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
}

/// The hints that wait for the thread of hints, in the order given.
struct Queue {
    jobs: Mutex<VecDeque<Job>>,
    more: Condvar,
}

/// The queue of the one thread of hints, which is started when the first
/// hint is given. `None` when it could not be started: then no hints are
/// given.
fn queue() -> Option<&'static Queue> {
    static QUEUE: OnceLock<Option<&'static Queue>> = OnceLock::new();
    *QUEUE.get_or_init(|| {
        // never freed: the thread uses it until the process ends
        let q: &'static Queue = Box::leak(Box::new(Queue { jobs: Mutex::new(VecDeque::new()), more: Condvar::new() }));
        std::thread::Builder::new()
            .name("hints".into())
            .spawn(move || loop {
                let job = {
                    let mut jobs = q.jobs.lock().unwrap_or_else(|e| e.into_inner());
                    loop {
                        match jobs.pop_front() {
                            Some(job) => break job,
                            None => jobs = q.more.wait(jobs).unwrap_or_else(|e| e.into_inner()),
                        }
                    }
                };
                // the ones after a hint that fails are given all the same
                run_here(job);
            })
            .ok()
            .map(|_| q)
    })
}

/// Gives `job` to the thread of hints (when hints are shown: runs it
/// here); false when it will not run.
fn push(job: Job) -> bool {
    match mode() {
        Mode::Off => false,
        Mode::Show => {
            run_here(job);
            true
        }
        Mode::On => {
            let Some(q) = queue() else { return false };
            q.jobs.lock().unwrap_or_else(|e| e.into_inner()).push_back(job);
            q.more.notify_one();
            true
        }
    }
}

/// Runs `job` on the thread of hints, after those given before it. `job`
/// may only give hints ([`advise`], [`read_through`], reads that are
/// dropped): it runs beside the run, in no order with it, and may be cut
/// off by the end of the process.
pub fn later(job: impl FnOnce() + Send + 'static) {
    push(Box::new(job));
}

/// Runs `job` on a thread of its own, at once. For a hint that is a row of
/// waits (a directory, an entry of it, a file), given from the thread of
/// hints so that the others do not wait behind it; the same rules as for
/// [`later`].
pub fn beside(job: impl FnOnce() + Send + 'static) {
    if mode() == Mode::Show {
        run_here(job);
    } else {
        let _ = std::thread::Builder::new().name("hints".into()).spawn(job);
    }
}

/// Returns when every hint given to [`later`] so far has run (for tests).
/// It does not wait for what such a hint started with [`beside`].
pub fn settle() {
    let done = Arc::new((Mutex::new(false), Condvar::new()));
    let d = done.clone();
    let queued = push(Box::new(move || {
        *d.0.lock().unwrap_or_else(|e| e.into_inner()) = true;
        d.1.notify_all();
    }));
    if queued {
        let mut reached = done.0.lock().unwrap_or_else(|e| e.into_inner());
        while !*reached {
            reached = done.1.wait(reached).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// `path` opened for reading if it is a regular file. Nothing else is
/// opened: opening a FIFO could wait for a writer, opening a device could
/// do something. Opened without setting the time of last access where
/// that is allowed (to the file's owner and to root): the run's own read
/// sets it, as ever.
fn regular(path: &Path) -> Option<File> {
    if !fs::metadata(path).map_or(false, |m| m.is_file()) {
        return None;
    }
    let quiet = libc::O_NONBLOCK | libc::O_NOCTTY;
    let open = |flags: i32| OpenOptions::new().read(true).custom_flags(flags).open(path);
    let f = open(quiet | libc::O_NOATIME).or_else(|_| open(quiet)).ok()?;
    f.metadata().map_or(false, |m| m.is_file()).then_some(f)
}

/// Asks the system to fetch the regular file `path`; does not wait for it.
pub fn advise(path: &Path) {
    if let Some(f) = regular(path) {
        named(path);
        // SAFETY: a call on a descriptor that is open here; it reads
        // nothing into this process
        unsafe {
            libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_WILLNEED);
        }
    }
}

/// The regular file `path` read to its end (64 MiB of it at most) and
/// dropped: for a big file, of which the system fetches only a first part
/// when asked, and on a file system that takes no advice.
pub fn read_through(path: &Path) {
    let Some(mut f) = regular(path) else { return };
    named(path);
    let mut buf = vec![0u8; 256 * 1024];
    let mut left: u64 = 64 << 20;
    while left > 0 {
        match f.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => left = left.saturating_sub(n as u64),
        }
    }
}

/// The files of one directory, each asked for once at most: several
/// packages unpacked at a time name the same files.
pub struct Files {
    dir: PathBuf,
    asked: Mutex<HashSet<String>>,
}

impl Files {
    pub fn new(dir: PathBuf) -> Files {
        Files { dir, asked: Mutex::new(HashSet::new()) }
    }

    /// Asks for those of `names` that were not asked for before; returns
    /// how many those were.
    pub fn ask(&self, names: Vec<String>) -> usize {
        let new: Vec<String> = {
            let mut asked = self.asked.lock().unwrap_or_else(|e| e.into_inner());
            names.into_iter().filter(|n| asked.insert(n.clone())).collect()
        };
        let n = new.len();
        if n > 0 {
            let dir = self.dir.clone();
            later(move || {
                for name in &new {
                    advise(&dir.join(name));
                }
            });
        }
        n
    }

    /// The names asked for so far, sorted.
    pub fn asked(&self) -> Vec<String> {
        let mut v: Vec<String> = self.asked.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("catalyst-warm-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn only_a_regular_file_is_opened_for_a_hint() {
        let d = dir("regular");
        fs::write(d.join("file"), vec![7u8; 300_000]).unwrap();
        fs::create_dir(d.join("dir")).unwrap();
        std::os::unix::fs::symlink("nowhere", d.join("dangling")).unwrap();
        std::os::unix::fs::symlink("file", d.join("link")).unwrap();
        let fifo = std::ffi::CString::new(d.join("fifo").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(regular(&d.join("file")).is_some());
        // a link to a regular file is the file, as for the read that follows
        assert!(regular(&d.join("link")).is_some());
        for other in ["dir", "dangling", "fifo", "missing"] {
            assert!(regular(&d.join(other)).is_none(), "{other}");
        }
        // none of these waits (a FIFO opened plainly would, for a writer)
        for name in ["file", "link", "dir", "dangling", "fifo", "missing"] {
            advise(&d.join(name));
            read_through(&d.join(name));
        }
        fs::remove_dir_all(&d).unwrap();
    }

    /// How many pages of `f` (of `len` bytes) are in memory.
    fn in_memory(f: &File, len: usize) -> usize {
        // SAFETY: a mapping of our own file, looked at and unmapped again
        unsafe {
            let page = libc::sysconf(libc::_SC_PAGESIZE) as usize;
            let m = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, f.as_raw_fd(), 0);
            assert!(m != libc::MAP_FAILED);
            let mut v = vec![0u8; (len + page - 1) / page];
            assert_eq!(libc::mincore(m, len, v.as_mut_ptr()), 0);
            libc::munmap(m, len);
            v.iter().filter(|&&b| b & 1 != 0).count()
        }
    }

    /// A hint does what it is for: a file that is not in memory is there
    /// soon after it was asked for, and whole once it was read through.
    #[test]
    fn a_hint_brings_a_file_into_memory() {
        let d = dir("effect");
        let p = d.join("big");
        let len = 4usize << 20;
        let pages = len / unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize };
        {
            let mut f = File::create(&p).unwrap();
            f.write_all(&vec![5u8; len]).unwrap();
            f.sync_all().unwrap();
        }
        let f = File::open(&p).unwrap();
        let forget = || unsafe { libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
        // Where this can be seen at all: a plain read leaves the file in
        // memory (not every file system keeps what is read), and it can be
        // let go of again (not on tmpfs).
        forget();
        let let_go = in_memory(&f, len) <= pages / 2;
        fs::read(&p).unwrap();
        let kept = in_memory(&f, len) > 0;
        forget();
        let left = in_memory(&f, len);
        if !let_go || !kept || left > pages / 2 {
            eprintln!("SKIPPED: this file system does not show what of a file is in memory");
            fs::remove_dir_all(&d).unwrap();
            return;
        }
        advise(&p);
        // asked for, not waited for: more of it is there within moments
        let t0 = std::time::Instant::now();
        while in_memory(&f, len) <= left && t0.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(in_memory(&f, len) > left, "asked for, and no page came to the {left} of {pages} that were there");
        forget();
        read_through(&p);
        assert!(in_memory(&f, len) >= pages - pages / 8, "read through, and {} of {pages} pages are there", in_memory(&f, len));
        fs::remove_dir_all(&d).unwrap();
    }

    /// A hint leaves a file's time of last access as it was (its owner may
    /// open it so). On a file system mounted `noatime` no read sets that
    /// time, and this shows nothing.
    #[test]
    fn a_hint_does_not_set_the_time_of_last_access() {
        let d = dir("atime");
        let p = d.join("file");
        fs::write(&p, vec![1u8; 100_000]).unwrap();
        // three days old and not after the last change: a plain read would set it
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 86400);
        File::options().write(true).open(&p).unwrap().set_times(fs::FileTimes::new().set_accessed(old).set_modified(old)).unwrap();
        let before = fs::metadata(&p).unwrap().accessed().unwrap();
        advise(&p);
        read_through(&p);
        assert_eq!(fs::metadata(&p).unwrap().accessed().unwrap(), before);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_file_of_a_directory_is_asked_for_once() {
        let d = dir("once");
        for n in ["a", "b", "c"] {
            fs::write(d.join(n), n).unwrap();
        }
        let files = Files::new(d.clone());
        assert_eq!(files.ask(vec!["a".into(), "b".into()]), 2);
        assert_eq!(files.ask(vec!["b".into(), "c".into(), "not-there".into()]), 2);
        assert_eq!(files.ask(vec!["a".into(), "c".into(), "not-there".into()]), 0);
        assert_eq!(files.ask(Vec::new()), 0);
        assert_eq!(files.asked(), ["a", "b", "c", "not-there"]);
        settle();
        fs::remove_dir_all(&d).unwrap();
    }

    /// Hints run in the order given, and one that fails does not keep the
    /// ones after it from running.
    #[test]
    fn hints_run_in_order_and_past_one_that_fails() {
        if mode() != Mode::On {
            return;
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        for i in 0..5 {
            let seen = seen.clone();
            later(move || {
                if i == 2 {
                    // unwinds without the message a panic prints
                    std::panic::resume_unwind(Box::new(()));
                }
                seen.lock().unwrap().push(i);
            });
        }
        // not settle(): if the hint that fails took the thread with it,
        // that would wait for ever
        let (tx, rx) = std::sync::mpsc::channel();
        later(move || {
            let _ = tx.send(());
        });
        assert!(rx.recv_timeout(std::time::Duration::from_secs(20)).is_ok(), "the hints after the one that failed did not run");
        let seen = seen.lock().unwrap().clone();
        // other tests give hints to the same thread, but these five keep their order
        assert_eq!(seen, [0, 1, 3, 4]);
    }
}
