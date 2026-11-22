S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # this folder
tr -d '\r' < $S/octet-bench-run.sh > /root/octet-test/bench/run.sh
CLEAN=1 bash /root/octet-test/bench/run.sh full.txt --runs 5 --stages 'disk base big jobs prof' < /dev/null > /root/octet-test/bench/full-wrapper.txt 2>&1
echo done
