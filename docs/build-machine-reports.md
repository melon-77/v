# Build-machine reports

The Claude Code session on the owner's build machine (the WSL2 host with KVM and the signing key) writes these reports for the developers and their Gemini agent. The newest report is first.

How this works:

- Put requests in `docs/build-machine-handoff.md`, as you do now. The handoff calls its reader "the Gemini agent running on the build machine"; the build machine actually runs Claude, and Gemini works on valence-dev. The requests reach the build machine either way.
- The owner approves each test pass. I run it and write the results here, then push to `testing`.
- I don't change your code (`vpm/`, `recipes/vpm`, your scripts) unless the owner asks. The findings below suggest fixes; making them is up to you.

---

## 2026-11-21: phase 10 stage 2, vpm-aware installers

Tree: `testing` 01369fa, `recipes` (vpkg `main`) 1f2af04. Line numbers below refer to these commits.

### Summary

- **apk mode** (the default): everything passes. That covers valence-base r48, calamares-valence r14 and vpm r2, the 8 console tests, the wish test and the 3 desktop tests.
- **vpm's own tests** (handoff section 2): they pass, with 38 unit, 5 CLI and 8 real-data tests. The handoff expects 4 CLI tests; there are now 5.
- **The vpm-mode console ISO** (handoff section 6, step 4):
  - It builds, and valence-install from it installs a system that boots on BIOS, UEFI and LUKS.
  - Five tests fail, because `qemu-test.py` drives apk in the installed system (finding I-5).
  - That means vpm's snapshot-before-change was not exercised on an installed system.
- **One high finding:** vpm cannot place the kernel files on a separate FAT32 `/boot` (I-1). I reproduced it on a real file system.
- **Not run yet:**
  - handoff section 3 (Cataclysm: DDA tiles and sound)
  - section 4 (Firefox)
  - section 5 (CLI essentials)
  - the full "everything" build
  - the kill test and the benchmark

  These wait for the owner's go-ahead.

### What ran

| Step | Result |
|---|---|
| `valence-build valence-base calamares-valence vpm` | OK: built 3, up to date 117, failed 0 |
| `cargo test --release` with `VPM_REPO`, `VPM_KEYS` (section 2.2) | OK: 38 + 5 + 8 passed |
| `vpm search -x cataclysm-dda` (section 2.3) | OK: `cataclysm-dda-0.9.1-r3` |
| `vpm --root /tmp/vpm-smoke --initdb --simulate add valence-base` (section 2.3) | OK: 39 to install, "OK: simulated", no root directory created |
| `mkiso.sh`, then `VALENCE_EDITION=desktop mkiso.sh` | OK |
| `ALL=1 test-console.sh` (apk ISO) | all 8 PASS |
| `qemu-test.py wish` (apk ISO) | OK |
| `test-desktop.sh` | all 3 PASS |
| `cargo build --release`, then `VALENCE_PKG=vpm mkiso.sh` | OK: the ISO has `/Programs` and `var/lib/vpm/world` |
| `ALL=1 test-console.sh` (vpm ISO) | see below |

Results on the vpm ISO:

| Test | Result | What failed |
|---|---|---|
| live-bios | PASS | |
| disk-bios | FAIL | the apk steps |
| disk-uefi | FAIL | the apk steps |
| rollback-bios | FAIL | apk-based state |
| live-luks | PASS | |
| disk-luks | FAIL | the apk steps |
| rollback-luks | FAIL | apk-based state |
| dualboot | PASS | |

Up to the apk steps, the disk tests pass: the system boots from GRUB on the installed disk, logs in, has its GNU tools and the Btrfs subvolumes with the swap file, and has no documentation installed. Then:

```
[   22.7s] apk add less-doc from a repository over the network, man less, apropos less: FAILED
[   23.3s] apk add docs: 0 -doc packages came with it, man finds 0 of 8 pages: FAILED
[   24.1s] apk took a snapshot before each change (0 -> 0), the boot menu lists 0: FAILED
ERROR: Failed to open apk database: No such file or directory
ERROR: Unable to lock database: No such file or directory
```

The rollback tests show the same thing:

```
before: {'LESS': 'less_710', 'LESSPKG': '', 'RUNIT': '', ... 'ROOT': '@', ...}
[   14.7s] the system runs from the subvolume @ and its state can be read: FAILED
```

The vpm ISO took over `out/valence-20261121-x86_64.iso` and deleted the apk one (finding I-6). I rebuilt the apk console ISO afterwards, so `out/` holds the normal one again.

### How the findings were checked

The findings come from a review of your diff (01369fa and vpkg 1f2af04 against the previous state) by several review agents. A second pass checked each finding against:

- the code at the commits above
- for comparisons, the apk-tools 3.0.8, Calamares 3.3.14 and squashfs-tools 4.7.5 sources
- for I-1, a reproduction on the build machine

Findings that did not hold are listed at the end so you don't chase them.

### Installers, scripts and tests

**I-1 (high): the kernel files in `/boot` are hard links into `/Programs`, but `/boot` is a separate FAT32 partition**

- **Cause:**
  - `recipes/linux-valence/VALENCEBUILD:32-34` installs `boot/vmlinuz-valence`, `boot/config-valence` and `boot/System.map-valence`.
  - vpm places every file with `place_link` (`vpm/src/commit.rs:241-272`, reached from `commit.rs:395`), which is a plain `fs::hard_link`.
  - There is no EXDEV fallback and nothing special for `/boot`.
  - README's "/Programs and /usr are one file system" holds, but `/boot` is not on that file system.
- **Effect 1, Calamares from a desktop ISO built with `VALENCE_PKG=vpm`:** this is from the sources, not run.
  - mkiso allows this combination, and `mksquashfs` keeps hard links.
  - `partition.conf:28-32` puts `/boot` on FAT32. Calamares runs `unsquashfs -i -f -d DEST SRC` (UnsquashRunner.cpp:74-79) with no `-ignore-errors`.
  - In squashfs-tools 4.7.5 a failed `link()` is fatal without that flag, so the unpack job aborts.
- **Effect 2, a system installed by valence-install, at the first kernel upgrade through vpm:**
  - `valence-install:197-198` writes `/boot` as plain copies, so the install itself works.
  - At the next upgrade, `place_link` gets EXDEV. `commit.rs:762-764` records the error and carries on.
  - `remove_old` (`commit.rs:767-772`) deletes the old version's files, including `usr/lib/modules/<old release>/`.
  - `set_current` (`commit.rs:773`) flips `Current`, and vpm exits 1.
  - `/boot` keeps the old kernel, whose modules are gone, so the next boot has no modules. The pre-commit snapshot is the way back.
- **Effect 3:** `vpm fix` fails on every system installed to disk (reproduced below).
- **Reproduction 1:** linux-valence is installed into a fresh vpm root. `/boot` is then turned into a separate FAT32 file system holding plain copies, the way valence-install does it. The output of `vpm fix linux-valence`, the same on every run:

  ```
  ERRORS: checked 601 entries (598 ok, 0 repaired, 3 errors)
  vpm: linux-valence-7.0.0-r6: /boot/System.map-valence: Invalid cross-device link (os error 18)
  vpm: linux-valence-7.0.0-r6: /boot/config-valence: Invalid cross-device link (os error 18)
  vpm: linux-valence-7.0.0-r6: /boot/vmlinuz-valence: Invalid cross-device link (os error 18)
  vpm fix exit: 1
  ```

- **Reproduction 2:** `/boot` is a FAT32 file system before `vpm add linux-valence`. This is the switch's own path, the same one an upgrade takes:

  ```
  ERRORS: 9 packages installed (9 new, 0 upgraded, 0 removed); 1648 files placed, 0 removed; ...
  vpm: 3 errors during the switch:
    linux-valence-7.0.0-r6: /boot/System.map-valence: Invalid cross-device link (os error 18)
    ...
  vpm add exit: 1
  afterwards: /boot is empty; Programs/linux-valence/Current -> 7.0.0-r6; usr/lib/modules/7.0.0-valence is there;
              `vpm list` shows linux-valence-7.0.0-r6 [world]
  ```

  The package counts as installed, but the kernel never reached `/boot`.
- **Suggested fix:**
  - Place an entry as a copy when its target directory is on another device than `/Programs`: write it to a temporary name in the target directory, fsync, then rename. Doing this for `boot/` only would also work; "on EXDEV, copy instead" is the general version.
  - In `place_link`, `remove_old` and `repair`, treat "same content hash, other device" as intact.
  - Say in the README that `/boot` is the exception.

**I-2 (medium): `valence-wish --root` under vpm searches only the ISO's extra packages**

- **Where:** `recipes/valence-base/files/usr/bin/valence-wish:100-113`.
  - Line 111 adds the installer's copy with `--repository`.
  - `vpm/src/engine.rs:173` reads the repositories file only when no `--repository` is given.
  - apk's `-X` adds to the file's list instead (database.c:2162-2180).
  - So during an install, vpm searches only the ISO's extras (`mkiso.sh:138-142`).
- **Scenario:** with `packages = htop, nano`:
  - `search -x` (line 132) finds nothing, and `flathub_id` returns 1 under `--root` (line 124).
  - valence-wish dies at line 144, before it sets the hostname, services and groups.
  - `valence-install:314` then prints "not applied completely".
  - A package the image already has, but that is not among the extras, fails the same way, because the search runs before `info -e`.
  - `qemu-test.py wish` would fail on a vpm ISO: it takes nano from the online repository (line 648).
- **Comment:** the comment at lines 107-108 says vpm reads the target's repositories file. With `--repository` it doesn't.
- **Suggested fix:** give vpm a way to add a repository to the file's list, as `-X` does, and use it here. Or pass the target's repository lines explicitly as well.
  - `vpm/src/repo.rs:376` opens local paths from the repositories file as host paths, not under `--root`. If valence-wish passes the target's lines itself, local paths need `$ROOT` in front.
  - Offline installs also need I-3 fixed.

**I-3 (medium): one unreachable repository makes `add`, `upgrade`, `update` and `search` fail**

- **Where:** `vpm/src/engine.rs:186-188` (`repo::load(...)?`).
  - `repo.rs:368-369` returns an error for a remote index with no cached copy.
  - `repo.rs:376` returns an error for a missing local index.
  - `del`, `list`, `info -e` and `fix` load no repositories, so they are not affected. Once an index is cached, an offline run uses the cache with a warning.
- **Scenario:**
  - A freshly installed system never has a cached index, because `mkiso.sh:63` empties `var/cache/vpm`.
  - Offline, `vpm add valence-vm-guest` fails on the installed system.
  - valence-wish ends in "no package of this name … is the network up?" because it discards the search's stderr.
  - The comment at valence-wish:112 ("one that cannot be reached is left out, with a warning") is not true for vpm.
- **Related (low):** `repo::load` downloads a remote index on every call and never reuses a recent cached copy. That is one download per wish package (the loop at line 132), plus the ones for `update` and `add`.
- **Suggested fix:**
  - Warn and skip the repository.
  - Stay fatal for `Untrusted`/`Integrity` errors, and when no repository loads at all.
  - Optionally reuse a cached index that is only minutes old.

**I-4 (medium): `valence-first-boot` still uses apk**

- **Where:** `recipes/valence-desktop/files/usr/bin/valence-first-boot:63, 70, 82, 99`, with messages at lines 73 and 102.
- **Why it fails:** a vpm root has no `etc/apk/world` and no `lib/apk/db/installed`, so apk stops with "Unable to read database".
- **Effect:** `apk info -e` is non-zero whatever vpm installed. So at each user's first login (or with `--again`), the developer tools, and the NVIDIA driver where it applies, are offered as missing. Then `doas apk add` fails, and the message blames "no internet?".
- **Suggested fix:** use valence-wish's detection: `var/lib/vpm/world` is present and `vpm` is on the PATH (valence-wish:100-105).
- **Cosmetic:** these messages still say apk under vpm: valence-wish:145 and 193, valence-snapshot:47 and 191, valence-install:8. valence-snapshot's list says "No snapshots yet: apk takes one before every change to the installed packages."

**I-5 (medium): the handoff's "QEMU-test a vpm-managed console ISO" cannot pass**

- **Where:** `scripts/qemu-test.py`.
  - Disk mode runs apk at lines 812-836 (`add less-doc`, `add docs`, `del docs`) and expects snapshots "before apk del docs less-doc" (line 842). It exits at 845-846.
  - Rollback mode reads `LESSPKG` with `apk list --installed` (line 481), checks it at 505-506, and updates with apk at 472-473.
  - Wish mode uses apk too.
  - Without `ALL=1`, `test-console.sh:26` stops after disk-bios.
- **Effect:** vpm's snapshot-before-change and rollback are never tested on an installed system.
- **Suggested fix:** have `qemu-test.py` check which package manager the guest has (`[ -e /var/lib/vpm/world ]`) and use `vpm add`, `vpm del` and `vpm list` there.
  - `vpm list` prints `name-version arch {origin} (license) [world]`. Like `apk list --installed`, the first field is the package, so `cut -d" " -f1` works for both.
  - The snapshot descriptions differ: match on the package names rather than on "apk".

**I-6 (low): a vpm-mode ISO overwrites the console ISO under the same name**

- **Where:** `scripts/mkiso.sh`.
  - Line 15 uses the same `OUTISO` in both modes.
  - Line 211 (`rm -f $M/out/valence-2*-$ISOARCH.iso`) deletes the apk ISO.
  - Line 213 recomputes `SHA256SUMS`.
  - A vpm desktop build overwrites `valence-desktop-*` the same way.
- **Risk:** it can get published. `scripts/release.sh:13` picks the newest `out/valence-2*-x86_64.iso`, so running handoff section 6 in order and then `release.sh` would release the vpm ISO. `test-console.sh:8` uses the same glob. It happened in this run.
- **Suggested fix:** in vpm mode, use a distinct name such as `valence-vpm-<date>-x86_64.iso`, and limit the `rm` to that name.

### vpm (engine and README)

**V-1 (medium): `vpm fix` and every upgrade overwrite changed symlinks under `etc/`, and can delete an admin's file**

- **Where:** `vpm/src/commit.rs:1131-1170` (the rename at 1157) in repair, and `place_symlink` (`commit.rs:274-313`) on every upgrade. place_symlink dates from stage 1.
- **Example:** ca-certificates ships `etc/ssl/cert.pem -> certs/ca-certificates.crt`. If an admin puts a regular file there, `vpm fix` renames a symlink over it. The file is gone, with no `.vpm-new` and no snapshot.
  - The repository has 29 symlink records under `etc/`, for example fontconfig's `etc/fonts/conf.d/*.conf`.
- **apk:** it keeps a changed entry under `etc/`, symlinks included (database.c:3073-3089), and writes the package's version as `.apk-new`. `apk fix` with no arguments only touches packages marked broken.
- **Suggested fix:** in both repair and `place_symlink`, when a symlink record is under `etc/` and something else is at the path, keep it and report it, or write `<path>.vpm-new`.

**V-2 (medium): the README says `vpm fix` skips corrupt Programs entries with a warning; it can't detect them**

- **Where:** README.md:286-287, `commit.rs:1224-1225`, `engine.rs:856`.
- **What happens:**
  - The check compares inodes and never the hash. Writing in place through the live hard link changes the Programs copy too, and fix says "ok".
  - A file missing from Programs is an error (`commit.rs:1177-1179`, `1217-1222`), and the run exits 1.
  - `PkgState::load(...)?` reads `.vpm/files`, which `Db::reload` never reads. So a package with a missing or garbled `.vpm/files` makes all of `vpm fix` fail, even `vpm fix otherpkg`.
  - The same `?` in transact (`engine.rs:462`) blocks `add`, `del` and `upgrade`.
- **Suggested fix:**
  - Either check hashes (always, or behind an option) or correct the README.
  - Load each package's state separately and report a bad one instead of aborting the whole run.

**V-3 (low-medium): `vpm fix` runs no commit hooks, so no snapshot is taken first**

- **Where:** `engine.rs:845-884`.
- **apk:** `apk fix` goes through `apk_solver_commit` and runs the pre-commit hooks (commit.c:476) before changing anything.
- **Suggested fix:** call `run_commit_hooks` around repair, and honour `--no-commit-hooks`.

**V-4 (low): post-commit hooks behave differently from apk**

- **vpm** (`engine.rs:494-501` and `401-407`, `commit.rs:1030-1031`):
  - A failing post-commit hook makes the run exit 1 and stops the later hooks.
  - Post-commit is skipped when fetch, unpack, the conflict check or the switch returns an error.
- **apk:** it checks the exit status of pre-commit hooks only (commit.c:300). It runs post-commit even after errors (commit.c:506-509).
- **No effect today:** Valence's hook exits at once unless it is pre-commit.
- **Not a bug:** vpm runs no hooks when there is nothing to do, where apk still runs pre-commit. vpm's way avoids empty snapshots.

**V-5 (low): other commit-hook differences**

- **Environment:** apk sets `APK_SCRIPT` (database.c:2493) next to `PATH`.
- **Non-executable files:** vpm skips them (`commit.rs:979`); apk tries to run them, and a failing pre-commit stops the change.
- **Order:** vpm's directory order is at `commit.rs:945-952`. In apk, the earlier directory wins and `etc/` comes first, so for the same name, vpm runs `usr/lib/vpm/X` where apk would run `etc/apk/X`.
- **Lock:**
  - `sys.rs:154-156` hands the lock fd to every child, hooks included. A hook that calls vpm stops with "another vpm is running on this root" (`db.rs:580-581`).
  - apk's lock fd is `O_CLOEXEC`.
  - Handing it to package scripts is deliberate (README:198-200); hooks don't need it.

**V-6 (low): `vpm search -x` matches package names only**

- **Where:** `engine.rs:617-619`; non-exact search ignores provides too (628-631).
- **apk:** it matches names and provides (app_search.c:127). `apk search -x pc:libacl` prints `acl-dev-2.3.2-r1`, where vpm finds nothing.
- **Effect:** only a wish item written as an `so:` or `pc:` name goes wrong (valence-wish:132).

**V-7 (low): `vpm update` reports OK without refreshing**

- **Where:** `repo.rs:358-372` falls back to the cache with a warning, and `engine.rs:597-609` always returns OK.
- **apk:** `update` returns the number of repositories that were unavailable or stale.
- See also I-3.

**V-8 (low): `vpm info -W` differs from apk**

- **Relative argument:** apk resolves it against the current directory (app_info.c:83); vpm treats it as a path from the root.
- **A path more than one package owns:** vpm prints the first by name (`engine.rs:747`), not the owner.
- **Directories:** vpm counts directory records, so `-W /usr/bin` names the first package alphabetically.
- **Symlinks:** apk retries once with the final component's symlink target (query.c:578-584); vpm does not.

**V-9 (low): `--simulate` is ignored by `update` and `cache clean`, and concurrent index downloads can corrupt each other**

- **`--simulate`:** ignored at `engine.rs:597-609` and `821-840`. apk honours it in cache clean and downloads no index under it.
- **Downloads:**
  - Two vpm processes download to the same `Packages.<key>.adb.new.part` name (create and truncate, in `fetch.rs`), so their bytes can interleave.
  - A corrupt index fails with `Untrusted` or `Integrity`. That does not fall back to the cache (`repo.rs:368`), so even a locked `upgrade` can abort.
  - Suggested fix: a unique temporary name per process.
- **`search`:** it uses only `abs_root` (`engine.rs:637`) and skips the `real_dir` checks.

**V-10 to V-13 (low; latent or defence in depth)**

- **V-10:** fix "repairs" a directory reached through a symlink on every run (`commit.rs:1084/1094` against `ensure_dir`'s `dir_canon`). No package triggers it today.
- **V-11:** missing parent directories come back as 0755 root:root (`commit.rs:1069-1075`; the switch does the same at 386). Only polkit and cups have restricted directories, and both list them before their files.
- **V-12:** restoring an `etc/` file follows symlinks inside Programs (`commit.rs:1176-1177`, `1191`; `copy_to` at 321 has the same pattern). Today no directory under `etc/` in Programs is writable by a user.
- **V-13:** `vpm --simulate fix` prints "OK:" and exits 1 when there are errors (`engine.rs:862-869`, `883`).

**V-14 (low): the README is out of date**

- Lines 3-5 still say stage 1.
- Lines 16-18 say only the repository list and the keys are used. vpm now also runs `commit_hooks.d` and reads `etc/apk/arch`.
- "The transaction" (lines 118-157) doesn't mention the hooks, and nothing documents their directories, order, environment or failure rules.
- `add -u` upgrades only the named packages. apk's `-u` also upgrades their dependencies (app_add.c:178-180). Document it or match apk.

### Checked and rejected

- **Fix putting back a deleted `etc/` symlink:** apk's reinstall restores it too, so this is not a symlink problem.
- **`vpm info -W /bin/sh` finding no owner:** apk 3.0.8 behaves the same ("Could not find owner package"). `/usr/bin/sh` works in both.
- **The README lists `fix` as not done yet:** it doesn't. It lists a limitation of `fix`, the false one in V-2.

### Next on the build machine

- Once fixes for **I-1** and **I-5** are on `testing`, ask in the handoff. With the owner's OK, I'll rerun the vpm ISO suite and add a report here.
- Still waiting for the owner's go-ahead: handoff sections 3, 4 and 5, the full build, the kill test and the benchmark.
