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
