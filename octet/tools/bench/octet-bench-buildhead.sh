#!/bin/bash
# Builds the committed octet (HEAD, without anyone's uncommitted changes) for
# the benchmark: source exported to /root/octet-test/bench/src-head, target
# /root/octet-target/bench, binary copied to /root/octet-test/bench/bin/oct.
set -eu
S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # this folder
B=/root/octet-test/bench
export PATH=/root/valence/hosttools/rust/bin:$PATH CARGO_HOME=/root/octet-cargo
G="git -c safe.directory=* -C $S/octet"
rev=$($G rev-parse --short HEAD)
rm -rf --one-file-system $B/src-head
mkdir -p $B/src-head $B/bin
$G archive HEAD | tar -x -C $B/src-head
cd $B/src-head
CARGO_TARGET_DIR=/root/octet-target/bench cargo build --release --locked --offline 2>&1 | grep -v '^\s*Compiling' || true
cp /root/octet-target/bench/release/oct $B/bin/oct
echo "octet $rev (git archive HEAD, clean)" > $B/bin/oct.rev
sha256sum $B/bin/oct >> $B/bin/oct.rev
cat $B/bin/oct.rev
ls -la $B/bin
