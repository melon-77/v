//! The lookup index: which installed packages claim each path, and every
//! installed package's metadata, kept in `var/lib/catalyst/index/` so that a
//! change need not read the record of every installed package.
//!
//! The records in `Programs/<name>/<version>/.catalyst/` stay the record of
//! what is installed; the index is a copy of what they say, and whenever
//! there is any doubt it is thrown away and rebuilt from them:
//! - `state` names the exact version of every package it describes and the
//!   directory symlinks of the tree (`lib64 -> usr/lib` and the like) as
//!   they were when it was written. [`Index::open`] checks both against
//!   the root (one `readlink` per package and per symlink) and refuses an
//!   index that doesn't match, or that is missing or damaged.
//! - `meta` holds every package's `info`, so that opening a root reads one
//!   file instead of one per package; in binary form ([`crate::codec`]),
//!   which reads back in a fifth of the time the text records take.
//! - `files/XXX` hold, per path, the packages that have a non-directory
//!   entry there and those that have a directory there: 65,536 buckets by
//!   a hash of the path, 128 to a file (512 segment files). A change reads
//!   only the buckets of its own paths and rewrites only their segments; a
//!   bucket it does not touch is written back as it was read. (With 4,096
//!   buckets, one to a file, the removal of a big package read some 3,700
//!   files and deleted 3,000, each a wait for the directory's lock; and on
//!   a system of 80,000 files every bucket it touched held twenty paths of
//!   other packages, which were parsed, copied and written out again: 80 ms
//!   of a 180 ms removal. A bucket now holds a path or two.)
//! - `aliases` holds the entries that reach another place through a
//!   directory symlink (canonical path, literal path, package).
//!
//! Every file of the index is checked when it is read: `state` holds which
//! buckets are not empty, the SHA-256 of `meta` and of `aliases`, and a sum
//! of itself; a segment file starts with a checksum of the rest of it (16
//! hex digits of its SHA-256: against damage, not against a forger, who
//! could write the root anyway), then the length of each of its buckets. A
//! missing, truncated or changed file makes the index unusable, and the
//! change reads every record instead. Before a change goes through the
//! index, everything it may look up there is read ([`Index::preload`]), so
//! that damage is found before the switch, not in the middle of it.
//!
//! A change that ends without a `syncfs` (README, "One sync") writes
//! nothing of the index after its one `syncfs`: the index of after the
//! change is worked out and written beside the current one, in `next/`,
//! before that `syncfs` ([`stage`]), and at the end of the switch it is put
//! in place by renames alone, `state` removed first and renamed in last
//! ([`commit_staged`]). So whatever a power cut leaves has either no
//! `state` (the index is rebuilt) or the new one with every file it
//! describes whole on disk. A change that syncs at its end (`--sync`, or a
//! script ran) updates the index inside the switch, before that final
//! `syncfs`, without an fsync per file (`state` removed first and written
//! last). A crash before that `syncfs` leaves the journal, which names the
//! records the switch reads and the index it was planned with. Finishing an
//! interrupted switch reads those same records with that same index while
//! the index is still the one from before the switch ([`open_before`]), and
//! every record otherwise. Nothing else uses the index while there is a
//! journal ([`crate::db::Db::reload`]). Outside a switch (a missing or
//! outdated index rebuilt when a root is opened, or by `oct fix`) the new
//! index is written beside the old one, synced, and swapped in.
//!
//! The index is canonical: the same installed packages give the same files,
//! whether it was updated change by change or rebuilt. One exception: the
//! list of directory symlinks in `state` is carried over by an update and
//! worked out afresh by a rebuild, so the two differ when a symlink of a
//! package the change does not touch starts or stops leading to a directory
//! (another package brings or takes away the directory it points at).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::commit::PkgState;
use crate::adb::FileKind;
use crate::db::{self, escape, unescape, FileRec, Installed, PkgMeta, RecKind};
use crate::digest;
use crate::error::{Error, Result};
use crate::rootfs::{self, RootFs};

pub const INDEX_DIR: &str = "var/lib/catalyst/index";
// 3: `meta` in binary form; 4: 4,096 buckets, eight to a file, a checksum
// per bucket (never released); 5: 65,536 buckets, 128 to a file, one
// checksum per file. An index of another form is rebuilt.
const HEADER: &str = "catalyst-index 5";
/// A segment's checksum: this many hex digits of a SHA-256.
const SUM_LEN: usize = 16;
const BUCKETS: u32 = 65536;
/// How many buckets one file of `files/` holds.
const PER_SEGMENT: u32 = 128;
/// A segment file's first line: this and the checksum of everything after
/// that line. The second line: the length in bytes of each of its buckets
/// (0: an empty one). The buckets follow, one after another: a line per
/// path, with the packages that have a non-directory entry there and those
/// that have a directory there.
const SEGMENT_HEADER: &str = "catalyst-segment";

/// A segment file as read (and checked): its text, and where each of its
/// buckets is in it.
struct Segment {
    text: String,
    at: Vec<(usize, usize)>,
}

impl Segment {
    /// The text of bucket `b` (empty: an empty bucket).
    fn bucket(&self, b: u32) -> &str {
        let (from, to) = self.at[(b % PER_SEGMENT) as usize];
        &self.text[from..to]
    }
}

fn bucket_of(path: &[u8]) -> u32 {
    // FNV-1a
    let mut h: u32 = 0x811c_9dc5;
    for &b in path {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h % BUCKETS
}

fn segment_of(b: u32) -> u32 {
    b / PER_SEGMENT
}

fn segment_name(s: u32) -> String {
    format!("{s:03x}")
}

/// The directory of the index of `root`.
pub fn dir(root: &Path) -> PathBuf {
    root.join(INDEX_DIR)
}

/// What one path of a bucket is claimed by.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
struct Claims {
    files: Vec<String>,
    dirs: Vec<String>,
}

type Bucket = BTreeMap<Vec<u8>, Claims>;

/// A list of package names as written: sorted, so that the index does not
/// depend on the order of the changes that made it (an updated index equals
/// a rebuilt one, byte for byte).
fn names_field(v: &[String]) -> String {
    if v.is_empty() {
        return "-".into();
    }
    let mut v: Vec<&str> = v.iter().map(String::as_str).collect();
    v.sort_unstable();
    v.dedup();
    v.join(" ")
}

fn parse_names(f: &str) -> Vec<String> {
    if f == "-" {
        Vec::new()
    } else {
        f.split(' ').filter(|s| !s.is_empty()).map(String::from).collect()
    }
}

fn parse_bucket(text: &str) -> Result<Bucket> {
    let mut b = Bucket::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let mut f = line.split('\t');
        let (Some(path), Some(files), Some(dirs), None) = (f.next(), f.next(), f.next(), f.next()) else {
            return Err(Error::Format("index: malformed bucket line".into()));
        };
        b.insert(unescape(path)?, Claims { files: parse_names(files), dirs: parse_names(dirs) });
    }
    Ok(b)
}

fn format_bucket(b: &Bucket) -> String {
    let mut body = String::new();
    for (p, c) in b {
        body.push_str(&format!("{}\t{}\t{}\n", escape(p), names_field(&c.files), names_field(&c.dirs)));
    }
    body
}

fn sum_of(data: &[u8]) -> String {
    digest::hex(&digest::sha256(data))
}

/// Whether bucket `b` is marked as not empty.
fn has(present: &[u8], b: u32) -> bool {
    present[(b / 8) as usize] & (1 << (b % 8)) != 0
}

fn set(present: &mut [u8], b: u32, on: bool) {
    let (i, m) = ((b / 8) as usize, 1u8 << (b % 8));
    if on {
        present[i] |= m;
    } else {
        present[i] &= !m;
    }
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// The stamp of a set of installed versions: SHA-256 of `name=version`
/// lines in name order.
pub fn stamp(versions: &BTreeMap<String, String>) -> String {
    let mut s = String::new();
    for (n, v) in versions {
        s.push_str(&format!("{n}={v}\n"));
    }
    digest::hex(&digest::sha256(s.as_bytes()))
}

/// What `state` says.
struct State {
    stamp: String,
    links: Vec<(Vec<u8>, Vec<u8>)>,
    /// One bit per bucket: set when the bucket is not empty (its file must be there).
    present: Vec<u8>,
    meta_sum: String,
    aliases_sum: String,
}

/// An index that matched the root when it was opened.
pub struct Index {
    dir: PathBuf,
    /// The directory symlinks it was written with: (canonical path of the
    /// link, its target).
    pub links: Vec<(Vec<u8>, Vec<u8>)>,
    present: Vec<u8>,
    meta_sum: String,
    aliases_sum: String,
    buckets: std::cell::RefCell<HashMap<u32, Bucket>>,
    /// The segment files read so far, as they were read.
    segs: std::cell::RefCell<HashMap<u32, Arc<Segment>>>,
    aliases: std::cell::RefCell<Option<HashMap<Vec<u8>, Vec<(Vec<u8>, String)>>>>,
    /// Each package's `info` as `meta` held it: an update writes these
    /// back for the packages it doesn't change instead of formatting them.
    raw_meta: std::cell::RefCell<HashMap<String, Vec<u8>>>,
}

fn read_state(dir: &Path) -> Option<State> {
    let t = fs::read_to_string(dir.join("state")).ok()?;
    // the last lines: the sum of everything before them, then "end"
    let at = t.rfind("\nsum\t")? + 1;
    let (body, tail) = t.split_at(at);
    if tail != format!("sum\t{}\nend\n", sum_of(body.as_bytes())) {
        return None;
    }
    let mut lines = body.lines();
    if lines.next()? != HEADER {
        return None;
    }
    let (mut stamp, mut present, mut meta_sum, mut aliases_sum) = (None, None, None, None);
    let mut links = Vec::new();
    for l in lines {
        let f: Vec<&str> = l.split('\t').collect();
        match f.as_slice() {
            ["packages", s] => stamp = Some(s.to_string()),
            ["link", p, t] => links.push((unescape(p).ok()?, unescape(t).ok()?)),
            ["buckets", b] => present = Some(unhex(b)?),
            ["meta", s] => meta_sum = Some(s.to_string()),
            ["aliases", s] => aliases_sum = Some(s.to_string()),
            _ => return None,
        }
    }
    let present = present?;
    if present.len() != (BUCKETS / 8) as usize {
        return None;
    }
    Some(State { stamp: stamp?, links, present, meta_sum: meta_sum?, aliases_sum: aliases_sum? })
}

impl Index {
    /// Opens the index of `root` if it describes exactly `versions` (name
    /// to version of every installed package) and the directory symlinks
    /// it was written with are still there, pointing where they pointed.
    pub fn open(root: &Path, versions: &BTreeMap<String, String>) -> Option<Index> {
        let dir = dir(root);
        let st = read_state(&dir)?;
        if st.stamp != stamp(versions) {
            return None;
        }
        // a readlink per directory symlink (about 230 on Valence): on a cold
        // cache each waits for the disk, so they may go to threads
        let same = on_threads(&st.links, |(p, t)| -> Result<bool> {
            let l = fs::read_link(root.join(std::ffi::OsStr::from_bytes(p)));
            Ok(l.map_or(false, |l| l.as_os_str().as_bytes() == t.as_slice()))
        })
        .ok()?;
        if same.contains(&false) {
            return None;
        }
        Some(Index {
            dir,
            links: st.links,
            present: st.present,
            meta_sum: st.meta_sum,
            aliases_sum: st.aliases_sum,
            buckets: Default::default(),
            segs: Default::default(),
            aliases: Default::default(),
            raw_meta: Default::default(),
        })
    }

    /// The metadata of every package it describes, by name.
    pub fn metas(&self) -> Result<BTreeMap<String, PkgMeta>> {
        let data = fs::read(self.dir.join("meta"))?;
        if sum_of(&data) != self.meta_sum {
            return Err(Error::Format("index: damaged meta".into()));
        }
        let mut out = BTreeMap::new();
        let mut i = 0;
        while i < data.len() {
            let nl = data[i..].iter().position(|&c| c == b'\n').ok_or_else(|| Error::Format("index: meta".into()))? + i;
            let head = std::str::from_utf8(&data[i..nl]).map_err(|_| Error::Format("index: meta".into()))?;
            let f: Vec<&str> = head.split(' ').collect();
            let ["pkg", name, len] = f.as_slice() else { return Err(Error::Format("index: meta".into())) };
            let len: usize = len.parse().map_err(|_| Error::Format("index: meta".into()))?;
            let body = data.get(nl + 1..nl + 1 + len).ok_or_else(|| Error::Format("index: meta".into()))?;
            let m = crate::codec::decode_meta(body).ok_or_else(|| Error::Format("index: meta".into()))?;
            if m.info.name != *name {
                return Err(Error::Format("index: meta".into()));
            }
            self.raw_meta.borrow_mut().insert(name.to_string(), body.to_vec());
            out.insert(name.to_string(), m);
            i = nl + 1 + len;
        }
        Ok(out)
    }

    /// Bucket `b`, read and checked once (lookups then use the copy). A bucket
    /// marked as not empty whose segment is missing or damaged is an error.
    fn load(&self, b: u32) -> Result<()> {
        if self.buckets.borrow().contains_key(&b) {
            return Ok(());
        }
        let bk = if has(&self.present, b) { bucket_in(&*self.segment(segment_of(b))?, b)? } else { Bucket::new() };
        self.buckets.borrow_mut().insert(b, bk);
        Ok(())
    }

    /// Segment `s` as its file holds it (read once).
    fn segment(&self, s: u32) -> Result<Arc<Segment>> {
        if let Some(seg) = self.segs.borrow().get(&s) {
            return Ok(seg.clone());
        }
        let seg = Arc::new(read_segment(&self.dir, s)?);
        self.segs.borrow_mut().insert(s, seg.clone());
        Ok(seg)
    }

    /// Like [`Index::load`] for many buckets: a big change touches nearly
    /// all of them, and read one after another from a cold cache each file
    /// is a wait for the disk, so the segments are read on several threads.
    fn load_many(&self, ids: impl IntoIterator<Item = u32>) -> Result<()> {
        let mut todo: Vec<u32> = {
            let c = self.buckets.borrow();
            ids.into_iter().filter(|b| !c.contains_key(b)).collect()
        };
        todo.sort_unstable();
        todo.dedup();
        // per segment: the buckets wanted of it that are not empty, and its
        // text if it was read before
        let mut groups: Vec<(u32, Vec<u32>, Option<Arc<Segment>>)> = Vec::new();
        for b in todo {
            if !has(&self.present, b) {
                self.buckets.borrow_mut().insert(b, Bucket::new());
                continue;
            }
            let s = segment_of(b);
            match groups.last_mut() {
                Some((gs, bs, _)) if *gs == s => bs.push(b),
                _ => groups.push((s, vec![b], self.segs.borrow().get(&s).cloned())),
            }
        }
        crate::prof::mark("      index: segments chosen");
        // only the path goes to the threads (the index's caches are not Sync)
        let dir = &self.dir;
        let read = on_threads(&groups, |(s, bs, known)| -> Result<(Arc<Segment>, Vec<Bucket>)> {
            let seg = match known {
                Some(seg) => seg.clone(),
                None => Arc::new(read_segment(dir, *s)?),
            };
            let parsed = bs.iter().map(|&b| bucket_in(&seg, b)).collect::<Result<Vec<Bucket>>>()?;
            Ok((seg, parsed))
        })?;
        crate::prof::mark("      index: segments read and their buckets parsed");
        for ((s, bs, _), (seg, parsed)) in groups.iter().zip(read) {
            self.segs.borrow_mut().insert(*s, seg);
            for (b, bk) in bs.iter().zip(parsed) {
                self.buckets.borrow_mut().insert(*b, bk);
            }
        }
        Ok(())
    }

    /// Each of these segments with its text as its file holds it, read where
    /// it was not yet (`None`: none of its buckets is marked as not empty, so
    /// there is no file to read).
    fn segments_for(&self, segs: &[u32]) -> Result<Vec<(u32, Option<Arc<Segment>>)>> {
        let any = |s: u32| (s * PER_SEGMENT..(s + 1) * PER_SEGMENT).any(|b| has(&self.present, b));
        let missing: Vec<u32> = {
            let c = self.segs.borrow();
            segs.iter().copied().filter(|&s| any(s) && !c.contains_key(&s)).collect()
        };
        let dir = &self.dir;
        let read = on_threads(&missing, |&s| read_segment(dir, s))?;
        for (s, seg) in missing.iter().zip(read) {
            self.segs.borrow_mut().insert(*s, Arc::new(seg));
        }
        let c = self.segs.borrow();
        Ok(segs.iter().map(|&s| (s, if any(s) { c.get(&s).cloned() } else { None })).collect())
    }

    fn with_bucket<T>(&self, path: &[u8], f: impl FnOnce(Option<&Claims>) -> T) -> Result<T> {
        let b = bucket_of(path);
        self.load(b)?;
        Ok(f(self.buckets.borrow()[&b].get(path)))
    }

    fn aliases_loaded(&self) -> Result<()> {
        let mut al = self.aliases.borrow_mut();
        if al.is_none() {
            *al = Some(read_aliases(&self.dir, &self.aliases_sum)?);
        }
        Ok(())
    }

    /// The packages with a non-directory entry at `path`.
    pub fn file_claimants(&self, path: &[u8]) -> Result<Vec<String>> {
        self.with_bucket(path, |c| c.map(|c| c.files.clone()).unwrap_or_default())
    }

    /// The packages with a directory entry at `path`.
    pub fn dir_claimants(&self, path: &[u8]) -> Result<Vec<String>> {
        self.with_bucket(path, |c| c.map(|c| c.dirs.clone()).unwrap_or_default())
    }

    /// The packages with an entry (of any kind) that is `canon` itself or
    /// reaches it through a directory symlink.
    pub fn canon_claimants(&self, canon: &[u8]) -> Result<Vec<String>> {
        let mut out = self.with_bucket(canon, |c| {
            c.map(|c| c.files.iter().chain(c.dirs.iter()).cloned().collect::<Vec<_>>()).unwrap_or_default()
        })?;
        self.aliases_loaded()?;
        if let Some(v) = self.aliases.borrow().as_ref().and_then(|m| m.get(canon)) {
            out.extend(v.iter().map(|(_, n)| n.clone()));
        }
        Ok(out)
    }

    /// Whether a package that is not in `changed` has a directory at `path`.
    pub fn dir_of_another(&self, path: &[u8], changed: &HashSet<String>) -> Result<bool> {
        self.with_bucket(path, |c| c.map_or(false, |c| c.dirs.iter().any(|n| !changed.contains(n))))
    }

    /// Whether a package that is not in `changed` has an entry (of any
    /// kind) that is `canon` itself or reaches it through a directory
    /// symlink ([`Index::canon_claimants`] without the list).
    pub fn place_of_another(&self, canon: &[u8], changed: &HashSet<String>) -> Result<bool> {
        let here = self.with_bucket(canon, |c| {
            c.map_or(false, |c| c.files.iter().chain(c.dirs.iter()).any(|n| !changed.contains(n)))
        })?;
        if here {
            return Ok(true);
        }
        self.aliases_loaded()?;
        let al = self.aliases.borrow();
        Ok(al.as_ref().and_then(|m| m.get(canon)).map_or(false, |v| v.iter().any(|(_, n)| !changed.contains(n))))
    }

    /// Reads, and so checks, everything about these packages' paths that a
    /// switch may look up: the buckets of the paths, of their directories and
    /// of their canonical places, of the directories their triggers name, and
    /// the aliases. Damage there is then found before the switch.
    pub fn preload(&self, rfs: &mut RootFs, pkgs: &[&PkgState]) -> Result<()> {
        self.aliases_loaded()?;
        let mut ids: HashSet<u32> = HashSet::new();
        for p in pkgs {
            // A record lists its entries directory by directory: what is the
            // same for all entries of one directory (its own bucket, and
            // where the directory really is) is worked out once for it.
            let mut now: Option<(&[u8], Option<Vec<u8>>)> = None;
            for r in &p.recs {
                ids.insert(bucket_of(&r.path));
                let (dir, name) = rootfs::split_rel(&r.path);
                if now.as_ref().map_or(true, |(d, _)| *d != dir) {
                    ids.insert(bucket_of(dir));
                    now = Some((dir, rfs.dir_if_exists(dir)));
                }
                // the entry's canonical place: another path than its own only
                // below a directory that is reached through a symlink
                if let Some((_, Some(canon_dir))) = &now {
                    if canon_dir.as_slice() != dir {
                        ids.insert(bucket_of(&rootfs::join_rel(canon_dir, name)));
                    }
                }
            }
            for t in &p.meta.triggers {
                ids.insert(bucket_of(t.trim_start_matches('+').trim_matches('/').as_bytes()));
            }
        }
        crate::prof::mark("      index: buckets of the paths worked out");
        self.load_many(ids)
    }
}

/// Segment `s` as its file holds it, checked: the checksum of its first
/// line fits the rest, and the lengths of its second line add up to what
/// follows.
fn read_segment(dir: &Path, s: u32) -> Result<Segment> {
    let damaged = || Error::Format(format!("index: damaged segment {}", segment_name(s)));
    let p = dir.join("files").join(segment_name(s));
    let text = fs::read_to_string(&p).map_err(|e| Error::Format(format!("index: segment {}: {e}", segment_name(s))))?;
    let (head, rest) = text.split_once('\n').ok_or_else(damaged)?;
    let sum = head.strip_prefix(SEGMENT_HEADER).and_then(|r| r.strip_prefix(' ')).ok_or_else(damaged)?;
    if sum.len() != SUM_LEN || sum_of(rest.as_bytes()).get(..SUM_LEN) != Some(sum) {
        return Err(damaged());
    }
    let (lens, body) = rest.split_once('\n').ok_or_else(damaged)?;
    let mut at = Vec::with_capacity(PER_SEGMENT as usize);
    let mut pos = text.len() - body.len();
    for n in lens.split(' ') {
        let n: usize = n.parse().map_err(|_| damaged())?;
        let end = pos.checked_add(n).ok_or_else(damaged)?;
        // inside the text, and not in the middle of a character
        text.get(pos..end).ok_or_else(damaged)?;
        at.push((pos, end));
        pos = end;
    }
    if at.len() != PER_SEGMENT as usize || pos != text.len() {
        return Err(damaged());
    }
    Ok(Segment { text, at })
}

/// Bucket `b` out of its segment. It is marked as not empty, so an empty
/// text there is damage.
fn bucket_in(seg: &Segment, b: u32) -> Result<Bucket> {
    let t = seg.bucket(b);
    if t.is_empty() {
        return Err(Error::Format("index: a bucket is missing from its segment".into()));
    }
    parse_bucket(t)
}

/// `f` over `items`: on the calling thread for a few, spread over threads
/// for many; the results in the order of `items`, or the first error.
pub(crate) fn on_threads<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> Result<R> + Sync) -> Result<Vec<R>> {
    on_threads_most(items, usize::MAX, f)
}

/// [`on_threads`] on `most` threads at most (1: all of it on the calling
/// thread), for work that does not spread over CPUs on every file system
/// ([`crate::sys::name_threads`]).
pub(crate) fn on_threads_most<T: Sync, R: Send>(
    items: &[T],
    most: usize,
    f: impl Fn(&T) -> Result<R> + Sync,
) -> Result<Vec<R>> {
    if items.len() < 16 || most < 2 {
        return items.iter().map(&f).collect();
    }
    // Starting the threads costs about 1.4 ms. When what is left would take
    // less than about twice that on this thread, it stays here: on a warm
    // cache, the 244 Current links of a big system take 0.8 ms here and
    // 1.5 ms on 16 threads. A cold cache (each item waits for the disk), or
    // thousands of items (the buckets of a big install), go to the threads.
    // The time the items so far took tells which. More threads than these
    // for items that wait were tried (24, 48, 96 for the links of 39 and of
    // 244 packages from a cold cache) and read nothing sooner.
    let t0 = std::time::Instant::now();
    let mut out = Vec::with_capacity(items.len());
    let mut i = 0;
    while i < items.len() {
        out.push(f(&items[i])?);
        i += 1;
        let left = items.len() - i;
        if left >= 8 {
            // Work on a warm cache is judged after eight items. Items that
            // wait for the disk show after two (150 microseconds each is no
            // work of ours), and each further one done here is another wait
            // in a row: eight of them were 4 ms of a cold `add`.
            let spent = t0.elapsed();
            let waits = i >= 2 && spent / i as u32 > std::time::Duration::from_micros(150);
            if (i >= 8 || waits) && spent * left as u32 / i as u32 > std::time::Duration::from_millis(3) {
                break;
            }
        }
    }
    let items = &items[i..];
    if items.is_empty() {
        return Ok(out);
    }
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 16).min(most);
    let per = (items.len() + threads - 1) / threads;
    let f = &f;
    std::thread::scope(|sc| {
        let hs: Vec<_> = items.chunks(per).map(|part| sc.spawn(move || part.iter().map(f).collect::<Result<Vec<R>>>())).collect();
        for h in hs {
            out.extend(h.join().map_err(|_| Error::Format("index: a worker thread failed".into()))??);
        }
        Ok(out)
    })
}

/// Writes the segment files of `segs` into the directory `files`: each
/// bucket that `new` has as `new` has it; each other bucket that `present`
/// marks as not empty as it is in the segment's old text (given with the
/// segment); the rest empty. A segment left without any bucket is removed.
/// On several threads when there are many.
///
/// An existing file is cut to nothing and written again, not replaced
/// through a new name: creating and renaming take the directory's lock,
/// which would make the threads wait for each other. That is safe because
/// nothing reads the index while it is written (an update removes `state`
/// first, a rebuild writes a new directory), and every file carries its
/// own checksum. It is cut first, never written over in place: after a
/// power cut, a file written over could still hold its old text, whole and
/// with a good checksum; a file that was cut holds the new text or not
/// enough of anything.
///
/// With `staged` the files are new ones in a directory of their own
/// ([`stage`]) and nothing is removed. Returns, for each segment, whether
/// it has a file (false: it is left without any bucket).
fn write_segments(
    files: &Path,
    segs: &[(u32, Option<Arc<Segment>>)],
    new: &HashMap<u32, Bucket>,
    present: &[u8],
    staged: bool,
) -> Result<Vec<bool>> {
    // staged files are all new names in one directory: made on several
    // threads they would wait for each other for its lock (11 ms for 512
    // of them, 6 ms one after another), so there the threads only work the
    // texts out
    let done = on_threads(segs, |(s, old)| -> Result<(bool, Option<String>)> {
        let mut texts: Vec<std::borrow::Cow<str>> = Vec::with_capacity(PER_SEGMENT as usize);
        for b in *s * PER_SEGMENT..(*s + 1) * PER_SEGMENT {
            texts.push(match new.get(&b) {
                Some(bk) => format_bucket(bk).into(),
                None if has(present, b) => {
                    let t = old.as_ref().map_or("", |o| o.bucket(b));
                    if t.is_empty() {
                        return Err(Error::Format("index: a bucket is missing from its segment".into()));
                    }
                    t.into()
                }
                None => "".into(),
            });
        }
        let p = files.join(segment_name(*s));
        if texts.iter().all(|t| t.is_empty()) {
            if staged {
                return Ok((false, None));
            }
            return match fs::remove_file(&p) {
                Ok(()) => Ok((false, None)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok((false, None)),
                Err(e) => Err(e.into()),
            };
        }
        let mut rest = String::with_capacity(texts.iter().map(|t| t.len() + 2).sum::<usize>() + 1);
        for (i, t) in texts.iter().enumerate() {
            if i > 0 {
                rest.push(' ');
            }
            rest.push_str(&t.len().to_string());
        }
        rest.push('\n');
        for t in &texts {
            rest.push_str(t);
        }
        let data = format!("{SEGMENT_HEADER} {}\n{rest}", &sum_of(rest.as_bytes())[..SUM_LEN]);
        if staged {
            return Ok((true, Some(data)));
        }
        match fs::OpenOptions::new().write(true).truncate(true).open(&p) {
            Ok(mut f) => io::Write::write_all(&mut f, data.as_bytes())?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => fs::write(&p, data)?,
            Err(e) => return Err(e.into()),
        }
        Ok((true, None))
    })?;
    let mut kept = Vec::with_capacity(done.len());
    for ((s, _), (has, data)) in segs.iter().zip(done) {
        if let Some(data) = data {
            fs::write(files.join(segment_name(*s)), data)?;
        }
        kept.push(has);
    }
    Ok(kept)
}

/// The segments of these buckets, in order, each once.
fn segments_of<'a>(buckets: impl Iterator<Item = &'a u32>) -> Vec<u32> {
    let mut v: Vec<u32> = buckets.map(|&b| segment_of(b)).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Asks the system for the segment files that a change with these records
/// will read (`files` is the index's directory of them): [`updated`] reads
/// the bucket of every path of the change. A hint ([`crate::warm`]), given
/// while the package of the records is still being unpacked.
pub fn ask_for(files: &crate::warm::Files, recs: &[FileRec]) {
    let buckets: Vec<u32> = recs.iter().map(|r| bucket_of(&r.path)).collect();
    files.ask(segments_of(buckets.iter()).into_iter().map(segment_name).collect());
}

fn read_aliases(dir: &Path, sum: &str) -> Result<HashMap<Vec<u8>, Vec<(Vec<u8>, String)>>> {
    let t = fs::read_to_string(dir.join("aliases")).map_err(|e| Error::Format(format!("index: aliases: {e}")))?;
    if sum_of(t.as_bytes()) != sum {
        return Err(Error::Format("index: damaged aliases".into()));
    }
    let mut m: HashMap<Vec<u8>, Vec<(Vec<u8>, String)>> = HashMap::new();
    for line in t.lines().filter(|l| !l.is_empty()) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() != 3 {
            return Err(Error::Format("index: malformed alias line".into()));
        }
        m.entry(unescape(f[0])?).or_default().push((unescape(f[1])?, f[2].to_string()));
    }
    Ok(m)
}

fn format_aliases(m: &[(Vec<u8>, Vec<u8>, String)]) -> String {
    let mut v: Vec<&(Vec<u8>, Vec<u8>, String)> = m.iter().collect();
    v.sort();
    let mut s = String::new();
    for (c, p, n) in v {
        s.push_str(&format!("{}\t{}\t{n}\n", escape(c), escape(p)));
    }
    s
}

/// `meta` for `metas`. With `reuse`, a package that is not in its changed
/// set and whose entry was read is written back as it was (it would format
/// the same: the index is canonical).
fn format_meta<M: std::borrow::Borrow<PkgMeta>>(
    metas: &BTreeMap<String, M>,
    reuse: Option<(&HashMap<String, Vec<u8>>, &HashSet<String>)>,
) -> Vec<u8> {
    let mut out = Vec::new();
    for (n, m) in metas {
        let kept = reuse.and_then(|(raw, changed)| if changed.contains(n) { None } else { raw.get(n) });
        let formatted;
        let body: &[u8] = match kept {
            Some(b) => b,
            None => {
                formatted = crate::codec::encode_meta(m.borrow());
                &formatted
            }
        };
        out.extend_from_slice(format!("pkg {n} {}\n", body.len()).as_bytes());
        out.extend_from_slice(body);
    }
    out
}

fn format_state(
    versions: &BTreeMap<String, String>,
    links: &[(Vec<u8>, Vec<u8>)],
    present: &[u8],
    meta_sum: &str,
    aliases_sum: &str,
) -> String {
    let mut s = format!("{HEADER}\npackages\t{}\n", stamp(versions));
    for (p, t) in links {
        s.push_str(&format!("link\t{}\t{}\n", escape(p), escape(t)));
    }
    s.push_str(&format!("buckets\t{}\nmeta\t{meta_sum}\naliases\t{aliases_sum}\n", digest::hex(present)));
    let sum = sum_of(s.as_bytes());
    s.push_str(&format!("sum\t{sum}\nend\n"));
    s
}

/// Replaces `path` with `data` through a temporary name, without an fsync of
/// its own (see the module documentation for what makes it durable).
fn write_replace(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".new");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)
}

/// Removes the index's `state`, so that nothing uses the index until it is
/// written again.
pub fn invalidate(root: &Path) {
    let _ = fs::remove_file(dir(root).join("state"));
}

/// The canonical path of entry `path` in the live tree (its directories
/// resolved, the entry itself not followed), if its directory exists.
fn canon_of(rfs: &mut RootFs, path: &[u8]) -> Option<Vec<u8>> {
    rfs.entry_if_dir_exists(path).ok().flatten().map(|e| e.canon)
}

/// Whether symlink entry `r` (at canonical place `canon`) currently leads to
/// a directory in the live tree.
fn is_dir_link(rfs: &RootFs, r: &FileRec) -> bool {
    if !matches!(r.kind, RecKind::File(FileKind::Symlink(_))) {
        return false;
    }
    match rfs.resolve(&r.path, true) {
        Ok(res) => fs::metadata(&res.host).map_or(false, |m| m.is_dir()),
        Err(_) => false,
    }
}

/// Builds the whole index from the records of the final set `pkgs` (every
/// installed package, records loaded), with the live tree as it is now.
/// With `durable` it is synced before it replaces the old index (outside a
/// switch; inside one, the switch's final `syncfs` and its journal cover it).
pub fn rebuild(root: &Path, pkgs: &[PkgState], durable: bool) -> Result<()> {
    let mut rfs = RootFs::new(root);
    let mut buckets: HashMap<u32, Bucket> = HashMap::new();
    let mut aliases: Vec<(Vec<u8>, Vec<u8>, String)> = Vec::new();
    let mut links: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut versions = BTreeMap::new();
    let mut metas = BTreeMap::new();
    for p in pkgs {
        let name = p.meta.name().to_string();
        versions.insert(name.clone(), p.meta.version().to_string());
        metas.insert(name.clone(), p.meta.clone());
        for r in &p.recs {
            let c = buckets.entry(bucket_of(&r.path)).or_default().entry(r.path.clone()).or_default();
            let list = if r.is_dir() { &mut c.dirs } else { &mut c.files };
            if !list.contains(&name) {
                list.push(name.clone());
            }
            if let Some(canon) = canon_of(&mut rfs, &r.path) {
                if canon != r.path {
                    aliases.push((canon.clone(), r.path.clone(), name.clone()));
                }
                if let RecKind::File(FileKind::Symlink(t)) = &r.kind {
                    if is_dir_link(&rfs, r) && !links.iter().any(|(lp, _)| *lp == canon) {
                        links.push((canon, t.clone()));
                    }
                }
            }
        }
    }
    links.sort();
    let d = dir(root);
    let tmp = root.join(db::STATE_DIR).join("index.new");
    if fs::symlink_metadata(&tmp).is_ok() {
        fs::remove_dir_all(&tmp)?;
    }
    fs::create_dir_all(tmp.join("files"))?;
    let mut present = vec![0u8; (BUCKETS / 8) as usize];
    let list: Vec<(u32, Option<Arc<Segment>>)> = segments_of(buckets.keys()).into_iter().map(|s| (s, None)).collect();
    write_segments(&tmp.join("files"), &list, &buckets, &present, false)?;
    for b in buckets.keys() {
        set(&mut present, *b, true);
    }
    let al = format_aliases(&aliases);
    let meta = format_meta(&metas, None);
    fs::write(tmp.join("aliases"), &al)?;
    fs::write(tmp.join("meta"), &meta)?;
    fs::write(tmp.join("state"), format_state(&versions, &links, &present, &sum_of(&meta), &sum_of(al.as_bytes())))?;
    if durable {
        crate::sys::sync_fs(&fs::File::open(root)?)?;
    }
    invalidate(root);
    let old = root.join(db::STATE_DIR).join("index.old");
    if fs::symlink_metadata(&old).is_ok() {
        fs::remove_dir_all(&old)?;
    }
    if fs::symlink_metadata(&d).is_ok() {
        fs::rename(&d, &old)?;
    }
    fs::rename(&tmp, &d)?;
    let _ = fs::remove_dir_all(&old);
    Ok(())
}

/// The index of after a change, worked out from the one before it.
struct Updated {
    /// the buckets of the change's paths, as they will be
    buckets: HashMap<u32, Bucket>,
    /// the segments to write again, each with its old text
    segs: Vec<(u32, Option<Arc<Segment>>)>,
    aliases: String,
    meta: Vec<u8>,
    state: String,
}

/// Works out the index of after a change: the entries of the changed
/// packages replaced by those of their new versions (or dropped for removed
/// packages), in the buckets of their paths only. `olds` are the old
/// versions' states, `news` the new versions' (records loaded), `metas` the
/// final set's metadata; `canon` gives the canonical place of a new entry.
fn updated(
    ix: &Index,
    changed: &HashSet<String>,
    olds: &[&PkgState],
    news: &[&PkgState],
    metas: &BTreeMap<String, &PkgMeta>,
    canon: &mut dyn FnMut(&[u8]) -> Option<Vec<u8>>,
) -> Result<Updated> {
    let mut touched: HashSet<u32> = HashSet::new();
    for p in olds.iter().chain(news.iter()) {
        for r in &p.recs {
            touched.insert(bucket_of(&r.path));
        }
    }
    // everything read (and checked) first, while the old index is whole
    ix.load_many(touched.iter().copied())?;
    ix.aliases_loaded()?;
    // the segments that will be written again, with the buckets the change
    // does not touch as their files hold them
    let segs = ix.segments_for(&segments_of(touched.iter()))?;
    crate::prof::mark("  index: buckets read");
    let mut buckets: HashMap<u32, Bucket> = touched.iter().map(|&b| (b, ix.buckets.borrow()[&b].clone())).collect();
    for bk in buckets.values_mut() {
        for c in bk.values_mut() {
            c.files.retain(|n| !changed.contains(n));
            c.dirs.retain(|n| !changed.contains(n));
        }
        bk.retain(|_, c| !c.files.is_empty() || !c.dirs.is_empty());
    }
    let mut new_aliases: Vec<(Vec<u8>, Vec<u8>, String)> = Vec::new();
    for p in news {
        let name = p.meta.name().to_string();
        for r in &p.recs {
            let c = buckets.entry(bucket_of(&r.path)).or_default().entry(r.path.clone()).or_default();
            let list = if r.is_dir() { &mut c.dirs } else { &mut c.files };
            if !list.contains(&name) {
                list.push(name.clone());
            }
            if let Some(canon) = canon(&r.path) {
                if canon != r.path {
                    new_aliases.push((canon, r.path.clone(), name.clone()));
                }
            }
        }
    }
    crate::prof::mark("  index: claims merged");
    let mut present = ix.present.clone();
    for (b, bk) in &buckets {
        set(&mut present, *b, !bk.is_empty());
    }
    let mut aliases: Vec<(Vec<u8>, Vec<u8>, String)> = ix
        .aliases
        .borrow()
        .as_ref()
        .map(|m| {
            m.iter()
                .flat_map(|(c, v)| v.iter().map(move |(p, n)| (c.clone(), p.clone(), n.clone())))
                .filter(|(_, _, n)| !changed.contains(n))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    aliases.extend(new_aliases);
    let aliases = format_aliases(&aliases);
    let meta = format_meta(metas, Some((&ix.raw_meta.borrow(), changed)));
    let versions: BTreeMap<String, String> =
        metas.iter().map(|(n, m)| (n.clone(), m.version().to_string())).collect();
    let state = format_state(&versions, &ix.links, &present, &sum_of(&meta), &sum_of(aliases.as_bytes()));
    Ok(Updated { buckets, segs, aliases, meta, state })
}

/// After a switch that ends with a `syncfs`: the index is updated in place
/// ([`updated`]; `rfs` sees the live tree after the switch).
pub fn update(
    root: &Path,
    ix: &Index,
    changed: &HashSet<String>,
    olds: &[&PkgState],
    news: &[&PkgState],
    metas: &BTreeMap<String, &PkgMeta>,
    rfs: &mut RootFs,
) -> Result<()> {
    let d = dir(root);
    let u = updated(ix, changed, olds, news, metas, &mut |p| canon_of(rfs, p))?;
    invalidate(root);
    fs::create_dir_all(d.join("files"))?;
    write_segments(&d.join("files"), &u.segs, &u.buckets, &ix.present, false)?;
    crate::prof::mark("  index: buckets written");
    write_replace(&d.join("aliases"), u.aliases.as_bytes())?;
    write_replace(&d.join("meta"), &u.meta)?;
    write_replace(&d.join("state"), u.state.as_bytes())?;
    Ok(())
}

/// The directory, inside the index, of an update that waits ([`stage`]).
const NEXT: &str = "next";

/// An update of the index that waits in `next/`.
pub struct Staged {
    /// the segments whose new file waits there
    replace: Vec<u32>,
    /// the segments that end without any bucket
    remove: Vec<u32>,
}

/// Before a switch (and before the `syncfs` that precedes it): the index
/// of after the change is worked out ([`updated`]) and written into
/// `next/`, where nothing reads it. `rfs` sees the live tree before the
/// switch: a new entry's canonical place is where its directory is or will
/// be made ([`RootFs::dir_planned`]). That is only right for a change that
/// leaves the tree's directory symlinks alone and runs no script, which is
/// when a staged update is used ([`wanted`], and the switch).
pub fn stage(
    root: &Path,
    ix: &Index,
    changed: &HashSet<String>,
    olds: &[&PkgState],
    news: &[&PkgState],
    metas: &BTreeMap<String, &PkgMeta>,
    rfs: &mut RootFs,
) -> Result<Staged> {
    let next = dir(root).join(NEXT);
    discard_staged(root);
    let mut planned: HashMap<Vec<u8>, Vec<u8>> = HashMap::new();
    let u = updated(ix, changed, olds, news, metas, &mut |p| {
        let (d, name) = rootfs::split_rel(p);
        let canon = rootfs::join_rel(&rfs.dir_planned(d, &mut planned).ok()?, name);
        (!db::is_reserved(&canon)).then_some(canon)
    })?;
    fs::create_dir_all(next.join("files"))?;
    let kept = write_segments(&next.join("files"), &u.segs, &u.buckets, &ix.present, true)?;
    let (mut replace, mut remove) = (Vec::new(), Vec::new());
    for ((s, _), kept) in u.segs.iter().zip(kept) {
        if kept {
            replace.push(*s);
        } else {
            remove.push(*s);
        }
    }
    fs::write(next.join("aliases"), u.aliases.as_bytes())?;
    fs::write(next.join("meta"), &u.meta)?;
    fs::write(next.join("state"), u.state.as_bytes())?;
    crate::prof::mark("  index: staged");
    Ok(Staged { replace, remove })
}

/// At the end of a switch that ends without a `syncfs`: the staged update
/// is put in place by renames and removals alone. `state` goes first and
/// its successor comes last, so at no moment, and in nothing a power cut
/// can leave, is there a `state` beside files it does not describe.
pub fn commit_staged(root: &Path, st: &Staged) -> Result<()> {
    let d = dir(root);
    let next = d.join(NEXT);
    invalidate(root);
    fs::create_dir_all(d.join("files"))?;
    for s in &st.replace {
        fs::rename(next.join("files").join(segment_name(*s)), d.join("files").join(segment_name(*s)))?;
    }
    for s in &st.remove {
        match fs::remove_file(d.join("files").join(segment_name(*s))) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    fs::rename(next.join("aliases"), d.join("aliases"))?;
    fs::rename(next.join("meta"), d.join("meta"))?;
    fs::rename(next.join("state"), d.join("state"))?;
    let _ = fs::remove_dir_all(&next);
    Ok(())
}

/// Removes a staged update that was not used.
pub fn discard_staged(root: &Path) {
    let next = dir(root).join(NEXT);
    if fs::symlink_metadata(&next).is_ok() {
        let _ = fs::remove_dir_all(&next);
    }
}

/// Which unchanged packages' records a change needs, with a matching index
/// (`None`: every one, and the switch rebuilds the index): those with a
/// non-directory entry at a path of the change ([`relevant`]), and every one
/// with a trigger, which may fire for a directory the change modifies. All
/// are needed when the change touches the tree's directory symlinks
/// ([`touches_links`]), or when a new version has a trigger with a pattern
/// (it is given every installed directory that matches).
pub fn wanted(
    ix: &Index,
    rfs: &mut RootFs,
    installed: &BTreeMap<String, Installed>,
    changed: &HashSet<String>,
    olds: &[&PkgState],
    news: &[&PkgState],
) -> Option<HashSet<String>> {
    if touches_links(ix, rfs, olds, news) {
        return None;
    }
    let pattern = |t: &String| t.trim_start_matches('+').bytes().any(|c| matches!(c, b'*' | b'?' | b'[' | b'\\'));
    if news.iter().any(|p| p.meta.triggers.iter().any(pattern)) {
        return None;
    }
    // everything the switch may look up, read and checked now: a damaged
    // index is found before the switch, which then reads every record
    let all: Vec<&PkgState> = olds.iter().chain(news.iter()).copied().collect();
    crate::prof::mark("    index: directory links checked");
    if ix.preload(rfs, &all).is_err() {
        return None;
    }
    crate::prof::mark("    index: buckets of the change read");
    let mut w = relevant(ix, changed, olds, news).ok()?;
    crate::prof::mark("    index: other claimants found");
    for (name, ip) in installed {
        if !changed.contains(name) && !ip.meta.triggers.is_empty() {
            w.insert(name.clone());
        }
    }
    Some(w)
}

/// Whether a change of these packages touches the tree's directory
/// symlinks (then the index's aliases and links no longer hold, and the
/// switch works from every record and rebuilds the index): a link the index
/// knows belongs to a changed package, or a symlink of an old or new version
/// leads to a directory, now or once the new versions are placed, or
/// replaces one.
pub fn touches_links(ix: &Index, rfs: &mut RootFs, olds: &[&PkgState], news: &[&PkgState]) -> bool {
    let known: HashSet<&[u8]> = ix.links.iter().map(|(p, _)| p.as_slice()).collect();
    // What the new versions put at each path: a link's target may be one of
    // them. A target is worked out from the link's canonical place, so the
    // new entries are known by theirs as well as by the path their record
    // gives (`lib/bar` is `usr/lib/bar` where `lib` is a symlink): where a
    // new entry's directory is, or will be made.
    let mut new_at = NewAt { literal: HashMap::new(), canon: HashMap::new() };
    let mut planned: HashMap<Vec<u8>, Vec<u8>> = HashMap::new();
    for p in news {
        let mut now: Option<(&[u8], Vec<u8>)> = None;
        for r in &p.recs {
            new_at.literal.insert(r.path.as_slice(), r);
            let (dir, name) = rootfs::split_rel(&r.path);
            if now.as_ref().map_or(true, |(d, _)| *d != dir) {
                // a directory that cannot be worked out: read every record
                let Ok(c) = rfs.dir_planned(dir, &mut planned) else { return true };
                now = Some((dir, c));
            }
            if let Some((_, c)) = &now {
                if c.as_slice() != dir {
                    new_at.canon.insert(rootfs::join_rel(c, name), r);
                }
            }
        }
    }
    for p in olds.iter().chain(news.iter()) {
        for r in &p.recs {
            let RecKind::File(FileKind::Symlink(t)) = &r.kind else { continue };
            // where the link is: its directories resolved in the live tree, or
            // its own path while they don't exist yet
            let place = canon_of(rfs, &r.path).unwrap_or_else(|| r.path.clone());
            if known.contains(place.as_slice()) || known.contains(r.path.as_slice()) {
                return true;
            }
            if fs::symlink_metadata(rfs.host(&place)).map_or(false, |m| m.is_dir()) {
                return true;
            }
            if leads_to_dir(rfs, &new_at, &place, t, 0) {
                return true;
            }
        }
    }
    false
}

/// `p` with `.` and `..` worked out literally (no symlink followed).
fn normalize(p: &[u8]) -> Vec<u8> {
    let mut out: Vec<&[u8]> = Vec::new();
    for c in p.split(|&c| c == b'/') {
        match c {
            b"" | b"." => {}
            b".." => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out.join(&b'/')
}

/// The entries of the new versions, by the path of their record and (where
/// that is another) by their canonical place.
struct NewAt<'a> {
    literal: HashMap<&'a [u8], &'a FileRec>,
    canon: HashMap<Vec<u8>, &'a FileRec>,
}

impl<'a> NewAt<'a> {
    fn get(&self, path: &[u8]) -> Option<&'a FileRec> {
        self.literal.get(path).or_else(|| self.canon.get(path)).copied()
    }
}

/// Whether a symlink at `place` with target `t` leads to a directory, now or
/// once the new versions are placed: their entries come first (a link to one
/// of their links is followed, 8 deep at most; deeper counts as a
/// directory), then the live tree.
fn leads_to_dir(rfs: &mut RootFs, new_at: &NewAt, place: &[u8], t: &[u8], depth: u32) -> bool {
    if depth >= 8 {
        return true;
    }
    let target = if t.first() == Some(&b'/') {
        t[1..].to_vec()
    } else {
        rootfs::join_rel(rootfs::split_rel(place).0, t)
    };
    // the target as written, and with its directories resolved in the live tree
    let mut names = vec![normalize(&target)];
    if let Ok(res) = rfs.resolve(&target, false) {
        names.push(res.canon);
    }
    for n in &names {
        if let Some(r) = new_at.get(n.as_slice()) {
            if r.is_dir() {
                return true;
            }
            if let RecKind::File(FileKind::Symlink(t2)) = &r.kind {
                return leads_to_dir(rfs, new_at, n, t2, depth + 1);
            }
            return false;
        }
    }
    match rfs.resolve(&target, true) {
        Ok(res) => fs::metadata(&res.host).map_or(false, |m| m.is_dir()),
        Err(_) => true,
    }
}

/// The packages of the final set (other than `changed`) whose records the
/// switch must read: those with a non-directory entry at a path of a
/// changed package's old or new version.
pub fn relevant(ix: &Index, changed: &HashSet<String>, olds: &[&PkgState], news: &[&PkgState]) -> Result<HashSet<String>> {
    let mut out = HashSet::new();
    let mut seen: HashSet<&[u8]> = HashSet::new();
    for p in olds.iter().chain(news.iter()) {
        for r in &p.recs {
            if r.is_dir() || !seen.insert(r.path.as_slice()) {
                continue;
            }
            ix.with_bucket(&r.path, |c| {
                for n in c.iter().flat_map(|c| c.files.iter()) {
                    if !changed.contains(n) && !out.contains(n) {
                        out.insert(n.clone());
                    }
                }
            })?;
        }
    }
    Ok(out)
}

/// The installed versions found in `Programs` (only entries whose `Current`
/// names a real version directory), and whether any entry was not like
/// that.
pub fn installed_versions(root: &Path) -> io::Result<(BTreeMap<String, String>, bool)> {
    let programs = root.join(db::PROGRAMS);
    let mut out = BTreeMap::new();
    let mut odd = false;
    let rd = match fs::read_dir(&programs) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((out, false)),
        Err(e) => return Err(e),
    };
    let mut names = Vec::new();
    for e in rd {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if !e.file_type()?.is_dir() || !db::safe_component(&name) {
            odd = true;
            continue;
        }
        names.push(name);
    }
    // a readlink and a stat per package: on a cold cache each waits for the
    // disk, so they are done on several threads
    let found = on_threads(&names, |name| -> Result<Option<(String, Option<String>)>> {
        let d = programs.join(name);
        let Ok(t) = fs::read_link(d.join(db::CURRENT)) else { return Ok(None) };
        let v = t.to_string_lossy().into_owned();
        let ok = db::safe_version(&v) && fs::symlink_metadata(d.join(&v)).map_or(false, |m| m.is_dir());
        Ok(Some((name.clone(), ok.then_some(v))))
    })
    .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    for (name, v) in found.into_iter().flatten() {
        match v {
            Some(v) => {
                out.insert(name, v);
            }
            None => odd = true,
        }
    }
    Ok((out, odd))
}

/// The final set's records, keyed by name, for [`rebuild`] from `installed`.
pub fn load_all(installed: &BTreeMap<String, Installed>) -> Result<Vec<PkgState>> {
    installed.values().map(|ip| PkgState::load(&ip.dir, false)).collect()
}

/// The index as it was before the switch of `ops`, if it is intact and is
/// the one the switch was planned with (state stamp `planned`): finishing an
/// interrupted switch with it reads the records the switch read.
pub fn open_before(root: &Path, ops: &[db::Op], planned: &str) -> Option<Index> {
    let (mut v, odd) = installed_versions(root).ok()?;
    if odd {
        return None;
    }
    for op in ops {
        match op {
            db::Op::Install { name, .. } => {
                v.remove(name);
            }
            db::Op::Upgrade { name, old, .. } | db::Op::Remove { name, old } => {
                v.insert(name.clone(), old.clone());
            }
        }
    }
    if stamp(&v) != planned {
        return None;
    }
    Index::open(root, &v)
}

/// Removes what an interrupted rebuild left (`index.new`, `index.old`), and
/// an update that was staged and not used.
pub fn clean_leftovers(root: &Path) {
    discard_staged(root);
    for n in ["index.new", "index.old"] {
        let p = root.join(db::STATE_DIR).join(n);
        if fs::symlink_metadata(&p).map_or(false, |m| m.is_dir()) {
            let _ = fs::remove_dir_all(&p);
        }
    }
}
