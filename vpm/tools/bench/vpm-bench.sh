#!/bin/bash
# vpm-bench.sh: install benchmark of vpm against apk-tools (and xbps)
#
#   vpm-bench.sh [--runs N] [--stages "disk base big jobs prof"] [--xbps]
#
# Run in WSL as root:  tr -d '\r' < vpm-bench.sh > /root/vpm-test/bench/vpm-bench.sh
#                      bash /root/vpm-test/bench/vpm-bench.sh [options]
#
# Same machine, same local repository (/root/valence/repo/x86_64), each
# run installs into an empty root /root/vpm-test/bench/root:
#   base  valence-base (39 packages)
#   big   valence-base + the 30 largest packages (by installed size) that
#         install together without conflict (list made by
#         vpm-bench-pick.py, kept in /root/vpm-test/bench/big-set.txt)
# each cold (sync; drop_caches before every run: repository, binaries and
# libraries come from disk) and warm (one unrecorded warm-up run per tool
# first), --no-scripts (apk and vpm then produce identical trees) and, for
# base, with scripts. apk and vpm alternate, the first tool changing every
# run. apk runs with --no-cache, vpm reads the local repository in place,
# so neither has a package cache.
#
# Per run: wall time of the command; wall time of the command plus a
# syncfs of the target filesystem right after it ("+sync": the data is on
# disk; vpm already syncs, apk leaves the writeback to the kernel); user
# and system CPU, max RSS, file system inputs/outputs (/usr/bin/time);
# vpm's own phase times; the 1-minute load average before the run (other
# work on the machine shows up there).
#
# Stages:
#   disk  cold and warm read throughput of the repository files
#   base  valence-base, cold and warm, --no-scripts and with scripts
#   big   the big set, cold and warm, --no-scripts
#   jobs  vpm --jobs 1 2 4 6 12, warm, base and big, --no-scripts
#   prof  the instrumented vpm ($B/bin/vpm-prof, VPM_PROF=1), warm and cold
#   xbps  (only with --xbps) Void's base-system with the static xbps from a
#         local copy of the repository: DOWNLOADS xbps-static (checked
#         against its published SHA-256) and Void's base-system packages
#         once into $B/xbps (needs the network), then times
#         "xbps-install -r ROOT -R LOCALREPO -S -y -U base-system" (-U:
#         unpack, no configure scripts, the counterpart of --no-scripts).
#         NOT RUN YET: written but untested (the download was not approved).
#
# Prerequisites (scripts next to this one):
#   vpm-bench-buildhead.sh  builds the committed vpm into $B/bin/vpm
#   vpm-bench-pick.py       picks the big set into $B/big-set.txt
#   vpm-bench-buildprof.sh  builds the instrumented copy (vpm-prof)
#                           into $B/bin/vpm-prof (only for the prof stage)
#
# Results: $B/results.tsv (one line per run), $B/sizes.tsv, $B/logs/,
# $B/summary.md (made by vpm-bench-summary.py).
set -u
S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # this folder
B=/root/vpm-test/bench
REPO=/root/valence/repo/x86_64/Packages.adb
KEYS=/root/valence/keys/trusted
APK=/root/valence/hosttools/bin/apk
VPM=${VPM:-$B/bin/vpm}
T=$B/root
RUNS=5
STAGES="disk base big jobs prof"
WITH_XBPS=0
while [ $# -gt 0 ]; do
    case "$1" in
        --runs) RUNS=$2; shift 2;;
        --stages) STAGES=$2; shift 2;;
        --xbps) WITH_XBPS=1; shift;;
        *) echo "usage: $0 [--runs N] [--stages \"disk base big jobs prof\"] [--xbps]"; exit 2;;
    esac
done
if [ $WITH_XBPS = 1 ]; then
    case " $STAGES " in *" xbps "*) ;; *) STAGES="$STAGES xbps";; esac
fi
[ "$(id -u)" = 0 ] || { echo "run as root"; exit 1; }
mkdir -p $B/logs
RES=$B/results.tsv
SIZES=$B/sizes.tsv
[ -f $RES ] || printf 'set\tcache\tscripts\ttool\trun\trc\twall\twall_sync\tuser\tsys\tmaxrss_kb\tmajflt\tfs_in\tfs_out\tpkgs\tresolve\tunpack\tswitch\tload1\textra\n' > $RES
[ -f $SIZES ] || printf 'set\ttool\tpkgs\tfiles\tbytes\n' > $SIZES

safe_rm() {
    case "$1" in /root/vpm-test/?*) ;; *) echo "refusing to remove $1"; exit 1;; esac
    if grep -q " $1" /proc/mounts; then echo "MOUNTS under $1, not removing"; exit 1; fi
    if [ -e "$1" ]; then rm -rf --one-file-system "$1"; fi
    return 0
}
drop_caches() { sync; echo 3 > /proc/sys/vm/drop_caches; }
drop_warmups() { sed -i '/^warmup\t/d' $RES; }
ns() { date +%s%N; }
secs() { awk -v a=$1 -v b=$2 'BEGIN { printf "%.4f", (b - a) / 1e9 }'; }

BASE_SET="valence-base"
BIG_SET="valence-base $(cat $B/big-set.txt 2>/dev/null | tr '\n' ' ')"

# the command line of TOOL installing NAMES into $T (scripts 0/1)
make_cmd() { # tool scripts names...
    local tool=$1 scripts=$2; shift 2
    local ns_=""; [ $scripts = 0 ] && ns_=--no-scripts
    case $tool in
        apk) CMD=($APK --root $T --initdb --arch x86_64 --keys-dir $KEYS --repositories-file /dev/null
                  --repository $REPO --no-cache $ns_ add "$@");;
        vpm) CMD=($VPM --root $T --initdb --repository $REPO --keys-dir $KEYS --plain $ns_ $VPM_EXTRA add "$@");;
        vpmprof) CMD=(env VPM_PROF=1 $B/bin/vpm-prof --root $T --initdb --repository $REPO --keys-dir $KEYS --plain $ns_ add "$@");;
        xbps) CMD=(env XBPS_ARCH=x86_64 $XBPS_INSTALL -r $T -C $B/xbps/empty.d -R $B/xbps/repo -S -y $XBPS_MODE "$@");;
    esac
}
VPM_EXTRA=""

# one recorded run: set cache scripts tool run label names...
one_run() {
    local set=$1 cache=$2 scripts=$3 tool=$4 run=$5 label=$6; shift 6
    safe_rm $T; sync
    [ $cache = cold ] && drop_caches
    make_cmd $tool $scripts "$@"
    local log=$B/logs/$set-$cache-s$scripts-$label-$run.log
    local load1 t0 t1 t2 rc
    load1=$(cut -d' ' -f1 /proc/loadavg)
    t0=$(ns)
    /usr/bin/time -o $B/time.out -f '%U %S %M %F %I %O' "${CMD[@]}" > $log 2>&1 < /dev/null
    rc=$?
    t1=$(ns)
    sync -f $T
    t2=$(ns)
    local user sys rss majf fin fout
    read -r user sys rss majf fin fout < <(tail -1 $B/time.out)
    local pkgs="" res="" unp="" sw="" extra=""
    case $tool in
        apk) pkgs=$(grep -o 'in [0-9]* packages' $log | tail -1 | grep -o '[0-9]*');;
        vpm*) local ok; ok=$(grep '^OK: [0-9]* packages installed' $log | tail -1)
             pkgs=$(echo "$ok" | sed -n 's/^OK: \([0-9]*\) packages.*/\1/p')
             res=$(echo "$ok" | sed -n 's/.*resolve \([0-9.]*\)s.*/\1/p')
             unp=$(echo "$ok" | sed -n 's/.*fetch+unpack \([0-9.]*\)s.*/\1/p')
             sw=$(echo "$ok" | sed -n 's/.*switch \([0-9.]*\)s.*/\1/p');;
        xbps) pkgs=$(grep -o '[0-9]* installed' $log | tail -1 | grep -o '[0-9]*');;
    esac
    [ -n "$VPM_EXTRA" ] && [ $tool = vpm ] && extra="$VPM_EXTRA"
    local w ws; w=$(secs $t0 $t1); ws=$(secs $t0 $t2)
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        $set $cache $scripts $label $run $rc $w $ws $user $sys $rss $majf $fin $fout "$pkgs" "$res" "$unp" "$sw" $load1 "$extra" >> $RES
    printf '  %-5s %-4s scripts=%s %-8s run %s: %7.3fs  +sync %7.3fs  cpu %5.2f+%5.2fs  rc=%s pkgs=%s %s load=%s\n' \
        $set $cache $scripts $label $run $w $ws $user $sys $rc "$pkgs" "${res:+resolve ${res}s unpack ${unp}s switch ${sw}s}" $load1
    # the installed tree, once per set and tool (hard links count once)
    if [ $set != warmup ] && [ $scripts = 0 ] && [ $rc = 0 ] &&
        ! awk -F'\t' -v s=$set -v l=$label '$1 == s && $2 == l { f = 1 } END { exit !f }' $SIZES; then
        local files bytes
        files=$(find $T -xdev ! -type d | wc -l)
        bytes=$(du -sbx $T | cut -f1)
        printf '%s\t%s\t%s\t%s\t%s\n' $set $label "$pkgs" $files $bytes >> $SIZES
    fi
}

# RUNS alternating runs of the tools: set cache scripts names...
bench() {
    local set=$1 cache=$2 scripts=$3; shift 3
    local tools="apk vpm"
    echo "== $set, $cache, scripts=$scripts ($RUNS runs each)"
    if [ $cache = warm ]; then
        cat /root/valence/repo/x86_64/*.apk > /dev/null
        for t in $tools; do one_run warmup $cache $scripts $t 0 $t "$@" > /dev/null; done
        drop_warmups
    fi
    for i in $(seq 1 $RUNS); do
        local order="$tools"; [ $((i % 2)) = 0 ] && order="vpm apk"
        for t in $order; do one_run $set $cache $scripts $t $i $t "$@"; done
    done
}

stage_disk() {
    echo "== disk: read all repository files ($(du -sh /root/valence/repo/x86_64 | cut -f1))"
    local bytes t0 t1
    bytes=$(cat /root/valence/repo/x86_64/*.apk | wc -c)
    for c in cold warm; do
        [ $c = cold ] && drop_caches
        t0=$(ns); cat /root/valence/repo/x86_64/*.apk > /dev/null; t1=$(ns)
        echo "  $c: $(secs $t0 $t1)s, $(awk -v b=$bytes -v a=$t0 -v c=$t1 'BEGIN { printf "%.0f", b / 1e6 / ((c - a) / 1e9) }') MB/s"
    done
    echo "== disk: write and syncfs 1 GB"
    safe_rm $B/ddtest; mkdir -p $B/ddtest
    t0=$(ns); dd if=/dev/zero of=$B/ddtest/f bs=1M count=1024 status=none; t1=$(ns); sync -f $B/ddtest; local t2; t2=$(ns)
    echo "  write $(secs $t0 $t1)s, +sync $(secs $t0 $t2)s"
    safe_rm $B/ddtest
}

stage_base() {
    bench base cold 0 $BASE_SET
    bench base warm 0 $BASE_SET
    bench base cold 1 $BASE_SET
    bench base warm 1 $BASE_SET
}

stage_big() {
    [ -s $B/big-set.txt ] || { echo "no $B/big-set.txt: run vpm-bench-pick.py first"; return 1; }
    bench big cold 0 $BIG_SET
    bench big warm 0 $BIG_SET
}

stage_jobs() {
    echo "== vpm --jobs sweep, warm, --no-scripts (3 runs each)"
    cat /root/valence/repo/x86_64/*.apk > /dev/null
    for set in base big; do
        local names=$BASE_SET; [ $set = big ] && names=$BIG_SET
        for j in 1 2 4 6 12; do
            VPM_EXTRA="--jobs $j"
            one_run warmup warm 0 vpm 0 vpm $names > /dev/null
            drop_warmups
            for i in 1 2 3; do one_run jobs-$set warm 0 vpm $i vpm-j$j $names; done
        done
    done
    VPM_EXTRA=""
}

stage_prof() {
    [ -x $B/bin/vpm-prof ] || { echo "no $B/bin/vpm-prof: build the instrumented copy first"; return 0; }
    echo "== instrumented vpm (VPM_PROF=1)"
    for set in base big; do
        local names=$BASE_SET; [ $set = big ] && names=$BIG_SET
        for cache in warm cold; do
            for scripts in 0 1; do
                [ $set = big ] && [ $scripts = 1 ] && continue
                if [ $cache = warm ]; then
                    one_run warmup warm $scripts vpmprof 0 vpmprof $names > /dev/null
                    drop_warmups
                fi
                one_run prof-$set $cache $scripts vpmprof 1 vpmprof $names
                grep '^prof' $B/logs/prof-$set-$cache-s$scripts-vpmprof-1.log > $B/prof-$set-$cache-s$scripts.txt
            done
        done
    done
}

# Void Linux: static xbps and base-system, downloaded once
XBPS_INSTALL=""
XBPS_MODE="-U"   # unpack only (no configure scripts), as --no-scripts
stage_xbps() {
    [ $WITH_XBPS = 1 ] || { echo "xbps: needs --xbps (it downloads from repo-default.voidlinux.org)"; return 0; }
    local X=$B/xbps
    local SU=https://repo-default.voidlinux.org/static
    local RU=https://repo-default.voidlinux.org/current
    local TB=xbps-static-latest.x86_64-musl.tar.xz
    mkdir -p $X/empty.d
    if [ ! -d $X/static/usr ]; then
        echo "== xbps: downloading $SU/$TB"
        mkdir -p $X/dl
        curl -fsSL -o $X/dl/$TB $SU/$TB || { echo "download failed"; return 1; }
        # checksum: a .sha256 next to the tarball, else the sha256sums.txt list
        local want=""
        if curl -fsSL -o $X/dl/$TB.sha256 $SU/$TB.sha256 2>/dev/null; then
            want=$(grep -o '[0-9a-f]\{64\}' $X/dl/$TB.sha256 | head -1)
        elif curl -fsSL -o $X/dl/sha256sums.txt $SU/sha256sums.txt 2>/dev/null; then
            want=$(grep " \*\?$TB\$\|($TB)" $X/dl/sha256sums.txt | grep -o '[0-9a-f]\{64\}' | head -1)
        fi
        local have; have=$(sha256sum $X/dl/$TB | cut -d' ' -f1)
        if [ -z "$want" ] || [ "$want" != "$have" ]; then
            # Void's "latest" line in sha256sums.txt can lag behind the tarball it points to;
            # accept if the exact hash appears under a versioned xbps-static-static-*x86_64-musl.tar.xz entry.
            if [ -f "$X/dl/sha256sums.txt" ] && grep -q "^SHA256 (xbps-static-static-.*\.x86_64-musl\.tar\.xz) = $have\$\|^$have  \*?xbps-static-static-.*\.x86_64-musl\.tar\.xz\$" "$X/dl/sha256sums.txt"; then
                echo "  sha256 $have matched versioned xbps-static entry in sha256sums.txt"
            else
                echo "xbps: checksum missing or wrong (want '$want', have $have); not using it"
                return 1
            fi
        else
            echo "  sha256 $have OK"
        fi
        mkdir -p $X/static
        tar -xJf $X/dl/$TB -C $X/static
    fi
    XBPS_INSTALL=$(ls $X/static/usr/bin/xbps-install* | head -1)
    local XRINDEX; XRINDEX=$(ls $X/static/usr/bin/xbps-rindex* | head -1)
    if [ ! -s $X/repo/x86_64-repodata ]; then
        echo "== xbps: downloading base-system and its dependencies from $RU (once)"
        safe_rm $X/dlroot; mkdir -p $X/dlroot/var/db/xbps/keys
        [ -d $X/static/var/db/xbps/keys ] && cp $X/static/var/db/xbps/keys/* $X/dlroot/var/db/xbps/keys/
        XBPS_ARCH=x86_64 $XBPS_INSTALL -r $X/dlroot -C $X/empty.d -R $RU -S -y --download-only base-system < /dev/null \
            || { echo "xbps download failed"; return 1; }
        mkdir -p $X/repo
        cp $X/dlroot/var/cache/xbps/*.xbps $X/repo/
        XBPS_ARCH=x86_64 $XRINDEX -a $X/repo/*.xbps || return 1
        safe_rm $X/dlroot
        echo "  $(ls $X/repo/*.xbps | wc -l) packages, $(du -sh $X/repo | cut -f1)"
    fi
    echo "== xbps base-system from the local copy ($XBPS_MODE: unpack, no configure scripts)"
    for cache in cold warm; do
        [ $cache = warm ] && { cat $X/repo/*.xbps > /dev/null; one_run warmup warm 0 xbps 0 xbps base-system > /dev/null; drop_warmups; }
        for i in $(seq 1 $RUNS); do one_run void-base $cache 0 xbps $i xbps base-system; done
    done
}

# Single-package add benchmarks (add-small: nano; add-large: go) on top of a fresh valence-base root
stage_small() {
    for pair in "add-small:nano" "add-large:go"; do
        local label=${pair%%:*} pkg=${pair#*:}
        for cache in cold warm; do
            echo "== $label ($pkg), $cache, scripts=0 ($RUNS runs each)"
            for i in $(seq 1 $RUNS); do
                local order="apk vpm"; [ $((i % 2)) = 0 ] && order="vpm apk"
                for t in $order; do
                    safe_rm $T; sync
                    make_cmd $t 0 $BASE_SET
                    "${CMD[@]}" > /dev/null 2>&1 < /dev/null
                    sync -f $T
                    [ $cache = cold ] && drop_caches
                    local log=$B/logs/$label-$cache-s0-$t-$i.log
                    local load1 t0 t1 t2 rc
                    load1=$(cut -d' ' -f1 /proc/loadavg)
                    if [ $t = apk ]; then
                        CMD=($APK --root $T --arch x86_64 --keys-dir $KEYS --repositories-file /dev/null --repository $REPO --no-cache --no-scripts add $pkg)
                    else
                        CMD=($VPM --root $T --repository $REPO --keys-dir $KEYS --plain --no-scripts $VPM_EXTRA add $pkg)
                    fi
                    t0=$(ns)
                    /usr/bin/time -o $B/time.out -f '%U %S %M %F %I %O' "${CMD[@]}" > $log 2>&1 < /dev/null
                    rc=$?
                    t1=$(ns); sync -f $T; t2=$(ns)
                    local user sys rss majf fin fout
                    read -r user sys rss majf fin fout < <(tail -1 $B/time.out)
                    local w ws; w=$(secs $t0 $t1); ws=$(secs $t0 $t2)
                    printf '%s\t%s\t0\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t1\t\t\t\t%s\t\n' \
                        $label $cache $t $i $rc $w $ws $user $sys $rss $majf $fin $fout $load1 >> $RES
                    printf '  %-9s %-4s %-4s run %s: %7.3fs  +sync %7.3fs  rc=%s\n' $label $cache $t $i $w $ws $rc
                done
            done
        done
    done
}

echo "vpm-bench $(date -Is); $(nproc) CPUs, $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2); $(uname -r)"
echo "vpm: $(head -1 $B/bin/vpm.rev 2>/dev/null) $VPM; apk: $($APK --version)"
echo "free: $(free -m | awk '/Mem:/ {print $2 " MB total, " $7 " MB available"}'); load: $(cat /proc/loadavg)"
for st in $STAGES; do
    stage_$st
done
safe_rm $T
python3 -I $S/vpm-bench-summary.py $RES $SIZES | tee $B/summary.md
echo "mounts under /root/vpm-test: $(grep -c vpm-test /proc/mounts)"
