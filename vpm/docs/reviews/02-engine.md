## vpm install engine and command line: done, and all end-to-end tests (a)–(d) pass

The engine is in `C:\Users\a\AppData\Local\Temp\claude\39afd7d0aa98685a\9f48dd28-e365-4eae-aba4-ec797468f31d\scratchpad\vpm` (in WSL: `/mnt/c/.../scratchpad/vpm`). It is committed locally as 76192c6 (the engine) and 9a02b8f (a trigger fix), with `Cargo.lock`. The author is "Valence port (local)" <port@valence.invalid>, set with `-c` options only, so no global git config was changed. Nothing was pushed.

- **Build:** `cargo build --release` gives no warnings and `cargo doc` is clean. The binary is `/root/vpm-target/release/vpm`.
- **Tests:** 31 unit tests and the 8 real-data tests pass.
- **New dependencies:** ureq 2.12.1 (no default features, `native-tls`) and native-tls 0.2.18, so TLS uses the system OpenSSL. ureq pulls in the url/idna/icu crates, which brings the build to 70 crates in total.
- **Build helper:** I overwrote `scratchpad\vpm-build.sh` without reading it first. It is now `vpm-build.sh [cargo args]`, defaulting to `build --release`.

### What was built (new modules)
- **`repo`**: reads repositories from `--repository` or `ROOT/etc/apk/repositories`.
  - A line ending in `.adb` is the index itself; anything else is a v3 base URL.
  - Local indexes are read in place. Remote ones are downloaded to `ROOT/var/cache/vpm/Packages.<hash8>.adb`.
  - Package URLs use the index's `pkgname-spec` or `${name}-${version}.apk`, with `${name}`, `${version}`, `${arch}`, `${hash}` and `:N` truncation.
- **`solver`**: a greedy resolver, not a full solver.
  - Packages match by name or by `provides` (so:, pc:, cmd:). A package's own name is preferred, then the installed version (add/del) or the newest (upgrade, or names just added), then `provider_priority`, then the provided version.
  - Depends are followed recursively. `install_if` runs to a fixpoint: a package is added when all its conditions are met.
  - `!name` conflicts refuse the transaction, and the result is in dependency order.
- **`fetch`**: http/https downloads go to `ROOT/var/cache/vpm/NAME-VERSION.HASH8.apk` through a `.part` file. Size and identity are checked against the index. Packages from local repositories are not copied; they are verified while being unpacked.
- **`unpack`**: one thread per CPU (at most 16), largest packages first, with fetch and unpack in the same worker.
  - Each package streams into `Programs/NAME/VERSION.partial`, then is renamed to `VERSION`. Trust is established before any content is read (identity from the index), and every file's hash is checked.
  - The core library rejects unsafe trees: absolute paths, `..`, anything below one of the package's own symlinks, bad hard links.
  - Mode, owner, mtime and xattrs are applied. Owners come from the live root's `etc/passwd` and `etc/group`; `root` is 0 and unknown names get 65534.
- **`db`**: `Programs/NAME/VERSION/.vpm/{info,files,scripts/}` in text formats documented in `db.rs`, plus `Programs/NAME/Current`, `var/lib/vpm/{world,journal,lock}` (flock) and `var/cache/vpm/`.
- **`commit`, the switch**: a journal (written to a temp file, fsynced, then renamed) lists the old and new version of each package, then for each package in dependency order (apk's order):
  1. pre-install or pre-upgrade script;
  2. owners re-resolved, since the script may have added users;
  3. each entry placed:
     - regular files outside `etc/` (and device nodes) are hard links to the Programs copy;
     - `etc/` files are copies, under the `.vpm-new` rule;
     - symlinks are recreated;
     - directories are made with the package's mode and owner;
     - each file goes to a temporary name next to its target, then `rename()` over it;
  4. the old version's entries that no package owns any more are removed, but only if they are still what it installed (same inode, hash or link target);
  5. `Current` flipped with a new symlink and a rename;
  6. post-install or post-upgrade script.

  Then removed packages (pre-deinstall, files, `Current`, post-deinstall), the world, a final owner pass, and triggers matched with `fnmatch` on apk's installed and modified directories. Last come syncfs, deleting the journal, and deleting the old versions.
- **Scripts**:
  - Each is written to `ROOT/var/lib/vpm/exec/NAME-VERSION.TYPE` and executed after `chroot(ROOT)` + `chdir("/")` + umask 022, so its `#!` line is honoured, as apk does.
  - Arguments are as apk passes them: pre/post-install `NEW`, pre/post-upgrade `NEW OLD`, pre/post-deinstall `OLD`, trigger `DIRS...`.
  - The environment is `PATH`, `APK_SCRIPT` and `APK_PACKAGE` only.
  - Every script that runs, and every package whose files are placed, is marked in the journal. A syncfs runs before each post script, so a recovery never re-places files under what a script changed.
  - One deliberate difference from apk: a script whose interpreter is not in the root yet is run after the other packages are placed. apk's own bash post-install fails because `/bin/sh` (busybox) comes later; under vpm it succeeds.
- **Recovery**: on every start, a leftover journal is finished (roll forward). Then leftover `.partial` and never-current version directories are deleted.
- **`rootfs`**: every live path is resolved inside the root the way `chroot` would. Absolute symlinks and `..` cannot lead to the host.
- **`progress`**: on a terminal, a full-screen view on the alternate screen, with one line and bar per package being fetched or unpacked and an overall bar. Otherwise (or with `--plain`) plain lines.

### Command line
```
vpm [--root DIR] [--repository URL_OR_PATH]... [--keys-dir DIR] [--initdb] [--plain]
    [--no-scripts] [--jobs N] [--allow-untrusted]
    add NAME... | del NAME... | upgrade | list | verify FILE... | dump-index FILE [--json] | dump FILE [--json] | manifest FILE
```
- Keys default to `ROOT/etc/apk/keys` then `ROOT/lib/apk/keys`; repositories default to `ROOT/etc/apk/repositories`.
- `add` accepts constraints such as `foo>=1.2`.
- Test hook: the environment variable `VPM_CRASH_AT=POINT` aborts the process at that point of the switch.

### Tests and results
Test scripts are in the scratchpad: `vpm-e2e-a.sh`, `-b.sh`, `-c.sh`, `-d.sh`, `-extra.sh`, `-big.sh`, `vpm-mkrepo2.sh`, `vpm-treecmp.py`, `vpm-upgcheck.py`. Logs are in `/root/vpm-test/log-*.txt`. No mounts were made, and `/proc/mounts` was checked empty before every `rm -rf --one-file-system`.

**(a) `vpm --root /root/vpm-test/base --initdb ... add valence-base`: pass**
- 39 packages, 716 files placed, in 0.2–0.26 s.
- Inside the chroot: `echo ok` gives ok; `ls --version` gives "ls (GNU coreutils) 9.12"; `apk --version` gives "apk-tools 3.0.8"; `$BASH_VERSION` is 5.3.9(1)-release; `id root` gives uid=0(root) gid=0(root).
- Hard links: all 617 regular files outside `etc/` share the inode of their Programs copy, with 0 failures. The 61 `etc/` files are separate copies (link count 1). `etc/passwd` is a regular file with link count 1.
- Against `apk --root X --initdb add valence-base --no-scripts`, ignoring apk's database and vpm's Programs, `var/lib/vpm` and `var/cache/vpm`:
  - A vpm root made with `--no-scripts` is identical: 799 entries, 0 differences in type, mode, uid/gid, size, mtime, content SHA-256 and link target.
  - The vpm root with scripts has 267 extra entries and nothing else different:
    - 262 applet symlinks to `/usr/bin/busybox`, made by busybox's post-install and trigger (`busybox --install -s`);
    - 5 device nodes in `/dev` (null, zero, random, urandom, console). vpm creates these when missing, like apk's own fallback when it cannot mount /dev. Without them, a script's `2>/dev/null` had created a regular file `dev/null`.

**(b) Same root: `add nano rsync`, `del rsync`, `list`: pass**
- add: 4 packages (nano, lz4, popt, rsync), 57 files, 0.04–0.11 s. The keys came from `ROOT/etc/apk/keys`, which a package installed, so no `--keys-dir` was needed.
- Inside the chroot, nano 8.7.1 and rsync 3.5.1 run.
- del rsync removes rsync, lz4 and popt (10 files) and `Programs/rsync`; nano still runs.
- list shows 40 packages, with world `nano`, `valence-base`.
- The same sequence with `--no-scripts` matches apk doing it: 848 entries, 0 differences.

**(c) Upgrade: pass**
- **Second repository:** `/root/vpm-test/repo2/x86_64`, built with `apk mkpkg`, signed with a throwaway key in `/root/vpm-test/keys/vpm-test.rsa` and indexed with `apk mkndx`. It holds:
  - valence-base-9999-r0: `etc/fstab` and `etc/hostname` changed, `usr/bin/halt` dropped, `usr/share/vpm-upgrade-test` added;
  - busybox-9999-r0: its scripts echo their arguments.
- Before the upgrade, the "admin" appended a line to the live `etc/fstab`. Then `vpm upgrade` ran with both repositories and `--keys-dir` holding both keys.
- **Every check passed:**
  - `Programs/{busybox,valence-base}` hold only `9999-r0` and `Current -> 9999-r0`; the old version folders are gone.
  - `etc/fstab` (admin-changed) was kept, and `etc/fstab.vpm-new` holds the new version.
  - `etc/hostname` was replaced.
  - `usr/bin/halt` was removed; the busybox trigger then recreated it as an applet symlink, which is correct.
  - The new file and `usr/bin/busybox` are hard links to the 9999-r0 inodes, and all 26 non-`etc` files of both packages link to the new versions.
  - The post-upgrade script received "9999-r0 1.37.0-r1".
  - No journal is left; a second `upgrade` says "nothing to do".
- **Interrupted upgrades:** each crash run started from a copy of the root taken before the upgrade, and was aborted at one of 11 points: journal, start, link:1, link:3, link:10, placed:busybox, placed:valence-base, placed, removed, world, triggers.
  - At every point, every live path of the two packages was the old or the new file (0 of 70–71 checked were neither), and the journal was present.
  - The next run (`vpm list`) finished the switch every time.
  - The result was identical to the uninterrupted upgrade (1117 entries, 0 differences), and scripts already marked as run were not run again.
  - Leftover `.partial` and stale version directories were removed on start.

**(d) Fresh install of valence-base, warm page cache, local repository** (12 CPUs):

| run | apk `--no-scripts` | vpm `--no-scripts` | apk with scripts | vpm with scripts |
|---|---|---|---|---|
| 1 | 0.264 s | 0.177 s | 0.284 s | 0.389 s |
| 2 | 0.264 s | 0.169 s | 0.292 s | 0.222 s |
| 3 | 0.269 s | 0.235 s | 0.287 s | 0.405 s |

- apk with scripts reports 1 error each time: bash's post-install cannot find `/bin/sh` yet.
- vpm with scripts ran at 0.200–0.267 s in an earlier round. The spread comes from the switch step, which varied 0.13–0.33 s.
- **vpm breakdown:** resolve 0.01 s, fetch+unpack 0.05–0.07 s, switch 0.10–0.33 s. The switch includes 2–4 syncfs calls (before the journal, before each post script, at the end); apk does not sync. I did not profile this further.
- **One thread:** `vpm --jobs 1 --no-scripts` takes 0.33–0.43 s, with fetch+unpack at 0.19–0.21 s.
- **Upgrade of 2 small packages:** apk 0.018 s on all 3 runs; vpm 0.037, 0.039 and 0.042 s (resolve 0.01 s plus the syncs).

**Extra checks (all pass)**
- **valence-desktop** (only 40 packages, because it depends only on valence-base):
  - With `--no-scripts`, identical to apk (927 entries).
  - With scripts, valence-desktop's pre-install created the sddm user and its post-install's `chown sddm:sddm` worked.
- **Owners after pre-install scripts** (dbus and polkit): `dbus-daemon-launch-helper` ends as `04110 root:messagebus` (gid 102 in the root), and no file is left owned by 65534.
- **mandoc:** its `/usr/share/man` trigger builds `mandoc.db` when a package adds man pages, and its post-deinstall runs on del. This test caught a trigger bug, fixed in 9a02b8f.
- **http repository**, served by a local `python3 -m http.server` on 127.0.0.1 reading the repo: 41 downloads, and the result is identical to a local install.
- **Damage is refused with nothing installed:**
  - a truncated download fails with "size differs from the index";
  - one flipped content byte fails with "content does not match its sha256 hash";
  - `!busybox`, `busybox<1.0` and unknown names are refused with clear messages.
- **Full-screen view:** checked on a pseudo-terminal with `script(1)`; it enters and leaves the alternate screen and the frames render.

### Not done or not right yet
- **Resolver:**
  - Greedy, with no backtracking: two requirements pinning different versions of one name fail with a message.
  - No `@tag`s, no pinning, `replaces` is not used while resolving, and checksum dependencies (`><`) are refused.
  - A rebuilt package with the same name and version but a different identity counts as installed, and there is no reinstall or "fix" command.
- **Network:** https is not tested against a live server, only http locally. Remote indexes are always downloaded again, and the cache is never cleaned.
- **Scripts:**
  - Nothing is mounted, so there is no /proc. busybox works anyway because it is called with an absolute path.
  - Scripts that need /proc were not in the tested sets: eudev's `udevadm hwdb`, the `depmod` calls of linux-valence and nvidia-open, `fc-cache`.
  - The task said "chroot ROOT /bin/sh"; vpm executes the script file directly after chroot, as apk does. For `#!/bin/sh` scripts this is the same.
- **Durability:** only process kills were tested (11 points), not power loss. syncfs is used at the steps described above.
- **File ownership** between packages is by literal path. Two packages reaching the same file through a symlinked directory (`bin/x` and `usr/bin/x`) are not detected as a conflict; removal is protected by canonical-path and inode checks. The real data has 0 shared paths.
- **`etc/` rule:**
  - It applies to every regular file under `etc/`; apk's `protected_paths.d` is not read.
  - An admin-changed `etc/` file of a removed package is kept, without a `.vpm-new`.
- **Hard links:** an in-place edit of a file outside `etc/` also changes its Programs copy, because they are the same inode. This is by design; fuse3's post-install `chmod` behaves the same way.
- **Directories:** existing ones keep their mode and owner, while apk sometimes updates them.
- **Untested code paths:** xattrs and device nodes inside packages are implemented, but the real data has none.
- **Non-root use** skips chown and is untested.
- **Progress:** there is no Ctrl-C handler, so a killed full-screen view leaves the terminal on the alternate screen. Only fetch/unpack is full-screen; the switch prints plain lines.
- **Commands not written:** no `search`, `info` or `--simulate`.

The test roots are left under `/root/vpm-test`: `base`, `base-noscripts`, `apk-base`, `crash-*`, `big-*`, `http*`, `bad*`, `repo2`, `keys`, `build`, `explore`, `pty` and the logs. Nothing is mounted there.
