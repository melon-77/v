#!/bin/bash
# apk against catalyst, the strict comparison, installs: RUNS rounds per tool, job, system and cache state,
# alternating in ABBA order after one untimed round; every run is kept. catalyst-strict-stats.py gives the
# verdicts. Removals, upgrades and an HTTP repository are in catalyst-strict2.sh.
#
# usage: [LABEL=name] [RUNS=15] [JOBS="nano cataclysm-dda-tiles"] [SETS="base big"] [CACHES="warm cold"]
#        [APKSYNC=--sync] [OCTSYNC=--sync] [BDIR=dir] [STRICT_OUT=dir] catalyst-strict.sh
#   APKSYNC=--sync  apk syncs at the end of the change, as it does on an installed system (with --root it
#                   would not): the comparison in the README is run this way
#   OCTSYNC=--sync  catalyst too returns only when the change is on disk
#   BDIR            where the roots are made (default /root/catalyst-test/benchab): a directory on the file
#                   system to measure, see catalyst-strict-fs.sh
#   STRICT_OUT      where the results go (default /root/catalyst-test/strict)
# Needs root (it drops the page cache for the cold runs), the built repository, the trusted keys in
# /root/catalyst-test/keys/trusted and the list of the big system's packages in
# /root/catalyst-test/bench2/big-set.txt (catalyst-bench-pick.py writes one).
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
V=${VALENCE:-/root/valence}
APK=$V/hosttools/bin/apk
OCT=${OCT:-$V/catalyst/target/release/oct}
R1=$V/repo/x86_64/Packages.adb
KD=/root/catalyst-test/keys/trusted
S=${STRICT_OUT:-/root/catalyst-test/strict}; mkdir -p "$S"
LABEL=${LABEL:-run}; RUNS=${RUNS:-15}
JOBS=${JOBS:-nano cataclysm-dda-tiles}; SETS=${SETS:-base big}; CACHES=${CACHES:-warm cold}
B=${BDIR:-/root/catalyst-test/benchab}; case $B in /root/catalyst-test/?*) ;; *) echo "BDIR must be under /root/catalyst-test"; exit 1;; esac
grep -q " $B " /proc/mounts && { echo "$B is a mount point"; exit 1; }
rm -rf --one-file-system $B; mkdir -p $B
BIG="valence-base $(tr '\n' ' ' < /root/catalyst-test/bench2/big-set.txt)"
run(){ local t=$1 r=$2; shift 2
  case $t in
    apk) $APK --root $r --arch x86_64 --keys-dir $KD --repositories-file /dev/null --repository $R1 --no-cache --no-scripts ${APKSYNC:-} "$@";;
    cat) $OCT --root $r --repository $R1 --keys-dir $KD --no-scripts --plain ${OCTSYNC:-} "$@";;
  esac; }
has(){ if [ $1 = apk ]; then grep -qx "P:$3" $2/lib/apk/db/installed; else [ -e $2/Programs/$3/Current ]; fi; }
OUT=$S/benchab-$LABEL.tsv
printf 'set\tjob\tcache\ttool\trun\twall\twall_sync\n' > $OUT
echo "benchab $LABEL: $RUNS runs; catalyst $(stat -c %y $OCT | cut -c1-19); $(date -u +%FT%TZ); load $(cut -d' ' -f1-3 /proc/loadavg); $(stat -f -c %T $B)"
for set in $SETS; do
  names=valence-base; [ $set = big ] && names=$BIG
  for t in apk cat; do R=$B/$set-$t; mkdir -p $R; run $t $R --initdb add $names > $B/prep.log 2>&1 || { echo "prep $set $t failed"; tail -n 3 $B/prep.log; exit 1; }; done
  for job in $JOBS; do
    for c in $CACHES; do
      # one untimed round first, then RUNS rounds in ABBA order
      for i in $(seq 0 $RUNS); do
        if [ $(( i % 2 )) = 0 ]; then order="apk cat"; else order="cat apk"; fi
        for t in $order; do
          R=$B/$set-$t
          sync; [ $c = cold ] && echo 3 > /proc/sys/vm/drop_caches
          t0=$(date +%s%N); run $t $R add $job > $B/op.log 2>&1; rc=$?; t1=$(date +%s%N); sync -f $R; t2=$(date +%s%N)
          if [ $rc != 0 ] || ! has $t $R $job; then echo "  NOT OK: $set $job $c $t $i (rc $rc)"; tail -n 2 $B/op.log; fi
          [ $i -gt 0 ] && printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' $set $job $c $t $i \
            $(awk -v a=$t0 -v b=$t1 'BEGIN{printf "%.4f",(b-a)/1e9}') $(awk -v a=$t0 -v b=$t2 'BEGIN{printf "%.4f",(b-a)/1e9}') >> $OUT
          run $t $R del $job > $B/undo.log 2>&1 || { echo "  undo failed: $set $t"; tail -n 2 $B/undo.log; }
        done
      done
    done
  done
done
rm -rf --one-file-system $B
python3 -I $HERE/catalyst-strict-stats.py $OUT
