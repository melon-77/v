#!/bin/bash
# Speedup experiments with the instrumented copy (bin/oct-prof), warm page
# cache, --no-scripts, 3 runs each, alternating:
#   baseline         as committed
#   early-wb         CATALYST_EARLY_WB=1: sync_file_range(WRITE) after each unpacked file
#   direct-link      CATALYST_DIRECT_LINK=1: link/symlink straight onto absent targets
#   both             early-wb + direct-link
#   flusher          CATALYST_FLUSHER=1: a separate thread calls syncfs in a loop during the unpack
#   flusher+direct   flusher + direct-link
#   zstd             the same packages recompressed with zstd (repo-zstd), baseline code
#   zstd+both, zstd+flusher+direct
#   apk, apk-zstd    apk-tools on the original and the zstd copy
# VARIANTS="..." picks variants; RUNS (3); OUT (exp.tsv).
# Output: /root/catalyst-test/bench/$OUT and a median table.
set -u
B=/root/catalyst-test/bench
T=$B/xroot
K=/root/valence/keys/trusted
RD=/root/valence/repo/x86_64/Packages.adb
RZ=$B/repo-zstd/x86_64/Packages.adb
OUT=${OUT:-$B/exp.tsv}
RUNS=${RUNS:-3}
safe_rm() {
    case "$1" in /root/catalyst-test/?*) ;; *) echo "refusing to remove $1"; exit 1;; esac
    if grep -q " $1" /proc/mounts; then echo "MOUNTS under $1, not removing"; exit 1; fi
    if [ -e "$1" ]; then rm -rf --one-file-system "$1"; fi
}
BIG="valence-base $(cat $B/big-set.txt | tr '\n' ' ')"
printf 'set\tvariant\trun\twall\tunpack_ms\tsyncfs1_ms\tplace_ms\tok\n' > $OUT
cat /root/valence/repo/x86_64/*.apk > /dev/null
[ -d $B/repo-zstd ] && cat $B/repo-zstd/x86_64/*.apk > /dev/null
one() { # set variant run
    local set=$1 v=$2 run=$3 names=valence-base repo=$RD envs=""
    [ $set = big ] && names=$BIG
    case $v in
        early-wb) envs="CATALYST_EARLY_WB=1";;
        flusher) envs="CATALYST_FLUSHER=1";;
        direct-link) envs="CATALYST_DIRECT_LINK=1";;
        both) envs="CATALYST_EARLY_WB=1 CATALYST_DIRECT_LINK=1";;
        flusher+direct) envs="CATALYST_FLUSHER=1 CATALYST_DIRECT_LINK=1";;
        zstd) repo=$RZ;;
        zstd+both) repo=$RZ; envs="CATALYST_EARLY_WB=1 CATALYST_DIRECT_LINK=1";;
        zstd+flusher+direct) repo=$RZ; envs="CATALYST_FLUSHER=1 CATALYST_DIRECT_LINK=1";;
    esac
    safe_rm $T; sync
    local log=$B/logs/exp-$set-$v-$run.log
    local t0 t1 rc
    t0=$(date +%s%N)
    case $v in
        apk|apk-zstd)
            [ $v = apk-zstd ] && repo=$RZ
            /root/valence/hosttools/bin/apk --root $T --initdb --arch x86_64 --keys-dir $K --repositories-file /dev/null \
                --repository $repo --no-cache --no-scripts add $names > $log 2>&1 < /dev/null
            rc=$?;;
        *)
            env CATALYST_PROF=1 $envs $B/bin/oct-prof --root $T --initdb --repository $repo --keys-dir $K --plain --no-scripts add $names > $log 2>&1 < /dev/null
            rc=$?;;
    esac
    t1=$(date +%s%N)
    local unp sy pl
    unp=$(grep 'fetch + unpack (threads)' $log | sed 's/.*(+ *\([0-9.]*\) ms).*/\1/')
    sy=$(grep 'syncfs before the journal' $log | sed 's/.*(+ *\([0-9.]*\) ms).*/\1/')
    pl=$(grep 'packages placed' $log | sed 's/.*(+ *\([0-9.]*\) ms).*/\1/')
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' $set $v $run $(awk -v a=$t0 -v b=$t1 'BEGIN { printf "%.3f", (b - a) / 1e9 }') "$unp" "$sy" "$pl" $rc >> $OUT
    tail -1 $OUT
}
VARIANTS=${VARIANTS:-"baseline early-wb direct-link both"}
[ -s $RZ ] && [ -z "${NOZSTD:-}" ] && VARIANTS="$VARIANTS zstd zstd+both apk apk-zstd"
for set in base big; do
    for v in $VARIANTS; do one $set $v 0 > /dev/null; done   # warm-up
    for i in $(seq 1 $RUNS); do
        for v in $VARIANTS; do one $set $v $i; done
    done
done
safe_rm $T
python3 -I - $OUT <<'EOF'
import csv, statistics, sys
rows = list(csv.DictReader(open(sys.argv[1]), delimiter="\t"))
g = {}
for r in rows:
    if r["run"] != "0":  # warm-up runs
        g.setdefault((r["set"], r["variant"]), []).append(r)
print("| set | variant | runs | wall median (min-max) | unpack | syncfs before journal | placing |")
print("|---|---|---|---|---|---|---|")
for (s, v), rs in g.items():
    def med(k):
        x = [float(r[k]) for r in rs if r[k]]
        return statistics.median(x) if x else float("nan")
    w = [float(r["wall"]) for r in rs]
    bad = sum(1 for r in rs if r["ok"] != "0")
    print("| %s | %s | %d%s | %.3f s (%.3f-%.3f) | %.0f ms | %.0f ms | %.0f ms |" % (
        s, v, len(rs), " (%d failed)" % bad if bad else "", statistics.median(w), min(w), max(w),
        med("unpack_ms"), med("syncfs1_ms"), med("place_ms")))
EOF
