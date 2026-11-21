# Contributing to Valence

Thank you for helping. Valence is built by a few people on their own computers, so these rules keep the work from
breaking each other's builds.

## Branches

- **Open pull requests against `testing`, never `main`.** `main` is what ISOs and the package repository are built
  from. It moves only when the owner has reviewed and tested `testing` and brings it over. Pull requests against
  `main` are closed.
- One change per pull request, small enough to review. Say in its description what it changes, why, and how you
  tested it.
- Never two people (or two sessions) in one checkout: use a branch or a `git worktree` of your own.

## Before you start

- **[AGENTS.md](AGENTS.md)** is the build system's manual: the recipe format and the numbered rules that keep the
  build working. Read the rules that touch what you change; they were each learned from a build that broke.
- **[BUILDING.md](BUILDING.md)** sets up a build machine (Ubuntu 24.04, Debian, WSL2 on Windows) and says how long
  things take.
- **[ROADMAP.md](ROADMAP.md)** is the plan. Work that is not on it, or that would change a decision listed under
  "Decided", should be discussed in an issue first.

## Changing a package

Packages are built from `recipes/<name>/VALENCEBUILD` by `scripts/valence-build`, each in a sysroot that holds only
what its recipe declares.

- **Every change to a recipe raises its `pkgrel`** (or `pkgver`, which resets `pkgrel` to 0). Without it installed
  systems never see the new package, and the builder refuses to treat the change as built (rule 6).
- **Declare what the build needs** in `makedepends`, as `-dev` packages. A clean sysroot has nothing else in it: a
  missing dependency fails the build or, worse, silently switches a feature off. `scripts/compare-with-index.py`
  shows the second kind.
- **Sources come from the project's own release**, downloaded from its own site and checked against its signature
  or its published checksum, never from another distribution's archive. Each source is a line in
  `sources/MANIFEST.tsv` with its SHA-256. The commit message says how the source was checked.
- No binaries in git, and never a private key. Packages are signed with a key that stays on the build machine; for
  your own builds, make your own (`BUILDING.md`).

## Testing

A change that can affect booting or installing must pass the QEMU tests before it is committed
(AGENTS.md, "Testing"):

```sh
scripts/valence-build --edition console && scripts/mkiso.sh
scripts/test-console.sh       # install, boot on BIOS and UEFI, roll back, all again encrypted, dual boot
```

The eight console tests take about 13 minutes without KVM. When the desktop or an installer changes, also:

```sh
scripts/valence-build --edition desktop && VALENCE_EDITION=desktop scripts/mkiso.sh
scripts/test-desktop.sh       # the live desktop, and an install that logs in to Plasma, on BIOS and UEFI
```

That is about 20 minutes; look at the screenshots it names. Calamares has no automated test: when
`calamares`, `kpmcore` or `calamares-valence` change, click through it with `scripts/qemu-gui.py` as AGENTS.md
describes, on BIOS, on UEFI and with "Encrypt system".

A change to documentation only needs no tests, but the documentation must stay true: when you change how something
works, change the text that describes it in the same commit.

## Commits

- Write what the commit changes and why, for someone reading `git log` in a year. Name the tests you ran and how
  you checked new sources.
- Keep generated files and their generator in step: some recipes are written by `scripts/gen-*.py`; change the
  generator and run it, not only its output.

## Reporting a problem

Open an issue with what you did, what happened and what you expected. On the computer it happened on,
`valence-hwreport` writes a description of the hardware and the system that helps.
