#!/bin/bash
# apk against catalyst, the strict comparison, what catalyst-strict.sh does not have: removals, upgrades, an
# HTTP repository. RUNS rounds per tool and case in ABBA order after one untimed round; every run is kept.
#
# usage: [LABEL=name] [RUNS=15] [CASES="del-nano del-cataclysm up-one up-all http-add-nano http25-add-nano"]
#        [SETS="base big"] [APKSYNC=--sync] [OCTSYNC=--sync] [BDIR=dir] [STRICT_OUT=dir] [PROFDIR=dir] catalyst-strict2.sh
# See catalyst-strict.sh for the variables. A third time is kept for every run: until the old versions are deleted
# as well, and that is on disk (a sync after the trash is gone; for apk, which leaves nothing for later, one more
# sync). The statistics give it only for the cases in which catalyst did leave something for later
# (OCTCLEAN=--clean-later and a big change); the last column says in which runs. In those cases the time "until on
# disk" of catalyst includes whatever its other process had deleted by then. The upgrades need the second repository of the kill test
# (/root/catalyst-test/kt-repo: 27 packages of valence-base rebuilt as 9999-r0).
# PROFDIR, with CATALYST_PROF=1 in the environment: the step times of every timed run of catalyst are kept there.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
V=${VALENCE:-/root/valence}
APK=$V/hosttools/bin/apk
OCT=${OCT:-$V/catalyst/target/release/oct}
R1=$V/repo/x86_64/Packages.adb
R3=/root/catalyst-test/kt-repo/x86_64/Packages.adb
KD=/root/catalyst-test/keys/trusted
S=${STRICT_OUT:-/root/catalyst-test/strict}; mkdir -p "$S"
LABEL=${LABEL:-run2}; RUNS=${RUNS:-15}
CASES=${CASES:-del-nano del-cataclysm up-one up-all http-add-nano http25-add-nano}; SETS=${SETS:-base big}
B=${BDIR:-/root/catalyst-test/benchab2}; case $B in /root/catalyst-test/?*) ;; *) echo "BDIR must be under /root/catalyst-test"; exit 1;; esac
grep -q " $B " /proc/mounts && { echo "$B is a mount point"; exit 1; }
rm -rf --one-file-system $B; mkdir -p $B
# Both tools are run from fresh copies on the file system that is measured, as an installed system has them. From
# a cold cache a run reads its own program first, and where a build left that file on the disk is no property of
# the tool: on 23 November 2026 catalyst's binary read in 15 ms from its build directory and in 7 ms as a fresh
# copy, apk's in 7 ms and in 5.5. COPY_TOOLS=0 runs them from where they are, as the runs before that day did.
# Each is one executable with all of its own code in it; the libraries they load (libssl and libcrypto both, libz
# and libzstd apk, libgcc_s catalyst) stay where the system has them.
BUILT=$OCT
if [ "${COPY_TOOLS:-1}" = 1 ]; then
  mkdir $B/bin; cp --reflink=never $APK $B/bin/apk; cp --reflink=never $OCT $B/bin/oct; sync
  APK=$B/bin/apk; OCT=$B/bin/oct
fi
coldread(){ sync; echo 3 > /proc/sys/vm/drop_caches; python3 -I -c 'import sys, time
t = time.monotonic(); open(sys.argv[1], "rb").read(); print("%.1f" % ((time.monotonic() - t) * 1000))' "$1"; }
BIG="valence-base $(tr '\n' ' ' < /root/catalyst-test/bench2/big-set.txt)"
H0=http://127.0.0.1:8097/x86_64/Packages.adb; H25=http://127.0.0.1:8098/x86_64/Packages.adb
# apk / catalyst with repositories REPOS ("local", "up" = with the 9999-r0 one, or an http URL)
run(){ local t=$1 r=$2 repos=$3; shift 3; local a=()
  case $repos in local) a=(--repository $R1);; up) a=(--repository $R1 --repository $R3);; *) a=(--repository $repos);; esac
  case $t in
    apk) local nc=--no-cache; case $repos in http*) nc=;; esac     # over HTTP apk keeps its index cache, as installed systems do
         $APK --root $r --arch x86_64 --keys-dir $KD --repositories-file /dev/null "${a[@]}" $nc --no-scripts ${APKSYNC:-} "$@";;
    cat) $OCT --root $r "${a[@]}" --keys-dir $KD --no-scripts --plain ${OCTSYNC:-} ${OCTCLEAN:-} "$@";;
  esac; }
# old versions left to a process of its own: gone before the next run is timed
wait_trash(){ local n=0; while [ -e "$1/var/lib/catalyst/trash" ]; do sleep 0.002; n=$((n + 1)); [ $n -lt 15000 ] || { echo "  NOT OK: the trash of $1 is still there"; return 1; }; done; }
has(){ if [ $1 = apk ]; then grep -qx "P:$3" $2/lib/apk/db/installed; else [ -e $2/Programs/$3/Current ]; fi; }
ver(){ if [ $1 = apk ]; then grep -A1 -x "P:$3" $2/lib/apk/db/installed | sed -n 's/^V://p'; else readlink $2/Programs/$3/Current; fi; }
n9999(){ if [ $1 = apk ]; then grep -c '^V:9999-r0' $2/lib/apk/db/installed; else ls -l $2/Programs/*/Current | grep -c -- '-> 9999-r0'; fi; }
fresh(){ rm -rf --one-file-system $2; mkdir -p $2; run $1 $2 local --initdb add $3 > $B/prep.log 2>&1 || { echo "prep $1 failed"; tail -n 3 $B/prep.log; exit 1; }; }

SRV0=; SRV25=
case " $CASES " in *" http"*)
cat > $B/srv.py <<'EOF'
import http.server, socketserver, sys, time, os
delay = float(sys.argv[2]) / 1000
class H(http.server.SimpleHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def do_GET(self):
        time.sleep(delay)
        super().do_GET()
    def log_message(self, fmt, *args):
        with open(sys.argv[3], 'a') as f:
            f.write('%s\n' % (fmt % args))
class S(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
os.chdir(sys.argv[4])
S(('127.0.0.1', int(sys.argv[1])), H).serve_forever()
EOF
python3 -I $B/srv.py 8097 0 $B/access0.log $V/repo & SRV0=$!
python3 -I $B/srv.py 8098 25 $B/access25.log $V/repo & SRV25=$!
trap 'kill $SRV0 $SRV25 2>/dev/null' EXIT
sleep 0.5;; esac

OUT=$S/benchab-$LABEL.tsv
printf 'set\tjob\tcache\ttool\trun\twall\twall_sync\twall_done\ttrash\n' > $OUT
REQ=$B/requests.tsv; : > $REQ
echo "benchab2 $LABEL: $RUNS runs; catalyst $(stat -c %y $BUILT | cut -c1-19); $(date -u +%FT%TZ); load $(cut -d' ' -f1-3 /proc/loadavg); $(stat -f -c %T $B)"
echo "tools: $OCT, $(stat -c %s $OCT) bytes, read from a cold cache in $(coldread $OCT) ms; $APK, $(stat -c %s $APK) bytes, in $(coldread $APK) ms"

# one timed operation of CASE by TOOL on ROOT; sets rc
timed(){ local case=$1 t=$2 R=$3
  case $case in
    del-nano) run $t $R local del nano;;
    del-cataclysm) run $t $R local del cataclysm-dda-tiles;;
    up-one) if [ $t = apk ]; then run $t $R up upgrade coreutils; else run $t $R up add -u coreutils; fi;;
    up-all) run $t $R up upgrade;;
    http-add-nano) run $t $R $H0 add nano;;
    http25-add-nano) run $t $R $H25 add nano;;
  esac; }
okay(){ local case=$1 t=$2 R=$3
  case $case in
    del-nano) ! has $t $R nano;;
    del-cataclysm) ! has $t $R cataclysm-dda-tiles;;
    up-one) [ "$(ver $t $R coreutils)" = 9999-r0 ];;
    up-all) [ "$(n9999 $t $R)" = 27 ];;
    http*) has $t $R nano;;
  esac; }
# puts ROOT back where the timed operation starts (not timed)
undo(){ local case=$1 t=$2 R=$3
  case $case in
    del-nano) run $t $R local add nano;;
    del-cataclysm) run $t $R local add cataclysm-dda-tiles;;
    up-one) run $t $R up add "coreutils=$OLD" && { [ $t != apk ] || run $t $R up add coreutils; } && [ "$(ver $t $R coreutils)" = "$OLD" ];;
    up-all) :;;   # a fresh root every round
    http-add-nano) run $t $R $H0 del nano; rm -f $R/var/cache/catalyst/nano-*.apk;;
    http25-add-nano) run $t $R $H25 del nano; rm -f $R/var/cache/catalyst/nano-*.apk;;
  esac; }

for set in $SETS; do
  names=valence-base; [ $set = big ] && names=$BIG
  for t in apk cat; do fresh $t $B/$set-$t "$names"; done
  OLD=$(ver cat $B/$set-cat coreutils)
  for case in $CASES; do
    caches="warm cold"
    case $case in http*) caches=warm;; up-all) [ $set = big ] && continue;; esac
    # the state each case starts from
    case $case in
      del-nano) for t in apk cat; do run $t $B/$set-$t local add nano > /dev/null 2>&1; done;;
      del-cataclysm) for t in apk cat; do run $t $B/$set-$t local add cataclysm-dda-tiles > /dev/null 2>&1; done;;
    esac
    for c in $caches; do
      for i in $(seq 0 $RUNS); do
        if [ $(( i % 2 )) = 0 ]; then order="apk cat"; else order="cat apk"; fi
        for t in $order; do
          R=$B/$set-$t
          [ $case = up-all ] && { R=$B/$set-$t-up; fresh $t $R "$names"; }
          case $case in http25*) : > $B/access25.log;; http*) : > $B/access0.log;; esac
          sync; [ $c = cold ] && echo 3 > /proc/sys/vm/drop_caches
          t0=$(date +%s%N); timed $case $t $R > $B/op.log 2>&1; rc=$?; t1=$(date +%s%N); tr=0; [ -e $R/var/lib/catalyst/trash ] && tr=1; sync -f $R; t2=$(date +%s%N); wait_trash $R; sync -f $R; t3=$(date +%s%N)
          if [ $rc != 0 ] || ! okay $case $t $R; then echo "  NOT OK: $set $case $c $t $i (rc $rc)"; tail -n 2 $B/op.log; fi
          [ -n "${PROFDIR:-}" ] && [ $t = cat ] && [ $i -gt 0 ] && grep '^prof:' $B/op.log > $PROFDIR/$set-$case-$c-$i.txt
          [ $i = 0 ] && echo "  $set $case $c $t: $(grep -E '^OK|^\([0-9]+/[0-9]+\)' $B/op.log | tail -n 1 | cut -c1-110)"
          if [ $i -gt 0 ]; then
            printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' $set $case $c $t $i \
              $(awk -v a=$t0 -v b=$t1 'BEGIN{printf "%.4f",(b-a)/1e9}') $(awk -v a=$t0 -v b=$t2 'BEGIN{printf "%.4f",(b-a)/1e9}') \
              $(awk -v a=$t0 -v b=$t3 'BEGIN{printf "%.4f",(b-a)/1e9}') $tr >> $OUT
            case $case in http25*) printf '%s\t%s\t%s\t%s\n' $set $case $t "$(awk '{print $2}' $B/access25.log | sed 's|.*/||' | sort | tr '\n' ' ')" >> $REQ;;
                          http*) printf '%s\t%s\t%s\t%s\n' $set $case $t "$(awk '{print $2}' $B/access0.log | sed 's|.*/||' | sort | tr '\n' ' ')" >> $REQ;; esac
          fi
          undo $case $t $R > $B/undo.log 2>&1 || { echo "  undo failed: $set $case $t $i"; tail -n 2 $B/undo.log; }
          wait_trash $R
        done
      done
    done
    # leave the roots as the next case expects them
    case $case in
      del-nano) for t in apk cat; do run $t $B/$set-$t local del nano > /dev/null 2>&1; done; wait_trash $B/$set-cat;;
      del-cataclysm) for t in apk cat; do run $t $B/$set-$t local del cataclysm-dda-tiles > /dev/null 2>&1; done; wait_trash $B/$set-cat;;
      up-all) rm -rf --one-file-system $B/$set-apk-up $B/$set-cat-up;;
    esac
  done
done
if [ -n "$SRV0" ]; then
  echo "--- HTTP requests per timed add (what every run of a tool asked for)"
  sort $REQ | uniq -c | awk '{c=$1; $1=""; print "  " c " runs:" $0}'
  kill $SRV0 $SRV25 2>/dev/null
fi
rm -rf --one-file-system $B
python3 -I $HERE/catalyst-strict-stats.py $OUT
