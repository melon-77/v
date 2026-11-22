# Build-machine handoff: vpm benchmark pass (with xbps), plus retest passes

This note is for the build machine (Claude Code on the WSL2 host with KVM and the signing key). It lists what has been fixed on `testing`, `vpm-sqlite-db` (PR #1), and `vpm-hardening` (`melon-77/v`) in response to `docs/build-machine-reports.md`, and what to run next.

---

## 0. Priority pass — `vpm` speed benchmark (`fc3ba9f` vs `vpm-sqlite-db` vs `vpm-hardening` vs `apk` vs `xbps`)

Requested per the 2026-11-22 speed report (pending owner approval to download Void's static `xbps` with SHA-256 verification via `vpm-bench.sh --xbps`):

- **`vpm-sqlite-db` (`447bef7`, PR #1):** per-package switch commits collapsed into 1 atomic `Db::commit_switch()` transaction (2 SQLite commits total per transaction), `PRAGMA synchronous = NORMAL` + 16 MiB page cache, empty-root collision probe short-circuited, `StagedPkg` deep clone removed, plus N-2 (`/boot` dir fsync) and N-3 (`etc/` file-to-symlink) fixed.
- **`vpm-hardening` (`717fa8c`):** rebased onto `447bef7` with bounded `RootFs` parent `dirfd` caching (`Rc<OwnedFd>`) so `openat2(RESOLVE_IN_ROOT)` is not called per file.

Please run `vpm/tools/bench/vpm-bench.sh` (with `--xbps`, cold and warm cache) comparing:
1. `apk` (hosttools)
2. `xbps` (Void's static `xbps-static-latest.x86_64-musl.tar.xz`, checksum verified)
3. `vpm` at `fc3ba9f` (stage 1 flat `.vpm/` baseline)
4. `vpm` at `vpm-sqlite-db` (`447bef7`)
5. `vpm` at `vpm-hardening` (`717fa8c`)

Workloads to record (wall time and `+sync`):
- Fresh-root install of `base` (and `big` if time permits), cold and warm
- An upgrade workload (recording both wall and `+sync`)
- Installed-root query timing (`list`, `info -W`) if convenient to show the SQLite trade-off

Once the table is posted in `docs/build-machine-reports.md`, we'll add it directly to PR #1 before any merge.

---

## 0b. Sync the tree and submodule

```sh
git checkout testing
git pull --ff-only
git submodule update --init --recursive
```

---

## 1. Fetch updated sources

`libevent` moved to `2.1.13-stable` (S1) and `gtk3` moved to `3.24.52` (S2/G8); `libXt` was removed. `cbindgen` was dropped from `scripts/host-setup.sh` (F8: `firefox` uses `$M/hosttools/rust/bin/cbindgen`).

```sh
"$PWD/scripts/fetch-sources.sh"
```

---

## 2. Pass A — Phase 10 (`vpm`): engine fixes, installers, ISOs, and QEMU suites

Fixes applied from the first report:
- **I-1 (high):** `vpm` now places `/boot` files as synced atomic copies (never hard-linked, so `mksquashfs` + Calamares `unsquashfs` on FAT32 `/boot` and kernel upgrades work), and falls back to synced atomic copy on any `EXDEV`, tolerating FAT32 `EPERM`/`EINVAL` on chown/chmod/utimens. `place_link`, `remove_old`, and `vpm fix` treat "same content hash, other device" as intact.
- **I-2:** added `-X` / `--add-repository` (appends to `ROOT/etc/apk/repositories`, matching `apk -X`) and switched `valence-wish` under `vpm` to use it.
- **I-3 & V-7:** unreachable repositories warn and are skipped (staying fatal on `Untrusted`/`Integrity` and when 0 repositories load). `vpm update` exits 1 when any repository failed or fell back to cache; `vpm search` reuses cached remote indexes up to 60s old.
- **I-4:** `valence-first-boot` now auto-detects `vpm` vs `apk` (`valence-desktop` r21); user messages in `valence-wish` and `valence-snapshot` no longer hardcode `apk` (`valence-base` r49).
- **I-5:** `scripts/qemu-test.py` auto-detects `vpm` (`[ -e /var/lib/vpm/world ]`) in `disk`, `rollback`, and `wish` modes.
- **I-6:** `VALENCE_PKG=vpm scripts/mkiso.sh` now writes `out/valence-vpm-<date>-x86_64.iso` and `out/valence-vpm-desktop-<date>-x86_64.iso` and never deletes normal `apk` ISOs.
- **V-1 to V-14:** preserved admin-modified symlinks under `/etc` (with `.vpm-new` on upgrade, kept on `fix`), verified `Programs/` hashes in `vpm fix`, added commit hooks around `vpm fix` when repairs are needed, fixed hook precedence/env/lock/post-commit behavior, added provides matching in `vpm search`, fixed `vpm info -W`, honoured `--simulate` in `update`/`cache clean`, made temp download names PID-unique, and updated `vpm/README.md`.

**Commands to run:**
```sh
# 1. Rebuild updated packages (includes glib r2 with GSettings trigger G2)
"$PWD/scripts/valence-build" glib vpm valence-base valence-desktop

# 2. Rebuild host vpm and run its full test suite against repo/x86_64
( cd vpm && VPM_REPO="$PWD/../repo/x86_64" VPM_KEYS="$PWD/../keys/trusted" cargo test --release )

# 3. Verify I-1 directly (re-run the FAT32 /boot add + fix reproduction if desired)

# 4. Build and QEMU-test BOTH normal (apk) ISOs (needed since glib r2 is in the base)
"$PWD/scripts/mkiso.sh"
VALENCE_EDITION=desktop "$PWD/scripts/mkiso.sh"
ALL=1 "$PWD/scripts/test-console.sh" out/valence-2*-x86_64.iso
"$PWD/scripts/qemu-test.py" wish out/valence-2*-x86_64.iso
"$PWD/scripts/test-desktop.sh" out/valence-desktop-2*-x86_64.iso

# 5. Build and QEMU-test BOTH vpm ISOs (console + desktop)
VALENCE_PKG=vpm "$PWD/scripts/mkiso.sh"
VALENCE_PKG=vpm VALENCE_EDITION=desktop "$PWD/scripts/mkiso.sh"
ALL=1 "$PWD/scripts/test-console.sh" out/valence-vpm-2*-x86_64.iso
"$PWD/scripts/qemu-test.py" wish out/valence-vpm-2*-x86_64.iso
"$PWD/scripts/test-desktop.sh" out/valence-vpm-desktop-2*-x86_64.iso
```

---

## 3. Pass C — GTK 3 (`3.24.52`) + Firefox (`153.7.0esr`, official branding, Wayland-only)

Fixes applied:
- **G2:** `glib` r2 trigger on `/usr/share/glib-2.0/schemas` running `glib-compile-schemas`.
- **S2/G8, G3, G5:** `gtk3` updated to `3.24.52`, `-Dbuiltin_immodules=backend -Dwrap_mode=nofallback -Dxinerama=no`, `harfbuzz-dev` added.
- **G4:** `at-spi2-core` r2 removes host-compiled `usr/lib/python*`.
- **F1–F6, F8, F9, F11:** `--target=$RUST_TARGET`, host-build stripping in `ff-cc/{cc,cxx}`, `unset AS`, `firefox-data` subpackage split (for GitHub's 100 MB limit), `--enable-official-branding` + `MOZ_APP_REMOTINGNAME=firefox` + `StartupWMClass=firefox` + icons 16..256, `depends=(firefox-data ffmpeg-libs mesa pipewire libpulse)`, `CBINDGEN=$M/hosttools/rust/bin/cbindgen`, cleaned `makedepends`, `arch=(x86_64)`.
- **libxt:** deleted (unused after Wayland-only decision).

```sh
"$PWD/scripts/valence-build" glib at-spi2-core gtk3 firefox
"$PWD/scripts/check-dev-deps.py"
```

Verify `firefox` and `firefox-data` `.apk` sizes are each under 100 MB, and check with `readelf -V` that no build-host tool linked against Valence's glibc 2.44.

---

## 4. Pass D — CLI & sysadmin essentials

Fixes applied:
- **C1, C14:** `vim` r1 (`srcdir=.` before `./configure`, disabled auto-detected optional features).
- **C2, C4, C13:** `wireguard-tools` r1 (no `CFLAGS=` override on make CLI, `RUNSTATEDIR=/run`, `options=(keepdirs)` + `/etc/wireguard` 0700, split-tunnel note in `pkgdesc`).
- **C3, C7, C17:** `openssh` r1 (added `/etc/sv/sshd/log/run` with `svlogd`, `license=SSH-OpenSSH`, removed empty `/usr/share`).
- **S1, C12:** `libevent` updated to `2.1.13-stable`, moved `event_rpcgen.py` to `libevent-dev` with `python3`.
- **C5, C8:** `htop` r1 (`GPL-2.0-or-later`, enabled sensors/capabilities/delayacct with `lm-sensors-dev libcap-dev libnl3-dev` and runtime `depends=(libsensors libnl3)`).
- **C9, C10, C11, C15:** `jq` r1 (`MIT AND BSD-2-Clause`, `onig-config` in `jq-dev`), `strace` r1 (`elfutils-dev` + `--with-libdw`), `tmux` r1 (`--with-TERM=tmux-256color`), `fd` r1 (`--no-default-features --features completions` + bash completion).

```sh
"$PWD/scripts/valence-build" openssh libevent tmux htop jq vim tree strace wireguard-tools fd
"$PWD/scripts/check-dev-deps.py"
```

---

## 5. Pass B — Cataclysm: DDA (`0.9.1-r4`, terminal + graphical tiles/sound) and SDL2 stack

Re-ported with subpackage variable bugs fixed (`pkgdesc_cataclysm_dda_tiles`, `depends_cataclysm_dda_tiles`, and `provides_sdl2_compat_dev` so `cataclysm-dda-tiles` depends on `cataclysm-dda=$pkgver-r$pkgrel` and `sdl2-compat-dev` provides `sdl2-dev`):
- `sdl2-compat` 2.32.74 (runs SDL2 over Valence's `sdl3`), `sdl2-image` 2.8.12, `sdl2-ttf` 2.24.0, `sdl2-mixer` 2.8.2
- `cataclysm-dda` 0.9.1-r4 (`cataclysm` terminal + `cataclysm-dda-tiles` graphical tiles & sound)

```sh
"$PWD/scripts/fetch-sources.sh"
"$PWD/scripts/valence-build" sdl2-compat sdl2-image sdl2-ttf sdl2-mixer cataclysm-dda
"$PWD/scripts/check-dev-deps.py"
```
