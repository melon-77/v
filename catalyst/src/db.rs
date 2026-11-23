//! catalyst's database inside a managed root (GoboLinux-style layout).
//!
//! ```text
//! ROOT/Programs/<name>/<version>/...          the package's files
//! ROOT/Programs/<name>/<version>/.catalyst/info    metadata, "key: value" lines
//! ROOT/Programs/<name>/<version>/.catalyst/files   one line per entry
//! ROOT/Programs/<name>/<version>/.catalyst/scripts/<type>
//! ROOT/Programs/<name>/Current -> <version>   the version in use
//! ROOT/var/lib/catalyst/world                      what was asked for
//! ROOT/var/lib/catalyst/journal                    the switch in progress
//! ROOT/var/lib/catalyst/lock                       flock'ed while catalyst runs
//! ROOT/var/cache/catalyst/                         downloaded packages
//! ```
//!
//! A package is installed iff `Programs/<name>/Current` exists; its target
//! names the version directory.
//!
//! `files` format: tab-separated fields
//! `kind mode user group size mtime hash extra xattrs path`, where kind is
//! `d f h l c b p` (directory, regular, hard link, symlink, char/block
//! device, fifo), mode is octal permission bits, user/group are names or
//! `-`, hash is hex or `-`, extra is the link target (`h`, `l`) or the
//! device number (`c b p`) or `-`, xattrs is `-` or `hexname=hexvalue`
//! items joined by `,`, and path is root-relative. Text fields escape
//! `\`, tab, newline and other control bytes as `\\`, `\t`, `\n`, `\xHH`.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::adb::{self, Dependency, FileKind, Package, PkgInfo};
use crate::digest;
use crate::error::{Error, Result};
use crate::sys;

pub const PROGRAMS: &str = "Programs";
pub const CURRENT: &str = "Current";
pub const META_DIR: &str = ".catalyst";
pub const PARTIAL_SUFFIX: &str = ".partial";
pub const STATE_DIR: &str = "var/lib/catalyst";
pub const CACHE_DIR: &str = "var/cache/catalyst";

/// What a `files` line describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecKind {
    Dir,
    File(FileKind),
}

/// One entry of a package (`files` line).
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

// ---------------------------------------------------------------- files

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
    // A big record (Cataclysm's has 8,500 lines, which took 7 ms) is cut at
    // line ends into parts that are parsed on several threads. If a part
    // fails, the whole text goes the one-thread way, which names the line.
    const PART: usize = 128 * 1024;
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8).min(text.len() / PART);
    parse_files_on(text, threads)
}

/// [`parse_files`] with the text cut into `threads` parts (fewer than two:
/// all of it on this thread).
fn parse_files_on(text: &str, threads: usize) -> Result<Vec<FileRec>> {
    if threads >= 2 {
        let size = text.len() / threads;
        let mut parts: Vec<&str> = Vec::with_capacity(threads);
        let mut rest = text;
        while parts.len() + 1 < threads && rest.len() > size {
            let Some(p) = rest.as_bytes()[size..].iter().position(|&c| c == b'\n') else { break };
            let (a, b) = rest.split_at(size + p + 1);
            parts.push(a);
            rest = b;
        }
        parts.push(rest);
        let parsed: Vec<Result<Vec<FileRec>>> = std::thread::scope(|sc| {
            let hs: Vec<_> = parts.iter().map(|&p| sc.spawn(move || parse_file_lines(p))).collect();
            hs.into_iter()
                .map(|h| h.join().unwrap_or_else(|_| Err(Error::Format("files: a worker thread failed".into()))))
                .collect()
        });
        if parsed.iter().all(|r| r.is_ok()) {
            let mut v = Vec::with_capacity(parsed.iter().map(|r| r.as_ref().map_or(0, |v| v.len())).sum());
            for r in parsed {
                v.extend(r?);
            }
            return Ok(v);
        }
    }
    parse_file_lines(text)
}

fn parse_file_lines(text: &str) -> Result<Vec<FileRec>> {
    let mut v = Vec::with_capacity(text.len() / 96);
    for (n, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let bad = || Error::Format(format!("files line {}: malformed", n + 1));
        // exactly ten fields
        let mut f = [""; 10];
        let mut fields = line.split('\t');
        for slot in f.iter_mut() {
            *slot = fields.next().ok_or_else(bad)?;
        }
        if fields.next().is_some() {
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

/// Package metadata kept in `.catalyst/info`.
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
            // the text form when it reads back as the same dependency;
            // otherwise (an op without a text form, an empty but present
            // version, ...) the raw fields: "KEY-raw: OP<tab>NAME<tab>VERSION"
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

/// Writes `.catalyst/` (info, files, scripts/) into a package directory.
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

/// A package's script body from its `.catalyst/scripts`.
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

/// catalyst's own directories in a root. No package entry may be, or lie
/// below, one of them, by its literal path or by where it resolves to in
/// the live tree: a package could otherwise plant a symlink there and make
/// catalyst write or delete through it.
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
/// catalyst's own directories are checked this way before anything is created
/// or deleted below them.
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

// ---------------------------------------------------------------- database

/// An installed package (metadata only; files are read on demand).
#[derive(Debug, Clone)]
pub struct Installed {
    pub meta: PkgMeta,
    pub dir: PathBuf,
}

pub struct Db {
    pub root: PathBuf,
    pub installed: BTreeMap<String, Installed>,
    pub world: Vec<Dependency>,
    /// Packages whose `Current` names a real version directory but whose
    /// record there can't be read: (name, version, why). Nothing is changed
    /// while there are any; `oct fix` rebuilds them from the signed packages.
    pub broken: Vec<(String, String, String)>,
    /// The lookup index, when it matched every installed version at the
    /// last (re)load.
    pub index: Option<crate::index::Index>,
    _lock: Option<File>,
}

impl Db {
    pub fn programs(root: &Path) -> PathBuf {
        root.join(PROGRAMS)
    }
    pub fn pkg_dir(root: &Path, name: &str, version: &str) -> PathBuf {
        root.join(PROGRAMS).join(name).join(version)
    }
    pub fn state(root: &Path) -> PathBuf {
        root.join(STATE_DIR)
    }
    pub fn journal_path(root: &Path) -> PathBuf {
        root.join(STATE_DIR).join("journal")
    }
    /// Where a journal is written before it is in effect; one left there by
    /// a crash was never in effect (the switch had not started) and goes.
    pub fn journal_pending_path(root: &Path) -> PathBuf {
        root.join(STATE_DIR).join("journal.new")
    }
    /// The world a change ends with, written with its journal before the
    /// switch; one left without a journal belongs to a change that never
    /// began and goes.
    pub fn world_pending_path(root: &Path) -> PathBuf {
        root.join(STATE_DIR).join("world.new")
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

    /// Opens (or with `initdb` creates) the database and takes the lock.
    pub fn open(root: &Path, initdb: bool) -> Result<Db> {
        let state = Self::state(root);
        // catalyst's own directories must be real directories, not symlinks
        // that would lead its writes and deletions elsewhere
        let own = Self::own_dirs(root);
        for d in &own {
            real_dir(d).map_err(Error::Io)?;
        }
        if initdb {
            fs::create_dir_all(root)?;
            for d in &own {
                if !d.exists() {
                    fs::create_dir_all(d)?;
                }
            }
            let w = state.join("world");
            if !w.exists() {
                fs::write(&w, "")?;
            }
        } else if !state.join("world").exists() {
            return Err(Error::Msg(format!(
                "{} has no catalyst database (create one with --initdb)",
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
            return Err(Error::Msg(format!("{}: another catalyst is running on this root", root.display())));
        }
        {
            use std::os::unix::io::AsRawFd;
            sys::set_lock_fd(lock.as_raw_fd());
        }
        let mut db = Db { root: root.to_path_buf(), installed: BTreeMap::new(), broken: Vec::new(), index: None, world: Vec::new(), _lock: Some(lock) };
        db.reload()?;
        Ok(db)
    }

    /// Opens the database read-only without taking a lock or creating
    /// directories. With `allow_missing`, a root without a database yet
    /// opens as empty (for `--initdb --simulate`).
    pub fn open_readonly(root: &Path, allow_missing: bool) -> Result<Db> {
        let state = Self::state(root);
        for d in &Self::own_dirs(root) {
            real_dir(d).map_err(Error::Io)?;
        }
        if !state.join("world").exists() {
            if allow_missing {
                return Ok(Db { root: root.to_path_buf(), installed: BTreeMap::new(), broken: Vec::new(), index: None, world: Vec::new(), _lock: None });
            }
            return Err(Error::Msg(format!(
                "{} has no catalyst database (create one with --initdb)",
                root.display()
            )));
        }
        let mut db = Db { root: root.to_path_buf(), installed: BTreeMap::new(), broken: Vec::new(), index: None, world: Vec::new(), _lock: None };
        db.reload()?;
        Ok(db)
    }

    /// (Re)reads the installed packages and the world. An entry of
    /// `Programs` that is not a valid installed package (a `Current` that
    /// is not a plain version name, a version directory that is a symlink,
    /// metadata that cannot be read or does not match its place) is skipped
    /// with a warning instead of making every command fail.
    pub fn reload(&mut self) -> Result<()> {
        self.installed.clear();
        self.broken.clear();
        self.index = None;
        // with an index that matches every Current exactly, the packages'
        // metadata comes from it instead of one info file per package; never
        // while there is a journal (a switch running, or interrupted)
        let journal = Self::journal_path(&self.root).exists();
        if let Some(Ok((versions, false))) = (!journal).then(|| crate::index::installed_versions(&self.root)) {
            if let Some(ix) = crate::index::Index::open(&self.root, &versions) {
                if let Ok(metas) = ix.metas() {
                    let same = metas.len() == versions.len()
                        && metas.iter().all(|(n, m)| versions.get(n).map(String::as_str) == Some(m.info.version.as_str()));
                    if same {
                        let programs = self.root.join(PROGRAMS);
                        for (n, m) in metas {
                            let dir = programs.join(&n).join(&m.info.version);
                            self.installed.insert(n, Installed { meta: m, dir });
                        }
                        self.index = Some(ix);
                        self.world = read_world(&self.root)?;
                        return Ok(());
                    }
                }
            }
        }
        let programs = self.root.join(PROGRAMS);
        if programs.exists() {
            let mut names: Vec<_> = fs::read_dir(&programs)?.collect::<io::Result<Vec<_>>>()?;
            names.sort_by_key(|e| e.file_name());
            for e in names {
                match Self::load_installed(&e.path()) {
                    Ok(Some(ip)) => {
                        self.installed.insert(ip.meta.info.name.clone(), ip);
                    }
                    Ok(None) => {}
                    Err(why) => {
                        eprintln!("oct: warning: {}: {why}; ignored", e.path().display());
                        // a Current naming a real version directory: the record of
                        // that version is what can't be read (`oct fix` rebuilds it)
                        let name = e.file_name().to_string_lossy().into_owned();
                        if let Ok(t) = fs::read_link(e.path().join(CURRENT)) {
                            let v = t.to_string_lossy().into_owned();
                            if safe_component(&name)
                                && safe_version(&v)
                                && fs::symlink_metadata(e.path().join(&v)).map_or(false, |m| m.is_dir())
                            {
                                self.broken.push((name, v, why));
                            }
                        }
                    }
                }
            }
        }
        self.world = read_world(&self.root)?;
        Ok(())
    }

    fn load_installed(pdir: &Path) -> std::result::Result<Option<Installed>, String> {
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
        if !safe_version(ts) {
            return Err(format!("Current points at '{}', not a version", String::from_utf8_lossy(tb)));
        }
        let dir = pdir.join(&target);
        match fs::symlink_metadata(&dir) {
            Ok(m) if m.is_dir() => {}
            _ => return Err(format!("{ts} is not a directory")),
        }
        let meta = read_meta(&dir).map_err(|e| e.to_string())?;
        let fname = pdir.file_name().map(|n| n.as_bytes()).unwrap_or(b"");
        if meta.info.name.as_bytes() != fname || meta.info.version.as_bytes() != tb {
            return Err("metadata does not match its place".into());
        }
        Ok(Some(Installed { meta, dir }))
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
            Err(e) => eprintln!("oct: warning: world: {e}; ignored"),
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

/// The world step of a switch. The new world was written as `world.new`
/// before the syncfs that preceded the switch ([`JournalFile::prepare`]),
/// so renaming it into place puts on disk a name whose content is already
/// there. A re-run finds the rename done. Without a `world.new` that says
/// what the plan says (a journal written by an older catalyst), the world is
/// written with an fsync of its own.
pub fn switch_world(root: &Path, world: &[Dependency]) -> io::Result<()> {
    let p = Db::state(root).join("world");
    let staged = Db::world_pending_path(root);
    let want = format_world(world);
    match fs::read(&staged) {
        Ok(b) if b == want.as_bytes() => return fs::rename(&staged, &p),
        Ok(_) => {
            let _ = fs::remove_file(&staged);
        }
        Err(_) => {}
    }
    if fs::read(&p).map_or(false, |b| b == want.as_bytes()) {
        return Ok(());
    }
    write_atomic(&p, want.as_bytes())
}

/// Writes a file atomically: temporary name, fsync, rename.
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("catalyst-tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

/// Replaces a file through a temporary name, without an fsync: for writes
/// that something later makes durable (a switch's final `syncfs`).
pub fn write_replace(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("catalyst-tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)
}

/// Points `Programs/<name>/Current` at `version` atomically.
pub fn set_current(root: &Path, name: &str, version: &str) -> io::Result<()> {
    let d = root.join(PROGRAMS).join(name);
    let tmp = d.join(".Current.catalyst-tmp");
    let _ = fs::remove_file(&tmp);
    symlink(version, &tmp)?;
    fs::rename(&tmp, d.join(CURRENT))
}

// ---------------------------------------------------------------- journal

/// One package change of a transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Install { name: String, new: String },
    Upgrade { name: String, old: String, new: String },
    Remove { name: String, old: String },
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
}

/// The plan of a switch, as written to the journal before the live tree
/// is touched, plus the steps marked done since.
#[derive(Debug, Clone, Default)]
pub struct Journal {
    /// Package changes in the order they are applied (dependencies first).
    pub ops: Vec<Op>,
    /// The world to write.
    pub world: Vec<String>,
    /// With the lookup index: the unchanged packages whose records the
    /// switch reads, and the stamp of the index it was planned with (both
    /// `None`: every record).
    pub reads: Option<Vec<String>>,
    pub index: Option<String>,
    /// Written by the catalyst of 23 November 2026 only, for an add it put
    /// in effect without any sync (see `unsynced.rs`). Never set now; a
    /// journal that has it is finished like any other, with a note.
    pub unsynced: bool,
    /// The change was asked for with `--no-scripts`: whoever finishes it
    /// after an interruption runs no script and no trigger either (a
    /// journal written before this was recorded is finished with the
    /// options of the command that finishes it).
    pub no_scripts: bool,
    /// The new versions of this change are closed (mode 0700) until the
    /// switch has given their entries their final owners, and are opened
    /// then: one that is found open has had that done, and is not gone
    /// through by path again (other users can enter it by then). A journal
    /// written before this was recorded has versions that were open from
    /// the start.
    pub closed_versions: bool,
    pub done: std::collections::HashSet<String>,
}

impl Journal {
    pub fn format(&self) -> String {
        let mut s = String::from("catalyst-journal 1\n");
        for op in &self.ops {
            match op {
                Op::Install { name, new } => s.push_str(&format!("install {name} {new}\n")),
                Op::Upgrade { name, old, new } => s.push_str(&format!("upgrade {name} {old} {new}\n")),
                Op::Remove { name, old } => s.push_str(&format!("remove {name} {old}\n")),
            }
        }
        for w in &self.world {
            s.push_str(&format!("world {w}\n"));
        }
        if let (Some(r), Some(st)) = (&self.reads, &self.index) {
            s.push_str(&format!("index {st}\nreads"));
            for n in r {
                s.push(' ');
                s.push_str(n);
            }
            s.push('\n');
        }
        if self.unsynced {
            s.push_str("unsynced\n");
        }
        if self.no_scripts {
            s.push_str("no-scripts\n");
        }
        if self.closed_versions {
            s.push_str("closed-versions\n");
        }
        s.push_str("end\n");
        s
    }

    pub fn parse(t: &str) -> Result<Journal> {
        let mut j = Journal::default();
        // only whole lines count: a last line without its newline is a done
        // mark that was cut while it was written
        let t = t.rfind('\n').map_or("", |i| &t[..=i]);
        let mut lines = t.lines();
        if lines.next() != Some("catalyst-journal 1") {
            return Err(Error::Format("journal: bad header".into()));
        }
        let mut ended = false;
        for l in lines {
            let f: Vec<&str> = l.split(' ').collect();
            let bad = || Error::Format(format!("journal: bad line '{l}'"));
            match (f[0], f.len()) {
                ("install", 3) if !ended => j.ops.push(Op::Install { name: f[1].into(), new: f[2].into() }),
                ("upgrade", 4) if !ended => {
                    j.ops.push(Op::Upgrade { name: f[1].into(), old: f[2].into(), new: f[3].into() })
                }
                ("remove", 3) if !ended => j.ops.push(Op::Remove { name: f[1].into(), old: f[2].into() }),
                ("world", 2) if !ended => j.world.push(f[1].into()),
                ("index", 2) if !ended => j.index = Some(f[1].into()),
                ("reads", _) if !ended => {
                    j.reads = Some(f[1..].iter().filter(|s| !s.is_empty()).map(|s| s.to_string()).collect())
                }
                ("unsynced", 1) if !ended => j.unsynced = true,
                ("no-scripts", 1) if !ended => j.no_scripts = true,
                ("closed-versions", 1) if !ended => j.closed_versions = true,
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
                && op.old_version().map_or(true, safe_version);
            if !ok {
                return Err(Error::Format("journal: unsafe name".into()));
            }
        }
        if j.reads.is_some() != j.index.is_some()
            || j.reads.iter().flatten().any(|n| !safe_component(n))
            || j.index.as_ref().map_or(false, |s| s.len() != 64 || !s.bytes().all(|c| c.is_ascii_hexdigit()))
        {
            return Err(Error::Format("journal: bad reads or index line".into()));
        }
        Ok(j)
    }
}

/// The journal file while a switch runs: steps are appended as they finish.
pub struct JournalFile {
    f: File,
    pub plan: Journal,
}

/// A journal written under its temporary name, not yet in effect.
pub struct PendingJournal {
    plan: Journal,
}

impl PendingJournal {
    /// Puts the journal in effect: renamed to its name, and the directory
    /// fsync'ed. Called after a syncfs made its content durable, so the
    /// journal is never there before what it says.
    pub fn commit(self, root: &Path) -> Result<JournalFile> {
        let p = Db::journal_path(root);
        fs::rename(Db::journal_pending_path(root), &p)?;
        File::open(Db::state(root))?.sync_all()?;
        crate::prof::mark("  journal: its name fsync'ed");
        let f = OpenOptions::new().append(true).open(&p)?;
        Ok(JournalFile { f, plan: self.plan })
    }

    /// Puts the journal in effect by its rename alone, for a light change
    /// (every change without `--sync`; README, "One sync"). Called, like
    /// [`commit`](Self::commit), after the syncfs that put the journal's
    /// content and everything it names on disk. The rename is the first
    /// thing the switch does to the file system, so on a file system that
    /// commits what was not fsync'ed in the order it was done, nothing the
    /// switch does can be on disk without it; `Switch::harden` fsyncs the
    /// directory before a script runs, which may not rest on that.
    pub fn commit_named(self, root: &Path) -> Result<JournalFile> {
        let p = Db::journal_path(root);
        fs::rename(Db::journal_pending_path(root), &p)?;
        let f = OpenOptions::new().append(true).open(&p)?;
        Ok(JournalFile { f, plan: self.plan })
    }
}

impl JournalFile {
    /// Writes the plan under the journal's temporary name, without an
    /// fsync: the syncfs before the switch makes it durable together with
    /// the unpacked packages, then [`PendingJournal::commit`] renames it.
    ///
    /// The world the plan ends with is written beside it, as `world.new`:
    /// the same syncfs puts it on disk, and the switch renames it into
    /// place ([`switch_world`]), so the world is never a name whose content
    /// has not reached the disk.
    pub fn prepare(root: &Path, plan: Journal) -> Result<PendingJournal> {
        let world: Vec<Dependency> = plan.world.iter().filter_map(|w| Dependency::parse(w).ok()).collect();
        fs::write(Db::world_pending_path(root), format_world(&world))?;
        fs::write(Db::journal_pending_path(root), plan.format())?;
        Ok(PendingJournal { plan })
    }

    pub fn open(root: &Path) -> Result<Option<JournalFile>> {
        let p = Db::journal_path(root);
        // marks appended without a sync can come back from a power cut as
        // anything: read as bytes, so that such a tail does not make the
        // plan before it unreadable
        let b = match fs::read(&p) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let plan = Journal::parse(&String::from_utf8_lossy(&b))?;
        let f = OpenOptions::new().append(true).open(&p)?;
        // A torn last mark is cut off, so that the next one starts on a
        // line of its own. It is not completed with a newline: what is left
        // of `done placed:zstd-libs` may read `done placed:zstd`, a whole
        // mark of another package for the next reader.
        let keep = b.iter().rposition(|&c| c == b'\n').map_or(0, |i| i + 1);
        if keep < b.len() {
            f.set_len(keep as u64)?;
        }
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

    /// Deletes the journal, without an fsync of its directory: the switch's
    /// final syncfs made every mark durable, so a journal that comes back
    /// after a crash is finished again with every step already marked done.
    pub fn remove(self, root: &Path) -> Result<()> {
        drop(self.f);
        fs::remove_file(Db::journal_path(root))?;
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
        // forms without a text form that reads back the same (§G2 quirk,
        // an op mask with no text, an empty but present version)
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
    fn journal_roundtrip() {
        let j = Journal {
            ops: vec![
                Op::Install { name: "a".into(), new: "1".into() },
                Op::Upgrade { name: "b".into(), old: "1".into(), new: "2".into() },
                Op::Remove { name: "c".into(), old: "3".into() },
            ],
            world: vec!["a".into(), "b>=2".into()],
            reads: Some(vec!["d".into(), "e".into()]),
            index: Some("ab".repeat(32)),
            unsynced: false,
            no_scripts: false,
            closed_versions: false,
            done: Default::default(),
        };
        // the line an earlier catalyst wrote is still read
        let u = Journal { unsynced: true, ..j.clone() };
        assert!(u.format().contains("\nunsynced\nend\n"));
        assert!(Journal::parse(&u.format()).unwrap().unsynced && !Journal::parse(&j.format()).unwrap().unsynced);
        // a change asked for without scripts says so, for whoever finishes it
        let n = Journal { no_scripts: true, ..j.clone() };
        assert!(n.format().ends_with("\nno-scripts\nend\n") && !j.format().contains("no-scripts"));
        assert!(Journal::parse(&n.format()).unwrap().no_scripts && !Journal::parse(&j.format()).unwrap().no_scripts);
        // new versions closed until their owners are final: said too
        let c = Journal { closed_versions: true, ..n.clone() };
        assert!(c.format().ends_with("\nno-scripts\nclosed-versions\nend\n"));
        assert!(Journal::parse(&c.format()).unwrap().closed_versions && !Journal::parse(&n.format()).unwrap().closed_versions);
        // marks: whole lines only; a cut last one does not count
        let t = format!("{}done placed:a\ndone placed:b", j.format());
        let p = Journal::parse(&t).unwrap();
        assert!(p.done.contains("placed:a") && !p.done.contains("placed:b"));
        assert!(Journal::parse(&j.format()[..j.format().len() - 1]).is_err(), "a plan cut before its end");
        let mut t = j.format();
        t.push_str("done pre:a\ndon");
        let p = Journal::parse(&t).unwrap();
        assert_eq!(p.ops, j.ops);
        assert_eq!(p.world, j.world);
        assert_eq!(p.reads, j.reads);
        assert_eq!(p.index, j.index);
        assert!(p.done.contains("pre:a"));
        assert!(Journal::parse("catalyst-journal 1\ninstall a 1\n").is_err());
        assert!(Journal::parse("catalyst-journal 1\ninstall ../a 1\nend\n").is_err());
    }

    /// A big record is parsed in parts on several threads: the result is
    /// that of the one-thread parser, and an error names the same line.
    #[test]
    fn a_big_record_parsed_in_parts_is_the_same() {
        let recs: Vec<FileRec> = (0..6000)
            .map(|i| FileRec {
                kind: if i % 50 == 0 { RecKind::Dir } else { RecKind::File(FileKind::Regular) },
                mode: 0o644,
                user: Some("root".into()),
                group: Some("root".into()),
                size: i,
                mtime: 1_700_000_000 + i,
                hash: if i % 50 == 0 { vec![] } else { vec![(i % 251) as u8; 32] },
                xattrs: vec![],
                path: format!("usr/share/a-package-with-many-files/dir{}/file-{i}-with-a-long-name.json", i / 50).into_bytes(),
            })
            .collect();
        let text = String::from_utf8(format_files(&recs)).unwrap();
        assert!(text.len() > 4 * 128 * 1024, "big enough for several parts: {}", text.len());
        // in four parts whatever this machine has (with one CPU parse_files
        // would not cut the text at all)
        let parsed = parse_files_on(&text, 4).unwrap();
        assert_eq!(parsed.len(), 6000);
        assert_eq!(parse_files(&text).unwrap().len(), 6000);
        assert!(format_files(&parsed) == text.as_bytes() && format_files(&parse_file_lines(&text).unwrap()) == text.as_bytes());
        // a bad line far into the text: the same error either way
        let mut lines: Vec<&str> = text.lines().collect();
        lines[5899] = "f\t0644\tnot enough fields";
        let bad = lines.join("\n") + "\n";
        let (a, b) = (parse_files_on(&bad, 4).unwrap_err().to_string(), parse_file_lines(&bad).unwrap_err().to_string());
        assert!(a == b && a.contains("line 5900"), "{a} / {b}");
        // without its last newline, and with empty lines
        assert_eq!(parse_files_on(text.trim_end(), 4).unwrap().len(), 6000);
        assert_eq!(parse_files_on(&text.replace("\nd\t", "\n\nd\t"), 4).unwrap().len(), 6000);
    }

    /// A journal whose last mark was cut while it was written: the mark
    /// does not count, and it is cut off, not completed, so that it does
    /// not count for the next reader either.
    #[test]
    fn a_torn_mark_of_the_journal_is_cut_off() {
        let root = std::env::temp_dir().join(format!("catalyst-journal-torn-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(Db::state(&root)).unwrap();
        let j = Journal {
            ops: vec![Op::Install { name: "zstd-libs".into(), new: "1".into() }, Op::Install { name: "zstd".into(), new: "1".into() }],
            ..Default::default()
        };
        // "done placed:zstd-libs" cut after "zstd"
        let whole = format!("{}done start\n", j.format());
        fs::write(Db::journal_path(&root), format!("{whole}done placed:zstd")).unwrap();
        for _ in 0..2 {
            let jf = JournalFile::open(&root).unwrap().unwrap();
            assert!(jf.is_done("start") && !jf.is_done("placed:zstd"));
            drop(jf);
            assert_eq!(fs::read_to_string(Db::journal_path(&root)).unwrap(), whole);
        }
        let mut jf = JournalFile::open(&root).unwrap().unwrap();
        jf.mark_with("placed:zstd-libs", false).unwrap();
        drop(jf);
        assert_eq!(fs::read_to_string(Db::journal_path(&root)).unwrap(), format!("{whole}done placed:zstd-libs\n"));
        fs::remove_dir_all(&root).unwrap();
    }
}
