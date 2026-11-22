#!/bin/bash
# stop only this benchmark's own processes
pkill -f 'octet-bench-pick.py' ; pkill -f 'octet-bench-runpick.sh'
sleep 1
pkill -f -- '--root /root/octet-test/bench/'
sleep 2
ps aux | grep -E 'octet-bench|/root/octet-test/bench' | grep -v grep | cut -c1-200
echo ---- other activity:
ps aux --sort=-%cpu | head -15 | cut -c1-220
grep octet-test /proc/mounts
