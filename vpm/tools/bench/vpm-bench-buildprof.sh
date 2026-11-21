#!/bin/bash
# Builds the instrumented copy (vpm-prof, VPM_PROF=1 prints step
# times) to /root/vpm-test/bench/bin/vpm-prof
set -u
S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # this folder
export PATH=/root/valence/hosttools/rust/bin:$PATH CARGO_HOME=/root/vpm-cargo CARGO_TARGET_DIR=/root/vpm-target/prof
cd $S/vpm-prof
cargo build --release --locked --offline 2>&1 | grep -v '^\s*Compiling'
[ ${PIPESTATUS[0]} = 0 ] || exit 1
cp /root/vpm-target/prof/release/vpm /root/vpm-test/bench/bin/vpm-prof
ls -la /root/vpm-test/bench/bin
