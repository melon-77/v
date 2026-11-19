# Building Valence on your own machine

Everything Valence ships is built by the scripts in this repo, so any x86_64 machine with Ubuntu 24.04
can build it. On an 8-core/16-thread CPU a full build (toolchain, base system, Plasma desktop, ISOs)
takes roughly 8–12 hours; on 2 cores it takes days.

## Windows + WSL2 build machine

Set up and used for the round-2 ISOs on a Windows 10 IoT Enterprise LTSC 2021 laptop (build 19044, Ryzen 7 5800U, 16 threads,
32 GB RAM, no Microsoft Store). Windows 11 is the same without the workarounds marked *(Windows 10)*. WSL2 needs Windows 10
build 19041 or newer.

1. Turn WSL2 on. This needs an **administrator** PowerShell and a **reboot**:

       wsl --install --no-distribution

   *(Windows 10 without the Store)* if that isn't available, `dism /online /enable-feature /featurename:Microsoft-Windows-Subsystem-Linux /all /norestart`
   and the same for `VirtualMachinePlatform`. After the reboot, still as administrator, get the current WSL (the inbox
   `wsl.exe` is old and only lists "Ubuntu"): `wsl --update --web-download`. The new program is
   `C:\Program Files\WSL\wsl.exe`; use that path in a shell that started before the update.

2. Install Ubuntu 24.04 (no Store needed with `--web-download`; `--no-launch` skips the first-run user prompt, the build runs as root anyway):

       wsl --install -d Ubuntu-24.04 --web-download --no-launch

3. Give WSL enough memory and never let it idle out. Qt and KWin need about 1 GB per compile job, and WSL only gets half the
   RAM by default. Create `%UserProfile%\.wslconfig`:

       [wsl2]
       memory=24GB        # 32 GB machine: leave Windows ~8 GB (use 12GB on 16 GB)
       swap=16GB
       processors=16
       vmIdleTimeout=-1

   then `wsl --shutdown`. Use `JOBS=12` for builds on 16 threads; with 16 GB of RAM or less, `JOBS=10`.

4. Inside Ubuntu, `/etc/wsl.conf` (the `[boot]` part is already there). **`appendWindowsPath=false` is required**: WSL appends the
   Windows `PATH`, which has entries with spaces (`/mnt/c/Program Files/...`), and a build step that expands `$PATH`
   unquoted breaks on them (`host-rust.sh` did: `env: 'Files/GnuPG/bin:...': No such file`, exit 127; fixed, other scripts may follow):

       [boot]
       systemd=true
       [interop]
       appendWindowsPath=false

   Also keep scripts that you write on Windows with LF line endings (Windows tools write CRLF, and `bash` then fails on the last line).

5. Build inside the Linux file system (`~/valence`), not under `/mnt/c`: it is many times faster, and keep the helper scripts there too. Start long builds detached (`setsid nohup script >log 2>&1 < /dev/null &`) and keep one
   `wsl.exe -d Ubuntu-24.04 -- sleep infinity` open, so WSL keeps its VM while no terminal is attached.

6. **Do not let Windows sleep.** Modern/connected standby freezes the WSL2 VM: after it wakes, every new `wsl` command hangs
   (even `wsl --shutdown` and `wsl --exec /bin/true`) and the builds inside do not progress, although `wsl -l -v` says
   "Running". This happened twice on AC power. Keep the machine plugged in, don't close the lid, set sleep to "never"
   (Settings → System → Power), or run something that holds a "system required" request while it builds. To recover
   without rebooting, in an administrator PowerShell: kill `wsl`, `wslhost`, `wslrelay`, `wslservice`, `vmwp` and `vmmem`
   with `Stop-Process -Force`, then `Start-Service WslService`. The Linux disk is intact; builds are resumable (below).

7. **There is no KVM on Windows 10, or on a CPU Windows can't nest on.** `/dev/kvm` may exist and QEMU still says
   "failed to initialize kvm: No such device" (`wsl` itself prints "Nested virtualization is not supported on this machine").
   `qemu-test.py` then uses software emulation (AGENTS.md, "Testing"): the console install and boot tests pass in about a minute,
   the Plasma desktop tests were not usable on that laptop.

8. A USB stick (or any drive Windows doesn't auto-mount in WSL) needs a mount: `D: /mnt/d drvfs defaults,nofail 0 0` in `/etc/fstab`.
   The signing key can stay on it and be used in place (`VALENCE_SIGN_KEY`, "The signing key" below). Keep the offline **backup** key
   (AGENTS.md, "Signing keys and rotation") off any drive that is plugged into the build machine.

9. **Two sessions on one machine: one checkout each.** A checkout's `tools/` and `sysroot/` are tied to its path, and a
   second session switching branches under a running build breaks both. Give each its own worktree:

       git -C ~/valence worktree add ~/valence-r2 <branch>
       cd ~/valence-r2
       for e in ~/valence/sources/*; do [ "${e##*/}" = MANIFEST.tsv ] || ln -sfn "$e" sources/; done   # shared downloads
       ln -sfn ~/valence/hosttools hosttools                                                            # shared host tools
       git --git-dir=$HOME/valence/.git archive origin/packages x86_64 | tar -x -C repo                 # its own copy of the online repo
       /root/valence-r2/scripts/toolchain.sh      # its own cross toolchain; absolute path

   Then `scripts/valence-build <recipe>` the recipes you changed. It builds what they need first, if the copied
   repository lacks it or lacks its record (copy `repo/<arch>/.state` too), and sets the sysroot up for each build
   from the recipe's own `makedepends` (clean builds, AGENTS.md "Recipe format"); a `-dev` package's pkg-config
   requirements are apk dependencies now, so they come along. That works for the base. A KDE recipe (`breeze`) still
   has no `makedepends` at all (Melon's full build relied on every earlier package being in the one sysroot), so it
   cannot be built this way until the generator gives it some (AGENTS.md, "State of the glibc switch"). The packages in the online repository were built on a
   machine whose checkout was `/home/hi/valence`, and some of their CMake files name that path (`KF6::WindowSystem` includes
   `/home/hi/valence/sysroot/usr/include`: "includes non-existent path"). Until you have rebuilt those packages yourself, let the
   path resolve: `mkdir -p /home/hi/valence && ln -s ~/valence-r2/sysroot /home/hi/valence/sysroot && ln -s ~/valence/hosttools /home/hi/valence/hosttools`.

   A worktree that only rebuilds packages with nothing to compile (`valence-base`, `valence-desktop`) and installs the rest from
   `repo/` can skip the toolchain (the 0.4 ISOs were made that way, with the round-2 packages copied in from the worktree that
   built them). Then nothing creates the sysroot's merged `/usr`: make `sysroot/usr/lib` and `sysroot/usr/bin` and the `lib`, `bin`
   and `sbin` links to them **before** `apk --initdb`, or apk makes `lib/` a real directory for its database and the first install
   fails with `valence-layout-1.0-r0: failed to commit lib: Is a directory`. WSL also keeps Windows' time zone; `mkiso.sh` dates
   the ISOs in UTC so they aren't named after the day before.

Measured on that laptop (12 jobs): `host-setup.sh` about 35 minutes for apt and the 2 GB of sources, a few minutes for GRUB, Python and Rust,
then 50 minutes for the host Qt; cross toolchain 14 to 21 minutes; `linux-valence` 22 minutes; `grub` 3 minutes; small recipes seconds;
the console ISO 14 seconds and the desktop ISO about a minute (they install the published packages from `repo/`).

Valence's first build, on an HP Victus 15 (i5-12450H, 12 threads, 32 GB, the same Windows 10 edition, `processors=12` in
`.wslconfig`; 18 November 2026): the sources are 4.9 GB. With the cross toolchain and the host Qt compiling side by side
(12 jobs each) the toolchain took 18 minutes and the host Qt 31. The 39 recipes of the console ISO then took 32 minutes,
14 of them `linux-valence`; the console ISO 7 seconds; the six console tests (`live`, `disk`, `disk --uefi`, `live --luks`,
`disk --luks`, `dualboot`) five minutes together, without KVM.
On glibc (the same machine and day): the cross toolchain 10 minutes on its own, the 50 recipes of the console ISO
29 minutes, 13 of them `linux-valence`. The six tests took 12 minutes that evening, but the machine was slower by
then for reasons outside the build (the musl ISO's install step took 100 s where it had taken 27 s): compare test
times only within one sitting.
The GNU tools (19 November 2026) added 14 recipes, about three minutes of building together. The console ISO is now
64 recipes and 405 MB, and the six tests took six minutes.
Splitting out the documentation (phase 3, the same day) rebuilt 41 of them in 11 minutes, glibc and GRUB among them,
and added mandoc, `docs` and `lang`. The ISO is still 405 MB. The six tests took 6 minutes with the new
documentation steps.
With the builder that orders builds itself (phase 4, the same day): from an empty repository and a sysroot as
`toolchain.sh` leaves it, `scripts/valence-build --edition console` built the 65 recipes the console ISO needs (62
are on it; the "64" above counted xz and mpg123, which the old list built and nothing on the ISO uses) in 26
minutes, each in a clean sysroot. Run again, it built nothing, in a few seconds. The six tests took 6
minutes.
With the Btrfs root and snapshots (phase 5, the same day): btrfs-progs is new, and xfsprogs, with the two libraries
only it used, is no longer built for the console ISO, which is now 48 recipes and 408 MB. The kernel was
rebuilt with Btrfs built in (13 minutes). There are eight console tests now: the two `rollback` ones start the VM
five times each, and the eight took 13 minutes.

Every time in the paragraphs before "On glibc" is from a musl build.

Sources that fail, and what `fetch-sources.sh` does about it: `ftp.gnu.org` often does not answer (on one run it served nine
files and then stalled on the tenth for good), and pinned Ubuntu `.deb`s leave the archive pool when Ubuntu updates the package
(three had by November 2026: `ovmf-generic`, `ca-certificates`, `linux-firmware-amd-graphics`). The script gives up on a stalled
download after a minute and falls back by itself: `https://mirrors.kernel.org/gnu/<project>/<file>` for GNU's files, Launchpad's
file store (`https://launchpad.net/ubuntu/+archive/primary/+files/<name>`) for `.deb`s. Every file is still checked against its
sha256 in `sources/MANIFEST.tsv`, whichever server it came from.
`apt` prints "Job for systemd-binfmt.service failed" in WSL: harmless.

## Steps (Ubuntu 24.04 or WSL2)

    git clone https://github.com/melon-77/v ~/valence
    cd ~/valence
    sudo scripts/host-setup.sh        # build tools, sources (~2 GB), host tools; about an hour
    # put the signing key in place (below)
    sudo JOBS=16 scripts/build-everything.sh

`build-everything.sh` is resumable: after a reboot or a crash, run it again. It skips finished packages
and continues an interrupted one. Logs are in `logs/` (`logs/pkg-<name>.log` per package). The ISOs
land in `out/`. `scripts/publish-repo.sh` then puts the packages online for installed systems.

The build runs as root because packages are installed into `sysroot/` with their real owners.
Programs built for Valence start on the build machine through `scripts/valence-run` (AGENTS.md, rule 18): nothing is set
up in the build machine's own `/lib` for them, and `host-setup.sh` removes the `/lib/ld-musl-*.so.1` links Melon made.

## Debian or Devuan as the build machine

The Ubuntu steps above work on Debian 13 and Devuan 6 too (not yet tried on a real install). `host-setup.sh` takes
Ubuntu's path there: it installs the same packages with apt, adds Ubuntu's archive key (`ubuntu-keyring`) because
older Valence sources still come from Ubuntu's source archive, and lists any package name your archive doesn't have
instead of stopping (paste that warning line if you get one). Without `sudo`, run the commands as root (`su -`).

**Moving from another build machine (Valence, Ubuntu, WSL):** copy `keys/`, `sources/`, `repo/` and `sysroot/` into the
new checkout, but not `tools/`, `hosttools/` or `work/`: they hold programs built for the old machine and linked against
its C library. A `repo/` or `sysroot/` from before the switch to glibc holds musl builds: don't carry those over
(`valence-build` skips a recipe whose record in `repo/<arch>/.state/` matches the recipe as it is now: copy `repo/` with that hidden directory, or everything is built again). `host-setup.sh` rebuilds the host
tools and `build-everything.sh` the cross toolchain before it carries on with the packages; finished packages in
`repo/` are not built again.

## Valence as the build machine

**Untested on glibc.** This section, rule 53 in AGENTS.md and `valence_deps` in `host-setup.sh` were written when Valence
was a musl system (Melon built itself this way). Nothing here has been run since the switch to glibc, and the build
tools it installs from Valence's own repository have not been rebuilt on glibc either.

On a Valence install (desktop or console profile), starting with no checkout:

1. `doas apk upgrade`, then `doas apk add git` (git is in the online repository, not on the ISOs).
2. `git clone -b testing https://github.com/melon-77/v ~/valence`, then `cd ~/valence`.
3. If you are moving from an old build machine, copy its `keys/`, `sources/`, `repo/` and `sysroot/` into `~/valence`
   (not `tools/` or `hosttools/`, see below). Otherwise put the signing key in `keys/` ("The signing key" below).
4. `doas scripts/host-setup.sh` (it lists any build tool Valence's repository doesn't have; if a step fails, it prints
   the end of `logs/host-setup.log`, where the full output of its builds goes)
5. `doas scripts/build-everything.sh` (it uses every core; for fewer, `doas env JOBS=10 scripts/build-everything.sh`:
   doas doesn't take `NAME=value` before the command the way sudo does)

What's different from an Ubuntu build machine:

- `host-setup.sh` installs the build tools from Valence's own package repository instead of Ubuntu's (it lists any name
  it can't find), adds a small `dpkg-deb -x` replacement for the recipes that unpack `.deb` files, uses Valence's
  own `apk` and `wayland-scanner`, and builds `rpcgen` into `/usr/local/bin` while the repository has no `rpcsvc-proto`
  (Python's Jinja2 and pyparsing likewise come from their source tarballs until their packages are published).
- A package that failed is unpacked fresh on the next run when its recipe changed since (after a `git pull` with a fix);
  otherwise `build-everything.sh` carries on where it stopped.
- **Moving from an Ubuntu or WSL build machine:** keep `keys/`, `sources/`, `repo/` and `sysroot/`, but delete
  `tools/` and `hosttools/` (except `hosttools/bin/apk`, which `host-setup.sh` replaces anyway): they hold programs
  built on the Ubuntu machine against its glibc (Valence had none when this was written). `host-setup.sh` rebuilds the
  host tools, and `build-everything.sh` rebuilds the cross toolchain (about an hour or two) before carrying on with
  the packages, skipping every package already in `repo/`.
- Programs from the sysroot start through `scripts/valence-run`, as on an Ubuntu build machine (AGENTS.md, rule 18):
  the system's own loader and libraries are never pointed at the sysroot.
- Keep the build machine's own packages up to date from the online repository, and don't upgrade it from packages it
  just built until they pass the QEMU tests: a broken gcc or glibc on the build machine stops it from fixing itself.
  Keep a Valence live USB around for that case.

## The 32-bit (i686) edition

The 32-bit edition has not been built on glibc, and whether Valence keeps it is undecided (ROADMAP.md). The steps below
are untested since the switch.

The same checkout builds the 32-bit edition next to the 64-bit one; its files go to `tools-x86/`, `sysroot-x86/`,
`work-x86/` and `repo/x86/`, so the two never mix:

    sudo VALENCE_ARCH=x86 JOBS=16 scripts/build-everything.sh

(on Valence: `doas env VALENCE_ARCH=x86 scripts/build-everything.sh`). It builds the i686 cross toolchain, the base
system, Qt and the LXQt desktop (not Plasma, Flatpak or the NVIDIA drivers), then `out/valence-*-i686.iso` and
`out/valence-desktop-*-i686.iso`. Logs carry `-x86` in their names (`logs/everything-x86-1.log`). Test with
`scripts/qemu-test.py ... --i686`: it runs `qemu-system-i386` with an Atom N270 CPU. The build machine runs i686
programs from the sysroot during the build, so its kernel needs 32-bit support (`CONFIG_IA32_EMULATION`, on in
Ubuntu, Debian, Devuan and Valence); they start through `scripts/valence-run` with the loader in `sysroot-x86`
(`ld-linux.so.2`; AGENTS.md, rule 18). Disk: the
finished 32-bit tree takes about 6 GB (toolchain, sysroot, packages, work), more while LLVM and Qt build; keep
20 GB free.

## The signing key

Packages and the repository index are signed with `keys/valence-signing.rsa`. It is deliberately not in
git. Copy it into `keys/` on the build machine and keep it private: whoever has it can publish packages
that every Valence system trusts. Losing it means installed systems need a new public key before they
accept new packages.

## Sources

`sources/MANIFEST.tsv` lists every source file with its sha256 and where it comes from: upstream release
tarballs and git tags, and for older recipes the Ubuntu source archive (being replaced by upstream as recipes are
updated; new sources never come from Ubuntu, see AGENTS.md "Sources"). `scripts/fetch-sources.sh` downloads them; after
adding a source, run `scripts/make-manifest.py` and commit the manifest.
