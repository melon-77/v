#!/bin/bash
B=/root/vpm-test/bench
T=$B/proot
safe_rm() {
    case "$1" in /root/vpm-test/?*) ;; *) echo "refusing to remove $1"; exit 1;; esac
    if grep -q " $1" /proc/mounts; then echo "MOUNTS under $1, not removing"; exit 1; fi
    if [ -e "$1" ]; then rm -rf --one-file-system "$1"; fi
}
cat /root/valence/repo/x86_64/*.apk > /dev/null
for ns in --no-scripts ""; do
  for i in 1 2; do
    safe_rm $T; sync
    echo "=== base warm $ns run $i"
    VPM_PROF=1 $B/bin/vpm-prof --root $T --initdb --repository /root/valence/repo/x86_64/Packages.adb --keys-dir /root/valence/keys/trusted --plain $ns add valence-base 2>&1 < /dev/null | grep -E '^prof|^OK'
  done
done
safe_rm $T
