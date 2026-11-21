## vpm vs apk-tools benchmark: vpm is 1.6–2.1× faster on every install measured; xbps was not run

vpm installs `valence-base` and the big set 1.6–1.8× faster than apk, cold and warm (median wall time). Counting until the data is on disk, the gap grows to 1.8–2.4×, because vpm already syncs and apk does not. vpm uses 1.1–1.3× apk's CPU, spread over its threads.

One finding changes the picture: if the repository were compressed with zstd instead of deflate, apk would beat vpm on the small `valence-base` install (0.117 s against 0.148 s for vpm's best variant). vpm stays ahead on the big install (5.0 s against 6.3 s).

**xbps was not run.** That stage needs xbps-static and Void's packages downloaded from repo-default.voidlinux.org, and the downloaded binary run as root. That needs the user's approval, which a workflow task cannot give. The stage is written but untested; to run it once approved: `bash /root/vpm-test/bench/vpm-bench.sh --stages "" --xbps`. It checks the tarball against its published sha256, downloads base-system once into a local repository indexed with xbps-rindex, and times `xbps-install -r ROOT -C empty.d -R LOCALREPO -S -y -U base-system`. Not verified: whether the static tarball ships Void's keys, the name of the checksum file, and how `-y` handles the key import prompt. vpm/apk rows already have MB/s, so an xbps row can be compared per MB.

### How it was measured
- **Machine:** i5-12450H (12 threads), 24 GB RAM, WSL2 kernel 6.18, ext4 in a VHDX.
  - Cold read of the repository: 519 MB/s. Warm read: 9.6 GB/s. Writing 1 GB plus syncfs: 0.56 s.
- **Binaries:** vpm is commit 9a02b8f, built clean from `git archive HEAD` (sha256 1e3d902a…). The vpm working tree has another agent's uncommitted `src/solver.rs` change, which was not benchmarked. apk is apk-tools 3.0.8 (host tool).
- **Sets:**
  - base: `valence-base`, 39 packages, 60 MB.
  - big: valence-base plus the 30 packages with the largest installed size. All 30 installed together with no conflict under both tools: qemu clang-dev rust go gcc boost-headers linux-valence cataclysm-dda clang-libs nvidia-open-firmware llvm-libs llvm-dev linux-firmware-intel-wifi cmake linux-firmware-nvidia mesa-vulkan libstdc++-dev qt6-qtdeclarative clang python3-dev g++ breeze kf6-breeze-icons linux-firmware-atheros perl mesa-dri plasma-workspace github-cli qt6-qtbase icu-libs. With dependencies that is 244 packages, 3.39 GB, 80,315 files.
- **Runs:** 5 per configuration, apk and vpm alternating, starting tool switched every run.
  - Cold: `sync; echo 3 > /proc/sys/vm/drop_caches` before every run.
  - Warm: one unrecorded warm-up run per tool first.
  - apk uses `--no-cache`; vpm reads the local repository in place, so neither has a package cache.
- **"+sync"** is the command's wall time plus a `sync -f` of the target filesystem right after it.
- **Caveat: the machine was shared.** Other agents' work ran throughout (qemu-system-aarch64 at 35–106% CPU, vpm kill and security tests). Load average was 2.6–6. This mostly widens vpm's spread, because syncfs also flushes other processes' dirty data. Rerun on a quiet machine before quoting absolute numbers.

### Results: median (min–max), 5 runs each

| set | cache | scripts | apk wall | vpm wall | vpm/apk | apk +sync | vpm +sync | CPU apk / vpm | max RSS apk / vpm | MB/s apk / vpm |
|---|---|---|---|---|---|---|---|---|---|---|
| base | cold | no | 0.375 s (0.363–0.432) | 0.237 s (0.224–0.277) | 0.63 | 0.470 (0.442–0.677) | 0.244 (0.229–0.283) | 0.27 / 0.30 s | 10 / 22 MB | 161 / 255 |
| base | warm | no | 0.296 (0.292–0.418) | 0.174 (0.158–0.342) | 0.59 | 0.433 (0.348–0.501) | 0.184 (0.164–0.346) | 0.27 / 0.32 | 10 / 22 | 204 / 348 |
| base | cold | yes | 0.406 (0.390–0.453)* | 0.274 (0.249–0.363) | 0.67 | 0.464 | 0.284 | 0.30 / 0.36 | 10 / 22 | |
| base | warm | yes | 0.367 (0.319–0.415)* | 0.177 (0.171–0.350) | 0.48 | 0.447 | 0.181 | 0.30 / 0.33 | 10 / 21 | |
| big | cold | no | 12.91 s (12.53–13.67) | 7.49 s (7.24–7.75) | 0.58 | 13.39 (12.86–19.64) | 7.50 (7.25–7.76) | 11.3 / 13.5 s | 17 / 90 MB | 263 / 454 |
| big | warm | no | 10.77 (10.60–11.76) | 5.93 (5.84–6.18) | 0.55 | 12.62 (12.59–18.28) | 5.94 (5.85–6.19) | 10.6 / 14.0 | 17 / 107 | 315 / 573 |

\* apk exits with code 1 on every run with scripts: bash's post-install cannot find `/bin/sh` yet. vpm defers that script until the other packages are placed.

- **Installed trees:**
  - base: 60.40 MB with apk, 60.54 MB with vpm.
  - big: 3,393.9 MB with apk, 3,401.7 MB with vpm.
  - vpm's extra bytes are its `.vpm` metadata. du counts hard links once, but vpm has twice the file names (two names per file).

**vpm `--jobs` sweep** (warm, `--no-scripts`, 3 runs each):

| set | j1 | j2 | j4 | j6 | j12 |
|---|---|---|---|---|---|
| big | 13.07 s | 7.25 | 6.66 | 6.04 | 6.10 |
| base | 0.50 s | 0.43 | 0.29 | 0.35 | 0.35 |

The big set's unpack phase alone takes 8.8–9.7 s on one thread and 1.3–1.9 s on 12. The base sweep is noisy: its syncfs varied from 0.14 to 0.56 s under load.

### Where vpm's time goes
There is no perf or strace in WSL, and installing them is outside the rules. Instead I profiled an instrumented copy at `scratchpad\vpm-prof` (`VPM_PROF=1` prints step times and summed counters). The vpm crate itself was not touched.

**Base, warm, `--no-scripts` (about 180 ms):**
- **Start, keys, index load and verify, solve:** 6.5 ms (4%).
- **Fetch and unpack:** 60–66 ms (35%).
  - About 6.5 of 12 threads are busy on average.
  - The phase ends when the slowest package ends: glibc takes 59–66 ms and starts at 0 ms.
  - On one thread the unpack takes 209 ms, of which 154 ms (74%) is inflate plus SHA-256.
- **syncfs before the journal:** 65–72 ms (37%), flushing about 59 MB. Under the other agents' load it measured 126–155 ms.
- **Placing in the live tree:** 21 ms (12%) for 716 entries.
  - This includes 5–7 ms for 3 fsync'd journal marks: scripts skipped under `--no-scripts` are still marked with an fsync.
- **Journal, world and directory fsyncs, triggers, final syncfs:** about 15 ms.
- **With scripts:** 3 scripts add about 9 ms and 2 extra syncfs add 4–8 ms.

**Big, warm, `--no-scripts` (5.86 s in that run):**
- **Unpack:** 1.90 s (32%), 10.4 of 12 threads busy.
  - The slowest package is boost-headers: 1.84 s, lots of small files at 82 MB/s. qemu, by comparison, unpacks at 565 MB/s.
  - Thread time: inflate plus SHA-256 8.8 s (45%), file creation 4.4 s (22%), write 2.4 s, mkdir 1.85 s (6,207 directories), open and verify 0.4 s.
- **syncfs before the journal:** 1.74 s (30%). It measured 2.5–4.2 s in the loaded runs.
- **Placing:** 1.94 s (33%), on one thread, about 24 µs per entry.
  - ensure_dir/mkdir 0.70 s, hard link plus rename for 62k files 0.77 s, 18k symlinks 0.37 s.
- **Reading `.vpm/files` back:** 105 ms for the ownership check, then another 68 ms in the switch.
- **Cold:** unpack takes 2.6 s (its first reads go to disk), syncfs 2.6 s, placing 2.1 s.

### The 3 most valuable speedups (measured on the instrumented copy, warm, `--no-scripts`)

| set | baseline | flusher | direct-link | flusher + direct | zstd + flusher + direct | apk | apk + zstd |
|---|---|---|---|---|---|---|---|
| base (5 runs) | 0.191 s (0.179–0.195) | 0.159 | 0.187 | 0.160 | **0.148** | 0.281 | **0.117** |
| big (5 runs) | 6.18 s (6.07–6.47) | 5.67 | 5.66 | 5.16 | **5.02** | 11.67 | 6.26 |

1. **Overlap the writeback with the unpack.** The syncfs before the journal is 30–37% of an install today.
   - A helper thread calling syncfs in a loop during the unpack cut that wait from 78 to 8 ms on base and from 2.8 s to 0.04 s on big. Net gain: −17% on base, −8% on big.
   - Starting writeback per file (`sync_file_range`) inside the unpack workers is the wrong place to do it. In an earlier, more loaded round it halved base (0.267 → 0.132 s), but on big it made the install about 9% slower (7.76 against 7.13 s): workers blocked in the call, stretching unpack from 1.35 to 5.3 s.
   - So the kick belongs off the workers. A targeted version (a helper flushing each finished package's own files) would also stop vpm waiting on other processes' dirty data.
   - The syncfs before the journal stays, so the durability guarantee is unchanged; it just has little left to do.
2. **Make placing cheaper, then parallel.**
   - When the target does not exist, linking or symlinking straight onto its name is already atomic. That skips the temporary name, the unlink, the rename and the hash of the temporary name.
   - Measured: placing on big went from 1.68 to 1.16 s (−31%) and the install from 6.18 to 5.66 s (−8%). Together with the flusher, big is −17%.
   - Not measured yet: placing packages that have no scripts on several threads (directories first), and keeping file records in memory instead of re-reading `.vpm/files` twice.
3. **Shorten the unpack's critical path.**
   - All 1,102 packages are deflate-compressed (ADBd).
   - I made a zstd copy of the 244 packages with `apk adbsign -c zstd:9`. The identity and the Valence signature are unchanged, both tools verify it, and it shrinks from 1.2 GB to 998 MB.
   - vpm's base unpack went from 63 to 47 ms. With speedups 1 and 2 as well: base −23%, big −19%.
   - **apk gains much more** (base 0.281 → 0.117 s, big 11.7 → 6.3 s), because it is single-threaded and decompression-bound. With a zstd repository, vpm's fixed costs dominate small installs:
     - syncs and fsyncs;
     - every directory created twice (once under Programs, once in the live tree);
     - the journal.
   - On big, what is left is scheduling by expected cost (file count, not only installed size), worth at most about 10%, since 10.4 of 12 threads are already busy.

### Files
Scripts (all in `C:\Users\a\AppData\Local\Temp\claude\39afd7d0aa98685a\9f48dd28-e365-4eae-aba4-ec797468f31d\scratchpad\`):
- `vpm-bench.sh` — the benchmark. In WSL: `tr -d '\r' < …/vpm-bench.sh > /root/vpm-test/bench/vpm-bench.sh; bash /root/vpm-test/bench/vpm-bench.sh --runs 5`. It takes about 12 minutes.
- `vpm-bench-summary.py` — makes the summary tables.
- `vpm-bench-pick.py` — picks the big set.
- `vpm-bench-buildhead.sh` — builds the clean `bin/vpm`.
- `vpm-bench-buildprof.sh` — builds `bin/vpm-prof`.
- `vpm-bench-exp.sh` — the speedup experiments.
- `vpm-bench-zstdrepo.sh` — makes the zstd copy.
- `vpm-prof\` — the instrumented copy with the `VPM_FLUSHER`, `VPM_DIRECT_LINK` and `VPM_EARLY_WB` experiments. It is not committed anywhere.

Results (all in `/root/vpm-test/bench/`):
- `results.tsv` — one line per run.
- `summary.md`, `full.txt`.
- `prof-*.txt` — step profiles.
- `exp.tsv`, `exp2.tsv` — experiment results.
- `logs/`.
- `repo-zstd/` (998 MB) — can be deleted.

Nothing is mounted under `/root/vpm-test`, nothing was committed or pushed, and `/root/valence` was only read.