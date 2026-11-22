//! Link system libsqlite3 (via pkg-config when available, respecting cross
//! PKG_CONFIG_* variables, with a plain `-lsqlite3` fallback).

use std::env;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=PKG_CONFIG");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_SYSROOT_DIR");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_LIBDIR");

    let pkg_config = env::var("PKG_CONFIG").unwrap_or_else(|_| "pkg-config".into());
    if let Ok(out) = Command::new(&pkg_config).args(["--libs", "sqlite3"]).output() {
        if out.status.success() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let mut linked_sqlite = false;
            for tok in stdout.split_whitespace() {
                if let Some(dir) = tok.strip_prefix("-L") {
                    if !dir.is_empty() {
                        println!("cargo:rustc-link-search=native={dir}");
                    }
                } else if let Some(lib) = tok.strip_prefix("-l") {
                    if !lib.is_empty() {
                        println!("cargo:rustc-link-lib={lib}");
                        if lib == "sqlite3" {
                            linked_sqlite = true;
                        }
                    }
                }
            }
            if linked_sqlite {
                return;
            }
        }
    }
    println!("cargo:rustc-link-lib=sqlite3");
}
