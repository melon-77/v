## vpm spec conformance review: readers and resolver

The readers match the spec and apk on all the real data. The resolver gives the same results as apk on the real repository, but synthetic repositories show 9 cases where it resolves differently. There are also a few problems in how repositories and indexes are handled. I did not change any vpm code.

### What I checked, and the evidence
- **Readers against apk, field by field:** for all 1102 packages and `Packages.adb`, `vpm dump --json` was compared with `apk adbdump --format json` (with 0 and empty values normalised away). There were 0 differences: same fields, slots, dependency rendering, ACLs, targets, hashes, scripts and triggers.
  - The real data contains 629 hard links (none with `./` targets), 22,861 symlinks (10 absolute, 1,883 containing `..`), 14 set-id/sticky files, 7 files and 2 directories with non-root owners, 87 empty files, and 3 packages with triggers. Every hash is 32 bytes. There are no xattrs or device nodes.
- **Version comparison:** 1,923 pairs (all real versions, related pairs, random pairs, and the §F1.6 and edge cases) were run through vpm and `apk version -t`.
  - All 1,885 pairs of valid versions agree.
  - The 38 other pairs contain an invalid version, which `apk version -t` refuses to compare.
  - Validity agrees on all 19 invalid strings (`apk version -c`).
- **Container:** `apk mkpkg` packages compressed as `ADB.`, `ADBd`, `ADBc` deflate:5, zstd and zstd:19, plus a zstd index, all verify in vpm. So does a package with two SIG blocks from `adbsign`, including when only the second key is trusted. A NUL inside an xattr value parses correctly. A broken `adbsign` output was rejected by both apk and vpm.

### Resolution: 25 requests on the real repository
`apk add --simulate` was compared with a harness that calls vpm's own `solver::solve` exactly as `vpm add` does (fresh root, `Mode::Keep`, the requested names as "fresh").

- **Same package set for every request that apk can resolve (23 of 23):** valence-base (39), valence-desktop (40), plasma-desktop (169), plasma-workspace-dev (275), qt6-qtbase-dev (109), so:libcurl.so.4 (9), so:libc.so.6 (2), so:libQt6Core.so.6 (51), pc:zlib (4), pc:Qt6Core>=6.10 (109), valence-lxqt (115), gcc (14), "python3 perl" (18), rust (17), calamares (83), "konsole dolphin" (125), mesa (23), curl-dev (17), "nano docs" (13, install_if -doc packages included), "busybox>=1.37 ncurses=6.6-r2" (6), valence-vm-guest (19), bash-doc (8), sddm (56).
- **Both refuse:** `busybox<1.0` and `valence-base !busybox`.
- **Install order:** identical in 20 cases. It differs in 3 ("python3 perl", "konsole dolphin", "nano docs"), see R8.

The real index has no name with more than one provider, no provider-priority, no unversioned provides, no conflicts in depends and one version per name. That is why the real data cannot show the resolver differences below; I built a signed synthetic repository to show them.

### Mismatches, most severe first
Each finding gives the reference, vpm's code location, the input that shows it, and the fix.

**R5. Constraints on one name are not merged before the version is picked.**
- Reference: apk merges the constraints of all requirers before selecting a name (solver.c `apply_constraint`, `dependencies_merged`, and the merge-group rules at 585-595).
- vpm: `solver.rs` `require`/`pick` (200-229, 136-162) picks the newest version at the first request and never revisits it.
- Input: t6-app depends `t6-lib`; t6-other depends `t6-lib<2`; t6-lib 1-r0 and 2-r0 exist.
  - `add t6-app t6-other` → apk installs t6-lib-1-r0.
  - vpm fails: "unable to satisfy 't6-lib<2' (required by t6-other-1-r0): t6-lib-2-r0 is already chosen".
  - The author's note covered two pins that cannot both hold. These two constraints are compatible.
- Fix: collect all pending constraints per name (expand depends breadth-first and pick a name only when its queue entries are all known), or backtrack: when a later constraint fails, re-pick the name from the intersection and re-expand.

**R7. `vpm add NAME` upgrades an installed NAME.**
- Reference: apk prefers the installed version unless `-u` is given (solver.c 630-638; app_add.c 42-45 and 178).
- vpm: `engine.rs` `add` (415-421) passes the added names as `fresh`, and `solver.rs:172` then prefers the newest.
- Input: t9-x-1-r0 installed (world `t9-x=1-r0`), t9-x-2-r0 available, then `add t9-x`.
  - vpm: "upgrade t9-x 1-r0 -> 2-r0".
  - apk: no change, and the world becomes `t9-x`.
- Fix: only treat a name as fresh when it is not installed, or add an explicit `-u`.

**P1. A base-URL repository looks for packages without the `${arch}/` directory.**
- Reference: §G1.5 and §G1.6 (context.c:38). The default spec for v2/v3 base repositories is `${arch}/${name}-${version}.apk`, using the package's own arch (so noarch packages are under `base/noarch/`). Only index-URL ("ndx") repositories use `${name}-${version}.apk`.
- vpm: `repo.rs` `locate` (94-109) and `Repo::package_url` (193-201) always use `${name}-${version}.apk` relative to the base.
- Input: `--repository /root/vpm-test/cr/synth add t9-x`.
  - vpm: "/root/vpm-test/cr/synth/t9-x-2-r0.apk: No such file".
  - apk with `v3 /root/vpm-test/cr/synth` installs from `synth/x86_64/`.
- Fix: return a default spec from `locate`: `${arch}/${name}-${version}.apk` for base repositories, `${name}-${version}.apk` for `.adb` URLs.

**R1. Provider preference order is not apk's.**
- Reference: `compare_providers` (solver.c 537-689). After the selectable, merge, conflict, pinning and installed rules it compares, in order:
  1. the provided version of the requested name (641-649);
  2. the package version when the principal name is the same (652-661);
  3. provider_priority (663-668);
  4. installed (670-677);
  5. the lowest repository (686-688).
  - apk has no "own name first" rule.
- vpm: `solver.rs` `better` (165-198) prefers the package's own name first, and compares provider_priority before the provided version.
- Inputs:
  - **T1:** t1-a provides `t1-virt=2` with priority 1; t1-b provides `t1-virt=1` with priority 100; t1-app depends `t1-virt`. apk picks t1-a, vpm picks t1-b.
  - **T4:** t4-x-1-r0 exists, and t4-y provides `t4-x=2`; t4-app depends `t4-x`. apk picks t4-y, vpm picks t4-x.
- Fix: reorder `better` as above:
  1. drop the own-name rule;
  2. in Keep mode, prefer the installed package only when it has the same name or either provided version is non-empty;
  3. then the provided version, the principal-name version when the names match, provider_priority, installed, and the lower repository.

**R4. Conflicts are checked only after selection.**
- Reference: apk disqualifies conflicting candidates while it chooses (solver.c 591-595 and `apply_constraint`).
- vpm: `check_conflicts` (`solver.rs` 287-311) runs at the end.
- Input: t5-app depends `t5-virt !t5-a`; t5-a provides `t5-virt=2`; t5-b provides `t5-virt=1`.
  - apk installs t5-b.
  - vpm fails: "conflict: t5-app-1-r0 conflicts with '!t5-a'".
- Fix: keep the set of active conflicts (world and chosen depends) and have `pick` skip any candidate they match.

**R3. Two chosen packages may provide the same versioned name.**
- Reference: apk refuses this with "conflict: same name provided" (`assign_name`, solver.c 691-736). Two unversioned provides of one name are allowed.
- vpm: `solver.rs` `pick` (145-149) only checks the candidate's own name.
- Input: t3-a and t3-b both provide `so:libt3.so.1=1`; `add t3-a t3-b`. apk refuses, vpm installs both.
- Fix: track the chosen provider per provided name, reject a second provider unless both provided versions are empty, and skip such candidates in `pick`.

**R2. Unversioned providers are auto-selected.**
- Reference: apk only auto-selects a provider when the provided version is not empty, or provider_priority is not 0, or the provider's own name has requirers (`is_provider_auto_selectable`, solver.c 376-383, used at 762-765).
- vpm: `pick` (`solver.rs` 136-162) takes any such provider.
- Input: t2-a provides `t2-virt` (no version, priority 0); t2-app depends `t2-virt`.
  - apk refuses: "please select one of the 'provided by' packages explicitly".
  - vpm installs t2-a.
- Fix: skip candidates where `pv == ""`, `provider_priority == 0`, and the candidate's own name is neither chosen nor required.

**R6. install_if is only evaluated for the newest version of a name.**
- Reference: apk evaluates install_if for every provider (`reconsider_name`, solver.c 419-440).
- vpm: `install_if_round` (`solver.rs` 243-285) picks the best candidate first, then tests only that one.
- Input: t7-foo-doc-1 has install-if `t7-foo=1-r0 t7-docs`; t7-foo-doc-2 has install-if `t7-foo=2-r0 t7-docs`; request `t7-foo<2 t7-docs`. apk adds t7-foo-doc-1, vpm adds no t7-foo-doc.
  - In the real index, all 255 -doc/-lang packages use install-if `X=VER docs/lang`. As soon as the repository keeps two versions and a user pins one, the docs and translations would silently be missing.
- Fix: try all candidates of the name, best first, and take the first whose install_if conditions hold.

**A1. One undecodable index entry rejects the whole repository.**
- Reference: names and strings are opaque bytes (§D4, §F4.1). apk degrades bad values to 0 or NULL (§B7.1), or skips only that entry.
- vpm:
  - `schema.rs` `ident` (98-104) requires UTF-8 for name, version, arch, origin, dependency names and tags.
  - `Index::from_adb` (283-300) turns any entry error into an error for the whole index.
- Input: a package built by `apk mkpkg --info origin:caf<0xE9>` plus a normal package u-good, indexed by `apk mkndx`.
  - vpm: "index package #1: origin is not valid UTF-8", and nothing can be installed from that repository.
  - apk installs u-good.
- Fix: keep these fields as bytes (or lossy strings for origin, arch and tags), and skip a bad entry with a warning instead of failing the index.

**R14. Invalid dependency text is accepted.**
- Reference: apk `add` and world parsing use `apk_blob_pull_dep`; an invalid version marks the dependency broken and `apk add` refuses it (app_add.c 164-171).
- vpm: `version::parse_dep` (`version.rs` 310-330), used by `engine::parse_names` (`engine.rs` 408-413) and `db::read_world`, accepts any version.
- Input: `busybox>=x`.
  - vpm installs busybox, because "x" compares as an INVALID token and so 1.37.0-r1 counts as GREATER.
  - apk: "'busybox>=x' is not a valid world dependency".
  - `busybox@main` becomes the literal name "busybox@main" in vpm.
- Fix: validate the version unless `(op & 6) == 6`, and reject `@` in names.

### Lower-severity mismatches
- **A10. Dependencies are stored lossily in `.vpm/info`.**
  - Location: `db.rs` `format_info` and `parse_info` (328-338, 376-380) write `Dependency::to_string()` and read it back with `parse`.
  - Problem cases:
    - op 7 with a version renders as "foo1" (§G2 quirk);
    - op masks with no text form render as "?";
    - an empty but present version (op EQUAL, §F3.5) renders as "foo=". That fails to parse, so `Db::reload` fails and every command on that root fails.
  - Writers do not produce these.
  - Fix: store name, op and version as separate fields.
- **R8. Install order.** apk sorts the world by name before solving (solver.c 1048-1052, 1083) and installs install_if packages right after the package that triggers them. vpm keeps the request order and appends install_if packages at the end (`order`, 315-348). Both orders put dependencies first; only script order differs. Example "nano docs": apk installs `less less-doc zlib zlib-doc …`, vpm installs `nano less … less-doc mandoc-doc nano-doc zlib-doc`.
- **R9. Packages are keyed by (name, version), not by identity.**
  - Reference: apk keys packages by the first 20 bytes of `hashes` (§D10.4, database.c 587).
  - vpm: `Universe::new` (`solver.rs` 58-84).
  - Effect: a rebuild with the same name and version is never seen as different (already noted by the author).
- **R10-R12. Small provider and arch details.**
  - Provides that repeat the package's own name are dropped (`solver.rs:89`); apk registers them (database.c 602-604).
  - provider_priority is u64; apk truncates it to 16 bits (package.c 576).
  - Only the first line of `etc/apk/arch` is used and there is no `--arch`; apk uses every line (database.c 546-556, 2048-2059).
- **P2-P5. Repository line parsing** (`repo.rs` 61-109, 159-189).
  - A line without a keyword that does not end in `.adb` is treated as v3; apk treats it as v2, and `.tar.gz` as ndx (repoparser.c 182-197).
  - Trailing `/` is trimmed before the `.adb` test; apk tests the raw word.
  - `${var:N}` truncates characters, not bytes, and `${var:}` is rejected (apk: N = 0).
  - `repositories.d/*.list` is not read.
  - `#` and CR handling is more lenient than apk's.
- **A3. NULL items in `paths` shift the DATA indices.**
  - Location: `schema.rs` 551-554 skips the item; `package.rs:277` then uses `d+1` as the path index.
  - apk counts NULL slots (extract_v3.c 228-243), so a later DATA block's `path_idx` no longer matches in vpm. Writers never emit NULL items (adb.c 1040).
  - Fix: keep a placeholder entry.
- **S1. Script `argv[0]`.** apk passes the relative path `lib/apk/exec/NAME-VER.TYPE` (package.c 743, 754, 757). vpm uses `/var/lib/vpm/exec/…` (`commit.rs:466`). Script names, arguments, environment (`PATH`, `APK_SCRIPT`, `APK_PACKAGE`) and umask 022 all match.
- **S4. Scripts of a package with installed-size 0.** apk never unpacks such a package, so its scripts do not run (database.c 3244). vpm runs them. mkpkg raises installed-size to 1 when a package has scripts, so this only affects hand-made packages.
- **F1. Duplicate user names in `etc/passwd`.** apk's id cache returns the last entry (io.c 1317-1329); `unpack.rs` `parse_ids` (45, `or_insert`) keeps the first.
- **T-1. Trigger glob `/` never fires.** `commit.rs` 764-772 leaves the root directory out of the installed directories. Everything else about triggers matches apk's `fire_triggers` (database.c 2418-2444): `+` prefix, `fnmatch` with `FNM_PATHNAME`, and the "run all for changed packages" rule.
- **Negligible:**
  - `MAX_SIG_PAYLOAD` is 128K-8; apk's real limit is 128K-4 for a short-form header (§C1.4).
  - The dependency `match` value is cut to u8 after the `op == 0` test (apk cuts to int first, package.c 356-369).
  - The hard-link entry's own ACL and mtime are not applied; it shares the inode, so there is no visible effect.

### Deviations that are allowed or deliberate (no fix needed)
- **Root-level hard link targets `./name`:** vpm strips `./` and creates the link (`schema.rs` 523-528). apk prints "no hard link target (./top) in archive" and skips it (tested). §E2 allows stripping.
- **Stricter rejections, as the spec recommends:** these inputs are rejected outright, where apk would degrade them to 0 or NULL or skip the entry:
  - mode bits above 07777;
  - a NUL inside a link target;
  - values of the wrong type, TRUE/FALSE, object slot count 0;
  - `ADBc` with algorithm 0;
  - trailing bytes after the deflate stream;
  - a hard link to a non-earlier file.

### Areas that conform
- **Container:** the `ADB` prefix and algorithms, the file header and schema, short and EXT block headers (inner type 3 or higher rejected), the padding check, `ADB SIG* DATA*` order, the 16..0x0fffffff ADB payload size, and `compat_ver`.
- **Values:** INT, INT_32, INT_64 and BLOB_8/16/32 with bounds checks; arrays told apart from objects by the schema.
- **Schema slots:** every slot of §D2-§D9.
- **Dependencies:** `apk_dep_from_adb` op normalisation, the op bits, `op_string`, `result_mask`, and fuzzy matching.
- **Providers:** opaque `so:`/`pc:`/`cmd:` names, the ignored op of provides, and the unversioned-provider quirk of §F4.4.
- **Identity and signatures:** the identity check (32/64 bytes; 20 bytes skipped), SIG verification (`hash_alg` 3/4/5, signed message, signature length), key IDs, and key directory shadowing.
- **Files:** DATA matching (path index, file index, size), hash by length, missing ancestors created 0755 root:root, xattrs split at the first NUL and not applied to symlinks or directories, no mtime on symlinks or directories, set-id bits restored after chown, owner resolution (`root` gives 0, unknown names give 65534).

### Files
- Harness, scripts and outputs (all in `C:\Users\a\AppData\Local\Temp\claude\39afd7d0aa98685a\9f48dd28-e365-4eae-aba4-ec797468f31d\scratchpad\`):
  - `vpm-cr-solvecheck.rs` (the harness), `vpm-cr-requests.txt`, `vpm-cr-compare.py`
  - `vpm-cr-run20.sh`, `vpm-cr-synth.sh` and its output `vpm-cr-synth.out`
  - `vpm-cr-dumpall.py` and its output `vpm-cr-dumpall.out`
  - `vpm-cr-vercmp.sh`, `vpm-cr-comp.sh`, `vpm-cr-comp2.sh`, `vpm-cr-utf8.sh`, `vpm-cr-badver.sh`
  - `vpm-cr-analyze.py` and its output `vpm-cr-analysis.txt`
- Build, data and test roots: `/root/vpm-test/cr/` holds the harness build (`solvecheck/`, `target/`), the outputs (`vpm20.txt`, `apk20/`, `vpm-synth.txt`, `apks/`, `vpm-ver.txt`, `apk-ver.txt`), the synthetic repository (`synth/x86_64`, signed with the throwaway key `keys/cr-test.rsa`), and the test roots `apkroot`, `sroot`, `v9`, `a9`, `v11`, `a11`, `vc`, `ac`, `vu`, `au`, `comp`, `utf8`. Nothing is mounted there.
- Nothing under `/root/valence` was modified.