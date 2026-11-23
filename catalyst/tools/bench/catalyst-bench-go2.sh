S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # this folder
B=/root/catalyst-test/bench
tr -d '\r' < $S/catalyst-bench-buildprof.sh | bash || exit 1
tr -d '\r' < $S/catalyst-bench-exp.sh > $B/exp.sh
echo "load before: $(cat /proc/loadavg)"
NOZSTD=1 OUT=$B/exp2.tsv VARIANTS="baseline flusher direct-link flusher+direct apk zstd+flusher+direct apk-zstd" RUNS=5 \
    bash $B/exp.sh < /dev/null > $B/exp2-out.txt 2>&1
echo "rc=$?; load after: $(cat /proc/loadavg)"
tail -18 $B/exp2-out.txt
