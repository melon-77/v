# catalyst's test tools

Written while building stage 1 on the build machine (WSL2 Ubuntu, `/root/valence`, test roots in `/root/catalyst-test`);
paths at the top of each script say what they expect.

- `killtest/catalyst-killtest.sh`: upgrades a fresh root (valence-base) to 27 rebuilt packages and kills catalyst at random
  moments, then checks that every file is the old or the new version, that `/bin/sh`, libc and the loader work under
  chroot, and that the next `oct upgrade` completes (`catalyst-kt-check.py`). `catalyst-kt-mkrepo.sh` makes the 9999-r0 packages
  with a throwaway key.
- `bench/catalyst-bench.sh`: catalyst against apk-tools installing valence-base and a 244-package set into empty roots, cold
  and warm, 5 runs each (`catalyst-fx-bench.sh` is the run whose numbers are in ../README.md).
- `killtest/catalyst-powercut.sh`: the same kind of check after a power cut instead of a kill. The root lives in a file
  system of its own (ext4, btrfs or XFS) behind a device-mapper device; at a random moment of a change the device is
  suspended and its image copied as the disk was then, which is mounted, recovered and compared with the root before
  and after (`catalyst-pc-fsync.py` is the second process whose fsyncs put only names on disk). Needs root.
- `bench/catalyst-strict.sh` (installs) and `bench/catalyst-strict2.sh` (removals, upgrades, an HTTP repository): the
  strict comparison with apk of ../README.md, 15 runs per tool and case in ABBA order, every run kept;
  `catalyst-strict-stats.py` gives the verdicts. `bench/catalyst-strict-fs.sh` runs them on a file system made for the
  run (btrfs mounted as Valence mounts its root, ext4 or XFS). Both tools are run from fresh copies in the directory of
  the run, so that a cold start does not measure where a build left a binary on the disk (`COPY_TOOLS=0`: from where
  they are).
