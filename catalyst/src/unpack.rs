//! Unpacking a package into `ROOT/Programs/<name>/<version>.partial/`,
//! renamed to `<version>` once complete.
//!
//! The package is read in one streaming pass ([`PackageReader`]): trust is
//! established before any content is read (identity from the signed index,
//! else a signature), the file tree is validated (no absolute paths, no
//! `..`, nothing below one of the package's own symlinks, hard links only
//! to earlier files), and every file's content is checked against its hash
//! as it is written. Nothing here touches the live tree.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::{self, File, FileTimes, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{fchown, lchown, symlink, DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use crate::adb::{Acl, EntryKind, FileKind, PackageReader};
use crate::adb::schema::{S_IFBLK, S_IFCHR, S_IFIFO};
use crate::db::{self, PkgMeta};
use crate::error::{Error, Result};
use crate::rootfs::RootFs;
use crate::sys;
use crate::verify::{TrustedKeys, VerifyOptions};

/// User and group names to ids, from a root's `etc/passwd` and
/// `etc/group` (§E5.2): a known name gives its id, `root` gives 0 even if
/// unknown, anything else gives 65534.
#[derive(Debug, Default, Clone)]
pub struct IdMap {
    users: HashMap<String, u32>,
    groups: HashMap<String, u32>,
}

pub const NOBODY: u32 = 65534;

fn parse_ids(text: &str) -> HashMap<String, u32> {
    let mut m = HashMap::new();
    for l in text.lines() {
        let f: Vec<&str> = l.split(':').collect();
        if f.len() >= 3 {
            if let Ok(id) = f[2].parse::<u32>() {
                // a name listed twice: the last entry counts, as in apk's
                // id cache (io.c 1317-1329)
                m.insert(f[0].to_string(), id);
            }
        }
    }
    m
}

impl IdMap {
    /// Reads the live root's files (resolved inside the root; regular
    /// files of at most 16 MiB).
    pub fn load(root: &Path) -> IdMap {
        let rfs = RootFs::new(root);
        let read = |p: &[u8]| {
            rfs.read_file(p, 16 << 20)
                .ok()
                .flatten()
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default()
        };
        IdMap { users: parse_ids(&read(b"etc/passwd")), groups: parse_ids(&read(b"etc/group")) }
    }

    /// Reads the files that [`Self::load`] will read, and drops them: a
    /// hint ([`crate::warm`]).
    pub fn hint(root: &Path) {
        let rfs = RootFs::new(root);
        for p in [&b"etc/passwd"[..], b"etc/group"] {
            if let Ok(Some(_)) = rfs.read_file(p, 16 << 20) {
                crate::warm::named(&rfs.host(p));
            }
        }
    }

    fn lookup(m: &HashMap<String, u32>, name: Option<&str>) -> (u32, bool) {
        match name {
            Some(n) => match m.get(n) {
                Some(&id) => (id, true),
                None if n == "root" => (0, true),
                None => (NOBODY, false),
            },
            None => (NOBODY, true),
        }
    }

    pub fn uid(&self, name: Option<&str>) -> u32 {
        Self::lookup(&self.users, name).0
    }
    pub fn gid(&self, name: Option<&str>) -> u32 {
        Self::lookup(&self.groups, name).0
    }
    /// Are both names of an ACL known?
    pub fn resolves(&self, user: Option<&str>, group: Option<&str>) -> bool {
        Self::lookup(&self.users, user).1 && Self::lookup(&self.groups, group).1
    }
}

/// What to unpack.
pub struct Job {
    pub name: String,
    pub version: String,
    /// The package file.
    pub src: PathBuf,
    /// The index entry (its `hashes` is the identity to check).
    pub index_info: crate::adb::PkgInfo,
    /// Where it came from (recorded in `.catalyst/info`).
    pub source: String,
}

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub entries: u64,
    pub bytes: u64,
    pub warnings: Vec<String>,
}

fn path_of(base: &Path, rel: &[u8]) -> PathBuf {
    base.join(OsStr::from_bytes(rel))
}

fn ensure_parent(
    base: &Path,
    rel: &[u8],
    made: &mut HashSet<Vec<u8>>,
    setgid: &mut HashSet<Vec<u8>>,
    chown: bool,
    // every directory made here needs its owner set (root's group is not 0)
    always: bool,
) -> io::Result<()> {
    let Some(i) = rel.iter().rposition(|&c| c == b'/') else { return Ok(()) };
    let parent = &rel[..i];
    if made.contains(parent) {
        return Ok(());
    }
    // an ancestor the package does not list: 0755 root:root (§E4)
    ensure_parent(base, parent, made, setgid, chown, always)?;
    let p = path_of(base, parent);
    let created = match fs::DirBuilder::new().mode(0o755).create(&p) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
        Err(e) => return Err(e),
    };
    let grandparent = crate::rootfs::split_rel(parent).0;
    if always || setgid.contains(grandparent) {
        if created && chown {
            lchown(&p, Some(0), Some(0))?;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755))?;
        } else {
            setgid.insert(parent.to_vec());
        }
    }
    made.insert(parent.to_vec());
    Ok(())
}

/// The times of a file whose package says `t` seconds. A time no clock of
/// this system holds (2^63 seconds and more) is an error of the package:
/// adding it up without the check stopped the whole process.
fn mtime(t: u64) -> io::Result<FileTimes> {
    let st = UNIX_EPOCH.checked_add(Duration::from_secs(t)).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, format!("modification time {t} cannot be represented"))
    })?;
    Ok(FileTimes::new().set_accessed(st).set_modified(st))
}

/// Whether what `f` holds has the hash `hash` of a record.
fn content_is(f: &mut File, hash: &[u8]) -> bool {
    let Some(alg) = crate::digest::Alg::from_len(hash.len()) else { return false };
    let mut h = crate::digest::Hasher::new(alg);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => h.update(&buf[..n]),
            Err(_) => return false,
        }
    }
    h.finish() == hash
}

/// Sets owner and mode of a just-made entry; with `chown` false (not
/// root) only the mode.
fn apply_acl(p: &Path, acl: &Acl, ids: &IdMap, chown: bool, symlink: bool) -> io::Result<()> {
    if chown {
        lchown(p, Some(ids.uid(acl.user.as_deref())), Some(ids.gid(acl.group.as_deref())))?;
    }
    if !symlink {
        // creation applied the umask; chown cleared set-id bits
        fs::set_permissions(p, fs::Permissions::from_mode(acl.mode & 0o7777))?;
    }
    Ok(())
}

fn integrity(path: &[u8], e: io::Error) -> Error {
    if e.kind() == io::ErrorKind::InvalidData {
        Error::Integrity(format!("{}: {e}", String::from_utf8_lossy(path)))
    } else {
        Error::Io(e)
    }
}

/// Unpacks `job` into `ROOT/Programs/<name>/<version>`. `progress` gets
/// content bytes as they are written. With `hints` (the segment files of
/// the index of installed files), those the package's paths belong to are
/// asked for as soon as the paths are known ([`crate::warm`]).
pub fn unpack(
    root: &Path,
    job: &Job,
    keys: &TrustedKeys,
    ids: &IdMap,
    allow_untrusted: bool,
    hints: Option<&crate::warm::Files>,
    progress: &mut dyn FnMut(u64),
) -> Result<Stats> {
    if !db::safe_component(&job.name) || !db::safe_version(&job.version) {
        return Err(Error::Schema(format!("unsafe package name or version '{}-{}'", job.name, job.version)));
    }
    // Programs and Programs/<name> must be real directories: nothing is
    // created or deleted through a symlink
    let pdir = db::checked_pkg_parent(root, &job.name)?;
    let fin = pdir.join(&job.version);
    let partial = pdir.join(format!("{}{}", job.version, db::PARTIAL_SUFFIX));
    match fs::DirBuilder::new().mode(0o755).create(&pdir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            db::real_dir(&pdir)?;
        }
        Err(e) => return Err(e.into()),
    }
    // a stale .partial, or a stale, never-current copy of this version
    // (the caller never unpacks over the version in use)
    for stale in [&partial, &fin] {
        match fs::symlink_metadata(stale) {
            Ok(m) if m.is_dir() => fs::remove_dir_all(stale)?,
            Ok(_) => fs::remove_file(stale)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    // For root only (0700), and it stays so until the switch has given
    // every entry its final owner (commit::switch, step 4): entries are
    // made, and given their owner and mode, by path in here, here and
    // again in the switch once a script has added users, and a package may
    // hold a directory that its own user can write to. Nobody else can
    // enter the tree before that is done, so nothing in it can be swapped
    // under root's hands. The live tree does not need it open: its files
    // are other names of the same inodes.
    fs::DirBuilder::new().mode(0o700).create(&partial)?;
    let r = unpack_into(&partial, job, keys, ids, allow_untrusted, hints, progress).and_then(|st| {
        fs::rename(&partial, &fin)?;
        Ok(st)
    });
    match r {
        Ok(st) => Ok(st),
        Err(e) => {
            let _ = fs::remove_dir_all(&partial);
            Err(e)
        }
    }
}

fn unpack_into(
    dest: &Path,
    job: &Job,
    keys: &TrustedKeys,
    ids: &IdMap,
    allow_untrusted: bool,
    hints: Option<&crate::warm::Files>,
    progress: &mut dyn FnMut(u64),
) -> Result<Stats> {
    let identity = &job.index_info.hashes;
    let opts = VerifyOptions { allow_untrusted, expected_identity: Some(identity.clone()), require_signature: false };
    let f = File::open(&job.src).map_err(|e| Error::Msg(format!("{}: {e}", job.src.display())))?;
    // the container reads through a buffer of its own
    let mut pr = PackageReader::open(f, keys, &opts)?;
    let pkg = pr.package().clone();
    if pkg.info.name != job.name || pkg.info.version != job.version || pkg.info.arch != job.index_info.arch {
        return Err(Error::Integrity(format!(
            "{}: contains {} ({}) instead of {}-{} ({})",
            job.src.display(),
            pkg.info.nv(),
            pkg.info.arch,
            job.name,
            job.version,
            job.index_info.arch
        )));
    }
    // nothing of a package may be in catalyst's own directories (Programs,
    // var/lib/catalyst, var/cache/catalyst); checked here on the literal paths and
    // again on the resolved ones when the switch places them
    for d in &pkg.dirs {
        let bad = (!d.name.is_empty() && db::is_reserved(&d.name))
            .then(|| d.name.clone())
            .or_else(|| d.files.iter().map(|f| d.file_path(f)).find(|p| db::is_reserved(p)));
        if let Some(p) = bad {
            return Err(Error::Schema(format!(
                "{}: entry /{} is inside catalyst's own directories",
                job.src.display(),
                String::from_utf8_lossy(&p)
            )));
        }
    }
    // the records are what the package says, known before any content is
    // written: the check and the switch will look their paths up in the
    // index of installed files, whose files are asked for now
    let recs = db::records(&pkg);
    if let Some(files) = hints {
        crate::index::ask_for(files, &recs);
    }
    let chown = sys::is_root();
    let root_egid_clean = chown && {
        use std::os::unix::fs::MetadataExt;
        (unsafe { libc::getegid() } == 0)
            && fs::symlink_metadata(dest).map_or(false, |m| m.gid() == 0 && (m.mode() & 0o2000) == 0)
    };
    let mut st = Stats::default();
    let mut made: HashSet<Vec<u8>> = HashSet::new();
    // The directories in which a new entry may come out with another group
    // than root's, so that it needs its owner set even when that is root:
    // those made setgid (what is created in them takes their group), and
    // those given a group other than root (on a file system mounted with
    // grpid or bsdgroups every new entry takes its directory's group). The
    // package's own directory is one of them when it is not plainly root's.
    let mut setgid: HashSet<Vec<u8>> = HashSet::new();
    if chown && !root_egid_clean {
        setgid.insert(Vec::new());
    }
    // a new file is root:root already only when euid==0, egid==0, and neither
    // the package root nor the file's parent directory is one of those
    let needs_chown = |rel: &[u8], uid: u32, gid: u32, setgid: &HashSet<Vec<u8>>| {
        !root_egid_clean || (uid, gid) != (0, 0) || setgid.contains(crate::rootfs::split_rel(rel).0)
    };
    // 64 KiB a write: a larger buffer costs page faults on every package
    let mut buf = vec![0u8; 64 * 1024];
    while let Some(mut e) = pr.next_entry()? {
        let rel = e.path.to_vec();
        let p = path_of(dest, &rel);
        ensure_parent(dest, &rel, &mut made, &mut setgid, chown, chown && !root_egid_clean)?;
        st.entries += 1;
        if e.kind == EntryKind::Dir {
            let acl = &e.dir.acl;
            match fs::DirBuilder::new().mode(0o700).create(&p) {
                Ok(()) => {}
                // made earlier as an unlisted ancestor
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists && made.contains(&rel) => {}
                Err(err) => return Err(err.into()),
            }
            apply_acl(&p, acl, ids, chown, false)?;
            if acl.mode & 0o2000 != 0 || (chown && ids.gid(acl.group.as_deref()) != 0) {
                setgid.insert(rel.clone());
            } else {
                setgid.remove(&rel);
            }
            made.insert(rel);
            continue;
        }
        let fe = e.file.expect("file entry").clone();
        let acl = &fe.acl;
        match &fe.kind {
            FileKind::Regular => {
                let mut out = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&p)?;
                if let Some(d) = e.data() {
                    loop {
                        let n = d.read(&mut buf).map_err(|err| integrity(&rel, err))?;
                        if n == 0 {
                            break;
                        }
                        out.write_all(&buf[..n])?;
                        st.bytes += n as u64;
                        progress(n as u64);
                    }
                }
                let (uid, gid) = (ids.uid(acl.user.as_deref()), ids.gid(acl.group.as_deref()));
                if chown && needs_chown(&rel, uid, gid, &setgid) {
                    fchown(&out, Some(uid), Some(gid))?;
                }
                out.set_permissions(fs::Permissions::from_mode(acl.mode & 0o7777))?;
                out.set_times(mtime(fe.mtime).map_err(|err| integrity(&rel, err))?)?;
            }
            FileKind::Hardlink(t) => {
                fs::hard_link(path_of(dest, t), &p)?;
            }
            FileKind::Symlink(t) => {
                symlink(OsStr::from_bytes(t), &p)?;
                let (uid, gid) = (ids.uid(acl.user.as_deref()), ids.gid(acl.group.as_deref()));
                if chown && needs_chown(&rel, uid, gid, &setgid) {
                    lchown(&p, Some(uid), Some(gid))?;
                }
            }
            FileKind::CharDev(dev) | FileKind::BlockDev(dev) | FileKind::Fifo(dev) => {
                let ty = match fe.kind {
                    FileKind::CharDev(_) => S_IFCHR,
                    FileKind::BlockDev(_) => S_IFBLK,
                    _ => S_IFIFO,
                } as u32;
                sys::make_node(&p, ty | 0o600, *dev)?;
                apply_acl(&p, acl, ids, chown, false)?;
                if let Err(err) = sys::set_mtime_nofollow(&p, fe.mtime) {
                    st.warnings.push(format!("{}: mtime: {err}", String::from_utf8_lossy(&rel)));
                }
            }
        }
        if !acl.xattrs.is_empty() && !matches!(fe.kind, FileKind::Symlink(_)) {
            for (n, v) in &acl.xattrs {
                if let Err(err) = sys::set_xattr(&p, n, v) {
                    st.warnings.push(format!(
                        "{}: xattr {}: {err}",
                        String::from_utf8_lossy(&rel),
                        String::from_utf8_lossy(n)
                    ));
                }
            }
        }
    }
    pr.finish()?;
    let meta = PkgMeta {
        info: crate::adb::PkgInfo {
            hashes: identity.clone(),
            file_size: job.index_info.file_size,
            ..pkg.info.clone()
        },
        triggers: pkg.triggers.clone(),
        replaces_priority: pkg.replaces_priority,
        source: job.source.clone(),
    };
    db::write_meta(dest, &meta, &recs, &pkg)?;
    Ok(st)
}

/// Re-applies owners of entries whose user or group is not `root`, now
/// that `ids` may know them (after a pre-install script added users).
/// Returns how many entries changed.
pub fn fix_owners(base: &Path, recs: &[db::FileRec], ids: &IdMap) -> u64 {
    use std::os::unix::fs::MetadataExt;
    if !sys::is_root() {
        return 0;
    }
    let mut n = 0;
    for r in recs {
        let special = |s: &Option<String>| s.as_deref().map_or(false, |s| s != "root");
        if !special(&r.user) && !special(&r.group) {
            continue;
        }
        let (uid, gid) = (ids.uid(r.user.as_deref()), ids.gid(r.group.as_deref()));
        let p = path_of(base, &r.path);
        let Ok(m) = fs::symlink_metadata(&p) else { continue };
        if m.uid() == uid && m.gid() == gid {
            continue;
        }
        if !m.is_file() && !m.is_dir() {
            // a symlink, device node or FIFO: the owner of the entry itself
            // (chown leaves the mode of these as it is)
            if lchown(&p, Some(uid), Some(gid)).is_ok() {
                n += 1;
            }
            continue;
        }
        // A file or a directory: owner and mode through a descriptor of the
        // entry itself. The switch calls this only while the tree
        // is root's alone (commit::owners_final); the tree of a journal an
        // earlier catalyst wrote can be entered by other users, and one of
        // the package's directories may belong to one of them: a chmod by
        // path would follow a symlink put in the entry's place. A mode with
        // set-id bits is given only to the content the package brought (a
        // file put there instead would get them otherwise).
        let f = if m.is_dir() { sys::open_dir_nofollow(&p) } else { sys::open_regular_nofollow(&p) };
        let Ok(mut f) = f else { continue };
        if m.is_file() && r.mode & 0o6000 != 0 && !content_is(&mut f, &r.hash) {
            continue;
        }
        if fchown(&f, Some(uid), Some(gid)).is_ok() {
            n += 1;
            let _ = f.set_permissions(fs::Permissions::from_mode(r.mode & 0o7777));
        }
    }
    n
}
