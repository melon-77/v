# Installing Valence

This guide takes you from nothing to an installed Valence. You don't need to know any other Linux distribution.

Valence has been installed many times in virtual machines (QEMU), by its automated tests and by following this
guide, but **not yet on a real computer**. Try it on a machine whose disk you can afford to erase, or next to
Windows after a backup.

## 1. What you need

- A 64-bit PC with an Intel or AMD processor (x86_64). Valence starts on both kinds of firmware: UEFI, which almost
  every computer from the last ten years has, and the older BIOS. To see which yours uses, in Windows: press
  Windows+R, type `msinfo32`, and look at "BIOS Mode" (UEFI, or Legacy for BIOS).
- **Memory:** the graphical installer refuses less than 1.5 GB. The tests run the console system with 3 GB and the
  desktop with 4 GB; give the desktop 4 GB or more.
- **Disk:** at least 16 GB for the desktop. The installer erases the disk you choose, or, on UEFI computers, uses
  free space next to Windows (section 6).
- **A USB stick** of 1 GB or more. Writing the ISO to it erases what is on it.
- An internet connection is **not** needed to install: everything comes from the USB stick. You need one later, for
  updates and for apps such as Firefox and Steam.

## 2. Choose an ISO

Both are on the repository's [Releases page](https://github.com/melon-77/v/releases). Take the newest release.

| File | What it is |
|---|---|
| `valence-desktop-<version>-x86_64.iso` (805 MB; Windows shows 767 MB) | The KDE Plasma desktop. It starts a live desktop you can try without installing, and has the graphical installer. **Take this one if you are unsure.** |
| `valence-<version>-x86_64.iso` (408 MB; Windows shows 389 MB) | The console system, without a desktop: for servers, or if you want to build up your own. It has the console installer. |

Also download `SHA256SUMS`, `SHA256SUMS.sig` and `valence-signing.rsa.pub` from the same release.

## 3. Check the download

The signature shows that `SHA256SUMS` was made by Valence; the checksum shows that your ISO is the one it lists.

First make sure the key is Valence's: its fingerprint must be

```
c49278931b2d3c56dd4f469ac66941e67eeef97e081af3fa1a083b340b1e2849
```

The same key is in the repository as `keys/trusted/valence-signing.rsa.pub`. With `openssl` (on Linux and macOS it
is usually there; on Windows it comes with Git for Windows, in Git Bash), in the folder with the downloads:

```sh
openssl rsa -pubin -in valence-signing.rsa.pub -outform DER | sha256sum     # the fingerprint
openssl dgst -sha256 -verify valence-signing.rsa.pub -signature SHA256SUMS.sig SHA256SUMS
```

The first prints `writing RSA key` and then the fingerprint followed by `  -`; it must be the one above. The second
must print `Verified OK`. Then the ISO itself, on Linux (macOS: `shasum -a 256 -c` instead of `sha256sum -c`):

```sh
sha256sum -c --ignore-missing SHA256SUMS
```

It must say `OK` after the ISO's name. On Windows, run this in PowerShell and compare the result with the line for
your file in `SHA256SUMS` (letter case does not matter):

```powershell
Get-FileHash .\valence-desktop-0.1.0-x86_64.iso
```

## 4. Write the ISO to the USB stick

The ISO is written to the stick as it is, byte for byte. Copying the file onto a stick is not the same and will not
start.

- **Windows:** [Rufus](https://rufus.ie). Choose the stick and the ISO, leave the other fields as Rufus fills them
  in, and press Start. If Rufus asks how to write the image, choose **"Write in DD Image mode"**.
- **Linux:** find the stick's name with `lsblk` (for example `sdb`; be sure, the stick is overwritten). Unmount
  anything the desktop mounted from it, then write to the whole stick (`sdb`, not a partition like `sdb1`):
  ```sh
  sudo umount /dev/sdb?*
  sudo dd if=valence-desktop-0.1.0-x86_64.iso of=/dev/sdb bs=4M status=progress oflag=sync
  ```
- **macOS:** `diskutil list` shows the stick (for example `disk4`); `diskutil unmountDisk /dev/disk4`, then
  `sudo dd if=valence-desktop-0.1.0-x86_64.iso of=/dev/rdisk4 bs=4m`.

## 5. Start the computer from the stick

1. **On a UEFI computer, turn Secure Boot off** in the firmware settings (often Del, F2 or F10 right after switching
   on). Valence's boot loader is not signed with Microsoft's key, so with Secure Boot on the computer refuses to
   start it. A BIOS computer has no Secure Boot: skip this. Turning it off does not change Windows, but **Windows
   with BitLocker or Device Encryption may ask for its recovery key** at its next start after a firmware setting
   changed: have the key ready (it is in the Microsoft account the PC is linked to), or suspend BitLocker in
   Windows first.
2. Plug the stick in, switch the computer on and open its boot menu: usually F12, F11, F9 (HP), F8 or Esc right after
   switching on. Choose the USB stick (on UEFI computers it may be listed twice; take the "UEFI" one).
3. Valence's own menu appears and **starts "Valence live" by itself after 5 seconds**. To choose another entry,
   press an arrow key at once: that stops the countdown. The box cuts the names short; from the top they are
   - *Valence live*: the normal start;
   - *Valence live (copy to RAM)*: loads the whole system into memory, so the stick can be pulled out (needs more
     memory);
   - *Valence live (safe graphics)*: if the screen stays black or garbled;
   - *Valence live (serial console)*, below the others (press Down): for servers without a screen.

The desktop ISO starts KDE Plasma by itself after a minute or two, as the user `live` (no password). The console
ISO logs in as `root` by itself and shows what you can do. Nothing is written to your disk until you install.

## 6. Install next to Windows (optional)

Skip this if Valence gets the whole disk.

Valence can go into **free space** on a disk that already has Windows (UEFI computers only). Make the room in
Windows first:

1. Back up what matters.
2. In Windows, open **Disk Management** (right-click the Start button), right-click the `C:` partition, choose
   **Shrink Volume** and shrink it by at least 20 GB (20480 MB). The space becomes "Unallocated". Leave it so.
3. Start Valence from the stick and install; choose the "alongside" option (below).

Windows itself is not touched: Valence adds its own partitions in the free space and its own folder on the EFI
system partition. Afterwards Valence's boot menu offers Windows too, and shows for 5 seconds at every start.

## 7a. Install with the graphical installer (desktop ISO)

On the live desktop, double-click **Install Valence**. The installer's steps are listed on the left: Welcome,
Partitions, Users, Summary, Install, Finish. **Next** is at the bottom right.

1. **Welcome.** Next. (The language list there changes only the installer's own language; the installed system is
   in US English. If the computer has too little memory or disk, this page says so and you cannot go on.)
2. **Partitions.** Choose the disk under "Select storage device", then one of:
   - **Erase disk**: Valence takes the whole disk. Everything on it is lost.
   - **Install alongside**: next to Windows; a slider chooses how much Valence gets. (The console installer's way of
     doing this, section 7b, is the one the tests cover.)
   - **Replace a partition** and **Manual partitioning**: for people who know partitioning. Manual partitioning has
     not been tested.

   On an empty disk only Erase disk and Manual partitioning are offered; the other two appear when the disk
   already has a system on it. After you choose Erase disk, a box **Encrypt system** appears at the bottom: tick
   it if the disk should be locked with a passphrase, and type the passphrase twice (you type it at every start;
   if you lose it, the data cannot be recovered). Leave "Boot loader location" as it is. Next.
3. **Users.** Your name; the login name and the computer's name are filled in from it (change them if you like);
   and a password, twice. While "Use the same password for the administrator account" stays ticked, as it is at
   first, the administrator (root) gets the same password. "Log in automatically" and "Require strong passwords"
   are up to you. Next.
4. **Summary.** It shows what will happen to the disk: a 1 GiB FAT32 partition for `/boot` and Btrfs for the rest.
   Press **Install**, and confirm with **Install Now**.

Installing takes 5 to 15 minutes (about twice as long with encryption). The progress bar can stand still for
several minutes while the system is copied. **The screen may go dark while it installs**: that is the live
system saving power, not the end and not a crash. Move the mouse or click to bring it back (a key press may not),
and do not pull the stick out.

When it says **All done**: untick **Restart now**, press **Done**, shut the computer down (the launcher at the
bottom left, then Shut Down), pull the USB stick out, and switch the computer on. (With Restart now ticked it
restarts at once, and the stick starts the live system again unless it is pulled out in those few seconds; if that
happens, shut down from the live desktop, pull the stick, and switch on.)

Valence is set up in US English and on UTC time. The keyboard layout and the time zone are changed afterwards in
Plasma's **System Settings** (Keyboard; Date & Time).

## 7b. Install with the console installer (both ISOs)

On the console ISO you are already logged in as root. On the desktop ISO open **Konsole** from the launcher. Type

```sh
valence-install
```

It lists the disks and asks, in this order (Enter takes the answer in brackets):

1. **Disk to install to**: the name from the list, for example `sda` or `nvme0n1`.
2. On a UEFI computer with Windows and at least 20 GB free space on that disk: **alongside** (into the free space)
   or **erase** (the whole disk).
3. **What to install**: `desktop` (KDE Plasma, the same system as the desktop ISO; only offered on the desktop ISO)
   or `base` (the console system). Enter takes the ISO's own.
4. **Hostname**: the computer's name.
5. **Root password**, twice: the administrator's password.
6. **Your user name** and its password, twice.
7. **Wi-Fi network name and password**, if the computer has Wi-Fi (Enter skips it).
8. **Encrypt the system with a passphrase (LUKS)**: `y` or `n`; with `y`, the passphrase twice.
9. It shows what it will do. Type `YES` (in capitals) to start.

When it says **Valence is installed**, type `poweroff`, pull the USB stick out, and switch the computer on.

## 8. The first start

- On a computer with only Valence the boot menu is hidden and the screen stays black for a few seconds (Esc or
  Shift while it starts shows the menu). Next to Windows the menu shows for 5 seconds.
- With encryption, Valence asks for the passphrase first.
- The desktop's login screen shows your name: type your password. The first login can take a minute before the
  desktop appears.
- The first time, a terminal window **"Welcome to Valence"** asks questions: whether to install Firefox, VLC, Steam
  and Prism Launcher from Flathub (`[Y/n]`: Enter means yes), the developer tools (a C/C++ compiler and make;
  `[y/N]`: Enter means no) and, on a computer with an NVIDIA graphics card, a driver for it. All of them need the
  internet: without a connection, answer `n`. `valence-first-boot --again` in a terminal asks again later.
- **Network:** a cable works by itself. On the desktop, Wi-Fi is in the panel's network icon. On the console
  system, `valence-wifi` lists the networks and `valence-wifi "Name" "password"` connects.
- **Administrator commands** start with `sudo` and ask for your password (Valence's `sudo` is a small wrapper around
  `doas`). Your user can use it; the installers put it into the `wheel` group.

Updating, installing software, and what to do when something goes wrong: [updating.md](updating.md).

## If something goes wrong while installing

- **The computer does not start from the stick:** Secure Boot is still on, or the stick was written by copying the
  file instead of the image (section 4), or the firmware's boot menu needs a different key.
- **The screen stays black:** start again and choose *safe graphics* (section 5).
- **The installer stops with a message:** the message says why. Nothing is installed when it stops before
  partitioning. Please report it, with the message, at the repository's
  [Issues](https://github.com/melon-77/v/issues) page; `valence-hwreport` writes a description of the computer that
  helps.
