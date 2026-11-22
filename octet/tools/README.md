# octet's test tools

Written while building stage 1 on the build machine (WSL2 Ubuntu, `/root/valence`, test roots in `/root/octet-test`);
paths at the top of each script say what they expect.

- `killtest/octet-killtest.sh`: upgrades a fresh root (valence-base) to 27 rebuilt packages and kills octet at random
  moments, then checks that every file is the old or the new version, that `/bin/sh`, libc and the loader work under
  chroot, and that the next `oct upgrade` completes (`octet-kt-check.py`). `octet-kt-mkrepo.sh` makes the 9999-r0 packages
  with a throwaway key.
- `bench/octet-bench.sh`: octet against apk-tools installing valence-base and a 244-package set into empty roots, cold
  and warm, 5 runs each (`octet-fx-bench.sh` is the run whose numbers are in ../README.md).
