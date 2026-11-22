
# Upgrade and single-package benchmark, 2026-11-22 (owner: "yes measure those too"): apk against vpm fc3ba9f / 447bef7 / 717fa8c.
# Each timed operation runs on a freshly installed valence-base root (the install is not timed), --no-scripts.
set -u
APK=/root/valence/hosttools/bin/apk
R1=/root/valence/repo/x86_64/Packages.adb
R3=/root/vpm-test/kt-repo/x86_64/Packages.adb      # 27 base packages as 9999-r0 (the kill test's upgrade)
KD=/root/vpm-test/keys/trusted
declare -A VPM=([vpm-flat]=/root/vpm-test/bench2/flat/bin/vpm [vpm-sqlite]=/root/vpm-test/bench2/sqlite/bin/vpm [vpm-hard]=/root/vpm-test/bench2/hard/bin/vpm)
TOOLS=(apk vpm-flat vpm-sqlite vpm-hard)
B=/root/vpm-test/upb; R=$B/root
RUNS=${RUNS:-5}
safe_rm(){ case "$1" in /root/vpm-test/?*) ;; *) echo "refusing $1"; exit 1;; esac
  grep -q " $1" /proc/mounts && { echo "mounts under $1"; exit 1; }; [ -e "$1" ] && rm -rf --one-file-system "$1"; return 0; }
safe_rm $B; mkdir -p $B/logs
RES=$B/results.tsv; printf 'work\tcache\ttool\trun\trc\tok\twall\twall_sync\tuser\tsys\tmaxrss_kb\n' > $RES

prep(){  # prep TOOL: a fresh valence-base root at $R
  safe_rm $R; mkdir -p $R
  if [ $1 = apk ]; then $APK --root $R --initdb --arch x86_64 --keys-dir $KD --repositories-file /dev/null --repository $R1 --no-cache --no-scripts add valence-base
  else ${VPM[$1]} --root $R --initdb --repository $R1 --keys-dir $KD --no-scripts --plain add valence-base; fi > $B/logs/prep.log 2>&1 || { echo "prep $1 failed"; exit 1; }
}
cmd(){  # cmd TOOL WORK -> CMD array
  local t=$1 w=$2
  if [ $t = apk ]; then
    local a=($APK --root $R --arch x86_64 --keys-dir $KD --repositories-file /dev/null --repository $R1 --repository $R3 --no-cache --no-scripts)
    case $w in up-all) CMD=("${a[@]}" upgrade);; up-one) CMD=("${a[@]}" upgrade coreutils);;
               add-small) CMD=("${a[@]}" add nano);; add-large) CMD=("${a[@]}" add go);; esac
  else
    local v=(${VPM[$t]} --root $R --repository $R1 --repository $R3 --keys-dir $KD --no-scripts --plain)
    case $w in up-all) CMD=("${v[@]}" upgrade);; up-one) CMD=("${v[@]}" add -u coreutils);;
               add-small) CMD=("${v[@]}" add nano);; add-large) CMD=("${v[@]}" add go);; esac
  fi
}
check(){  # check TOOL WORK: did the operation do what it should?
  local t=$1 w=$2 n
  if [ $t = apk ]; then
    case $w in up-all) n=$(grep -c '^V:9999-r0' $R/lib/apk/db/installed); [ "$n" = 27 ];;
               up-one) grep -A1 -x 'P:coreutils' $R/lib/apk/db/installed | grep -q '^V:9999-r0';;
               add-small) grep -q -x 'P:nano' $R/lib/apk/db/installed;; add-large) grep -q -x 'P:go' $R/lib/apk/db/installed;; esac
  else
    case $w in up-all) n=$(ls -l $R/Programs/*/Current | grep -c -- '-> 9999-r0'); [ "$n" = 27 ];;
               up-one) [ "$(readlink $R/Programs/coreutils/Current)" = 9999-r0 ];;
               add-small) [ -e $R/Programs/nano/Current ];; add-large) [ -e $R/Programs/go/Current ];; esac
  fi
}
one(){  # one WORK CACHE TOOL RUN [record]
  local w=$1 c=$2 t=$3 i=$4 rec=${5:-1}
  prep $t; cmd $t $w
  sync; [ $c = cold ] && echo 3 > /proc/sys/vm/drop_caches
  local load; load=$(cut -d' ' -f1 /proc/loadavg)
  local t0 t1 t2 rc
  t0=$(date +%s%N)
  /usr/bin/time -f '%U %S %M' -o $B/time.out "${CMD[@]}" > $B/logs/$w-$c-$t-$i.log 2>&1; rc=$?
  t1=$(date +%s%N); sync -f $R; t2=$(date +%s%N)
  local ok=0; check $t $w && ok=1
  [ $rec = 1 ] && printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' $w $c $t $i $rc $ok \
      $(awk -v a=$t0 -v b=$t1 'BEGIN{printf "%.4f",(b-a)/1e9}') $(awk -v a=$t0 -v b=$t2 'BEGIN{printf "%.4f",(b-a)/1e9}') \
      "$(tr ' ' '\t' < $B/time.out)" >> $RES
  [ $ok = 1 ] || echo "  NOT OK: $w $c $t run $i (rc $rc): $B/logs/$w-$c-$t-$i.log"
}
echo "upgrade/single benchmark $(date -u -Is); $(nproc) CPUs; load $(cut -d' ' -f1-3 /proc/loadavg)"
for w in up-all up-one add-small add-large; do
  for c in cold warm; do
    echo "== $(date -u +%T) $w $c"
    [ $c = warm ] && for t in "${TOOLS[@]}"; do one $w warm $t 0 0; done        # unrecorded warm-up per tool
    for i in $(seq 1 $RUNS); do
      for k in 0 1 2 3; do t=${TOOLS[$(( (i + k) % 4 ))]}; one $w $c $t $i; done   # rotate who goes first
    done
  done
done
safe_rm $R
echo "== $(date -u +%T) done"
python3 -I - $RES <<'EOF'
import csv, statistics as st, sys
rows = list(csv.DictReader(open(sys.argv[1]), delimiter='\t'))
works = ['up-all', 'up-one', 'add-small', 'add-large']; tools = ['apk', 'vpm-flat', 'vpm-sqlite', 'vpm-hard']
print('| work | cache | tool | ok | wall median (min-max) | wall+sync median | CPU user+sys | max RSS | vs apk (wall / +sync) |')
print('|---|---|---|---|---|---|---|---|---|')
for w in works:
    for c in ['cold', 'warm']:
        med = {}
        for t in tools:
            r = [x for x in rows if x['work'] == w and x['cache'] == c and x['tool'] == t]
            if not r: continue
            wall = [float(x['wall']) for x in r]; ws = [float(x['wall_sync']) for x in r]
            cpu = st.median([float(x['user']) + float(x['sys']) for x in r]); rss = st.median([int(x['maxrss_kb']) for x in r]) / 1024
            ok = sum(int(x['ok']) for x in r)
            med[t] = (st.median(wall), st.median(ws))
            rel = '' if t == 'apk' else '%.2f / %.2f' % (med[t][0] / med['apk'][0], med[t][1] / med['apk'][1])
            print(f'| {w} | {c} | {t} | {ok}/{len(r)} | {st.median(wall):.3f}s ({min(wall):.3f}-{max(wall):.3f}) | {st.median(ws):.3f}s | {cpu:.2f}s | {rss:.0f} MB | {rel} |')
EOF
