# Valence Linux

> **Contributing: open pull requests against the `testing` branch only, never `main`.**
> `main` is the stable branch that ISOs and the package repository are built from. Changes reach it only
> when the owner has personally reviewed and tested them on `testing` and decides to bring them over.
> Pull requests against `main` will be closed.

## Status: being ported from Melon Linux

Valence is the successor to [Melon Linux](https://github.com/melon-77/melon-os). It starts as a port of Melon's
engineering (this tree was imported from Melon's `main` at `8849e5a` on 18 November 2026) and then changes a handful
of things: glibc instead of musl, documentation and translations in their own packages instead of deleted, a builder
that orders builds itself, and a Btrfs root with a snapshot before every update. **[ROADMAP.md](ROADMAP.md) is the
plan**: what stays, what changes, the phases with their "done when", and what is still undecided.

There is one test build, a console ISO, on the repository's Releases page, and no online package repository yet. The
base system is on glibc: the cross toolchain and the 62 recipes the console ISO is made of build on it, in an order
the builder works out from the recipes, each in a sysroot holding only what its recipe declares (ROADMAP.md has the
test status). Its documentation is in packages of its own: a fresh install has no man pages, and `apk add docs`
from a package repository adds them, with `man`, for everything installed. The rest is still as
inherited from Melon: the XFS root, the desktop and every other recipe beyond the console ISO (not yet rebuilt on
glibc, with Melon's musl workarounds still in them), the 32-bit edition, and dated status notes that are Melon's
history. Where this file, `AGENTS.md` and `BUILDING.md` describe those parts, they describe Melon's system. The
desktop's pictures (wallpaper, splash, login and boot menu backgrounds) are still Melon's too; the logo is Valence's.

## What it is

A from-scratch, rolling x86_64 distribution:

| | |
|---|---|
| libc | glibc 2.44 |
| userland | the GNU tools (coreutils, findutils, grep, sed, tar, gzip, diffutils) with less, procps-ng, iproute2 and shadow; BusyBox 1.37 for `/bin/sh`, the initramfs and the small daemons |
| documentation | man pages, info pages and docs are in `<package>-doc`, translations in `<package>-lang`; installing `docs` or `lang` brings them for everything installed; `man` is mandoc |
| init | runit 2.3 |
| packages | apk-tools 3 (signed apk v3 packages, binary + source recipes) |
| kernel | Linux 7.0, generic flavour |
| boot | GRUB 2.14, one ISO and one install that boot on both BIOS and UEFI |
| root fs | XFS (FAT32 `/boot`) |
| shell | bash 5.3 |
| desktop | KDE Plasma 6 on Wayland; Flatpak for Steam and other apps; more desktops as online installs |
| 32-bit edition | i686 (Pentium M, Pentium 4, Atom netbooks such as the MSI Wind U100) with LXQt on Wayland (labwc); Melon's, not built on glibc, and undecided for Valence (ROADMAP.md) |

## Layout

```
scripts/toolchain.sh   builds the x86_64-valence-linux-gnu cross toolchain (binutils 2.46, GCC 15.2, glibc 2.44)
scripts/valence-build    builds packages into signed .apk files, in the order their recipes' dependencies give, each in a clean sysroot
scripts/valence-run    runs a program built for Valence on the build machine, through the sysroot's loader
scripts/mkiso.sh       assembles the hybrid BIOS/UEFI live ISO from the repo
recipes/<pkg>/VALENCEBUILD   package recipes (APKBUILD-like)
iso-files/init         live initramfs init
branding/              the logo (SVG and PNGs)
art/logo.py            draws the logo
ROADMAP.md             the plan and its phases
recipes/calamares-valence/   graphical installer: Calamares settings, branding and finishing scripts
```

## Building

Sources come from upstream releases (`scripts/fetch-sources.sh`, into `sources/`; older recipes still use the Ubuntu
source archive's copies of the same tarballs, and new ones come straight from upstream). Then:

```sh
scripts/toolchain.sh
scripts/valence-build --edition console   # the console system and all it needs, in the order the recipes give
scripts/mkiso.sh
```

The build machine can be Ubuntu 24.04 (or WSL2, also on Windows 10: see BUILDING.md for its traps), Debian or Devuan, or **Valence itself**: `scripts/host-setup.sh`
notices which one it runs on and installs the build tools with `apt` or with Valence's own `apk`. Building on Valence
itself was set up when it was a musl system and is untested on glibc. `BUILDING.md` has
them all, including what to keep and what to rebuild when moving a build machine.

Packages are signed with `keys/valence-signing.rsa` (not committed; generate your own with
`openssl genrsa -out keys/valence-signing.rsa 4096` and put the public half in `keys/`).

## Installing

Boot the ISO. The desktop ISO's graphical installer is Calamares ("Install Valence" on the live desktop, or
`valence-install-gui`). Both ISOs also have a console installer, `valence-install` (it needs root and runs itself through
doas otherwise): base or desktop system, a whole disk or next to Windows on UEFI, optional LUKS encryption, and
`VALENCE_*` variables for unattended installs (`valence-install --help`).

The ISOs are published as GitHub release assets; `docs/iso-hosting.md` compares other free file hosts (SourceForge,
Internet Archive, Cloudflare R2). Where else to host them is still open.

## 32-bit edition (old netbooks)

This is Melon's 32-bit edition as inherited. It has not been built on glibc, and whether Valence keeps it is undecided
(ROADMAP.md).

Valence also builds for 32-bit PCs with SSE2 (Pentium M, Pentium 4, Intel Atom: the MSI Wind U100 and similar netbooks).
Its desktop is **LXQt** on Wayland with the labwc compositor, lighter than Plasma and comfortable in 1–2 GB of RAM,
with zram swap and drivers for every graphics and network chip an old 32-bit netbook or laptop is likely to have: Intel GMA, old Radeon (r300, r600) and GeForce (nouveau) graphics; Atheros, Ralink, Realtek, Intel 3945/4965 and Broadcom Wi-Fi, USB Wi-Fi sticks and the usual wired chips; the MSI Wind's Fn keys and radio switch (msi-laptop). It
uses the same installers (Calamares and the console installer). Flatpak and the NVIDIA drivers are
64-bit only. Build it with `VALENCE_ARCH=x86` (`BUILDING.md`); the ISOs are `valence-*-i686.iso` and
`valence-desktop-*-i686.iso`. Status (7 October 2026): both ISOs are built, signed with Valence's key and pass their install tests on an emulated
Atom N270 with 2 GB of RAM (LXQt logs in through SDDM, printing works); the 32-bit packages are in the online
repository (`packages/x86`); the ISOs are on the pre-release `i686-20261007`; a test on a real U100 comes next. The QEMU
guest agent isn't in the 32-bit edition yet.

## Developer tools

Compilers and build tools come from the package repository (they're not on the ISOs), for example:

```sh
sudo apk add gcc g++ make pkgconf      # also offered on first login
sudo apk add clang go cmake meson git github-cli
sudo apk add qemu ovmf                  # virtual machines with KVM, BIOS and UEFI
```

**Claude Code** is not packaged; it installs with Anthropic's own installer. On Melon the installer recognised musl
and installed its musl build for your user, with the commands below. Valence is now a glibc system, where software
prebuilt for glibc can run on the system itself; the installer has not been tested on it yet:

```sh
sudo apk add bash curl ca-certificates libgcc libstdc++ ripgrep
curl -fsSL https://claude.ai/install.sh | bash
echo 'export USE_BUILTIN_RIPGREP=0' >> ~/.bashrc    # Claude Code then searches with valence's ripgrep
```

## Boot time

An installed system that has no other operating system next to it skips its boot menu after 1 second (hold Shift or press Esc
to open it; "safe graphics" is in there; `GRUB_TIMEOUT` and `GRUB_TIMEOUT_STYLE` in `/etc/default/grub` change this, and
`sudo valence-update-grub` applies it). Next to Windows the menu still shows for 5 seconds. Startup no longer waits for every
device to be probed: it mounts as soon as the disks are there and lets the graphics driver, Wi-Fi and sound finish while the
services start (`valence.udev=sync` on the kernel command line brings the old, slower order back if a machine misbehaves), and
services that wait for each other check every 20 ms instead of every 0.42 s. `valence-boottime` shows where a boot's time went:
the milestones of stage 1 and when the main daemons started, to 10 ms.

**NVIDIA graphics:** on a computer with an NVIDIA card (GTX 16/RTX 20 and newer), `valence-first-boot` offers the open
driver (`sudo apk add linux-firmware-nvidia mesa-nvk`: nouveau with NVK, the desktop and games use the card) or NVIDIA's
own kernel driver (`sudo apk add nvidia-open`: Flatpak games get NVIDIA's libraries from Flathub). Restart after
installing either. On Melon the desktop couldn't use NVIDIA's libraries, as they need glibc; on glibc it could, but
Valence has no package of them, and neither driver has been tested on glibc.
