S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # this folder
tr -d '\r' < $S/catalyst-bench-run.sh > /root/catalyst-test/bench/run.sh
CLEAN=1 bash /root/catalyst-test/bench/run.sh full.txt --runs 5 --stages 'disk base big jobs prof' < /dev/null > /root/catalyst-test/bench/full-wrapper.txt 2>&1
echo done
