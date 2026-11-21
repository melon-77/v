# Valence build environment
#   VALENCE_ARCH=x86_64 (default), VALENCE_ARCH=x86 (32-bit, i686) or VALENCE_ARCH=aarch64 (64-bit ARM: the Raspberry Pi
#   edition, ROADMAP.md phase 9; armv8-a, which every 64-bit Pi has)
#   x86 is i686 with SSE2 (gcc --with-arch=pentium-m --with-fpmath=sse, as Alpine): Pentium M, Pentium 4, Atom and newer;
#   Qt 6 needs SSE2 on 32-bit x86
export M=${M:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}
export VALENCE_ARCH=${VALENCE_ARCH:-x86_64}
case $VALENCE_ARCH in
  x86_64) export TARGET=x86_64-valence-linux-gnu APK_ARCH=x86_64 ARCH_SUFFIX= KARCH=x86_64 \
                 LDSO=ld-linux-x86-64.so.2 GCC_ARCH=x86-64 MESON_CPU_FAMILY=x86_64 MESON_CPU=x86_64 EFI_TARGET=x86_64-efi EFI_BOOT=BOOTX64.EFI \
                 RUST_TARGET=x86_64-unknown-linux-gnu ;;
  x86)    export TARGET=i686-valence-linux-gnu APK_ARCH=x86 ARCH_SUFFIX=-x86 KARCH=i386 \
                 LDSO=ld-linux.so.2 GCC_ARCH=pentium-m GCC_FPMATH=sse MESON_CPU_FAMILY=x86 MESON_CPU=i686 EFI_TARGET=i386-efi EFI_BOOT=BOOTIA32.EFI \
                 RUST_TARGET=i686-unknown-linux-gnu ;;
  aarch64) export TARGET=aarch64-valence-linux-gnu APK_ARCH=aarch64 ARCH_SUFFIX=-aarch64 KARCH=arm64 \
                 LDSO=ld-linux-aarch64.so.1 GCC_ARCH=armv8-a MESON_CPU_FAMILY=aarch64 MESON_CPU=aarch64 EFI_TARGET=arm64-efi EFI_BOOT=BOOTAA64.EFI \
                 RUST_TARGET=aarch64-unknown-linux-gnu ;;
  *) echo "unknown VALENCE_ARCH $VALENCE_ARCH" >&2; return 1 2>/dev/null || exit 1 ;;
esac
export TOOLS=$M/tools$ARCH_SUFFIX
export SYSROOT=$M/sysroot$ARCH_SUFFIX
export SRC=$M/sources
export WORK=$M/work$ARCH_SUFFIX
export REPO=$M/repo
export PATH=$TOOLS/bin:$PATH
# The build machine: Ubuntu (or WSL2 Ubuntu), or Valence itself (BUILDING.md, "Valence as the build machine").
# On Valence, `gcc -dumpmachine` is Valence's own target triple, x86_64-valence-linux-gnu; configure scripts would take the
# cross toolchain for a native one, so the build machine calls itself <arch>-pc-linux-gnu (AGENTS.md rule 53).
export VALENCE_HOST=$( (. /etc/os-release 2>/dev/null && echo "${ID:-unknown}") || echo unknown)
BUILD_TRIPLE=$(gcc -dumpmachine 2>/dev/null || echo "$(uname -m)-pc-linux-gnu")
case $BUILD_TRIPLE in *-valence-linux-*) BUILD_TRIPLE=$(uname -m)-pc-linux-gnu ;; esac
export BUILD_TRIPLE
# the cross toolchain's binutils and gcc: say which machine they run on when that's Valence
TOOLCHAIN_HOST_FLAGS=; [ "$VALENCE_HOST" = valence ] && TOOLCHAIN_HOST_FLAGS="--build=$BUILD_TRIPLE --host=$BUILD_TRIPLE"
export TOOLCHAIN_HOST_FLAGS
# a target the build machine's processor cannot run: built programs start through QEMU's user-mode emulator
# (scripts/valence-run; Ubuntu's qemu-user)
case $VALENCE_ARCH in
  x86_64|x86) export VALENCE_EMU= ;;
  *) export VALENCE_EMU=qemu-$VALENCE_ARCH ;;
esac
export JOBS=${JOBS:-$(nproc)}   # JOBS=10 scripts/... to use fewer (RAM: about 1 GB per job for Qt/KDE)
