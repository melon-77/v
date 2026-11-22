# vpm benchmark data, 22 November 2026

Everything measured on the build machine on 22 Nov 2026: vpm against apk (and xbps where it can be compared), for installs, upgrades and single packages. The raw data, the scripts and the charts are in this folder. Every run's log is in `vpm-benchmark-2026-11-22.zip`, which the owner sent you, and on the build machine (`/root/vpm-test/bench2/*/logs`, `/root/vpm-test/upb/logs`).

**In one line: vpm wins big jobs and loses small ones.** Installing many packages is about 2 times faster than apk. Installing or upgrading one package is 1.2 to 4.4 times slower, because vpm has a fixed cost of tens of milliseconds per run: mostly start-up on a cold cache, plus the switch step's sync.

## The machine and the builds

| | |
|---|---|
| CPU | Intel Core i5-12450H, 12 threads |
| Memory | 24 GB (WSL2 VM) |
| OS | WSL2, kernel 6.18.58.1-microsoft-standard-WSL2, ext4 on a virtual disk |
| Package repository | the build machine's local `repo/x86_64`, recipes e7f3380, 1,132 packages |
| apk | apk-tools 3.0.8 (`hosttools/bin/apk`) |
| xbps | static xbps 0.60.4 (Void's `xbps-static-static-0.60.4_1.x86_64-musl.tar.xz`, sha256 checked against Void's list) |
| vpm "flat" | fc3ba9f (the flat `.vpm/` files, current `testing`), cargo release build, **unstripped** (30.8 MB) |
| vpm "SQLite" | 447bef7 (`vpm-sqlite-db`, PR #1), unstripped (31.5 MB) |
| vpm "hardening" | 717fa8c (`vpm-hardening`), unstripped (31.8 MB) |
| Rust | 1.98.1 |

**Conventions everywhere below:**
- Each number is the median of 5 runs.
- **Cold:** `sync; echo 3 > /proc/sys/vm/drop_caches` right before every timed run. **Warm:** one unrecorded warm-up run per tool first.
- All runs use `--no-scripts`.
- **"wall"** is the command's own time. **"+sync"** adds a `syncfs` of the target file system right after it. vpm syncs before it exits; apk and xbps leave the writeback to the kernel, so "+sync" is the fair "the data is really on disk" figure.
- **Ratios** are vpm's time divided by apk's. Below 1, vpm is faster.

## 1. Installing into an empty root (`vpm/tools/bench/vpm-bench.sh`)

- **base:** `valence-base`, 39 packages, 61 MB, 734 files.
- **big:** valence-base plus the 30 largest packages that install together, 244 packages, 3.3 GB, 72,692 files (`install/big-set.txt`).
- **Sittings:** each vpm build ran in its own sitting, with apk rerun alongside it as a control.

| set | cache | apk (same sitting) | vpm flat | vpm SQLite | vpm hardening |
|---|---|---|---|---|---|
| base | cold | 0.393 / 0.393 / 0.384 s | **0.207 s** (0.53) | 0.239 s (0.61) | 0.247 s (0.64) |
| base | warm | 0.269 / 0.266 / 0.265 s | **0.157 s** (0.58) | 0.266 s* (1.00) | 0.186 s (0.70) |
| big | cold | 11.94 / 12.53 / 12.80 s | **6.35 s** (0.53) | 6.76 s (0.54) | 6.70 s (0.52) |
| big | warm | 10.31 / 10.32 / 10.46 s | 4.89 s (0.47) | 5.02 s (0.49) | **4.89 s** (0.47) |

The apk column lists apk's time from the flat, SQLite and hardening sittings, in that order.

\* Noisy: the minimum was 0.181 s and the load average was 2.1 during that cell.

**The same with "+sync":**

| set | cache | apk (same sitting) | vpm flat | vpm SQLite | vpm hardening |
|---|---|---|---|---|---|
| base | cold | 0.475 / 0.470 / 0.469 s | 0.215 s (0.45) | 0.247 s (0.53) | 0.253 s (0.54) |
| base | warm | 0.345 / 0.419 / 0.411 s | 0.161 s (0.46) | 0.271 s (0.65) | 0.192 s (0.47) |
| big | cold | 13.77 / 14.16 / 14.34 s | 6.36 s (0.46) | 6.77 s (0.48) | 6.71 s (0.47) |
| big | warm | 12.66 / 12.66 / 12.43 s | 4.90 s (0.39) | 5.03 s (0.40) | 4.90 s (0.39) |

**Other figures from these runs:**
- **With install scripts (base):** vpm flat 0.238 s cold and 0.187 s warm; SQLite 0.281 / 0.201; hardening 0.275 / 0.219. apk fails every run with scripts under `--root` ("1 error" from `busybox.trigger`; it did in stage 1 too), so there's no apk comparison.
- **CPU time:** vpm uses 1.0 to 1.35 times apk's (parallel unpack).
- **Memory:** vpm uses 22 to 133 MB, apk 10 to 17 MB.

**xbps** installs Void's `base-system` (136 packages, 1,178 MB, 17,831 files), not Valence's packages, so compare the throughput only:

| | cold | cold +sync | warm | warm +sync |
|---|---|---|---|---|
| xbps, Void base-system | 2.81 s, 420 MB/s | 3.97 s, 296 MB/s | 2.59 s, 455 MB/s | 3.75 s, 314 MB/s |
| apk, big set | 276 MB/s | 240 MB/s | 320 MB/s | 261 MB/s |
| vpm flat, big set | 525 MB/s | 524 MB/s | 681 MB/s | 680 MB/s |
| vpm SQLite, big set | 493 MB/s | 493 MB/s | 664 MB/s | 663 MB/s |
| vpm hardening, big set | 498 MB/s | 497 MB/s | 682 MB/s | 681 MB/s |

**Files:** `install/results-{flat,sqlite,hard}.tsv` (one line per run, with vpm's phase times), `install/summary-*.md`, `install/sizes-*.tsv`. The logs are in the zip.

## 2. Upgrades and single packages (`scripts/up-bench.sh`)

**Setup:**
- Each timed operation runs on a freshly installed `valence-base` root; that install isn't timed.
- The tool order rotates every run.
- **up-all:** the kill test's upgrade, 27 base packages to 9999-r0 (built by `vpm/tools/killtest/vpm-kt-mkrepo.sh`, signed with the test key).
- **up-one:** only `coreutils` to 9999-r0. apk runs `upgrade coreutils`; vpm runs `add -u coreutils`.
- **add-small:** `nano`, 1 package, 0.5 MB.
- **add-large:** `go`, 1 package, 175 MB.
- Every one of the 160 runs was checked for the right result, and all passed.

| job | cache | apk | vpm flat | vpm SQLite | vpm hardening |
|---|---|---|---|---|---|
| up-all | cold | 0.328 s | 0.341 s (1.04) | 0.339 s (1.03) | 0.324 s (0.99) |
| up-all | warm | 0.219 s | **0.149 s** (0.68) | 0.169 s (0.77) | 0.168 s (0.76) |
| up-one | cold | **0.095 s** | 0.217 s (2.28) | 0.199 s (2.09) | 0.186 s (1.95) |
| up-one | warm | **0.068 s** | 0.081 s (1.19) | 0.091 s (1.34) | 0.095 s (1.40) |
| add-small | cold | **0.042 s** | 0.150 s (3.53) | 0.140 s (3.31) | 0.145 s (3.41) |
| add-small | warm | **0.014 s** | 0.047 s (3.43) | 0.060 s (4.41) | 0.057 s (4.22) |
| add-large | cold | **0.775 s** | 0.936 s (1.21) | 0.961 s (1.24) | 0.980 s (1.27) |
| add-large | warm | **0.749 s** | 0.903 s (1.21) | 0.947 s (1.27) | 0.903 s (1.21) |

**With "+sync":**
- **add-large:** vpm wins, 0.941 s against apk's 1.067 s cold (0.88 to 0.92).
- **add-small and up-one:** vpm still loses, 1.8 to 3.3 times slower.

**Files:** `upgrade-single/results.tsv` (with CPU and memory), `upgrade-single/phase-times.tsv`. The logs are in the zip.

## 3. Where vpm's time goes on small jobs

These are vpm's own phase times (the `OK:` line, 0.01 s resolution), medians of 5. "Outside vpm's phases" is the measured wall time minus vpm's own `total`.

| job | cache | wall | resolve | fetch+unpack | switch | vpm total | outside vpm's phases |
|---|---|---|---|---|---|---|---|
| add-small | cold | 0.150 | 0.02 | 0.01 | 0.02 | 0.06 | **0.090** |
| add-small | warm | 0.047 | 0.01 | 0.01 | **0.02** | 0.04 | 0.007 |
| up-one | cold | 0.217 | 0.02 | 0.06 | 0.03 | 0.12 | **0.097** |
| up-one | warm | 0.081 | 0.01 | 0.04 | **0.03** | 0.08 | 0.001 |
| up-all | cold | 0.341 | 0.02 | 0.13 | 0.10 | 0.25 | **0.091** |
| up-all | warm | 0.149 | 0.01 | 0.10 | 0.04 | 0.14 | 0.009 |
| add-large | cold | 0.936 | 0.02 | 0.66 | 0.17 | 0.87 | 0.066 |

That's the flat build; the other two are in `phase-times.tsv` and look the same.

**Stripped binary experiment** (`scripts/up-strip.sh`, `upgrade-single/stripped-experiment.txt`). Cold `add nano`, 6 runs:
- unstripped (30.8 MB): median 0.125 s
- stripped (2.4 MB, `strip`): median 0.098 s
- warm: unchanged at about 0.045 s

**What that says:**

1. **Cold start costs 61 to 97 ms before vpm's first phase begins**, on every job. Warm, the same gap is 1 to 10 ms. So it's reading from disk at start-up: the binary, its libraries, and whatever vpm reads before `resolve` (repository index, keys, the installed record, the lock).
   - Stripping the binary saves about 30 ms of it.
   - apk's entire cold `add nano` takes 42 ms.
   - We had no `strace` or `perf` on the build machine, so that's as far as this goes. Measure it with `strace -f -tt -T`, `perf trace`, or `VPM_PROF=1` (`vpm-bench-buildprof.sh`).
2. **The switch costs 20 to 30 ms even for one 0.5 MB package**, warm. apk's whole warm `add nano` is 14 ms.
   - Most of it is presumably the durability work: `syncfs` before the journal and at the end, plus the journal itself.
   - apk does none of that, and that's a feature of vpm. But it means every transaction pays a fixed tax that a multi-package job spreads out and a single-package job doesn't.
   - Making it proportional to the work (fsync only the files and directories the transaction touched, instead of the whole file system, when it's small?) without weakening crash safety is the design question. The kill test (`vpm/tools/killtest`) has to keep passing.
3. **Resolve takes 10 to 20 ms**, even for one package with no new dependencies. Worth checking whether that's parsing the whole index every time.
4. **The 175 MB single install** spends 0.66 s in fetch+unpack, nearly as long as apk takes for the whole job (0.75 to 0.78 s). With one package there's nothing to unpack in parallel.
5. **Ship a stripped binary.** The cargo build carries 28 MB of debug info. Check that the `vpm` package is stripped.
6. **Keep these jobs measured:** add up-one, add-small and add-large to `vpm-bench.sh`.

## How to reproduce (on the build machine, as root)

- **Installs:** `scripts/bench-run.sh` copies `vpm/tools/bench` into `/root/vpm-test/bench2/tools`, makes one copy of `vpm-bench.sh` per build (only `B=` differs), picks the big set, and runs
  ```sh
  VPM=<build> vpm-bench-<build>.sh --runs 5 --stages "disk base big"
  ```
  with `--xbps` for the first build. The vpm binaries are expected at `/root/vpm-test/bench2/{flat,sqlite,hard}/bin/vpm`.
- **Upgrades and single packages:** `scripts/up-bench.sh`. It needs the kill test's 9999-r0 repository at `/root/vpm-test/kt-repo`; `vpm-killtest.sh` builds it if it's missing. Set `RUNS=` to change the run count.
- **Stripped experiment:** `scripts/up-strip.sh`.

## What's in this folder

```
README.md                       this file
env.txt                         machine, tools and build versions
install/results-*.tsv           installs, one line per run (set, cache, scripts, tool, run, rc, wall, wall_sync, cpu, rss, vpm phases, load)
install/sizes-*.tsv             what each install put on disk
install/summary-*.md            the per-build tables made by vpm-bench-summary.py
install/big-set.txt             the 30 packages of the big set
install/logs-*/                 every install run's output (in the zip only)
upgrade-single/results.tsv      upgrades and single packages, one line per run
upgrade-single/phase-times.tsv  the same runs with vpm's own phase times
upgrade-single/stripped-experiment.txt
upgrade-single/logs/            every run's output (in the zip only)
scripts/                        the scripts that produced all of the above
charts/                         the three charts (simple, scorecard, install detail)
```

The build-machine reports on `testing` have the same numbers in prose: `docs/build-machine-reports.md`, sections "benchmark pass" (db0240b) and "apk strikes back" (4144a46).
