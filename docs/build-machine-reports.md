# Build-machine reports

The Claude Code session on the owner's build machine (the WSL2 host with KVM and the signing key) writes these reports for the developers and their Gemini agent. The newest report is first.

How this works:

- Put requests in `docs/build-machine-handoff.md`, as you do now. The handoff calls its reader "the Gemini agent running on the build machine"; the build machine actually runs Claude, and Gemini works on valence-dev. The requests reach the build machine either way.
- The owner approves each test pass. I run it and write the results here, then push to `testing`.
- I don't change your code (`vpm/`, `recipes/vpm`, your scripts) unless the owner asks. The findings below suggest fixes; making them is up to you.

---

## 2026-11-21, night: the owner's defaults for the open questions

The owner had no preference yet on the questions the reports left to them, and took the recommended defaults: "yeah sure put them on record". They are in `ROADMAP.md` under "Decided" and can still change.

| Question | Decision | What it means for you |
|---|---|---|
| G2: GSettings schemas | Yes: a trigger in glib that runs `glib-compile-schemas` | Approved. Whoever picks it up adds it to `recipes/glib` and bumps glib's pkgrel. It's a base change, so run the console and desktop tests |
| F5: Firefox's branding | Official (`--enable-official-branding`) | Add it to the firefox recipe together with F1 to F4 |
| C4: WireGuard full tunnels | Not now | No kernel change. Say in wireguard-tools' pkgdesc or the docs that only split tunnels without `DNS=` work |
| Phase 10's name | `vpm` stays the working name | The final name is chosen before apk leaves the installed system |
| `-dev`, `-doc`, `-lang` under vpm | Folders of their own, as now | Nothing to change |

---

## 2026-11-21, evening: the owner's decisions on the report below, and what changed

Tree: `testing` a995db8, `recipes` 2771bdf.

The owner answered the four questions the last report left open: "1, fix 2, delete, 3 that for wayland only, 4 name it to firefox". Here is what each answer changed.

**1. The locale bug: fixed in glibc 2.44-r2.**

- `recipes/glibc/VALENCEBUILD` now compiles C.UTF-8 in `package()` with the localedef it has just built (`make -C localedata install-files-C.UTF-8/UTF-8`). The files go in `/usr/lib/locale/C.utf8/`, with no locale archive.
- The build stops if `LC_CTYPE` isn't there.
- The wrong comment ("built in") is gone.
- `/etc/profile` is unchanged: `LANG=C.UTF-8` was right, the locale was just missing.

**2. Cataclysm: deleted.**

- Removed recipes (vpkg): `cataclysm-dda`, `sdl2-compat`, `sdl2-image`, `sdl2-ttf`, `sdl2-mixer`. Nothing else depended on them.
- Also removed:
  - their five rows in `sources/MANIFEST.tsv` and the five URLs in `scripts/make-manifest.py`
  - the three mentions in `AGENTS.md`
  - the old `cataclysm-dda-0.9.1-r3` packages in the build machine's repository
- The package repository has never been published (there is no `packages` branch), so there was nothing to take down.
- SDL3 (`sdl3`, `sdl3-image`, `sdl3-ttf`) stays. It was in Valence before Cataclysm's update, and `AGENTS.md` lists it as a library of its own.
- `docs/build-machine-handoff.md` still lists pass B. That file is yours; drop pass B the next time you edit it.

**3. GTK 3 and Firefox: Wayland only.** This settles G1 with option (b). cairo stays as it is, with no X11.

| Recipe | Change |
|---|---|
| `gtk3` r1 | `-Dx11_backend=false`; the 10 X11 `-dev` makedepends removed; pkgdesc says "Wayland only" |
| `at-spi2-core` r1 | `-Dx11=disabled`; `libx11-dev libxtst-dev libxi-dev` removed |
| `firefox` r1 | `--enable-default-toolkit=cairo-gtk3-wayland-only`; the 11 X11 `-dev` makedepends removed, `libxt-dev` with them |

- `libxt` is now used by nothing. I left the recipe in place: delete it if you don't need it.
- The Firefox change is untested, because Firefox still can't build until F1 to F4 are fixed. If its configure asks for an X11 library after all, add that one back as a makedepend.

**4. The package name stays `firefox`.** This settles F7.

- Once a `firefox` package is in the repository, `packages = firefox` in a wish list installs it instead of Flathub's `org.mozilla.firefox`. `valence-wish` asks the repository first.
- `valence-first-boot` still offers Firefox from Flathub. Switching that, and the docs that say Firefox comes from Flathub, is for when the native package really exists.

### What ran

| Step | Result |
|---|---|
| `valence-build glibc at-spi2-core gtk3` | built 3, failed 0: glibc 2.44-r2 (5.9 MB), at-spi2-core r1 (244 kB), gtk3 r1 (4.4 MB, plus 8.3 MB of `-lang`) |
| `VALENCE_ARCH=aarch64 valence-build glibc` | built: the aarch64 localedef ran through QEMU's user emulation, and the recipe's C.UTF-8 check passed |
| locale test: a fresh root of busybox, valence-base, tmux and coreutils, the way the ISO is made | passes (output below) |
| gtk3 r1 installed in a fresh root | `pkg-config --variable=targets gtk+-3.0` is `wayland`; there is no `gtk+-x11-3.0`. Its own library dependencies are Wayland's and xkbcommon, with no X11 library |
| console ISO from a995db8 and the eight console tests | all 8 pass. The live system gains the 14 entries of `usr/lib/locale/C.utf8`. Apart from apk's database, the only other difference from this morning's image is that `var/cache/apk` is gone |
| desktop ISO from a995db8 and `scripts/test-desktop.sh` | all 3 pass: the live desktop, and installs on BIOS and on UEFI that log in to Plasma |

```
login shell (/etc/profile):  LANG=C.UTF-8, locale charmap: UTF-8, every LC_* "C.UTF-8" but LC_COLLATE=C
printf 'é' | wc -m           ->  1        (2 bytes, 1 character)
LANG=C locale charmap        ->  ANSI_X3.4-1968   (control)
tmux 3.8: new-session -d, ls, display -p, kill-server  ->  exit 0
/usr/lib/locale/C.utf8: 12 LC_* entries, 420 kB in all (LC_CTYPE 369 kB)
```

### Not touched

F1 to F4 (Firefox), C1 (vim), C2 (wireguard-tools) and the other findings below are unchanged. They are yours, unless the owner says otherwise.

---

## 2026-11-21, later: handoff passes C and D (GTK 3, Firefox, command-line tools)

Tree: `testing` 318a15b, `recipes` 1f2af04. Line numbers below refer to these commits.

On pass B, the owner said: "we dont need cataclysm at all". So pass B was not run. The full build also left out `cataclysm-dda` and the four SDL2 libraries that only it uses (`sdl2-compat`, `sdl2-image`, `sdl2-ttf`, `sdl2-mixer`). The owner hasn't decided yet whether to remove those recipes.

### Summary

- **Sources:**
  - All 14 new source files downloaded, and their sha256 values match `MANIFEST.tsv`.
  - The review checked their signatures independently, and all 14 are genuine. Only the commit messages are missing how they were checked (S3, which also has the fingerprints to cite).
- **Pass D:** 8 of the 10 recipes build: openssh, libevent, tmux, htop, jq, tree, strace and fd. vim and wireguard-tools fail, each on one line of its recipe (C1, C2).
- **Pass C:**
  - at-spi2-core and libxt build.
  - gtk3 fails at configure (G1).
  - Firefox was not tried, because it needs gtk3. The review expects four more problems to stop it (F1 to F4).
- **Smoke test:** the 8 packages that built were installed with apk into a fresh root, made the way the ISO makes its own, with install scripts on.
  - Every program runs, and all its libraries are found.
  - The sshd user is created, and sshd's configuration check passes.
  - tmux doesn't start, because of a bug in Valence's base, not in tmux (see below).
- **`check-dev-deps.py`:** 316 packages, 0 problems.
- **Full build without Cataclysm:** built 0, up to date 461, failed 3 (vim, wireguard-tools, gtk3), not tried 1 (firefox).

### What ran

| Step | Result |
|---|---|
| `host-setup.sh` (adds clang-19, lld-19, nodejs, cbindgen, zip) | OK: clang/lld 19.1.1, nodejs 18.19.1, cbindgen 0.26.0 (too old, see F8), zip 3.0 |
| `fetch-sources.sh` | OK: all 14 new files present, sha256 matches |
| `valence-build openssh libevent tmux htop jq vim tree strace wireguard-tools fd` | built 8, failed 2: vim, wireguard-tools |
| `valence-build at-spi2-core libxt gtk3` | built 2, failed 1: gtk3 |
| `valence-build` with every recipe except Cataclysm and SDL2 | failed 3; firefox "waits for gtk3" |
| `check-dev-deps.py` | 0 things named that the package does not bring |
| apk smoke test of the 8 built packages | all run; tmux blocked by the base locale bug |

Package sizes: openssh 3.4 MB, fd 1.7 MB, tmux 613 kB, strace 618 kB, jq 392 kB, libevent 330 kB, at-spi2-core 265 kB, libxt 188 kB, htop 168 kB, tree 41 kB, plus their -dev, -doc and -lang packages.

The smoke test: busybox, then valence-base, then `apk add openssh tmux htop jq strace tree fd libevent-dev jq-dev`, then each program run in a chroot.

```
htop 3.5.3 / jq-1.8.2 / strace -- version 7.3 / tree v2.3.2 / fd 10.5.2 / OpenSSH_10.6p2
echo '{"a":[1,2,{"b":"ok"}]}' | jq -r '.a[2].b'   ->  ok
ld-linux --list on tmux, htop, jq, strace, tree, fd, ssh: 0 libraries not found
openssh.pre-install: sshd:x:100:101:sshd:/var/empty:/sbin/nologin, group sshd:x:101
ssh-keygen -A: RSA ECDSA ED25519 MLDSA44-ED25519; sshd -t: exit 0; not enabled by default
sshd -T: permitrootlogin prohibit-password, passwordauthentication yes, kbdinteractiveauthentication yes
tmux -V: "tmux: invalid LC_ALL, LC_CTYPE or LANG"
```

### Found in Valence's base (the owner's to fix, not yours)

**`LANG=C.UTF-8` names a locale that doesn't exist on Valence**

- **Cause:**
  - `recipes/valence-base/files/etc/profile:4` exports `LANG=C.UTF-8`.
  - The comment at `recipes/glibc/VALENCEBUILD:34-35` says "The C.UTF-8 locale is built in".
  - glibc's own NEWS says the opposite: "The locale is not built into glibc, and must be installed."
  - Nothing in Valence runs `localedef`, and no package ships `/usr/lib/locale`.
- **Effect:**
  - On a base system, `locale charmap` prints `ANSI_X3.4-1968`, so every program runs in plain ASCII.
  - tmux refuses to start: "invalid LC_ALL, LC_CTYPE or LANG", even with `-u`.
  - The tmux recipe needs no change; it will work once the base is fixed.

### Findings

These come from four review agents. A second agent then checked each finding against the code, the upstream sources and the build logs. Severity is the checked one.

**Build blockers**

- **C1 (high): vim doesn't build.** `recipes/vim/VALENCEBUILD:18`.
  - The builder exports `srcdir`, and vim's `src/configure` turns it into `--srcdir`. Configure then looks for `vim.h` in the unpack directory: "configure: error: cannot find sources (vim.h) in /root/valence/work/pkg/vim/src".
  - Fix: put `srcdir=.` in front of `./configure`, or run `(unset srcdir; ./configure ...)`.
- **C2 (high): wireguard-tools doesn't build.** `recipes/wireguard-tools/VALENCEBUILD:10`.
  - `CFLAGS=` on make's command line overrides every `CFLAGS +=` in `src/Makefile`, so `-DRUNSTATEDIR`, `-std=gnu99 -D_GNU_SOURCE` and `-isystem uapi/linux` are lost: "'RUNSTATEDIR' undeclared".
  - Fix: `make -C src RUNSTATEDIR=/run -j$JOBS`. The builder already exports CC, CFLAGS and LDFLAGS in the environment. Pass `RUNSTATEDIR=/run` in `package()` too.
- **G1 (high): gtk3 stops at configure.** `recipes/gtk3/VALENCEBUILD:15`; the cause is `recipes/cairo/VALENCEBUILD:10-11`.
  - With `-Dx11_backend=true`, gtk 3.24's meson.build:497-506 requires `cairo-xlib`, but Valence's cairo is built with `-Dxlib=disabled -Dxcb=disabled`: `Dependency "cairo-xlib" not found`.
  - Every other module gtk3 needs is in the sysroot.
  - **Owner's decision:** cairo is in the desktop ISO. Either:
    - **(a)** turn on `-Dxlib=enabled` in cairo (keep xcb off), add `libx11-dev libxext-dev libxrender-dev` to its makedepends and bump its pkgrel; or
    - **(b)** build gtk3 with `-Dx11_backend=false` and Firefox with `--enable-default-toolkit=cairo-gtk3-wayland-only`.
- **F1 (high): Firefox's configure can't map `--target=x86_64-valence-linux-gnu` to a Rust triple.** `recipes/firefox/VALENCEBUILD:62`.
  - Tracing `detect_rustc_target` in `build/moz.configure/rust.configure:390-444` by hand, the vendor "valence" matches none of the host Rust's targets, so configure dies.
  - Fix: `ac_add_options --target=$RUST_TARGET` (`x86_64-unknown-linux-gnu`, from `scripts/env.sh:11`). Keep `--target=$TARGET` inside the `ff-cc` wrappers for clang. Do not use `x86_64-pc-linux-gnu`: it equals `--host` and turns cross-compiling off.
- **F2 (high): once F1 is fixed, host and target Rust share one triple.** `recipes/firefox/VALENCEBUILD:30-47`, with Firefox's `config/makefiles/rust.mk:205-213, 412-429, 481`.
  - Firefox's own comment says cargo then uses the target linker for host code too. So build scripts and proc-macros would be linked against Valence's sysroot (rules 18 and 46).
  - Fix: have the `ff-cc` wrappers do what `rust_cc` does the other way round. When the output is under cargo's host directories (`/release/build/` or `/release/deps/` not under `/$RUST_TARGET/`), drop every `--sysroot` argument and exec plain clang-19. After the first build, check with `readelf -V` that no build script needs a GLIBC_ version above the build machine's 2.39.
- **F3 (high): the builder's `AS=x86_64-valence-linux-gnu-as` reaches Firefox's configure.** `scripts/valence-build-recipe:35`.
  - Firefox then runs GNU as directly on `.S` files with compiler flags. Checked: GNU as 2.46 fails on `-DFOO` and `-fPIC` ("invalid option").
  - Fix: `unset AS` in `build()`, next to the CC/CXX exports.
- **F4 (high): `firefox.apk` will be far over GitHub's 100 MB limit, and `scripts/publish-repo.sh:20-21` then refuses to publish the whole repository.**
  - Mozilla's own 153.7.0esr build has a 185 MB `libxul.so` that deflates to 77 MB, and about 122 MB for the whole tree.
  - Fix: a `firefox-data` subpackage (`omni.ja`, `browser/omni.ja`, fonts, dictionaries) that firefox depends on, plus a size guard in `package()`. Measure again after the first real build.

**Security and correctness**

- **C3 (high): sshd sends its whole log to the console (rule 14).** `recipes/openssh/VALENCEBUILD:27`.
  - `sshd -D -e` with `exec 2>&1` and no `log/run` puts every connection and failed login on runsvdir's stdout, the console. It's off by default, but floods the console once switched on.
  - Fix: add `etc/sv/sshd/log/run` like ntpd and avahi have (`exec svlogd -tt /var/log/$name`), or drop `-e` so sshd uses syslog. Bump pkgrel.
- **G2 (high): nothing ever compiles GSettings schemas, so GTK 3's file, colour and emoji choosers abort the program.** The mechanism belongs in `recipes/glib/VALENCEBUILD`, which is in the base.
  - gtk3 installs four `org.gtk.Settings.*.gschema.xml`, but meson skips the post-install step in a cross build, and no trigger exists. GLib's `g_settings_new` aborts on a schema that isn't compiled.
  - On Plasma, Firefox's file picker goes through the KDE portal, so Plasma users mostly won't hit it.
  - Fix: a glib trigger on `/usr/share/glib-2.0/schemas` that runs `glib-compile-schemas`, with a pkgrel bump for glib.
- **S1 (medium): libevent 2.1.12 (2020) is not current.** `sources/MANIFEST.tsv:823`.
  - 2.1.13-stable (2026-07-01) fixes nine security advisories. tmux, the only user, calls none of the affected modules.
  - Fix: `libevent-2.1.13-stable.tar.gz`, sha256 `f7e9383b8c0baa81b687e5b5eecc01beefaf1b19b64151d95ed61647fe7a315c`. Its signature checks against Nick Mathewson's key (primary `2133BC600AB133E1D826D173FE43009C4607B1FB`), and it matches Arch and Alpine. tmux needs no rebuild.
- **S2 / G8 (medium): gtk 3.24.43 is five releases behind.** `sources/MANIFEST.tsv:820`.
  - The 3.24 series continues as `gtk/3.24/gtk-3.24.52.tar.xz`. Its NEWS fixes a Firefox Wayland drag-and-drop crash (#7976).
  - sha256 `80931fa472a77b9a164f6740e3c0b444fac6770054632d35a7ff9d679e5e7b9f`, from GNOME's .sha256sum and matching Debian's .dsc. The meson options are unchanged. It does not fix G1.
- **S3 / C6 / G7 (medium): the commits don't say how the sources were checked** (AGENTS.md:1239-1242). Every file checks out; add the record in the next commit that touches them.
  - Signatures:
    - Mozilla SHA256SUMS.asc: `14F26682D0916CDD81E37B6D61B7B526D98F0353`
    - libXt .sig: `3BB639E56F861FA2E86505690FDD682D974CA72A`
    - OpenSSH .asc: `7168B983815A5EEF59A4ADFD2A3F414E736060BA`
    - strace .asc: `7BECFE3AF7B280BB52FF77F104BA4521C996DDE1`
    - WireGuard .tar.asc: `AB9942E6D4A4CFC3412620A749FC7012A5DE03AE`
    - jq: `93079A9511EFC0B7D6CDBFB5B0DA60FB454BAF18`
    - vim's signed tag: `4F19708816918E19AAE19DEEF3F92DA383FDDE09`
    - libevent 2.1.12 .asc: `9E3AC83A27974B84D1B3401DB86086848EF8686D`
  - Checksums with a distribution cross-check:
    - at-spi2-core: GNOME sha256sum + Alpine
    - tmux: Alpine
    - htop: upstream .sha256 + Alpine
    - tree: Alpine
  - fd: the crates.io crate fd-find 10.5.2 is pinned by Debian's `rust-fd-find_10.5.2-1.dsc`, and it is byte-identical to the GitHub archive.
- **C4 (medium): wg-quick fails for full-tunnel and `DNS=` configurations.** Valence has no nft, iptables or resolvconf. The kernel also lacks `NFT_FIB_IPV4/IPV6/INET`, `NETFILTER_ADVANCED`, `NF_CONNTRACK_MARK` and `IPV6_MULTIPLE_TABLES`, which are the owner's kernel config. Until then, say in pkgdesc or the docs that only split tunnels without `DNS=` work.
- **C5 (medium): htop's sensors, capabilities and delay accounting are switched off silently (rule 63).**
  - Fix: makedepends `lm-sensors-dev libcap-dev libnl3-dev`; `--enable-sensors --enable-capabilities --enable-delayacct`; `depends=(libsensors libnl3)`, since both are dlopen()ed.
- **F5 (medium): Firefox gets no branding or channel options.** It calls itself "Nightly", its app_id is `firefox-default`, and `firefox.desktop` matches neither.
  - Fix: `export MOZ_APP_REMOTINGNAME=firefox` (or `--enable-update-channel=release`), `StartupWMClass=firefox` in the .desktop file, and icons at all sizes (16 22 24 32 48 64 128 256).
  - `--enable-official-branding` is the owner's decision, because of Mozilla's trademark terms.
- **F6 (medium): Firefox's depends.** It dlopen()s libpulse, which the so: scan can't see, and loads ffmpeg-libs, not the ffmpeg tools. Fix: `depends=(ffmpeg-libs mesa pipewire libpulse)`.
- **F7 (medium, owner's decision): a package named `firefox` silently changes `packages = firefox` wishes from Flathub to this build** (`valence-wish:114-134`).
  - AGENTS.md (34, 745, 1277, 1316), README.md:41, ROADMAP.md (94, 173, 189) and docs/updating.md (44, 57) still say Firefox comes from Flathub.
  - Ask the owner before publishing it. Either name it `firefox-esr`, or switch on purpose and update those docs in the same change.

**Low**

- **C7:** openssh's license is `SSH-OpenSSH`, not BSD-2-Clause.
- **C8:** htop is `GPL-2.0-or-later`.
- **C9:** jq installs its bundled oniguruma as a system library and claims `so:libonig.so.5`. Package oniguruma, or move `onig-config` to jq-dev and fix the license field.
- **C10:** strace was built without `-k`, because elfutils-dev is missing. Add `--with-libdw`, or say `--without-libdw`.
- **C11:** tmux's default-terminal falls back to `screen`. Use `--with-TERM=tmux-256color`.
- **C12:** libevent's runtime package ships `event_rpcgen.py`. Move it to libevent-dev, with python3.
- **C13:** `/etc/wireguard` (mode 0700) is dropped. Use `options=(keepdirs)` or create it in a post-install.
- **C14:** vim's optional features are auto-detected. Pass `--disable-gpm --disable-canberra --disable-libsodium --disable-selinux --disable-smack` and choose acl. `--disable-nls` is right: English only is decided.
- **C15:** fd's bundled jemalloc runs its configure tests on the build machine, and no shell completions are shipped.
- **C16:** the wish-list examples in docs/updating.md:44 name the sshd service without openssh. AGENTS.md:83-85 should list openssh among the packages that call `adduser`.
- **C17:** `keepdirs` also ships an empty `/usr/share` in openssh. Cosmetic.
- **G3:** gtk3's input-method cache is never written. Use `-Dbuiltin_immodules=backend`.
- **G4:** at-spi2-core ships `python3/dist-packages/.../Atspi.py`, byte-compiled for the build machine's Python 3.12. Remove it in `package()` and bump pkgrel (r0 is already in the repository).
- **G5:** gtk3 needs `-Dxinerama=yes -Dwrap_mode=nofallback` and `harfbuzz-dev` in makedepends.
- **G6 (plausible):** no SVG loader (librsvg) and no Adwaita icons, so GTK dialogs miss icons.
- **G9:** libxt should pass `--disable-specs --without-xmlto --without-fop --without-xsltproc` like libx11, and bump pkgrel.
- **F8:** Ubuntu's cbindgen 0.26 is below Firefox's minimum of 0.29.4. It works only because hosttools' 0.29.4 comes first in PATH.
  - `export CBINDGEN=$M/hosttools/rust/bin/cbindgen` and drop cbindgen from host-setup.sh.
  - The handoff's `cargo install cbindgen` would download outside `sources/`.
- **F9:** Firefox's makedepends need libice-dev and libsm-dev, not libxt-dev. Drop pixman-dev, pipewire-dev and libdrm-dev, or pass their `--with-system-*` options.
- **F10:** Firefox's private NSS, NSPR and libxul libraries would become global `so:` provides. Nothing breaks yet.
- **F11 (plausible):** add `arch=(x86_64)`, because the host Rust has std only for x86_64.

Checked and rejected: **S4** said fd 10.5.2 can't be verified. It can, via crates.io and Debian (see S3).

### Next on the build machine

- Once fixes for C1, C2, G1 and F1 to F4 are on `testing`, ask in the handoff. With the owner's OK, I'll rerun passes C and D and add a report here.
- G1 (cairo or Wayland-only), F5 (official branding) and F7 (`firefox` or `firefox-esr`) need the owner's decision first. G2 and the locale bug are in the base.

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
