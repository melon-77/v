
set -u
B=/root/vpm-test/upb; R=$B/root
V=/root/vpm-test/bench2/flat/bin/vpm; VS=$B/vpm-stripped
R1=/root/valence/repo/x86_64/Packages.adb; KD=/root/vpm-test/keys/trusted
case $R in /root/vpm-test/?*) ;; *) exit 1;; esac
strip -o $VS $V; echo "unstripped $(stat -c %s $V) bytes, stripped $(stat -c %s $VS) bytes"
for bin in $V $VS; do
  for c in cold warm; do
    ts=""
    for i in 1 2 3 4 5 6; do
      rm -rf --one-file-system $R; mkdir -p $R
      $bin --root $R --initdb --repository $R1 --keys-dir $KD --no-scripts --plain add valence-base >/dev/null 2>&1; sync
      [ $c = cold ] && echo 3 > /proc/sys/vm/drop_caches
      t0=$(date +%s%N); $bin --root $R --repository $R1 --keys-dir $KD --no-scripts --plain add nano > $B/strip.log 2>&1; t1=$(date +%s%N)
      [ $i = 1 ] && [ $c = warm ] && continue    # warm-up
      ts="$ts $(awk -v a=$t0 -v b=$t1 'BEGIN{printf "%.3f",(b-a)/1e9}')"
    done
    echo "$(basename $bin) $c add nano: $ts   (vpm's own: $(grep '^OK:' $B/strip.log | sed 's/.*total/total/'))"
  done
done
rm -rf --one-file-system $R
