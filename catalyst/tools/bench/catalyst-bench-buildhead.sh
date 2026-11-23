#!/bin/bash
# Builds the committed catalyst (HEAD, without anyone's uncommitted changes) for
# the benchmark: source exported to /root/catalyst-test/bench/src-head, target
# /root/catalyst-target/bench, binary copied to /root/catalyst-test/bench/bin/oct.
set -eu
S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # this folder
B=/root/catalyst-test/bench
export PATH=/root/valence/hosttools/rust/bin:$PATH CARGO_HOME=/root/catalyst-cargo
G="git -c safe.directory=* -C $S/catalyst"
rev=$($G rev-parse --short HEAD)
rm -rf --one-file-system $B/src-head
mkdir -p $B/src-head $B/bin
$G archive HEAD | tar -x -C $B/src-head
cd $B/src-head
CARGO_TARGET_DIR=/root/catalyst-target/bench cargo build --release --locked --offline 2>&1 | grep -v '^\s*Compiling' || true
cp /root/catalyst-target/bench/release/oct $B/bin/oct
echo "catalyst $rev (git archive HEAD, clean)" > $B/bin/oct.rev
sha256sum $B/bin/oct >> $B/bin/oct.rev
cat $B/bin/oct.rev
ls -la $B/bin
