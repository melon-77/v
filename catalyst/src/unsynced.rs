//! Adds that are not synced.
//!
//! A change that only adds new packages (nothing installed is upgraded or
//! removed) and unpacks less than 32 MiB skips the device flushes of the
//! transaction: the `syncfs` before the switch, the fsync that puts the
//! journal in effect, and the final `syncfs`. The owner decided this on
//! 2026-11-23: those flushes were most of what a small install cost, and
//! what they protect against, a power cut during the install or in the
//! seconds after it, is rare. What such a cut can cost is bounded:
//! - an unsynced switch only makes new names. Before it replaces anything
//!   that is there, and before the first script or trigger runs, it calls
//!   `syncfs` and is a synced switch from there on (`commit::Switch`);
//! - the world is appended to, not rewritten (`db::write_world_unsynced`);
//! - a process that is killed loses nothing (the page cache is intact), so
//!   the journal works as it always did.
//!
//! So a power cut can leave the packages being added incomplete, and
//! nothing else. This module keeps the one durable thing needed to find
//! that out afterwards, `var/lib/catalyst/unsynced`:
//!
//! ```text
//! catalyst-unsynced 1
//! boot <the kernel's boot id>
//! had <name> <version>       every package installed when it was written
//! end
//! ```
//!
//! It is written, with an fsync, before the first unsynced add after a
//! boot. A mark of another boot means the system went down (cleanly or
//! not) since adds that may not have reached the disk: the next run that
//! may change the root checks every package that is not in the `had` list
//! and removes those that are not whole (`engine::after_unsynced_stop`),
//! then deletes the mark. Where the boot id cannot be read, nothing is
//! left unsynced.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::db::{self, Db, Installed};

const HEADER: &str = "catalyst-unsynced 1";

/// The mark: the boot it was written in, and what was installed then.
pub struct Mark {
    pub boot: String,
    pub had: HashSet<(String, String)>,
}

pub fn path(root: &Path) -> PathBuf {
    Db::state(root).join("unsynced")
}

/// The kernel's boot id. `CATALYST_BOOT_ID` replaces it: a test hook that
/// plays a reboot.
pub fn boot_id() -> Option<String> {
    let t = match std::env::var("CATALYST_BOOT_ID") {
        Ok(v) => v,
        Err(_) => fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?,
    };
    let t = t.trim();
    (t.len() >= 4 && t.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')).then(|| t.to_string())
}

fn boot_of(t: &str) -> Option<&str> {
    let mut lines = t.lines();
    if lines.next()? != HEADER {
        return None;
    }
    lines.next()?.strip_prefix("boot ")
}

fn parse(t: &str) -> Option<Mark> {
    let boot = boot_of(t)?.to_string();
    let mut had = HashSet::new();
    let mut ended = false;
    for l in t.lines().skip(2) {
        if ended {
            return None;
        }
        let f: Vec<&str> = l.split(' ').collect();
        match f.as_slice() {
            ["had", n, v] => {
                had.insert((n.to_string(), v.to_string()));
            }
            ["end"] => ended = true,
            _ => return None,
        }
    }
    ended.then_some(Mark { boot, had })
}

/// The mark of another boot, if there is one: the system went down since
/// adds that were not synced. A mark that cannot be read counts as one
/// that lists nothing (every package is checked).
pub fn stale(root: &Path) -> Option<Mark> {
    let t = match fs::read_to_string(path(root)) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(_) => String::new(),
    };
    match parse(&t) {
        Some(m) if Some(&m.boot) == boot_id().as_ref() => None,
        Some(m) => Some(m),
        None => Some(Mark { boot: String::new(), had: HashSet::new() }),
    }
}

/// Makes sure this boot's mark is on disk, before a change goes unsynced.
/// False when it is not (no boot id, a mark of another boot, a write
/// error): the change is then synced like any other.
pub fn ensure(root: &Path, installed: &BTreeMap<String, Installed>) -> bool {
    let Some(boot) = boot_id() else { return false };
    let p = path(root);
    match fs::read_to_string(&p) {
        Ok(t) => return boot_of(&t) == Some(boot.as_str()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return false,
    }
    let mut t = format!("{HEADER}\nboot {boot}\n");
    for (n, ip) in installed {
        t.push_str(&format!("had {n} {}\n", ip.meta.info.version));
    }
    t.push_str("end\n");
    // the file, then its name: both on disk before anything relies on it
    db::write_atomic(&p, t.as_bytes()).and_then(|_| fs::File::open(Db::state(root))?.sync_all()).is_ok()
}

pub fn remove(root: &Path) {
    let _ = fs::remove_file(path(root));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mark_is_read_back_and_anything_else_is_not_a_mark() {
        let t = format!("{HEADER}\nboot abcd-1234\nhad nano 8.7.1-r1\nhad zlib 1.3.1-r1\nend\n");
        let m = parse(&t).unwrap();
        assert_eq!(m.boot, "abcd-1234");
        assert!(m.had.contains(&("nano".to_string(), "8.7.1-r1".to_string())) && m.had.len() == 2);
        assert_eq!(boot_of(&t), Some("abcd-1234"));
        // cut short, or with something after its end
        assert!(parse(&t[..t.len() - 4]).is_none());
        assert!(parse(&format!("{t}had x 1\n")).is_none());
        assert!(parse("").is_none() && boot_of("catalyst-unsynced 2\nboot x\n").is_none());
        // no packages at all is a mark too
        assert!(parse(&format!("{HEADER}\nboot abcd\nend\n")).unwrap().had.is_empty());
    }
}
