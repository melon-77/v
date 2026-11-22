//! vpm: Valence's package manager, core library.
//!
//! This crate reads the signed apk v3 files that Valence's builder makes:
//! the repository index (`Packages.adb`, schema `indx`) and package files
//! (`name-version.apk`, schema `pckg`). The format follows apk-tools 3.0.8;
//! section references such as "§B5" point into the format specification
//! (`apk3-format.md`).
//!
//! Modules:
//! - [`adb`]: the container (compression, blocks, values) and typed readers
//!   for indexes and packages, including a streaming reader of package
//!   contents that checks every file against its hash.
//! - [`verify`]: trusted keys, key ids, and signature/identity checks.
//! - [`version`]: version comparison and dependency matching.
//! - [`digest`]: SHA-1/256/512 helpers.
//! - [`dump`]: JSON rendering compatible with `apk adbdump --format json`.
//!
//! The install engine:
//! - [`repo`]: repositories, indexes, package locations.
//! - [`solver`]: dependency resolution.
//! - [`fetch`]: downloads into the cache, identity checks.
//! - [`unpack`]: packages into `ROOT/Programs/<name>/<version>`.
//! - [`db`]: the SQLite metadata database (`var/lib/vpm/state.db`), world,
//!   journal, and `Programs/` layout helpers.
//! - [`sqlite`]: direct FFI wrapper over system `libsqlite3`.
//! - [`commit`]: the switch of the live tree, scripts, triggers, recovery.
//! - [`engine`]: the `add`/`del`/`upgrade`/`list` transactions.
//! - [`progress`], [`rootfs`], [`sys`]: display, in-root path resolution,
//!   libc helpers.

pub mod adb;
pub mod commit;
pub mod db;
pub mod digest;
pub mod dump;
pub mod engine;
pub mod error;
pub mod fetch;
pub mod progress;
pub mod repo;
pub mod rootfs;
pub mod solver;
pub mod sqlite;
pub mod sys;
pub mod unpack;
pub mod verify;
pub mod version;

pub use error::{Error, Result};
