#!/bin/bash
# test-desktop.sh [ISO]: the three automated desktop tests (AGENTS.md, "Testing") on a desktop ISO, the newest in
# out/ by default: the live desktop, and an install with the console installer on BIOS and on UEFI that logs in to
# Plasma through the login screen. About 20 minutes without KVM. Look at the screenshots afterwards:
# logs/qemu-desktop.ppm (live) and logs/qemu-desktop-{greeter,splash,installed}.ppm (the last install).
# Calamares has no automated test (AGENTS.md says how to click through it).
set -uo pipefail
M=$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd); cd $M
ISO=${1:-$(ls -t $M/out/valence-desktop-2*-x86_64.iso 2>/dev/null | head -n 1)}
[ -f "$ISO" ] || { echo "test-desktop: no desktop ISO (VALENCE_EDITION=desktop scripts/mkiso.sh)" >&2; exit 1; }
ls -l "$ISO"
T=$M/work/tests; mkdir -p $T $M/logs
fail=0
echo "== $(date +%H:%M:%S) desktop (live)"
python3 scripts/qemu-test.py desktop "$ISO" > logs/test-desktop.log 2>&1; rc=$?
grep -a -E '^\[ *[0-9.]+s\]' logs/test-desktop.log | tail -n 9 | cut -c1-200
echo "   desktop: $([ $rc = 0 ] && echo PASS || echo "FAIL (exit $rc)")"; [ $rc = 0 ] || fail=1
for fw in bios uefi; do
  rm -f $T/desk.img; qemu-img create -q -f raw $T/desk.img 24G
  echo "== $(date +%H:%M:%S) desktop-install on $fw"
  python3 scripts/qemu-test.py desktop-install "$ISO" $T/desk.img $([ $fw = uefi ] && echo --uefi) > logs/test-desktop-install-$fw.log 2>&1; rc=$?
  grep -a -E '^\[ *[0-9.]+s\]' logs/test-desktop-install-$fw.log | tail -n 11 | cut -c1-200
  echo "   desktop-install-$fw: $([ $rc = 0 ] && echo PASS || echo "FAIL (exit $rc)")"; [ $rc = 0 ] || fail=1
done
rm -f $T/desk.img
echo "== $(date +%H:%M:%S) desktop tests finished: $([ $fail = 0 ] && echo 'all passed' || echo FAILED)"
exit $fail
