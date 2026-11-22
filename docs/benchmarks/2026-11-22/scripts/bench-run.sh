
# Benchmark pass 2026-11-22 (owner: "test with xbps"): vpm fc3ba9f / 447bef7 / 717fa8c against apk, and xbps.
set -u
W=/root/vpm-test/bench2
TOOLS=$W/tools
FLAT=/root/valence/vpm/target/release/vpm      # fc3ba9f (vpm/ unchanged up to testing 05ddfce)
SQL=/root/pr1/vpm/target/release/vpm           # 447bef7
HARD=/root/pr1h/vpm/target/release/vpm         # 717fa8c
case $W in /root/vpm-test/?*) ;; *) exit 1;; esac
if grep -q " $W" /proc/mounts; then echo "mounted under $W"; exit 1; fi
rm -rf --one-file-system $W; mkdir -p $W
cp -a /root/pr1h/vpm/tools/bench $TOOLS
for v in flat sqlite hard; do
  sed "s|^B=/root/vpm-test/bench\$|B=$W/$v|" $TOOLS/vpm-bench.sh > $TOOLS/vpm-bench-$v.sh
  grep -q "^B=$W/$v\$" $TOOLS/vpm-bench-$v.sh || { echo "could not set B for $v"; exit 1; }
  mkdir -p $W/$v/bin
done
echo "== $(date -u +%T) index and big set"
$FLAT --keys-dir /root/valence/keys/trusted dump-index /root/valence/repo/x86_64/Packages.adb --json > $W/index.json
VPM=$FLAT python3 -I $TOOLS/vpm-bench-pick.py $W/index.json $W/pick 30 > $W/big-set.txt 2> $W/pick.log
echo "big set: $(wc -l < $W/big-set.txt) packages: $(tr '\n' ' ' < $W/big-set.txt)"
for v in flat sqlite hard; do cp $W/big-set.txt $W/$v/; done
cp $FLAT $W/flat/bin/vpm; cp $SQL $W/sqlite/bin/vpm; cp $HARD $W/hard/bin/vpm
echo "flat fc3ba9f" > $W/flat/bin/vpm.rev; echo "sqlite 447bef7" > $W/sqlite/bin/vpm.rev; echo "hard 717fa8c" > $W/hard/bin/vpm.rev
for v in flat sqlite hard; do
  st="base big"; x=""; [ $v = flat ] && { st="disk base big"; x=--xbps; }
  echo "== $(date -u +%T) $v: stages '$st' $x (load $(cut -d' ' -f1-3 /proc/loadavg))"
  VPM=$W/$v/bin/vpm bash $TOOLS/vpm-bench-$v.sh --runs 5 --stages "$st" $x > $W/$v.out 2>&1; echo "   exit $?"
  grep -E '^== xbps|sha256|packages,|download|checksum' $W/$v.out | head -n 8
done
echo "== $(date -u +%T) done"
for v in flat sqlite hard; do echo "######## $v"; cat $W/$v/summary.md 2>/dev/null | head -n 60; done
