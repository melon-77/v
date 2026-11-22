//! vpm's database inside a managed root (SQLite metadata + GoboLinux-style
//! `/Programs` file layout).
//!
//! ```text
//! ROOT/Programs/<name>/<version>/...          the package's unpacked files
//! ROOT/Programs/<name>/Current -> <version>   the version in use
//! ROOT/var/lib/vpm/state.db                   SQLite primary metadata DB
//! ROOT/var/lib/vpm/world                      plain-text mirror of world
//! ROOT/var/lib/vpm/journal                    the switch in progress
//! ROOT/var/lib/vpm/lock                       flock'ed while vpm runs
//! ROOT/var/cache/vpm/                         downloaded packages
//! ```
//!
//! `state.db` holds package metadata, dependencies/provides/replaces/install-if/
//! recommends, tags, triggers, scripts, indexed file manifests, and the world.
//! `/Programs/<name>/<version>` holds only the unpacked package files;
//! `Programs/<name>/Current` points at the active version.
//!
//! Legacy roots created before the SQLite transition (with per-package
//! `Programs/<name>/<version>/.vpm/{info,files,scripts}`) are migrated
//! automatically into `state.db` on the first writable open, and can still be
//! read transparently by read-only commands before migration.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::adb::{self, Dependency, FileKind, Package, PkgInfo};
use crate::digest;
use crate::error::{Error, Result};
use crate::sqlite::{Connection, Statement};
use crate::sys;

pub const PROGRAMS: &str = "Programs";
pub const CURRENT: &str = "Current";
pub const META_DIR: &str = ".vpm";
pub const PARTIAL_SUFFIX: &str = ".partial";
pub const STATE_DIR: &str = "var/lib/vpm";
pub const STATE_DB: &str = "state.db";
pub const CACHE_DIR: &str = "var/cache/vpm";

const SCHEMA_VERSION: i64 = 2;

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS packages (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    name              TEXT NOT NULL,
    version           TEXT NOT NULL,
    dir_name          TEXT NOT NULL,
    state             TEXT NOT NULL CHECK (state IN ('staged', 'installed', 'removing')),
    arch              TEXT NOT NULL DEFAULT '',
    description       TEXT NOT NULL DEFAULT '',
    url               TEXT NOT NULL DEFAULT '',
    license           TEXT NOT NULL DEFAULT '',
    origin            TEXT NOT NULL DEFAULT '',
    maintainer        TEXT NOT NULL DEFAULT '',
    build_time        INTEGER NOT NULL DEFAULT 0,
    installed_size    INTEGER NOT NULL DEFAULT 0,
    file_size         INTEGER NOT NULL DEFAULT 0,
    identity          BLOB NOT NULL DEFAULT X'',
    repo_commit       BLOB NOT NULL DEFAULT X'',
    provider_priority INTEGER NOT NULL DEFAULT 0,
    replaces_priority INTEGER NOT NULL DEFAULT 0,
    layer             INTEGER NOT NULL DEFAULT 0,
    source            TEXT NOT NULL DEFAULT '',
    UNIQUE(name, dir_name)
);
CREATE INDEX IF NOT EXISTS idx_packages_state ON packages(state, name);

CREATE TABLE IF NOT EXISTS pkg_deps (
    pkg_id   INTEGER NOT NULL REFERENCES packages(id) ON DELETE CASCADE,
    kind     TEXT NOT NULL,
    ord      INTEGER NOT NULL,
    name     TEXT NOT NULL,
    version  TEXT NOT NULL,
    op       INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_pkg_deps_pkg ON pkg_deps(pkg_id, kind, ord);
CREATE INDEX IF NOT EXISTS idx_pkg_deps_provide ON pkg_deps(kind, name);

CREATE TABLE IF NOT EXISTS pkg_tags (
    pkg_id   INTEGER NOT NULL REFERENCES packages(id) ON DELETE CASCADE,
    ord      INTEGER NOT NULL,
    tag      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_pkg_tags_pkg ON pkg_tags(pkg_id, ord);

CREATE TABLE IF NOT EXISTS pkg_triggers (
    pkg_id   INTEGER NOT NULL REFERENCES packages(id) ON DELETE CASCADE,
    ord      INTEGER NOT NULL,
    pattern  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_pkg_triggers_pkg ON pkg_triggers(pkg_id, ord);

CREATE TABLE IF NOT EXISTS pkg_scripts (
    pkg_id   INTEGER NOT NULL REFERENCES packages(id) ON DELETE CASCADE,
    kind     TEXT NOT NULL,
    body     BLOB NOT NULL,
    PRIMARY KEY (pkg_id, kind)
);

CREATE TABLE IF NOT EXISTS pkg_files (
    pkg_id   INTEGER NOT NULL REFERENCES packages(id) ON DELETE CASCADE,
    ord      INTEGER NOT NULL,
    kind     TEXT NOT NULL,
    mode     INTEGER NOT NULL,
    user     TEXT,
    grp      TEXT,
    size     INTEGER NOT NULL,
    mtime    INTEGER NOT NULL,
    hash     BLOB NOT NULL DEFAULT X'',
    extra    BLOB,
    dev      INTEGER,
    xattrs   BLOB,
    path     BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_pkg_files_pkg ON pkg_files(pkg_id, ord);
CREATE INDEX IF NOT EXISTS idx_pkg_files_path ON pkg_files(path);

CREATE TABLE IF NOT EXISTS world (
    spec     TEXT PRIMARY KEY
);
";

/// What a file record describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecKind {
    Dir,
    File(FileKind),
}

/// One entry of a package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRec {
    pub kind: RecKind,
    pub mode: u32,
    pub user: Option<String>,
    pub group: Option<String>,
    pub size: u64,
    pub mtime: u64,
    pub hash: Vec<u8>,
    pub xattrs: Vec<(Vec<u8>, Vec<u8>)>,
    pub path: Vec<u8>,
}

impl FileRec {
    pub fn is_dir(&self) -> bool {
        self.kind == RecKind::Dir
    }
    /// Regular file or hard link: content that lives in Programs.
    pub fn is_regular(&self) -> bool {
        matches!(self.kind, RecKind::File(FileKind::Regular) | RecKind::File(FileKind::Hardlink(_)))
    }
    pub fn path_lossy(&self) -> String {
        String::from_utf8_lossy(&self.path).into_owned()
    }
}

/// All extracted metadata for an unpacked package, ready to stage into SQLite.
#[derive(Debug, Clone)]
pub struct StagedPkg {
    /// Directory name under `Programs/<name>/` (usually `meta.version()`, or
    /// `<version>+<id8>` when upgrading a same-version rebuild).
    pub dir_name: String,
    pub meta: PkgMeta,
    pub recs: Vec<FileRec>,
    pub scripts: Vec<(String, Vec<u8>)>,
}

impl StagedPkg {
    pub fn effective_dir_name(&self) -> &str {
        if self.dir_name.is_empty() {
            self.meta.version()
        } else {
            &self.dir_name
        }
    }
}

/// The records of a package, in stored order (directories before their
/// files). The root directory entry is not listed.
pub fn records(pkg: &Package) -> Vec<FileRec> {
    let mut v = Vec::with_capacity(pkg.dirs.len() + pkg.file_count());
    for d in &pkg.dirs {
        if !d.name.is_empty() {
            v.push(FileRec {
                kind: RecKind::Dir,
                mode: d.acl.mode,
                user: d.acl.user.clone(),
                group: d.acl.group.clone(),
                size: 0,
                mtime: 0,
                hash: Vec::new(),
                xattrs: d.acl.xattrs.clone(),
                path: d.name.clone(),
            });
        }
        for f in &d.files {
            v.push(FileRec {
                kind: RecKind::File(f.kind.clone()),
                mode: f.acl.mode,
                user: f.acl.user.clone(),
                group: f.acl.group.clone(),
                size: f.size,
                mtime: f.mtime,
                hash: f.hash.clone(),
                xattrs: f.acl.xattrs.clone(),
                path: d.file_path(f),
            });
        }
    }
    v
}

// ---------------------------------------------------------------- xattrs blob

fn encode_xattrs(xattrs: &[(Vec<u8>, Vec<u8>)]) -> Option<Vec<u8>> {
    if xattrs.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for (k, v) in xattrs {
        out.extend_from_slice(&(k.len() as u32).to_le_bytes());
        out.extend_from_slice(k);
        out.extend_from_slice(&(v.len() as u32).to_le_bytes());
        out.extend_from_slice(v);
    }
    Some(out)
}

fn decode_xattrs(blob: Option<Vec<u8>>) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let Some(b) = blob else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if i + 4 > b.len() {
            return Err(Error::Format("sqlite xattrs: truncated key length".into()));
        }
        let klen = u32::from_le_bytes(b[i..i + 4].try_into().unwrap()) as usize;
        i += 4;
        if i + klen + 4 > b.len() {
            return Err(Error::Format("sqlite xattrs: truncated key/val".into()));
        }
        let k = b[i..i + klen].to_vec();
        i += klen;
        let vlen = u32::from_le_bytes(b[i..i + 4].try_into().unwrap()) as usize;
        i += 4;
        if i + vlen > b.len() {
            return Err(Error::Format("sqlite xattrs: truncated value".into()));
        }
        let v = b[i..i + vlen].to_vec();
        i += vlen;
        out.push((k, v));
    }
    Ok(out)
}

// ---------------------------------------------------------------- escaping

pub fn escape(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b'\\' => s.push_str("\\\\"),
            b'\t' => s.push_str("\\t"),
            b'\n' => s.push_str("\\n"),
            c if c < 0x20 || c == 0x7f => s.push_str(&format!("\\x{c:02x}")),
            c if c < 0x80 => s.push(c as char),
            _ => {
                // keep valid UTF-8 as is, escape anything else
                let len = match c {
                    0xc2..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf4 => 4,
                    _ => 0,
                };
                if len > 0 && i + len <= b.len() {
                    if let Ok(t) = std::str::from_utf8(&b[i..i + len]) {
                        s.push_str(t);
                        i += len;
                        continue;
                    }
                }
                s.push_str(&format!("\\x{c:02x}"));
            }
        }
        i += 1;
    }
    s
}

pub fn unescape(s: &str) -> Result<Vec<u8>> {
    let b = s.as_bytes();
    let mut v = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            v.push(b[i]);
            i += 1;
            continue;
        }
        match b.get(i + 1) {
            Some(b'\\') => v.push(b'\\'),
            Some(b't') => v.push(b'\t'),
            Some(b'n') => v.push(b'\n'),
            Some(b'x') if i + 4 <= b.len() => {
                let h = std::str::from_utf8(&b[i + 2..i + 4]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                match h {
                    Some(c) => v.push(c),
                    None => return Err(Error::Format(format!("bad escape in '{s}'"))),
                }
                i += 4;
                continue;
            }
            _ => return Err(Error::Format(format!("bad escape in '{s}'"))),
        }
        i += 2;
    }
    Ok(v)
}

fn opt_name(s: &Option<String>) -> String {
    match s {
        Some(n) => escape(n.as_bytes()),
        None => "-".into(),
    }
}

fn parse_opt_name(s: &str) -> Result<Option<String>> {
    if s == "-" {
        return Ok(None);
    }
    String::from_utf8(unescape(s)?).map(Some).map_err(|_| Error::Format("owner name is not UTF-8".into()))
}

// ---------------------------------------------------------------- legacy flat-file formats

pub fn format_files(recs: &[FileRec]) -> Vec<u8> {
    let mut out = Vec::with_capacity(recs.len() * 120);
    for r in recs {
        let (k, extra) = match &r.kind {
            RecKind::Dir => ('d', "-".to_string()),
            RecKind::File(FileKind::Regular) => ('f', "-".to_string()),
            RecKind::File(FileKind::Hardlink(t)) => ('h', escape(t)),
            RecKind::File(FileKind::Symlink(t)) => ('l', escape(t)),
            RecKind::File(FileKind::CharDev(d)) => ('c', d.to_string()),
            RecKind::File(FileKind::BlockDev(d)) => ('b', d.to_string()),
            RecKind::File(FileKind::Fifo(d)) => ('p', d.to_string()),
        };
        let hash = if r.hash.is_empty() { "-".to_string() } else { digest::hex(&r.hash) };
        let xattrs = if r.xattrs.is_empty() {
            "-".to_string()
        } else {
            r.xattrs.iter().map(|(n, v)| format!("{}={}", digest::hex(n), digest::hex(v))).collect::<Vec<_>>().join(",")
        };
        let line = format!(
            "{k}\t{:04o}\t{}\t{}\t{}\t{}\t{hash}\t{extra}\t{xattrs}\t{}\n",
            r.mode,
            opt_name(&r.user),
            opt_name(&r.group),
            r.size,
            r.mtime,
            escape(&r.path)
        );
        out.extend_from_slice(line.as_bytes());
    }
    out
}

pub fn parse_files(text: &str) -> Result<Vec<FileRec>> {
    let mut v = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let bad = || Error::Format(format!("files line {}: malformed", n + 1));
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() != 10 {
            return Err(bad());
        }
        let num = |s: &str| s.parse::<u64>().map_err(|_| bad());
        let extra = f[7];
        let kind = match f[0] {
            "d" => RecKind::Dir,
            "f" => RecKind::File(FileKind::Regular),
            "h" => RecKind::File(FileKind::Hardlink(unescape(extra)?)),
            "l" => RecKind::File(FileKind::Symlink(unescape(extra)?)),
            "c" => RecKind::File(FileKind::CharDev(num(extra)?)),
            "b" => RecKind::File(FileKind::BlockDev(num(extra)?)),
            "p" => RecKind::File(FileKind::Fifo(num(extra)?)),
            _ => return Err(bad()),
        };
        let mut xattrs = Vec::new();
        if f[8] != "-" {
            for item in f[8].split(',') {
                let (a, b) = item.split_once('=').ok_or_else(bad)?;
                xattrs.push((digest::from_hex(a).ok_or_else(bad)?, digest::from_hex(b).ok_or_else(bad)?));
            }
        }
        v.push(FileRec {
            kind,
            mode: u32::from_str_radix(f[1], 8).map_err(|_| bad())?,
            user: parse_opt_name(f[2])?,
            group: parse_opt_name(f[3])?,
            size: num(f[4])?,
            mtime: num(f[5])?,
            hash: if f[6] == "-" { Vec::new() } else { digest::from_hex(f[6]).ok_or_else(bad)? },
            xattrs,
            path: unescape(f[9])?,
        });
    }
    Ok(v)
}

// ---------------------------------------------------------------- info

/// Package metadata stored in `state.db` (and legacy `.vpm/info`).
#[derive(Debug, Clone, Default)]
pub struct PkgMeta {
    /// `hashes` holds the identity from the index (32 bytes) when the
    /// package came from an index.
    pub info: PkgInfo,
    pub triggers: Vec<String>,
    pub replaces_priority: u64,
    /// Where it was installed from (informational).
    pub source: String,
}

impl PkgMeta {
    pub fn name(&self) -> &str {
        &self.info.name
    }
    pub fn version(&self) -> &str {
        &self.info.version
    }
    pub fn nv(&self) -> String {
        self.info.nv()
    }
}

pub fn format_info(m: &PkgMeta) -> String {
    let i = &m.info;
    let mut s = String::new();
    let mut kv = |k: &str, v: &str| {
        s.push_str(k);
        s.push_str(": ");
        s.push_str(&escape(v.as_bytes()));
        s.push('\n');
    };
    kv("name", &i.name);
    kv("version", &i.version);
    kv("arch", &i.arch);
    kv("description", &i.description);
    kv("url", &i.url);
    kv("license", &i.license);
    kv("origin", &i.origin);
    kv("maintainer", &i.maintainer);
    kv("build-time", &i.build_time.to_string());
    kv("installed-size", &i.installed_size.to_string());
    kv("file-size", &i.file_size.to_string());
    kv("identity", &digest::hex(&i.hashes));
    if !i.repo_commit.is_empty() {
        kv("repo-commit", &digest::hex(&i.repo_commit));
    }
    kv("provider-priority", &i.provider_priority.to_string());
    kv("replaces-priority", &m.replaces_priority.to_string());
    kv("layer", &i.layer.to_string());
    kv("source", &m.source);
    for (k, deps) in [
        ("depends", &i.depends),
        ("provides", &i.provides),
        ("replaces", &i.replaces),
        ("install-if", &i.install_if),
        ("recommends", &i.recommends),
    ] {
        for d in deps {
            let t = d.to_string();
            if Dependency::parse(&t).ok().as_ref() == Some(d) {
                kv(k, &t);
            } else {
                let (n, v) = (escape(d.name.as_bytes()), escape(d.version.as_bytes()));
                kv(&format!("{k}-raw"), &format!("{}\t{n}\t{v}", d.op));
            }
        }
    }
    for t in &i.tags {
        kv("tag", t);
    }
    for t in &m.triggers {
        kv("trigger", t);
    }
    s
}

/// A dependency stored as "OP<tab>NAME<tab>VERSION".
fn raw_dep(v: &str) -> Result<Dependency> {
    let bad = || Error::Format(format!("info: bad raw dependency '{v}'"));
    let f: Vec<&str> = v.split('\t').collect();
    if f.len() != 3 {
        return Err(bad());
    }
    let op = f[0].parse::<u8>().map_err(|_| bad())?;
    let text = |s: &str| unescape(s).ok().and_then(|b| String::from_utf8(b).ok()).ok_or_else(bad);
    Ok(Dependency { name: text(f[1])?, version: text(f[2])?, op })
}

pub fn parse_info(text: &str) -> Result<PkgMeta> {
    let mut m = PkgMeta::default();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let (k, v) = line.split_once(": ").ok_or_else(|| Error::Format(format!("info: bad line '{line}'")))?;
        let v = String::from_utf8(unescape(v)?).map_err(|_| Error::Format("info: not UTF-8".into()))?;
        let num = |v: &str| v.parse::<u64>().map_err(|_| Error::Format(format!("info: bad number for {k}")));
        let i = &mut m.info;
        match k {
            "name" => i.name = v,
            "version" => i.version = v,
            "arch" => i.arch = v,
            "description" => i.description = v,
            "url" => i.url = v,
            "license" => i.license = v,
            "origin" => i.origin = v,
            "maintainer" => i.maintainer = v,
            "build-time" => i.build_time = num(&v)?,
            "installed-size" => i.installed_size = num(&v)?,
            "file-size" => i.file_size = num(&v)?,
            "identity" => i.hashes = digest::from_hex(&v).unwrap_or_default(),
            "repo-commit" => i.repo_commit = digest::from_hex(&v).unwrap_or_default(),
            "provider-priority" => i.provider_priority = num(&v)?,
            "replaces-priority" => m.replaces_priority = num(&v)?,
            "layer" => i.layer = num(&v)?,
            "source" => m.source = v,
            "depends" => i.depends.push(Dependency::parse(&v)?),
            "provides" => i.provides.push(Dependency::parse(&v)?),
            "replaces" => i.replaces.push(Dependency::parse(&v)?),
            "install-if" => i.install_if.push(Dependency::parse(&v)?),
            "recommends" => i.recommends.push(Dependency::parse(&v)?),
            "depends-raw" => i.depends.push(raw_dep(&v)?),
            "provides-raw" => i.provides.push(raw_dep(&v)?),
            "replaces-raw" => i.replaces.push(raw_dep(&v)?),
            "install-if-raw" => i.install_if.push(raw_dep(&v)?),
            "recommends-raw" => i.recommends.push(raw_dep(&v)?),
            "tag" => i.tags.push(v),
            "trigger" => m.triggers.push(v),
            _ => {} // unknown keys from newer versions are ignored
        }
    }
    if m.info.name.is_empty() || m.info.version.is_empty() {
        return Err(Error::Format("info: no name or version".into()));
    }
    Ok(m)
}

/// Legacy helper: writes `.vpm/` (info, files, scripts/) into a package dir.
pub fn write_meta(pkg_dir: &Path, meta: &PkgMeta, recs: &[FileRec], pkg: &Package) -> Result<()> {
    let md = pkg_dir.join(META_DIR);
    fs::create_dir(&md)?;
    fs::write(md.join("info"), format_info(meta))?;
    fs::write(md.join("files"), format_files(recs))?;
    if !pkg.scripts.is_empty() {
        let sd = md.join("scripts");
        fs::create_dir(&sd)?;
        for (name, body) in pkg.scripts.iter() {
            let p = sd.join(name);
            fs::write(&p, body)?;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(())
}

/// Legacy helper: a package's script body from its `.vpm/scripts`.
pub fn read_script(pkg_dir: &Path, typ: &str) -> Option<Vec<u8>> {
    fs::read(pkg_dir.join(META_DIR).join("scripts").join(typ)).ok()
}

pub fn read_meta(pkg_dir: &Path) -> Result<PkgMeta> {
    let t = fs::read_to_string(pkg_dir.join(META_DIR).join("info"))
        .map_err(|e| Error::Msg(format!("{}: {e}", pkg_dir.join(META_DIR).join("info").display())))?;
    parse_info(&t)
}

pub fn read_files(pkg_dir: &Path) -> Result<Vec<FileRec>> {
    let p = pkg_dir.join(META_DIR).join("files");
    let t = fs::read_to_string(&p).map_err(|e| Error::Msg(format!("{}: {e}", p.display())))?;
    parse_files(&t)
}

/// Is `s` usable as one path component (package name or version)?
pub fn safe_component(s: &str) -> bool {
    adb::schema::valid_filename(s.as_bytes()) && !s.starts_with('.') && s != CURRENT
}

/// Is `s` usable as a version directory: a safe component and a valid apk
/// version (so never `etc`, `usr` or another name a package path could
/// collide with).
pub fn safe_version(s: &str) -> bool {
    safe_component(s) && crate::version::validate(s)
}

/// Is `s` usable as a `Programs/<name>/<dir_name>` directory: either a valid
/// apk version, or `<version>+<8 lowercase hex>` (for same-version rebuilds).
pub fn safe_dir_name(s: &str) -> bool {
    if safe_version(s) {
        return true;
    }
    let Some((ver, tag)) = s.split_once('+') else { return false };
    safe_version(ver) && tag.len() == 8 && tag.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Picks a `Programs/<name>/<dir_name>` directory name for `(version, identity)`
/// that does not collide with an currently active `conflict_dir`.
pub fn choose_dir_name(version: &str, identity: &[u8], conflict_dir: Option<&str>) -> String {
    if conflict_dir == Some(version) {
        let h = if identity.len() >= 4 {
            digest::hex(&identity[..4])
        } else {
            digest::hex(&digest::sha256(identity)[..4])
        };
        format!("{version}+{h}")
    } else {
        version.to_string()
    }
}

/// vpm's own directories in a root. No package entry may be, or lie
/// below, one of them, by its literal path or by where it resolves to in
/// the live tree: a package could otherwise plant a symlink there and make
/// vpm write or delete through it.
pub const RESERVED: [&str; 3] = [PROGRAMS, STATE_DIR, CACHE_DIR];

/// Is the root-relative path `p` (canonical or literal, no leading `/`)
/// one of [`RESERVED`] or below one?
pub fn is_reserved(p: &[u8]) -> bool {
    let p = p.strip_prefix(b"/").unwrap_or(p);
    RESERVED.iter().any(|r| {
        let r = r.as_bytes();
        p.starts_with(r) && (p.len() == r.len() || p[r.len()] == b'/')
    })
}

/// Checks that `p` is a real directory (not a symlink to one): `Ok(true)`
/// if it is, `Ok(false)` if it does not exist, an error for anything else.
pub fn real_dir(p: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => Ok(true),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::Other,
            format!("{} is not a directory (a symlink?); refusing to use it", p.display()),
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// `Programs/<name>`, checked to be a real directory below a real
/// `Programs` (or absent).
pub fn checked_pkg_parent(root: &Path, name: &str) -> io::Result<PathBuf> {
    let programs = root.join(PROGRAMS);
    real_dir(&programs)?;
    let d = programs.join(name);
    real_dir(&d)?;
    Ok(d)
}

// ---------------------------------------------------------------- SQLite helpers

const MIGRATE_V1_TO_V2: &str = "
CREATE TABLE packages_v2 (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    name              TEXT NOT NULL,
    version           TEXT NOT NULL,
    dir_name          TEXT NOT NULL,
    state             TEXT NOT NULL CHECK (state IN ('staged', 'installed', 'removing')),
    arch              TEXT NOT NULL DEFAULT '',
    description       TEXT NOT NULL DEFAULT '',
    url               TEXT NOT NULL DEFAULT '',
    license           TEXT NOT NULL DEFAULT '',
    origin            TEXT NOT NULL DEFAULT '',
    maintainer        TEXT NOT NULL DEFAULT '',
    build_time        INTEGER NOT NULL DEFAULT 0,
    installed_size    INTEGER NOT NULL DEFAULT 0,
    file_size         INTEGER NOT NULL DEFAULT 0,
    identity          BLOB NOT NULL DEFAULT X'',
    repo_commit       BLOB NOT NULL DEFAULT X'',
    provider_priority INTEGER NOT NULL DEFAULT 0,
    replaces_priority INTEGER NOT NULL DEFAULT 0,
    layer             INTEGER NOT NULL DEFAULT 0,
    source            TEXT NOT NULL DEFAULT '',
    UNIQUE(name, dir_name)
);
INSERT INTO packages_v2 (
    id, name, version, dir_name, state, arch, description, url, license, origin, maintainer,
    build_time, installed_size, file_size, identity, repo_commit, provider_priority,
    replaces_priority, layer, source
)
SELECT
    id, name, version, version, state, arch, description, url, license, origin, maintainer,
    build_time, installed_size, file_size, identity, repo_commit, provider_priority,
    replaces_priority, layer, source
FROM packages;
DROP TABLE packages;
ALTER TABLE packages_v2 RENAME TO packages;
CREATE INDEX IF NOT EXISTS idx_packages_state ON packages(state, name);
PRAGMA user_version = 2;
";

fn init_schema(conn: &Connection) -> Result<()> {
    let ver = conn.pragma_i64("user_version")?;
    if ver > SCHEMA_VERSION {
        return Err(Error::Format(format!(
            "state.db schema version {ver} is newer than this vpm supports ({SCHEMA_VERSION})"
        )));
    }
    if ver == 0 {
        conn.transaction(|c| {
            c.exec_batch(SCHEMA_SQL)?;
            c.exec_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
            Ok(())
        })?;
    } else if ver == 1 {
        // PRAGMA foreign_keys must be toggled outside a transaction in SQLite.
        conn.exec_batch("PRAGMA foreign_keys = OFF;")?;
        let r = conn.transaction(|c| c.exec_batch(MIGRATE_V1_TO_V2));
        let _ = conn.exec_batch("PRAGMA foreign_keys = ON;");
        r?;
    }
    Ok(())
}

fn read_pkg_row(st: &Statement<'_>) -> (i64, String, PkgMeta) {
    let id = st.col_i64(0);
    let mut m = PkgMeta::default();
    let i = &mut m.info;
    i.name = st.col_text(1);
    i.version = st.col_text(2);
    let dir_name = st.col_text(3);
    i.arch = st.col_text(4);
    i.description = st.col_text(5);
    i.url = st.col_text(6);
    i.license = st.col_text(7);
    i.origin = st.col_text(8);
    i.maintainer = st.col_text(9);
    i.build_time = st.col_i64(10) as u64;
    i.installed_size = st.col_i64(11) as u64;
    i.file_size = st.col_i64(12) as u64;
    i.hashes = st.col_blob(13);
    i.repo_commit = st.col_blob(14);
    i.provider_priority = st.col_i64(15) as u64;
    m.replaces_priority = st.col_i64(16) as u64;
    i.layer = st.col_i64(17) as u64;
    m.source = st.col_text(18);
    let dir_name = if dir_name.is_empty() { i.version.clone() } else { dir_name };
    (id, dir_name, m)
}

fn insert_pkg_in_tx(
    conn: &Connection,
    sp: &StagedPkg,
    state: &str,
    st_del: &mut Statement<'_>,
    st_pkg: &mut Statement<'_>,
    st_dep: &mut Statement<'_>,
    st_tag: &mut Statement<'_>,
    st_trig: &mut Statement<'_>,
    st_script: &mut Statement<'_>,
    st_file: &mut Statement<'_>,
) -> Result<i64> {
    let i = &sp.meta.info;
    let dir_name = sp.effective_dir_name();
    // Remove any existing row for (name, dir_name) before inserting fresh data.
    st_del.bind_text(1, &i.name)?;
    st_del.bind_text(2, dir_name)?;
    st_del.execute()?;

    st_pkg.bind_text(1, &i.name)?;
    st_pkg.bind_text(2, &i.version)?;
    st_pkg.bind_text(3, dir_name)?;
    st_pkg.bind_text(4, state)?;
    st_pkg.bind_text(5, &i.arch)?;
    st_pkg.bind_text(6, &i.description)?;
    st_pkg.bind_text(7, &i.url)?;
    st_pkg.bind_text(8, &i.license)?;
    st_pkg.bind_text(9, &i.origin)?;
    st_pkg.bind_text(10, &i.maintainer)?;
    st_pkg.bind_i64(11, i.build_time as i64)?;
    st_pkg.bind_i64(12, i.installed_size as i64)?;
    st_pkg.bind_i64(13, i.file_size as i64)?;
    st_pkg.bind_blob(14, &i.hashes)?;
    st_pkg.bind_blob(15, &i.repo_commit)?;
    st_pkg.bind_i64(16, i.provider_priority as i64)?;
    st_pkg.bind_i64(17, sp.meta.replaces_priority as i64)?;
    st_pkg.bind_i64(18, i.layer as i64)?;
    st_pkg.bind_text(19, &sp.meta.source)?;
    st_pkg.execute()?;

    let pkg_id = conn.last_insert_rowid();

    for (kind, deps) in [
        ("depends", &i.depends),
        ("provides", &i.provides),
        ("replaces", &i.replaces),
        ("install_if", &i.install_if),
        ("recommends", &i.recommends),
    ] {
        for (ord, d) in deps.iter().enumerate() {
            st_dep.bind_i64(1, pkg_id)?;
            st_dep.bind_text(2, kind)?;
            st_dep.bind_i64(3, ord as i64)?;
            st_dep.bind_text(4, &d.name)?;
            st_dep.bind_text(5, &d.version)?;
            st_dep.bind_i64(6, d.op as i64)?;
            st_dep.execute()?;
        }
    }

    for (ord, tag) in i.tags.iter().enumerate() {
        st_tag.bind_i64(1, pkg_id)?;
        st_tag.bind_i64(2, ord as i64)?;
        st_tag.bind_text(3, tag)?;
        st_tag.execute()?;
    }

    for (ord, pat) in sp.meta.triggers.iter().enumerate() {
        st_trig.bind_i64(1, pkg_id)?;
        st_trig.bind_i64(2, ord as i64)?;
        st_trig.bind_text(3, pat)?;
        st_trig.execute()?;
    }

    for (kind, body) in &sp.scripts {
        st_script.bind_i64(1, pkg_id)?;
        st_script.bind_text(2, kind)?;
        st_script.bind_blob(3, body)?;
        st_script.execute()?;
    }

    for (ord, r) in sp.recs.iter().enumerate() {
        let (kind, extra, dev): (&str, Option<&[u8]>, Option<i64>) = match &r.kind {
            RecKind::Dir => ("d", None, None),
            RecKind::File(FileKind::Regular) => ("f", None, None),
            RecKind::File(FileKind::Hardlink(t)) => ("h", Some(t.as_slice()), None),
            RecKind::File(FileKind::Symlink(t)) => ("l", Some(t.as_slice()), None),
            RecKind::File(FileKind::CharDev(d)) => ("c", None, Some(*d as i64)),
            RecKind::File(FileKind::BlockDev(d)) => ("b", None, Some(*d as i64)),
            RecKind::File(FileKind::Fifo(d)) => ("p", None, Some(*d as i64)),
        };
        let xa = encode_xattrs(&r.xattrs);
        st_file.bind_i64(1, pkg_id)?;
        st_file.bind_i64(2, ord as i64)?;
        st_file.bind_text(3, kind)?;
        st_file.bind_i64(4, r.mode as i64)?;
        st_file.bind_opt_text(5, r.user.as_deref())?;
        st_file.bind_opt_text(6, r.group.as_deref())?;
        st_file.bind_i64(7, r.size as i64)?;
        st_file.bind_i64(8, r.mtime as i64)?;
        st_file.bind_blob(9, &r.hash)?;
        st_file.bind_opt_blob(10, extra)?;
        st_file.bind_opt_i64(11, dev)?;
        st_file.bind_opt_blob(12, xa.as_deref())?;
        st_file.bind_blob(13, &r.path)?;
        st_file.execute()?;
    }

    Ok(pkg_id)
}

fn read_file_rec_row(st: &Statement<'_>) -> Result<FileRec> {
    let k = st.col_text(0);
    let extra = st.col_opt_blob(7);
    let dev = st.col_opt_i64(8).unwrap_or(0) as u64;
    let kind = match k.as_str() {
        "d" => RecKind::Dir,
        "f" => RecKind::File(FileKind::Regular),
        "h" => RecKind::File(FileKind::Hardlink(extra.unwrap_or_default())),
        "l" => RecKind::File(FileKind::Symlink(extra.unwrap_or_default())),
        "c" => RecKind::File(FileKind::CharDev(dev)),
        "b" => RecKind::File(FileKind::BlockDev(dev)),
        "p" => RecKind::File(FileKind::Fifo(dev)),
        other => return Err(Error::Format(format!("sqlite pkg_files: unknown kind '{other}'"))),
    };
    Ok(FileRec {
        kind,
        mode: st.col_i64(1) as u32,
        user: st.col_opt_text(2),
        group: st.col_opt_text(3),
        size: st.col_i64(4) as u64,
        mtime: st.col_i64(5) as u64,
        hash: st.col_blob(6),
        xattrs: decode_xattrs(st.col_opt_blob(9))?,
        path: st.col_blob(10),
    })
}

fn read_legacy_scripts(pkg_dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let sd = pkg_dir.join(META_DIR).join("scripts");
    let Ok(rd) = fs::read_dir(&sd) else { return out };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if let Some(name) = e.file_name().to_str() {
            if let Ok(body) = fs::read(e.path()) {
                out.push((name.to_string(), body));
            }
        }
    }
    out
}

/// Imports a pre-SQLite root (`Programs/*/*/.vpm/` + `var/lib/vpm/world`)
/// into `state.db` in a single atomic transaction, then removes the old `.vpm`
/// metadata directories once durable.
fn migrate_legacy_root(root: &Path, conn: &Connection) -> Result<()> {
    let programs = root.join(PROGRAMS);
    let mut to_insert: Vec<(StagedPkg, &'static str, PathBuf)> = Vec::new();

    if programs.exists() {
        let mut names: Vec<_> = fs::read_dir(&programs)?.collect::<io::Result<Vec<_>>>()?;
        names.sort_by_key(|e| e.file_name());
        for e in names {
            let pdir = e.path();
            match Db::load_installed_legacy(&pdir) {
                Ok(Some(ip)) => {
                    let recs = match read_files(&ip.dir) {
                        Ok(r) => r,
                        Err(err) => {
                            eprintln!("vpm: warning: migration: {}: {err}; skipped", ip.dir.display());
                            continue;
                        }
                    };
                    let scripts = read_legacy_scripts(&ip.dir);
                    let meta_dir = ip.dir.join(META_DIR);
                    to_insert.push((StagedPkg { dir_name: ip.dir_name, meta: ip.meta, recs, scripts }, "installed", meta_dir));
                }
                Ok(None) => {}
                Err(why) => eprintln!("vpm: warning: migration: {}: {why}; ignored", pdir.display()),
            }
        }
    }

    // If a legacy root crashed mid-switch and left a journal, also import any
    // staged/old versions referenced in the journal so recovery can finish.
    if let Ok(Some(jf)) = JournalFile::open(root) {
        let mut seen: std::collections::HashSet<(String, String)> = to_insert
            .iter()
            .map(|(s, _, _)| (s.meta.name().to_string(), s.effective_dir_name().to_string()))
            .collect();
        for op in &jf.plan.ops {
            for (dir_opt, st) in [(op.new_dir(), "staged"), (op.old_dir(), "removing")] {
                let Some(dn) = dir_opt else { continue };
                if !seen.insert((op.name().to_string(), dn.to_string())) {
                    continue;
                }
                let dir = Db::pkg_dir(root, op.name(), dn);
                if !dir.join(META_DIR).exists() {
                    continue;
                }
                if let (Ok(meta), Ok(recs)) = (read_meta(&dir), read_files(&dir)) {
                    let scripts = read_legacy_scripts(&dir);
                    to_insert.push((StagedPkg { dir_name: dn.to_string(), meta, recs, scripts }, st, dir.join(META_DIR)));
                }
            }
        }
    }

    let world = read_world(root)?;
    let world_specs: Vec<String> = format_world(&world).lines().filter(|l| !l.is_empty()).map(String::from).collect();

    conn.transaction(|c| {
        let mut st_del = c.prepare("DELETE FROM packages WHERE name = ?1 AND dir_name = ?2")?;
        let mut st_pkg = c.prepare(
            "INSERT INTO packages (name, version, dir_name, state, arch, description, url, license, origin, maintainer, \
             build_time, installed_size, file_size, identity, repo_commit, provider_priority, replaces_priority, \
             layer, source) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
        )?;
        let mut st_dep = c.prepare(
            "INSERT INTO pkg_deps (pkg_id, kind, ord, name, version, op) VALUES (?1,?2,?3,?4,?5,?6)",
        )?;
        let mut st_tag = c.prepare("INSERT INTO pkg_tags (pkg_id, ord, tag) VALUES (?1,?2,?3)")?;
        let mut st_trig = c.prepare("INSERT INTO pkg_triggers (pkg_id, ord, pattern) VALUES (?1,?2,?3)")?;
        let mut st_script = c.prepare("INSERT INTO pkg_scripts (pkg_id, kind, body) VALUES (?1,?2,?3)")?;
        let mut st_file = c.prepare(
            "INSERT INTO pkg_files (pkg_id, ord, kind, mode, user, grp, size, mtime, hash, extra, dev, xattrs, path) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        )?;

        for (sp, st, _) in &to_insert {
            insert_pkg_in_tx(
                c,
                sp,
                st,
                &mut st_del,
                &mut st_pkg,
                &mut st_dep,
                &mut st_tag,
                &mut st_trig,
                &mut st_script,
                &mut st_file,
            )?;
        }

        c.exec_batch("DELETE FROM world;")?;
        let mut st_w = c.prepare("INSERT OR IGNORE INTO world (spec) VALUES (?1)")?;
        for w in &world_specs {
            st_w.bind_text(1, w)?;
            st_w.execute()?;
        }
        Ok(())
    })?;

    // Now that state.db is durably committed, drop legacy .vpm directories.
    for (_, _, meta_dir) in &to_insert {
        let _ = fs::remove_dir_all(meta_dir);
    }
    Ok(())
}

// ---------------------------------------------------------------- database

/// An installed package (metadata only; files are read on demand).
#[derive(Debug, Clone)]
pub struct Installed {
    pub meta: PkgMeta,
    pub dir_name: String,
    pub dir: PathBuf,
}

pub struct Db {
    pub root: PathBuf,
    pub installed: BTreeMap<String, Installed>,
    pub world: Vec<Dependency>,
    conn: Option<Connection>,
    schema_ver: i64,
    _lock: Option<File>,
}

impl Db {
    pub fn programs(root: &Path) -> PathBuf {
        root.join(PROGRAMS)
    }
    pub fn pkg_dir(root: &Path, name: &str, dir_name: &str) -> PathBuf {
        root.join(PROGRAMS).join(name).join(dir_name)
    }
    pub fn state(root: &Path) -> PathBuf {
        root.join(STATE_DIR)
    }
    pub fn db_path(root: &Path) -> PathBuf {
        root.join(STATE_DIR).join(STATE_DB)
    }
    pub fn journal_path(root: &Path) -> PathBuf {
        root.join(STATE_DIR).join("journal")
    }
    pub fn dirty_path(root: &Path) -> PathBuf {
        root.join(STATE_DIR).join("dirty")
    }

    /// Flushes `state.db`, `state.db-wal` (if present), and the `var/lib/vpm`
    /// directory so WAL commits made under `PRAGMA synchronous = NORMAL` are
    /// durable without calling whole-filesystem `syncfs`.
    pub fn sync_state_dir(root: &Path) -> Result<()> {
        let state = Self::state(root);
        for name in [STATE_DB, "state.db-wal"] {
            let p = state.join(name);
            if let Ok(f) = sys::open_regular_nofollow(&p) {
                f.sync_data()?;
            }
        }
        if let Ok(d) = sys::open_dir_nofollow(&state) {
            d.sync_all()?;
        }
        Ok(())
    }


    fn own_dirs(root: &Path) -> [PathBuf; 6] {
        [
            root.join("var"),
            root.join("var/lib"),
            root.join("var/cache"),
            Self::state(root),
            root.join(CACHE_DIR),
            root.join(PROGRAMS),
        ]
    }

    fn dir_col(&self) -> &'static str {
        if self.schema_ver >= 2 { "dir_name" } else { "version" }
    }

    /// Opens (or with `initdb` creates) the SQLite database and takes the lock.
    /// Automatically migrates a legacy `.vpm/` root into `state.db`.
    pub fn open(root: &Path, initdb: bool) -> Result<Db> {
        let state = Self::state(root);
        let db_path = Self::db_path(root);
        let world_path = state.join("world");

        // vpm's own directories must be real directories, not symlinks
        let own = Self::own_dirs(root);
        for d in &own {
            real_dir(d).map_err(Error::Io)?;
        }

        let has_sqlite = db_path.exists();
        let has_legacy = world_path.exists();

        if initdb {
            fs::create_dir_all(root)?;
            for d in &own {
                if !d.exists() {
                    fs::create_dir_all(d)?;
                }
            }
            if !world_path.exists() {
                fs::write(&world_path, "")?;
            }
        } else if !has_sqlite && !has_legacy {
            return Err(Error::Msg(format!(
                "{} has no vpm database (create one with --initdb)",
                root.display()
            )));
        }

        for d in [root.join(PROGRAMS), root.join(CACHE_DIR)] {
            if !d.exists() {
                fs::create_dir_all(&d)?;
            }
        }

        let lock = OpenOptions::new().create(true).truncate(false).write(true).open(state.join("lock"))?;
        if !sys::try_lock(&lock)? {
            return Err(Error::Msg(format!("{}: another vpm is running on this root", root.display())));
        }
        {
            use std::os::unix::io::AsRawFd;
            sys::set_lock_fd(lock.as_raw_fd());
        }

        let conn = Connection::open_rw(&db_path)?;
        init_schema(&conn)?;
        if !has_sqlite && has_legacy {
            migrate_legacy_root(root, &conn)?;
        }

        let mut db = Db {
            root: root.to_path_buf(),
            installed: BTreeMap::new(),
            world: Vec::new(),
            conn: Some(conn),
            schema_ver: SCHEMA_VERSION,
            _lock: Some(lock),
        };
        db.reload()?;
        Ok(db)
    }

    /// Opens the database read-only without taking a lock or creating
    /// directories. Falls back to reading legacy `.vpm/` metadata if `state.db`
    /// has not been created yet. With `allow_missing`, a root without a
    /// database yet opens as empty (for `--initdb --simulate`).
    pub fn open_readonly(root: &Path, allow_missing: bool) -> Result<Db> {
        let state = Self::state(root);
        let db_path = Self::db_path(root);
        let world_path = state.join("world");

        for d in &Self::own_dirs(root) {
            real_dir(d).map_err(Error::Io)?;
        }

        if db_path.exists() {
            let conn = Connection::open_ro(&db_path)?;
            let ver = conn.pragma_i64("user_version")?;
            if ver > SCHEMA_VERSION {
                return Err(Error::Format(format!(
                    "state.db schema version {ver} is newer than this vpm supports ({SCHEMA_VERSION})"
                )));
            }
            let mut db = Db {
                root: root.to_path_buf(),
                installed: BTreeMap::new(),
                world: Vec::new(),
                conn: Some(conn),
                schema_ver: ver,
                _lock: None,
            };
            db.reload()?;
            return Ok(db);
        }

        if world_path.exists() {
            let mut db = Db {
                root: root.to_path_buf(),
                installed: BTreeMap::new(),
                world: Vec::new(),
                conn: None,
                schema_ver: 0,
                _lock: None,
            };
            db.reload()?;
            return Ok(db);
        }

        if allow_missing {
            return Ok(Db {
                root: root.to_path_buf(),
                installed: BTreeMap::new(),
                world: Vec::new(),
                conn: None,
                schema_ver: 0,
                _lock: None,
            });
        }
        Err(Error::Msg(format!(
            "{} has no vpm database (create one with --initdb)",
            root.display()
        )))
    }

    pub fn is_sqlite(&self) -> bool {
        self.conn.is_some()
    }

    /// (Re)reads the installed packages and the world.
    pub fn reload(&mut self) -> Result<()> {
        if self.conn.is_some() {
            self.reload_sqlite()
        } else {
            self.reload_legacy()
        }
    }

    fn reload_sqlite(&mut self) -> Result<()> {
        self.installed.clear();
        self.world.clear();
        let conn = self.conn.as_ref().unwrap();
        let dcol = self.dir_col();

        let mut by_id: HashMap<i64, (String, PkgMeta)> = HashMap::new();
        {
            let mut st = conn.prepare(&format!(
                "SELECT id, name, version, {dcol}, arch, description, url, license, origin, maintainer, \
                 build_time, installed_size, file_size, identity, repo_commit, provider_priority, \
                 replaces_priority, layer, source \
                 FROM packages WHERE state = 'installed' ORDER BY name"
            ))?;
            while st.step_row()? {
                let (id, dir_name, meta) = read_pkg_row(&st);
                if !safe_component(&meta.info.name) || !safe_version(&meta.info.version) || !safe_dir_name(&dir_name) {
                    eprintln!(
                        "vpm: warning: state.db has unsafe package '{}-{}' ({dir_name}); ignored",
                        meta.info.name, meta.info.version
                    );
                    continue;
                }
                by_id.insert(id, (dir_name, meta));
            }
        }

        if !by_id.is_empty() {
            let mut st = conn.prepare(
                "SELECT d.pkg_id, d.kind, d.name, d.version, d.op \
                 FROM pkg_deps d JOIN packages p ON p.id = d.pkg_id \
                 WHERE p.state = 'installed' ORDER BY d.pkg_id, d.ord",
            )?;
            while st.step_row()? {
                let pid = st.col_i64(0);
                let Some((_, m)) = by_id.get_mut(&pid) else { continue };
                let kind = st.col_text(1);
                let dep = Dependency {
                    name: st.col_text(2),
                    version: st.col_text(3),
                    op: st.col_i64(4) as u8,
                };
                match kind.as_str() {
                    "depends" => m.info.depends.push(dep),
                    "provides" => m.info.provides.push(dep),
                    "replaces" => m.info.replaces.push(dep),
                    "install_if" => m.info.install_if.push(dep),
                    "recommends" => m.info.recommends.push(dep),
                    _ => {}
                }
            }

            let mut st = conn.prepare(
                "SELECT t.pkg_id, t.tag FROM pkg_tags t JOIN packages p ON p.id = t.pkg_id \
                 WHERE p.state = 'installed' ORDER BY t.pkg_id, t.ord",
            )?;
            while st.step_row()? {
                if let Some((_, m)) = by_id.get_mut(&st.col_i64(0)) {
                    m.info.tags.push(st.col_text(1));
                }
            }

            let mut st = conn.prepare(
                "SELECT t.pkg_id, t.pattern FROM pkg_triggers t JOIN packages p ON p.id = t.pkg_id \
                 WHERE p.state = 'installed' ORDER BY t.pkg_id, t.ord",
            )?;
            while st.step_row()? {
                if let Some((_, m)) = by_id.get_mut(&st.col_i64(0)) {
                    m.triggers.push(st.col_text(1));
                }
            }
        }

        for (_, (dir_name, meta)) in by_id {
            let dir = Self::pkg_dir(&self.root, meta.name(), &dir_name);
            self.installed.insert(meta.info.name.clone(), Installed { meta, dir_name, dir });
        }

        let mut st = conn.prepare("SELECT spec FROM world ORDER BY spec")?;
        while st.step_row()? {
            let w = st.col_text(0);
            match Dependency::parse_world(&w) {
                Ok(d) => self.world.push(d),
                Err(e) => eprintln!("vpm: warning: world: {e}; ignored"),
            }
        }
        Ok(())
    }

    fn reload_legacy(&mut self) -> Result<()> {
        self.installed.clear();
        let programs = self.root.join(PROGRAMS);
        if programs.exists() {
            let mut names: Vec<_> = fs::read_dir(&programs)?.collect::<io::Result<Vec<_>>>()?;
            names.sort_by_key(|e| e.file_name());
            for e in names {
                match Self::load_installed_legacy(&e.path()) {
                    Ok(Some(ip)) => {
                        self.installed.insert(ip.meta.info.name.clone(), ip);
                    }
                    Ok(None) => {}
                    Err(why) => eprintln!("vpm: warning: {}: {why}; ignored", e.path().display()),
                }
            }
        }
        self.world = read_world(&self.root)?;
        Ok(())
    }

    fn load_installed_legacy(pdir: &Path) -> std::result::Result<Option<Installed>, String> {
        match fs::symlink_metadata(pdir) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => return Err("not a directory".into()),
            Err(e) => return Err(e.to_string()),
        }
        let cur = pdir.join(CURRENT);
        let target = match fs::read_link(&cur) {
            Ok(t) => t,
            Err(_) => return Ok(None),
        };
        let tb = target.as_os_str().as_bytes();
        let ts = std::str::from_utf8(tb).unwrap_or("");
        if !safe_dir_name(ts) {
            return Err(format!("Current points at '{}', not a version", String::from_utf8_lossy(tb)));
        }
        let dir = pdir.join(&target);
        match fs::symlink_metadata(&dir) {
            Ok(m) if m.is_dir() => {}
            _ => return Err(format!("{ts} is not a directory")),
        }
        let meta = read_meta(&dir).map_err(|e| e.to_string())?;
        let fname = pdir.file_name().map(|n| n.as_bytes()).unwrap_or(b"");
        if meta.info.name.as_bytes() != fname || (meta.info.version != ts && !ts.starts_with(&format!("{}+", meta.info.version))) {
            return Err("metadata does not match its place".into());
        }
        Ok(Some(Installed { meta, dir_name: ts.to_string(), dir }))
    }

    // ---- SQLite package queries & mutations ----

    /// Inserts all newly unpacked packages into `state.db` with `state = 'staged'`
    /// in a single ACID transaction.
    pub fn stage_packages(&self, staged: &[StagedPkg]) -> Result<()> {
        if staged.is_empty() {
            return Ok(());
        }
        let conn = self.conn.as_ref().ok_or_else(|| Error::Msg("database is read-only".into()))?;
        conn.transaction(|c| {
            let mut st_del = c.prepare("DELETE FROM packages WHERE name = ?1 AND dir_name = ?2")?;
            let mut st_pkg = c.prepare(
                "INSERT INTO packages (name, version, dir_name, state, arch, description, url, license, origin, maintainer, \
                 build_time, installed_size, file_size, identity, repo_commit, provider_priority, replaces_priority, \
                 layer, source) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
            )?;
            let mut st_dep = c.prepare(
                "INSERT INTO pkg_deps (pkg_id, kind, ord, name, version, op) VALUES (?1,?2,?3,?4,?5,?6)",
            )?;
            let mut st_tag = c.prepare("INSERT INTO pkg_tags (pkg_id, ord, tag) VALUES (?1,?2,?3)")?;
            let mut st_trig = c.prepare("INSERT INTO pkg_triggers (pkg_id, ord, pattern) VALUES (?1,?2,?3)")?;
            let mut st_script = c.prepare("INSERT INTO pkg_scripts (pkg_id, kind, body) VALUES (?1,?2,?3)")?;
            let mut st_file = c.prepare(
                "INSERT INTO pkg_files (pkg_id, ord, kind, mode, user, grp, size, mtime, hash, extra, dev, xattrs, path) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            )?;

            for sp in staged {
                insert_pkg_in_tx(
                    c,
                    sp,
                    "staged",
                    &mut st_del,
                    &mut st_pkg,
                    &mut st_dep,
                    &mut st_tag,
                    &mut st_trig,
                    &mut st_script,
                    &mut st_file,
                )?;
            }
            Ok(())
        })
    }

    /// Returns whether `(name, dir_name)` exists in the database (in any state).
    pub fn has_pkg(&self, name: &str, dir_name: &str) -> Result<bool> {
        if let Some(conn) = &self.conn {
            let dcol = self.dir_col();
            let mut st = conn.prepare(&format!("SELECT 1 FROM packages WHERE name = ?1 AND {dcol} = ?2 LIMIT 1"))?;
            st.bind_text(1, name)?;
            st.bind_text(2, dir_name)?;
            st.step_row()
        } else {
            Ok(Self::pkg_dir(&self.root, name, dir_name).join(META_DIR).exists())
        }
    }

    /// Reads full metadata for `(name, dir_name)` (any state: staged/installed/removing).
    pub fn read_pkg_meta(&self, name: &str, dir_name: &str) -> Result<PkgMeta> {
        let Some(conn) = &self.conn else {
            return read_meta(&Self::pkg_dir(&self.root, name, dir_name));
        };
        let dcol = self.dir_col();
        let mut st = conn.prepare(&format!(
            "SELECT id, name, version, {dcol}, arch, description, url, license, origin, maintainer, \
             build_time, installed_size, file_size, identity, repo_commit, provider_priority, \
             replaces_priority, layer, source \
             FROM packages WHERE name = ?1 AND {dcol} = ?2 LIMIT 1"
        ))?;
        st.bind_text(1, name)?;
        st.bind_text(2, dir_name)?;
        if !st.step_row()? {
            return Err(Error::Msg(format!("state.db: {name}-{dir_name}: metadata not found")));
        }
        let (pkg_id, _, mut meta) = read_pkg_row(&st);
        drop(st);

        let mut st = conn.prepare(
            "SELECT kind, name, version, op FROM pkg_deps WHERE pkg_id = ?1 ORDER BY ord",
        )?;
        st.bind_i64(1, pkg_id)?;
        while st.step_row()? {
            let kind = st.col_text(0);
            let dep = Dependency {
                name: st.col_text(1),
                version: st.col_text(2),
                op: st.col_i64(3) as u8,
            };
            match kind.as_str() {
                "depends" => meta.info.depends.push(dep),
                "provides" => meta.info.provides.push(dep),
                "replaces" => meta.info.replaces.push(dep),
                "install_if" => meta.info.install_if.push(dep),
                "recommends" => meta.info.recommends.push(dep),
                _ => {}
            }
        }

        let mut st = conn.prepare("SELECT tag FROM pkg_tags WHERE pkg_id = ?1 ORDER BY ord")?;
        st.bind_i64(1, pkg_id)?;
        while st.step_row()? {
            meta.info.tags.push(st.col_text(0));
        }

        let mut st = conn.prepare("SELECT pattern FROM pkg_triggers WHERE pkg_id = ?1 ORDER BY ord")?;
        st.bind_i64(1, pkg_id)?;
        while st.step_row()? {
            meta.triggers.push(st.col_text(0));
        }
        Ok(meta)
    }

    /// Reads all file records for `(name, dir_name)` in stored order.
    pub fn read_pkg_files(&self, name: &str, dir_name: &str) -> Result<Vec<FileRec>> {
        let Some(conn) = &self.conn else {
            return read_files(&Self::pkg_dir(&self.root, name, dir_name));
        };
        let dcol = self.dir_col();
        let mut st = conn.prepare(&format!(
            "SELECT f.kind, f.mode, f.user, f.grp, f.size, f.mtime, f.hash, f.extra, f.dev, f.xattrs, f.path \
             FROM pkg_files f JOIN packages p ON p.id = f.pkg_id \
             WHERE p.name = ?1 AND p.{dcol} = ?2 ORDER BY f.ord"
        ))?;
        st.bind_text(1, name)?;
        st.bind_text(2, dir_name)?;
        let mut out = Vec::new();
        while st.step_row()? {
            out.push(read_file_rec_row(&st)?);
        }
        Ok(out)
    }

    /// Reads a script body (`pre-install`, `post-install`, `trigger`, ...) for `(name, dir_name)`.
    pub fn read_pkg_script(&self, name: &str, dir_name: &str, kind: &str) -> Option<Vec<u8>> {
        let Some(conn) = &self.conn else {
            return read_script(&Self::pkg_dir(&self.root, name, dir_name), kind);
        };
        let dcol = self.dir_col();
        let mut st = conn
            .prepare(&format!(
                "SELECT s.body FROM pkg_scripts s JOIN packages p ON p.id = s.pkg_id \
                 WHERE p.name = ?1 AND p.{dcol} = ?2 AND s.kind = ?3 LIMIT 1"
            ))
            .ok()?;
        st.bind_text(1, name).ok()?;
        st.bind_text(2, dir_name).ok()?;
        st.bind_text(3, kind).ok()?;
        if st.step_row().ok()? {
            Some(st.col_blob(0))
        } else {
            None
        }
    }

    /// Fast existence check for a package script without reading its body.
    pub fn has_pkg_script(&self, name: &str, dir_name: &str, kind: &str) -> bool {
        let Some(conn) = &self.conn else {
            return Self::pkg_dir(&self.root, name, dir_name).join(META_DIR).join("scripts").join(kind).exists();
        };
        let dcol = self.dir_col();
        let Ok(mut st) = conn.prepare(&format!(
            "SELECT 1 FROM pkg_scripts s JOIN packages p ON p.id = s.pkg_id \
             WHERE p.name = ?1 AND p.{dcol} = ?2 AND s.kind = ?3 LIMIT 1"
        )) else {
            return false;
        };
        if st.bind_text(1, name).is_err() || st.bind_text(2, dir_name).is_err() || st.bind_text(3, kind).is_err() {
            return false;
        }
        st.step_row().unwrap_or(false)
    }

    /// Indexed O(log N) lookup of installed packages that own non-directory `path`
    /// (root-relative, no leading `/`). Returns package names.
    pub fn find_path_owners(&self, path: &[u8]) -> Result<Option<Vec<String>>> {
        let Some(conn) = &self.conn else {
            return Ok(None); // caller falls back to full scan for legacy roots
        };
        let mut st = conn.prepare(
            "SELECT DISTINCT p.name FROM pkg_files f \
             JOIN packages p ON p.id = f.pkg_id \
             WHERE p.state = 'installed' AND f.path = ?1 AND f.kind != 'd' \
             ORDER BY p.name",
        )?;
        st.bind_blob(1, path)?;
        let mut names = Vec::new();
        while st.step_row()? {
            names.push(st.col_text(0));
        }
        Ok(Some(names))
    }

    /// Indexed O(M log N) lookup returning the names of installed packages
    /// (excluding `exclude`) that own any non-directory path present in
    /// `staged`. Returns `Ok(None)` for legacy non-SQLite roots.
    pub fn find_colliding_installed_pkgs(
        &self,
        staged: &[StagedPkg],
        exclude: &std::collections::HashSet<&str>,
    ) -> Result<Option<std::collections::BTreeSet<String>>> {
        let Some(conn) = &self.conn else {
            return Ok(None);
        };
        if self.installed.is_empty() || self.installed.keys().all(|k| exclude.contains(k.as_str())) {
            return Ok(Some(std::collections::BTreeSet::new()));
        }
        let mut st = conn.prepare(
            "SELECT DISTINCT p.name FROM pkg_files f \
             JOIN packages p ON p.id = f.pkg_id \
             WHERE f.path = ?1 AND p.state = 'installed' AND f.kind != 'd'",
        )?;
        let mut out = std::collections::BTreeSet::new();
        for sp in staged {
            for r in &sp.recs {
                if r.is_dir() {
                    continue;
                }
                st.bind_blob(1, &r.path)?;
                while st.step_row()? {
                    let name = st.col_text(0);
                    if !exclude.contains(name.as_str()) {
                        out.insert(name);
                    }
                }
                st.reset();
            }
        }
        Ok(Some(out))
    }

    /// True if `state.db` contains any `'staged'` or `'removing'` rows (a
    /// pure read-only check so clean `open()` avoids starting a write tx).
    pub fn has_non_installed(&self) -> Result<bool> {
        let Some(conn) = &self.conn else { return Ok(false) };
        let mut st = conn.prepare("SELECT 1 FROM packages WHERE state != 'installed' LIMIT 1")?;
        st.step_row()
    }

    /// Indexed O(M log N) lookup returning the names of installed packages
    /// (excluding `exclude`) that own any non-directory path in `paths`.
    /// Returns `Ok(None)` for legacy non-SQLite roots.
    pub fn find_colliding_paths(
        &self,
        paths: &[&[u8]],
        exclude: &std::collections::HashSet<&str>,
    ) -> Result<Option<std::collections::BTreeSet<String>>> {
        let Some(conn) = &self.conn else {
            return Ok(None);
        };
        if self.installed.is_empty() || self.installed.keys().all(|k| exclude.contains(k.as_str())) {
            return Ok(Some(std::collections::BTreeSet::new()));
        }
        let mut st = conn.prepare(
            "SELECT DISTINCT p.name FROM pkg_files f \
             JOIN packages p ON p.id = f.pkg_id \
             WHERE f.path = ?1 AND p.state = 'installed' AND f.kind != 'd'",
        )?;
        let mut out = std::collections::BTreeSet::new();
        for p in paths {
            st.bind_blob(1, p)?;
            while st.step_row()? {
                let name = st.col_text(0);
                if !exclude.contains(name.as_str()) {
                    out.insert(name);
                }
            }
            st.reset();
        }
        Ok(Some(out))
    }

    /// Returns the subset of `dirs` still owned by at least one installed
    /// package not in `exclude`. Returns `Ok(None)` for legacy roots.
    pub fn find_owned_dirs(
        &self,
        dirs: &[&[u8]],
        exclude: &std::collections::HashSet<&str>,
    ) -> Result<Option<std::collections::HashSet<Vec<u8>>>> {
        let Some(conn) = &self.conn else {
            return Ok(None);
        };
        let mut out = std::collections::HashSet::new();
        if dirs.is_empty() || self.installed.is_empty() || self.installed.keys().all(|k| exclude.contains(k.as_str())) {
            return Ok(Some(out));
        }
        let mut st = conn.prepare(
            "SELECT p.name FROM pkg_files f \
             JOIN packages p ON p.id = f.pkg_id \
             WHERE f.path = ?1 AND p.state = 'installed' AND f.kind = 'd'",
        )?;
        for d in dirs {
            st.bind_blob(1, d)?;
            while st.step_row()? {
                let name = st.col_text(0);
                if !exclude.contains(name.as_str()) {
                    out.insert(d.to_vec());
                    break;
                }
            }
            st.reset();
        }
        Ok(Some(out))
    }

    /// Returns the subset of `dirs` that exist in the current `'installed'` set
    /// (either as an explicit directory record or as an ancestor of any file
    /// record, using O(log N) B-tree index probes on `pkg_files.path`).
    /// Intended for use after `commit_switch` when matching triggers.
    pub fn dir_present_in_installed(
        &self,
        dirs: &[&[u8]],
    ) -> Result<Option<std::collections::HashSet<Vec<u8>>>> {
        let Some(conn) = &self.conn else {
            return Ok(None);
        };
        let mut out = std::collections::HashSet::new();
        if dirs.is_empty() {
            return Ok(Some(out));
        }
        let mut st_exact = conn.prepare(
            "SELECT 1 FROM pkg_files f \
             JOIN packages p ON p.id = f.pkg_id \
             WHERE f.path = ?1 AND f.kind = 'd' AND p.state = 'installed' LIMIT 1",
        )?;
        let mut st_prefix = conn.prepare(
            "SELECT 1 FROM pkg_files f \
             JOIN packages p ON p.id = f.pkg_id \
             WHERE f.path >= ?1 AND f.path < ?2 AND p.state = 'installed' LIMIT 1",
        )?;
        let mut lo = Vec::new();
        let mut hi = Vec::new();
        for &d in dirs {
            if d.is_empty() {
                out.insert(Vec::new());
                continue;
            }
            st_exact.bind_blob(1, d)?;
            let found = st_exact.step_row()?;
            st_exact.reset();
            if found {
                out.insert(d.to_vec());
                continue;
            }
            // In BLOB byte order, '/' is 0x2F and '0' is 0x30, so
            // [d || '/', d || '0') matches every path with prefix `d/`.
            lo.clear();
            lo.extend_from_slice(d);
            lo.push(b'/');
            hi.clear();
            hi.extend_from_slice(d);
            hi.push(b'0');
            st_prefix.bind_blob(1, &lo)?;
            st_prefix.bind_blob(2, &hi)?;
            let has_child = st_prefix.step_row()?;
            st_prefix.reset();
            if has_child {
                out.insert(d.to_vec());
            }
        }
        Ok(Some(out))
    }

    /// Returns all directory paths (explicit dirs, file parent dirs, and their
    /// ancestors) in the current `'installed'` set. Called only when a changed
    /// package declares triggers.
    pub fn list_all_installed_dirs(&self) -> Result<Option<std::collections::BTreeSet<Vec<u8>>>> {
        let Some(conn) = &self.conn else {
            return Ok(None);
        };
        let mut out = std::collections::BTreeSet::new();
        out.insert(Vec::new());
        let mut st = conn.prepare(
            "SELECT f.path, f.kind FROM pkg_files f \
             JOIN packages p ON p.id = f.pkg_id \
             WHERE p.state = 'installed'",
        )?;
        while st.step_row()? {
            let path = st.col_blob(0);
            let kind = st.col_text(1);
            let mut d = if kind == "d" {
                path
            } else {
                crate::rootfs::split_rel(&path).0.to_vec()
            };
            while !d.is_empty() && out.insert(d.clone()) {
                d = crate::rootfs::split_rel(&d).0.to_vec();
            }
        }
        Ok(Some(out))
    }


    /// Checks whether `name` is installed or provided by an installed package.
    pub fn is_installed_or_provided(&self, name: &str) -> bool {
        if self.installed.contains_key(name) {
            return true;
        }
        if let Some(conn) = &self.conn {
            if let Ok(mut st) = conn.prepare(
                "SELECT 1 FROM pkg_deps d JOIN packages p ON p.id = d.pkg_id \
                 WHERE p.state = 'installed' AND d.kind = 'provides' AND d.name = ?1 LIMIT 1",
            ) {
                if st.bind_text(1, name).is_ok() {
                    if let Ok(found) = st.step_row() {
                        return found;
                    }
                }
            }
        }
        self.installed.values().any(|ip| ip.meta.info.provides.iter().any(|p| p.name == name))
    }

    /// Commits all state transitions of a switch (`Install`, `Upgrade`, `Remove`),
    /// updates `world`, and purges non-installed rows in a single atomic SQLite
    /// transaction, then updates the plain-text `world` mirror.
    ///
    /// Idempotent so crash recovery can re-run it safely. Replaces per-package
    /// SQLite commits during the switch (cutting ~40-250 commits down to 1).
    pub fn commit_switch(&self, ops: &[Op], world: &[Dependency]) -> Result<()> {
        let formatted = format_world(world);
        let specs: Vec<&str> = formatted.lines().filter(|l| !l.is_empty()).collect();
        if let Some(conn) = &self.conn {
            conn.transaction(|c| {
                let mut st_rem = c.prepare("UPDATE packages SET state = 'removing' WHERE name = ?1 AND dir_name = ?2")?;
                let mut st_ins = c.prepare("UPDATE packages SET state = 'installed' WHERE name = ?1 AND dir_name = ?2")?;
                for op in ops {
                    match op {
                        Op::Install { name, new_dir, .. } => {
                            st_ins.bind_text(1, name)?;
                            st_ins.bind_text(2, new_dir)?;
                            st_ins.execute()?;
                        }
                        Op::Upgrade { name, old_dir, new_dir, .. } => {
                            st_rem.bind_text(1, name)?;
                            st_rem.bind_text(2, old_dir)?;
                            st_rem.execute()?;
                            st_ins.bind_text(1, name)?;
                            st_ins.bind_text(2, new_dir)?;
                            st_ins.execute()?;
                        }
                        Op::Remove { name, old_dir, .. } => {
                            st_rem.bind_text(1, name)?;
                            st_rem.bind_text(2, old_dir)?;
                            st_rem.execute()?;
                        }
                    }
                }
                c.exec_batch("DELETE FROM packages WHERE state != 'installed';")?;
                c.exec_batch("DELETE FROM world;")?;
                let mut st_w = c.prepare("INSERT OR IGNORE INTO world (spec) VALUES (?1)")?;
                for s in &specs {
                    st_w.bind_text(1, s)?;
                    st_w.execute()?;
                }
                Ok(())
            })?;
        }
        write_atomic(&Self::state(&self.root).join("world"), formatted.as_bytes())?;
        Ok(())
    }

    /// Marks `(name, new_dir)` as `'installed'` and (if upgrading) `(name, old_dir)`
    /// as `'removing'` atomically in `state.db`.
    pub fn mark_installed(&self, name: &str, new_dir: &str, old_dir: Option<&str>) -> Result<()> {
        let conn = self.conn.as_ref().ok_or_else(|| Error::Msg("database is read-only".into()))?;
        conn.transaction(|c| {
            if let Some(old) = old_dir {
                let mut st = c.prepare("UPDATE packages SET state = 'removing' WHERE name = ?1 AND dir_name = ?2")?;
                st.bind_text(1, name)?;
                st.bind_text(2, old)?;
                st.execute()?;
            }
            let mut st = c.prepare("UPDATE packages SET state = 'installed' WHERE name = ?1 AND dir_name = ?2")?;
            st.bind_text(1, name)?;
            st.bind_text(2, new_dir)?;
            st.execute()?;
            Ok(())
        })
    }

    /// Marks `(name, old_dir)` as `'removing'` in `state.db`.
    pub fn mark_removed(&self, name: &str, old_dir: &str) -> Result<()> {
        let conn = self.conn.as_ref().ok_or_else(|| Error::Msg("database is read-only".into()))?;
        let mut st = conn.prepare("UPDATE packages SET state = 'removing' WHERE name = ?1 AND dir_name = ?2")?;
        st.bind_text(1, name)?;
        st.bind_text(2, old_dir)?;
        st.execute()
    }

    /// Writes `world` atomically to `state.db` AND mirrors it to
    /// `ROOT/var/lib/vpm/world` for shell scripts (installers, `valence-wish`,
    /// `valence-first-boot`, `qemu-test.py`).
    pub fn save_world(&self, world: &[Dependency]) -> Result<()> {
        let formatted = format_world(world);
        let specs: Vec<&str> = formatted.lines().filter(|l| !l.is_empty()).collect();
        if let Some(conn) = &self.conn {
            conn.transaction(|c| {
                c.exec_batch("DELETE FROM world;")?;
                let mut st = c.prepare("INSERT OR IGNORE INTO world (spec) VALUES (?1)")?;
                for s in &specs {
                    st.bind_text(1, s)?;
                    st.execute()?;
                }
                Ok(())
            })?;
        }
        write_atomic(&Self::state(&self.root).join("world"), formatted.as_bytes())?;
        Ok(())
    }

    /// Deletes all `'staged'` and `'removing'` package rows (CASCADE removes
    /// their deps, tags, triggers, scripts and file records).
    pub fn purge_non_installed(&self) -> Result<()> {
        let Some(conn) = &self.conn else { return Ok(()) };
        conn.transaction(|c| {
            c.exec_batch("DELETE FROM packages WHERE state != 'installed';")
        })
    }

    /// Deletes staged rows for a failed transaction before the switch.
    pub fn delete_staged(&self, ops: &[Op]) -> Result<()> {
        let Some(conn) = &self.conn else { return Ok(()) };
        conn.transaction(|c| {
            let mut st = c.prepare("DELETE FROM packages WHERE name = ?1 AND dir_name = ?2 AND state = 'staged'")?;
            for op in ops {
                if let Some(dn) = op.new_dir() {
                    st.bind_text(1, op.name())?;
                    st.bind_text(2, dn)?;
                    st.execute()?;
                }
            }
            Ok(())
        })
    }
}

// ---------------------------------------------------------------- world

pub fn read_world(root: &Path) -> Result<Vec<Dependency>> {
    let p = Db::state(root).join("world");
    let t = match fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut v = Vec::new();
    for w in t.split_whitespace() {
        // as apk: an entry with an invalid version is not used (§G2)
        match Dependency::parse_world(w) {
            Ok(d) => v.push(d),
            Err(e) => eprintln!("vpm: warning: world: {e}; ignored"),
        }
    }
    Ok(v)
}

pub fn format_world(world: &[Dependency]) -> String {
    let mut w: Vec<String> = world.iter().map(|d| d.to_string()).collect();
    w.sort();
    w.dedup();
    let mut s = w.join("\n");
    s.push('\n');
    s
}

/// Writes a file atomically: temporary name, fsync, rename.
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("vpm-tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

/// Points `Programs/<name>/Current` at `version` atomically.
pub fn set_current(root: &Path, name: &str, version: &str) -> io::Result<()> {
    let d = root.join(PROGRAMS).join(name);
    let tmp = d.join(".Current.vpm-tmp");
    let _ = fs::remove_file(&tmp);
    symlink(version, &tmp)?;
    fs::rename(&tmp, d.join(CURRENT))
}

// ---------------------------------------------------------------- journal

/// One package change of a transaction. `new_dir`/`old_dir` name the
/// directories under `Programs/<name>/` (usually equal to `new`/`old`, or
/// `<version>+<id8>` on a same-version rebuild).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Install { name: String, new: String, new_dir: String },
    Upgrade { name: String, old: String, old_dir: String, new: String, new_dir: String },
    Remove { name: String, old: String, old_dir: String },
}

impl Op {
    pub fn name(&self) -> &str {
        match self {
            Op::Install { name, .. } | Op::Upgrade { name, .. } | Op::Remove { name, .. } => name,
        }
    }
    pub fn new_version(&self) -> Option<&str> {
        match self {
            Op::Install { new, .. } | Op::Upgrade { new, .. } => Some(new),
            Op::Remove { .. } => None,
        }
    }
    pub fn old_version(&self) -> Option<&str> {
        match self {
            Op::Upgrade { old, .. } | Op::Remove { old, .. } => Some(old),
            Op::Install { .. } => None,
        }
    }
    pub fn new_dir(&self) -> Option<&str> {
        match self {
            Op::Install { new_dir, .. } | Op::Upgrade { new_dir, .. } => Some(new_dir),
            Op::Remove { .. } => None,
        }
    }
    pub fn old_dir(&self) -> Option<&str> {
        match self {
            Op::Upgrade { old_dir, .. } | Op::Remove { old_dir, .. } => Some(old_dir),
            Op::Install { .. } => None,
        }
    }
}

fn fmt_ver_dir(ver: &str, dir: &str) -> String {
    if dir == ver || dir.is_empty() {
        ver.to_string()
    } else {
        format!("{ver}@{dir}")
    }
}

fn parse_ver_dir(tok: &str) -> (String, String) {
    match tok.split_once('@') {
        Some((v, d)) => (v.to_string(), d.to_string()),
        None => (tok.to_string(), tok.to_string()),
    }
}

/// The plan of a switch, as written to the journal before the live tree
/// is touched, plus the steps marked done since.
#[derive(Debug, Clone, Default)]
pub struct Journal {
    /// Package changes in the order they are applied (dependencies first).
    pub ops: Vec<Op>,
    /// The world to write.
    pub world: Vec<String>,
    pub done: std::collections::HashSet<String>,
}

impl Journal {
    pub fn format(&self) -> String {
        let mut s = String::from("vpm-journal 1\n");
        for op in &self.ops {
            match op {
                Op::Install { name, new, new_dir } => {
                    s.push_str(&format!("install {name} {}\n", fmt_ver_dir(new, new_dir)))
                }
                Op::Upgrade { name, old, old_dir, new, new_dir } => {
                    s.push_str(&format!("upgrade {name} {} {}\n", fmt_ver_dir(old, old_dir), fmt_ver_dir(new, new_dir)))
                }
                Op::Remove { name, old, old_dir } => {
                    s.push_str(&format!("remove {name} {}\n", fmt_ver_dir(old, old_dir)))
                }
            }
        }
        for w in &self.world {
            s.push_str(&format!("world {w}\n"));
        }
        s.push_str("end\n");
        s
    }

    pub fn parse(t: &str) -> Result<Journal> {
        let mut j = Journal::default();
        let mut lines = t.lines();
        if lines.next() != Some("vpm-journal 1") {
            return Err(Error::Format("journal: bad header".into()));
        }
        let mut ended = false;
        for l in lines {
            let f: Vec<&str> = l.split(' ').collect();
            let bad = || Error::Format(format!("journal: bad line '{l}'"));
            match (f[0], f.len()) {
                ("install", 3) if !ended => {
                    let (new, new_dir) = parse_ver_dir(f[2]);
                    j.ops.push(Op::Install { name: f[1].into(), new, new_dir });
                }
                ("upgrade", 4) if !ended => {
                    let (old, old_dir) = parse_ver_dir(f[2]);
                    let (new, new_dir) = parse_ver_dir(f[3]);
                    j.ops.push(Op::Upgrade { name: f[1].into(), old, old_dir, new, new_dir });
                }
                ("remove", 3) if !ended => {
                    let (old, old_dir) = parse_ver_dir(f[2]);
                    j.ops.push(Op::Remove { name: f[1].into(), old, old_dir });
                }
                ("world", 2) if !ended => j.world.push(f[1].into()),
                ("end", 1) => ended = true,
                ("done", 2) if ended => {
                    j.done.insert(f[1].into());
                }
                ("", 1) => {}
                _ if ended => {} // a torn "done" line at the end
                _ => return Err(bad()),
            }
        }
        if !ended {
            return Err(Error::Format("journal: incomplete".into()));
        }
        for op in &j.ops {
            let ok = safe_component(op.name())
                && op.new_version().map_or(true, safe_version)
                && op.old_version().map_or(true, safe_version)
                && op.new_dir().map_or(true, safe_dir_name)
                && op.old_dir().map_or(true, safe_dir_name);
            if !ok {
                return Err(Error::Format("journal: unsafe name".into()));
            }
        }
        Ok(j)
    }
}

/// The journal file while a switch runs: steps are appended as they finish.
pub struct JournalFile {
    f: File,
    pub plan: Journal,
}

impl JournalFile {
    /// Writes the plan atomically (temporary file, fsync, rename, fsync of
    /// the directory).
    pub fn create(root: &Path, plan: Journal) -> Result<JournalFile> {
        let p = Db::journal_path(root);
        write_atomic(&p, plan.format().as_bytes())?;
        File::open(Db::state(root))?.sync_all()?;
        let f = OpenOptions::new().append(true).open(&p)?;
        Ok(JournalFile { f, plan })
    }

    pub fn open(root: &Path) -> Result<Option<JournalFile>> {
        let p = Db::journal_path(root);
        let t = match fs::read_to_string(&p) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let plan = Journal::parse(&t)?;
        let f = OpenOptions::new().append(true).open(&p)?;
        Ok(Some(JournalFile { f, plan }))
    }

    pub fn is_done(&self, key: &str) -> bool {
        self.plan.done.contains(key)
    }

    /// Records a finished step (fsync'ed: a step recorded here is not
    /// repeated by a recovery).
    pub fn mark(&mut self, key: &str) -> Result<()> {
        self.mark_with(key, true)
    }

    /// Records a finished step; without `sync` it becomes durable with the
    /// next synced mark (a lost unsynced mark only repeats an idempotent
    /// step).
    pub fn mark_with(&mut self, key: &str, sync: bool) -> Result<()> {
        self.f.write_all(format!("done {key}\n").as_bytes())?;
        if sync {
            self.f.sync_data()?;
        }
        self.plan.done.insert(key.to_string());
        Ok(())
    }

    pub fn remove(self, root: &Path) -> Result<()> {
        drop(self.f);
        fs::remove_file(Db::journal_path(root))?;
        File::open(Db::state(root))?.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_roundtrip() {
        for s in [&b"usr/bin/a b"[..], b"x\ty\nz\\w", b"\x01\x7f\xff\xfe", "naïve".as_bytes(), b""] {
            assert_eq!(unescape(&escape(s)).unwrap(), s);
        }
        assert!(!escape(b"a\tb\nc").contains('\t'));
    }

    #[test]
    fn files_roundtrip() {
        let recs = vec![
            FileRec {
                kind: RecKind::Dir,
                mode: 0o1777,
                user: Some("root".into()),
                group: None,
                size: 0,
                mtime: 0,
                hash: vec![],
                xattrs: vec![],
                path: b"tmp".to_vec(),
            },
            FileRec {
                kind: RecKind::File(FileKind::Symlink(b"../x\ty".to_vec())),
                mode: 0o777,
                user: Some("root".into()),
                group: Some("lp".into()),
                size: 6,
                mtime: 5,
                hash: vec![],
                xattrs: vec![(b"user.a".to_vec(), b"\x00v".to_vec())],
                path: b"usr/bin/a b".to_vec(),
            },
            FileRec {
                kind: RecKind::File(FileKind::CharDev(0x103)),
                mode: 0o666,
                user: None,
                group: None,
                size: 0,
                mtime: 7,
                hash: vec![1, 2, 3],
                xattrs: vec![],
                path: b"dev/null".to_vec(),
            },
        ];
        let t = format_files(&recs);
        assert_eq!(parse_files(std::str::from_utf8(&t).unwrap()).unwrap(), recs);
    }

    #[test]
    fn info_roundtrip() {
        let mut m = PkgMeta::default();
        m.info.name = "foo".into();
        m.info.version = "1.2-r0".into();
        m.info.description = "a: b\nc".into();
        m.info.hashes = vec![0xab; 32];
        m.info.depends = vec![Dependency::parse("so:libc.so.6").unwrap(), Dependency::parse("bar>=2").unwrap()];
        m.info.provides = vec![Dependency::parse("cmd:foo=1.2-r0").unwrap()];
        m.info.install_if = vec![Dependency::parse("x").unwrap(), Dependency::parse("!y").unwrap()];
        let d = |n: &str, v: &str, op: u8| Dependency { name: n.into(), version: v.into(), op };
        m.info.recommends = vec![d("foo", "1", 7), d("bar", "2", 12), d("baz", "", 1), d("q\tx", "1\t2", 21)];
        m.triggers = vec!["/usr/share/*".into()];
        m.replaces_priority = 3;
        let p = parse_info(&format_info(&m)).unwrap();
        assert_eq!(p.info, m.info);
        assert_eq!(p.triggers, m.triggers);
        assert_eq!(p.replaces_priority, 3);
    }

    #[test]
    fn dir_name_helpers() {
        assert!(safe_dir_name("1.0-r0"));
        assert!(safe_dir_name("1.0-r0+0123abcd"));
        assert!(!safe_dir_name("1.0-r0+0123ABCD")); // uppercase rejected
        assert!(!safe_dir_name("1.0-r0+0123")); // wrong length
        assert!(!safe_dir_name("../1.0-r0+0123abcd"));

        let id_a = vec![0xaa; 32];
        let id_b = vec![0xbb; 32];
        assert_eq!(choose_dir_name("1.0-r0", &id_a, None), "1.0-r0");
        assert_eq!(choose_dir_name("1.0-r0", &id_a, Some("0.9-r0")), "1.0-r0");
        assert_eq!(choose_dir_name("1.0-r0", &id_b, Some("1.0-r0")), "1.0-r0+bbbbbbbb");
        // Upgrading back from a +id8 dir to a fresh rebuild of the same version uses bare version
        assert_eq!(choose_dir_name("1.0-r0", &id_a, Some("1.0-r0+bbbbbbbb")), "1.0-r0");
    }

    #[test]
    fn journal_roundtrip() {
        let j = Journal {
            ops: vec![
                Op::Install { name: "a".into(), new: "1".into(), new_dir: "1".into() },
                Op::Upgrade {
                    name: "b".into(),
                    old: "1".into(),
                    old_dir: "1".into(),
                    new: "2".into(),
                    new_dir: "2".into(),
                },
                Op::Upgrade {
                    name: "d".into(),
                    old: "1.0-r0".into(),
                    old_dir: "1.0-r0".into(),
                    new: "1.0-r0".into(),
                    new_dir: "1.0-r0+deadbeef".into(),
                },
                Op::Remove { name: "c".into(), old: "3".into(), old_dir: "3".into() },
            ],
            world: vec!["a".into(), "b>=2".into()],
            done: Default::default(),
        };
        let mut t = j.format();
        // Plain versions stay clean without '@', while distinct dir_name uses ver@dir
        assert!(t.contains("install a 1\n"));
        assert!(t.contains("upgrade b 1 2\n"));
        assert!(t.contains("upgrade d 1.0-r0 1.0-r0@1.0-r0+deadbeef\n"));
        t.push_str("done pre:a\ndon");
        let p = Journal::parse(&t).unwrap();
        assert_eq!(p.ops, j.ops);
        assert_eq!(p.world, j.world);
        assert!(p.done.contains("pre:a"));
        assert!(Journal::parse("vpm-journal 1\ninstall a 1\n").is_err());
        assert!(Journal::parse("vpm-journal 1\ninstall ../a 1\nend\n").is_err());
        assert!(Journal::parse("vpm-journal 1\ninstall a 1@../bad\nend\n").is_err());
    }

    #[test]
    fn sqlite_stage_install_query_and_migrate() {
        let root = std::env::temp_dir().join(format!("vpm-db-sqlite-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);

        let mut db = Db::open(&root, true).unwrap();
        assert!(db.is_sqlite());
        assert!(Db::db_path(&root).exists());

        let mut meta = PkgMeta::default();
        meta.info.name = "demo".into();
        meta.info.version = "1.0-r0".into();
        meta.info.arch = "x86_64".into();
        meta.info.description = "demo package".into();
        meta.info.hashes = vec![0x42; 32];
        meta.info.depends = vec![Dependency::parse("so:libc.so.6").unwrap()];
        meta.info.provides = vec![Dependency::parse("cmd:demo=1.0-r0").unwrap()];
        meta.info.tags = vec!["base".into()];
        meta.triggers = vec!["/usr/share/demo".into()];
        meta.replaces_priority = 5;
        meta.source = "https://example.org/demo-1.0-r0.apk".into();

        let recs = vec![
            FileRec {
                kind: RecKind::Dir,
                mode: 0o755,
                user: Some("root".into()),
                group: Some("root".into()),
                size: 0,
                mtime: 0,
                hash: vec![],
                xattrs: vec![],
                path: b"usr/bin".to_vec(),
            },
            FileRec {
                kind: RecKind::File(FileKind::Regular),
                mode: 0o755,
                user: Some("root".into()),
                group: None,
                size: 12,
                mtime: 100,
                hash: vec![1, 2, 3, 4],
                xattrs: vec![(b"security.capability".to_vec(), b"\x01\x00".to_vec())],
                path: b"usr/bin/demo".to_vec(),
            },
            FileRec {
                kind: RecKind::File(FileKind::Symlink(b"demo".to_vec())),
                mode: 0o777,
                user: None,
                group: None,
                size: 4,
                mtime: 100,
                hash: vec![],
                xattrs: vec![],
                path: b"usr/bin/demo2".to_vec(),
            },
        ];

        let sp = StagedPkg {
            dir_name: "1.0-r0".into(),
            meta: meta.clone(),
            recs: recs.clone(),
            scripts: vec![("post-install".into(), b"#!/bin/sh\nexit 0\n".to_vec())],
        };
        db.stage_packages(&[sp]).unwrap();
        assert!(db.has_pkg("demo", "1.0-r0").unwrap());
        db.mark_installed("demo", "1.0-r0", None).unwrap();
        db.save_world(&[Dependency::parse_world("demo").unwrap()]).unwrap();
        db.reload().unwrap();

        assert_eq!(db.installed.len(), 1);
        let ip = &db.installed["demo"];
        assert_eq!(ip.dir_name, "1.0-r0");
        assert_eq!(ip.meta.info, meta.info);
        assert_eq!(ip.meta.triggers, meta.triggers);
        assert_eq!(ip.meta.replaces_priority, 5);
        assert_eq!(ip.meta.source, meta.source);
        assert_eq!(db.read_pkg_files("demo", "1.0-r0").unwrap(), recs);
        assert_eq!(db.read_pkg_script("demo", "1.0-r0", "post-install").unwrap(), b"#!/bin/sh\nexit 0\n");
        assert!(db.has_pkg_script("demo", "1.0-r0", "post-install"));
        assert!(!db.has_pkg_script("demo", "1.0-r0", "pre-install"));
        assert!(db.is_installed_or_provided("cmd:demo"));
        assert_eq!(db.find_path_owners(b"usr/bin/demo").unwrap(), Some(vec!["demo".into()]));
        assert_eq!(db.find_path_owners(b"usr/bin").unwrap(), Some(vec![]));
        assert_eq!(fs::read_to_string(Db::state(&root).join("world")).unwrap(), "demo\n");

        // Stage a same-version rebuild alongside the installed copy with dir_name 1.0-r0+99999999
        let mut rebuilt_meta = meta.clone();
        rebuilt_meta.info.hashes = vec![0x99; 32];
        let rebuilt_sp = StagedPkg {
            dir_name: "1.0-r0+99999999".into(),
            meta: rebuilt_meta,
            recs: recs.clone(),
            scripts: vec![],
        };
        db.stage_packages(&[rebuilt_sp]).unwrap();
        assert!(db.has_pkg("demo", "1.0-r0").unwrap());
        assert!(db.has_pkg("demo", "1.0-r0+99999999").unwrap());
        db.mark_installed("demo", "1.0-r0+99999999", Some("1.0-r0")).unwrap();
        db.purge_non_installed().unwrap();
        db.reload().unwrap();
        assert!(!db.has_pkg("demo", "1.0-r0").unwrap());
        assert!(db.has_pkg("demo", "1.0-r0+99999999").unwrap());
        assert_eq!(db.installed["demo"].dir_name, "1.0-r0+99999999");
        assert_eq!(db.installed["demo"].meta.info.hashes, vec![0x99; 32]);

        // Indexed conflict lookup finds colliding unchanged package and ignores excluded/non-colliding ones
        let mut other_meta = PkgMeta::default();
        other_meta.info.name = "other".into();
        other_meta.info.version = "1.0-r0".into();
        let colliding_sp = StagedPkg {
            dir_name: "1.0-r0".into(),
            meta: other_meta.clone(),
            recs: vec![FileRec {
                kind: RecKind::File(FileKind::Regular),
                mode: 0o755,
                user: None,
                group: None,
                size: 1,
                mtime: 0,
                hash: vec![9],
                xattrs: vec![],
                path: b"usr/bin/demo".to_vec(),
            }],
            scripts: vec![],
        };
        let clean_sp = StagedPkg {
            dir_name: "1.0-r0".into(),
            meta: other_meta,
            recs: vec![FileRec {
                kind: RecKind::File(FileKind::Regular),
                mode: 0o755,
                user: None,
                group: None,
                size: 1,
                mtime: 0,
                hash: vec![9],
                xattrs: vec![],
                path: b"usr/bin/unique".to_vec(),
            }],
            scripts: vec![],
        };
        let empty_excl = std::collections::HashSet::new();
        let demo_excl: std::collections::HashSet<&str> = ["demo"].into_iter().collect();
        assert_eq!(
            db.find_colliding_installed_pkgs(&[colliding_sp.clone()], &empty_excl).unwrap(),
            Some(["demo".to_string()].into_iter().collect())
        );
        assert_eq!(
            db.find_colliding_installed_pkgs(&[colliding_sp], &demo_excl).unwrap(),
            Some(std::collections::BTreeSet::new())
        );
        assert_eq!(
            db.find_colliding_installed_pkgs(&[clean_sp], &empty_excl).unwrap(),
            Some(std::collections::BTreeSet::new())
        );

        drop(db);

        // Read-only open sees identical state
        let ro = Db::open_readonly(&root, false).unwrap();
        assert!(ro.is_sqlite());
        assert_eq!(ro.installed.len(), 1);
        assert_eq!(ro.installed["demo"].dir_name, "1.0-r0+99999999");
        assert_eq!(ro.world.len(), 1);
        assert_eq!(ro.find_path_owners(b"usr/bin/demo").unwrap(), Some(vec!["demo".into()]));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sqlite_v1_readonly_and_online_migration_to_v2() {
        let root = std::env::temp_dir().join(format!("vpm-db-v1v2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(Db::programs(&root)).unwrap();
        fs::create_dir_all(Db::state(&root)).unwrap();

        // Hand-craft a schema v1 state.db (without dir_name column)
        {
            let conn = Connection::open_rw(&Db::db_path(&root)).unwrap();
            conn.exec_batch(
                "CREATE TABLE packages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL,
                    version TEXT NOT NULL,
                    state TEXT NOT NULL CHECK (state IN ('staged', 'installed', 'removing')),
                    arch TEXT NOT NULL DEFAULT '',
                    description TEXT NOT NULL DEFAULT '',
                    url TEXT NOT NULL DEFAULT '',
                    license TEXT NOT NULL DEFAULT '',
                    origin TEXT NOT NULL DEFAULT '',
                    maintainer TEXT NOT NULL DEFAULT '',
                    build_time INTEGER NOT NULL DEFAULT 0,
                    installed_size INTEGER NOT NULL DEFAULT 0,
                    file_size INTEGER NOT NULL DEFAULT 0,
                    identity BLOB NOT NULL DEFAULT X'',
                    repo_commit BLOB NOT NULL DEFAULT X'',
                    provider_priority INTEGER NOT NULL DEFAULT 0,
                    replaces_priority INTEGER NOT NULL DEFAULT 0,
                    layer INTEGER NOT NULL DEFAULT 0,
                    source TEXT NOT NULL DEFAULT '',
                    UNIQUE(name, version)
                );
                CREATE INDEX idx_packages_state ON packages(state, name);
                CREATE TABLE pkg_deps (pkg_id INTEGER NOT NULL REFERENCES packages(id) ON DELETE CASCADE, kind TEXT NOT NULL, ord INTEGER NOT NULL, name TEXT NOT NULL, version TEXT NOT NULL, op INTEGER NOT NULL);
                CREATE TABLE pkg_tags (pkg_id INTEGER NOT NULL REFERENCES packages(id) ON DELETE CASCADE, ord INTEGER NOT NULL, tag TEXT NOT NULL);
                CREATE TABLE pkg_triggers (pkg_id INTEGER NOT NULL REFERENCES packages(id) ON DELETE CASCADE, ord INTEGER NOT NULL, pattern TEXT NOT NULL);
                CREATE TABLE pkg_scripts (pkg_id INTEGER NOT NULL REFERENCES packages(id) ON DELETE CASCADE, kind TEXT NOT NULL, body BLOB NOT NULL, PRIMARY KEY (pkg_id, kind));
                CREATE TABLE pkg_files (pkg_id INTEGER NOT NULL REFERENCES packages(id) ON DELETE CASCADE, ord INTEGER NOT NULL, kind TEXT NOT NULL, mode INTEGER NOT NULL, user TEXT, grp TEXT, size INTEGER NOT NULL, mtime INTEGER NOT NULL, hash BLOB NOT NULL DEFAULT X'', extra BLOB, dev INTEGER, xattrs BLOB, path BLOB NOT NULL);
                CREATE TABLE world (spec TEXT PRIMARY KEY);
                INSERT INTO packages (name, version, state, arch) VALUES ('foo', '1.0-r0', 'installed', 'x86_64');
                INSERT INTO pkg_files (pkg_id, ord, kind, mode, size, mtime, path) VALUES (1, 0, 'f', 493, 5, 0, X'7573722F62696E2F666F6F');
                INSERT INTO world (spec) VALUES ('foo');
                PRAGMA user_version = 1;",
            )
            .unwrap();
        }

        // Read-only open works on v1 DB without migrating it
        {
            let ro = Db::open_readonly(&root, false).unwrap();
            assert_eq!(ro.schema_ver, 1);
            assert_eq!(ro.installed["foo"].dir_name, "1.0-r0");
            assert_eq!(ro.read_pkg_files("foo", "1.0-r0").unwrap().len(), 1);
        }

        // Writable open upgrades v1 -> v2 in place while preserving all rows and FK children
        {
            let db = Db::open(&root, false).unwrap();
            assert_eq!(db.schema_ver, 2);
            assert_eq!(db.installed["foo"].dir_name, "1.0-r0");
            assert_eq!(db.read_pkg_files("foo", "1.0-r0").unwrap().len(), 1);
            assert_eq!(db.world.len(), 1);
            // Can now stage a same-version rebuild alongside 1.0-r0
            let mut m = PkgMeta::default();
            m.info.name = "foo".into();
            m.info.version = "1.0-r0".into();
            m.info.hashes = vec![0xab; 32];
            db.stage_packages(&[StagedPkg {
                dir_name: "1.0-r0+abababab".into(),
                meta: m,
                recs: vec![],
                scripts: vec![],
            }])
            .unwrap();
            assert!(db.has_pkg("foo", "1.0-r0").unwrap());
            assert!(db.has_pkg("foo", "1.0-r0+abababab").unwrap());
        }

        let _ = fs::remove_dir_all(&root);
    }
}
