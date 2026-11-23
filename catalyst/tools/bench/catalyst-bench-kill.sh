#!/bin/bash
# stop only this benchmark's own processes
pkill -f 'catalyst-bench-pick.py' ; pkill -f 'catalyst-bench-runpick.sh'
sleep 1
pkill -f -- '--root /root/catalyst-test/bench/'
sleep 2
ps aux | grep -E 'catalyst-bench|/root/catalyst-test/bench' | grep -v grep | cut -c1-200
echo ---- other activity:
ps aux --sort=-%cpu | head -15 | cut -c1-220
grep catalyst-test /proc/mounts
