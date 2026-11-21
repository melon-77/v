#!/bin/bash
mkdir -p /root/vpm-test/bench
V=/root/vpm-target/release/vpm
$V --keys-dir /root/valence/keys/trusted dump-index /root/valence/repo/x86_64/Packages.adb --json > /root/vpm-test/bench/index.json
ls -la /root/vpm-test/bench/index.json
head -c 1500 /root/vpm-test/bench/index.json; echo
command -v gcc cc
/root/valence/hosttools/bin/apk --help 2>&1 | head -40
