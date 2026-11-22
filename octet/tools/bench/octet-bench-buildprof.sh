#!/bin/bash
# Builds the instrumented copy (octet-prof, OCTET_PROF=1 prints step
# times) to /root/octet-test/bench/bin/oct-prof
set -u
S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # this folder
export PATH=/root/valence/hosttools/rust/bin:$PATH CARGO_HOME=/root/octet-cargo CARGO_TARGET_DIR=/root/octet-target/prof
cd $S/octet-prof
cargo build --release --locked --offline 2>&1 | grep -v '^\s*Compiling'
[ ${PIPESTATUS[0]} = 0 ] || exit 1
cp /root/octet-target/prof/release/oct /root/octet-test/bench/bin/oct-prof
ls -la /root/octet-test/bench/bin
