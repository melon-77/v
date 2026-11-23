# catalyst

catalyst is Valence's own package manager; its command is `oct` (`oct add gcc`).
Both names are the owner's (22 November 2026). The command comes from the octet rule: an atom
is stable once its outer shell is full, like a system that is complete; and `oct add` sounds
like octet. The package manager was called vpm, then octet, and the documents in `docs/` still
use the first name.

catalyst is Valence's own package manager, written in Rust (roadmap phase 10:
stage 1 engine plus stage 2 CLI commands, commit hooks, repair, and
installer/ISO integration via `VALENCE_PKG=catalyst`).

catalyst reads the same files apk reads, the ones Valence's builder makes:
- the signed apk v3 repository index, `Packages.adb`;
- the packages, `name-version.apk`.

It follows apk-tools 3.0.8 where it matters, which is the format,
signatures, versions, dependency choice, commit hooks and scripts. The
format specification is `apk3-format.md`; the `§` references in the code
point into it.

catalyst manages only roots it created itself (`--initdb`). It never reads or
writes apk's database (`lib/apk/db`, `etc/apk/world`). From apk's
configuration it uses the repository list, the trusted keys,
`etc/apk/arch`, and `commit_hooks.d`.

## Building

```sh
cargo build --release      # target/release/oct
cargo test --release       # unit tests; tests/real_data.rs also runs over
                           # Valence's repository when it is present
                           # (CATALYST_REPO, CATALYST_KEYS)
```

The dependencies are pinned in `Cargo.lock`:
- flate2, for deflate;
- zstd;
- openssl, for SHA-1/256/512 and RSA. It links the system's libcrypto and libssl.
- ureq with native-tls, for http and https;
- libc.

There is no async runtime. Parallel work uses std threads.

## Commands

```
oct [--root DIR] [--repository INDEX]... [-X|--add-repository INDEX]...
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
  cache clean      remove downloaded .apk files from var/cache/catalyst
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
- Otherwise, or with `--plain`, catalyst prints one line per package.

Exit status: 0 on success; 1 on an error or when the switch reported
errors. In the second case the last line starts with `ERRORS:` instead of
`OK:`.

## Layout of a root

```
ROOT/Programs/<name>/<version>/...         the package's files at their paths
ROOT/Programs/<name>/<version>/.catalyst/info   name, version, arch, origin, depends,
                                           provides, install-if, identity, ...
ROOT/Programs/<name>/<version>/.catalyst/files  one line per entry: kind, mode,
                                           owner, size, mtime, hash, target, path
ROOT/Programs/<name>/<version>/.catalyst/scripts/<type>
ROOT/Programs/<name>/Current -> <version>  the version in use
ROOT/var/lib/catalyst/world                     what was asked for
ROOT/var/lib/catalyst/journal                   the switch in progress
ROOT/var/lib/catalyst/lock                      held while catalyst (or a script it runs) runs
ROOT/var/lib/catalyst/indexes/                  what each repository offered last
ROOT/var/lib/catalyst/index/                    the lookup index of what is installed
ROOT/var/lib/catalyst/unsynced                  adds since this boot were not synced (see Unsynced adds)
ROOT/var/cache/catalyst/                        downloaded packages and indexes
ROOT/var/cache/catalyst/parsed-<16 hex>         a verified repository index, parsed
```

The usual tree is made from those folders:
- A regular file outside `etc/` and `boot/` is a **hard link** to its copy
  under `Programs`: the same inode under a second name. `/Programs` and
  `/usr` are one file system; if a target sits on another device (`EXDEV`),
  catalyst falls back to a synced atomic copy.
- Files under `boot/` are always **synced copies**, because `/boot` is a
  separate FAT32 partition on installed systems (so upgrades and squashfs
  unpacks never hit cross-device link errors).
- Symlinks and device nodes are recreated.
- Directories get the package's mode and owner. Directories that already
  exist are left alone.
- Files and symlinks under `etc/` are preserved when edited by the admin.
  Files under `etc/` are **copies**. On upgrade, an `etc/` file or symlink
  that the admin did not change is replaced; a changed one is kept, and the
  new one is written as `<file>.catalyst-new`.

A package is installed if and only if `Programs/<name>/Current` exists.

## The lookup index

The folders are the record of what is installed. `var/lib/catalyst/index/`
is a copy of what they say, so that a change doesn't have to read the
record of every installed package:
- `meta` holds every installed package's `info`, so opening a root reads
  one file instead of one per package. It is in a compact binary form,
  which reads back in about a fifth of the time the text records take.
- `files/XXX` (4096 buckets by a hash of the path) say which packages have
  an entry at each path, files apart from directories.
- `aliases` lists the entries that reach another place through a directory
  symlink, such as `lib64 -> usr/lib`.
- `state` names every package's version and the tree's directory symlinks
  as they were when the index was written.

A change reads the records of the packages it changes, of those that
share a file path with them, and of those with a trigger. A trigger may
fire for a directory the change modifies, and the index says whether that
directory is still installed. The change rewrites only the buckets of its
own paths. Two kinds of change read every record and rebuild the index:
one that touches a directory symlink, and one that installs a trigger
with a pattern, since that trigger is given every installed directory that
matches.

The index is used only when `state` matches the root exactly: the version
every `Current` names, and every directory symlink's target. Otherwise,
and always while there is a journal, catalyst reads the records. A missing or
outdated index is rebuilt when a root is opened for a change, and `oct fix`
always rebuilds it. The index is canonical: one updated change by change
is byte for byte the one a rebuild writes, and the tests compare the two.

Every file of the index is checked when it is read. `state` holds which
buckets are not empty, the SHA-256 of `meta` and of `aliases`, and a sum
of itself; each bucket starts with the SHA-256 of the rest of it. Before a
change goes through the index, everything it may look up there is read
and checked: the buckets of its paths, of their directories and of their
canonical places, and the aliases. A missing, truncated or changed file is
found there, before anything is switched, and the change then reads every
record and rebuilds the index.

A big change touches thousands of buckets (installing Cataclysm: DDA on a
system of 244 packages, about 3,600 of the 4,096), so they are read and
written on several threads. A bucket file that exists is rewritten in
place rather than through a temporary name: creating and renaming files in
one directory would make the threads wait for each other. Nothing reads
the index while it is written, and every bucket has its checksum.

Threads are started only when they pay. Starting them costs about 1.4 ms,
so work that would take less than about 3 ms on one thread stays there:
on a warm cache, the 244 `Current` links of a big system take 0.8 ms on
one thread and 1.5 ms on sixteen. A cold cache, where every read waits
for the disk, and the thousands of buckets of a big install go to the
threads. The time the first items took tells which it is.

Inside a switch the index is updated before the final `syncfs`. The
journal names the records the switch reads and the index it was planned
with, and finishing an interrupted switch reads the same records with that
index, as long as the index is still the one from before the switch. If
the crash came while the index was being updated, the recovery reads every
record. A rebuild outside a switch writes the new index beside the old
one, syncs it, then swaps it in.

A package's directories, for the index, are the ones its record lists.
Real packages list every directory of their files (all 1,953 on
valence-base); a hand-made record that leaves one out is read slightly
differently with the index than without it.

## The transaction

1. **Open.** Take the lock. Finish a leftover journal (see Recovery).
   Delete `.partial` directories, version directories that are not current,
   and interrupted downloads. Rebuild the lookup index if it is missing or
   out of date.
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
     renamed to `<version>`.
   - Unsafe content is refused: absolute paths, `..`, anything below one
     of the package's own symlinks, hard links to anything but an earlier
     file of the package, and any entry in catalyst's own directories.
   - Meanwhile, when the packages hold 32 MiB or more, a helper thread
     keeps flushing the file system, so that the `syncfs` before the
     journal finds little left to write. A smaller job does without it:
     its `syncfs` is cheap anyway.
5. **Check.** Refuse if two packages claim the same file. The ownership
   rules follow apk: `replaces`, the same origin, or identical content.
   Every record the switch will read must be readable, or nothing is
   changed (see Recovery): the old versions' records and those of the
   installed packages that share a path with the change, or every
   installed package's when the lookup index can't be used.
6. **Switch.** This is the only step that touches the live tree, and it
   comes last.
   1. The journal (which package goes from which version to which) is
      written under a temporary name; one `syncfs` puts it on disk
      together with everything unpacked; only then is it renamed into
      place, and its directory fsync'ed. A journal is never there before
      what it points at. (An add that is not synced skips this `syncfs`
      and fsync, and the final `syncfs` of step 6: see Unsynced adds.)
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
   6. The lookup index is updated, or rebuilt. `syncfs`. The journal is
      deleted (without a sync of its own: by then every step is marked
      done in it, so a journal that a crash brings back is finished again
      without changing anything), then the old versions.
7. **Post-commit hooks.** Run after the switch (and also if an error occurs
   after `pre-commit` ran). Failures warn and do not abort remaining hooks.

Commit hooks:
- Searched in `etc/catalyst/commit_hooks.d`, `etc/apk/commit_hooks.d`,
  `usr/lib/catalyst/commit_hooks.d`, `lib/catalyst/commit_hooks.d`,
  `usr/lib/apk/commit_hooks.d`, and `lib/apk/commit_hooks.d` (first match
  per filename wins; deduplicated by `(dev, ino)` for merged `/usr`;
  sorted by filename; dotfiles and non-executable files skipped).
- Run chrooted (when `root != "/"`), with `PATH`, `APK_SCRIPT` (`pre-commit`
  or `post-commit`), and `VALENCE_SNAPSHOT_TEXT` (if set), without
  inheriting the root lock descriptor.
- `oct fix` runs `pre-commit` and `post-commit` hooks only when entries
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

A `journal.new` left by a crash was never in effect (the crash came
before the switch began) and is deleted.

While there is a journal, nothing but the recovery uses the lookup index.
The recovery reads the records the switch read, with the index from
before the switch (see The lookup index), or every record when that index
is gone.

Leftover `.partial` and stale version directories are deleted by the
next run after an interrupted one: a run that finished a journal, or that
finds the `var/lib/catalyst/dirty` mark, which a change sets before it unpacks
and removes once it has succeeded. A clean run skips that walk over
`Programs`.

A record that can't be read (a `.catalyst/info` or `.catalyst/files` of an
installed version that is missing or damaged) stops every change that
needs it before anything is unpacked or a journal is written, with a
message that names it. `oct fix` checks both files of every record and
rebuilds a damaged one from the signed package of that version in the
repositories, verifying the whole package file first, and then checks the
files against it. It does this before it finishes a leftover journal,
since finishing the journal may need the record. While a record stays
unreadable, `oct fix` repairs nothing: who owns which file can't be known.

So an upgrade killed at any point leaves every path either the old or the
new entry, with a working `/bin/sh`, libc and dynamic loader, and the next
run completes the upgrade. The kill test checks exactly this (see Tests).

Limits of the recovery:
- A script killed mid-run is run again by the recovery, so package scripts
  must tolerate that (apk's would be run again by a re-install too).
- Scripts inherit the lock. A script that outlives a killed catalyst keeps the
  root locked until it ends, and the next catalyst refuses to start beside it
  instead of running the same recovery and script at the same time.

## Unsynced adds

A change that only adds new packages, and unpacks less than 32 MiB, is
not synced. It skips the `syncfs` before the switch, the fsync that puts
the journal in effect, and the final `syncfs`. The owner decided this on
23 November 2026 (ROADMAP, "Decided"): those three flushes were about
9.5 ms of a 22 ms `add nano`, and what they cover, the power going during
an install or in the seconds after it, is rare. Every other change is
synced exactly as described above: an upgrade, a removal, an add that
upgrades something, an install of 32 MiB or more.

What a power cut can cost is the packages being added, and nothing else:
- **Only new names.** An unsynced switch creates entries that were not
  there. Before it replaces anything that is there, and before the first
  script or trigger runs, it calls `syncfs`. From there on it is an
  ordinary synced switch, final `syncfs` included.
- **The world is appended to,** not rewritten, so the entries already in
  it are never at risk. The file is then not sorted until the next synced
  change; it is read as a set. If the new world is not the old one plus
  entries, the new file is fsync'ed before it replaces the old one.
- **A killed process loses nothing.** Without a power cut the page cache
  is intact, so the journal works as always: the next run finishes the
  switch, after a `syncfs`.

Finding out afterwards. Before the first unsynced add after a boot,
catalyst writes `var/lib/catalyst/unsynced`, with an fsync: the kernel's
boot id and the packages installed at that moment. A run that finds that
file with another boot id knows the system went down since adds that may
not have reached the disk. Before anything else it checks every package
that is not in that list:
- A package is whole when its record can be read, every entry of it is
  there, no file is shorter than recorded and no script is empty. What a
  power cut takes from unsynced files is their content: they come back
  short or empty.
- A package that is not whole is removed, with what it had in the live
  tree: its hard-linked files (found by inode, not through its record,
  which may be what was lost), and its symlinks, its untouched or empty
  copies under `etc/` and its empty directories where no other package
  has an entry.
- A journal left by an unsynced add is finished only if everything it
  installs is whole and nothing had to be removed. Otherwise it is
  deleted, and the versions it had not made current go the same way.
- The lookup index is rebuilt unless it has every entry of the recent
  packages.

After a clean shutdown all of that finds nothing to do; it takes about a
millisecond per small package added in the earlier boot. catalyst cannot
tell a clean shutdown from a power cut, which is why it checks after
every reboot that follows an unsynced add.

What is left to the user after a power cut:
- a removed package that the world still names is installed again by the
  next change, or by `oct add` at once;
- a whole package whose world entry did not reach the disk is removed by
  the next change, like any package nothing asks for.

Limits:
- This relies on the file system committing metadata in order, as ext4,
  XFS, btrfs and f2fs do: if a later step of a switch is on disk, the
  earlier ones are.
- A file of a recently added package that someone shortened by hand
  before a reboot looks like one a power cut shortened: the package is
  removed, and installed again by the next change.
- Where the boot id can't be read (`/proc` not mounted), nothing is left
  unsynced.
- `CATALYST_BOOT_ID` replaces the boot id; the tests use it to play a
  reboot.

## Trust and limits

Trust works as in apk v3:
- An index must carry a valid signature by a trusted key (RSA PKCS#1 v1.5,
  SHA-512, `hash_alg` 3/4/5).
- A package is trusted when its identity matches the signed index: the
  SHA-256 or SHA-512 of its ADB block. Failing that, its own signature
  must verify. When the identity matches, the package's signature blocks
  are not checked: they add nothing to it (apk does not check them
  either), and checking one costs an RSA verification on every package.
- An identity mismatch is fatal, even with `--allow-untrusted`.
- Name, version and arch must equal the index entry.
- An index that was verified is kept parsed in
  `var/cache/catalyst/parsed-<16 hex>` and taken from there while nothing
  changed. That file's key is the SHA-256 of the index file, its location,
  the trusted keys and the verify options, so another index, another set
  of keys or a changed file is verified from scratch; the file has a
  checksum, and a damaged one is ignored and replaced. The rollback check
  below is not cached: it runs on every load.

What an untrusted file or a hostile mirror can make catalyst do is bounded:
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
- **Rollback:** for each repository, catalyst remembers the newest build-time
  and the newest version of every package name it offered.
  - A validly signed index that is older is refused unless
    `--allow-rollback` is given. Older means an older newest build-time,
    or a name offered only in a lower version: a mirror replaying an old
    index to hold back fixes.
  - A downloaded index replaces the cached one only after it was verified
    and found fresh.
  - catalyst warns when an installed package is newer than anything the
    repositories offer.
  - A root that never saw a repository has nothing to compare with. That
    would need a signed timestamp in the index.
- **Confinement:**
  - All paths are resolved inside the root (`..` and absolute symlinks are
    clamped at ROOT).
  - No entry may be, or resolve into, `Programs`, `var/lib/catalyst` or
    `var/cache/catalyst`.
  - catalyst's own directories are never followed through a symlink.
  - Versions must be valid apk versions.
  - Live files are hashed, and their owner and mode set, through
    `O_NOFOLLOW` descriptors.
  - Keys, the repository list, `etc/apk/arch`, `passwd` and `group` are
    read inside the root, as regular files of bounded size.

## Resolution

catalyst uses a greedy resolver that makes apk's choices. The order follows
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
- catalyst manages its own roots only: no import of an apk-managed system, no
  `apk` database, no `/` without `--initdb` history.
- `oct fix` checks hashes of regular files in `Programs/`, repairs the
  live tree and rebuilds a damaged record from the signed package; corrupt
  files in `Programs/` itself are reported as errors and need a reinstall.
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
  resolve into catalyst's own directories.
- The lookup index follows the directory symlinks that packages own. One
  made by hand or by a script (a directory moved to another disk and
  linked back) is not tracked until `oct fix` rebuilds the index. If two
  packages then reach one file through different paths, which is already a
  conflict catalyst doesn't report, a change may remove that file where
  reading every record would keep it.
- Durability is `syncfs` before the journal and at the end; files copied
  to another file system (`/boot`) are synced together with their
  directory before the old version is removed. Only process kills were
  tested, not power loss.

Benchmarks:
- xbps was measured on 22 November 2026 (`docs/benchmarks/2026-11-22/` in
  the repository), as was catalyst against apk on one-package jobs.

## Measured

Setup:
- Machine: i5-12450H, 12 threads, 24 GB RAM, WSL2 (kernel 6.18), ext4 in a
  VHDX.
- The machine was shared with other jobs, so load averages ran from 3 to 11
  during the runs. Treat the absolute numbers as rough.
- `catalyst-bench.sh`, 5 runs per configuration, apk and catalyst alternating.
- apk-tools is 3.0.8 with `--no-cache`. catalyst reads the local repository in
  place.
- Cold runs drop the caches first.
- `+sync` adds a `sync -f` right after the command. catalyst syncs itself; apk
  leaves writeback to the kernel.

The sets:
- **base** is `valence-base`: 39 packages, 60 MB.
- **big** is valence-base plus the 30 largest packages: 244 packages,
  3.39 GB, 80,315 files.

| set | cache | scripts | apk wall | catalyst wall | catalyst/apk | apk +sync | catalyst +sync | CPU apk / catalyst |
|---|---|---|---|---|---|---|---|---|
| base | cold | no | 0.385 s | 0.225 s | 0.59 | 0.466 s | 0.231 s | 0.29 / 0.30 s |
| base | cold | yes | 0.396 s* | 0.246 s | 0.62 | 0.464 s | 0.255 s | 0.31 / 0.31 s |
| base | warm | no | 0.283 s | 0.153 s | 0.54 | 0.357 s | 0.157 s | 0.27 / 0.32 s |
| base | warm | yes | 0.300 s* | 0.177 s | 0.59 | 0.358 s | 0.183 s | 0.29 / 0.33 s |
| big | cold | no | 12.42 s | 6.45 s | 0.52 | 13.64 s | 6.46 s | 11.3 / 11.7 s |
| big | warm | no | 11.38 s | 5.14 s | 0.45 | 13.03 s | 5.15 s | 11.3 / 13.7 s |

\* apk exits 1 with scripts: bash's post-install finds no `/bin/sh` yet.
catalyst defers that script.

Wall times are medians of 5 runs. Max RSS is 10-17 MB for apk; for catalyst,
22 MB on base and about 110-130 MB on big. The installed trees have the
same content: catalyst's are larger by its `.catalyst` metadata, and have twice the
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
- Keeping the file records in memory instead of reading `.catalyst/files` twice.

One-package jobs (`add nano`, `--no-scripts`, 7 runs each; one root per
tool, each add undone by an untimed `del`; 23 November 2026). "No index"
is the committed code before the lookup index; "+ sync" adds a `sync -f`
after the command, since apk leaves writeback to the kernel:

| set | cache | apk 3.0.8 | catalyst, no index | catalyst, index | catalyst / apk | + sync: apk, catalyst |
|---|---|---|---|---|---|---|
| base | warm | 13 ms | 36 ms | 39 ms | 2.95 | 22 ms, 44 ms |
| base | cold | 48 ms | 95 ms | 81 ms | 1.67 | 59 ms, 86 ms |
| big | warm | 32 ms | 214 ms | 51 ms | 1.56 | 44 ms, 56 ms |
| big | cold | 77 ms | 514 ms | 160 ms | 2.09 | 89 ms, 165 ms |

A one-app job with real dependencies and many files: `add
cataclysm-dda-tiles` (the game with graphics and sound; 7,929 data files),
5 runs each, medians, 23 November 2026:

| system | cache | packages added | apk 3.0.8 | catalyst | + sync: apk, catalyst |
|---|---|---|---|---|---|
| 39 packages | warm | 39 | 1.58 s | 0.91 s | 2.02 s, 0.92 s |
| 39 packages | cold | 39 | 1.72 s | 1.16 s | 2.15 s, 1.17 s |
| 244 packages | warm | 8 | 0.93 s | 0.94 s | 1.50 s, 0.95 s |
| 244 packages | cold | 8 | 1.07 s | 1.09 s | 1.38 s, 1.10 s |

### The strict comparison

The tables above are medians of 5 or 7 runs. To call something faster or
slower as a fact, not as one lucky run, the strict benchmark runs every
case 15 times per tool, alternating in ABBA order, and keeps every run.
It gives a verdict only when both of these hold:
- a Mann-Whitney U test with p < 0.01;
- a 95% bootstrap interval of the ratio of the medians that lies entirely
  below 0.95 (faster) or above 1.05 (slower).

Anything else is "no clear difference". "+ sync" adds a `sync -f` to both
tools. 23 November 2026, the 39-package and the 244-package system,
`--no-scripts`. `add nano` is an add that is not synced (see Unsynced
adds), as apk's never are; the Cataclysm install is over 32 MiB, so
catalyst syncs it:

| system | job | cache | apk | catalyst | catalyst / apk (95% CI) | verdict | + sync: apk, catalyst | ratio (95% CI) | verdict |
|---|---|---|---|---|---|---|---|---|---|
| 39 | nano | warm | 13 ms | 11 ms | 0.80 (0.79-0.85) | **faster** | 20 ms, 18 ms | 0.89 (0.86-0.92) | **faster** |
| 39 | nano | cold | 46 ms | 48 ms | 1.05 (1.01-1.10) | no clear difference | 54 ms, 58 ms | 1.06 (1.02-1.10) | no clear difference |
| 244 | nano | warm | 32 ms | 16 ms | 0.48 (0.45-0.49) | **faster** | 44 ms, 23 ms | 0.52 (0.48-0.55) | **faster** |
| 244 | nano | cold | 69 ms | 77 ms | 1.11 (1.01-1.17) | no clear difference | 82 ms, 86 ms | 1.04 (0.95-1.11) | no clear difference |
| 39 | Cataclysm | warm | 1.58 s | 1.07 s | 0.68 (0.65-0.79) | **faster** | 2.35 s, 1.07 s | 0.46 (0.41-0.55) | **faster** |
| 39 | Cataclysm | cold | 1.97 s | 1.29 s | 0.65 (0.62-0.70) | **faster** | 2.43 s, 1.30 s | 0.53 (0.50-0.56) | **faster** |
| 244 | Cataclysm | warm | 944 ms | 913 ms | 0.97 (0.92-1.02) | no clear difference | 1.45 s, 916 ms | 0.63 (0.56-0.68) | **faster** |
| 244 | Cataclysm | cold | 1.08 s | 1.07 s | 0.99 (0.92-1.02) | no clear difference | 1.44 s, 1.08 s | 0.75 (0.71-0.78) | **faster** |

Removals, upgrades and a network repository, by the same rules (23
November 2026). The upgrades use a second repository with 27 packages of
valence-base rebuilt as a newer version; the 27-package upgrade starts
from a fresh root every round, so it was run on the small system only.
The HTTP server is on the same machine, once as it is and once with
25 ms added to every request, as a mirror some way off would; apk keeps
its index cache there, as an installed system does:

| system | job | cache | apk | catalyst | catalyst / apk (95% CI) | verdict | + sync: apk, catalyst | verdict |
|---|---|---|---|---|---|---|---|---|
| 39 | remove nano | warm | 11 ms | 14 ms | 1.25 (1.19-1.32) | slower | 15 ms, 18 ms | slower |
| 39 | remove nano | cold | 42 ms | 46 ms | 1.10 (1.04-1.16) | no clear difference | 48 ms, 51 ms | no clear difference |
| 244 | remove nano | warm | 30 ms | 22 ms | 0.73 (0.67-0.77) | **faster** | 38 ms, 26 ms | **faster** |
| 244 | remove nano | cold | 68 ms | 83 ms | 1.22 (1.10-1.30) | no clear difference | 78 ms, 88 ms | no clear difference |
| 39 | remove Cataclysm | warm | 76 ms | 243 ms | 3.20 (3.05-3.49) | slower | 137 ms, 290 ms | slower |
| 39 | remove Cataclysm | cold | 226 ms | 558 ms | 2.47 (2.00-6.69) | slower | 244 ms, 570 ms | slower |
| 244 | remove Cataclysm | warm | 89 ms | 283 ms | 3.17 (2.97-3.27) | slower | 174 ms, 326 ms | slower |
| 244 | remove Cataclysm | cold | 260 ms | 625 ms | 2.40 (1.92-5.33) | slower | 282 ms, 637 ms | slower |
| 39 | upgrade one package | warm | 65 ms | 69 ms | 1.06 (1.02-1.20) | no clear difference | 71 ms, 74 ms | no clear difference |
| 39 | upgrade one package | cold | 107 ms | 118 ms | 1.10 (1.03-1.20) | no clear difference | 115 ms, 124 ms | no clear difference |
| 244 | upgrade one package | warm | 87 ms | 94 ms | 1.07 (0.93-1.15) | no clear difference | 116 ms, 102 ms | no clear difference |
| 244 | upgrade one package | cold | 145 ms | 171 ms | 1.18 (1.14-1.32) | slower | 157 ms, 178 ms | slower |
| 39 | upgrade 27 packages | warm | 222 ms | 170 ms | 0.77 (0.75-0.79) | **faster** | 259 ms, 176 ms | **faster** |
| 39 | upgrade 27 packages | cold | 312 ms | 274 ms | 0.88 (0.83-0.91) | **faster** | 322 ms, 281 ms | **faster** |
| 39 | add nano over HTTP | warm | 14 ms | 43 ms | 3.12 (3.07-3.19) | slower | 20 ms, 51 ms | slower |
| 244 | add nano over HTTP | warm | 34 ms | 48 ms | 1.42 (1.40-1.44) | slower | 44 ms, 56 ms | slower |
| 39 | add nano over HTTP, +25 ms a request | warm | 41 ms | 104 ms | 2.56 (2.51-2.58) | slower | 48 ms, 112 ms | slower |
| 244 | add nano over HTTP, +25 ms a request | warm | 61 ms | 108 ms | 1.79 (1.77-1.82) | slower | 71 ms, 117 ms | slower |

Where catalyst loses, and why:
- **Removing a big package:** every file has two names, one under
  `Programs` and one in the live tree, so it is unlinked twice (about
  55 ms and 77 ms for Cataclysm's 8,517 entries), each pass on one thread;
  apk unlinks once. Then about 44 ms in the three flushes, 42 ms reading
  the records (twice) and checking ownership, and 34 ms rewriting about
  3,600 bucket files of the lookup index.
- **A network repository:** catalyst downloads the index on every `add`
  and apk does not (it uses its cached index for four hours: with the
  cached file made 3 hours old it asked for nothing, 5 hours old it asked
  again). So catalyst makes two requests where apk makes one, and it sets
  up TLS, reading the system's certificates, on every run with a remote
  repository, even one over plain HTTP.
- **Removing one small package on a small system:** the three flushes,
  which a removal keeps.

`add nano`, warm, over the rounds of this work (medians of 15):

| system | first strict run | round two | round three | adds not synced | apk |
|---|---|---|---|---|---|
| 39 packages | 32 ms | 29 ms | 23 ms | 11 ms | 13 ms |
| 244 packages | 44 ms | 37 ms | 27 ms | 16 ms | 32 ms |

Where a one-package install on the small system spends its 11 ms:
- starting and ending the process: about 5 ms, the same for apk here;
- unpacking: about 3.5 ms, mostly inflating and hashing;
- the repository index, from its cache: about 1.5 ms;
- everything else: about 1.5 ms.

Until round three it also spent about 9.5 ms in three device flushes.
Upgrades, removals and installs of 32 MiB or more still do: about 4% of
the Cataclysm install. On the big system apk reads and rewrites its whole
installed database and catalyst's lookup index does not, which is why the
gap is wider there. Cold, a small add is level with apk: most of its time
is reading files from the disk.

`CATALYST_PROF=1` prints, on stderr, how long each step of a run took.

## Tests

- **`cargo test`:** 43 unit tests, 11 command tests and 16 tests over the
  real repository:
  - every package verified against the index;
  - tampering of every kind is rejected;
  - damaged input never panics;
  - a verified index comes back from its cache unchanged, a damaged cache
    file is not used, and other keys verify from scratch;
  - an add that is not synced, with a power cut played by damaging files
    and changing the boot id: a package left short is removed after the
    reboot and the rest of the tree is as it was; an add cut in its switch
    is finished only when what it installs is whole; an add that has to
    replace something is synced from there on;
  - a broken record stops only the changes that need it, and `oct fix`
    rebuilds it from the signed package;
  - the lookup index knows the tree's directory symlinks and, after a
    series of changes, equals the one `oct fix` rebuilds;
  - a damaged index (a bucket file missing, the aliases damaged) sends a
    change to every record instead, and a switch killed while going
    through the index is finished without the records it didn't read.
- **End-to-end tests (a) to (c),** run by `catalyst-e2e-all.sh` and the
  scripts it calls (`catalyst-e2e-a.sh`, `catalyst-e2e-b.sh`, `catalyst-e2e-c.sh`).
  These scripts live outside the crate, in the scratchpad folder.
  - (a) and (b): catalyst and apk produce identical trees for install, add and
    del.
  - (c): an upgrade killed at 11 points (`CATALYST_CRASH_AT`, which SIGKILLs
    itself) is completed by the next run.
- **Kill test,** run by `catalyst-killtest.sh` with the scripts named
  `catalyst-kt-*` beside it. It upgrades valence-base to 27 rebuilt packages
  and SIGKILLs catalyst's process group at random moments: during file
  placement, between journal steps, inside scripts, during the deletion of
  old versions, and twice in a row. After each kill it checks that:
  - every path is the old or the new entry;
  - `/bin/sh`, libc and `ld.so` work under chroot;
  - the next run completes the upgrade and leaves a root identical to an
    uninterrupted one, lookup index included.

  With `KT_JOB=small` the job is four of those upgrades and a new package
  instead: a change that leaves the directory symlinks alone, so the
  switch goes through the lookup index. With `KT_JOB=add` it adds eight
  new packages (14 with their dependencies, 16.6 MiB): an add that is not
  synced.

  Results on stage 1's code: 310 rounds, 298 of them killed while
  running and 82 with a package half placed. There were 0 failures. With
  the lookup index: 80 rounds of the upgrade and 80 of `KT_JOB=small`,
  0 failures. With unsynced adds: 80 rounds each of the upgrade,
  `KT_JOB=small` and `KT_JOB=add`, 0 failures. One
  earlier failure, a directory left 0700 by a kill between `mkdir` and
  `chmod`, led to the atomic directory creation described under The
  transaction.
