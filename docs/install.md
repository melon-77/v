# Installing Valence

This guide takes you from nothing to an installed Valence. You don't need to know any other Linux distribution.

Valence has been installed many times in virtual machines (QEMU) by its automated tests, but **not yet on a real
computer**. Try it on a machine whose disk you can afford to erase, or next to Windows after a backup.

## 1. What you need

- A 64-bit PC with an Intel or AMD processor (x86_64). Valence starts on both kinds of firmware, UEFI and the older
  BIOS.
- **Memory:** the graphical installer refuses less than 1.5 GB. The tests run the console system with 3 GB and the
  desktop with 4 GB; give the desktop 4 GB or more.
- **Disk:** at least 16 GB for the desktop. The installer erases the disk you choose, or uses free space next to
  Windows (section 6).
- **A USB stick** of 1 GB or more. Writing the ISO to it erases what is on it.
- An internet connection is **not** needed to install: everything comes from the USB stick. You need one later, for
  updates and for apps such as Firefox and Steam.

## 2. Choose an ISO

Both are on the repository's [Releases page](https://github.com/melon-77/v/releases). Take the newest release.

| File | What it is |
|---|---|
| `valence-desktop-<version>-x86_64.iso` (about 805 MB) | The KDE Plasma desktop. It starts a live desktop you can try without installing, and has the graphical installer. **Take this one if you are unsure.** |
| `valence-<version>-x86_64.iso` (about 410 MB) | The console system, without a desktop: for servers, or if you want to build up your own. It has the console installer. |

Also download `SHA256SUMS`, `SHA256SUMS.sig` and `valence-signing.rsa.pub` from the same release.

## 3. Check the download

The checksum shows the file arrived intact; the signature shows that `SHA256SUMS` was made by Valence.

On Linux (or macOS with `shasum -a 256 -c` instead of `sha256sum -c`), with the files in one folder:

```sh
sha256sum -c --ignore-missing SHA256SUMS
```

It must say `OK` after the ISO's name. On Windows, in PowerShell, and compare the result with the line for your
file in `SHA256SUMS` (letter case does not matter):

```powershell
Get-FileHash .\valence-desktop-0.1.0-x86_64.iso
```

To check the signature you need `openssl` (on Linux and macOS it is usually there; on Windows it comes with Git for
Windows, in Git Bash):

```sh
openssl dgst -sha256 -verify valence-signing.rsa.pub -signature SHA256SUMS.sig SHA256SUMS
```

It must print `Verified OK`. The key is the one every Valence package is signed with. To be sure you have the real
one, its fingerprint must be

```
c49278931b2d3c56dd4f469ac66941e67eeef97e081af3fa1a083b340b1e2849
```

which `openssl rsa -pubin -in valence-signing.rsa.pub -outform DER | sha256sum` prints. The same key is in the
repository as `keys/trusted/valence-signing.rsa.pub`.

## 4. Write the ISO to the USB stick

The ISO is written to the stick as it is, byte for byte. Copying the file onto a stick is not the same and will not
start.

- **Windows:** [Rufus](https://rufus.ie). Choose the stick and the ISO and press Start. When Rufus asks how to write
  the image, choose **"Write in DD Image mode"**.
- **Linux:** find the stick's name with `lsblk` (for example `sdb`; be sure, the stick is overwritten), then
  ```sh
  sudo dd if=valence-desktop-0.1.0-x86_64.iso of=/dev/sdb bs=4M status=progress oflag=sync
  ```
- **macOS:** `diskutil list` shows the stick (for example `disk4`); `diskutil unmountDisk /dev/disk4`, then
  `sudo dd if=valence-desktop-0.1.0-x86_64.iso of=/dev/rdisk4 bs=4m`.

## 5. Start the computer from the stick

1. **Turn Secure Boot off** in the computer's firmware settings (often Del, F2 or F10 right after switching on).
   Valence's boot loader is not signed with Microsoft's key, so with Secure Boot on the computer refuses to start
   it. (Turning it off does not harm an installed Windows.)
2. Plug the stick in, switch the computer on and open its boot menu: usually F12, F11, F9 (HP), F8 or Esc right after
   switching on. Choose the USB stick (on UEFI machines it may be listed twice; take the "UEFI" one).
3. Valence's own menu appears. Press Enter on **Valence live**. The other entries:
   - *copy to RAM*: loads the whole system into memory, so the stick can be pulled out (needs more memory);
   - *safe graphics*: if the screen stays black or garbled;
   - *serial console*: for servers without a screen.

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

On the live desktop, double-click **Install Valence**. The installer has four pages; **Next** is at the bottom right.

1. **Welcome.** It checks the memory and the disk size. Next.
2. **Partitions.** Choose the disk at the top, then one of:
   - **Erase disk**: Valence takes the whole disk. Everything on it is lost.
   - **Install alongside**: next to Windows. Use the slider to choose how much Valence gets. (The console
     installer's way of doing this, section 7b, is the one the tests cover.)
   - **Replace a partition** and **Manual partitioning**: for people who know partitioning. Manual partitioning
     has not been tested.

   Tick **Encrypt system** if the disk should be locked with a passphrase (you type it at every start; if you lose
   it, the data cannot be recovered). Next.
3. **Users.** Your name, the name you log in with, the computer's name, and a password (twice). The same password
   becomes the administrator's (root's). Next.
4. **Summary.** Check it, then **Install**, and confirm with **Install Now**. Installing takes a few minutes (about
   twice as long with encryption).

When it says **All done**, press **Done**. The computer restarts; pull the USB stick out when the screen goes dark,
or choose your disk in the computer's boot menu.

Valence is set up in US English and on UTC time. Change the language, keyboard layout and time zone afterwards in
Plasma's **System Settings**.

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

When it says **Valence is installed**, remove the USB stick and type `reboot`.

## 8. The first start

- With encryption, Valence asks for the passphrase first.
- The desktop shows the login screen; log in with your user name and password. The first time, a window
  "Welcome to Valence" offers Firefox, VLC, Steam and Prism Launcher from Flathub, the developer tools (a C/C++
  compiler and make) and, on a computer with an NVIDIA graphics card, a driver for it. Each needs the internet;
  `valence-first-boot --again` in a terminal asks again later.
- **Network:** a cable works by itself. On the desktop, Wi-Fi is in the panel's network icon. On the console
  system, `valence-wifi` lists the networks and `valence-wifi "Name" "password"` connects.
- **Administrator commands** start with `sudo` and ask for your password (Valence's `sudo` is a small wrapper around
  `doas`). Your user can use it; the installers put it into the `wheel` group.

Updating, installing software, and what to do when something goes wrong: [updating.md](updating.md).

## If something goes wrong while installing

- **The computer does not start from the stick:** Secure Boot is still on, or the stick was written by copying the
  file instead of the image (section 4), or the firmware's boot menu needs a different key.
- **The screen stays black:** start again and choose *safe graphics*.
- **The installer stops with a message:** the message says why. Nothing is installed when it stops before
  partitioning. Please report it, with the message, at the repository's
  [Issues](https://github.com/melon-77/v/issues) page; `valence-hwreport` writes a description of the computer that
  helps.
