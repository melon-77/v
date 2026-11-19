# Valence: plan and roadmap

Valence is a from-scratch, rolling x86_64 Linux distribution. It is the serious successor to
[Melon Linux](https://github.com/melon-77/melon-os): the same engineering, without the jokes, and with a handful
of fixes where Melon chose small over sensible.

Status, 19 November 2026: phases 1 to 3 and 5 are done. The base system is on glibc 2.44 and its commands are the
GNU tools: the cross toolchain, the 58 recipes the console ISO is made of and the ISO build. The ISO passes all
eight console tests in QEMU (install and boot on BIOS, boot on UEFI, an encrypted install and its boot, dual boot next
to Windows on UEFI, and two rollback tests), and the installed system adds a package from a repository over the
network. BusyBox stays for `/bin/sh`, the initramfs and the small daemons. Documentation and translations are
packages of their own: the builder makes `<pkg>-doc` and `<pkg>-lang` from what a recipe installs, no recipe deletes
them any more, and a fresh install has no documentation files until `apk add docs` (or one `-doc` package) brings
them, with `man`. The recipes beyond the base (the desktop, the developer tools) have not been rebuilt on glibc, so
their `-doc` and `-lang` packages do not exist yet. Phase 4's builder is written and in use for the base: from an
empty repository, one command built the console system in an order worked out from the recipes, each package in a
clean sysroot, and a second run built nothing. It cannot rebuild the whole repository yet, because the recipes beyond
the base do not build on glibc and the desktop's generated recipes do not say what they need: that part of phase 4
waits for phase 6. The console installer makes a Btrfs root in subvolumes, and apk takes a snapshot before every
change to the installed packages. Going back works the way NixOS's generations do: the boot menu lists every
snapshot and starts the one chosen, and `valence-snapshot rollback` makes a snapshot the system that starts by
default. The tests break an update on purpose both ways: one program, undone with the command; and init together
with the kernel, after which the disk does not boot until the boot menu starts the snapshot from before it. On a
plain disk on BIOS and on an encrypted one on UEFI. The graphical installer is part of the desktop and still makes
an XFS root. Nothing later has started.

## Why it exists

Melon is close to the distro I want to use: runit, apk, a cross toolchain built from source, Plasma on Wayland,
one ISO that boots anywhere. But it began as a joke, and a lot of it still is one (the installer asks 200
questions before it lets you in). It is hard to recommend to anyone, and hard to take seriously myself.

Valence keeps what works and drops the bit. One rule decides the rest:

> Reasonable, not extreme minimalism to chase a small number.

Melon deletes man pages and translations to make the ISO smaller, and uses musl partly because it is small.
Valence stops optimising for the number and optimises for a system that behaves the way people expect.

## What stays the same as Melon

| | |
|---|---|
| Init | runit |
| Packages | Signed apk v3 packages, built from source recipes. apk-tools 3 installs them until Valence's own package manager does (phase 9) |
| Recipes | Melon's APKBUILD-like format. Most of its 442 recipes come over with little change |
| Toolchain | A cross toolchain built from source (binutils, GCC), then the system rebuilds itself |
| Kernel | Mainline Linux, generic flavour |
| Boot | GRUB; one ISO and one install that boot on both BIOS and UEFI |
| Desktop | KDE Plasma 6 on Wayland, no separate X11 session; SDDM; Flatpak available |
| Installer | Calamares |
| Testing | Installs tested automatically in QEMU, on BIOS and on UEFI |
| Releases | Rolling. Changes land on `testing` and reach `main` only after they have been tested there |
| `tribe.txt` | Stays |

## What changes

**1. glibc instead of musl.**
The toolchain target becomes `x86_64-valence-linux-gnu`. NVIDIA's own driver, prebuilt binaries and most
proprietary software then run on the system itself; on Melon they only work inside Flatpak. It also removes
work: 29 of Melon's recipes carry musl workarounds, and they can go, along with the musl patches.

**2. Documentation and translations are split out, not deleted.**
304 of Melon's 442 recipes delete man pages, docs or locales as they package. In Valence those files go into
`-doc` and `-lang` subpackages. The ISO can still leave them off, and anyone who wants them can install them.

**3. One builder that works out the build order itself.**
Melon builds through `queue-1.sh` to `queue-4.sh`, `retry-failed.sh` and `resume.sh`, with the order kept by
hand. `valence-build` reads each recipe's dependencies, builds in the right order, and rebuilds only what
changed. A full rebuild from nothing is one command.

**4. Btrfs, with a snapshot before every update.**
Melon's root is XFS. Valence's is Btrfs, and updating takes a snapshot first, so a bad update on a rolling
distro is a rollback and not a reinstall.

**5. The joke layer goes.**
Removed: the 200-question gauntlet and the secret unlock in the installer, the console installer's song and
intro, `melon-sounds`, `melon-pinball`, `nethack`, and the in-jokes scattered through the tree. The installer
is plain Calamares and first boot has normal defaults.

**6. A proper identity.**
A new name, a logo (a bismuth hopper crystal seen from straight above; the files are in `branding/`), and real
documentation: how to install, how to update, how to recover, and how to contribute.

**7. Its own package manager, and a folder for every program.**
On the installed system apk gives way to a package manager written for Valence: fast, with a proper screen to
watch, and built around one idea from GoboLinux, that every program lives in a folder of its own. The builder,
the recipes and the signed packages stay as they are. Phase 9 has the design.

**8. The system can be described in one file.**
A proposal (19 November 2026). The packages, the services, the users and the settings of a system are written in
one file, and one command makes the system match it, as NixOS does with `configuration.nix`. It stands on the
snapshots of point 4, not on a store like Nix's. Phase 10 has the design.

## Roadmap

There are no dates. A phase is finished when its "done when" is true, and the next one starts after that.

- [x] **0. Name, logo, plan.** This file.

- [x] **1. Bring Melon's engineering over.**
  Copy the toolchain, build and ISO scripts, the QEMU tests, the signing setup and the ordinary recipes
  (about 430). Rename `melon` to `valence` throughout: the target triple, `melon-build`, `melon-base`,
  `melon-layout`, `linux-melon` and the rest. Leave out everything listed under "The joke layer goes".
  *Done when:* nothing in the tree says "melon" except the credit line, and every recipe still parses.

- [x] **2. Switch to glibc.**
  `toolchain.sh` builds binutils, GCC and glibc for `x86_64-valence-linux-gnu`. Rebuild the base on it
  (layout, kernel headers, glibc, GCC runtime, zlib, zstd, OpenSSL, apk-tools, the shell and core tools, runit,
  GRUB). Delete the musl-only patches and workarounds as each recipe is rebuilt.
  In two halves, so a failure points at one change: first glibc with BusyBox still providing every command, tests
  green; then the GNU tools on the installed system (see "Decided" below), tests green again.
  *Done when:* the console ISO boots in QEMU on BIOS and on UEFI, installs, and the installed system can
  `apk add` a package.

- [x] **3. Split `-doc` and `-lang`.**
  Teach the builder to move man pages, info pages and docs into `<pkg>-doc`, and locales into `<pkg>-lang`.
  Remove the delete lines from the 304 recipes that have them.
  *Done when:* a base install has no docs and is no bigger for it, and installing a `-doc` package makes
  `man` work for that package.

- [ ] **4. `valence-build`.**
  The dependency-ordered builder replaces the queue, retry and resume scripts.
  *Done when:* one command rebuilds the whole repository from empty with no order written down anywhere, and a
  second run with nothing changed builds nothing.
  *So far (19 November 2026):* true for the console system, 65 recipes. The queue, retry, resume and build-all
  scripts and the ordered lists in `build-everything.sh` are gone. Open: the other 388 recipes, which must first
  build on glibc and declare their dependencies (phase 6).

- [x] **5. Btrfs and snapshots.**
  The installer creates a Btrfs root with subvolumes. Updating takes a snapshot first. Rolling back is
  documented and tested.
  *Done when:* a QEMU test breaks an update on purpose, rolls back, and boots.

- [ ] **6. The desktop.**
  Rebuild the Plasma 6 stack on glibc (Mesa, Qt, KDE Frameworks, Plasma, SDDM, PipeWire, the apps Melon
  ships). Plain Calamares branding, and Calamares makes the same Btrfs layout as the console installer (it still
  makes an XFS root, which has no snapshots). A normal first boot. The wallpaper, the splash, login and boot menu pictures
  and the green and orange colour scheme are still Melon's, and are replaced here.
  *Done when:* the desktop ISO installs in QEMU on BIOS and UEFI and logs in to Plasma through SDDM.

- [ ] **7. Documentation and the first test release.**
  README, install guide, update and recovery guide, contributing rules (`testing` before `main`, as in Melon).
  Signed ISOs with checksums.
  *Done when:* someone who has never seen Melon can install Valence from the guide alone.

- [ ] **8. A 64-bit ARM edition for Raspberry Pi.**
  A second architecture, `aarch64-valence-linux-gnu`, next to x86_64 in the same tree (the way Melon carried its
  32-bit edition): cross toolchain, the base system, a Pi kernel and firmware, and a disk image to write to an SD
  card instead of an ISO. It needs the build to stop running freshly built programs directly on the build machine,
  which phase 2 has to solve for glibc anyway, so it starts after phase 2. Console system first; the desktop follows
  phase 6. Assumed: the 64-bit boards (Pi 3, 4, 5, Zero 2 W), not the 32-bit ones.
  *Done when:* the image boots on a Raspberry Pi 4 to a login prompt, gets a network address, and can `apk add` a
  package; and the same image boots in QEMU as an automatic test.

- [ ] **9. Valence's own package manager, with a folder for every program.**
  A new package manager for the installed system, in Rust, reading the same signed `.apk` packages and
  repositories that the builder makes today.
  *A folder per program*, as in GoboLinux: each package is unpacked into a directory of its own,
  `/Programs/<name>/<version>`, and those directories are the record of what is installed. Removing a program
  is removing its folder. The usual tree (`/usr/bin`, `/usr/lib`, ...) is made from the folders with hard links:
  each file is the same file under a second name, so every program sees ordinary files where it expects them,
  software that was not built for Valence included, and nothing needs patching. (GoboLinux uses symbolic links;
  programs that look for their files next to themselves, such as Qt and Python, then need fixes.) Hard links do
  not cross file systems or Btrfs subvolumes, so `/Programs` and `/usr` share one. Files under `/etc` are copies,
  because they are edited.
  *Speed*: packages download and unpack in parallel, each into its own folder, where nothing can collide. An
  upgrade touches the running system in one short step at the end, when the usual tree is pointed at the new
  folders; the old folders stay until they are cleaned up, so going back is the same step in reverse.
  *Something to watch*: a full-screen view on a terminal, plain lines on a serial console or in a script.
  In stages: the engine first, on the console system next to apk and measured against apk and xbps; then the
  installers and the ISO; apk leaves the installed system last. The build machine keeps using apk for its sysroot.
  *Done when:* a console install and a desktop install made by it pass the QEMU tests; an upgrade that is killed at
  any point leaves a system that boots; and it installs the base system measurably faster than apk and xbps do
  on the same machine.
  Open: its name. Whether `-dev`, `-doc` and `-lang` parts get folders of their own or live inside the program's
  folder. And when: before the desktop, the desktop is fitted to the layout once.

- [ ] **10. A system described in one file.**
  A proposal, put here on 19 November 2026; the design below is a starting point. One file says what the system
  is: its packages, the services that run, the users and their groups, the host name and time zone, kernel
  options. One command makes the system match the file, the way `nixos-rebuild` does with `configuration.nix`.
  It stands on phase 5 and needs nothing later. The command makes a writable copy of the system, as the boot menu's
  snapshot entries do; applies the file inside the copy (apk for the packages, whose list `/etc/apk/world` already
  is; links in `/var/service` for the services; shadow's tools for the users); and swaps the copy in as the system
  that starts by default, from the next start on, as a rollback does. If a step fails, the copy is thrown away and
  the running system was never touched. Each apply is one more entry in the boot menu, so going back is what phase
  5 already does.
  What it is not: Nix. The repository is rolling, so the same file gives newer packages next month unless it also
  pins the repository to a date. What was done by hand stays (an edit in `/etc`, a package added with apk), outside
  the things the file manages. The format is a plain list of settings, not a language. Several versions of a
  program side by side, and environments per user, need a store: the folders of phase 9 are where those could come
  from, and that package manager could take the same file as its input.
  In stages: packages and services first; then users, the host name and the rest; then the installers write the
  file for a new system, so that every install starts with one.
  *Done when:* a QEMU test installs a system, changes the file (a package and a service added, another package
  removed), applies it, restarts into a system that matches the file, and gets the earlier one back from the boot
  menu; and an apply that is made to fail leaves the running system and the default as they were.

## Not decided yet

These are open on purpose. Each gets settled before the phase that needs it.

- **The 32-bit edition.** Melon has an i686 build with LXQt for old netbooks. Carry it over, or leave it with
  Melon? (After phase 6.)
- **Steam.** Native Steam needs 32-bit libraries next to the 64-bit ones, which is a lot of extra packages.
  Until that is worth doing, Steam stays on Flatpak. (After phase 6.)
- **Snapshot details.** apk keeps its ten newest snapshots (NixOS keeps every generation until told to collect
  garbage); `/home` and `/var/log` are outside them, so going back leaves both alone; the filesystem is compressed
  with zstd at level 1; the command is called `valence-snapshot`. All were chosen while building phase 5 and are
  easy to change.
- **Hosting.** Where the package repository and the ISOs live.
- **Translations of the base tools.** bash, coreutils, grep and 38 other recipes are built with `--disable-nls`,
  as on Melon, so they build no translations and have no `-lang` package: their messages are English only. Turning
  it on is a rebuild of those recipes. (Before phase 7.)
- **The man-page reader.** mandoc for now: one small package, which the `-doc` packages bring in. The usual
  alternative is man-db with groff, about 15 MB, which is what Arch and Debian use.
- **Documentation on the ISO.** The ISO's own package repository carries only a few extras, so `apk add docs` needs
  the online repository. All 38 `-doc` packages of the base and mandoc together are about 5 MB.
- **The system file (phase 10).** Its name and the command's, the format, how much it manages beyond packages and
  services, and whether changes made by hand in what it manages are undone or only reported. And when: it needs
  only phase 5, so it could be built before the desktop.
- **Website**, and the font for the wordmark (the current one is a stand-in).

## Decided

- **Rollback: both a command and the boot menu, and "however Nix does it"** (19 November 2026). Taken as NixOS's
  behaviour on Btrfs snapshots: the boot menu lists every snapshot and starts the one chosen, without changing what
  starts by default; `valence-snapshot rollback` makes a snapshot the default; what a rollback replaces is kept.
  Not taken as Nix's store, a folder per package with the system assembled from them, which is nearer to phase 9.
- **Clean builds** (19 November 2026). Every package is built in a sysroot that holds only what its recipe
  declares, so a missing dependency fails the build instead of going unnoticed, and the same recipe gives the same
  package whatever was built before it.
- **Core tools: the GNU tools on the installed system** (18 November 2026). coreutils, findutils, grep, sed, tar,
  gzip, diffutils, less, procps, iproute2 and shadow take over the command names, the way gawk and bc already do.
  BusyBox stays installed for the initramfs and for the small daemons the services run (getty, DHCP, mdev, the
  loggers, cron, time sync).

## How the work is done

- Recipes and scripts live in this repository. Binaries do not.
- Building happens on a real machine, not in CI: a full build is hours of compiling.
- Work lands on `testing`. `main` is what ISOs and the package repository are built from, and it only moves
  when `testing` has been checked.

Built on the work in [Melon Linux](https://github.com/melon-77/melon-os).
