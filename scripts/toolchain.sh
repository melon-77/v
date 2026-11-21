#!/bin/bash
# Build the Valence cross toolchain for $VALENCE_ARCH (x86_64-, i686- or aarch64-valence-linux-gnu).
# The order a glibc cross toolchain needs: binutils; the kernel's headers; the compiler alone; glibc's headers and
# start files; libgcc; glibc; then the rest of GCC (libstdc++ and friends). Each step leaves a stamp in
# work/toolchain, so an interrupted run carries on where it stopped; delete work/toolchain to start over.
set -euo pipefail
. "$(dirname "$(readlink -f "$0")")/env.sh"
GLIBC=2.44 GCCV=15.2.0 BINUTILS=2.46 LINUX=7.0
B=$WORK/toolchain; mkdir -p $B $SYSROOT/usr/lib $SYSROOT/usr/bin $SYSROOT/usr/include; cd $B
# Valence uses a merged /usr: /lib -> usr/lib, /bin -> usr/bin. lib64 is only a link: the x86_64 ABI fixes the
# loader's name as /lib64/ld-linux-x86-64.so.2, and every 64-bit program asks for it by that name.
ln -sfn usr/lib $SYSROOT/lib; ln -sfn usr/bin $SYSROOT/bin; ln -sfn usr/bin $SYSROOT/sbin; ln -sfn bin $SYSROOT/usr/sbin
ln -sfn usr/lib $SYSROOT/lib64; ln -sfn lib $SYSROOT/usr/lib64

step(){ echo "=== $(date +%T) $*"; }
todo(){ [ ! -e $B/.done-$1 ]; }
finished(){ touch $B/.done-$1; }

if todo binutils; then step binutils
  rm -rf b-binutils binutils-with-gold-$BINUTILS
  tar xf $SRC/binutils-$BINUTILS.tar.xz; mkdir b-binutils; cd b-binutils
  ../binutils-with-gold-$BINUTILS/configure $TOOLCHAIN_HOST_FLAGS --target=$TARGET --prefix=$TOOLS --with-sysroot=$SYSROOT \
    --disable-nls --disable-werror --disable-multilib --disable-gprofng --enable-deterministic-archives --disable-gold >/dev/null
  make -j$JOBS >/dev/null; make install >/dev/null; cd ..
  finished binutils
fi

if todo headers; then step "linux headers"
  rm -rf linux-$LINUX
  tar xzf $SRC/linux-$LINUX.tar.gz linux-$LINUX
  make -C linux-$LINUX ARCH=$KARCH INSTALL_HDR_PATH=$SYSROOT/usr headers_install >/dev/null
  rm -rf linux-$LINUX
  finished headers
fi

if todo sources; then step "gcc and glibc sources"
  rm -rf gcc-$GCCV glibc-$GLIBC b-gcc b-glibc
  tar xf $SRC/gcc-$GCCV.tar.xz; cd gcc-$GCCV
  # 64-bit libraries go to lib, not lib64 (only a link here)
  sed -i '/m64=/s/lib64/lib/' gcc/config/i386/t-linux64
  sed -i '/mabi.lp64=/s/lib64/lib/' gcc/config/aarch64/t-aarch64-linux
  tar xf $SRC/gmp-6.3.0.tar.xz && mv gmp-6.3.0+dfsg gmp
  # the Debian dfsg tarball drops the docs; stop GMP from expecting them
  sed -i "s| doc/Makefile||" gmp/configure; sed -i "s/^SUBDIRS = \(.*\) doc$/SUBDIRS = \1/" gmp/Makefile.in
  tar xf $SRC/mpfr-4.2.2.tar.xz && mv mpfr-4.2.2 mpfr
  tar xf $SRC/mpc-1.3.1.tar.gz && mv mpc-1.3.1 mpc
  cd ..
  tar xf $SRC/glibc-$GLIBC.tar.xz
  finished sources
fi

# libstdc++ keeps its symbol versions (GLIBCXX_*): programs built on other distributions look for them.
GCC_CONF="$TOOLCHAIN_HOST_FLAGS --target=$TARGET --prefix=$TOOLS --with-sysroot=$SYSROOT --with-build-sysroot=$SYSROOT
  --with-glibc-version=$GLIBC --enable-languages=c,c++ --disable-multilib --disable-nls --disable-werror
  --disable-libsanitizer --disable-libssp
  --enable-default-pie --enable-default-ssp --enable-tls --enable-initfini-array
  --enable-libstdcxx-time --enable-__cxa_atexit --enable-threads=posix --enable-shared
  --enable-gnu-indirect-function --enable-gnu-unique-object --enable-linker-build-id --with-linker-hash-style=gnu
  --with-pkgversion=valence --with-arch=$GCC_ARCH ${GCC_FPMATH:+--with-fpmath=$GCC_FPMATH} --with-tune=generic"

if todo gcc1; then step "gcc, the compiler alone"
  rm -rf b-gcc; mkdir b-gcc; cd b-gcc
  # -std=gnu17 for the build machine's compiler: in-tree GMP's configure test calls `void g(){}` with arguments, which
  # GCC 15's default C23 rejects ("could not find a working compiler", rule 40). GCC 13 on Ubuntu defaults to gnu17 anyway;
  # the target libraries keep their own flags (CFLAGS_FOR_TARGET).
  CFLAGS="-g -O2 -std=gnu17" ../gcc-$GCCV/configure $GCC_CONF >/dev/null
  make -j$JOBS all-gcc >/dev/null; make install-gcc >/dev/null; cd ..
  finished gcc1
fi

# glibc, the same way recipes/glibc builds it: everything in /usr/lib (the loader too), programs in /usr/bin.
glibc_configure(){
  printf 'slibdir=/usr/lib\nrtlddir=/usr/lib\nsbindir=/usr/bin\nrootsbindir=/usr/bin\n' > configparms
  ../glibc-$GLIBC/configure --prefix=/usr --host=$TARGET --build=$BUILD_TRIPLE --libdir=/usr/lib --libexecdir=/usr/lib \
    --with-headers=$SYSROOT/usr/include --enable-kernel=5.10 --enable-bind-now --enable-stack-protector=strong \
    --disable-werror --disable-nscd --disable-profile MAKEINFO=: CFLAGS="-O2" >/dev/null
}

if todo glibc-start; then step "glibc headers and start files"
  rm -rf b-glibc; mkdir b-glibc; cd b-glibc; glibc_configure
  make install-bootstrap-headers=yes install-headers install_root=$SYSROOT >/dev/null
  make -j$JOBS csu/subdir_lib >/dev/null
  install -m644 csu/crt1.o csu/crti.o csu/crtn.o $SYSROOT/usr/lib/
  # an empty libc.so and stubs.h are enough for libgcc to build; the real ones replace them two steps on
  $TARGET-gcc -nostdlib -nostartfiles -shared -x c /dev/null -o $SYSROOT/usr/lib/libc.so
  touch $SYSROOT/usr/include/gnu/stubs.h
  cd ..
  # GCC put its own <limits.h> together when the compiler was built, and found no C library header to chain to:
  # that copy stops at GCC's own limits, and PATH_MAX, NAME_MAX and the rest of glibc's never appear. Now that
  # the header is there, have GCC make its headers again (the next step reinstalls them).
  rm -f b-gcc/gcc/stmp-int-hdrs
  finished glibc-start
fi

if todo libgcc; then step "gcc's headers again, and libgcc"
  cd b-gcc; make -j$JOBS all-gcc all-target-libgcc >/dev/null 2>../libgcc.err || { tail -n 30 ../libgcc.err; exit 1; }
  make install-gcc install-target-libgcc >/dev/null; cd ..
  finished libgcc
fi

if todo glibc; then step glibc
  cd b-glibc
  # "lib" first: it makes linkobj/libc_pic.a once, in this one make. In a plain parallel "make all", two directories
  # (rt and login) can each start making that archive at the same moment and delete each other's files
  # ("ar: link.os: No such file or directory"); it happened on the second build here.
  rm -rf linkobj
  make -j$JOBS lib >/dev/null 2>../glibc.err || { tail -n 40 ../glibc.err; exit 1; }
  make -j$JOBS >/dev/null 2>>../glibc.err || { tail -n 40 ../glibc.err; exit 1; }
  make install install_root=$SYSROOT >/dev/null 2>>../glibc.err || { tail -n 40 ../glibc.err; exit 1; }
  cd ..
  finished glibc
fi

if todo gcc; then step "gcc, the rest (libstdc++ and the other target libraries)"
  cd b-gcc; make -j$JOBS >/dev/null 2>../gcc-final.err || { tail -n 30 ../gcc-final.err; exit 1; }
  make install >/dev/null; cd ..
  finished gcc
fi

step done
$TARGET-gcc --version | head -1
# a program built by the new compiler, started through the new loader (scripts/valence-run). It only compiles if
# <limits.h> reaches glibc's part (see "glibc headers and start files" above).
printf '#include <stdio.h>\n#include <limits.h>\n#include <gnu/libc-version.h>\nint main(void){ printf("glibc %%s, PATH_MAX %%d\\n", gnu_get_libc_version(), PATH_MAX); return 0; }\n' > hello.c
$TARGET-gcc -O2 hello.c -o hello && $M/scripts/valence-run $B/hello
