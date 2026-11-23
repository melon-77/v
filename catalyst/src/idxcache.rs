//! Repository indexes already verified and parsed. An index whose bytes,
//! location and trusted keys are those of an earlier run is taken from
//! here instead of being decompressed, checked against its signature and
//! parsed again, which is most of what reading an index costs (on the
//! Valence repository, 1144 packages: about 3 ms of a 30 ms `add`).
//!
//! One file per repository location, `var/cache/catalyst/parsed-<16 hex>`:
//!
//! ```text
//! "catalyst-parsed-index 1\n"
//! key     32 bytes  SHA-256 of the index file, its location, the trusted
//!                   keys (names, key ids, sizes) and the verify options
//! check    8 bytes  checksum of the body: a torn or damaged file is not used
//! length   8 bytes  of the body
//! body              the trust decision, the SHA-256 of the index's
//!                   freshness record, the index ([`crate::codec`])
//! ```
//!
//! Nothing in a file is used unless its key matches, i.e. the very same
//! index was verified with the very same keys and options before; a file
//! that does not match in every respect is ignored and replaced. Like the
//! rest of the root it is trusted as far as the root is: only root can
//! write it. The freshness check (rollback protection) is not cached: it
//! runs on every load, against the record of the index accepted last.

use std::fs;
use std::path::{Path, PathBuf};

use openssl::sha::Sha256;

use crate::adb::Index;
use crate::codec::{self, Rd};
use crate::verify::{TrustInfo, TrustedKeys, VerifyOptions};

const MAGIC: &[u8] = b"catalyst-parsed-index 1\n";
const HEAD: usize = MAGIC.len() + 32 + 8 + 8;

/// What a cache file gives back.
pub struct Cached {
    pub index: Index,
    pub trust: TrustInfo,
    /// The SHA-256 of the index's freshness record (`repo::Seen`).
    pub offer: [u8; 32],
}

/// The cache file of the repository whose location has the key `url_key`
/// (64 hex digits).
pub fn path(cache_dir: &Path, url_key: &str) -> PathBuf {
    cache_dir.join(format!("parsed-{}", &url_key[..16]))
}

/// What a cached result depends on.
pub fn key(file: &[u8], index_url: &str, keys: &TrustedKeys, opts: &VerifyOptions) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(MAGIC);
    let fp = keys.fingerprint();
    for part in [file, index_url.as_bytes(), &fp[..]] {
        h.update(&(part.len() as u64).to_le_bytes());
        h.update(part);
    }
    h.update(&[opts.allow_untrusted as u8, opts.require_signature as u8, opts.expected_identity.is_some() as u8]);
    h.finish()
}

/// The cached result for `key`, if the file has it intact.
pub fn load(path: &Path, key: &[u8; 32]) -> Option<Cached> {
    let b = fs::read(path).ok()?;
    let m = MAGIC.len();
    if b.len() < HEAD || &b[..m] != MAGIC || b[m..m + 32] != key[..] {
        return None;
    }
    let check = u64::from_le_bytes(b[m + 32..m + 40].try_into().ok()?);
    let len = u64::from_le_bytes(b[m + 40..HEAD].try_into().ok()?);
    let body = &b[HEAD..];
    if body.len() as u64 != len || check64(body) != check {
        return None;
    }
    let mut r = Rd::new(body);
    let trust = r.trust()?;
    let offer: [u8; 32] = r.take(32)?.try_into().ok()?;
    let index = r.index()?;
    if !r.at_end() {
        return None;
    }
    Some(Cached { index, trust, offer })
}

/// Writes the cache file (best effort: a cache that cannot be written is
/// not an error). Written under a temporary name and renamed, without an
/// fsync: a file a crash damages fails its checksum and is replaced.
pub fn store(path: &Path, key: &[u8; 32], index: &Index, trust: &TrustInfo, offer: &[u8; 32]) {
    let mut o = Vec::with_capacity(1 << 20);
    o.extend_from_slice(MAGIC);
    o.extend_from_slice(key);
    o.extend_from_slice(&[0; 16]);
    codec::put_trust(&mut o, trust);
    o.extend_from_slice(offer);
    codec::put_index(&mut o, index);
    let check = check64(&o[HEAD..]);
    let len = (o.len() - HEAD) as u64;
    let m = MAGIC.len();
    o[m + 32..m + 40].copy_from_slice(&check.to_le_bytes());
    o[m + 40..HEAD].copy_from_slice(&len.to_le_bytes());
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(format!(".{}.new", std::process::id()));
    let tmp = PathBuf::from(tmp);
    if fs::write(&tmp, &o).and_then(|_| fs::rename(&tmp, path)).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

/// A fast checksum (64 bits, eight bytes a step); it detects damage, it is
/// not meant to resist a forger (who could write the root anyway).
fn check64(b: &[u8]) -> u64 {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let mut h: u64 = 0x9e37_79b9_7f4a_7c15 ^ b.len() as u64;
    let mut it = b.chunks_exact(8);
    for c in &mut it {
        h = (h.rotate_left(5) ^ u64::from_le_bytes(c.try_into().unwrap())).wrapping_mul(K);
    }
    for &x in it.remainder() {
        h = (h.rotate_left(5) ^ x as u64).wrapping_mul(K);
    }
    h ^ (h >> 29)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adb::{Dependency, PkgInfo};
    use crate::verify::SignerInfo;

    fn sample() -> (Index, TrustInfo) {
        let dep = |n: &str, v: &str, op: u8| Dependency { name: n.into(), version: v.into(), op };
        let p = |n: &str, v: &str| PkgInfo {
            name: n.into(),
            version: v.into(),
            hashes: (0..32).collect(),
            description: format!("the {n} package, ünïcode"),
            arch: "x86_64".into(),
            license: "MIT".into(),
            origin: n.into(),
            maintainer: "someone <a@b>".into(),
            url: "https://example.org".into(),
            repo_commit: vec![7; 20],
            build_time: 1_700_000_000,
            installed_size: 123_456,
            file_size: 1 << 40,
            provider_priority: 0,
            depends: vec![dep("so:libc.so.6", "", 7), dep("zlib", "1.3", 2)],
            provides: vec![dep(&format!("cmd:{n}"), v, 1)],
            replaces: vec![],
            install_if: vec![dep("x", "", 7 | 16)],
            recommends: vec![],
            layer: 1,
            tags: vec!["a".into(), "".into()],
        };
        let index = Index {
            description: "test".into(),
            packages: vec![p("nano", "8.2-r0"), p("zlib", "1.3.1-r1"), PkgInfo::default()],
            pkgname_spec: Some("${name}-${version}.apk".into()),
            skipped: vec!["one entry".into()],
        };
        let trust = TrustInfo {
            signer: Some(SignerInfo { key_name: "k.rsa.pub".into(), key_id: [9; 16], hash_alg: 4, sig_index: 0 }),
            identity: None,
            allowed_untrusted: false,
            sig_blocks: 2,
            sig_errors: vec!["first: unknown key".into()],
        };
        (index, trust)
    }

    #[test]
    fn a_stored_index_comes_back_equal_and_only_for_its_key() {
        let dir = std::env::temp_dir().join(format!("catalyst-idxcache-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let (index, trust) = sample();
        let f = path(&dir, &"ab".repeat(32));
        let key = [5u8; 32];
        store(&f, &key, &index, &trust, &[3; 32]);
        let c = load(&f, &key).expect("cached");
        assert_eq!(c.index, index);
        assert_eq!(c.offer, [3; 32]);
        assert_eq!(format!("{:?}", c.trust), format!("{trust:?}"));
        assert!(load(&f, &[6; 32]).is_none(), "another key");
        // damage anywhere, or a torn end, and the file is not used
        let good = fs::read(&f).unwrap();
        for at in [HEAD + 1, good.len() / 2, good.len() - 1] {
            let mut b = good.clone();
            b[at] ^= 0x10;
            fs::write(&f, &b).unwrap();
            assert!(load(&f, &key).is_none(), "damaged at {at}");
        }
        fs::write(&f, &good[..good.len() - 3]).unwrap();
        assert!(load(&f, &key).is_none(), "torn");
        // nothing is left under a temporary name
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_key_covers_the_file_the_location_the_keys_and_the_options() {
        let keys = TrustedKeys::new();
        let o = VerifyOptions::default();
        let k = key(b"index", "/r/Packages.adb", &keys, &o);
        assert_ne!(k, key(b"indey", "/r/Packages.adb", &keys, &o));
        assert_ne!(k, key(b"index", "/s/Packages.adb", &keys, &o));
        assert_ne!(k, key(b"index", "/r/Packages.adb", &keys, &VerifyOptions { allow_untrusted: true, ..o.clone() }));
        let mut other = TrustedKeys::new();
        let rsa = openssl::rsa::Rsa::generate(2048).unwrap();
        other.add_pem("k.rsa.pub", &rsa.public_key_to_pem().unwrap()).unwrap();
        assert_ne!(k, key(b"index", "/r/Packages.adb", &other, &o));
    }
}
