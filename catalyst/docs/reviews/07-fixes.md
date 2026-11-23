## vpm review fixes: every real finding fixed or explicitly left; all tests pass; vpm now installs about 1.6–2.2× faster than apk

I checked each finding against the code before changing anything. None of the four reports' claims turned out to be wrong; a few are left on purpose, listed below. Everything is committed locally as **c3ce6fc** on top of 9a02b8f. Nothing was pushed. `/root/valence` was only read, and nothing is mounted under `/root/vpm-test`.

The kill test caught one bug that my own changes introduced, and it is fixed. My first version of the symlink-safe directory creation (F6) did `mkdir` with mode 0700 and set owner and mode afterwards. A kill in between left a 0700 directory, which recovery keeps (round 22 of the first run). Directories are now created under a temporary name with their final owner and mode, then renamed into place.

### Fixed: security
- **F1 (memory before any trust check):**
  - ADB block limit by schema: 16 MiB for packages (the largest real one is 1.3 MB), 64 MiB for indexes.
  - At most 8 SIG blocks. The zstd window limit is set explicitly to 2^27; that is the same as the default, so this one changes nothing.
  - When the index gives an identity, it is compared right after the ADB block, before any SIG block is read.
  - Result: `sigbomb-8192` now fails at 8.4 MB RSS (was 1.08 GB), the ADB bomb is refused at 7 MB, and the package SIG-bomb at 9 MB.
- **F2 (replayed old index):**
  - For each repository, `var/lib/vpm/indexes/<sha256>` keeps the newest build-time and the newest version of every package name.
  - An older index is refused unless the new `--allow-rollback` is given.
  - A newly downloaded index goes to `.new`, is verified and checked, and only then replaces the cached copy.
  - vpm warns when an installed package is newer than anything the repositories offer.
  - The reviewer's H1 replay is now refused: "hello 1.0-r0 < 2.0-r0".
  - Not fixed: a fresh root that has never seen the repository still accepts an old index. Closing that needs a signed timestamp in the index.
- **F3 (unbounded HTTP downloads):**
  - A package stops at the index's `file-size`; a `Content-Length` that differs is refused before any data is read.
  - An index stops at 64 MiB; the reviewer's 512 MiB junk index is refused at once and the cached index is used.
  - A transfer slower than 1 KiB/s over 30 s is abandoned. A trickling mirror now gives up after 30 s (was stuck past 75 s).
  - An https repository uses an `https_only` agent, so no redirect to http is followed. This was not tested live (no local TLS server).
  - Leftover `.part` and `.new` files are removed at start.
  - My extra tests X1–X3 cover a mirror that sends no `Content-Length`, a trickling mirror, and stale `.part` files.
- **F4 (vpm's own directories reachable from packages):**
  - No package entry may be, or resolve into, `Programs`, `var/lib/vpm` or `var/cache/vpm`. This is checked on the literal path at unpack and on the resolved path when the switch places it.
  - `Programs`, `Programs/<name>` and vpm's state directories are checked to be real directories before anything is created or deleted in them.
  - Versions must be valid apk versions.
  - A corrupt `Programs` entry is skipped with a warning instead of making every command fail.
  - The reviewer's tests C3, C4, C5 and C7 are now all refused, and the host canaries survive.
- **F5 (stale directory cache after scripts):** the cache is cleared after every script and after the `/dev` node setup. In C6 the file now stays inside the root (the switch reports an error, nothing is written on the host).
- **F6 (symlinks followed on live paths):**
  - Live files are hashed, chowned and chmodded through `O_NOFOLLOW|O_NONBLOCK` descriptors, and only regular files are read.
  - Directories are made atomically as described above.
  - The script directory is checked and set through a descriptor.
- **F7 (host paths read for root files):** keys, the repositories file, `repositories.d/*.list`, `etc/apk/arch`, `passwd` and `group` are now resolved inside the root and read only as bounded regular files.
- **F8 (old set-id binaries reachable):** an old version's directory is made 0700 once its package has been switched.
- **F9 (info):** only the arch check is done: the package's arch must now equal the index entry's.

### Fixed: kill safety
- **Finding 1, the resolver skipping 6 packages:** fixed as part of the new provider order (R1). The installed build is preferred only between different packages. The test `upgrade_through_provides` is kept, and all 27 packages now upgrade.
- **Observation 1:** package scripts inherit the lock descriptor. A script that outlives a killed vpm keeps the root locked, so a second vpm cannot run the same recovery beside it.
- **Observation 4:** `VPM_CRASH_AT` now ends the process with SIGKILL instead of `abort()`. In e2e (c) the exit code is now 137.
- **Journal marks:** scripts skipped under `--no-scripts` are marked without an fsync.

### Fixed: spec conformance
- **R1:** apk's `compare_providers` order, without the own-name rule.
- **R2:** an unversioned provider with no priority is not auto-selected. vpm now gives apk's "please select one of the 'provided by' packages explicitly".
- **R3:** a versioned name provided by two chosen packages is refused.
- **R4:** active conflicts exclude candidates while choosing, not only at the end.
- **R5:** a constraint that an earlier choice made impossible is learned, and resolution restarts with it.
- **R6:** install_if tries every version of a name, best first.
- **R7:** `add` keeps an installed name's version.
- **R8:** the world is taken in name order, and install_if packages come right after the packages that trigger them.
- **R10–R12:** provides that repeat the package's own name are registered; provider_priority is truncated to 16 bits; every line of `etc/apk/arch` is used.
- **R14:** dependency text is validated (a bad version is refused, `name@tag` is refused).
- **P1:** base-URL repositories find packages at `${arch}/${name}-${version}.apk`.
- **P2–P5:** the `.adb` test is done on the raw word, `${var:N}` truncates bytes and `${var:}` gives nothing, and `repositories.d/*.list` is read.
- **A1:** undecodable index entries are skipped with a warning; origin, arch and tags are read lossily.
- **A3:** NULL slots are counted in DATA indices.
- **A10:** dependencies are stored losslessly in `.vpm/info` (a `KEY-raw` form when the text form would not read back the same).
- **S1:** scripts get `argv[0]` relative to the root.
- **F1 (passwd):** the last entry wins.
- **T-1:** the root directory counts for triggers.

### Fixed: speed
- **Speedup 1:** a helper thread calls syncfs while the packages unpack, so the sync before the journal has little left.
- **Speedup 2:** new names are linked or symlinked directly onto their path. Symlinks are made this way only when no owner change is needed.

### Left, and why
- **Rollback for fresh roots, and a signed timestamp in the index:** this needs a change in Valence's builder.
- **Doing all live-tree work through `openat2(RESOLVE_IN_ROOT)` and `*at()` calls:** a large rewrite. The race it closes needs another process changing the tree during the switch.
- **SHA-1 and `hash_alg` 3/5 acceptance:** kept for apk compatibility.
- **The zstd window:** a lower limit than 2^27 could reject packages built at zstd levels 20–22.
- **A repair command.**
- **R9 (packages keyed by name and version rather than identity):** a design change.
- **P2, keyword-less base URL:** apk would treat it as a v2 repository; vpm reads only v3, so it is treated as v3 on purpose.
- **Leniency with `#` and CR in the repositories file:** left as is.
- **S4 (scripts of packages with installed-size 0):** only hand-made packages have that.
- **Negligible items:** the 128K−4 SIG payload limit, the u8 cut of the dependency match value, and the hard-link entry's own ACL.
- **Speedup 3 (zstd packages):** a repository-side change. Parallel placing and keeping file records in memory are not simple changes.
- **Scripts running twice after a kill:** this is how roll-forward works (kill observation 2).
- **Power loss and scripts that need `/proc`:** not tested.

### Final test results
- **`cargo test --release`:** 37 unit tests and 8 real-data tests pass, with no warnings. New tests cover apk's answers on the synthetic cases, add-of-installed, rollback refusal, package URLs, the repositories file, and lossless `.vpm/info`.
- **End-to-end tests:**
  - (a): the trees from vpm and apk (both `--no-scripts`) are identical, 799 entries.
  - (b): after `add nano rsync` and `del rsync`, identical again, 848 entries.
  - (c): the upgrade plus all 11 crash points give final checks exit 0, with 0 paths that are neither old nor new.
- **Kill test:**
  - The first run hit the regression above and the checker's new-state-directory issue. I updated `vpm-kt-check.py` to accept the `indexes` directory, which may contain only SHA-256-named files.
  - After the fix: 80 + 150 + 80 rounds, **310 rounds with 0 failures**. 298 of them killed vpm while it ran, and 82 caught a package half placed.
- **Security scripts 1–4, rerun, plus my extra tests X1–X3:** every Medium finding is now refused, and every check that held before still holds.
- **Resolution:**
  - The spec review's synthetic cases, now run through the real `vpm` command line: all 12, plus T9 (add-of-installed) and T11 (base-URL repository), match apk.
  - The 25 real-repository requests: the same package set for all 23 that resolve, and the same order in 22 (was 20). "nano docs" still differs at position 6.

### Benchmark (`vpm-bench.sh`, 5 runs, medians; machine shared, load 3–11)

| set | cache | scripts | apk | vpm | vpm/apk | apk +sync | vpm +sync |
|---|---|---|---|---|---|---|---|
| base | cold | no | 0.385 s | 0.225 s | 0.59 | 0.466 | 0.231 |
| base | cold | yes | 0.396 s* | 0.246 s | 0.62 | 0.464 | 0.255 |
| base | warm | no | 0.283 s | 0.153 s | 0.54 | 0.357 | 0.157 |
| base | warm | yes | 0.300 s* | 0.177 s | 0.59 | 0.358 | 0.183 |
| big | cold | no | 12.42 s | 6.45 s | 0.52 | 13.64 | 6.46 |
| big | warm | no | 11.38 s | 5.14 s | 0.45 | 13.03 | 5.15 |

\* apk exits 1 when scripts run (bash's post-install).

Against the reviewed commit 9a02b8f, in the same session (warm, `--no-scripts`):
- base: 0.377 s → 0.190 s (−50%);
- big: 7.28 s → 6.08 s (−16%).

On big, the flusher makes the unpack phase slower (about 1.3 s → 3–4.6 s), but the switch drops from about 5.7 s to 1.4 s. Max RSS on big rose from 90–107 MB to 108–127 MB. xbps was not run; it needs approval for the downloads.

### Files in the crate (`…\scratchpad\vpm`)
- `Cargo.toml` (adds `libc`), `Cargo.lock`, `README.md` (new), `.gitignore`
- `src/`: `lib.rs`, `main.rs`, `engine.rs`, `commit.rs`, `db.rs`, `unpack.rs`, `fetch.rs`, `repo.rs`, `solver.rs`, `rootfs.rs`, `sys.rs`, `verify.rs`, `version.rs`, `digest.rs`, `dump.rs`, `error.rs`, `progress.rs`
- `src/adb/`: `mod.rs`, `container.rs`, `value.rs`, `schema.rs`, `package.rs`
- `tests/real_data.rs`, `tools/compare-with-apk.py`

The README covers what vpm is, the commands, the layout, the transaction and recovery, trust and limits, resolution, what stage 1 does not do yet, and the measured numbers.

New scripts in `C:\Users\a\AppData\Local\Temp\claude\39afd7d0aa98685a\9f48dd28-e365-4eae-aba4-ec797468f31d\scratchpad\`, all named `vpm-fx-*`:
- `vpm-fx-build.sh`
- `vpm-fx-sec.sh`
- `vpm-fx-http.sh`, `vpm-fx-httpd.py`
- `vpm-fx-e2e.sh`
- `vpm-fx-synth.sh`
- `vpm-fx-real20.sh`
- `vpm-fx-bench.sh`
- `vpm-fx-ktfail.sh`
- `vpm-fx-commit.sh`, `vpm-fx-status.sh`, `vpm-fx-gitinfo.sh`, `vpm-fx-look.sh`, `vpm-fx-adbsize.py` (helpers)

Updated: `vpm-kt-check.py` (accepts the `var/lib/vpm/indexes` directory).

Results in `/root/vpm-test/`:
- `bench-fx/` (`summary.md`, `results.tsv`, `full.txt`, `before-after.tsv`)
- `kt-fx1.txt` to `kt-fx4.txt`, `kt-fx3-results.txt`
- `sec-fx-log-1.txt` to `sec-fx-log-5.txt`
- `log-a.txt`, `log-b.txt`, `log-c.txt`
- `cr/vpm20-fx.txt`