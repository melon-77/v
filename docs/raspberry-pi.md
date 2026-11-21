# Valence on a Raspberry Pi

A test image for the Raspberry Pi 3, 4, 400, 5 and Zero 2 W, 64-bit (ROADMAP.md, phase 9). It is the console system:
no desktop and no installer. The SD card is the system.

It boots in QEMU's emulated Raspberry Pi 3B and passes its automated test there; **it has not yet been tried on a
real Pi.**

## Writing the card

The image is `valence-rpi-<date>-aarch64.img` (2 GB; the `.img.xz` download unpacks to it). Write it to an SD card of
2 GB or more, byte for byte, like the PC ISOs (docs/install.md, section 4): Raspberry Pi Imager ("Use custom"),
Rufus or balenaEtcher on Windows, or on Linux

```sh
xz -d valence-rpi-20261121-aarch64.img.xz
sudo dd if=valence-rpi-20261121-aarch64.img of=/dev/sdX bs=4M status=progress oflag=sync
```

The first start grows the system to the whole card by itself.

## The first start

- Connect a screen and keyboard, or a USB serial adapter on GPIO 14/15 (115200 baud; on a Pi 5 also its debug
  connector), and a network cable.
- Log in as **root** with the password **valence**. It asks for a new password at once.
- The network cable works by itself (`ip addr` shows the address). Wi-Fi: `valence-wifi`.
- `apk add <package>` installs software, with a snapshot first (`valence-snapshot`). The card carries a small copy of
  the packages (nano among them) for while the online repository cannot be reached; everything else comes from it.
- There is no SSH server on the card, and nothing else listens on the network.

## What is not there yet

A desktop; Wi-Fi tested on a real board; the Pi's real-time clock (it has none: the time is right once ntpd reaches
the internet); rolling back from a boot menu (there is none; `valence-snapshot rollback` is there but has not been tried on the
card, and it would keep the newest kernel); documentation packages (mandoc is not built for ARM yet).
