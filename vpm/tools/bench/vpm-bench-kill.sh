#!/bin/bash
# stop only this benchmark's own processes
pkill -f 'vpm-bench-pick.py' ; pkill -f 'vpm-bench-runpick.sh'
sleep 1
pkill -f -- '--root /root/vpm-test/bench/'
sleep 2
ps aux | grep -E 'vpm-bench|/root/vpm-test/bench' | grep -v grep | cut -c1-200
echo ---- other activity:
ps aux --sort=-%cpu | head -15 | cut -c1-220
grep vpm-test /proc/mounts
