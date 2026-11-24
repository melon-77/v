//! Old versions deleted after the command has returned.
//!
//! When a change replaces or removes a big package, deleting the old
//! version's tree is about half of the work (every file has a name there
//! and one in the live tree), and nothing waits for it: the tree is out of
//! use once the switch is over. Where it is asked for
//! ([`crate::commit::SwitchOptions::clean_later`]) such trees are moved,
//! one rename each, into `var/lib/catalyst/trash/<entry>/`, and a process
//! of its own (`oct empty-trash`, started detached) deletes them while the
//! command has already returned.
//!
//! Emptying needs no lock on the root and touches nothing but what was moved
//! into the trash. (Putting an unchecked directory back, below, is for a
//! run that holds the root's lock.) Whoever fills or empties an entry
//! holds a `flock` on its directory. An entry that nobody holds is one
//! whose process died (a kill, a power cut): the next run that changes the
//! root, or `oct recover`, deletes it.
//!
//! Besides old versions, directories of the live tree that held nothing but
//! a removed package's own entries are moved here whole (commit.rs,
//! `move_whole_dirs`). Such a directory is `N.unchecked`, with a link
//! `N.from` that says where it was, until it has been looked through once
//! more after the move; only then is it `N` and may be deleted. One found
//! `unchecked` by the run that finishes a journal was in flight when its
//! run died: it is put back where it was before the journal is finished
//! ([`settle_unchecked`]). One that could not be put back is `N.stuck`;
//! every run that changes the root says that it is there, and what to do
//! with it is the administrator's to decide (it holds a removed package's
//! files, and perhaps a file that is not the package's). Neither is ever
//! deleted here.
//!
//! The trash is used only as a real directory of catalyst's own: on the
//! state directory's file system, not a mount point, root's alone. A
//! symlink or a file under its name is removed, never followed; anything
//! else that is not that directory is left alone, with a warning.

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

use crate::db::Db;
use crate::sys;

/// The trash of a root.
pub fn dir(root: &Path) -> PathBuf {
    Db::state(root).join("trash")
}

/// The trash, checked (see the module's last paragraph); with `make`, made
/// first if it is not there; with `clean`, a non-directory under its name is
/// removed (false for read-only `--simulate` queries). `None`: there is none,
/// or it is not to be used.
fn base(root: &Path, make: bool, clean: bool) -> Option<PathBuf> {
    base_how(root, make, clean, &sys::is_mount_point, &sys::mounts)
}

/// [`base`], given the two things it asks of the system (the tests give
/// other answers): whether the kernel says that a name in a directory is a
/// mount point, and the mounts.
fn base_how(
    root: &Path,
    make: bool,
    clean: bool,
    says: &dyn Fn(&File, &std::ffi::CStr) -> Option<bool>,
    mounts: &dyn Fn() -> Option<Vec<sys::Mount>>,
) -> Option<PathBuf> {
    let state = Db::state(root);
    let b = dir(root);
    let mut made = false;
    if make {
        match fs::DirBuilder::new().mode(0o700).create(&b) {
            Ok(()) => made = true,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }
    }
    let m = fs::symlink_metadata(&b).ok()?;
    if !m.is_dir() {
        if clean {
            // not ours: removed under this name, never followed
            let _ = fs::remove_file(&b);
        }
        return None;
    }
    let st = fs::symlink_metadata(&state).ok()?;
    let name = std::ffi::CStr::from_bytes_with_nul(b"trash\0").unwrap();
    // Whether the trash is a mount point. Where the kernel does not say (no
    // `openat2`: Linux before 5.6, or a filter that forbids the call),
    // `mountinfo` does, on every kernel: what is mounted on the trash, or
    // in it, is listed there by its path. Where that gives no answer
    // either, nothing tells, and the trash is not used.
    let mounted = match File::open(&state).ok().and_then(|d| says(&d, name)) {
        Some(v) => Some(v),
        None => match (fs::canonicalize(&b), mounts()) {
            (Ok(abs), Some(ms)) => Some(sys::mounted_in(&ms, &abs)),
            _ => None,
        },
    };
    let ours = st.is_dir() && st.dev() == m.dev() && mounted == Some(false) && m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o022 == 0;
    if !ours {
        if made {
            // made a moment ago for this and not to be used: not left
            // behind, where every later run would stop at it
            let _ = fs::remove_dir(&b);
        }
        let what = if made { "it is not used" } else { "left alone" };
        if mounted.is_none() {
            eprintln!(
                "oct: warning: whether something is mounted on {} cannot be told here (the kernel does not say, and /proc/self/mountinfo gives no answer): {what}",
                b.display()
            );
        } else {
            eprintln!(
                "oct: warning: {} is not a directory of catalyst's own (a mount, another owner, or others may write to it): {what}",
                b.display()
            );
        }
        return None;
    }
    Some(b)
}

/// An entry of the trash that this process holds while it fills it.
pub struct Entry {
    dir: PathBuf,
    _held: File,
    /// the next number
    n: usize,
    /// how many trees in it wait to be deleted
    waiting: usize,
}

impl Entry {
    /// A new entry, held. `None` where there can be none: the trash is not
    /// on the file system of `Programs` (a tree gets there by a rename), or
    /// it cannot be made. Nothing is left behind then.
    pub fn new(root: &Path) -> Option<Entry> {
        let base = base(root, true, true)?;
        let made = (|| {
            let same = fs::metadata(Db::programs(root)).ok()?.dev() == fs::symlink_metadata(&base).ok()?.dev();
            if !same {
                return None;
            }
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
            let d = base.join(format!("{}-{nanos:x}", std::process::id()));
            fs::DirBuilder::new().mode(0o700).create(&d).ok()?;
            let held = sys::open_dir_nofollow(&d).ok().filter(|f| sys::try_lock(f).unwrap_or(false));
            match held {
                Some(held) => Some(Entry { dir: d, _held: held, n: 0, waiting: 0 }),
                None => {
                    let _ = fs::remove_dir(&d);
                    None
                }
            }
        })();
        if made.is_none() {
            let _ = fs::remove_dir(&base);
        }
        made
    }

    /// Moves the tree at `tree` into the entry (one rename) and says where
    /// it is now. On an error it is where it was.
    pub fn take(&mut self, tree: &Path) -> io::Result<PathBuf> {
        let to = self.dir.join(self.n.to_string());
        fs::rename(tree, &to)?;
        self.n += 1;
        self.waiting += 1;
        Ok(to)
    }

    /// Moves the directory at `dir`, whose canonical place in the root is
    /// `from`, into the entry as one that is still to be looked through
    /// (`N.unchecked`); a link beside it (`N.from`) says where it was, so
    /// that a run that finds it there can put it back. Then one of
    /// [`Entry::accept`], [`Entry::gave_back`] or [`Entry::stuck`].
    pub fn take_unchecked(&mut self, dir: &Path, from: &[u8]) -> io::Result<PathBuf> {
        use std::os::unix::ffi::OsStrExt;
        let link = self.dir.join(format!("{}.from", self.n));
        std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(from), &link)?;
        let to = self.dir.join(format!("{}.unchecked", self.n));
        if let Err(e) = fs::rename(dir, &to) {
            let _ = fs::remove_file(&link);
            return Err(e);
        }
        self.n += 1;
        Ok(to)
    }

    /// The unchecked directory at `to` was looked through and holds nothing
    /// but what may be deleted.
    pub fn accept(&mut self, to: &Path) -> io::Result<()> {
        fs::rename(to, to.with_extension(""))?;
        let _ = fs::remove_file(to.with_extension("from"));
        self.waiting += 1;
        Ok(())
    }

    /// The unchecked directory that was at `to` is back where it came from.
    pub fn gave_back(&mut self, to: &Path) {
        let _ = fs::remove_file(to.with_extension("from"));
    }

    /// The unchecked directory at `to` could not be put back: it stays, as
    /// `N.stuck`, where nothing deletes it. Where it is now.
    pub fn stuck(&mut self, to: &Path) -> PathBuf {
        let s = to.with_extension("stuck");
        match fs::rename(to, &s) {
            Ok(()) => s,
            // as unchecked it is not deleted either
            Err(_) => to.to_path_buf(),
        }
    }

    /// Nothing in it that waits to be deleted.
    pub fn is_empty(&self) -> bool {
        self.waiting == 0
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        // nothing waits in it: no entry (unless something stuck is in it),
        // and no trash if it has no other
        if self.waiting == 0 {
            let _ = fs::remove_dir(&self.dir);
            if let Some(base) = self.dir.parent() {
                let _ = fs::remove_dir(base);
            }
        }
    }
}

/// The names of the entries in the trash (for `--simulate`).
pub fn entries(root: &Path) -> Vec<String> {
    let Some(base) = base(root, false, false) else { return Vec::new() };
    let mut v: Vec<String> =
        fs::read_dir(base).map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
    v.sort();
    v
}

/// What a name in an entry is: a tree to delete, one still to be looked
/// through, one that could not be put back, or the link of one of those.
fn held_back(name: &std::ffi::OsStr) -> bool {
    Path::new(name).extension().map_or(false, |x| x == "unchecked" || x == "stuck" || x == "from")
}

/// Whether the trash holds anything that a run could deal with: something
/// to delete, a directory still to be looked through, or nothing at all (it
/// is removed then). What is stuck waits for the administrator. Looks only:
/// nothing is removed and nothing is said.
pub fn has_work(root: &Path) -> bool {
    let b = dir(root);
    match fs::symlink_metadata(&b) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return true, // something else under the name: a run removes it
        Err(_) => return false,
    }
    let Ok(rd) = fs::read_dir(&b) else { return true };
    let mut entries = 0;
    for e in rd.flatten() {
        entries += 1;
        let Ok(inside) = fs::read_dir(e.path()) else { return true };
        let mut names = 0;
        for t in inside.flatten() {
            names += 1;
            let n = t.file_name();
            match Path::new(&n).extension() {
                Some(x) if x == "stuck" => {}
                Some(x) if x == "from" && !orphan_link(&t.path()) => {}
                _ => return true,
            }
        }
        if names == 0 {
            return true;
        }
    }
    entries == 0
}

/// A link `N.from` with neither `N.unchecked` nor `N.stuck` beside it: its
/// directory was accepted, given back or taken away, and the link was left.
/// (Only when both are known not to be there: an error says nothing.)
fn orphan_link(link: &Path) -> bool {
    let gone = |p: PathBuf| matches!(fs::symlink_metadata(p), Err(e) if e.kind() == io::ErrorKind::NotFound);
    gone(link.with_extension("unchecked")) && gone(link.with_extension("stuck"))
}

/// The directories still to be looked through (`N.unchecked`) in the
/// entries of the trash at `trash`, sorted; none if it is no directory.
/// Looks only, by name, and follows nothing.
fn waiting_unchecked(trash: &Path) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    match fs::symlink_metadata(trash) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Ok(found),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(found),
        Err(e) => return Err(e),
    }
    for e in fs::read_dir(trash)? {
        let p = e?.path();
        if !fs::symlink_metadata(&p).map_or(false, |m| m.is_dir()) {
            continue;
        }
        for t in fs::read_dir(&p)? {
            let t = t?.path();
            if t.extension().map_or(false, |x| x == "unchecked") {
                found.push(t);
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Restores `u` to canonical relative path `rel` inside `root` without
/// following any symlink in any ancestor component (walked with `openat` +
/// `O_DIRECTORY | O_NOFOLLOW`, then `renameat2(RENAME_NOREPLACE)` into the
/// opened parent descriptor). Refuses symlinks so recovery can never escape
/// `root` or redirect into catalyst's own directories. The root itself is
/// whatever it was given as: a symlink there is followed, as by every
/// other step of a run.
fn restore_confined(root: &Path, u: &Path, rel: &[u8]) -> io::Result<()> {
    let parts: Vec<&[u8]> = rel.split(|&c| c == b'/').collect();
    let Some((leaf, parents)) = parts.split_last() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty restore path"));
    };
    let mut dir = sys::open_dir(root)?;
    for comp in parents {
        let cname = std::ffi::CString::new(*comp)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in restore path"))?;
        dir = sys::open_dir_at_nofollow(&dir, &cname)?;
    }
    let cname = std::ffi::CString::new(*leaf)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in restore path"))?;
    sys::rename_noreplace_into(u, &dir, &cname)
}

/// Deals with the directories that a run moved out of the live tree and did
/// not come to look through (it was killed, or the power went). For a run
/// that holds the root's lock.
///
/// With `restore`, which is for the run that is about to finish that run's
/// journal: each `N.unchecked` goes back to the place its `N.from` names,
/// replacing nothing; the entries in it are then removed one by one with
/// the rest of the journal, as if no directory had been moved. One that
/// cannot be put back, and without `restore` every one (its journal is
/// gone: putting it back would bring a removed package's files into the
/// tree), becomes `N.stuck`, which nothing deletes and every run reports.
///
/// An entry that a process is emptying is waited for: it never touches
/// what is unchecked, and it ends.
///
/// An error means that an entry could not be gone through (not that a
/// directory could not be put back, which is dealt with here), or, with
/// `restore`, that a directory waits to be put back in a trash that is not
/// used, or that something mounted on the trash hides what is in it: with
/// `restore` the caller must not finish the journal then, or a directory
/// that was to go back would stay out of the tree.
pub fn settle_unchecked(root: &Path, restore: bool) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let Some(base) = base(root, false, true) else {
        // A trash that is there and is not used (`base` has said why). If a
        // directory that a stopped run moved out of the live tree waits in
        // it, that run's journal must not be finished without it: the
        // directory would stay out of the tree, with whatever is in it.
        // Looked for by name only: nothing in such a trash is touched.
        if restore {
            let t = dir(root);
            // Something mounted on the trash hides what is in it: whether a
            // directory waits under the mount cannot be seen. Asked as
            // `base` asks it: the kernel where it says, else `mountinfo`;
            // where neither tells, a trash on another device than the state
            // directory is taken for one. (Where the kernel says that it is
            // no mount point, another device is no sign of one: a btrfs
            // subvolume has its own.)
            let covered = match (fs::symlink_metadata(&t), fs::symlink_metadata(Db::state(root))) {
                (Ok(m), Ok(st)) if m.is_dir() => {
                    let name = std::ffi::CStr::from_bytes_with_nul(b"trash\0").unwrap();
                    match File::open(Db::state(root)).ok().and_then(|d| sys::is_mount_point(&d, name)) {
                        Some(v) => v,
                        None => match (fs::canonicalize(&t), sys::mounts()) {
                            (Ok(abs), Some(ms)) => sys::mounted_in(&ms, &abs),
                            _ => m.dev() != st.dev(),
                        },
                    }
                }
                _ => false,
            };
            if covered {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("something is mounted on {}, and a directory to be put back may be under it; unmount it and run again", t.display()),
                ));
            }
            let waiting = waiting_unchecked(&t)?;
            if let Some(u) = waiting.first() {
                let names = fs::read_link(u.with_extension("from")).map_or_else(|_| "nothing (it has no link)".into(), |f| format!("{:?}", f.display().to_string()));
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!(
                        "{} waits to be put back (its link names {names}; {} such in all), and the trash it is in is not used (the warning above says why). Look at every *.unchecked directory in the entries of {} and at the link beside it first: a trash that others could change may hold what catalyst did not put there, and the next run puts each of them where its link says. Then put right what the warning names and run again",
                        u.display(),
                        waiting.len(),
                        t.display()
                    ),
                ));
            }
        }
        return Ok(());
    };
    let mut first: Option<io::Error> = None;
    for e in fs::read_dir(&base)?.flatten() {
        let p = e.path();
        if !fs::symlink_metadata(&p).map_or(false, |m| m.is_dir()) {
            continue;
        }
        let inside = match fs::read_dir(&p) {
            Ok(i) => i,
            Err(e) => {
                first.get_or_insert(e);
                continue;
            }
        };
        let names: Vec<PathBuf> = inside.flatten().map(|t| t.path()).filter(|t| held_back(t.file_name().unwrap_or_default())).collect();
        if names.is_empty() {
            continue;
        }
        let held = match sys::open_dir_nofollow(&p) {
            Ok(h) => h,
            Err(e) => {
                first.get_or_insert(e);
                continue;
            }
        };
        if !sys::try_lock(&held).unwrap_or(false) {
            eprintln!("oct: waiting for the process that empties {}", p.display());
            if let Err(e) = sys::lock_wait(&held) {
                first.get_or_insert(e);
                continue;
            }
        }
        for u in &names {
            match u.extension() {
                Some(x) if x == "unchecked" => {}
                Some(x) if x == "from" => {
                    if orphan_link(u) {
                        let _ = fs::remove_file(u);
                    }
                    continue;
                }
                _ => continue,
            }
            let link = u.with_extension("from");
            let from = fs::read_link(&link).ok().filter(|f| {
                let b = f.as_os_str().as_bytes();
                !b.is_empty() && !b.starts_with(b"/") && !b.split(|&c| c == b'/').any(|c| c == b".." || c == b"." || c.is_empty()) && !crate::db::is_reserved(b)
            });
            let back = match (&from, restore) {
                (_, false) => Err(io::Error::new(io::ErrorKind::Other, "the change it belonged to is over")),
                (Some(f), true) => restore_confined(root, u, f.as_os_str().as_bytes()).map(|()| f.clone()),
                (None, true) => Err(io::Error::new(io::ErrorKind::NotFound, "nothing says where it was")),
            };
            match back {
                Ok(f) => {
                    let _ = fs::remove_file(&link);
                    eprintln!("oct: note: /{} was being removed when a run stopped; it is back, and its entries are removed one by one", f.display());
                }
                Err(e) => {
                    let s = u.with_extension("stuck");
                    let now = if fs::rename(u, &s).is_ok() { s } else { u.clone() };
                    eprintln!(
                        "oct: warning: {} was moved out of the live tree{} by a run that stopped and is not put back ({e}); it is not deleted: look at it",
                        now.display(),
                        from.map_or(String::new(), |f| format!(" (from /{})", f.display()))
                    );
                }
            }
        }
    }
    match first {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Deletes every entry of the trash that no process holds, and the trash
/// itself when nothing is left in it. How many entries were deleted. A
/// directory of the live tree that is still to be looked through, or that
/// could not be put back, is never deleted, and its entry stays with it.
pub fn empty(root: &Path) -> io::Result<usize> {
    let Some(base) = base(root, false, true) else { return Ok(0) };
    let most = sys::name_threads(&base, sys::NameWork::Trees);
    let mut n = 0;
    let mut first: Option<io::Error> = None;
    for e in fs::read_dir(&base)?.flatten() {
        let p = e.path();
        // only real directories are entries; anything else has no business here
        match fs::symlink_metadata(&p) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                let _ = fs::remove_file(&p);
                continue;
            }
            Err(_) => continue,
        }
        let Ok(held) = sys::open_dir_nofollow(&p) else { continue };
        if !sys::try_lock(&held).unwrap_or(false) {
            continue; // someone is filling or emptying it
        }
        let mut trees: Vec<PathBuf> = Vec::new();
        let mut kept = 0;
        let Ok(inside) = fs::read_dir(&p) else { continue };
        for t in inside.flatten() {
            if held_back(&t.file_name()) {
                let x = t.path();
                if x.extension().map_or(false, |x| x == "from") && orphan_link(&x) {
                    // its directory is gone from here: the link has nothing to say
                    let _ = fs::remove_file(&x);
                    continue;
                }
                if x.extension().map_or(false, |x| x == "stuck") {
                    eprintln!("oct: warning: {} was moved out of the live tree and was not put back; it is not deleted: look at it", x.display());
                }
                kept += 1;
                continue;
            }
            match fs::symlink_metadata(t.path()) {
                Ok(m) if m.is_dir() => trees.push(t.path()),
                Ok(_) => {
                    let _ = fs::remove_file(t.path());
                }
                Err(_) => {}
            }
        }
        match crate::commit::remove_trees(&trees, true, most) {
            Ok(()) if kept > 0 => {}
            Ok(()) => match fs::remove_dir(&p) {
                Ok(()) => n += 1,
                Err(e) => {
                    first.get_or_insert(e);
                }
            },
            Err(e) => {
                first.get_or_insert(e);
            }
        }
    }
    let _ = fs::remove_dir(&base);
    match first {
        Some(e) => Err(e),
        None => Ok(n),
    }
}

/// What `oct empty-trash` writes first: whoever started it knows by this
/// that it runs (see [`empty_later`]).
pub const STARTED: &[u8] = b"oct: emptying the trash\n";

/// Starts `oct --root ROOT empty-trash` as a process of its own and returns
/// once it runs: in a session of its own (no controlling terminal), with
/// nothing inherited but the environment (no descriptor above 2, so no lock
/// or pipe of whoever started oct is kept open by it). `false` when it did
/// not come to run (no `/proc`, or this program's file or libraries were
/// just replaced by the change itself): the caller empties the trash then.
pub fn empty_later(root: &Path) -> bool {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    // this very program, also when its file was just replaced or moved
    let mut cmd = Command::new("/proc/self/exe");
    cmd.arg("--root").arg(root).arg("empty-trash").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).current_dir("/");
    // SAFETY: only async-signal-safe calls between fork and exec
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            sys::cloexec_above(2);
            Ok(())
        });
    }
    let Ok(mut child) = cmd.spawn() else { return false };
    // it says so when it runs; an end of file instead means it never did
    let mut b = [0u8; 1];
    let started = child.stdout.take().map_or(false, |mut o| matches!(o.read(&mut b), Ok(1)));
    if !started {
        let _ = child.wait();
    }
    started
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("catalyst-trash-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(Db::state(&root)).unwrap();
        fs::create_dir_all(Db::programs(&root)).unwrap();
        root
    }

    /// A tree moved into an entry is deleted by `empty` once nobody holds
    /// the entry, and not before; the trash goes with its last entry.
    #[test]
    fn an_entry_is_emptied_once_nobody_holds_it() {
        let root = root("entry");
        let old = Db::programs(&root).join("a/1.0");
        fs::create_dir_all(old.join("usr/share/a")).unwrap();
        fs::write(old.join("usr/share/a/file"), b"x").unwrap();
        // nothing moved: nothing is left behind
        drop(Entry::new(&root).expect("an entry"));
        assert!(!dir(&root).exists());
        assert_eq!(empty(&root).unwrap(), 0);
        let mut e = Entry::new(&root).expect("an entry");
        let to = e.take(&old).unwrap();
        assert!(!old.exists() && !e.is_empty() && to.join("usr/share/a/file").exists());
        // a directory of the live tree, moved and not yet looked through
        let live = root.join("usr/share/b");
        fs::create_dir_all(&live).unwrap();
        fs::write(live.join("file"), b"y").unwrap();
        let u = e.take_unchecked(&live, b"usr/share/b").unwrap();
        assert!(!live.exists() && u.join("file").exists() && u.with_extension("from").symlink_metadata().is_ok());
        assert!(e.take(&Db::programs(&root).join("missing")).is_err());
        // held: left alone
        assert_eq!(empty(&root).unwrap(), 0);
        assert_eq!(entries(&root).len(), 1);
        drop(e);
        // The filler is gone and never looked through the directory: the
        // old version is deleted, the directory is not, and its entry stays.
        fs::write(dir(&root).join("stray"), b"").unwrap();
        assert_eq!(empty(&root).unwrap(), 0);
        assert!(!to.exists() && u.join("file").exists() && !dir(&root).join("stray").exists());
        assert!(has_work(&root));
        // the run that finishes the journal puts it back where it was...
        settle_unchecked(&root, true).unwrap();
        assert_eq!(fs::read(live.join("file")).unwrap(), b"y");
        assert!(!u.exists() && u.with_extension("from").symlink_metadata().is_err());
        // ...and then the entry goes, and the trash with it
        assert_eq!(empty(&root).unwrap(), 1);
        assert!(!dir(&root).exists() && Db::programs(&root).join("a").exists());

        // looked through and accepted: deleted; given back: nothing waits
        let mut e = Entry::new(&root).expect("an entry");
        let u = e.take_unchecked(&live, b"usr/share/b").unwrap();
        assert!(e.is_empty());
        e.accept(&u).unwrap();
        assert!(!e.is_empty() && u.with_extension("").join("file").exists() && u.with_extension("from").symlink_metadata().is_err());
        drop(e);
        assert_eq!(empty(&root).unwrap(), 1);
        fs::create_dir_all(&live).unwrap();
        let mut e = Entry::new(&root).expect("an entry");
        let u = e.take_unchecked(&live, b"usr/share/b").unwrap();
        fs::rename(&u, &live).unwrap();
        e.gave_back(&u);
        assert!(e.is_empty());
        drop(e);
        assert!(!dir(&root).exists(), "nothing was left: no entry, no trash");

        // a link left without its directory (the run died between the two
        // steps) does not keep the entry
        let mut e = Entry::new(&root).expect("an entry");
        let u = e.take_unchecked(&live, b"usr/share/b").unwrap();
        fs::rename(&u, &live).unwrap();
        drop(e);
        assert!(u.with_extension("from").symlink_metadata().is_ok() && has_work(&root));
        assert_eq!(empty(&root).unwrap(), 1);
        assert!(!dir(&root).exists() && !has_work(&root));
        // an entry with nothing in it, a trash with nothing in it, and
        // something else under the trash's name are work too; looking at
        // them changes nothing
        fs::create_dir_all(dir(&root).join("dead-entry")).unwrap();
        assert!(has_work(&root) && dir(&root).join("dead-entry").exists());
        fs::remove_dir(dir(&root).join("dead-entry")).unwrap();
        assert!(has_work(&root) && dir(&root).exists());
        fs::remove_dir(dir(&root)).unwrap();
        fs::write(dir(&root), b"").unwrap();
        assert!(has_work(&root) && dir(&root).exists());
        fs::remove_file(dir(&root)).unwrap();

        // its place is taken meanwhile: not put back, not deleted by anyone,
        // stuck from then on, and no work for a run
        let mut e = Entry::new(&root).expect("an entry");
        let u = e.take_unchecked(&live, b"usr/share/b").unwrap();
        fs::write(u.join("precious"), b"z").unwrap();
        drop(e);
        fs::create_dir_all(&live).unwrap();
        settle_unchecked(&root, true).unwrap();
        let stuck = u.with_extension("stuck");
        assert!(!u.exists() && fs::read(stuck.join("precious")).unwrap() == b"z");
        assert_eq!(empty(&root).unwrap(), 0);
        assert!(stuck.join("precious").exists() && !has_work(&root));
        // its place is free again: what is stuck is not put back later either
        fs::remove_dir(&live).unwrap();
        settle_unchecked(&root, true).unwrap();
        assert!(stuck.join("precious").exists() && !live.exists());
        // one whose change is over (no journal is being finished): never
        // put back, though its place is free
        let mut e = Entry::new(&root).expect("an entry");
        fs::create_dir_all(root.join("usr/share/c")).unwrap();
        let u2 = e.take_unchecked(&root.join("usr/share/c"), b"usr/share/c").unwrap();
        drop(e);
        settle_unchecked(&root, false).unwrap();
        assert!(u2.with_extension("stuck").is_dir() && !root.join("usr/share/c").exists());
        // a link that points out of the root, or to catalyst's own places, is not followed
        let outside = root.with_extension("outside");
        let absolute = outside.to_string_lossy().into_owned();
        for bad in [absolute.as_str(), "../x", "usr/../../x", "Programs/x", ""] {
            let mut e = Entry::new(&root).expect("an entry");
            fs::create_dir_all(root.join("usr/share/d")).unwrap();
            let u3 = e.take_unchecked(&root.join("usr/share/d"), b"usr/share/d").unwrap();
            drop(e);
            fs::remove_file(u3.with_extension("from")).unwrap();
            if !bad.is_empty() {
                std::os::unix::fs::symlink(bad, u3.with_extension("from")).unwrap();
            }
            settle_unchecked(&root, true).unwrap();
            assert!(u3.with_extension("stuck").is_dir() && !root.join("usr/share/d").exists() && !outside.exists(), "{bad:?}");
            fs::remove_dir_all(u3.parent().unwrap()).unwrap();
        }
        fs::remove_dir_all(&root).unwrap();
    }

    /// Something else under the trash's name is never emptied: a symlink is
    /// removed without a look at what it points to, a file is removed, and
    /// a directory that others may write to is left as it is.
    #[test]
    fn the_trash_is_used_only_as_a_directory_of_our_own() {
        use std::os::unix::fs::PermissionsExt;
        let root = root("own");
        let elsewhere = root.join("elsewhere");
        fs::create_dir_all(elsewhere.join("dir/sub")).unwrap();
        fs::write(elsewhere.join("file"), b"keep").unwrap();
        fs::write(elsewhere.join("dir/sub/file"), b"keep").unwrap();
        let kept = |what: &str| {
            assert!(elsewhere.join("file").exists() && elsewhere.join("dir/sub/file").exists(), "{what}");
        };
        std::os::unix::fs::symlink(&elsewhere, dir(&root)).unwrap();
        assert_eq!(empty(&root).unwrap(), 0);
        kept("a symlink at the trash");
        assert!(fs::symlink_metadata(dir(&root)).is_err(), "the link is gone");
        std::os::unix::fs::symlink(&elsewhere, dir(&root)).unwrap();
        assert!(entries(&root).is_empty());
        assert!(fs::symlink_metadata(dir(&root)).is_ok(), "entries() for --simulate must not remove the link");
        let e = Entry::new(&root);
        assert!(e.is_none() && fs::symlink_metadata(dir(&root)).is_err());
        kept("an entry asked for with a symlink there");
        fs::write(dir(&root), b"").unwrap();
        assert!(entries(&root).is_empty() && fs::symlink_metadata(dir(&root)).is_ok(), "entries() leaves a file untouched");
        assert_eq!(empty(&root).unwrap(), 0);
        assert!(fs::symlink_metadata(dir(&root)).is_err(), "a file under the name is removed");
        // a directory others may write to: not ours, left alone with what is in it
        fs::create_dir_all(dir(&root).join("x/0")).unwrap();
        fs::set_permissions(dir(&root), fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(empty(&root).unwrap(), 0);
        assert!(dir(&root).join("x/0").exists() && Entry::new(&root).is_none());
        fs::remove_dir_all(&root).unwrap();
    }

    /// Recovery of an unchecked directory walks every ancestor with O_NOFOLLOW:
    /// a parent component replaced by a symlink (pointing outside the root or
    /// into catalyst's own state) is never followed; the directory becomes stuck.
    #[test]
    fn settle_unchecked_refuses_symlink_in_parent_path() {
        let root = root("symlink-parent");
        let outside = root.with_extension("outside");
        let _ = fs::remove_dir_all(&outside);
        fs::create_dir_all(&outside).unwrap();
        for (target, check_dst) in [
            (outside.clone(), outside.join("escaped")),
            (PathBuf::from("../var/lib/catalyst"), Db::state(&root).join("escaped")),
        ] {
            let _ = fs::remove_dir_all(root.join("usr"));
            fs::create_dir_all(root.join("usr")).unwrap();
            std::os::unix::fs::symlink(&target, root.join("usr/share")).unwrap();
            let mut e = Entry::new(&root).expect("an entry");
            let staging = root.join("tmp-src");
            fs::create_dir_all(&staging).unwrap();
            fs::write(staging.join("data"), b"keep").unwrap();
            let u = e.take_unchecked(&staging, b"usr/share/escaped").unwrap();
            drop(e);
            settle_unchecked(&root, true).unwrap();
            assert!(!check_dst.exists(), "must not follow parent symlink to {}", check_dst.display());
            assert!(u.with_extension("stuck").join("data").exists(), "must stay stuck in trash");
            fs::remove_dir_all(u.parent().unwrap()).unwrap();
        }
        fs::remove_dir_all(&outside).unwrap();
        fs::remove_dir_all(&root).unwrap();
    }

    /// Where the kernel does not say whether the trash is a mount point (no
    /// `openat2`), `mountinfo` does. Where nothing tells, the trash is not
    /// used, and one made for the asking is not left behind.
    #[test]
    fn the_trash_is_checked_for_mounts_on_any_kernel() {
        let root = root("any-kernel");
        let silent = |_: &File, _: &std::ffi::CStr| -> Option<bool> { None };
        let blind = || -> Option<Vec<sys::Mount>> { None };
        let mount_at = |rel: &'static str| {
            let root = root.clone();
            move || -> Option<Vec<sys::Mount>> {
                let at = fs::canonicalize(dir(&root)).unwrap().join(rel);
                Some(vec![
                    sys::Mount { id: 1, dev: b"8:1".to_vec(), root: "/".into(), at: "/".into() },
                    sys::Mount { id: 2, dev: b"0:40".to_vec(), root: "/".into(), at },
                ])
            }
        };
        // the kernel does not say, mountinfo does: used, as where the kernel says
        if sys::mounts().is_some() {
            let b = base_how(&root, true, true, &silent, &sys::mounts).expect("the trash, by mountinfo");
            assert_eq!(b, dir(&root));
            fs::remove_dir(&b).unwrap();
        } else {
            eprintln!("no /proc/self/mountinfo here: that part is not tested");
        }
        // mountinfo says that something is mounted on it, or in it
        for rel in ["", "5-5/0"] {
            assert!(base_how(&root, true, true, &silent, &mount_at(rel)).is_none(), "a mount at {rel:?}");
            assert!(fs::symlink_metadata(dir(&root)).is_err(), "made for the asking, refused, and not left behind");
        }
        // a mount elsewhere is none of its business
        let elsewhere = || -> Option<Vec<sys::Mount>> {
            Some(vec![sys::Mount { id: 2, dev: b"0:40".to_vec(), root: "/".into(), at: "/somewhere/else".into() }])
        };
        let b = base_how(&root, true, true, &silent, &elsewhere).expect("the trash");
        fs::remove_dir(&b).unwrap();
        // nothing tells
        assert!(base_how(&root, true, true, &silent, &blind).is_none());
        assert!(fs::symlink_metadata(dir(&root)).is_err(), "not left behind");
        // one that was there before stays as it is, with what is in it
        fs::create_dir_all(dir(&root).join("7-7/0")).unwrap();
        assert!(base_how(&root, true, true, &silent, &blind).is_none());
        assert!(base_how(&root, false, true, &silent, &mount_at("")).is_none());
        assert!(dir(&root).join("7-7/0").is_dir());
        // and where the kernel says, it is asked alone
        let says_no = |_: &File, _: &std::ffi::CStr| -> Option<bool> { Some(false) };
        let says_yes = |_: &File, _: &std::ffi::CStr| -> Option<bool> { Some(true) };
        fs::remove_dir_all(dir(&root)).unwrap();
        let b = base_how(&root, true, true, &says_no, &blind).expect("the trash");
        fs::remove_dir(&b).unwrap();
        assert!(base_how(&root, true, true, &says_yes, &elsewhere).is_none());
        assert!(fs::symlink_metadata(dir(&root)).is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    /// A root given as a symlink is the directory it leads to: an unchecked
    /// directory goes back into it. (Below the root no link is followed:
    /// `settle_unchecked_refuses_symlink_in_parent_path`.)
    #[test]
    fn an_unchecked_directory_goes_back_under_a_root_given_as_a_link() {
        let real = root("root-as-link");
        let link = real.with_extension("the-link");
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(&real, &link).unwrap();
        fs::create_dir_all(real.join("usr/share")).unwrap();
        let mut e = Entry::new(&link).expect("an entry");
        let staging = real.join("tmp-src");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("data"), b"keep").unwrap();
        let u = e.take_unchecked(&staging, b"usr/share/thing").unwrap();
        drop(e);
        settle_unchecked(&link, true).unwrap();
        assert_eq!(fs::read(real.join("usr/share/thing/data")).unwrap(), b"keep");
        assert!(fs::symlink_metadata(&u).is_err() && fs::symlink_metadata(u.with_extension("stuck")).is_err());
        fs::remove_file(&link).unwrap();
        fs::remove_dir_all(&real).unwrap();
    }

    /// A directory that waits to be put back in a trash that is not used
    /// (here: others may write to it) stops the run that would finish its
    /// journal, and nothing in that trash is touched. Any other run goes
    /// on; and once the trash is root's alone again the directory goes
    /// back. A trash that is not used and holds nothing unchecked stops
    /// nobody.
    #[test]
    fn a_journal_is_not_finished_past_a_directory_in_a_trash_that_is_not_used() {
        use std::os::unix::fs::PermissionsExt;
        let root = root("shut-trash");
        fs::create_dir_all(root.join("usr/share")).unwrap();
        let mut e = Entry::new(&root).expect("an entry");
        let staging = root.join("tmp-src");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("data"), b"keep").unwrap();
        let u = e.take_unchecked(&staging, b"usr/share/thing").unwrap();
        drop(e);
        let shut = |mode: u32| fs::set_permissions(dir(&root), fs::Permissions::from_mode(mode)).unwrap();
        shut(0o775);
        let err = settle_unchecked(&root, true).unwrap_err().to_string();
        // it names the directory and what its link names, says how many
        // wait, and says to look at all of them first
        assert!(err.contains("0.unchecked waits to be put back (its link names \"usr/share/thing\"; 1 such in all)"), "{err}");
        assert!(err.contains("Look at every *.unchecked directory"), "{err}");
        assert!(u.join("data").exists() && u.with_extension("from").symlink_metadata().is_ok(), "nothing in it was touched");
        assert!(!root.join("usr/share/thing").exists());
        // a second one, whose link names a place it would never be put at:
        // counted, and the first is named as its link has it
        let entry = u.parent().unwrap().to_path_buf();
        fs::create_dir(entry.join("5.unchecked")).unwrap();
        std::os::unix::fs::symlink("../outside", entry.join("5.from")).unwrap();
        let err = settle_unchecked(&root, true).unwrap_err().to_string();
        assert!(err.contains("0.unchecked waits to be put back") && err.contains("2 such in all"), "{err}");
        fs::remove_file(entry.join("5.from")).unwrap();
        fs::remove_dir(entry.join("5.unchecked")).unwrap();
        // a run with no journal to finish goes on, and leaves it as it is
        settle_unchecked(&root, false).unwrap();
        assert_eq!(empty(&root).unwrap(), 0);
        assert!(u.join("data").exists());
        // root's alone again: it goes back
        shut(0o700);
        settle_unchecked(&root, true).unwrap();
        assert_eq!(fs::read(root.join("usr/share/thing/data")).unwrap(), b"keep");
        assert!(fs::symlink_metadata(&u).is_err());
        // not used, and nothing unchecked in it: nobody is stopped
        fs::create_dir_all(dir(&root).join("9-9/0")).unwrap();
        fs::write(dir(&root).join("9-9/1.stuck"), b"").unwrap();
        shut(0o775);
        settle_unchecked(&root, true).unwrap();
        assert!(dir(&root).join("9-9/0").is_dir());
        shut(0o700);
        // Something mounted on the trash hides what waits in it: the run
        // that would finish a journal stops, whatever the mount holds (as
        // root, where a tmpfs can be mounted).
        let again = root.join("tmp-src-2");
        fs::create_dir_all(&again).unwrap();
        fs::write(again.join("data"), b"keep too").unwrap();
        let mut e = Entry::new(&root).expect("an entry");
        e.take_unchecked(&again, b"usr/share/other").unwrap();
        drop(e);
        let mounted = unsafe { libc::geteuid() } == 0
            && std::process::Command::new("mount").args(["-t", "tmpfs", "-o", "size=1m,mode=0700", "none"]).arg(dir(&root)).status().map_or(false, |s| s.success());
        if mounted {
            // (two wait under it now: the message of the look would say so)
            let err = settle_unchecked(&root, true).map_err(|e| e.to_string());
            let unmounted = std::process::Command::new("umount").arg(dir(&root)).status().map_or(false, |s| s.success());
            assert!(unmounted, "the test's own mount on {} is still there", dir(&root).display());
            assert!(err.as_ref().err().map_or(false, |m| m.contains("mounted on")), "{err:?}");
            assert!(!root.join("usr/share/other").exists());
        } else {
            eprintln!("no tmpfs could be mounted here: that part is not tested");
        }
        // and with nothing on it the directory goes back
        settle_unchecked(&root, true).unwrap();
        assert_eq!(fs::read(root.join("usr/share/other/data")).unwrap(), b"keep too");
        fs::remove_dir_all(&root).unwrap();
    }
}
