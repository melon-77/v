//! Trusted keys and the trust decision for apk v3 files (Part C).
//!
//! A file is trusted when any one of these holds (§C5):
//! - **identity**: the caller knows the package's digest from a signed
//!   index (32-byte SHA-256 or 64-byte SHA-512 of the ADB block payload)
//!   and it matches. A mismatch is fatal, even with `allow_untrusted`.
//! - **signature**: a SIG block verifies: `sign_ver` 0, the key id is that
//!   of a trusted key, `hash_alg` 3/4/5 gives `md = digest(ADB payload)`,
//!   and the signature is RSASSA-PKCS1-v1_5 with SHA-512 over
//!   `M = schema_le32 || sig_payload[0..18] || md`.
//! - `allow_untrusted` (tests only).
//!
//! Key id: the first 16 bytes of SHA-512 over the DER `RSAPublicKey`
//! (PKCS#1) of the key (§C3).

use std::fs;
use std::io::Read;
use std::path::Path;

use openssl::hash::MessageDigest;
use openssl::pkey::{PKey, Private, Public};
use openssl::rsa::Rsa;
use openssl::sign::{Signer, Verifier};

use crate::adb::{self, Compression, EntryKind, SCHEMA_INDEX, SCHEMA_PACKAGE};
use crate::digest::{self, Alg};
use crate::error::{Error, Result};

/// `hash_alg` values accepted in SIG blocks (§C1.3).
pub const HASH_SHA256: u8 = 3;
pub const HASH_SHA512: u8 = 4;
pub const HASH_SHA256_160: u8 = 5;

/// A trusted public key.
pub struct TrustedKey {
    /// The file name it was loaded from (informational; v3 matching is by
    /// key id only).
    pub name: String,
    pub id: [u8; 16],
    pub bits: u32,
    key: PKey<Public>,
}

impl std::fmt::Debug for TrustedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TrustedKey({}, {}, {} bits)", self.name, digest::hex(&self.id), self.bits)
    }
}

/// The set of trusted keys.
#[derive(Debug, Default)]
pub struct TrustedKeys {
    keys: Vec<TrustedKey>,
    /// Files that were skipped while loading a directory, with the reason.
    pub skipped: Vec<(String, String)>,
}

/// The key id of an RSA public key (§C3).
pub fn key_id(key: &PKey<Public>) -> Result<[u8; 16]> {
    let rsa = key
        .rsa()
        .map_err(|_| Error::Key("only RSA keys are supported".into()))?;
    let der = rsa.public_key_to_der_pkcs1()?;
    let mut id = [0u8; 16];
    id.copy_from_slice(&digest::sha512(&der)[..16]);
    Ok(id)
}

fn parse_public_pem(pem: &[u8]) -> Result<PKey<Public>> {
    if let Ok(k) = PKey::public_key_from_pem(pem) {
        return Ok(k);
    }
    if let Ok(r) = Rsa::public_key_from_pem_pkcs1(pem) {
        return Ok(PKey::from_rsa(r)?);
    }
    Err(Error::Key("not a PEM public key".into()))
}

impl TrustedKeys {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a PEM public key (`PUBLIC KEY` or `RSA PUBLIC KEY`).
    pub fn add_pem(&mut self, name: &str, pem: &[u8]) -> Result<&TrustedKey> {
        let key = parse_public_pem(pem)?;
        self.add_key(name, key)
    }

    /// Adds a public key (RSA only).
    pub fn add_key(&mut self, name: &str, key: PKey<Public>) -> Result<&TrustedKey> {
        let id = key_id(&key)?;
        let bits = key.bits();
        self.keys.push(TrustedKey { name: name.to_string(), id, bits, key });
        Ok(self.keys.last().unwrap())
    }

    /// Loads every non-hidden file of `dir` in name order. Files that are
    /// not RSA public keys are skipped and listed in `skipped` (apk ignores
    /// them silently, §C4.2). A directory that cannot be read is an error.
    pub fn load_dir(&mut self, dir: &Path) -> Result<()> {
        let mut names: Vec<String> = Vec::new();
        for e in fs::read_dir(dir).map_err(|e| Error::Key(format!("{}: {e}", dir.display())))? {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            names.push(name);
        }
        names.sort();
        for name in names {
            let p = dir.join(&name);
            let res = fs::metadata(&p)
                .map_err(Error::from)
                .and_then(|m| {
                    if m.is_file() && m.len() <= 64000 {
                        Ok(())
                    } else {
                        Err(Error::Key("not a regular file of at most 64000 bytes".into()))
                    }
                })
                .and_then(|_| Ok(fs::read(&p)?))
                .and_then(|pem| self.add_pem(&name, &pem).map(|_| ()));
            if let Err(e) = res {
                self.skipped.push((name, e.to_string()));
            }
        }
        Ok(())
    }

    /// Keys from one directory (`--keys-dir`).
    pub fn from_dir(dir: &Path) -> Result<Self> {
        let mut k = Self::new();
        k.load_dir(dir)?;
        Ok(k)
    }

    /// Keys of a root: `ROOT/etc/apk/keys`, then `ROOT/lib/apk/keys`; a
    /// file name seen in the first hides the same name in the second, and
    /// missing directories are skipped (§C4.1). Paths are resolved inside
    /// the root (a symlink to `/usr/share/...` means the root's, never the
    /// host's), and only regular files of at most 64000 bytes are read.
    pub fn from_root(root: &Path) -> Result<Self> {
        let mut k = Self::new();
        let mut seen = std::collections::HashSet::new();
        let rfs = crate::rootfs::RootFs::new(root);
        for sub in ["etc/apk/keys", "lib/apk/keys"] {
            let (dir, list) = match rfs.list_dir(sub.as_bytes()) {
                Ok(Some(x)) => x,
                _ => continue,
            };
            let mut names: Vec<Vec<u8>> = Vec::new();
            for name in list {
                if !name.starts_with(b".") && seen.insert(name.clone()) {
                    names.push(name);
                }
            }
            names.sort();
            for name in names {
                let shown = String::from_utf8_lossy(&name).into_owned();
                let rel = crate::rootfs::join_rel(&dir, &name);
                let res = match rfs.read_file(&rel, 64000) {
                    Ok(Some(pem)) => k.add_pem(&shown, &pem).map(|_| ()),
                    Ok(None) => Err(Error::Key("vanished".into())),
                    Err(e) => Err(Error::Key(e.to_string())),
                };
                if let Err(e) = res {
                    k.skipped.push((shown, e.to_string()));
                }
            }
        }
        Ok(k)
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &TrustedKey> {
        self.keys.iter()
    }
}

/// How a file is to be verified.
#[derive(Debug, Clone, Default)]
pub struct VerifyOptions {
    /// Accept files that are unsigned or not signed by a trusted key. For
    /// tests only. An identity mismatch is still fatal.
    pub allow_untrusted: bool,
    /// The package's `hashes` from a signed index (packages only). 32 or 64
    /// bytes are checked against the ADB payload and a match makes the file
    /// trusted; 20 bytes cannot be checked and are ignored (§D10.5).
    pub expected_identity: Option<Vec<u8>>,
    /// Also require a valid signature when the identity matched (apk does
    /// not).
    pub require_signature: bool,
}

/// Who signed a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignerInfo {
    pub key_name: String,
    pub key_id: [u8; 16],
    /// `hash_alg` of the SIG block (4 = SHA-512 is what apk writes).
    pub hash_alg: u8,
    /// 0-based index of the SIG block that verified.
    pub sig_index: usize,
}

/// Why a file is trusted.
#[derive(Debug, Clone, Default)]
pub struct TrustInfo {
    /// The first SIG block that verified, if any.
    pub signer: Option<SignerInfo>,
    /// The identity digest algorithm, when the identity was checked and
    /// matched.
    pub identity: Option<Alg>,
    /// Trusted only because `allow_untrusted` was set.
    pub allowed_untrusted: bool,
    /// Number of SIG blocks in the file.
    pub sig_blocks: usize,
    /// Why SIG blocks before the verifying one (or all) did not verify.
    pub sig_errors: Vec<String>,
}

impl TrustInfo {
    /// A one-line description ("signed by NAME (key ID)", ...).
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(s) = &self.signer {
            parts.push(format!("signed by {} (key {})", s.key_name, digest::hex(&s.key_id)));
        }
        if let Some(a) = self.identity {
            parts.push(format!("identity {} matches", a.name()));
        }
        if self.allowed_untrusted {
            parts.push("UNTRUSTED (allowed)".into());
        }
        parts.join(", ")
    }
}

struct DigestCache<'a> {
    adb: &'a [u8],
    s256: Option<[u8; 32]>,
    s512: Option<[u8; 64]>,
}

impl<'a> DigestCache<'a> {
    fn new(adb: &'a [u8]) -> Self {
        DigestCache { adb, s256: None, s512: None }
    }
    fn sha256(&mut self) -> &[u8; 32] {
        let adb = self.adb;
        self.s256.get_or_insert_with(|| digest::sha256(adb))
    }
    fn sha512(&mut self) -> &[u8; 64] {
        let adb = self.adb;
        self.s512.get_or_insert_with(|| digest::sha512(adb))
    }
    /// `md` for a SIG block's `hash_alg` (`adb_digest_adb`, §C2.1).
    fn md(&mut self, hash_alg: u8) -> Option<Vec<u8>> {
        match hash_alg {
            HASH_SHA256 => Some(self.sha256().to_vec()),
            HASH_SHA512 => Some(self.sha512().to_vec()),
            HASH_SHA256_160 => Some(self.sha256()[..20].to_vec()),
            _ => None,
        }
    }
}

/// The message a SIG block signs (§C2.2).
fn signed_message(schema: u32, sig_hdr: &[u8], md: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(4 + 18 + md.len());
    m.extend_from_slice(&schema.to_le_bytes());
    m.extend_from_slice(&sig_hdr[..18]);
    m.extend_from_slice(md);
    m
}

fn rsa_verify(key: &PKey<Public>, msg: &[u8], sig: &[u8]) -> bool {
    if sig.len() != key.size() {
        return false;
    }
    let mut v = match Verifier::new(MessageDigest::sha512(), key) {
        Ok(v) => v,
        Err(_) => return false,
    };
    v.update(msg).is_ok() && v.verify(sig).unwrap_or(false)
}

/// Checks one SIG block payload (`adb_trust_verify_signature`, §C5.2).
/// Returns the key that verified it, or why it does not.
fn verify_sig_block<'k>(
    keys: &'k TrustedKeys,
    schema: u32,
    cache: &mut DigestCache,
    p: &[u8],
) -> std::result::Result<&'k TrustedKey, String> {
    if p.len() < 2 {
        return Err("SIG payload too short".into());
    }
    if p[0] != 0 {
        return Err(format!("unsupported signature version {}", p[0]));
    }
    if p.len() < 18 {
        return Err("SIG payload too short".into());
    }
    let id = &p[2..18];
    let mut matched = false;
    for k in keys.keys.iter().filter(|k| k.id == id) {
        matched = true;
        let md = match cache.md(p[1]) {
            Some(md) => md,
            None => return Err(format!("unsupported hash algorithm {}", p[1])),
        };
        if rsa_verify(&k.key, &signed_message(schema, p, &md), &p[18..]) {
            return Ok(k);
        }
    }
    if matched {
        Err("bad signature".into())
    } else {
        Err(format!("key {} is not trusted", digest::hex(id)))
    }
}

/// Decides whether a file is trusted, from its schema (the LE u32 of the
/// file header), its ADB block payload and its SIG block payloads.
///
/// All SIG blocks are tried in order until one verifies, also when the
/// identity already matched, so that [`TrustInfo::signer`] tells who signed
/// the file.
pub fn check_trust(
    keys: &TrustedKeys,
    schema: u32,
    adb: &[u8],
    sigs: &[Vec<u8>],
    opts: &VerifyOptions,
) -> Result<TrustInfo> {
    let mut cache = DigestCache::new(adb);
    let mut info = TrustInfo { sig_blocks: sigs.len(), ..Default::default() };
    if let Some(id) = &opts.expected_identity {
        let (alg, got): (Option<Alg>, Vec<u8>) = match id.len() {
            32 => (Some(Alg::Sha256), cache.sha256().to_vec()),
            64 => (Some(Alg::Sha512), cache.sha512().to_vec()),
            20 => (None, Vec::new()),
            n => return Err(Error::Integrity(format!("unusable identity of {n} bytes"))),
        };
        if let Some(alg) = alg {
            if &got != id {
                return Err(Error::Integrity(format!(
                    "package identity mismatch: index has {}, file has {}",
                    digest::hex(id),
                    digest::hex(&got)
                )));
            }
            info.identity = Some(alg);
        }
    }
    for (i, s) in sigs.iter().enumerate() {
        match verify_sig_block(keys, schema, &mut cache, s) {
            Ok(k) => {
                info.signer = Some(SignerInfo { key_name: k.name.clone(), key_id: k.id, hash_alg: s[1], sig_index: i });
                break;
            }
            Err(e) => info.sig_errors.push(format!("SIG block {}: {e}", i + 1)),
        }
    }
    if info.signer.is_none() && (info.identity.is_none() || opts.require_signature) {
        if opts.allow_untrusted {
            info.allowed_untrusted = true;
        } else if sigs.is_empty() {
            return Err(Error::Untrusted("the file is not signed".into()));
        } else if keys.is_empty() {
            return Err(Error::Untrusted("no trusted keys are loaded".into()));
        } else {
            return Err(Error::Untrusted(format!("no valid signature by a trusted key ({})", info.sig_errors.join("; "))));
        }
    }
    Ok(info)
}

/// Makes a SIG block payload for an ADB payload, as apk's writer does
/// (`sign_ver` 0, `hash_alg` 4). For tests and tools.
pub fn sign_adb(key: &PKey<Private>, schema: u32, adb: &[u8]) -> Result<Vec<u8>> {
    let pubkey = PKey::public_key_from_der(&key.public_key_to_der()?)?;
    let id = key_id(&pubkey)?;
    let mut hdr = vec![0u8, HASH_SHA512];
    hdr.extend_from_slice(&id);
    let m = signed_message(schema, &hdr, &digest::sha512(adb));
    let mut s = Signer::new(MessageDigest::sha512(), key)?;
    s.update(&m)?;
    hdr.extend(s.sign_to_vec()?);
    Ok(hdr)
}

/// What [`verify_file`] found.
#[derive(Debug, Clone)]
pub enum Verified {
    Index {
        description: String,
        packages: usize,
        compression: Compression,
        trust: TrustInfo,
    },
    Package {
        name: String,
        version: String,
        arch: String,
        dirs: usize,
        files: usize,
        data_blocks: u64,
        data_bytes: u64,
        /// SHA-256 of the ADB payload: what an index stores as `hashes`.
        identity: [u8; 32],
        compression: Compression,
        trust: TrustInfo,
    },
}

/// Verifies an index (signature, structure) or a package (signature or
/// identity, structure, and every file's content against its hash).
pub fn verify_reader<R: Read>(reader: R, keys: &TrustedKeys, opts: &VerifyOptions) -> Result<Verified> {
    let head = adb::read_head(reader)?;
    let compression = head.container.compression();
    match head.container.schema() {
        SCHEMA_INDEX => {
            let (idx, trust) = adb::index_from_head(head, keys, opts)?;
            Ok(Verified::Index { description: idx.description, packages: idx.packages.len(), compression, trust })
        }
        SCHEMA_PACKAGE => {
            let mut pr = adb::PackageReader::from_head(head, keys, opts)?;
            let identity = digest::sha256(pr.adb().bytes());
            let compression = pr.compression();
            let (mut dirs, mut files) = (0usize, 0usize);
            let mut sink = vec![0u8; 128 * 1024];
            while let Some(mut e) = pr.next_entry()? {
                match e.kind {
                    EntryKind::Dir => dirs += 1,
                    EntryKind::File => files += 1,
                }
                if let Some(d) = e.data() {
                    loop {
                        let n = d.read(&mut sink).map_err(|err| Error::Integrity(err.to_string()))?;
                        if n == 0 {
                            break;
                        }
                    }
                }
            }
            let data_blocks = pr.data_blocks();
            let p = pr.package();
            let (name, version, arch, data_bytes) =
                (p.info.name.clone(), p.info.version.clone(), p.info.arch.clone(), p.data_bytes());
            let trust = pr.finish()?;
            Ok(Verified::Package { name, version, arch, dirs, files, data_blocks, data_bytes, identity, compression, trust })
        }
        s => Err(Error::Schema(format!("unsupported schema '{}'", adb::schema_name(s)))),
    }
}

/// Verifies a file of either schema; see [`verify_reader`].
pub fn verify_file(path: &Path, keys: &TrustedKeys, opts: &VerifyOptions) -> Result<Verified> {
    verify_reader(fs::File::open(path)?, keys, opts)
}

/// Verifies an index file; returns the index.
pub fn verify_index_file(path: &Path, keys: &TrustedKeys) -> Result<(adb::Index, TrustInfo)> {
    adb::read_index_file(path, keys, &VerifyOptions::default())
}

/// Verifies a package file completely (signature, or identity when
/// `expected_identity` is given, and all contents).
pub fn verify_package_file(path: &Path, keys: &TrustedKeys, expected_identity: Option<&[u8]>) -> Result<Verified> {
    let opts = VerifyOptions { expected_identity: expected_identity.map(|i| i.to_vec()), ..Default::default() };
    match verify_reader(fs::File::open(path)?, keys, &opts)? {
        v @ Verified::Package { .. } => Ok(v),
        _ => Err(Error::Schema("not a package".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> PKey<Private> {
        PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap()
    }

    fn public(k: &PKey<Private>) -> PKey<Public> {
        PKey::public_key_from_der(&k.public_key_to_der().unwrap()).unwrap()
    }

    #[test]
    fn key_id_is_sha512_of_pkcs1() {
        let k = test_key();
        let p = public(&k);
        let der = p.rsa().unwrap().public_key_to_der_pkcs1().unwrap();
        assert_eq!(key_id(&p).unwrap()[..], digest::sha512(&der)[..16]);
        // PEM in both forms gives the same id
        let mut keys = TrustedKeys::new();
        let spki = p.public_key_to_pem().unwrap();
        let pkcs1 = p.rsa().unwrap().public_key_to_pem_pkcs1().unwrap();
        let a = keys.add_pem("a", &spki).unwrap().id;
        let b = keys.add_pem("b", &pkcs1).unwrap().id;
        assert_eq!(a, b);
        assert!(keys.add_pem("c", b"garbage").is_err());
    }

    #[test]
    fn sign_and_check() {
        let k = test_key();
        let other = test_key();
        let mut keys = TrustedKeys::new();
        keys.add_key("test.pub", public(&k)).unwrap();
        let adb = b"\0\0\0\0\0\0\0\0some adb payload".to_vec();
        let sig = sign_adb(&k, adb::SCHEMA_INDEX, &adb).unwrap();
        let opts = VerifyOptions::default();
        let t = check_trust(&keys, adb::SCHEMA_INDEX, &adb, &[sig.clone()], &opts).unwrap();
        assert_eq!(t.signer.as_ref().unwrap().key_name, "test.pub");
        // wrong schema
        assert!(check_trust(&keys, adb::SCHEMA_PACKAGE, &adb, &[sig.clone()], &opts).is_err());
        // changed payload
        let mut bad = adb.clone();
        bad[9] ^= 1;
        assert_eq!(check_trust(&keys, adb::SCHEMA_INDEX, &bad, &[sig.clone()], &opts).unwrap_err().kind(), "untrusted");
        // signature byte flipped
        let mut s2 = sig.clone();
        let l = s2.len();
        s2[l - 1] ^= 0x80;
        assert!(check_trust(&keys, adb::SCHEMA_INDEX, &adb, &[s2.clone()], &opts).is_err());
        // a bad SIG block followed by a good one is fine
        let t = check_trust(&keys, adb::SCHEMA_INDEX, &adb, &[s2, sig.clone()], &opts).unwrap();
        assert_eq!(t.signer.unwrap().sig_index, 1);
        assert_eq!(t.sig_errors.len(), 1);
        // hash_alg is covered by the signature
        let mut s3 = sig.clone();
        s3[1] = HASH_SHA256;
        assert!(check_trust(&keys, adb::SCHEMA_INDEX, &adb, &[s3], &opts).is_err());
        // untrusted signer
        let sig_other = sign_adb(&other, adb::SCHEMA_INDEX, &adb).unwrap();
        let e = check_trust(&keys, adb::SCHEMA_INDEX, &adb, &[sig_other.clone()], &opts).unwrap_err();
        assert_eq!(e.kind(), "untrusted");
        // unsigned
        assert!(check_trust(&keys, adb::SCHEMA_INDEX, &adb, &[], &opts).is_err());
        // allow_untrusted
        let au = VerifyOptions { allow_untrusted: true, ..Default::default() };
        assert!(check_trust(&keys, adb::SCHEMA_INDEX, &adb, &[sig_other], &au).unwrap().allowed_untrusted);
    }

    #[test]
    fn identity() {
        let keys = TrustedKeys::new();
        let adb = b"\0\0\0\0\0\0\0\0payload payload".to_vec();
        let good = digest::sha256(&adb).to_vec();
        let opts = VerifyOptions { expected_identity: Some(good.clone()), ..Default::default() };
        let t = check_trust(&keys, adb::SCHEMA_PACKAGE, &adb, &[], &opts).unwrap();
        assert_eq!(t.identity, Some(Alg::Sha256));
        let mut bad = good.clone();
        bad[0] ^= 1;
        let opts_bad = VerifyOptions { expected_identity: Some(bad), allow_untrusted: true, ..Default::default() };
        assert_eq!(check_trust(&keys, adb::SCHEMA_PACKAGE, &adb, &[], &opts_bad).unwrap_err().kind(), "integrity");
        let s512 = VerifyOptions { expected_identity: Some(digest::sha512(&adb).to_vec()), ..Default::default() };
        assert_eq!(check_trust(&keys, adb::SCHEMA_PACKAGE, &adb, &[], &s512).unwrap().identity, Some(Alg::Sha512));
        // 20 bytes: cannot be checked, so it does not establish trust
        let s20 = VerifyOptions { expected_identity: Some(good[..20].to_vec()), ..Default::default() };
        assert!(check_trust(&keys, adb::SCHEMA_PACKAGE, &adb, &[], &s20).is_err());
        // identity plus required signature
        let rs = VerifyOptions { expected_identity: Some(good), require_signature: true, ..Default::default() };
        assert!(check_trust(&keys, adb::SCHEMA_PACKAGE, &adb, &[], &rs).is_err());
    }
}
