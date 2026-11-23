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
//!   file instead of one per package.
//! - `files/XXX` (4096 buckets by a hash of the path) hold, per path, the
//!   packages that have a non-directory entry there and those that have a
//!   directory there. A change reads and rewrites only the buckets of its
//!   own paths.
//! - `aliases` holds the entries that reach another place through a
//!   directory symlink (canonical path, literal path, package).
//!
//! Every file of the index is checked when it is read: `state` holds which
//! buckets are not empty, the SHA-256 of `meta` and of `aliases`, and a sum
//! of itself; each bucket starts with the SHA-256 of the rest of it. A
//! missing, truncated or changed file makes the index unusable, and the
//! change reads every record instead. Before a change goes through the
//! index, everything it may look up there is read ([`Index::preload`]), so
//! that damage is found before the switch, not in the middle of it.
//!
//! A change updates the index inside the switch, before its final
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
//! whether it was updated change by change or rebuilt.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::commit::PkgState;
use crate::adb::FileKind;
use crate::db::{self, escape, unescape, FileRec, Installed, PkgMeta, RecKind};
use crate::digest;
use crate::error::{Error, Result};
use crate::rootfs::{self, RootFs};

pub const INDEX_DIR: &str = "var/lib/catalyst/index";
const HEADER: &str = "catalyst-index 2";
/// A bucket file's first line: this, a space, and the SHA-256 of the rest.
const BUCKET_HEADER: &str = "catalyst-bucket";
const BUCKETS: u32 = 4096;

fn bucket_of(path: &[u8]) -> u32 {
    // FNV-1a
    let mut h: u32 = 0x811c_9dc5;
    for &b in path {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h % BUCKETS
}

fn bucket_name(b: u32) -> String {
    format!("{b:03x}")
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
    let damaged = || Error::Format("index: damaged bucket".into());
    let (head, body) = text.split_once('\n').ok_or_else(damaged)?;
    let sum = head.strip_prefix(BUCKET_HEADER).and_then(|s| s.strip_prefix(' ')).ok_or_else(damaged)?;
    if sum_of(body.as_bytes()) != sum {
        return Err(damaged());
    }
    let mut b = Bucket::new();
    for line in body.lines() {
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() != 3 {
            return Err(Error::Format("index: malformed bucket line".into()));
        }
        b.insert(unescape(f[0])?, Claims { files: parse_names(f[1]), dirs: parse_names(f[2]) });
    }
    Ok(b)
}

fn format_bucket(b: &Bucket) -> String {
    let mut body = String::new();
    for (p, c) in b {
        body.push_str(&format!("{}\t{}\t{}\n", escape(p), names_field(&c.files), names_field(&c.dirs)));
    }
    format!("{BUCKET_HEADER} {}\n{body}", sum_of(body.as_bytes()))
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
    aliases: std::cell::RefCell<Option<HashMap<Vec<u8>, Vec<(Vec<u8>, String)>>>>,
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
        for (p, t) in &st.links {
            let l = fs::read_link(root.join(std::ffi::OsStr::from_bytes(p))).ok()?;
            if l.as_os_str().as_bytes() != t.as_slice() {
                return None;
            }
        }
        Some(Index {
            dir,
            links: st.links,
            present: st.present,
            meta_sum: st.meta_sum,
            aliases_sum: st.aliases_sum,
            buckets: Default::default(),
            aliases: Default::default(),
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
            let m = db::parse_info(std::str::from_utf8(body).map_err(|_| Error::Format("index: meta".into()))?)?;
            if m.info.name != *name {
                return Err(Error::Format("index: meta".into()));
            }
            out.insert(name.to_string(), m);
            i = nl + 1 + len;
        }
        Ok(out)
    }

    /// Bucket `b`, read and checked once (lookups then use the copy). A bucket
    /// marked as not empty whose file is missing or damaged is an error.
    fn load(&self, b: u32) -> Result<()> {
        let mut cache = self.buckets.borrow_mut();
        if !cache.contains_key(&b) {
            let bk = if has(&self.present, b) {
                let p = self.dir.join("files").join(bucket_name(b));
                let t = fs::read_to_string(&p).map_err(|e| Error::Format(format!("index: bucket {}: {e}", bucket_name(b))))?;
                parse_bucket(&t)?
            } else {
                Bucket::new()
            };
            cache.insert(b, bk);
        }
        Ok(())
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

    /// Reads, and so checks, everything about these packages' paths that a
    /// switch may look up: the buckets of the paths, of their directories and
    /// of their canonical places, of the directories their triggers name, and
    /// the aliases. Damage there is then found before the switch.
    pub fn preload(&self, rfs: &mut RootFs, pkgs: &[&PkgState]) -> Result<()> {
        self.aliases_loaded()?;
        for p in pkgs {
            for r in &p.recs {
                self.load(bucket_of(&r.path))?;
                self.load(bucket_of(rootfs::split_rel(&r.path).0))?;
                if let Some(c) = canon_of(rfs, &r.path) {
                    self.load(bucket_of(&c))?;
                }
            }
            for t in &p.meta.triggers {
                self.load(bucket_of(t.trim_start_matches('+').trim_matches('/').as_bytes()))?;
            }
        }
        Ok(())
    }
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

fn format_meta(metas: &BTreeMap<String, PkgMeta>) -> Vec<u8> {
    let mut out = Vec::new();
    for (n, m) in metas {
        let body = db::format_info(m);
        out.extend_from_slice(format!("pkg {n} {}\n", body.len()).as_bytes());
        out.extend_from_slice(body.as_bytes());
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
    for (b, bk) in &buckets {
        fs::write(tmp.join("files").join(bucket_name(*b)), format_bucket(bk))?;
        set(&mut present, *b, true);
    }
    let al = format_aliases(&aliases);
    let meta = format_meta(&metas);
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

/// After a switch: the entries of the changed packages are replaced by
/// those of their new versions (or dropped for removed packages), in the
/// buckets of their paths only. `olds` are the old versions' states, `news`
/// the new versions' (records loaded), `metas` the final set's metadata.
/// `rfs` sees the live tree after the switch.
pub fn update(
    root: &Path,
    ix: &Index,
    changed: &HashSet<String>,
    olds: &[&PkgState],
    news: &[&PkgState],
    metas: &BTreeMap<String, PkgMeta>,
    rfs: &mut RootFs,
) -> Result<()> {
    let d = dir(root);
    let mut touched: HashSet<u32> = HashSet::new();
    for p in olds.iter().chain(news.iter()) {
        for r in &p.recs {
            touched.insert(bucket_of(&r.path));
        }
    }
    // everything read (and checked) first, while the old index is whole
    for &b in &touched {
        ix.load(b)?;
    }
    ix.aliases_loaded()?;
    invalidate(root);
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
            if let Some(canon) = canon_of(rfs, &r.path) {
                if canon != r.path {
                    new_aliases.push((canon, r.path.clone(), name.clone()));
                }
            }
        }
    }
    fs::create_dir_all(d.join("files"))?;
    let mut present = ix.present.clone();
    for (b, bk) in &buckets {
        let p = d.join("files").join(bucket_name(*b));
        if bk.is_empty() {
            match fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            set(&mut present, *b, false);
        } else {
            write_replace(&p, format_bucket(bk).as_bytes())?;
            set(&mut present, *b, true);
        }
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
    let al = format_aliases(&aliases);
    let meta = format_meta(metas);
    write_replace(&d.join("aliases"), al.as_bytes())?;
    write_replace(&d.join("meta"), &meta)?;
    let versions: BTreeMap<String, String> =
        metas.iter().map(|(n, m)| (n.clone(), m.version().to_string())).collect();
    let state = format_state(&versions, &ix.links, &present, &sum_of(&meta), &sum_of(al.as_bytes()));
    write_replace(&d.join("state"), state.as_bytes())?;
    Ok(())
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
    if ix.preload(rfs, &all).is_err() {
        return None;
    }
    let mut w = relevant(ix, changed, olds, news).ok()?;
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
    // what the new versions put at each path: a link's target may be one of them
    let mut new_at: HashMap<&[u8], &FileRec> = HashMap::new();
    for p in news {
        for r in &p.recs {
            new_at.insert(r.path.as_slice(), r);
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

/// Whether a symlink at `place` with target `t` leads to a directory, now or
/// once the new versions are placed: their entries come first (a link to one
/// of their links is followed, 8 deep at most; deeper counts as a
/// directory), then the live tree.
fn leads_to_dir(rfs: &mut RootFs, new_at: &HashMap<&[u8], &FileRec>, place: &[u8], t: &[u8], depth: u32) -> bool {
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
            for n in ix.file_claimants(&r.path)? {
                if !changed.contains(&n) {
                    out.insert(n);
                }
            }
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
    for e in rd {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if !e.file_type()?.is_dir() || !db::safe_component(&name) {
            odd = true;
            continue;
        }
        let t = match fs::read_link(e.path().join(db::CURRENT)) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let v = t.to_string_lossy().into_owned();
        if !db::safe_version(&v) || !fs::symlink_metadata(e.path().join(&v)).map_or(false, |m| m.is_dir()) {
            odd = true;
            continue;
        }
        out.insert(name, v);
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

/// Removes what an interrupted rebuild left (`index.new`, `index.old`).
pub fn clean_leftovers(root: &Path) {
    for n in ["index.new", "index.old"] {
        let p = root.join(db::STATE_DIR).join(n);
        if fs::symlink_metadata(&p).map_or(false, |m| m.is_dir()) {
            let _ = fs::remove_dir_all(&p);
        }
    }
}
