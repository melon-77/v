#!/bin/sh
# valence-diag: the hardware and service report for problem reports now lives in valence-hwreport (installed on every
# Valence system: run "valence-hwreport" or "sudo valence-hwreport"). This wrapper stays because older instructions
# point at its URL:
#   curl -fsSL https://raw.githubusercontent.com/melon-77/valence/main/scripts/valence-diag.sh | sh > ~/valence-diag.txt 2>&1
# It prints the report from the newest valence-hwreport, or the installed one when it can't download it.
url=https://raw.githubusercontent.com/melon-77/valence/main/recipes/valence-base/files/usr/bin/valence-hwreport
echo "(valence-diag is now valence-hwreport; next time run: sudo valence-hwreport)"
if code=$(curl -fsSL "$url" 2>/dev/null) && [ -n "$code" ]; then
  sh -c "$code" valence-hwreport - </dev/null
elif command -v valence-hwreport >/dev/null 2>&1; then
  valence-hwreport - </dev/null
else
  echo "valence-diag: couldn't download $url and valence-hwreport isn't installed" >&2; exit 1
fi
