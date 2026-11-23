//! A compact binary form of parsed records, for catalyst's own derived
//! files: the cache of verified repository indexes ([`crate::idxcache`])
//! and the metadata in the lookup index ([`crate::index`]). Reading it back
//! is a few times faster than parsing the text records or the ADB again.
//!
//! Numbers and lengths are LEB128; a string or byte string is its length
//! and its bytes; a list is its length and its items; an option is a 0/1
//! byte and, after 1, the value. The writers destructure every struct, so
//! adding a field fails to compile until the format has it. The encoding
//! is canonical: the same value always gives the same bytes.

use crate::adb::{Dependency, Index, PkgInfo};
use crate::db::PkgMeta;
use crate::digest::Alg;
use crate::verify::{SignerInfo, TrustInfo};

pub(crate) fn put_n(o: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            o.push(b);
            return;
        }
        o.push(b | 0x80);
    }
}

pub(crate) fn put_b(o: &mut Vec<u8>, b: &[u8]) {
    put_n(o, b.len() as u64);
    o.extend_from_slice(b);
}

fn put_strs(o: &mut Vec<u8>, v: &[String]) {
    put_n(o, v.len() as u64);
    for s in v {
        put_b(o, s.as_bytes());
    }
}

fn put_deps(o: &mut Vec<u8>, v: &[Dependency]) {
    put_n(o, v.len() as u64);
    for d in v {
        let Dependency { name, version, op } = d;
        put_b(o, name.as_bytes());
        put_b(o, version.as_bytes());
        o.push(*op);
    }
}

pub(crate) fn put_pkg(o: &mut Vec<u8>, p: &PkgInfo) {
    let PkgInfo {
        name,
        version,
        hashes,
        description,
        arch,
        license,
        origin,
        maintainer,
        url,
        repo_commit,
        build_time,
        installed_size,
        file_size,
        provider_priority,
        depends,
        provides,
        replaces,
        install_if,
        recommends,
        layer,
        tags,
    } = p;
    put_b(o, name.as_bytes());
    put_b(o, version.as_bytes());
    put_b(o, hashes);
    for s in [description, arch, license, origin, maintainer, url] {
        put_b(o, s.as_bytes());
    }
    put_b(o, repo_commit);
    for v in [build_time, installed_size, file_size, provider_priority] {
        put_n(o, *v);
    }
    for d in [depends, provides, replaces, install_if, recommends] {
        put_deps(o, d);
    }
    put_n(o, *layer);
    put_strs(o, tags);
}

pub(crate) fn put_index(o: &mut Vec<u8>, ix: &Index) {
    let Index { description, packages, pkgname_spec, skipped } = ix;
    put_b(o, description.as_bytes());
    match pkgname_spec {
        Some(s) => {
            o.push(1);
            put_b(o, s.as_bytes());
        }
        None => o.push(0),
    }
    put_strs(o, skipped);
    put_n(o, packages.len() as u64);
    for p in packages {
        put_pkg(o, p);
    }
}

pub(crate) fn put_trust(o: &mut Vec<u8>, t: &TrustInfo) {
    let TrustInfo { signer, identity, allowed_untrusted, sig_blocks, sig_errors } = t;
    match signer {
        Some(SignerInfo { key_name, key_id, hash_alg, sig_index }) => {
            o.push(1);
            put_b(o, key_name.as_bytes());
            o.extend_from_slice(key_id);
            o.push(*hash_alg);
            put_n(o, *sig_index as u64);
        }
        None => o.push(0),
    }
    o.push(match identity {
        None => 0,
        Some(Alg::Sha1) => 1,
        Some(Alg::Sha256) => 2,
        Some(Alg::Sha512) => 3,
    });
    o.push(*allowed_untrusted as u8);
    put_n(o, *sig_blocks as u64);
    put_strs(o, sig_errors);
}

pub(crate) fn put_meta(o: &mut Vec<u8>, m: &PkgMeta) {
    let PkgMeta { info, triggers, replaces_priority, source } = m;
    put_pkg(o, info);
    put_strs(o, triggers);
    put_n(o, *replaces_priority);
    put_b(o, source.as_bytes());
}

/// [`put_meta`] into a new buffer.
pub(crate) fn encode_meta(m: &PkgMeta) -> Vec<u8> {
    let mut o = Vec::with_capacity(512);
    put_meta(&mut o, m);
    o
}

/// What [`encode_meta`] wrote, if `b` is exactly that.
pub(crate) fn decode_meta(b: &[u8]) -> Option<PkgMeta> {
    let mut r = Rd::new(b);
    let m = r.meta()?;
    r.at_end().then_some(m)
}

/// Reads what the `put_` functions wrote. Every method returns `None` on
/// anything else: data that ends early, a bad UTF-8 string, an unknown tag.
pub(crate) struct Rd<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Rd<'a> {
    pub(crate) fn new(b: &'a [u8]) -> Self {
        Rd { b, i: 0 }
    }

    pub(crate) fn at_end(&self) -> bool {
        self.i == self.b.len()
    }

    fn byte(&mut self) -> Option<u8> {
        let x = *self.b.get(self.i)?;
        self.i += 1;
        Some(x)
    }

    pub(crate) fn n(&mut self) -> Option<u64> {
        let mut v = 0u64;
        let mut s = 0;
        loop {
            let b = self.byte()?;
            if s > 63 {
                return None;
            }
            v |= ((b & 0x7f) as u64) << s;
            if b & 0x80 == 0 {
                return Some(v);
            }
            s += 7;
        }
    }

    pub(crate) fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.i.checked_add(n)?;
        let s = self.b.get(self.i..end)?;
        self.i = end;
        Some(s)
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = usize::try_from(self.n()?).ok()?;
        self.take(n)
    }

    fn str(&mut self) -> Option<String> {
        String::from_utf8(self.bytes()?.to_vec()).ok()
    }

    /// A list length, at most the bytes left (every item takes one at least).
    fn count(&mut self) -> Option<usize> {
        let n = usize::try_from(self.n()?).ok()?;
        if n > self.b.len() - self.i {
            return None;
        }
        Some(n)
    }

    fn strs(&mut self) -> Option<Vec<String>> {
        let n = self.count()?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(self.str()?);
        }
        Some(v)
    }

    fn deps(&mut self) -> Option<Vec<Dependency>> {
        let n = self.count()?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(Dependency { name: self.str()?, version: self.str()?, op: self.byte()? });
        }
        Some(v)
    }

    pub(crate) fn pkg(&mut self) -> Option<PkgInfo> {
        // fields are evaluated in the order written: the order put_pkg wrote
        Some(PkgInfo {
            name: self.str()?,
            version: self.str()?,
            hashes: self.bytes()?.to_vec(),
            description: self.str()?,
            arch: self.str()?,
            license: self.str()?,
            origin: self.str()?,
            maintainer: self.str()?,
            url: self.str()?,
            repo_commit: self.bytes()?.to_vec(),
            build_time: self.n()?,
            installed_size: self.n()?,
            file_size: self.n()?,
            provider_priority: self.n()?,
            depends: self.deps()?,
            provides: self.deps()?,
            replaces: self.deps()?,
            install_if: self.deps()?,
            recommends: self.deps()?,
            layer: self.n()?,
            tags: self.strs()?,
        })
    }

    pub(crate) fn index(&mut self) -> Option<Index> {
        let description = self.str()?;
        let pkgname_spec = match self.byte()? {
            0 => None,
            1 => Some(self.str()?),
            _ => return None,
        };
        let skipped = self.strs()?;
        let n = self.count()?;
        let mut packages = Vec::with_capacity(n);
        for _ in 0..n {
            packages.push(self.pkg()?);
        }
        Some(Index { description, packages, pkgname_spec, skipped })
    }

    pub(crate) fn trust(&mut self) -> Option<TrustInfo> {
        let signer = match self.byte()? {
            0 => None,
            1 => Some(SignerInfo {
                key_name: self.str()?,
                key_id: self.take(16)?.try_into().ok()?,
                hash_alg: self.byte()?,
                sig_index: usize::try_from(self.n()?).ok()?,
            }),
            _ => return None,
        };
        let identity = match self.byte()? {
            0 => None,
            1 => Some(Alg::Sha1),
            2 => Some(Alg::Sha256),
            3 => Some(Alg::Sha512),
            _ => return None,
        };
        let allowed_untrusted = match self.byte()? {
            0 => false,
            1 => true,
            _ => return None,
        };
        let sig_blocks = usize::try_from(self.n()?).ok()?;
        let sig_errors = self.strs()?;
        Some(TrustInfo { signer, identity, allowed_untrusted, sig_blocks, sig_errors })
    }

    pub(crate) fn meta(&mut self) -> Option<PkgMeta> {
        Some(PkgMeta { info: self.pkg()?, triggers: self.strs()?, replaces_priority: self.n()?, source: self.str()? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_meta_comes_back_equal_and_nothing_else_is_taken_for_one() {
        let m = PkgMeta {
            info: PkgInfo {
                name: "nano".into(),
                version: "8.7.1-r1".into(),
                hashes: vec![1; 32],
                depends: vec![Dependency { name: "so:libc.so.6".into(), version: String::new(), op: 7 }],
                build_time: u64::MAX,
                ..Default::default()
            },
            triggers: vec!["/usr/share/nano/*".into()],
            replaces_priority: 300,
            source: "/r/x86_64/nano-8.7.1-r1.apk".into(),
        };
        let b = encode_meta(&m);
        let back = decode_meta(&b).expect("decoded");
        assert_eq!(format!("{back:?}"), format!("{m:?}"));
        assert_eq!(encode_meta(&back), b, "canonical");
        assert!(decode_meta(&b[..b.len() - 1]).is_none(), "short");
        let mut more = b.clone();
        more.push(0);
        assert!(decode_meta(&more).is_none(), "trailing bytes");
        assert!(decode_meta(b"name: nano\nversion: 1\n").is_none(), "a text record");
    }
}
