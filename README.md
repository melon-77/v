# Valence Linux

> **Contributing: open pull requests against the `testing` branch only, never `main`.**
> `main` is the stable branch that ISOs and the package repository are built from. Changes reach it only
> when the owner has personally reviewed and tested them on `testing` and decides to bring them over.
> Pull requests against `main` will be closed. More in [CONTRIBUTING.md](CONTRIBUTING.md).

Valence is a rolling Linux distribution for 64-bit PCs, built from source: glibc and the GNU tools, runit as init,
apk-tools 3 for signed packages, and a Btrfs root that takes a snapshot of itself before every update, so a bad
update can be taken back from a command or from the boot menu. Its desktop is KDE Plasma 6 on Wayland, with Flatpak
for apps such as Firefox and Steam. It is the successor to [Melon Linux](https://github.com/melon-77/melon-os).

## Get it

- **[Install guide](docs/install.md)**: which ISO, checking it, writing it to a USB stick, both installers,
  installing next to Windows, the first start.
- **[Updating and repairing](docs/updating.md)**: updates, software, snapshots and going back, repairing from the
  USB stick.
- The ISOs are on the [Releases page](https://github.com/melon-77/v/releases): `valence-desktop-<version>` (KDE
  Plasma, live and installable) and `valence-<version>` (the console system), with `SHA256SUMS` signed by
  Valence's key.

**Status.** 0.1.0 is the first release. Both ISOs, both installers, encryption, installing next to Windows and
rolling back pass automated tests in virtual machines (QEMU), on BIOS and UEFI; **Valence has not been tried on a
real computer yet.** The package repository becomes reachable when this repository is made public; until then an
installed system has what the ISO brought and cannot update. [ROADMAP.md](ROADMAP.md) is the plan, with what is done
and what comes next.

## What it is

| | |
|---|---|
| libc | glibc 2.44 |
| userland | the GNU tools (coreutils, findutils, grep, sed, tar, gzip, diffutils) with less, procps-ng, iproute2 and shadow; BusyBox 1.37 for `/bin/sh`, the initramfs and the small daemons |
| packages | apk-tools 3: signed packages, built from the recipes in this repository |
| init | runit 2.3 |
| kernel | Linux 7.0 |
| boot | GRUB 2.14; one ISO and one install boot on both BIOS and UEFI |
| root fs | Btrfs, with the system, `/home`, `/var/log`, the snapshots and the swap file each in a subvolume of its own; FAT32 `/boot` |
| snapshots | apk takes one before every change to the installed packages; the boot menu starts any of them, and `valence-snapshot rollback` makes one the default, the way NixOS's generations work |
| wish lists | a file like `packages = firefox, nano` / `services = sshd` / `hostname = ...` that `valence-wish` (or the installer) applies in one go: it only adds, takes a snapshot first, and changes nothing the second time |
| documentation | man pages and other docs are in `<package>-doc` packages, translations in `<package>-lang`; `apk add docs` brings the docs of everything installed; `man` is mandoc |
| desktop | KDE Plasma 6 on Wayland, SDDM, PipeWire, NetworkManager, CUPS; Flatpak and Flathub for big apps |
| language | US English (the base tools have no translations; Plasma's language can be changed) |
| shell | bash 5.3 |

## Documentation

| | |
|---|---|
| [docs/install.md](docs/install.md) | installing, for someone who has never installed Linux |
| [docs/updating.md](docs/updating.md) | keeping it up to date, snapshots, repairing |
| [CONTRIBUTING.md](CONTRIBUTING.md) | how to send changes: branches, recipes, sources, tests |
| [ROADMAP.md](ROADMAP.md) | the plan: phases, what is decided, what is open |
| [BUILDING.md](BUILDING.md) | setting up a build machine (Ubuntu, Debian, WSL2 on Windows) and how long builds take |
| [AGENTS.md](AGENTS.md) | the build system's manual: recipe format, the numbered rules, the installers, testing |

## Building

```sh
scripts/host-setup.sh                         # the build tools and the host's own Qt, Rust, GRUB (once)
scripts/fetch-sources.sh                      # the sources, checked against sources/MANIFEST.tsv
scripts/toolchain.sh                          # the x86_64-valence-linux-gnu cross toolchain
scripts/valence-build                         # every package, in the order the recipes' dependencies give
scripts/mkiso.sh                              # the console ISO; VALENCE_EDITION=desktop for the desktop ISO
```

`scripts/valence-build` works the order out itself, builds each package in a sysroot that holds only what its recipe
declares, and builds nothing that is already built: from an empty repository, all 451 recipes take about five hours
on a 12-thread laptop. `--edition console` or `--edition desktop` builds only what that ISO needs. Packages are signed
with `keys/valence-signing.rsa`, which is not in the repository: make your own with
`openssl genrsa -out keys/valence-signing.rsa 4096` and put its public half in `keys/` and `keys/trusted/`.
`BUILDING.md` has the details and the traps.

```
scripts/toolchain.sh        the cross toolchain (binutils 2.46, GCC 15.2, glibc 2.44)
scripts/valence-build       the builder: recipes -> signed .apk packages in repo/
scripts/mkiso.sh            the hybrid BIOS/UEFI live ISOs, from the packages
scripts/qemu-test.py        the automated tests (scripts/test-console.sh and test-desktop.sh run them all)
recipes/<pkg>/VALENCEBUILD  the package recipes
recipes/valence-base/       Valence's own commands: valence-install, valence-snapshot, valence-wifi, ...
recipes/calamares-valence/  the graphical installer's settings, look and finishing scripts
art/                        the logo and the desktop's pictures and colours, made by scripts
```

## Developer tools

Compilers and build tools are in the package repository, not on the ISOs: GCC 15.2, clang 21, Rust 1.98, Go 1.27,
perl, Python, cmake, meson, git and more. Each has been tried in a root made of the packages, not yet on an
installed system.

```sh
sudo apk add gcc g++ make pkgconf      # also offered at the first login
sudo apk add clang go cmake meson git github-cli
sudo apk add qemu ovmf                 # virtual machines, BIOS and UEFI
```

**Claude Code** is not packaged; it installs with Anthropic's own installer, which has not been tried on Valence
yet:

```sh
sudo apk add bash curl ca-certificates libgcc libstdc++ ripgrep
curl -fsSL https://claude.ai/install.sh | bash
echo 'export USE_BUILTIN_RIPGREP=0' >> ~/.bashrc    # Claude Code then searches with Valence's ripgrep
```

## NVIDIA graphics

On a computer with an NVIDIA card (GTX 16/RTX 20 and newer), the first login offers a driver: the open one
(`sudo apk add linux-firmware-nvidia mesa-nvk`: nouveau with NVK, the desktop and games use the card) or NVIDIA's own
kernel driver (`sudo apk add nvidia-open`: games from Flathub get NVIDIA's libraries from Flathub). Restart after
installing either. Neither has been tried on a real card yet.

## Boot time

On a computer with only Valence, the boot menu is skipped after 1 second (hold Shift or press Esc to open it; it
has a "safe graphics" entry). `GRUB_TIMEOUT` and `GRUB_TIMEOUT_STYLE` in `/etc/default/grub` change that, and
`sudo valence-update-grub` applies it. Next to Windows the menu shows for 5 seconds. Startup mounts the disks as soon
as they are there and lets the graphics, Wi-Fi and sound drivers finish while the services start
(`valence.udev=sync` on the kernel command line brings back the slower, stricter order if a machine misbehaves).
`valence-boottime` shows where a start's time went.

## Raspberry Pi

A 64-bit test image for the Raspberry Pi 3, 4, 400, 5 and Zero 2 W: the console system on an SD card
([docs/raspberry-pi.md](docs/raspberry-pi.md)). It passes its test in QEMU's emulated Pi 3B and has not been tried on a
real board yet (ROADMAP.md, phase 9).

## 32-bit edition

Melon had a 32-bit edition with LXQt for old netbooks. It has not been built on Valence's glibc, and whether Valence
keeps it is undecided (ROADMAP.md). Its desktop's recipes (LXQt, labwc) build for x86_64.

## History

This tree was imported from Melon's `main` at `8849e5a` on 18 November 2026 and changed from there: glibc instead of
musl, the GNU tools instead of BusyBox's, documentation and translations in their own packages instead of deleted,
a builder that works out the build order itself, Btrfs with snapshots and rollback, and its own look. Notes in
`AGENTS.md` and `BUILDING.md` that are dated before then describe Melon.
