#!/bin/bash
# mkiso.sh: assemble the Valence live/installer ISO (BIOS + UEFI hybrid) from the Valence package repo.
#
# The ISO carries two squashfs layers:
#   valence/rootfs.sqfs  a pristine, apk-installed Valence system (exactly what gets installed)
#   valence/live.sqfs    the small live-session layer on top (autologin, hostname, live services, motd)
# The initramfs stacks them with a RAM overlay. The installers copy rootfs.sqfs to disk (fast, and no
# second copy of every package on the ISO) and use the repo on the ISO only for extra packages.
set -euo pipefail
. "$(dirname "$(readlink -f "$0")")/env.sh"
APK=$M/hosttools/bin/apk
ROOT=$WORK/liveroot LIVE=$WORK/livelayer ISO=$WORK/iso INITRD=$WORK/initrd
DATE=$(date -u +%Y%m%d)   # UTC: WSL keeps Windows' time zone, which gave ISOs the day before's date
ISOARCH=$([ $VALENCE_ARCH = x86 ] && echo i686 || echo x86_64)
OUTISO=$M/out/valence-$DATE-$ISOARCH.iso
PKGS=${PKGS:-$(grep -v '^#' $M/scripts/console-packages.txt | xargs)}
# leave out optional packages that haven't been built for this architecture yet (with a warning)
_p=; for x in $PKGS; do
  if ls $M/repo/$APK_ARCH/$x-[0-9]*.apk >/dev/null 2>&1; then _p="$_p $x"; else echo "warning: $x is not built for $APK_ARCH, left out"; fi
done; PKGS=${_p# }
# VALENCE_EDITION=desktop: the Plasma live ISO. Its system image already contains the desktop (the installers
# copy it as is) plus the live-only installer packages, which both installers remove from the new system.
EDITION=${VALENCE_EDITION:-console}
DESKTOP_PKGS=${DESKTOP_PKGS:-}
LIVE_ONLY=
if [ "$EDITION" = desktop ]; then
  # the 32-bit edition has its own list (LXQt instead of Plasma): desktop-packages-x86.txt
  _list=$M/scripts/desktop-packages$ARCH_SUFFIX.txt; [ -f $_list ] || _list=$M/scripts/desktop-packages.txt
  DESKTOP_PKGS=${DESKTOP_PKGS:-$(grep -v '^#' $_list | xargs)}
  LIVE_ONLY=${LIVE_ONLY:-calamares-valence}
  OUTISO=$M/out/valence-desktop-$DATE-$ISOARCH.iso
fi
step(){ printf '\033[1;35m== %s\033[0m\n' "$*"; }
apkx(){ $APK --arch $APK_ARCH --keys-dir $M/keys/trusted --repositories-file /dev/null --repository $REPO/$APK_ARCH/Packages.adb --no-cache "$@"; }
PKG_MGR=${VALENCE_PKG:-apk}
if [ "$PKG_MGR" = vpm ]; then
  VPM=${VPM:-$M/vpm/target/release/vpm}
  [ -x "$VPM" ] || VPM=$M/hosttools/bin/vpm
  [ -x "$VPM" ] || { echo "VALENCE_PKG=vpm needs host vpm (run: cd vpm && cargo build --release)" >&2; exit 1; }
  case " $PKGS " in *" vpm "*) ;; *) PKGS="$PKGS vpm" ;; esac
  if [ "$EDITION" = desktop ]; then
    OUTISO=$M/out/valence-vpm-desktop-$DATE-$ISOARCH.iso
  else
    OUTISO=$M/out/valence-vpm-$DATE-$ISOARCH.iso
  fi
  pmx(){ "$VPM" --plain --arch $APK_ARCH --keys-dir $M/keys/trusted --repository $REPO/$APK_ARCH/Packages.adb --no-commit-hooks "$@"; }
else
  pmx(){ apkx "$@"; }
fi

step "system layer (what gets installed; package manager: $PKG_MGR)"
rm -rf $ROOT $LIVE $ISO $INITRD; mkdir -p $ROOT $LIVE $ISO/boot/grub $ISO/valence $INITRD
if [ "$EDITION" = desktop ]; then
  _p=; for x in $DESKTOP_PKGS $LIVE_ONLY; do
    if ls $M/repo/$APK_ARCH/$x-[0-9]*.apk >/dev/null 2>&1; then _p="$_p $x"; else echo "warning: $x is not built for $APK_ARCH, left out"; fi
  done; DESKTOP_PKGS=${_p# }
fi
if [ "$PKG_MGR" = vpm ]; then
  mkdir -p $ROOT/etc/apk
  echo "$APK_ARCH" > $ROOT/etc/apk/arch
fi
# BusyBox first, then the base layer (valence-base owns /etc/passwd and /etc/group), then the rest: install scripts
# need /bin/sh and addgroup/adduser (eudev builds its hwdb, ...), and the users they add (dbus's messagebus,
# polkitd) must not be overwritten by valence-base's passwd arriving later. apk only orders by declared deps.
pmx --root $ROOT --initdb add busybox
pmx --root $ROOT add valence-base
pmx --root $ROOT add $PKGS $DESKTOP_PKGS
rm -rf $ROOT/var/cache/apk/* $ROOT/var/cache/vpm/*
mkdir -p $ROOT/usr/share/valence/profiles
printf '%s\n' $PKGS > $ROOT/usr/share/valence/profiles/base
[ -n "$LIVE_ONLY" ] && printf '%s\n' $LIVE_ONLY > $ROOT/usr/share/valence/profiles/live-only
if [ -n "$DESKTOP_PKGS" ]; then
  printf '%s\n' $PKGS $(printf '%s\n' $DESKTOP_PKGS | grep -vxF "${LIVE_ONLY:-@none@}") > $ROOT/usr/share/valence/profiles/desktop
  # services for the desktop profile: udev replaces mdev, NetworkManager replaces the dhcp/wpa services
  # (only the ones the profile's packages ship: the 32-bit edition has no CUPS, Avahi or power-profiles-daemon)
  for s in -mdevd -dhcp udevd dbus elogind polkitd NetworkManager bluetoothd power-profiles-daemon zram sddm avahi-daemon cupsd; do
    case $s in -*) echo $s ;; *) [ ! -d $ROOT/etc/sv/$s ] || echo $s ;; esac
  done > $ROOT/usr/share/valence/profiles/desktop.services
fi
cp $ROOT/boot/vmlinuz-valence $ISO/boot/vmlinuz

step "live layer"
mkdir -p $LIVE/etc/sv/getty-tty1 $LIVE/etc/sv/getty-ttyS0 $LIVE/var/service $LIVE/etc/apk
echo valence-live > $LIVE/etc/hostname
sed 's/^root:[^:]*:/root::/' $ROOT/etc/shadow > $LIVE/etc/shadow; chmod 640 $LIVE/etc/shadow
echo 'GETTY_ARGS="-n -l /usr/bin/valence-autologin"' > $LIVE/etc/sv/getty-tty1/conf
echo 'GETTY_ARGS="-n -l /usr/bin/valence-autologin"' > $LIVE/etc/sv/getty-ttyS0/conf
if [ "$EDITION" = desktop ]; then
  for s in getty-tty2 getty-tty3 getty-ttyS0 syslogd klogd udevd dbus elogind polkitd NetworkManager bluetoothd \
           power-profiles-daemon zram sddm avahi-daemon cupsd qemu-ga vmtoolsd hv_kvp_daemon hv_vss_daemon hv_fcopy_uio_daemon; do
    [ ! -d $ROOT/etc/sv/$s ] || ln -sfn /etc/sv/$s $LIVE/var/service/$s; done
  # the live user: logs in automatically to Plasma (LXQt on the 32-bit edition), may use doas without a password, has the installer on the desktop
  awk -F: '$1!="live"' $ROOT/etc/passwd > $LIVE/etc/passwd; echo 'live:x:1000:1000:Valence live:/home/live:/bin/bash' >> $LIVE/etc/passwd
  awk -F: '$1!="live"' $LIVE/etc/shadow > $LIVE/etc/shadow.t; echo 'live::20000:0:99999:7:::' >> $LIVE/etc/shadow.t
  mv $LIVE/etc/shadow.t $LIVE/etc/shadow; chmod 640 $LIVE/etc/shadow
  awk -F: -v OFS=: '$1=="live"{next} $1 ~ /^(wheel|audio|video|input|render|plugdev|netdev|users)$/{$4=($4==""?"live":$4",live")} {print}' \
    $ROOT/etc/group > $LIVE/etc/group; echo 'live:x:1000:' >> $LIVE/etc/group
  mkdir -p $LIVE/home/live/Desktop $LIVE/etc/sddm.conf.d
  session_is_plasma=0; [ ! -f $ROOT/usr/share/wayland-sessions/plasma.desktop ] || session_is_plasma=1
  cp -a $ROOT/etc/skel/. $LIVE/home/live/
  # The live session does not lock its screen. Plasma would after five minutes without input, which an install on a
  # slow machine outlasts, and nobody can know that the live user's password is empty.
  if [ $session_is_plasma = 1 ]; then
    mkdir -p $LIVE/home/live/.config
    printf '[Daemon]\nAutolock=false\nLockOnResume=false\n' > $LIVE/home/live/.config/kscreenlockerrc
    # Nor does it dim or switch off the screen, or suspend: someone following the install guide saw the screen go
    # dark in the middle of an install (Plasma's power saving) and could take it for the end, or a crash, and pull
    # the stick. (powerdevil's own setting names: [<profile>][Display] and [<profile>][SuspendAndShutdown].)
    for prof in AC Battery LowBattery; do
      printf '[%s][Display]\nDimDisplayWhenIdle=false\nTurnOffDisplayWhenIdle=false\n\n[%s][SuspendAndShutdown]\nAutoSuspendAction=0\n\n' $prof $prof
    done > $LIVE/home/live/.config/powerdevilrc
  fi
  install -m755 $ROOT/usr/share/applications/valence-install.desktop $LIVE/home/live/Desktop/valence-install.desktop 2>/dev/null || true
  chown -R 1000:1000 $LIVE/home/live
  session=plasma; [ -f $ROOT/usr/share/wayland-sessions/plasma.desktop ] || session=lxqt-wayland
  printf '[Autologin]\nUser=live\nSession=%s\nRelogin=false\n' $session > $LIVE/etc/sddm.conf.d/20-live.conf
  { cat $ROOT/etc/doas.conf 2>/dev/null; echo 'permit nopass live'; } > $LIVE/etc/doas.conf; chmod 600 $LIVE/etc/doas.conf
else
  for s in getty-tty1 getty-tty2 getty-tty3 getty-ttyS0 mdevd syslogd klogd dhcp; do ln -sfn /etc/sv/$s $LIVE/var/service/$s; done
fi
{ echo "$(cat $ROOT/usr/share/valence/repo-url)/$APK_ARCH/Packages.adb"; echo "/media/valence/valence/repo/$APK_ARCH/Packages.adb"; } > $LIVE/etc/apk/repositories
cat > $LIVE/etc/motd <<'MOTD'

  Welcome to the Valence live system.

    valence-install     install Valence to a disk (valence-install --help for unattended use)
    valencefetch        show system info
    valence-svc list    list services
    valence-wifi        list Wi-Fi networks;  valence-wifi "Name" "password"  to connect
    (wired networks connect by themselves within a few seconds; ip addr shows the address)

MOTD

step "squashfs"
mksquashfs $ROOT $ISO/valence/rootfs.sqfs -comp zstd -Xcompression-level 15 -noappend -quiet
mksquashfs $LIVE $ISO/valence/live.sqfs -comp zstd -Xcompression-level 15 -noappend -quiet   # built as root; /home/live keeps its owner

step "package repository (extras only; the base system comes from rootfs.sqfs)"
mkdir -p $ISO/valence/repo/$APK_ARCH
# (the desktop packages themselves are NOT copied here: they are already in rootfs.sqfs, which the installers copy, and
# a second copy cost about 460 MB on the ISO and on every installed system; anything else comes from the online repo)
# always carry the small, commonly wanted extras so an offline install can still add them
extras="valence-base bash busybox glibc apk-tools"
[ "$PKG_MGR" = vpm ] && extras="$extras vpm"
# guest tools, which the installers add when they run inside a VM
ls $M/repo/$APK_ARCH/valence-vm-guest-[0-9]*.apk >/dev/null 2>&1 && extras="$extras valence-vm-guest"
apkx fetch --recursive --output $ISO/valence/repo/$APK_ARCH $extras >/dev/null
( cd $ISO/valence/repo/$APK_ARCH && $APK --keys-dir $M/keys/trusted --sign-key "${VALENCE_SIGN_KEY:-$M/keys/valence-signing.rsa}" mkndx -d "Valence $DATE" -o Packages.adb *.apk )

step "initramfs"
mkdir -p $INITRD/bin $INITRD/usr/lib $INITRD/dev
ln -s usr/lib $INITRD/lib; ln -s usr/lib $INITRD/lib64
cp $ROOT/usr/bin/busybox $INITRD/bin/busybox; ln -s busybox $INITRD/bin/sh
# busybox is a dynamic program: it needs the loader (it asks for it as /lib64/$LDSO) and the libraries it names
cp -L $ROOT/usr/lib/$LDSO $INITRD/usr/lib/
for l in $($TARGET-readelf -d $ROOT/usr/bin/busybox | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p'); do cp -L $ROOT/usr/lib/$l $INITRD/usr/lib/; done
cp $M/iso-files/init $INITRD/init
(cd $INITRD && find . | cpio -o -H newc --quiet | zstd -q -19 > $ISO/boot/initramfs.img)

step "grub"
cat > $ISO/boot/grub/grub.cfg <<'CFG'
set timeout=5
set default=0
# serial console only where there is a serial port (most VMs and PCs have none)
if serial --unit=0 --speed=115200; then
  terminal_input console serial
  terminal_output console serial
fi
# Valence is 64-bit: say so instead of booting into a black screen on a 32-bit CPU or VM
# (VirtualBox VMs created as "Other Linux" 32-bit have no 64-bit mode)
insmod cpuid
if cpuid -l; then set valence_cpu64=1; fi
if [ "$valence_cpu64" != 1 ]; then
  echo "Valence needs a 64-bit (x86_64) processor, and this computer or virtual machine doesn't offer one."
  echo ""
  echo "VirtualBox: Settings > General > Basic, set Version to 'Other Linux (64-bit)'."
  echo "  If only 32-bit versions are listed: turn on VT-x / AMD-V (SVM) in the PC's BIOS, and in"
  echo "  Windows turn off Hyper-V / Memory integrity so VirtualBox can use them."
  echo "Other VM apps (VMware, Hyper-V, QEMU): choose a 64-bit Linux guest."
  echo ""
  echo "Press any key to power off."
  sleep --interruptible 3600
  halt
fi
insmod all_video
# the kernel starts in the firmware's own mode (VGA text on BIOS, so its first messages and errors
# stay visible; the GOP framebuffer on UEFI)
set menu_color_normal=light-gray/black
set menu_color_highlight=black/light-green
menuentry 'Valence live' {
  linux /boot/vmlinuz quiet
  initrd /boot/initramfs.img
}
menuentry 'Valence live (copy to RAM)' {
  linux /boot/vmlinuz quiet toram
  initrd /boot/initramfs.img
}
menuentry 'Valence live (safe graphics)' {
  linux /boot/vmlinuz nomodeset
  initrd /boot/initramfs.img
}
menuentry 'Valence live (serial console)' {
  linux /boot/vmlinuz console=tty0 console=ttyS0,115200
  initrd /boot/initramfs.img
}
CFG
# the 32-bit edition runs on any x86 CPU: no 64-bit check
[ $VALENCE_ARCH != x86 ] || sed -i '/^# Valence is 64-bit/,/^fi$/d' $ISO/boot/grub/grub.cfg
# desktop edition: the Valence boot menu theme (from valence-desktop)
if [ -f $ROOT/usr/share/valence/grub/themes/valence/theme.txt ]; then
  mkdir -p $ISO/boot/grub/themes && cp -r $ROOT/usr/share/valence/grub/themes/valence $ISO/boot/grub/themes/
  fonts=$(cd $ISO/boot/grub/themes/valence && for f in *.pf2; do printf 'loadfont /boot/grub/themes/valence/%s; ' $f; done)
  sed -i "s|^insmod all_video\$|insmod all_video\nif loadfont unicode; then set gfxmode=auto; insmod gfxterm; insmod png; terminal_output gfxterm serial; ${fonts}set theme=/boot/grub/themes/valence/theme.txt; fi|" $ISO/boot/grub/grub.cfg
fi
mkdir -p $M/out
if [ "$PKG_MGR" = vpm ]; then
  if [ "$EDITION" = desktop ]; then rm -f $M/out/valence-vpm-desktop-2*-$ISOARCH.iso; else rm -f $M/out/valence-vpm-2*-$ISOARCH.iso; fi
else
  if [ "$EDITION" = desktop ]; then rm -f $M/out/valence-desktop-2*-$ISOARCH.iso; else rm -f $M/out/valence-2*-$ISOARCH.iso; fi
fi
$M/hosttools/grub$ARCH_SUFFIX/bin/grub-mkrescue -o $OUTISO $ISO -- -volid VALENCE 2>&1 | grep -v -E '^xorriso|^Drive|^Media|^libisofs|^Added|^ISO image|^Writing|^Written|^$' || true
( cd $M/out && sha256sum valence-*.iso > SHA256SUMS )
ls -la $OUTISO
