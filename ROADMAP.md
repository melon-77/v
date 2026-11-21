# Valence: plan and roadmap

Valence is a from-scratch, rolling x86_64 Linux distribution. It is the serious successor to
[Melon Linux](https://github.com/melon-77/melon-os): the same engineering, without the jokes, and with a handful
of fixes where Melon chose small over sensible.

Status, 21 November 2026: phases 1 to 8 are done, and 0.1.0 is the first release. The system is on glibc 2.44 and its commands are the
GNU tools. Both ISOs build: the console ISO (58 recipes on it) and the Plasma desktop ISO (345 recipes with the
console's: LLVM, Mesa, Qt 6, KDE Frameworks and Plasma 6, SDDM, PipeWire, Flatpak, Calamares). The console ISO passes
its eight tests in QEMU (install and boot on BIOS, boot on UEFI, an encrypted install and its boot, dual boot next
to Windows on UEFI, and two rollback tests), and the installed system adds a package from a repository over the
network. The desktop ISO passes its three: the live desktop (services, Plasma, a USB stick, video encoders, printing,
Wi-Fi), and an install on BIOS and one on UEFI that each log in to Plasma through SDDM. The graphical installer was
run by hand on BIOS, on UEFI and with an encrypted disk. BusyBox stays for `/bin/sh`, the initramfs and the small
daemons. Documentation and translations are packages of their own: the builder makes `<pkg>-doc` and `<pkg>-lang`
from what a recipe installs, no recipe deletes them any more, and a fresh install has no documentation files until
`apk add docs` (or one `-doc` package) brings them, with `man`. Phase 4's builder builds everything, 451 recipes,
from an empty repository with one command: in an order worked out from the recipes, each package in a sysroot that
holds only what its recipe declares, and a second run builds nothing. The 106 recipes outside the two ISOs (the
developer tools, the NVIDIA packages, an LXQt desktop) build on glibc with the rest; they were tried in a root made
of the packages, not on an installed system. Both installers make a Btrfs root in
subvolumes, and apk takes a snapshot before every change to the installed packages. Going back works the way
NixOS's generations do: the boot menu lists every snapshot and starts the one chosen, and `valence-snapshot rollback`
makes a snapshot the system that starts by default. The tests break an update on purpose both ways: one program,
undone with the command; and init together with the kernel, after which the disk does not boot until the boot menu
starts the snapshot from before it. On a plain disk on BIOS and on an encrypted one on UEFI. The desktop's look is
Valence's own: a plain black wallpaper and the logo's greys. Phases 7 to 10 have not started.

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
| Packages | Signed apk v3 packages, built from source recipes. apk-tools 3 installs them until Valence's own package manager does (phase 10) |
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
A new name, a logo (a folded V in greys; the files are in `branding/`), and real
documentation: how to install, how to update, how to recover, and how to contribute.

**7. Its own package manager, and a folder for every program.**
On the installed system apk gives way to a package manager written for Valence: fast, with a proper screen to
watch, and built around one idea from GoboLinux, that every program lives in a folder of its own. The builder,
the recipes and the signed packages stay as they are. Phase 10 has the design.

**8. A file you can throw in to get the system you want.**
A proposal (19 November 2026). Instead of installing and setting up everything by hand, you write what you want in
one short file (`packages = steam, firefox, nano`, the groups a user is in, the services that run), and the
installer, or one command on a running system, does it. It is a convenience and not a specification of the system:
it adds what it lists and leaves the rest alone. Phase 8 has the design.

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

- [x] **4. `valence-build`.**
  The dependency-ordered builder replaces the queue, retry and resume scripts.
  *Done when:* one command rebuilds the whole repository from empty with no order written down anywhere, and a
  second run with nothing changed builds nothing.
  *Done (20 November 2026):* with the repository, the sysroot and the cross toolchain moved away,
  `scripts/toolchain.sh` and then one `scripts/valence-build` built all 451 recipes, 1102 packages, in 5 hours 3
  minutes without a failure; the same command again built nothing, in 2 seconds. Both ISOs made of those packages
  pass their tests (eight and three). It was the second such run. The first stopped being one after 72 minutes:
  gpgmepp was built before glib, which gpgme's `-dev` package turned out to need, something no build with a full
  repository could show. The builder now refuses a package like that when it is made (AGENTS.md, rule 70).
  Before those runs the 106 recipes outside the two ISOs were made to build on glibc and to declare what they need;
  AGENTS.md, "State of the glibc switch", says how far they were tested. The queue, retry, resume and build-all
  scripts and the ordered lists in `build-everything.sh` are gone.

- [x] **5. Btrfs and snapshots.**
  The installer creates a Btrfs root with subvolumes. Updating takes a snapshot first. Rolling back is
  documented and tested.
  *Done when:* a QEMU test breaks an update on purpose, rolls back, and boots.

- [x] **6. The desktop.**
  Rebuild the Plasma 6 stack on glibc (Mesa, Qt, KDE Frameworks, Plasma, SDDM, PipeWire, the apps Melon
  ships). Plain Calamares branding, and Calamares makes the same Btrfs layout as the console installer. A normal
  first boot. The wallpaper, the splash, login and boot menu pictures and the colour scheme are Valence's own (plain
  black, and the logo's greys), in place of Melon's planet and its green and orange.
  *Done when:* the desktop ISO installs in QEMU on BIOS and UEFI and logs in to Plasma through SDDM.
  *Done (20 November 2026):* it does, with the console installer in the automated test and with Calamares by hand
  (BIOS, UEFI, and an encrypted disk). The first encrypted Calamares run left a disk that did not start, a fault in
  the settings inherited from Melon; AGENTS.md, "Installers", has it.

- [x] **7. Documentation and the first test release.**
  README, install guide, update and recovery guide, contributing rules (`testing` before `main`, as in Melon).
  Signed ISOs with checksums.
  *Done when:* someone who has never seen Melon can install Valence from the guide alone.
  *Done (21 November 2026):* `docs/install.md`, `docs/updating.md`, `CONTRIBUTING.md`, and a README rewritten as
  the front page; `scripts/release.sh` signs `SHA256SUMS` with the packages' key, and release 0.1.0 carries both
  ISOs, the checksums, the signature and the key. Someone who had never seen Melon or Valence (a fresh Claude agent
  given only the guide) installed the desktop ISO in a virtual machine with the graphical installer, following the
  guide alone, and the installed system logged in to Plasma. What it found unclear went into the guide; the two
  things it found in the ISOs (the live session turned the screen off in the middle of the install, and the boot
  menu cut its entries' names short) are fixed in the test builds after 0.1.0. Not done: an install by a person on
  a real computer.

- [x] **8. A file you can throw in to get the system you want.**
  A proposal, put here on 19 November 2026 as the last phase and moved ahead of the ARM edition the day after
  ("wishlist should be before arm edition"). The design below is a starting point. What is wanted, in the owner's
  words: "a file you can throw in to just get what you want", instead of installing everything by hand, "rather
  than a reproducible system". A sketch of one:

      packages = steam, firefox, nano
      services = sshd
      hostname = valence
      user.jcole.groups = wheel, audio, video

  It is a list of wishes. Applying it does what you would otherwise do by hand, a line at a time: the packages are
  installed, the services enabled, the user put into the groups, the settings set. What is already so is left as
  it is, so applying a file a second time changes nothing. What the file does not mention is left alone too:
  nothing is removed because a file leaves it out. That is the difference from NixOS's `configuration.nix`, which
  is the whole truth about a system and builds it the same way every time; it was considered and is not what is
  wanted here.
  Where a file goes in: the installers take one (a file beside the ISO's own on the stick, or one named when the
  installer starts), so a new system comes up with everything on the list; and one command applies a file to a
  running system, at once, with no restart. A snapshot is taken first (phase 5), so one rollback takes back a file
  that turned out wrong.
  A name under `packages` is looked for in the package repository first, and then among the apps Valence gets
  from Flathub, which is where Steam and Firefox come from today. Packages are installed through apk until
  Valence's own package manager takes over (phase 10).
  In stages: packages, services, groups and the host name on a running system; then the installers; then whatever
  else people set by hand after every install and would rather write down once.
  *Done when:* a QEMU test installs a system with a file thrown in and finds its packages, services and groups
  there; applies a second file on the running system and has everything on it, with nothing gone that was there
  before; applies it again and nothing changes; and one rollback takes the second file's changes back.
  *Done (21 November 2026):* `valence-wish` (AGENTS.md, "Wish lists"), taken by both installers from a
  `valence-wishlist.txt` on a USB stick or another drive. `qemu-test.py wish` passes: the console installer finds a
  list on a USB stick and applies it, and the installed system has its package, service, group and host name; a
  second list applied on the running system adds its own and keeps the first one's, after one snapshot; applied
  again it changes nothing; a list naming a package that does not exist changes nothing; one rollback and a restart
  take the second list back and keep the first. Not in the test: Flathub apps in a list (they need the internet),
  and Calamares' use of a list (Calamares has no automated test).

- [ ] **9. A 64-bit ARM edition for Raspberry Pi.**
  A second architecture, `aarch64-valence-linux-gnu`, next to x86_64 in the same tree (the way Melon carried its
  32-bit edition): cross toolchain, the base system, a Pi kernel and firmware, and a disk image to write to an SD
  card instead of an ISO. It needs the build to stop running freshly built programs directly on the build machine,
  which phase 2 has to solve for glibc anyway, so it starts after phase 2. Console system first; the desktop follows
  phase 6. Assumed: the 64-bit boards (Pi 3, 4, 5, Zero 2 W), not the 32-bit ones.
  *Done when:* the image boots on a Raspberry Pi 4 to a login prompt, gets a network address, and can `apk add` a
  package; and the same image boots in QEMU as an automatic test.

- [ ] **10. Valence's own package manager, with a folder for every program.**
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
- **PAM in the base system.** The desktop brings linux-pam (SDDM and the lock screen need it); the base has none,
  so a console login and `su` do not go through PAM. Move it into the base and rebuild shadow on it, or leave the
  console system without?
- **Sources of the desktop's recipes.** About 150 of them (Qt, KDE, X.Org, Mesa) still build from Ubuntu's `orig`
  tarballs, as on Melon. The rule is upstream's own releases, verified; moving them is a piece of work of its own.
- **The man-page reader.** mandoc for now: one small package, which the `-doc` packages bring in. The usual
  alternative is man-db with groff, about 15 MB, which is what Arch and Debian use.
- **Documentation on the ISO.** The ISO's own package repository carries only a few extras, so `apk add docs` needs
  the online repository. All 38 `-doc` packages of the base and mandoc together are about 5 MB.
- **The wish list's details (phase 8).** Chosen while building it, mine and easy to change: the command
  `valence-wish`, the file `valence-wishlist.txt` when an installer is to find it, `key = a, b` lines with `#`
  comments, the keys `packages`, `services`, `hostname` and `user.NAME.groups`, and a Flathub app found by its full
  ID, a table of common names, or Flathub's search when exactly one app has that name. Not covered yet: what
  people set by hand after every install beyond those (time zone, keyboard layout, language, Wi-Fi networks, more
  users).
- **Website**, and the font for the wordmark (the current one is a stand-in).

## Decided
- **The first release is 0.1.0, signed with the everyday key only** (20 November 2026). Tags are `v<version>`; the
  owner chose 0.1.0 over a date (`v2026.11.21`) and 1.0.0. AGENTS.md wanted a backup signing key before the first
  release; the owner chose to release without one, so until one exists a lost everyday key cannot be rotated the
  way AGENTS.md describes.
- **English only** (20 November 2026): "we only need English". The base tools stay built with `--disable-nls` (bash,
  coreutils, grep and 38 other recipes), so their messages are English and they have no `-lang` package. The
  `-lang` packages the other recipes make stay as they are: optional, and installed by nothing by default.
- **Hosting is the repository itself** (20 November 2026): "hosting will be done on the repo itself because it'll be
  made public soon after everything is done". The package repository is the `packages` branch of
  github.com/melon-77/v (`scripts/publish-repo.sh`, AGENTS.md "Package repository"), which installed systems already
  point at, and the ISOs are that repository's release assets. Both reach everyone once the repository is public,
  after the phases here are done; until then they need a GitHub account with access. The other hosts in
  `docs/iso-hosting.md` are not used.

- **The wish-list file comes before the ARM edition** (20 November 2026): "wishlist should be before arm edition".
  It is phase 8 now; the ARM edition is phase 9 and Valence's own package manager phase 10.
- **The wallpaper is plain black** (20 November 2026): "make it a pure black wallpaper actually". The login, lock
  and loading screens and the boot menu follow it. Before that the owner had worked a drawing out over several
  previews, a lattice gate woven from the logo's V ("imagine it as like a fence gate where the next row is flipped
  compared to the other row", "too dense reduce the amount of v's by like half", "normal gate no logo"); the first
  wallpaper on the desktop ISO, small V's set into the gaps of large ones, was not what was meant. The gate is kept
  in `art/desktop.py` and one command exports it instead of black.
- **The logo is a folded V in greys, and the desktop takes its colours from it** (19 and 20 November 2026). It
  replaced the bismuth crystal. The colour scheme is the logo's greys.
- **The system file is a wish list, not a specification** (19 November 2026). "A file you can throw in to just
  get what you want", rather than a reproducible system: it adds what it lists and removes nothing. (Phase 8.)
- **Rollback: both a command and the boot menu, and "however Nix does it"** (19 November 2026). Taken as NixOS's
  behaviour on Btrfs snapshots: the boot menu lists every snapshot and starts the one chosen, without changing what
  starts by default; `valence-snapshot rollback` makes a snapshot the default; what a rollback replaces is kept.
  Not taken as Nix's store, a folder per package with the system assembled from them, which is nearer to phase 10.
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
