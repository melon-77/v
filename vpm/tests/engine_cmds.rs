//! Integration tests for vpm's Stage 2 CLI commands and flags:
//! `--print-arch`, `--arch`, `--simulate`, `list`, `info` (`-e`, `-a`, `-L`,
//! `-W`), `cache clean`, and read-only database access.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

fn vpm_bin() -> &'static str {
    env!("CARGO_BIN_EXE_vpm")
}

fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vpm-test-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Seeds a minimal valid vpm root with one installed package (`hello-1.0-r0`)
/// owning `usr/bin/hello`.
fn seed_root(root: &Path) {
    fs::create_dir_all(root.join("var/lib/vpm")).unwrap();
    fs::create_dir_all(root.join("var/cache/vpm")).unwrap();
    fs::create_dir_all(root.join("etc/apk")).unwrap();
    fs::write(root.join("etc/apk/arch"), "x86_64\n").unwrap();
    fs::write(root.join("var/lib/vpm/world"), "hello\n").unwrap();

    let pkg_dir = root.join("Programs/hello/1.0-r0");
    fs::create_dir_all(pkg_dir.join(".vpm")).unwrap();
    fs::create_dir_all(pkg_dir.join("usr/bin")).unwrap();
    fs::write(pkg_dir.join("usr/bin/hello"), b"#!/bin/sh\necho hi\n").unwrap();
    symlink("1.0-r0", root.join("Programs/hello/Current")).unwrap();

    fs::write(
        pkg_dir.join(".vpm/info"),
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
        pkg_dir.join(".vpm/files"),
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

    let out = Command::new(vpm_bin())
        .args(["--root", root.to_str().unwrap(), "--print-arch"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "aarch64");

    let out = Command::new(vpm_bin())
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

    // `vpm info` with no args lists installed package names
    let out = Command::new(vpm_bin()).args(["--root", r, "info"]).output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");

    // `vpm info -e` checks installed packages and provides
    let out = Command::new(vpm_bin()).args(["--root", r, "info", "-e", "hello"]).output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");

    let out = Command::new(vpm_bin()).args(["--root", r, "info", "-e", "cmd:hello"]).output().unwrap();
    assert!(out.status.success());

    let out = Command::new(vpm_bin()).args(["--root", r, "info", "-e", "nonexistent"]).output().unwrap();
    assert!(!out.status.success());

    // `vpm info -a hello` prints full metadata and contents
    let out = Command::new(vpm_bin()).args(["--root", r, "info", "-a", "hello"]).output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("A tiny greeting tool"), "stdout: {s}");
    assert!(s.contains("MIT"), "stdout: {s}");
    assert!(s.contains("cmd:hello=1.0-r0"), "stdout: {s}");
    assert!(s.contains("usr/bin/hello"), "stdout: {s}");

    // `vpm info -W` resolves file ownership
    let out = Command::new(vpm_bin()).args(["--root", r, "info", "-W", "/usr/bin/hello"]).output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("owned by hello-1.0-r0"));

    let out = Command::new(vpm_bin()).args(["--root", r, "info", "-W", "/usr/bin/nope"]).output().unwrap();
    assert!(!out.status.success());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn simulate_del_makes_no_changes() {
    let root = temp_root("sim");
    seed_root(&root);
    let r = root.to_str().unwrap();

    let out = Command::new(vpm_bin()).args(["--root", r, "--simulate", "del", "hello"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("remove   hello 1.0-r0"), "stdout: {s}");
    assert!(s.contains("simulated"), "stdout: {s}");

    // Nothing was removed and world is unchanged
    assert!(root.join("Programs/hello/Current").exists());
    assert_eq!(fs::read_to_string(root.join("var/lib/vpm/world")).unwrap(), "hello\n");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn cache_clean_removes_apks_and_keeps_indexes() {
    let root = temp_root("cache");
    seed_root(&root);
    let cache = root.join("var/cache/vpm");
    fs::write(cache.join("hello-1.0-r0.apk"), b"fake apk bytes").unwrap();
    fs::write(cache.join("Packages.12345678.adb"), b"fake index").unwrap();

    let out = Command::new(vpm_bin())
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

    // Update .vpm/files to include a symlink, an edited etc file, and a missing etc file
    fs::write(
        pkg_dir.join(".vpm/files"),
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
    let out = Command::new(vpm_bin()).args(["--root", r, "--simulate", "fix"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("3 would be repaired"), "stdout: {s}");
    assert!(!root.join("etc/missing.conf").exists());

    // Real `vpm fix` repairs all 3 and keeps admin's etc/hello.conf
    let out = Command::new(vpm_bin()).args(["--root", r, "fix"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("3 repaired, 0 errors"), "stdout: {s}");

    let src_md = fs::symlink_metadata(pkg_dir.join("usr/bin/hello")).unwrap();
    let dst_md = fs::symlink_metadata(root.join("usr/bin/hello")).unwrap();
    assert_eq!((src_md.dev(), src_md.ino()), (dst_md.dev(), dst_md.ino()));
    assert_eq!(fs::read_link(root.join("usr/bin/hi")).unwrap(), PathBuf::from("hello"));
    assert_eq!(fs::read_to_string(root.join("etc/hello.conf")).unwrap(), "admin_edit=1\n");
    assert_eq!(fs::read_to_string(root.join("etc/missing.conf")).unwrap(), "restore=1\n");

    // Second `vpm fix` has 0 repairs
    let out = Command::new(vpm_bin()).args(["--root", r, "fix", "hello"]).output().unwrap();
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
    let hello_hash = vpm::digest::hex(&vpm::digest::sha256(b"#!/bin/sh\necho hi\n"));
    let boot_hash = vpm::digest::hex(&vpm::digest::sha256(b"kernel-image"));

    fs::write(
        pkg_dir.join(".vpm/files"),
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
    let out = Command::new(vpm_bin()).args(["--root", r, "fix"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    // etc/ssl/cert.pem kept intact, boot/vmlinuz-test copied (NOT hard-linked!)
    assert_eq!(fs::read_to_string(root.join("etc/ssl/cert.pem")).unwrap(), "custom-ca-bundle\n");
    assert_eq!(fs::read(root.join("boot/vmlinuz-test")).unwrap(), b"kernel-image");
    let boot_src = fs::symlink_metadata(pkg_dir.join("boot/vmlinuz-test")).unwrap();
    let boot_dst = fs::symlink_metadata(root.join("boot/vmlinuz-test")).unwrap();
    assert_ne!(boot_src.ino(), boot_dst.ino(), "/boot file must be a copy, not a hard link");

    // Corrupt usr/bin/hello in-place through the hard link -> Programs copy is corrupted too -> fix detects it (V-2, V-13)!
    fs::write(root.join("usr/bin/hello"), b"tampered-in-place\n").unwrap();
    let out = Command::new(vpm_bin()).args(["--root", r, "--simulate", "fix"]).output().unwrap();
    assert!(!out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("ERRORS:"), "simulate fix with error must print ERRORS: (V-13), got: {s}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("corrupt in Programs"));

    // cache clean --simulate does not delete files (V-9)
    let cache = root.join("var/cache/vpm");
    fs::write(cache.join("demo-1.0.apk"), b"apk").unwrap();
    let out = Command::new(vpm_bin()).args(["--root", r, "--simulate", "cache", "clean"]).output().unwrap();
    assert!(out.status.success());
    assert!(cache.join("demo-1.0.apk").exists());

    // info -W skips directories (V-8)
    let out = Command::new(vpm_bin()).args(["--root", r, "info", "-W", "/usr/bin"]).output().unwrap();
    assert!(!out.status.success());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn sqlite_migration_and_indexed_queries() {
    let root = temp_root("sqlite-migrate");
    seed_root(&root);
    let r = root.to_str().unwrap();

    // Before migration: state.db does not exist; read-only command succeeds via legacy fallback
    // without creating state.db.
    assert!(!root.join("var/lib/vpm/state.db").exists());
    let out = Command::new(vpm_bin()).args(["--root", r, "info", "-W", "/usr/bin/hello"]).output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("owned by hello-1.0-r0"));
    assert!(!root.join("var/lib/vpm/state.db").exists());

    // First writable command (`cache clean`) triggers automatic migration into state.db and removes .vpm/.
    let out = Command::new(vpm_bin()).args(["--root", r, "cache", "clean"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(root.join("var/lib/vpm/state.db").exists(), "state.db should be created on migration");
    assert!(!root.join("Programs/hello/1.0-r0/.vpm").exists(), "legacy .vpm/ dir should be removed after migration");
    assert!(!root.join("var/lib/vpm/state.db-wal").exists(), "DELETE journal mode must leave no -wal sidecar");
    assert!(!root.join("var/lib/vpm/state.db-shm").exists(), "DELETE journal mode must leave no -shm sidecar");

    // Now every query hits SQLite directly: list, info -e, info -a, info -W (direct + symlink), fix.
    let out = Command::new(vpm_bin()).args(["--root", r, "list"]).output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("hello-1.0-r0 x86_64 {hello} (MIT) [world]"), "stdout: {s}");

    let out = Command::new(vpm_bin()).args(["--root", r, "info", "-e", "cmd:hello"]).output().unwrap();
    assert!(out.status.success());

    let out = Command::new(vpm_bin()).args(["--root", r, "info", "-a", "hello"]).output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("usr/bin/hello"));

    // Create a symlink in the live tree and verify indexed `info -W` resolves through it.
    fs::create_dir_all(root.join("usr/bin")).unwrap();
    symlink("hello", root.join("usr/bin/hello-link")).unwrap();
    let out = Command::new(vpm_bin()).args(["--root", r, "info", "-W", "/usr/bin/hello-link"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("owned by hello-1.0-r0"));

    // `vpm fix` repairs from SQLite records + Programs/hello/1.0-r0/usr/bin/hello
    let out = Command::new(vpm_bin()).args(["--root", r, "fix"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(root.join("usr/bin/hello").exists());

    // Verify `del` removes hello from SQLite AND updates var/lib/vpm/world mirror
    let out = Command::new(vpm_bin()).args(["--root", r, "--no-scripts", "del", "hello"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(!root.join("Programs/hello").exists());
    assert_eq!(fs::read_to_string(root.join("var/lib/vpm/world")).unwrap().trim(), "");
    let out = Command::new(vpm_bin()).args(["--root", r, "list"]).output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn readonly_sqlite_mount_queries_succeed() {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_root("sqlite-ro");
    seed_root(&root);
    let r = root.to_str().unwrap();

    // Migrate to SQLite and populate live tree first
    let out = Command::new(vpm_bin()).args(["--root", r, "fix"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(root.join("var/lib/vpm/state.db").exists());

    // Lock var/lib/vpm and state.db strictly read-only (simulating a live squashfs ISO)
    let state_dir = root.join("var/lib/vpm");
    let db_file = state_dir.join("state.db");
    let progs_dir = root.join("Programs");
    fs::set_permissions(&db_file, fs::Permissions::from_mode(0o444)).unwrap();
    fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o555)).unwrap();
    fs::set_permissions(&progs_dir, fs::Permissions::from_mode(0o555)).unwrap();

    let run = |args: &[&str]| {
        let mut cmd = Command::new(vpm_bin());
        cmd.args(["--root", r]).args(args);
        cmd.output().unwrap()
    };

    let out = run(&["list"]);
    assert!(out.status.success(), "list failed on RO root: {}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("hello-1.0-r0"));

    let out = run(&["info", "-a", "hello"]);
    assert!(out.status.success(), "info -a failed on RO root: {}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("usr/bin/hello"));

    let out = run(&["info", "-e", "cmd:hello"]);
    assert!(out.status.success(), "info -e failed on RO root: {}", String::from_utf8_lossy(&out.stderr));

    let out = run(&["info", "-W", "/usr/bin/hello"]);
    assert!(out.status.success(), "info -W failed on RO root: {}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("owned by hello-1.0-r0"));

    let out = run(&["--simulate", "del", "hello"]);
    assert!(out.status.success(), "--simulate del failed on RO root: {}", String::from_utf8_lossy(&out.stderr));

    let out = run(&["--simulate", "fix"]);
    assert!(out.status.success(), "--simulate fix failed on RO root: {}", String::from_utf8_lossy(&out.stderr));

    // Zero sidecars or temp files created in var/lib/vpm
    assert!(!state_dir.join("state.db-wal").exists());
    assert!(!state_dir.join("state.db-shm").exists());

    // Restore permissions before cleanup
    fs::set_permissions(&progs_dir, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&db_file, fs::Permissions::from_mode(0o644)).unwrap();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn sqlite_crash_recovery_rolls_forward() {
    use std::os::unix::fs::MetadataExt;
    use vpm::adb::{Dependency, FileKind};
    use vpm::db::{Db, FileRec, Journal, JournalFile, Op, PkgMeta, RecKind, StagedPkg};

    let make_pkg =
        |ver: &str, dir_name: &str, identity: Vec<u8>, hello_body: &[u8], extra_file: Option<(&[u8], &[u8])>| -> StagedPkg {
            let mut meta = PkgMeta::default();
            meta.info.name = "hello".into();
            meta.info.version = ver.into();
            meta.info.arch = "x86_64".into();
            meta.info.origin = "hello".into();
            meta.info.license = "MIT".into();
            meta.info.hashes = identity;
            meta.info.description = format!("hello {ver}");
            meta.info.provides = vec![Dependency::parse(&format!("cmd:hello={ver}")).unwrap()];
            let mut recs = vec![
                FileRec {
                    kind: RecKind::Dir,
                    mode: 0o755,
                    user: None,
                    group: None,
                    size: 0,
                    mtime: 0,
                    hash: vec![],
                    xattrs: vec![],
                    path: b"usr".to_vec(),
                },
                FileRec {
                    kind: RecKind::Dir,
                    mode: 0o755,
                    user: None,
                    group: None,
                    size: 0,
                    mtime: 0,
                    hash: vec![],
                    xattrs: vec![],
                    path: b"usr/bin".to_vec(),
                },
                FileRec {
                    kind: RecKind::File(FileKind::Regular),
                    mode: 0o755,
                    user: None,
                    group: None,
                    size: hello_body.len() as u64,
                    mtime: 100,
                    hash: vpm::digest::sha256(hello_body).to_vec(),
                    xattrs: vec![],
                    path: b"usr/bin/hello".to_vec(),
                },
            ];
            if let Some((path, body)) = extra_file {
                recs.push(FileRec {
                    kind: RecKind::File(FileKind::Regular),
                    mode: 0o755,
                    user: None,
                    group: None,
                    size: body.len() as u64,
                    mtime: 100,
                    hash: vpm::digest::sha256(body).to_vec(),
                    xattrs: vec![],
                    path: path.to_vec(),
                });
            }
            StagedPkg { dir_name: dir_name.into(), meta, recs, scripts: vec![] }
        };

    // Case 1: killed right after journal creation (before placed:hello)
    // Case 2: killed right after placed:hello (Current + state.db already switched, old dir & journal remain)
    // Case 3: same-version rebuild (1.0-r0 -> 1.0-r0+22222222) killed before placed:hello
    for (tag, already_placed, new_ver, new_dir) in [
        ("before-place", false, "2.0-r0", "2.0-r0"),
        ("after-place", true, "2.0-r0", "2.0-r0"),
        ("same-ver-rebuild", false, "1.0-r0", "1.0-r0+22222222"),
    ] {
        let root = temp_root(&format!("sqlite-recover-{tag}"));
        let r = root.to_str().unwrap();

        let v1_body = b"#!/bin/sh\necho v1\n";
        let v1_old = b"old-only-tool\n";
        let v2_body = b"#!/bin/sh\necho v2\n";
        let v2_new = b"new-only-tool\n";

        let sp1 = make_pkg("1.0-r0", "1.0-r0", vec![0x11; 32], v1_body, Some((b"usr/bin/old-only", v1_old)));
        let sp2 = make_pkg(new_ver, new_dir, vec![0x22; 32], v2_body, Some((b"usr/bin/new-only", v2_new)));

        let db = Db::open(&root, true).unwrap();
        let p1 = Db::pkg_dir(&root, "hello", "1.0-r0");
        let p2 = Db::pkg_dir(&root, "hello", new_dir);
        fs::create_dir_all(p1.join("usr/bin")).unwrap();
        fs::write(p1.join("usr/bin/hello"), v1_body).unwrap();
        fs::write(p1.join("usr/bin/old-only"), v1_old).unwrap();
        fs::create_dir_all(p2.join("usr/bin")).unwrap();
        fs::write(p2.join("usr/bin/hello"), v2_body).unwrap();
        fs::write(p2.join("usr/bin/new-only"), v2_new).unwrap();

        fs::create_dir_all(root.join("usr/bin")).unwrap();
        if already_placed {
            fs::hard_link(p2.join("usr/bin/hello"), root.join("usr/bin/hello")).unwrap();
            fs::hard_link(p2.join("usr/bin/new-only"), root.join("usr/bin/new-only")).unwrap();
            vpm::db::set_current(&root, "hello", new_dir).unwrap();
        } else {
            fs::hard_link(p1.join("usr/bin/hello"), root.join("usr/bin/hello")).unwrap();
            fs::hard_link(p1.join("usr/bin/old-only"), root.join("usr/bin/old-only")).unwrap();
            vpm::db::set_current(&root, "hello", "1.0-r0").unwrap();
        }

        db.stage_packages(&[sp1, sp2]).unwrap();
        db.mark_installed("hello", "1.0-r0", None).unwrap();
        if already_placed {
            db.mark_installed("hello", new_dir, Some("1.0-r0")).unwrap();
        }
        db.save_world(&[Dependency::parse_world("hello").unwrap()]).unwrap();

        let mut jf = JournalFile::create(
            &root,
            Journal {
                ops: vec![Op::Upgrade {
                    name: "hello".into(),
                    old: "1.0-r0".into(),
                    old_dir: "1.0-r0".into(),
                    new: new_ver.into(),
                    new_dir: new_dir.into(),
                }],
                world: vec!["hello".into()],
                done: Default::default(),
            },
        )
        .unwrap();
        if already_placed {
            jf.mark("placed:hello").unwrap();
        }
        drop(jf);
        drop(db);

        // Next writable vpm invocation rolls the interrupted switch forward cleanly
        let out = Command::new(vpm_bin())
            .args(["--root", r, "--no-scripts", "cache", "clean"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{tag} recovery failed: {}", String::from_utf8_lossy(&out.stderr));
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("finishing an interrupted switch"),
            "{tag} stdout: {}",
            String::from_utf8_lossy(&out.stdout)
        );

        // Journal & old version cleaned up; Current -> new_dir
        assert!(!root.join("var/lib/vpm/journal").exists(), "{tag}: journal still present");
        assert!(!p1.exists(), "{tag}: old version dir 1.0-r0 still present");
        assert_eq!(fs::read_link(root.join("Programs/hello/Current")).unwrap(), PathBuf::from(new_dir));

        // Live tree matches new build (hard-linked) and old-only file is gone
        assert_eq!(fs::read(root.join("usr/bin/hello")).unwrap(), v2_body);
        assert_eq!(fs::read(root.join("usr/bin/new-only")).unwrap(), v2_new);
        assert!(!root.join("usr/bin/old-only").exists(), "{tag}: old-only file was not removed");
        let src_md = fs::symlink_metadata(p2.join("usr/bin/hello")).unwrap();
        let dst_md = fs::symlink_metadata(root.join("usr/bin/hello")).unwrap();
        assert_eq!((src_md.dev(), src_md.ino()), (dst_md.dev(), dst_md.ino()));

        // SQLite state matches new_dir only (1.0-r0 rows purged)
        let ro = Db::open_readonly(&root, false).unwrap();
        assert!(!ro.has_pkg("hello", "1.0-r0").unwrap(), "{tag}: 1.0-r0 row was not purged from state.db");
        assert!(ro.has_pkg("hello", new_dir).unwrap());
        assert_eq!(ro.installed["hello"].meta.version(), new_ver);
        assert_eq!(ro.installed["hello"].dir_name, new_dir);
        assert_eq!(ro.installed["hello"].meta.info.hashes, vec![0x22; 32]);
        assert_eq!(ro.find_path_owners(b"usr/bin/hello").unwrap(), Some(vec!["hello".into()]));
        assert_eq!(ro.find_path_owners(b"usr/bin/old-only").unwrap(), Some(vec![]));
        assert_eq!(ro.find_path_owners(b"usr/bin/new-only").unwrap(), Some(vec!["hello".into()]));

        let _ = fs::remove_dir_all(&root);
    }
}
