//! vpm's database inside a managed root (GoboLinux-style layout).
//!
//! ```text
//! ROOT/Programs/<name>/<version>/...          the package's files
//! ROOT/Programs/<name>/<version>/.vpm/info    metadata, "key: value" lines
//! ROOT/Programs/<name>/<version>/.vpm/files   one line per entry
//! ROOT/Programs/<name>/<version>/.vpm/scripts/<type>
//! ROOT/Programs/<name>/Current -> <version>   the version in use
//! ROOT/var/lib/vpm/world                      what was asked for
//! ROOT/var/lib/vpm/journal                    the switch in progress
//! ROOT/var/lib/vpm/lock                       flock'ed while vpm runs
//! ROOT/var/cache/vpm/                         downloaded packages
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
pub const META_DIR: &str = ".vpm";
pub const PARTIAL_SUFFIX: &str = ".partial";
pub const STATE_DIR: &str = "var/lib/vpm";
pub const CACHE_DIR: &str = "var/cache/vpm";

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

/// Package metadata kept in `.vpm/info`.
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

/// Writes `.vpm/` (info, files, scripts/) into a package directory.
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

/// A package's script body from its `.vpm/scripts`.
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
/// vpm's own directories are checked this way before anything is created
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
    /// while there are any; `vpm fix` rebuilds them from the signed packages.
    pub broken: Vec<(String, String, String)>,
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
        // vpm's own directories must be real directories, not symlinks
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
        let mut db = Db { root: root.to_path_buf(), installed: BTreeMap::new(), broken: Vec::new(), world: Vec::new(), _lock: Some(lock) };
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
                return Ok(Db { root: root.to_path_buf(), installed: BTreeMap::new(), broken: Vec::new(), world: Vec::new(), _lock: None });
            }
            return Err(Error::Msg(format!(
                "{} has no vpm database (create one with --initdb)",
                root.display()
            )));
        }
        let mut db = Db { root: root.to_path_buf(), installed: BTreeMap::new(), broken: Vec::new(), world: Vec::new(), _lock: None };
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
                        eprintln!("vpm: warning: {}: {why}; ignored", e.path().display());
                        // a Current naming a real version directory: the record of
                        // that version is what can't be read (`vpm fix` rebuilds it)
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
    pub done: std::collections::HashSet<String>,
}

impl Journal {
    pub fn format(&self) -> String {
        let mut s = String::from("vpm-journal 1\n");
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
                ("install", 3) if !ended => j.ops.push(Op::Install { name: f[1].into(), new: f[2].into() }),
                ("upgrade", 4) if !ended => {
                    j.ops.push(Op::Upgrade { name: f[1].into(), old: f[2].into(), new: f[3].into() })
                }
                ("remove", 3) if !ended => j.ops.push(Op::Remove { name: f[1].into(), old: f[2].into() }),
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
                && op.old_version().map_or(true, safe_version);
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
            done: Default::default(),
        };
        let mut t = j.format();
        t.push_str("done pre:a\ndon");
        let p = Journal::parse(&t).unwrap();
        assert_eq!(p.ops, j.ops);
        assert_eq!(p.world, j.world);
        assert!(p.done.contains("pre:a"));
        assert!(Journal::parse("vpm-journal 1\ninstall a 1\n").is_err());
        assert!(Journal::parse("vpm-journal 1\ninstall ../a 1\nend\n").is_err());
    }
}
