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
    [--simulate|-s] [--sync] [--no-scripts] [--no-commit-hooks] [--jobs N]
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
  recover          finish a change that was interrupted, and nothing else
                   (every command does this first; see One sync)
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
- `--sync` makes a change return only when it is on disk (see One sync).

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
ROOT/var/lib/catalyst/index/next/               the index of after a change, until its switch is done
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
- `files/XXX` say which packages have an entry at each path, files apart
  from directories: 65,536 buckets by a hash of the path, 128 to a file
  (512 files).
- `aliases` lists the entries that reach another place through a directory
  symlink, such as `lib64 -> usr/lib`.
- `state` names every package's version and the tree's directory symlinks
  as they were when the index was written.

A change reads the records of the packages it changes, of those that
share a file path with them, and of those with a trigger. A trigger may
fire for a directory the change modifies, and the index says whether that
directory is still installed. The change rewrites only the files that
hold the buckets of its own paths; a bucket it does not touch is written
back as it was read. Two kinds of change read every record and rebuild the
index:
one that touches a directory symlink, and one that installs a trigger
with a pattern, since that trigger is given every installed directory that
matches.

The index is used only when `state` matches the root exactly: the version
every `Current` names, and every directory symlink's target. Otherwise,
and always while there is a journal, catalyst reads the records. A missing or
outdated index is rebuilt when a root is opened for a change, and `oct fix`
always rebuilds it. The index is canonical: one updated change by change
is byte for byte the one a rebuild writes, and the tests compare the two.
(One exception: the list of directory symlinks in `state` is carried over
by an update and worked out afresh by a rebuild, so the two differ when a
symlink of a package the change does not touch starts or stops leading to
a directory, because another package brought or took away what it points
at.)

Every file of the index is checked when it is read. `state` holds which
buckets are not empty, the SHA-256 of `meta` and of `aliases`, and a sum
of itself; each file of buckets starts with a checksum of the rest of it,
then the length of each of its buckets. Before a
change goes through the index, everything it may look up there is read
and checked: the buckets of its paths, of their directories and of their
canonical places, and the aliases. A missing, truncated or changed file is
found there, before anything is switched, and the change then reads every
record and rebuilds the index.

A bucket holds a path or two, so a change parses and writes little more
than its own paths; the buckets it does not touch are copied as text. The
first index had 4,096 buckets, one to a file. Removing Cataclysm: DDA then
read some 3,700 files and deleted 3,000, each a wait for the directory's
lock, and on a system of 80,000 files every bucket held twenty paths of
other packages, which were parsed and written out again: 80 ms of a 180 ms
removal. A big change still rewrites all 512 files, on several threads. A
file written inside a switch is cut to nothing and written again rather
than replaced through a temporary name (creating and renaming files in one
directory would make the threads wait for each other), and never written
over in place: after a power cut a file written over could still hold its
old text, whole and with a good checksum. Nothing reads the index while it
is written.

Threads are started only when they pay. Starting them costs about 1.4 ms,
so work that would take less than about 3 ms on one thread stays there:
on a warm cache, the 244 `Current` links of a big system take 0.8 ms on
one thread and 1.5 ms on sixteen. A cold cache, where every read waits
for the disk, and the hundreds of files of a big change go to the threads.
The time the first items took tells which it is: work is judged after
eight items, waiting for the disk shows after two.

A change that ends without a `syncfs` (see One sync) writes nothing of
the index after its one `syncfs`. The index of after the change is worked
out before the switch and written beside the current one, in
`index/next/`; that `syncfs` puts it on disk; and at the end of the switch
it is put in place by renames alone, `state` removed first and renamed in
last. Whatever a power cut leaves then has either no `state`, and the
index is rebuilt, or the new `state` with every file it describes whole on
disk. (A new entry's canonical place is worked out before its directory
exists: the part of the path that is there, resolved, and the rest as
written. That is only right while the tree's directory symlinks stay as
they are and no script runs, which is when a staged index is used.)

A change that ends with a `syncfs` (one that ran a script, one asked for
with `--sync`, one that rebuilds the index) updates the index inside the
switch, before that `syncfs`. The journal names the records the switch
reads and the index it was planned with, and finishing an interrupted
switch reads the same records with that index, as long as the index is
still the one from before the switch; otherwise the recovery reads every
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
     renamed to `<version>`. It is for root only (0700) from the start
     until the switch has given every entry its final owner (step 6.4):
     owners and modes are set by path in there, a package may hold a
     directory that belongs to its own user, and nobody may rearrange the
     tree under root's hands. (A version with nothing in it for anyone but
     root is opened as soon as it is current. A version that is open is
     never gone through by path again, also not by a run that finishes an
     interrupted switch: the journal says that its versions start closed.)
     A downloaded package is not fsync'ed by itself (that was a device
     flush for each); the `syncfs` of step 6 puts it on disk, and a file
     cut short in the cache does not verify, is removed, and is fetched
     again. A downloaded repository index is fsync'ed before it replaces
     the cached one, because no `syncfs` follows it.
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
   installed package's when the lookup index can't be used. What the
   switch will put in place by a rename is made now: the copies of the new
   versions' files under `etc/` (inside the new version, in
   `.catalyst/staged`) and the lookup index of after the change
   (`index/next`).
6. **Switch.** This is the only step that touches the live tree, and it
   comes last.
   1. The journal (which package goes from which version to which, the
      world it ends with, whether scripts are off) is written under a
      temporary name, and the new world beside it. One `syncfs` puts them
      on disk together with everything unpacked and prepared. Only then is
      the journal renamed into place: a journal is never there before what
      it points at. (With `--sync` its directory is fsync'ed as well.)
   2. Per package, dependencies first:
      - the pre-install or pre-upgrade script;
      - its entries placed (see "How entries are placed" below; the hard
        links of a big package on several threads);
      - the old version's entries that no package owns any more removed,
        but only if they are still what it installed;
      - `Current` flipped (new symlink, then `rename`), and the old
        version's directory made 0700, since its set-id files are no longer
        in the live tree;
      - the post-install or post-upgrade script.
   3. Removed packages: the pre-deinstall script, their entries,
      `Current`, then the post-deinstall script.
   4. The world is put in place (a rename of the file written in 1).
      Owners are set once more, now that every `etc/passwd` and
      `etc/group` is in place, and the new versions are opened for reading
      (0755). A version that is open already, from an earlier run of this
      switch, is left as it is.
   5. The triggers of affected packages run.
   6. The lookup index of after the change is put in place (renames), or,
      in a switch that ends with a `syncfs`, updated or rebuilt. Then that
      `syncfs`, if the switch ran a script, wrote the index here, or was
      asked for with `--sync`; any other switch ends without one (see One
      sync). The journal is deleted, then the old versions (a big one
      directory by directory on several threads).
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
- A new name is `link`ed or `symlink`ed straight onto its path (the call
  itself tells whether something is there).
- An entry that replaces an existing one is made under a temporary name
  next to its target, then `rename`d over it.
- A new directory that is root's, mode 0755 or stricter, under a directory
  of root's, is one `mkdir`, which gives exactly that. Any other is made
  under a temporary name with its final mode and owner, then renamed into
  place.

All of these are atomic: every path is at every moment either the old
entry or the new one.

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
- Before the first script or trigger of a switch runs, the journal's
  directory is fsync'ed; after each one a `syncfs` makes its mark in the
  journal durable, so that a recovery does not run it again; and such a
  switch ends with a `syncfs` (see One sync).
- What a script wrote on another file system that the switch knows of is
  synced before the script's mark is written: the file systems the switch
  worked on itself, and `boot`, `boot/efi` and `efi` where they are mounts
  of their own (a kernel's or a boot loader's script writes there). A
  script that writes to yet another file system has to sync it itself.

## Recovery

The journal is written only after everything it points at is complete on
disk. A run that finds a journal finishes it first (roll forward):
- every step is idempotent;
- placed packages and scripts that ran are marked in the journal, so they
  are not placed or run again.

A `journal.new` left by a crash was never in effect (the crash came
before the switch began) and is deleted, with what was prepared beside it
(`world.new`, `index/next`). A journal that is there is finished after a
`syncfs`, as a switch that ends with one. A change that was asked for with
`--no-scripts` says so in its journal and is finished without scripts,
whatever the options of the run that finishes it.

`oct recover` does this and nothing else; when nothing was left behind it
does not even lock the root. Every command does the same first. Run at
system start (a line in `/etc/runit/1`, once the root is writable), it
finishes a change that a power cut interrupted before anyone logs in. Its
exit status is 1 when the switch it finished reported errors; any other
command that finishes such a switch first says so and exits with 1 too,
whatever its own work did.

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

## One sync

A change is put on disk by one `syncfs`, before the switch, and ends
without another. The owner decided this on 23 November 2026 (ROADMAP,
"Decided": "one sync").

That one `syncfs` covers everything the switch needs: the unpacked
packages, the journal and the new world under their temporary names, the
copies of `etc/` files, the lookup index of after the change. After it the
switch only links, renames, removes and makes directories, and appends
"done" marks to its journal. So:

- **A killed process loses nothing.** The journal is there, and the next
  run finishes the switch.
- **A power cut**, during the change or in the seconds after it, leaves
  what the file system had committed: the change not begun (a
  `journal.new`, which the next run deletes with what was prepared), begun
  (a `journal`, which the next run finishes), or complete. Never half a
  package, and never a file with a name and no content: every file that
  gets a name in the switch had its content on disk before the switch
  began.
- **What is given up** is that the change is on disk when the command
  returns. Until the file system writes out its own journal (after 5 s on
  ext4, 30 s on btrfs, unless something syncs sooner) a power cut can take
  the whole change back, and then it is as if the command had not been
  run. `oct --sync` returns only when the change is on disk: it fsyncs the
  journal's name before the switch and ends with a `syncfs`. That is what
  apk gives on an installed system, with one `sync` at the end, and no
  journal before it.

A switch ends with a `syncfs` after all, and has the journal's name
fsync'ed before the step that calls for it, when:
- a package script or trigger runs (a script may write, and sync, what it
  likes). A `syncfs` after each one also makes its mark in the journal
  durable, so that it is not run again;
- the switch itself has to fsync a file: a copy under `boot/`, or a copy
  onto another file system (`EXDEV`);
- anything is to be done on another file system than the journal's (a
  mounted `/boot`, a separate `/var/log`): the file system keeps no order
  between that and the journal's rename;
- the lookup index has to be written inside the switch: it could not be
  prepared, or the change rebuilds it (a first install, a change that
  touches a directory symlink).

When commit hooks are installed, the change is synced once more before the
post-commit hooks run, for the same reason as before a script.

**More than one file system.** No file system keeps an order with another
one. Wherever a switch works outside the journal's file system:
- the journal's name is fsync'ed before the first step there (above);
- that file system is synced before the mark that records the work is
  appended to the journal, since a recovery skips what a mark records; and
  once more, before the root's, at the end, before the journal is deleted;
- where the root directory or `Programs` is itself not on the file system
  of `var/lib/catalyst` (a separate `/var`), all of the switch is such
  work: it is a full switch from its start, and every mark waits for a
  `syncfs` of those file systems. That layout is safe and slow.

A directory counts as another file system when its device number differs
from that of `var/lib/catalyst`. On Valence that includes `/var/log`, a
btrfs subvolume of its own: a package with entries there takes this path.

This rests on the file system committing what was not fsync'ed in the
order it was done, as ext4, XFS, btrfs and f2fs do: if a later step of a
switch is on disk, the earlier ones are. (ext4 mounted `data=writeback`
can show stale blocks in a file after a power cut whatever a program does;
catalyst does not allow for that.) The power-cut test (see Tests) cuts the
power, in effect, at random moments of real changes on ext4, btrfs and XFS
and checks what is left. It has one file system under the root: the rules
for more than one are covered by a test that mounts a second one and looks
at what the switch syncs and in which order, not by cutting the power.

History. For a few hours on 23 November 2026, adds of new packages under
32 MiB were not synced at all, and a mark, `var/lib/catalyst/unsynced`,
made the run after a reboot check what such adds had left on disk. That
check had to guess whether a file had been cut short by a power cut or
changed by the admin; an independent review found eleven defects in it,
among them healthy packages uninstalled after a clean reboot. It is gone.
A mark that is still there is deleted, with a note that names the packages
added under it; nothing is checked and nothing is uninstalled.

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
  - `add` plans with a remote repository's cached index when that is less
    than four hours old, without asking the server. apk does the same,
    with the same four hours (the owner's decision of 23 November 2026;
    ROADMAP, "Decided"). `add -u`, `upgrade` and `update` always ask the
    server. The rollback check runs on a cached index as on a downloaded
    one. If a download fails after planning with a cached index (the
    server may have replaced that package since), the index is fetched
    and the change is tried once more.
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
- `+sync` adds a `sync -f` right after the command. catalyst synced itself
  when these tables were made. apk was run with `--root`, and then it does
  not sync; on an installed system it calls `sync()` once at the end of
  every change (see "What is compared" under The strict comparison, whose
  tables are the current ones).

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

Since then done: the files of a package placed on several threads, and
its records read once. The next speedups are measured but not done:
- zstd-compressed packages: the repository is deflate today. This helps
  apk even more, which is single-threaded and decompression-bound.
- Fewer small files to read from a cold disk (see below).
- Inflating a big package on one thread while another hashes and writes.

One-package jobs (`add nano`, `--no-scripts`, 7 runs each; one root per
tool, each add undone by an untimed `del`; 23 November 2026). "No index"
is the committed code before the lookup index; "+ sync" adds a `sync -f`
after the command, since apk run with `--root` does not sync:

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

The tables above are medians of 5 or 7 runs, from earlier stages of this
work. To call something faster or slower as a fact, not as one lucky run,
the strict benchmark runs every case 15 times per tool, alternating in
ABBA order, and keeps every run. It gives a verdict only when both of
these hold:
- a Mann-Whitney U test with p < 0.01;
- a 95% bootstrap interval of the ratio of the medians that lies entirely
  below 0.95 (faster) or above 1.05 (slower).

Anything else is "no clear difference".

**What is compared.** apk-tools 3.0.8 ends every change on an installed
system with one `sync()` of all file systems (`sync_if_needed` in its
`src/commit.c`). It skips that under `--root`, as a benchmark has to run
it, so here it is run with `--sync`, which forces it. (Until 23 November
2026 this file compared against apk without its sync and said that apk
never syncs. That was wrong, and those tables are gone.) catalyst is run
as it is: one `syncfs` before the switch (see One sync). So the two have
done different things when they return:
- apk's change is on disk. A power cut in the middle of it can leave it
  half done, and nothing knows.
- catalyst's change cannot be left half done from its one `syncfs` on,
  and it is on disk when the file system next writes its journal, or at
  once with `--sync`.

"Until on disk" adds a `sync -f` after the command to both tools: the
time until the change is on disk whatever the tool did by itself. Both
pay there for starting the `sync` program and for one more flush.

23 November 2026, the 39-package and the 244-package system,
`--no-scripts`, ext4 in a VHDX under WSL2 (Valence's own root is btrfs:
see "On other file systems" below):

| system | job | cache | apk | catalyst | catalyst / apk (95% CI) | verdict | until on disk: apk, catalyst | ratio (95% CI) | verdict |
|---|---|---|---|---|---|---|---|---|---|
| 39 | add nano | warm | 18 ms | 17 ms | 0.94 (0.92-0.99) | no clear difference | 21 ms, 21 ms | 1.00 (0.95-1.03) | no clear difference |
| 39 | add nano | cold | 52 ms | 57 ms | 1.09 (1.05-1.13) | slower | 58 ms, 64 ms | 1.10 (1.06-1.14) | slower |
| 244 | add nano | warm | 42 ms | 22 ms | 0.51 (0.47-0.54) | **faster** | 47 ms, 26 ms | 0.55 (0.52-0.58) | **faster** |
| 244 | add nano | cold | 86 ms | 80 ms | 0.93 (0.90-1.01) | no clear difference | 91 ms, 87 ms | 0.95 (0.90-1.01) | no clear difference |
| 39 | add Cataclysm | warm | 2.27 s | 780 ms | 0.34 (0.31-0.37) | **faster** | 2.28 s, 860 ms | 0.38 (0.32-0.42) | **faster** |
| 39 | add Cataclysm | cold | 2.44 s | 1.12 s | 0.46 (0.44-0.50) | **faster** | 2.45 s, 1.23 s | 0.50 (0.47-0.53) | **faster** |
| 244 | add Cataclysm | warm | 1.45 s | 718 ms | 0.50 (0.45-0.54) | **faster** | 1.45 s, 806 ms | 0.56 (0.50-0.61) | **faster** |
| 244 | add Cataclysm | cold | 1.51 s | 982 ms | 0.65 (0.56-0.71) | **faster** | 1.51 s, 1.05 s | 0.70 (0.61-0.76) | **faster** |

Removals, upgrades and a network repository. The upgrades use a second
repository with 27 packages of valence-base rebuilt as a newer version;
the 27-package upgrade starts from a fresh root every round, so it was run
on the small system only. The HTTP server is on the same machine, once as
it is and once with 25 ms added to every request, as a mirror some way off
would; both tools keep their index cache there, as an installed system
does:

| system | job | cache | apk | catalyst | catalyst / apk (95% CI) | verdict | until on disk: apk, catalyst | ratio (95% CI) | verdict |
|---|---|---|---|---|---|---|---|---|---|
| 39 | remove nano | warm | 14 ms | 10 ms | 0.69 (0.65-0.72) | **faster** | 17 ms, 14 ms | 0.79 (0.75-0.82) | **faster** |
| 39 | remove nano | cold | 51 ms | 47 ms | 0.92 (0.87-1.08) | no clear difference | 57 ms, 53 ms | 0.94 (0.88-1.10) | no clear difference |
| 244 | remove nano | warm | 39 ms | 15 ms | 0.39 (0.37-0.40) | **faster** | 42 ms, 19 ms | 0.45 (0.43-0.47) | **faster** |
| 244 | remove nano | cold | 81 ms | 64 ms | 0.79 (0.75-0.86) | **faster** | 86 ms, 69 ms | 0.80 (0.77-0.87) | **faster** |
| 39 | remove Cataclysm | warm | 126 ms | 130 ms | 1.03 (0.95-1.14) | no clear difference | 138 ms, 195 ms | 1.41 (1.17-1.52) | slower |
| 39 | remove Cataclysm | cold | 245 ms | 256 ms | 1.04 (0.89-2.51) | no clear difference | 250 ms, 272 ms | 1.09 (0.93-2.52) | no clear difference |
| 244 | remove Cataclysm | warm | 170 ms | 202 ms | 1.19 (0.90-1.65) | no clear difference | 178 ms, 235 ms | 1.32 (0.95-1.87) | no clear difference |
| 244 | remove Cataclysm | cold | 252 ms | 268 ms | 1.06 (0.79-1.84) | no clear difference | 259 ms, 286 ms | 1.11 (0.83-1.86) | no clear difference |
| 39 | upgrade one package | warm | 74 ms | 65 ms | 0.88 (0.84-0.94) | **faster** | 77 ms, 71 ms | 0.92 (0.87-0.98) | no clear difference |
| 39 | upgrade one package | cold | 124 ms | 120 ms | 0.97 (0.89-1.06) | no clear difference | 129 ms, 128 ms | 0.99 (0.91-1.08) | no clear difference |
| 244 | upgrade one package | warm | 113 ms | 91 ms | 0.80 (0.46-0.88) | **faster** | 120 ms, 101 ms | 0.84 (0.49-0.96) | no clear difference |
| 244 | upgrade one package | cold | 160 ms | 158 ms | 0.99 (0.94-1.04) | no clear difference | 165 ms, 168 ms | 1.02 (0.96-1.06) | no clear difference |
| 39 | upgrade 27 packages | warm | 247 ms | 163 ms | 0.66 (0.48-0.70) | **faster** | 255 ms, 169 ms | 0.66 (0.49-0.71) | **faster** |
| 39 | upgrade 27 packages | cold | 331 ms | 254 ms | 0.77 (0.75-0.79) | **faster** | 337 ms, 261 ms | 0.77 (0.76-0.80) | **faster** |
| 39 | add nano over HTTP | warm | 18 ms | 19 ms | 1.05 (1.02-1.10) | no clear difference | 22 ms, 23 ms | 1.07 (1.04-1.13) | no clear difference |
| 244 | add nano over HTTP | warm | 44 ms | 24 ms | 0.54 (0.51-0.55) | **faster** | 48 ms, 28 ms | 0.58 (0.56-0.61) | **faster** |
| 39 | add nano over HTTP, +25 ms a request | warm | 46 ms | 47 ms | 1.03 (1.00-1.04) | no clear difference | 49 ms, 52 ms | 1.05 (1.02-1.07) | no clear difference |
| 244 | add nano over HTTP, +25 ms a request | warm | 72 ms | 51 ms | 0.71 (0.69-0.73) | **faster** | 76 ms, 56 ms | 0.73 (0.72-0.75) | **faster** |

Of the 26 cases catalyst is faster in 14, slower in 1, and in 11 there is
no clear difference. Until the change is on disk: faster in 12, slower in
2, no clear difference in 12.

Where it is not ahead, and why:
- **Removing a big package** (level; until it is on disk, 1.4 times apk's
  time on the small system). Every file has two names, one under
  `Programs` and one in the live tree, and both are unlinked; apk unlinks
  one. Both passes run on several threads now, the second directory by
  directory, and unlinks on ext4 gain about a factor of two from threads
  and no more. On 22 November this removal took 175 ms (with three
  flushes, 3,700 index files read and 37,000 `lstat` calls); it takes
  130 ms.
- **A one-package install on the small system:** 17 ms against 18 from a
  warm cache (level), 57 ms against 52 from a cold one (slower). catalyst
  itself starts faster than apk from a cold cache (15 ms against 22), but
  then reads more small files: the `Current` link of every package, the
  files of the lookup index, a repository index and its parsed copy where
  apk reads one file. On the big system the same install is level cold
  and twice as fast warm, because apk reads and rewrites its whole
  database there.
- **The same install over HTTP on the small system:** level; the request
  is the same for both.
- **Upgrading one package from a cold cache:** level.

`oct --sync` returns, as apk does, only when the change is on disk. It
then flushes three times where apk flushes once (the journal's content,
its name, the end), and a small change is slower than apk's by that, about
4 ms on this disk; a big one is not. Small system:

| system | job | cache | apk | catalyst | catalyst / apk (95% CI) | verdict | until on disk: apk, catalyst | ratio (95% CI) | verdict |
|---|---|---|---|---|---|---|---|---|---|
| 39 | add nano | warm | 18 ms | 22 ms | 1.20 (1.16-1.26) | slower | 22 ms, 25 ms | 1.16 (1.13-1.23) | slower |
| 39 | add Cataclysm | warm | 2.39 s | 871 ms | 0.36 (0.34-0.39) | **faster** | 2.40 s, 877 ms | 0.37 (0.34-0.40) | **faster** |
| 39 | remove nano | warm | 14 ms | 16 ms | 1.18 (1.06-1.27) | slower | 17 ms, 20 ms | 1.17 (1.11-1.26) | slower |
| 39 | remove nano | cold | 44 ms | 50 ms | 1.13 (1.08-1.16) | slower | 50 ms, 56 ms | 1.11 (1.06-1.17) | slower |
| 39 | remove Cataclysm | warm | 134 ms | 130 ms | 0.97 (0.90-1.11) | no clear difference | 143 ms, 142 ms | 0.99 (0.94-1.15) | no clear difference |
| 39 | remove Cataclysm | cold | 263 ms | 284 ms | 1.08 (0.71-2.59) | no clear difference | 270 ms, 298 ms | 1.10 (0.73-2.57) | no clear difference |
| 39 | upgrade one package | warm | 74 ms | 72 ms | 0.98 (0.90-1.02) | no clear difference | 78 ms, 77 ms | 1.00 (0.91-1.04) | no clear difference |
| 39 | upgrade one package | cold | 122 ms | 122 ms | 1.00 (0.97-1.05) | no clear difference | 128 ms, 128 ms | 1.01 (0.97-1.05) | no clear difference |

`add nano`, warm, over the rounds of this work (medians of 15):

| system | first strict run | round two | round three | adds not synced at all | one sync | apk, no sync | apk, with its sync |
|---|---|---|---|---|---|---|---|
| 39 packages | 32 ms | 29 ms | 23 ms | 11 ms | 17 ms | 13 ms | 18 ms |
| 244 packages | 44 ms | 37 ms | 27 ms | 16 ms | 22 ms | 32 ms | 42 ms |

Where a one-package install on the small system spends its 17 ms:
- the one `syncfs`: about 4.5 ms (apk's `sync` costs it the same);
- unpacking: about 3.5 ms, mostly inflating and hashing;
- starting and ending the process: about 3 ms;
- the repository index from its cache, and working out what to install:
  about 2.5 ms;
- the switch and the lookup index: about 1.5 ms.

What made the difference in the last round (23 November):
- one `syncfs` for every change instead of three flushes for upgrades and
  removals (a small removal: 17.5 ms to 10);
- a directory resolved with one `lstat` once the directory above it is
  known, where every component was looked at again each time;
- the lookup index in 512 files instead of 4,096, with buckets small
  enough that a change parses little but its own paths;
- records parsed on several threads, lookups that copy nothing;
- the files of a package linked on several threads, each with one call
  (the link itself says whether something is there);
- old version trees deleted directory by directory on several threads,
  by path only where nobody else can rearrange them;
- no fsync of each downloaded file.

Over the network catalyst was 1.4 to 3.1 times slower until it stopped
downloading the index on every `add` (see Trust and limits: a cached
index is used for four hours, as apk does) and stopped setting up TLS,
which reads the system's certificates, for URLs that need none.

`CATALYST_PROF=1` prints, on stderr, how long each step of a run took.

### On other file systems

Everything above was measured on ext4. Valence's own root is btrfs
(`subvol=@,noatime,compress=zstd:1`), and making and removing names
behaves differently there: btrfs keeps all the names of a subvolume in
one tree, and threads that change it wait for each other. Cataclysm's
8,500 entries, each file system in an image on the same disk, Linux 6.18,
12 CPUs, three runs each (milliseconds on 1, 2, 4 and 16 threads):

| | ext4 | XFS | btrfs (zstd:1) |
|---|---|---|---|
| linking them into place | 28, 17, 11, 11 | 33, 20, 15, 15 | 83, 67, 114, 173 |
| unlinking them there | 41, 28, 21, 20 | 46, 31, 25, 24 | 121, 145, 314, 275 |
| deleting the old version's tree | 77, 57, 41, 40 | 137, 170, 162, 167 | 120, 210, 474, 522 |

So catalyst picks the number of threads by file system
(`sys::name_threads`): as many as there are on ext4 and on what was not
measured; on XFS a tree is deleted on one; on btrfs links are made on two
and everything is removed on one. `CATALYST_LINK_THREADS`,
`CATALYST_UNLINK_THREADS` and `CATALYST_TREE_THREADS` set the numbers, for
measuring elsewhere. Until 23 November every file system got the threads
that suit ext4, and removing Cataclysm on btrfs took about 900 ms.

With that rule, not yet as a strict run (three runs each, apk with
`--sync`; catalyst's own step times against apk's wall time, which is
close enough for a change of this size and not for a small one):

| Cataclysm | ext4 | XFS | btrfs |
|---|---|---|---|
| add: apk, catalyst | 2.1 s, 0.7 s | 2.4 s, 0.75 s | 2.2 s, 0.95 s |
| remove: apk, catalyst | 100 ms, 125 ms | 180 ms, 250 ms | 170 ms, 310 ms |

Removing a big package is where catalyst is behind, on every one of them
here: it unlinks two names for every file, and on btrfs and XFS that
cannot be spread over threads. (On the ext4 of the machine's own disk the
strict run above had it level.) `tools/bench/catalyst-strict-fs.sh` runs
the strict comparison on a file system of its own; its results for btrfs
are to follow.

## Tests

- **`cargo test`:** 48 unit tests, 12 command tests and 24 tests over the
  real repository:
  - every package verified against the index;
  - tampering of every kind is rejected;
  - damaged input never panics;
  - a verified index comes back from its cache unchanged, a damaged cache
    file is not used, and other keys verify from scratch;
  - a change takes one `syncfs`, and with `--sync` its journal's name is
    fsync'ed and it ends with one too; a file under `etc/` arrives as a
    whole copy, put in place by a rename of the copy made before the
    switch; after each change the lookup index, put in place by renames,
    is the one `oct fix` rebuilds;
  - an add and a removal stopped at each stage: before its journal is in
    effect the change is discarded and the tree is as it was, after it the
    next run (`oct recover` for the removal) finishes it; a new version is
    root's alone until the switch has set its owners;
  - a change asked for with `--no-scripts` says so in its journal, and a
    recovery run without the option runs no script (and does run it when
    the line is taken out of the journal);
  - a journal mark that was cut while it was written does not count and is
    cut off; a big record parsed in parts on several threads is the one
    the one-thread parser gives, with the same error for a bad line;
  - a mark of the earlier unsynced adds is retired without uninstalling
    anything;
  - against an HTTP server inside the test: `add` uses a cached index for
    four hours and `add -u` and `upgrade` ask the server; an add that
    fails with a cached index is tried again with a fresh one; a cached
    index that is new enough but damaged makes `add` ask the server;
    `--simulate` asks the server for an old index and leaves the cache as
    it was; a repository that redirects is followed;
  - a broken record stops only the changes that need it, and `oct fix`
    rebuilds it from the signed package;
  - the lookup index knows the tree's directory symlinks and, after a
    series of changes, equals the one `oct fix` rebuilds;
  - a damaged index (a file of buckets missing, the aliases damaged) sends
    a change to every record instead, and a switch killed while going
    through the index is finished without the records it didn't read;
  - an add killed when some of a package's files were linked (the hook
    `link:N`) is finished by the next run, every file of the package is in
    the tree, and the lookup index the recovery leaves is the one `oct fix`
    rebuilds;
  - a package with entries on a second file system (a tmpfs mounted inside
    the root, as root): the journal's name is fsync'ed before the first
    entry goes there, that file system is synced before the mark that
    records the work, the switch ends with a `syncfs`, and the files
    arrive as whole copies; the same for the removal;
  - a plain `mkdir` is taken for exact only where it is: not for a mode
    with bits the umask or a plain `mkdir` would change, and not under a
    set-group-id directory or one with a default ACL;
  - big trees are deleted directory by directory on several threads, a
    directory others may write to through descriptors, and no symlink is
    followed in either.
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
  new packages (14 with their dependencies), and with `KT_JOB=del` it
  removes them again.

  Results on stage 1's code: 310 rounds, 298 of them killed while
  running and 82 with a package half placed. There were 0 failures. With
  the lookup index: 80 rounds of the upgrade and 80 of `KT_JOB=small`,
  0 failures. With one sync for every change, the packed index and the
  switch on threads (23 November 2026): 80 rounds each of the upgrade,
  `small`, `add` and `del`, 0 failures. Those 320 rounds were weaker than
  they look: a change to the checker had broken the step that picks the
  file to watch, and the rounds meant to kill while files are placed
  killed nothing and passed (237 killed while running, 2 with a package
  half placed). A review found it. The test now stops when no file is
  picked and ends as inconclusive when none of those rounds killed
  anything. Run again on the source as committed: 320 rounds, 307 killed
  while running (238 of them inside the switch), 51 with a package half
  placed, 0 failures. One earlier failure, a directory left 0700 by a kill
  between `mkdir` and `chmod`, led to the atomic directory creation
  described under The transaction.
- **Power-cut test,** run by `catalyst-powercut.sh` (as root; it needs
  device-mapper). A kill leaves everything the process wrote in the page
  cache; a power cut also loses what was not written out, and unevenly: a
  file's name can be on disk without its content. The root lives in a
  file system of its own in an image file behind a device-mapper device.
  The change is started; at a random moment the device is suspended and
  the image copied as it is: the disk after a power cut at that moment.
  In two rounds of five catalyst is first stopped at a random point of its
  switch and what it has done so far is put on disk, either all of it or,
  through the fsyncs of a second process, only the names, without the
  contents of files still in the page cache. In one round of five the cut
  comes just after the command returned. The copy is then mounted, and:
  - `oct recover` must succeed, and `oct fix` find nothing to repair;
  - the root must equal the root before the change or the root after a
    clean run of it, entry for entry, the packages' own copies of `etc/`
    files and the lookup index included; after a command run with `--sync`
    that had returned, it must be the root after;
  - the change run again must succeed and give the clean run's root.

  Results on 23 November 2026: 515 rounds, 0 failures. On ext4 (mounted
  `noauto_da_alloc`, so that it writes no file content early on its own
  account): 270 rounds over the add and the removal of eight packages
  with and without scripts, the upgrades, the add and the removal of
  Cataclysm (8,517 files, placed and removed on threads), and an add with
  `--sync`. On btrfs, which Valence's root is: 155 rounds. On XFS: 90. A
  journal was on the cut disk in 168 of the 206 rounds that stopped a
  switch. Before it passed, this test found three defects that the kill
  test could not see: an index marked valid whose new files were still
  empty, a temporary file left in the state directory, and a recovery
  that ran scripts for a change that had been asked for without them.

  After the second review's fixes, on the source as committed: 340 more
  rounds, 0 failures (ext4 215, btrfs 80, XFS 45). A journal was on the
  cut disk in 116 of the 136 rounds that stopped a switch, and in 68 of
  the 88 that stopped it some way in. Only those 88 count now for whether
  a run says anything about a switch under way (it ends as inconclusive
  when fewer than a third of them had their journal): the rounds stopped
  at the switch's first moment always have it, and used to make that
  check pass by themselves.

  One message, for a journal the unsynced adds left cut, was changed
  after these runs and the kill test's last ones; on the binary of this
  commit 40 more kill rounds and 20 more power-cut rounds passed.
