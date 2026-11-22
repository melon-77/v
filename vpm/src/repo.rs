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
//! version of each package name) is kept in `ROOT/var/lib/vpm/indexes/`.
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
    /// True if a remote refresh failed and the cached index was used.
    pub stale: bool,
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
/// one; else the architecture vpm was built for.
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
/// `.adb` (or `.tar.gz`, which vpm cannot read) makes it an index
/// location, anything else a base URL. apk would take a keyword-less base
/// URL as a v2 repository (`APKINDEX.tar.gz`); vpm only reads v3 indexes
/// and looks for `Packages.adb` there.
pub fn locate(spec: &str, arch: &str) -> (String, String, String) {
    let (kw, raw) = match spec.split_once(' ') {
        Some((k @ ("v3" | "ndx"), u)) => (k, u.trim()),
        _ => ("", spec.trim()),
    };
    let ndx = kw == "ndx" || (kw.is_empty() && (raw.ends_with(".adb") || raw.ends_with(".tar.gz")));
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
/// `var/lib/vpm/indexes/`, named by the SHA-256 of the index location),
/// and whether an older index may be accepted.
#[derive(Debug, Default, Clone)]
pub struct Freshness {
    pub seen: HashMap<String, Seen>,
    pub allow_rollback: bool,
    /// Optional maximum age for remote indexes (enforced even on a fresh root
    /// that has never seen the repository before).
    pub max_age: Option<std::time::Duration>,
    dir: PathBuf,
}

fn url_key(index_url: &str) -> String {
    digest::hex(&digest::sha256(index_url.as_bytes()))
}

/// Converts a UTC civil date/time to Unix epoch seconds (Howard Hinnant's
/// civil-to-days algorithm; valid for all Gregorian years >= 1970).
fn utc_to_unix(year: i64, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> Option<u64> {
    if year < 1970 || !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 59 {
        return None;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
    let max_day = match month {
        2 => if leap { 29 } else { 28 },
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day > max_day {
        return None;
    }
    let y = year - if month <= 2 { 1 } else { 0 };
    let era = y.div_euclid(400);
    let yoe = y - era * 400; // [0, 399]
    let m = month as i64;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + (day as i64) - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    let days = era * 146_097 + doe - 719_468;
    if days < 0 {
        return None;
    }
    Some((days as u64) * 86_400 + (hour as u64) * 3600 + (min as u64) * 60 + (sec as u64))
}

/// Parses a signed UTC timestamp from an index's `description` field
/// (`NDX_DESCRIPTION`), supporting `YYYYMMDDTHHMMSSZ` (preferred) and legacy
/// `YYYYMMDD` tokens (e.g. `"Valence 20261122T164500Z"` or `"Valence 20261122"`).
pub fn parse_description_timestamp(desc: &str) -> Option<u64> {
    let mut best: Option<u64> = None;
    for tok in desc.split_whitespace() {
        let b = tok.as_bytes();
        let ts = if b.len() == 16 && b[8] == b'T' && b[15] == b'Z' {
            let y: i64 = tok[0..4].parse().ok()?;
            let mo: u32 = tok[4..6].parse().ok()?;
            let d: u32 = tok[6..8].parse().ok()?;
            let h: u32 = tok[9..11].parse().ok()?;
            let mi: u32 = tok[11..13].parse().ok()?;
            let s: u32 = tok[13..15].parse().ok()?;
            utc_to_unix(y, mo, d, h, mi, s)
        } else if b.len() == 8 && b.iter().all(|c| c.is_ascii_digit()) {
            let y: i64 = tok[0..4].parse().ok()?;
            let mo: u32 = tok[4..6].parse().ok()?;
            let d: u32 = tok[6..8].parse().ok()?;
            utc_to_unix(y, mo, d, 0, 0, 0)
        } else {
            None
        };
        if let Some(t) = ts {
            best = Some(best.map_or(t, |cur| cur.max(t)));
        }
    }
    best
}

impl Freshness {
    pub fn load(root: &Path, allow_rollback: bool) -> Freshness {
        Self::load_with_max_age(root, allow_rollback, None)
    }

    pub fn load_with_max_age(root: &Path, allow_rollback: bool, max_age: Option<std::time::Duration>) -> Freshness {
        let dir = root.join(db::STATE_DIR).join("indexes");
        let mut seen = HashMap::new();
        if let Ok(rd) = fs::read_dir(&dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.len() != 64 || !e.file_type().map_or(false, |t| t.is_file()) {
                    continue;
                }
                if let Some(s) = fs::read_to_string(e.path()).ok().and_then(|t| Seen::parse(&t)) {
                    seen.insert(name, s);
                }
            }
        }
        Freshness { seen, allow_rollback, max_age, dir }
    }

    /// Refuses an index that is older than the one accepted before from
    /// the same location, or (for remote indexes when `max_age` is set)
    /// older than `now - max_age` even on a brand-new root.
    fn check(&self, index_url: &str, index: &Index, remote: bool) -> Result<()> {
        let now_seen = Seen::of(index);
        if remote {
            if let Some(max_age) = self.max_age {
                let wall_now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let limit = wall_now.saturating_sub(max_age.as_secs());
                if now_seen.generation < limit {
                    let age_days = wall_now.saturating_sub(now_seen.generation) / 86_400;
                    let max_days = max_age.as_secs() / 86_400;
                    let msg = format!(
                        "{index_url}: signed index timestamp is {age_days}d old (exceeds --max-index-age {max_days}d)"
                    );
                    if self.allow_rollback {
                        eprintln!("vpm: warning: {msg}; accepted (--allow-rollback)");
                    } else {
                        return Err(Error::Untrusted(format!("{msg}; refused (--allow-rollback accepts it)")));
                    }
                }
            }
        }
        let Some(prev) = self.seen.get(&url_key(index_url)) else { return Ok(()) };
        let mut why = Vec::new();
        if now_seen.generation < prev.generation {
            why.push(format!("index timestamp/build time {} < {}", now_seen.generation, prev.generation));
        }
        for (n, v) in &now_seen.newest {
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
            eprintln!("vpm: warning: {msg}; accepted (--allow-rollback)");
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
            let now = Seen::of(&r.index);
            if self.seen.get(&k) == Some(&now) {
                continue;
            }
            if !db::real_dir(&self.dir)? {
                fs::create_dir(&self.dir)?;
            }
            db::write_atomic(&self.dir.join(&k), now.format(&r.index_url).as_bytes())?;
            self.seen.insert(k, now);
        }
        Ok(())
    }
}

fn generation(index: &Index) -> u64 {
    let pkg_max = index.packages.iter().map(|p| p.build_time).max().unwrap_or(0);
    let desc_ts = parse_description_timestamp(&index.description).unwrap_or(0);
    pkg_max.max(desc_ts)
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
    let (index_url, base, default_spec) = locate(spec, arch);
    let remote = is_remote(&index_url);
    let read = |path: &Path| -> Result<(Index, TrustInfo, u64)> {
        let (index, trust) =
            adb::read_index_file(path, keys, opts).map_err(|e| prefix(e, &format!("{index_url}: ")))?;
        fresh.check(&index_url, &index, remote)?;
        let g = generation(&index);
        Ok((index, trust, g))
    };
    let mut stale = false;
    let (index, trust, generation) = if remote {
        let name = format!("Packages.{}.adb", &url_key(&index_url)[..8]);
        let dest = cache_dir.join(name);
        let cache_fresh = cache_max_age.and_then(|max_age| {
            let mtime = fs::metadata(&dest).ok()?.modified().ok()?;
            let age = std::time::SystemTime::now().duration_since(mtime).ok()?;
            if age <= max_age { Some(()) } else { None }
        });
        if cache_fresh.is_some() {
            read(&dest)?
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
                    fs::rename(&new, &dest)?;
                    x
                }
                Err(e) => {
                    let _ = fs::remove_file(&new);
                    if !dest.exists() || matches!(e, Error::Untrusted(_) | Error::Integrity(_)) {
                        return Err(e);
                    }
                    eprintln!("vpm: warning: {e}; using the cached index");
                    stale = true;
                    read(&dest)?
                }
            }
        }
    } else {
        read(Path::new(local_path(&index_url)))?
    };
    for s in &index.skipped {
        eprintln!("vpm: warning: {index_url}: {s}; skipped");
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
        stale,
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
                stale: false,
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
        assert!(f.check("u", &v2, false).is_ok()); // nothing seen yet
        f.seen.insert(url_key("u"), Seen::of(&v2));
        assert_eq!(Seen::parse(&Seen::of(&v2).format("u")), Some(Seen::of(&v2)));
        // same, newer, a new name, a name removed: fine
        assert!(f.check("u", &v2, false).is_ok());
        assert!(f.check("u", &idx(&[("hello", "2.1-r0", 120), ("x", "1", 120)]), false).is_ok());
        // a name only in a lower version, or an older newest build-time
        assert_eq!(f.check("u", &idx(&[("hello", "1.0-r0", 100)]), false).unwrap_err().kind(), "untrusted");
        assert!(f.check("u", &idx(&[("hello", "2.0-r0", 90)]), false).is_err());
        // another location is not compared
        assert!(f.check("w", &idx(&[("hello", "1.0-r0", 1)]), false).is_ok());
        f.allow_rollback = true;
        assert!(f.check("u", &idx(&[("hello", "1.0-r0", 100)]), false).is_ok());
    }

    #[test]
    fn signed_timestamp_and_fresh_root_max_age() {
        // 1970-01-01T00:00:00Z = 0; 2024-02-29T23:59:59Z = 1709251199
        assert_eq!(parse_description_timestamp("Valence 19700101T000000Z"), Some(0));
        assert_eq!(parse_description_timestamp("Valence 20240229T235959Z"), Some(1_709_251_199));
        assert_eq!(parse_description_timestamp("Valence 20240229"), Some(1_709_164_800));
        assert_eq!(parse_description_timestamp("Valence 20230229"), None); // not a leap year
        assert_eq!(parse_description_timestamp("no date here"), None);

        // Fresh root with max_age refuses a remote index signed in 2020, allows local, allows --allow-rollback
        let old_idx = Index {
            description: "Valence 20200101T000000Z".into(),
            ..Default::default()
        };
        let mut f = Freshness {
            max_age: Some(std::time::Duration::from_secs(30 * 86_400)),
            ..Default::default()
        };
        assert_eq!(f.check("https://example.org/Packages.adb", &old_idx, true).unwrap_err().kind(), "untrusted");
        assert!(f.check("/local/Packages.adb", &old_idx, false).is_ok());
        f.allow_rollback = true;
        assert!(f.check("https://example.org/Packages.adb", &old_idx, true).is_ok());
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
