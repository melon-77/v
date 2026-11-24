# Contributing to Valence (instructions for agents)

This file is for AI agents (and humans) who work on Valence. Read it fully before changing anything.
`CLAUDE.md` points here.

## What Valence is

A from-scratch, rolling, x86_64 Linux distribution. Every package is cross-compiled from upstream
source by our own scripts and shipped as a signed apk v3 package.

| Piece | Choice | Notes |
|---|---|---|
| libc | glibc 2.44 (`recipes/glibc`: `glibc`, `glibc-dev`, `glibc-locales`), with `libxcrypt` 4.5.2 (`libcrypt.so.2`) for `crypt()` | `C.UTF-8` is built in; other locales are made with `localedef` from `glibc-locales`. `ldd`, `getent`, `locale`, `iconv` and `ldconfig` come with `glibc`, which also ships `/etc/nsswitch.conf` and `/etc/ld.so.conf`. glibc no longer has `crypt()`: a recipe that calls it adds `libxcrypt-dev` to `makedepends` (`opendoas`). Only the base system is on glibc so far: see "State of the glibc switch" below |
| toolchain | GCC 15.2, binutils 2.46 | target triple `x86_64-valence-linux-gnu` (32-bit: `i686-valence-linux-gnu`); built by `scripts/toolchain.sh` (rule 8) |
| userland | the GNU tools: coreutils 9.12, findutils 4.11, grep 3.12, sed 4.10, tar 1.35, gzip 1.15, diffutils 3.12; with less 710, procps-ng 4.0.7, iproute2 7.3 and shadow 4.20 (`login`, `su`, `passwd`, `useradd`). BusyBox 1.37 for the rest | all of them are dependencies of `valence-base`. BusyBox links each of its commands into `/usr/bin` wherever no file exists (its post-install script and trigger); a package that ships the real command replaces the link, and the trigger puts the link back when that package is removed. BusyBox remains `/bin/sh`, the initramfs, the small daemons (getty, udhcpc, mdev, syslogd, klogd, crond, ntpd) and some 250 commands nothing else provides (`vi`, `wget`, `ping`, `hostname`, `adduser`, ...). `shadow` is built without PAM: `/etc/login.defs` governs logins, and new password hashes are yescrypt (see "State of the glibc switch" and rule 61) |
| shell | bash 5.3 (login shell), BusyBox ash is `/bin/sh` | |
| privileges | doas (OpenDoas 6.8.2) with a `sudo` command on top | members of `wheel` |
| init | runit 2.3 | stages in `/etc/runit/{1,2,3}`, services in `/etc/sv`, enabled = symlink in `/var/service`; stays Valence's init (see Roadmap) |
| devices | eudev 3.2 on the desktop profile (`udevd` service), BusyBox mdev (`mdevd` service) on the console profile | the desktop profile swaps `mdevd` for `udevd`; see rule 44 |
| packages | apk-tools 3.0.8 | repo index `Packages.adb`, signed with `keys/valence-signing.rsa` |
| documentation | `<pkg>-doc` (man pages, info pages, docs) and `<pkg>-lang` (translations), made by the builder; `docs` and `lang` bring them for everything installed; mandoc 1.14.6 is `man` | not on the ISOs: a fresh install has no documentation files. `apk add docs`, or one `<pkg>-doc`, which brings mandoc. "Recipe format" has the mechanism |
| kernel | Linux 7.0, `linux-valence` (generic) | config = `x86_64_defconfig` + `recipes/linux-valence/config-valence` |
| boot | GRUB 2.14, both `i386-pc` and `x86_64-efi` | ISO and installs boot on BIOS **and** UEFI; the installed system's boot menu is hidden (skipped after 1 s, Shift or Esc opens it) unless another system is found, then it shows for 5 s |
| disks | GPT: 1 MiB BIOS boot, 1 GiB FAT32 `/boot` (also the ESP), Btrfs for the rest, in subvolumes: `@` is `/`, with `@home`, `@log` (`/var/log`), `@snapshots` (`/.snapshots`) and `@swap` beside it | kernel boots with `root=PARTUUID=... rootflags=subvol=@`; only encrypted (LUKS) installs have an initramfs (rule 28); apk takes a snapshot of `@` before every change, the boot menu starts any snapshot and `valence-snapshot rollback` makes one the default ("Snapshots and rollback"); dual boot next to Windows on UEFI (see Installers) |
| filesystem | merged `/usr`: `/bin`, `/sbin`, `/usr/sbin` -> `usr/bin`, `/lib` -> `usr/lib`; on x86_64 also `/lib64` -> `usr/lib` and `/usr/lib64` -> `lib` (`valence-layout`) | packages must only ship files under `/usr`, `/etc`, `/var`, `/boot`. All libraries, the loader included, live in `/usr/lib`; `lib64` is only a link, there because the x86_64 ABI names the loader `/lib64/ld-linux-x86-64.so.2` |
| desktop | KDE Plasma 6.6 on Wayland (KWin, Xwayland), Qt 6.10, SDDM | desktop ISO and desktop profile; list in `scripts/desktop-packages.txt` |
| 32-bit edition | i686 (`VALENCE_ARCH=x86`, `-march=pentium-m -mfpmath=sse`: SSE2, as Qt 6 needs), LXQt 2.4 on Wayland with labwc 0.20 (wlroots), SDDM, Mesa with LLVM built for the X86 backend only (i915, crocus, r300, r600, nouveau, llvmpipe, softpipe) | **not built on glibc**, and whether Valence keeps this edition is undecided (ROADMAP.md); as inherited from Melon: the owner's decision (7 October 2026): LXQt is the 32-bit desktop, for netbooks such as the MSI Wind U100 (Atom N270, 1–2 GB); drivers for every netbook-era Wi-Fi, Ethernet, graphics and laptop chip in `recipes/linux-valence/config-valence-x86` (the owner: "it should just work"; Broadcom b43 cards still need firmware nobody may redistribute); package list `scripts/desktop-packages-x86.txt`, recipes from `scripts/gen-lxqt-recipes.py` plus `recipes/valence-lxqt` (Valence's LXQt defaults); no Plasma, Flatpak, NVIDIA or QEMU guest agent there (QEMU 11 doesn't build for 32-bit hosts) |
| desktop plumbing | D-Bus, elogind, polkit, PipeWire + WirePlumber, NetworkManager, BlueZ, CUPS, UDisks2 | console profile keeps `dhcp` + wpa_supplicant |
| graphics | Mesa 26.0 with LLVM: radeonsi/RADV, iris/ANV, nouveau, llvmpipe; zink (OpenGL on Vulkan) | |
| developer tools | gcc 15.2 + g++, binutils 2.46, make 4.4.1, pkgconf, patch (`gcc`, `g++`, `binutils`, `make`, `pkgconf`, `patch`) | built cross-native with the cross toolchain's settings (PIE, SSP); in the package repository only, not on the ISOs; `valence-first-boot` offers them on first login (default no) |
| build tools | perl 5.44, m4, bison, flex, gawk, gperf, GNU bc, texinfo, autoconf 2.73, automake 1.19, autoconf-archive, file; cmake 4.4, meson 1.12, ninja 1.13, git 2.56, nasm 3.01, tcl 8.6, rsync 3.5, lz4; xorriso, mtools, scdoc, itstool, dtc, pahole (`dwarves`), rpcgen (`rpcsvc-proto`), the Public Suffix List, Python's mako, Jinja2, pyparsing, PyYAML, packaging, pexpect and libxml2 bindings (`python3-*`) | what Valence's recipes need to build on Valence itself (self-hosting, see Roadmap); package repository only. gawk and GNU bc take over BusyBox's `awk`/`bc`/`dc` links; BusyBox's `/usr/bin` trigger puts them back when those packages go |
| compilers, VMs | clang 21 (+ `libclc`, `spirv-llvm-translator`), Go 1.27, gh (`github-cli`), QEMU 11.1 + OVMF (`qemu`, `ovmf`) | package repository only. clang: Mesa's OpenCL C shader compiler on Valence; QEMU + OVMF run Valence's own install tests on Valence; OVMF is Ubuntu 26.04's prebuilt `ovmf-generic` at Ubuntu's paths |
| NVIDIA | the open driver: nouveau + `linux-firmware-nvidia` (GSP 570.144 from upstream linux-firmware) + NVK (`mesa-nvk`), with zink for OpenGL; or NVIDIA's own kernel driver: `nvidia-open` 615 (open modules + NVIDIA's firmware) | package repository only; `valence-first-boot` asks which one on computers with an NVIDIA card (GTX 16/RTX 20 and newer). With `nvidia-open`, only Flatpak apps (Steam) get NVIDIA's userspace libraries, from Flathub. On Melon that was the only way, because those libraries need glibc; on glibc the system itself could use them, but Valence has no package of them and none of this has been tested on glibc |
| gaming | Flatpak 1.16 + Flathub, GameMode | `valence-first-boot` offers Steam (and Firefox, VLC, Prism Launcher) from Flathub on first login. Steam is built for glibc, so on Melon Flatpak was the only way to run it; on glibc a native Steam is possible but untested, and it also needs 32-bit libraries (undecided, ROADMAP.md) |
| game library | SDL3 3.4 + SDL3_image + SDL3_ttf (`sdl3`, `sdl3-image`, `sdl3-ttf`), plus SDL2 compatibility stack (`sdl2-compat`, `sdl2-image`, `sdl2-ttf`, `sdl2-mixer`) | package repository only; SDL dlopen()s its Wayland/X11/audio backends |
| apps and games | Cataclysm: DDA (`cataclysm-dda` + `cataclysm-dda-tiles`) and GNU gettext (package repository only) | what goes on an ISO follows the size rule in "Contributing" |

Owner's config (`CONFIG_*` answers) lives in `docs/config.txt`. Don't change those choices without the owner's approval.

To build on another machine (Ubuntu 24.04 or WSL2, Debian or Devuan through the same apt path, or Valence), follow
`BUILDING.md`: `scripts/host-setup.sh`, then `scripts/build-everything.sh`. Scripts find the repo from their own
location; recipes use `$M`. Never write a machine's absolute path into a script or recipe. A checkout's `tools/` and
`sysroot/` are tied to its path (the cross compiler has `--with-sysroot=<checkout>/sysroot` built in): **two sessions must
not share one checkout** (one switching branches under the other's build). Give each its own `git worktree` with its own
toolchain, sysroot and `repo/` ("Windows + WSL2 build machine" in `BUILDING.md` says how). Run scripts by their absolute
path (rule 60). The signing key can stay where it is:
`VALENCE_SIGN_KEY=/path/to/valence-signing.rsa` is honoured by `valence-build` and `mkiso.sh`.

### State of the glibc switch

Valence's C library was musl, as in Melon, and is now glibc (ROADMAP.md, phase 2). **Every recipe builds on it:**
`scripts/valence-build` built all 451 from an empty repository in one run (ROADMAP.md, phase 4). Two editions are
made of them and pass their tests (ROADMAP.md has the status): the console system, which is the cross toolchain and
the 58 recipes the console ISO is made of, the GNU tools among them (with mandoc, `docs` and `lang` beside them,
for its documentation); and the Plasma desktop (`--edition desktop`, 345 recipes with the console's: LLVM and Mesa,
Qt 6, KDE Frameworks, Plasma, SDDM, PipeWire, Flatpak, CUPS, Calamares and the applications).

The other recipes (106 when phase 4 ended, 122 on 2026-11-21 counted the way given below) are on no ISO: the
developer tools (binutils, gcc, clang, cmake, meson, ninja, git, perl, Rust,
Go, qemu, ...), the LXQt desktop that Melon's 32-bit edition used (LXQt, labwc, wlroots and their libraries, built
here for x86_64), and what Melon's online repository offered beside the ISOs (nvidia-open, linux-firmware-nvidia,
mesa-nvk, xfsprogs, ...). Compare `ls recipes` with `scripts/valence-build --edition desktop --order`
for the list. Phase 4 took the musl workarounds out of them and gave them their `makedepends`, and checked the
result three ways: `scripts/compare-with-index.py` against Melon's packages (what differs is meant, and the recipe
says why); `scripts/check-dev-deps.py`; and a root made of the packages with apk, in which gcc, g++, clang, rustc,
cargo, Go (with and without cgo), perl, python3, cmake, meson, git, ripgrep, gh, bison, nasm, rsync and the others
each did a small job, and none of the 1644 programs and libraries in it lacked a library (all three checks run
again on the repository the from-empty run made).
**That is all the testing they have had.** No QEMU test adds a compiler to an installed Valence and builds with it,
LXQt has not been started, and the NVIDIA packages have never met a card.

Not converted yet, or not checked:

- **Self-hosting.** "Valence as the build machine" in BUILDING.md, rule 53 and `valence_deps` in `host-setup.sh` were
  written for a musl system and are untested on glibc. The tools it needs build on glibc now (above); building
  Valence on Valence has not been tried.
- **The 32-bit edition is dropped for now** (the owner, 21 November 2026; ROADMAP.md "Decided"): not built on glibc,
  not tested; `VALENCE_ARCH=x86` and its lists stay in the tree, unmaintained.
- **shadow has no PAM.** The base system has no linux-pam, so `recipes/shadow` is built without it and a console
  login does not go through PAM (BusyBox's login never did either). The desktop is converted now and linux-pam is in
  every desktop install; whether it moves into the base, with shadow rebuilt on it, is the owner's to decide
  (ROADMAP.md, "Not decided yet").
- **Account tools in other recipes.** Both installers use shadow's `useradd`, `usermod` and `chpasswd` (Calamares
  through its users job; the BusyBox-backed `shadow-shim` it needed on Melon is gone). The install scripts of dbus,
  polkit, cups, avahi, openssh and valence-desktop still call BusyBox's `adduser` and `addgroup` (BusyBox stays
  installed, so they work).
- **Extended attributes.** `valence-install` copies the system with `tar --xattrs --xattrs-include='*'` (valence-base
  44), so file capabilities would survive the copy. No package on either ISO has any (`getcap -r` on the desktop's
  system image finds none), so nothing tests that yet.
- **Docs and translations.** No recipe deletes them any more: the 451 recipes make 158 `-doc` and 97 `-lang`
  packages. The largest package of any kind is 88 MB (linux-firmware-intel-wifi), under GitHub's 100 MB limit
  ("Package repository"). All 1102 packages go into one root together (apk, without their scripts, 20 November
  2026): nothing is refused and no file belongs to two packages. Not looked for: a program that reads its own files
  from `usr/share/doc` at run time (it needs `options=(keepdocs)`).
- **Sources still from Ubuntu's archive.** "Sources" says a recipe that is touched moves to its upstream tarball.
  Phase 6 changed 153 recipes (their `makedepends`, mostly) and moved none: the KDE, Qt and X.Org recipes still build
  from Ubuntu 26.04's `orig` tarballs. That is its own piece of work (about 150 downloads to verify).

Where this file describes the parts that are not converted, it describes them as they were built on Melon.

## Repository layout

```
scripts/env.sh          paths: M, TARGET, TOOLS, SYSROOT, SRC, WORK, REPO; LDSO, the loader's file name
scripts/toolchain.sh    cross toolchain: binutils, kernel headers, GCC and glibc, in one resumable script (rule 8)
scripts/valence-build   build packages in the order the recipes' own dependencies give, and only what changed: every
                        recipe, the ones named (with what they need), or an edition (--edition console|desktop)
scripts/valence-build-recipe   the step it runs for each: ONE recipe -> signed .apk(s) in repo/x86_64, built in a
                        sysroot that holds what the recipe declares and nothing else; reindex
recipes/                the package recipes: a git submodule, github.com/melon-77/vpkg (branch main), since
                        21 November 2026; a recipe change is a commit there, and the pointer is bumped here
scripts/console-packages.txt   what the console system is made of (mkiso.sh and --edition console read it)
scripts/valence-run     start a program built for Valence on the build machine, through the sysroot's loader (rule 18)
scripts/elf-unbake.py   take the sysroot's loader and library path out of what an options=(runhere) recipe linked;
                        with --check, refuse any package that still names them (rule 18)
scripts/test-console.sh the eight console tests on an ISO ("Testing"); scripts/test-desktop.sh the three desktop ones
scripts/release.sh      a release's files from the tested ISOs: renamed ISOs, SHA256SUMS, its signature, the key
scripts/suggest-makedepends.py   what a recipe's source asks for, as -dev packages; writes scripts/kde-deps.json,
                        the generated KDE recipes' makedepends (rule 67)
scripts/compare-with-index.py    what the built packages link, against another index: finds a feature that a missing
                        makedepends switched off without an error (rule 67)
scripts/check-dev-deps.py        does every package bring what its CMake files and library links name? (rule 68)
scripts/mkiso.sh        live/installer ISO: rootfs.sqfs (pristine apk-installed system, copied to disk by the
                        installers) + live.sqfs (small live-session layer) + a small repo of extras (VM guest tools,
                        a few base packages; never a second copy of what rootfs.sqfs already holds)
scripts/qemu-test.py    headless boot + install + reboot test over the serial console
scripts/qemu-gui.py     look at a VM's screen, click and type, by hand: how Calamares is tested ("Testing")
recipes/<name>/VALENCEBUILD   one directory per recipe (see below)
recipes/<name>/*.patch      applied automatically with patch -p1, in name order
recipes/<name>/<pkg>.post-install etc.   apk scripts for (sub)package <pkg>
iso-files/init          live initramfs /init
branding/               the logo: valence-logo.svg and transparent PNGs, written by art/logo.py
art/logo.py             the logo: a folded V in five greys (the owner's drawing; `--check` compares it with the
                        checksums of the files the owner approved). `art/logo.py --export branding` needs cairosvg
art/desktop.py          the desktop's pictures (plain black: wallpaper, splash, boot menu background), the logo's
                        copies in recipes (icons, Calamares branding) and the colour scheme (the logo's greys).
                        `art/desktop.py --export` writes them into valence-desktop and calamares-valence
ROADMAP.md              the plan: what Valence keeps from Melon, what changes, the phases and their "done when"
recipes/calamares-valence/    graphical installer: Calamares settings, branding, job configs, finishing scripts (stage 2)
```

Not in git (see `.gitignore`): `sources/`, `work/`, `tools/`, `sysroot/`, `hosttools/`, `repo/`, `out/`,
`logs/`, and **the private signing key**. Never commit `keys/*.rsa`. Only `keys/*.rsa.pub` is committed.

## Recipe format (VALENCEBUILD)

A bash fragment sourced by `scripts/valence-build-recipe`. Minimal example:

```bash
pkgname=zlib
pkgver=1.3.1
pkgrel=0                     # bump when you change the recipe but not the version
pkgdesc="A compression/decompression library"
license=Zlib
url=https://zlib.net
source=(zlib-1.3.1.tar.gz)   # file names inside $SRC (sources/), or files in the recipe dir
subpackages=(zlib-dev)       # -dev has a default split function; -doc and -lang are made without being listed
makedepends=()               # what the build needs: with the build base, the only packages in its sysroot
depends=()                   # runtime deps; so: deps are added automatically from ELF NEEDED
build(){ ./configure --prefix=/usr --libdir=/usr/lib --shared >/dev/null; make -j$JOBS >/dev/null; }
package(){ make DESTDIR=$pkgdir install >/dev/null; }
```

Things the builder gives you:
- `$CC`, `$CXX`, etc. already point at the cross toolchain. `$conf_flags` holds the standard autoconf flags
  (`--host`, `--prefix=/usr`, `--sbindir=/usr/bin`, ...). `meson_setup` wraps meson with our cross file.
- `$srcdir`, `$builddir` (default `$srcdir/$pkgname-$pkgver`), `$pkgdir`, `$startdir` (recipe dir), `$subpkgdir`.
- Split functions: `pkg_<suffix>()` for subpackage `<pkgname>-<suffix>`, or `pkg_<name>()` for a
  differently named subpackage. Non-alphanumerics become `_` (e.g. `libstdc++-dev` -> `pkg_libstdc___dev`).
  Use `amove path...` (globs allowed, relative to `$pkgdir`) to move files into the subpackage.
- Per-subpackage variables use the same mangling: `depends_<x>`, `pkgdesc_<x>`, `provides_<x>` (array).
- Triggers: `triggers_<x>=(dir ...)` plus a `<pkg>.trigger` script; apk runs the script (with the changed
  directories as arguments) after any transaction that touches those directories, and when `<pkg>` itself is
  installed. Use them for caches other packages feed, e.g. `recipes/shared-mime-info`.
- `cmake_setup <args>` is the CMake cross build. The programs such a build (or a meson build) makes and then runs
  start through `scripts/valence-run` (rule 18); a recipe that runs a built program by hand calls
  `$M/scripts/valence-run PROGRAM ARGS...` itself.
- `cmake_native <args>` configures CMake as a native build with Valence's compilers and sysroot, so that generators
  the build makes (clang's tablegen) are started directly. It needs `options=(runhere)`, which is what lets them run
  (rule 18), and passes that option's link flags to CMake. `recipes/clang`, `libclc` and `spirv-llvm-translator`
  call it; everything else is a cross build with `cmake_setup`.
- `options=(runhere)`: what the recipe links can run on the build machine (rule 18). `options=(rust)`: the recipe
  puts Rust code into a package (rule 46).
- `arch=(aarch64)`: the only architectures the recipe is for (`$APK_ARCH` names: x86_64, x86, aarch64). Without it a
  recipe is for all. `valence-build` leaves such a recipe out when it builds everything for another architecture,
  and refuses it there by name or as a dependency (`recipes/linux-rpi`, `recipes/rpi-firmware`).
- `py_install <name> <dirs...>` installs pure-Python modules into python3's site-packages with a small `.dist-info`
  (`recipes/python3-mako`). Modules with C parts need their build system (`recipes/python3-libxml2`).
- `options=('!strip')` skips stripping. `options=(keepdirs)` keeps empty directories.
- **Docs and translations are split out by the builder, and a recipe never deletes them** (ROADMAP.md, phase 3).
  What `package()` leaves in `usr/share/{man,info,doc,gtk-doc,help}` goes into `<pkgname>-doc`, and
  `usr/share/locale` into `<pkgname>-lang`. Each exists only when there is something to put in it, and neither is
  installed into the sysroot. A recipe lists `<pkgname>-doc` in `subpackages` only to give it a `pkg_doc()` of its
  own (`recipes/perl`). Files a split function has already moved into another subpackage stay there
  (`glibc-locales` keeps `usr/share/locale`). `options=(keepdocs)` or `options=(keeplocale)` leaves the files in the
  main package, for a package whose content they are.
  Every `-doc` package carries `install-if: docs <pkgname>=<version>`, and every `-lang` package the same with
  `lang`: with the `docs` package installed, apk adds the `-doc` part of everything installed and of whatever is
  installed later, and removes them all again with it (`recipes/docs`, `recipes/lang`; both are empty).
  A `-doc` package with man pages depends on `mandoc`, the reader (`man`, `apropos`, `whatis`): its trigger keeps
  the index `/usr/share/man/mandoc.db` current, and without that index mandoc's `man` warns on every page. BusyBox
  has a `man` of its own, which is what `/usr/bin/man` is while mandoc is not installed; it cannot format a page.
  `scripts/desktop-packages*.txt` list `lang`, so the desktop keeps its translations.
  Not covered: the recipes that switch documentation off when they configure (about 180: `-Dgtk_doc=false`,
  `--disable-doc`, ...), mostly because building it needs tools the build machine lacks; and the 41 that pass
  `--disable-nls`, which build no translations at all (the base tools among them: ROADMAP.md, "Not decided yet").
- After packaging, files in `/bin`, `/sbin`, `/lib`, `/usr/sbin` are folded into `/usr/bin` and `/usr/lib`,
  `.la` files are deleted, and ELF files are stripped.
- `so:` provides/depends are generated from SONAME/NEEDED. glibc's libraries carry a SONAME (`libc.so.6`), so the
  glibc recipe declares none by hand.
- `pc:` provides/depends are generated from pkg-config files the same way: a package provides `pc:<module>` for
  every `.pc` file it ships, and depends on the modules their `Requires` and `Requires.private` lines name. So
  `makedepends=(glib-dev)` brings pcre2-dev, libffi-dev, zlib-dev and util-linux-dev with it, because glib's `.pc`
  files ask for them.
- A `-dev` package also depends on the `-dev` packages of the libraries that the recipe's own libraries link (its
  programs do not count), as far as those were in the build's sysroot. Qt6Gui's CMake files ask for OpenGL's and
  xkbcommon's, a KDE framework's for the frameworks under it, and there is no `.pc` file to say so. What nothing
  links is still written by hand, as `depends_<x>_dev` (for a generated KDE recipe in `DEV_MORE` in the generator):
  another library's headers used alone, or a library that went into a sister subpackage. `scripts/check-dev-deps.py`
  finds those (rule 68).

### Building: the order, clean sysroots, and what counts as built

`scripts/valence-build NAME...` builds the recipes or packages named and whatever they need first; with no name it
builds every recipe; `--edition console` (or `desktop`) builds what that ISO is made of. `-n` shows what it would
build and why, `--order` prints the order, `-f` builds the named ones even if nothing changed, `-x` stops at the
first failure (otherwise it carries on with everything that does not need the failed recipe, and lists at the end
what failed and what waited for it). Each recipe's output is in `logs/pkg-<recipe>.log`.

- **No list says in which order recipes are built.** A recipe is built after the recipes that make its
  `makedepends` and the build base, and after the recipes that make whatever those packages depend on (declared
  `depends`, and a `-dev` package's main package), since apk installs all of that into the build's sysroot. Recipes
  that need each other are reported and nothing is built. What apk goes by and no recipe declares (the libraries
  a package links, the modules its `.pc` files require) is only known once the package is made; rule 70 is what
  keeps that from mattering.
- **Clean builds (the owner's decision, 19 November 2026).** `valence-build-recipe` sets the sysroot up for every
  build: the build base (`BUILD_BASE` there: `glibc-dev libgcc libstdc++`) and the recipe's `makedepends`, with the
  packages those depend on, and no other package. apk works out the difference from the last build's sysroot
  (`apk upgrade --available` on a rewritten world file), so only what differs is unpacked or removed. A library the
  recipe does not name is not there: the build fails, or configure reports the feature as off. Name every library a
  recipe uses in `makedepends`, and give optional features their explicit switch (rule 63).
- **`options=(bootstrap)`** marks the recipes the build base is made from (`valence-layout`, `linux-headers`, `glibc`,
  `gcc-runtime`). They are built first, on the C library `toolchain.sh` put into the sysroot, which is left as it
  is for them; `valence-build` refuses to start if a package of the build base comes from a recipe without it.
- **What counts as built.** `repo/<arch>/.state/<recipe>` records the recipe's fingerprint (every file in its
  directory), the builder's `BUILD_EPOCH`, the version and the package files. A recipe is built when that record is
  missing, when the fingerprint or the epoch differs, or when one of the files is gone. Run the same command twice
  and the second run builds nothing. Bump `BUILD_EPOCH` in `valence-build-recipe` when a change to it alters what
  packages contain: every recipe is then built again. Recipes that depend on a rebuilt recipe are **not** rebuilt
  by themselves; bump their `pkgrel` when they have to be (a library's SONAME changed).
- **One build at a time** in a checkout (rule 29).

`scripts/valence-build-recipe <recipe>` runs the one step by hand: the recipe is built whatever its record says, and
recorded as built "by hand", so `valence-build` builds it again the next time.
Rebuilding the kernel takes about an hour on 2 cores.

## Hard-won rules (each one broke the build once)

1. **Never run build steps as `yes | cmd` under `set -o pipefail`.** `yes` dies with SIGPIPE and the recipe
   aborts silently. Use `yes "" | cmd || [ -f expected-output ]`.
2. **Host tools must not see the sysroot.** Anything built with the host compiler (kernel `scripts/`,
   `certs/extract-cert`, GRUB's firmware code) must run with `PKG_CONFIG_SYSROOT_DIR`, `PKG_CONFIG_LIBDIR`,
   `CFLAGS` and `LDFLAGS` unset (see `_kmake` in the kernel recipe and `_tgt` in the GRUB recipe).
   Otherwise it links against the sysroot's libcrypto, which is built for Valence and not for the build machine (on
   Melon that failed with "invalid ELF header").
3. **GRUB firmware code gets `TARGET_CFLAGS=-Os` and empty `CFLAGS/LDFLAGS`.** Our `-fno-plt` produces GOT
   references that make `moddep.lst` fail.
4. **ncurses is built without a separate libtinfo.** `libtinfo.so` is a symlink to `libncursesw.so`.
   A split tinfo breaks readline and bash with `--as-needed`.
5. *(A musl rule, kept so the numbering holds: `bsd-compat-headers` for `sys/cdefs.h`. glibc has `sys/cdefs.h` and
   `sys/queue.h`, and that recipe is gone.)*
6. **Bump `pkgrel` whenever you change a recipe**, or the change won't reach users: apk only replaces a package
   whose version-release changed. (The build's own sysroot no longer depends on that: it is set up with
   `apk upgrade --available`, which also replaces a package rebuilt under the same version. `valence-build` says
   so when a recipe changed and its version did not.)
7. **Install scripts must not create config files that another package ships.** apk would then keep the
   script's version and write the package's as `*.apk-new`. See `recipes/bash/bash.post-install`.
8. **GCC's 64-bit target libraries install to `tools/x86_64-valence-linux-gnu/lib`**, not `lib64` (which is only a
   link on Valence). `scripts/toolchain.sh` is one script, and it builds in the order a glibc cross toolchain needs:
   binutils, the kernel's headers, the compiler alone, glibc's headers and start files, GCC's headers again plus
   libgcc, glibc, then the rest of GCC. Each step leaves a stamp in `work/toolchain`, so an interrupted run resumes;
   delete `work/toolchain` to start over. Two traps, both explained in the script's comments: GCC makes its own
   `<limits.h>` before glibc's header exists, so `PATH_MAX` goes missing unless GCC's headers are made again once
   glibc's are installed (`rm b-gcc/gcc/stmp-int-hdrs`); and glibc's parallel `make all` can race on
   `linkobj/libc_pic.a`, so `make lib` runs first (`recipes/glibc` does the same). libstdc++ keeps its symbol
   versions (`--disable-symvers` was dropped), and libquadmath is built.
9. Anything that runs cross-built binaries on the build host starts them through `scripts/valence-run` (rule 18).
10. **Module handling uses kmod, not BusyBox.** BusyBox's `depmod -b` produced a modules.dep that
    `modprobe` couldn't resolve, so no drivers loaded. The kernel package runs `depmod -a` (kmod) in its
    post-install, and `kmod` is a dependency of `linux-valence`.
11. **The build host needs `zstd`** (the kernel compresses bzImage with it), `mtools`, `xorriso` and
    `squashfs-tools`.
12. **GRUB on a serial console:** `terminal_output gfxterm` replaces the serial output unless `serial`
    is listed too. `valence-update-grub` handles this when `GRUB_SERIAL=1` is set in `/etc/default/grub`.
13. **`valence-update-grub` is given the root and boot devices explicitly** by the installer
    (`VALENCE_ROOT_DEV`, `VALENCE_BOOT_DEV`). Guessing them from inside a chroot picked the wrong filesystem UUID.

14. **Services never write to the console.** Every `/etc/sv/<svc>/run` starts with `exec 2>&1`, and
    anything chatty has a `log/run` that pipes it to `svlogd` in `/var/log/<svc>/`. A crash-looping
    service that prints to the console floods the screen (this happened with wpa_supplicant).
15. **Only pass options the binary was built with.** wpa_supplicant's `-s` needs `CONFIG_DEBUG_SYSLOG=y`.
    Without it, every start printed the help text.
16. **Shutdown unmounts `/boot` explicitly** (see `/etc/runit/3`). BusyBox's `umount -a -t no...` left the
    FAT `/boot` dirty.
17. **`sudo` is a small front end for doas** (`recipes/opendoas/sudo`). Rules live in `/etc/doas.conf`
    (`permit persist :wheel`). New users get their shell setup from `/etc/skel`.
18. **Programs built for Valence start through `scripts/valence-run` on the build machine.**
    `scripts/valence-run PROGRAM ARGS...` starts a program with the sysroot's loader and libraries
    (`$SYSROOT/usr/lib/$LDSO --library-path $SYSROOT/usr/lib`; `env.sh` exports `LDSO`). PROGRAM is a path to the
    file: the loader does not search `PATH`. The reason: the build machine's glibc (2.39 on Ubuntu 24.04) is older
    than Valence's, and a program started directly would get the build machine's loader and libraries.
    `valence-build` gives the wrapper to meson (`exe_wrapper` and `needs_exe_wrapper = true` in the cross file) and to
    CMake (`CMAKE_CROSSCOMPILING_EMULATOR` in the toolchain file); a recipe that runs a built program by hand must
    call it through `$M/scripts/valence-run`. CMake uses that only for programs the project itself builds: the
    programs of packages in the sysroot (moc, kconfig_compiler, wayland-scanner) are wrapped by the builder (rule
    66). Autoconf builds are cross builds and don't run test programs: their
    answers are in `scripts/config.site` (rule 25). On 64-bit ARM (`VALENCE_ARCH=aarch64`,
    ROADMAP.md phase 9) the wrapper starts them inside QEMU's user-mode emulator (`VALENCE_EMU` in `env.sh`, Ubuntu's
    qemu-user), and package scripts run in an aarch64 root through the kernel's binfmt registration
    (qemu-user-static, with the F flag, so it works in a chroot); `host-setup.sh` installs both.
    **`options=(runhere)` is for a build that runs what it has just built and cannot be handed a wrapper for it:**
    perl's Configure and its miniperl, bc's `fbc`, `file` compiling its magic, clang's tablegen in a build configured
    as native (`cmake_native`), Rust's build running the compiler it has made. Everything such a recipe links gets
    the sysroot's loader as its program interpreter and the sysroot's library directory as an RPATH
    (`RUNHERE_LDFLAGS`, which the builder appends to `$CC` and `$CXX`; the old kind of RPATH, which also holds for
    what those libraries need in turn). Its programs then run on the build machine like the machine's own. Before a
    package is made, `scripts/elf-unbake.py` puts the system's loader back and takes the RPATH out. **And no
    package is made, from any recipe, while a file in it still names the sysroot's loader or library path**: that
    check runs on everything, and the first thing it caught was a plain `-rpath` that bison's configure had added
    for a library it was pointed to in the sysroot (`--disable-rpath`). A recipe with the option still has to clean
    what its build wrote down as text: perl records the compiler's command line (rule 48). The option is for the
    case where build machine and target are the same kind of machine; a second architecture (the planned ARM
    edition) needs another answer for these recipes.
    Before a program built for Valence could simply fail to start here, it often ran by accident: built against
    glibc 2.44 it starts with the build machine's loader and older glibc as long as it uses nothing newer, and then
    loads the build machine's libraries. bc, file, scdoc and rpcsvc-proto built that way in phase 4's first run.
    That is not a reason to leave the option off.
    Melon pointed `/lib/ld-musl-x86_64.so.1` and `/etc/ld-musl-x86_64.path` at the sysroot, so that such programs
    just ran. That is gone: `host-setup.sh` removes those links, `MUSL_LDSO` became `LDSO`, and `MUSL_ARCH` is gone
    (recipes test `$APK_ARCH`).
19. **`meson_setup` drops `-D` options the project doesn't have** (`scripts/meson-filter-opts.py`) and
    unsets the `PKG_CONFIG_*` variables for native lookups. Upstream renames options often; a stale
    option must not fail the whole build.
20. **Some generators must come from the host, not the sysroot:** `wayland-scanner` 1.24 in
    `/usr/local`, Mesa's `mesa_clc`/`vtn_bindgen2` in `hosttools/bin` (built against host LLVM), and the
    Qt 6 host tools in `hosttools/qt6` (`scripts/host-qt.sh`, same Qt version as the target, passed as
    `QT_HOST_PATH`). The host also needs `libltdl-dev` (libffi's autoreconf). Meson only finds programs for a
    cross build in the cross file's `[binaries]`, never on `PATH`, so `valence-build` lists the host Qt tools
    (moc, uic, rcc, ...) and `bwrap` there.
21. **Never kill build processes with `pkill -f <pattern>`** when your own shell's command line contains
    the pattern: it kills your shell too. Find the PID and kill that.
22. **Edit scripts that may be running (`valence-build-recipe`, `mkiso.sh`) through a temporary file
    and `mv`.** bash reads scripts while running them, so an in-place edit corrupts a running build.
23. **Never type keys or checksums from memory.** The Flathub key is downloaded from Flathub's own
    `.flatpakrepo` at first use.
24. *(A musl rule, kept so the numbering holds: `libc.musl-<arch>.so.1` let Alpine's packages run on Melon. They are
    built for musl and cannot run on glibc, and the opt-in Alpine repository is removed.)*
25. **Cross autoconf answers live in `scripts/config.site`** (`CONFIG_SITE`). Without them `AC_FUNC_MALLOC`
    assumes a broken malloc and the library ends up with undefined `rpl_malloc` (libndp).
26. **`msgfmt` needs `GETTEXTDATADIRS`** pointing at the sysroot's `/usr/share/gettext` to find the ITS
    rules (polkit policies, GSettings schemas) that earlier valence packages installed (upower).
27. **Host packages the build needs** besides the toolchain: `tcl` (sqlite's amalgamation), `hwdata`
    (libdisplay-info reads `pnp.ids` at build time), `publicsuffix` (libpsl's built-in list), `autopoint`
    (cryptsetup's autoreconf), `libltdl-dev`, `libxml2-utils` (shared-mime-info runs `xmllint`), `appstream` +
    `libappstream-dev` + `itstool` (appstream's metainfo), `nasm` (FFmpeg's x86 assembly), `bubblewrap` (meson checks
    for `bwrap` in flatpak and xdg-desktop-portal).
28. **Only encrypted installs have an initramfs.** `valence-mkinitramfs` builds it when
    `/etc/valence/encrypted-root` exists; `valence-update-grub` then writes `cryptroot=UUID=<luks>
    root=/dev/mapper/valenceroot` and an `initrd` line. Everything else still boots straight from the kernel.
    The initramfs holds BusyBox, cryptsetup and kmod with the glibc loader and the libraries they need (`/lib64` is a
    link in it too), and `libgcc_s.so.1`, which glibc loads by name when a thread exits or is cancelled (rule 64). The
    live ISO's initramfs (`mkiso.sh`) copies the loader and the libraries BusyBox needs the same way.
    Its init takes `rootfstype=`, `rootflags=` (the Btrfs subvolume) and `init=` from the kernel command line the way
    the kernel itself does: the boot menu's snapshot entries name a snapshot and their own init with them.
30. *(A musl rule, kept so the numbering holds: musl 1.2.4+ hid the `*64` LFS types unless `_LARGEFILE64_SOURCE`
    was defined, so gpgme got `-D_LARGEFILE64_SOURCE`.)*
31. **apk only replaces a package whose version-release changed** (rule 6). Generated
    recipes take their `pkgrel` from the `PKGREL` table in `scripts/gen-simple-recipes.py`, plus `REBUILD`, which is
    added to every recipe that generator writes (`gen-kde-recipes.py` has the same pair; `gen-lxqt-recipes.py` one
    number for all).
29. **One build at a time per checkout and architecture.** Every build sets the sysroot up for itself (clean
    builds), so two at once would pull it out from under each other: `valence-build-recipe` holds
    `work/.build.lock` and refuses to start a second one. For builds side by side, use a second checkout
    (`git worktree`, BUILDING.md). Packages are still written under a temporary name first, and the index is
    rebuilt under `flock repo/<arch>/.lock`.

32. **pkg-config puts the sysroot in front of path *variables*** (`xkb_base`, `dridriverdir`, dbus and polkit
    dirs, ...). Builds then either install under `/home/.../sysroot/...` (valence-build moves those files back and
    logs `LEAK: files installed under ...`) or compile the build machine's path into a program (logged as
    `LEAK: build-machine path compiled into ...`). **Read the LEAK lines after every build**: compiled-in paths
    need an explicit option (`-Dxkb_bin_dir=/usr/bin`, elogind's `-Ddbuspolicydir=...`) or a patch that strips
    `meson.get_external_property('sys_root')` (libxkbcommon, Xwayland). Paths in dev files (`.pri`, sbom) are harmless.
33. **Runtime dependencies nothing links against are not found automatically**: data (xkeyboard-config), QML
    modules, programs started by name (kded6, xkbcomp), Qt platform plugins (qt6-qtwayland). Declare them in
    `depends=` or list them in `scripts/desktop-packages.txt`.
34. **runit readiness:** `sv` defaults to `/service`, so `SVDIR=/var/service` is set in stage 2 and `/etc/profile`.
    `sv check <svc>` only means "the process exists" unless the service has a `./check` script; services others
    wait for ship one (dbus: bus socket exists; elogind: seat0 is set up; polkitd: owns its bus name). Only runit
    starts elogind and polkitd (their D-Bus activation files run /bin/false): otherwise the first daemon to ask for
    them gets a D-Bus-activated copy, and the supervised one exits and restarts every second because the name is
    taken (polkitd did that on installed systems). Services that need them at startup wait with `sv check`.
35. **Image assembly installs BusyBox, then valence-base, then everything else** (`mkiso.sh`): install scripts need
    /bin/sh and adduser, and the users they add (messagebus, polkitd, sddm) must not be overwritten by valence-base's
    `/etc/passwd` arriving later.
36. **Build Qt QML (qt6-qtdeclarative) before anything with QML parts.** Qt modules and frameworks built without it
    silently leave their QML modules out (`Could NOT find Qt6Qml` in the log); rebuild them with a pkgrel bump.
37. **KWin needs `KWIN_BUILD_X11=ON`** even in a Wayland-only session: in 6.6 that switch also carries Xwayland,
    and startplasma starts KWin with `--xwayland`.
38. **SDDM's greeter runs on VT 7** (`-DSDDM_INITIAL_VT=7`). SDDM only counts a VT as busy when a logind session
    is on it, so with its default (VT 1) the greeter fights getty-tty1 for the terminal and crash-loops
    (`Failed to take control of "/dev/tty1"`, helper exit 5). Installed systems keep text logins on tty1-3.
    The live ISO has no getty on tty1 and never showed it: test desktop changes on an *installed* system
    (`qemu-test.py desktop-install`).
39. **`set -e` and `VAR=$(cmd)`: a failing `cmd` ends the script.** An assignment's exit status is its last command
    substitution. Probes that may find nothing (no sound card, no network) need `|| true` inside the `$( )`.
40. **GCC 15 compiles C23 by default**, where `void f()` means "no arguments". Old code and old configure tests
    that call such functions with arguments fail ("too many arguments to function"): build them with
    `CFLAGS="$CFLAGS -std=gnu17"` (gmp). Old `config.sub` files don't know `*-linux-musl`, so on Melon recipes copied
    the build host's (`/usr/share/misc/config.sub`, `config.guess`); libcanberra, libatasmart, libdaemon and flite
    still do, since those files do not know `x86_64-valence-linux-gnu` either. Tools a build runs on the build machine come from `CC_FOR_BUILD=gcc` when the project supports it.
41. **Absolute symlinks in `usr/lib` and `usr/include`** (`libfoo.so -> /usr/lib/libfoo.so.1`) point into the
    build machine's own `/usr` from the sysroot, so the linker can't find the library. valence-build makes them
    relative when packaging; a package built before that needs a pkgrel bump to reach the sysroot (rule 31).
42. **`.pc` files and `*-config` scripts copy pkg-config's sysroot answers** (`Libs.private: -L<sysroot>/usr/lib`).
    valence-build turns sysroot paths in `.pc` files into plain `/usr` paths; a `*-config` script needs its recipe to
    do the same (`recipes/cups`). The `LEAK:` lines name what is left.
43. **Kernel options with unmet dependencies vanish silently** in `olddefconfig` (the touchpad's
    `I2C_DESIGNWARE_PLATFORM` needed `I2C_DESIGNWARE_CORE`; `HP_WMI` needed `X86_PLATFORM_DRIVERS_HP`). The kernel
    recipe now stops when an option from `config-valence` isn't in the final `.config`: look its dependencies up in the
    Kconfig files and add them. The built config ships as `/boot/config-valence`; check it, not the fragment. For a
    real machine, `sudo valence-hwreport` lists every PCI/USB/ACPI/I2C device with the driver it got, the modaliases of
    devices that got none, graphics, network, sound, Bluetooth, power and service state, and the kernel's firmware
    messages; map IDs to modules with `modprobe -R <modalias>` against the built kernel. (`scripts/valence-diag.sh`, the
    old `curl | sh` tool, is now a wrapper that prints the same report.)
44. **Never restart udevd while devices are still appearing: events in the gap are lost.** Stage 1 runs a udevd for the boot
    coldplug. It used to stop it and let the runit service start a new one a moment later; a Wi-Fi card's interface
    appears only after its firmware loads (MT7921, iwlwifi, ath11k), often in that gap, so udev never processed it, kept the
    kernel name `wlan0` and NetworkManager left it "unmanaged" (the ProBook's MT7921). Now stage 1 leaves its udevd
    running and the `udevd` service adopts it (`recipes/eudev/udevd.run`: it waits for that process and stops it on
    TERM), so udev never restarts at boot. The service still replays the "add" events for net, ieee80211, rfkill and
    bluetooth devices when it has to start a fresh udevd (`sv restart udevd`, or the kernel option `valence.udev=sync`,
    which brings back the old stop-and-restart; `qemu-test.py desktop` covers that path).
    The two halves are in different packages, stage 1 in valence-base (`/etc/runit/1`) and the adoption in eudev
    (`udevd.run`), and valence-base doesn't pin eudev's version: **publish valence-base 0.1-r36 or later online only together
    with eudev 3.2.14-r2 or later.** With an older eudev the service starts a second udevd next to stage 1's, which can't
    start, so runit restarts it every second. The other mix is safe: a newer eudev with an older valence-base finds no udevd
    running and starts its own.
    Stage 1 no longer waits for udev to finish: it waits only for the devices in `/etc/fstab`, so **a service or a
    stage 1 step that needs a device other than a mounted disk must wait for it itself** (SDDM waits until elogind reports seat0 graphical, i.e. until the GPU driver is up).
45. **Packaged files must not keep the builder's account.** `cp -a $startdir/files/.` keeps the checkout's owner;
    apk records that account's name, and on a valence system without it the files become `nobody`'s. Service run
    scripts, `/etc/profile.d` and the installer's helpers were writable by `nobody`, then run by root. `valence-build`
    gives root every file owned by an account from 1000 up; system accounts a recipe sets on purpose (below 1000)
    stay. The desktop-install test fails if anything under `/usr` or `/etc` belongs to `nobody`.
46. **Rust recipes are compiled with the build machine's Rust** (`scripts/host-rust.sh` puts upstream's release
    binaries in `hosttools/rust`). valence-build points cargo at `$RUST_TARGET` and links with Valence's gcc.
    **On x86_64 that target, `x86_64-unknown-linux-gnu`, is also the build machine's own triple**, so cargo has one
    set of compiler and linker settings for two kinds of output: Valence's programs, and what runs on the build
    machine while they are built (build scripts, proc-macros, and the C code their crates compile for themselves).
    The two can only be told apart by where they are written: given `--target`, cargo puts the target's files under
    `<triple>/release` and the build machine's under `release`. So cargo's linker and the `cc` crate's compilers are
    `$WORK/rust-cc/cc` and `c++` (the builder's `rust_cc`), which look at their arguments and at `OUT_DIR` and run
    Valence's compiler for the first kind and the build machine's for the second; and `cargo_build` always passes
    `--target`, also where it names the machine's own triple. Without that, build scripts are Valence programs that
    run here by accident (rule 18), and proc-macros built against Valence's glibc are loaded into the build
    machine's rustc.
    `cargo_fetch` (in `prepare()`) downloads crates from crates.io into `sources/cargo`, pinned by the checksums in
    the project's `Cargo.lock`; `cargo_build` then builds offline with `--locked`, and `cargo_out` names the output
    directory. A build script that asks git for a commit hash would find Valence's own checkout: valence-build
    exports `GIT_CEILING_DIRECTORIES=$WORK` for every build (clang's `--version` had named Valence's repo). A `-sys`
    crate may build its own static copy of a C library when it is not told otherwise or does not find what it looks
    for: `PCRE2_SYS_STATIC=0` in ripgrep; and cargo's own curl crate links the system's libcurl only if a
    `curl-config` in `PATH` reports HTTP2 (on Melon's build machine the machine's own answered; `recipes/rust` puts
    the sysroot's first). `scripts/compare-with-index.py` shows such a copy as a library that is no longer linked.
    **Every recipe that puts Rust code into a package says `options=(rust)`**, so the Rust packages can be found
    from the recipes (`grep -l 'options=(.*rust' recipes/*/VALENCEBUILD`).
    `recipes/rust` builds Valence's own Rust the same way with Rust's `x.py`: stage 0 is `hosttools/rust`, the
    compilers it is given come from `rust_cc` with runhere's link flags for Valence's side, and `options=(runhere)`
    lets the build run the compiler it has made (rule 18). 17 to 19 minutes on 12 threads.

47. **Flatpak must be built with X11 authorization (`-Dxauth=enabled`, libXau).** Without it, sandboxed X11 apps get
    the host's `DISPLAY` and a path to an Xauthority file that doesn't exist inside the sandbox. Xwayland refuses them
    ("Authorization required, but no authorization protocol specified") and they never open a window: Steam and VLC
    "refused to launch". The desktop-install test checks that `/usr/bin/flatpak` links libXau.

48. **A configure that must run its test programs runs them here: `options=(runhere)`** (rule 18). perl's
    `Configure` gets `-Dcc=$CC` and the sysroot's `libpth`/`usrinc`, and afterwards the recipe takes the sysroot,
    the triple, runhere's link flags and the cross compiler's own search directories out of the installed
    `Config.pm`, `Config_heavy.pl` and `CORE/config.h`, so that modules built on Valence use `gcc` and `/usr`;
    `package()` stops if one of those files still names the checkout. The same option lets `file` compile its
    magic database with the `file` just built, bc run its `fbc`, scdoc write its own man page and rpcsvc-proto run
    its `rpcgen`. Perl's MakeMaker writes the directory of every library it finds into the module's RPATH through
    `LD_RUN_PATH`: `make LD_RUN_PATH=` keeps the sysroot out. CMake's own `try_run` checks start through the
    emulator, like every CMake build's (recipes/cmake). A library built without a SONAME (perl's, Tcl's) gets one on
    its link line, or the `so:` dependency on it can't be met.

49. **Valence's llvm-dev keeps only five static archives** (LLVMSupport, LLVMDemangle, LLVMTableGen, -Basic, -Common): the
    rest (about 1 GB) would pass GitHub's file limit, and everything links the shared libLLVM. Tablegen tools always link
    those five statically, and a standalone build against valence's LLVM (clang, recipes/clang) makes its own
    `clang-tblgen`. The recipe also drops the deleted archives from `LLVMExports-release.cmake`: `find_package(LLVM)`
    refuses to configure while an exported file is missing.
50. **Go records its defaults when it is built:** a set `CGO_ENABLED` becomes the permanent default, and the C compiler's
    name is baked in. recipes/go leaves `CGO_ENABLED` unset (cgo switches on by itself when a compiler is installed) and
    builds with `CC=gcc`, a wrapper that runs valence's cross compiler, so Go on valence calls plain `gcc`. Go programs in
    recipes (recipes/github-cli) build with valence's own `go` from the sysroot; their modules are downloaded in
    `prepare()` into `sources/gomod`, checked against `go.sum` and sum.golang.org, and the build runs with `GOPROXY=off`.
    The sysroot's `go`, its compiler and linker are statically linked Go programs and run on the build machine as
    they are; github-cli is built with `CGO_ENABLED=0` and is static too.
51. **NVIDIA's kernel modules fit one exact kernel.** `nvidia-open` is built against Valence's kernel source with the exact
    `/boot/config-valence` (a full kernel build, for `Module.symvers`, which NVIDIA's configure checks read) and depends on
    that `linux-valence` version-release: **every linux-valence version or pkgrel change needs an nvidia-open pkgrel bump
    and rebuild in the same change**, or systems with it can't take the new kernel. Its firmware (`nvidia-open-firmware`)
    is NVIDIA's, shipped unmodified with NVIDIA's licence (the licence's condition for redistributing it). A real test
    needs an NVIDIA card: QEMU can't emulate one. What the build machine checks: the modules' vermagic, `depmod -e`
    against `System.map`, and loading them in a valence VM (`NVRM: No NVIDIA GPU found`, then a clean exit).
52. **NVK (recipes/mesa-nvk) is Rust**, so it is its own recipe with `options=(rust)` (the rest of Mesa isn't Rust and
    builds without it). Keep its `pkgver` equal to `mesa`'s. Meson
    cross-compiles its Rust through an extra cross file (`rust` with its target and Valence's gcc as the linker, `bindgen`, `cbindgen`); bindgen and
    cbindgen are the build machine's (`scripts/host-rust.sh`) and load the build machine's libclang, which needs
    `-resource-dir` to find its own headers (`BINDGEN_EXTRA_CLANG_ARGS`). Mesa's Rust crates come through its meson wraps
    (pinned by hash) into `sources/mesa-packagecache`.

53. **On Valence, the build machine's triple is not Valence's.** `gcc -dumpmachine` on Valence prints `x86_64-valence-linux-gnu`,
    the cross toolchain's own target, so binutils and gcc would configure themselves as native compilers and autoconf
    would see `--build` equal to `--host`. `env.sh` sets `BUILD_TRIPLE` (`x86_64-pc-linux-gnu` on valence, gcc's own
    answer elsewhere): `valence-build` passes it as `--build`, and `toolchain.sh` gives binutils and gcc
    `--build`/`--host` from `TOOLCHAIN_HOST_FLAGS` (empty on Ubuntu, so Ubuntu builds don't change).
    **The rest of this rule was written when Valence was a musl system and is untested on glibc** ("State of the glibc
    switch"). Its musl parts no longer describe Valence: a build machine without glibc, upstream's musl-hosted Rust,
    static musl programs that can't dlopen. Programs built for the Ubuntu host (`tools/`, `hosttools/`) didn't run on
    valence (it had no glibc): rebuild them there. Host Rust is
    upstream's musl-hosted build on valence (`host-rust.sh`); there the build machine's Rust triple equals
    `$RUST_TARGET`, so build scripts' C parts use valence's cross gcc too, and they still run because the build machine
    is valence. Recipes must not assume Ubuntu paths (`/usr/lib/llvm-*`): look for valence's (`/usr/lib`) as well.
    valence's `tar` was BusyBox's until the GNU tools arrived (phase 2); GNU tar runs `xz`, which is still BusyBox's
    unless the `xz` package is installed, and BusyBox's xz decoder stops at a 64 MiB dictionary (Rust's tarballs use 128 MiB: "tar: corrupted
    data"): unpack `.xz` with `untar` (valence-build) or `xz -dc | tar -xf -`, never `tar xJf`. Host Rust programs that dlopen()
    (bindgen loads libclang) must be built with `-C target-feature=-crt-static` there: a static musl program can't dlopen.
    valence's python3 has no pip.
    `find`, `grep`, `sed`, `diff`, `ln` and `realpath` were BusyBox's too until phase 2, and scripts and recipes still
    carry the workarounds (harmless with the GNU tools): no `find -uid/-gid` or size suffixes beyond `k`, no `grep --exclude`,
    no `realpath` options, no `ln -r`, no `diff --version` (libvpx's configure asks), and `sed` isn't GNU sed; valence's bison has no `yacc` command (`YACC="bison -y"`).
    Scripts and recipes use what both have (or python3). The build machine's gcc is GCC 15 (C23 by default,
    rule 40: the cross toolchain's in-tree GMP gets `-std=gnu17`), and its cmake is CMake 4
    (`CMAKE_POLICY_VERSION_MINIMUM=3.5` in valence-build for projects asking for less than 3.5). host-setup.sh links
    automake's `config.sub`/`config.guess` into `/usr/share/misc` and installs `libtool-dev` and `gettext-dev`
    (libtool.m4, autopoint) for autoreconf. Python modules a build imports on the build machine are valence packages too
    (no pip): elogind's build imports Jinja2 and flatpak's pyparsing, so host-setup.sh asks for `python3-jinja2` and
    `python3-pyparsing` and, while the online repository doesn't have them, copies each module from its sdist in
    `sources/` (a new recipe's package is online only once the owner publishes it, so whatever the build machine itself
    needs must also work before that). The same goes for tools: `rpcgen`
    (open-vm-tools' configure; valence's libtirpc has none) is built from `sources/` into `/usr/local/bin` until
    `rpcsvc-proto` is published.
    valence's `-dev` packages don't pull in the `-dev` packages their `.pc` files require, so a native pkg-config lookup
    on valence fails unless host-setup.sh asks for those too (`pcre2-dev` for glib-2.0, `xorgproto` and the libXau, Xdmcp,
    Xfixes and Xxf86vm `-dev` packages for x11 and gl): appstream's cross build looks the build machine's appstream up.
    QEMU's configure makes a venv with pip (`mkvenv.py`), which valence's python3 can't (no ensurepip): on valence,
    recipes/qemu and qemu-guest-agent use the host Python (`hosttools/python` keeps ensurepip) and put setuptools
    and wheel (PyPI wheels in `sources/`) next to QEMU's own in `python/wheels`, where its offline "tooling" group
    looks for them.
54. **i686 needs SSE2.** The 32-bit toolchain targets `-march=pentium-m -mfpmath=sse`
    (`GCC_ARCH`/`GCC_FPMATH` in `env.sh`): Qt 6 refuses to build without SSE2, and Alpine's x86 does the same. The
    second half of this rule was for musl, which doesn't provide `__stack_chk_fail_local`: `libssp_nonshared.a` from
    `toolchain-finish.sh` and the musl recipe, and `patches/gcc-i686/ssp-nonshared.patch`. All three are gone.
    The 32-bit edition has not been built on glibc (whether it stays is undecided, ROADMAP.md).
55. **No text relocations on i686.** BusyBox's SHA-NI assembly (`CONFIG_SHA1_HWACCEL`, `CONFIG_SHA256_HWACCEL`) isn't
    position-independent on i386: the PIE BusyBox got a TEXTREL and every applet segfaulted at start. The recipe
    switches those off on i386. After a new 32-bit package, check `readelf -d` for `TEXTREL`.
56. **GRUB 2.14 with binutils 2.44 or newer links its kernel at the wrong address.** Its configure picks
    `-Wl,--image-base=0x400000`, which newer ld honours instead of `-Ttext`: `grub-mkimage` then stops with "kernel.img
    miscompiled ... start address is 0x9074 instead of 0x9000" (i386-pc). `recipes/grub` and `scripts/hostgrub.sh`
    preset `ax_cv_check_ldflags___Wl___image_base_0x400000=no`.
57. **Generated recipes are edited in their generator.** `gen-simple-recipes.py`, `gen-kde-recipes.py` and
    `gen-lxqt-recipes.py` rewrite their recipes at every `valence-build` start, so an edit made only in
    `recipes/<name>/VALENCEBUILD` is silently undone (qtdeclarative's "no PCH on x86" was). Change the generator, run it,
    and commit both. Per-arch options go inside the arguments as `$([ "$APK_ARCH" != x86 ] || echo ...)`. On x86,
    qt6-qtdeclarative builds without precompiled headers: with them its build tree passed 11 GB and filled the disk.
58. **A failed package unpacks fresh on the next run.** A CMake cache from a run that failed remembers what wasn't
    there yet (Qt's `HAVE_EGL=false` before Mesa existed) and keeps failing after the dependency is built:
    `valence-build` deletes `work/pkg/<name>/.prepared` of a package that failed. One that was interrupted keeps it
    and carries on in its source tree.
59. **SDDM's greeter on labwc (valence-lxqt) is an xdg-shell window that labwc makes full screen.** With SDDM's
    default layer-shell integration under labwc the greeter came up as a small box on a black screen.
    `/etc/sddm.conf.d/15-lxqt.conf` sets `QT_WAYLAND_SHELL_INTEGRATION=xdg-shell` for the greeter and starts labwc with
    `/usr/share/valence/sddm-labwc/rc.xml` (a window rule: no decorations, full screen). wlroots' Xwayland path must be
    `/usr/bin/Xwayland` (recipes/wlroots), not the sysroot's, and labwc's own `labwc.desktop` session is removed so
    SDDM starts LXQt.

60. **WSL2 on Windows is a supported build machine, with traps** (BUILDING.md, "Windows + WSL2 build machine"):
    the PC must not go to sleep while a build runs (connected standby freezes the VM and even `wsl --shutdown` hangs; recover with
    an elevated stop of WslService and `vmmem`/`vmwp`, or a restart); Windows' `PATH` is switched off in `/etc/wsl.conf`
    (`appendWindowsPath=false`), because a space in it broke `host-rust.sh`; scripts and queues are run by absolute path from
    the Linux disk, never from `/mnt/c`; and on Windows 10 there is no KVM, so QEMU tests are slow software emulation. A signing
    key kept on a USB stick is used in place through `VALENCE_SIGN_KEY` (`valence-build` and `mkiso.sh` read it), never copied.
61. **TIOCSTI is off** (`dev.tty.legacy_tiocsti = 0` in valence-base's `/etc/sysctl.conf`). shadow's `su` (since 4.20),
    when root runs it, drops the controlling terminal unless that ioctl is disabled: `su - user` then starts a shell
    with no job control ("cannot set terminal process group"), and doas and sudo answer "a tty is required". The disk
    test's `su - jcole` and `sudo id -u` steps are what catch it. `su -c command` has always run without a terminal.
62. **util-linux's `--disable-all-programs` also switches off every program that has no `--enable` option of its
    own, whatever the command line says.** `--enable-findmnt --enable-mkswap --enable-findfs --enable-blkdiscard`
    were "unrecognized options" in configure's output, and Valence had no `findmnt` (BusyBox supplied the other
    three). `recipes/util-linux` takes those four out of that default in `configure` itself, and stops if one of
    them is not built. Before adding `--enable-<name>` for another of its programs, look in `configure.ac` for an
    `AC_ARG_ENABLE([<name>]`; without one, the program goes into the recipe's `_extra` list.
63. **A recipe says which optional libraries it uses.** zstd's makefile turns on `.gz`, `.xz` and `.lz4` support
    for whichever of zlib, liblzma and liblz4 it finds in the sysroot. First built before xz, it had no liblzma;
    rebuilt after it (a `pkgrel` bump that changed nothing else), it linked liblzma and pulled xz's library onto the
    ISO. `recipes/zstd` passes `HAVE_ZLIB=1 HAVE_LZMA=0 HAVE_LZ4=0`. That was with one sysroot shared by every
    build. Since clean builds a library the recipe does not name is simply absent, and the first clean build of the
    base showed three more recipes that had been living off the shared sysroot: open-vm-tools (configure stops
    without `crypt()`: libxcrypt-dev), shadow (ACL support quietly off without attr-dev's header) and xfsprogs
    (`xfs_io` linked libdevmapper when it found one). What remains possible is the quiet kind: a configure script
    that drops a feature when a library is missing. Give such a feature its explicit switch (`--with-acl`), so that a
    missing `makedepends` is an error; and after rebuilding base packages, compare the packages (dependencies, files,
    sizes) or the file list of `work/liveroot` with the ones from before, which is how all of these were caught.
64. **glibc loads `libgcc_s.so.1` by name, so no package's `so:` dependencies ever ask for it.** It is needed when a
    thread exits or is cancelled (cryptsetup in the unlock initramfs, rule 28). The console system had it only
    because xfsprogs needed libinih, whose C++ half needs libstdc++, which needs libgcc; when Btrfs replaced XFS and
    xfsprogs left the ISO, libgcc left with it, and `valence-mkinitramfs` would have copied nothing (its `cp` of that
    file forgives a missing one). `valence-base` now depends on `libgcc`. The comparison of `work/liveroot`'s file
    list with the one from before (rule 63) is what showed it, before any test ran.
65. **A POSIX shell function's variables are the whole script's.** `/bin/sh` has no `local`, and `valence-snapshot`'s
    `prune` looped over `id`, the variable its callers hold the snapshot's number in: every message named the oldest
    snapshot ("snapshot 1 taken", "Rolled back to snapshot 1") while the right one had been taken or rolled back to.
    The functions there now prefix their variables (`p_id`, `c_id`, `r_name`). Do the same in any function added to
    the scripts in `valence-base`, and let a test read numbers back from what the script printed, which is how this
    one was caught.
66. **CMake starts a program from the sysroot as it is, so the builder wraps those programs.** CMake puts
    `CMAKE_CROSSCOMPILING_EMULATOR` (rule 18) in front of a command only when the command is the bare name of a
    program the project itself builds. `Qt6::moc`, `KF6::kconfig_compiler` and `wayland-scanner` come from packages
    in the sysroot (imported targets, or paths from `find_program`), and CMake 3.28 starts them directly: with the
    build machine's loader and its older glibc, where they fail, or run against the build machine's libraries.
    Before every build `valence-build-recipe` (`wrap_sysroot_programs`) therefore moves each dynamically linked
    program in the sysroot's `usr/bin`, `usr/libexec`, `usr/lib/qt6/bin` and `usr/lib/qt6/libexec` to
    `$SYSROOT/.real/` and leaves a two-line script in its place that starts it through the sysroot's loader, with
    `--argv0` so that the program still sees its own path (Qt's tools find their files from it). A package that
    keeps build tools in another directory needs that directory added to the list there. Two cases stay with the
    recipe. A command written as `$<TARGET_FILE:tool>` for a tool the project has just built gets no emulator: name
    the target instead (`PRE['kf6-breeze-icons']` in `gen-kde-recipes.py`). And a tool that must run on the build
    machine *while* the project is built for Valence is built for the build machine first: LLVM's tablegens
    (`CROSS_TOOLCHAIN_FLAGS_NATIVE` in `recipes/llvm`, which is now a CMake cross build like the others), KWin's
    Wayland scanner and KSyntaxHighlighting's indexer (`PRE` in the generator).
67. **The generated KDE recipes take their `makedepends` from `scripts/kde-deps.json`, and a comparison with Melon's
    packages shows what a build lost.** Melon built these recipes in a fixed order into one sysroot and declared
    nothing. `scripts/suggest-makedepends.py INDEX --kde-json scripts/kde-deps.json` works the table out from four
    things: the libraries Melon's package was linked against (INDEX is Melon's last `Packages.adb`,
    `https://raw.githubusercontent.com/melon-77/melon-os/packages/x86_64/Packages.adb`), the source's
    `.kde-ci.yml`, its `find_package()` calls, and a `poqm/` directory (translations Qt loads: `lrelease`, in
    qt6-qttools-dev). It keeps only what the generator's `ORDER` builds earlier, so the table cannot make a cycle.
    What none of the four shows goes into `DEPS_MORE` in the generator, with the reason (headers used without
    linking, a library Qt loads by name); `gen-simple-recipes.py` has `MORE_DEPS` for the same.
    `suggest-makedepends.py INDEX RECIPE...` prints the suggestion for a hand-written recipe.
    After building, run `scripts/compare-with-index.py INDEX`. It compares what every built package links and
    provides with the reference. "No longer links" means a clean sysroot switched a feature off and nothing failed
    (rule 63): qt6-qtmultimedia had lost libQt6DBus, which was its screen capture through PipeWire, because
    pipewire-dev was not declared; mesa needed llvm-dev (llvmpipe and the shader compilers), python3 the libraries of
    its bz2, lzma, zstd, readline and uuid modules, cups acl-dev, avahi libcap-dev, and so on through 31 recipes
    outside the KDE generator. "No longer provides" is a plugin that was not built: plasma-workspace's camera
    indicator needs pipewire-dev. The differences that are meant: glib without libelf (the base has no elfutils; the
    recipe says `-Dlibelf=disabled`), xfsprogs without libdevmapper and zstd without liblzma and liblz4 (rule 63),
    libvorbis with libmvec (glibc's vector math library, which musl does not have), calamares-valence without Melon's
    question-game page (ROADMAP.md, "The joke layer goes"), and linux-firmware, whose subpackages are cut differently
    from the ones in Melon's last index.
68. **A `-dev` package brings what its own files name.** A project that says `find_package(Plasma)` reads
    `PlasmaConfig.cmake`, which says `find_dependency(KF6KirigamiPlatform)`; in a clean sysroot that only works if
    libplasma-dev depends on kf6-kirigami-dev, and when it does not, the failure appears in milou and kscreenlocker,
    far from the recipe that is wrong. Three things fill a `-dev` package's dependencies: its `.pc` files'
    `Requires`, the libraries the recipe's libraries link ("Recipe format"), and `depends_<x>_dev` by hand.
    `scripts/check-dev-deps.py` reads the built packages and reports what is still missing: a `find_dependency()`
    or a pkg-config module (`pkg_check_modules`) in a package's CMake files, an include directory or an imported file
    named there, when nothing the package depends on ships it; and a link `libx.so -> libx.so.1` whose library is in
    a package the link's package does not depend on. It found libplasma-dev (above), kf6-modemmanager-qt-dev and
    kf6-networkmanager-qt-dev (ModemManager's and NetworkManager's headers: nothing is linked), plasma5support-dev
    (KF6Service), kf6-kquickcharts-dev and kwin-dev (ECM), and three whose library sits in a sister subpackage:
    appstream-dev (libAppStreamQt is in appstream-qt), btrfs-progs-dev (the tools do not link the libraries in
    btrfs-progs-libs) and flite-dev. Run it after building
    a recipe that has a `-dev` package; it must report nothing. With `-v` it also lists what no built package ships at
    all (find modules that come with CMake, LLVM's static libraries, which are not packaged), which it cannot judge.
69. **What glibc changed for sources that built on musl.** Found while the desktop was rebuilt (glibc 2.44):
    `strrchr()`, `strchr()` and their relatives return a `const` pointer when given one (C23, since glibc 2.43),
    and a project that builds with `-Werror=discarded-qualifiers` stops (`recipes/pipewire/glibc-const-strrchr.patch`).
    glibc takes its errno names from the kernel's headers, which gave two values a second name (`EFSBADCRC`,
    `EFSCORRUPTED`); a table built from that list has them twice
    (`recipes/elogind/0012-errno-list-leave-out-the-kernel-s-second-names.patch`). `crypt()` is libxcrypt's, not the
    C library's, so no index of Melon's shows who uses it: linux-pam, calamares, plasma-workspace and open-vm-tools
    declare libxcrypt-dev. And the patches written to make a source build on musl go, with the stand-in libraries
    they needed: elfutils lost its patches, and the recipes `argp-standalone`, `musl-fts` and `musl-obstack`, which
    only it used, are deleted; cairo lost one patch, elogind eight.
70. **A package may only need what its own build saw.** `valence-build` orders builds by what recipes declare. apk
    sets a sysroot up by what packages need, and part of that the builder only works out when it makes the package:
    the libraries it links (`so:`) and the modules its `.pc` files require (`pc:`). A linked library was in the
    build's sysroot, so its recipe had been built first. A `.pc` file can require a module the build never looked
    for: gpgme installs `gpgme-glib.pc` (`Requires: glib-2.0`) without using glib, so gpgme-dev depends on glib-dev,
    and nothing said that glib comes before whatever uses gpgme-dev. Every build before the first one from an empty
    repository had glib in the repository already. That one reached gpgmepp four places before glib, could not set
    its sysroot up ("pc:glib-2.0 (no such package)"), and would have left everything that needs gpgmepp untried.
    So `make_apk` stops when a `.pc` file requires a module that neither the build's sysroot nor the recipe itself
    has: add the `-dev` package that has it to `makedepends` (gpgme: glib-dev), or leave the `.pc` file out if it
    is for something the recipe does not build. Checked for the whole tree at the time, against the complete
    repository of before: with every recipe's sysroot as apk resolves it, the order `--order` prints had this one
    gap among 451 recipes, and gpgme was the only recipe with such a `.pc` file among 497 modules.

To resume a failed long build without unpacking again (for example the kernel):
`VALENCE_KEEP_SRC=1 scripts/valence-build-recipe linux-valence`. `valence-build` does that by itself (`VALENCE_AUTO_RESUME=1`), but
only while the recipe and its patches are the ones the tree was unpacked with (`work/pkg/<name>/.prepared` holds their
checksum): after a fix to the recipe, the package unpacks fresh so the new patches and `prepare()` run.

The build container can be reclaimed while idle, which kills background builds: run the same command again
(`scripts/valence-build ...` or `scripts/build-everything.sh`). What is built is skipped, and the package that
was compiling carries on.

After an unclean shutdown (WSL restart, power loss), files written in the last minutes can be empty or
truncated. Before resuming: look for empty packages (`find repo -name '*.apk' -size 0`; the builder skips
nothing that is empty, but a truncated `.apk` still blocks reindexing), delete `work/pkg/<name>/.prepared`
of the package that was compiling so it unpacks fresh, and check `git fsck` (empty objects in `.git/objects`
can be restored with `git fetch` once they are moved aside). A package that fails to install into the sysroot
(a file conflict) fails every build that needs it, with apk's message in that build's log: fix the recipe that
ships the clashing file. The sysroot itself needs no repair, because every build writes its world file anew
(`apk_sysroot` in valence-build-recipe). `apk verify` needs an absolute `--keys-dir`:
a relative one is resolved against `/` and reports every package as UNTRUSTED.

## Wish lists

ROADMAP.md, phase 8: "a file you can throw in to get the system you want", a list of wishes, not a description of
the whole system. Everything is `valence-wish` in valence-base, with a few lines in the installers.

- **The file** (`valence-wishlist.txt` when an installer is to find it): `key = item, item, ...` lines, `#` starts a
  comment, a key may come back and its items add up. Keys: `packages`, `services` (folders of `/etc/sv`),
  `hostname`, `user.NAME.groups`. Anything else is an error that names the line, and so is a hostname that is not
  one; `valence-wish --check FILE` only reads the file.
- **Adding, never removing.** `valence-wish FILE` works out what the file lists and the system does not have yet
  (packages not installed, services without a link in `/var/service`, groups the user is not in, another host
  name) and does only that; what the file does not mention stays as it is. When everything is already so it says
  "nothing to do", changes nothing and takes no snapshot, so applying a file twice is the same as once. `-n`
  prints what it would add.
- **Everything is checked before anything changes**: a package that is in neither the repository nor Flathub, a
  service or group that is not there, a user who is not there. A service or group that a package of the same file
  brings can only be checked once the package is installed; if it is still missing then, `valence-wish` stops
  there and says that the packages were added.
- **One snapshot for the whole file.** On a running system it calls `valence-snapshot auto` first, with
  `VALENCE_SNAPSHOT_TEXT` saying what it is ("before valence-wish wishes.txt"), and runs apk with
  `--no-commit-hooks`, so the file is one snapshot and one `valence-snapshot rollback` takes all of it back (hostname,
  services and groups live in `@` too). `AUTO=no` in `snapshot.conf` switches this snapshot off with apk's.
- **Packages or Flathub.** A name under `packages` is an apk package when the repository has one of exactly that
  name (`apk search -x`). Otherwise it is a Flathub app: a full app ID (`org.gimp.GIMP`) as it is, a common name
  from a table in the script (`firefox`, `steam`, `vlc`, `discord`, `gimp`, ...), or else the one app whose name
  Flathub's own search matches exactly. Flathub needs flatpak, so a base system refuses such a name. Apps are
  installed with `valence-flathub`, system-wide.
- **Installers.** `valence-wish --find [--exclude DISK]` looks for `valence-wishlist.txt` at the top of any
  partition (FAT, exFAT, NTFS, ext4, Btrfs, XFS) other than the target disk's, mounted or not: a second USB stick,
  the data partition of a multi-ISO stick next to the ISO, a drive in the machine. `valence-install` offers a file it
  found (the default answer, so `VALENCE_YES=1` takes it; `VALENCE_WISHLIST=FILE|none` decides beforehand), checks
  it before partitioning, and applies it with `valence-wish --root $TARGET` after the services are set up; `cal-finish`
  does the same after the users job. A wish that cannot be had does not stop an install: the system is installed
  without it and the installer says how to apply it later. The file is kept as `/etc/valence/wishlist.txt`.
  With `--root` there is no snapshot (the system is still being put together) and Flathub apps, which need a
  network, are written to `/var/lib/valence/wish-flathub`: `valence-first-boot` installs them at the first login.
  A hostname in the list wins over the installer's.
- **Test:** `qemu-test.py wish ISO DISK` (ROADMAP.md, phase 8's "done when"): a USB stick with a list on it; the
  installer finds and applies it; the installed system has its package, service, group and host name; a second list
  applied on the running system adds its own and keeps the first one's, after one snapshot; applied again it
  changes nothing; a list naming a package that does not exist changes nothing; one rollback and a restart take
  the second list back and keep the first. Calamares' part is the same command; it is not tested automatically.
  Flathub is not in the test (it needs the internet).

## The Raspberry Pi edition (phase 9)

ROADMAP.md, phase 9. 64-bit ARM, `VALENCE_ARCH=aarch64` (`aarch64-valence-linux-gnu`, armv8-a; its own `tools-aarch64`,
`sysroot-aarch64`, `work-aarch64` and `repo/aarch64`). The console system only, as an SD card image; `docs/raspberry-pi.md`
is for the person writing the card.

- **Building:** `VALENCE_ARCH=aarch64 scripts/toolchain.sh` (13 minutes), then `VALENCE_ARCH=aarch64 scripts/valence-build
  valence-base rpi-firmware linux-rpi kmod zstd openssl ca-certificates ncurses-terminfo wpa_supplicant nano` (41
  recipes; the kernel is about 20 minutes), then `scripts/mkimage-rpi.sh`. The build machine needs QEMU's user-mode
  emulator twice: `valence-run` starts built programs in it (`VALENCE_EMU`), and package scripts run in the aarch64
  root through binfmt (qemu-user-static); `host-setup.sh` installs both, with qemu-system-arm and btrfs-progs. What
  else builds for aarch64 has not been tried; openssl (its configure target) and gzip (a macro clash with the
  kernel's sigcontext headers) needed changes for it.
- **The kernel, `recipes/linux-rpi`** (`arch=(aarch64)`): the Raspberry Pi Foundation's tree at a stable tag, their
  `bcm2711_defconfig` (its `kernel8.img` starts the Pi 3, 4, 400, Zero 2 W and 5) plus `config-rpi`: Btrfs and the
  SD controllers built in (no initramfs, rule 28), the serial consoles, and **modules uncompressed**: the
  Foundation's configuration compresses them with xz, which Valence's kmod does not read, so depmod listed none and
  no driver could load. Like `linux-valence` it runs depmod in its post-install and post-upgrade scripts (without
  them there is no `modules.dep`). The package carries the device trees of the Pi 3, 4 and 5 boards and the overlays
  into `/boot`.
- **The firmware, `recipes/rpi-firmware`** (`arch=(aarch64)`): `start*.elf`, `fixup*.dat` and the Pi 3's `bootcode.bin`
  from the Foundation's firmware repository at a tag, one file at a time (the repository's archive is gigabytes).
- **The card, `scripts/mkimage-rpi.sh`:** MBR; a 256 MiB FAT32 partition mounted at `/boot` (the firmware, the kernel,
  the device trees, `config.txt` with `arm_64bit=1`, `kernel=kernel8.img` and `enable_uart=1`, `cmdline.txt` with
  `root=PARTUUID=... rootflags=subvol=@`); a Btrfs partition with `@`, `@home`, `@log` and `@snapshots`, so apk's
  snapshots work (no boot menu, so no starting a snapshot; a rollback keeps the newest kernel, since the snapshot hook
  copies only the PC's kernel file names). The first start grows partition 2 and the filesystem to the card
  (`rpi-grow-root`, which removes itself). root's first password is `valence`, expired: shadow's login refuses an
  empty root password (`PREVENT_NO_AUTH`), and root may only log in on terminals in `/etc/securetty`, to which the
  card adds the PL011 ports (`ttyAMA0` is the Pi 5's GPIO UART, `ttyAMA10` its debug connector). A login is offered on
  ttyS0 when the kernel's console is there (the Pi 3 and 4: the firmware turns `console=serial0` into it) and on the
  `ttyAMA` port the console names (`getty-ttyAMA`). The card carries a small signed repository (`EXTRAS`, nano) in
  `/var/lib/valence/repo` after the online one in `/etc/apk/repositories`.
- **Test: `scripts/qemu-test-rpi.py IMAGE`** (QEMU 8.2 has a Raspberry Pi 3B, not a 4 or 5). QEMU starts the kernel
  itself, without the Pi's GPU firmware, so the test takes the card's own `kernel8.img`, `cmdline.txt` and Pi 3B
  device tree, with two changes the firmware would have made or QEMU needs: the console named directly
  (`console=ttyAMA1`, the PL011 under its alias `serial1`, plus `earlycon`), and the device tree's Bluetooth node
  removed, since on a Pi 3 the PL011 belongs to Bluetooth and the kernel would make no tty of it (`fdtput -r`). The
  mainline device tree (`bcm2837-rpi-3-b.dtb`) has a console in QEMU but its SD driver does not read the card there.
  The test checks: login with the expired password, the 64-bit system on Btrfs, the boot files, the grown root, an
  address on a USB network card, `apk add` from the card and over the network, the snapshot. A busy emulated board
  loses characters typed at its serial port: the test types slowly. It passes in about 85 seconds.
- **Not done:** the real board (the roadmap's "done when": a Pi 4 to a login prompt, an address, `apk add`); Wi-Fi
  (the Foundation's NVRAM files for the brcmfmac chips are not packaged); a desktop; mandoc and the other recipes for
  aarch64.

## Valence's own package manager: catalyst (phase 10)

ROADMAP.md, phase 10. The source is `catalyst/` (Rust; `catalyst/README.md` is its manual: commands, layout, transaction
and recovery, trust), the package `recipes/catalyst` (built from that folder; Valence's libzstd and OpenSSL, rule 46),
the command `oct` (`oct add gcc`). The name is the owner's of 22 November 2026; it was called vpm, then octet. Stage 1, "the engine first ... next to apk and measured against apk", is done:

- It reads apk-tools 3's formats itself, written against a specification taken from apk-tools 3.0.8's source
  (compression wrapper, ADB value encoding, signatures and key ids, index and package schemas, versions and
  dependencies) and checked against apk on the real repository: every entry of the index, the files and hashes of
  real packages, the signatures (a byte flipped anywhere is refused, so is an untrusted key).
- It manages roots it made (`oct --root DIR --initdb add ...`): each package is unpacked into
  `Programs/<name>/<version>`, the usual tree is hard links to those files (copies under `etc/`, kept on upgrade
  when changed), downloads and unpacking run in parallel, and one short switch at the end writes a journal first,
  so the next run finishes it after a kill. Scripts and triggers run as apk runs them.
- Checked: the same files as apk for valence-base (799 entries) and after an add and a del; 310 upgrades killed at
  random moments, every file the old or the new version and the next run completing; a security review with a
  malicious mirror and crafted packages (its findings fixed: bounded memory before any trust check, no replay of an
  older index once a newer was seen, bounded downloads, catalyst's own directories out of reach); a review against the
  specification (package choices match apk's on 23 real requests); 1.6 to 2.2 times faster than apk (valence-base
  0.15 to 0.25 s against 0.28 to 0.40 s; 244 packages 5 to 6.5 s against 11 to 12.5 s).
- Measured on 22 November 2026 against apk and xbps (`docs/benchmarks/2026-11-22/`): installing many packages,
  about twice as fast as apk and 1.2 to 2.2 times xbps's throughput; one-package jobs 1.2 to 4.4 times slower than
  apk, a fixed cost per run (start-up, reading every record, the syncs), which the rest of phase 10 works on.
- The lookup index (`var/lib/catalyst/index/`, `catalyst/README.md` "The lookup index"): a change reads only the records
  it needs, not every installed package's; the folders stay the record, and the index is rebuilt from them whenever
  it doesn't match or is damaged (every file of it is checksummed). Adding one package to 244 installed went from 0.21 s to 0.05 s (apk 0.03 s);
  start-up and the syncs are next.
- A record that can't be read stops every change before anything is unpacked; `oct fix` rebuilds it from the
  signed package (`catalyst/README.md`, "Recovery").
- Not yet: installed systems using it (the installers, snapshots before changes, a running system's upgrades, apk
  leaving); a signed timestamp in the index, without which a root that never saw the repository accepts a replayed
  old index.

Its tests: `cargo test --release` in `catalyst/` (the real-data tests read `repo/x86_64`), `catalyst/tools/killtest/` and
`catalyst/tools/bench/` (`catalyst/tools/README.md`).

**Phase 10 is back with the owner and the build machine since 22 November 2026.** Another developer had it from
21 November; their SQLite database as the only record was taken out again, on the owner's decision that the folders
stay the record. Changes to `catalyst/` and `recipes/catalyst` come from the build machine; ideas and bug reports go in
`docs/build-machine-handoff.md`.

## Testing (required before you commit a change that affects boot or install)

```sh
scripts/mkiso.sh
qemu-img create -f raw /tmp/disk.img 12G
scripts/qemu-test.py live out/valence-*-x86_64.iso /tmp/disk.img            # BIOS: boot ISO + unattended install
scripts/qemu-test.py disk /tmp/disk.img                                   # BIOS: boot the installed system
scripts/qemu-test.py disk /tmp/disk.img --uefi                            # UEFI: same disk
scripts/qemu-test.py live out/valence-*-x86_64.iso /tmp/disk.img --luks     # install with an encrypted root
scripts/qemu-test.py disk /tmp/disk.img --luks                            # boot it, typing the passphrase
scripts/qemu-test.py rollback /tmp/disk.img [--luks] [--uefi]             # on an installed disk: break an update on purpose and go
                                                                          # back, with the command and from the boot menu (below)
scripts/qemu-test.py dualboot out/valence-*-x86_64.iso /tmp/dual.img        # install next to a fake Windows (UEFI), Windows untouched
scripts/qemu-test.py wish out/valence-*-x86_64.iso /tmp/disk.img            # a wish list on a USB stick, then on the running system ("Wish lists")
VALENCE_EDITION=desktop scripts/mkiso.sh                                    # the Plasma live ISO (desktop edition)
scripts/qemu-test.py desktop out/valence-desktop-*-x86_64.iso              # services ready, Plasma running, a USB stick mounts through
                                                                          # UDisks2, screen-recording encoders work, a text file prints
                                                                          # to a virtual IPP Everywhere printer, nmcli online, and
                                                                          # NetworkManager joins WPA2 Wi-Fi on mac80211_hwsim radios;
                                                                          # LOOK at logs/qemu-desktop.ppm
qemu-img create -f raw /tmp/desk.img 16G
scripts/qemu-test.py desktop-install out/valence-desktop-*-x86_64.iso /tmp/desk.img [--uefi]   # install, SDDM greeter stays
                                                                          # up, log in through it, Plasma runs; LOOK at the screenshots
scripts/qemu-gui.py start cd [--uefi]                                     # Calamares has no automated test: click through it by hand
                                                                          # ("The graphical installer" below)
# --offline (any mode): the VM keeps its network card but reaches nothing outside; the installers must still work
```

The tests run the console installer by name (`valence-install`, with the `VALENCE_*` variables from "Installers").
`scripts/test-console.sh [ISO]` runs the eight console tests above in order and `scripts/test-desktop.sh [ISO]` the
three desktop ones (on the newest ISO in `out/` when none is named). Each prints PASS or FAIL per test, with the end
of a failed test's log; `test-console.sh` stops at the first failure unless `ALL=1`.
`desktop-install` does too: it installs the desktop profile with the console installer, on BIOS and with `--uefi`.

**The graphical installer is tested by hand**, because Calamares cannot be scripted and the system it installs has no
serial console. `scripts/qemu-gui.py` starts the desktop ISO in a VM, takes screenshots, and clicks and types where
you tell it. In one terminal `scripts/qemu-gui.py start cd` (QEMU stays in the foreground), in another, when a
screenshot shows the desktop (`qemu-gui.py shot a`, about a minute and a half; it writes `logs/gui/a.png`):

```sh
g=scripts/qemu-gui.py                     # positions are for the VM's 1280x800 screen; look at a screenshot when unsure
$g click 57 45 , click 57 45 , wait 25 , shot welcome      # double-click "Install Valence"; CHECK that the installer opened
$g click 1007 727 , wait 12 , click 312 107 , wait 6 , shot partitions   # Next; "Erase disk"
#   the picture must show valenceboot (1 GiB, FAT32) and valence (Btrfs); on UEFI an EFI system partition before them
#   for an encrypted install: $g click 312 531 , wait 4 , click 547 531 , type 'valenceluks' , click 774 531 , type 'valenceluks'
#   (BIOS positions; on UEFI the row is 28 lower). The summary must then say luks2 for valence and still fat32 for /boot
$g click 1007 727 , wait 8 , click 404 90 , type 'Jo Cole' , click 404 146 , key ctrl+a , type 'jcole' , \
   click 404 202 , key ctrl+a , type 'valencecal' , click 404 258 , type 'valenceuser' , click 610 258 , type 'valenceuser'
$g click 1007 727 , wait 8 , shot summary , click 1007 727 , wait 6 , click 730 421      # Next; Install; "Install Now"
$g wait 240 , shot done                   # "All done." (an encrypted install takes about twice as long without KVM)
$g click 1105 727 , wait 30 , quit        # Done (it restarts into the ISO again), then stop the VM
```

Then `scripts/qemu-gui.py start disk` (with `--uefi` if the install was): an encrypted disk asks for the passphrase
on the screen (`$g shot boot`, then `$g type 'valenceluks' , key ret`), and after about a minute the login screen
shows the user. `$g key ctrl+alt+f2` gives a text console to log in on as root (the users page gave root the same
password) and look: `findmnt -t btrfs,vfat` must show `/`, `/home`, `/var/log`, `/.snapshots` and `/swap` as the
five subvolumes and `/boot` on its own FAT32 partition (with `/boot/efi` on UEFI), `swapon --show` the swap file,
`id jcole` the group `lpadmin`, and after `apk del gamemode` `valence-snapshot list` must list the snapshot taken
before it. Do the run on BIOS, on UEFI and on BIOS with "Encrypt system" whenever `calamares`, `kpmcore` or
`calamares-valence` changes. (20 November 2026: all three pass. The first encrypted run found that Calamares
encrypted `/boot` too and left a disk that did not start, while the installer said "All done": "Installers".
24 November 2026: all three pass again.) For a desktop ISO built with catalyst, name it:
`ISO=out/valence-catalyst-desktop-<date>-x86_64.iso scripts/qemu-gui.py start cd`, and remove the package with
`oct del gamemode`. (24 November 2026: UEFI and BIOS with "Encrypt system" pass; `/var/lib/catalyst/world` is there
and apk's database is not.)

`disk` also checks what the roadmap's phases promise of an installed system. The GNU tools own their command names,
and `findmnt`, `mkswap`, `findfs` and `blkdiscard` are util-linux's programs (rule 62). A fresh install has no
documentation files. `apk add less-doc` from this build's `repo/`, which the test serves over HTTP itself, makes
`man less` and `apropos less` work; `apk add docs` brings the `-doc` part of everything installed, and `man` finds
pages from eight of those packages; `apk del docs` leaves nothing behind. `--offline` skips the steps that need the
repository.

`disk` also checks phase 5's layout: `/`, `/home`, `/var/log`, `/.snapshots` and `/swap` are the subvolumes `@`,
`@home`, `@log`, `@snapshots` and `@swap` of one Btrfs filesystem, mounted with `noatime,compress=zstd:1`; the swap
file is in use; and each of the three package changes the documentation steps make had a snapshot taken before it,
which the boot menu lists.

`rollback` is phase 5's "done when", on a disk the `live` test installed. It makes two broken updates itself (real
packages, version 9999 of an installed one, signed with the repository key and served over HTTP). The first replaces
`less` with a script: `valence-snapshot rollback` and a restart bring the program and the package database back, while
what was written to `/home` and `/var/log` after the snapshot stays; apk refuses to change anything between the
rollback and the restart; the replaced system is deleted after it. The second replaces init with a script that fails
and overwrites the kernel on `/boot`, as a kernel update gone wrong would: the disk must then not boot. The test
opens the hidden boot menu with Esc and chooses "Valence snapshots" and the snapshot from before the update: the
system starts as it was, from a copy of the snapshot and with the snapshot's own kernel, says so at login, and the
default system is still the broken one. `valence-snapshot rollback` in that session makes it the default at once, and
the next start, with nothing chosen, is that system. Then `create`, `delete` and `KEEP`. Run it on a plain disk on
BIOS and on an encrypted one on UEFI (`--luks --uefi`): the encrypted one goes through the snapshot's initramfs.

Logs go to `logs/qemu-*.log`. The tests use KVM when it really works (`qemu-test.py` asks the kernel with the
`KVM_GET_API_VERSION` ioctl: WSL2 on Windows 11 does, WSL2 on **Windows 10, or on a CPU Windows can't nest on, creates a
`/dev/kvm` that QEMU can't use**, "failed to initialize kvm: No such device"); without it QEMU runs in software emulation
(`-accel tcg,thread=multi -cpu max`) and everything is slow. Measured on a Ryzen 7 5800U under Windows 10 (4 vCPUs, no KVM, 9 October 2026):
the console `live` install takes 77 s, `disk` and `disk --uefi` about 40 s each, and the desktop ISO boots and takes a login on
the serial console in 25 s, but the Plasma desktop tests (`desktop`, `desktop-install`) were not usable there (another session saw
Plasma not start in 15 minutes). On an i5-12450H under Windows 10 (also no KVM, 20 November 2026, Valence on glibc) they are:
`desktop` takes 3 minutes, `desktop-install` 9 to 14 minutes on each of BIOS and UEFI (two rounds the same night),
and Calamares installs in 4 to 9 minutes (about 10 with encryption). Use generous timeouts. The ISO's GRUB and the installed system both use
the serial port, so tests don't need a screen. The VM has an Intel HD Audio card (the `live` and `disk` tests list
the sound devices the kernel found); QEMU records whatever it plays to `logs/audio-capture.wav`.

## Running Valence in a VM

- **Kernel:** the virtual disk controllers are built in (virtio, VMware PVSCSI and LSI, Hyper-V storvsc,
  Xen blkfront, AHCI), because installed systems boot without an initramfs. Network, video, balloon,
  vsock and VirtualBox drivers are modules (`recipes/linux-valence/config-valence`, "virtual machine guest").
- **Guest tools:** `valence-vm-guest` pulls in `qemu-guest-agent`, `open-vm-tools` and `hvtools`. Their runit
  services are always enabled but switch themselves off (`sv down "$PWD"`) outside their hypervisor.
  mdev doesn't create `/dev/virtio-ports/*` links, so `qemu-ga` finds its port through sysfs.
- **Installers** add `valence-vm-guest` when `valence-detect-virt` says they run in a VM. The console ISO carries
  it in its package repo; the desktop image includes it.
- **vmwgfx is blacklisted except on real VMware** (`/etc/modprobe.d/valence-vm.conf`, loaded from `/etc/runit/1`
  when `valence-detect-virt` says `vmware`). VirtualBox's default VMSVGA adapter and QEMU's vmware-svga look
  like VMware's; vmwgfx switched off the boot console there and then failed, so the screen froze at
  "Starting devices" (reported from VirtualBox). GRUB passes a framebuffer (`gfxpayload`) so simpledrm
  keeps the console alive without any GPU driver. Check the screen, not just the serial log:
  boot with `-vga vmware -serial none` and take a `screendump` from the QEMU monitor.
- **Not included (yet):** VirtualBox's userspace (VBoxClient/VBoxService: clipboard, drag and drop) and
  VMware's X11 helper. Shared folders on VirtualBox work with `mount -t vboxsf`.
- **Test:** `scripts/qemu-test.py live|disk ... --vmware` uses PVSCSI + VMXNET3 + VMware SVGA; every test
  also checks the QEMU guest agent and ends with a shutdown requested by the host through it.

## Installers: do not break these product decisions

- **Graphical installer (desktop ISO): Calamares.** Its pages are welcome, partitioning, users and summary, then the
  install and the finished page (`/etc/calamares/settings.conf` in `calamares-valence`). Valence is US English and UTC,
  so there are no locale or keyboard pages (Plasma's settings change both after the install). The live desktop's
  "Install Valence" icon runs `valence-install-gui`, which starts Calamares as root through doas.
- **Valence's look (valence-desktop):** Plasma style `valence`, colours ValenceDark, Konsole profile Valence, SDDM theme `valence`
  (its own QML, `usr/share/sddm/themes/valence`), GRUB theme `usr/share/valence/grub/themes/valence` (the desktop ISO uses
  it too). The pictures and the colours are Valence's own and come from one script, `art/desktop.py`. **The
  wallpaper is plain black** (the owner, 20 November 2026: "make it a pure black wallpaper actually"), and so is
  everything made from it: the login and lock screens show the wallpaper itself, the loading screen fades into it
  under the word "Valence" and a progress bar, and the boot menu's background goes with them. The logo is on the
  desktop as the launcher's button, the icons and the installer's pictures. The script keeps one drawing that can be
  exported instead (`art/desktop.py --export gate`): a lattice gate woven from the logo's V, which the owner chose
  from previews before choosing black. The colour scheme is the logo's greys with a blue-grey accent, in Plasma,
  Konsole, the login screen, the boot menu and the installer. Melon's planet and its green and orange are gone.
  Change a picture in the script and run `art/desktop.py --export`, not in the files it writes. The colours were
  turned from Melon's into these once: to change one now, change it in the script's table and in the files
  (`--export` leaves a colour it has already converted alone). `/etc/issue` and `valencefetch` draw the logo's V.
- **Calamares makes the console installer's Btrfs layout.** `partition.conf` offers Btrfs only and lays an erased
  disk out as a 1 GiB FAT32 `/boot` and a Btrfs `/` (on UEFI Calamares puts its EFI system partition, `/boot/efi`,
  before them); `mount.conf` makes the subvolumes `@`, `@home` and `@log` and mounts them with
  `noatime,compress=zstd:1`. The rest is Valence's own job, `cal-finish` (in `calamares-valence`, run as
  `shellprocess@finish` after the users job): it makes `@snapshots` and `@swap`, writes `/etc/fstab`, makes the swap
  file with `btrfs filesystem mkswapfile`, removes the live-only packages (with `--no-commit-hooks`: no snapshot of
  a system that is still being put together), copies the ISO's package repository, builds the unlock initramfs for
  an encrypted root, enables the profile's services and installs GRUB. A root that manual partitioning puts on
  another filesystem is installed as before and has no snapshots. Calamares' users job calls shadow's `useradd`,
  `usermod` and `groupadd`, which the system image has: the BusyBox-backed shims of Melon's `cal-prepare` are gone.
  The user it makes is in the same groups as the console installer's, `lpadmin` among them.
- **`/boot` is never encrypted, and an install that cannot start is an error.** GRUB reads the kernel and the unlock
  initramfs from `/boot` before any passphrase is typed. Calamares encrypts every partition of `partitionLayout` when
  "Encrypt system" is ticked unless the entry says `noEncrypt: true`; the entry for `/boot` did not (the comment
  above it said `/boot` stays unencrypted), so an encrypted install had `/boot` inside LUKS, `grub-install` failed
  where nobody saw it (`|| true`), and Calamares finished with "All done" on a disk that stopped at "Booting from
  Hard Disk". Now the entry has `noEncrypt`, and `cal-finish` stops the install with a message when `/boot` is
  encrypted or on a filesystem GRUB cannot read, when GRUB's BIOS boot code cannot be written on a BIOS machine, or
  when no loader ended up on the EFI partition. (On UEFI the extra BIOS boot code and the firmware's boot entry stay
  optional: a GPT disk has no room for the first, and some firmware refuses the second.)
- **The live desktop does not lock its screen.** `mkiso.sh` gives the live user a `kscreenlockerrc` with
  `Autolock=false`: Plasma locks after five minutes without input, an install on a slow machine takes longer, and
  nobody can know that the live user's password is empty.
- **Live-only packages** (`/usr/share/valence/profiles/live-only`, e.g. `calamares-valence`) are in the
  desktop ISO's system image and both installers remove them from the new system. Both installers make
  the new system's `/etc/apk/world` match the chosen profile.
- **Console installer: `valence-install`** (`/usr/bin/valence-install`, in valence-base), on both ISOs and named in the live
  system's motd. It needs root (started by another user, it runs itself again through doas) and the live ISO's system image.
  It asks for the disk, base or desktop (the desktop profile installs exactly what the desktop ISO installs), the host
  name, the root and user passwords, a Wi-Fi network (when there is a Wi-Fi card) and LUKS
  encryption, then partitions, copies `rootfs.sqfs` and installs GRUB. `valence-install --help` lists the unattended
  variables below. The root it makes is Btrfs in the subvolume layout of "Snapshots and rollback"; the swap file is
  made with `btrfs filesystem mkswapfile` in `@swap`.
- **Dual boot.** On UEFI, when the chosen disk already has an EFI system partition and at least 20 GiB unallocated
  (Windows' Disk Management "Shrink Volume" makes that), the console installer offers `alongside` (the default
  then; `VALENCE_MODE=alongside|erase`): a 1 GiB FAT32 `/boot` (extended boot loader type) and `/` go into the free
  space, the existing EFI partition becomes `/boot/efi`, GRUB goes into `EFI/valence` with a firmware boot entry
  (efibootmgr), and nothing of the other system is touched: no BIOS boot code, and `EFI/BOOT` stays theirs.
  Calamares offers "Install alongside" (it shrinks NTFS with ntfs-3g's ntfsresize), "Replace a partition" and manual
  partitioning; nothing is preselected, and `cal-finish` only writes `EFI/BOOT` on an EFI partition that is
  Valence's alone. `valence-update-grub` adds a Windows entry for Windows Boot Manager on any EFI partition (UEFI) or
  `bootmgr` on an NTFS partition (BIOS). Test: `qemu-test.py dualboot` (a fake Windows disk, persistent UEFI boot
  menu, checksums of everything Windows owns before and after).
- Unattended install variables: `VALENCE_DISK VALENCE_HOSTNAME VALENCE_ROOTPW VALENCE_USER VALENCE_USERPW
  VALENCE_PROFILE VALENCE_YES=1 VALENCE_SERIAL=1 VALENCE_WIFI_SSID VALENCE_WIFI_PSK
  VALENCE_ENCRYPT=y VALENCE_LUKSPW VALENCE_MODE=alongside VALENCE_WISHLIST=FILE|none` ("Wish lists"). With `VALENCE_YES=1`, questions that have a default take it.
- **Profiles** live in `/usr/share/valence/profiles/` on the live system: `<name>` is the package list,
  `<name>.services` the runit services (`name` enables one, `-name` drops a base service). `mkiso.sh`
  writes them. The desktop profile swaps `mdevd`/`dhcp` for `udevd` and NetworkManager, and the
  installer gives NetworkManager the Wi-Fi network instead of wpa_supplicant.
- **No other distro's repository is offered.** Melon's opt-in Alpine repository is removed (the `valence-repo`
  command, `VALENCE_ALPINE`, the installer's question, Alpine's keys and the motd line): Alpine's packages are built
  for musl and cannot run on glibc. Void was never offered (xbps, not apk). The owner's rule stands for any repo
  offered later: never make a foreign repo untagged or on by default.

## Snapshots and rollback

ROADMAP.md, phase 5. **The owner's decisions (19 November 2026): rollback is offered both as a command and in the
boot menu, and it works "however Nix does it".** That is taken as NixOS's behaviour on top of the Btrfs snapshots the
roadmap asks for, not as Nix's store (which is nearer to phase 10): every state of the system is listed in the boot
menu and can be started from there without changing the default; a command makes a state the default; nothing is
lost by going back. Everything is in `valence-base`: `valence-snapshot`, apk's hook, `boot-snapshot`, and the parts
of `valence-install`, `valence-update-grub`, `valence-mkinitramfs` and `/etc/runit/1` that know about it.

- **The layout** (`valence-install`): one Btrfs filesystem on the root partition, `noatime,compress=zstd:1`, with five
  subvolumes. `@` is `/`: what a snapshot holds and a rollback replaces. `@home` and `@log` (`/var/log`) are beside
  it, so a rollback touches neither the user's files nor the logs of what went wrong. `@snapshots` is `/.snapshots`:
  `<n>/root` is snapshot n, read-only, and `<n>/info` its date, type (`update`, `manual`, `rollback`), text and kernel
  files. `@swap` holds the swap file, because a subvolume with an active swap file cannot be snapshotted. `/boot`
  stays FAT32 on its own partition: GRUB never reads Btrfs. Btrfs is built into the kernel (no initramfs, rule 28),
  and the kernel is told `rootflags=subvol=@`. XFS is still built in, for systems installed from the first test ISOs.
- **A snapshot before every change.** `/usr/lib/apk/commit_hooks.d/valence-snapshot` is apk's pre-commit hook and
  runs `valence-snapshot auto`: a read-only snapshot of `/`, named after what apk was asked to do ("before apk add
  less-doc"). apk runs the hook only when it is about to change something, and not with `--no-scripts` or
  `--simulate`. If no snapshot can be taken, the hook fails and apk changes nothing (`apk --no-commit-hooks` goes on
  without one). The hook does nothing, successfully, wherever `/` is not that layout: the live ISO, the image
  `mkiso.sh` assembles, the installer's target (it does not mount `/.snapshots` there, on purpose), an XFS install.
  The ten newest `update` snapshots are kept (`KEEP=` in `/etc/valence/snapshot.conf`; `AUTO=no` switches the hook
  off), the three newest `rollback` ones, and every `manual` one until it is deleted. NixOS keeps every generation
  until someone collects garbage; on Btrfs that fills the disk quietly, so here the number is bounded.
- **The kernel is outside Btrfs, so each snapshot keeps its own.** `/boot/vmlinuz-*`, `initramfs-*`, `System.map-*`
  and `config-*` are copied to `/boot/snapshots/<name>.<first 16 digits of the sha256>`: one copy per different file,
  however many snapshots use it, removed when the last of them goes. A rollback puts the snapshot's files back on
  `/boot` and removes kernel files that came later. Without this a rolled-back system would start the new kernel
  with the old modules.
- **The boot menu starts a snapshot; it does not roll back.** `valence-update-grub` writes
  `/boot/grub/snapshots.cfg`, a submenu "Valence snapshots" with an entry per snapshot, newest first, and `grub.cfg`
  reads it after the systems that start normally (`valence-update-grub --snapshots` rewrites only that file;
  `valence-snapshot` runs it after every snapshot). An entry starts the snapshot's own kernel (and initramfs, on an
  encrypted disk) with the read-only snapshot as `/` and `init=/usr/libexec/valence/boot-snapshot
  valence.snapshot=<n>`. That script makes a writable copy of the snapshot, the subvolume `@trial`, mounts it,
  `pivot_root`s onto it and starts the normal init there. The system then runs as it was in the snapshot, says so at
  login (`/etc/profile.d/valence-snapshot.sh`, from `/run/valence/trial`), and `@` has not been touched: the next
  start is the default system again, and its stage 1 deletes the copy. The menu is hidden on a computer with one
  system (Esc or Shift opens it), as before.
- **`valence-snapshot rollback [N]` makes a snapshot the default.** In a system the boot menu started, with no
  number: that system. `@` is kept as a `rollback` snapshot, then `@trial` and `@` swap names in one
  `renameat2(RENAME_EXCHANGE)` (`mv --exchange`), so there is an `@` at every moment, and what `@` was is deleted; it
  takes effect at once, because the running system is now `@`. Otherwise: snapshot N, or without a number the one
  from before the last package change (after a rollback, the one before the snapshot rolled back to, so repeating it
  steps further back). `@` is kept as a `rollback` snapshot, a writable copy of N is swapped in as `@`, and the system
  that was running keeps running, as `@old`, until the restart; stage 1 of the next boot deletes it
  (`/.snapshots/.cleanup` marks that there is something to delete). NixOS switches a running system at once; a Btrfs
  root cannot be swapped under running programs, so here it is the next start.
- **Only `@` takes package changes.** While the system runs from `@trial` or `@old`, the hook fails with a message
  that says which, and apk changes nothing: the change would be gone, or in the wrong place, after the restart.
- **A root running under another name says so in its own fstab.** util-linux's `mount -a` compares the subvolume a
  mounted `/` came from with the `subvol=` of its fstab line, and takes `/` for not mounted when they differ: it
  would mount the default `@` over the running root. `boot-snapshot` rewrites the copy's line to `subvol=@trial`, a
  rollback rewrites the running root's to `subvol=@old`, and keeping a started snapshot writes `subvol=@` back
  (`fstab_root` in `valence-snapshot`). A remount ignores `subvol=`, so stage 1's `mount -o remount,rw /` is fine
  either way.
- **The kernel calls a root it mounted itself `/dev/root`** in `/proc/mounts` until something scans the devices
  ("device path /dev/root changed to /dev/vda3 scanned by mount" in the log). `boot-snapshot` runs before that and
  takes the device from `root=` on the command line when the name is not a device node; `valence-snapshot` asks for
  the device of `/.snapshots`, which `mount -a` mounted by UUID.
- **What is not there.** No space accounting (snapshots hold on to the data of what was replaced; ten of them bound
  it), no snapshots of `/home`, and no graphical front end. Subvolumes someone creates inside `@` are not part of its snapshots, and stop a replaced `@` from
  being deleted.
- **Mine, not the owner's, and listed in ROADMAP.md under "Not decided yet":** ten snapshots kept, `/home` and
  `/var/log` outside them, zstd level 1 compression, and the name `valence-snapshot`.

## Boot time

Where the time goes after the firmware: GRUB (the live ISO's menu waits 5 s; an installed valence-only system's is hidden and
skipped after 1 s, `GRUB_TIMEOUT` and `GRUB_TIMEOUT_STYLE` in `/etc/default/grub` change that; the `grub` package's own `/etc/default/grub` must not set `GRUB_TIMEOUT`, or it wins over `valence-update-grub`'s default: it did, until `grub` pkgrel 2), the kernel, stage 1 (`/etc/runit/1`:
mounts, udev coldplug, filesystems), then stage 2 (runsvdir starts every service at once; services that need another
wait for it with `sv check`, which polls for up to 7 s, every 20 ms since `runit` pkgrel 1; it was every 0.42 s). Stage 1 writes a
timeline to `/run/valence-boot-times` and **`valence-boottime`** prints it with the start times of the main daemons (read from
their processes, so to 10 ms): run it on a real machine before and after changing anything that touches boot, and quote its
numbers in the PR.

**Stage 1 doesn't wait for udev to finish** (`valence-base` pkgrel 34). It starts udevd, triggers the coldplug, and waits only
for the devices named in `/etc/fstab` (their `/dev/disk/by-*` links, at most 20 s in all; `noauto` and `nofail` entries don't
count) before it mounts. The rest of the coldplug (the GPU's driver and firmware, Wi-Fi, sound) goes on in the background while
stage 2 starts the services, and `valence-boottime` shows when it ended as `udev-settled`. The `udevd` service adopts the udevd
stage 1 started instead of replacing it (rule 44). Services that need a device wait for it themselves: SDDM waits until elogind
reports seat0 graphical, NetworkManager and BlueZ handle devices that appear later. The kernel option `valence.udev=sync` (add it
in GRUB's editor, or to `GRUB_CMDLINE_LINUX_DEFAULT`) brings back the old behaviour (wait for everything, stop udevd, let
the service start a new one) if a machine misbehaves: an issue report with and without it is the first thing to ask for.

**The live system and the installers read `rootfs.sqfs` through squashfs.** The kernel's defaults were one decompressor and one
shared cache (`SQUASHFS_DECOMP_SINGLE`, `SQUASHFS_FILE_CACHE`), so every program the live desktop starts waited for the
others' reads to decompress. `config-valence` now asks for `SQUASHFS_COMPILE_DECOMP_MULTI_PERCPU` and `SQUASHFS_FILE_DIRECT`
(one decompressor per CPU, straight into the page cache; `linux-valence` pkgrel 5, and `nvidia-open` pkgrel 1 with it, rule 51).
Installed systems don't use squashfs, so only the ISO sessions and installs gain.

Measured on 7 and 8 October 2026 with the 29 September desktop image in QEMU **without KVM** (software emulation on 4 vCPUs, so
every CPU-bound step is several times slower than on real hardware; read the proportions, not the seconds). Seconds counted
from the start of stage 1, one boot each except the old order, which ran twice:

| | old order | new order (`valence.udev=sync` gives the old one back) |
|---|---|---|
| udev coldplug + settle in stage 1 | 3.9 to 4.8 s | 0 (finishes 3.8 s after stage 1, in parallel with the services) |
| stage 1 lasted | 6.5 to 7.3 s | 4.2 s |
| SDDM started | 7.5 to 7.9 s | 4.9 s |
| SDDM's session helper started | 13.2 to 13.5 s | 11.8 s |

Kernel to `/init` was 2.4 s and the initramfs 0.5 s in all of them. The squashfs options (new kernel) moved the live
desktop's `plasma_session` from 30.3 s to 27.1 s after stage 1 in one run each, which is inside the run-to-run noise (about 3 s)
of this setup; the gain is real on a machine that reads from the ISO on several cores, but this container can't size it.
The 20 ms `sv` poll can't be seen in these numbers either (the services start within 0.4 s of each other). Not measured yet:
real hardware (a GPU's firmware load blocks `udevadm settle`, so the gain should be larger there) and the installed system, where
`valence-boottime` is the tool. Ideas, not done: shutdown spends a fixed 1 s between TERM and KILL (`/etc/runit/3`); a smaller
kernel config and built-in drivers for the boot disk.

## Smaller images, second round

Measured on the 29 September desktop image with `mksquashfs` (zstd, 512 KiB blocks), 8 October 2026:

- **`grub` pkgrel 3 drops the build leftovers in `/usr/lib/grub/*/`**: `*.module`, `kernel.exec`, `gdb_grub`, `gdb_helper.py`. Nothing
  reads them on a running system: `grub-install` copies only `.mod`, `.lst`, `.img`, `.efi`, `modinfo.sh` and the efiemu
  objects (`util/grub-install-common.c`), and `grub-mkimage` reads `.mod` and `kernel.img`; a `core.img` for `i386-pc` and
  for `x86_64-efi` made with and without them is byte-identical. 36 MB less on disk, 11 MB less in the compressed image.
- **`breeze` pkgrel 1 drops two sizes of the default "Next" wallpaper** (in `scripts/gen-kde-recipes.py`: the recipe is generated, rule 57) (`7680x2160` for 32:9 screens and `1440x2960` for
  phones, light and dark: 24 MB of PNG, which doesn't compress). 16:9 and 16:10 screens use the `5120x2880` one, and Plasma
  scales it on the others.
- Left alone on purpose: translations (49 MB compressed: dropping languages is a product decision), the Mesa and LLVM
  libraries, the kernel modules, firmware (see "Smaller images" if #49 has landed).

Not rebuilt in a container: both recipes only `rm` files in `package()`, so the check was on the extracted image. A new ISO
shows the real sizes.

## Package repository (online)

`scripts/publish-repo.sh` puts `repo/<arch>/` on the `packages` branch of github.com/melon-77/v (one
commit, force-pushed each time), served as
`https://raw.githubusercontent.com/melon-77/v/packages/<arch>/Packages.adb`. The base URL is in
`/usr/share/valence/repo-url` (valence-base); the installers write it into `/etc/apk/repositories` before the
offline copy from the ISO, and the live ISO uses it too. Everything is signed with the Valence key, so the
host doesn't need to be trusted. GitHub rejects files over 100 MB: split big packages (Intel Bluetooth
firmware is its own package for that reason). Publish after building packages people should get.

## Releases

Releases are numbered, starting at 0.1.0 (the owner's choice, 20 November 2026; Melon named its releases after
fruit varieties, and that stayed with Melon). Tags are `v<version>`; test builds between releases are pre-releases
with a dated tag (`valence-20261120-2`). A test build can carry a second set of ISOs built with catalyst (the
owner's "Both sets", 24 November 2026): `WITH_CATALYST=1 scripts/release.sh <version>` adds
`valence-catalyst-<version>-x86_64.iso` and `valence-catalyst-desktop-<version>-x86_64.iso`, and the notes say which
set to take. A release is marked Latest, carries every ISO, `SHA256SUMS`,
`SHA256SUMS.sig` and `valence-signing.rsa.pub` (`scripts/release.sh <version>` makes them from the tested ISOs in
`out/`, named `valence-<version>-x86_64.iso` and `valence-desktop-<version>-x86_64.iso`; `docs/install.md` tells
users how to check them and gives the key's fingerprint),
has notes written for users (what's new, which file to download, `doas apk upgrade` for installed systems), and the
release it replaces is retitled "(superseded)" with a link to the new one.

The Raspberry Pi edition has its own **pre-releases**, never marked Latest, so the PC release stays the Latest one:
tag `rpi-<date>` (`rpi-20261121`), title "Valence for Raspberry Pi (64-bit ARM) test build, <date>", the compressed
card image (`valence-rpi-<date>-aarch64.img.xz`), `SHA256SUMS`, its signature and the key. (Melon's 32-bit edition had
pre-releases the same way, `i686-<date>`; it is dropped for now.)

**New ISOs for meaningful changes (the owner's rule, 7 October 2026).** When a change lands that alters what is on an ISO or how it
installs or boots (kernel, drivers and firmware, installers, the desktop, base packages, boot, hardware support), new ISOs are built,
tested (see Testing) and added to a GitHub release:

1. Build and test the ISOs on a machine with the signing key (the owner's computer, or a cloud session the owner has given the key: it can
   sign packages and push the package repo through git).
2. Publish the packages the ISOs were made from (`scripts/publish-repo.sh`), then create the release (name, notes,
   the files `scripts/release.sh` made), mark it Latest, retitle the one it replaces. **A cloud session can't do
   this step**: GitHub answers 403 "not permitted for this session type" for release creation and asset uploads, so the ISOs go to the
   owner's computer (WSL, or through `/mnt/project-files/releases/<tag>/`) and `gh release create` / `gh release upload` run there.

The ISOs are hosted as this repository's release assets and nowhere else (the owner's decision, 20 November 2026:
ROADMAP.md, "Decided"); the package repository is its `packages` branch. Both reach everyone once the repository is
public. `docs/iso-hosting.md` compares other hosts and is kept for if that changes.

A change that touches only recipes in the online repository or docs doesn't need new ISOs.

## Signing keys and rotation

Installed systems are meant to trust two keys, both shipped in `/etc/apk/keys` by `apk-tools` (and in `keys/trusted/`):

- `valence-signing.rsa`: the everyday key. Everything is signed with it. Keep it on **one** build machine.
- `valence-backup.rsa`: the offline backup key. Its private half lives off every build machine (a USB stick
  in a drawer) and is used only for a rotation. Never leave it in `keys/`.

**Valence has no backup key yet** (18 November 2026). Its everyday key was made new on the first Valence build machine
(Melon's keys are not trusted), and `apk-tools` ships a backup key only when `keys/valence-backup.rsa.pub` exists.
This section wanted one before the first release; the owner chose to release 0.1.0 without one (20 November
2026). To add one, make it on a machine that is not the build machine (`openssl genrsa`, as in step 1 below),
commit its `.pub` to `keys/` and `keys/trusted/`, and bump `pkgrel` of `apk-tools`. Until then a lost or leaked
everyday key can't be rotated the way this section describes.

If `valence-signing.rsa` leaks or is lost:

1. Make a new everyday key: `openssl genrsa -out keys/valence-signing.rsa 4096`, then
   `openssl rsa -in keys/valence-signing.rsa -pubout -out keys/valence-signing.rsa.pub`, and copy the `.pub`
   into `keys/trusted/` (replacing the old one).
2. Bump `pkgrel` of `apk-tools` (it now ships the new public key and the backup key, not the old key) and
   build it signed with the backup key: `VALENCE_SIGN_KEY=/path/to/valence-backup.rsa scripts/valence-build apk-tools`.
   Installed systems accept it because they already trust the backup key.
3. Publish that transition repo (its index is signed with the backup key too) and let systems upgrade.
4. Rebuild and re-sign everything with the new key (`scripts/valence-build` with an empty `repo/`), publish.
   Systems that upgraded in step 3 trust only the new key and the backup key from then on.
5. Make a new backup key the same way, ship its public half in `apk-tools` (bump `pkgrel`), put the private
   half offline.

## Sources

**New sources come from upstream, not from Ubuntu** (the owner's decision, 29 September 2026). Take each project's own
release: the tarball from its official site or a pinned tag of its official repository. Firmware comes from upstream
linux-firmware (kernel.org), NVIDIA's driver from NVIDIA. Verify every download: the release's signature where the
project signs (the GNU keyring, kernel.org's `.sign` files, the release manager's key), otherwise its published checksum,
cross-checked against another distribution's recipe (Alpine's APKBUILD, Arch's PKGBUILD); say in the commit how it was
checked. List the file in `URL` (method `url`) or `GIT` in `scripts/make-manifest.py` and add its row to
`sources/MANIFEST.tsv` (sha256 checked on every fetch). On a machine that got its sources from
`fetch-sources.sh`, `make-manifest.py` rewrites rows that have nothing to do with the new file (git archives hash
differently from machine to machine, and a `deb/` file fetched from Launchpad has no `.dsc` beside it, so its
row is left out): run it, keep only the new rows, and check that `git diff` shows additions alone.

History: the first build container could only reach the Ubuntu archive and GitHub, so many older recipes still use
Ubuntu 26.04's `*.orig.tar.*` (the upstream tarballs, repacked by name only; method `apt`) and some firmware and data
come from Ubuntu `.deb`s (method `pool`). That's no reason to keep pulling from Ubuntu: when you touch one of those
recipes (a version bump, a fix), move its source to upstream. The build machine happens to run Ubuntu (BUILDING.md);
that is the only place Ubuntu is still needed.

## Roadmap

The stages and status notes in this section are Melon's, kept as its history: Valence's plan is `ROADMAP.md`. Of what
is listed here, everything builds on glibc for x86_64 ("State of the glibc switch" above); the 32-bit edition does
not, and the tests these notes speak of were Melon's, on musl.

- **Stage 1 (base): done.** Toolchain, base packages, the console ISO and the quick console installer, BIOS and
  UEFI, LUKS encryption, dual boot next to Windows, VM guest tools.
- **Stage 2 (desktop): done.** eudev, D-Bus, elogind, polkit, Mesa with LLVM, Qt 6, KDE Frameworks 6, Plasma and
  KWin on Wayland, SDDM, PipeWire, NetworkManager, Bluetooth, printing, Calamares, the desktop ISO
  and the desktop profile for both installers (`qemu-test.py desktop` and `desktop-install`). Open hardware work
  is tracked in GitHub issues: Intel SOF audio, newer linux-firmware, Broadcom Wi-Fi.
- **32-bit (i686) edition with LXQt: built, signed and published, waiting for real hardware** (resumed by the owner,
  7 October 2026, for an MSI Wind U100: Atom N270, 2 GB). `VALENCE_ARCH=x86 scripts/build-everything.sh` builds the
  toolchain, the base system, Qt, LXQt and labwc, then both ISOs (BUILDING.md). Both pass their tests on QEMU's Atom
  N270 CPU model: the console ISO (`qemu-test.py live|disk --i686`) and the LXQt desktop ISO
  (`desktop-install --i686`, 2 GB of RAM: install through Calamares, SDDM's greeter on labwc, the LXQt session,
  printing). Not yet on 32-bit: the QEMU guest agent. Signed with Valence's key and published (7 October 2026): `repo/x86` is on the `packages` branch
  next to `x86_64` (publish-repo.sh replaces the whole branch, so a machine that built only one architecture must start
  from the branch's current contents); both ISOs rebuilt from scratch on a new build container and retested (the desktop
  test waits 90 s before typing when there is no KVM: the greeter ignores keys for a while on an emulated CPU). When
  Ubuntu's source tool can't find an old version, its `orig` tarballs are still in `archive.ubuntu.com/ubuntu/pool/`
  (same sha256 as the manifest). Next: a test on the owner's U100 (`sudo valence-hwreport` for Wi-Fi and graphics).
- **Stage 3 (gaming): in progress.** Done: Flatpak, the Flathub remote (`valence-flathub`), Steam, Firefox, VLC and
  Prism Launcher offered from Flathub on first login, GameMode. Still to do: gamepad and controller udev rules,
  MangoHud (`docs/stage2-plan.md`).
- **Init: runit stays Valence's init (the owner's decision).** dinit may one day become an *optional variant* the owner
  builds, never a switch forced on the whole OS. Don't start a dinit port. Improve runit instead:
  readiness through `./check` scripts, and a clear start order in run scripts (a small shared helper is being
  considered). A variant would need services for both inits, so keep run scripts simple and self-contained; what a
  dinit variant would have to cover is listed in `docs/dinit-variant.md`.
- **Smaller desktop ISO: done.** The ISO's offline repo no longer carries a second copy of the desktop (it did: about
  460 MB on the ISO and on every install, in `/var/lib/valence/repo`). The installers only need that repo for extras; a
  desktop app someone removes comes back from the online repo. Tested with `qemu-test.py ... --offline`.
- **Rust:** recipes can be written in Rust (rule 46; ripgrep is the first), and Valence has its own `rust` and `cargo`
  packages (`recipes/rust`, rustc 1.98.1 built from source, in the package repository only, not on the ISOs). Built
  and tested on the build machine (29 September 2026): cargo builds and runs a program with a crates.io dependency on
  Valence. `codegen-tests = false` in its `bootstrap.toml`, because Valence's llvm ships no FileCheck. The recipe is
  built cross-native with build = host = target = `x86_64-unknown-linux-gnu`, from the build machine's Rust as
  stage 0, against Valence's LLVM 21 (shared), and `cargo build` on Valence links against the system's glibc like
  Valence's own programs. The recipe says `options=(rust)` (rule 46). Watch
  for: RAM (rustc wants 2 to 3 GB per job), the size of `librustc_driver` against GitHub's 100 MB file limit
  (rule in "Package repository"), and `LEAK:` lines.
- **Self-hosting (building Valence on Valence): in progress.** Step 1, the build tools as recipes, is done. The first batch (perl,
  m4, bison, flex, gawk, gperf, bc, texinfo, autoconf, automake, autoconf-archive, file) is done and tested on Valence
  (an autotools project with a bison grammar, a flex scanner, gperf and a Texinfo manual builds and runs; a Perl XS
  module builds and passes its tests). The second batch (cmake, meson, ninja, git, nasm, tcl, rsync, lz4) is done too:
  cmake and meson projects build with ninja, git clones over https, nasm output links and runs. The third batch
  (xorriso, mtools, scdoc, itstool, dtc, pahole, the Public Suffix List, and Python's mako, PyYAML, packaging, pexpect
  and libxml2 bindings) too: each was run on Valence (an ISO and a FAT image made, itstool, scdoc, dtc and pahole output,
  pexpect driving a shell). **Step 2 is done too:** clang 21 with libclc and the SPIR-V translator (Mesa's
  `mesa_clc` and `vtn_bindgen2` build on Valence from Mesa's source and turn OpenCL C into valid SPIR-V), QEMU 11.1 with
  libslirp and OVMF (Valence's own `qemu-test.py live`, `disk` and `disk --uefi` pass when run inside Valence with Valence's
  QEMU, Python and pexpect), Go 1.27 (cgo on by default with Valence's gcc) and gh. On Melon, Claude Code installed with
  Anthropic's own installer, which picked its musl build; that is not tested on glibc yet (README, "Developer tools").
  It isn't packaged, as it isn't open source. Step 3: build Valence on Valence (`build-everything.sh` inside a Valence system, the recipes' remaining build-machine
  tools such as Mesa's `hosttools/bin` and the host Qt and Rust replaced by Valence's own), run the test suite there, and
  compare the packages with the WSL-built ones.
- **NVIDIA (the owner: both drivers, 29 September 2026): built, waiting for a test on real hardware** by a contributor
  with an NVIDIA card (GTX 16/RTX 20 or newer). Both ways are online installs chosen in `valence-first-boot`. Later: a
  `linux-valence-dev` package (the kernel's build files, `Module.symvers`) would spare nvidia-open its own kernel build and
  let people build other out-of-tree modules on Valence.
- **Later:** a native Firefox build (needs clang and Node for Valence as well as Rust; Firefox comes from Flathub until then).

## Contributing: workflow and the owner's rules

- **Pull requests go to `testing`.** `main` is protected ("changes must be made through a pull request"): nobody
  pushes to it directly, the owner and the build machine included. `testing` reaches `main` through its own pull
  request (`testing` -> `main`, merge commit), after which `testing` is fast-forwarded to `main` again.
- **Rebase, don't merge.** When `testing` moves, rebase your branch; no "Merge testing into ..." commits. PRs are
  squash-merged. No test/webhook commits and no personal email addresses in history (use GitHub's noreply address).
- **Delete a branch once its PR is merged or closed.** GitHub's "delete branch on merge" is off, so branches pile up. Before deleting
  one, check that no open PR, open issue or running thread still uses it, and write its name and last commit down so it can be restored.
  A squash merge leaves the branch's own commits looking unmerged, so compare file contents with `testing`, not commit counts.
  Keep `main`, `testing`, `gh-pages` and `packages`. (11 merged branches were cleaned up on 8 October 2026.)
- **The build machine reviews PRs** about every 20 minutes. Its comments start with "Automated check from the Valence
  build machine:" or "Automated reply ...". It reads the diff, builds every changed recipe, installs and runs the
  packages on a scratch Valence root, runs the QEMU install tests when ISOs or installers change, and then requests
  changes with the exact errors, asks the owner about product decisions, or merges into `testing`. It never pushes
  to a contributor's branch. The owner answers on PRs or on the pinned issue #17 ("Owner <-> build machine").
- **What goes on an ISO (the owner's size rule):** small packages, below about 45 MB, may go on the ISOs; anything
  bigger is an online install from the package repository (gcc and friends, clang, QEMU). **Extra desktops are
  always online installs, whatever their size**: Plasma stays the only desktop on the 64-bit ISO (issue #18, niri +
  Noctalia). The 32-bit desktop ISO carries LXQt instead of Plasma (the owner's decision); LXQt is in the 64-bit
  package repository too, as an online install.
- **Decided, don't re-propose:** Nix is available but off by default (not on the ISOs, no service, only root trusted);
  foreign repos stay opt-in and tagged.

## Git conventions

- Small, focused commits. The message says what changed and why.
- Build outputs and downloaded sources are never committed.
- If you change a recipe, bump `pkgrel` (or `pkgver`) in the same commit.
- **Keep the docs true (the owner's rule, 7 October 2026):** when you find a bug or make a significant change, update every
  `.md` file it touches (README, AGENTS, BUILDING, `docs/`) in the same pull request, so nothing is out of date.
