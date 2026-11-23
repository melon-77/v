//! Typed readers for the index and package schemas (Part D).
//!
//! Field numbers are the on-disk slot numbers. Arrays and objects are told
//! apart by the schema, never by the type tag (§D1.2). Absent strings read
//! as "", absent integers as 0, absent arrays as empty (§D1.1).

use std::collections::{HashMap, HashSet};

use super::value::{Adb, Obj, Val};
use crate::digest::Alg;
use crate::error::{Error, Result};
use crate::version;

// schema_index (§D2)
pub const NDX_DESCRIPTION: usize = 1;
pub const NDX_PACKAGES: usize = 2;
pub const NDX_PKGNAME_SPEC: usize = 3;

// schema_pkginfo (§D4)
pub const PI_NAME: usize = 1;
pub const PI_VERSION: usize = 2;
pub const PI_HASHES: usize = 3;
pub const PI_DESCRIPTION: usize = 4;
pub const PI_ARCH: usize = 5;
pub const PI_LICENSE: usize = 6;
pub const PI_ORIGIN: usize = 7;
pub const PI_MAINTAINER: usize = 8;
pub const PI_URL: usize = 9;
pub const PI_REPO_COMMIT: usize = 10;
pub const PI_BUILD_TIME: usize = 11;
pub const PI_INSTALLED_SIZE: usize = 12;
pub const PI_FILE_SIZE: usize = 13;
pub const PI_PROVIDER_PRIORITY: usize = 14;
pub const PI_DEPENDS: usize = 15;
pub const PI_PROVIDES: usize = 16;
pub const PI_REPLACES: usize = 17;
pub const PI_INSTALL_IF: usize = 18;
pub const PI_RECOMMENDS: usize = 19;
pub const PI_LAYER: usize = 20;
pub const PI_TAGS: usize = 21;

// schema_dependency (§F3.3)
pub const DEP_NAME: usize = 1;
pub const DEP_VERSION: usize = 2;
pub const DEP_MATCH: usize = 3;

// schema_package (§D6)
pub const PKG_INFO: usize = 1;
pub const PKG_PATHS: usize = 2;
pub const PKG_SCRIPTS: usize = 3;
pub const PKG_TRIGGERS: usize = 4;
pub const PKG_REPLACES_PRIORITY: usize = 5;

// schema_dir (§D7)
pub const DI_NAME: usize = 1;
pub const DI_ACL: usize = 2;
pub const DI_FILES: usize = 3;

// schema_file (§D8.1)
pub const FI_NAME: usize = 1;
pub const FI_ACL: usize = 2;
pub const FI_SIZE: usize = 3;
pub const FI_MTIME: usize = 4;
pub const FI_HASH: usize = 5;
pub const FI_TARGET: usize = 6;

// schema_acl (§D8.3)
pub const ACL_MODE: usize = 1;
pub const ACL_USER: usize = 2;
pub const ACL_GROUP: usize = 3;
pub const ACL_XATTRS: usize = 4;

/// Script slots of schema_scripts (§D9), with their names.
pub const SCRIPT_NAMES: [&str; 7] = [
    "trigger",
    "pre-install",
    "post-install",
    "pre-deinstall",
    "post-deinstall",
    "pre-upgrade",
    "post-upgrade",
];

// Linux file type bits used in file targets (§D8.2).
pub const S_IFIFO: u16 = 0o010000;
pub const S_IFCHR: u16 = 0o020000;
pub const S_IFBLK: u16 = 0o060000;
pub const S_IFREG: u16 = 0o100000;
pub const S_IFLNK: u16 = 0o120000;

/// Linux `PATH_MAX`; apk's path builder needs `len + 2 < PATH_MAX` (§E7.1).
pub const PATH_MAX: usize = 4096;

fn utf8(b: &[u8], what: &str) -> Result<String> {
    String::from_utf8(b.to_vec()).map_err(|_| Error::Schema(format!("{what} is not valid UTF-8")))
}

/// An identifier-like string (names, versions, arch...): must be UTF-8.
fn ident(o: &Obj, slot: usize, what: &str) -> Result<String> {
    match o.blob(slot)? {
        Some(b) => utf8(b, what),
        None => Ok(String::new()),
    }
}

/// Free text (description, license, url...): invalid UTF-8 is replaced.
fn text(o: &Obj, slot: usize) -> Result<String> {
    Ok(o.blob(slot)?.map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default())
}

fn bytes(o: &Obj, slot: usize) -> Result<Vec<u8>> {
    Ok(o.blob(slot)?.map(|b| b.to_vec()).unwrap_or_default())
}

/// A dependency, as apk loads it from ADB (`apk_dep_from_adb`, §F3.5).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Dependency {
    /// Name, possibly namespaced (`so:libc.so.6`, `pc:zlib`, `cmd:sh`).
    pub name: String,
    /// Version, "" when absent.
    pub version: String,
    /// Match bits (§F2.1), normalised: no version gives ANY (7), a version
    /// with no match field gives EQUAL. Includes the CONFLICT bit (16).
    pub op: u8,
}

impl Dependency {
    pub(crate) fn from_obj(o: &Obj) -> Result<Dependency> {
        let raw = o.int(DEP_MATCH)?;
        let ver = o.blob(DEP_VERSION)?;
        let mut op = raw;
        match ver {
            None => op |= version::ANY as u64,
            Some(_) if op == 0 => op = version::EQUAL as u64,
            _ => {}
        }
        Ok(Dependency {
            name: ident(o, DEP_NAME, "dependency name")?,
            version: match ver {
                Some(v) => utf8(v, "dependency version")?,
                None => String::new(),
            },
            op: op as u8,
        })
    }

    /// Builds a dependency from text such as `foo`, `!foo`, `so:libc.so.6`,
    /// `foo>=1.2` (`apk_dep_parse`, §F3.1).
    pub fn parse(text: &str) -> Result<Dependency> {
        let (name, op, ver) = version::parse_dep(text)
            .ok_or_else(|| Error::Schema(format!("invalid dependency '{text}'")))?;
        Ok(Dependency { name: name.to_string(), version: ver.unwrap_or("").to_string(), op })
    }

    /// [`Dependency::parse`] for what a user asks for and the world file,
    /// checked as apk checks world entries (`apk_blob_pull_dep`, app_add.c
    /// 164-171): the name must not be empty, the version must be a valid
    /// version unless the op is the checksum form, and `name@tag`
    /// (repository pinning) is refused, since catalyst has no tagged
    /// repositories.
    pub fn parse_world(text: &str) -> Result<Dependency> {
        let d = Self::parse(text)?;
        if d.name.is_empty() {
            return Err(Error::Schema(format!("'{text}': no package name")));
        }
        if d.name.contains('@') {
            return Err(Error::Schema(format!("'{text}': repository tags (name@tag) are not supported")));
        }
        let checksum = d.op & version::CHECKSUM == version::CHECKSUM;
        if !d.version.is_empty() && !checksum && !version::validate(&d.version) {
            return Err(Error::Schema(format!("'{text}' is not a valid dependency: bad version '{}'", d.version)));
        }
        Ok(d)
    }

    pub fn is_conflict(&self) -> bool {
        self.op & version::CONFLICT != 0
    }

    /// Whether a provider version satisfies this dependency
    /// (`apk_version_match`; the checksum form `><` is not handled here).
    pub fn matches_version(&self, provider_version: &str) -> bool {
        version::version_match(provider_version, self.op, &self.version)
    }
}

impl std::fmt::Display for Dependency {
    /// Text form as apk prints it (`dependency_tostring`, §F3.6).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let bang = if self.is_conflict() { "!" } else { "" };
        if self.version.is_empty() && self.op & version::ANY == version::ANY {
            write!(f, "{bang}{}", self.name)
        } else {
            write!(f, "{bang}{}{}{}", self.name, version::op_string(self.op), self.version)
        }
    }
}

fn deps(o: &Obj, slot: usize) -> Result<Vec<Dependency>> {
    let arr = o.obj(slot)?;
    let mut v = Vec::with_capacity(arr.len());
    for item in arr.items() {
        if item.is_null() {
            continue;
        }
        v.push(Dependency::from_obj(&o.adb().obj(item)?)?);
    }
    Ok(v)
}

fn strings(o: &Obj, slot: usize, what: &str) -> Result<Vec<String>> {
    let arr = o.obj(slot)?;
    let mut v = Vec::with_capacity(arr.len());
    for item in arr.items() {
        if let Some(b) = o.adb().blob(item)? {
            v.push(utf8(b, what)?);
        }
    }
    Ok(v)
}

/// Like [`strings`], invalid UTF-8 replaced (opaque labels such as tags).
fn strings_lossy(o: &Obj, slot: usize) -> Result<Vec<String>> {
    let arr = o.obj(slot)?;
    let mut v = Vec::with_capacity(arr.len());
    for item in arr.items() {
        if let Some(b) = o.adb().blob(item)? {
            v.push(String::from_utf8_lossy(b).into_owned());
        }
    }
    Ok(v)
}

/// Package metadata (`schema_pkginfo`, §D4): an index entry, or a
/// package file's `info`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PkgInfo {
    pub name: String,
    pub version: String,
    /// Raw digest bytes. In an index: the package identity (SHA-256 of the
    /// package's ADB block payload, §D10.2). In a package file: a 20-byte
    /// unique id (§D10.1).
    pub hashes: Vec<u8>,
    pub description: String,
    pub arch: String,
    pub license: String,
    pub origin: String,
    pub maintainer: String,
    pub url: String,
    /// Raw bytes, normally a 20-byte git commit id.
    pub repo_commit: Vec<u8>,
    pub build_time: u64,
    pub installed_size: u64,
    /// Size of the .apk file; set in index entries only.
    pub file_size: u64,
    pub provider_priority: u64,
    pub depends: Vec<Dependency>,
    pub provides: Vec<Dependency>,
    pub replaces: Vec<Dependency>,
    pub install_if: Vec<Dependency>,
    pub recommends: Vec<Dependency>,
    pub layer: u64,
    pub tags: Vec<String>,
}

impl PkgInfo {
    pub(crate) fn from_obj(o: &Obj) -> Result<PkgInfo> {
        Ok(PkgInfo {
            name: ident(o, PI_NAME, "package name")?,
            version: ident(o, PI_VERSION, "package version")?,
            hashes: bytes(o, PI_HASHES)?,
            description: text(o, PI_DESCRIPTION)?,
            // opaque labels (§D4): not used as names, so invalid UTF-8 is
            // replaced instead of rejecting the entry
            arch: text(o, PI_ARCH)?,
            license: text(o, PI_LICENSE)?,
            origin: text(o, PI_ORIGIN)?,
            maintainer: text(o, PI_MAINTAINER)?,
            url: text(o, PI_URL)?,
            repo_commit: bytes(o, PI_REPO_COMMIT)?,
            build_time: o.int(PI_BUILD_TIME)?,
            installed_size: o.int(PI_INSTALLED_SIZE)?,
            file_size: o.int(PI_FILE_SIZE)?,
            provider_priority: o.int(PI_PROVIDER_PRIORITY)?,
            depends: deps(o, PI_DEPENDS)?,
            provides: deps(o, PI_PROVIDES)?,
            replaces: deps(o, PI_REPLACES)?,
            install_if: deps(o, PI_INSTALL_IF)?,
            recommends: deps(o, PI_RECOMMENDS)?,
            layer: o.int(PI_LAYER)?,
            tags: strings_lossy(o, PI_TAGS)?,
        })
    }

    /// The package identity from an index entry: the digest algorithm
    /// (by length, §D10.3) and its bytes. `None` when `hashes` is not
    /// 20, 32 or 64 bytes long: apk skips such entries.
    pub fn identity(&self) -> Option<(Alg, &[u8])> {
        Alg::from_len(self.hashes.len()).map(|a| (a, &self.hashes[..]))
    }

    /// `name-version`.
    pub fn nv(&self) -> String {
        format!("{}-{}", self.name, self.version)
    }
}

/// A repository index (`Packages.adb`, schema `indx`, §D2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Index {
    pub description: String,
    /// In stored order (normally sorted by name, then version).
    pub packages: Vec<PkgInfo>,
    /// Download-name template, if the index carries one (§G1.6).
    pub pkgname_spec: Option<String>,
    /// Entries that could not be read (an undecodable name, version or
    /// dependency), with the reason; they are left out of `packages`, as
    /// apk leaves out what it cannot use instead of rejecting the index.
    pub skipped: Vec<String>,
}

impl Index {
    pub fn from_adb(adb: &Adb) -> Result<Index> {
        let root = adb.root_obj()?;
        let arr = root.obj(NDX_PACKAGES)?;
        let mut packages = Vec::with_capacity(arr.len());
        let mut skipped = Vec::new();
        for (i, item) in arr.items().enumerate() {
            if item.is_null() {
                continue;
            }
            match adb.obj(item).and_then(|o| PkgInfo::from_obj(&o)) {
                Ok(p) => packages.push(p),
                Err(e) => skipped.push(format!("index package #{}: {e}", i + 1)),
            }
        }
        let spec = root.blob(NDX_PKGNAME_SPEC)?.map(|b| utf8(b, "pkgname-spec")).transpose()?;
        Ok(Index { description: text(&root, NDX_DESCRIPTION)?, packages, pkgname_spec: spec, skipped })
    }
}

/// Owner, group, mode and xattrs (`schema_acl`, §D8.3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Acl {
    /// Permission bits only (0..=0o7777); larger values are rejected.
    pub mode: u32,
    /// User name; `None` when absent (apk then uses uid 65534).
    pub user: Option<String>,
    /// Group name; `None` when absent (gid 65534).
    pub group: Option<String>,
    /// (name, value) pairs, split at the first NUL (§E5.3).
    pub xattrs: Vec<(Vec<u8>, Vec<u8>)>,
}

impl Acl {
    fn from_val(adb: &Adb, v: Val) -> Result<Acl> {
        let o = adb.obj(v)?;
        let mode = o.int(ACL_MODE)?;
        if mode > 0o7777 {
            return Err(Error::Schema(format!("mode {mode:o} has bits outside 07777")));
        }
        let name = |slot, what| -> Result<Option<String>> {
            o.blob(slot)?.map(|b| utf8(b, what)).transpose()
        };
        let mut xattrs = Vec::new();
        for item in o.obj(ACL_XATTRS)?.items() {
            let b = adb
                .blob(item)?
                .ok_or_else(|| Error::Schema("NULL xattr item".into()))?;
            let nul = b
                .iter()
                .position(|&c| c == 0)
                .ok_or_else(|| Error::Schema("xattr without a NUL separator".into()))?;
            xattrs.push((b[..nul].to_vec(), b[nul + 1..].to_vec()));
        }
        Ok(Acl { mode: mode as u32, user: name(ACL_USER, "user name")?, group: name(ACL_GROUP, "group name")?, xattrs })
    }
}

/// What a file entry is (§E2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileKind {
    /// Regular file; content in a DATA block when `size != 0`.
    Regular,
    /// Hard link to an earlier regular file of the same package, given as
    /// a package-relative path (a leading `./` written by mkpkg for
    /// root-level targets is removed, §E2).
    Hardlink(Vec<u8>),
    /// Symbolic link with its target, as stored (not validated by apk).
    Symlink(Vec<u8>),
    CharDev(u64),
    BlockDev(u64),
    Fifo(u64),
}

/// A file entry (`schema_file`, §D8.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// The base name (one path component).
    pub name: Vec<u8>,
    pub acl: Acl,
    /// Content size for regular files; the target length for symlinks.
    pub size: u64,
    pub mtime: u64,
    /// Content digest (20/32/64 bytes) for regular files and hard links;
    /// usually empty for other kinds.
    pub hash: Vec<u8>,
    pub kind: FileKind,
    /// The raw `target` blob (type u16 + payload), if present.
    pub target_raw: Option<Vec<u8>>,
    /// 1-based slot in its directory's `files` array, NULL slots counted
    /// (the file index of its DATA block, extract_v3.c 228-243).
    pub index: usize,
}

impl FileEntry {
    /// A DATA block carries the content iff the file is regular (no
    /// target) and its size is not 0 (§E1.2).
    pub fn has_data(&self) -> bool {
        self.kind == FileKind::Regular && self.size != 0
    }

    /// The content digest algorithm, for regular files and hard links.
    pub fn hash_alg(&self) -> Option<Alg> {
        Alg::from_len(self.hash.len())
    }

    /// st_mode: the permission bits with the file type bits.
    pub fn st_mode(&self) -> u32 {
        let t = match self.kind {
            FileKind::Regular | FileKind::Hardlink(_) => S_IFREG,
            FileKind::Symlink(_) => S_IFLNK,
            FileKind::CharDev(_) => S_IFCHR,
            FileKind::BlockDev(_) => S_IFBLK,
            FileKind::Fifo(_) => S_IFIFO,
        };
        t as u32 | self.acl.mode
    }
}

/// A directory with its files (`schema_dir`, §D7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dir {
    /// Path relative to the package root, empty for the root itself.
    /// One trailing `/` is tolerated by apk and removed here.
    pub name: Vec<u8>,
    pub acl: Acl,
    pub files: Vec<FileEntry>,
    /// 1-based slot in the package's `paths` array, NULL slots counted
    /// (the path index of its files' DATA blocks).
    pub index: usize,
}

impl Dir {
    /// Full package-relative path of one of its files.
    pub fn file_path(&self, f: &FileEntry) -> Vec<u8> {
        join(&self.name, &f.name)
    }
}

/// `dir/name`, or `name` for the root.
pub fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(dir.len() + 1 + name.len());
    if !dir.is_empty() {
        p.extend_from_slice(dir);
        p.push(b'/');
    }
    p.extend_from_slice(name);
    p
}

/// Package scripts (`schema_scripts`, §D9). Indexed like [`SCRIPT_NAMES`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scripts {
    pub scripts: [Option<Vec<u8>>; 7],
}

impl Scripts {
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        let i = SCRIPT_NAMES.iter().position(|&n| n == name)?;
        self.scripts[i].as_deref()
    }
    pub fn trigger(&self) -> Option<&[u8]> {
        self.scripts[0].as_deref()
    }
    pub fn pre_install(&self) -> Option<&[u8]> {
        self.scripts[1].as_deref()
    }
    pub fn post_install(&self) -> Option<&[u8]> {
        self.scripts[2].as_deref()
    }
    pub fn pre_deinstall(&self) -> Option<&[u8]> {
        self.scripts[3].as_deref()
    }
    pub fn post_deinstall(&self) -> Option<&[u8]> {
        self.scripts[4].as_deref()
    }
    pub fn pre_upgrade(&self) -> Option<&[u8]> {
        self.scripts[5].as_deref()
    }
    pub fn post_upgrade(&self) -> Option<&[u8]> {
        self.scripts[6].as_deref()
    }
    /// (name, body) of every script present.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &[u8])> {
        SCRIPT_NAMES.iter().zip(self.scripts.iter()).filter_map(|(n, s)| s.as_deref().map(|s| (*n, s)))
    }
    pub fn is_empty(&self) -> bool {
        self.scripts.iter().all(|s| s.is_none())
    }
}

/// A package file's metadata (`schema_package`, §D6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub info: PkgInfo,
    /// Directories in stored order (parents before children).
    pub dirs: Vec<Dir>,
    pub scripts: Scripts,
    /// Trigger globs (`+` prefix allowed), in stored order.
    pub triggers: Vec<String>,
    pub replaces_priority: u64,
}

/// `apk_fs_is_malicious_filename` (§E7.2), inverted: a single safe path
/// component.
pub fn valid_filename(name: &[u8]) -> bool {
    !(name.is_empty()
        || name == b"."
        || name == b".."
        || name.iter().any(|&c| c < 0x20 || c == 0x7f || c == b'/')
        || (name.len() == 53 && name.starts_with(b".apk.")))
}

/// `apk_fs_is_malicious_pathname` (§E7.2), inverted: every component is a
/// valid file name, so no leading `/`, no `//`, no `.` or `..` component.
/// One trailing `/` is tolerated, like apk.
pub fn valid_pathname(path: &[u8]) -> bool {
    let p = path.strip_suffix(b"/").unwrap_or(path);
    !p.is_empty() && p.split(|&c| c == b'/').all(valid_filename)
}

fn parse_target(t: &[u8]) -> Result<FileKind> {
    if t.len() < 2 {
        return Err(Error::Schema("file target shorter than 2 bytes".into()));
    }
    let ty = u16::from_le_bytes([t[0], t[1]]);
    let p = &t[2..];
    let dev = || -> Result<u64> {
        if p.len() != 8 {
            return Err(Error::Schema("device target payload is not 8 bytes".into()));
        }
        Ok(u64::from_le_bytes(p.try_into().unwrap()))
    };
    let link = || -> Result<Vec<u8>> {
        if p.len() >= PATH_MAX - 1 {
            return Err(Error::Schema("link target too long".into()));
        }
        if p.contains(&0) {
            return Err(Error::Schema("link target contains a NUL byte".into()));
        }
        Ok(p.to_vec())
    };
    Ok(match ty {
        S_IFIFO => FileKind::Fifo(dev()?),
        S_IFCHR => FileKind::CharDev(dev()?),
        S_IFBLK => FileKind::BlockDev(dev()?),
        S_IFLNK => FileKind::Symlink(link()?),
        S_IFREG => {
            let mut l = link()?;
            if l.starts_with(b"./") {
                l.drain(..2);
            }
            FileKind::Hardlink(l)
        }
        other => return Err(Error::Schema(format!("unknown file target type {other:#o}"))),
    })
}

fn path_str(p: &[u8]) -> String {
    String::from_utf8_lossy(p).into_owned()
}

impl Package {
    /// Reads and validates a package's metadata. Beyond apk's own checks
    /// (§E7.2: every directory name a safe path, every file name a safe
    /// component), this rejects: over-long paths (§E7.1), mode bits above
    /// 07777, regular files without a 20/32/64-byte hash, malformed targets
    /// and xattrs, duplicate paths, entries below a path that the package
    /// itself makes a non-directory (symlink escapes), and hard links that
    /// do not point at an earlier regular file of the package.
    pub fn from_adb(adb: &Adb) -> Result<Package> {
        let root = adb.root_obj()?;
        let info = PkgInfo::from_obj(&root.obj(PKG_INFO)?)?;
        let paths = root.obj(PKG_PATHS)?;
        let mut dirs = Vec::with_capacity(paths.len());
        for (slot, item) in paths.items().enumerate() {
            if item.is_null() {
                continue;
            }
            let d = adb.obj(item)?;
            let mut name = bytes(&d, DI_NAME)?;
            if !name.is_empty() {
                if !valid_pathname(&name) {
                    return Err(Error::Schema(format!("unsafe directory name '{}'", path_str(&name))));
                }
                if name.ends_with(b"/") {
                    name.pop();
                }
                if name.len() + 2 >= PATH_MAX {
                    return Err(Error::Schema("directory name too long".into()));
                }
            }
            let acl = Acl::from_val(adb, d.get(DI_ACL))?;
            let farr = d.obj(DI_FILES)?;
            let mut files = Vec::with_capacity(farr.len());
            for (fslot, fv) in farr.items().enumerate() {
                if fv.is_null() {
                    continue;
                }
                let f = adb.obj(fv)?;
                let fname = bytes(&f, FI_NAME)?;
                if !valid_filename(&fname) {
                    return Err(Error::Schema(format!(
                        "unsafe file name '{}' in '{}'",
                        path_str(&fname),
                        path_str(&name)
                    )));
                }
                if name.len() + fname.len() + 2 >= PATH_MAX {
                    return Err(Error::Schema("file path too long".into()));
                }
                let facl = Acl::from_val(adb, f.get(FI_ACL))?;
                let target = f.blob(FI_TARGET)?;
                let kind = match target {
                    Some(t) => parse_target(t)?,
                    None => FileKind::Regular,
                };
                let hash = bytes(&f, FI_HASH)?;
                if kind == FileKind::Regular && Alg::from_len(hash.len()).is_none() {
                    return Err(Error::Schema(format!(
                        "regular file '{}' has no valid hash",
                        path_str(&join(&name, &fname))
                    )));
                }
                files.push(FileEntry {
                    name: fname,
                    acl: facl,
                    size: f.int(FI_SIZE)?,
                    mtime: f.int(FI_MTIME)?,
                    hash,
                    kind,
                    target_raw: target.map(|t| t.to_vec()),
                    index: fslot + 1,
                });
            }
            dirs.push(Dir { name, acl, files, index: slot + 1 });
        }
        let so = root.obj(PKG_SCRIPTS)?;
        let mut scripts = Scripts::default();
        for (i, s) in scripts.scripts.iter_mut().enumerate() {
            *s = so.blob(i + 1)?.map(|b| b.to_vec());
        }
        let pkg = Package {
            info,
            dirs,
            scripts,
            triggers: strings(&root, PKG_TRIGGERS, "trigger")?,
            replaces_priority: root.int(PKG_REPLACES_PRIORITY)?,
        };
        pkg.check_tree()?;
        Ok(pkg)
    }

    fn check_tree(&self) -> Result<()> {
        // path -> is_dir
        let mut seen: HashMap<Vec<u8>, bool> = HashMap::new();
        let mut regular: HashSet<Vec<u8>> = HashSet::new();
        for d in &self.dirs {
            if !d.name.is_empty() && seen.insert(d.name.clone(), true).is_some() {
                return Err(Error::Schema(format!("duplicate directory '{}'", path_str(&d.name))));
            }
            for f in &d.files {
                let p = d.file_path(f);
                if seen.insert(p.clone(), false).is_some() {
                    return Err(Error::Schema(format!("duplicate path '{}'", path_str(&p))));
                }
                match &f.kind {
                    FileKind::Regular => {
                        regular.insert(p);
                    }
                    FileKind::Hardlink(t) => {
                        if !valid_pathname(t) || !regular.contains(t.as_slice()) {
                            return Err(Error::Schema(format!(
                                "hard link '{}' points to '{}', which is not an earlier file of the package",
                                path_str(&p),
                                path_str(t)
                            )));
                        }
                    }
                    _ => {}
                }
            }
        }
        // no entry may live below a non-directory entry of the package
        for p in seen.keys() {
            let mut i = 0;
            while let Some(k) = p[i..].iter().position(|&c| c == b'/') {
                let anc = &p[..i + k];
                if seen.get(anc) == Some(&false) {
                    return Err(Error::Schema(format!(
                        "'{}' is below '{}', which is not a directory",
                        path_str(p),
                        path_str(anc)
                    )));
                }
                i += k + 1;
            }
        }
        Ok(())
    }

    /// Number of file entries (all kinds).
    pub fn file_count(&self) -> usize {
        self.dirs.iter().map(|d| d.files.len()).sum()
    }

    /// Number of DATA blocks the package must carry.
    pub fn data_count(&self) -> usize {
        self.dirs.iter().flat_map(|d| &d.files).filter(|f| f.has_data()).count()
    }

    /// Sum of the sizes of the files that carry data.
    pub fn data_bytes(&self) -> u64 {
        self.dirs.iter().flat_map(|d| &d.files).filter(|f| f.has_data()).map(|f| f.size).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_filename(b"busybox"));
        assert!(valid_filename(b"a b\x80"));
        assert!(!valid_filename(b""));
        assert!(!valid_filename(b"."));
        assert!(!valid_filename(b".."));
        assert!(!valid_filename(b"a/b"));
        assert!(!valid_filename(b"a\nb"));
        assert!(!valid_filename(b"a\x7f"));
        let tmp = format!(".apk.{}", "0".repeat(48));
        assert!(!valid_filename(tmp.as_bytes()));
        assert!(valid_filename(format!(".apk.{}", "0".repeat(47)).as_bytes()));

        assert!(valid_pathname(b"usr/bin"));
        assert!(valid_pathname(b"usr/bin/"));
        assert!(!valid_pathname(b"usr/bin//"));
        assert!(!valid_pathname(b"/usr"));
        assert!(!valid_pathname(b"usr//bin"));
        assert!(!valid_pathname(b"usr/../etc"));
        assert!(!valid_pathname(b"./usr"));
        assert!(!valid_pathname(b""));
    }

    #[test]
    fn targets() {
        assert_eq!(parse_target(b"\x00\xa0busybox").unwrap(), FileKind::Symlink(b"busybox".to_vec()));
        assert_eq!(parse_target(b"\x00\x80./x").unwrap(), FileKind::Hardlink(b"x".to_vec()));
        let mut dev = vec![0x00, 0x20];
        dev.extend_from_slice(&0x103u64.to_le_bytes());
        assert_eq!(parse_target(&dev).unwrap(), FileKind::CharDev(0x103));
        assert!(parse_target(&dev[..9]).is_err());
        assert!(parse_target(b"\x00").is_err());
        assert!(parse_target(b"").is_err());
        assert!(parse_target(b"\x00\x40x").is_err()); // S_IFDIR
        assert!(parse_target(b"\xed\x81x").is_err()); // mode bits included
        assert!(parse_target(b"\x00\xa0a\x00b").is_err());
    }

    #[test]
    fn dependency_text() {
        let d = |n: &str, v: &str, op: u8| Dependency { name: n.into(), version: v.into(), op };
        assert_eq!(d("foo", "", 7).to_string(), "foo");
        assert_eq!(d("foo", "", 23).to_string(), "!foo");
        assert_eq!(d("foo", "1.2", 1).to_string(), "foo=1.2");
        assert_eq!(d("foo", "1.2", 5).to_string(), "foo>=1.2");
        assert_eq!(d("foo", "1.2", 9).to_string(), "foo~1.2");
        assert_eq!(d("foo", "1.2", 18).to_string(), "!foo<1.2");
        assert_eq!(Dependency::parse("so:libc.so.6").unwrap(), d("so:libc.so.6", "", 7));
        assert_eq!(Dependency::parse("!foo>=1.2").unwrap(), d("foo", "1.2", 21));
        assert!(Dependency::parse("foo>=").is_err());
    }
}
