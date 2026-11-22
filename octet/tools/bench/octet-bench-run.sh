#!/bin/bash
# wrapper: octet-bench-run.sh OUTNAME [octet-bench.sh args]; set CLEAN=1 to start with empty results
S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # this folder
B=/root/octet-test/bench
out=$1; shift
if [ "${CLEAN:-0}" = 1 ]; then rm -f $B/results.tsv $B/sizes.tsv $B/summary.md $B/prof-*.txt; rm -rf --one-file-system $B/logs; fi
tr -d '\r' < $S/octet-bench.sh > $B/octet-bench.sh
bash $B/octet-bench.sh "$@" > $B/$out 2>&1
echo "rc=$?"
cat $B/$out
