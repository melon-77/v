#!/bin/bash
R=/root/valence/repo/x86_64
for f in glibc-2.44-r1 coreutils-9.12-r1 busybox-1.37.0-r1; do
  printf '%s: ' $f; head -c 8 $R/$f.apk | od -An -c | tr -s ' '
done
head -c 8 $R/Packages.adb | od -An -c
# compression of all packages
for f in $R/*.apk; do head -c 5 $f | od -An -c | tr -d ' \n'; echo; done | sort | uniq -c
