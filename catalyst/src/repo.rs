//! Repositories: where indexes come from and where their packages are
//! (§G1).
//!
//! A repository is named by its index: a `Packages.adb` path or an
//! `http(s)://` URL (Valence writes one such line per repository into
//! `ROOT/etc/apk/repositories`), an "ndx" repository in apk's terms: its
//! packages are at `<index directory>/${name}-${version}.apk`. A line that
//! does not end in `.adb` is a base URL in the apk v3 sense: the index is
//! `URL/ARCH/Packages.adb` and packages are at
//! `URL/${arch}/${name}-${version}.apk` with the package's own arch (so
//! `noarch` packages are under `URL/noarch/`). An index's own
//! `pkgname-spec` replaces either default (§G1.5, §G1.6).
//!
//! Rollback protection: for every repository, what its last accepted index
//! offered (the newest `build-time` among its packages, and the newest
//! version of each package name) is kept in `ROOT/var/lib/catalyst/indexes/`.
//! A validly signed index that is older (an older newest build-time, or a
//! name offered only in a lower version than before: a mirror replaying an
//! old index to hold back fixes) is refused unless `--allow-rollback` is
//! given, and a downloaded index replaces the cached one only after it was
//! verified and found fresh. A root that never saw the repository has
//! nothing to compare with (a signed timestamp in the index would be
//! needed for that).

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use crate::adb::{self, Index, PkgInfo};
use crate::db;
use crate::digest;
use crate::idxcache;
use crate::version;
use crate::error::{Error, Result};
use crate::fetch::{self, Http, Limit};
use crate::rootfs::RootFs;
use crate::verify::{TrustInfo, TrustedKeys, VerifyOptions};

/// A loaded repository.
pub struct Repo {
    /// As configured.
    pub name: String,
    /// The index location (path or URL).
    pub index_url: String,
    /// Directory the package names are relative to.
    pub base: String,
    /// The package name template when the index has none.
    pub default_spec: String,
    pub remote: bool,
    pub index: Index,
    pub trust: TrustInfo,
    /// The newest build-time among its packages.
    pub generation: u64,
    /// The SHA-256 of the freshness record of what the index offers
    /// ([`Seen`]): equal to the stored record's when nothing changed.
    pub offer: [u8; 32],
    /// True if a remote refresh failed and the cached index was used.
    pub stale: bool,
    /// True if the cached index was used without asking the server: it was
    /// newer than the age the caller allows.
    pub reused: bool,
}

pub fn is_remote(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// A local location without its `file:` prefix.
pub fn local_path(url: &str) -> &str {
    url.strip_prefix("file://").or_else(|| url.strip_prefix("file:")).unwrap_or(url)
}

fn default_arch() -> String {
    match std::env::consts::ARCH {
        "x86" => "x86".into(),
        "arm" => "armv7".into(),
        a => a.into(),
    }
}

/// The architectures of a root: each non-empty line of
/// `ROOT/etc/apk/arch` (read inside the root), the first being the primary
/// one; else the architecture catalyst was built for.
pub fn root_arches(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    if let Ok(Some(b)) = RootFs::new(root).read_file(b"etc/apk/arch", 4096) {
        for l in String::from_utf8_lossy(&b).lines() {
            if l.is_empty() {
                continue;
            }
            let a = l.trim_end().to_string();
            if !v.contains(&a) {
                v.push(a);
            }
        }
    }
    if v.is_empty() {
        v.push(default_arch());
    }
    v
}

/// The primary architecture of a root.
pub fn root_arch(root: &Path) -> String {
    root_arches(root).swap_remove(0)
}

/// Repository lines of `ROOT/etc/apk/repositories`, then of the `*.list`
/// files in `ROOT/etc/apk/repositories.d` and `ROOT/lib/apk/repositories.d`
/// (sorted by name; a name in the first directory hides the same name in
/// the second, §G1.1). Everything is read inside the root. Empty lines and
/// `#` comments are skipped; a leading `ndx`/`v3`/`v2` keyword and `@tag`
/// are accepted (the tag is ignored).
pub fn read_repositories_file(root: &Path) -> Result<Vec<String>> {
    let rfs = RootFs::new(root);
    let mut files: Vec<Vec<u8>> = vec![b"etc/apk/repositories".to_vec()];
    let mut seen = std::collections::HashSet::new();
    let mut lists: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for d in ["etc/apk/repositories.d", "lib/apk/repositories.d"] {
        if let Ok(Some((canon, names))) = rfs.list_dir(d.as_bytes()) {
            for n in names {
                if n.starts_with(b".") || !n.ends_with(b".list") || !seen.insert(n.clone()) {
                    continue;
                }
                lists.push((n.clone(), crate::rootfs::join_rel(&canon, &n)));
            }
        }
    }
    lists.sort();
    files.extend(lists.into_iter().map(|(_, p)| p));
    let mut v = Vec::new();
    for f in files {
        let shown = format!("{}/{}", root.display(), String::from_utf8_lossy(&f));
        let t = match rfs.read_file(&f, 1 << 20) {
            Ok(Some(t)) => String::from_utf8_lossy(&t).into_owned(),
            Ok(None) => continue,
            Err(e) => return Err(Error::Msg(format!("{shown}: {e}"))),
        };
        v.extend(parse_repositories(&t).map_err(|m| Error::Msg(format!("{shown}: {m}")))?);
    }
    Ok(v)
}

fn parse_repositories(t: &str) -> std::result::Result<Vec<String>, String> {
    let mut v = Vec::new();
    for line in t.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        let mut i = 0;
        let mut kw = "";
        if matches!(words[0], "ndx" | "v3" | "v2") {
            kw = words[0];
            i = 1;
        }
        if words.get(i).map_or(false, |w| w.starts_with('@')) {
            i += 1;
        }
        let Some(url) = words.get(i) else { continue };
        if words.len() > i + 1 {
            return Err(format!("repository components are not supported: '{line}'"));
        }
        v.push(match kw {
            "v3" | "v2" => format!("v3 {url}"),
            "ndx" => format!("ndx {url}"),
            _ => url.to_string(),
        });
    }
    Ok(v)
}

/// Where a configured repository is: (index location, package base,
/// default package name template).
///
/// The type is decided on the raw word as apk does (§G1.4): ending in
/// `.adb` (or `.tar.gz`, which catalyst cannot read) makes it an index
/// location, anything else a base URL. apk would take a keyword-less base
/// URL as a v2 repository (`APKINDEX.tar.gz`); catalyst only reads v3 indexes
/// and looks for `Packages.adb` there.
pub fn locate(spec: &str, arch: &str) -> (String, String, String) {
    let (ndx, raw) = split_spec(spec);
    let url = raw.trim_end_matches('/');
    if ndx {
        let base = match url.rfind('/') {
            Some(i) => url[..i].to_string(),
            None => ".".to_string(),
        };
        (url.to_string(), base, "${name}-${version}.apk".to_string())
    } else {
        (format!("{url}/{arch}/Packages.adb"), url.to_string(), "${arch}/${name}-${version}.apk".to_string())
    }
}

/// What an accepted index offered: its generation (newest build-time) and
/// the newest version of each package name.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Seen {
    pub generation: u64,
    pub newest: BTreeMap<String, String>,
}

impl Seen {
    pub fn of(index: &Index) -> Seen {
        let mut newest: BTreeMap<String, String> = BTreeMap::new();
        for p in &index.packages {
            match newest.get(&p.name) {
                Some(v) if version::cmp(&p.version, v) != std::cmp::Ordering::Greater => {}
                _ => {
                    newest.insert(p.name.clone(), p.version.clone());
                }
            }
        }
        Seen { generation: generation(index), newest }
    }

    fn format(&self, index_url: &str) -> String {
        let mut t = format!("url\t{}\ngeneration\t{}\n", db::escape(index_url.as_bytes()), self.generation);
        for (n, v) in &self.newest {
            t.push_str(&format!("pkg\t{}\t{}\n", db::escape(n.as_bytes()), db::escape(v.as_bytes())));
        }
        t
    }

    fn parse(t: &str) -> Option<Seen> {
        let mut s = Seen::default();
        for l in t.lines() {
            let f: Vec<&str> = l.split('\t').collect();
            match f.as_slice() {
                ["generation", g] => s.generation = g.parse().ok()?,
                ["pkg", n, v] => {
                    let n = String::from_utf8(db::unescape(n).ok()?).ok()?;
                    let v = String::from_utf8(db::unescape(v).ok()?).ok()?;
                    s.newest.insert(n, v);
                }
                _ => {}
            }
        }
        Some(s)
    }
}

/// What was seen of each repository before (one file per repository in
/// `var/lib/catalyst/indexes/`, named by the SHA-256 of the index location),
/// and whether an older index may be accepted.
#[derive(Debug, Default, Clone)]
pub struct Freshness {
    /// The records as read, by location key; one is parsed only when an
    /// index offers something else than it says.
    records: HashMap<String, String>,
    pub allow_rollback: bool,
    dir: PathBuf,
}

/// What `index` offers as its freshness record has it, and the SHA-256 of
/// that record.
fn offer_of(index: &Index, index_url: &str) -> (Seen, [u8; 32]) {
    let now = Seen::of(index);
    let d = digest::sha256(now.format(index_url).as_bytes());
    (now, d)
}

fn url_key(index_url: &str) -> String {
    digest::hex(&digest::sha256(index_url.as_bytes()))
}

/// Where the index of a remote repository is kept once it was fetched.
fn cached_index(cache_dir: &Path, index_url: &str) -> PathBuf {
    cache_dir.join(format!("Packages.{}.adb", &url_key(index_url)[..8]))
}

/// A repository's line taken apart: whether it names the index file itself
/// (`ndx`, or no keyword and a name that ends in `.adb` or `.tar.gz`), and
/// the address in it.
fn split_spec(spec: &str) -> (bool, &str) {
    let (kw, raw) = match spec.split_once(' ') {
        Some((k @ ("v3" | "ndx"), u)) => (k, u.trim()),
        _ => ("", spec.trim()),
    };
    (kw == "ndx" || (kw.is_empty() && (raw.ends_with(".adb") || raw.ends_with(".tar.gz"))), raw)
}

/// Whether `spec` names its index file itself. If not it names the
/// repository's directory, and the index is the one of an architecture in
/// it: only then does [`locate`] use the architecture it is given.
pub fn names_its_index(spec: &str) -> bool {
    split_spec(spec).0
}

/// Whether the cached copy `dest` of a remote index is used without asking
/// the server: it is no older than `max_age` (`None`: the server is asked).
fn cached_is_fresh(dest: &Path, max_age: Option<std::time::Duration>) -> bool {
    max_age
        .and_then(|max_age| {
            let mtime = fs::metadata(dest).ok()?.modified().ok()?;
            let age = std::time::SystemTime::now().duration_since(mtime).ok()?;
            (age <= max_age).then_some(())
        })
        .is_some()
}

/// The files that loading `spec` reads from the disk: a local index and
/// its parsed copy; for a remote one the cached index and its parsed copy
/// when the cached index is used as it is (`max_age`, as for
/// [`load_with_cache`]), and the parsed copy alone when the server is asked
/// first (what it sends is looked up there). For a hint ([`crate::warm`]);
/// [`load_with_cache`] finds them by the same functions.
pub fn files_read(spec: &str, arch: &str, cache_dir: &Path, max_age: Option<std::time::Duration>) -> Vec<PathBuf> {
    let (index_url, _, _) = locate(spec, arch);
    let parsed = idxcache::path(cache_dir, &url_key(&index_url));
    if !is_remote(&index_url) {
        return vec![PathBuf::from(local_path(&index_url)), parsed];
    }
    let dest = cached_index(cache_dir, &index_url);
    if cached_is_fresh(&dest, max_age) {
        vec![dest, parsed]
    } else {
        vec![parsed]
    }
}

impl Freshness {
    pub fn load(root: &Path, allow_rollback: bool) -> Freshness {
        let dir = root.join(db::STATE_DIR).join("indexes");
        let mut records = HashMap::new();
        if let Ok(rd) = fs::read_dir(&dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.len() != 64 || !e.file_type().map_or(false, |t| t.is_file()) {
                    continue;
                }
                if let Ok(t) = fs::read_to_string(e.path()) {
                    records.insert(name, t);
                }
            }
        }
        Freshness { records, allow_rollback, dir }
    }

    /// Refuses an index that is older than the one accepted before from
    /// the same location: its newest build-time is older, or it offers a
    /// package name only in a lower version than before.
    #[cfg(test)]
    fn check(&self, index_url: &str, index: &Index) -> Result<()> {
        let (now, offer) = offer_of(index, index_url);
        self.check_offer(index_url, &offer, index, Some(now))
    }

    /// [`check`](Self::check) with what the index offers already known:
    /// `offer` (see [`Repo::offer`]) and, if computed, `now`. An index that
    /// offers what the record says is not compared further.
    fn check_offer(&self, index_url: &str, offer: &[u8; 32], index: &Index, now: Option<Seen>) -> Result<()> {
        let Some(text) = self.records.get(&url_key(index_url)) else { return Ok(()) };
        if digest::sha256(text.as_bytes()) == *offer {
            return Ok(());
        }
        // a record that cannot be read compares like none
        let Some(prev) = Seen::parse(text) else { return Ok(()) };
        let now = now.unwrap_or_else(|| Seen::of(index));
        let mut why = Vec::new();
        if now.generation < prev.generation {
            why.push(format!("newest package build time {} < {}", now.generation, prev.generation));
        }
        for (n, v) in &now.newest {
            if let Some(pv) = prev.newest.get(n) {
                if version::cmp(v, pv) == std::cmp::Ordering::Less {
                    why.push(format!("{n} {v} < {pv}"));
                }
            }
        }
        if why.is_empty() {
            return Ok(());
        }
        let more = if why.len() > 3 { format!(" and {} more", why.len() - 3) } else { String::new() };
        why.truncate(3);
        let msg = format!(
            "{index_url}: this index is older than the one used before ({}{more}): \
             a mirror may be replaying an old index to hold back updates",
            why.join(", ")
        );
        if self.allow_rollback {
            eprintln!("oct: warning: {msg}; accepted (--allow-rollback)");
            Ok(())
        } else {
            Err(Error::Untrusted(format!("{msg}; refused (--allow-rollback accepts it)")))
        }
    }

    /// Records what the repositories offer now (they passed [`check`]) in
    /// the root, for the next run to compare against.
    pub fn save(&mut self, repos: &[Repo]) -> Result<()> {
        for r in repos {
            let k = url_key(&r.index_url);
            if self.records.get(&k).map(|t| digest::sha256(t.as_bytes())) == Some(r.offer) {
                continue;
            }
            let text = Seen::of(&r.index).format(&r.index_url);
            if !db::real_dir(&self.dir)? {
                fs::create_dir(&self.dir)?;
            }
            db::write_atomic(&self.dir.join(&k), text.as_bytes())?;
            self.records.insert(k, text);
        }
        Ok(())
    }
}

fn generation(index: &Index) -> u64 {
    index.packages.iter().map(|p| p.build_time).max().unwrap_or(0)
}

/// Loads one repository index and checks its signature and freshness.
/// A remote index is downloaded to `<cache>.<pid>.new`, verified, checked
/// against `fresh`, and only then renamed over the cached copy; if the
/// download fails, the cached copy is used (and checked the same way, with
/// `Repo::stale` set). If `cache_max_age` is given and the cached copy is
/// newer than that, it is reused without hitting the network.
#[allow(clippy::too_many_arguments)]
pub fn load(
    spec: &str,
    arch: &str,
    keys: &TrustedKeys,
    opts: &VerifyOptions,
    http: Option<&Http>,
    cache_dir: &Path,
    fresh: &Freshness,
    cache_max_age: Option<std::time::Duration>,
) -> Result<Repo> {
    load_with_cache(spec, arch, keys, opts, http, cache_dir, fresh, cache_max_age, true, true)
}

/// Like [`load`], but writes the parsed-index cache (`var/cache/catalyst/parsed-*`)
/// only when `write_cache` is true (skipped under `--simulate` and read-only queries).
/// A downloaded index replaces the cached one when `write_cache` is true;
/// in a read-only query (`keep_download` without `write_cache`: search,
/// info) only when it was accepted without `--allow-untrusted` and
/// `--allow-rollback`; under `--simulate` (neither) never.
#[allow(clippy::too_many_arguments)]
pub fn load_with_cache(
    spec: &str,
    arch: &str,
    keys: &TrustedKeys,
    opts: &VerifyOptions,
    http: Option<&Http>,
    cache_dir: &Path,
    fresh: &Freshness,
    cache_max_age: Option<std::time::Duration>,
    write_cache: bool,
    keep_download: bool,
) -> Result<Repo> {
    let (index_url, base, default_spec) = locate(spec, arch);
    let remote = is_remote(&index_url);
    let at = |e: Error| prefix(e, &format!("{index_url}: "));
    let cache_file = idxcache::path(cache_dir, &url_key(&index_url));
    // verified and parsed, from the cache when this very index was before
    // (with these keys and options); the freshness check runs either way
    let read = |path: &Path| -> Result<(Index, TrustInfo, [u8; 32])> {
        let len = fs::metadata(path).map_err(|e| at(e.into()))?.len();
        if len > fetch::MAX_INDEX_FILE {
            let (index, trust) = adb::read_index_file(path, keys, opts).map_err(at)?;
            let (now, offer) = offer_of(&index, &index_url);
            fresh.check_offer(&index_url, &offer, &index, Some(now))?;
            return Ok((index, trust, offer));
        }
        let file = fs::read(path).map_err(|e| at(e.into()))?;
        let key = idxcache::key(&file, &index_url, keys, opts);
        if let Some(c) = idxcache::load(&cache_file, &key) {
            crate::prof::mark("  repositories: verified index from the cache");
            fresh.check_offer(&index_url, &c.offer, &c.index, None)?;
            crate::prof::mark("  repositories: freshness check");
            return Ok((c.index, c.trust, c.offer));
        }
        let (index, trust) = adb::read_index(&file[..], keys, opts).map_err(at)?;
        let (now, offer) = offer_of(&index, &index_url);
        fresh.check_offer(&index_url, &offer, &index, Some(now))?;
        crate::prof::mark("  repositories: freshness check");
        if write_cache {
            idxcache::store(&cache_file, &key, &index, &trust, &offer);
            crate::prof::mark("  repositories: cache written");
        }
        Ok((index, trust, offer))
    };
    let mut stale = false;
    let mut reused = false;
    let (index, trust, offer) = if remote {
        let dest = cached_index(cache_dir, &index_url);
        let cache_fresh = cached_is_fresh(&dest, cache_max_age);
        // A cached index that is new enough but cannot be used (it no longer
        // verifies with these keys, or is older than what was accepted
        // since) is no reason to stop: the server is asked, as for an old one.
        let cached = if cache_fresh { read(&dest).ok() } else { None };
        if let Some(x) = cached {
            reused = true;
            x
        } else {
            let http = http.ok_or_else(|| Error::Msg("no HTTP client".into()))?;
            let mut new = dest.as_os_str().to_os_string();
            new.push(format!(".{}.new", std::process::id()));
            let new = PathBuf::from(new);
            let got = http
                .download(&index_url, &new, Limit::Max(fetch::MAX_INDEX_FILE), &mut |_| {})
                .and_then(|_| read(&new));
            match got {
                Ok(x) => {
                    // what a run that changes nothing was shown must not
                    // become what the next `add` plans with, least of all an
                    // index let through by --allow-untrusted or --allow-rollback
                    if write_cache || (keep_download && !opts.allow_untrusted && !fresh.allow_rollback) {
                        // Unlike a package, the index is not followed by a
                        // syncfs (`update`, `search`, an `add` with nothing
                        // to do end without one): its content goes to disk
                        // before its name replaces the cached index, so
                        // that a power cut leaves the old index or the new
                        // one, not an empty file for a run without network.
                        fs::File::open(&new)?.sync_all()?;
                        fs::rename(&new, &dest)?;
                    } else {
                        let _ = fs::remove_file(&new);
                    }
                    x
                }
                Err(e) => {
                    let _ = fs::remove_file(&new);
                    if !dest.exists() || matches!(e, Error::Untrusted(_) | Error::Integrity(_)) {
                        return Err(e);
                    }
                    eprintln!("oct: warning: {e}; using the cached index");
                    stale = true;
                    read(&dest)?
                }
            }
        }
    } else {
        read(Path::new(local_path(&index_url)))?
    };
    let generation = generation(&index);
    for s in &index.skipped {
        eprintln!("oct: warning: {index_url}: {s}; skipped");
    }
    // a remote base needs the scheme; a local base is a path
    Ok(Repo {
        name: spec.to_string(),
        index_url,
        base: if remote { base } else { local_path(&base).to_string() },
        default_spec,
        remote,
        index,
        trust,
        generation,
        offer,
        stale,
        reused,
    })
}

fn prefix(e: Error, p: &str) -> Error {
    match e {
        Error::Io(e) => Error::Msg(format!("{p}{e}")),
        Error::Msg(m) => Error::Msg(format!("{p}{m}")),
        Error::Format(m) => Error::Format(format!("{p}{m}")),
        Error::Schema(m) => Error::Schema(format!("{p}{m}")),
        Error::Untrusted(m) => Error::Untrusted(format!("{p}{m}")),
        Error::Integrity(m) => Error::Integrity(format!("{p}{m}")),
        Error::Key(m) => Error::Key(format!("{p}{m}")),
        e => e,
    }
}

/// `apk_blob_subst` with the package variables (§G1.3, §G1.6).
/// `${var:N}` keeps at most N bytes; `${var:}` keeps none.
pub fn subst(spec: &str, p: &PkgInfo) -> Result<String> {
    let mut out: Vec<u8> = Vec::new();
    let mut rest = spec;
    while let Some(i) = rest.find("${") {
        out.extend_from_slice(rest[..i].as_bytes());
        let after = &rest[i + 2..];
        let end = after.find('}').ok_or_else(|| Error::Format(format!("pkgname-spec '{spec}': unterminated ${{")))?;
        let var = &after[..end];
        let (name, max) = match var.split_once(':') {
            Some((n, "")) => (n, Some(0)),
            Some((n, m)) => {
                let m: usize = m.parse().map_err(|_| Error::Format(format!("pkgname-spec '{spec}': bad length")))?;
                (n, Some(m))
            }
            None => (var, None),
        };
        let val = match name {
            "name" => p.name.clone(),
            "version" => p.version.clone(),
            "arch" => p.arch.clone(),
            "hash" => digest::hex(&p.hashes),
            _ => return Err(Error::Format(format!("pkgname-spec '{spec}': unknown variable '{name}'"))),
        };
        let vb = val.as_bytes();
        out.extend_from_slice(&vb[..max.map_or(vb.len(), |m| m.min(vb.len()))]);
        rest = &after[end + 1..];
    }
    out.extend_from_slice(rest.as_bytes());
    Ok(String::from_utf8_lossy(&out).into_owned())
}

impl Repo {
    /// Where a package of this repository is.
    pub fn package_url(&self, p: &PkgInfo) -> Result<String> {
        let spec = self.index.pkgname_spec.as_deref().unwrap_or(&self.default_spec);
        let s = subst(spec, p)?;
        if spec.contains("://") {
            Ok(s)
        } else {
            Ok(format!("{}/{}", self.base, s))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations() {
        let ndx = "${name}-${version}.apk".to_string();
        let v3 = "${arch}/${name}-${version}.apk".to_string();
        assert_eq!(
            locate("/srv/repo/x86_64/Packages.adb", "x86_64"),
            ("/srv/repo/x86_64/Packages.adb".into(), "/srv/repo/x86_64".into(), ndx.clone())
        );
        assert_eq!(
            locate("https://h/v/x86_64/Packages.adb", "x86_64"),
            ("https://h/v/x86_64/Packages.adb".into(), "https://h/v/x86_64".into(), ndx.clone())
        );
        assert_eq!(
            locate("https://h/v/", "x86_64"),
            ("https://h/v/x86_64/Packages.adb".into(), "https://h/v".into(), v3.clone())
        );
        assert_eq!(locate("v3 https://h/v", "aarch64").0, "https://h/v/aarch64/Packages.adb");
        assert_eq!(locate("ndx https://h/v/idx", "x86_64").0, "https://h/v/idx");
        assert_eq!(locate("Packages.adb", "x86_64"), ("Packages.adb".into(), ".".into(), ndx));
        // the .adb test is on the raw word, before trailing '/' are trimmed
        assert_eq!(locate("https://h/x/Packages.adb/", "x86_64").0, "https://h/x/Packages.adb/x86_64/Packages.adb");
    }

    #[test]
    fn package_urls() {
        let p = PkgInfo { name: "foo".into(), version: "1-r0".into(), arch: "noarch".into(), ..Default::default() };
        let mk = |spec: &str| {
            let (index_url, base, default_spec) = locate(spec, "x86_64");
            Repo {
                name: spec.into(),
                index_url,
                base,
                default_spec,
                remote: false,
                index: Index::default(),
                trust: TrustInfo::default(),
                generation: 0,
                offer: [0; 32],
                stale: false,
                reused: false,
            }
        };
        assert_eq!(mk("/r/x86_64/Packages.adb").package_url(&p).unwrap(), "/r/x86_64/foo-1-r0.apk");
        assert_eq!(mk("/r").package_url(&p).unwrap(), "/r/noarch/foo-1-r0.apk");
    }

    #[test]
    fn rollback_is_refused() {
        let idx = |pkgs: &[(&str, &str, u64)]| Index {
            packages: pkgs
                .iter()
                .map(|(n, v, t)| PkgInfo { name: n.to_string(), version: v.to_string(), build_time: *t, ..Default::default() })
                .collect(),
            ..Default::default()
        };
        let v2 = idx(&[("hello", "2.0-r0", 100), ("lib", "1-r0", 50)]);
        let mut f = Freshness::default();
        assert!(f.check("u", &v2).is_ok()); // nothing seen yet
        f.records.insert(url_key("u"), Seen::of(&v2).format("u"));
        assert_eq!(Seen::parse(&Seen::of(&v2).format("u")), Some(Seen::of(&v2)));
        // same, newer, a new name, a name removed: fine
        assert!(f.check("u", &v2).is_ok());
        assert!(f.check("u", &idx(&[("hello", "2.1-r0", 120), ("x", "1", 120)])).is_ok());
        // a name only in a lower version, or an older newest build-time
        assert_eq!(f.check("u", &idx(&[("hello", "1.0-r0", 100)])).unwrap_err().kind(), "untrusted");
        assert!(f.check("u", &idx(&[("hello", "2.0-r0", 90)])).is_err());
        // another location is not compared
        assert!(f.check("w", &idx(&[("hello", "1.0-r0", 1)])).is_ok());
        f.allow_rollback = true;
        assert!(f.check("u", &idx(&[("hello", "1.0-r0", 100)])).is_ok());
    }

    #[test]
    fn repositories_lines() {
        let v = parse_repositories("# c\n\n/r/x86_64/Packages.adb\nv3 @edge https://h/v\nndx /x/y\n").unwrap();
        assert_eq!(v, vec!["/r/x86_64/Packages.adb", "v3 https://h/v", "ndx /x/y"]);
        assert!(parse_repositories("https://h/v main community\n").is_err());
    }

    #[test]
    fn substitution() {
        let p = PkgInfo {
            name: "foo".into(),
            version: "1.0-r0".into(),
            arch: "x86_64".into(),
            hashes: vec![0xab, 0xcd, 0xef],
            ..Default::default()
        };
        assert_eq!(subst("${name}-${version}.apk", &p).unwrap(), "foo-1.0-r0.apk");
        assert_eq!(subst("${arch}/${name}.${hash:4}.apk", &p).unwrap(), "x86_64/foo.abcd.apk");
        assert_eq!(subst("${name:}x", &p).unwrap(), "x");
        assert!(subst("${nope}", &p).is_err());
        assert!(subst("${name", &p).is_err());
        assert_eq!(subst("$name}", &p).unwrap(), "$name}");
    }
}

#[cfg(test)]
mod files_read_tests {
    use super::*;

    /// What is named for a hint is found as loading a repository finds its
    /// files: by the same functions, and for a remote one by the same rule
    /// of when its cached index is used as it is. (That a run asks for the
    /// files it then reads is tested on a real repository.)
    #[test]
    fn the_files_named_for_a_hint_are_found_as_loading_finds_them() {
        let cache = std::env::temp_dir().join(format!("catalyst-files-read-{}", std::process::id()));
        let _ = fs::remove_dir_all(&cache);
        fs::create_dir_all(&cache).unwrap();
        let four_hours = Some(std::time::Duration::from_secs(4 * 3600));
        // a local index: where it is, and its parsed copy, whatever the age
        let local = files_read("/some/where/Packages.adb", "x86_64", &cache, None);
        assert_eq!(local.len(), 2);
        assert_eq!(local[0], PathBuf::from("/some/where/Packages.adb"));
        assert_eq!(local[1], idxcache::path(&cache, &url_key("/some/where/Packages.adb")));
        assert_eq!(files_read("file:///some/where/Packages.adb", "x86_64", &cache, four_hours)[0], PathBuf::from("/some/where/Packages.adb"));
        // a repository given by its directory: the index of the architecture
        assert_eq!(files_read("/some/where", "x86_64", &cache, None)[0], PathBuf::from("/some/where/x86_64/Packages.adb"));
        // which only such a one needs to be told
        assert!(names_its_index("/some/where/Packages.adb") && names_its_index("ndx https://example.org/an-index"));
        assert!(names_its_index("file:///some/where/APKINDEX.tar.gz"));
        assert!(!names_its_index("/some/where") && !names_its_index("v3 https://example.org/repo") && !names_its_index("https://example.org/repo"));
        assert_eq!(files_read("/some/where/Packages.adb", "", &cache, None), local);
        // a remote one of which nothing was fetched yet: the parsed copy alone
        let url = "https://example.org/repo";
        let index_url = "https://example.org/repo/x86_64/Packages.adb";
        let parsed = idxcache::path(&cache, &url_key(index_url));
        assert_eq!(files_read(url, "x86_64", &cache, four_hours), vec![parsed.clone()]);
        // fetched a moment ago: the cached index too, where a cached index is used as it is
        let dest = cached_index(&cache, index_url);
        fs::write(&dest, b"x").unwrap();
        assert!(cached_is_fresh(&dest, four_hours) && !cached_is_fresh(&dest, None));
        assert_eq!(files_read(url, "x86_64", &cache, four_hours), vec![dest.clone(), parsed.clone()]);
        assert_eq!(files_read(url, "x86_64", &cache, None), vec![parsed.clone()]);
        // fetched five hours ago: the server is asked first
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(5 * 3600);
        fs::File::options().write(true).open(&dest).unwrap().set_modified(old).unwrap();
        assert!(!cached_is_fresh(&dest, four_hours));
        assert_eq!(files_read(url, "x86_64", &cache, four_hours), vec![parsed]);
        fs::remove_dir_all(&cache).unwrap();
    }
}
