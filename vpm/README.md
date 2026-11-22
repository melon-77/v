# vpm

vpm is Valence's own package manager, written in Rust (roadmap phase 10:
stage 1 engine plus stage 2 CLI commands, commit hooks, repair, and
installer/ISO integration via `VALENCE_PKG=vpm`).

vpm reads the same files apk reads, the ones Valence's builder makes:
- the signed apk v3 repository index, `Packages.adb`;
- the packages, `name-version.apk`.

It follows apk-tools 3.0.8 where it matters, which is the format,
signatures, versions, dependency choice, commit hooks and scripts. The
format specification is `apk3-format.md`; the `§` references in the code
point into it.

vpm manages only roots it created itself (`--initdb`). It never reads or
writes apk's database (`lib/apk/db`, `etc/apk/world`). From apk's
configuration it uses the repository list, the trusted keys,
`etc/apk/arch`, and `commit_hooks.d`.

## Building

```sh
cargo build --release      # target/release/vpm
cargo test --release       # unit tests; tests/real_data.rs also runs over
                           # Valence's repository when it is present
                           # (VPM_REPO, VPM_KEYS)
```

The dependencies are pinned in `Cargo.lock`:
- flate2, for deflate;
- zstd;
- openssl, for SHA-1/256/512 and RSA. It links the system's libcrypto and libssl.
- ureq with native-tls, for http and https;
- libc (also used to link the system's `libsqlite3` directly via `build.rs` with no extra crate).

There is no async runtime. Parallel work uses std threads.

## Commands

```
vpm [--root DIR] [--repository INDEX]... [-X|--add-repository INDEX]...
    [--keys-dir DIR] [--arch ARCH] [--print-arch] [--initdb] [--plain]
    [--simulate|-s] [--no-scripts] [--no-commit-hooks] [--jobs N]
    [--allow-rollback] [--allow-untrusted] COMMAND

  add [-u] NAME... add to the world and install. NAME may carry a constraint
                   ('foo>=1.2') or be a provided name ('so:libz.so.1').
                   An already installed NAME keeps its version unless -u is
                   given (-u upgrades the named packages, not transitive
                   dependencies).
  del NAME...      remove from the world; uninstall what is no longer needed
  upgrade          move everything to the newest versions
  update           refresh repository indexes (exits 1 if any repo failed or
                   fell back to cache)
  list             the installed packages ([world] = asked for)
  search [-x] [-d] [PAT...]
                   search repositories by name or provides (-x exact match,
                   -d include descriptions)
  info [-e|-a|-d|-L|-R|-P|-s|-W] [NAME...]
                   show package info (-e installed check, -W who owns PATH;
                   no args lists installed package names)
  fix [NAME...]    verify hashes in /Programs and repair live-tree files
  cache clean      remove downloaded .apk files from var/cache/vpm
  verify FILE...   check signature, identity and every file's content
  dump FILE [--json], dump-index FILE [--json], manifest FILE
```

Defaults:
- `--root` defaults to `/`.
- Repositories come from `ROOT/etc/apk/repositories`, then from the `*.list`
  files in `ROOT/etc/apk/repositories.d` and `ROOT/lib/apk/repositories.d`.
  `--repository` replaces that list; `-X` / `--add-repository` appends to it.
  Unreachable repositories warn and are skipped (signature/integrity failures
  and having zero usable repositories remain fatal).
  Each line gives one repository:
  - a `Packages.adb` path or URL, as Valence writes them. Packages are
    found next to the index.
  - or a base URL, optionally with `v3`. The index is then
    `URL/ARCH/Packages.adb`, and packages are at
    `URL/<package arch>/<name>-<version>.apk`.
  - An index's own `pkgname-spec` replaces either default.
- Keys come from `ROOT/etc/apk/keys`, then `ROOT/lib/apk/keys`. These are
  PEM public keys, read inside the root. `--keys-dir` replaces both.
- `--jobs` defaults to one thread per CPU, at most 16.

Progress:
- On a terminal, a full-screen view shows one line per package being
  fetched or unpacked, and an overall bar.
- Otherwise, or with `--plain`, vpm prints one line per package.

Exit status: 0 on success; 1 on an error or when the switch reported
errors. In the second case the last line starts with `ERRORS:` instead of
`OK:`.

## Layout of a root

```
ROOT/Programs/<name>/<version>/...         the package's unpacked files only
ROOT/Programs/<name>/Current -> <version>  the version in use
ROOT/var/lib/vpm/state.db                  SQLite primary metadata database
                                           (packages, deps/provides, tags,
                                           triggers, scripts, indexed files,
                                           and world)
ROOT/var/lib/vpm/world                     plain-text mirror of world (kept
                                           in sync for installers & scripts)
ROOT/var/lib/vpm/journal                   the switch in progress
ROOT/var/lib/vpm/lock                      held while vpm (or a script it runs) runs
ROOT/var/lib/vpm/indexes/                  what each repository offered last
ROOT/var/cache/vpm/                        downloaded packages and indexes
```

`state.db` uses `PRAGMA journal_mode = DELETE; PRAGMA synchronous = FULL;` so
read-only mounts (such as live squashfs ISOs) can open `state.db` cleanly with
no `-wal`/`-shm` sidecars, while keeping full ACID durability inside
transactions. Legacy roots with per-package `.vpm/{info,files,scripts}`
directories are migrated automatically into `state.db` on the first writable
open and remain readable by read-only commands beforehand.

The usual tree is made from those folders:
- A regular file outside `etc/` and `boot/` is a **hard link** to its copy
  under `Programs`: the same inode under a second name. `/Programs` and
  `/usr` are one file system; if a target sits on another device (`EXDEV`),
  vpm falls back to a synced atomic copy.
- Files under `boot/` are always **synced copies**, because `/boot` is a
  separate FAT32 partition on installed systems (so upgrades and squashfs
  unpacks never hit cross-device link errors).
- Symlinks and device nodes are recreated.
- Directories get the package's mode and owner. Directories that already
  exist are left alone.
- Files and symlinks under `etc/` are preserved when edited by the admin.
  Files under `etc/` are **copies**. On upgrade, an `etc/` file or symlink
  that the admin did not change is replaced; a changed one is kept, and the
  new one is written as `<file>.vpm-new`.

Installed package metadata lives in `state.db` (`packages.state = 'installed'`),
paired with `Programs/<name>/Current -> <version>` on disk. File ownership
queries (`vpm info -W`) resolve in O(log N) via `idx_pkg_files_path`.

## The transaction

1. **Open.** Take the lock. Migrate a legacy `.vpm/` root into `state.db` if
   needed. Finish a leftover journal (see Recovery). Delete `.partial`
   directories, stale non-installed DB rows, version directories that are not
   current, and interrupted downloads.
2. **Resolve.** Work out, from the indexes and what is installed, the
   packages the world needs (see Resolution). If nothing changes, stop
   without running commit hooks.
3. **Pre-commit hooks.** Unless `--no-scripts`, `--no-commit-hooks`, or
   `--simulate` is set, executable hooks are run with `pre-commit` (e.g.
   `valence-snapshot auto`). A failing `pre-commit` hook aborts before
   anything is fetched or changed.
4. **Fetch and unpack in parallel.** Worker threads fetch and unpack the
   new packages; no package is unpacked twice.
   - **Fetch** (remote only): a package is downloaded into the cache. Its
     size is checked against the index, and its identity is checked before
     its signature blocks are read.
   - **Unpack:** each package is streamed into
     `Programs/<name>/<version>.partial/`, and every file is checked
     against its hash as it is written. When complete, the directory is
     renamed to `<version>`, and the worker returns its extracted metadata
     and file records in memory.
   - Unsafe content is refused: absolute paths, `..`, anything below one
     of the package's own symlinks, hard links to anything but an earlier
     file of the package, and any entry in vpm's own directories.
   - Meanwhile, a helper thread keeps flushing the file system.
5. **Check and stage.** Probe `state.db` via `idx_pkg_files_path` for only
   the paths introduced by new packages (loading unchanged packages only when
   a path actually collides, and reusing the in-memory records from unpack
   with no double read). Refuse if two packages claim the same file under
   apk's ownership rules (`replaces`, same origin, or identical content).
   Then stage all unpacked metadata, file manifests, and scripts into
   `state.db` (`state = 'staged'`) in a single SQLite transaction.
6. **Switch.** This is the only step that touches the live tree, and it
   comes last.
   1. `syncfs`, so everything unpacked is on disk. Then write the journal
      (which package goes from which version to which), and fsync it.
   2. Per package, dependencies first:
      - the pre-install or pre-upgrade script;
      - its entries placed (see "How entries are placed" below);
      - the old version's entries that no package owns any more removed,
        but only if they are still what it installed;
      - `Current` flipped (new symlink, then `rename`), and the old
        version's directory made 0700, since its set-id files are no longer
        in the live tree;
      - the post-install or post-upgrade script.
   3. Removed packages: the pre-deinstall script, their entries,
      `Current`, then the post-deinstall script.
   4. The world is written.
   5. The triggers of affected packages run.
   6. `syncfs`. The journal is deleted, then the old versions.
7. **Post-commit hooks.** Run after the switch (and also if an error occurs
   after `pre-commit` ran). Failures warn and do not abort remaining hooks.

Commit hooks:
- Searched in `etc/vpm/commit_hooks.d`, `etc/apk/commit_hooks.d`,
  `usr/lib/vpm/commit_hooks.d`, `lib/vpm/commit_hooks.d`,
  `usr/lib/apk/commit_hooks.d`, and `lib/apk/commit_hooks.d` (first match
  per filename wins; deduplicated by `(dev, ino)` for merged `/usr`;
  sorted by filename; dotfiles and non-executable files skipped).
- Run chrooted (when `root != "/"`), with `PATH`, `APK_SCRIPT` (`pre-commit`
  or `post-commit`), and `VALENCE_SNAPSHOT_TEXT` (if set), without
  inheriting the root lock descriptor.
- `vpm fix` runs `pre-commit` and `post-commit` hooks only when entries
  actually need repairing.

How entries are placed:
- An entry that replaces an existing one is made under a temporary name
  next to its target, then `rename`d over it.
- A new name is `link`ed or `symlink`ed straight onto its path.
- A new directory is made under a temporary name with its final mode and
  owner, then renamed into place.

All three are atomic: every path is at every moment either the old entry
or the new one.

Scripts:
- They run as apk runs them: `chroot ROOT`, the script's path relative to
  the root as `argv[0]`, the versions as arguments, a clean environment
  (`PATH`, `APK_SCRIPT`, `APK_PACKAGE`) and umask 022.
- Nothing is mounted for them. Missing basic device nodes in `/dev` are
  created, as apk does when it cannot bind-mount `/dev`.
- A script whose `#!` interpreter is not installed yet waits until all
  packages are placed. bash's post-install, for example, needs busybox's
  `/bin/sh`; under apk it fails.
- After each script, directories are resolved afresh, because a script may
  have replaced one by a symlink.

## Recovery

The journal is written only after everything it points at is complete on
disk. A run that finds a journal finishes it first (roll forward):
- every step is idempotent;
- placed packages and scripts that ran are marked in the journal, so they
  are not placed or run again.

Leftover `.partial` and stale version directories are deleted.

So an upgrade killed at any point leaves every path either the old or the
new entry, with a working `/bin/sh`, libc and dynamic loader, and the next
run completes the upgrade. The kill test checks exactly this (see Tests).

Limits of the recovery:
- A script killed mid-run is run again by the recovery, so package scripts
  must tolerate that (apk's would be run again by a re-install too).
- Scripts inherit the lock. A script that outlives a killed vpm keeps the
  root locked until it ends, and the next vpm refuses to start beside it
  instead of running the same recovery and script at the same time.

## Trust and limits

Trust works as in apk v3:
- An index must carry a valid signature by a trusted key (RSA PKCS#1 v1.5,
  SHA-512, `hash_alg` 3/4/5).
- A package is trusted when its identity matches the signed index: the
  SHA-256 or SHA-512 of its ADB block. Failing that, its own signature
  must verify.
- An identity mismatch is fatal, even with `--allow-untrusted`.
- Name, version and arch must equal the index entry.

What an untrusted file or a hostile mirror can make vpm do is bounded:
- **Metadata size:**
  - ADB block: at most 16 MiB in a package and 64 MiB in an index.
    Valence's largest is 1.3 MB.
  - At most 8 SIG blocks.
  - zstd window at most 2^27.
- **Downloads:**
  - A package download stops as soon as it exceeds the size the index
    gives. A `Content-Length` that differs from it is refused before any
    data is read.
  - An index download stops at 64 MiB.
  - A transfer slower than 1 KiB/s over 30 s is abandoned.
  - An `https://` repository is only fetched over https: no redirect to
    http is followed.
- **Rollback:** for each repository, vpm remembers the newest build-time
  and the newest version of every package name it offered.
  - A validly signed index that is older is refused unless
    `--allow-rollback` is given. Older means an older newest build-time,
    or a name offered only in a lower version: a mirror replaying an old
    index to hold back fixes.
  - A downloaded index replaces the cached one only after it was verified
    and found fresh.
  - vpm warns when an installed package is newer than anything the
    repositories offer.
  - A root that never saw a repository has nothing to compare with. That
    would need a signed timestamp in the index.
- **Confinement:**
  - All paths are resolved inside the root (`..` and absolute symlinks are
    clamped at ROOT).
  - No entry may be, or resolve into, `Programs`, `var/lib/vpm` or
    `var/cache/vpm`.
  - vpm's own directories are never followed through a symlink.
  - Versions must be valid apk versions.
  - Live files are hashed, and their owner and mode set, through
    `O_NOFOLLOW` descriptors.
  - Keys, the repository list, `etc/apk/arch`, `passwd` and `group` are
    read inside the root, as regular files of bounded size.

## Resolution

vpm uses a greedy resolver that makes apk's choices. The order follows
apk's `compare_providers`:
1. When keeping (not upgrading), the installed package.
2. The higher provided version of the requested name.
3. The newer version of the same package.
4. The higher `provider_priority` (16 bits).
5. The installed package.
6. The earlier repository.

Candidates are refused when they would break what is already decided:
- a versioned name provided by two packages;
- a conflict (`!name`) already in force;
- a package whose own conflicts hit what is chosen.

Other rules:
- A virtual provider with no version and no priority is not picked unless
  it is asked for by name.
- A later constraint that an earlier, unconstrained choice made impossible
  is learned, and resolution starts over with it.
- `install_if` tries every version of a name, best first.
- The world is taken in name order. An install_if package comes right after
  the packages that trigger it.

Compared with `apk add --simulate` on Valence's repository, 25 requests
(base, desktops, Qt, LLVM, Rust, Go, docs, constraints, conflicts) give the
same package sets, and the same order in 22 of the 23 that resolve. The 10
synthetic edge cases from the spec review match apk too.

## What stage 1 does not do yet

Database and commands:
- vpm manages its own roots only: no import of an apk-managed system, no
  `apk` database, no `/` without `--initdb` history.
- `vpm fix` checks hashes of regular files in `Programs/` and repairs the
  live tree; corrupt `Programs/` files or `state.db` metadata are reported as
  errors and need a reinstall.
- No dot-files (`.list`, `.trigger`...).
- No `--latest` or pinning to a tagged repository (`name@tag` is refused).

Repositories and packages:
- Only v3 indexes. A keyword-less base URL is treated as v3, where apk
  would look for a v2 `APKINDEX.tar.gz`. Repository components and
  `set` variables are not supported.
- Packages are told apart by name and version, not by identity, so a
  rebuild with the same version is not picked up.
- Packages with `installed-size` 0 still run their scripts (apk skips
  them; only hand-made packages have that).

Resolution:
- This is not apk's full solver. It has no backtracking beyond learning
  constraints, and no `provider_priority` tie-breaking across
  repositories with tags.

Scripts:
- Nothing is mounted for them. A script that needs `/proc` or `/sys` fails
  as it would under apk without them.
- They run as root in a plain chroot, which is not a security boundary.

Live tree:
- Operations on the live tree resolve paths in userspace and then use
  them, so another process changing the tree at the same moment could
  race them. `openat2(RESOLVE_IN_ROOT)` would close this.
- File conflicts are detected by literal path, so a symlinked directory in
  one package can still route another package's file onto a third's
  without a conflict being reported. The switch refuses only paths that
  resolve into vpm's own directories.
- Durability is `syncfs` before the journal and at the end. Only process
  kills were tested, not power loss.

Benchmarks:
- xbps was not measured. The script exists, but it downloads Void's static
  binaries and needs approval for that.

## Measured

Setup:
- Machine: i5-12450H, 12 threads, 24 GB RAM, WSL2 (kernel 6.18), ext4 in a
  VHDX.
- The machine was shared with other jobs, so load averages ran from 3 to 11
  during the runs. Treat the absolute numbers as rough.
- `vpm-bench.sh`, 5 runs per configuration, apk and vpm alternating.
- apk-tools is 3.0.8 with `--no-cache`. vpm reads the local repository in
  place.
- Cold runs drop the caches first.
- `+sync` adds a `sync -f` right after the command. vpm syncs itself; apk
  leaves writeback to the kernel.

The sets:
- **base** is `valence-base`: 39 packages, 60 MB.
- **big** is valence-base plus the 30 largest packages: 244 packages,
  3.39 GB, 80,315 files.

| set | cache | scripts | apk wall | vpm wall | vpm/apk | apk +sync | vpm +sync | CPU apk / vpm |
|---|---|---|---|---|---|---|---|---|
| base | cold | no | 0.385 s | 0.225 s | 0.59 | 0.466 s | 0.231 s | 0.29 / 0.30 s |
| base | cold | yes | 0.396 s* | 0.246 s | 0.62 | 0.464 s | 0.255 s | 0.31 / 0.31 s |
| base | warm | no | 0.283 s | 0.153 s | 0.54 | 0.357 s | 0.157 s | 0.27 / 0.32 s |
| base | warm | yes | 0.300 s* | 0.177 s | 0.59 | 0.358 s | 0.183 s | 0.29 / 0.33 s |
| big | cold | no | 12.42 s | 6.45 s | 0.52 | 13.64 s | 6.46 s | 11.3 / 11.7 s |
| big | warm | no | 11.38 s | 5.14 s | 0.45 | 13.03 s | 5.15 s | 11.3 / 13.7 s |

\* apk exits 1 with scripts: bash's post-install finds no `/bin/sh` yet.
vpm defers that script.

Wall times are medians of 5 runs. Max RSS is 10-17 MB for apk; for vpm,
22 MB on base and about 110-130 MB on big. The installed trees have the
same content: vpm's are larger by its `.vpm` metadata, and have twice the
names, since a file appears under `Programs` and in the live tree.

Against the reviewed commit (9a02b8f), in the same session:

| set | 9a02b8f | now | change |
|---|---|---|---|
| base | 0.377 s | 0.190 s | -50% |
| big | 7.28 s | 6.08 s | -16% |

These runs are warm, with `--no-scripts`, 5 runs each, alternating.

Where the speed comes from:
- **Parallel unpack.** A package is inflated, hashed and written in one
  streaming pass. Then a hard link per file, instead of a second copy.
- **Writeback overlapped with unpacking.** The `syncfs` before the journal
  used to take 30-37% of an install; now it finds little left to write.
  The unpack phase takes longer while the flusher runs, but the switch
  shrinks more.
- **Cheaper placing of new names.** New names are linked directly, with no
  temporary name, rename or hash of a temporary name.

The next speedups are measured but not done:
- zstd-compressed packages: the repository is deflate today. This helps
  apk even more, which is single-threaded and decompression-bound.
- Placing packages without scripts on several threads.

## Tests

- **`cargo test`:** 37 unit tests and 8 tests over the real repository:
  - every package verified against the index;
  - tampering of every kind is rejected;
  - damaged input never panics.
- **End-to-end tests (a) to (c),** run by `vpm-e2e-all.sh` and the
  scripts it calls (`vpm-e2e-a.sh`, `vpm-e2e-b.sh`, `vpm-e2e-c.sh`).
  These scripts live outside the crate, in the scratchpad folder.
  - (a) and (b): vpm and apk produce identical trees for install, add and
    del.
  - (c): an upgrade killed at 11 points (`VPM_CRASH_AT`, which SIGKILLs
    itself) is completed by the next run.
- **Kill test,** run by `vpm-killtest.sh` with the scripts named
  `vpm-kt-*` beside it. It upgrades valence-base to 27 rebuilt packages
  and SIGKILLs vpm's process group at random moments: during file
  placement, between journal steps, inside scripts, during the deletion of
  old versions, and twice in a row. After each kill it checks that:
  - every path is the old or the new entry;
  - `/bin/sh`, libc and `ld.so` work under chroot;
  - the next run completes the upgrade and leaves a root identical to an
    uninterrupted one.

  Results on the current code: 310 rounds, 298 of them killed while
  running and 82 with a package half placed. There were 0 failures. One
  earlier failure, a directory left 0700 by a kill between `mkdir` and
  `chmod`, led to the atomic directory creation described under The
  transaction.
