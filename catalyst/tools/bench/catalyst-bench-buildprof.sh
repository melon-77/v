#!/bin/bash
# Builds the instrumented copy (catalyst-prof, CATALYST_PROF=1 prints step
# times) to /root/catalyst-test/bench/bin/oct-prof
set -u
S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # this folder
export PATH=/root/valence/hosttools/rust/bin:$PATH CARGO_HOME=/root/catalyst-cargo CARGO_TARGET_DIR=/root/catalyst-target/prof
cd $S/catalyst-prof
cargo build --release --locked --offline 2>&1 | grep -v '^\s*Compiling'
[ ${PIPESTATUS[0]} = 0 ] || exit 1
cp /root/catalyst-target/prof/release/oct /root/catalyst-test/bench/bin/oct-prof
ls -la /root/catalyst-test/bench/bin
