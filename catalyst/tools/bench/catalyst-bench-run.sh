#!/bin/bash
# wrapper: catalyst-bench-run.sh OUTNAME [catalyst-bench.sh args]; set CLEAN=1 to start with empty results
S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # this folder
B=/root/catalyst-test/bench
out=$1; shift
if [ "${CLEAN:-0}" = 1 ]; then rm -f $B/results.tsv $B/sizes.tsv $B/summary.md $B/prof-*.txt; rm -rf --one-file-system $B/logs; fi
tr -d '\r' < $S/catalyst-bench.sh > $B/catalyst-bench.sh
bash $B/catalyst-bench.sh "$@" > $B/$out 2>&1
echo "rc=$?"
cat $B/$out
