//! Integration tests for catalyst's Stage 2 CLI commands and flags:
//! `--print-arch`, `--arch`, `--simulate`, `list`, `info` (`-e`, `-a`, `-L`,
//! `-W`), `cache clean`, and read-only database access.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

fn oct_bin() -> &'static str {
    env!("CARGO_BIN_EXE_oct")
}

fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("catalyst-test-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Seeds a minimal valid catalyst root with one installed package (`hello-1.0-r0`)
/// owning `usr/bin/hello`.
fn seed_root(root: &Path) {
    fs::create_dir_all(root.join("var/lib/catalyst")).unwrap();
    fs::create_dir_all(root.join("var/cache/catalyst")).unwrap();
    fs::create_dir_all(root.join("etc/apk")).unwrap();
    fs::write(root.join("etc/apk/arch"), "x86_64\n").unwrap();
    fs::write(root.join("var/lib/catalyst/world"), "hello\n").unwrap();

    let pkg_dir = root.join("Programs/hello/1.0-r0");
    fs::create_dir_all(pkg_dir.join(".catalyst")).unwrap();
    fs::create_dir_all(pkg_dir.join("usr/bin")).unwrap();
    fs::write(pkg_dir.join("usr/bin/hello"), b"#!/bin/sh\necho hi\n").unwrap();
    symlink("1.0-r0", root.join("Programs/hello/Current")).unwrap();

    fs::write(
        pkg_dir.join(".catalyst/info"),
        "\
name: hello
version: 1.0-r0
arch: x86_64
origin: hello
license: MIT
description: A tiny greeting tool
url: https://example.org/hello
installed-size: 4096
provides: cmd:hello=1.0-r0
",
    )
    .unwrap();

    fs::write(
        pkg_dir.join(".catalyst/files"),
        "\
d\t0755\troot\troot\t0\t0\t-\t-\t-\tusr
d\t0755\troot\troot\t0\t0\t-\t-\t-\tusr/bin
f\t0755\troot\troot\t18\t0\t-\t-\t-\tusr/bin/hello
",
    )
    .unwrap();
}

#[test]
fn print_arch_and_override() {
    let root = temp_root("arch");
    fs::create_dir_all(root.join("etc/apk")).unwrap();
    fs::write(root.join("etc/apk/arch"), "aarch64\n").unwrap();

    let out = Command::new(oct_bin())
        .args(["--root", root.to_str().unwrap(), "--print-arch"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "aarch64");

    let out = Command::new(oct_bin())
        .args(["--root", root.to_str().unwrap(), "--arch", "x86_64", "--print-arch"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "x86_64");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn info_list_and_who_owns() {
    let root = temp_root("info");
    seed_root(&root);
    let r = root.to_str().unwrap();

    // `oct info` with no args lists installed package names
    let out = Command::new(oct_bin()).args(["--root", r, "info"]).output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");

    // `oct info -e` checks installed packages and provides
    let out = Command::new(oct_bin()).args(["--root", r, "info", "-e", "hello"]).output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");

    let out = Command::new(oct_bin()).args(["--root", r, "info", "-e", "cmd:hello"]).output().unwrap();
    assert!(out.status.success());

    let out = Command::new(oct_bin()).args(["--root", r, "info", "-e", "nonexistent"]).output().unwrap();
    assert!(!out.status.success());

    // `oct info -a hello` prints full metadata and contents
    let out = Command::new(oct_bin()).args(["--root", r, "info", "-a", "hello"]).output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("A tiny greeting tool"), "stdout: {s}");
    assert!(s.contains("MIT"), "stdout: {s}");
    assert!(s.contains("cmd:hello=1.0-r0"), "stdout: {s}");
    assert!(s.contains("usr/bin/hello"), "stdout: {s}");

    // `oct info -W` resolves file ownership
    let out = Command::new(oct_bin()).args(["--root", r, "info", "-W", "/usr/bin/hello"]).output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("owned by hello-1.0-r0"));

    let out = Command::new(oct_bin()).args(["--root", r, "info", "-W", "/usr/bin/nope"]).output().unwrap();
    assert!(!out.status.success());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn simulate_del_makes_no_changes() {
    let root = temp_root("sim");
    seed_root(&root);
    let r = root.to_str().unwrap();

    let out = Command::new(oct_bin()).args(["--root", r, "--simulate", "del", "hello"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("remove   hello 1.0-r0"), "stdout: {s}");
    assert!(s.contains("simulated"), "stdout: {s}");

    // Nothing was removed and world is unchanged
    assert!(root.join("Programs/hello/Current").exists());
    assert_eq!(fs::read_to_string(root.join("var/lib/catalyst/world")).unwrap(), "hello\n");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn cache_clean_removes_apks_and_keeps_indexes() {
    let root = temp_root("cache");
    seed_root(&root);
    let cache = root.join("var/cache/catalyst");
    fs::write(cache.join("hello-1.0-r0.apk"), b"fake apk bytes").unwrap();
    fs::write(cache.join("Packages.12345678.adb"), b"fake index").unwrap();

    let out = Command::new(oct_bin())
        .args(["--root", root.to_str().unwrap(), "cache", "clean"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(!cache.join("hello-1.0-r0.apk").exists());
    assert!(cache.join("Packages.12345678.adb").exists());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn fix_repairs_live_tree_and_preserves_etc() {
    use std::os::unix::fs::MetadataExt;

    let root = temp_root("fix");
    seed_root(&root);
    let pkg_dir = root.join("Programs/hello/1.0-r0");
    fs::create_dir_all(pkg_dir.join("etc")).unwrap();
    fs::write(pkg_dir.join("etc/hello.conf"), b"default=1\n").unwrap();
    fs::write(pkg_dir.join("etc/missing.conf"), b"restore=1\n").unwrap();

    // Update .catalyst/files to include a symlink, an edited etc file, and a missing etc file
    fs::write(
        pkg_dir.join(".catalyst/files"),
        "\
d\t0755\troot\troot\t0\t0\t-\t-\t-\tusr
d\t0755\troot\troot\t0\t0\t-\t-\t-\tusr/bin
f\t0755\troot\troot\t18\t0\t-\t-\t-\tusr/bin/hello
l\t0777\troot\troot\t0\t0\t-\thello\t-\tusr/bin/hi
d\t0755\troot\troot\t0\t0\t-\t-\t-\tetc
f\t0644\troot\troot\t10\t0\t-\t-\t-\tetc/hello.conf
f\t0644\troot\troot\t10\t0\t-\t-\t-\tetc/missing.conf
",
    )
    .unwrap();

    // Seed live tree with:
    // - usr/bin/hello replaced by an unlinked file (wrong inode)
    // - usr/bin/hi pointing to wrong target
    // - etc/hello.conf edited by admin (must be preserved!)
    // - etc/missing.conf missing (must be restored!)
    fs::create_dir_all(root.join("usr/bin")).unwrap();
    fs::create_dir_all(root.join("etc")).unwrap();
    fs::write(root.join("usr/bin/hello"), b"corrupted\n").unwrap();
    symlink("wrong", root.join("usr/bin/hi")).unwrap();
    fs::write(root.join("etc/hello.conf"), b"admin_edit=1\n").unwrap();

    let r = root.to_str().unwrap();

    // --simulate reports repairs without changing disk
    let out = Command::new(oct_bin()).args(["--root", r, "--simulate", "fix"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("3 would be repaired"), "stdout: {s}");
    assert!(!root.join("etc/missing.conf").exists());

    // Real `oct fix` repairs all 3 and keeps admin's etc/hello.conf
    let out = Command::new(oct_bin()).args(["--root", r, "fix"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("3 repaired, 0 errors"), "stdout: {s}");

    let src_md = fs::symlink_metadata(pkg_dir.join("usr/bin/hello")).unwrap();
    let dst_md = fs::symlink_metadata(root.join("usr/bin/hello")).unwrap();
    assert_eq!((src_md.dev(), src_md.ino()), (dst_md.dev(), dst_md.ino()));
    assert_eq!(fs::read_link(root.join("usr/bin/hi")).unwrap(), PathBuf::from("hello"));
    assert_eq!(fs::read_to_string(root.join("etc/hello.conf")).unwrap(), "admin_edit=1\n");
    assert_eq!(fs::read_to_string(root.join("etc/missing.conf")).unwrap(), "restore=1\n");

    // Second `oct fix` has 0 repairs
    let out = Command::new(oct_bin()).args(["--root", r, "fix", "hello"]).output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("0 repaired, 0 errors"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn fix_preserves_etc_symlink_copies_boot_and_detects_programs_corruption() {
    use std::os::unix::fs::MetadataExt;

    let root = temp_root("fix-extra");
    seed_root(&root);
    let pkg_dir = root.join("Programs/hello/1.0-r0");
    fs::create_dir_all(pkg_dir.join("etc/ssl")).unwrap();
    fs::create_dir_all(pkg_dir.join("boot")).unwrap();
    fs::write(pkg_dir.join("boot/vmlinuz-test"), b"kernel-image").unwrap();
    // Real sha256 of "#!/bin/sh\necho hi\n" and "kernel-image"
    let hello_hash = catalyst::digest::hex(&catalyst::digest::sha256(b"#!/bin/sh\necho hi\n"));
    let boot_hash = catalyst::digest::hex(&catalyst::digest::sha256(b"kernel-image"));

    fs::write(
        pkg_dir.join(".catalyst/files"),
        format!(
            "\
d\t0755\troot\troot\t0\t0\t-\t-\t-\tusr
d\t0755\troot\troot\t0\t0\t-\t-\t-\tusr/bin
f\t0755\troot\troot\t18\t0\t{hello_hash}\t-\t-\tusr/bin/hello
d\t0755\troot\troot\t0\t0\t-\t-\t-\tetc
d\t0755\troot\troot\t0\t0\t-\t-\t-\tetc/ssl
l\t0777\troot\troot\t0\t0\t-\tcerts/ca.crt\t-\tetc/ssl/cert.pem
d\t0755\troot\troot\t0\t0\t-\t-\t-\tboot
f\t0644\troot\troot\t12\t0\t{boot_hash}\t-\t-\tboot/vmlinuz-test
"
        ),
    )
    .unwrap();

    // Admin put a custom regular file at etc/ssl/cert.pem — must NOT be overwritten by fix (V-1)!
    fs::create_dir_all(root.join("etc/ssl")).unwrap();
    fs::write(root.join("etc/ssl/cert.pem"), b"custom-ca-bundle\n").unwrap();
    fs::create_dir_all(root.join("usr/bin")).unwrap();
    fs::hard_link(pkg_dir.join("usr/bin/hello"), root.join("usr/bin/hello")).unwrap();

    let r = root.to_str().unwrap();
    let out = Command::new(oct_bin()).args(["--root", r, "fix"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    // etc/ssl/cert.pem kept intact, boot/vmlinuz-test copied (NOT hard-linked!)
    assert_eq!(fs::read_to_string(root.join("etc/ssl/cert.pem")).unwrap(), "custom-ca-bundle\n");
    assert_eq!(fs::read(root.join("boot/vmlinuz-test")).unwrap(), b"kernel-image");
    let boot_src = fs::symlink_metadata(pkg_dir.join("boot/vmlinuz-test")).unwrap();
    let boot_dst = fs::symlink_metadata(root.join("boot/vmlinuz-test")).unwrap();
    assert_ne!(boot_src.ino(), boot_dst.ino(), "/boot file must be a copy, not a hard link");

    // Corrupt usr/bin/hello in-place through the hard link -> Programs copy is corrupted too -> fix detects it (V-2, V-13)!
    fs::write(root.join("usr/bin/hello"), b"tampered-in-place\n").unwrap();
    let out = Command::new(oct_bin()).args(["--root", r, "--simulate", "fix"]).output().unwrap();
    assert!(!out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("ERRORS:"), "simulate fix with error must print ERRORS: (V-13), got: {s}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("corrupt in Programs"));

    // cache clean --simulate does not delete files (V-9)
    let cache = root.join("var/cache/catalyst");
    fs::write(cache.join("demo-1.0.apk"), b"apk").unwrap();
    let out = Command::new(oct_bin()).args(["--root", r, "--simulate", "cache", "clean"]).output().unwrap();
    assert!(out.status.success());
    assert!(cache.join("demo-1.0.apk").exists());

    // info -W skips directories (V-8)
    let out = Command::new(oct_bin()).args(["--root", r, "info", "-W", "/usr/bin"]).output().unwrap();
    assert!(!out.status.success());

    let _ = fs::remove_dir_all(&root);
}


// ------------------------------------------------------------ phase 10, stage A (build machine, 2026-11-22)

fn sha256_hex(b: &[u8]) -> String {
    catalyst::digest::hex(&catalyst::digest::sha256(b))
}

/// A root with `cfg-1` installed (`etc/x` a regular file), `cfg-2` unpacked
/// (`etc/x` a symlink to `x.real`), a live `etc/x` holding `live`, and a
/// journal that upgrades 1 to 2: any locked command finishes it.
fn seed_etc_upgrade(root: &Path, live: &[u8]) {
    for d in ["var/lib/catalyst", "var/cache/catalyst", "etc/apk", "Programs/cfg/1/.catalyst", "Programs/cfg/1/etc", "Programs/cfg/2/.catalyst", "Programs/cfg/2/etc"] {
        fs::create_dir_all(root.join(d)).unwrap();
    }
    fs::write(root.join("etc/apk/arch"), "x86_64\n").unwrap();
    fs::write(root.join("var/lib/catalyst/world"), "cfg\n").unwrap();
    for v in ["1", "2"] {
        fs::write(
            root.join(format!("Programs/cfg/{v}/.catalyst/info")),
            format!("name: cfg\nversion: {v}\narch: x86_64\norigin: cfg\nlicense: MIT\ndescription: test\nurl: https://example.org\ninstalled-size: 4096\n"),
        )
        .unwrap();
    }
    fs::write(root.join("Programs/cfg/1/etc/x"), b"v1\n").unwrap();
    fs::write(
        root.join("Programs/cfg/1/.catalyst/files"),
        format!("d\t0755\troot\troot\t0\t0\t-\t-\t-\tetc\nf\t0644\troot\troot\t3\t0\t{}\t-\t-\tetc/x\n", sha256_hex(b"v1\n")),
    )
    .unwrap();
    fs::write(root.join("Programs/cfg/2/etc/x.real"), b"v2\n").unwrap();
    symlink("x.real", root.join("Programs/cfg/2/etc/x")).unwrap();
    fs::write(
        root.join("Programs/cfg/2/.catalyst/files"),
        format!(
            "d\t0755\troot\troot\t0\t0\t-\t-\t-\tetc\nf\t0644\troot\troot\t3\t0\t{}\t-\t-\tetc/x.real\nl\t0777\troot\troot\t6\t0\t-\tx.real\t-\tetc/x\n",
            sha256_hex(b"v2\n")
        ),
    )
    .unwrap();
    symlink("1", root.join("Programs/cfg/Current")).unwrap();
    fs::write(root.join("etc/x"), live).unwrap();
    fs::write(root.join("var/lib/catalyst/journal"), "catalyst-journal 1\nupgrade cfg 1 2\nworld cfg\nend\n").unwrap();
}

#[test]
fn etc_file_that_becomes_a_symlink_is_replaced_only_when_untouched() {
    // N-3: the admin never changed etc/x, so the new version's symlink replaces it
    let root = temp_root("etc-symlink-pristine");
    seed_etc_upgrade(&root, b"v1\n");
    let out = Command::new(oct_bin()).args(["--root", root.to_str().unwrap(), "--no-scripts", "--plain", "fix"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(fs::read_link(root.join("etc/x")).unwrap(), Path::new("x.real"));
    assert!(fs::symlink_metadata(root.join("etc/x.catalyst-new")).is_err());
    assert_eq!(fs::read_link(root.join("Programs/cfg/Current")).unwrap(), Path::new("2"));

    // the admin changed it: kept, and the new version's symlink next to it
    let root = temp_root("etc-symlink-edited");
    seed_etc_upgrade(&root, b"admin\n");
    let out = Command::new(oct_bin()).args(["--root", root.to_str().unwrap(), "--no-scripts", "--plain", "fix"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(fs::read(root.join("etc/x")).unwrap(), b"admin\n");
    assert!(fs::symlink_metadata(root.join("etc/x")).unwrap().is_file());
    assert_eq!(fs::read_link(root.join("etc/x.catalyst-new")).unwrap(), Path::new("x.real"));
}

#[test]
fn clean_up_walk_runs_only_after_an_interrupted_run() {
    let root = temp_root("dirty-mark");
    seed_root(&root);
    let partial = root.join("Programs/hello/9.0-r0.partial");
    fs::create_dir_all(&partial).unwrap();
    let fix = || Command::new(oct_bin()).args(["--root", root.to_str().unwrap(), "--plain", "fix"]).output().unwrap();
    // a clean root: the walk over Programs is skipped
    assert!(fix().status.success());
    assert!(partial.exists());
    // the dirty mark (left by a change that didn't finish): cleaned up, mark gone
    fs::write(root.join("var/lib/catalyst/dirty"), b"").unwrap();
    assert!(fix().status.success());
    assert!(!partial.exists());
    assert!(!root.join("var/lib/catalyst/dirty").exists());
    assert!(root.join("Programs/hello/1.0-r0/usr/bin/hello").exists());
}

#[test]
fn unreadable_record_blocks_changes_before_any_journal() {
    // N-1: nothing is changed, and no journal written, while a record can't be read
    let root = temp_root("broken-record");
    seed_root(&root);
    fs::write(root.join("Programs/hello/1.0-r0/.catalyst/files"), b"\x00garbled\n").unwrap();
    let out = Command::new(oct_bin()).args(["--root", root.to_str().unwrap(), "--plain", "del", "hello"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("can't be read") && err.contains("oct fix hello"), "{err}");
    assert!(!root.join("var/lib/catalyst/journal").exists());
    assert!(root.join("Programs/hello/1.0-r0/usr/bin/hello").exists());
    // with no repository to rebuild it from, fix reports it and repairs nothing
    let out = Command::new(oct_bin()).args(["--root", root.to_str().unwrap(), "--plain", "fix"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("hello-1.0-r0") && err.contains("nothing is repaired"), "{err}");
}
