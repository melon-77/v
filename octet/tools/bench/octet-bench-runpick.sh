#!/bin/bash
S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # this folder
B=/root/octet-test/bench
mkdir -p $B/bin
# snapshot of the binary under test (other work may rebuild the original)
cp /root/octet-target/release/oct $B/bin/oct
(cd $S/octet && git -c safe.directory='*' rev-parse --short HEAD && git -c safe.directory='*' status --short) > $B/bin/oct.rev
sha256sum $B/bin/oct >> $B/bin/oct.rev
cat $B/bin/oct.rev
cd $B
rm -f $B/pick/rejected.txt
export OCT=$B/bin/oct
time python3 -I $S/octet-bench-pick.py $B/index.json $B/pick 30 > $B/big-set.txt 2> $B/pick.log
echo rc=$?
grep -v '^ *(' $B/pick.log | tail -70
echo ---
cat $B/big-set.txt | tr '\n' ' '; echo
cat $B/pick/rejected.txt
grep octet-test /proc/mounts
