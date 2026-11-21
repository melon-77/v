#!/bin/bash
B=/root/vpm-test/bench
T=$B/proot
safe_rm() {
    case "$1" in /root/vpm-test/?*) ;; *) echo "refusing to remove $1"; exit 1;; esac
    if grep -q " $1" /proc/mounts; then echo "MOUNTS under $1, not removing"; exit 1; fi
    if [ -e "$1" ]; then rm -rf --one-file-system "$1"; fi
}
BIG="valence-base $(cat $B/big-set.txt | tr '\n' ' ')"
cat /root/valence/repo/x86_64/*.apk > /dev/null
echo "load: $(cat /proc/loadavg)"
safe_rm $T; sync
echo "=== base warm --no-scripts --jobs 1"
VPM_PROF=1 $B/bin/vpm-prof --root $T --initdb --repository /root/valence/repo/x86_64/Packages.adb --keys-dir /root/valence/keys/trusted --plain --no-scripts --jobs 1 add valence-base 2>&1 < /dev/null | grep -E '^prof (cnt|step)|^OK|last package|thread busy'
safe_rm $T; sync
echo "=== big warm --no-scripts"
/usr/bin/time -f "%e s wall, %U user, %S sys, %M KB" env VPM_PROF=1 $B/bin/vpm-prof --root $T --initdb --repository /root/valence/repo/x86_64/Packages.adb --keys-dir /root/valence/keys/trusted --plain --no-scripts add $BIG 2>&1 < /dev/null | grep -E '^prof|^OK|wall|to install'
du -sh $T; find $T -xdev ! -type d | wc -l
safe_rm $T; sync
echo "=== apk big warm --no-scripts"
/usr/bin/time -f "%e s wall, %U user, %S sys, %M KB" /root/valence/hosttools/bin/apk --root $T --initdb --arch x86_64 --keys-dir /root/valence/keys/trusted --repositories-file /dev/null --repository /root/valence/repo/x86_64/Packages.adb --no-cache --no-scripts add $BIG 2>&1 < /dev/null | tail -2
t0=$(date +%s%N); sync -f $T; t1=$(date +%s%N); echo "apk: sync after: $(( (t1 - t0) / 1000000 )) ms"
safe_rm $T
echo "load: $(cat /proc/loadavg)"
