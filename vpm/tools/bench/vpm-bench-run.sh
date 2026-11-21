#!/bin/bash
# wrapper: vpm-bench-run.sh OUTNAME [vpm-bench.sh args]; set CLEAN=1 to start with empty results
S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # this folder
B=/root/vpm-test/bench
out=$1; shift
if [ "${CLEAN:-0}" = 1 ]; then rm -f $B/results.tsv $B/sizes.tsv $B/summary.md $B/prof-*.txt; rm -rf --one-file-system $B/logs; fi
tr -d '\r' < $S/vpm-bench.sh > $B/vpm-bench.sh
bash $B/vpm-bench.sh "$@" > $B/$out 2>&1
echo "rc=$?"
cat $B/$out
