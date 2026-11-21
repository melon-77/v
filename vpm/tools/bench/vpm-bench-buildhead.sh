#!/bin/bash
# Builds the committed vpm (HEAD, without anyone's uncommitted changes) for
# the benchmark: source exported to /root/vpm-test/bench/src-head, target
# /root/vpm-target/bench, binary copied to /root/vpm-test/bench/bin/vpm.
set -eu
S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # this folder
B=/root/vpm-test/bench
export PATH=/root/valence/hosttools/rust/bin:$PATH CARGO_HOME=/root/vpm-cargo
G="git -c safe.directory=* -C $S/vpm"
rev=$($G rev-parse --short HEAD)
rm -rf --one-file-system $B/src-head
mkdir -p $B/src-head $B/bin
$G archive HEAD | tar -x -C $B/src-head
cd $B/src-head
CARGO_TARGET_DIR=/root/vpm-target/bench cargo build --release --locked --offline 2>&1 | grep -v '^\s*Compiling' || true
cp /root/vpm-target/bench/release/vpm $B/bin/vpm
echo "vpm $rev (git archive HEAD, clean)" > $B/bin/vpm.rev
sha256sum $B/bin/vpm >> $B/bin/vpm.rev
cat $B/bin/vpm.rev
ls -la $B/bin
