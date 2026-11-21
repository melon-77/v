## vpm core library: done, builds clean, matches apk on all of the real data

The crate is at `C:\Users\a\AppData\Local\Temp\claude\39afd7d0aa98685a\9f48dd28-e365-4eae-aba4-ec797468f31d\scratchpad\vpm` (in WSL: `/mnt/c/.../scratchpad/vpm`). It has a library and a binary. Everything is committed in a local git repo: commit 10ac304, with `Cargo.lock`. The author is "Valence port (local)" <port@valence.invalid>, given with `-c` options only, so no global git config was changed. Nothing was pushed.

- **Dependencies:** flate2 1.1.11 (zlib-rs backend), zstd 0.13.5, openssl 0.10.82 (system libssl 3.0.13). There is no ureq yet, because nothing fetches over the network in this stage.
- **Build:** `cargo build --release` and debug builds give no warnings; `cargo doc` is clean. `cargo clippy` does not exist in the hosttools toolchain, so it was not run.
- **Binary:** `/root/vpm-target/release/vpm`.
- **Build helper:** `scratchpad\vpm-build.sh [cargo args]`.
- **Tests:** run them with `--release`. The real-data tests decompress 4.6 GB and would be slow in a debug build.

## Module APIs

**`error`**
- `Error { Io, Format(String), Schema(String), Untrusted(String), Integrity(String), Key(String), Crypto }`
- `Error::kind() -> &str`, and `Result<T>`.

**`digest`**
- `Alg { Sha1, Sha256, Sha512 }` with `from_len`, `len`, `name`.
- `Hasher::new(alg)`, `.update()`, `.finish()`.
- `digest(alg, &[u8])`, `sha256`, `sha512`, `hex`, `from_hex`. All of these go through OpenSSL.

**`adb`, the container layer**
- `Container<R: Read>::new(r)` reads the compression prefix and the file header, then:
  - `.schema() -> u32`, `.compression()`
  - `.next_block() -> Result<Option<BlockHeader>>`
  - `.read_payload(&mut [u8])`, `.read_payload_exact`, `.read_payload_vec`, `.end_block()`, `.payload_remaining()`
- `BlockHeader { kind, payload_len, ext, offset }`.
- `Compression { None, Deflate{level: Option<u8>}, Zstd{level} }`.
- Constants: `SCHEMA_INDEX`, `SCHEMA_PACKAGE`, `SCHEMA_INSTALLED_DB`, `BLOCK_ADB`, `BLOCK_SIG`, `BLOCK_DATA`.
- Helpers: `decompress_all(r) -> (Compression, Vec<u8>)` and `schema_name(u32)`.
- The container streams. A lying size field cannot make it allocate much.

**`adb`, the value layer**
- `Adb::new(Vec<u8>)` with `.bytes()`, `.root()`, `.root_obj()`, `.int(Val) -> Result<u64>`, `.blob(Val) -> Result<Option<&[u8]>>`, `.obj(Val) -> Result<Obj>`.
- `Obj` has `get(i) -> Val`, `len`, `items`, `int(i)`, `blob(i)`, `obj(i)`.
- `Val(u32)` has `type_tag`, `value`, `is_null`.
- Every access is bounds-checked. A wrong type or an out-of-range value is an error, never a panic.

**`adb`, the typed readers**
- `Index { description, packages: Vec<PkgInfo>, pkgname_spec: Option<String> }`, built with `Index::from_adb(&Adb)`.
- `PkgInfo` has all 21 fields: name, version, hashes, description, arch, license, origin, maintainer, url, repo_commit, build_time, installed_size, file_size, provider_priority, depends, provides, replaces, install_if, recommends, layer, tags. Methods: `.identity() -> Option<(Alg, &[u8])>`, `.nv()`.
- `Dependency { name, version, op }`:
  - `op` is normalised as `apk_dep_from_adb` does: no version gives `|= 7`; no match field gives `=`; the conflict bit is kept.
  - Methods: `Dependency::parse(&str)`, `is_conflict`, `matches_version(&str)`.
  - `Display` prints the same text as apk.
- `Package { info, dirs: Vec<Dir>, scripts: Scripts, triggers: Vec<String>, replaces_priority }`, built with `Package::from_adb`. Also `file_count`, `data_count`, `data_bytes`.
- `Dir { name: Vec<u8>, acl, files }` with `file_path(&FileEntry)`.
- `FileEntry { name, acl, size, mtime, hash, kind, target_raw }` with `has_data`, `hash_alg`, `st_mode`.
- `FileKind { Regular, Hardlink(path), Symlink(target), CharDev(u64), BlockDev(u64), Fifo(u64) }`.
- `Acl { mode: u32, user: Option<String>, group: Option<String>, xattrs: Vec<(name, value)> }`.
- `Scripts` has `get(name)`, `pre_install()` and the other script accessors, and `iter()`.
- Path checks: `valid_filename` and `valid_pathname`.

**`adb`, entry points**
- `read_head(r) -> Head { container, adb, sigs, data_pending }`.
- `read_index(r, &TrustedKeys, &VerifyOptions) -> (Index, TrustInfo)`, plus `read_index_file` and `index_from_head`.
- `open_package_file(path, keys, opts)`, and `file_schema(path)`.

**`adb::PackageReader`**
- Constructors: `PackageReader::open(r, keys, opts)` and `from_head(head, keys, opts)`. Both establish trust before any DATA block is read, then validate the file tree.
- Accessors: `.package()`, `.adb()`, `.trust()`, `.compression()`, `.data_blocks()`.
- `.next_entry() -> Result<Option<Entry>>` walks the tree in stored order.
  - `Entry { kind: Dir|File, path: &[u8], dir, file, path_idx, file_idx }`.
  - `Entry::data() -> Option<&mut DataReader>`. `DataReader` implements `Read`, hashes as it reads, and returns `InvalidData` at the end if the content does not match its hash.
  - Content the caller does not read is still read and checked before the next entry is returned.
- `.finish(self) -> Result<TrustInfo>` checks that no DATA block is left over and that the file ends cleanly.

**`verify`**
- Keys:
  - `TrustedKeys` can be built with `new`, `from_dir`, or `from_root` (which reads `ROOT/etc/apk/keys`, then `ROOT/lib/apk/keys`, with shadowing by file name). It also has `load_dir`, `add_pem` (accepts `PUBLIC KEY` or `RSA PUBLIC KEY`), `add_key`, `iter`, and `.skipped`.
  - `TrustedKey { name, id: [u8;16], bits }`.
  - `key_id(&PKey<Public>)` returns the first 16 bytes of SHA-512 over the PKCS#1 DER key.
- Options: `VerifyOptions { allow_untrusted, expected_identity: Option<Vec<u8>>, require_signature }`.
- Trust decision: `check_trust(keys, schema, adb, sigs, opts) -> TrustInfo`.
  - `TrustInfo { signer: Option<SignerInfo{key_name, key_id, hash_alg, sig_index}>, identity: Option<Alg>, allowed_untrusted, sig_blocks, sig_errors }` with `.describe()`.
  - An identity mismatch is fatal even with `allow_untrusted`.
- Whole files:
  - `verify_file`, `verify_reader`, `verify_index_file`, `verify_package_file(path, keys, Option<identity>)`.
  - These return `Verified::Index{..}` or `Verified::Package{name, version, arch, dirs, files, data_blocks, data_bytes, identity: [u8;32], ..}`.
- `sign_adb(&PKey<Private>, schema, adb)` builds a SIG payload. It is only for tests and tools.

**`version`**
- Constants: `EQUAL`, `LESS`, `GREATER`, `FUZZY`, `CONFLICT`, `ANY`, `CHECKSUM`.
- `validate`, `compare`, `compare_fuzzy`, `compare_opt` (absent versions), `cmp -> Ordering`, `version_match(a, op, b)`, `op_string`, `op_from_str`, `parse_dep(&str) -> Option<(name, op, Option<version>)>`.

**`dump`**
- `index_json`, `package_json`, `pkginfo_json`; `Json::render()` gives the same fields as `apk adbdump --format json`.
- `index_text`, `package_text`.

**Command line**
- `vpm [--root DIR] [--repository X]... [--keys-dir DIR] [--allow-untrusted] [--initdb] [--plain]` followed by:
  - `verify FILE...`: prints the schema, the package count or name-version/arch/file count/data blocks, the identity, the compression and the signing key. With `--repository` it also checks each package against that index.
  - `dump-index FILE [--json]` and `dump FILE [--json]`.
  - `manifest FILE`: prints the SHA-256 of the content actually read from each DATA block.
- `add`, `del`, `upgrade` and `list` say "not implemented yet" and exit 2.

## What was tested

**Unit tests (23):**
- Version: every example in F1.6, F1.7 fuzzy matching, the F1.3 valid/invalid lists, F2.2 and F2.3 operators, F3.1/F3.4 parsing, the F4.3/F4.4 provider quirks, F3.8, and the F1.5 notes (invalid tails, u64 wraparound).
- Container: the B7.10 example; ADBd, ADBc deflate and zstd; truncated streams, trailing bytes, bad prefixes, missing padding, wrong block order, `rawsize < hdrsize`, EXT overflow and EXT type 3, payload limits.
- Values: bounds and wrong types.
- Names, targets, key ids, sign/verify, identity.

**Real-data integration tests (8, `tests/real_data.rs`), all pass in about 7 s.** They are skipped if the repository is missing; `VPM_REPO` and `VPM_KEYS` override the paths.
- **Index:** 1102 packages for 1102 `.apk` files. Every `file-size` equals the file's size, every hash is 32 bytes, every version is valid.
- **All 1102 packages:** each was verified with both the identity from the index and the signature required. That covers 148,986 file entries and 125,409 DATA blocks, 4,662,385,150 content bytes, every one checked against its hash. Each package's own info equals its index entry apart from `hashes` and `file-size`.
- **Tampering busybox, all rejected:**
  - one byte flipped in the ADB block: untrusted; the compat-version byte gives a format error;
  - the schema bytes changed;
  - sign_ver, hash_alg, key id, or the start, middle or end of the signature: untrusted;
  - content of each DATA block: integrity error, both through `verify` and through the streaming `DataReader`;
  - DATA header (path or file index): schema error;
  - wrong identity, even with `allow_untrusted`: integrity error;
  - SIG block removed: untrusted;
  - truncation at 7 points, both plain and re-deflated;
  - trailing bytes, a DATA block dropped, a DATA block duplicated;
  - bytes flipped inside the compressed file.
- Uncompressed and re-deflated copies still verify, because the signature covers the decompressed bytes.
- **Index tampering:** ADB and SIG flips are rejected. An identity cannot make an index trusted.
- **Untrusted key:** with only a generated test key trusted, the real index and busybox are rejected. Re-signed with that test key, they verify with it and are rejected by the real key.
- **Damaged input:** every byte of the ADB blocks of acl-dev, busybox and valence-base flipped three ways, plus 9,000 random 4-byte corruptions, read with `allow_untrusted`. There were 27,132 runs and no panic.
- **Writer facts survey:** see the last section.

**Against apk itself** (`vpm/tools/compare-with-apk.py`; run it in WSL with `python3 -I`, about 11 s):
1. The index from `apk adbdump --format json` and from `vpm dump-index --json` are identical: 1102 packages, 14,406 fields, 7,230 dependency entries. They are also identical to apk's runtime view, `apk query --available --fields all` (run in an empty-database test root), for every field of all 1102 packages. apk's `download-url` was `<index dir>/name-version.apk` for every package.
2. All 1102 packages: `apk adbdump --format json X.apk` and `vpm dump --json X.apk` are identical. That covers 28,226 directories, 148,986 files, 22,861 symlinks, 629 hard links and 25 scripts: info, paths, ACLs, sizes, mtimes, hashes, targets and scripts.
3. 62 packages extracted with `apk extract` match `vpm manifest` exactly. There are 14,314 entries, compared on type, mode, owner, size, mtime, content SHA-256 and link target. The set includes busybox, glibc, glibc-dev, glibc-locales, valence-base, all 11 linux-firmware packages, 8 `-dev`, 8 `-doc`, packages with hard links and packages with scripts. apk gave no warnings.

**Speed, measured once:**
- `verify` of all 1102 packages in one process: vpm 8.9 s and about 7 MB per package; apk 12.5 s. vpm took 10.9 s with flate2's default miniz_oxide backend, which is why I switched to zlib-rs.
- The rust package, 215 MB of content: vpm 0.47 s, apk 0.69 s.

## Spec points

No statement in the spec turned out wrong on the real files. Code paths that the real data does not reach (zstd, extended block headers, device nodes, xattrs, root-level hard links written as `./name`) are covered only by synthetic tests. What the real files showed:
- **Compression and signatures:** all 1103 files start with `ADBd` (§B3.3). Each has exactly one SIG block with sign_ver 0, hash_alg 4 and a 512-byte RSA-4096 signature, key id `806a7390d8a56e35c483482ac6e095d3` (§C1, §C3 key id rule confirmed).
- **Array tags:** every array is tagged OBJECT: the index's packages, depends, paths and files (§D1.2). A reader that went by the tag would fail.
- **The package's own `hashes`:** in all 1102 packages it is the 20-byte unique id, equal to SHA-256 of the ADB block with those 20 bytes zeroed, first 20 bytes (§D10.1). So `doc/apk-package.5.scd` is wrong here, as §H1 says.
- **Index `hashes`:** the SHA-256 of the package's ADB block payload, for all 1102 (§D10.2).
- **Root directory:** an entry for it is present in every package (default compat, §D7.3).
- **Files:**
  - all 87 empty files carry SHA-256 of nothing;
  - all 629 hard links carry their target's size and hash, and none uses the `./` target quirk (§E2, §H2.3 not exercised);
  - every symlink's size equals its target's length, with no hash.

Decisions I took where apk is lenient or the spec leaves a choice:
- **Limits:**
  - ADB payload must be 16 to 0x0fffffff bytes (apk's stream path);
  - SIG payload at most 128 KiB − 8 bytes;
  - `ADBc` with algorithm 0 is rejected.
- **Compressed stream:** a truncated deflate stream or bytes after it are errors; apk reads both as a clean end of file.
- **Value errors:** out-of-range or wrong-typed values are errors, where apk reads them as 0, NULL or empty.
- **Rejected entries:**
  - a mode above 07777;
  - a regular file without a 20, 32 or 64-byte hash (checked when the package is read);
  - duplicate paths;
  - an entry below a path that the package itself makes a non-directory (the symlink-escape guard);
  - a hard link not pointing at an earlier regular file of the package;
  - a NUL inside a link target;
  - paths over PATH_MAX.
- **Hard link targets:** a leading `./` is removed.
- **Absent values:** NULL array items are skipped; an absent user or group stays `None`.
- **20-byte identity:** it cannot be checked, so it does not establish trust; a signature is then required.
- **Signatures:** every SIG block is tried even when the identity already matched, so the caller always learns who signed the file.
- **Keys:** they are loaded from both `etc/apk/keys` and `lib/apk/keys`, as apk does (§C4.1); the task named only the first. Non-RSA keys are skipped and listed in `TrustedKeys::skipped`.

## Not done yet
- Checksum dependencies (`><`, op 6) are not matched against package identities.
- Package URLs and `pkgname-spec` substitution (§G1.6) are not done.
- Repository files and the world file are not read; the dependency parser they will need exists.
- No fetching. The whole install engine is the next stage.
