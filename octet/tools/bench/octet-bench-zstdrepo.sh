#!/bin/bash
# Makes /root/octet-test/bench/repo-zstd/x86_64: the packages of the big set
# (and valence-base), recompressed with zstd by `apk adbsign -c zstd:LEVEL`
# (the ADB block, hence the package identity and the original signature,
# do not change), next to a copy of the original signed Packages.adb.
set -u
B=/root/octet-test/bench
R=/root/valence/repo/x86_64
Z=$B/repo-zstd/x86_64
APK=/root/valence/hosttools/bin/apk
LEVEL=${1:-9}
case "$Z" in /root/octet-test/bench/*) ;; *) exit 1;; esac
grep -q " $B" /proc/mounts && exit 1
rm -rf --one-file-system $B/repo-zstd
mkdir -p $Z
cp $R/Packages.adb $Z/
# the package files of the big closure: from a octet install of it
T=$B/zroot
rm -rf --one-file-system $T
BIG="valence-base $(cat $B/big-set.txt | tr '\n' ' ')"
$B/bin/oct --root $T --initdb --repository $R/Packages.adb --keys-dir /root/valence/keys/trusted --plain --no-scripts add $BIG > /dev/null < /dev/null || exit 1
for d in $T/Programs/*/; do n=$(basename $d); v=$(readlink $d/Current); echo "$n-$v.apk"; done > $B/zstd-files.txt
rm -rf --one-file-system $T
wc -l < $B/zstd-files.txt
(cd $R && cp $(cat $B/zstd-files.txt) $Z/)
du -sh $Z
t0=$(date +%s)
(cd $Z && cat $B/zstd-files.txt | xargs -P 12 -n 4 $APK --keys-dir /root/valence/keys/trusted --allow-untrusted adbsign -c zstd:$LEVEL 2>&1 | head -5) || echo "adbsign failed"
echo "recompressed in $(( $(date +%s) - t0 )) s"
du -sh $Z
for f in glibc-2.44-r1.apk boost-headers-1.90.0-r0.apk; do printf '%s: ' $f; head -c 5 $Z/$f | od -An -c | tr -s ' '; done
# both tools must accept the result
$B/bin/oct --keys-dir /root/valence/keys/trusted verify $Z/glibc-2.44-r1.apk
$APK --keys-dir /root/valence/keys/trusted verify $Z/glibc-2.44-r1.apk
