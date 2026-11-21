#!/bin/bash
# host-rust.sh: the Rust toolchain recipes use to compile Rust code for Valence, installed to hosttools/rust.
# Upstream's release binaries for the build machine (rustc, cargo, std). Their standard library is also the one for
# Valence's target, which has the build machine's triple (x86_64-unknown-linux-gnu). Same version as recipes/rust,
# which starts from this one; nothing from here ships in Valence.
# valence-build links Rust programs with Valence's gcc against Valence's glibc (see "Rust" in valence-build-recipe).
set -euo pipefail
M=$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd); V=1.98.1; H=$M/hosttools/rust; W=$M/work/host-rust
# bindgen (Mesa's NVK turns C headers into Rust with it): upstream's crate at a fixed version, built from its own
# lockfile (--locked); crates come from crates.io into sources/cargo like every Rust recipe's, checked by checksum.
# It loads the build machine's libclang (LIBCLANG_PATH in recipes/mesa-nvk).
BG=0.73.2 CB=0.29.4   # and cbindgen, the other direction (C headers for Rust code)
HT=x86_64-unknown-linux-gnu   # the build machine's own Rust: upstream's glibc builds (Ubuntu, or Valence itself)
_inst(){ PATH="$H/bin:$PATH" CARGO_HOME=$M/sources/cargo $H/bin/cargo install --quiet --locked --force --root $H "$@"; }
bindgen(){ $H/bin/bindgen --version 2>/dev/null | grep -q " $BG$" || _inst bindgen-cli@$BG
  $H/bin/cbindgen --version 2>/dev/null | grep -q " $CB$" || _inst cbindgen@$CB
  $H/bin/bindgen --version; $H/bin/cbindgen --version; }
if [ -x $H/bin/rustc ] && $H/bin/rustc --version | grep -q "^rustc $V "; then echo "host rust $V already installed"; bindgen; exit 0; fi
rm -rf $W $H; mkdir -p $W; cd $W
for c in rustc-$V-$HT cargo-$V-$HT rust-std-$V-$HT; do
  xz -dc $M/sources/$c.tar.xz | tar -xf -
  $c/install.sh --prefix=$H --disable-ldconfig --without=rust-docs >/dev/null
done
cd /; rm -rf $W
$H/bin/rustc --version
$H/bin/rustc --print target-list >/dev/null
[ -d $H/lib/rustlib/$HT/lib ] || { echo "host-rust: the standard library is missing" >&2; exit 1; }
bindgen
echo "host rust: $H"
