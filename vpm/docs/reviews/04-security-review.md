## vpm security review: no signature or identity bypass; 3 Medium findings an untrusted mirror can trigger, 2 Medium confinement findings that need signed content, plus hardening items

I reviewed `C:\Users\a\AppData\Local\Temp\claude\39afd7d0aa98685a\9f48dd28-e365-4eae-aba4-ec797468f31d\scratchpad\vpm` at commit 9a02b8f (the binary matches it) and ran crafted inputs against `/root/vpm-target/release/vpm`. I changed no code. Nothing was mounted, every delete checked `/proc/mounts` first, and the mirror ran only on 127.0.0.1.

### Test material
- **ADB writer:** `C:\Users\a\AppData\Local\Temp\claude\39afd7d0aa98685a\9f48dd28-e365-4eae-aba4-ec797468f31d\scratchpad\vpm-sec-adb.py`, written by hand. It signs with RSA PKCS#1 v1.5 SHA-512, as apk does.
- **Input generator:** `C:\Users\a\AppData\Local\Temp\claude\39afd7d0aa98685a\9f48dd28-e365-4eae-aba4-ec797468f31d\scratchpad\vpm-sec-gen.py`. It writes everything to `/root/vpm-test/sec/`. Key **T** (`keys/T.rsa`) stands in for the real signing key and is trusted through `keys/trusted`. Key **A** is the attacker's and is not trusted.
- **Hostile mirror:** `C:\Users\a\AppData\Local\Temp\claude\39afd7d0aa98685a\9f48dd28-e365-4eae-aba4-ec797468f31d\scratchpad\vpm-sec-httpd.py`.
- **Test scripts:** `C:\Users\a\AppData\Local\Temp\claude\39afd7d0aa98685a\9f48dd28-e365-4eae-aba4-ec797468f31d\scratchpad\vpm-sec-1.sh` (signatures and identity), `vpm-sec-2.sh` (bombs), `vpm-sec-3.sh` (confinement), `vpm-sec-4.sh` (HTTP) and `vpm-sec-lib.sh` (shared helpers), all in the same scratchpad folder.
- **Logs:** `/root/vpm-test/sec-log-1.txt` to `/root/vpm-test/sec-log-4.txt`. Test roots are in `/root/vpm-test/sec/roots/`.

### Checks that held (no finding)
- **Forged indexes are refused:**
  - an index signed by A;
  - a SIG block that carries T's key id but A's signature, with hash_alg 4 and with hash_alg 3;
  - `sign_ver` 1;
  - an unsigned index.
- **Tampered packages are refused, and nothing reaches the live tree or Programs:**
  - a package swapped for an A-signed one fails with "package identity mismatch";
  - one changed content byte fails the sha256 check;
  - an extra DATA block is refused, and so are trailing bytes.
- **Accepted, and correctly so:** an unsigned package whose ADB matches the index identity, because the identity alone establishes trust, as in apk.
- **Unsafe paths inside a package are refused:** `usr/../../escape`, `/tmp`, a file named `..`, an entry below the package's own symlink, and hard links to `../etc/passwd` or `/etc/passwd`.
- **A symlink in package A to an absolute host path, with package B placing a file below it, stays inside the root.** B's file then fails with ENOENT while the run still prints "OK: 2 packages installed". That is a robustness bug, not a security one.
- **There is no check-then-use gap:** content is hashed in the same pass that writes it, and a cached download is verified again when it is unpacked.
- **Temporary files in unpack and `copy_to`** are created with `O_EXCL` and mode 0600, then `fchown`/`fchmod`.

### Findings, most severe first

**F1. Medium. Memory exhaustion before any signature check (SIG blocks are kept without limit; the ADB block can be 256 MiB).** Anyone who can serve a file (a mirror, or a crafted .apk or .adb passed to `verify`, `dump` or `manifest`) can do this.

Reproduction:

| Run | Input | Result |
|---|---|---|
| `vpm --keys-dir /root/vpm-test/sec/keys/trusted verify /root/vpm-test/sec/bomb/sigbomb-8192.adb` | 1,072,055-byte deflate file: 1 ADB block + 8192 SIG blocks of 131,064 zero bytes | max RSS 1,082,000 KiB |
| same, `sigbomb-1024.adb` | 134 KB | 142,308 KiB (growth is linear) |
| `vpm --root R --initdb --repository …/sigbomb-8192.adb add hello` | same file as an index | 1,082,504 KiB |
| `apk verify` on the same file | | 8,340 KiB |
| `/root/vpm-test/sec/u7-pkg-sigbomb/hello-1.0-r0.apk` | the real ADB block (identity matches) + 8192 junk SIG blocks, local repository | installed successfully after using 1,057,464 KiB |
| `bomb/adbbomb.adb` | 260 KB | 268,216 KiB |

- Each worker multiplies this; there are up to 16 workers (`engine.rs:163-169`).
- For remote packages the file must have the size given in the index, so a large real package leaves room for the bomb. Remote indexes have no size check at all.

Code:
- `src/adb/mod.rs:54-63`: `read_head` pushes every SIG payload into `sigs`, with no limit on their number.
- `src/adb/container.rs:28,464-478`: an ADB payload of up to 0x0fffffff bytes is read into memory before any trust check.
- `src/verify.rs:360-368`: an RSA check is attempted for every SIG block whose key id matches a trusted key, and one error string is kept per block.
- Callers: `src/fetch.rs:94` and `src/unpack.rs:202`.

Fix:
- Handle SIG blocks as a stream. The ADB digest is known by then, so verify each block as it is read, stop at the first one that verifies, and skip the rest without storing them.
- Cap the SIG block count (apk writes 1; refuse more than about 8) and the length of the error message.
- When the index gives an identity, compare it right after the ADB block, before reading any SIG block.
- Cap the ADB payload: a small fixed limit for packages, and a configurable one (about 64 MiB) for indexes.
- Set the zstd decoder's `window_log_max`.

**F2. Medium. An older, validly signed index is accepted silently (replay and freeze), and it overwrites the cached newer one.**

Reproduction (`vpm-sec-4.sh` H1):
1. The mirror serves `replay-v2` (hello-2.0-r0); `add hello` installs 2.0-r0.
2. The mirror switches to `replay-v1`.
3. `upgrade` prints "OK: nothing to do" with no warning, and the cached `Packages.0c8d82f2.adb` is now v1.
4. A fresh root running `add hello` installs **hello-1.0-r0** with no warning.

An attacker can therefore hold back security fixes indefinitely and hand new installs the old vulnerable versions. Installed packages are not downgraded by `upgrade`, because the solver keeps the newer installed candidate. apk v3 has the same weakness.

Code:
- `src/repo.rs:124-140`: the download replaces the cache, and on a failed download the cache is used.
- No freshness state is kept anywhere.

Fix:
- Keep, per repository in `var/lib/vpm/`, the identity of the last accepted index and a monotonic generation. Use the maximum `build-time` of its packages, or better a signed timestamp that Valence's builder adds to the index.
- Refuse an older index unless an `--allow-rollback` option is given, and warn when the generation has not advanced for N days.
- Warn whenever an index offers only versions lower than the installed ones.

**F3. Medium. HTTP downloads have no size cap and no overall deadline, and the cached index is replaced before it is verified.**

Reproduction (H2–H4):
- **Oversized index:** a 512 MiB junk index was written whole into `var/cache/vpm/Packages.0c8d82f2.adb`, replacing the verified one. With the mirror then returning 404, vpm printed "using the cached index" and failed on the junk. The failure persists.
- **Oversized package:** the index says 719 bytes, but the mirror streamed 256 MiB. All 268,435,456 bytes went to disk before the "size differs" error.
- **Slow mirror:** at 1 byte every 10 s, vpm was still waiting when `timeout` stopped it at 75 s, holding the root's lock the whole time. A `.part` file was left behind and is never cleaned up.

Code:
- `src/fetch.rs:29-38`: the read timeout applies to each read only, and there is no `https_only`. ureq follows https→http redirects by default.
- `src/fetch.rs:42-67`: no byte limit.
- `src/fetch.rs:126-127`: the size is checked only after the download.
- `src/repo.rs:128-134`.

Fix:
- Pass a maximum length to `download`. For a package it is the index `file-size` exactly: check `Content-Length` up front and abort as soon as the count is exceeded. For an index, use a configurable cap.
- Add an overall deadline or minimum transfer rate.
- Download the index to `.new`, verify it (and check freshness, F2), and only then rename it over the cache.
- Remove stale `.part` files at start.
- Set `https_only` for https repositories.

**F4. Medium (needs a trusted-signed package; works with `--no-scripts`). Package entries may sit inside vpm's own namespace, and vpm uses those directories through symlinks on the host, so it writes and recursively deletes outside `--root`.**

Reproduction (`vpm-sec-3.sh`):
- **C3:** package `evil-plant` contains the symlink `Programs/victim -> /root/vpm-test/sec/host-programs`. The host directory held `1.0-r0/canary`. Running `add victim --no-scripts`:
  - **deleted the canary**, through `remove_dir_all` on `fin`;
  - unpacked victim's files and `.vpm/` metadata into the host directory;
  - the root now lists victim as installed from there.
  
  A version such as `etc` is accepted (C7 installed `Programs/weirdver/etc`), so with the symlink pointing at `/` this would `remove_dir_all("/etc")` on the host.
- **C4:** `Programs/zzz/Current -> ../x` bricks the database: `list`, `add` and `del` all fail with "unexpected link target".
- **C5:** `var/lib/vpm/exec -> /root/vpm-test/sec/host-exec`. The next script run changed the host directory to mode 0700 and overwrote and then deleted a host file with the same name as the script.

Planted files under `Programs/` also get around the inter-package file-conflict check, for example by rewriting another package's Programs copy or its `.vpm/files`.

Code:
- `src/commit.rs:340-358`: `place` accepts any path.
- `src/adb/schema.rs:546-626`: no reserved prefixes.
- `src/unpack.rs:165-177`: `create_dir_all`, `exists`, `remove_dir_all` and `rename` through `Programs/<name>`.
- `src/commit.rs:459-465`: the exec directory.
- `src/db.rs:493-506`: `reload` aborts.
- `src/db.rs:428-430`: versions are not validated; `version::validate` exists but is unused.
- `src/commit.rs:814-829`.

Fix:
- Refuse entries whose literal path, or whose canonical in-root path at switch time, is or lies under `Programs`, `var/lib/vpm` or `var/cache/vpm`.
- Open vpm's own directories with `O_DIRECTORY|O_NOFOLLOW` and work through `*at()` calls relative to those descriptors. Treat a symlinked `Programs/<name>` or `<version>` as corruption and never follow it.
- Validate versions with `version::validate` inside `safe_component`.
- In `reload`, skip corrupt entries with a warning, and add a repair command.

**F5. Medium/Low. The directory cache in `RootFs` is never cleared after scripts, so a harmless post-install can make vpm write outside the root.**

Reproduction (C6): `stale-a`'s post-install runs `rm -rf /var/x; ln -s /root/vpm-test/sec/host-stale /var/x`, a common pattern (compare `/var/run -> /run`). `stale-b`, in the same transaction, has `var/x/b`. vpm placed b at **`/root/vpm-test/sec/host-stale/b` on the host**, as a hard link sharing the inode of `Programs/stale-b/1.0-r0/var/x/b`. `remove_old` uses the same cache, so deletions can go astray the same way.

Note that scripts run as root in a bare chroot, which is not a boundary: they can mount proc and reach `/proc/1/root`. So this matters for harmless scripts and for `--root` image builds, not as protection against malicious scripts.

Code:
- `src/rootfs.rs:127-147`: `dir_cache`.
- `src/commit.rs:373-418`: `script_at` reloads the user and group ids but does not call `fs.forget()`.
- `src/commit.rs:577`: `forget` is called only here.

Fix:
- Call `forget()` after every script and after `ensure_dev_nodes`.
- Better: do every live-tree operation relative to a descriptor for the root, using `openat2(RESOLVE_IN_ROOT|RESOLVE_NO_MAGICLINKS)` for the parent directory, then `linkat`, `renameat`, `unlinkat`, `mkdirat` and `symlinkat`. This also removes the gap between `resolve()` and use.

**F6. Low. Some operations on live paths follow symlinks and are not atomic.**
- `src/commit.rs:747-753`: step 4 runs `hash_file` and `set_permissions` on etc files with special owners, following a final symlink resolved on the host. This can chmod a host file, or hang reading a FIFO or `/dev/zero`.
- `src/commit.rs:325`: `hash_file(<file>.vpm-new)` without an lstat first.
- `src/rootfs.rs:162-166`: `mkdir`, then `lchown`, then chmod by path. In a directory writable by another user, that user can swap in a symlink and get an arbitrary file chmodded.
- `src/commit.rs:461`: chmod of the exec directory.

Fix: open with `O_NOFOLLOW|O_NONBLOCK`, check with `fstat` that it is a regular file, then hash and `fchmod`/`fchown` the same descriptor. For directories use `mkdirat`, then `openat(O_DIRECTORY|O_NOFOLLOW)`, then `fchown`/`fchmod`.

**F7. Low. Keys, repositories and arch are read from the root through host paths.** In `src/verify.rs:142-168`, `from_root` follows absolute symlinks to host files and has no size cap (`from_dir` caps at 64000). A key file that is a symlink to `/dev/zero` is read until memory runs out, and an absolute symlink pulls host keys into the root's trusted set. The same host-path reads happen in `src/repo.rs:45-67`. Fix: resolve these paths inside the root, accept regular files only, and apply the 64000-byte cap.

**F8. Low. Old set-id binaries stay reachable during the switch.** Upgraded set-id binaries remain executable at the known path `Programs/<name>/<old>/…` until step 6 (`src/commit.rs:814-821`), and after a crash until the next run. Fix: after placing a package, strip the set-id bits (or chmod 0700 the old version directory) once its inodes are no longer linked from the live tree.

**F9. Info.**
- SHA-1 (20-byte) content hashes are accepted (`src/digest.rs:17-24`).
- A 20-byte index identity is silently ignored and trust falls back to the signature alone (`src/verify.rs:346`). Name and version are then the only binding, and arch is not compared (`src/unpack.rs:204`).
- SIG `hash_alg` 3 and 5 are accepted (`src/verify.rs:261-267`), 5 being a 160-bit truncation.
- Suggestion for Valence: accept only SHA-256/512 and hash_alg 4.
- File conflicts are detected by literal path, so a symlinked directory in one package can redirect another package's files over a third package's files without a conflict being reported (the engine author already noted this).
