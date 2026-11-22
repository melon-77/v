//! Streaming access to a package's file tree and contents (Part E).
//!
//! [`PackageReader::open`] reads the ADB and SIG blocks, establishes trust
//! (signature and/or identity, see [`crate::verify`]) **before** any DATA
//! block is read, and validates the file tree. [`PackageReader::next_entry`]
//! then walks the tree in stored order, like apk's extractor (§E8): one
//! entry per directory (except the root) and per file. A regular file with
//! content comes with a [`DataReader`] over its DATA block; the content is
//! hashed while it is read and checked against the file's hash when its
//! last byte has been read. Content a caller does not read is still read
//! and checked before the next entry is returned.

use std::io::{self, Read};

use super::container::{BlockHeader, Container, BLOCK_DATA, SCHEMA_PACKAGE};
use super::schema::{Dir, FileEntry, Package};
use super::value::Adb;
use crate::digest::{self, Alg, Hasher};
use crate::error::{Error, Result};
use crate::verify::{self, TrustInfo, TrustedKeys, VerifyOptions};

/// The kind of an entry returned by [`PackageReader::next_entry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
}

struct DataState {
    /// Content bytes not read yet.
    remaining: u64,
    hasher: Option<Hasher>,
    expected: Vec<u8>,
    alg: Alg,
    active: bool,
    /// Set when a content check failed; the reader is unusable afterwards.
    failed: Option<String>,
}

/// Reads a package file: metadata, then entries with verified contents.
pub struct PackageReader<R: Read> {
    c: Container<R>,
    adb: Adb,
    pkg: Package,
    trust: TrustInfo,
    pending: Option<BlockHeader>,
    // walk position: next directory, whether its entry was emitted, next file
    d: usize,
    dir_done: bool,
    f: usize,
    path: Vec<u8>,
    data: DataState,
    finished: bool,
    data_blocks: u64,
}

/// One directory or file of the package.
pub struct Entry<'a, R: Read> {
    pub kind: EntryKind,
    /// Full path relative to the package root (no leading `/`).
    pub path: &'a [u8],
    /// The directory, or the directory that holds the file.
    pub dir: &'a Dir,
    /// The file (for `EntryKind::File`).
    pub file: Option<&'a FileEntry>,
    /// 1-based indices as in DATA blocks: directory, and file (0 for a
    /// directory entry).
    pub path_idx: usize,
    pub file_idx: usize,
    data: Option<DataReader<'a, R>>,
}

impl<'a, R: Read> Entry<'a, R> {
    /// The content of a regular file with `size != 0`.
    pub fn data(&mut self) -> Option<&mut DataReader<'a, R>> {
        self.data.as_mut()
    }

    pub fn path_lossy(&self) -> String {
        String::from_utf8_lossy(self.path).into_owned()
    }
}

/// The content of one file, read from its DATA block. Implements
/// [`Read`]; returns an `InvalidData` error instead of the last chunk if
/// the content does not match the file's hash.
pub struct DataReader<'a, R: Read> {
    c: &'a mut Container<R>,
    st: &'a mut DataState,
}

impl<'a, R: Read> DataReader<'a, R> {
    /// Content bytes not read yet.
    pub fn remaining(&self) -> u64 {
        self.st.remaining
    }
}

fn read_data<R: Read>(c: &mut Container<R>, st: &mut DataState, buf: &mut [u8]) -> Result<usize> {
    if let Some(m) = &st.failed {
        return Err(Error::Integrity(m.clone()));
    }
    if st.remaining == 0 || buf.is_empty() {
        return Ok(0);
    }
    let want = (buf.len() as u64).min(st.remaining) as usize;
    let n = c.read_payload(&mut buf[..want])?;
    if n == 0 {
        return Err(Error::Format("DATA block ends early".into()));
    }
    st.remaining -= n as u64;
    if let Some(h) = st.hasher.as_mut() {
        h.update(&buf[..n]);
    }
    if st.remaining == 0 {
        let got = st.hasher.take().map(|h| h.finish()).unwrap_or_default();
        if got != st.expected {
            let m = format!(
                "content does not match its {} hash (expected {}, got {})",
                st.alg.name(),
                digest::hex(&st.expected),
                digest::hex(&got)
            );
            st.failed = Some(m.clone());
            return Err(Error::Integrity(m));
        }
    }
    Ok(n)
}

impl<'a, R: Read> Read for DataReader<'a, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        read_data(self.c, self.st, buf).map_err(|e| match e {
            Error::Io(e) => e,
            e => io::Error::new(io::ErrorKind::InvalidData, e.to_string()),
        })
    }
}

impl<R: Read> PackageReader<R> {
    /// Opens a package. Reads the ADB and SIG blocks, checks trust as
    /// [`verify::check_trust`] does with `opts`, and validates the metadata.
    /// Nothing of the package is returned unless it is trusted.
    pub fn open(reader: R, keys: &TrustedKeys, opts: &VerifyOptions) -> Result<Self> {
        Self::from_head(super::read_head_expecting(reader, opts.expected_identity.as_deref())?, keys, opts)
    }

    /// [`PackageReader::open`] from an already read [`super::Head`].
    pub fn from_head(head: super::Head<R>, keys: &TrustedKeys, opts: &VerifyOptions) -> Result<Self> {
        if head.container.schema() != SCHEMA_PACKAGE {
            return Err(Error::Schema(format!(
                "not a package (schema '{}')",
                super::schema_name(head.container.schema())
            )));
        }
        let trust = verify::check_trust(keys, head.container.schema(), head.adb.bytes(), &head.sigs, opts)?;
        let pkg = Package::from_adb(&head.adb)?;
        Ok(PackageReader {
            c: head.container,
            adb: head.adb,
            pkg,
            trust,
            pending: head.data_pending,
            d: 0,
            dir_done: false,
            f: 0,
            path: Vec::new(),
            data: DataState {
                remaining: 0,
                hasher: None,
                expected: Vec::new(),
                alg: Alg::Sha256,
                active: false,
                failed: None,
            },
            finished: false,
            data_blocks: 0,
        })
    }

    pub fn package(&self) -> &Package {
        &self.pkg
    }

    /// The ADB block payload (its SHA-256 is the package identity).
    pub fn adb(&self) -> &Adb {
        &self.adb
    }

    pub fn trust(&self) -> &TrustInfo {
        &self.trust
    }

    pub fn compression(&self) -> super::Compression {
        self.c.compression()
    }

    /// DATA blocks read so far.
    pub fn data_blocks(&self) -> u64 {
        self.data_blocks
    }

    /// Reads and checks whatever the caller left of the current content.
    fn finish_current(&mut self) -> Result<()> {
        if let Some(m) = &self.data.failed {
            return Err(Error::Integrity(m.clone()));
        }
        if !self.data.active {
            return Ok(());
        }
        let mut buf = vec![0u8; 64 * 1024];
        while self.data.remaining > 0 {
            read_data(&mut self.c, &mut self.data, &mut buf)?;
        }
        if self.c.payload_remaining() != 0 {
            return Err(Error::Format("DATA block longer than its file".into()));
        }
        self.c.end_block()?;
        self.data.active = false;
        Ok(())
    }

    /// The next directory or file in stored order, or `None` after the last
    /// one. At the end, checks that no DATA block is left over and that the
    /// file ends cleanly.
    pub fn next_entry(&mut self) -> Result<Option<Entry<'_, R>>> {
        self.finish_current()?;
        if self.finished {
            return Ok(None);
        }
        loop {
            if self.d >= self.pkg.dirs.len() {
                let extra = match self.pending.take() {
                    Some(h) => Some(h),
                    None => self.c.next_block()?,
                };
                if let Some(h) = extra {
                    return Err(Error::Schema(format!(
                        "DATA block at offset {} does not belong to any file",
                        h.offset
                    )));
                }
                self.finished = true;
                return Ok(None);
            }
            let dir = &self.pkg.dirs[self.d];
            if !self.dir_done {
                self.dir_done = true;
                if !dir.name.is_empty() {
                    self.path.clear();
                    self.path.extend_from_slice(&dir.name);
                    return Ok(Some(Entry {
                        kind: EntryKind::Dir,
                        path: &self.path,
                        dir,
                        file: None,
                        path_idx: dir.index,
                        file_idx: 0,
                        data: None,
                    }));
                }
            }
            if self.f >= dir.files.len() {
                self.d += 1;
                self.f = 0;
                self.dir_done = false;
                continue;
            }
            let file = &dir.files[self.f];
            self.f += 1;
            self.path.clear();
            if !dir.name.is_empty() {
                self.path.extend_from_slice(&dir.name);
                self.path.push(b'/');
            }
            self.path.extend_from_slice(&file.name);
            // the slots in the paths and files arrays, NULL slots counted
            let (path_idx, file_idx) = (dir.index, file.index);
            if !file.has_data() {
                return Ok(Some(Entry {
                    kind: EntryKind::File,
                    path: &self.path,
                    dir,
                    file: Some(file),
                    path_idx,
                    file_idx,
                    data: None,
                }));
            }
            let what = || String::from_utf8_lossy(&dir.file_path(file)).into_owned();
            let h = match self.pending.take() {
                Some(h) => h,
                None => self
                    .c
                    .next_block()?
                    .ok_or_else(|| Error::Schema(format!("no DATA block for '{}'", what())))?,
            };
            if h.kind != BLOCK_DATA {
                return Err(Error::Format("expected a DATA block".into()));
            }
            if h.payload_len < 8 {
                return Err(Error::Format("DATA block shorter than its header".into()));
            }
            let mut idx = [0u8; 8];
            self.c.read_payload_exact(&mut idx)?;
            let pi = u32::from_le_bytes(idx[0..4].try_into().unwrap()) as usize;
            let fi = u32::from_le_bytes(idx[4..8].try_into().unwrap()) as usize;
            if pi != path_idx || fi != file_idx || h.payload_len - 8 != file.size {
                return Err(Error::Schema(format!(
                    "DATA block ({pi}, {fi}, {} bytes) does not match the next file '{}' ({path_idx}, {file_idx}, {} bytes)",
                    h.payload_len - 8,
                    what(),
                    file.size
                )));
            }
            // validated in Package::from_adb
            let alg = file.hash_alg().ok_or_else(|| Error::Schema("file without hash".into()))?;
            self.data.remaining = file.size;
            self.data.hasher = Some(Hasher::new(alg));
            self.data.expected = file.hash.clone();
            self.data.alg = alg;
            self.data.active = true;
            self.data_blocks += 1;
            return Ok(Some(Entry {
                kind: EntryKind::File,
                path: &self.path,
                dir,
                file: Some(file),
                path_idx,
                file_idx,
                data: Some(DataReader { c: &mut self.c, st: &mut self.data }),
            }));
        }
    }

    /// Reads and checks everything not read yet, then the end of the file.
    /// Returns the trust information.
    pub fn finish(mut self) -> Result<TrustInfo> {
        while self.next_entry()?.is_some() {}
        Ok(self.trust)
    }
}
