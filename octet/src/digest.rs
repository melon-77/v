//! Digests (SHA-1, SHA-256, SHA-512) through OpenSSL, and hex helpers.
//!
//! apk stores digests without an algorithm id; the algorithm is chosen by
//! the length alone (§A3.1): 20 bytes SHA-1, 32 SHA-256, 64 SHA-512.

use openssl::sha::{Sha1, Sha256, Sha512};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Alg {
    Sha1,
    Sha256,
    Sha512,
}

impl Alg {
    /// `apk_digest_alg_by_len`: 20 -> SHA-1, 32 -> SHA-256, 64 -> SHA-512.
    pub fn from_len(len: usize) -> Option<Alg> {
        match len {
            20 => Some(Alg::Sha1),
            32 => Some(Alg::Sha256),
            64 => Some(Alg::Sha512),
            _ => None,
        }
    }

    pub fn len(self) -> usize {
        match self {
            Alg::Sha1 => 20,
            Alg::Sha256 => 32,
            Alg::Sha512 => 64,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Alg::Sha1 => "sha1",
            Alg::Sha256 => "sha256",
            Alg::Sha512 => "sha512",
        }
    }
}

/// An incremental hasher for one of the three algorithms.
pub enum Hasher {
    Sha1(Sha1),
    Sha256(Sha256),
    Sha512(Sha512),
}

impl Hasher {
    pub fn new(alg: Alg) -> Hasher {
        match alg {
            Alg::Sha1 => Hasher::Sha1(Sha1::new()),
            Alg::Sha256 => Hasher::Sha256(Sha256::new()),
            Alg::Sha512 => Hasher::Sha512(Sha512::new()),
        }
    }

    pub fn update(&mut self, data: &[u8]) {
        match self {
            Hasher::Sha1(h) => h.update(data),
            Hasher::Sha256(h) => h.update(data),
            Hasher::Sha512(h) => h.update(data),
        }
    }

    pub fn finish(self) -> Vec<u8> {
        match self {
            Hasher::Sha1(h) => h.finish().to_vec(),
            Hasher::Sha256(h) => h.finish().to_vec(),
            Hasher::Sha512(h) => h.finish().to_vec(),
        }
    }
}

/// One-shot digest.
pub fn digest(alg: Alg, data: &[u8]) -> Vec<u8> {
    match alg {
        Alg::Sha1 => openssl::sha::sha1(data).to_vec(),
        Alg::Sha256 => openssl::sha::sha256(data).to_vec(),
        Alg::Sha512 => openssl::sha::sha512(data).to_vec(),
    }
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    openssl::sha::sha256(data)
}

pub fn sha512(data: &[u8]) -> [u8; 64] {
    openssl::sha::sha512(data)
}

/// Lower-case hex.
pub fn hex(bytes: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 15) as usize] as char);
    }
    s
}

/// Parses hex (either case); `None` on odd length or a non-hex character.
pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if b.len() % 2 != 0 {
        return None;
    }
    fn nib(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
    b.chunks(2)
        .map(|p| Some(nib(p[0])? << 4 | nib(p[1])?))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_digests() {
        assert_eq!(
            hex(&digest(Alg::Sha256, b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(hex(&digest(Alg::Sha1, b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        let mut h = Hasher::new(Alg::Sha512);
        h.update(b"a");
        h.update(b"bc");
        assert_eq!(h.finish(), digest(Alg::Sha512, b"abc"));
    }

    #[test]
    fn hex_roundtrip() {
        assert_eq!(from_hex("00ff10Ab"), Some(vec![0, 255, 16, 171]));
        assert_eq!(from_hex("0"), None);
        assert_eq!(from_hex("zz"), None);
        assert_eq!(hex(&[0, 255, 16]), "00ff10");
        assert_eq!(Alg::from_len(32), Some(Alg::Sha256));
        assert_eq!(Alg::from_len(21), None);
    }
}
