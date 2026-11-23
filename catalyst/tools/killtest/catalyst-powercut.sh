#!/bin/bash
# catalyst power-cut test: what is on the disk when the power goes, at any
# moment of a change or right after it, must be the change not made or a
# state the next run finishes.
#
# The kill test (catalyst-killtest.sh) stops the process; everything it had
# written is still in the page cache and reaches the disk. A power cut also
# loses what was not written out yet, and it does so unevenly: a file's name
# can be on disk without its content. That is what this test produces.
#
# The root lives in a file system of its own (ext4 by default) in an image
# file, mounted through a device-mapper device. Each round a fresh copy of
# the image is mounted and the change is started. Then the device is
# suspended (nothing more reaches the image) and the image is copied as it
# is: that copy is the disk after a power cut at that moment. Three kinds of
# round, in rotation:
#   time     the cut comes at a random moment of the whole run;
#   switch   catalyst is stopped (SIGSTOP) at a random moment of its switch,
#            counted from when the journal appears; what it has done so far
#            is put on disk; then the cut. In half of these rounds "put on
#            disk" is a syncfs (everything so far, contents included). In
#            the other half, on ext4 and XFS, a second process has been
#            renaming and fsyncing a file of its own on that file system
#            every few milliseconds: each of its fsyncs commits the file
#            system's journal, so every name made so far (links, renames,
#            removals, by anybody) is on disk, and the contents of files
#            still in the page cache are not. (On btrfs an fsync logs that
#            one file only, so there every switch round uses the syncfs.)
#   late     the cut comes up to 40 ms after the command returned (it must
#            have succeeded).
# In the even rounds the second process runs throughout.
#
# Then the copy is mounted (the file system replays its own journal), and:
#   1. `oct recover` must succeed, and `oct --simulate fix` must find
#      nothing to repair;
#   2. the root must equal either the root before the change or the root
#      after a clean run of it, entry for entry (type, mode, owner, size,
#      mtime, content, link target, hard links, Programs, the packages' own
#      copies of files under etc/, world, lookup index;
#      catalyst-kt-check.py final): all or nothing. After a command run
#      with --sync that had returned, it must be the root after;
#   3. the change is run again and must succeed; the root must equal the
#      clean run's; one more run must have nothing to do;
#   4. `oct --simulate fix` must again find nothing to repair;
#   5. every 7th round, and after a failed one, e2fsck checks the copied
#      image as it was cut (a check of the test itself: the copy must be a
#      file system state that can exist).
# A run in which fewer than a third of the switch rounds left a journal on
# the disk says nothing about a begun switch: it ends with status 3.
#
# usage: catalyst-powercut.sh [ROUNDS]     (default 60; must run as root)
#   env PC_JOB=add|del|small|upgrade|bigadd|bigdel   the change (default add):
#         add      add nano less tree htop tmux jq bc file
#         del      the removal of those eight
#         small    add -u coreutils util-linux shadow gzip nano  (9999-r0
#                  upgrades with their scripts, and a new package)
#         upgrade  upgrade to the 27 packages rebuilt as 9999-r0
#         bigadd   add cataclysm-dda-tiles (39 packages, 8,600 files: the
#                  switch places and links on several threads; needs
#                  PC_SIZE_MB=2000)
#         bigdel   the removal of those (entries removed and old versions
#                  deleted on several threads)
#   env PC_SCRIPTS=0     pass --no-scripts (no script, no trigger: the change
#                        stays on the one-sync path all the way)
#   env PC_SYNC=1        pass --sync
#   env PC_FS=ext4|btrfs|xfs  the file system of the image (default ext4,
#                        mounted noauto_da_alloc so that it does not write a
#                        file's content early on its own account)
#   env PC_SEED=N        fixed seed (default: the time)
#   env PC_OCT=PATH      the catalyst binary (default /root/catalyst-target/release/oct)
#   env PC_SIZE_MB=N     size of the image (default 900)
# Writes only under /dev/shm/catalyst-pc (the images) and
# /root/catalyst-test/pc-mnt (the mount point). Exit status 0 when every
# round passes, 1 when one failed, 3 when the run was inconclusive.
set -u
# In a mount namespace of its own: a service that starts while an image is
# mounted (and takes a copy of every mount with it) would otherwise keep
# the image's device busy after the test has unmounted it.
if [ -z "${PC_IN_NS:-}" ]; then
    exec env PC_IN_NS=1 unshare --mount --propagation private bash "$(readlink -f "$0")" "$@"
fi
ROUNDS=${1:-60}
SEED=${PC_SEED:-$(date +%s)}
V=${PC_OCT:-/root/catalyst-target/release/oct}
S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)
R1=/root/valence/repo/x86_64/Packages.adb
R3=/root/catalyst-test/kt-repo/x86_64/Packages.adb
KD=/root/catalyst-test/keys/trusted
PC=/dev/shm/catalyst-pc
MNT=/root/catalyst-test/pc-mnt
DM=catalyst-pc
FS=${PC_FS:-ext4}
SIZE=${PC_SIZE_MB:-900}
PY="python3 -I"
CHK="$PY $S/catalyst-kt-check.py"
LOOP=""; BG=""
MOPT=noatime; [ $FS = ext4 ] && MOPT=noatime,noauto_da_alloc

die() { echo "ABORT: $*"; cleanup; exit 2; }
detach() {
    mountpoint -q $MNT && { umount $MNT 2>/dev/null || { sleep 0.3; umount $MNT 2>/dev/null || umount -l $MNT; }; }
    if dmsetup info $DM > /dev/null 2>&1; then
        dmsetup resume $DM 2>/dev/null
        # the device can stay busy for a moment after the unmount
        for try in $(seq 50); do dmsetup remove $DM 2>/dev/null && break; sleep 0.1; done
        dmsetup info $DM > /dev/null 2>&1 && dmsetup remove -f $DM
    fi
    [ -n "$LOOP" ] && losetup -d $LOOP 2>/dev/null
    LOOP=""
}
cleanup() {
    [ -n "$BG" ] && kill -KILL $BG 2>/dev/null
    BG=""
    detach
}
trap 'cleanup' EXIT
attach() {  # attach IMAGE: mounted at $MNT through the dm device
    LOOP=$(losetup --find --show "$1") || die "losetup $1"
    dmsetup create $DM --table "0 $(blockdev --getsz $LOOP) linear $LOOP 0" || die "dmsetup create"
    mount -o $MOPT /dev/mapper/$DM $MNT || die "mount of $1 failed"
}

[ "$(id -u)" = 0 ] || { echo "must run as root"; exit 2; }
[ -x $V ] || { echo "$V not built"; exit 2; }
command -v dmsetup > /dev/null && command -v mkfs.$FS > /dev/null || { echo "dmsetup and mkfs.$FS are needed"; exit 2; }
mkdir -p $PC $MNT
mountpoint -q $MNT && { echo "$MNT is a mount point already"; exit 2; }
dmsetup info $DM > /dev/null 2>&1 && { echo "a device $DM exists already"; exit 2; }
rm -f $PC/*.img $PC/*.log $PC/*.json $PC/*.out

FLAGS="--plain"
[ "${PC_SCRIPTS:-1}" = 0 ] && FLAGS="$FLAGS --no-scripts"
[ "${PC_SYNC:-0}" = 1 ] && FLAGS="$FLAGS --sync"
# PC_FLAGS: more options for the change, e.g. --clean-later
FLAGS="$FLAGS${PC_FLAGS:+ $PC_FLAGS}"
EIGHT="nano less tree htop tmux jq bc file"
PREP=""
case ${PC_JOB:-add} in
    add) JOB="add $EIGHT";;
    del) PREP="$EIGHT"; JOB="del $EIGHT";;
    small) JOB="add -u coreutils util-linux shadow gzip nano";;
    upgrade) JOB="upgrade";;
    bigadd) JOB="add cataclysm-dda-tiles";;
    bigdel) PREP="cataclysm-dda-tiles"; JOB="del cataclysm-dda-tiles";;
    *) echo "PC_JOB is add, del, small, upgrade, bigadd or bigdel"; exit 2;;
esac
case ${PC_JOB:-add} in small|upgrade) [ -f $R3 ] || { echo "$R3 is missing (run catalyst-killtest.sh once: it builds it)"; exit 2; };; esac
REPOS="--repository $R1"; [ -f $R3 ] && REPOS="$REPOS --repository $R3"
oct() { $V --root $MNT/r $REPOS --keys-dir $KD $FLAGS "$@"; }
# old versions that a change left to a process of its own (--clean-later): gone before a root is looked at
wait_trash() { local n=0; while [ -e "$1/var/lib/catalyst/trash" ] && [ $n -lt 6000 ]; do sleep 0.005; n=$((n + 1)); done; }
rnd() { $PY -c "import random,sys; random.seed(sys.argv[1]); print('%.4f' % random.uniform(float(sys.argv[2]), float(sys.argv[3])))" "$SEED-$1" "$2" "$3"; }
now() { date +%s.%N; }
calc() { awk "BEGIN{printf \"%.4f\", $*}"; }
J=$MNT/r/var/lib/catalyst/journal

echo "=== catalyst power-cut test: $ROUNDS rounds of 'oct $FLAGS $JOB' on $FS ($MOPT), seed $SEED, $(date)"
echo "    catalyst binary of $(ls -l --time-style=+%F.%T $V | awk '{print $6}')"

# ------------------------------------------------------------ the base image
truncate -s ${SIZE}M $PC/base.img
case $FS in
    ext4) mkfs.ext4 -q -F $PC/base.img;;
    btrfs) mkfs.btrfs -q -f $PC/base.img;;
    xfs) mkfs.xfs -q -f $PC/base.img;;
    *) echo "PC_FS is ext4, btrfs or xfs"; exit 2;;
esac || die "mkfs.$FS"
attach $PC/base.img
$V --root $MNT/r --initdb --repository $R1 --keys-dir $KD --plain add valence-base > $PC/install.log 2>&1 || die "install failed: $PC/install.log"
if [ -n "$PREP" ]; then
    $V --root $MNT/r --repository $R1 --keys-dir $KD --plain add $PREP >> $PC/install.log 2>&1 || die "install of $PREP failed: $PC/install.log"
fi
echo "# admin was here" >> $MNT/r/etc/fstab
touch -d @1795000000 $MNT/r/etc/fstab
sync
wait_trash $MNT/r; $CHK snap $MNT/r $PC/pre.json > /dev/null || die "snapshot of the base root"
detach

# ------------------------------------------------------------ the clean run (and how long its journal lives)
cp --sparse=always $PC/base.img $PC/work.img
attach $PC/work.img
t0=$(now)
oct $JOB > $PC/ref.log 2>&1 &
JP=$!
tj0=""; tj1=""
while kill -0 $JP 2>/dev/null; do
    if [ -z "$tj0" ]; then [ -e $J ] && tj0=$(now); elif [ -z "$tj1" ]; then [ -e $J ] || tj1=$(now); fi
done
wait $JP || die "the clean run failed: $PC/ref.log"
t1=$(now)
TREF=$(calc "$t1 - $t0")
JLIFE=0.002; [ -n "$tj0" ] && JLIFE=$(calc "${tj1:-$t1} - $tj0 + 0.001")
sync
wait_trash $MNT/r; $CHK snap $MNT/r $PC/ref.json > /dev/null || die "snapshot of the reference root"
wait_trash $MNT/r; $CHK final $MNT/r $PC/pre.json > /dev/null 2>&1 && die "the change changes nothing the check sees"
detach
echo "    clean run: ${TREF}s, its journal there for about ${JLIFE}s; $(grep '^OK:' $PC/ref.log | cut -c1-100)"
WIN=$(calc "$TREF * 1.1 + 0.005")
SWIN=$(calc "$JLIFE * 1.1")
echo "    cuts: at 0 .. ${WIN}s of the run (2 rounds of 5); stopped 0 .. ${SWIN}s into the switch (2 of 5); 0 .. 0.04s after the command returned (1 of 5)"

# ------------------------------------------------------------ rounds
FAILS=0; NRUN=0; NDONE=0; NUNDONE=0; NJ=0; KEPT=0; NSW=0; NSWD=0; NSWDJ=0; NSWJ=0
printf '%3s %-6s %-7s %-9s %-8s %-8s %-10s %s\n' "#" fsync when "cut at" "running" journal "recovered" result | tee $PC/results.txt
for i in $(seq 1 $ROUNDS); do
    L=$PC/round.log; : > $L
    cp --sparse=always $PC/base.img $PC/work.img
    attach $PC/work.img
    fsy=no
    if [ $(( i % 2 )) = 0 ]; then fsy=yes; $PY $S/catalyst-pc-fsync.py $MNT "$SEED-$i" & BG=$!; sleep 0.03; fi
    # the delays are worked out before the command starts
    case $(( i % 5 )) in
        0) when=late; d=$(rnd $i 0 0.04);;
        1|2) when=time; d=$(rnd $i 0 $WIN);;
        *) when=switch; d=0; [ $(( i % 3 )) = 0 ] || d=$(rnd $i 0 $SWIN);;
    esac
    setsid $V --root $MNT/r $REPOS --keys-dir $KD $FLAGS $JOB > $PC/run1.log 2>&1 &
    JP=$!
    ts=$(now); jrc=0; fail=""
    case $when in
        late) wait $JP; jrc=$?; sleep $d;;
        time) sleep $d;;
        switch)
            while [ ! -e $J ] && kill -0 $JP 2>/dev/null; do :; done
            [ $d = 0 ] || sleep $d
            # frozen where it is; what it has done so far goes to the disk:
            # the names only (the other process's fsyncs), or everything
            kill -STOP -- -$JP 2>/dev/null
            if [ $fsy = yes ] && [ $FS != btrfs ]; then sleep 0.02; else sync -f $MNT; fi;;
    esac
    running=no; kill -0 $JP 2>/dev/null && running=yes
    # the power cut: nothing more reaches the image; it is copied as it is
    dmsetup suspend --noflush --nolockfs $DM || die "dmsetup suspend"
    at=$(calc "$(now) - $ts")
    cp --sparse=always $PC/work.img $PC/cut.img
    kill -KILL -- -$JP 2>/dev/null; [ -n "$BG" ] && kill -KILL $BG 2>/dev/null
    # the process that deletes old versions after the command (--clean-later) is its own group: it ends with the power too
    pkill -KILL -f -- "--root $MNT/r empty-trash" 2>/dev/null
    dmsetup resume $DM
    wait $JP 2>/dev/null; [ -n "$BG" ] && wait $BG 2>/dev/null; BG=""
    detach
    cp --sparse=always $PC/cut.img $PC/cut0.img   # the disk as it was cut (for e2fsck, and kept when the round fails)
    [ $running = yes ] && NRUN=$((NRUN + 1))
    [ $when = late ] && [ $jrc != 0 ] && fail="$fail command-exit-$jrc"
    # the disk after the cut
    attach $PC/cut.img
    jr=no; [ -e $J ] && { jr=yes; NJ=$((NJ + 1)); }
    [ $when = switch ] && { NSW=$((NSW + 1)); [ $jr = yes ] && NSWJ=$((NSWJ + 1)); }
    # the switch rounds cut some way into the switch, not at its first moment
    [ $when = switch ] && [ $d != 0 ] && { NSWD=$((NSWD + 1)); [ $jr = yes ] && NSWDJ=$((NSWDJ + 1)); }
    { echo "=== round $i: fsync $fsy, $when, cut at ${at}s (running $running), journal $jr"; ls -la $MNT/r/var/lib/catalyst/ | sed 's/^/    | /'; } >> $L
    $V --root $MNT/r --plain recover > $PC/recover.log 2>&1; rc=$?
    { echo "--- recover: exit $rc"; sed 's/^/    | /' $PC/recover.log | tail -6; } >> $L
    [ $rc = 0 ] || fail="$fail recover-exit-$rc"
    $V --root $MNT/r --plain --simulate fix > $PC/fix0.log 2>&1 || fail="$fail fix-after-recover-failed"
    grep -q ' 0 would be repaired' $PC/fix0.log || { fail="$fail fix-finds-damage-after-recover"; tail -n 5 $PC/fix0.log >> $L; }
    state=neither
    if wait_trash $MNT/r; $CHK final $MNT/r $PC/ref.json > $PC/f-ref.out 2>&1; then state=done; NDONE=$((NDONE + 1));
    elif wait_trash $MNT/r; $CHK final $MNT/r $PC/pre.json > $PC/f-pre.out 2>&1; then state=undone; NUNDONE=$((NUNDONE + 1));
    else fail="$fail neither-before-nor-after"; { echo "--- against the clean run:"; tail -n 12 $PC/f-ref.out; echo "--- against the root before:"; tail -n 12 $PC/f-pre.out; } >> $L; fi
    # a command run with --sync that had returned: its change is on disk
    [ $when = late ] && [ "${PC_SYNC:-0}" = 1 ] && [ $state != done ] && fail="$fail returned-with---sync-but-not-on-disk"
    oct $JOB > $PC/complete.log 2>&1; rc=$?
    { echo "--- completing run: exit $rc"; sed 's/^/    | /' $PC/complete.log | grep -v '^    |   \(upgrade\|install\|remove\) ' | tail -6; } >> $L
    [ $rc = 0 ] || fail="$fail complete-exit-$rc"
    oct $JOB > $PC/again.log 2>&1 || fail="$fail again-failed"
    grep -q 'nothing to do' $PC/again.log || fail="$fail not-done"
    wait_trash $MNT/r; $CHK final $MNT/r $PC/ref.json > $PC/final.out 2>&1 || { fail="$fail final-differs"; tail -n 14 $PC/final.out >> $L; }
    $V --root $MNT/r --plain --simulate fix > $PC/fix.log 2>&1 || fail="$fail fix-failed"
    grep -q ' 0 would be repaired' $PC/fix.log || { fail="$fail fix-finds-damage"; tail -n 5 $PC/fix.log >> $L; }
    detach
    if [ $FS = ext4 ] && { [ $(( i % 7 )) = 0 ] || [ -n "$fail" ]; }; then
        cp --sparse=always $PC/cut0.img $PC/fsck.img
        e2fsck -fy $PC/fsck.img > $PC/fsck.out 2>&1; frc=$?
        # 0: clean; 1: errors corrected (the journal replayed and orphans cleared count as that)
        [ $frc -le 1 ] || { fail="$fail e2fsck-$frc(the test's copy is not a valid state)"; tail -n 5 $PC/fsck.out >> $L; }
        rm -f $PC/fsck.img
    fi
    printf '%3d %-6s %-7s %-9s %-8s %-8s %-10s %s\n' $i $fsy $when "${at}s" $running $jr $state "$( [ -z "$fail" ] && echo PASS || echo "FAIL:$fail" )" | tee -a $PC/results.txt
    cat $L >> $PC/all-rounds.log
    if [ -n "$fail" ]; then
        FAILS=$((FAILS + 1))
        cp $L $PC/fail-$i.log
        if [ $KEPT -lt ${PC_KEEP:-3} ]; then mv $PC/cut0.img $PC/fail-$i.img; KEPT=$((KEPT + 1)); echo "    the image as it was cut is kept as $PC/fail-$i.img, log $PC/fail-$i.log"; fi
    fi
done
rm -f $PC/work.img $PC/cut.img $PC/cut0.img
echo "=== $ROUNDS rounds: cut while the command ran in $NRUN; a journal on the disk in $NJ ($NSWJ of the $NSW switch rounds, $NSWDJ of the $NSWD cut some way into the switch); after recovery the change was whole in $NDONE and absent in $NUNDONE; $FAILS failed (seed $SEED)"
echo "    results in $PC/results.txt, every round's log in $PC/all-rounds.log"
[ $FAILS = 0 ] || exit 1
# The rounds cut at the switch's first moment always have their journal and nothing placed: they say nothing
# about a switch under way. Of the others, a third at least must have caught one.
if [ $NSWD -gt 0 ] && [ $(( NSWDJ * 3 )) -lt $NSWD ]; then
    echo "INCONCLUSIVE: a journal was on the disk in only $NSWDJ of the $NSWD rounds cut some way into the switch"
    exit 3
fi
exit 0
