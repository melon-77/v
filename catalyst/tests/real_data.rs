//! Tests against Valence's real repository. They are skipped (with a
//! message) when the files are not there. Paths can be changed with
//! CATALYST_REPO (the directory with Packages.adb and the .apk files) and
//! CATALYST_KEYS (the trusted keys directory).

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use openssl::pkey::PKey;
use openssl::rsa::Rsa;

use catalyst::adb::{self, EntryKind, FileKind, PackageReader};
use catalyst::digest;
use catalyst::verify::{self, TrustedKeys, Verified, VerifyOptions};
use catalyst::{version, Error};

fn repo() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var("CATALYST_REPO").unwrap_or_else(|_| "/root/valence/repo/x86_64".into()));
    if p.join("Packages.adb").exists() {
        Some(p)
    } else {
        eprintln!("SKIPPED: no repository at {}", p.display());
        None
    }
}

fn keys() -> TrustedKeys {
    let d = std::env::var("CATALYST_KEYS").unwrap_or_else(|_| "/root/valence/keys/trusted".into());
    let k = TrustedKeys::from_dir(Path::new(&d)).unwrap();
    assert!(!k.is_empty(), "no keys in {d}");
    k
}

fn strict() -> VerifyOptions {
    VerifyOptions::default()
}

fn decompressed(p: &Path) -> Vec<u8> {
    adb::decompress_all(std::fs::File::open(p).unwrap()).unwrap().1
}

/// (kind, header offset, payload offset, payload length) of each block of
/// a decompressed file.
fn blocks(d: &[u8]) -> Vec<(u32, usize, usize, usize)> {
    let mut v = Vec::new();
    let mut pos = 8;
    while pos < d.len() {
        let ts = u32::from_le_bytes(d[pos..pos + 4].try_into().unwrap());
        let (kind, raw, hdr) = if ts >> 30 == 3 {
            (ts & 0x3fff_ffff, u64::from_le_bytes(d[pos + 8..pos + 16].try_into().unwrap()) as usize, 16)
        } else {
            (ts >> 30, (ts & 0x3fff_ffff) as usize, 4)
        };
        v.push((kind, pos, pos + hdr, raw - hdr));
        pos += (raw + 7) & !7;
    }
    v
}

fn deflate_file(d: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(b"ADBd".to_vec(), flate2::Compression::fast());
    e.write_all(d).unwrap();
    e.finish().unwrap()
}

fn verify_bytes(b: &[u8], keys: &TrustedKeys, opts: &VerifyOptions) -> catalyst::Result<Verified> {
    verify::verify_reader(Cursor::new(b), keys, opts)
}

fn index(repo: &Path, keys: &TrustedKeys) -> adb::Index {
    adb::read_index_file(&repo.join("Packages.adb"), keys, &strict()).unwrap().0
}

#[test]
fn index_parses_and_matches_the_repository() {
    let Some(repo) = repo() else { return };
    let keys = keys();
    let (idx, trust) = adb::read_index_file(&repo.join("Packages.adb"), &keys, &strict()).unwrap();
    assert!(trust.signer.is_some());
    let apks: Vec<_> = std::fs::read_dir(&repo)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".apk"))
        .collect();
    eprintln!("index: {} packages, {} .apk files", idx.packages.len(), apks.len());
    assert_eq!(idx.packages.len(), apks.len());
    let mut names = std::collections::HashSet::new();
    for p in &idx.packages {
        assert!(!p.name.is_empty());
        assert!(version::validate(&p.version), "{}: bad version {}", p.name, p.version);
        assert_eq!(p.hashes.len(), 32, "{}", p.nv());
        assert!(p.identity().is_some());
        assert!(names.insert(p.nv()), "duplicate {}", p.nv());
        let f = repo.join(format!("{}.apk", p.nv()));
        assert_eq!(std::fs::metadata(&f).unwrap().len(), p.file_size, "{}", p.nv());
        for d in p.depends.iter().chain(&p.provides).chain(&p.install_if) {
            assert!(!d.name.is_empty());
            if !d.version.is_empty() {
                assert!(version::validate(&d.version), "{}: dep {d}", p.nv());
            }
        }
    }
}

/// Every package: identity equals the index hash, the signature verifies,
/// every file's content matches its hash, and the package's own info
/// equals the index entry apart from `hashes` and `file-size`.
#[test]
fn all_packages_verify_against_index() {
    let Some(repo) = repo() else { return };
    let keys = keys();
    let idx = index(&repo, &keys);
    let next = AtomicUsize::new(0);
    let totals = Mutex::new((0usize, 0u64, 0u64, 0usize, 0usize));
    let errors = Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(entry) = idx.packages.get(i) else { break };
                let path = repo.join(format!("{}.apk", entry.nv()));
                let opts = VerifyOptions { expected_identity: Some(entry.hashes.clone()), require_signature: true, ..Default::default() };
                let res = (|| -> catalyst::Result<(usize, u64, u64, usize)> {
                    let mut pr = adb::open_package_file(&path, &keys, &opts)?;
                    let info = &pr.package().info;
                    let mut a = info.clone();
                    let mut b = entry.clone();
                    a.hashes.clear();
                    b.hashes.clear();
                    b.file_size = 0;
                    if a != b {
                        return Err(Error::Schema(format!("info differs from index: {a:?} vs {b:?}")));
                    }
                    if pr.trust().identity.is_none() || pr.trust().signer.is_none() {
                        return Err(Error::Untrusted("identity or signature missing".into()));
                    }
                    let mut files = 0;
                    let mut buf = vec![0u8; 256 * 1024];
                    while let Some(mut e) = pr.next_entry()? {
                        if e.kind == EntryKind::File {
                            files += 1;
                        }
                        if let Some(d) = e.data() {
                            while d.read(&mut buf).map_err(|e| Error::Integrity(e.to_string()))? > 0 {}
                        }
                    }
                    let blocks = pr.data_blocks();
                    let bytes = pr.package().data_bytes();
                    let scripts = pr.package().scripts.iter().count();
                    pr.finish()?;
                    Ok((files, blocks, bytes, scripts))
                })();
                match res {
                    Ok((f, b, by, sc)) => {
                        let mut t = totals.lock().unwrap();
                        t.0 += f;
                        t.1 += b;
                        t.2 += by;
                        t.3 += 1;
                        t.4 += sc;
                    }
                    Err(e) => errors.lock().unwrap().push(format!("{}: {e}", entry.nv())),
                }
            });
        }
    });
    let t = totals.into_inner().unwrap();
    let errors = errors.into_inner().unwrap();
    eprintln!(
        "verified {} packages: {} file entries, {} data blocks, {} content bytes, {} scripts; {} errors",
        t.3,
        t.0,
        t.1,
        t.2,
        t.4,
        errors.len()
    );
    for e in &errors {
        eprintln!("  {e}");
    }
    assert!(errors.is_empty());
    assert_eq!(t.3, idx.packages.len());
}

fn pick(repo: &Path, prefix: &str) -> PathBuf {
    let mut v: Vec<_> = std::fs::read_dir(repo)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(prefix))
        .collect();
    v.sort();
    v.into_iter().next().unwrap_or_else(|| panic!("no package {prefix}*"))
}

#[test]
fn tampered_package_is_rejected() {
    let Some(repo) = repo() else { return };
    let keys = keys();
    let path = pick(&repo, "busybox-1");
    let d = decompressed(&path);
    let bl = blocks(&d);
    let (_, _, adb_off, adb_len) = bl[0];
    let sig = bl.iter().find(|b| b.0 == 1).unwrap();
    let data: Vec<_> = bl.iter().filter(|b| b.0 == 2).collect();
    assert!(!data.is_empty());

    // the decompressed stream is itself a valid uncompressed file, and
    // recompression does not matter (the signature covers decompressed data)
    assert!(verify_bytes(&d, &keys, &strict()).is_ok());
    assert!(verify_bytes(&deflate_file(&d), &keys, &strict()).is_ok());

    let flip = |off: usize, mask: u8| {
        let mut t = d.clone();
        t[off] ^= mask;
        t
    };
    // ADB block: compat version byte -> format error
    assert_eq!(verify_bytes(&flip(adb_off, 0x01), &keys, &strict()).unwrap_err().kind(), "format");
    // ADB block: any other byte changed breaks the signature
    for off in [adb_off + 1, adb_off + 4, adb_off + adb_len / 3, adb_off + adb_len / 2, adb_off + adb_len - 1] {
        let e = verify_bytes(&flip(off, 0x01), &keys, &strict()).unwrap_err();
        assert_eq!(e.kind(), "untrusted", "ADB flip at {off}: {e}");
        let e = verify_bytes(&deflate_file(&flip(off, 0x01)), &keys, &strict()).unwrap_err();
        assert_eq!(e.kind(), "untrusted", "ADB flip at {off} (deflated): {e}");
    }
    // schema in the file header is covered by the signature too
    let mut t = d.clone();
    t[4..8].copy_from_slice(b"pckh");
    assert!(verify_bytes(&t, &keys, &strict()).is_err());
    // SIG block: version, hash_alg, key id, signature bytes
    let (_, _, sig_off, sig_len) = *sig;
    for (off, what) in [
        (sig_off, "sign_ver"),
        (sig_off + 1, "hash_alg"),
        (sig_off + 5, "key id"),
        (sig_off + 18, "signature start"),
        (sig_off + 18 + (sig_len - 18) / 2, "signature middle"),
        (sig_off + sig_len - 1, "signature end"),
    ] {
        let e = verify_bytes(&flip(off, 0x01), &keys, &strict()).unwrap_err();
        assert_eq!(e.kind(), "untrusted", "{what}: {e}");
    }
    // DATA block content: rejected by the content hash
    for b in &data {
        let (_, _, p_off, p_len) = **b;
        for off in [p_off + 8, p_off + 8 + (p_len - 8) / 2, p_off + p_len - 1] {
            let e = verify_bytes(&flip(off, 0x80), &keys, &strict()).unwrap_err();
            assert_eq!(e.kind(), "integrity", "DATA flip at {off}: {e}");
        }
        // DATA header (path_idx / file_idx)
        let e = verify_bytes(&flip(p_off, 0x02), &keys, &strict()).unwrap_err();
        assert_eq!(e.kind(), "schema", "DATA header flip: {e}");
    }
    // the same through the streaming reader: the DataReader returns an
    // error at the end of the content and the reader stays failed
    let (_, _, p_off, _) = *data[0];
    let t = flip(p_off + 9, 0x10);
    let mut pr = PackageReader::open(Cursor::new(&t), &keys, &strict()).unwrap();
    let mut saw = false;
    loop {
        match pr.next_entry() {
            Ok(Some(mut e)) => {
                if let Some(r) = e.data() {
                    let mut v = Vec::new();
                    assert!(r.read_to_end(&mut v).is_err());
                    saw = true;
                }
            }
            Ok(None) => panic!("tampered content accepted"),
            Err(e) => {
                assert!(saw);
                assert_eq!(e.kind(), "integrity");
                break;
            }
        }
    }
    // identity mismatch is fatal even with allow_untrusted
    let entry = index(&repo, &keys).packages.into_iter().find(|p| path.ends_with(format!("{}.apk", p.nv()))).unwrap();
    let mut wrong = entry.hashes.clone();
    wrong[31] ^= 1;
    let o = VerifyOptions { expected_identity: Some(wrong), allow_untrusted: true, ..Default::default() };
    assert_eq!(verify_bytes(&d, &keys, &o).unwrap_err().kind(), "integrity");
    // correct identity, no keys at all: trusted by identity alone (as apk)
    let o = VerifyOptions { expected_identity: Some(entry.hashes.clone()), ..Default::default() };
    match verify_bytes(&d, &TrustedKeys::new(), &o).unwrap() {
        Verified::Package { trust, .. } => assert!(trust.identity.is_some() && trust.signer.is_none()),
        _ => panic!(),
    }
    // ... and a tampered ADB with the right identity of the original fails
    assert_eq!(verify_bytes(&flip(adb_off + 20, 1), &TrustedKeys::new(), &o).unwrap_err().kind(), "integrity");
    // removing the SIG block: unsigned
    let mut t = d[..sig.1].to_vec();
    t.extend_from_slice(&d[sig.1 + ((sig.3 + 4 + 7) & !7)..]);
    let e = verify_bytes(&t, &keys, &strict()).unwrap_err();
    assert_eq!(e.kind(), "untrusted");
    assert!(verify_bytes(&t, &keys, &VerifyOptions { allow_untrusted: true, ..Default::default() }).is_ok());
    // truncation anywhere is an error
    for cut in [9, adb_off + 3, adb_off + adb_len, sig_off + 10, data[0].2 + 4, d.len() - 1, d.len() - 8] {
        assert!(verify_bytes(&d[..cut], &keys, &strict()).is_err(), "cut at {cut}");
        assert!(verify_bytes(&deflate_file(&d[..cut]), &keys, &strict()).is_err(), "cut at {cut} (deflated)");
    }
    // trailing bytes
    let mut t = d.clone();
    t.extend_from_slice(&[0; 8]);
    assert!(verify_bytes(&t, &keys, &strict()).is_err());
    // a DATA block dropped
    let (_, h, _, l) = *data[data.len() - 1];
    let mut t = d[..h].to_vec();
    t.extend_from_slice(&d[h + ((l + 4 + 7) & !7)..]);
    assert_eq!(verify_bytes(&t, &keys, &strict()).unwrap_err().kind(), "schema");
    // a DATA block duplicated
    let mut t = d.clone();
    t.extend_from_slice(&d[h..h + ((l + 4 + 7) & !7)]);
    assert!(verify_bytes(&t, &keys, &strict()).is_err());
    // corrupt compressed bytes of the real file
    let raw = std::fs::read(&path).unwrap();
    for off in [10, raw.len() / 3, raw.len() / 2, raw.len() - 5] {
        let mut t = raw.clone();
        t[off] ^= 0x55;
        assert!(verify_bytes(&t, &keys, &strict()).is_err(), "compressed flip at {off}");
    }
}

#[test]
fn tampered_index_is_rejected() {
    let Some(repo) = repo() else { return };
    let keys = keys();
    let d = decompressed(&repo.join("Packages.adb"));
    let bl = blocks(&d);
    assert_eq!(bl.len(), 2);
    let (_, _, adb_off, adb_len) = bl[0];
    let (_, _, sig_off, sig_len) = bl[1];
    assert!(verify_bytes(&d, &keys, &strict()).is_ok());
    for off in [adb_off + 1, adb_off + 100, adb_off + adb_len / 2, adb_off + adb_len - 1, sig_off + 2, sig_off + sig_len - 1] {
        let mut t = d.clone();
        t[off] ^= 4;
        let e = verify_bytes(&t, &keys, &strict()).unwrap_err();
        assert_eq!(e.kind(), "untrusted", "flip at {off}: {e}");
    }
    // the identity option does not apply to an index: still needs a signature
    let o = VerifyOptions { expected_identity: Some(digest::sha256(&d[adb_off..adb_off + adb_len]).to_vec()), ..Default::default() };
    assert!(verify_bytes(&d, &TrustedKeys::new(), &o).is_err());
}

#[test]
fn untrusted_key_is_rejected() {
    let Some(repo) = repo() else { return };
    let real = keys();
    let test_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let mut only_test = TrustedKeys::new();
    only_test.add_pem("test.rsa.pub", &test_key.public_key_to_pem().unwrap()).unwrap();

    for path in [repo.join("Packages.adb"), pick(&repo, "busybox-1")] {
        // real file, but only an unrelated key is trusted
        let e = verify::verify_file(&path, &only_test, &strict()).unwrap_err();
        assert_eq!(e.kind(), "untrusted", "{}: {e}", path.display());

        // the same file re-signed with the test key (SIG block replaced)
        let d = decompressed(&path);
        let bl = blocks(&d);
        let (_, _, adb_off, adb_len) = bl[0];
        let sig = bl.iter().find(|b| b.0 == 1).unwrap();
        let schema = u32::from_le_bytes(d[4..8].try_into().unwrap());
        let payload = verify::sign_adb(&test_key, schema, &d[adb_off..adb_off + adb_len]).unwrap();
        let mut t = d[..sig.1].to_vec();
        t.extend_from_slice(&((1u32 << 30) | (payload.len() as u32 + 4)).to_le_bytes());
        t.extend_from_slice(&payload);
        while t.len() % 8 != 0 {
            t.push(0);
        }
        t.extend_from_slice(&d[sig.1 + ((sig.3 + 4 + 7) & !7)..]);
        match verify_bytes(&t, &only_test, &strict()).unwrap() {
            Verified::Index { trust, .. } | Verified::Package { trust, .. } => {
                assert_eq!(trust.signer.unwrap().key_name, "test.rsa.pub")
            }
        }
        let e = verify_bytes(&t, &real, &strict()).unwrap_err();
        assert_eq!(e.kind(), "untrusted");
        // both keys trusted: the re-signed file verifies (with the test key)
        let mut both = keys();
        both.add_pem("test.rsa.pub", &test_key.public_key_to_pem().unwrap()).unwrap();
        assert!(verify_bytes(&t, &both, &strict()).is_ok());
    }
}

/// Byte flips all over a small package's ADB block, read with
/// allow_untrusted so the parser sees the damage: errors are fine, panics
/// are not. Also random multi-byte damage over the whole file.
#[test]
fn damaged_input_never_panics() {
    let Some(repo) = repo() else { return };
    let keys = keys();
    let au = VerifyOptions { allow_untrusted: true, ..Default::default() };
    let mut outcomes: HashMap<&'static str, usize> = HashMap::new();
    for prefix in ["acl-dev-", "busybox-1", "valence-base-"] {
        let d = decompressed(&pick(&repo, prefix));
        let bl = blocks(&d);
        let (_, _, adb_off, adb_len) = bl[0];
        for off in adb_off..adb_off + adb_len.min(4096) {
            for mask in [0x01u8, 0x80, 0xff] {
                let mut t = d.clone();
                t[off] ^= mask;
                let r = verify_bytes(&t, &keys, &au);
                *outcomes.entry(match &r { Ok(_) => "ok", Err(e) => e.kind() }).or_default() += 1;
            }
        }
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        for _ in 0..3000 {
            let mut t = d.clone();
            for _ in 0..4 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let off = (x as usize) % t.len();
                t[off] = (x >> 32) as u8;
            }
            let r = verify_bytes(&t, &keys, &au);
            *outcomes.entry(match &r { Ok(_) => "ok", Err(e) => e.kind() }).or_default() += 1;
        }
    }
    eprintln!("damaged-input outcomes: {outcomes:?}");
}

/// Checks the spec's writer facts on every real file and prints what was
/// found: compression prefixes (§B3.3), SIG parameters (§C1), array type
/// tags (§D1.2), the package unique id (§D10.1), root directory entries
/// (§D7.3), empty files and hard links (§E2).
#[test]
fn writer_facts_survey() {
    use catalyst::adb::schema::{DI_FILES, NDX_PACKAGES, PI_DEPENDS, PI_HASHES, PKG_INFO, PKG_PATHS};
    use catalyst::adb::value::{TYPE_ARRAY, TYPE_BLOB_8, TYPE_OBJECT};
    let Some(repo) = repo() else { return };
    let mut c: std::collections::BTreeMap<String, usize> = Default::default();
    let mut bump = |k: String| *c.entry(k).or_default() += 1;
    let tag = |t: u32| match t {
        TYPE_ARRAY => "ARRAY(0xd)",
        TYPE_OBJECT => "OBJECT(0xe)",
        _ => "other",
    };
    let mut files: Vec<PathBuf> = vec![repo.join("Packages.adb")];
    let mut apks: Vec<PathBuf> = std::fs::read_dir(&repo)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "apk").unwrap_or(false))
        .collect();
    apks.sort();
    files.extend(apks);
    let empty_sha = digest::sha256(b"").to_vec();
    for f in &files {
        let raw_prefix = {
            let mut b = [0u8; 4];
            std::fs::File::open(f).unwrap().read_exact(&mut b).unwrap();
            b
        };
        bump(format!("prefix {}", String::from_utf8_lossy(&raw_prefix)));
        let head = adb::read_head(std::fs::File::open(f).unwrap()).unwrap();
        bump(format!("SIG blocks per file: {}", head.sigs.len()));
        for s in &head.sigs {
            bump(format!("SIG sign_ver {} hash_alg {} signature {} bytes key {}", s[0], s[1], s.len() - 18, digest::hex(&s[2..18])));
        }
        let a = &head.adb;
        let root = a.root_obj().unwrap();
        if head.container.schema() == adb::SCHEMA_INDEX {
            bump(format!("index packages array tag {}", tag(root.get(NDX_PACKAGES).type_tag())));
            for item in root.obj(NDX_PACKAGES).unwrap().items() {
                let pi = a.obj(item).unwrap();
                if !pi.get(PI_DEPENDS).is_null() {
                    bump(format!("index depends array tag {}", tag(pi.get(PI_DEPENDS).type_tag())));
                }
            }
            continue;
        }
        let info = root.obj(PKG_INFO).unwrap();
        if !info.get(PI_DEPENDS).is_null() {
            bump(format!("package depends array tag {}", tag(info.get(PI_DEPENDS).type_tag())));
        }
        bump(format!("package paths array tag {}", tag(root.get(PKG_PATHS).type_tag())));
        // §D10.1: info.hashes = first 20 bytes of SHA-256 over the ADB
        // payload with those 20 bytes zeroed
        let hv = info.get(PI_HASHES);
        assert_eq!(hv.type_tag(), TYPE_BLOB_8);
        let off = hv.value() as usize + 1;
        let mut z = a.bytes().to_vec();
        let id = z[off..off + 20].to_vec();
        z[off..off + 20].fill(0);
        if digest::sha256(&z)[..20] == id[..] {
            bump("unique id = sha256(ADB with id zeroed)[..20]: yes".into());
        } else {
            bump("unique id = sha256(ADB with id zeroed)[..20]: NO".into());
        }
        let pkg = adb::Package::from_adb(a).unwrap();
        bump(format!("root directory entry present: {}", pkg.dirs.first().map(|d| d.name.is_empty()).unwrap_or(false)));
        let paths = root.obj(PKG_PATHS).unwrap();
        for (i, d) in pkg.dirs.iter().enumerate() {
            if !d.files.is_empty() {
                bump(format!("files array tag {}", tag(paths.items().nth(i).map(|v| a.obj(v).unwrap().get(DI_FILES).type_tag()).unwrap())));
            }
        }
        let mut regular: HashMap<Vec<u8>, (u64, Vec<u8>)> = HashMap::new();
        for d in &pkg.dirs {
            for fe in &d.files {
                let p = d.file_path(fe);
                match &fe.kind {
                    FileKind::Regular => {
                        bump(format!("regular file hash length {}", fe.hash.len()));
                        if fe.size == 0 {
                            bump(format!("empty file hash = sha256(\"\"): {}", fe.hash == empty_sha));
                        }
                        regular.insert(p, (fe.size, fe.hash.clone()));
                    }
                    FileKind::Hardlink(t) => {
                        let raw = fe.target_raw.as_ref().unwrap();
                        bump(format!("hard link target written with './': {}", raw[2..].starts_with(b"./")));
                        let (sz, h) = regular.get(t).unwrap();
                        bump(format!("hard link entry carries target's size and hash: {}", *sz == fe.size && *h == fe.hash));
                    }
                    FileKind::Symlink(t) => {
                        bump(format!("symlink: size == target length: {}, hash present: {}", fe.size == t.len() as u64, !fe.hash.is_empty()));
                    }
                    _ => bump("device/fifo".into()),
                }
            }
        }
    }
    eprintln!("writer facts over {} files:", files.len());
    for (k, n) in &c {
        eprintln!("  {n:7}  {k}");
    }
    assert!(!c.keys().any(|k| k.ends_with("NO")));
}

#[test]
fn package_kinds() {
    let Some(repo) = repo() else { return };
    let keys = keys();
    let idx = index(&repo, &keys);
    let (mut symlinks, mut hardlinks, mut devices, mut empty, mut with_scripts, mut with_triggers, mut xattrs) =
        (0, 0, 0, 0, 0, 0, 0);
    for p in &idx.packages {
        let pr = adb::open_package_file(&repo.join(format!("{}.apk", p.nv())), &keys, &strict()).unwrap();
        let pkg = pr.package();
        for d in &pkg.dirs {
            for f in &d.files {
                match f.kind {
                    FileKind::Symlink(_) => symlinks += 1,
                    FileKind::Hardlink(_) => hardlinks += 1,
                    FileKind::Regular if f.size == 0 => empty += 1,
                    FileKind::Regular => {}
                    _ => devices += 1,
                }
                if !f.acl.xattrs.is_empty() {
                    xattrs += 1;
                }
            }
        }
        if !pkg.scripts.is_empty() {
            with_scripts += 1;
        }
        if !pkg.triggers.is_empty() {
            with_triggers += 1;
        }
    }
    eprintln!(
        "kinds: {symlinks} symlinks, {hardlinks} hard links, {devices} device/fifo nodes, {empty} empty files, {xattrs} with xattrs; {with_scripts} packages with scripts, {with_triggers} with triggers"
    );
    assert!(symlinks > 0);
}


// ------------------------------------------------------------ phase 10, stage A (build machine, 2026-11-22)

/// N-1: a garbled record stops every change before a journal is written,
/// and `oct fix` rebuilds it from the signed package.
#[test]
fn fix_rebuilds_a_garbled_record_from_the_signed_package() {
    let Some(repo) = repo() else { return };
    let keys = std::env::var("CATALYST_KEYS").unwrap_or_else(|_| "/root/valence/keys/trusted".into());
    let root = std::env::temp_dir().join(format!("catalyst-rd-rebuild-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let index = repo.join("Packages.adb");
    let catalyst = |args: &[&str]| {
        let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_oct"));
        c.args(["--root", root.to_str().unwrap(), "--repository", index.to_str().unwrap(), "--keys-dir", &keys, "--no-scripts", "--plain"]);
        c.args(args).output().unwrap()
    };
    assert!(catalyst(&["--initdb", "add", "zstd-libs"]).status.success());
    let cur = std::fs::read_link(root.join("Programs/zstd-libs/Current")).unwrap();
    let files = root.join("Programs/zstd-libs").join(&cur).join(".catalyst/files");
    let good = std::fs::read(&files).unwrap();
    std::fs::write(&files, b"\x00garbled\n").unwrap();
    // a change that shares no file with zstd-libs doesn't read its record (the index says who has which path)
    assert!(catalyst(&["add", "xz-libs"]).status.success());
    // one that needs it stops before any journal is written
    let out = catalyst(&["del", "zstd-libs"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("oct fix zstd-libs"));
    assert!(!root.join("var/lib/catalyst/journal").exists());
    let out = catalyst(&["fix"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("rebuilt the record of zstd-libs"));
    assert_eq!(std::fs::read(&files).unwrap(), good);
    assert!(catalyst(&["del", "zstd-libs"]).status.success());
    let _ = std::fs::remove_dir_all(&root);
}

/// Stage B: an install that makes the tree's directory symlinks
/// (`lib64 -> usr/lib`) leaves them in the lookup index, and after a series of
/// changes the index is byte for byte the one `oct fix` rebuilds from the records.
#[test]
fn lookup_index_knows_the_directory_symlinks_and_equals_a_rebuild() {
    let Some(repo) = repo() else { return };
    let keys = std::env::var("CATALYST_KEYS").unwrap_or_else(|_| "/root/valence/keys/trusted".into());
    let root = std::env::temp_dir().join(format!("catalyst-rd-index-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let index = repo.join("Packages.adb");
    let ok = |args: &[&str]| {
        let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_oct"));
        c.args(["--root", root.to_str().unwrap(), "--repository", index.to_str().unwrap(), "--keys-dir", &keys, "--no-scripts", "--plain"]);
        let out = c.args(args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    ok(&["--initdb", "add", "valence-base"]);
    let ix = root.join("var/lib/catalyst/index");
    let state = std::fs::read_to_string(ix.join("state")).unwrap();
    assert!(state.contains("link\tlib64\tusr/lib\n"), "{state}");
    ok(&["add", "nano", "htop"]);
    ok(&["del", "nano"]);
    let snap = || {
        let mut m = std::collections::BTreeMap::new();
        for e in std::fs::read_dir(ix.join("files")).unwrap() {
            let e = e.unwrap();
            m.insert(format!("files/{}", e.file_name().to_string_lossy()), std::fs::read(e.path()).unwrap());
        }
        for f in ["aliases", "meta", "state"] {
            m.insert(f.to_string(), std::fs::read(ix.join(f)).unwrap());
        }
        m
    };
    let updated = snap();
    ok(&["fix"]);
    assert!(updated == snap(), "the updated index differs from the one rebuilt by oct fix");
    let _ = std::fs::remove_dir_all(&root);
}

/// `oct` on `root` with the real repository, `--no-scripts`, and `env` added.
fn oct_real(root: &std::path::Path, env: &[(&str, &str)], args: &[&str]) -> std::process::Output {
    let index = repo().unwrap().join("Packages.adb");
    let keys = std::env::var("CATALYST_KEYS").unwrap_or_else(|_| "/root/valence/keys/trusted".into());
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_oct"));
    c.args(["--root", root.to_str().unwrap(), "--repository", index.to_str().unwrap(), "--keys-dir", &keys, "--no-scripts", "--plain"]);
    for (k, v) in env {
        c.env(k, v);
    }
    c.args(args).output().unwrap()
}

fn fresh_root(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("catalyst-rd-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// Gemini's audit (5b18213), bug 1: with a valid index the package list comes
/// from its copy of every info, and `oct fix` must still find, and rebuild, a
/// deleted `.catalyst/info`.
#[test]
fn fix_rebuilds_a_deleted_info_while_the_index_is_valid() {
    if repo().is_none() {
        return;
    }
    let root = fresh_root("deleted-info");
    assert!(oct_real(&root, &[], &["--initdb", "add", "zstd-libs", "xz-libs"]).status.success());
    let info = root.join("Programs/zstd-libs/Current/.catalyst/info");
    std::fs::remove_file(&info).unwrap();
    let out = oct_real(&root, &[], &["fix"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("rebuilt the record of zstd-libs"));
    assert!(info.exists());
    let _ = std::fs::remove_dir_all(&root);
}

/// Bug 2: a record that a change through the index doesn't read can't be
/// read, and the change is killed in its switch. Finishing it reads only what
/// the switch read; when the index from before the switch is gone as well,
/// `oct fix` rebuilds the record first and then finishes the switch.
#[test]
fn a_killed_switch_is_finished_without_the_records_it_did_not_read() {
    if repo().is_none() {
        return;
    }
    for lose_index in [false, true] {
        let root = fresh_root(if lose_index { "killed-noindex" } else { "killed" });
        assert!(oct_real(&root, &[], &["--initdb", "add", "zstd-libs", "xz-libs"]).status.success());
        std::fs::write(root.join("Programs/zstd-libs/Current/.catalyst/files"), b"\x00garbled\n").unwrap();
        let out = oct_real(&root, &[("CATALYST_CRASH_AT", "placed")], &["del", "xz-libs"]);
        assert!(!out.status.success());
        assert!(root.join("var/lib/catalyst/journal").exists());
        if lose_index {
            std::fs::remove_file(root.join("var/lib/catalyst/index/state")).unwrap();
            let out = oct_real(&root, &[], &["upgrade"]);
            assert!(!out.status.success());
            assert!(String::from_utf8_lossy(&out.stderr).contains("oct fix"), "{}", String::from_utf8_lossy(&out.stderr));
            let out = oct_real(&root, &[], &["fix"]);
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            assert!(String::from_utf8_lossy(&out.stdout).contains("rebuilt the record of zstd-libs"));
        } else {
            let out = oct_real(&root, &[], &["upgrade"]);
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        }
        assert!(!root.join("var/lib/catalyst/journal").exists());
        assert!(!root.join("Programs/xz-libs/Current").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
