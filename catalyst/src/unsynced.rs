//! What is left of "unsynced adds".
//!
//! For a few hours on 23 November 2026 catalyst left small adds of new
//! packages without any sync, and wrote a mark,
//! `var/lib/catalyst/unsynced`, so that the run after a reboot could check
//! what those adds had put on disk. That check had to guess whether a file
//! had been damaged by a power cut or changed by the admin, and a wrong
//! guess removed a healthy package; an independent review found eleven
//! defects in it. Adds now take one sync before the switch (README, "Adds:
//! one sync") and need no mark and no check.
//!
//! All this module does is retire a mark that is still there: it is read
//! (so that the packages added under it can be named in a note) and
//! deleted. Nothing is uninstalled.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::db::Db;

const HEADER: &str = "catalyst-unsynced 1";

pub fn path(root: &Path) -> PathBuf {
    Db::state(root).join("unsynced")
}

/// The `had` list of a mark (every package installed when it was written;
/// empty if the mark cannot be read), or `None` when there is no mark.
fn parse(t: &str) -> HashSet<(String, String)> {
    let mut had = HashSet::new();
    let mut lines = t.lines();
    if lines.next() != Some(HEADER) {
        return had;
    }
    for l in lines {
        if let ["had", n, v] = l.split(' ').collect::<Vec<_>>().as_slice() {
            had.insert((n.to_string(), v.to_string()));
        }
    }
    had
}

/// Removes a mark that is there and returns its `had` list.
pub fn take_legacy(root: &Path) -> Option<HashSet<(String, String)>> {
    let p = path(root);
    let t = match fs::read(&p) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(_) => return None,
    };
    let _ = fs::remove_file(&p);
    Some(parse(&t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_legacy_mark_gives_its_list_and_anything_else_an_empty_one() {
        let had = parse(&format!("{HEADER}\nboot abcd-1234\nhad nano 8.7.1-r1\nhad zlib 1.3.1-r1\nend\n"));
        assert!(had.contains(&("nano".to_string(), "8.7.1-r1".to_string())) && had.len() == 2);
        assert!(parse("").is_empty() && parse("garbage\nhad a 1\n").is_empty());
    }
}
