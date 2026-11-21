#!/bin/bash
S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # this folder
B=/root/vpm-test/bench
mkdir -p $B/bin
# snapshot of the binary under test (other work may rebuild the original)
cp /root/vpm-target/release/vpm $B/bin/vpm
(cd $S/vpm && git -c safe.directory='*' rev-parse --short HEAD && git -c safe.directory='*' status --short) > $B/bin/vpm.rev
sha256sum $B/bin/vpm >> $B/bin/vpm.rev
cat $B/bin/vpm.rev
cd $B
rm -f $B/pick/rejected.txt
export VPM=$B/bin/vpm
time python3 -I $S/vpm-bench-pick.py $B/index.json $B/pick 30 > $B/big-set.txt 2> $B/pick.log
echo rc=$?
grep -v '^ *(' $B/pick.log | tail -70
echo ---
cat $B/big-set.txt | tr '\n' ' '; echo
cat $B/pick/rejected.txt
grep vpm-test /proc/mounts
