#!/bin/bash
# catalyst kill-safety test for upgrades (ROADMAP phase 10, stage 1: "an upgrade
# that is killed at any point leaves a system that boots").
#
# Upgrade: a fresh root with valence-base (39 packages, from the real
# repository) is upgraded to 27 of its packages rebuilt as 9999-r0 (glibc,
# busybox, bash, coreutils, util-linux, shadow, ncurses-libs, ...; built by
# catalyst-kt-mkrepo.sh, signed with the throwaway key in /root/catalyst-test/keys;
# with content changes, a file removed and one added, a file that becomes a
# symlink, the lib64 symlink moving between packages, and pre/post-upgrade
# scripts), after an "admin" changed etc/fstab.
#
# Each round: fresh root; snapshot; "oct upgrade" started and its whole
# process group SIGKILLed (a package script it runs dies too, as on a
# crash) at a random moment; then the invariants (catalyst-kt-check.py crash):
#   - every path is the old or the new version: files outside etc/ are the
#     old inode or the inode of the new Programs copy; etc/ files have the
#     old or the new content; symlinks the old or the new target; owner and
#     mode match; a path present before and after is never missing; nothing
#     unexpected appeared (in-flight .catalyst.* temporaries are only noted);
#     every Programs/*/Current names a complete version;
#   - /bin/sh, /usr/lib/libc.so.6 and the dynamic loader exist (resolved in
#     the root) and work under chroot: sh -c 'echo ok', running libc.so.6,
#     ld.so --version, ld.so --list /usr/bin/ls, ls /, bash;
# then "oct upgrade" must succeed (finishing the switch when a journal was
# left), a further one must have nothing to do, and the root must equal one
# upgraded without interruption (catalyst-kt-check.py final: every entry with
# type, mode, owner, size, mtime, content, link target; owners; Programs
# versions, Current and metadata; world; the lookup index; hard links; no
# journal, temporaries or .partial left).
#
# Kill moments, in rotation (catalyst-kt-kill.py; SIGKILL to the process group):
#   time      uniform over [0, 1.1 x the median clean upgrade]: resolve,
#             fetch/unpack, the syncfs before the journal, the switch, the
#             deletion of old versions
#   file      the moment a random file (not the last) of a random upgraded
#             package is replaced in the live tree: the package is half
#             placed (the switch's time is mostly syncfs, its file placing
#             takes a few ms, so "time" alone rarely lands inside it)
#   journal   uniform over the switch (after the journal appears)
#   file+thr  like file, with catalyst throttled from outside during the switch
#             (30 us running, 0.5 ms stopped) and the kill sent while it is
#             stopped: within ~30 us of work after the file was placed
#   double    like file, then the recovering run is killed as well at a
#             uniform moment of its duration, and checked again
#   step+thr  right after the journal got its K-th "done" mark (K uniform),
#             throttled: between two steps (a script and the files placed
#             around it)
#   gone      uniform over the deletion of old versions (after the journal
#             is deleted)
#   script    0-3 ms after the K-th package script was written out to be
#             run (K uniform): the script dies with catalyst, mid-run or before
#             it starts, and must be run again by the recovery
# The switch could not be slowed by an option (catalyst has none, only the abort
# hook CATALYST_CRASH_AT, not used here: an abort goes through WSL's crash
# capture); throttling with SIGSTOP/SIGCONT changes nothing in catalyst.
#
# usage: oct-killtest.sh [ROUNDS]   (default 80)
#   env KT_SEED=N  fixed seed (default: the time)
#   env KT_KEEP=N  keep the first N failed roots as /root/catalyst-test/kt/fail-* (default 5)
#   env KT_OCT=PATH    the catalyst binary (default /root/catalyst-target/release/oct)
#   env KT_DIR=DIR     work directory under /root/catalyst-test (default /root/catalyst-test/kt)
#   env KT_MODES="..." the rotation of kill modes (default: all of the above)
#   env KT_JOB=small   instead of the upgrade: add -u coreutils util-linux shadow gzip nano
#                      (four of the 9999-r0 upgrades with their scripts, gzip's zcmp that
#                      becomes a symlink, and a new package). It leaves the directory
#                      symlinks alone, so the switch reads only the records it needs and
#                      updates the lookup index; the upgrade moves lib64, so it reads every
#                      record and rebuilds the index.
#   env KT_JOB=add     instead of the upgrade: add nano less tree htop tmux jq bc file
#                      (new packages only). The kill modes that need an upgraded file or
#                      a script are left out of the default rotation.
#   env KT_JOB=del     the removal of those eight from a root that has them (the same
#                      rotation as add)
#   env KT_JOB=bigdel  the removal of cataclysm-dda-tiles (39 packages, 8,600 files) with
#                      --clean-later: the old versions go to the trash and a process of
#                      its own deletes them; a kill of catalyst leaves that process alone,
#                      as it would be on a real system
# Exit status 0 when every round passes. Writes only under /root/catalyst-test.
set -u
ROUNDS=${1:-80}
SEED=${KT_SEED:-$(date +%s)}
KEEP=${KT_KEEP:-5}
V=${KT_OCT:-/root/catalyst-target/release/oct}
S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # this folder
R1=/root/valence/repo/x86_64/Packages.adb
R3=/root/catalyst-test/kt-repo/x86_64/Packages.adb
KD=/root/catalyst-test/keys/trusted
KT=${KT_DIR:-/root/catalyst-test/kt}
T=$KT/root
PY="python3 -I"
CHK="$PY $S/catalyst-kt-check.py"
# old versions that a change left to a process of its own (--clean-later): gone before a root is looked at
wait_trash() { local n=0; while [ -e "$1/var/lib/catalyst/trash" ] && [ $n -lt 6000 ]; do sleep 0.005; n=$((n + 1)); done; }
KILL="$PY $S/catalyst-kt-kill.py"

safe_rm() {
    case "$1" in /root/catalyst-test/?*) ;; *) echo "refusing to remove $1"; exit 1;; esac
    if grep -q " $1" /proc/mounts; then echo "MOUNTS under $1, not removing"; exit 1; fi
    [ -e "$1" ] && rm -rf --one-file-system "$1"
    return 0
}
die() { echo "ABORT: $*"; exit 2; }

[ -x $V ] || die "$V not built (catalyst-build.sh)"
if [ ! -f $R3 ]; then
    echo "=== building the 9999-r0 repository"
    tr -d '\r' < $S/catalyst-kt-mkrepo.sh | bash > /root/catalyst-test/kt-mkrepo.log 2>&1 || die "catalyst-kt-mkrepo.sh failed: /root/catalyst-test/kt-mkrepo.log"
fi
mkdir -p $KT
for d in $KT/root $KT/ref $KT/ref2 $KT/ref3 $KT/fail-*; do safe_rm $d; done
rm -f $KT/*.log $KT/*.out $KT/*.json $KT/results.txt

fresh() {  # fresh ROOT: valence-base from the real repository, plus an admin edit
    safe_rm $1
    $V --root $1 --initdb --repository $R1 --keys-dir $KD --plain add valence-base > $1.install.log 2>&1 \
        || die "install into $1 failed: $1.install.log"
    if [ -n "${PREP:-}" ]; then   # what the job removes
        $V --root $1 --repository $R1 --keys-dir $KD --plain add $PREP >> $1.install.log 2>&1 \
            || die "install of $PREP into $1 failed: $1.install.log"
    fi
    echo "# admin was here" >> $1/etc/fstab
    touch -d @1795000000 $1/etc/fstab   # a fixed mtime, so roots compare equal
}
case ${KT_JOB:-upgrade} in
    upgrade) JOB="upgrade";;
    small) JOB="add -u coreutils util-linux shadow gzip nano";;
    add) JOB="add nano less tree htop tmux jq bc file"; : ${KT_MODES:="time journal step+thr step+thr gone step+thr"};;
    del) PREP="nano less tree htop tmux jq bc file"; JOB="del $PREP"; : ${KT_MODES:="time journal step+thr step+thr gone step+thr"};;
    bigdel) PREP="cataclysm-dda-tiles"; JOB="--clean-later del $PREP"; : ${KT_MODES:="time journal step+thr gone time gone"};;
    *) die "KT_JOB is upgrade, small, add, del or bigdel";;
esac
upg() { echo "$V --root $1 --repository $R1 --repository $R3 --keys-dir $KD --plain $JOB"; }
calc() { awk "BEGIN{printf \"%.4f\", $*}"; }
kv() { echo "$1" | tr ' ' '\n' | sed -n "s/^$2=//p"; }
rnd() { $PY -c "import random,sys; random.seed(sys.argv[1]); print('%.4f' % random.uniform(0, float(sys.argv[2])))" "$SEED-$1" "$2"; }
rndi() { $PY -c "import random,sys; random.seed(sys.argv[1]); print(random.randint(0, int(sys.argv[2])))" "$SEED-$1" "$2"; }

echo "=== catalyst kill test: $ROUNDS rounds of '$JOB', seed $SEED, $(date)"
echo "    catalyst binary of $(ls -l --time-style=+%F.%T $V | awk '{print $6}'), $(nproc) CPUs"

# ------------------------------------------------------------ reference
echo "=== clean upgrades (reference and timing)"
TOT=(); SW=(); GONE=(); MM=(); SC=()
for m in 1 2 3; do
    r=$KT/ref; [ $m -gt 1 ] && r=$KT/ref$m
    fresh $r
    [ $m -eq 1 ] && { wait_trash $r; $CHK snap $r $KT/pre-ref.json; }
    res=$($KILL measure - $r $r.upgrade.log -- $(upg $r))
    [ "$(kv "$res" rc)" = 0 ] || die "clean upgrade $m failed: $r.upgrade.log"
    t=$(kv "$res" total); j0=$(kv "$res" tj0); j1=$(kv "$res" tj1)
    TOT+=($t); SW+=($(calc "$j1 - $j0")); GONE+=($(calc "$t - $j1")); MM+=($(kv "$res" marks_max)); SC+=($(kv "$res" scripts))
    echo "  clean upgrade $m: total ${t}s, journal present ${j0}s .. ${j1}s, $(kv "$res" marks_max) steps marked, $(kv "$res" scripts) scripts run;"
    echo "    $(grep '^OK:' $r.upgrade.log)"
done
grep -E '^  (upgrade|install|remove) ' $KT/ref.upgrade.log | awk '{print $1}' | sort | uniq -c | sed 's/^/  /'
wait_trash $KT/ref; $CHK snap $KT/ref $KT/ref.json
echo "--- the reference root itself"
$CHK crit $KT/ref | sed 's/^/  /'
$CHK crash $KT/ref $KT/pre-ref.json $KT/ref.json > $KT/ref-check.out 2>&1 || { cat $KT/ref-check.out; die "the reference root fails the checks"; }
grep 'paths checked' $KT/ref-check.out
echo "--- a second clean upgrade equals the reference (the comparison is deterministic)"
wait_trash $KT/ref2; $CHK final $KT/ref2 $KT/ref.json > $KT/ref2-check.out 2>&1 || { cat $KT/ref2-check.out; die "two clean upgrades differ: the comparison is not usable"; }
sed 's/^/  /' $KT/ref2-check.out
safe_rm $KT/ref2; safe_rm $KT/ref3
med() { printf '%s\n' "$@" | sort -g | sed -n 2p; }   # median of 3
TMED=$(med "${TOT[@]}"); SWMED=$(med "${SW[@]}"); GMED=$(med "${GONE[@]}")
MMAX=$(printf '%s\n' "${MM[@]}" | sort -g | tail -1)
SMAX=$(printf '%s\n' "${SC[@]}" | sort -g | head -1)
TW=$(calc "$TMED * 1.1"); SWW=$(calc "$SWMED * 1.1"); GW=$(calc "$GMED * 1.2")
echo "  kill windows: time [0, $TW]s; journal [0, $SWW]s after it appears; step K in [1, $MMAX]; script K in [1, $SMAX] + [0, 0.003]s;" \
     "gone [0, $GW]s after the journal is deleted"

echo "--- checker self-test: damage in a root must be caught"
fresh $T
wait_trash $T; $CHK snap $T $KT/pre.json
cp -p $T/usr/bin/ls $T/usr/bin/.ls.x && mv $T/usr/bin/.ls.x $T/usr/bin/ls   # same content, another inode
: > $T/usr/bin/bash                                                          # truncated
rm $T/usr/lib/libc.so.6                                                      # missing
echo stray > $T/usr/bin/stray                                                # unexpected
echo "# neither old nor new" >> $T/etc/hosts                                 # etc/ content
ln -sfn nowhere $T/usr/bin/sh                                                # symlink target
$CHK crash $T $KT/pre.json $KT/ref.json > $KT/selftest.out 2>&1 && die "checker self-test: damage not caught"
for w in 'neither old nor new: /usr/bin/ls$' 'neither old nor new: /usr/bin/bash$' 'missing: /usr/lib/libc.so.6' \
         'unexpected new entry: /usr/bin/stray' 'neither old nor new: /etc/hosts$' 'neither old nor new: /usr/bin/sh$' \
         'critical: /usr/lib/libc.so.6' 'critical: chroot /bin/sh -c echo ok failed'; do
    grep -q "$w" $KT/selftest.out || { cat $KT/selftest.out; die "checker self-test: '$w' not reported"; }
done
wait_trash $T; $CHK final $T $KT/ref.json > /dev/null 2>&1 && die "checker self-test: final check passed a root that was not upgraded"
echo "  ok: $(grep -c VIOLATION $KT/selftest.out) violations reported for 6 kinds of damage, chroot failures seen; final check rejects it"
safe_rm $T
T_START=$(date +%s)

# ------------------------------------------------------------ rounds
MODES=(${KT_MODES:-time file journal file+thr double step+thr gone script file})
FAILS=0; NKILLED=0; KEPT=0; NHALF=0; NFILE=0; NFILEK=0
printf '%3s %-8s %-14s %-44s %-46s %s\n' "#" mode "killed in" "when (kill helper mode and parameter)" "state at the kill (journal; Current links)" "half-placed packages | scripts re-run | result" | tee $KT/results.txt
for i in $(seq 1 $ROUNDS); do
    mode=${MODES[$(( (i - 1) % ${#MODES[@]} ))]}
    case $mode in
        time) km=time; p=$(rnd $i $TW);;
        journal) km=journal; p=$(rnd $i $SWW);;
        # a round that watches no file would kill nothing and pass: no file is an error of the test itself
        file|double) km=file; pf="$($CHK pickfile $KT/ref.json $SEED-$i 9999-r0)"; [ -n "$pf" ] || { echo "ABORT: pickfile gave no file (round $i)"; exit 2; }; p="$pf/0";;
        file+thr) km=file+throttle; pf="$($CHK pickfile $KT/ref.json $SEED-$i 9999-r0)"; [ -n "$pf" ] || { echo "ABORT: pickfile gave no file (round $i)"; exit 2; }; p="$pf/0";;
        step+thr) km=step+throttle; p="$(( $(rndi $i $(( MMAX - 1 ))) + 1 ))/0";;
        gone) km=gone; p=$(rnd $i $GW);;
        script) km=script; p="$(( $(rndi $i $(( SMAX - 1 ))) + 1 ))/$(rnd $i-d 0.003)";;
    esac
    L=$KT/round.log; : > $L
    fresh $T
    wait_trash $T; $CHK snap $T $KT/pre.json
    res=$($KILL $km $p $T $KT/run1.log -- $(upg $T))
    killed=$(kv "$res" killed)
    [ "$killed" = 1 ] && NKILLED=$((NKILLED + 1))
    # the rounds that watch a file of the change: the ones meant to leave a package half placed
    case $km in file*) NFILE=$((NFILE + 1)); [ "$killed" = 1 ] && NFILEK=$((NFILEK + 1));; esac
    fail=""
    { echo "=== round $i: mode $mode ($km $p): $res"; sed 's/^/    | /' $KT/run1.log | tail -4; } >> $L
    ph="completed"; half="-"
    if [ "$killed" = 1 ]; then
        $CHK crash $T $KT/pre.json $KT/ref.json > $KT/crash1.out 2>&1 || fail="$fail crash-check"
        cat $KT/crash1.out >> $L
        ph=$(sed -n 's/^  stopped at: //p' $KT/crash1.out)
        half=$(sed -n 's/^  packages wholly new [0-9]*, half placed: //p' $KT/crash1.out)
        [ "$half" != none ] && NHALF=$((NHALF + 1))
    fi
    if [ $mode = double ] && [ "$killed" = 1 ]; then
        d2=$(rnd $i-2 $(calc "$SWW + 0.05"))
        res2=$($KILL time $d2 $T $KT/run2.log -- $(upg $T))
        echo "--- round $i: the recovering run, killed after ${d2}s: $res2" >> $L
        sed 's/^/    | /' $KT/run2.log | tail -4 >> $L
        if [ "$(kv "$res2" killed)" = 1 ]; then
            $CHK crash $T $KT/pre.json $KT/ref.json > $KT/crash2.out 2>&1 || fail="$fail crash-check-2"
            cat $KT/crash2.out >> $L
            half="$half; recovery killed at ${d2}s: $(sed -n 's/^  stopped at: journal=\([01]\)(done \([0-9]*\)).*/journal \1, \2 marks/p' $KT/crash2.out)"
        else
            half="$half; recovery not killed"
        fi
    fi
    # the next run completes it
    j=$( [ -e $T/var/lib/catalyst/journal ] && echo 1 || echo 0 )
    $(upg $T) > $KT/complete.log 2>&1; rc=$?
    { echo "--- completing run: exit $rc (journal before: $j)"; sed 's/^/    | /' $KT/complete.log | grep -v '^    |   \(upgrade\|install\|remove\) ' | tail -8; } >> $L
    [ $rc = 0 ] || fail="$fail complete-exit-$rc"
    if [ $j = 1 ] && ! grep -q 'finishing an interrupted switch' $KT/complete.log; then fail="$fail no-recovery"; fi
    $(upg $T) > $KT/again.log 2>&1; rc2=$?
    grep -q 'nothing to do' $KT/again.log || { fail="$fail not-done"; sed 's/^/    | /' $KT/again.log >> $L; }
    [ $rc2 = 0 ] || fail="$fail again-exit-$rc2"
    wait_trash $T; $CHK final $T $KT/ref.json > $KT/final.out 2>&1 || fail="$fail final-differs"
    cat $KT/final.out >> $L
    # (the log of the 9999-r0 packages' scripts; a job without them has none)
    reruns=$(( $(cat $T/var/log/catalyst-kt.log 2>/dev/null | wc -l) - $(cat $KT/ref/var/log/catalyst-kt.log 2>/dev/null | wc -l) ))
    case "$ph" in
        completed) cls=-;;
        journal=1*) cls=switch;;
        *"current: 0 new"*) cls=before-journal;;
        *) cls=after-journal;;
    esac
    when=$( [ "$killed" = 1 ] && echo "$(kv "$res" at)s, $km $p" || echo "finished ($km $p)" )
    printf '%3d %-8s %-14s %-44s %-46s %s | %d | %s\n' $i $mode $cls "$when" \
        "$(echo "$ph" | sed 's/; [0-9]* with two versions//; s/; 0 .partial//; s/current: //')" "$half" $reruns \
        "$( [ -z "$fail" ] && echo PASS || echo "FAIL:$fail" )" | tee -a $KT/results.txt
    cat $L >> $KT/all-rounds.log
    if [ -n "$fail" ]; then
        FAILS=$((FAILS + 1))
        cp $L $KT/fail-$i.log
        if [ $KEPT -lt $KEEP ]; then
            safe_rm $KT/fail-$i; mv $T $KT/fail-$i; KEPT=$((KEPT + 1))
            echo "    root kept as $KT/fail-$i, log $KT/fail-$i.log"
        fi
    fi
    rm -f $KT/crash1.out $KT/crash2.out
done
safe_rm $T
echo "=== $ROUNDS rounds in $(( $(date +%s) - T_START ))s: $NKILLED killed while running, $NHALF of them with a package half placed; $FAILS failed (seed $SEED)"
echo "    killed in: $(tail -n +2 $KT/results.txt | awk '{print $3}' | sort | uniq -c | awk '{printf "%s %s, ", $1, ($2 == "-" ? "(not killed)" : $2)}')"
echo "    results in $KT/results.txt, every round's log in $KT/all-rounds.log"
grep -q catalyst-test /proc/mounts && echo "WARNING: mounts under /root/catalyst-test"
[ $FAILS = 0 ] || exit 1
# A kill can come too late in one round. When none of the rounds that watch a file killed anything, the test
# has not done what it is for (as when the checker's pickfile failed and every such round passed unkilled).
if [ $NFILE -gt 0 ] && [ $NFILEK = 0 ]; then
    echo "INCONCLUSIVE: none of the $NFILE rounds that watch a file killed catalyst"
    exit 3
fi
exit 0
