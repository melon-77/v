#!/bin/bash
# runs vpm-bench.sh (stages disk, base, big; 5 runs) with the current vpm
# build, results in /root/vpm-test/bench-fx (the earlier results in
# /root/vpm-test/bench are kept); then a short before/after comparison of
# the vpm binaries (the reviewed commit 9a02b8f in /root/vpm-test/bench/bin/vpm
# against the current build), warm, --no-scripts, alternating, 5 runs each
set -u
S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # this folder
B=/root/vpm-test/bench-fx
NEW=/root/vpm-target/release/vpm
OLD=/root/vpm-test/bench/bin/vpm
REPO=/root/valence/repo/x86_64/Packages.adb
KEYS=/root/valence/keys/trusted
mkdir -p $B
cp /root/vpm-test/bench/big-set.txt $B/
tr -d '\r' < $S/vpm-bench.sh | sed 's#^B=/root/vpm-test/bench$#B='$B'#' > $B/vpm-bench.sh
grep -q "^B=$B\$" $B/vpm-bench.sh || { echo "could not set B"; exit 1; }
echo "load before: $(cat /proc/loadavg)"
VPM=$NEW bash $B/vpm-bench.sh --runs 5 --stages "disk base big" > $B/full.txt 2>&1
echo "bench exit $?"
cat $B/summary.md

echo
echo "=== before/after: vpm 9a02b8f vs current, warm, --no-scripts, 5 runs each"
T=$B/root
safe_rm() {
    case "$1" in /root/vpm-test/?*) ;; *) echo "refusing $1"; exit 1;; esac
    grep -q " $1" /proc/mounts && { echo "MOUNTED $1"; exit 1; }
    [ -e "$1" ] && rm -rf --one-file-system "$1"
    return 0
}
cat /root/valence/repo/x86_64/*.apk > /dev/null
BIG="valence-base $(tr '\n' ' ' < $B/big-set.txt)"
for set in base big; do
    names=valence-base; [ $set = big ] && names=$BIG
    for v in $OLD $NEW; do safe_rm $T; $v --root $T --initdb --repository $REPO --keys-dir $KEYS --plain --no-scripts add $names > /dev/null 2>&1; done
    for i in 1 2 3 4 5; do
        for v in $OLD $NEW; do
            safe_rm $T; sync
            t0=$(date +%s%N)
            $v --root $T --initdb --repository $REPO --keys-dir $KEYS --plain --no-scripts add $names > $B/ba.log 2>&1
            rc=$?; t1=$(date +%s%N)
            tag=new; [ $v = $OLD ] && tag=old
            printf '%s\t%s\t%s\t%s\t%s\n' $set $tag $i $rc $(awk -v a=$t0 -v b=$t1 'BEGIN{printf "%.3f", (b-a)/1e9}') >> $B/before-after.tsv
            grep -o 'fetch+unpack.*' $B/ba.log | sed "s/^/  $set $tag $i: /"
        done
    done
done
safe_rm $T
python3 -I - $B/before-after.tsv <<'EOF'
import sys, statistics as st
rows = [l.split('\t') for l in open(sys.argv[1]).read().split('\n') if l]
for s in ('base', 'big'):
    out = []
    for t in ('old', 'new'):
        v = sorted(float(r[4]) for r in rows if r[0] == s and r[1] == t and r[3] == '0')
        out.append('%s median %.3f s (%.3f-%.3f, n=%d)' % (t, st.median(v), v[0], v[-1], len(v)))
    print(s + ': ' + '; '.join(out))
EOF
echo "mounts: $(grep -c vpm-test /proc/mounts)"
