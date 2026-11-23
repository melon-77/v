//! Getting package files: local repositories are read in place, remote
//! ones (http/https) are downloaded into `ROOT/var/cache/catalyst/` as
//! `<name>-<version>.<first 8 hex digits of the identity>.apk` (§G1.7).
//!
//! A download goes to `<file>.part` and is renamed when its size equals
//! the index's `file-size` and the identity (SHA-256 of the ADB block)
//! matches the index. The contents are verified again, file by file, when
//! the package is unpacked.
//!
//! What a mirror can make catalyst do is bounded: a package download stops as
//! soon as it exceeds the size the signed index gives (a `Content-Length`
//! that differs is refused before anything is read), an index download
//! stops at [`MAX_INDEX_FILE`], a transfer slower than [`MIN_RATE`] bytes/s
//! over [`RATE_WINDOW`] is abandoned, and an `https://` repository is only
//! ever fetched over https (no redirect to plain http is followed).

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::adb::{self, PkgInfo};
use crate::digest;
use crate::error::{Error, Result};
use crate::repo::{self, Repo};
use crate::verify::{self, TrustedKeys, VerifyOptions};

/// Largest index file accepted from a mirror.
pub const MAX_INDEX_FILE: u64 = 64 << 20;
/// Largest package file when the index gives no size.
pub const MAX_PACKAGE_FILE: u64 = 16 << 30;
/// Slowest transfer kept going: bytes per second, averaged over
/// [`RATE_WINDOW`].
pub const MIN_RATE: u64 = 1024;
pub const RATE_WINDOW: Duration = Duration::from_secs(30);

/// How large a download may be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    /// Exactly this many bytes (a package whose index gives its size).
    Exact(u64),
    /// At most this many bytes.
    Max(u64),
}

impl Limit {
    fn max(self) -> u64 {
        match self {
            Limit::Exact(n) | Limit::Max(n) => n,
        }
    }
}

/// A blocking HTTP(S) client, shared by the fetch threads.
#[derive(Clone)]
pub struct Http {
    /// for http:// URLs
    plain: ureq::Agent,
    /// for https:// URLs: refuses to follow a redirect to http://
    secure: ureq::Agent,
}

impl Http {
    pub fn new() -> Result<Http> {
        let tls = Arc::new(native_tls::TlsConnector::new().map_err(|e| Error::Msg(format!("TLS setup: {e}")))?);
        let base = || {
            ureq::AgentBuilder::new()
                .tls_connector(tls.clone())
                .timeout_connect(Duration::from_secs(20))
                .timeout_read(RATE_WINDOW)
                .redirects(5)
                .user_agent(concat!("catalyst/", env!("CARGO_PKG_VERSION")))
        };
        Ok(Http { plain: base().build(), secure: base().https_only(true).build() })
    }

    fn agent(&self, url: &str) -> &ureq::Agent {
        if url.starts_with("https://") {
            &self.secure
        } else {
            &self.plain
        }
    }

    /// Downloads `url` into `dest` (through `dest.part`, renamed at the
    /// end), within `limit`. `progress` gets the number of bytes of each
    /// chunk.
    pub fn download(&self, url: &str, dest: &Path, limit: Limit, progress: &mut dyn FnMut(u64)) -> Result<u64> {
        let part = part_name(dest);
        let r = self.download_to(url, &part, limit, progress);
        if r.is_err() {
            let _ = fs::remove_file(&part);
            return r;
        }
        fs::rename(&part, dest)?;
        r
    }

    fn download_to(&self, url: &str, part: &Path, limit: Limit, progress: &mut dyn FnMut(u64)) -> Result<u64> {
        let resp = self.agent(url).get(url).call().map_err(|e| Error::Msg(format!("{url}: {e}")))?;
        if let Some(cl) = resp.header("Content-Length").and_then(|v| v.trim().parse::<u64>().ok()) {
            match limit {
                Limit::Exact(n) if cl != n => {
                    return Err(Error::Integrity(format!("{url}: the server sends {cl} bytes, the index says {n}")))
                }
                Limit::Max(n) if cl > n => {
                    return Err(Error::Msg(format!("{url}: {cl} bytes is more than the {n} accepted")))
                }
                _ => {}
            }
        }
        let mut r = resp.into_reader();
        let mut f = OpenOptions::new().write(true).create(true).truncate(true).open(part)?;
        let mut buf = vec![0u8; 256 * 1024];
        let mut total = 0u64;
        let mut window = (Instant::now(), 0u64);
        loop {
            let n = match r.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(Error::Msg(format!("{url}: {e}"))),
            };
            total += n as u64;
            if total > limit.max() {
                return Err(Error::Integrity(format!("{url}: more than the {} bytes expected", limit.max())));
            }
            f.write_all(&buf[..n])?;
            progress(n as u64);
            window.1 += n as u64;
            let el = window.0.elapsed();
            if el >= RATE_WINDOW {
                if window.1 < MIN_RATE * el.as_secs() {
                    return Err(Error::Msg(format!(
                        "{url}: too slow ({} bytes in {}s); giving up",
                        window.1,
                        el.as_secs()
                    )));
                }
                window = (Instant::now(), 0);
            }
        }
        if let Limit::Exact(n) = limit {
            if total != n {
                return Err(Error::Integrity(format!("{url}: got {total} bytes, the index says {n}")));
            }
        }
        f.sync_all()?;
        Ok(total)
    }
}

pub fn part_name(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(format!(".{}.part", std::process::id()));
    PathBuf::from(s)
}

/// The cache file name of a package (`${name}-${version}.${hash:8}.apk`).
pub fn cache_name(p: &PkgInfo) -> String {
    let h = digest::hex(&p.hashes);
    format!("{}-{}.{}.apk", p.name, p.version, &h[..h.len().min(8)])
}

/// Checks a package file's size and identity against its index entry
/// (reads only the metadata part of the file; the identity is compared
/// before any SIG block is read).
pub fn check_identity(path: &Path, p: &PkgInfo, keys: &TrustedKeys, allow_untrusted: bool) -> Result<()> {
    let md = fs::symlink_metadata(path)?;
    if !md.is_file() {
        return Err(Error::Integrity(format!("{}: not a regular file", path.display())));
    }
    if p.file_size != 0 && md.len() != p.file_size {
        return Err(Error::Integrity(format!(
            "{}: size {} differs from the index ({})",
            path.display(),
            md.len(),
            p.file_size
        )));
    }
    let head = adb::read_head_expecting(BufReader::new(File::open(path)?), Some(&p.hashes))?;
    let opts = VerifyOptions { allow_untrusted, expected_identity: Some(p.hashes.clone()), require_signature: false };
    verify::check_trust(keys, head.container.schema(), head.adb.bytes(), &head.sigs, &opts)?;
    Ok(())
}

/// Where the package file is (or will be) on disk, and whether it must be
/// downloaded first.
pub fn source_path(repo: &Repo, p: &PkgInfo, cache_dir: &Path) -> Result<(PathBuf, Option<String>)> {
    let url = repo.package_url(p)?;
    if repo::is_remote(&url) {
        Ok((cache_dir.join(cache_name(p)), Some(url)))
    } else {
        Ok((PathBuf::from(repo::local_path(&url)), None))
    }
}

/// Makes sure a remote package is in the cache and matches its index
/// entry. A cached copy is reused when it matches.
pub fn fetch(
    http: &Http,
    url: &str,
    dest: &Path,
    p: &PkgInfo,
    keys: &TrustedKeys,
    allow_untrusted: bool,
    progress: &mut dyn FnMut(u64),
) -> Result<()> {
    if dest.exists() && check_identity(dest, p, keys, allow_untrusted).is_ok() {
        progress(p.file_size);
        return Ok(());
    }
    let limit = if p.file_size != 0 { Limit::Exact(p.file_size) } else { Limit::Max(MAX_PACKAGE_FILE) };
    http.download(url, dest, limit, progress)?;
    if let Err(e) = check_identity(dest, p, keys, allow_untrusted) {
        let _ = fs::remove_file(dest);
        return Err(e);
    }
    Ok(())
}
