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
//! Nothing here needs the root's lock, and nothing here touches anything
//! but what was moved into the trash. Whoever fills or empties an entry
//! holds a `flock` on its directory. An entry that nobody holds is one
//! whose process died (a kill, a power cut): the next run that changes the
//! root, or `oct recover`, deletes it.
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
/// first if it is not there. `None`: there is none, or it is not to be used.
fn base(root: &Path, make: bool) -> Option<PathBuf> {
    let state = Db::state(root);
    let b = dir(root);
    if make {
        match fs::DirBuilder::new().mode(0o700).create(&b) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }
    }
    let m = fs::symlink_metadata(&b).ok()?;
    if !m.is_dir() {
        // not ours: removed under this name, never followed
        let _ = fs::remove_file(&b);
        return None;
    }
    let st = fs::symlink_metadata(&state).ok()?;
    let name = std::ffi::CStr::from_bytes_with_nul(b"trash\0").unwrap();
    let mounted = File::open(&state).ok().and_then(|d| sys::is_mount_point(&d, name)) == Some(true);
    let ours = st.is_dir() && st.dev() == m.dev() && !mounted && m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o022 == 0;
    if !ours {
        eprintln!(
            "oct: warning: {} is not a directory of catalyst's own (a mount, another owner, or others may write to it): left alone",
            b.display()
        );
        return None;
    }
    Some(b)
}

/// An entry of the trash that this process holds while it fills it.
pub struct Entry {
    dir: PathBuf,
    _held: File,
    n: usize,
}

impl Entry {
    /// A new entry, held. `None` where there can be none: the trash is not
    /// on the file system of `Programs` (a tree gets there by a rename), or
    /// it cannot be made. Nothing is left behind then.
    pub fn new(root: &Path) -> Option<Entry> {
        let base = base(root, true)?;
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
                Some(held) => Some(Entry { dir: d, _held: held, n: 0 }),
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

    /// Moves the tree at `tree` into the entry (one rename). On an error it
    /// is where it was.
    pub fn take(&mut self, tree: &Path) -> io::Result<()> {
        fs::rename(tree, self.dir.join(self.n.to_string()))?;
        self.n += 1;
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        // nothing was moved: no entry, and no trash if it has no other
        if self.n == 0 {
            let _ = fs::remove_dir(&self.dir);
            if let Some(base) = self.dir.parent() {
                let _ = fs::remove_dir(base);
            }
        }
    }
}

/// The names of the entries in the trash (for `--simulate`).
pub fn entries(root: &Path) -> Vec<String> {
    let Some(base) = base(root, false) else { return Vec::new() };
    let mut v: Vec<String> =
        fs::read_dir(base).map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
    v.sort();
    v
}

/// Deletes every entry of the trash that no process holds, and the trash
/// itself when nothing is left in it. How many entries were deleted.
pub fn empty(root: &Path) -> io::Result<usize> {
    let Some(base) = base(root, false) else { return Ok(0) };
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
        let Ok(inside) = fs::read_dir(&p) else { continue };
        for t in inside.flatten() {
            match fs::symlink_metadata(t.path()) {
                Ok(m) if m.is_dir() => trees.push(t.path()),
                Ok(_) => {
                    let _ = fs::remove_file(t.path());
                }
                Err(_) => {}
            }
        }
        match crate::commit::remove_trees(&trees, true, most).and_then(|()| fs::remove_dir(&p)) {
            Ok(()) => n += 1,
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
        e.take(&old).unwrap();
        assert!(!old.exists() && !e.is_empty());
        assert!(e.take(&Db::programs(&root).join("missing")).is_err());
        // held: left alone
        assert_eq!(empty(&root).unwrap(), 0);
        assert_eq!(entries(&root).len(), 1);
        drop(e);
        // a stray file in the trash goes too
        fs::write(dir(&root).join("stray"), b"").unwrap();
        assert_eq!(empty(&root).unwrap(), 1);
        assert!(!dir(&root).exists() && Db::programs(&root).join("a").exists());
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
        std::os::unix::fs::symlink(&elsewhere, dir(&root)).unwrap();
        let e = Entry::new(&root);
        assert!(e.is_none() && fs::symlink_metadata(dir(&root)).is_err());
        kept("an entry asked for with a symlink there");
        fs::write(dir(&root), b"").unwrap();
        assert_eq!(empty(&root).unwrap(), 0);
        assert!(fs::symlink_metadata(dir(&root)).is_err(), "a file under the name is removed");
        // a directory others may write to: not ours, left alone with what is in it
        fs::create_dir_all(dir(&root).join("x/0")).unwrap();
        fs::set_permissions(dir(&root), fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(empty(&root).unwrap(), 0);
        assert!(dir(&root).join("x/0").exists() && Entry::new(&root).is_none());
        fs::remove_dir_all(&root).unwrap();
    }
}
