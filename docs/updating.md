# Updating Valence, and when something goes wrong

Valence is a rolling distribution: there are no versions to move between, the system is simply kept up to date.
Before every change to the installed packages it takes a snapshot of itself, so an update that breaks something can
be taken back, even when the computer no longer starts.

## Updating

In a terminal:

```sh
sudo apk update      # fetch the list of what is new
sudo apk upgrade     # install it
```

apk says what it will change, takes a snapshot ("before apk upgrade"), and installs. When the kernel was updated,
restart. Apps from Flathub (Firefox, Steam, ...) are updated separately, with Discover or with `flatpak update`.

The packages come from Valence's package repository, the `packages` branch of the GitHub repository. **While that
repository is private, an installed system cannot reach it** and `apk update` reports an error: it becomes public
once the work on the roadmap is done. Until then everything on the ISO works offline, but nothing can be updated
or added except from a copy of the repository.

## Installing and removing software

```sh
apk search firefox           # find a package by name
sudo apk add htop            # install one (and what it needs)
sudo apk del htop            # remove it again
apk info -L htop             # the files a package installed
```

Bigger desktop apps come from Flathub instead: Discover (in the launcher) or `flatpak install flathub <app>`.

Documentation is not installed by default. `sudo apk add docs` brings the manual pages of everything installed, and
`man <command>` shows them; `sudo apk add htop-doc` brings one package's. `sudo apk del docs` removes them again.

## Snapshots

```sh
sudo valence-snapshot              # the snapshots: number, when, and before what
sudo valence-snapshot create note  # take one by hand (it stays until you delete it)
sudo valence-snapshot delete 12    # remove snapshot 12
```

apk keeps the ten newest of its own snapshots (`KEEP=` in `/etc/valence/snapshot.conf` changes the number,
`AUTO=no` there switches them off). A snapshot holds the system: programs, settings in `/etc`, the package database
and the kernel. It does **not** hold `/home` or `/var/log`, so going back never touches your own files, and the logs
of what went wrong are still there. Snapshots take space only for what changed after them; delete old ones when the
disk gets full.

## Taking an update back

**While the system still starts:**

```sh
sudo valence-snapshot rollback     # back to before the last package change ("rollback 12": to snapshot 12)
sudo reboot                        # it takes effect at the next start
```

**When it no longer starts:** switch the computer on and press **Esc** (or hold Shift) while it starts, to open
Valence's boot menu (next to Windows it shows by itself). Choose **Valence snapshots**, then the snapshot from
before the update. The system starts as it was then, with the kernel it had then, so this also helps after a kernel
update that does not start. Starting a snapshot changes nothing by itself: at the next restart the broken system is
the default again. To keep the one you started:

```sh
sudo valence-snapshot rollback     # this system now starts by default; no restart needed
```

Nothing is lost by going back: the system a rollback replaces is kept as a snapshot too ("before rolling back to
..."), and rolling back to that one undoes the rollback. Between starting a snapshot and keeping it, and between a
rollback and the restart, apk refuses to change packages, because the change would land in a system that is about
to be put aside.

## Other problems at start

- **Black or garbled screen:** open the boot menu (Esc) and choose the entry ending in **safe graphics**.
- **A wrong passphrase prompt loop or a forgotten passphrase** on an encrypted disk: the passphrase cannot be
  recovered. Only reinstalling helps, and the data on the disk is lost.
- **A forgotten password:** repair from the USB stick (below) and set a new one with `passwd <user>`.

## Repairing from the USB stick

When nothing in the boot menu helps, start the computer from the Valence USB stick (the install guide, section 5)
and open a terminal (Konsole on the desktop ISO). Become root and find the disk:

```sh
sudo -s
lsblk -f        # Valence's disk has a vfat partition of 1 GB (/boot) and a btrfs one (the system)
```

Say the disk is `sda`, with `/boot` on `sda2` and the system on `sda3` (on NVMe disks the names look like
`nvme0n1p2`). Mount the installed system, its `/boot` and the parts of the live system it needs:

```sh
cryptsetup open /dev/sda3 valenceroot     # only if the disk is encrypted; then use /dev/mapper/valenceroot below
mount -o subvol=@ /dev/sda3 /mnt
mount /dev/sda2 /mnt/boot
mount -o subvol=@home /dev/sda3 /mnt/home
for d in dev proc sys; do mount --rbind /$d /mnt/$d; done
chroot /mnt /bin/bash
```

You are now root in the installed system. Set a password (`passwd jcole`), look at the logs in `/var/log`, or
remove a package that breaks the start (`apk --no-commit-hooks del <package>`; there is no snapshot to take here).
`exit` leaves it, then `reboot`.

To pick a snapshot without starting it, mount the snapshots instead: `mount -o subvol=@snapshots /dev/sda3 /mnt`
shows them as numbered folders, each with the system in `root/` and a description in `info`; files can be copied
from there.

## Reinstalling

A new install over the old one erases the disk, `/home` included. Copy your files elsewhere first, or from the
repair session above (`/mnt/home`).
