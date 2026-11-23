#!/bin/bash
# The strict comparison on a file system of its own: FS=btrfs (mounted as valence-install mounts the root:
# subvol=@, noatime, compress=zstd:1), ext4 or xfs (noatime). The file system is made in an image file on the
# real disk, attached with direct I/O so that a flush reaches the disk and nothing is cached twice, and
# everything runs in a private mount namespace (nothing else on the machine can copy the mount and keep the
# device busy). Image, loop device and mount point are removed at the end.
#
# usage: [FS=btrfs] [LABEL=bt] [PARTS="a b sa sb"] [RUNS=15] [STRICT_OUT=dir] catalyst-strict-fs.sh
#   parts: a   installs (catalyst-strict.sh)
#          b   removals and upgrades (catalyst-strict2.sh without the HTTP cases, which measure no file system)
#          sa  installs on the small system, warm, with catalyst run with --sync
#          sb  removals and one upgrade on the small system with catalyst run with --sync
# apk is always run with --sync, as it behaves on an installed system.
# Or, to run something else on such a file system: INNER=script (it gets $MNT) in place of the parts.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
S=${STRICT_OUT:-/root/catalyst-test/strict}; mkdir -p "$S"
FS=${FS:-btrfs}; L=${LABEL:-$FS}; PARTS=${PARTS:-a b sa sb}
IMG=/root/catalyst-test/strict-fs.img
MNT=/root/catalyst-test/strict-fs
grep -q " $MNT " /proc/mounts && { echo "$MNT is mounted already"; exit 1; }
losetup -j $IMG | grep -q . && { echo "$IMG is attached already"; exit 1; }
rm -f $IMG; truncate -s ${IMG_SIZE:-24G} $IMG
LOOP=$(losetup --direct-io=on -f --show $IMG) || exit 1
cleanup(){ losetup -d $LOOP 2>/dev/null; rm -f $IMG; rmdir $MNT 2>/dev/null; }
trap cleanup EXIT
mkdir -p $MNT
echo "catalyst $(stat -c %y ${OCT:-${VALENCE:-/root/valence}/catalyst/target/release/oct} | cut -c1-19); $LOOP, direct I/O $(losetup -l -O DIO -n $LOOP | tr -d ' ')"
export HERE S L PARTS LOOP MNT FS STRICT_OUT=$S INNER=${INNER:-}
unshare --mount --propagation private bash -c '
  set -u
  case $FS in
    btrfs) mkfs.btrfs -q -f -L valence $LOOP || exit 1
           mount -t btrfs $LOOP $MNT || exit 1
           btrfs subvolume create $MNT/@ > /dev/null || exit 1
           umount $MNT
           mount -t btrfs -o subvol=@,noatime,compress=zstd:1 $LOOP $MNT || exit 1;;
    xfs)   mkfs.xfs -q -f $LOOP || exit 1; mount -t xfs -o noatime $LOOP $MNT || exit 1;;
    ext4)  mkfs.ext4 -q -F $LOOP || exit 1; mount -t ext4 -o noatime $LOOP $MNT || exit 1;;
    *)     echo "FS=$FS?"; exit 1;;
  esac
  grep " $MNT " /proc/mounts | cut -d" " -f3-4
  if [ -n "$INNER" ]; then
    bash "$INNER"
  else
    export APKSYNC=--sync RUNS=${RUNS:-15}
    for p in $PARTS; do
      case $p in
        a)  LABEL=$L-a  BDIR=$MNT/benchab  bash $HERE/catalyst-strict.sh > $S/bench-$L-a.log 2>&1;;
        b)  LABEL=$L-b  BDIR=$MNT/benchab2 CASES="del-nano del-cataclysm up-one up-all" bash $HERE/catalyst-strict2.sh > $S/bench-$L-b.log 2>&1;;
        sa) LABEL=$L-sa BDIR=$MNT/benchab  OCTSYNC=--sync SETS=base CACHES=warm bash $HERE/catalyst-strict.sh > $S/bench-$L-sa.log 2>&1;;
        sb) LABEL=$L-sb BDIR=$MNT/benchab2 OCTSYNC=--sync SETS=base CASES="del-nano del-cataclysm up-one" bash $HERE/catalyst-strict2.sh > $S/bench-$L-sb.log 2>&1;;
      esac
      echo "part $p done $(date -u +%T)"
    done
  fi
  umount $MNT || { sleep 2; umount $MNT; }
'
[ -n "$INNER" ] || { echo done; grep -c "faster" $S/bench-$L-*.log; grep -E "NOT OK|failed" $S/bench-$L-*.log | head; }
