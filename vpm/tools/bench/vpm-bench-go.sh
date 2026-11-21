S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # this folder
tr -d '\r' < $S/vpm-bench-run.sh > /root/vpm-test/bench/run.sh
CLEAN=1 bash /root/vpm-test/bench/run.sh full.txt --runs 5 --stages 'disk base big jobs prof' < /dev/null > /root/vpm-test/bench/full-wrapper.txt 2>&1
echo done
