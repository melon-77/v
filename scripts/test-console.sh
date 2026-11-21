#!/bin/bash
# test-console.sh [ISO]: the eight console tests (AGENTS.md, "Testing") on an ISO, the newest console ISO in out/ by
# default: install on BIOS, boot on BIOS and UEFI, roll back, the same encrypted, and dual boot next to a fake Windows.
# Stops at the first failure unless ALL=1. About 13 minutes without KVM. Logs: logs/test-<name>.log, logs/qemu-*.log.
set -uo pipefail
M=$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd); cd $M
step(){ echo "== $(date +%H:%M:%S) $*"; }
ISO=${1:-$(ls -t $M/out/valence-2*-x86_64.iso 2>/dev/null | head -n 1)}
[ -f "$ISO" ] || { echo "test-console: no ISO (build one with scripts/mkiso.sh)" >&2; exit 1; }
ls -l "$ISO"
T=$M/work/tests; mkdir -p $T $M/logs
run(){   # run <name> <qemu-test.py arguments...>
  local name=$1; shift
  step "test: $name"
  python3 $M/scripts/qemu-test.py "$@" > $M/logs/test-$name.log 2>&1; local rc=$?
  grep -a -E '^\[ *[0-9.]+s\]' $M/logs/test-$name.log | tail -n 12 | cut -c1-220
  echo "   $name: $([ $rc = 0 ] && echo PASS || echo "FAIL (exit $rc)")"
  if [ $rc != 0 ]; then
    echo "--- end of logs/test-$name.log"; tail -n 12 $M/logs/test-$name.log | cut -c1-240
    q=$(ls -t $M/logs/qemu-*.log | head -n 1); echo "--- end of $q"
    tail -c 2500 "$q" | tr -d '\r' | sed 's/\x1b\[[0-9;?]*[a-zA-Z]//g' | grep -a -v '^ *$' | tail -n 25 | cut -c1-220
  fi
  return $rc
}
fail=0
go(){ if [ $fail = 0 ] || [ "${ALL:-0}" = 1 ]; then run "$@" || fail=1; fi; }
rm -f $T/disk.img; qemu-img create -q -f raw $T/disk.img 12G
go live-bios     live "$ISO" $T/disk.img
go disk-bios     disk $T/disk.img
go disk-uefi     disk $T/disk.img --uefi
go rollback-bios rollback $T/disk.img
go live-luks     live "$ISO" $T/disk.img --luks
go disk-luks     disk $T/disk.img --luks
go rollback-luks rollback $T/disk.img --luks --uefi
rm -f $T/dual.img
go dualboot      dualboot "$ISO" $T/dual.img
rm -f $T/disk.img $T/dual.img
step "tests finished: $([ $fail = 0 ] && echo 'all passed' || echo 'FAILED')"
exit $fail
