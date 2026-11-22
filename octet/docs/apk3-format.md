# apk-tools 3.0.8 binary formats: a specification for a Rust reader

Scope: the v3 repository index (`Packages.adb`), v3 package files (`.apk`), signature verification and trusted keys, and version and dependency matching, as implemented by apk-tools 3.0.8.

## 0. About this document

- **Sources.** Everything was checked against the C sources under `apk-tools-3.0.8/src` and the documentation under `apk-tools-3.0.8/doc`. A path with no directory is in `src/`; `doc/...` paths are documentation. Line numbers are for 3.0.8. Citations use `file:function:lines`, `file:lines (function)` or `file:lines`.
- **How it was assembled.** Five separately verified parts were merged:
  - container, compression and the ADB value encoding (Part B);
  - signatures and trusted keys (Part C);
  - the index and package schemas (Part D);
  - file contents, DATA blocks and extraction (Part E);
  - versions, dependencies and repositories (Parts F and G).

  Text that two or more parts shared appears once, with the union of their details and citations. Each part's verification log ("corrections made") is not repeated, because its content is already in the text. Each part's open points are kept where they arise and are collected in Part H.
- **Markers.**
  - **[lib]**: behaviour of OpenSSL or mbedTLS, not of apk code. A reimplementer should test it.
  - **[unconfirmed]**: not confirmed from the source, or traced through the code but not run.
  - **Reader rule**: what apk's own reader accepts or rejects.
  - **Writer fact**: what apk's writers (`mkpkg`, `mkndx`, `convndx`, `convdb`, `adbgen`, `adbsign`) produce. Writer facts tell you what real files look like; apk's reader does not enforce them.
  - **Recommendation**: what a Rust reader should do where the C code is lenient or inconsistent.
- **Byte order.** All multi-byte integers on disk are **little-endian**. The code reads every one through `le16toh`/`le32toh`/`le64toh` and writes it through the matching `htole*`. The one exception is a writer bug in `adb_w_copy` (§B7.5).
- **Platform values.** `S_IFxxx` and `PATH_MAX` are not defined by apk; they come from the system headers. The values given here are the Linux ones. `apk_defines.h` does not define `PATH_MAX`; Linux uses 4096.
- **Error codes** (`-APKE_*`) are internal and never on disk. Their numeric values (§A3.3) matter only if you want to reproduce apk's error reporting.

---

## Part A. Quick start

### A1. Reading a package, step by step

1. **Detect v3.** Read at least 4 bytes; fewer is an error. If bytes 0..3 are `ADB` (`41 44 42`), the file is v3. Anything else is a v2 tar.gz package, which this document does not cover (`extract_v3.c:apk_extract:319-330`). See §B2.
2. **Strip the compression prefix.** Byte 3 selects it (`adb_comp.c:adb_decompress:82-119`; §B3):
   - `.` (0x2E): uncompressed. Nothing is consumed; the 4 bytes are the file-header magic.
   - `d` (0x64): raw RFC 1951 deflate starts at byte 4.
   - `c` (0x63): byte 4 is the algorithm (1 = deflate, 2 = zstd), byte 5 is the level (ignored on read), and the compressed data starts at byte 6.
   - anything else: error.

   Recommendation: require a complete deflate stream or zstd frame, and reject bytes after it.
3. **Read the 8-byte file header** at decompressed offset 0: LE u32 magic `0x2e424441` (`"ADB."`), then LE u32 schema. A package has schema `0x676b6370` (`"pckg"`). Keep the 4 raw schema bytes, because they are part of the signed message. See §B4.
4. **Walk the blocks** from decompressed offset 8. Each block is a 4-byte header (16 bytes when the top two bits of the first u32 are both set), then the payload, then 0..7 zero bytes of padding, so that the block occupies a multiple of 8 bytes. The only legal order is `ADB SIG* DATA*`. Use checked arithmetic, require `rawsize >= hdrsize`, require the padding to be present, and reject any bytes after the last block. See §B5 and §B10.
5. **Take the ADB block payload `P`.** apk requires `16 <= len(P) <= 0x0fffffff` and `P[0] == 0` (`adb_compat_ver`). Every value offset is relative to `P[0]`. The root value is the LE u32 at `P[4..8]`. See §B6 and §B7.
6. **Establish trust before the first DATA block**, and in any case by the end of the file. Any one of these is enough (§B5.2, Part C):
   - the caller's allow-untrusted policy;
   - **identity**: you know the package's `hashes` value from a signed index, and it is 32 bytes (SHA-256) or 64 bytes (SHA-512). Compute that digest over `P`. A match makes the file trusted. A mismatch is fatal, even with allow-untrusted. apk labels a 20-byte `hashes` value SHA1 and skips the identity check for it (§C5.3, §D10.5);
   - **a SIG block that verifies**:
     - payload ≥ 18 bytes and `sign_ver` (byte 0) = 0;
     - the key ID (bytes 2..18) equals `SHA512(DER RSAPublicKey)[0..16]` of a trusted key;
     - `hash_alg` (byte 1) ∈ {3 SHA-256, 4 SHA-512, 5 SHA-256 truncated to 20 bytes} gives `md = digest(P)`;
     - `M = schema_le32 ‖ sig_payload[0..18] ‖ md`;
     - `sig_payload[18..]` verifies as RSASSA-PKCS1-v1_5 with SHA-512 over `M`.

     A SIG block that fails any of these is skipped, not fatal (§C1 to §C5).
7. **Decode the root object** with `schema_package` (§D6): slot 1 `info` (pkginfo, §D4), slot 2 `paths` (array of directories, §D7), slot 3 `scripts` (§D9), slot 4 `triggers` (strings), slot 5 `replaces-priority`.
   - Decide array versus object from the schema, never from the type tag: both `0xD…` and `0xE…` tags occur (§D1.2).
   - Absent slots read as NULL, 0 or empty. Ignore unknown slots. Guard against reference cycles (§B7).
8. **Validate the file tree before using anything** (`extract_v3.c:apk_extract_v3_validate_filetree:141-159`). Every non-empty directory name must pass the pathname rule, and every file name must pass the filename rule. An absent or empty file name is rejected. Failure is `-APKE_FORMAT_INVALID`. See §E7.2.
9. **Walk the entries in stored order**: `paths[1..n]`, and within each directory `files[1..m]` (§D7, §D8, §E8). Classify each file entry (§E2):
   - `target` present: symlink, hardlink, device or FIFO, decoded from the target blob's LE u16 type (§D8.2). It never has a DATA block.
   - `target` absent: regular file. Its `hash` must be 20, 32 or 64 bytes.

   A file needs a DATA block **iff** `size != 0` and `target` is absent.
10. **Match DATA blocks one-to-one to those files, in that order** (§E1).
    - Payload: LE u32 `path_idx`, LE u32 `file_idx` (both 1-based; they must equal the current directory and file index), then exactly `size` content bytes.
    - Hash the content with the algorithm given by the hash length (20 SHA-1, 32 SHA-256, 64 SHA-512) and compare (§E3).
    - A DATA block that matches no pending file, or a pending file with no DATA block at the end, is an error (`-APKE_ADB_BLOCK`).
11. **Apply the metadata** (§E4 to §E6):
    - mode = `acl.mode` OR the type bits. Recommendation: mask `acl.mode` with 07777.
    - `user` and `group` are names, resolved against the target root's passwd and group files: `root` gives 0, an unresolved name gives 65534.
    - xattrs are `name\0value`, split at the first NUL.
    - mtime is applied, except to symlinks and directories.
    - Ancestor directories that have no `paths` entry must be created. apk's installer creates them as 0755 root:root.
12. **Scripts are not in DATA blocks.** They are blobs in the `scripts` object. Triggers are strings (§D9).

### A2. Reading an index, step by step

1. **Locate the index** (§G1.2 to §G1.5).
   - A repository line `v3 <url>` uses `<url>/<primary-arch>/Packages.adb`.
   - A line whose raw URL word ends in `.adb` or `.tar.gz`, or that uses the `ndx` keyword, names the index file directly.
   - A bare URL with no keyword is a **v2** repository (`<url>/<arch>/APKINDEX.tar.gz`). The format is detected by content, so an ADB file served there still loads.
2. **Container**: steps 1 to 5 of §A1. The schema must be `0x78646e69` (`"indx"`). An index has no DATA blocks, so the grammar is `ADB SIG*`; a DATA block in a non-`pckg` file is `-APKE_ADB_SCHEMA` (`extract_v3.c:225`).
3. **Trust.** An index is trusted only through a SIG block that verifies (§C5.2), or through allow-untrusted. None of apk's identity-verification call sites is on the index-loading path (§C5.3), so an unsigned index ends in `-APKE_SIGNATURE_UNTRUSTED`.
4. **Root object** (`schema_index`, §D2): slot 1 `description` (string), slot 2 `packages` (array of pkginfo objects, usually OBJECT-tagged), slot 3 `pkgname-spec` (optional string). A NULL root reads as an empty object.
5. **Each `packages` item is a pkginfo object** (§D4): 1 name, 2 version, 3 hashes, 4 description, 5 arch, 6 license, 7 origin, 8 maintainer, 9 url, 10 repo-commit, 11 build-time, 12 installed-size, 13 file-size, 14 provider-priority, 15 depends, 16 provides, 17 replaces, 18 install-if, 19 recommends, 20 layer, 21 tags. Ignore unknown slots. Do not rely on sort order or uniqueness.
6. **The package identity is `hashes`** (§D10).
   - Length < 20: apk skips the entry ("packages without hash").
   - Length 20 means SHA1-class, 32 means SHA-256, 64 means SHA-512. Any other length is skipped too.
   - For a v3 package, mkndx stores the SHA-256 (default) of the package's ADB block payload, or its first 20 bytes (`--hash sha256-160`). A 20-byte value cannot be interpreted from the index alone (§D10.2, §H2).
   - apk keys packages by the first 20 bytes. The first copy seen wins; later copies only add their repository bit (§F4.1).
7. **Dependencies** (`depends`, `provides`, `install-if`, `recommends`; apk reads `replaces` only from the package, at install time). Decode each dependency object with the `apk_dep_from_adb` rule (§F3.5):
   - `op = match`, or 0 if absent;
   - if `version` is absent, `op |= 7`;
   - otherwise, if `op == 0`, `op = 1`.

   Names are opaque bytes; the prefixes `so:`, `pc:` and `cmd:` have no special encoding.
8. **Arch filter**: a package whose `arch` is not `""`, not `noarch` and not one of the configured arches is uninstallable (§F4.1).
9. **Package URL** (§G1.6): `url_base + "/" + subst(spec)`, or `subst(spec)` alone when the spec contains `://`.
   - The spec is the index's `pkgname-spec` if present.
   - Otherwise it is `${arch}/${name}-${version}.apk` for a repository given by base URL, or `${name}-${version}.apk` for a direct index URL (relative to the index's directory).
   - `${arch}` is the package's own `arch` field.
10. **After fetching a package**, verify it against `hashes` as in step 6 of §A1 (§D10.5).

### A3. Constants

#### A3.1 Digest algorithm IDs (`apk_crypto.h:16-27`, `crypto.c`)

| Name | Value | Digest length (bytes) | String in `apk_digest_str[]` (`crypto.c`) | Notes |
|---|---|---|---|---|
| `APK_DIGEST_NONE` | `0x00` | 0 | `"none"` | |
| *(MD5)* | `0x01` | – | NULL slot | `// #define APK_DIGEST_MD5 0x01 // support removed` (`apk_crypto.h:17`). `apk_digest_alg_str(1)` returns NULL, not `"unknown"` (`crypto.c:11-17`). |
| `APK_DIGEST_SHA1` | `0x02` | 20 | `"sha1"` | |
| `APK_DIGEST_SHA256` | `0x03` | 32 | `"sha256"` | |
| `APK_DIGEST_SHA512` | `0x04` | 64 | `"sha512"` | |
| `APK_DIGEST_SHA256_160` | `0x05` | 20 | `"sha256-160"` | The first 20 bytes of SHA-256. Both backends map it to SHA-256 (`crypto_openssl.c:apk_digest_alg_to_evp` 90-91; `crypto_mbedtls.c:apk_digest_alg_to_mbedtls_type` 50-51). The length is then cut to 20 by `apk_digest_alg_len` / `apk_digest_set`. |

- `APK_DIGEST_LENGTH_MAX` = 64. `APK_BLOB_DIGEST_BUF` = 2 + 2·64 = 130 (`apk_crypto.h:27,31`). `struct apk_digest { uint8_t alg, len; uint8_t data[64]; }` (`apk_crypto.h:36-39`). `APK_DIGEST_LENGTH_SHA1` = 20.
- `apk_digest_alg_len(alg)` returns 0 for any unknown value (`crypto.c:28-37`).
- **Algorithm by length.** `apk_digest_alg_by_len(len)` (`crypto.c:39-48`) maps:
  - 0 → NONE
  - 20 → **SHA1** (never SHA256_160)
  - 32 → SHA256
  - 64 → SHA512
  - anything else → NONE

  `apk_digest_from_blob` (`crypto.c:50-59`) uses this mapping. Every digest stored without an algorithm ID (index `hashes`, file `hash`) is interpreted this way, so a 20-byte value is always labelled SHA1 (§D10.3).
- `apk_digest_cmp` (`apk_crypto.h:49-52`) requires the same algorithm and compares with `memcmp` over the computed length.

#### A3.2 Format constants

| Constant | Value | Source |
|---|---|---|
| `ADB_FORMAT_MAGIC` | `0x2e424441` (on-disk bytes `41 44 42 2E` = `"ADB."`) | `adb.h:48` |
| `ADB_SCHEMA_ANY` | `0` | `adb.h:49` |
| `ADB_SCHEMA_IMPLIED` | `0x80000000` | `adb.h:50` |
| `ADB_SCHEMA_INDEX` | `0x78646e69` (`"indx"`) | `apk_adb.h:5` |
| `ADB_SCHEMA_PACKAGE` | `0x676b6370` (`"pckg"`) | `apk_adb.h:6` |
| `ADB_SCHEMA_INSTALLED_DB` | `0x00626469` (`"idb\0"`) | `apk_adb.h:7` |
| `ADB_BLOCK_ALIGNMENT` | 8 | `adb.h:58` |
| `ADB_BLOCK_ADB` / `SIG` / `DATA` / `EXT` / `MAX` | 0 / 1 / 2 / 3 / 4 | `adb.h:59-63` |
| `ADB_MAX_SIGNATURE_LEN` | 2048 (used only by the writer, `adb.c:1322`) | `adb.h:102` |
| `ADB_TYPE_MASK` | `0xf0000000` | `adb.h:15-45` |
| `ADB_VALUE_MASK` | `0x0fffffff`; also the stream reader's maximum ADB block payload (§B6) | `adb.h:28` |
| `ADBI_NUM_ENTRIES` / `ADBI_FIRST` / `ADBI_MAX` | `0x00` / `0x01` / `0x1000000` | `adb.h:43-45` |
| `ADB_COMP_NONE` / `DEFLATE` / `ZSTD` | `0x00` / `0x01` / `0x02` | `adb.h:310-312` |
| `APK_ISTREAM_ALIGN_SYNC` | 8 | `io.c:37` |
| `apk_io_bufsize` | 131072 (128 KiB) | `io.c:39` |
| mbedTLS `APK_KEYFILE_MAX_LENGTH` | 64000 | `crypto_mbedtls.c:44` |
| `APK_MAX_SCRIPT_SIZE` | 262144 (v2 path only, §D9) | `apk_defines.h:145` |
| `APK_MAX_REPOS` | 32 | `apk_defines.h:146` |
| `APK_MAX_TAGS` | 16 | `apk_defines.h:147` |

Value type tags are in §B7.1, dependency match bits in §F2.1, and the schema field numbers in Part D.

#### A3.3 Error codes (internal only, never on disk)

The `enum` at `apk_defines.h:35-105` starts at `APKE_FIRST_VALUE = 1024`. Functions return the negated value. Values were confirmed for these codes; other `APKE_*` codes named in this document (for example `APKE_FORMAT_INVALID`, `APKE_FILE_INTEGRITY`, `APKE_DEPENDENCY_FORMAT`, `APKE_REPO_SYNTAX`) were not enumerated.

| Error | Value |
|---|---|
| `APKE_EOF` | 1025 |
| `APKE_CRYPTO_ERROR` | 1051 |
| `APKE_CRYPTO_NOT_SUPPORTED` | 1052 |
| `APKE_CRYPTO_KEY_FORMAT` | 1053 |
| `APKE_SIGNATURE_GEN_FAILURE` | 1054 |
| `APKE_SIGNATURE_UNTRUSTED` | 1055 |
| `APKE_SIGNATURE_INVALID` | 1056 |
| `APKE_ADB_COMPRESSION` | 1063 |
| `APKE_ADB_HEADER` | 1064 |
| `APKE_ADB_VERSION` | 1065 |
| `APKE_ADB_SCHEMA` | 1066 |
| `APKE_ADB_BLOCK` | 1067 |
| `APKE_ADB_SIGNATURE` | 1068 |
| `APKE_ADB_INTEGRITY` | 1069 |
| `APKE_BUFFER_SIZE` | 1090 |

---

## Part B. Container, compression and the ADB value encoding

### B1. Layering overview

```
file on disk
 ├─ [optional compression prefix: 4 bytes "ADBd", or 6 bytes "ADBc"+alg+level]   (adb_comp.c)
 │   ("ADB." files have no prefix; the 4 bytes are the file header itself)
 └─ (decompressed) ADB stream:
      ├─ adb_file_header  (8 bytes: "ADB." + schema u32)                (adb.c __adb_m_mmap / __adb_m_stream)
      └─ blocks…  each = block header (4 or 16 bytes) + payload + 0..7 zero bytes of padding
           ├─ exactly one ADB block, first: payload = adb_hdr (8 bytes) + value heap;
           │     value offsets are relative to payload byte 0
           ├─ SIG* (0..n)
           └─ DATA* (0..n): meaningful only in packages; apk_extract rejects them for other schemas
      Nothing may follow the last block (§B5.2).
```

### B2. Telling v3 from v2 (`extract_v3.c:apk_extract:319-330`)

- `apk_extract` peeks 4 bytes. A stream shorter than 4 bytes is an error.
- If the first **3** bytes are `"ADB"` (`41 44 42`), the file goes to `apk_extract_v3`. Anything else goes to the v2 (tar.gz) path, `apk_extract_v2`.
- `apk_extract_v3` (`extract_v3.c:273-317`) calls `adb_m_process(&ctx.db, adb_decompress(is, 0), ADB_SCHEMA_ANY, trust, ectx, apk_extract_v3_data_block)` (`extract_v3.c:289-290`).
- Consumers include:
  - package install and fetch: `database.c:713`, `3162`;
  - repository index loading: `database.c:load_index:1520-1529`, `1661`, `2683`;
  - `verify`, `manifest`, `extract`, `convndx`, and `mkndx` (for input packages).
- After parsing, the file header's schema selects the handling (`extract_v3.c:291-310`):
  - `ADB_SCHEMA_PACKAGE` leads to file extraction (§E8).
  - `ADB_SCHEMA_INDEX` leads to the `v3index` callback.
  - Any other schema, including `idb`, returns `-APKE_ADB_SCHEMA`.
  - Then `-ECANCELED` is mapped to 0. After that, if the ADB payload length is 0, the result is `-APKE_ADB_BLOCK` (`extract_v3.c:311-312`). In practice this fires only on the mmap path (§E8).
- The format is detected by content, not by file name or repository type (§G1.5). The repository index file name for a v3 repository is `"Packages.adb"` (`repoparser.c:194-196`).

### B3. The compression prefix

#### B3.1 Prefix bytes (`adb_comp.c:adb_decompress:82-119`)

- `adb_decompress` peeks 4 bytes. A stream shorter than 4 bytes gives an error, normally `-APKE_EOF` from `apk_istream_peek` (`io.c:163-178`).
- If the first 3 bytes are not `"ADB"`, the result is `-APKE_ADB_HEADER`.
- The 4th byte selects the format:

| Byte 3 | Bytes on disk | Bytes consumed by adb_decompress | Meaning |
|---|---|---|---|
| `'.'` (0x2E) | `41 44 42 2E …` | **none** (peek only) | Uncompressed. The stream is returned unchanged, so `"ADB."` is read again as the file-header magic. Reported spec = {alg 0, level 1}. |
| `'d'` (0x64) | `41 44 42 64` + deflate data | 4 | Raw deflate, no level byte. Reported spec = {alg 1, level 0}. |
| `'c'` (0x63) | `41 44 42 63` + `alg:u8` + `level:u8` + compressed data | 4 + 2 | Generic form. `struct adb_compression_spec { uint8_t alg; uint8_t level; }` (`adb.h:302-305`) is read raw: byte 4 = alg, byte 5 = level. |
| other | | | `-APKE_ADB_COMPRESSION` |

Algorithm IDs (`adb.h:310-312`): `ADB_COMP_NONE = 0x00`, `ADB_COMP_DEFLATE = 0x01`, `ADB_COMP_ZSTD = 0x02`.

`compression_info_by_alg` (`adb_comp.c:51-55`) rejects `alg >= ARRAY_SIZE(compression_infos)`:
- The array has 3 entries only when built with `HAVE_ZSTD` (`adb_comp.c:19-37`).
- zstd is a build-time option (`src/meson.build:112-115`, set when libzstd is found). Without it, alg 2 gives `-APKE_ADB_COMPRESSION`.
- alg ≥ 3 is always rejected.

**The level byte is ignored when reading.** It only matters to the writer.

#### B3.2 Algorithms

**Deflate is raw RFC 1951 deflate**, with no zlib or gzip header and no checksum trailer:
- `apk_istream_deflate` is `apk_istream_zlib(is, raw=1, NULL, NULL)` (`apk_io.h:246-248`).
- `window_bits(15, raw)` returns `-15` (`io_gunzip.c:149-153`).
- `inflateInit2(&zs, -15)` is called at `io_gunzip.c:175`.
- The writer is `apk_ostream_deflate(os, level)` = `apk_ostream_zlib(os, 1, level)` (`apk_io.h:254-256`), which calls `deflateInit2(level ?: 9, Z_DEFLATED, -15, 8, Z_DEFAULT_STRATEGY)` (`io_gunzip.c:256`). Level 0 therefore means zlib level 9.

**Zstd uses standard zstd frames**, decoded with `ZSTD_decompressStream` (`io_zstd.c:zi_read:36-82`):
- The loop keeps calling `ZSTD_decompressStream` while input remains, so the C reader also accepts several concatenated frames. That is standard libzstd streaming behaviour.
- The writer sets `ZSTD_c_compressionLevel = level ?: 9` (`io_zstd.c:237`) and finishes with `ZSTD_e_end` (`io_zstd.c:zo_close:166-191`), which yields one frame. It may use up to 6 worker threads (`io_zstd.c:220-246`); that does not change the frame format.

Behaviour of the C decoders that a Rust reader may handle more strictly:

- **Deflate, after the end of the stream.** At `Z_STREAM_END` the C code re-initialises with `inflateInit2(…, 15+32)`, which auto-detects gzip or zlib (`io_gunzip.c:112-114`).
  - Bytes after the raw deflate stream are parsed as a gzip/zlib member. A valid member would be decoded and appended to the output.
  - Garbage gives `-APKE_FORMAT_INVALID` (`io_gunzip.c:123-124`).
  - Writers never emit trailing bytes.
  - **Recommendation:** treat the deflate end of stream as the end of data, and reject trailing bytes.
- **Truncated deflate input** gives `Z_BUF_ERROR` once input is exhausted, which the C code treats as plain EOF, not an error (`io_gunzip.c:119-122`).
- **Truncated zstd input** is likewise reported as EOF (`io_zstd.c:52-55`).
  - In both cases the truncation is caught only by the ADB layer: a short header or payload read, or missing padding (§B8). If it falls exactly on a block boundary, it is caught only by a higher-level check (untrusted, or a missing DATA block).
  - **Recommendation:** require a complete deflate stream (BFINAL block seen) or a complete zstd frame.
- **Error codes.** zstd decode errors set `-EIO` (`io_zstd.c:64-66`). Deflate data errors give `-APKE_FORMAT_INVALID` (`io_gunzip.c:124`).
  - Both errors surface on the **next read** from the decompressed stream: `gzi_read` returns the error; `zi_read` sets `is->err`, which `apk_istream_read_max` and `apk_istream_peek` check (`io.c:101,167`).
  - They are also propagated by `apk_istream_close_error` at the end of `__adb_m_stream`.

#### B3.3 What the writers emit (`adb_comp.c:adb_compress:121-152`, `adb_parse_compression:57-80`)

The output depends on the compression spec. The default `ac->compspec` is all zeros (`context.c:apk_ctx_init:18` memsets the context).

- **{alg 0, level 0} (the default):** replaced by {DEFLATE, 0}. The output is `"ADBd"` followed by raw deflate at zlib level 9.
  - This is the default for `mkpkg` (`app_mkpkg.c:545`), `mkndx` (`app_mkndx.c:358-359`), `convndx` (`app_convndx.c:72-73`) and `convdb` (`app_convdb.c:222-224`).
- **{NONE, 1}** (from `--compression none`): no prefix. The file starts directly with `"ADB."`.
- **{DEFLATE, level ≠ 0}:** `"ADBc"`, then `01`, then the level byte, then raw deflate. `deflate` and `deflate:0` both give {DEFLATE, 0}, which is written as `"ADBd"`.
- **{ZSTD, level}:** `"ADBc"`, then `02`, then the level byte (0..22), then zstd data. `zstd` without a level gives level byte `00`, compressed at zstd level 9.
- **`adbsign`** re-emits the input's own spec as returned by `adb_decompress`, unless `--compression` was given (`app_adbsign.c:97-99`).
  - An `"ADB."` input gives {NONE, 1}, so the output stays uncompressed.
  - An `"ADBd"` input gives {DEFLATE, 0}, so the output stays `"ADBd"`.
- **`adbgen`** always writes uncompressed (`"ADB."`). It calls `adb_c_create` on stdout without `adb_compress` (`app_adbgen.c:110`, `329-333`).

The `--compression` option (`apk.c:281-282`) accepts `none`, `deflate[:0-9]` and `zstd[:0-22]` (the last only when built with zstd). The level is parsed with `strtol(…, 0)`, so hex and octal forms are accepted. `none:N` is an error, and so is a trailing `:` with no number.

#### B3.4 What the decompressed stream starts with

The compressor wraps the **entire** ADB file:
- `adb_compress` writes the prefix and returns the compressing stream.
- `adb_c_adb` writes the file header (`adb_c_header`, `adb.c:1200-1207`) and the ADB block into that stream, then the SIG blocks (`adb.c:adb_c_adb:1285-1295`).
- `mkpkg` then appends the DATA blocks (`app_mkpkg.c:551-583`).

**The decompressed bytes therefore start again with `"ADB."` and the schema** (§B4). The 4- or 6-byte prefix is not part of the decompressed stream. All block positions, and all hashing and signing, are on the **decompressed** stream: every reader parses after `adb_decompress()` (`extract_v3.c:289`, `app_adbsign.c:97`, `app_mkndx.c:290`). Blocks follow immediately after the 8-byte file header, so every block starts at an 8-aligned offset of the decompressed stream (except nested blocks, §B7.9).

#### B3.5 Prefix edge cases

- **Unchecked reads in the prefix branches** (`adb_comp.c:97,101-102`). The `'d'` and `'c'` branches do not check the return values of `apk_istream_get(is, 4)` and `apk_istream_read(is, &spec, 2)`.
  - `get(4)` cannot fail, because the 4-byte peek already succeeded.
  - A short read of the 2 spec bytes sets `is->err = -APKE_EOF` (`io.c:136-140`) and leaves `spec` partly filled. The failure surfaces later as a read error, or as `-APKE_ADB_COMPRESSION` if the partial alg byte is invalid.
- **`"ADBc"` with alg = 0 (NONE)** installs no decompressor; the stream continues uncompressed after the 6 bytes. The outcome depends on how the file was opened:
  - **mmap'd file:** `apk_istream_mmap` returns the *whole* buffer from offset 0 (`io.c:apk_istream_mmap:90-95`, which ignores `is->ptr`). The header check then sees `"ADBc"` instead of `"ADB."` and fails with `-APKE_ADB_HEADER`.
  - **Non-mmap stream:** the next 8 bytes are read as the file header. This works if they are `"ADB."` + schema.
  - Writers never produce this form. **Recommendation:** reject it, or treat it as "uncompressed after the 6 bytes". The C behaviour is inconsistent.

### B4. ADB file header and schema IDs

```c
struct adb_file_header {   // adb.h:52-55, 8 bytes, no padding
    uint32_t magic;        // LE, must equal ADB_FORMAT_MAGIC
    uint32_t schema;       // LE, schema id
};
#define ADB_FORMAT_MAGIC   0x2e424441   // bytes 41 44 42 2E = "ADB."   (adb.h:48)
#define ADB_SCHEMA_ANY     0            // adb.h:49
#define ADB_SCHEMA_IMPLIED 0x80000000   // adb.h:50
```

Known schema IDs (`apk_adb.h:5-7`, registered in `apk_adb.c:adb_all_schemas:556-561`):

| Constant | u32 value | Bytes on disk | Root object schema | File |
|---|---|---|---|---|
| `ADB_SCHEMA_INDEX` | `0x78646e69` | `69 6E 64 78` = `"indx"` | `schema_index` (§D2) | `Packages.adb` |
| `ADB_SCHEMA_PACKAGE` | `0x676b6370` | `70 63 6B 67` = `"pckg"` | `schema_package` (§D6) | `.apk` |
| `ADB_SCHEMA_INSTALLED_DB` | `0x00626469` | `69 64 62 00` = `"idb\0"` | `schema_idb` (§D11) | `installed.adb` |

`ADB_SCHEMA_INSTALLED_DB` is written by `apk convdb` (`app_convdb.c:206,222-225`, output file `installed.adb`) and can also be produced by `adbgen` from YAML. Nothing in 3.0.8 except the generic `adbdump` walker reads it.

How `expected_schema` is handled (`adb.c:__adb_m_mmap:212-219`, `__adb_m_stream:243-254`, `adb_m_process:345-346`):

- **Header present** (bit `0x80000000` clear):
  - The magic must equal `ADB_FORMAT_MAGIC`, otherwise `-APKE_ADB_HEADER`.
  - A stream shorter than 8 bytes gives `-APKE_ADB_HEADER` on the mmap path, or `-APKE_EOF` from `apk_istream_read` on the stream path.
  - If `expected_schema != 0` and it differs from `hdr.schema`, the result is `-APKE_ADB_SCHEMA`.
  - Then `db->schema = le32toh(hdr.schema)` (`adb.c:217` mmap, `adb.c:253` stream). `ADB_SCHEMA_ANY` (0) accepts any schema value, including unknown ones.
- **`ADB_SCHEMA_IMPLIED`** (bit set): **there is no file header.**
  - Block parsing starts at byte 0, and `db->schema = expected_schema & ~0x80000000`.
  - This is used only for nested ADBs stored inside a value (§B7.9, `adb_walk_adb.c:dump_item:46-53`).
  - The separate entry point `adb_m_blob` (`adb.c:198-202`) also parses headerless blocks, but leaves `db->schema = 0` (`adb_init` memsets). It is used by `wadbcmp`.

### B5. Blocks

#### B5.1 Block header (`adb.h:57-100`)

```c
#define ADB_BLOCK_ALIGNMENT 8
#define ADB_BLOCK_ADB  0
#define ADB_BLOCK_SIG  1
#define ADB_BLOCK_DATA 2
#define ADB_BLOCK_EXT  3
#define ADB_BLOCK_MAX  4
struct adb_block {          // adb.h:65-69; 16 bytes in memory; only the first 4 bytes are on disk for the short form
    uint32_t type_size;     // offset 0, LE
    uint32_t reserved;      // offset 4 (EXT form only)
    uint64_t x_size;        // offset 8, LE (EXT form only)
};
```

Decoding, with `ts = le32(type_size)` (`adb_block_is_ext` `adb.h:82-84`, `adb_block_type` `adb.h:85-87`, `adb_block_rawsize`, `adb_block_hdrsize`):

- **Short form**, when `ts >> 30` is 0, 1 or 2:
  - type = `ts >> 30`
  - rawsize = `ts & 0x3fffffff`
  - header size = **4** bytes
- **EXT form**, when `ts >> 30 == 3`:
  - type = `ts & 0x3fffffff`
  - header size = **16** bytes (`type_size`, `reserved`, `x_size`)
  - rawsize = `le64(x_size)`
  - `reserved` is ignored by readers and written as 0 (compound-literal zero-init in `adb_block_init`).

Derived quantities (`adb.h:88-100`; `ROUND_UP(x,a) = ((x)+(a)-1) & ~((a)-1)`, `apk_defines.h:143`):
- `rawsize` = header size + payload length, i.e. **the header is included**.
- `length` (payload length, `adb_block_length`) = `rawsize - hdrsize`.
- `size` (bytes the block occupies, `adb_block_size`) = `ROUND_UP(rawsize, 8)`.
- `padding` = `size - rawsize`, which is 0..7 bytes.
  - Writers fill it with zeros (`adb.c:14` `padding_zeroes`; `adb_c_block:1209-1227`; `adb_c_block_data:1229-1253`).
  - Readers skip it and never check its content. The mmap path steps over it; the stream path calls `apk_istream_skip` (`adb.c:319-321`).
  - The padding must be physically present. Stream path: EOF while skipping gives `-APKE_EOF` (`io.c:645-659`, `188-198`). mmap path: `adb_block_validate` rejects a block whose padded size exceeds the remaining data (`adb.c:25`).
  - Padding is not part of the payload and is not signed or hashed.
- The payload starts immediately after the header.

Padding depends **only on rawsize**, not on the absolute position. In practice every block starts at a multiple of 8 in the decompressed stream, because the file header is 8 bytes and every block's `size` is a multiple of 8. Nested blocks are the exception: they start at a 4-aligned heap offset (§B7.9).

Writer's choice of form (`adb.h:adb_block_init:71-81`):
- **Short form** if `length <= 0x3fffffff - 4`, with `type_size = (type<<30) + 4 + length`.
- **EXT form** otherwise, with `type_size = (3<<30) + type` and `x_size = 16 + length`. For DATA this is `type_size = 0xC0000002`.
- In practice only DATA blocks for files larger than `0x3fffffff - 4 - 8` = 1,073,741,811 bytes use EXT (the DATA payload is the 8-byte `adb_data_package` header plus the file). ADB payloads cannot get that large (§B6), and SIG blocks are never written in EXT form.

Readers accept the EXT form at any size, carrying type 0, 1 or 2; a reader must not assume it appears only for payloads ≥ 1 GiB. An EXT header whose inner type is 3 or ≥ 4 is rejected by the type check in §B5.2.

`doc/apk-v3.5.scd:123-130,159` names type 3 "DATAX" and says it is illegal. **The code instead uses 3 as the extended-header escape** described above. Follow the code (§H1).

#### B5.2 Block order, the trust gate and the end of the stream

Sources: `adb.c:__adb_m_parse:138-196` (mmap path) and `adb.c:__adb_m_stream:229-334` (stream path). Which path is used is in §B8. The file header (magic, plus schema if `expected_schema != 0`) is checked first, unless `ADB_SCHEMA_IMPLIED` is set (`adb.c:212-219`, `243-254`).

1. **Start.** `trusted := (trust != NULL && trust->allow_untrusted)` (`adb.c:145`, `238`). `allowed := BIT(ADB)`.
2. **Type check.** A block whose `type >= ADB_BLOCK_MAX` (4), or whose bit is not in `allowed`, is a hard `-APKE_ADB_BLOCK` (`adb.c:151-154`, `264-267`). This includes an EXT header whose inner type is 3.
3. **ADB block.**
   - `allowed := SIG|DATA`.
   - Size check (§B6): mmap path, payload ≥ 8 bytes (`adb.c:158-161`); stream path, `16 <= payload <= 0x0fffffff` (`adb.c:273-276`). Otherwise `-APKE_ADB_BLOCK`.
   - `adb_compat_ver` (payload byte 0) must be 0, otherwise `-APKE_ADB_VERSION` (`adb.c:162-165`, `284-287`).
   - The identity check runs (§C5.3; called at `adb.c:167` on the mmap path and `adb.c:288` on the stream path). A return of 1 sets `trusted`. A negative return is a hard error, **even with allow-untrusted**.
4. **SIG block.**
   - `if (!trusted && adb_trust_verify_signature(...) == 0) trusted := true` (§C5.2).
   - Any other result is **silently ignored**. A malformed block, an unknown `sign_ver` or `hash_alg`, an unknown key or a bad signature only means that this block does not establish trust (`adb.c:172-174`, `301-303`).
   - Once `trusted` is true, later SIG blocks are not examined at all.
   - Exception: the stream path first peeks the whole SIG payload, even when the file is already trusted, and a failed peek is fatal (`adb.c:296-300`; limit in §C1.4).
   - `allowed` is unchanged, so any number of SIG blocks may sit between the ADB block and the first DATA block.
5. **DATA block.**
   - `allowed := DATA`. A SIG or ADB block after a DATA block is therefore `-APKE_ADB_BLOCK`.
   - If `!trusted`, the result is `-APKE_SIGNATURE_UNTRUSTED`, **before** the block's callback runs (`adb.c:176-181`, `305-310`).
6. **Grammar.** The result is `ADB SIG* DATA*`: exactly one ADB block, and it comes first. Zero DATA blocks is valid (every index). `doc/apk-v3.5.scd:132-133` says "one or more DATA blocks", which is wrong for the code (§H1).
7. **Trailing bytes after the last block are an error** on both paths. With trailing zero bytes, `ts = 0` decodes as type ADB:
   - on the mmap path its rawsize 0 is below the header size, giving `-APKE_ADB_BLOCK` (`adb.c:24`);
   - on the stream path ADB is no longer allowed, giving `-APKE_ADB_BLOCK`.
   - 1..3 stray bytes give `-APKE_ADB_BLOCK` on both paths (mmap: `adb.c:20-21`; stream: `adb.c:260-261` returns a short positive count, mapped at `adb.c:324`).
8. **End of the stream** (a clean EOF at a block boundary). The checks run in this order:
   1. The mmap path first reports any block-validation error that stopped the iterator (`-APKE_ADB_BLOCK`; `adb.c:190`).
   2. Not trusted gives `-APKE_SIGNATURE_UNTRUSTED`.
   3. No ADB block gives `-APKE_ADB_BLOCK` (`adb.c:187-193`, `324-328`).

   On the stream path the end is detected when the block-header read returns fewer than `hdrsize` bytes. Exactly 0 bytes is a clean end; 1..hdrsize−1 bytes gives `r > 0`, which becomes `-APKE_ADB_BLOCK` (`adb.c:260-261,324`). The untrusted and no-ADB checks apply for `r == 0` **and for `r == -ECANCELED`** from a callback (`adb.c:325-328`).
9. **Callback return values.**
   - A negative return aborts the parse.
   - A positive return is mapped to `-APKE_ADB_BLOCK` (`adb.c:188`, `324`). On the mmap path this only takes effect for the *last* block, because each callback overwrites `r`. On the stream path, positive returns are overwritten by the segment close or the padding skip.
   - `-ECANCELED` (an early stop) differs by path:
     - **Stream path:** the trust and ADB checks still apply, the ADB payload is kept, and `-ECANCELED` is returned (`adb.c:325-333`). `apk_extract_v3` maps it to 0.
     - **mmap path:** it is treated as an error and returned as-is, and `db->adb` is discarded (`adb.c:185,194`; `__adb_m_mmap:222-226`).
   - Any non-zero result clears `db->adb`. On the mmap path the db is also freed (`adb.c:185-194`, `224-226`).

#### B5.3 Block payloads

- **ADB:** the `adb_hdr` and the value heap; §B6 and §B7.
- **SIG:** `u8 sign_ver`, `u8 hash_alg`, `u8 id[16]`, then the signature to the end of the payload; §C1.
- **DATA** (packages only): `u32 path_idx`, `u32 file_idx`, then the raw file content; §E1. The schema must be PACKAGE, otherwise `-APKE_ADB_SCHEMA` (`extract_v3.c:225`).

### B6. The ADB block payload

```c
struct adb_hdr {            // adb.h:104-109, 8 bytes
    uint8_t  adb_compat_ver;   // payload offset 0, must be 0
    uint8_t  adb_ver;          // offset 1, writer 0; never checked (only printed by adbdump, adb_walk_adb.c:125-126)
    uint16_t reserved;         // offset 2, writer 0; ignored
    adb_val_t root;            // offset 4, LE u32 value tag of the root (normally an OBJECT)
};
```

Checks:
- `adb_compat_ver != 0` gives `-APKE_ADB_VERSION` (`adb.c:162-165` mmap, `284-287` stream).
- **Payload length limits differ by path:**
  - mmap path: `b.len < 8` gives `-APKE_ADB_BLOCK` (`adb.c:158-161`). There is no upper bound.
  - stream path: `sz < 16 || sz > 0x0fffffff` (`ADB_VALUE_MASK`) gives `-APKE_ADB_BLOCK` (`adb.c:273-276`).
- **Which path applies in practice.** The mmap path is used only when the source istream has `APK_ISTREAM_SINGLE_READ` (§B8). Every `apk_extract` consumer (package install and verify, index loading) therefore uses the **stream path**.
  - **Recommendation:** accept 16..=0x0fffffff to match what apk itself loads, or 8..=0x0fffffff to be maximally lenient. Offsets are 28 bits, so data beyond 0x0FFFFFFF could only be reached as the tail of a value.
- **The writer can produce an 8-byte payload.** When the root object ends up empty, it is written as `ADB_NULL` and the payload is only the 8-byte header (`adb.c:__adb_w_obj:949-954`, `adb_w_init_dynamic:385-401`). The stream path rejects such a file.
- **Root.** The root is read as `((struct adb_hdr*)adb.ptr)->root`. If the in-memory payload is shorter than 8 bytes, the root is `ADB_NULL` (`adb.c:adb_r_root:421-425`). The root object is read with `adb_r_rootobj` (`adb.c:508-511`).

**Offset base.** Every indirect value's 28-bit offset is a byte offset from **byte 0 of the ADB block payload**, which is the first byte of `struct adb_hdr`, immediately after the 4- or 16-byte block header.
- It is not relative to the file, the block header or the decompressed stream (`adb.c:adb_r_deref:414-419`: `ptr = db->adb.ptr + ADB_VAL_VALUE(v) + offs`).
- The writer starts its buffer with the 8-byte `adb_hdr` at offset 0 (`adb_w_init_dynamic:385-401`), so the first value data is at offset ≥ 8.

**Bounds rule** (`adb_r_deref`): an access of `s` bytes at `ADB_VAL_VALUE(v) + extra` is valid iff `value + extra + s <= payload_length`.
- The arithmetic is in `size_t`.
- The payload length excludes the block padding.
- On failure `adb_r_deref` returns NULL, and the caller returns 0, a NULL blob or an empty object (§B7).

**Alignment.** In the decompressed stream the payload starts at offset 12 (short-form header) or 24 (EXT). Payload-relative 4-byte alignment therefore coincides with stream alignment; 8-byte alignment does not.
- **The reader never checks alignment** (`adb_r_deref`, `adb.c:414-419`). Use unaligned little-endian loads everywhere.

### B7. Values (`adb_val_t`, `adb.h:15-45`)

#### B7.1 Type tags

A value is a 32-bit little-endian tag. Decode `t = le32(raw)`, then:
- type = `t & 0xF0000000` (`ADB_TYPE_MASK`)
- value = `t & 0x0FFFFFFF` (`ADB_VALUE_MASK`, 28 bits)

| Type nibble | Constant | Kind | value field |
|---|---|---|---|
| 0x0 | `ADB_TYPE_SPECIAL` 0x00000000 | direct | 0 = NULL, 1 = TRUE, 2 = FALSE |
| 0x1 | `ADB_TYPE_INT` 0x10000000 | direct | unsigned integer 0..0x0FFFFFFF |
| 0x2 | `ADB_TYPE_INT_32` 0x20000000 | indirect | offset of a u32 |
| 0x3 | `ADB_TYPE_INT_64` 0x30000000 | indirect | offset of a u64 |
| 0x8 | `ADB_TYPE_BLOB_8` 0x80000000 | indirect | offset of a u8 length + bytes |
| 0x9 | `ADB_TYPE_BLOB_16` 0x90000000 | indirect | offset of a u16 length + bytes |
| 0xA | `ADB_TYPE_BLOB_32` 0xA0000000 | indirect | offset of a u32 length + bytes |
| 0xD | `ADB_TYPE_ARRAY` 0xD0000000 | indirect | offset of a slot vector |
| 0xE | `ADB_TYPE_OBJECT` 0xE0000000 | indirect | offset of a slot vector |
| 0xF | `ADB_TYPE_ERROR` 0xF0000000 | — | in-memory writer error sentinel (see below) |

Unassigned and unexpected types:
- Nibbles 0x4–0x7, 0xB and 0xC are unassigned.
- The C accessors treat these, `ADB_TYPE_ERROR`, and any type that does not match the accessor the same way: `adb_r_int` returns 0, `adb_r_blob` returns NULL, `adb_r_obj` returns an empty object.

`ADB_TYPE_ERROR` is meant never to reach disk:
- `adb_wo_val` and `adb_wa_append` refuse it (`adb.c:993`, `1039`).
- `adb_wo_val_fromstring` stores the result of `adb_w_fromstring` without that check, including `ADB_ERROR(...)` values (`adb.c:998-1003`).
- Several callers never check that result: `apk convndx` (`app_convndx.c:31`), `apk convdb` (`app_convdb.c:130`) and `adbgen` (`app_adbgen.c:163`). mkpkg does check (`assign_fields`, `app_mkpkg.c:410-416`).
- By code trace, a malformed v2 field converted by convndx or convdb therefore lands in the output as an `ADB_TYPE_ERROR` value; the exact error code depends on which `fromstring` failed. **[unconfirmed]** No such file was produced to confirm that an error tag reaches disk.
- **Recommendation:** treat a 0xF tag as invalid or unknown, like any wrong type. apk's readers do the same (0, NULL or empty).

#### B7.2 Special

- `ADB_NULL` is the all-zero word `0x00000000`. "Field absent" checks compare the raw slot against exactly this value (`adb_walk_adb.c:30,98`; `adb.c:561`).
- TRUE (`0x00000001`) and FALSE (`0x00000002`) are defined (`ADB_VAL_TRUE`, `ADB_VAL_FALSE`, `adb.h:36-38`) but **not used anywhere in 3.0.8**; a grep finds no use outside `adb.h`. No schema uses them.
- Every special value other than 0 is non-NULL, yet `adb_r_int` returns 0 for it and `adb_r_blob` returns NULL.

#### B7.3 INT (direct)

- `adb_r_int` returns `t & 0x0FFFFFFF` (`adb.c:adb_r_int:432-433`).
- The writer uses INT for values `< 0x10000000` (`adb_w_int:780-791`).
- **0 is encoded as `0x10000000`, which is not NULL**, and it is stored explicitly. For example, mkpkg always writes a file's `size`, even when it is 0.
- Reading an integer from a NULL slot, a non-integer slot or an out-of-bounds INT_32/INT_64 yields 0 (`adb_r_int`, `adb.c:427-445`). For readers, absent and 0 are therefore equivalent.

#### B7.4 INT_32 and INT_64 (indirect)

- **INT_32:** 4 bytes, LE u32, at payload offset `value`. Out of bounds gives 0 (`adb.c:434-437`). The writer uses it for `0x10000000 <= v <= 0xFFFFFFFF`, 4-byte aligned.
- **INT_64:** 8 bytes, LE u64, at `value`. Out of bounds gives 0 (`adb.c:438-441`). It is read through `apk_aligned32_le64`, which assumes 4-byte alignment (`apk_defines.h:172-176`).
- The writer uses INT_64 for `v >= 0x100000000`. **Its alignment depends on the writer path:**
  - `adb_w_int` aligns to 4 (`adb.c:784`).
  - `adb_w_copy` aligns to 8 (`adb.c:807-809`). `mkndx` uses `adb_w_copy` when copying pkginfo (`app_mkndx.c:194,313`).
- Readers accept any width for any magnitude, for example an INT_32 or INT_64 holding a small number.
- Integers are unsigned only; the result type is `uint64_t`. All schema integers are `u64` at the format level.
- Any other type passed to `adb_r_int` (blob, object, …) returns 0 (`adb.c:442-443`).

#### B7.5 BLOB_8, BLOB_16, BLOB_32 (indirect; `adb.c:adb_r_blob:447-471`)

| Type | Length prefix at `off` | Data starts at | Writer uses it for | Writer alignment of `off` |
|---|---|---|---|---|
| BLOB_8 | u8 | `off+1` | 1..=255 bytes | 1 |
| BLOB_16 | u16 LE | `off+2` | 256..=65535 bytes | 2 |
| BLOB_32 | u32 LE | `off+4` | ≥ 65536 bytes; also every nested ADB, whatever its size (§B7.9) | 4 |

Reader behaviour:
- A blob read from a NULL slot or any non-blob type gives `APK_BLOB_NULL` (ptr NULL, len 0; `adb.c:468-470`).
- If the length prefix is out of bounds, the result is `APK_BLOB_NULL`.
- If the prefix is in bounds but the data range `[off+P, off+P+len)` is not, the result is `{ptr=NULL, len=len}`: ptr NULL but the **declared, possibly nonzero, length**. `APK_BLOB_IS_NULL` checks only `ptr` (`apk_blob.h:25`), so apk counts this as NULL. The effect depends on the field:
  - an out-of-range `target` is treated as absent;
  - an out-of-range file `hash` of length 20, 32 or 64 is passed to `memcpy` from NULL (`crypto.c:50-59`);
  - an out-of-range `name` is dereferenced by `apk_fs_is_malicious_filename` (`fs_fsys.c:266-269`).

  Neither of the last two is handled gracefully. **Recommendation:** reject any out-of-range blob as an error.
- Any width is accepted for any length (for example a BLOB_32 holding 3 bytes).
- **Zero-length blobs.** A length of 0 yields an empty, **non-NULL** blob, provided `off+P <= payload_len`. apk's reader can tell this apart from NULL:
  - a present empty `target` is "not NULL", so it fails `target.len < 2` with `-APKE_ADB_SCHEMA`;
  - a present empty file `name` is rejected as malicious;
  - a present empty directory `name` behaves like the root (`pb.namelen == 0`).
- **The writer never creates empty blobs.** `adb_w_blob_vec` (`adb.c:748-764`) returns `ADB_VAL_NULL` when the total size is 0 (check at `adb.c:756-762`), and `adb_wo_val` then stores NULL without growing the object (`adb.c:990-996`). On the writer side an empty string and an absent field are therefore the same: empty names, descriptions and similar values are never stored (the root directory's `name` is absent, for example).
- Blobs carry no terminator, encoding or type information.

Writer bug (affects big-endian hosts only): `adb_w_copy` reads BLOB_16 and BLOB_32 length prefixes without `le16toh`/`le32toh` (`adb.c:820,826`). On a big-endian host, copied blobs would get a wrong copy size. Little-endian hosts are unaffected. **[unconfirmed]** Impact on real files.

#### B7.6 ARRAY and OBJECT (indirect; `adb.c:adb_r_obj:473-506`)

The layout at payload offset `off` is a vector of `num` little-endian u32 slots:

```
off+0      : slot[0] = num   (u32 LE, total slot count INCLUDING slot 0)   ADBI_NUM_ENTRIES = 0
off+4*i    : slot[i] = adb_val_t, for i = 1 .. num-1                        ADBI_FIRST = 1
```

Validation:
- The type must be ARRAY or OBJECT. **`adb_r_obj` accepts either tag in either role** (`adb.c:478-480`). Writers routinely tag arrays as OBJECT (§D1.2), so **a reader must decide array versus object from the schema, never from the tag.**
- 4 bytes at `off` must be in bounds.
- `num` must be in `1..=ADBI_MAX`, where `ADBI_MAX = 0x1000000` (`adb.h:45`, check at `adb.c:486`).
- `4*num` bytes at `off` must be in bounds.
- On any failure the result is an "empty" object, `{num = 1, obj = NULL}` (`adb.c:498-505`). Every field reads as NULL and the array has 0 items. **No error is reported.** Absent arrays therefore read as empty arrays.
- `num == 1` is a valid empty vector. Readers accept it, but writers never emit it.

Slot meaning:
- **Object:** slot `i` holds field `i`. The schema's `ADBI_*` constants are 1-based and map directly to slots (`ADB_FIELD(_i,…)` maps to `fields[_i-1]`, `adb.h:130`). A schema's `num_fields` is `ADBI_*_MAX`, one more than the highest field index.
- **Array:** items are slots `1..num-1`. The item count is `adb_ra_num = (num ?: 1) - 1` (`adb.h:220`).
- **Missing fields read as NULL:** `adb_ro_val(o, i)` returns `ADB_NULL` when `i >= num` (`adb.c:523-527`). A slot can also hold `0x00000000` explicitly. Objects are often shorter than their schema (§B7.7).
- **Unknown fields are ignored.** `adb_ro_val` returns them, but slots at or beyond the schema's `num_fields` are skipped by the dumper (`adb_walk_adb.c:dump_object:96-102`, `i < schema_len`) and never accessed by fixed-index readers. This gives forward compatibility. A reader should ignore them.
- **`adb_w_copy` copies the raw slot vector**, including its `num`, its type tag and any slots unknown to the schema (`adb.c:828-838`). Index files built by `mkndx` can therefore carry such slots through.
- NULL array items are skipped when dumping (`dump_item:30`).

#### B7.7 Writer conventions (readers must not rely on them)

- **Alignment.** Slot vectors are written 4-byte aligned (`adb.c:__adb_w_obj:953`).
- **Trimming.** Trailing NULL slots are dropped (`adb.c:949-950`), so an object may have fewer slots than its schema.
- **Empty means NULL.** An object or array with no non-NULL slot (`n <= 1` after trimming) is written as `ADB_NULL`, never as a 1-slot vector (`adb.c:945, 951-954`). The parent slot is then NULL; an empty `files` or `paths` array, for example, is absent.
- **No NULL items.** `adb_wa_append` drops NULL (`adb.c:1040`).
- **Capacity.** An array schema's `num_fields` (8, 32 or 128) is only the writer's initial buffer size. `adb_wa_append` doubles it as needed (`adb.c:886-900`, `1036-1054`). The only read limit is `ADBI_MAX`.
- **Sorting.** Many array schemas sort before commit (`pre_commit = adb_wa_sort`, or `adb_wa_sort_unique` for dependency arrays; `apk_adb.c:173,408,444,485,503,543`). Readers do not check it.
- **Write order and dedup.** Children are written before parents and the root is written last, so child offsets are lower than parent offsets. Identical byte runs at a compatible alignment are deduplicated through a hash-bucket cache, so several values, including slots in different objects, may share one offset (`adb_w_data:671-704`). `adb_wo_blob_raw` bypasses deduplication (`adb.c:772-778, 1016-1020`). The reader enforces neither ordering nor acyclicity. **A robust reader must allow shared references, must not assume offsets increase, and must guard against reference cycles and very deep recursion.**

#### B7.8 Writer padding inside the payload

`adb_w_raw` (`adb.c:355-382`) zero-fills from the current end of the payload up to `ROUND_UP(len, alignment)` before appending. Alignment is therefore relative to **payload offset 0**:
- `adb_hdr`: 8
- blobs: 1, 2 or 4, matching the prefix width
- INT_32: 4
- INT_64: 4 (8 when written by `adb_w_copy`)
- object and array slot vectors: 4
- nested-ADB blobs: 4

The payload's total length is not padded; the block padding handles that.

#### B7.9 Nested ADB (`ADB_KIND_ADB`; `adb.c:adb_w_adb:848-861`, `adb_walk_adb.c:46-53`)

The only nested-ADB field in 3.0.8 is the `packages` array of the installed DB (§D11):
- `schema_idb` → `schema_package_adb_array` → `schema_package_adb`, with `schema_id = ADB_SCHEMA_PACKAGE` (`apk_adb.c:535-554`).
- **The index (`indx`) does not use it.** Its `packages` field is a plain array of `pkginfo` OBJECTs (`apk_adb.c:441-456`).
- Writers: `convdb` (`app_convdb.c:124`) and `adbgen` (`app_adbgen.c:117-121`).

**Encoding:**
- A **BLOB_32**, 4-byte aligned and not deduplicated, whose u32 length prefix covers exactly the following bytes: `[block header 4|16][inner ADB payload][0..7 zero padding]`. This is one complete ADB block with **no file header and no SIG blocks**.
  - Block header: `u32 type_size = (ADB_BLOCK_ADB=0 << 30) + 4 + len`, or for very large sizes the 16-byte extended form `{u32 (3<<30)+type, u32 reserved, u64 16+len}` (`adb.h:71-81`).
- The prefix is 4-aligned in the outer heap, so the inner block header is at blob offset + 4, and the embedded payload is only 4-byte aligned in the outer buffer.
- Padding is still computed from rawsize, and it must be present inside the blob, because `adb_block_validate` requires it (`adb.c:25`).
- The writer returns NULL instead of a blob if the inner payload is ≤ 8 bytes (`adb.c:858`).

**Reading:**
- Parse the blob content as blocks with `ADB_SCHEMA_IMPLIED | schema_id`: no file header, schema taken from the field.
- Inner value offsets are relative to the **inner** ADB payload, not to the outer one.
- C path: `adb_m_process(…, trust = NULL, …)`. The blob istream is single-read, so this takes the mmap path.
  - With `t = NULL`, `trusted` stays 0. The callback dumps the ADB block first; the parse then formally ends with `-APKE_SIGNATURE_UNTRUSTED`, and `db->adb` is nulled (`adb.c:189-194`). The dumper ignores the error.
- **Hardening.** `adb_trust_verify_signature` dereferences `trust` with no NULL check (`adb.c:1375`). If a nested blob contained a SIG block of at least 18 bytes with `sign_ver == 0`, the C code would dereference the NULL `trust` pointer (reached from `__adb_m_parse:172-173` with `t == NULL`) and crash. **Recommendation:** require exactly one ADB block inside a nested blob, and parse nested ADB values without any trust check; they are covered by the outer file's signature.
- **Sorting is not reliable.** `wadbcmp` (`adb.c:1093-1103`) parses both elements with `adb_m_blob(…, NULL)`, which always fails the trust check and nulls `db->adb`. Every element then compares as empty, so the "sort" leaves the order to the libc `qsort_r` implementation. **Do not assume nested-ADB arrays are sorted.**

#### B7.10 Worked example

This example is hand-constructed from the writer rules, not taken from a real file. It is an uncompressed index whose root object has only `description = "x"` (field 1).

```
00: 41 44 42 2E            "ADB."   file magic
04: 69 6E 64 78            schema "indx" (0x78646e69)
08: 18 00 00 00            block hdr: type 0 (ADB), rawsize 0x18 = 4 + 20 → payload 20 bytes, padding 0
--- ADB payload; offsets below are payload-relative ---
p00: 00 00 00 00           compat_ver 0, ver 0, reserved 0
p04: 0C 00 00 E0           root = OBJECT @ 0x0C
p08: 01 78                 BLOB_8: len 1, "x"
p0A: 00 00                 writer alignment padding (object needs 4)
p0C: 02 00 00 00           num = 2 (slot0 + 1 field)
p10: 08 00 00 80           slot1 = BLOB_8 @ 0x08
(SIG blocks, if any, would follow at file offset 0x20)
```

- The payload is 20 bytes, so it is valid on the stream path (≥ 16).
- Without SIG blocks it loads only with allow-untrusted.
- By default a writer would emit `"ADBd"` followed by the raw-deflate compression of exactly these 32 bytes.

### B8. How the C code reads a whole file

1. **Decompress.** `adb_decompress(is)` strips and handles the prefix (§B3).
2. **Dispatch:** `adb_m_process(db, is, expected_schema, trust, ectx, cb)` (`adb.c:336-353`).
   - Zero `db`. If IMPLIED is set, `db->schema = expected & 0x7FFFFFFF`. A NULL callback is replaced by a dummy.
   - `apk_istream_mmap(is)` is non-NULL only for `APK_ISTREAM_SINGLE_READ` streams (`io.c:90-95`). Those are mmap'd files (`io.c:531-559`, created only by `apk_istream_from_file_mmap`) and in-memory blobs (`io.c:252-264`).
   - mmap'd files are opened only by `adbdump`, `adbsign` and `mkndx --index` (`app_adbdump.c:40`, `app_adbsign.c:97`, `app_mkndx.c:290`), and those files must also be uncompressed (`"ADB."`). In-memory blobs are the nested ADBs.
   - With such a buffer the **mmap path** runs (`db->is = is`, then `__adb_m_mmap`). Otherwise the **stream path** runs (`__adb_m_stream`).
   - The stream path is always used for deflate or zstd input, for network streams, and for every `apk_extract` consumer. They open files with `apk_istream_from_file` (try_mmap = 0, `apk_io.h:97`) or `apk_istream_from_fd_url`, without mmap. **Package extraction and index loading therefore always use the stream path.**
3. **mmap path** (`__adb_m_mmap:204-227` → `__adb_m_parse:138-196`):
   - Check the file header (§B4) unless IMPLIED, then parse the remaining bytes.
   - Iterate blocks with `adb_block_validate` (`adb.c:17-27`). Let `rem` be the bytes left.
     - `rem == 0`: clean end.
     - `rem < 4`: error.
     - `hdrsize > rem`: error.
     - `rawsize < hdrsize`: error.
     - `ROUND_UP(rawsize, 8) > rem`: error. **The last block's padding must be present.**
     - All of these errors are `-APKE_ADB_BLOCK`.
     - The next block starts at `cur + ROUND_UP(rawsize, 8)`.
   - For each block:
     - Apply the type and order check and the ADB/SIG/DATA handling (§B5.2).
     - For ADB: after the `len >= 8` and `compat_ver == 0` checks, `db->adb` = the payload slice (zero-copy, `adb_block_blob(blk)`, `adb.c:149,166`), then the identity check runs.
     - Then call `cb(db, blk, blob-istream over payload)`.
   - Final checks as in §B5.2. On error, `db->adb = NULL`, and `adb_free` closes the stream.
4. **Stream path** (`__adb_m_stream:229-334`):
   - Read the 8-byte header unless IMPLIED.
   - **Loop:**
     - Peek 4 bytes to choose `hdrsize`: 16 if `ts>>30 == 3`, else 4. If the peek fails (EOF or error), it defaults to 16. Then `apk_istream_read_max(hdrsize)`.
       - A read returning exactly 0 bytes (EOF) is a clean end.
       - A short positive read gives `-APKE_ADB_BLOCK`.
       - A negative read is returned as the error.
     - Apply the type and order check, then compute `sz = rawsize - hdrsize` (u64, unchecked; §B9 item 1).
     - **ADB:** require `16 <= sz <= 0x0FFFFFFF`. Copy the payload into a malloc'd buffer of `sz = adb_block_length(&blk)` bytes (`adb.c:269,277-283`); a short read gives `-APKE_EOF`. Check `compat_ver == 0` and identity, call `cb`, then skip the padding.
     - **SIG:** `apk_istream_peek(sz)`, subject to the buffer limit in §C1.4. Verify if not yet trusted.
     - **DATA:** require trusted.
     - For SIG and DATA: wrap the next `sz` bytes in a segment stream and call `cb`. On close, the segment skips whatever the callback did not consume (`adb.c:314-316`); **errors from that skip are ignored** (`io.c:segment_close:296-303`). Then `apk_istream_skip(padding)` (`adb.c:319-321`).
     - **Missing padding at EOF is an error**: `apk_istream_skip` → `apk_istream_get_max` returns `-APKE_EOF` (`io.c:188-198,645-659`).
     - **Truncated DATA payload.** Reading past the end of the inner stream inside a segment returns `-ECONNABORTED` (`io.c:283-288`). But a final DATA block whose payload is truncated, whose padding is 0, and which the callback did not fully read, ends as a clean EOF, because the segment-close skip error is ignored. The installer and `verify` read everything and catch this. **Recommendation:** require the complete payload and padding.
   - Final checks as in §B5.2. Then `apk_istream_close_error(is, r)`.
   - Decompressor errors normally surface earlier, at the read that hits them (§B3.2).
5. **Value access** happens afterwards through `adb_r_root`, `adb_r_obj`, `adb_ro_val`, `adb_r_int` and `adb_r_blob` (§B7). These functions never return errors; bad references degrade to 0, NULL or empty.

### B9. Container hardening notes

1. **The stream path does not check `rawsize >= hdrsize`; the mmap path does (`adb.c:24`).** On the stream path `sz = rawsize - hdrsize` (`adb_block_length`) underflows as a u64:
   - ADB is then rejected by the `> 0x0FFFFFFF` check;
   - SIG fails in the peek;
   - DATA becomes a huge segment, which in `apk_extract` then fails the `sz == file.size` check with `-APKE_ADB_BLOCK`.
   - **Reject `rawsize < hdrsize` explicitly.**
2. **The mmap path can overflow `ROUND_UP`.** An EXT `x_size` in `[2^64-7, 2^64-1]` rounds to 0. The block then passes validation (`size 0 <= rem`), gets a negative `long` payload length, and `adb_block_next` returns the same block again:
   - for SIG or DATA this loops forever;
   - for ADB it fails on the second visit.
   - **Use checked arithmetic, and require `rawsize <= remaining`.**
3. **ADB payload size limits differ** between the paths (§B6).
4. **No alignment checks anywhere on read.** Writer alignment is relative to the payload start, which is at decompressed-stream offset 12 or 24.
5. **Invalid references and object headers are not reported as errors** by the C accessors; they read as 0, NULL or empty. A stricter Rust reader may surface them as errors, but must accept every file the writers produce, including 8-byte-aligned INT_64 values, copied objects and OBJECT-tagged arrays.
6. **Cycles and recursion depth are not guarded.** Writers only emit backward references.
7. **Signature verification rules are policy**, not structure: trusted before the first DATA block and at the end, invalid SIG blocks silently skipped. A reader that skips verification should still enforce the `ADB SIG* DATA*` order and reject trailing bytes.
8. **Never read:** `adb_ver`, the `adb_hdr.reserved` field, the EXT `reserved` field, the padding bytes, and the compression level byte.
9. **Nested ADB blobs** should contain exactly one ADB block (§B7.9).

### B10. Container reader checklist

```
read_file(bytes):
  if bytes.len < 4 or bytes[0..3] != "ADB": not v3 (apk would try v2 tar.gz)
  match bytes[3]:
    '.' => s = bytes                                         // prefix not consumed
    'd' => s = raw_inflate(bytes[4..])                       // RFC1951, wbits -15, no header/trailer
    'c' => require bytes.len >= 6
           alg = bytes[4]; _level = bytes[5]                 // level ignored on read
           s = match alg { 1 => raw_inflate(bytes[6..]),
                           2 => zstd_decode(bytes[6..]),     // standard frame(s)
                           0 => bytes[6..] (C inconsistent; or reject),
                           _ => err(COMPRESSION) }
    _   => err(COMPRESSION)
  // recommended: require complete deflate stream / zstd frame, reject trailing compressed-layer bytes
  require s.len >= 8 and le32(s[0..4]) == 0x2e424441         // "ADB."
  schema = le32(s[4..8])
  parse_blocks(s[8..], schema)

parse_blocks(b, schema):   // also used for nested ADB blobs (no file header, schema implied)
  allowed = {ADB}; pos = 0; adb = None
  while pos < b.len:
    require b.len - pos >= 4
    ts = le32(b[pos..])
    if ts >> 30 == 3 { hdr = 16; require b.len - pos >= 16; type = ts & 0x3fffffff; raw = le64(b[pos+8..]) }
    else             { hdr = 4;  type = ts >> 30;           raw = ts & 0x3fffffff }
    require raw >= hdr
    size = checked_round_up8(raw); require size <= b.len - pos
    require type < 4 and type in allowed
    payload = b[pos+hdr .. pos+raw]
    match type:
      ADB : require 16 <= payload.len <= 0x0fffffff  (apk's stream path; mmap path allows >= 8)
            require payload[0] == 0                    // adb_compat_ver
            adb = payload; allowed = {SIG, DATA}
      SIG : optional verify (Part C); allowed unchanged
      DATA: allowed = {DATA}                           // apk requires trust before the first DATA
    pos += size                                        // skip zero padding, content unchecked
  require adb present                                  // and, for apk semantics, trusted

value(v): t = le32(v); type = t & 0xF0000000; off = t & 0x0FFFFFFF
          all offsets relative to adb[0]; bounds: off + extra + size <= adb.len
          use unaligned LE loads; guard against cycles and recursion depth
```

---

## Part C. Signatures and trusted keys

Statements about OpenSSL or mbedTLS behaviour describe those libraries, not apk code; they are marked **[lib]**. The signature structures are made only of `uint8_t` fields, so they have **no internal padding** and **no alignment requirement**. Blocks start 8-aligned (§B3.4).

### C1. The SIG block

#### C1.1 C structures (`adb.h:111-119`)

```c
struct adb_sign_hdr { uint8_t sign_ver, hash_alg; };          // sizeof == 2
struct adb_sign_v0  { struct adb_sign_hdr hdr;                  // sizeof == 18
                      uint8_t id[16];
                      uint8_t sig[]; };                          // flexible array, not counted in sizeof
```

#### C1.2 Framing

A SIG block is an ordinary block of type 1 (`ADB_BLOCK_SIG`; header encoding in §B5.1). Points specific to SIG:

- The reader accepts a SIG block in EXT form. The writer never produces one, because `adb_block_init` uses EXT only when the payload is longer than `0x3fffffff - 4` (`adb.h:71-81`).
- The payload is the `adb_block_blob()` of the block. It excludes the header and the padding; the padding is not signed.
- The mmap path rejects `rawsize < hdrsize`, and blocks whose padded size exceeds the remaining data, with `-APKE_ADB_BLOCK` (`adb.c:adb_block_validate` 17-27). The stream path does **not** check `rawsize < hdrsize`: `adb_block_length` underflows (u64) and the subsequent `apk_istream_peek(is, sz)` fails, so the result is still an error. A Rust reader should reject `rawsize < hdrsize` explicitly.

**Example.** A SIG block with a 512-byte (RSA-4096-sized) signature:
- payload = 18 + 512 = 530
- rawsize = 534
- `type_size = (1<<30) + 534 = 0x40000216`, on-disk bytes `16 02 00 40`
- padded size 536, so 2 padding bytes

With a 256-byte signature: payload 274, `type_size = 0x40000116`, 2 padding bytes. The key sizes are only illustrations; the source fixes no key size.

#### C1.3 Byte layout of the SIG payload

| Offset | Size | Field | Meaning |
|---|---|---|---|
| 0 | 1 | `sign_ver` | Must be `0`. Any other value makes `adb_trust_verify_signature` return `-APKE_ADB_SIGNATURE` for this block (`adb.c:1371`). The block is then simply ignored (§B5.2). |
| 1 | 1 | `hash_alg` | The digest used to **pre-hash the ADB block payload** (§C2.1). The verifier accepts 3 (SHA256), 4 (SHA512) and 5 (SHA256_160) (`adb.c:adb_digest_adb` 80-91). The writer always emits **4** (`adb.c:1340`). |
| 2 | 16 | `id` | The key ID (§C3). Compared with raw `memcmp` against each trusted key's ID (`adb.c:1376`). |
| 18 | N | `sig` | Raw signature bytes up to the **end of the payload**. There is no length field. N = payload_len − 18 (`adb.c:1381`, `sigb.len - sizeof *sig0`). For RSA this is the big-endian signature integer, I2OSP to the modulus length **[lib]**. |

`hash_alg` values are the digest IDs of §A3.1 (`apk_crypto.h:16-21`): NONE = 0, SHA1 = 2, SHA256 = 3, SHA512 = 4, SHA256_160 = 5. Only 3, 4 and 5 are accepted by `adb_digest_adb`; any other value, including SHA1 = 2, gives `-APKE_CRYPTO_NOT_SUPPORTED`, and that key is skipped.

#### C1.4 Rules and limits

- **Minimum payload length** (`adb.c:1367-1373`):
  - If `db->adb` is NULL, the result is `-APKE_ADB_BLOCK`. This cannot happen in practice, because SIG is only allowed after the ADB block.
  - A payload shorter than 2 bytes gives `-APKE_ADB_SIGNATURE`.
  - Then `sign_ver` is checked.
  - A payload shorter than 18 bytes gives `-APKE_ADB_SIGNATURE`.
  - N = 0 passes these checks and then fails cryptographic verification.
  - All of these only mean "this block does not establish trust". They are not fatal (§B5.2).
- **One signature per SIG block.**
  - The writer emits one SIG block per private key (`adb.c:adb_trust_write_signatures` 1337-1353).
  - The reader treats everything after byte 18 as one signature.
  - `doc/apk-v3.5.scd:138-142` says "The SIG block contains one or more signatures". The 3.0.8 code contradicts this; follow the code (§H1).
- **Number of SIG blocks.** Zero or more. Each is examined independently, until one verifies (§B5.2).
- **Writer size limit.**
  - The writer signs into `union { ...; unsigned char buf[ADB_MAX_SIGNATURE_LEN]; }`.
  - It passes `siglen = sizeof buf - sizeof sig.v0` = 2048 − 18 = **2030 bytes** as the output capacity (`adb.c:1319-1323,1344`), so a whole SIG payload is at most 2048 bytes (`adb.c:1317-1358`).
  - A key whose signature is longer than that fails to sign. The 2048 limit applies to the writer only.
- **Reader size limit.**
  - There is no explicit maximum.
  - **mmap path:** the whole file is mapped (`io.c:apk_mmap_istream_from_fd` 531-559; `apk_istream_mmap` 90-95), so there is no limit.
  - **Stream path:** the SIG payload is obtained with `apk_istream_peek(is, sz)` (`adb.c:296`), even when the file is already trusted (`adb.c:296-300`). It must fit in the stream buffer:
    - `apk_io_bufsize` = 128 KiB for fd streams (`io.c:604-613`), gzip (`io_gunzip.c:161-167`), zstd (`io_zstd.c:110-128`) and libfetch (`io_url_libfetch.c:143-163`).
    - Up to 7 bytes are lost to `APK_ISTREAM_ALIGN_SYNC` realignment: the real limit is `buf_size - (offset % 8)` (`io.c:__apk_istream_fill` 142-161).
    - If the buffer is full, the result is `-APKE_BUFFER_SIZE` (`io.c:154`). If the stream ends first, the result is `-APKE_EOF`.
    - A peek failure is **fatal** for the whole parse (`adb.c:297-300`), even if the file is already trusted.
- **Placement.** SIG blocks are allowed only **after the ADB block and before the first DATA block**. A SIG block anywhere else is a hard `-APKE_ADB_BLOCK` (§B5.2).

### C2. What is hashed and what is signed

The scheme has two stages. `doc/apk-v3.5.scd:144-151` agrees: "the ADB block is not signed directly, but it is hashed first by a secure digest (currently SHA512). After this a small payload with this pre-calculated digest is signed".

#### C2.1 Stage 1: `md`, the digest of the ADB block payload (`adb.c:adb_digest_adb` 74-102)

**Input bytes.** Exactly the **ADB block payload**, `db->adb`:
- mmap path: `db->adb = adb_block_blob(blk)` (`adb.c:149,166`).
- Stream path: a malloc'd copy of `sz = adb_block_length(&blk)` bytes (`adb.c:269,277-283`).
- `apk adbsign` hashes the same bytes: `adb_c_block_copy` feeds exactly `adb_block_length(b)` bytes into a SHA-512 context (`adb.c:1255-1272`).

**Included.** The leading 8-byte `struct adb_hdr` (`adb_compat_ver` u8, `adb_ver` u8, `reserved` u16, `root` u32 LE; `adb.h:104-109`) and everything up to the end of the payload.

**Excluded:**
- the 8-byte file header (magic and schema), although the schema enters stage 2;
- the block header word(s);
- the block's trailing padding;
- the compression wrapper (`"ADBd"`, or `"ADBc"` + 2-byte `{alg, level}` spec; `adb_comp.c:adb_decompress` 82-119). Hashing is done on **decompressed** bytes (§B3.4);
- all SIG and DATA blocks.

**Algorithm, selected by `hash_alg`:**
- `SHA512` (4): `md` = SHA-512(payload), 64 bytes.
- `SHA256` (3): `md` = SHA-256(payload), 32 bytes.
- `SHA256_160` (5): the `case` falls through with `alg = APK_DIGEST_SHA256`, so SHA-256 is computed. `md` is its **first 20 bytes**, because the `*pmd` length is `apk_digest_alg_len(hash_alg)` (`adb.c:81-83`, line 100; `crypto.c:apk_digest_alg_len:28-37`).
- Any other value returns `-APKE_CRYPTO_NOT_SUPPORTED`. `adb_trust_verify_signature` then `continue`s to the next key (`adb.c:1377`). With no other key the block fails.

**Caching.**
- `struct adb_verify_ctx { uint32_t calc; struct apk_digest sha256, sha512; }` (`adb.h:280-284`).
- Bit `1<<3` marks the SHA-256 cache, which SHA256 and SHA256_160 share. Bit `1<<4` marks SHA-512 (`adb.c:93-98`).
- The context is a local `= {}` in each parse (`adb.c:142`, `adb.c:234`). Each digest is therefore computed at most once per file, and is shared by the identity check (§C5.3) and every SIG block.
- This is an optimisation and does not change the format.

#### C2.2 Stage 2: the signed message `M` (`adb.c:adb_digest_v0_signature` 1304-1315)

Three `apk_digest_ctx_update` calls, in this order:

```
M = le32(schema)            // 4 bytes
  || sig_payload[0..18]     // 18 bytes: sign_ver, hash_alg, id[16], exactly as stored in the SIG block
  || md                     // 20, 32 or 64 bytes (§C2.1)
```

**`schema`** is `db->schema`, normalised with `htole32` (line 1309, comment "it is imporant to normalize this").
- For a top-level file, `db->schema = le32toh(hdr->schema)` (`adb.c:217` mmap, `adb.c:253` stream). The 4 bytes are therefore exactly **bytes 4..8 of the decompressed file**.
- For an implied or nested ADB (`expected_schema & ADB_SCHEMA_IMPLIED`), there is no file header and `db->schema = expected_schema & ~0x80000000` (`adb.c:345-346`).
- `adb_m_blob` leaves `db->schema = 0` (`adb.c:198-202`, `adb_init` memsets).

**The 18 header bytes** are `sizeof *sig0`, taken from the SIG payload as stored. `sign_ver`, `hash_alg` and the key ID are therefore covered by the signature. The signature bytes at offset 18 and later are not part of `M`.

**Length of `M`:**

| `hash_alg` | Length of `M` |
|---|---|
| SHA512 (what the writer produces) | 4 + 18 + 64 = **86 bytes** |
| SHA256 | 54 bytes |
| SHA256_160 | 42 bytes |

#### C2.3 Signature primitive

The verifier always calls `apk_verify_start(&trust->dctx, APK_DIGEST_SHA512, &tkey->key)` (`adb.c:1379`). The signer always calls `apk_sign_start(&trust->dctx, APK_DIGEST_SHA512, ...)` (`adb.c:1346`). So **the signing primitive hashes `M` with SHA-512, whatever `hash_alg` says**. `hash_alg` affects only `md`.

**OpenSSL backend.**
- Sign: `EVP_MD_CTX_reset`, then `EVP_DigestSignInit(ctx, NULL, sha512, NULL, pkey)`. Updates go through `EVP_DigestUpdate`, then `EVP_DigestSignFinal` (`crypto_openssl.c:211-225`, update at 146-150).
- Verify: `EVP_DigestVerifyInit(ctx, NULL, sha512, NULL, pkey)` … `EVP_DigestVerifyFinal(ctx, sig, len)`. Failure gives `-APKE_SIGNATURE_INVALID` (`crypto_openssl.c:227-241`).
- apk never sets padding or any other `EVP_PKEY_CTX` parameter.
- **[lib]** The OpenSSL default for RSA is PKCS#1 v1.5. The result is **RSASSA-PKCS1-v1_5 with SHA-512 over `M`**, i.e. `EMSA-PKCS1-v1_5(DigestInfo(sha512, SHA512(M)))`.

**mbedTLS backend.**
- `apk_verify_start` calls `apk_digest_ctx_reset_alg(dctx, SHA512)` and remembers the key (`crypto_mbedtls.c:323-332`).
- `apk_verify` finalises `SHA512(M)` and calls `mbedtls_pk_verify(pk, MBEDTLS_MD_SHA512, hash, 64, sig, len)` (`crypto_mbedtls.c:334-355`).
- Signing uses `mbedtls_pk_sign(..., MBEDTLS_MD_SHA512, hash, 64, ...)` (`crypto_mbedtls.c:232-241,296-321`).
- The mbedTLS < 3.0 branch at line 253 refers to `mp->pkg`, which looks like a compile error in that configuration. This is writer-only and outside the format.
- **[lib]** The default mbedTLS RSA padding is PKCS#1 v1.5, and apk never changes it. The scheme is the same as with OpenSSL.

**Signature length.** apk passes `payload_len − 18` unchanged and does not check it.
- **[lib]** OpenSSL's RSA verify rejects `siglen != RSA_size`.
- **[lib]** mbedTLS `rsa_verify_wrap` fails for a short signature and returns `MBEDTLS_ERR_PK_SIG_LEN_MISMATCH` (non-zero, so apk treats it as invalid) for a long one.
- A reader should therefore require `N == modulus_len_bytes`.

**Key types.** apk never checks the key type.
- **[lib]** With an EC key, both libraries would do ECDSA over SHA-512 with a DER `ECDSA-Sig-Value` signature.
- **[lib]** EdDSA keys cannot be used through `EVP_DigestSignInit`/`EVP_DigestVerifyInit` with an explicit digest.
- **[unconfirmed]** Whether non-RSA keys are used anywhere in practice. The source does not say.
- A reader can implement RSA PKCS#1 v1.5/SHA-512 and treat other key types as "no match".

**Rust equivalent** (crate API, not from the source):

```rust
rsa::pkcs1v15::VerifyingKey::<sha2::Sha512>::new(pubkey)
    .verify(&M, &Signature::try_from(sig_bytes)?)
```

This is the prefixed DigestInfo form. Equivalently, `pubkey.verify(Pkcs1v15Sign::new::<Sha512>(), &Sha512::digest(&M), sig_bytes)`.

#### C2.4 Worked layout (SHA-512, which is what apk writes)

```
file (after decompression):
  [0..4)   "ADB."                 magic 0x2e424441 LE
  [4..8)   schema (LE u32)        --> first 4 bytes of M
  ADB block:  hdr(4 or 16) | payload P (starts with adb_hdr) | pad to 8
  SIG block:  hdr(4) e.g. 16 02 00 40 | 00 04 <id:16> <sig:N> | pad to 8
  [more SIG blocks]
  DATA blocks ... (packages only)

md = SHA512(P)                                   (64 bytes)
M  = schema_le32 || 00 04 <id:16> || md          (86 bytes)
valid iff RSASSA-PKCS1-v1_5-VERIFY(pubkey, M, sig) with SHA-512
```

### C3. Key ID derivation (16 bytes)

**OpenSSL** (`crypto_openssl.c:apk_pkey_init` 167-183):
- `len = i2d_PublicKey(key, &pub)`
- `EVP_Digest(pub, len, dig, &dlen, EVP_sha512(), NULL)`
- `memcpy(pkey->id, dig, 16)`

**mbedTLS** (`crypto_mbedtls.c:apk_pkey_fingerprint` 191-209):
- `mbedtls_pk_write_pubkey(&c, pub, pk)` writes backwards into `pub[APK_ENC_KEY_MAX_LENGTH]`, where `APK_ENC_KEY_MAX_LENGTH = 38 + 2*MBEDTLS_MPI_MAX_SIZE` (line 42).
- SHA-512 of those `len` bytes starting at `c`.
- The first 16 bytes are copied.

**Result:** `key_id = SHA512(encoded_public_key)[0..16]`.

**[lib] Encoding.** Both `i2d_PublicKey` and `mbedtls_pk_write_pubkey` write the **type-specific raw public key, not the X.509 SubjectPublicKeyInfo**:
- For RSA, this is the DER **PKCS#1 `RSAPublicKey`**: `SEQUENCE { modulus INTEGER, publicExponent INTEGER }`, with minimal INTEGER encoding and a leading `00` when the high bit is set.
- For EC, it is the EC point octets. **[unconfirmed]** Whether both backends always produce the same point form (compressed or uncompressed) for EC keys, and so the same key ID.
- apk does **not** use the SPKI writers (`i2d_PUBKEY`, `mbedtls_pk_write_pubkey_der`).

**Recipe for a Rust reader (RSA):**
1. Parse the PEM `PUBLIC KEY` (SPKI) and extract `(n, e)`.
2. Re-encode canonical DER `RSAPublicKey`.
3. `key_id = SHA512(der)[0..16]`.

For a canonically encoded SPKI, this equals the content of the SPKI `subjectPublicKey` BIT STRING after its unused-bits byte (`00`). Re-encoding from `(n, e)` is the safe choice.

Related facts:
- `struct apk_pkey { uint8_t id[16]; void *priv; }` is defined at `apk_crypto.h:93-96`.
- `doc/apk-keys.5.scd:15-17`: "APKv3 files are matched using the public key identity and filename is not significant."

### C4. Trusted key loading

#### C4.1 Directories (`context.c:apk_ctx_get_trust` 151-169)

Keys are loaded lazily, once per context (`ac->keys_loaded`). Every consumer goes through `apk_ctx_get_trust`, e.g. `extract_v3.c:276` and `adb_walk_adb.c:166`.

**Default** (no `--keys-dir`):
- apk calls `apk_dir_foreach_config_file(ac->root_fd, cb, &ac->trust, apk_filename_is_hidden, "etc/apk/keys", "lib/apk/keys", NULL)`.
- Both paths are relative to the root fd. `ac->root` defaults to `"/"` (`context.c:72`), is opened at `context.c:88`, and can be changed with `--root`.

Merge semantics (`io.c:apk_dir_foreach_config_file` 966-1004; also used for repository `.list` files, §G1.1):
- A directory that cannot be opened is skipped (`continue`, line 983).
- A file name already collected from an earlier directory **shadows** the same name in a later directory. The lookup is a `bsearch` over the names gathered from previous directories (line 960). The list is sorted after each directory (line 989).
- `doc/apk.8.scd:464-468` documents this: "Once a given filename is seen, any file of the same name in subsequent directories is ignored."
- The callback then runs over the merged list **in `strcmp` order of file name** (lines 991-997). Each file is opened relative to the fd of the directory it came from.
- If enumerating any directory returns an error (for example `fdopendir` fails), the loop breaks and **no** callback runs, so no keys are loaded (lines 986-991).

**`--keys-dir DIR`** (`apk.c:62,150-151`):
- Only that directory is read, with `apk_dir_foreach_file(ac->root_fd, ac->keys_dir, ...)` (`context.c:162-164`), in raw `readdir` order (`io.c:870-904`).
- `doc/apk.8.scd:200-203` says it is "treated relative to _ROOT_". Because the code uses `openat`, an absolute `DIR` ignores the root fd (POSIX semantics).

#### C4.2 Which files are loaded

- Every entry is offered except `.`, `..` (`io.c:897`) and names starting with `'.'` (`io.c:apk_filename_is_hidden` 865-868).
- **No extension or naming convention is enforced.**
- There is no `d_type` check and no recursion. A subdirectory or other non-key file is opened and fails to parse.
- `__apk_ctx_load_pubkey` (`context.c:140-149`) calls `apk_trust_load_key(dirfd, filename, 0)` (`trust.c:5-22`). On failure the file is **silently ignored** (the callback always returns 0).
- On success the key is appended with `list_add_tail` to `trust->trusted_key_list` (`apk_trust.h:20-25`). The list is therefore in the callback order above.
- `tkey->filename` is the bare file name.
- Files are opened with `openat(..., O_RDONLY|O_CLOEXEC)`, so symlinks are followed (`crypto_openssl.c:196`, `crypto_mbedtls.c:156`).

#### C4.3 File format (`apk_pkey_load`)

**OpenSSL** (`crypto_openssl.c:190-209`):
- Public keys are read with `PEM_read_bio_PUBKEY`, called **once**, so at most one key is loaded per file. Failure gives `-APKE_CRYPTO_KEY_FORMAT`.
- **[lib]** At minimum this accepts `-----BEGIN PUBLIC KEY-----` (SPKI) of any key type.
- **[unconfirmed]** Whether other PEM types (for example `RSA PUBLIC KEY`) are accepted. It depends on the OpenSSL version; the OpenSSL 3 decoder path may accept more.

**mbedTLS** (`crypto_mbedtls.c:150-189,259-294`):
- The file size must be > 0 and ≤ **64000** bytes. Otherwise the result is `-APKE_CRYPTO_KEY_FORMAT` (line 167).
- The buffer is NUL-terminated. If it contains `"-----BEGIN "`, the length passed to the parser includes the NUL (lines 181-186).
- Parsing uses `mbedtls_pk_parse_public_key`. A **parse or fingerprint failure gives `-APKE_CRYPTO_ERROR`** (lines 287-291), not KEY_FORMAT.
- **[lib]** mbedTLS accepts PEM or DER, in SPKI or PKCS#1 form.

**Private keys** (writers only, `--sign-key FILE`; `apk.c:268-293`):
- Loaded relative to the current directory (`AT_FDCWD`) with `PEM_read_bio_PrivateKey` (OpenSSL) or `mbedtls_pk_parse_key`.
- Appended to `trust->private_key_list`. The key ID is derived from the public half in the same way.

**Recommendation for a reader:** accept PEM `PUBLIC KEY` (SPKI) files with RSA keys. Silently skip files that fail to parse, and hidden files.

#### C4.4 Filename relevance

- **v3:** the filename plays no part. Matching is by the 16-byte ID only (`adb.c:1376`). Several loaded keys may share an ID, and each is tried in list order.
- **v2 (legacy):** the filename is the key name.
  - `apk_trust_key_by_name` (`trust.c:51-59`) is matched against the `<name>` in `.SIGN.<TYPE>.<name>` tar entries.
  - The recognised type prefixes are `RSA512`, `RSA256`, `RSA` and `DSA` (`extract_v2.c:92-100`).
  - The match is done at `extract_v2.c:141-153`. Signature files larger than 65536 bytes are ignored (line 151).

#### C4.5 Where the options can come from

- `--allow-untrusted` and `--keys-dir` are global options.
- They can also be set in the config file: `$APK_CONFIG`, else `/etc/apk/config`, else `/lib/apk/config` when the former is ENOENT. This file is not relative to `--root`.
- Each line is `option [value]`, and `#` starts a comment (`apk.c:load_config` 516-544; global options matched at `apk.c:408-416`).

### C5. What makes a v3 file trusted

#### C5.1 The trust state machine

The trust rules are part of the block walk and are given in full in §B5.2: `trusted` starts as allow-untrusted, can be set by the identity check at the ADB block or by the first SIG block that verifies, must be true before the first DATA block and at the end of the file, and an identity mismatch is fatal even with allow-untrusted.

Readers built on this (`apk_extract_v3`, `extract_v3.c:273-317`) run all index and package metadata callbacks only after `adb_m_process` succeeded, or after a DATA block passed the trust check. Untrusted content is therefore never acted on.

#### C5.2 The per-block signature check (`adb.c:adb_trust_verify_signature` 1360-1388)

After the length and version checks of §C1.4, apk iterates `trust->trusted_key_list` in order. For each key:
1. Skip the key unless `memcmp(sig.id, key.id, 16) == 0`.
2. Skip the key unless `adb_digest_adb(vfy, sig.hash_alg, db->adb, &md) == 0`.
3. Skip the key unless the SHA-512 PKCS#1 v1.5 verification of `M` (§C2.2, §C2.3) succeeds.
4. Otherwise return 0 (trusted).

If no key passes, the result is `-APKE_SIGNATURE_UNTRUSTED`.

**Trust policy:** one valid signature from any one trusted key is enough. There is no threshold, no rule that every signature must be valid, and no per-key scope or usage restriction.

#### C5.3 Trust by identity, with no signature needed (`adb.c:__adb_handle_identity` 109-136)

This runs only when the caller passes an extract context (only `apk_extract_v3` does), at the ADB block (called at `adb.c:167` mmap, `adb.c:288` stream), before any SIG block, and even when `trusted` is already true.

**Verify mode.** `apk_extract_verify_identity(&ectx, pkg->digest_alg, apk_pkg_digest_blob(pkg))` is called in three places:
- `database.c:717` (cache download),
- `database.c:3190` (install),
- `app_fetch.c:197`.

None of these is on the index-loading path, so indexes are never trusted by identity. The expected digest is the package's `hashes` field from a (signed) index, `ADBI_PI_HASHES` (`package.c:565-566`):
- It is used only if it is at least 20 bytes long.
- Its algorithm is inferred **from its length** with `apk_digest_from_blob` (§A3.1).
- Lengths other than 20, 32 or 64 give `NONE`. Such packages are skipped as "without hash" (`database.c:1496-1499`).
- `apk_pkg_digest_blob` is `pkg->digest` with length `apk_digest_alg_len(pkg->digest_alg)` (`apk_package.h:108-110`).

**Generate mode.** `apk_extract_generate_identity`:
- `apk mkndx` uses the `--hash` algorithm: default SHA256 (`app_mkndx.c:62`); only SHA256 or SHA256_160 are allowed (`app_mkndx.c:42,73-77`). It stores the result as `hashes` (`app_mkndx.c:318,326`; §D10.2).
- `apk_pkg_read` uses SHA256, for local `.apk` files (`package.c:673`; §D10.7).
- Generate mode **never confers trust**. Those files need a signature or allow-untrusted.

The check:

```
alg = generate_alg ?: verify_alg
if alg ∈ {NONE, SHA1}: return 0             // "Ignore the sha1 identity as they are 'unique-id' instead of hash" (adb.c:118-119)
r = adb_digest_adb(alg, ADB payload)        // same digest/cache as §C2.1; error -> returned (hard error)
if generating: store digest (alg, len), return 0
if apk_blob_compare(verify_digest, calculated) == 0: return 1   // length+bytes equal -> trusted
else if verify_alg == SHA256_160: return 0  // mismatch ignored; fall back to signatures
else: return -APKE_ADB_INTEGRITY            // hard error, even with --allow-untrusted
```

Notes:
- **The identity is a digest of the ADB block payload only** (no file header, block header or padding), not of the whole file.
- A 20-byte `hashes` value is labelled SHA1, so identity verification is skipped for it. It becomes `SHA256_160` only through `apk_db_pkg_add` (`database.c:593-594`): when the template's `ipkg` has the `sha256_160` flag, which is set from the installed database (`database.c:1032,2779`).
- The resulting behaviour per index digest is tabulated in §D10.5.

#### C5.4 Allow-untrusted, dump and edge cases

**`--allow-untrusted`:**
- Sets `APK_ALLOW_UNTRUSTED = BIT(3)` (`apk.c:43,93-94`; `apk_context.h:22`). `apk_ctx_prepare` copies it to `ac->trust.allow_untrusted` (`context.c:69`).
- `trusted` then starts as true. SIG blocks are never checked, and unsigned files or files with bad signatures are accepted.
- An identity mismatch (§C5.3) is still fatal.
- `doc/apk.8.scd:117-118`: "Install packages with untrusted signature or no signature."

**`apk adbdump`** (`adb_walk_adb.c:153-172`):
- Parses with a local `{ .allow_untrusted = 1 }` trust.
- For each SIG block, its callback separately calls `adb_trust_verify_signature` with the real key set (`ser->trust = apk_ctx_get_trust(ac)`).
- It prints `"sig v%02x h%02x "`, then hex starting at payload offset 2 (key ID, then signature bytes). The hex is truncated with `".."` when the 160-byte buffer runs low. Then it prints `": OK"` or `": <error>"` (lines 132-141).
- This is for display only.

**No keys loaded:** with an empty trusted-key list and no allow-untrusted, every file is untrusted except those validated by identity (§C5.3).

**Nested ADBs and NULL trust:** values of kind `ADB_KIND_ADB` (`schema_package_adb`, `apk_adb.c:535-537`, used inside the installed-db schema) are bare unsigned ADB blocks. Internal readers pass `t = NULL` (`adb_walk_adb.c:49-51`, `adb.c:wadbcmp` 1098-1099), every such parse ends in `-APKE_SIGNATURE_UNTRUSTED`, and a nested SIG block could crash apk. This looks like an upstream quirk. Details and the recommendation (parse nested ADBs without a trust check) are in §B7.9.

#### C5.5 Coverage

- The signature covers the ADB block payload (through `md`), the file's schema u32, and the SIG header's own `sign_ver`, `hash_alg` and `id`.
- DATA blocks are not signed directly. They are protected by hashes stored inside the ADB block (`doc/apk-v3.5.scd:153-157`; §E3).
- **Not covered:**
  - the 4-byte magic (fixed value),
  - the compression wrapper and spec,
  - block headers,
  - padding bytes (never checked),
  - other SIG blocks,
  - the presence, order and count of DATA blocks, apart from what the ADB contents constrain.

### C6. Writer side, for cross-checking

`adb.c:adb_trust_write_signatures` 1317-1358:
1. Compute `md = SHA512(db->adb)` once, using the cached SHA-512 if `vfy` is supplied (line 1334).
2. For each key in `trust->private_key_list`, in `--sign-key` order:
   1. Build the header `sign_ver=0`, `hash_alg=4`, `id` = key ID.
   2. Sign `M = le32(schema) || hdr18 || md` with SHA-512 (output capacity 2030).
   3. Emit `adb_c_block(os, ADB_BLOCK_SIG, hdr18 || sig)`, zero-padded to 8.
3. With no private keys, no SIG block is written (an unsigned file).

Output order:
- `adb_c_adb` writes the file header, the ADB block, then the SIG blocks (`adb.c:1285-1295`). It refuses `schema == 0` (`-APKE_ADB_HEADER`).
- `apk mkpkg` then appends DATA blocks (`app_mkpkg.c:551` onward).

`apk adbsign` (`app_adbsign.c:40-104`):
- Re-emits the header and copies the ADB block while computing SHA-512.
- Keeps existing SIG blocks, or drops them with `--reset-signatures`.
- Writes new signatures before the first DATA block, or at the end for files without DATA. New signatures therefore come **after** any kept SIG blocks.
- **Observation [unconfirmed, not tested]:**
  - The input is parsed with `&ac->trust` (line 82).
  - Trusted public keys are loaded lazily only when `process_signatures` runs (line 46). That happens after the DATA trust check, or not at all if the parse fails.
  - So, as far as can be told from the code, the first file must be acceptable without loaded keys, i.e. with `--allow-untrusted`.
  - This is writer-only.

### C7. Signature reader checklist (Rust)

1. Load trusted keys:
   - Default: `etc/apk/keys` then `lib/apk/keys` under the root. Hidden names skipped, an earlier directory shadows a same-named later file, processed in `strcmp` name order, parse failures ignored.
   - Or only `--keys-dir`.
2. For each key, compute `id = SHA512(DER RSAPublicKey)[..16]`.
3. Decompress (`ADB.` / `ADBd` / `ADBc` + 2-byte spec). Read the 8-byte header and keep the schema's raw 4 bytes.
4. Walk the blocks:
   - First the ADB block (keep its payload `P`).
   - Then optional SIG blocks.
   - Then DATA blocks.
   - Any other order is an error.
5. If an expected identity digest is known (20/32/64 bytes; 20 bytes means "skip" unless it is known to be SHA256-160):
   - Compare it to the digest of `P`.
   - A match means trusted.
   - A mismatch is fatal, except for SHA256-160, where it is ignored.
6. For each SIG block, until trusted:
   - Require payload ≥ 18 and `sign_ver == 0`.
   - Find the keys with an equal `id`.
   - Compute `md` from `hash_alg` ∈ {3, 4, 5}.
   - Build `M = schema_le32 || payload[0..18] || md`.
   - Verify RSASSA-PKCS1-v1_5/SHA-512 of `M` with `payload[18..]`.
   - Any failure only skips that block or key.
7. Reject with "untrusted" at the first DATA block, or at the end, if nothing established trust and allow-untrusted is not set.

---

## Part D. The index and package schemas

This part says which value goes in which slot and how to read it. The value encoding itself (tags, blobs, integers, slot vectors, absent and empty values) is in §B7.

### D1. General conventions

#### D1.1 Slots, field numbers and names

- **Containers are slot vectors** (§B7.6). Slot 0 (`ADBI_NUM_ENTRIES` = 0x00) holds the total slot count `num`, including slot 0; fields and items start at slot 1 (`ADBI_FIRST` = 0x01). The reader rejects `num == 0`, `num > ADBI_MAX` (0x1000000) and any `num` whose slot vector would extend past the end of the ADB buffer (`adb.h:43-45`; `adb.c:adb_r_obj` 473-506). It does not check alignment; the writer always places objects and arrays 4-byte aligned (`adb.c:953`).
- **Field constant = slot number.** A field whose C constant is `ADBI_xxx_FOO = k` is stored in **slot k**. `ADB_FIELD(_i,_n,_t)` places the schema entry at `fields[_i-1]` (`adb.h:130`). The constants listed below are therefore the on-disk slot numbers.
- **`ADBI_xxx_MAX`** equals the schema's `num_fields`, which counts slot 0. Valid field slots are `1 .. MAX-1`.
- **Name strings** (for example `"name"` or `"installed-size"`) are used only in text dumps (`adbdump`/`adbgen`), in `--info key:value` parsing (`app_mkpkg.c:parse_info` 90-124), and in mkndx/mkpkg `${...}` substitution (`adb_s_field_subst`, `adb.c:1166-1197`). They never appear in the binary.
- **Reader behaviour** for short objects, absent slots, unknown slots, empty containers, shared values and wrong-typed values is in §B7.2 to §B7.7. In short: a slot `i >= num` is NULL; NULL reads as 0, the null blob or an empty container; unknown slots are ignored; containers may be shared and are never empty on disk (an empty container is stored as NULL in the parent).

#### D1.2 Array versus object type tag (important)

`adb_r_obj` accepts **either** `ADB_TYPE_ARRAY` (0xd…) or `ADB_TYPE_OBJECT` (0xe…) for any container (`adb.c:478-480`). The 3.0.8 writers routinely tag arrays as OBJECT:
- `adb_wo_obj()` always calls `adb_w_obj()`, which writes `ADB_TYPE_OBJECT` via `__adb_w_obj(o, ADB_TYPE_OBJECT)` (`adb.c:1022-1027, 960-963`). It is used for:
  - `ADBI_NDX_PACKAGES` (`app_mkndx.c:355`, `app_convndx.c:69`);
  - `ADBI_PKG_PATHS`, `ADBI_DI_FILES`, `ADBI_PKG_TRIGGERS` (`app_mkpkg.c:504, 387, 517`);
  - `ADBI_IDB_PACKAGES` (`app_convdb.c:219`).
- `adb_w_fromstring()` writes text-parsed arrays (dependency arrays, tags) with `adb_w_obj`, so they are OBJECT-tagged as well (`adb.c:871-879`). This covers everything mkpkg takes from `--info` and everything convndx/convdb convert.
- Only `adb_w_arr`/`adb_wo_arr` emit 0xd. Examples are the xattr arrays (`app_mkpkg.c:211`) and the depends/provides/replaces arrays that mkndx builds for **v2** packages (`app_mkndx.c:177-179`).
- `adb_w_copy` keeps whatever tag the source had (`adb.c:828-838`). mkndx uses it to copy v3 package info and old-index entries.

**A reader must decide array versus object from the schema, never from the tag.**

#### D1.3 Value kinds used by these schemas

Kinds (`adb.h:122-126`): `ADB_KIND_ADB`=1, `OBJECT`=2, `ARRAY`=3, `BLOB`=4, `NUMERIC`=5. The scalar schemas are defined in `apk_adb.c`.

| Scalar schema (apk_adb.c) | Kind | Binary storage | Text form (dump/gen only) | Write-side validation (`fromstring`) | Sort compare |
|---|---|---|---|---|---|
| `scalar_string` (93-98) | BLOB | raw bytes | as-is | none | `apk_blob_sort` (via `string_compare`, 88-91) |
| `scalar_mstring` (100-106) | BLOB | raw bytes | as-is, multiline flag | none | `apk_blob_sort` |
| `scalar_name` (188-193) | BLOB | raw bytes | as-is | `name_fromstring` 177-186 (§D4) | `apk_blob_sort` |
| `scalar_version` (210-215) | BLOB | raw ASCII | as-is | `apk_version_validate` (195-199) | `apk_version_compare`, mapped LESS→-1, GREATER→+1, anything else→0 (201-208) |
| `scalar_hexblob` (244-249) | BLOB | **raw binary bytes** | lowercase hex (217-228) | even-length hex, ≤256 decoded bytes (230-242) | `apk_blob_sort` |
| `scalar_int` (272-277) | NUMERIC | ADB int | decimal | decimal `apk_blob_pull_uint`; leftover characters give EINVAL (256-261) | numeric (263-270) |
| `scalar_time` (279-285) | NUMERIC | ADB int, Unix seconds | decimal, hint `APK_SERIALIZE_TIME` | decimal | numeric |
| `scalar_oct` (299-305) | NUMERIC | ADB int | octal (`%o`), hint `APK_SERIALIZE_OCTAL` | octal | numeric |
| `scalar_hsize` (320-326) | NUMERIC | ADB int, bytes | decimal, hint `APK_SERIALIZE_SIZE` | decimal, or `"N unit"` (307-318) | numeric |
| `schema_xattr` (163-168) | BLOB | `name` `\0` `value` (§D8.4) | `name=hexvalue` | (146-161) | `apk_blob_sort` |

Notes:
- **`apk_blob_sort`** (`blob.c:150-155`) is `memcmp` over the shorter length. If that ties, it returns `a.len - b.len`, so the shorter blob sorts first and a NULL blob sorts before any non-empty blob. This is unsigned byte order.
- **Integer storage** (writer, `adb_w_int`, `adb.c:780-791`): values below 0x10000000 inline as `ADB_TYPE_INT`; values below 2^32 as `ADB_TYPE_INT_32` (4 bytes LE, 4-byte aligned); larger values as `ADB_TYPE_INT_64` (8 bytes LE, **4-byte** aligned; read with `apk_aligned32_le64`). The reader accepts all three forms (§B7.3, §B7.4).

#### D1.4 Root object and dispatch

- The file schema IDs are in §B4. The root object is the value in `adb_hdr.root` (`adb_r_rootobj`, `adb.c:508-511`).
- `apk_extract_v3` opens files with `ADB_SCHEMA_ANY` and dispatches on the header's schema (`extract_v3.c:289-310`): PACKAGE to package extraction, INDEX to index loading, anything else, **including `idb`**, to `-APKE_ADB_SCHEMA` (§B2).
- **The root may be NULL.** An index with no description and no packages has root = `ADB_NULL` (§B7.7), which reads as an empty object.

### D2. Index root object: `schema_index` (`Packages.adb`, schema `"indx"`)

Defined at `apk_adb.h:85-88` and `apk_adb.c:448-456`. Kind OBJECT, `num_fields` = `ADBI_NDX_MAX` = 0x04, no `num_compare` (0).

| Slot | Constant | Name | Value kind |
|---|---|---|---|
| 1 | `ADBI_NDX_DESCRIPTION` = 0x01 | `"description"` | string blob |
| 2 | `ADBI_NDX_PACKAGES` = 0x02 | `"packages"` | array of `schema_pkginfo` (`schema_pkginfo_array`) |
| 3 | `ADBI_NDX_PKGNAME_SPEC` = 0x03 | `"pkgname-spec"` | string blob |

- **description:**
  - Free text. It is read into `repo->description` (`database.c:load_v3index` 1485).
  - mkndx writes `--description`. When the option is not given, `APK_BLOB_STR(NULL)` is the null blob and the slot stays NULL (`app_mkndx.c:353`; `apk_blob.h:33-36`).
  - convndx never writes it.
- **packages:** see §D3 and §D4. It is usually OBJECT-tagged (§D1.2). It is read item by item at `database.c:1492-1507`.
- **pkgname-spec:**
  - Optional download-name template. mkndx writes it only when `--pkgname-spec` is given (`app_mkndx.c:354`). v2 indexes cannot carry a spec.
  - When present it replaces the repository's default spec. If it contains `"://"` anywhere, it is an absolute URL (`absolute_pkgname`); otherwise it is relative to the repository base URL (`database.c:1485-1490`, `apk_repo_package_url` 660-680).
  - It is **not validated at load time**. `apk_pkg_subst_validate` (`package.c:116-141`) exists but is never called in 3.0.8.
  - Template syntax and URL construction: §G1.6.
- **Index location** (`repoparser.c:160-197`, `database.c:add_repository_component` 1553-1576): §G1.4 and §G1.5.

### D3. Package-info array: `schema_pkginfo_array`

Defined at `apk_adb.c:441-446`. Kind ARRAY, item `schema_pkginfo`, initial capacity 128.

- **Writer sort.** The writer sorts with `pre_commit = adb_wa_sort`: a `qsort_r` over the item values using `adb_ro_cmpobj(..., ADB_OBJCMP_EXACT)` (`adb.c:1105-1132, 551-572`).
  - EXACT compares slots 1, 2, 3, … up to `max(num_a, num_b)-1`, each with its scalar compare. Nested arrays and objects are compared recursively (`adb.c:574-597`).
  - The effective order is: **name** (bytewise), then **version** (`apk_version_compare`; two versions also compare equal when exactly one is NULL, §F1.5), then **hashes**, description, arch, and so on.
  - `qsort` is not stable, so the order between entries that compare equal is unspecified.
- **Readers do not rely on the order.** Only apk's writer produces sorted output, and nothing on the read path checks it.
  - `load_v3index` iterates every item.
  - The only binary search is `adb_ra_find` in mkndx's old-index reuse (`app_mkndx.c:204-250`, `adb.c:599-629`).
- **Lookup modes.** `schema_pkginfo.num_compare = ADBI_PI_HASHES` (3) (`apk_adb.c:415`).
  - In `ADB_OBJCMP_TEMPLATE` mode, only slots set in the template are compared, and only up to slot 3.
  - In `ADB_OBJCMP_INDEX` mode, comparison stops at the first unset template slot (`adb.c:560-570`).

### D4. Package-info object: `schema_pkginfo`

Defined at `apk_adb.h:16-37` and `apk_adb.c:412-439`. Kind OBJECT, `num_fields` = `ADBI_PI_MAX` = 0x16 (22), so the field slots are 1-21.

The same object appears in two places:
- as `info` (slot 1) of a package file (§D6);
- as each entry of the index `packages` array (§D3). There the `hashes` value is different and `file-size` is added (§D10).

| Slot | Constant | Name | Scalar/array schema | v2 letter (`adb_pkg_field_index`, apk_adb.c:48-74) |
|---|---|---|---|---|
| 1 | `ADBI_PI_NAME` = 0x01 | `"name"` | `scalar_name` | `P` |
| 2 | `ADBI_PI_VERSION` = 0x02 | `"version"` | `scalar_version` | `V` |
| 3 | `ADBI_PI_HASHES` = 0x03 | `"hashes"` | `scalar_hexblob` (raw bytes) | `C` |
| 4 | `ADBI_PI_DESCRIPTION` = 0x04 | `"description"` | `scalar_string` | `T` |
| 5 | `ADBI_PI_ARCH` = 0x05 | `"arch"` | `scalar_string` | `A` |
| 6 | `ADBI_PI_LICENSE` = 0x06 | `"license"` | `scalar_string` | `L` |
| 7 | `ADBI_PI_ORIGIN` = 0x07 | `"origin"` | `scalar_string` | `o` |
| 8 | `ADBI_PI_MAINTAINER` = 0x08 | `"maintainer"` | `scalar_string` | `m` |
| 9 | `ADBI_PI_URL` = 0x09 | `"url"` | `scalar_string` | `U` |
| 10 | `ADBI_PI_REPO_COMMIT` = 0x0a | `"repo-commit"` | `scalar_hexblob` (raw bytes) | `c` |
| 11 | `ADBI_PI_BUILD_TIME` = 0x0b | `"build-time"` | `scalar_time` | `t` |
| 12 | `ADBI_PI_INSTALLED_SIZE` = 0x0c | `"installed-size"` | `scalar_hsize` | `I` |
| 13 | `ADBI_PI_FILE_SIZE` = 0x0d | `"file-size"` | `scalar_hsize` | `S` |
| 14 | `ADBI_PI_PROVIDER_PRIORITY` = 0x0e | `"provider-priority"` | `scalar_int` | `k` |
| 15 | `ADBI_PI_DEPENDS` = 0x0f | `"depends"` | `schema_dependency_array` | `D` |
| 16 | `ADBI_PI_PROVIDES` = 0x10 | `"provides"` | `schema_dependency_array` | `p` |
| 17 | `ADBI_PI_REPLACES` = 0x11 | `"replaces"` | `schema_dependency_array` | `r` |
| 18 | `ADBI_PI_INSTALL_IF` = 0x12 | `"install-if"` | `schema_dependency_array` | `i` |
| 19 | `ADBI_PI_RECOMMENDS` = 0x13 | `"recommends"` | `schema_dependency_array` | (none) |
| 20 | `ADBI_PI_LAYER` = 0x14 | `"layer"` | `scalar_int` | (none) |
| 21 | `ADBI_PI_TAGS` = 0x15 | `"tags"` | `schema_tags_array` | `g` |

How apk reads these fields: `package.c:apk_pkgtmpl_from_adb` 558-588. Documentation: `doc/apk-package.5.scd`.

- **name:**
  - On write, `name_fromstring` requires length ≥ 1, `isalnum(first byte)`, and every byte in the PACKAGE_NAME class `[A-Za-z0-9+\-._]` (`apk_adb.c:177-186`; `ctype.c:14, 21-95`).
  - Readers do not validate it. `app_convdb.c:133-135` deliberately writes an unchecked name ("allow leading dot").
  - An **absent name is not rejected by apk's loader.** `apk_db_get_name` with a null blob creates a package name `""` (`database.c:167-189`).
  - Names are opaque byte strings at runtime (§F4.1).
- **version:**
  - ASCII version string (§F1), validated only on write.
  - If absent, apk gets `&apk_atom_null` (`{0,""}`, `atom.c:12, 48`), which is a non-NULL pointer. `apk_db_pkg_add`'s `!pkg->version` check (`database.c:584`) therefore does not catch it.
- **hashes:**
  - Raw digest bytes. Their meaning depends on context: §D10.1 for a package file, §D10.2 for an index.
  - apk uses the value only when its length is ≥ 20 (`APK_DIGEST_LENGTH_SHA1`). The algorithm comes from the length (`package.c:565-566`; `crypto.c:39-59`).
- **description:** apk truncates it to 512 bytes when loading (`package.c:570`). This is reader policy, not a format limit.
- **arch:** for example `noarch` or `x86_64` (`doc/apk-package.5.scd:85-114`). mkpkg defaults it to `APK_DEFAULT_ARCH` (`app_mkpkg.c:469-470`). It drives the arch filter (§F4.1) and `${arch}` in download URLs (§G1.6).
- **license, origin, maintainer, url:** opaque strings.
- **repo-commit:**
  - Raw bytes, normally 20 (binary git SHA-1).
  - `adb_wo_pkginfo`, used for v2 conversion, needs at least 40 input characters. It decodes the first 40 hex chars into exactly 20 bytes and ignores the rest (`apk_adb.c:35-39`).
  - apk renders the value as lowercase hex into an 80-character buffer, so values longer than 40 bytes become the empty atom (`package.c:commit_id` 540-549).
- **build-time:** Unix seconds.
- **installed-size** (bytes, an estimate):
  - mkpkg adds `st_size` of each regular file and counts a hardlinked inode only once (`app_mkpkg.c:288-301`).
  - If `--files` was given, a total of 0 is raised to 1 (line 497). A total of 0 is also raised to 1 when scripts exist (line 499).
  - Otherwise the field is written as an explicit INT 0 (line 500; §B7.3).
  - mkpkg refuses a user-supplied value (`app_mkpkg.c:104-106`).
- **file-size:**
  - Size of the `.apk` file in bytes. mkpkg refuses to write it (`app_mkpkg.c:104-106`).
  - mkndx sets it in the index copy from `stat()` (`app_mkndx.c:305-307, 325`).
  - When apk reads a local package file it overrides the value with the file's size (`package.c:685`).
- **provider-priority:** read with `adb_ro_int(0x0E)` into an `unsigned short`, so it is truncated to 16 bits (`apk_package.h:91`; `package.c:576`).
- **depends, provides, replaces, install-if, recommends:** dependency arrays (§D5.1, §F3).
  - `apk_pkgtmpl_from_adb` loads depends, provides, install_if, recommends and tags (`package.c:583-587`).
  - apk does not load `replaces` from the index. It reads `replaces` from the package at install time, into `ipkg->replaces` (`database.c:apk_db_install_v3meta` 2777).
  - `recommends` is loaded (`package.c:586`) but not used by the solver (`doc/apk-package.5.scd:236-239`).
- **layer:** `APK_DB_LAYER_ROOT` = 0, `APK_DB_LAYER_UVOL` = 1 (`apk_database.h:157-159`). apk stores it in a 3-bit field (`apk_package.h:99`), so it is truncated mod 8.
- **tags:** see §D5.3. apk loads them in array order (`package.c:551-556, 587`).
- **v2 letter map.** `adb_pkg_field_index` (used by `app_convndx.c:30-31` and `app_convdb.c:129-130`) has no entries for `recommends` or `layer`.

### D5. Arrays of scalars and dependencies

#### D5.1 `schema_dependency_array` (`apk_adb.c:404-410`)

Kind ARRAY of `schema_dependency`, initial capacity 32, `pre_commit = adb_wa_sort_unique`. The dependency object, the writer sort and the reader rules are in §F3.3 to §F3.7.

#### D5.2 `schema_string_array` (`apk_adb.c:125-129`)

- Kind ARRAY of `scalar_string`, capacity 32.
- **No `pre_commit`**, so items keep insertion order.
- Used for `ADBI_PKG_TRIGGERS`.

#### D5.3 `schema_tags_array` (`apk_adb.c:118-123`)

- Kind ARRAY of `scalar_string`, capacity 32. No sort and no deduplication.
- Text form is split on `' '` (`tags_fromstring` 108-116; `apk_blob_foreach_word`, `apk_blob.h:78`).
- On write, each tag must consist only of TAG_NAME bytes `[A-Za-z0-9_+\-.,:/\[\]=]` (`ctype.c:18-19, 22-30, 67-69`).

#### D5.4 `schema_xattr_array` (`apk_adb.c:170-175`)

- Kind ARRAY of `schema_xattr`, capacity 8.
- `pre_commit = adb_wa_sort`, bytewise over the raw item blobs with `string_compare`.
- mkpkg writes this array ARRAY-tagged (0xd).
- Item format: §D8.4.

### D6. Package root object: `schema_package` (package file, schema `"pckg"`)

Defined at `apk_adb.h:72-77` and `apk_adb.c:522-533`. Kind OBJECT, `num_fields` = `ADBI_PKG_MAX` = 0x06, `num_compare = ADBI_PKG_PKGINFO`. All file metadata lives here, in the ADB block: names, modes, owners, sizes, hashes, link targets and xattrs. DATA blocks carry only the raw bytes of regular files (`doc/apk-v3.5.scd:153-157`).

| Slot | Constant | Name | Schema |
|---|---|---|---|
| 1 | `ADBI_PKG_PKGINFO` = 0x01 | `"info"` | `schema_pkginfo` (§D4) |
| 2 | `ADBI_PKG_PATHS` = 0x02 | `"paths"` | `schema_dir_array` (§D7) |
| 3 | `ADBI_PKG_SCRIPTS` = 0x03 | `"scripts"` | `schema_scripts` (§D9) |
| 4 | `ADBI_PKG_TRIGGERS` = 0x04 | `"triggers"` | `schema_string_array` (§D5.2, §D9) |
| 5 | `ADBI_PKG_REPLACES_PRIORITY` = 0x05 | `"replaces-priority"` | `scalar_int` |

- **info:** in a package written by mkpkg, `info.hashes` is the 20-byte unique id (§D10.1) and `info.file-size` is absent.
- **paths:** mkpkg writes this array OBJECT-tagged. It is absent when there are no directory entries.
- **scripts:** written only if `--script` was used (`app_mkpkg.c:505-511`). Empty script files become NULL slots, and an all-NULL scripts object becomes absent (§B7.7).
- **triggers:** a list of directory globs, order preserved; OBJECT-tagged; absent when empty (§D9).
- **replaces-priority:** read into an `unsigned short` (`apk_package.h:64`; `database.c:2778`).

**DATA block header.** `struct adb_data_package` (`apk_adb.h:79-82`) is the 8-byte header at the start of each DATA block payload: LE u32 `path_idx` (1-based index into `paths`) and LE u32 `file_idx` (1-based index into that directory's `files`), no padding. See §E1.

### D7. Directory object: `schema_dir`, and `schema_dir_array`

Defined at `apk_adb.h:56-59` and `apk_adb.c:490-506`. `schema_dir` (`apk_adb.c:490-499`) is an OBJECT with `num_fields` = `ADBI_DI_MAX` = 0x04 and `num_compare = ADBI_DI_NAME` (1).

| Slot | Constant | Name | Schema |
|---|---|---|---|
| 1 | `ADBI_DI_NAME` = 0x01 | `"name"` | `scalar_string`: the full directory path relative to the package root |
| 2 | `ADBI_DI_ACL` = 0x02 | `"acl"` | `schema_acl` (§D8.3) |
| 3 | `ADBI_DI_FILES` = 0x03 | `"files"` | `schema_file_array` (§D8.1) |

`schema_dir_array` (`apk_adb.c:501-506`) is an ARRAY with capacity 128 and `pre_commit = adb_wa_sort` (EXACT).

#### D7.1 Sort order of `paths` and `files` (writer side; the order a reader will see)

- `paths` (`schema_dir_array`) and `files` (`schema_file_array`, `apk_adb.c:483-488`) both have `pre_commit = adb_wa_sort`, which uses `qsort_r` with `adb_ro_cmpobj(..., ADB_OBJCMP_EXACT)` (`adb.c:1105-1124`).
- EXACT mode compares **every** slot in index order (`adb.c:559-571`): `name` first, then on ties `acl` (recursively), and so on.
- `name` is compared with `string_compare` → `apk_blob_sort`: memcmp over the common length, then the shorter blob first (`apk_adb.c:88-91`, `blob.c:150-155`). This is unsigned byte order.
- In practice names are unique, so both arrays are sorted by name.
- The root entry has a NULL (length 0) name, so it sorts **first** (index 1) when present.
- A directory name is a proper prefix of its descendants' names, so **a parent directory entry always precedes its descendants** in `paths`. Example order: `"a" < "a-b" < "a/b"`, because `'-'` = 0x2d < `'/'` = 0x2f.
- mkpkg's processing order matches the stored order: root first, then directory paths sorted with `strcmp` (`app_mkpkg.c:487-496`; `apk_string_array_qsort`, `common.c:90-93`); entries within a directory sorted with `strcmp` (`apk_dir_foreach_file_sorted`, `io.c:912-934`). For NUL-free names, `strcmp` order equals `apk_blob_sort` order.
- **The extractor never checks sortedness or uniqueness.** It walks the arrays in stored order.

#### D7.2 Semantics

- **name:**
  - A path relative to the package root, components joined by `/`. Writer fact: mkpkg never writes a leading or trailing `/`; it builds names with the path builder (`mkpkg_scan_dirent`, `app_mkpkg.c:230-231`).
  - The **root directory has an empty name, stored as absent (NULL)**: mkpkg passes `APK_BLOB_STR(NULL)` (`app_mkpkg.c:359, 380`).
  - The extractor emits a directory entry only for a non-empty name (`extract_v3.c:194-198`), so the root's ACL is never applied (§E4).
  - Directories have no size or mtime.
- **Validation on read** (`apk_extract_v3_validate_filetree`): §E7.2. A trailing `/` in a directory name is accepted.
- **Empty directories** are `paths` entries whose `files` are absent or empty.

#### D7.3 Pruning by mkpkg (writer fact; `app_mkpkg.c:369-376, 421-439`)

`mkpkg_process_directory` may omit a directory. A directory is a candidate (it is "default") only if all of these hold:
- `st_mode & ~S_IFMT == 0755`;
- no file entries;
- its owner and group resolve to user name `"root"` and group name `"root"`.

Two flags, `compat_rootnode` and `compat_dirnode`, are set by `mkpkg_setup_compat` from `--compat` (default `"3.0.0_pre1"`, `app_mkpkg.c:138`; `--rootnode` / `--no-rootnode` set `3.0.0_pre1` / `3.0.0_pre3`, `app_mkpkg.c:152-153`):

| `--compat` value | Flags | Root dir | Non-root dirs |
|---|---|---|---|
| < `3.0.0_pre3` (includes the default) | both set | always emitted | always emitted |
| ≥ `3.0.0_pre3` and < `3.0.0_rc9` | only `compat_dirnode` set | omitted when "default" | always emitted |
| ≥ `3.0.0_rc9` | neither set | omitted when "default" | omitted when "default" and `num_dirents != 0` |

- The root is pruned when it is a candidate and `!compat_rootnode`.
- A subdirectory is pruned when it is a candidate, `num_dirents != 0` and `!compat_dirnode`. `num_dirents` counts every directory entry seen, including subdirectories and ignored special files. So only directories that contain nothing but subdirectories or ignored entries are pruned; empty leaf directories are kept.
- A directory that holds files is never pruned, so **every file's immediate parent has a `paths` entry**; only ancestors can be missing. apk's installer requires a directory entry for any file outside the root (`database.c:2832-2839`, error "no dirent in archive"). Readers must create missing ancestors (§E4).

### D8. File, target, ACL and xattr objects

#### D8.1 `schema_file` and `schema_file_array`

Defined at `apk_adb.h:47-53` and `apk_adb.c:469-488`. `schema_file` (`apk_adb.c:469-481`) is an OBJECT with `num_fields` = `ADBI_FI_MAX` = 0x07 and `num_compare = ADBI_FI_NAME` (1). `schema_file_array` is an ARRAY with capacity 128 and `pre_commit = adb_wa_sort` (EXACT, effectively by name; §D7.1).

| Slot | Constant | Name | Schema |
|---|---|---|---|
| 1 | `ADBI_FI_NAME` = 0x01 | `"name"` | `scalar_string`: the basename, a single path component (validation forbids `/`, §E7.2) |
| 2 | `ADBI_FI_ACL` = 0x02 | `"acl"` | `schema_acl` (§D8.3) |
| 3 | `ADBI_FI_SIZE` = 0x03 | `"size"` | `scalar_int` (u64, bytes) |
| 4 | `ADBI_FI_MTIME` = 0x04 | `"mtime"` | `scalar_time` (u64, seconds since the epoch) |
| 5 | `ADBI_FI_HASHES` = 0x05 | `"hash"` (singular) | `scalar_hexblob`: **raw digest bytes** (hex only in text dumps) |
| 6 | `ADBI_FI_TARGET` = 0x06 | `"target"` | `scalar_hexblob`: raw bytes (§D8.2) |

Reader: `extract_v3.c:apk_extract_v3_file` 52-117. Writer: `app_mkpkg.c:mkpkg_process_dirent` 252-348.

- **hash:**
  - The algorithm comes from the length via `apk_digest_from_blob`: 20 → SHA1 (0x02), 32 → SHA256 (0x03), 64 → SHA512 (0x04), anything else → NONE (`crypto.c:39-59`; §A3.1).
  - mkpkg writes a 32-byte SHA-256 of the content (`APK_FI_DIGEST(APK_DIGEST_SHA256)`) for every `S_IFREG` entry, hardlinks included (`app_mkpkg.c:279, 329-330`). No hash is written for symlinks, devices or FIFOs.
  - convdb converts v2 installed-DB `Z:` values, which are usually 20-byte SHA-1 (`app_convdb.c:178-181`).
- **Regular file** (no target): the hash is mandatory, NONE gives `-APKE_ADB_SCHEMA` (`extract_v3.c:105`); the mode becomes `acl.mode | S_IFREG`; content comes from a DATA block iff `size != 0` (`extract_v3.c:112`, `204-207`). Details in §E2.
- **target present:** the entry is a symlink, device, FIFO or hardlink. It never has a DATA block, whatever its `size` (`extract_v3.c:204`; `app_mkpkg.c:566`). No hash is required.
- **size:** mkpkg writes the `lstat` size for every entry type, so symlinks get the full link length (`app_mkpkg.c:279, 336`; `io.c:789-792`). It is written even when 0.
- **mtime:** mkpkg writes `apk_get_build_time(st_mtime)` (`app_mkpkg.c:335`). If `SOURCE_DATE_EPOCH` is set and non-empty, that value **replaces** every mtime unconditionally; it is not a clamp to a maximum (`common.c:95-110`).

#### D8.2 `ADBI_FI_TARGET` byte layout

Sources: reader `extract_v3.c:71-103`; writer `app_mkpkg.c:237-250, 263-273, 302-315`.

```
offset 0: u16 LE  file type (an exact S_IFMT value; no permission bits)
offset 2: type-specific payload
```

The reader `switch`es on the whole u16, so permission bits make it an unknown type. The constants are Linux `<sys/stat.h>` values:

| type | octal | hex | LE bytes | payload after the 2 bytes |
|---|---|---|---|---|
| `S_IFIFO` | 0010000 | 0x1000 | `00 10` | exactly 8 bytes: u64 LE device number |
| `S_IFCHR` | 0020000 | 0x2000 | `00 20` | exactly 8 bytes: u64 LE device number |
| `S_IFBLK` | 0060000 | 0x6000 | `00 60` | exactly 8 bytes: u64 LE device number |
| `S_IFREG` | 0100000 | 0x8000 | `00 80` | **hardlink**: path of the link target relative to the package root (bytes, no NUL) |
| `S_IFLNK` | 0120000 | 0xA000 | `00 a0` | symlink target (bytes, no NUL) |

(For reference, `S_IFDIR` is 0x4000 and `S_IFSOCK` 0xC000 on Linux.)

Reader checks (`extract_v3.c:76-100`):
- A target blob shorter than 2 bytes gives `-APKE_ADB_SCHEMA` (`extract_v3.c:76`). This includes a present but zero-length target.
- **BLK, CHR, FIFO:** the remaining length must be exactly 8 (`sizeof(uint64_t)`), total blob length 10; any other length gives `-APKE_ADB_SCHEMA`. `fi.device = apk_unaligned_le64(...)`. Writer fact: mkpkg packs `{u16 mode; u64 dev}` with `__attribute__((packed))` (`ft.dev`, 10 bytes).
- **LNK, REG:** the remaining length must be `< PATH_MAX - 1`, i.e. ≤ 4094 with Linux `PATH_MAX = 4096`, else `-APKE_ADB_SCHEMA`.
  - The bytes are copied to an `alloca` buffer and NUL-terminated. The target is then used as a C string, so an embedded NUL truncates it.
  - A 2-byte target (empty path) passes this check.
  - Writer fact: mkpkg's symlink buffer is 1022 bytes (`ft.symlink.target[1022]`), so it writes at most 1022 target bytes (§E2).
- Any other type value, including `S_IFDIR` (0x4000) and `S_IFSOCK` (0xC000), gives `-APKE_ADB_SCHEMA`.
- Then `fi.mode |= type` (`extract_v3.c:100`), and the callback runs with a NULL stream (§E8).

Per-kind semantics, including the hardlink-target quirk for root-level files, are in §E2.

#### D8.3 ACL object: `schema_acl`

Defined at `apk_adb.h:40-44` and `apk_adb.c:458-467`. Kind OBJECT, `num_fields` = `ADBI_ACL_MAX` = 0x05, no `num_compare`.

| Slot | Constant | Name | Schema |
|---|---|---|---|
| 1 | `ADBI_ACL_MODE` = 0x01 | `"mode"` | `scalar_oct` ("oct" only describes the text form) |
| 2 | `ADBI_ACL_USER` = 0x02 | `"user"` | `scalar_string`: a user **name** |
| 3 | `ADBI_ACL_GROUP` = 0x03 | `"group"` | `scalar_string`: a group **name** |
| 4 | `ADBI_ACL_XATTRS` = 0x04 | `"xattrs"` | `schema_xattr_array` (§D5.4) |

- `doc/apk-v3.5.scd:107-115` lists only mode, user and group; the code also has xattrs.
- **mode:** permission bits only. Writer fact: mkpkg writes `st_mode & 07777` for files (`app_mkpkg.c:338`) and `st_mode & ~S_IFMT` for directories (`app_mkpkg.c:371`); neither includes file-type bits. The type comes from context: `S_IFDIR` for directories, and `S_IFREG` or the target's type for files (`extract_v3.c:100, 106, 133`).
- **user and group** are names, not numeric ids. Writer fact: mkpkg writes `"root"` for id 0 and `"nobody"` for an id it cannot resolve (`io.c:1479-1497`; `app_mkpkg.c:339-340, 369-370`).
- **ACL absent:** mode 0, uid and gid 65534.
- How the reader turns these into a mode, uid and gid: §E5.

#### D8.4 xattr item (`schema_xattr`, `apk_adb.c:131-175`)

- Each item is one blob: the attribute name bytes, one `\0`, then the raw value bytes.
- Writer fact: mkpkg's `create_xattrs` (`app_mkpkg.c:188-216`) concatenates the name together with its terminating NUL, then the raw value (vector at `app_mkpkg.c:205-209`); value buffers are 1024 bytes. mkpkg does not record xattrs on symlinks, because `openat(..., O_NOFOLLOW)` fails (`app_mkpkg.c:342`).
- The reader splits at the **first** NUL; a blob without a NUL, including a NULL item, fails the entry with `-APKE_ADB_SCHEMA` (§E5.3).
- Text form: `name=<hex>`.
- **Text-form writer anomaly.** `xattr_fromstring` (`apk_adb.c:146-161`), used when building an ADB from text, splits `"name=hex"` at the last `'='` and then does `b[0].len++`. That appends the byte following the name in the input, which is the `'='`, not a NUL. As read, such blobs would contain no NUL and would fail the reader's split. This is writer-side only (mkpkg does not use it). **[unconfirmed]** Not tested.

### D9. Scripts and triggers

**Scripts object: `schema_scripts`.** Defined at `apk_adb.h:62-69` and `apk_adb.c:508-520`. Kind OBJECT, `num_fields` = `ADBI_SCRPT_MAX` = 0x08. Every field is `scalar_mstring`: a raw blob (BLOB_8/16/32) holding the script file bytes. **Scripts are not in DATA blocks.**

| Slot | Constant | Name | apk internal type (`apk_package.h:24-32`) |
|---|---|---|---|
| 1 | `ADBI_SCRPT_TRIGGER` = 0x01 | `"trigger"` | `APK_SCRIPT_TRIGGER` = 6 |
| 2 | `ADBI_SCRPT_PREINST` = 0x02 | `"pre-install"` | `APK_SCRIPT_PRE_INSTALL` = 0 |
| 3 | `ADBI_SCRPT_POSTINST` = 0x03 | `"post-install"` | `APK_SCRIPT_POST_INSTALL` = 1 |
| 4 | `ADBI_SCRPT_PREDEINST` = 0x04 | `"pre-deinstall"` | `APK_SCRIPT_PRE_DEINSTALL` = 2 |
| 5 | `ADBI_SCRPT_POSTDEINST` = 0x05 | `"post-deinstall"` | `APK_SCRIPT_POST_DEINSTALL` = 3 |
| 6 | `ADBI_SCRPT_PREUPGRADE` = 0x06 | `"pre-upgrade"` | `APK_SCRIPT_PRE_UPGRADE` = 4 |
| 7 | `ADBI_SCRPT_POSTUPGRADE` = 0x07 | `"post-upgrade"` | `APK_SCRIPT_POST_UPGRADE` = 5 |

(`APK_SCRIPT_MAX` = 7.)

- The slot-to-type mapping is at `database.c:2760-2768`. An absent slot means no script of that kind (`database.c:2783-2784`). Script arguments are described in `doc/apk-package.5.scd:275-282`.
- **Writer.** mkpkg adds the `scripts` object only if at least one `--script type:file` was given (`app_mkpkg.c:155-171, 505-511`). An empty script file becomes an absent field, because zero-length blobs are not written.
- **Installer.** Each script blob is duplicated into `ipkg->script[]` (`apk_ipkg_assign_script`, `package.c:696-706`). `APK_MAX_SCRIPT_SIZE = 262144` (`apk_defines.h:145`) is enforced only in `apk_ipkg_add_script` (`package.c:708-714`), which is the v2 path. **The v3 path has no size limit.** `ops->script` is never called by extract_v3.
- **Execution name.** `lib/apk/exec/<pkgname>-<version>.<type>` (`apk_ipkg_run_script`, `package.c:743, 754`; `PKG_VER_FMT "%s-" BLOB_FMT`, `apk_package.h:119`). `<type>` comes from `apk_script_types[]` (`package.c:385-393`), the same strings as the field names. apk uses a memfd when possible; otherwise it writes the file relative to the root with mode 0755 and unlinks it afterwards (`package.c:759-815`).
- When the pending pre-install or pre-upgrade script runs: §E8.1.

**Triggers.** `package[4]` is an array of strings: directory globs, each optionally prefixed with `+` (`doc/apk-package.5.scd:286-297`). Order is preserved (no `pre_commit`).
- The installer copies them into `ipkg->triggers` (`database.c:2789-2794`).
- mkpkg writes them from `--trigger`, OBJECT-tagged. An empty list becomes an absent field (`app_mkpkg.c:512-519`; `adb.c:949-954`).

**`replaces-priority`** is `package[5]`, an int (`database.c:2778`).

### D10. What `hashes` contains, and how packages are identified

#### D10.1 In a package file (`info.hashes`): a 20-byte "unique id"

Source: `app_mkpkg.c:449-452, 501, 527-530`.
1. mkpkg writes 20 zero bytes into `info.hashes` with `adb_wo_blob_raw`, which bypasses deduplication (`adb.c:772-778, 1016-1020`). The result is a BLOB_8 with length byte 0x14.
2. After the whole ADB is built, mkpkg computes **SHA-256 over the entire ADB buffer** (`ctx->db.adb`, starting at `adb_hdr`) while those 20 bytes are still zero. This buffer is exactly the ADB block payload later written by `adb_c_adb` (`adb.c:1285-1295`).
3. It copies the **first 20 bytes** of that digest into the blob in place.

A reader could recompute this value by zeroing those 20 bytes and hashing, but apk never does: `__adb_handle_identity` ignores SHA1-class identities "as they are 'unique-id' instead of hash" (`adb.c:118-119`). `doc/apk-package.5.scd:69-71` says `hashes` exists only in the index copy, which does not match what mkpkg writes (§H1).

#### D10.2 In the index (`packages[i].hashes`): the package identity

Source: `app_mkndx.c:315-336`.

mkndx extracts each new package with `apk_extract_generate_identity(…, hash_alg, &digest)`. It then writes `file-size` and **overwrites** `ADBI_PI_HASHES` with that digest. For v3 packages, every other field of `info` is copied verbatim, slot tags included (`mkndx_parse_v3meta` 188-197, `adb_wo_copyobj`). Entries reused from an old index (`--index`) are copied unchanged (line 313).

- **v3 package:** the identity is a digest of **the ADB block payload exactly as stored in the .apk**, after decompression (`__adb_handle_identity`, `adb.c:109-136`; called at `adb.c:167` for mmap and `288` for streams).
  - The payload starts at `adb_hdr`, runs to the end of the value area, and includes the 20-byte unique id from §D10.1.
  - It excludes the `"ADB."`+schema file header, the block header, the block's alignment padding, and all SIG and DATA blocks.
  - `--hash` accepts only `sha256` (default, `APK_DIGEST_SHA256`) and `sha256-160` (`ALLOWED_HASH`, `app_mkndx.c:42, 62, 72-78`; names from `crypto.c:3-9`):

    | `--hash` | Stored value |
    |---|---|
    | `sha256` | the full 32-byte SHA-256 |
    | `sha256-160` | the first 20 bytes of SHA-256 (`adb_digest_adb`, `adb.c:80-85, 100`) |
- **v2 package indexed by mkndx:** the identity is always the **SHA-1 (20 bytes) of the compressed bytes of the v2 control gzip segment**, whatever `--hash` says.
  - `extract_v2.c:56` initialises the identity context as SHA-1; lines 183-188 feed it; lines 235-236 finalise it.
  - The multipart callback receives compressed input bytes (`io_gunzip.c:82-83`).
- **v2 APKINDEX converted by `apk convndx`:** the `C:` value is decoded by `apk_blob_pull_digest` (`apk_adb.c:30-34`; `blob.c:567-615`; algorithm in §F3.8):
  - `Q1` + base64 gives 20 bytes and `X1` + hex gives 20 bytes;
  - `Q2`/`X2` give 32 bytes;
  - quirk: a `Q1`/`X1` string followed by exactly 16 more base64 or 24 more hex characters is upgraded to a 32-byte SHA-256 (`blob.c:592-607`).
  - Values shorter than 4 characters are rejected (`apk_adb.c:31`).

#### D10.3 How a reader interprets `hashes` (`package.c:565-566`; `crypto.c:39-59`; `database.c:1496-1500`)

- If `len < 20`, `tmpl.id` stays NONE. `load_v3index` counts the entry as broken, **skips the package**, and warns "Repository … has N packages without hash" (`database.c:1510`).
- Otherwise the algorithm is chosen by length: **20 → SHA1 (0x02), 32 → SHA256 (0x03), 64 → SHA512 (0x04)**. Any other length gives NONE and the entry is skipped.
- A 20-byte sha256-160 index hash is therefore classified as SHA1. `APK_DIGEST_SHA256_160` (0x05) is assigned only to packages with an installed record carrying the `sha256_160` flag (`database.c:593-594`). That flag is set when a v3 package is installed (`database.c:2779`) or read from the installed DB (`database.c:1032`).
- `apk_db_pkg_add` (`database.c:584`) rejects only `id.len < 20`, a NULL name or a NULL version. As noted in §D4, absent name and version still produce non-NULL values. In practice the `-APKE_ADB_SCHEMA` branch of `load_v3index` (`database.c:1503-1505`) is reached only on allocation failure.

#### D10.4 The lookup key (`apk_package.h:104-110`; `database.c:84-103, 579-631`)

- apk's package table is keyed by the **first 20 bytes** (`APK_DIGEST_LENGTH_SHA1`) of the identity (`apk_pkg_hash_blob`). The hash function is the first 4 bytes read as u32 LE (`csum_hash`).
- The same 20-byte key in several repositories is treated as one package, and the repository bitmask is OR-ed (`database.c:607-615`; first copy wins, §F4.1).
- The full digest and its algorithm are kept (`digest_alg`, `digest[]`) for verification.
- Each package is registered as a provider of its own `name` and of every `provides` name (`database.c:602-604`; §F4.1).

#### D10.5 Verification when a package is fetched

Sources: `database.c:717, 3190`; `app_fetch.c:197` (all call `apk_extract_verify_identity`); `adb.c:109-136` (§C5.3).

| Index digest algorithm | v3 package | v2 package |
|---|---|---|
| SHA256 (32 bytes) | SHA-256 of the downloaded ADB payload must match, else `-APKE_ADB_INTEGRITY`. A match marks the package trusted, so no signature is needed. | (not covered here) |
| SHA1 (any 20-byte hash) | identity check skipped; trust must come from a valid SIG block | SHA-1 of the compressed control segment is compared (`extract_v2.c:254-263`) |
| SHA256_160 (installed records only) | a mismatch is silently ignored and the signature decides; a match marks the package trusted | (not covered here) |

SHA512 (64 bytes) is handled like SHA256: the digest must match, and a match is trusted (§C5.3).

#### D10.6 File content hashes

`ADBI_FI_HASHES` is the digest of the file content: §D8.1 and §E3.

#### D10.7 Edge case: local package files

When apk reads a v3 `.apk` directly (`apk_pkg_read`, `package.c:662-694`):
1. It sets up generation of the SHA-256 identity (line 673). The identity is computed while the ADB block is processed (`adb.c:167/288`).
2. Later, `apk_extract_v3_next_file` calls `v3meta` (`extract_v3.c:176`). `apk_pkg_v3meta` then calls `apk_pkgtmpl_from_adb`, which **overwrites** `tmpl->id` with `info.hashes`, the 20-byte unique id, because its length is ≥ 20 (`package.c:565-566, 651-652`).

A locally read package is therefore keyed by its unique id, while the same package from an index is keyed by the first 20 bytes of its SHA-256 identity. This is what the code does; the source does not say whether it is intended.

### D11. Installed-DB schema (`ADB_SCHEMA_INSTALLED_DB`, `"idb"`): experimental

Defined at `apk_adb.h:91-92` and `apk_adb.c:535-554`.

- **Writer and readers.** The writers in 3.0.8 are `apk convdb`, which writes `./installed.adb` (`app_convdb.c:222-225`), and `adbgen` from YAML. No apk code path loads `schema_idb` except the generic dumper; `apk_extract_v3` rejects this schema (§D1.4).
- **Root.** `schema_idb` is an OBJECT with `num_fields` = `ADBI_IDB_MAX` = 0x02. Its one field is slot 1, `ADBI_IDB_PACKAGES` (0x01), `"packages"`, of type `schema_package_adb_array`. convdb writes the array OBJECT-tagged (`app_convdb.c:219`).
- **Array.** `schema_package_adb_array` is an ARRAY (capacity 128, `pre_commit = adb_wa_sort`). Its items have kind `ADB_KIND_ADB` (`schema_package_adb`: `schema_id = ADB_SCHEMA_PACKAGE`, `schema = &schema_package`).
- **Items.** Each item is a nested ADB: a BLOB_32 holding one complete, headerless, unsigned ADB block whose root is a `schema_package` object. Encoding, reading rules (offsets relative to the embedded payload, schema implied), the NULL-trust crash and the hardening recommendation are in §B7.9.
- **Sort.** `adb_wa_sort` on this array calls `adb_m_blob(..., NULL)` (`adb.c:1093-1103`). With no trust context that parse ends with `-APKE_SIGNATURE_UNTRUSTED` and clears `db->adb`. The comparison therefore probably sees empty objects, and the stored order is not meaningful. **[unconfirmed]** Read from the code, not tested.

---

## Part E. Package contents: DATA blocks and extraction

Block framing and the block-order and trust rules are in §B5; the package schemas in §D6 to §D9. This part covers how file contents are tied to the schema and how apk extracts them.

### E1. DATA blocks

#### E1.1 Payload layout (`apk_adb.h:79-82`; reader `extract_v3.c:234-245`; writer `app_mkpkg.c:568-577`)

```
offset  size  field
0       4     u32 LE path_idx   -- 1-based index into package.paths[]
4       4     u32 LE file_idx   -- 1-based index into paths[path_idx].files[]
8       N     raw file content; N == file.size exactly
```

- `struct adb_data_package { uint32_t path_idx; uint32_t file_idx; }` is 8 bytes with no padding. The content follows immediately. There is no per-file padding beyond the block padding.
- Writer facts: the payload length is 8 + file size (`adb.c:adb_c_block_data:1229-1253`; `app_mkpkg.c:561-578`). DATA blocks are written only for files with no `target` and size ≠ 0, in directory/file array order, with `path_idx` and `file_idx` the 1-based array indices. mkpkg re-reads the committed root before emitting data (`app_mkpkg.c:522-525`). Files larger than 1,073,741,811 bytes get an EXT header (§B5.1).

Worked example: a 5-byte file "hello" at path 2, file 3. payload_len = 8 + 5 = 13, raw_size = 17.

```
11 00 00 80                 type_size = 0x80000000 | 17
02 00 00 00 03 00 00 00     path_idx=2, file_idx=3
68 65 6c 6c 6f              "hello"
00 00 00 00 00 00 00        7 padding bytes (17 -> 24)
```

#### E1.2 Which entries get a DATA block, and in what order

A `file` entry gets exactly one DATA block **iff** `size != 0` **and** `target` is NULL. Here NULL means `ptr == NULL`, i.e. the field is absent or out of range (§B7.5).
- Reader: `apk_extract_v3_next_file`, `extract_v3.c:203-205`.
- Writer: `mkpkg_main` skips non-NULL targets and `size == 0` (`app_mkpkg.c:564-567`).

Consequences:
- Empty regular files, symlinks, hardlinks, and device/FIFO nodes never have DATA blocks.
- Directories are not `file` entries.
- A zero-size file **must not** have a DATA block. Such a block would not match the pending file and fails with `-APKE_ADB_BLOCK`.

**Order.** Take the entries in this order:
1. walk `paths[1..n]` in stored order;
2. within each path, walk `files[1..m]` in stored order;
3. keep only the entries that need data.

Each DATA block's `(path_idx, file_idx)` must equal the next such entry. No gaps, reordering or extra blocks are allowed (`extract_v3.c:228-243`). Errors, all `-APKE_ADB_BLOCK`:
- a mismatched index, or `block_length - 8 != size` (`extract_v3.c:238-243`);
- a DATA block left over after all such files are consumed (`next_file` returns 1, `extract_v3.c:230`);
- a required DATA block missing at end of file (`extract_v3.c:294-296`).

Other conditions:
- DATA blocks are accepted only in files with schema `pckg`; otherwise `-APKE_ADB_SCHEMA` (`extract_v3.c:225`).
- If the consumer's `ops->v3meta` is NULL, a DATA block gives `-APKE_FORMAT_NOT_SUPPORTED` (`extract_v3.c:226`).
- The block-order and trust rules (ADB, then SIG*, then DATA*; DATA only once trusted) are in §B5.2.

#### E1.3 Per-DATA-block checks (`apk_extract_v3_data_block`, `extract_v3.c:216-246`)

1. Non-DATA blocks: return 0. The callback is also invoked for ADB and SIG blocks.
2. Call `apk_extract_v3_next_file` (§E8). While advancing, this may emit directory and no-data file callbacks.
   - `< 0`: returned as-is.
   - `> 0`: no file is pending (all done), so `-APKE_ADB_BLOCK`.
3. Read 8 header bytes with `apk_istream_get`, then `sz = block_length - 8`.
   - The subtraction precedes the error check.
   - A payload shorter than 8 bytes makes the `get` fail, normally with `-APKE_EOF` (`io.c:163-186`).
4. All three must hold, else `-APKE_ADB_BLOCK`:
   - `le32(path_idx) == cur_path`
   - `le32(file_idx) == cur_file`
   - `sz == file.size` (u64 compare)
5. Extract the file with the remaining `sz` bytes as its stream (§E3, §E8).

### E2. File kinds

The target blob layout and its reader checks are in §D8.2.

**Regular file with content** (`size > 0`, `target` NULL):
- The content comes from the matching DATA block.
- `hash` is required. Its length must be 20, 32 or 64. Otherwise the digest algorithm is NONE and the result is `-APKE_ADB_SCHEMA` (`extract_v3.c:69, 105`).
- `fi.mode = acl.mode | S_IFREG` (`extract_v3.c:106`).
- The content is verified against `hash` (§E3).

**Empty regular file** (`size == 0`, `target` NULL):
- There is no DATA block. The file is extracted during the tree walk with a NULL stream (`extract_v3.c:207, 107-109`).
- `hash` must still have length 20, 32 or 64, else `-APKE_ADB_SCHEMA` (line 105 runs before the `!is` test). The hash is **not compared** with anything.
- Writer fact: mkpkg writes the SHA-256 of the empty content (`app_mkpkg.c:279, 329-330`; `io.c:846-858`).
- Consumer: `apk_stream_copy(NULL, os, 0, ...)` never touches the stream (`io.c:661-685`; `fs_fsys.c:113-125`).

**Symlink** (type `S_IFLNK`):
- `fi.link_target` is the payload, and `fi.mode = acl.mode | S_IFLNK`. There is no DATA block.
- Writer facts:
  - mkpkg reads the target with `readlinkat` into a **1022-byte** buffer (`ft.symlink.target[1022]`, `app_mkpkg.c:270-273, 309-314`). Longer targets are silently truncated to 1022 bytes.
  - It writes no `hash` (the hash is written only for `S_IFREG`, `app_mkpkg.c:329-330`).
  - `size` is the `lstat` `st_size`, i.e. the full untruncated target length (`io.c:789-792`). A reader must not treat it as a content length.
- The target string is not validated: `symlinkat(link_target, ...)` as-is (`fs_fsys.c:136-137`). Absolute and `..` targets are allowed.

**Hardlink** (type `S_IFREG` with a target):
- The payload is the package-relative path of an earlier file entry: `"<dir name>/<file name>"`. apk resolves it against files already extracted from the same package (`database.c:2845-2853` → `apk_db_ipkg_find_file` 857-878).
- `fi.mode = acl.mode | S_IFREG`, `fi.link_target` = that path.
- There is **no DATA block**, even though, as mkpkg writes it, `size` holds the full file size and `hash` holds the content digest (mkpkg writes the hash for every `S_IFREG`, `app_mkpkg.c:282-301, 329-336`).
- Writer facts (how mkpkg picks targets, `app_mkpkg.c:283-299`):
  - The first-processed occurrence of an inode with `st_nlink > 1` is written as a normal file with data, and its target string is remembered.
  - Later occurrences of the same `(st_dev, st_ino)` get that target.
  - Processing order equals stored order (§D7.1): root first, then subdirectories in sorted order. So the target precedes the link in the extraction walk.
- **Writer quirk for root-level targets.**
  - `hardlink_target(db, path, name)` writes `path + "/" + name` whenever `path[0] != 0` (`app_mkpkg.c:244-247`).
  - For a subdirectory, `path` is the directory name, so targets look like `"usr/bin/foo"`.
  - For the package root, `mkpkg_process_directory` passes `path = entry ?: "."` (`app_mkpkg.c:358, 366`), and `apk_dir_foreach_file_sorted(atfd, ".", ...)` forwards that `path` to the callback (`io.c:912-927`, line 926).
  - So a hardlink whose target is a **root-level** file gets the target string **`"./<name>"`**, not `"<name>"`.
- Consumers:
  - **fs layer.** `fsys_file_extract` rejects a target that fails `apk_fs_is_malicious_pathname` with `-APKE_ADB_SCHEMA`. Otherwise it calls `linkat(target, name)`. The installer links against the target's temporary name (`fs_fsys.c:126-134`).
  - **Database installer.** The target must be a file already recorded in this package's dir instances (`apk_db_ipkg_find_file`, `database.c:857-878, 2845-2854`). If not, it logs "no hard link target (...) in archive", sets `ipkg->broken_files = 1` and **skips the entry, without aborting**.
  - **`apk extract`.** The target must be a regular file already extracted from the same package in this run (`app_extract.c:114-121`). If not, it warns and skips the entry.
- Consequences of `"./<name>"` **[unconfirmed: traced through the code, not tested]**:
  - the installer looks up directory `"."`, finds nothing, and the hardlink is skipped with `broken_files` set;
  - `apk extract` warns, because the key `"./name"` is not in its hash;
  - the fs layer would reject it, because the component `"."` is malicious (`fs_fsys.c:129, 264-285`).
  - This is an inconsistency between apk's writer and its readers. A reimplementation may choose to strip a leading `"./"`; apk itself does not.

**Block device, char device, FIFO:**
- The payload is a u64 LE device number.
- Writer fact: `ft.dev` is a packed `{u16 mode; u64 dev}` of 10 bytes. mkpkg writes `htole64(fi.device)`, where `fi.device = st_rdev` (`app_mkpkg.c:265-268, 302-307`; `io.c:797`). FIFOs also carry 8 device bytes, normally 0.
- Consumer: `mknodat(fn, fi->mode, fi->device)` (`fs_fsys.c:139-144`). So the value is the host libc/kernel `dev_t` encoding; apk defines no major/minor split of its own.
  - On Linux (glibc/musl) `makedev(1,3) = 0x103`. That is libc knowledge, not from this source, although apk itself calls `makedev(1, 3)` for `/dev/null` (`package.c:721`).
- With `APK_FSEXTRACTF_NO_DEVICES = 0x0008`, the node is not created and the result is `-APKE_NOT_EXTRACTED`. Usermode databases set this flag (`fs_fsys.c:142`; `apk_fs.h:52`; `database.c:2044`).
- No hash, no DATA block.

**Sockets and other types** cannot be represented.
- mkpkg prints "special file ignored" and skips them (`app_mkpkg.c:319-323`).
- A `target` with type `S_IFSOCK` is a schema error on read.

**Directories** are `paths` entries, not `file` entries (§E4).

### E3. Content verification

**Digest algorithm** is chosen purely by the `hash` length (`apk_digest_from_blob` → `apk_digest_alg_by_len`, `crypto.c:39-59`; table in §A3.1): 20 → SHA-1, 32 → SHA-256 (what mkpkg always writes), 64 → SHA-512, anything else including 0 → NONE. Length 20 never maps to `APK_DIGEST_SHA256_160`.

**What is hashed.** Exactly the `size` content bytes: the DATA payload after the 8-byte header. The header and the padding are not hashed (`extract_v3.c:112, 234-245`).

**When it is checked.**
- `apk_istream_verify` (`io.c:377-395`) wraps the stream.
  - At construction it **immediately hashes whatever is already buffered** in the underlying stream (`is->ptr..is->end`, at most the rest of the payload) and subtracts that from `size_left`.
  - Every later read through the wrapper is hashed too (`digest_read`, `io.c:342-353`).
- On close, `digest_close` (`io.c:355-369`) finalizes and compares **only if `size_left == 0`**.
  - The comparison is `apk_digest_cmp` (`apk_crypto.h:49-52`): the same alg and memcmp over the computed length.
  - A mismatch sets `-APKE_FILE_INTEGRITY`, which `apk_extract_v3_file` returns through `apk_istream_close_error` (`extract_v3.c:112-113`).
- So a consumer that reads less than the whole content skips verification only if the not-yet-buffered remainder was never read.
  - In the mmap path the whole payload is buffered, so verification always happens.
  - In the stream path the outcome depends on buffering.
  - The rest of the block is then skipped by `segment_close` (`io.c:296-303`; `adb.c:314-316`).
  - **A reimplementation should always verify.**
- The installer verifies **after** writing the file to its temporary name. A mismatch aborts the whole package with `-APKE_FILE_INTEGRITY`.

**`verify` applet.** It calls `apk_extract` with `ops == NULL`, so `apk_extract_v3` substitutes `extract_v3verify_ops` (`app_verify.c:25-26`; `extract_v3.c:258-271, 284`). Its file callback skips `fi->size` bytes and then closes, so every content hash is checked.

**Installed-database bookkeeping** (not part of the wire format; `database.c:2894-2910`):
- The DB stores at most a **20-byte** digest per file: `uint8_t digest[20]` (`apk_database.h:49`).
- `apk_dbf_digest_set` stores NONE when the algorithm's length exceeds 20 (`apk_database.h:57-65`).
- v3 installs set `ipkg->sha256_160 = 1` (`database.c:2779`). Then:
  - symlinks: `SHA256_160` of the target string (`apk_digest_calc(..., APK_DIGEST_SHA256_160, link_target, strlen)`);
  - content digest SHA-256: stored as `SHA256_160`, i.e. the first 20 bytes of the SHA-256, labelled `APK_DIGEST_SHA256_160`;
  - SHA-1: stored as SHA-1;
  - SHA-512: stored as NONE.

### E4. Directories in extraction (`apk_extract_v3_directory`, `extract_v3.c:119-139`)

**When the callback fires.**
- Each `paths[i]` whose path-builder length is non-zero produces **one** directory callback, before any of its files.
- The condition is `ctx->pb.namelen != 0 && ctx->cur_file == ADBI_FIRST` (`extract_v3.c:194-198`).
- Resuming inside a path after a DATA block has `cur_file > 1`, so the callback is not repeated.

**Callback fields.**
- `name` = the directory name as stored.
- `mode = acl.mode | S_IFDIR`.
- uid and gid come from the ACL; xattrs too (§E5).
- `size`, `mtime` and digest are all 0 / NONE.

**Root entry.** An entry with a NULL or empty name produces **no** callback. Its ACL is ignored by extraction, and its files get names with no `/`.

**Ancestors with no `paths` entry.** extract_v3 never synthesizes parents. mkpkg may prune directories (§D7.3), but never a file's immediate parent. Readers must create missing ancestors:
- **Installer.** `apk_db_dir_get` recursively creates parent dir objects (`database.c:336-343`). `apk_db_dir_prepare` creates them on disk with `apk_default_acl_dir` = mode 0755, uid 0, gid 0 (`database.c:247-248, 2025`). In usermode, `S_IWUSR | S_IXUSR` is added (`database.c:52-57`). So implied ancestors are **0755 root:root**.
- **`apk extract`.** It warns "parent is not a directory" and skips any entry whose parent was not extracted earlier in the same run (`app_extract.c:105-112`). As written, everything beneath a pruned ancestor would be skipped. **[unconfirmed]** Inferred from code, not tested.

**Consumers.**
- `apk extract` uses `apk_fs_extract` → `apk_fsdir_create` (`fs_fsys.c:305-308`).
- The installer uses `apk_db_dir_prepare` → `apk_db_dir_mkdir` → `apk_fsdir_create` (`database.c:233-270, 2924-2934`).
- Both end in `fsys_dir_create` (`fs_fsys.c:20-36`):
  - `mkdirat(name, mode)`;
  - then `fchownat(uid, gid)`;
  - then `fchmodat` only if `mode & 07000` (`reset_mode` is false).
- **Directory xattrs are never applied.** The installer records only their digest (`database.c:2930`).

### E5. ACL: mode, owner, group and xattrs (`apk_extract_v3_acl`, `extract_v3.c:26-50`)

#### E5.1 Mode

- `fi->mode = adb_ro_int(acl, ADBI_ACL_MODE)`, truncated to `mode_t` (32-bit; `apk_io.h:53`). **apk does not mask it.** The file type is then OR'ed in (§E2, §E4).
- Bits outside 07777 corrupt the type. Example: `acl.mode` containing 0x4000 on a regular file gives `0x4000 | 0x8000 = 0xC000 = S_IFSOCK`. `fsys_file_extract` would then return `-APKE_NOT_EXTRACTED` (`fs_fsys.c:145-146`).
- **Recommendation:** mask with 07777, or reject higher bits.
- An absent ACL or absent mode gives mode **0**: the entry is created with no permission bits (subject to umask and `fchmodat` rules).

#### E5.2 Owner and group

- `user` and `group` are **names**. They are resolved through the id cache against `etc/passwd` and `etc/group`, opened relative to the context's **root fd** (`apk_ctx_get_id_cache`, `context.c:171-176`; `io.c:1393-1457`). The root fd is not the `apk extract --destination` directory.
- Resolution order (`apk_id_cache_resolve_uid/gid`, `io.c:1459-1477`):
  1. If the name is found in the file, use its id.
  2. Otherwise, if the name equals `"root"`, use 0, even if the passwd/group file lacks it.
  3. Otherwise use the default **65534**, passed at `extract_v3.c:33-34`.
- An absent ACL or absent user/group resolves the NULL blob. That gives 65534/65534 unless the passwd/group file contains an empty name.

#### E5.3 xattrs

**Reader** (`extract_v3.c:36-47`):
- It splits at the **first** NUL: `apk_blob_split(x, APK_BLOB_BUF(""), &key, &value)`. `APK_BLOB_BUF("")` is the 1-byte blob `"\0"` (`apk_blob.h:27`; `blob.c:61-76`).
- `key` = the bytes before the NUL. Its `.ptr` is used as a C string; the following NUL terminates it. The key may be empty.
- `value` = everything after the NUL. It may be empty and may contain NULs.
- A blob with no NUL, or a non-blob element (including a NULL item), fails the whole entry with `-APKE_ADB_SCHEMA` (`extract_v3.c:41-42, 67-68`).
- An absent array means no xattrs.

**Applying** (`fsys_file_extract`, `fs_fsys.c:152-166`):
- After creation, apk calls `openat(fn, O_RDWR)` and then `fsetxattr` for each xattr. This applies to regular files, hardlinks and device/FIFO nodes.
- **Never applied to symlinks or directories.**
- With `APK_FSEXTRACTF_NO_SYS_XATTRS = 0x0004`, names not starting with `"user."` are skipped (`fs_fsys.c:91-94, 157-158`).
- Failures only produce the warning flag `APK_EXTRACTW_XATTR = 0x0008`.

**xattr digest (database only).** `apk_fileinfo_hash_xattr(fi, APK_DIGEST_SHA1)` (`io.c:742-775`):
- sorts the xattrs by `strcmp` on the name;
- hashes, for each one, `be32(strlen(name)) || name || be32(len(value)) || value`;
- gives NONE when there are no xattrs.

### E6. mtime

- `fi.mtime = adb_ro_int(file, ADBI_FI_MTIME)`, stored into `time_t`. Absent means 0 (`extract_v3.c:59`).
- Applied with `utimensat(..., AT_SYMLINK_NOFOLLOW)`, setting both atime and mtime to that value with nsec 0 (`fs_fsys.c:168-174`).
- **Not applied to symlinks, or to directories.** `dir` objects have no mtime field, and the directory path never calls `utimensat`.
- Writer fact (`SOURCE_DATE_EPOCH` replacement): §D8.1.

### E7. Names and path construction

#### E7.1 Full path

The full path is built with the path builder (`pathbuilder.c:12-28`; `apk_pathbuilder.h:13-26`). The buffer is `char name[PATH_MAX]`.
- `setb(dir)` sets `namelen = 0` and then pushes the directory name.
- `pushb(name)` inserts `'/'` only if `namelen != 0`, then appends the name.
- `pushb` fails with `-ENAMETOOLONG` when `namelen + b.len + 2 >= PATH_MAX`.
  - With 4096 this allows directory names up to 4093 bytes.
  - It allows `dir.len + file.len <= 4093`, i.e. a full path of at most 4094 bytes.
  - Root-level file names can be up to 4093 bytes.

**extract_v3 ignores these failures** (`extract_v3.c:194, 202, 210`):
- **Over-long file name.** The builder still holds the directory path, so the file callback receives the *directory's* path as `fi.name`.
- **Over-long directory name.** `namelen` is 0, but the buffer keeps stale text from the previous entry. No directory callback is emitted. The directory's files are pushed at offset 0, so they are extracted **at the package root** under their bare names.
- **Recommendation:** reject over-long paths.

#### E7.2 Validation before anything is extracted

`apk_extract_v3_validate_filetree` (`extract_v3.c:141-159`) runs once, during the one-time init and before `v3meta` (lines 173-176), so before any metadata callback. Any failure aborts the package with `-APKE_FORMAT_INVALID`.
- Each **directory name** with `len != 0` must pass `apk_fs_is_malicious_pathname`. The root (empty name) is skipped.
- Each **file name** must pass `apk_fs_is_malicious_filename`. An absent name has length 0 and is rejected, so file names are mandatory; any `/` in a file name is rejected.

`apk_fs_is_malicious_filename` (`fs_fsys.c:264-277`) rejects a name when any of these holds:
- it contains a byte `< 0x20`, `0x7f`, or `'/'` (compared as unsigned; bytes `>= 0x80` and space are allowed);
- its length is 0;
- it is exactly `"."` or `".."`;
- its length is exactly 53 (`5 + 48`) and it starts with `".apk."`. These names are reserved for apk's temporary files `<dir>/.apk.<48 lowercase hex chars>`, i.e. the hex of the first 24 bytes of SHA-256(pkgctx ‖ fullname) (`format_tmpname`, `fs_fsys.c:61-82`).

`apk_fs_is_malicious_pathname` (`fs_fsys.c:279-285`):
- splits on `"/"` and applies the filename check to every component that is followed by a `/`;
- so a leading `"/"`, `"//"`, or any `"."`/`".."` component is rejected;
- the final component is checked only if non-empty, so **one** trailing `"/"` is tolerated (`"a/"` passes, `"a//"` does not).

Not checked by the reader: duplicate names, duplicate directory entries, sort order, symlink targets, hardlink targets (checked only in the fs layer, §E2), and over-long paths (§E7.1).

### E8. Extraction algorithm (as apk does it)

**Entry.** `apk_extract` (`extract_v3.c:319-330`) peeks 4 bytes and dispatches to `apk_extract_v3` or `apk_extract_v2` (§B2). `apk_extract_v3` (`extract_v3.c:273-317`) then runs:

```
adb_m_process(&db, adb_decompress(is, 0), ADB_SCHEMA_ANY, trust, ectx,
              apk_extract_v3_data_block)
```

State: `cur_path` and `cur_file` (both 0 initially), the `paths`, `path`, `files` and `file` objects, and the path builder.

```
on_block(b):                                   # invoked for every block, in file order
    if type(b) != DATA: return 0
    if schema != PACKAGE: return -APKE_ADB_SCHEMA
    if ops.v3meta == NULL: return -APKE_FORMAT_NOT_SUPPORTED
    r = next_file()
    if r < 0: return r
    if r > 0: return -APKE_ADB_BLOCK           # data, but nothing pending
    hdr = get 8 bytes (fail -> e.g. -APKE_EOF); sz = len(b) - 8
    if hdr.path_idx != cur_path or hdr.file_idx != cur_file or sz != file.size:
        return -APKE_ADB_BLOCK
    return extract_file(stream = the next sz bytes)

next_file():           # 0 = stopped at a file needing DATA, 1 = done, <0 = error
    if cur_path == 0:                          # one-time init
        cur_path = cur_file = 1
        pkg = root object (schema_package)
        r = validate_filetree(pkg);  if r < 0: return r     # §E7.2
        r = ops.v3meta(pkg);         if r < 0: return r
        paths = pkg.paths
        if ops.file == NULL: return -ECANCELED              # metadata-only consumer
    else:
        cur_file += 1
        if cur_file > len(files): cur_path += 1; cur_file = 1
    while cur_path <= len(paths):
        if cur_file == 1: path = paths[cur_path]; files = path.files
        pb.setb(path.name)
        if pb.namelen != 0 and cur_file == 1:
            r = emit_directory(path); if r != 0: return r   # §E4
        while cur_file <= len(files):
            file = files[cur_file]; n = pb.pushb(file.name)
            if file.size != 0 and file.target.ptr == NULL:
                return 0                       # wait for its DATA block
            r = extract_file(stream = NULL); if r != 0: return r
            pb.pop(n); cur_file += 1
        cur_path += 1; cur_file = 1
    return 1

extract_file(stream):                          # apk_extract_v3_file, extract_v3.c:52-117
    fi.name = pb; fi.size = file.size; fi.mtime = file.mtime
    apply ACL (§E5)                            # bad xattr -> -APKE_ADB_SCHEMA
    fi.digest = digest_from_blob(file.hash)    # §E3
    if file.target.ptr != NULL:
        parse target (§D8.2); fi.mode |= type
        return ops.file(fi, stream)            # stream is always NULL here
    if fi.digest.alg == NONE: return -APKE_ADB_SCHEMA
    fi.mode |= S_IFREG
    if stream == NULL: return ops.file(fi, NULL)
    r = ops.file(fi, verify_wrap(stream, fi.size, fi.digest))
    return close_verify(r)                     # may yield -APKE_FILE_INTEGRITY
```

Callback return values: any non-zero value from a directory or no-data file callback is returned from `next_file` as-is. A positive value would therefore be misread as "done" (1), or turned into `-APKE_ADB_BLOCK` by `on_block`. apk's own consumers convert warnings (positive) to 0 before returning (`database.c:2886-2893`; `app_extract.c:128-132`).

**After all blocks** (`extract_v3.c:291-312`):
- If `adb_m_process` returned 0 and the schema is PACKAGE, `next_file()` is called once more:
  - `0` (a file is still waiting for data) becomes `-APKE_ADB_BLOCK`. **A non-empty regular file without its DATA block fails the whole package.**
  - `1` becomes success.
- If no DATA block appeared, this final call is the one that performs the one-time init and walks the whole tree.
- For schema INDEX, `ops.v3index` is called instead. Any other schema gives `-APKE_ADB_SCHEMA`.
- `-ECANCELED` becomes 0. This is how metadata-only readers such as `apk_pkg_read` (`package.c:644-660`) stop early.
- After that, `r == 0 && db.adb.len == 0` gives `-APKE_ADB_BLOCK` (`extract_v3.c:311-312`).
  - In the stream path the ADB data is kept on `-ECANCELED` (`adb.c:325-332`), so this does not trigger.
  - In the mmap path `__adb_m_parse` clears `db->adb` on any non-zero result, including `-ECANCELED` (`adb.c:194`), and `__adb_m_mmap` frees the db. A metadata-only mmap read would therefore end in `-APKE_ADB_BLOCK`. **[unconfirmed]** Inferred from code; package extraction never uses mmap (§B8).

**Consequences.**
- Callbacks happen strictly in stored tree order.
- `v3meta` always precedes the first directory or file callback.
- Entries without data are emitted while walking toward the next file that needs data.
- Parents precede children (§D7.1).

#### E8.1 What the installer does with the callbacks (`database.c`)

**`v3meta`** (`apk_db_install_v3meta`, `database.c:2758-2797`):
- reads `info.replaces`, `replaces-priority`, the scripts (§D9) and the triggers;
- sets `ipkg->sha256_160 = 1`.
- The pending pre-install or pre-upgrade script, if present, runs at the **first** file or directory callback (`apk_db_run_pending_script`, `database.c:2822`). If there are no entries, it runs after extraction succeeds (`database.c:3194`).

**Files** (`apk_db_install_file`, `database.c:2809-2937`):
- If a non-root file's directory has no dir instance for this package, the result is "no dirent in archive": `broken_files`, and the entry is skipped (`database.c:2832-2839`). This cannot happen with mkpkg output, because every file's own directory has a `paths` entry.
- A hardlink target is checked as in §E2.
- Each file is written with `apk_fs_extract(..., apk_pkg_ctx(pkg))`. It lands at `<dir>/.apk.<hex24>` and is renamed on commit (`fs_fsys.c:61-82, 103-105, 191-198`).
- Hardlinks are made against the target's temporary name (`fs_fsys.c:127-133`).
- Names equal to `uvol` or beginning `uvol/` go to the uvol backend (`apk_fsops_get`, `fs_fsys.c:255-262, 310`). It runs the external `uvol` tool with `create <basename> <size> ro`, then `write` with the content stream, then `up` (`fs_uvol.c:61-85`).

**Per-file steps in fs_fsys** (`fsys_file_extract`, `fs_fsys.c:96-177`):
1. Unless `APK_FSEXTRACTF_NO_OVERWRITE` (0x0002) is set, `unlinkat` the target name; `ENOENT` is ignored.
2. Create the entry:
   - regular file: `openat(O_RDWR|O_CREAT|O_TRUNC|O_CLOEXEC|O_EXCL, mode & 07777)`, then copy `size` bytes;
   - hardlink: `linkat`;
   - symlink: `symlinkat`;
   - device or FIFO: `mknodat`.
3. Unless `APK_FSEXTRACTF_NO_CHOWN` (0x0001) is set: `fchownat(uid, gid, AT_SYMLINK_NOFOLLOW)`, then `fchmodat(mode & 07777)` only if `mode & 07000` (`fs_fsys.c:20-28`).
4. Apply xattrs (not on symlinks).
5. Set mtime (not on symlinks).

Steps 3-5 only produce warning bits: `APK_EXTRACTW_OWNER` 0x1, `PERMISSION` 0x2, `MTIME` 0x4, `XATTR` 0x8 (`apk_extract.h:41-44`).

**Precheck.** `apk_fs_extract` refuses a non-link regular file with digest NONE with `-APKE_FORMAT_OBSOLETE` (`need_checksum`, `fs_fsys.c:287-304`). extract_v3 already enforces this.

#### E8.2 The `apk extract` applet (`app_extract.c`)

- It keeps a hash of names extracted in this run, across all package arguments.
- A trailing `"/"` is stripped from the key.
- It skips an entry with a warning when any of these holds:
  - the name was already extracted as a different kind of entry;
  - the parent (the part before the last `"/"`) was not extracted as a directory;
  - a hardlink target is not an extracted regular file of the same package.
- Skipped entries do not read their stream (see §E3 on verification).
- Flags:
  - `NO_OVERWRITE` is set unless `--force-overwrite`;
  - `NO_CHOWN | NO_SYS_XATTRS` is set when not root;
  - pkgctx is NULL, so no temporary names are used.

### E9. Error summary (extraction)

| condition | result |
|---|---|
| malicious directory name or file name, or absent file name | `-APKE_FORMAT_INVALID` (before `v3meta`, before any extraction) |
| xattr blob without a NUL | `-APKE_ADB_SCHEMA` |
| target shorter than 2, unknown type, device payload not 8 bytes, or link payload ≥ `PATH_MAX-1` (4095) | `-APKE_ADB_SCHEMA` |
| present but zero-length target | `-APKE_ADB_SCHEMA` |
| regular file (no target) whose hash length is not 20/32/64 | `-APKE_ADB_SCHEMA` |
| DATA block with wrong `path_idx`, `file_idx` or size; DATA after all files are done; a pending file without DATA at the end | `-APKE_ADB_BLOCK` |
| DATA payload shorter than 8 bytes | `-APKE_EOF` (from `apk_istream_get`) |
| block type ≥ 4 or not allowed at this position | `-APKE_ADB_BLOCK` |
| missing padding / truncated stream (stream path) | `-APKE_EOF` or `-ECONNABORTED` |
| DATA before trust, or untrusted at the end | `-APKE_SIGNATURE_UNTRUSTED` |
| DATA in a non-package ADB | `-APKE_ADB_SCHEMA` |
| consumer without `v3meta` meets DATA | `-APKE_FORMAT_NOT_SUPPORTED` |
| content hash mismatch | `-APKE_FILE_INTEGRITY` (checked when all `size` bytes passed through the verifier, §E3) |
| malicious hardlink target path | `-APKE_ADB_SCHEMA` (fs layer) |
| hardlink target not in package (installer) | not fatal: entry skipped, `broken_files = 1` |
| device node with `NO_DEVICES` | `-APKE_NOT_EXTRACTED` (installer marks the file broken, not fatal) |

---

## Part F. Versions, dependencies and providers

"Reader" in this part means a Rust reimplementation that parses real files and resolves the same results as apk. There are no test vectors in the source tarball (only `src/` and `doc/`); the worked examples in §F1.6, §F3.4 and §F4.3 were traced by hand through the code.

### F0. Primitive helpers used below

| Helper | Behaviour | Source |
|---|---|---|
| `apk_blob_t` | `struct { long len; char *ptr; }`. A **NULL blob** has `ptr == NULL` (`APK_BLOB_IS_NULL`). An **empty blob** has `len == 0` and a non-NULL `ptr`. The two are different. | `apk_blob.h:15-18, 25-26` |
| `apk_atom_null` | `{0, ""}`. It is empty, **not** NULL. `apk_atomize_dup()` returns `&apk_atom_null` when `len <= 0` or `ptr == NULL`. All runtime package, provide and dependency versions are atoms, so at runtime "no version" is always the empty string `""`, never NULL. The solver tests "unversioned" by pointer identity (`== &apk_atom_null`). Since every empty input maps to that one atom, a reader can use "version is empty" instead. | `atom.c:12, 42-59` (line 48) |
| `apk_blob_sort(a,b)` | `memcmp` over `min(len)`. If equal, returns `a.len - b.len`, so the shorter blob sorts first. | `blob.c:150-155` |
| `apk_blob_compare(a,b)` | If the lengths are equal, `memcmp`. Otherwise the shorter blob is less (-1/1). Used for equality. | `blob.c:141-148` |
| `apk_blob_split(b, sep, l, r)` | Splits at the **first** occurrence of the substring `sep` (`memmem`). Returns 0 if absent. | `blob.c:61-76` |
| `apk_blob_rsplit(b, ch, l, r)` | Splits at the **last** occurrence of the byte `ch`. | `blob.c:45-59` |
| `apk_blob_tokenize` / `apk_blob_foreach_word` | Splits on the exact token (`" "` for `foreach_word`). **Empty pieces are skipped**, so runs of the token collapse. No other whitespace counts. | `blob.c:210-220`; `apk_blob.h:77-78` |
| `apk_blob_pull_uint(b, radix)` | Consumes bytes while `b.len > 0`, the byte is not NUL, and `digitdecode[c] < radix`. For radix 10 only `'0'..'9'` qualify. Accumulates in `uint64_t` with **silent wrap-around**. Returns 0 if no digit was consumed. | `blob.c:418-436` |
| `apk_blob_trim(b)` | Strips **trailing** `isspace` bytes only (space, `\t\n\v\f\r`). Leading whitespace is kept. | `apk_blob.h:37-40` |

Character classes (`ctype.c:13-106`, enum in `apk_ctype.h:11-22`). `apk_blob_spn` and `apk_blob_cspn` use them (`ctype.c:121-157`).

| Class (value) | Members |
|---|---|
| `HEXDIGIT` (0) | `0-9 a-f` (lower case only) |
| `PACKAGE_NAME` (1) | `A-Z a-z 0-9 + - . _` |
| `VERSION_SUFFIX` (2) | `a-z` |
| `DEPENDENCY_NAME` (3) | PACKAGE_NAME plus `, : / [ ]` |
| `DEPENDENCY_COMPARER` (4) | `< = > ~` |
| `VARIABLE_NAME` (5) | `A-Z a-z 0-9 _` |
| `TAG_NAME` (6) | `+ , - . : = / [ ] A-Z a-z 0-9 _` |
| (7) | unused |
| `DEPENDENCY_SEPARATOR` (8) | space and `\n` only (not TAB, not CR) |
| `REPOSITORY_SEPARATOR` (9) | space and `\t` only |

Classes 0-6 use a table of size 127 (`'~'+1`). Classes 8-9 use a table of size 33 (`' '+1`). A byte beyond the table (any byte ≥ 0x7F for classes 0-6, any byte > 0x20 for classes 8-9) belongs to no class.

### F1. Version strings (`version.c`)

#### F1.1 Representation and grammar

- A version is a plain byte string in a blob (`scalar_version`), with no binary encoding. Versions are validated only on write (`apk_adb.c:195-199`); readers never validate them.
- The source comment (`version.c:25`) gives `digit{.digit}...{letter}{_suf{#}}...{~hash}{-r#}`. In grammar form:

```
version  := digits ( '.' digits )* [ letter ] ( '_' suffix [digits] )* [ '~' hex+ ] [ '-r' digits ]
digits   := [0-9]+                 (non-empty; parsed as u64 with wrapping overflow, stops at NUL: blob.c:418-436)
letter   := [a-z]                  (exactly one, only right after a numeric component)
suffix   := alpha | beta | pre | rc | cvs | svn | git | hg | p   (the full [a-z]* run must equal one of these)
hex      := [0-9a-f]               (lowercase only, APK_CTYPE_HEXDIGIT)
```

The tokenizer in §F1.3 defines the precise grammar.

#### F1.2 Token kinds and suffixes

Token kinds (`version.c:27-37`). The numeric order matters: it drives both validity checks and comparison.

| Token | Value |
|---|---|
| `TOKEN_INITIAL_DIGIT` | 0 |
| `TOKEN_DIGIT` | 1 |
| `TOKEN_LETTER` | 2 |
| `TOKEN_SUFFIX` | 3 |
| `TOKEN_SUFFIX_NO` | 4 |
| `TOKEN_COMMIT_HASH` | 5 |
| `TOKEN_REVISION_NO` | 6 |
| `TOKEN_END` | 7 |
| `TOKEN_INVALID` | 8 |

Suffix enum (`version.c:39-56`). `SUFFIX_ENUM_START = -1`, so INVALID is 0.

| Suffix | Value |
|---|---|
| (INVALID) | 0 |
| `alpha` | 1 |
| `beta` | 2 |
| `pre` | 3 |
| `rc` | 4 |
| (NONE, sentinel only) | 5 |
| `cvs` | 6 |
| `svn` | 7 |
| `git` | 8 |
| `hg` | 9 |
| `p` | 10 |

Suffixes below NONE (5) are pre-release suffixes.

`suffix_value(s)` (`version.c:65-98`) picks a candidate from the first byte, then requires an **exact** match (`apk_blob_compare`) with that literal:
- `a`→alpha, `b`→beta, `c`→cvs, `g`→git, `h`→hg, `r`→rc, `s`→svn.
- `p`→`pre` if `len > 1`, otherwise `p`.
- An empty string or any other first byte gives INVALID.

So `_pa`, `_r`, `_Rc` and `_` (empty) are invalid; `_p` and `_pre` are valid.

#### F1.3 Tokenizer

`token_first` (`version.c:147-151`) sets the token to `INITIAL_DIGIT` and calls `token_parse_digits` (`version.c:139-145`), which runs `pull_uint(10)`. `value` is the consumed digit string. An empty `value` gives INVALID, so **a version must start with an ASCII digit**.

`token_next(t, b)` (`version.c:153-213`) branches on the next byte. `tok` is the kind of the token just produced.

| Next input | Allowed only if | Result |
|---|---|---|
| end of input (`len == 0`) | always | `END` |
| `a`-`z` | `tok <= DIGIT` | `LETTER`; `value` = that byte; consume 1 |
| `.` | `tok <= DIGIT` | consume `.`, then fall into the digit case. An empty digit run gives `INVALID`, so `1.`, `1..2` and `1.a` are invalid. |
| `0`-`9` | `tok ∈ {INITIAL_DIGIT, DIGIT}` → `DIGIT`; `tok == SUFFIX` → `SUFFIX_NO`; anything else → `INVALID` | `token_parse_digits` |
| `_` | `tok <= SUFFIX_NO` | consume `_`; `value` = longest `[a-z]*`; `suffix = suffix_value(value)`; INVALID suffix gives `INVALID`, otherwise `SUFFIX` |
| `~` | `tok < COMMIT_HASH` | consume `~`; `value` = longest `[0-9a-f]*`; empty gives `INVALID`, otherwise `COMMIT_HASH` |
| `-` | `tok < REVISION_NO` | must start with the literal `-r`, otherwise `INVALID`; consume 2 bytes; `REVISION_NO`; `token_parse_digits` (at least one digit, otherwise `INVALID`) |
| any other byte (upper case, `+`, NUL, ≥0x80, …) | none | `INVALID` |

Some branches consume input before failing (`.`, `_`, `~`, `-r`). This does not matter: no caller continues past an `INVALID` token, because every loop condition is `tok < END`.

Consequences:
- **Letter:** at most one, directly after the numeric components (`1a` and `1.2a` are valid). `1.2ab`, `1.2a.3` and `1.2a3` are invalid.
- **Suffixes:** any number, each optionally followed by a number; they may chain, as in `_alpha_beta2`. Allowed after digits, a letter, a suffix or a suffix number. `1.0_rc` and `1.0_alpha1_p2` are valid. A `.` or a digit run after a suffix number is invalid.
- **`~hash`:** at most one, before `-r`. Lower-case hex only, at least one character, unlimited length.
- **`-rN`:** at most one and must be last, with at least one digit. `1.0-r`, `1.0-x1` and `1.0-r1a` are invalid.

`apk_version_validate(v)` (`version.c:273-279`) loops while `tok < END` and returns `tok == END`. The empty string is invalid. There is no length limit.

#### F1.4 Token comparison: `token_cmp` (`version.c:100-137`)

It is called only when both tokens have the same kind and that kind is < END.

| Token kind | Comparison |
|---|---|
| DIGIT (after `.`) | if *either* `value[0] == '0'`, string comparison (`apk_blob_sort` over the digit strings, without the `.`); otherwise numeric over `u64` |
| INITIAL_DIGIT, SUFFIX_NO, REVISION_NO | always numeric (u64); leading zeros are irrelevant (`01` == `1`) |
| LETTER | byte value |
| SUFFIX | suffix enum value: alpha=1 < beta=2 < pre=3 < rc=4 < (none=5) < cvs=6 < svn=7 < git=8 < hg=9 < p=10 |
| COMMIT_HASH (the default case) | `apk_blob_sort` over the hex strings |

The result is `APK_VERSION_LESS` (2), `EQUAL` (1) or `GREATER` (4).

#### F1.5 Full comparison: `apk_version_compare_fuzzy(a, b, fuzzy)` (`version.c:281-316`)

`apk_version_compare(a, b)` is the same with `fuzzy = false` (`version.c:318-321`).

```text
if a is NULL-blob or b is NULL-blob:
    return EQUAL if both NULL else (EQUAL|LESS|GREATER)   // = 7
first(ta,a); first(tb,b)
while ta.tok == tb.tok && ta.tok < END:
    r = token_cmp(ta,tb); if r != EQUAL: return r
    next(ta,a); next(tb,b)
if ta.tok == tb.tok:                       return EQUAL    // both END, or both INVALID
if tb.tok == END && fuzzy:                 return EQUAL    // b is a token-prefix of a (a may even be INVALID here)
if ta.tok == SUFFIX && ta.suffix < NONE:   return LESS     // a continues with a pre-release suffix
if tb.tok == SUFFIX && tb.suffix < NONE:   return GREATER
if ta.tok > tb.tok:                        return LESS     // larger token kind = smaller version
if tb.tok > ta.tok:                        return GREATER
return EQUAL
```

So the side with the higher token kind at the first difference is LESS: for example `1.0` > `1`, `1_rc1` < `1`, and an INVALID tail is LESS.

Reader notes:
- The NULL branch (result 7) is reachable only where raw ADB blobs are compared, for example `version_compare` in ADB array sorting (`apk_adb.c:201-208`), which maps 7 to 0. Runtime dependency matching always sees atoms (§F0). Model absent as `None` and present-but-empty as `Some(b"")`. `Some(b"")` tokenizes as INVALID immediately (§F4.4).
- Trailing invalid parts compare equal to each other: `1.2X` vs `1.2Y` gives EQUAL. An invalid part is less than END: `1.2X` < `1.2`. Two strings that are invalid from the start (`""`, `"abc"`) compare EQUAL.
- Numeric runs longer than 19-20 digits wrap modulo 2^64. Runs with a leading `0` in DIGIT position use string comparison and do not wrap.

#### F1.6 Worked results (traced by hand through the code above)

| a | b | result |
|---|---|---|
| `1.2` | `1.2.3` | LESS (END 7 > DIGIT 1) |
| `1.2.3_rc1` | `1.2.3` | LESS (pre-release rule) |
| `1.2.3_alpha` < `_beta` < `_pre` < `_rc` | | increasing |
| `1.2.3` | `1.2.3_p1` | LESS (`_p` is not pre-release; END 7 > SUFFIX 3) |
| `1.2.3_p1` | `1.2.3-r1` | GREATER (SUFFIX 3 < REVISION_NO 6) |
| `1.2.3` | `1.2.3-r0` | LESS |
| `1.0_alpha` | `1.0_alpha1` | LESS; `1.0_alpha` < `1.0_alpha0` too |
| `1.0_rc1` | `1.0_rc` | GREATER |
| `1.2` | `1.2a` | LESS |
| `1.2a` | `1.2.1` | LESS (LETTER 2 > DIGIT 1) |
| `1.0_p1` | `1.0a` | LESS (SUFFIX 3 > LETTER 2) |
| `1.10` | `1.9` | GREATER (numeric) |
| `1.02` | `1.1` | LESS (string `"02"` < `"1"`) |
| `1.010` | `1.09` | LESS (string) |
| `1.0` | `1.00` | LESS (string, `"0"` is shorter) |
| `01` | `1` | EQUAL (initial digit is numeric) |
| `1.0~abc` | `1.0` | GREATER; `1.0~abc` > `1.0-r1` too (COMMIT_HASH 5 < REVISION_NO 6) |
| `1.0~abc` | `1.0~abd` | LESS (string) |

#### F1.7 Fuzzy (`~`) matching

With `fuzzy = true`, b matches when b's tokens are a **token-wise** prefix of a's tokens (rule "fuzzy mode and `b` is at END: EQUAL"). The digit token `2` does not prefix `20`.
- `~1.2` matches `1.2`, `1.2.3`, `1.2a`, `1.2_rc1`, `1.2-r5` and `1.2~ab`.
- `~1.2` also matches `1.2X` (a invalid after the prefix).
- `~1.2` does not match `1.20`, `1.3` or `1.1`.

This agrees with `doc/apk-world.5.scd:70-72`.

### F2. Dependency operators

#### F2.1 Match bits (`apk_version.h:13-22`)

| Name | Value |
|---|---|
| `APK_VERSION_UNKNOWN` | 0 |
| `APK_VERSION_EQUAL` | 0x01 |
| `APK_VERSION_LESS` | 0x02 |
| `APK_VERSION_GREATER` | 0x04 |
| `APK_VERSION_FUZZY` | 0x08 |
| `APK_VERSION_CONFLICT` | 0x10 |
| `APK_DEPMASK_ANY` (EQUAL, LESS and GREATER) | 0x07 |
| `APK_DEPMASK_CHECKSUM` (LESS and GREATER) | 0x06 |

apk keeps the op in a `uint8_t` (`apk_package.h:47`).

#### F2.2 Text operator to mask: `apk_version_result_mask_blob` (`version.c:244-266`)

The result is the OR over the bytes, in any order, duplicates allowed: `<`→2, `>`→4, `=`→1, `~`→9 (FUZZY|EQUAL). Any other byte makes the whole result 0.

So `<=` = `=<` = 3. `~`, `=~` and `~=` are all 9. `<>` = `><` = 6 (checksum). `<=>` = 7 (ANY).

#### F2.3 Mask to text: `apk_version_op_string` (`version.c:215-242`)

The CONFLICT bit is masked off first. Then:

| Mask | Text |
|---|---|
| 2 | `<` |
| 3 | `<=` |
| 11 | `<~` |
| 9 or 8 | `~` |
| 1 | `=` |
| 5 | `>=` |
| 13 | `>~` |
| 4 | `>` |
| 6 | `><` |
| 7 | `""` (empty) |
| anything else | `?` |

#### F2.4 Matching: `apk_version_match(a, op, b)` (`version.c:323-330`)

`a` is the candidate's (package's or provider's) version; `b` is the dependency's version.

```text
ok = (op & 7) == 7  ||  (compare_fuzzy(a, b, (op & 8) != 0) & op) != 0
if op & 16: ok = !ok
```

- `CONFLICT` inverts the result.
- A mask of 8 alone can never match, because the compare result is 1, 2, 4 or 7.
- A mask of 16 alone always ends up satisfied (not ok, then inverted).
- Neither of those two masks is produced by the parsers.
- When `dep->op` is exactly 6, `apk_dep_match_checksum` is used instead (`package.c:263-284`; §F3.8).

### F3. Dependency syntax and ADB encoding

#### F3.1 Textual syntax: `apk_dep_parse` (`package.c:143-168`)

The form is `[!]name[op version]`:
1. A NULL spec fails.
2. One optional leading `!` sets CONFLICT (0x10). A second `!` becomes part of the name.
3. `name` is everything up to the first DEPENDENCY_COMPARER byte (`<`, `=`, `>`, `~`; `apk_blob_cspn` with the COMPARER class; `ctype.c:27-29, 96`).
4. If a comparer is found, `op |= mask(maximal run of [<=>~])` (§F2.2) and `version` is the rest of the string.
   - The version **must be non-empty**: `apk_blob_spn` returns 0 when the whole remainder is comparer bytes, and that makes the parse fail. So `name=` and `name>=` are rejected (`package.c:153`).
   - The "op without CONFLICT is 0 → fail" check (line 155) can never fire, because the run contains only comparer bytes and is non-empty.
5. If there is no comparer at all: `op |= 7` (`APK_DEPMASK_ANY`) and `version = NULL`.
6. On failure: name and version become NULL, `op = 7`, and the return value is `-APKE_DEPENDENCY_FORMAT`.

#### F3.2 The two users of the parser

- **ADB text input**, `dependency_fromstring` (`apk_adb.c:352-373`). This is used by mkpkg `--info`, mkndx (v2 to v3 conversion), convndx/convdb and adbgen.
  - The version must pass `apk_version_validate`, unless `(op & 0x06) == 0x06` (`apk_adb.c:358-359`). That exempts ANY (7), checksum (6), and their conflict forms 22 and 23 (so `!name`, 0x17, skips validation).
  - The name must consist only of DEPENDENCY_NAME bytes: the PACKAGE_NAME class plus `,`, `:`, `/`, `[`, `]` (`ctype.c:14-16, 22-30, 67-68`; check at `apk_adb.c:361`).
    - There is **no** alphanumeric-first-character rule.
    - `@` (the v2 repository-tag syntax) is **not** in the class, so `name@tag` is rejected.
    - An **empty name passes** the class check. For example `=1.0` is accepted, with slot 1 absent.
  - Lists are split by `apk_dep_split` (`apk_adb.c:12-20`; `dependencies_fromstring` 388-402): it skips runs of DEPENDENCY_SEPARATOR bytes (space or `\n` only; `ctype.c:99-106`), then takes the run up to the next separator.
- **v2 text and world**, `apk_blob_pull_dep` (`package.c:193-227`).
  - The token is cut at the first space or `\n` (`cspn` DEPENDENCY_SEPARATOR), then any following run of separators is skipped.
  - An invalid version does not fail: the dependency is marked `broken`. Validation is skipped when `(op & 6) == 6`.
  - The text before the operator is split at the first `@`.
    - With `allow_tag`, `btag` is the text after `@` **without** the `@`, looked up with `apk_db_get_tag_id`.
    - Without `allow_tag`, a name containing `@` fails.
  - Names are **not** character-validated. `apk_db_get_name` accepts any bytes, including the empty string (`database.c:167-189`).

#### F3.3 ADB dependency object: `schema_dependency`

Defined at `apk_adb.h:10-13` and `apk_adb.c:375-386`. Kind OBJECT, `num_fields` = `ADBI_DEP_MAX` = 0x04 (slots 0..3, where slot 0 is the entry count), `num_compare = ADBI_DEP_NAME` (1). It has a `tostring` and a `fromstring`, so text dumps show it as a single string such as `"name>=1.2"`.

| Slot | Constant | Name | Schema | Meaning |
|---|---|---|---|---|
| 1 | `ADBI_DEP_NAME` = 0x01 | `"name"` | `scalar_string` (blob) | dependency or provided name, raw bytes |
| 2 | `ADBI_DEP_VERSION` = 0x02 | `"version"` | `scalar_version` (blob) | version string, or checksum text for `><` |
| 3 | `ADBI_DEP_MATCH` = 0x03 | `"match"` | `scalar_int` (numeric) | op bit mask from §F2.1 |

**Namespaced names such as `so:libc.so.6`, `pc:foo` and `cmd:bar` have no special encoding.** The prefix and colon are ordinary bytes of the `name` blob, and there are no `so:`, `pc:` or `cmd:` literals anywhere in `src/`.

This schema is used for the pkginfo fields `depends` 0x0F, `provides` 0x10, `replaces` 0x11, `install-if` 0x12 and `recommends` 0x13 (`apk_adb.c:431-435`; `apk_adb.h:30-34`).

#### F3.4 Writer encoding (`dependency_fromstring`)

- `name` is always written to slot 1; it becomes NULL (absent) if empty (§B7.5).
- If `op == 0x07` exactly, neither `version` nor `match` is written. This includes `foo<=>1`: the version text is **dropped** and the dependency becomes plain `foo`.
- Otherwise `version` is written to slot 2. `adb_w_blob` of a NULL or empty blob returns `ADB_VAL_NULL` (`adb.c:748-764`, in `adb_w_blob_vec`), and `adb_wo_val` stores NULL without growing the object (`adb.c:990-996`), so a null version leaves the field absent.
- `match = op` is written to slot 3 only if `op != 0x01` (and `op != 0x07`). `adb_w_int` stores values below `0x10000000` inline as `ADB_TYPE_INT`; larger values use `INT_32` or `INT_64` (`adb.c:780-791`).
- Therefore a plain `name` stores **only** slot 1, and `name=ver` stores slots 1 and 2 with `match` absent.

Worked encodings (derived from the rules above):

| Text | Mask | name (slot 1) | version (slot 2) | match (slot 3) |
|---|---|---|---|---|
| `foo` | 7 | `foo` | absent | absent |
| `so:libc.so.6` | 7 | `so:libc.so.6` | absent | absent |
| `foo=1.2` | 1 | `foo` | `1.2` | absent (EQUAL implied) |
| `foo<1.2` | 2 | `foo` | `1.2` | 0x02 |
| `foo<=1.2` | 3 | `foo` | `1.2` | 0x03 |
| `foo>1.2` | 4 | `foo` | `1.2` | 0x04 |
| `foo>=1.2` | 5 | `foo` | `1.2` | 0x05 |
| `foo><X` or `foo<>X` (e.g. `foo><Q1…`) | 6 | `foo` | `X`, checksum text (not version-validated) | 0x06 |
| `foo~1.2` (also `=~`, `~=`) | 9 | `foo` | `1.2` | 0x09 |
| `foo<~1.2` | 11 | `foo` | `1.2` | 0x0B |
| `foo>~1.2` | 13 | `foo` | `1.2` | 0x0D |
| `!foo` | 23 | `foo` | absent (NULL) | 0x17 |
| `!foo=1.2` | 17 | `foo` | `1.2` | 0x11 |
| `!foo<1.2` | 18 | `foo` | `1.2` | 0x12 |
| `!foo>=1.2` | 21 | `foo` | `1.2` | 0x15 |
| `foo<=>1` / `foo<>=1` | 7 | `foo` | absent (dropped) | absent |
| `!foo<>=1` | 23 | `foo` | `1` | 0x17 |
| `=1.0` | 1 | absent | `1.0` | absent |
| `foo=` / `foo>=` | – | parse error | | |

- For `!foo`, slot 2 is NULL but slot 3 is set, so `num` = 4.
- The other `!` forms are 16 plus the base mask.

#### F3.5 Reader decoding: `apk_dep_from_adb` (`package.c:356-369`)

This is the rule to implement:

```text
op  = adb_ro_int(match)      // 0 if absent, or the slot is not INT/INT_32/INT_64 (adb.c:427-445); value is truncated to C int
ver = adb_ro_blob(version)   // NULL if absent or not a BLOB_8/16/32 (adb.c:447-471)
if ver is NULL: op |= 7          // APK_DEPMASK_ANY; keeps CONFLICT: match=0x17 + no version → 23
elif op == 0:   op = 1           // APK_VERSION_EQUAL
dep.name    = get_name(blob(name))   // absent name → apk creates the empty name ""
dep.version = atomize(ver)           // NULL or empty → "" (apk_atom_null)
dep.op      = (uint8_t)op            // apk_package.h:47; only the low 8 bits survive
```

Edge cases:
- If `match` is present but `version` is absent, the dependency becomes ANY (plus CONFLICT if set). So `match=2` with no version means "any version".
- A present but zero-length version blob is not NULL, so `op` defaults to EQUAL, while the version atom is `""`. The writer never produces this.
- `match ≥ 256` is truncated. For example, 256 passes the `op == 0` test and then becomes 0, which can never match and is not a conflict.
- Slots ≥ 4 in a dependency object are never read. No version validation happens on read.

#### F3.6 Text rendering: `dependency_tostring` (`apk_adb.c:328-350`)

- `op = match ?: EQUAL`.
- A null name gives no string.
- A null version gives `!name` when the CONFLICT bit is set, otherwise `name`.
- Otherwise the output is `[!]name` + `apk_version_op_string(op)` (§F2.3) + `version`.

#### F3.7 Dependency arrays: `schema_dependency_array` (`apk_adb.c:404-410`)

- Kind ARRAY, item `schema_dependency`. `num_fields = 32` is only the **initial writer capacity**; `adb_wa_append` doubles it as needed (`adb.c:886-900, 1036-1054`), so there is no 32-entry limit. On read, any object or array may have up to `ADBI_MAX = 0x1000000` slots (`adb.c:486`; `adb.h:45`). `adb_ra_num = num - 1`, and elements are at indices 1..num-1 (`adb.h:220`).
- **Writer sort.** `pre_commit = adb_wa_sort_unique` (`adb.c:1134-1147`):
  - It sorts with `ADB_OBJCMP_EXACT` (`adb.c:1105-1132, 551-572`): field 1 by `apk_blob_sort` (bytewise), field 2 by `apk_version_compare` (LESS→-1, GREATER→1, anything else 0; `apk_adb.c:201-208`), field 3 numerically.
  - After sorting, an item whose 32-bit `adb_val_t` equals its predecessor's is dropped. Because the writer deduplicates identical data (`adb.c:671-704`), identical dependency objects usually share one value and are removed.
  - The sort is not a strict order: a NULL version compares equal to any version (§F1.5).
- **A reader must not rely on order or uniqueness.**
- An absent array field reads as an empty array (`adb_r_obj` error path gives `num = 1`; `adb.c:498-505`).
- Text form: tokens separated by DEPENDENCY_SEPARATOR bytes, only `' '` and `'\n'` (§F3.2).
- Loading: `apk_pkgtmpl_from_adb` loads depends, provides, install_if, recommends and tags (`package.c:583-587`). `replaces` is loaded only from the v3 package file at install time, into `ipkg->replaces` (`database.c:2777`).

#### F3.8 Checksum dependencies (`><`, mask exactly 6)

These are created by `apk add ./file.apk` (`app_add.c:149-160`) via `apk_dep_from_pkg` (`package.c:247-261`).
- The version is `"Q1"` plus the base64 of the **first 20 bytes** of the package identity (`apk_pkg_hash_blob`, `apk_package.h:104-106`; `apk_blob_push_hash`, `blob.c:310-321`).
- That is 2 + 28 = 30 characters. The base64 part ends in one `=`.

Matching uses `apk_dep_match_checksum` (`package.c:263-270`), which parses the text with `apk_blob_pull_digest` (`blob.c:567-615`):
- First byte: `X` (hex) or `Q` (base64). Second byte: `1` (SHA1, 20 bytes) or `2` (SHA256, 32 bytes). Anything else fails, leaving digest length 0.
- After decoding 20 bytes for `X1`/`Q1`, if **exactly** 24 (hex) or 16 (base64) bytes remain and the first of them is a valid digit, the digest becomes SHA256 and 12 more bytes are decoded.
- The decoded digest is compared with `apk_blob_compare` against the package's first 20 digest bytes. Lengths must match, so **only 20-byte checksums can ever match**.

The checksum path is taken only when `op == 6` exactly (`package.c:276, 283`).
- `!foo><Q1…` (22) uses ordinary version matching instead.
- `"Q1…"` tokenizes as INVALID, so `compare(provider_version, "Q1…")` gives GREATER for any version that starts with a digit. Then `4 & 22` is non-zero, and inverting makes the dependency fail. So `!foo><…` conflicts with every normally versioned provider of `foo`. This is a quirk of the code, not a documented feature.

### F4. Providers and how `provides` matches

#### F4.1 Registration: `apk_db_pkg_add` (`database.c:579-631`)

- A package is rejected (NULL return) if `name` or `version` is NULL, or the identity is shorter than 20 bytes (line 584).
  - From ADB, name and version are never NULL. An absent `name` gives the empty name `""` and an absent `version` gives `""` (§D4).
  - `load_v3index` skips packages whose `hashes` (0x03) length is not 20, 32 or 64, and warns "packages without hash" (`database.c:1496-1500, 1510`; `package.c:565-566`; `crypto.c:39-58`).
- **Arch filter:** if `pkg.arch` is not `""`, not `noarch` and not in `db.arches`, the package is marked `uninstallable` (`database.c:558-564, 585`).
- Packages are keyed by the **first 20 bytes** of the identity (line 587; §D10.4).
  - **First copy wins.** A later copy with the same key only ORs in its repository bits (line 609). It can also fill `filename_ndx`, or update `size` if the package had no repositories before (lines 610-614). Its other metadata is ignored.
- On first insertion (lines 601-604):
  - `name(pkg).providers += (pkg, pkg.version)`.
  - For each `d` in `pkg.provides`: `d.name.providers += (pkg, d.version)` (macros at `apk_package.h:115-116`).
- **The `op` of a provides entry is never consulted.** Only its name and version are used. A grep for `->op` and `.op` shows no use on provides; `query.c:668` also uses only the provides version.
- An unversioned provide has `version = ""` (`apk_atom_null`).
- Names are opaque byte strings with no special handling of `so:`, `pc:` or `cmd:`.
  - Names are stored NUL-terminated (`pn->name[name.len] = 0`) and later used as C strings, so an embedded NUL would truncate the name in output and in `apk_name_cmp_display`. **[unconfirmed]** Behaviour with such names was not traced further.
- `provider_priority` is stored as `unsigned short` (`apk_package.h:91`). From ADB it is `adb_ro_int(0x0E)`, truncated to 16 bits (`package.c:576`).

#### F4.2 Is a dependency satisfied by a provider? `apk_dep_is_provided` (`package.c:272-278`)

```text
if p is NULL or p.pkg is NULL:            return dep is conflict   // name resolved to nothing
if dep is conflict && deppkg == p.pkg:    return 1                 // a package never conflicts with its own provides
if dep.op == 6:                           return checksum_match(dep, p.pkg)
return apk_version_match(p.version, dep.op, dep.version)
```

- `apk_dep_is_materialized` (`package.c:280-285`) is the own-name variant. It has no self-conflict exemption.
- `apk_dep_analyze` (`package.c:287-301`) checks the package's own name first, then the **first** provides entry with a matching name.

#### F4.3 Examples

- `provides: so:libfoo.so.1=0` registers `(pkg, "0")` under the name `so:libfoo.so.1`.
  - `depends: so:libfoo.so.1` (op 7) matches.
  - `so:libfoo.so.1>=0` compares `"0"` with `"0"`, gives EQUAL, and matches.
- `provides: pc:foo=1.2` registers `(pkg, "1.2")`.
  - `pc:foo>=1.0` matches (GREATER).
  - `pc:foo~1` matches (fuzzy prefix).
  - `pc:foo<1` does not match (GREATER).

#### F4.4 Quirk: an unversioned provider against a versioned dependency

The provider version is `""`, not NULL, so the NULL path in §F1.5 is not taken.
- `""` is INVALID (8). Against a dependency version starting with a digit (INITIAL_DIGIT, 0) the result is **LESS** (`version.c:313`).
- So an unversioned provide of `foo` satisfies `foo`, `foo<X`, `foo<=X` and `foo<~X`. It does not satisfy `foo=X`, `foo>X`, `foo>=X` or `foo~X`.

This is what the code does; it looks unintended. **[unconfirmed]** Traced from code, not run.

#### F4.5 Solver semantics of versioned and unversioned provides

These match `doc/apk-package.5.scd:188-215`.
- **Auto-selectable** (`is_provider_auto_selectable`, `solver.c:376-383`): the provider version is not `""`, or `provider_priority != 0`, or the package's own name has requirers. `select_package` applies this test to every candidate (`solver.c:762-765`).
- **`assign_name`** (`solver.c:691-736`):
  - If the name is already locked to another provider, it is an error ("conflict: same name provided"). The exceptions are when both the new and the locked provider versions are `""`, or conflicts are ignored.
  - On assignment, every other provider of the name is disqualified, except when both the chosen and the other provider are unversioned (lines 721-730).
- **Choosing among candidates:** `compare_providers` (`solver.c:537-689`), without `--latest`. In order, prefer:
  1. selectable;
  2. in the last dependency-merge group;
  3. fewer conflicts;
  4. installed, only during preupgrade or with the INSTALLED flag;
  5. `tag_ok`;
  6. available, only with the AVAILABLE flag;
  7. `tag_preferred`;
  8. installed, unless REMOVE/UPGRADE and only if the same name or either provider is versioned;
  9. the **higher provider version for the requested name**;
  10. if both packages have the same principal name, the higher package version;
  11. higher `provider_priority`;
  12. installed, unless REMOVE;
  13. selectable;
  14. lowest `ffs(repos)` (the lowest repository index).

### F5. `install_if`

- **Storage:** pkginfo 0x12 `install-if`, a dependency array (`apk_adb.c:434`).
  - The v2 field is `i`. If `apk_blob_pull_deps` returns non-zero, which includes merely a broken version, the whole list is cleared (`package.c:502-508`).
- **Reverse index:** for each install_if dependency name N, the package's own name **and every provides name** are added to `N.rinstall_if` (`database.c:523-527, 1736-1747`).
- **Trigger** (`reconsider_name`, `solver.c:419-444`). Evaluated for a selectable package P that is neither triggered nor failed; it walks P's install_if dependencies in order:
  - If `dep.name` is not locked, P stays undecided (stop). If the dependency is a conflict (`!x`), x is flagged `iif_needed` and queued.
  - Else, if `!apk_dep_is_provided(P, dep, &dep.name.chosen)`, P **fails** (`iif_failed = 1`; stop).
  - If every dependency passes, P is triggered (`iif_triggered = 1`).
  - Matching uses the **chosen provider**, so a version constraint is checked against the provided version when the name is satisfied via `provides`.
  - A name resolved to nothing satisfies only `!name` (§F4.2).
- **Selection:** a name with no requirers is considered only through triggered providers that are `tag_ok` and selectable (`solver.c:746-761`). After such an auto-selection, still-unlocked install_if dependencies are applied as constraints (`solver.c:716-719`).
- **Initial state** (`solver.c:224-227`): `iif_failed = 1` when install_if is empty, or when the AVAILABLE solver flag is set and the package is not available.
- **Changesets:** `cset_check_install_by_iif` and `cset_check_removal_by_iif` (`solver.c:855-883`).
- **Documented convention** (`doc/apk-package.5.scd:223-227`): at least two dependencies, one using `=`. Nothing in `src/` enforces it.

---

## Part G. Repositories, package URLs, cache and world

### G1. Repositories

#### G1.1 Sources and processing order (`apk_db_open`, `database.c:2160-2183`)

One shared `apk_repoparser` is used throughout, so `set` variables carry over between sources. Repository indices are assigned in the order repositories are added (0..31).

1. Each `--repository X` (`apk.c:189-190`), parsed with `allow_keywords = false`, skipped with `APK_OPENF_NO_CMDLINE_REPOS`.
   - The string is **still split into words**. A leading `@tag` word is still recognised, and trailing words are still components.
   - What is disabled: keywords (`set`, `ndx`, `v2`, `v3`) and the `is_url` check, so relative paths are allowed.
   - Errors are ignored (the return value is discarded).
2. Each `--repository-config` string (`apk.c:192-193`), split on `\n` with `apk_blob_foreach_token` (empty lines skipped), keywords allowed.
   - Each line is independent: an error on one line does not stop later lines.
3. Unless `APK_OPENF_NO_SYS_REPOS`:
   - Without `--repositories-file`: `<root>/etc/apk/repositories` (opened via root_fd), then `*.list` files from `<root>/etc/apk/repositories.d` and `<root>/lib/apk/repositories.d` (`io.c:953-1004`).
     - All names go into one list sorted by `strcmp`. A name seen in an earlier directory hides the same name in a later one (same merge as the key directories, §C4.1).
     - Names starting with `.` or not ending in `.list` (last `.` onward) are skipped (`database.c:1432-1437`; `io.c:865-868, 897`).
   - With `--repositories-file F`: only F, opened relative to the **working directory** (`AT_FDCWD`).

Per-file behaviour:
- Lines are split on `\n` only, and **CR is not stripped**. A final line without a newline is still processed; an empty piece after a trailing `\n` is passed and ignored (`io.c:200-229`).
- The first line returning a negative value aborts the **rest of that file** (`apk_db_parse_istream`, `database.c:530-544`).
- For root_fd and `.d` files, all errors, including a missing file, are then ignored silently (`database.c:1697`).
- For `--repositories-file`, an error prints "failed to read repositories: …". `apk_db_open` **ignores the return value** (`database.c:2180`), so this is not fatal.

#### G1.2 Line grammar: `apk_repoparser_parse` (`repoparser.c:150-224`)

- A line that is empty or whose **first byte** is `#` is ignored. A NULL line is also ignored.
- The first, second and third words are taken with `get_word` (`repoparser.c:72-77`): the run up to the next space or TAB, then any run of spaces or TABs is skipped. An empty word gives `-APKE_REPO_SYNTAX`, so a line with leading whitespace, or only whitespace, is a syntax error.
- There are no inline comments.
- A CR is an ordinary byte. In a CRLF file, `url\r` keeps the CR in the URL, and a blank `\r` line becomes an unknown keyword, which aborts the rest of the file.

```text
line    := "set" setargs
         | [type] ["@"tag] url [components]       (order fixed: type, then tag, then url)
type    := "ndx" | "v2" | "v3"                    (only if allow_keywords)
setargs := {"-default"} KEY "=" VALUE
```

- **Keyword detection** (`is_keyword`, `repoparser.c:143-148`), applied to the **raw** first word: it is a keyword candidate unless it starts with `@` or `$`, or `is_url` holds.
  - `is_url` (`repoparser.c:138-141`): the first byte is `/`, or `://` occurs at offset > 0.
  - `set` gives a variable line. Any other non-type word gives `-APKE_REPO_KEYWORD`; this includes a relative path in a file.
  - After a type keyword, a missing next word gives `-APKE_REPO_SYNTAX`.
- **Tag:** a word starting with `@` in the tag position. It is not variable-substituted and not character-validated.
  - Stored with the `@` included (`apk_db_get_tag_id`, `database.c:2382-2416`). The plain name without `@` is kept alongside for world lookups.
  - Slot 0 is the untagged default (`APK_DEFAULT_REPOSITORY_TAG = 0`, `apk_database.h:165`). At most `APK_MAX_TAGS = 16` slots exist (`apk_defines.h:147`).
  - With slots exhausted the function returns -1, and `BIT(-1)` is then used in `add_repository_component`. That is undefined behaviour; **[unconfirmed]** not traced further.
- **URL:** the raw word is substituted (§G1.3), then trailing `/` bytes are trimmed. With `allow_keywords`, the result must pass `is_url`, otherwise `-APKE_REPO_SYNTAX`.
- **Supported schemes** (`doc/apk-repositories.5.scd:54-59`): `http://`, `https://`, `file://`, and absolute `/path`.
  - Locality is decided by `apk_url_local_file` (`io.c:1343-1354`). These are local:
    - length < 4;
    - first byte `/`;
    - prefix `file:` (stripped before opening);
    - prefix `test:` (stripped, but forced "remote" by `add_repository_component`);
    - otherwise, when the first `:` within byte offsets `0 .. min(10,len)-3` is not immediately followed by `//`, or there is no such `:`.
  - Everything else is remote.
  - Local index files are opened relative to the **process working directory** (`AT_FDCWD`, `database.c:633-641, 1630`; `io.c:622-627`), **not** `--root`.
- **Components:** the rest of the line after the URL word, then substituted, then split by `apk_blob_foreach_word`.
  - Only spaces separate components; runs collapse. A TAB or CR stays inside a component.
  - Each component calls the repository callback with `url + "/" + component` (`apk_pathbuilder_pushb`, `pathbuilder.c:12-21`).
  - The doc says components are invalid with `ndx` (`doc/apk-repositories.5.scd:63`), but the code accepts them (§H1).
  - The first non-zero callback return stops the line.

#### G1.3 Variables (`set`) and substitution

`apk_repoparser_parse_set` (`repoparser.c:108-136`):
- Leading words that start with `-` are options. Only `-default` is valid: it clears `APK_VARF_OVERWRITE` (1). Anything else gives `-APKE_REPO_SYNTAX`.
- The rest is split at the first `=`. The line is rejected with `-APKE_REPO_VARIABLE` if:
  - there is no `=`;
  - KEY starts with `APK_`;
  - KEY's first byte is not `isalpha`;
  - KEY contains a non-VARIABLE_NAME byte. Spaces around `=` are therefore invalid.
- VALUE is everything after the first `=`, spaces included, then substituted into a `PATH_MAX` buffer.
- `apk_variable_set` (`repoparser.c:35-54`): a new key is always set. An existing key is changed only if OVERWRITE is set and the variable is not `APK_VARF_READONLY` (2). A refused change returns 0 silently.
- Built-in: `APK_ARCH` = primary arch, read-only (`database.c:2060`).

`apk_blob_subst` (`blob.c:182-208`), with the resolver `apk_repoparser_subst` (`repoparser.c:99-106`). The same syntax is used for package-name specs (§G1.6):
- `${NAME}` expands to the value. `NAME` is everything up to the first `}`; there is no nesting.
- `${NAME:N}` truncates the expansion to at most N bytes. N is parsed with `pull_uint(10)` and must consume the whole remainder, otherwise `-APKE_FORMAT_INVALID`.
  - `${NAME:}` is accepted and expands to nothing (N = 0).
- `${` without a later `}` gives `-APKE_FORMAT_INVALID`.
- An undefined name gives a warning plus `-APKE_REPO_VARIABLE`.
- Output larger than the buffer gives `-APKE_BUFFER_SIZE`.
- `$x` without braces, and a lone `}`, stay literal.

#### G1.4 Repository type (`repoparser.c:182-197`)

- Without a type keyword: if the **raw, unsubstituted, untrimmed** URL word ends in `.adb` or `.tar.gz`, the type is `ndx`; otherwise **`v2`**. The default is v2, not v3.
  - So `https://h/x/Packages.adb/` (trailing slash) is treated as v2.
  - `${MIRROR}/x86_64/Packages.adb` is ndx.
- Index file: `v2` uses `APKINDEX.tar.gz`, `v3` uses `Packages.adb` (`repoparser.c:194-196`), and `ndx` has none (the URL is the index).

#### G1.5 Repository record: `add_repository_component` (`database.c:1553-1609`)

`ARCH` is `db.arches[0]`, the primary arch. It is chosen in this order (`database.c:2048-2059`):
1. The `--arch` values in command-line order (`apk.c:96-97`), if `--root` is set to something other than `/` (`context.c:75-83`), or the applet has `APK_OPENF_ALLOW_ARCH`.
2. Otherwise each line of `<root>/etc/apk/arch` (`apk_db_add_arch`, `database.c:546-556`). Zero-length lines are skipped, then trailing whitespace is trimmed. A whitespace-only line therefore adds the arch `""`. Duplicates are skipped.
3. If there is still none, the compile-time `APK_DEFAULT_ARCH` (`apk_arch.h:63-67`).

| Type | `url_index` | `url_base` | default `pkgname_spec` |
|---|---|---|---|
| v2 / v3 | `URL "/" ARCH "/" index_file` | `URL` | `${arch}/${name}-${version}.apk` (`context.c:38`) |
| ndx | `URL` | `URL` up to its last `/`, or `.` if there is none (`database.c:1562-1576`) | `${name}-${version}.apk` (`context.c:37`) |

Other properties of a repository record:
- **Dedupe:** if an existing repository has the same `(url_base, url_index)` (`apk_blob_compare`), only `BIT(tag_id)` is ORed into its `tag_mask`.
- **Limit:** at most `APK_MAX_REPOS = 32` (`apk_defines.h:146`). A 33rd returns -1, which aborts the rest of that file or line.
- `is_remote = (apk_url_local_file(url_index) == NULL) || url_index starts with "test:"`.
- `v2_allowed = !url_index.ends_with(".adb")`. `load_v2index` returns `-APKE_FORMAT_INVALID` for such a repository (`database.c:1466`). v3 indexes are accepted for **every** type (`load_v3index` has no check).
- **Format detection is by content** (`apk_extract`, `extract_v3.c:319-330`; §B2). It peeks 4 bytes (a shorter stream is an error); if the first 3 are `"ADB"` the stream is v3, otherwise v2 tar.
- `repo.hash = SHA-256(url_index bytes)` (`database.c:1606`).
- **Index root fields** (`apk_adb.c:448-456`; `apk_adb.h:85-88`; §D2): 0x01 `description`, 0x02 `packages`, 0x03 `pkgname-spec`.
  - If 0x03 is present, it replaces the repository's spec.
  - `absolute_pkgname` is set if the spec contains `://` anywhere (`database.c:1485-1490`).
  - mkndx writes 0x03 only when `--pkgname-spec` was given (`app_mkndx.c:354`). v2 indexes cannot carry a spec.

#### G1.6 Package URL: `apk_repo_package_url` (`database.c:660-680`)

```text
url = (absolute_pkgname ? "" : url_base + "/") + subst(pkgname_spec, pkg)
```

- **Spec selection.** The index's `pkgname-spec` if present; otherwise the repository default from the table in §G1.5.
- **Variables** are resolved by `apk_pkg_subst` (`package.c:100-114`) through `apk_blob_subst` (`blob.c:182-208`; syntax in §G1.3). Names are exact and case-sensitive; any other name gives `-APKE_PACKAGE_NAME_SPEC`. `${var:N}` truncates the expansion to N characters.

| Variable | Value |
|---|---|
| `name` | package name |
| `version` | package version string |
| `arch` | **the package's own** `arch` (pkginfo 0x05), not the repository arch; `""` if absent |
| `hash` | lower-case hex of the full stored identity, `apk_digest_alg_len(digest_alg)` bytes: 40 hex characters for SHA1, 64 for SHA256. The identity length chooses the algorithm: 20 → SHA1, 32 → SHA256, 64 → SHA512 (`crypto.c:39-58`). |

Consequences:
- **v2/v3 base-URL repository with no index spec:** a `noarch` package is fetched from `<base>/noarch/<name>-<version>.apk`. `mkndx --rewrite-arch` is rejected with a hint to use `--pkgname-spec 'ARCH/${name}-${version}.apk'` instead (`app_mkndx.c:89-91`).
- **An index spec is relative to `url_base`, and `url_base` depends on how the index is referenced.**
  - Referenced as `v3 <base>`, the spec is relative to `<base>`.
  - Referenced as an ndx URL, the spec is relative to the index's directory.
  - So the spec `${arch}/…` resolves to `<base>/x86_64/x86_64/…` when the index is referenced as `<base>/x86_64/Packages.adb`.
- A spec starting with `/` is not absolute: it produces `url_base + "//…"`. Only `://` makes a spec absolute.
- `apk_pkg_subst_validate` (`package.c:116-141`) requires the last path segment to start with `${name}` followed by `_`, `-` or `.`. It is **never called** in 3.0.8, so an index's spec is not validated at load time. The filename-to-package reverse lookups still assume that shape: `apk_db_get_pkg_by_name` (`database.c:2597-2635`) and mkndx `find_package` (`app_mkndx.c:204-250`).

Download source selection (`apk_db_select_repo`, `database.c:2667-2679`):
1. The cache, if the package is cached.
2. An explicit file (`filename_ndx`).
3. Otherwise the lowest-index repository in `pkg.repos & available_repos`, restricted to local repositories if any local one qualifies.

#### G1.7 Cache

- **Package cache name:** `${name}-${version}.${hash:8}.apk`, the first 8 hex characters of the identity (`context.c:39`). The cache repository has `absolute_pkgname = 1`, so the name is relative to the cache fd (`database.c:1704-1714`).
- **Index cache name:** `APKINDEX.${hash:8}.tar.gz`, where hash is SHA-256(`url_index`). The same name is used **even for v3 `Packages.adb`** (`database.c:643-658`).
- **Location** (`setup_cache`, `database.c:1927-1953`), relative to `--root`:
  1. `--cache-dir`, or the default `etc/apk/cache` (`context.c:70-71`).
  2. If the default does not exist (ENOENT) and `--cache-dir` was not given, fall back to `var/cache/apk`, creating it if needed.
  3. If that still fails: in write mode, `-EROFS`; otherwise the cache is unavailable.
  - Package caching (`cache_packages`) is enabled automatically only when the first choice opened (or with `cache_predownload`).
  - `--no-cache` (`APK_NO_CACHE`) skips all of this.
- **Index reading** (`open_repository`, `database.c:1622-1681`):
  - Local indexes are read in place.
  - Remote indexes are read from the cache file when `is_index_cacheable`: not `--no-cache`, and either not stale or the cache is writable (`database.c:1615-1620`). A stale index is downloaded into the cache first.
  - Otherwise the remote index is fetched directly.

#### G1.8 Notes for Valence

- A line with the full index path, such as `https://host/valence/x86_64/Packages.adb` or `/srv/repo/x86_64/Packages.adb`:
  - is auto-detected as `ndx`, and v2 is disallowed;
  - packages resolve to `https://host/valence/x86_64/<name>-<version>.apk`, with no arch directory added, unless the index carries a `pkgname-spec`.
- `v3 https://host/valence` resolves the index to `…/valence/<primary arch>/Packages.adb` and packages to `…/valence/<pkg arch>/<name>-<version>.apk`. `noarch` packages therefore go to `…/valence/noarch/`.
- A bare base URL with no keyword means v2 and requests `…/<arch>/APKINDEX.tar.gz`. Detection is by content, so an ADB file served under that name would still load.
- Putting `v3` in front of a full `Packages.adb` URL requests `…/Packages.adb/<arch>/Packages.adb`.

### G2. `/etc/apk/world`

**Paths** (`database.c:46, 1331-1335, 1955-1964`):
- Root layer (0): `<root>/etc/apk/world`.
- uvol layer (1): `<root>/lib/apk/db-uvol/world`.

**Read** (`apk_db_read_layer`, `database.c:1316-1344`; skipped with `APK_OPENF_NO_WORLD`):
1. Read the whole file (`apk_blob_from_file`) and apply `apk_blob_trim`, which removes trailing whitespace only.
2. Parse with `apk_blob_pull_deps(…, allow_tag = true)` (`package.c:229-245`; §F3.2).
3. Each token runs to the next space or `\n`, and the following run of separators is skipped.
   - TAB and CR are not separators. In a CRLF file every entry except the last keeps a trailing `\r`, either in the name or in the version, which then becomes `broken`.
4. Token syntax: `[!]name[@tag][op version]`. The text before the operator is split at the first `@`. The tag is matched against the stored **plain** name, without `@`.
   - A tag with no slot creates one (`database.c:2393-2415`).
   - With an empty tag (`foo@`) the code reads `tag.ptr[0]` past the end. **[unconfirmed]** Not further traced.
5. World dependencies do **not** record which file they came from. The read sets `layer = 0`. After solving, each world dependency's `layer` is set to the chosen package's layer (`solver.c:1140-1143`), and that decides which file it is written to.

Errors and edge cases:
- **Syntax error:** parsing stops; earlier dependencies are kept; `-APKE_DEPENDENCY_FORMAT`.
- **Invalid version:** the dependency is kept with `broken = 1`, parsing continues, and the return is `-APKE_PKGVERSION_FORMAT`. The solver skips broken world dependencies (`solver.c:1098-1108`).
- **Root layer:** any non-zero result makes `apk_db_open` fail with "Unable to read database" (`database.c:2137-2147`). That covers a syntax error, an invalid version, and any failure to read the file, which is reported as `-ENOENT` (line 1342). The only exception is `-ENOENT` with `APK_OPENF_CREATE`.
- **uvol layer:** any error just leaves the layer inactive (line 2141). Dependencies already parsed stay in `db->world`.
- **Leading separator:** a separator at the start of the file (for example a leading blank line) produces a dependency on the empty name `""`. `apk_db_get_name("")` succeeds. By code trace, the solver then sees a required name with no providers and counts an error (`solver.c:787-793`). **[unconfirmed]** Not run.
- **Unknown tag:** a tag that no available repository carries gets `allowed_repos == 0`. `apk_db_check_world` warns (`database.c:2572-2589`), and `apk_solver_commit` then **refuses**: "Not committing changes due to missing repository tags." (`commit.c:400-403`). `--force-broken-world` bypasses this.
- More than 15 distinct tags gives tag id -1, stored in the 6-bit `repository_tag` field as 63, which indexes past `repo_tags[16]`. That is undefined behaviour.

**Write** (`apk_db_write_layers`, `database.c:2252-2263`; `apk_deps_write_layer`, `package.c:326-349`; `apk_blob_push_dep`, `package.c:303-315`):
- Each world dependency whose `layer` matches is written as `["!"] name ["@"tag-with-@] op_string(op) version`.
  - The version atom is never a NULL pointer, so `op_string` and the version are always appended.
  - For ANY both are empty.
  - Quirk: `foo<=>1` is rewritten as `foo1`.
- Entries are joined by `\n`, and one final `\n` is always written. An empty world is the single byte `\n`.
- Each entry goes through a 256-byte buffer. An entry that does not fit aborts writing the rest of that layer's list; the final `\n` is still written.
- The world array is sorted in place by `apk_name_cmp_display` before solving: `strcasecmp`, then `strcmp` (`solver.c:1048-1052, 1083`; `apk_database.h:262-264`). Readers must not rely on order.

**Typical content:**
- `name`, `name@tag`, `name>=1.2`, `!name`.
- `name><Q1` plus 28 base64 characters, from `apk add ./file.apk` (§F3.8).
- `virt=YYYYMMDD.HHMMSS` (UTC, `gmtime_r`) from `apk add -t virt` (`app_add.c:96-105, 121-144`). An explicit `-t virt=VER` is also accepted.

---

## Part H. Open, unconfirmed and inconsistent points

### H1. Where the documentation disagrees with the code

Follow the code in every case.

| Document | Says | Code (3.0.8) | See |
|---|---|---|---|
| `doc/apk-v3.5.scd:133-135` | the ADB block "must begin with a magic number indicating the schema" | the schema is in the 8-byte file header before the first block | §B4 |
| `doc/apk-v3.5.scd:123-130, 159` | block type 3 is "DATAX" and illegal | 3 is the extended (16-byte) block-header escape; the real type is in the low 30 bits | §B5.1 |
| `doc/apk-v3.5.scd:132-133` | "one or more DATA blocks" | zero DATA blocks are valid (every index) | §B5.2 |
| `doc/apk-v3.5.scd:138-142` | "The SIG block contains one or more signatures" | exactly one signature per SIG block | §C1.4 |
| `doc/apk-v3.5.scd:107-115` | ACL has mode, user and group | the ACL also has `xattrs` (slot 4) | §D8.3 |
| `doc/apk-package.5.scd:69-71` | `hashes` is present only in the index copy | mkpkg writes a 20-byte unique id into the package's own `info.hashes` | §D10.1 |
| `doc/apk-repositories.5.scd:63` | components are invalid with `ndx` | the code accepts them | §G1.2 |
| `doc/apk.8.scd:200-203` | `--keys-dir` is "treated relative to _ROOT_" | true for relative paths; an absolute path ignores the root fd (`openat`) | §C4.1 |
| `doc/apk-package.5.scd:223-227` | install_if should have at least two dependencies, one using `=` | a convention only; nothing in `src/` enforces it | §F5 |

### H2. Unconfirmed points, by part

#### H2.1 Container, compression and value encoding (Part B)

- **`ADB_TYPE_ERROR` on disk.** Whether any real writer path actually stores an error tag in a file. By code trace, convndx (`app_convndx.c:31`), convdb (`app_convdb.c:130`) and adbgen (`app_adbgen.c:163`) do not check `adb_w_fromstring`, so a malformed v2 field should produce one; not reproduced (§B7.1).
- **Big-endian `adb_w_copy` bug.** BLOB_16/BLOB_32 length prefixes are read without byte swapping (`adb.c:820,826`); the effect on files written on big-endian hosts is unconfirmed (§B7.5).
- **`"ADBc"` with alg 0** behaves inconsistently between the mmap and stream paths; writers never produce it (§B3.5).
- **Decoder leniency.** The C deflate decoder parses bytes after the end of the raw stream as a gzip/zlib member, and both decoders report truncated input as EOF (§B3.2).
- **Nested ADBs.** A SIG block inside a nested ADB blob would make apk dereference a NULL trust pointer; nested-ADB arrays are not reliably sorted (§B7.9).

#### H2.2 Signatures and trusted keys (Part C)

- **[lib]** Library behaviour that the apk source does not state: the RSA padding (PKCS#1 v1.5 default); the `i2d_PublicKey` / `mbedtls_pk_write_pubkey` encoding (PKCS#1 `RSAPublicKey`); which PEM types each parser accepts; the exact signature-length rules (§C2.3, §C3, §C4.3).
- Whether non-RSA (ECDSA) keys are used in practice. The code allows any key type the backend can use with SHA-512. For EC keys the two backends may differ in point encoding, and so in key ID (§C2.3, §C3).
- Whether OpenSSL's `PEM_read_bio_PUBKEY` accepts PEM types other than `PUBLIC KEY` (for example `RSA PUBLIC KEY`); this depends on the OpenSSL version (§C4.3).
- **Crypto backend default.** `src/meson.build:1` uses `get_option('crypto_backend')`, but `meson_options.txt` is not in the provided tree. `src/Makefile:9-15` selects OpenSSL flags unless `CRYPTO=mbedtls`, but line 25 builds `crypto_$(CRYPTO).o`, so `CRYPTO` must be set by a top-level Makefile, which is also absent. The format is the same with either backend (for RSA).
- `adbsign` appears to need `--allow-untrusted` for its input, because trusted keys are loaded only after the parse; not tested (§C6).
- The mbedTLS < 3.0 signing branch refers to `mp->pkg` (`crypto_mbedtls.c:253`), which looks like a compile error in that configuration (writer-only, §C2.3).
- Part C did not itself check the stream-path behaviour when a file ends inside a block's trailing padding. Part B's trace answers it: missing padding at EOF is `-APKE_EOF` (`io.c:188-198,645-659`; §B5.1, §B8).

#### H2.3 Schemas (Part D)

- **`S_IF*` and `PATH_MAX`** come from platform headers, not apk-tools. §D8.2 gives the standard Linux values.
- **Root-directory hardlinks.** mkpkg writes the target `./<name>` for a hardlink to a root-level file. apk 3.0.8's installer (`apk_db_ipkg_find_file`, looking up directory `"."`) and its fs extractor (`apk_fs_is_malicious_pathname`) would both reject such a target. Found by reading the code, not by testing (§E2).
- **Sort order** is enforced only by the writer (`pre_commit`). Dependency sorting treats a NULL version as equal to any version. Readers must not rely on order or uniqueness (§D3, §F3.7).
- **`num_compare` and `file-size`.** In TEMPLATE and INDEX comparisons apk stops at slot 3, so mkndx's `find_package()` never compares `file-size` (slot 13), although it sets it in the template (`app_mkndx.c:235`; `adb.c:566-569`). Writer only.
- **`${hash}` in mkndx/mkpkg.** Those tools expand templates with `adb_s_field_subst`, which knows pkginfo field names (`"hashes"`, not `"hash"`) (`adb.c:1166-1197`). The runtime expander `apk_pkg_subst` knows `"hash"`. With these sources, `${hash}` in `--pkgname-spec` makes `adb_s_field_subst` return `-APKE_ADB_SCHEMA` at index time, while it works at download time.
- **Unknown slots during re-indexing.** `adb_wo_copyobj` would hit `APKE_ADB_LIMIT` for a package `info` slot ≥ `ADBI_PI_MAX` (`adb.c:992`). That error sets `db->schema = 0` (`adb.c:665-669`), so `adb_c_adb` would fail with `-APKE_ADB_HEADER`. Readers simply ignore such slots.
- **Meaning of a 20-byte hash.** A 20-byte `hashes` value cannot be interpreted from the index alone: it can be the SHA-1 of a v2 control segment, a truncated SHA-256 of a v3 ADB payload (sha256-160), or a convndx-decoded `Q1`. apk treats every 20-byte value as SHA1-class and relies on signatures for v3 packages (§D10).
- **ERROR values from the conversion tools.** convndx and convdb can store `ADB_TYPE_ERROR` values for malformed v2 fields; the exact error codes depend on which `fromstring` failed (§B7.1).
- **Installed-DB array order** is probably not meaningful, because the sort compares empty objects; read from the code, not tested (§D11).
- **Local packages vs index entries** are keyed differently (unique id vs the first 20 bytes of the SHA-256 identity); the source does not say whether this is intended (§D10.7).

#### H2.4 File contents and extraction (Part E)

- **Root-level hardlinks** (same issue as in §H2.3): per-consumer consequences traced, not tested (§E2).
- **`apk extract` with pruned ancestors.** For packages built with `--compat >= 3.0.0_rc9`, `apk extract` appears unable to extract anything beneath a pruned ancestor directory. Inferred from code, not tested (§E4, §E8.2).
- **`xattr_fromstring`** appears to write `'='` instead of the NUL separator. Inferred from code, not tested (§D8.4).
- **Unhandled reader edge cases.** apk does not handle these gracefully; a reader should reject them: out-of-range blobs (§B7.5), path-builder overflow (§E7.1), mode bits above 07777 (§E5.1), `rawsize < hdrsize` on the stream path (§B9).
- **Truncation on the stream path** may go undetected when the consumer skips the last file's content and the block needs no padding. Inferred from code (§B8).
- **Device numbers** are raw host `dev_t` values (`st_rdev`); apk defines no portable major/minor encoding (§E2).
- **Unchecked bytes.** Padding bytes and the extended header's `reserved` field are never checked (§B5.1).
- **Metadata-only reads on the mmap path** would end in `-APKE_ADB_BLOCK`, because `db->adb` is cleared on `-ECANCELED`. Inferred from code; package extraction never uses mmap (§E8).

#### H2.5 Versions, dependencies and repositories (Parts F and G)

- The runtime effect of an empty-name world entry (§G2) and of an unversioned provide satisfying `<`-type dependencies (§F4.4). Both were traced from code, not run, and both look unintended.
- `!foo><…` conflicts with every normally versioned provider of `foo` (§F3.8): a code quirk, not a documented feature.
- `apk_url_sanitize` (the printable URL form) was not traced. It does not affect `url_index` or the hash.
- Behaviour with more than 16 tag slots (`BIT(-1)` in `add_repository_component`; tag id 63 indexing past `repo_tags[16]` in world), and with an empty `foo@` tag: undefined behaviour in the C code (§G1.2, §G2).
- Names containing NUL bytes: hashing uses the full blob, but most later uses treat the name as a C string (§F4.1).
- There are no test vectors in the tarball (only `src/` and `doc/`). The examples in §F1.6, §F3.4 and §F4.3 come from tracing the code by hand.

