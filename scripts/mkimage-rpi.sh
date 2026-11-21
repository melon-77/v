#!/bin/bash
# mkimage-rpi.sh: the Raspberry Pi edition's SD card image (ROADMAP.md, phase 9), from the packages in repo/aarch64
# (VALENCE_ARCH=aarch64 scripts/valence-build valence-base linux-rpi rpi-firmware ...):
#     out/valence-rpi-<date>-aarch64.img      2 GiB; written to an SD card of 2 GB or more like an ISO to a stick
# For the Raspberry Pi 3, 4, 400, 5 and Zero 2 W, 64-bit. The card's layout (MBR, which the Pi's firmware reads):
#   1  256 MiB  FAT32  /boot: the firmware, kernel8.img, the boards' device trees, config.txt, cmdline.txt
#   2  rest     Btrfs  /: subvolumes @ (the system), @home, @log (/var/log) and @snapshots (/.snapshots), as on a PC
# The first start grows partition 2 and its filesystem to the whole card. There is no installer: the card is the
# system. root's first password is "valence", already expired: the first login asks for a new one (shadow's login
# refuses an empty root password, PREVENT_NO_AUTH). There is no SSH server.
set -euo pipefail
export VALENCE_ARCH=aarch64
. "$(dirname "$(readlink -f "$0")")/env.sh"
APK=$M/hosttools/bin/apk
DATE=$(date -u +%Y%m%d)
OUT=$M/out/valence-rpi-$DATE-aarch64.img
ROOT=$WORK/rpiroot MNT=$WORK/rpimnt
SIZE_MIB=${SIZE_MIB:-2048}
PKGS=${PKGS:-"valence-base linux-rpi rpi-firmware kmod zstd openssl ca-certificates ncurses-terminfo wpa_supplicant"}
apkx(){ $APK --arch aarch64 --keys-dir $M/keys/trusted --repositories-file /dev/null --repository $REPO/aarch64/Packages.adb --no-cache "$@"; }
step(){ printf '\033[1;35m== %s\033[0m\n' "$*"; }
# the package scripts are aarch64 programs: they run through QEMU's user-mode emulator (binfmt, Ubuntu's qemu-user-static)
[ -e /proc/sys/fs/binfmt_misc/qemu-aarch64 ] || { echo "mkimage-rpi: aarch64 programs cannot run here: apt install qemu-user-static" >&2; exit 1; }

cleanup(){ set +e; for m in $MNT/boot $MNT/root $MNT/top; do mountpoint -q $m && umount $m; done; [ -n "${LOOP:-}" ] && losetup -d $LOOP; }
trap cleanup EXIT

step "the system"
rm -rf --one-file-system $ROOT; mkdir -p $ROOT
apkx --root $ROOT --initdb add busybox
apkx --root $ROOT add valence-base
apkx --root $ROOT add $PKGS
rm -rf $ROOT/var/cache/apk/*
printf '%s\n' $PKGS > $ROOT/usr/share/valence/rpi-packages

step "settings"
echo valence > $ROOT/etc/hostname
sed -i 's/^127\.0\.1\.1.*/127.0.1.1\tvalence/' $ROOT/etc/hosts
# the online repository, and a small copy on the card (as the ISOs have) so that apk can add a few things offline,
# and while the online one cannot be reached
echo "$(cat $ROOT/usr/share/valence/repo-url)/aarch64/Packages.adb" > $ROOT/etc/apk/repositories
echo /var/lib/valence/repo/aarch64/Packages.adb >> $ROOT/etc/apk/repositories
EXTRAS=${EXTRAS:-"nano"}
mkdir -p $ROOT/var/lib/valence/repo/aarch64
apkx fetch --recursive --output $ROOT/var/lib/valence/repo/aarch64 $EXTRAS >/dev/null
( cd $ROOT/var/lib/valence/repo/aarch64 && $APK --keys-dir $M/keys/trusted --sign-key "${VALENCE_SIGN_KEY:-$M/keys/valence-signing.rsa}" \
    mkndx -d "Valence for the Raspberry Pi $DATE" -o Packages.adb *.apk )
# root's first password, "valence", expired from the start (last change on day 0): the first login asks for a new one
sed -i "s|^root:[^:]*:[^:]*:|root:$(openssl passwd -6 valence):0:|" $ROOT/etc/shadow
printf '\nFirst login: root, password valence. You choose a new password right away.\n\n' >> $ROOT/etc/issue
# root may log in on the Pi's serial ports too: shadow's login allows root only on the terminals in /etc/securetty,
# which lists the PC's ttyS0 (the Pi 3 and 4's console) but not the PL011s: ttyAMA0 (the Pi 5's GPIO UART),
# ttyAMA10 (the Pi 5's debug connector) and ttyAMA1 (QEMU's Raspberry Pi)
for t in ttyAMA0 ttyAMA1 ttyAMA10; do grep -qx $t $ROOT/etc/securetty || echo $t >> $ROOT/etc/securetty; done
cat > $ROOT/etc/motd <<'EOF'

  Welcome to Valence on the Raspberry Pi.

    passwd              change root's password
    valence-wifi        Wi-Fi networks; valence-wifi "Name" "password" to connect
    (a network cable works by itself; ip addr shows the address)
    apk add <package>   install software (a snapshot is taken first: valence-snapshot)

EOF
# the first start: partition 2 and its filesystem grow to the whole card, once
mkdir -p $ROOT/etc/sv/rpi-grow-root
cat > $ROOT/etc/sv/rpi-grow-root/run <<'EOF'
#!/bin/sh
# The first start of a Valence SD card: partition 2 and the Btrfs on it grow to the end of the card. Runs once.
exec 2>&1
dev=$(findmnt -n -o SOURCE / | sed 's/\[.*//')
disk=/dev/$(lsblk -n -d -o PKNAME "$dev")
echo ", +" | sfdisk -q --no-reread --no-tell-kernel -N 2 "$disk" && partx -u "$disk"
btrfs filesystem resize max / && echo "rpi-grow-root: / now fills the card"
rm -f /var/service/rpi-grow-root
exec sleep 3600
EOF
chmod +x $ROOT/etc/sv/rpi-grow-root/run
# a login on the PL011 serial port too (ttyAMA<n>), only when the kernel's console is there: on a Pi with Bluetooth
# that port is the Bluetooth chip's, and the firmware turns cmdline.txt's console=serial0 into ttyS0 (getty-ttyS0);
# QEMU's Raspberry Pi has no firmware to do that, and its serial port is the PL011 (scripts/qemu-test-rpi.py)
mkdir -p $ROOT/etc/sv/getty-ttyAMA
cat > $ROOT/etc/sv/getty-ttyAMA/run <<'EOF'
#!/bin/sh
# a login on the PL011 port the kernel's console is on (console=ttyAMA<n> on its command line), and none otherwise
[ -r conf ] && . ./conf
t=$(tr ' ' '\n' < /proc/cmdline | sed -n 's/^console=\(ttyAMA[0-9]*\).*/\1/p' | tail -n 1)
[ -n "$t" ] || exec sleep 2147483647
exec setsid getty ${GETTY_ARGS} 115200 $t vt100
EOF
chmod +x $ROOT/etc/sv/getty-ttyAMA/run
mkdir -p $ROOT/var/service
for s in getty-tty1 getty-ttyS0 getty-ttyAMA mdevd syslogd klogd crond ntpd dhcp rpi-grow-root; do
  [ -d $ROOT/etc/sv/$s ] && ln -sfn /etc/sv/$s $ROOT/var/service/$s
done

step "the card"
rm -f $OUT; truncate -s ${SIZE_MIB}M $OUT
sfdisk -q $OUT <<EOF
label: dos
start=2048, size=256MiB, type=c, bootable
type=83
EOF
LOOP=$(losetup -P -f --show $OUT)
mkfs.vfat -F 32 -n VALENCEBOOT ${LOOP}p1 >/dev/null
mkfs.btrfs -q -f -L valence ${LOOP}p2
PARTUUID=$(blkid -s PTUUID -o value $LOOP)
ROOT_UUID=$(blkid -s UUID -o value ${LOOP}p2) BOOT_UUID=$(blkid -s UUID -o value ${LOOP}p1)
mkdir -p $MNT/top $MNT/root $MNT/boot
mount ${LOOP}p2 $MNT/top
for v in @ @home @log @snapshots; do btrfs subvolume create $MNT/top/$v >/dev/null; done
mount -o subvol=@,compress=zstd:1 ${LOOP}p2 $MNT/root
mount ${LOOP}p1 $MNT/boot
# the system into @, /home and /var/log into their own, /boot onto the FAT partition
(cd $ROOT && tar --xattrs --xattrs-include='*' --exclude=./boot --exclude=./home --exclude=./var/log -cf - .) | (cd $MNT/root && tar -xpf -)
mkdir -p $MNT/root/boot $MNT/root/home $MNT/root/var/log $MNT/root/.snapshots
(cd $ROOT/home && tar -cf - .) | (cd $MNT/top/@home && tar -xpf -)
(cd $ROOT/var/log && tar -cf - .) | (cd $MNT/top/@log && tar -xpf -)
cp -r $ROOT/boot/. $MNT/boot/
O=noatime,compress=zstd:1
cat > $MNT/root/etc/fstab <<EOF
# <device>                                 <mountpoint> <type> <options>                 <dump> <pass>
UUID=$ROOT_UUID  /            btrfs  subvol=@,$O           0  0
UUID=$ROOT_UUID  /home        btrfs  subvol=@home,$O       0  0
UUID=$ROOT_UUID  /var/log     btrfs  subvol=@log,$O        0  0
UUID=$ROOT_UUID  /.snapshots  btrfs  subvol=@snapshots,$O  0  0
UUID=$BOOT_UUID                             /boot        vfat   defaults,noatime          0  2
EOF
cat > $MNT/boot/config.txt <<'EOF'
# Valence on the Raspberry Pi (3, 4, 400, Zero 2 W): read by the Pi's firmware at every start
arm_64bit=1
kernel=kernel8.img
# the serial console on GPIO 14 and 15 (the mini UART on boards with Bluetooth): 115200 baud
enable_uart=1
# the kernel's own graphics driver
dtoverlay=vc4-kms-v3d
# (kernel8.img on every board, the Pi 5 too, which would otherwise look for kernel_2712.img)
disable_overscan=1
EOF
echo "console=serial0,115200 console=tty1 root=PARTUUID=$PARTUUID-02 rootfstype=btrfs rootflags=subvol=@ rootwait" > $MNT/boot/cmdline.txt
du -sh $MNT/root $MNT/boot | sed 's/^/   /'
cleanup; trap - EXIT
ls -la $OUT
