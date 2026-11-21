# Build-machine handoff: first test pass (vpm, CDDA tiles, Firefox, ISOs)

This note is for the Gemini agent running on the **build machine** (Ubuntu 24.04 / WSL2 with KVM, signing key, and disk space). It explains what was just added on `testing` (`melon-77/v`) and `main` (`melon-77/vpkg`), how to build and test each piece in order, and what to watch out for.

Read `AGENTS.md` before changing recipes or build scripts.

---

## 0. Sync the tree and submodule

Package recipes now live in the `recipes/` git submodule (`https://github.com/melon-77/vpkg`, branch `main`).

```sh
git checkout testing
git pull --ff-only
git submodule update --init --recursive
```

Verify `VALENCE_SIGN_KEY` (or `keys/valence-signing.rsa`) is in place before building.

---

## 1. Refresh host setup & fetch new sources

`scripts/host-setup.sh` was updated to add `clang-19 lld-19 nodejs cbindgen zip` for Firefox. Re-run it (idempotent) and fetch the new upstream tarballs (SDL2 stack, GTK 3 stack, Firefox 153.7.0esr):

```sh
sudo "$PWD/scripts/host-setup.sh"
"$PWD/scripts/fetch-sources.sh"
```

If this is a fresh checkout without a cross toolchain yet:

```sh
"$PWD/scripts/toolchain.sh"
```

---

## 2. Test pass A — `vpm` (Phase 10, Stage 2 engine additions)

Commit `bea0118` added commit hooks (`pre-commit`/`post-commit`, `--no-commit-hooks`), `Db::open_readonly`, `--simulate` (`-s`), `add -u`, `--arch`, `--print-arch`, `update`, `search`, `info`, and `cache clean`, and bumped `recipes/vpm` to `pkgrel=1`.

1. **Rebuild the `vpm` package:**
   ```sh
   "$PWD/scripts/valence-build" vpm
   ```
2. **Run the full Rust test suite against the real signed repository:**
   ```sh
   cd vpm
   VPM_REPO="$PWD/../repo/x86_64" VPM_KEYS="$PWD/../keys/trusted" cargo test --release
   ```
   All 38 unit tests, 4 CLI integration tests (`tests/engine_cmds.rs`), and 8 real-data tests (`tests/real_data.rs`) must pass.
3. **Smoke-test the new CLI commands against `repo/x86_64`:**
   ```sh
   vpm/target/release/vpm --keys-dir keys/trusted --repository repo/x86_64/Packages.adb search -x cataclysm-dda
   vpm/target/release/vpm --root /tmp/vpm-smoke --initdb --simulate --keys-dir keys/trusted --repository repo/x86_64/Packages.adb add valence-base
   ```
4. **Optional:** run the kill test (`vpm/tools/killtest/vpm-killtest.sh`) and benchmark (`vpm/tools/bench/vpm-bench.sh`) if you want fresh numbers after the commit-hook changes.

---

## 3. Test pass B — Cataclysm: DDA graphical tiles + sound (`SDL2` stack)

Commit `f9a17dd` in `vpkg` added `sdl2-compat`, `sdl2-image`, `sdl2-ttf`, `sdl2-mixer` and updated `cataclysm-dda` (`pkgrel=4`) to build both `cataclysm` (curses) and `cataclysm-dda-tiles` (graphical tiles + sound).

```sh
"$PWD/scripts/valence-build" cataclysm-dda
```

This builds `sdl3` (if not built yet) → `sdl2-compat` → `sdl2-image`, `sdl2-ttf`, `sdl2-mixer` → `cataclysm-dda` + `cataclysm-dda-tiles`.

**Verify:**
- Check `repo/x86_64/` contains `cataclysm-dda-0.9.1-r4.apk` and `cataclysm-dda-tiles-0.9.1-r4.apk` (and neither exceeds GitHub's 100 MB file limit).
- Run `"$PWD/scripts/check-dev-deps.py"` to confirm `sdl2-compat-dev`, `sdl2-image-dev`, `sdl2-ttf-dev`, and `sdl2-mixer-dev` declare all CMake/link dependencies cleanly.
- If `sdl2-compat` or `cataclysm-dda-tiles` hits a CMake/Makefile quirk during cross-compile, fix it in `recipes/<pkg>/VALENCEBUILD` (bump `pkgrel` if already built).

---

## 4. Test pass C — Firefox 153.7.0esr (`GTK 3` stack)

Added `at-spi2-core` (2.62.1), `libxt` (1.3.1), `gtk3` (3.24.43), and `firefox` (153.7.0esr). This is the **first real cross-compile attempt** of Firefox on Valence — expect at least one iteration!

1. **Build the GTK 3 stack first** so any meson/cross issues are isolated from Firefox:
   ```sh
   "$PWD/scripts/valence-build" at-spi2-core libxt gtk3
   ```
   - Watch for `gtk3` running host/target helpers (`gtk-update-icon-cache`, `glib-compile-resources`) during cross-build; `meson_setup` routes target binaries through `scripts/valence-run`, and `glib-compile-resources` comes from the host `libglib2.0-dev`.
2. **Build Firefox:**
   ```sh
   "$PWD/scripts/valence-build" firefox
   ```
   **Common things to watch for & fix in `recipes/firefox/VALENCEBUILD`:**
   - **Host tool versions:** Firefox 153.7.0esr may ask for a newer `cbindgen` or `nodejs` than Ubuntu 24.04's apt packages. If configure complains about `cbindgen` version, install a newer one into `$M/hosttools/bin` via `cargo install cbindgen` (using `$M/hosttools/rust/bin/cargo`); if `nodejs` is too old, note the required version and update `host-setup.sh`.
   - **Clang + GCC 15 headers/libgcc:** the recipe passes `--target=$TARGET --sysroot=$SYSROOT --gcc-toolchain=$TOOLS` to `clang-19`. If clang cannot find Valence's `x86_64-valence-linux-gnu` GCC 15 C++ headers, pass `-isystem $TOOLS/$TARGET/include/c++/15.2.0` (and its target subdir) or check `clang-19 -v` search paths.
   - **Rust target triple:** `host-rust.sh` ships `x86_64-unknown-linux-gnu`, while `$TARGET` is `x86_64-valence-linux-gnu`. If `./mach` passes `--target x86_64-valence-linux-gnu` to `rustc`, either set `ac_add_options --target=x86_64-unknown-linux-gnu` (matching `recipes/rust`) or set `CARGO_BUILD_TARGET=$RUST_TARGET`.
   - **Python virtualenv in `./mach`:** `MACH_BUILD_PYTHON_NATIVE_PACKAGE_SOURCE=system` is set; if `./mach` wants a missing Python package, install it in `host-setup.sh` or unset that env var if network is allowed during `prepare()`.
   - **Package size:** verify `firefox-153.7.0-r0.apk` stays under 100 MB (GitHub's limit).
   - **Missing library check:** verify no `LEAK:` lines and no missing `so:` deps (`scripts/check-dev-deps.py`).

---

## 5. Full repository build, ISOs, and QEMU test suites

Once the new packages build cleanly (or if you want to validate the whole distro end-to-end):

```sh
# 1. Build everything that changed (or everything from empty)
"$PWD/scripts/valence-build"
"$PWD/scripts/check-dev-deps.py"

# 2. Build both ISOs
"$PWD/scripts/mkiso.sh"
VALENCE_EDITION=desktop "$PWD/scripts/mkiso.sh"

# 3. Run the automated QEMU tests (8 console + 3 desktop)
"$PWD/scripts/test-console.sh" out/valence-*-x86_64.iso
"$PWD/scripts/test-desktop.sh" out/valence-desktop-*-x86_64.iso
```

---

## 6. Committing fixes

- Recipe fixes go into `recipes/` (`melon-77/vpkg`, branch `main`), pushed first, then bump the `recipes` submodule pointer in `melon-77/v` (`testing`).
- Script/engine/doc fixes go into `melon-77/v` (`testing`).
- Never push directly to `main` of `melon-77/v`.
- Include the trailer: `Co-Authored-By: Gemini 4 Neon <noreply@google.com>`.
