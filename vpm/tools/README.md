# vpm's test tools

Written while building stage 1 on the build machine (WSL2 Ubuntu, `/root/valence`, test roots in `/root/vpm-test`);
paths at the top of each script say what they expect.

- `killtest/vpm-killtest.sh`: upgrades a fresh root (valence-base) to 27 rebuilt packages and kills vpm at random
  moments, then checks that every file is the old or the new version, that `/bin/sh`, libc and the loader work under
  chroot, and that the next `vpm upgrade` completes (`vpm-kt-check.py`). `vpm-kt-mkrepo.sh` makes the 9999-r0 packages
  with a throwaway key.
- `bench/vpm-bench.sh`: vpm against apk-tools installing valence-base and a 244-package set into empty roots, cold
  and warm, 5 runs each (`vpm-fx-bench.sh` is the run whose numbers are in ../README.md).
