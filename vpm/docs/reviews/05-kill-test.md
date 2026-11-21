## vpm kill-safety test: built and run, 0 kill-safety failures in vpm

I built the kill test and ran it. Across 242 rounds (three runs, 212 of them killed mid-upgrade), no kill left a root that broke the invariants, and every next `vpm upgrade` finished the job. The test did turn up one bug that has nothing to do with killing: `vpm upgrade` skipped 6 of the 27 packages. I fixed that in the resolver (not committed).

### Results
**Final run** (90 rounds, seed 1795277545, 181 s): exit 0, all rounds PASS.
- 86 of the 90 runs were killed while running:

  | where the kill landed | rounds |
  |---|---|
  | during the switch | 68 |
  | before the journal was written (fetch/unpack/syncfs) | 7 |
  | after the journal was deleted (old versions being deleted) | 11 |
  | not killed (upgrade had already finished) | 4 |

- 28 kills caught a package with some files placed and others not, for example glibc with 220 old / 78 new files, coreutils 84/19, util-linux, shadow, iproute2, procps-ng, gzip, ncurses-libs.
- 10 "double" rounds also killed the recovering run; 8 of those kills landed.
- 9 "script" rounds killed a package script while it ran. The recovery ran it again: in round 26 the kill hit util-linux's post-upgrade, and the next run executed it again.
- 6 kills caught a `.vpm.*` temporary file in flight; the next run always cleaned it up.

**Earlier runs:** 72 rounds (48 killed) and 80 rounds (78 killed, 31 with a package half-placed), all PASS.

**The checks themselves were tested:**
- **Damaged-root check (runs at the start of each test run):** a root damaged six ways must fail. Those are a copied file, a truncated file, a missing libc, a stray file, a changed etc file and a broken /bin/sh. It reports 13 violations plus the chroot failures.
- **Deliberately broken vpm:** a copy built from separate sources (`vpm-kt-mutant.sh`) places files by unlink-then-link instead of temporary + rename. The test caught it in 2 of 16 rounds, with `missing: /usr/lib/gconv/GREEK7.so` and `missing: /usr/bin/xargs`. So the test does detect files missing mid-switch, though not in every round.

### Finding 1 (fixed, not committed): `vpm upgrade` skips packages reached only through `provides`
- **Symptom:** zlib, pcre2, ncurses-libs, libacl, libattr and libmnl stayed at their old versions although 9999-r0 builds were available. These packages are only reached through `so:` dependencies.
- **Cause:** `src/solver.rs:185` (`Solver::better`). When two candidates provide the same thing at the same version, the check `if ca.installed != cb.installed { return ca.installed; }` runs before the package versions are compared. So in Upgrade mode the installed build always wins. A security rebuild such as zlib 1.3.1-r1 → r2 would never be picked up.
- **Fix applied:** prefer the installed candidate only between *different* package names, so the installed provider is kept. Two builds of the same package fall through to the version comparison.
- **Test added:** `upgrade_through_provides` in the solver's tests checks both Upgrade and Keep mode.
- **Checked after the fix:** all 27 packages upgrade; the build has no warnings; 32 unit tests and 8 real-data tests pass; `/root/vpm-target/release/vpm` is rebuilt with the fix.
- **To do:** `src/solver.rs` is modified but not committed. HEAD is still 9a02b8f; nothing was pushed.

### Other observations (not failures)
1. **A running script outlives a kill of the vpm process alone.** The test kills vpm's whole process group, as a crash would. With only `kill -9 PID`, a running script keeps running as an orphan. The lock is released anyway, because Rust opens it close-on-exec (`src/db.rs:483`), so the next vpm can recover and run the same script at the same time. I did not test this case.
   - Proposed fix: make the lock descriptor inheritable by scripts in `run_script` (`src/commit.rs:458`), or run scripts in their own process group and wait for or kill leftovers on recovery.
2. **Scripts can run twice.** A script killed mid-run, or before its "done" mark, runs again on recovery. That is how the roll-forward design works, but package scripts must tolerate it.
3. **syncfs dominates the switch on WSL.** The same 27-package upgrade took 0.23–2.9 s in total; the switch alone took 0.16–2.84 s. Placing the files takes only a few ms. This matters for the speed comparison with apk, which does not sync.
4. **The `VPM_CRASH_AT` hook calls `abort()`** (`src/commit.rs:200`, `:219`, `src/engine.rs:382`). On this WSL that goes through the crash handler `/wsl-capture-crash`, so I did not use it. Raising SIGKILL on itself would be a cleaner test hook.
5. **Not tested:** power loss (only process kills) and scripts that need `/proc`.

### How the test works
- **The upgrade:** a fresh root gets valence-base from the real repository (39 packages). The "admin" edits `etc/fstab`. Then `vpm upgrade` moves 27 of the packages to 9999-r0 builds, signed with the existing throwaway key in `/root/vpm-test/keys`.
- **What changes in the 9999-r0 builds:**
  - glibc, busybox, bash, coreutils, util-linux, shadow, ncurses-libs and others are rebuilt;
  - etc files are changed, a file is removed and one added in a new directory;
  - `gzip`'s `zcmp` turns from a file into a symlink;
  - the `lib64` symlink (the dynamic loader's path) moves from valence-layout to glibc;
  - pre- and post-upgrade scripts were added, plus the existing busybox trigger.
- **The kill:** SIGKILL to the whole process group, in one of nine modes in rotation:
  - a random time across the whole upgrade (median clean duration);
  - the moment a random file of a random package is replaced;
  - the same with vpm throttled from outside (SIGSTOP/SIGCONT, so the kill lands within about 30 µs of that file);
  - a random time within the switch;
  - right after the K-th journal mark;
  - during a package script;
  - during deletion of the old versions;
  - "double", which also kills the recovering run.

  vpm has no option to slow the switch, and the throttling changes nothing inside vpm.
- **Checks after each kill:**
  - every path is the old or the new version: same inode as before, or the new Programs copy's inode; content for `etc/`; target for symlinks; owner and mode match;
  - nothing that exists before and after is missing, and nothing unexpected appears;
  - every `Current` link points at a complete version;
  - under chroot: `/bin/sh -c 'echo ok'`, `/usr/lib/libc.so.6`, `ld.so --version`, `ld.so --list /usr/bin/ls`, `ls /` and bash all work.
- **Completion:** `vpm upgrade` must succeed and say it is finishing the interrupted switch when a journal was left; a further run must say "nothing to do". The root must then equal a clean, uninterrupted upgrade entry for entry: type, mode, owner, size, mtime, content and link targets, plus owners, Programs versions and metadata, world and hard links. No journal, temporary or `.partial` may be left.

### Running it
`bash /mnt/c/Users/a/AppData/Local/Temp/claude/39afd7d0aa98685a/9f48dd28-e365-4eae-aba4-ec797468f31d/scratchpad/vpm-killtest.sh [ROUNDS]` as root in WSL. The default is 80 rounds, about 3 minutes; exit 0 means all rounds passed.
- It builds the 9999 repository itself if it is missing.
- Environment variables: `KT_SEED` (fixed seed), `KT_KEEP` (how many failed roots to keep), `KT_VPM` (another binary), `KT_DIR` (another work directory), `KT_MODES` (which kill modes).
- It writes only under `/root/vpm-test`. No mounts were made; `/proc/mounts` showed nothing under `/root/vpm-test` at the end.

### Left in /root/vpm-test
- `kt/` holds the reference root, `results.txt` and `all-rounds.log` from the final run.
- `kt-full1.txt`, `kt-full2.txt` and `kt-full3.txt` are the three run logs.
- `kt-repo/` and `kt-build/` hold the 9999 repository and its build tree.
- `kt-mutant/` holds the broken-vpm run, with one failed root kept as `fail-4`.
- `mut-src/` and `mut-target/` hold the broken copy's sources and build.

Files are in C:\Users\a\AppData\Local\Temp\claude\39afd7d0aa98685a\9f48dd28-e365-4eae-aba4-ec797468f31d\scratchpad:
- vpm-killtest.sh
- vpm-kt-mkrepo.sh
- vpm-kt-pkgargs.py
- vpm-kt-check.py
- vpm-kt-kill.py
- vpm-kt-mutant.sh
- vpm\src\solver.rs