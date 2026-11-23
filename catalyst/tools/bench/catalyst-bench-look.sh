#!/bin/bash
mkdir -p /root/catalyst-test/bench
V=/root/catalyst-target/release/oct
$V --keys-dir /root/valence/keys/trusted dump-index /root/valence/repo/x86_64/Packages.adb --json > /root/catalyst-test/bench/index.json
ls -la /root/catalyst-test/bench/index.json
head -c 1500 /root/catalyst-test/bench/index.json; echo
command -v gcc cc
/root/valence/hosttools/bin/apk --help 2>&1 | head -40
