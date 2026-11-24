#!/bin/bash
# release.sh VERSION: the files of a release, from the tested ISOs in out/, in out/release/vVERSION/:
#   valence-VERSION-x86_64.iso and valence-desktop-VERSION-x86_64.iso (the newest of each in out/, renamed),
#   SHA256SUMS of the two, SHA256SUMS.sig (signed with keys/valence-signing.rsa, the key the packages are signed
#   with) and valence-signing.rsa.pub to check it with:
#     openssl dgst -sha256 -verify valence-signing.rsa.pub -signature SHA256SUMS.sig SHA256SUMS
# WITH_CATALYST=1: also the newest pair built with VALENCE_PKG=catalyst, as valence-catalyst-VERSION-x86_64.iso and
#   valence-catalyst-desktop-VERSION-x86_64.iso (a test build that carries both sets).
# It checks what it made. Publishing (the GitHub release, the packages branch) is AGENTS.md, "Releases".
set -euo pipefail
M=$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)
V=${1:?usage: release.sh VERSION (for example 0.1.0)}
KEY=$M/keys/valence-signing.rsa PUB=$M/keys/trusted/valence-signing.rsa.pub
[ -f $KEY ] || { echo "release: no signing key at $KEY" >&2; exit 1; }
C=$(ls -t $M/out/valence-2*-x86_64.iso | head -n 1) D=$(ls -t $M/out/valence-desktop-2*-x86_64.iso | head -n 1)
R=$M/out/release/v$V; rm -rf $R; mkdir -p $R
# hard links: two names for the tested files, no copy of 1.2 GB
ln "$C" $R/valence-$V-x86_64.iso 2>/dev/null || cp "$C" $R/valence-$V-x86_64.iso
ln "$D" $R/valence-desktop-$V-x86_64.iso 2>/dev/null || cp "$D" $R/valence-desktop-$V-x86_64.iso
FILES="valence-$V-x86_64.iso valence-desktop-$V-x86_64.iso"
FROM="$(basename "$C"), $(basename "$D")"
if [ "${WITH_CATALYST:-0}" = 1 ]; then
  CC=$(ls -t $M/out/valence-catalyst-2*-x86_64.iso | head -n 1) CD=$(ls -t $M/out/valence-catalyst-desktop-2*-x86_64.iso | head -n 1)
  ln "$CC" $R/valence-catalyst-$V-x86_64.iso 2>/dev/null || cp "$CC" $R/valence-catalyst-$V-x86_64.iso
  ln "$CD" $R/valence-catalyst-desktop-$V-x86_64.iso 2>/dev/null || cp "$CD" $R/valence-catalyst-desktop-$V-x86_64.iso
  FILES="$FILES valence-catalyst-$V-x86_64.iso valence-catalyst-desktop-$V-x86_64.iso"
  FROM="$FROM, $(basename "$CC"), $(basename "$CD")"
fi
cp $PUB $R/
cd $R
sha256sum $FILES > SHA256SUMS
openssl dgst -sha256 -sign $KEY -out SHA256SUMS.sig SHA256SUMS
sha256sum -c SHA256SUMS
openssl dgst -sha256 -verify valence-signing.rsa.pub -signature SHA256SUMS.sig SHA256SUMS
echo "from: $FROM"
echo "key fingerprint (sha256 of the DER public key): $(openssl rsa -pubin -in valence-signing.rsa.pub -outform DER 2>/dev/null | sha256sum | cut -d' ' -f1)"
ls -l $R
