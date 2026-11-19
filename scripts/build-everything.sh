#!/bin/bash
# build-everything.sh: build Valence from nothing on a prepared host (scripts/host-setup.sh first).
#   the cross toolchain -> every package, in the order the recipes themselves give (scripts/valence-build) -> the ISOs
# Resumable: run it again after an interruption. What is built is skipped, and a package that was interrupted
# mid-build continues where it stopped. Needs keys/valence-signing.rsa (see BUILDING.md).
#   JOBS=16 scripts/build-everything.sh
#   VALENCE_ARCH=x86 scripts/build-everything.sh    the 32-bit edition: what its two ISOs are made of, not every recipe
set -uo pipefail
. "$(dirname "$(readlink -f "$0")")/env.sh"
LOG=$M/logs; mkdir -p $LOG $REPO/$APK_ARCH
step(){ printf '\033[1;35m== %s  %s\033[0m\n' "$(date +%H:%M)" "$*"; }
[ -f $M/keys/valence-signing.rsa ] || { echo "keys/valence-signing.rsa is missing (see BUILDING.md)" >&2; exit 1; }

if [ ! -x $TOOLS/bin/$TARGET-gcc ] || ! grep -q '^EXIT 0' $LOG/toolchain$ARCH_SUFFIX.log 2>/dev/null; then
  step "cross toolchain ($TARGET)"
  $M/scripts/toolchain.sh > $LOG/toolchain$ARCH_SUFFIX.log 2>&1; echo "EXIT $?" >> $LOG/toolchain$ARCH_SUFFIX.log
  grep -q '^EXIT 0' $LOG/toolchain$ARCH_SUFFIX.log || { echo "toolchain failed, see logs/toolchain$ARCH_SUFFIX.log"; exit 1; }
fi

# 64-bit: every recipe. The 32-bit (i686) edition, for old netbooks and laptops (the owner, 7 October 2026: LXQt as
# its desktop): the console system, the LXQt desktop list (scripts/desktop-packages-x86.txt), the installer, and
# whatever those need. No Plasma, Flatpak, NVIDIA, Rust or developer tools: those stay 64-bit.
step "packages"
if [ "$APK_ARCH" = x86 ]; then what="--edition desktop"; else what=; fi
$M/scripts/valence-build $what > $LOG/everything$ARCH_SUFFIX.log 2>&1; rc=$?
grep -E '^##### [0-9:]+ (built|would build) |^##### FAILED: ' $LOG/everything$ARCH_SUFFIX.log | sed 's/^##### /   /'

step "ISOs"
ISOARCH=$([ $APK_ARCH = x86 ] && echo i686 || echo x86_64)
# grub-mkrescue for 32-bit ISOs (i386-pc + i386-efi); host-setup.sh made the 64-bit one
[ -x $M/hosttools/grub$ARCH_SUFFIX/bin/grub-mkrescue ] || $M/scripts/hostgrub.sh > $LOG/hostgrub$ARCH_SUFFIX.log 2>&1
$M/scripts/mkiso.sh > $LOG/mkiso$ARCH_SUFFIX.log 2>&1 && echo "   console ISO: $(ls $M/out/valence-2*-$ISOARCH.iso)"
VALENCE_EDITION=desktop $M/scripts/mkiso.sh > $LOG/mkiso-desktop$ARCH_SUFFIX.log 2>&1 && echo "   desktop ISO: $(ls $M/out/valence-desktop-*-$ISOARCH.iso)"
[ $rc = 0 ] || { echo "not every package was built: logs/everything$ARCH_SUFFIX.log, then logs/pkg-<name>.log"; exit 1; }
