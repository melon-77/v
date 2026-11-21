#!/bin/bash
uname -a
nproc; grep 'model name' /proc/cpuinfo | head -1; free -m | head -2
df -h /root /mnt/c | cat
for t in perf strace ltrace hyperfine valgrind bpftrace /usr/bin/time python3 xbps-install curl wget sha256sum minisign signify xz; do printf '%s: ' $t; command -v $t || echo missing; done
ls /usr/lib/linux-tools* 2>/dev/null | head
cat /proc/sys/kernel/perf_event_paranoid
ls -la /root/vpm-target/release/vpm /root/valence/hosttools/bin/apk
ls /root/vpm-test
grep vpm-test /proc/mounts
cd /mnt/c/Users/a/AppData/Local/Temp/claude/39afd7d0aa98685a/9f48dd28-e365-4eae-aba4-ec797468f31d/scratchpad/vpm && git -c safe.directory='*' log --oneline | head -5 && git -c safe.directory='*' status --short | head
ls /root/valence/repo/x86_64 | wc -l
du -sh /root/valence/repo/x86_64
ls -lS /root/valence/repo/x86_64/*.apk | head -50 | awk '{print $5, $9}'
