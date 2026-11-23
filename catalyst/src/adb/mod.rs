//! apk v3 container and schemas.
//!
//! - [`container`]: compression, file header, blocks ([`Container`]).
//! - [`value`]: the value encoding ([`Adb`], [`Val`], [`Obj`]).
//! - [`schema`]: typed readers ([`Index`], [`PkgInfo`], [`Package`] ...).
//! - [`package`]: streaming package contents ([`PackageReader`]).
//!
//! Entry points: [`read_index`] / [`read_index_file`] for `Packages.adb`,
//! [`PackageReader::open`] / [`open_package_file`] for `.apk` files, and
//! [`read_head`] for the lower-level ADB+SIG part of any file.

pub mod container;
pub mod package;
pub mod schema;
pub mod value;

use std::fs::File;
use std::io::Read;
use std::path::Path;

pub use container::{
    decompress_all, schema_name, BlockHeader, Compression, Container, BLOCK_ADB, BLOCK_DATA, BLOCK_SIG,
    SCHEMA_INDEX, SCHEMA_INSTALLED_DB, SCHEMA_PACKAGE,
};
pub use package::{DataReader, Entry, EntryKind, PackageReader};
pub use schema::{Acl, Dependency, Dir, FileEntry, FileKind, Index, Package, PkgInfo, Scripts};
pub use value::{Adb, Obj, Val};

use crate::error::{Error, Result};
use crate::verify::{self, TrustInfo, TrustedKeys, VerifyOptions};

/// The part of a file up to the first DATA block: the ADB block and the
/// SIG blocks. The container is positioned at the first DATA block, whose
/// header has been read (`data_pending`), or at the end of the file.
pub struct Head<R: Read> {
    pub container: Container<R>,
    pub adb: Adb,
    /// SIG block payloads in file order.
    pub sigs: Vec<Vec<u8>>,
    pub data_pending: Option<BlockHeader>,
}

/// Reads the file header, the ADB block and all SIG blocks. Does not
/// decide trust.
pub fn read_head<R: Read>(reader: R) -> Result<Head<R>> {
    read_head_expecting(reader, None)
}

/// [`read_head`], and when the package identity is known (32 or 64 bytes
/// from a signed index) it is compared right after the ADB block, before
/// any SIG block is read: a file that is not the expected package costs
/// no more than its ADB block. At most [`container::MAX_SIG_BLOCKS`] SIG
/// blocks are accepted.
pub fn read_head_expecting<R: Read>(reader: R, identity: Option<&[u8]>) -> Result<Head<R>> {
    let mut c = Container::new(reader)?;
    match c.next_block()? {
        Some(h) if h.kind == BLOCK_ADB => {}
        _ => return Err(Error::Format("no ADB block".into())),
    }
    let adb = Adb::new(c.read_payload_vec()?)?;
    if let Some(id) = identity {
        let got = match id.len() {
            32 => Some(crate::digest::sha256(adb.bytes()).to_vec()),
            64 => Some(crate::digest::sha512(adb.bytes()).to_vec()),
            _ => None,
        };
        if let Some(got) = got {
            if got != id {
                return Err(Error::Integrity(format!(
                    "package identity mismatch: index has {}, file has {}",
                    crate::digest::hex(id),
                    crate::digest::hex(&got)
                )));
            }
        }
    }
    let mut sigs = Vec::new();
    let mut data_pending = None;
    while let Some(h) = c.next_block()? {
        match h.kind {
            BLOCK_SIG => {
                if sigs.len() >= container::MAX_SIG_BLOCKS {
                    return Err(Error::Format(format!(
                        "more than {} SIG blocks",
                        container::MAX_SIG_BLOCKS
                    )));
                }
                sigs.push(c.read_payload_vec()?)
            }
            BLOCK_DATA => {
                data_pending = Some(h);
                break;
            }
            _ => return Err(Error::Format("unexpected block".into())),
        }
    }
    Ok(Head { container: c, adb, sigs, data_pending })
}

/// Reads and verifies a repository index (schema `indx`). An index is
/// trusted only by a signature (or `opts.allow_untrusted`), §C5.3.
pub fn read_index<R: Read>(reader: R, keys: &TrustedKeys, opts: &VerifyOptions) -> Result<(Index, TrustInfo)> {
    index_from_head(read_head(reader)?, keys, opts)
}

/// [`read_index`] from an already read [`Head`].
pub fn index_from_head<R: Read>(head: Head<R>, keys: &TrustedKeys, opts: &VerifyOptions) -> Result<(Index, TrustInfo)> {
    let schema = head.container.schema();
    if schema != SCHEMA_INDEX {
        return Err(Error::Schema(format!("not an index (schema '{}')", schema_name(schema))));
    }
    if head.data_pending.is_some() {
        return Err(Error::Format("DATA block in an index".into()));
    }
    crate::prof::mark("  repositories: index read, decompressed");
    let opts = VerifyOptions { expected_identity: None, ..opts.clone() };
    let trust = verify::check_trust(keys, schema, head.adb.bytes(), &head.sigs, &opts)?;
    crate::prof::mark("  repositories: signature checked");
    let index = Index::from_adb(&head.adb)?;
    crate::prof::mark("  repositories: index parsed");
    Ok((index, trust))
}

/// [`read_index`] on a file.
pub fn read_index_file(path: &Path, keys: &TrustedKeys, opts: &VerifyOptions) -> Result<(Index, TrustInfo)> {
    read_index(File::open(path)?, keys, opts)
}

/// [`PackageReader::open`] on a file.
pub fn open_package_file(path: &Path, keys: &TrustedKeys, opts: &VerifyOptions) -> Result<PackageReader<File>> {
    PackageReader::open(File::open(path)?, keys, opts)
}

/// The schema of a file, from its header (decompressing as needed).
pub fn file_schema(path: &Path) -> Result<u32> {
    Ok(Container::new(File::open(path)?)?.schema())
}
