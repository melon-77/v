# vpm: handing phase 10 over

The owner handed phase 10 (ROADMAP.md, "Valence's own package manager, with a folder for every program") to another
developer on 21 November 2026, after stage 1. This note is where to start.

## What exists

- `vpm/` (this folder): the engine, in Rust. `README.md` is its manual: the commands, the layout of a root, the
  transaction, recovery, trust and limits, resolution, **what stage 1 does not do yet**, the measured numbers, and the
  tests.
- `recipes/vpm`: the Valence package, built from this folder (Valence's libzstd and OpenSSL, AGENTS.md rule 46). It
  builds in about half a minute and, installed in a root, verifies the repository's index and packages.
- `docs/apk3-format.md`: the specification of apk-tools 3's formats that vpm was written against, taken from
  apk-tools 3.0.8's source (part A is a step-by-step quick start; part H lists what stayed unconfirmed and where
  apk's own man pages disagree with its code: follow the code).
- `docs/reviews/`: the reports of the agents that built and checked stage 1: the core library (01), the engine (02),
  the review against the specification (03), the security review (04), the kill test (05), the benchmark against apk
  (06) and the fixes made after them (07, with what was left and why).
- `tools/`: the kill test and the benchmark (`tools/README.md`).

## Where stage 1 stands

Reads Valence's signed indexes and packages itself; installs into roots it made, a folder per program
(`Programs/<name>/<version>`) with the usual tree as hard links; one short switch at the end behind a journal. The
same files as apk for valence-base; 310 upgrades killed at random moments without a broken root; 1.6 to 2.2 times
faster than apk (README.md, "Measured").

## What the roadmap still asks (phase 10's "done when")

1. A console install and a desktop install **made by vpm** pass the QEMU tests (`scripts/test-console.sh`,
   `scripts/test-desktop.sh`): the installers (`valence-install` in valence-base, `cal-finish` in
   calamares-valence) and `scripts/mkiso.sh` install with vpm instead of apk.
2. An upgrade killed at any point leaves a system that **boots**: the kill test so far checks a root (files, loader,
   chroot), not a boot.
3. It installs the base system **measurably faster than apk and xbps** on the same machine: apk is measured, xbps is
   not (`tools/bench`; it needs Void's static xbps and base-system downloaded once).

On the way, from the roadmap's stages: the engine on the console system next to apk; then the installers and the ISO;
apk leaves the installed system last (the build machine keeps apk for its sysroots). That means vpm taking over what
apk does on a running Valence today: the snapshot before every change (`valence-snapshot auto`, AGENTS.md
"Snapshots and rollback"), triggers, `/etc/apk/world`, the commit hooks, and `valence-wish` (which calls apk).
Still open in the roadmap: the name, and whether `-dev`, `-doc` and `-lang` parts get folders of their own (today they
do, being packages of their own).

## Known open points (07's "Left, and why", and README.md)

- A signed timestamp in the index (the builder would have to add it): without it a root that never saw the
  repository accepts a replayed older index.
- Live-tree operations through `openat2(RESOLVE_IN_ROOT)` and `*at()` calls (a race with another process changing the
  tree during the switch).
- Packages told apart by name and version, not by identity.
- No repair command, no `--simulate`, no `search`/`info`; power loss untested (only kills).

## Building and testing

Any Rust 1.98 or newer with cargo, OpenSSL's development files and pkg-config:

```sh
cd vpm
cargo build --release
cargo test --release      # the real-data tests read a repository: VPM_REPO=.../repo/x86_64 VPM_KEYS=.../keys/trusted
```

The real-data tests, the kill test and the benchmark need Valence's packages and the key that signed them. They are
not in git (AGENTS.md "Package repository": the `packages` branch, published from the build machine). Ask the owner
for a copy of `repo/x86_64` and `keys/trusted/` until that branch is published.

Rules for the tree (CONTRIBUTING.md): pull requests against `testing`; every recipe change bumps `pkgrel`; a change
that affects installing or booting passes the QEMU tests before it is committed.
