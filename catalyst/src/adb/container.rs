//! The outer layers of an apk v3 file: the compression prefix (§B3), the
//! 8-byte file header (§B4) and the block stream (§B5).
//!
//! [`Container`] reads a file as a stream, block by block. It never holds
//! more than one block payload in memory, so large packages can be unpacked
//! with constant memory. All sizes are checked: a block that claims more
//! bytes than the stream holds, a block header smaller than itself, missing
//! padding, trailing bytes and truncated compressed streams are errors.

use std::io::{self, BufRead, BufReader, Read};

use flate2::{Decompress, FlushDecompress, Status};

use crate::error::{Error, Result};

/// `"ADB."` read as a little-endian u32.
pub const ADB_FORMAT_MAGIC: u32 = 0x2e42_4441;
pub const SCHEMA_INDEX: u32 = 0x7864_6e69; // "indx"
pub const SCHEMA_PACKAGE: u32 = 0x676b_6370; // "pckg"
pub const SCHEMA_INSTALLED_DB: u32 = 0x0062_6469; // "idb\0"

pub const BLOCK_ADB: u32 = 0;
pub const BLOCK_SIG: u32 = 1;
pub const BLOCK_DATA: u32 = 2;

/// apk's stream path accepts ADB payloads of 16..=0x0fffffff bytes (§B6).
pub const MIN_ADB_PAYLOAD: u64 = 16;
pub const MAX_ADB_PAYLOAD: u64 = 0x0fff_ffff;
/// catalyst's own, lower limits, by schema: the ADB block is held in memory and
/// read before trust is decided, so its size is what an untrusted file can
/// make catalyst allocate. Valence's largest package metadata is 1.3 MB
/// (boost-headers), its index 0.3 MB for 1102 packages.
pub const MAX_PACKAGE_ADB: u64 = 16 << 20;
pub const MAX_INDEX_ADB: u64 = 64 << 20;
/// apk peeks a SIG payload into its 128 KiB stream buffer (§C1.4); larger
/// SIG blocks cannot be read by apk either.
pub const MAX_SIG_PAYLOAD: u64 = 128 * 1024 - 8;
/// SIG blocks per file: apk's signer writes one per key (`adbsign` may add
/// more); every one is held in memory until trust is decided.
pub const MAX_SIG_BLOCKS: usize = 8;
/// zstd window limit (log2), the decoder's default and what apk accepts:
/// frames needing more are refused instead of allocating more.
pub const ZSTD_WINDOW_LOG_MAX: u32 = 27;

/// The ADB payload limit for a file of schema `schema`.
pub fn adb_limit(schema: u32) -> u64 {
    match schema {
        SCHEMA_PACKAGE => MAX_PACKAGE_ADB,
        SCHEMA_INDEX => MAX_INDEX_ADB,
        _ => MAX_ADB_PAYLOAD,
    }
}

const IO_BUF: usize = 128 * 1024;

/// The compression found in front of the ADB stream (§B3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// `"ADB."`: uncompressed.
    None,
    /// `"ADBd"` (level `None`) or `"ADBc"` with alg 1 (level given).
    Deflate { level: Option<u8> },
    /// `"ADBc"` with alg 2.
    Zstd { level: u8 },
}

impl std::fmt::Display for Compression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Compression::None => write!(f, "none"),
            Compression::Deflate { level: None } => write!(f, "deflate"),
            Compression::Deflate { level: Some(l) } => write!(f, "deflate:{l}"),
            Compression::Zstd { level } => write!(f, "zstd:{level}"),
        }
    }
}

/// Returns the 4 schema bytes as text when printable ("indx", "pckg").
pub fn schema_name(schema: u32) -> String {
    let b = schema.to_le_bytes();
    let s: String = b
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { '?' })
        .collect();
    s
}

/// Raw RFC 1951 inflate that knows whether the stream was complete.
///
/// flate2's own readers report a truncated stream as a clean end of file;
/// apk's C reader does the same (§B3.2). We require the final deflate block
/// and reject bytes after it.
struct Inflater<R: BufRead> {
    inner: R,
    z: Decompress,
    done: bool,
}

impl<R: BufRead> Inflater<R> {
    fn new(inner: R) -> Self {
        Inflater { inner, z: Decompress::new(false), done: false }
    }
}

impl<R: BufRead> Read for Inflater<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        let mut stalls = 0;
        loop {
            let input = self.inner.fill_buf()?;
            let eof = input.is_empty();
            let (in0, out0) = (self.z.total_in(), self.z.total_out());
            let status = self
                .z
                .decompress(input, buf, FlushDecompress::None)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("deflate: {e}")))?;
            let consumed = (self.z.total_in() - in0) as usize;
            let produced = (self.z.total_out() - out0) as usize;
            self.inner.consume(consumed);
            match status {
                Status::StreamEnd => {
                    self.done = true;
                    return Ok(produced);
                }
                Status::Ok | Status::BufError => {
                    if produced > 0 {
                        return Ok(produced);
                    }
                    if eof {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "deflate stream is truncated",
                        ));
                    }
                    if consumed == 0 {
                        stalls += 1;
                        if stalls > 2 {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "deflate stream makes no progress",
                            ));
                        }
                    }
                }
            }
        }
    }
}

enum Decoder<R: Read> {
    Plain(BufReader<R>),
    Deflate(Inflater<BufReader<R>>),
    Zstd(zstd::stream::read::Decoder<'static, BufReader<R>>),
}

impl<R: Read> Read for Decoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Decoder::Plain(r) => r.read(buf),
            Decoder::Deflate(r) => r.read(buf),
            Decoder::Zstd(r) => r.read(buf),
        }
    }
}

impl<R: Read> Decoder<R> {
    /// Called when the decompressed stream reported its end: the
    /// compressed layer must be complete with nothing after it.
    fn check_end(&mut self) -> Result<()> {
        match self {
            Decoder::Plain(_) => Ok(()),
            Decoder::Deflate(z) => {
                if !z.done {
                    return Err(Error::Format("deflate stream is truncated".into()));
                }
                let rest = z.inner.fill_buf().map_err(Error::Io)?;
                if !rest.is_empty() {
                    return Err(Error::Format("trailing bytes after the deflate stream".into()));
                }
                Ok(())
            }
            // The zstd decoder reports an incomplete frame as an error and
            // treats anything after a frame as the start of another frame.
            Decoder::Zstd(_) => Ok(()),
        }
    }
}

/// Maps an I/O error from the decompressed stream. Decoder errors come as
/// `InvalidData`, `InvalidInput`, `UnexpectedEof` or `Other` (zstd).
fn stream_err(e: io::Error, what: &str) -> Error {
    match e.kind() {
        io::ErrorKind::InvalidData
        | io::ErrorKind::InvalidInput
        | io::ErrorKind::UnexpectedEof
        | io::ErrorKind::Other => Error::Format(format!("{what}: {e}")),
        _ => Error::Io(e),
    }
}

/// Reads until `buf` is full or the stream ends; returns the count.
pub(crate) fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// A block header (§B5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockHeader {
    /// `BLOCK_ADB`, `BLOCK_SIG` or `BLOCK_DATA`.
    pub kind: u32,
    /// Payload length: rawsize minus the header size.
    pub payload_len: u64,
    /// The 16-byte extended header form was used.
    pub ext: bool,
    /// Offset of the block header in the decompressed stream.
    pub offset: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    ExpectAdb,
    AfterAdb,
    InData,
    End,
}

/// Streaming reader of the block layer.
///
/// Enforces the grammar `ADB SIG* DATA*` (§B5.2), the size limits of the
/// ADB and SIG payloads, the presence of padding, and that nothing follows
/// the last block. Trust is not decided here (see [`crate::verify`]).
pub struct Container<R: Read> {
    dec: Decoder<R>,
    compression: Compression,
    schema: u32,
    state: State,
    /// Decompressed offset of the next unread byte.
    offset: u64,
    in_block: bool,
    remaining: u64,
    padding: u8,
}

impl<R: Read> Container<R> {
    /// Detects the compression and reads the file header.
    pub fn new(reader: R) -> Result<Self> {
        let mut br = BufReader::with_capacity(IO_BUF, reader);
        let mut pfx = [0u8; 4];
        let n = read_full(&mut br, &mut pfx).map_err(Error::Io)?;
        if n < 4 || &pfx[..3] != b"ADB" {
            return Err(Error::Format("not an apk v3 file (no ADB prefix)".into()));
        }
        let (mut dec, compression, header_in_prefix) = match pfx[3] {
            b'.' => (Decoder::Plain(br), Compression::None, true),
            b'd' => (Decoder::Deflate(Inflater::new(br)), Compression::Deflate { level: None }, false),
            b'c' => {
                let mut spec = [0u8; 2];
                if read_full(&mut br, &mut spec).map_err(Error::Io)? != 2 {
                    return Err(Error::Format("truncated compression spec".into()));
                }
                match spec[0] {
                    1 => (
                        Decoder::Deflate(Inflater::new(br)),
                        Compression::Deflate { level: Some(spec[1]) },
                        false,
                    ),
                    2 => {
                        let mut z = zstd::stream::read::Decoder::with_buffer(br).map_err(Error::Io)?;
                        z.window_log_max(ZSTD_WINDOW_LOG_MAX).map_err(Error::Io)?;
                        (Decoder::Zstd(z), Compression::Zstd { level: spec[1] }, false)
                    }
                    a => {
                        // alg 0 inside "ADBc" is never written and apk handles
                        // it inconsistently (§B3.5); reject it like alg >= 3.
                        return Err(Error::Format(format!("unsupported compression algorithm {a}")));
                    }
                }
            }
            c => return Err(Error::Format(format!("unknown compression prefix 'ADB{}'", c.escape_ascii()))),
        };
        let mut hdr = [0u8; 8];
        let schema = if header_in_prefix {
            // "ADB." is both the prefix and the file magic.
            let n = read_full(&mut dec, &mut hdr[4..]).map_err(|e| stream_err(e, "file header"))?;
            if n != 4 {
                return Err(Error::Format("truncated file header".into()));
            }
            u32::from_le_bytes(hdr[4..8].try_into().unwrap())
        } else {
            let n = read_full(&mut dec, &mut hdr).map_err(|e| stream_err(e, "file header"))?;
            if n != 8 {
                return Err(Error::Format("truncated file header".into()));
            }
            if u32::from_le_bytes(hdr[0..4].try_into().unwrap()) != ADB_FORMAT_MAGIC {
                return Err(Error::Format("bad ADB file magic".into()));
            }
            u32::from_le_bytes(hdr[4..8].try_into().unwrap())
        };
        Ok(Container {
            dec,
            compression,
            schema,
            state: State::ExpectAdb,
            offset: 8,
            in_block: false,
            remaining: 0,
            padding: 0,
        })
    }

    /// The schema from the file header, as the little-endian u32 (bytes
    /// 4..8 of the decompressed stream). It is part of the signed message.
    pub fn schema(&self) -> u32 {
        self.schema
    }

    pub fn compression(&self) -> Compression {
        self.compression
    }

    /// Payload bytes of the current block not read yet.
    pub fn payload_remaining(&self) -> u64 {
        if self.in_block {
            self.remaining
        } else {
            0
        }
    }

    fn skip(&mut self, mut n: u64, what: &str) -> Result<()> {
        let mut buf = [0u8; 16 * 1024];
        while n > 0 {
            let want = n.min(buf.len() as u64) as usize;
            let got = read_full(&mut self.dec, &mut buf[..want]).map_err(|e| stream_err(e, what))?;
            if got == 0 {
                return Err(Error::Format(format!("{what}: unexpected end of file")));
            }
            n -= got as u64;
            self.offset += got as u64;
        }
        Ok(())
    }

    /// Skips the unread part of the current payload and its padding.
    pub fn end_block(&mut self) -> Result<()> {
        if !self.in_block {
            return Ok(());
        }
        let rem = self.remaining;
        self.skip(rem, "truncated block payload")?;
        self.remaining = 0;
        let pad = self.padding as u64;
        self.skip(pad, "missing block padding")?;
        self.in_block = false;
        Ok(())
    }

    /// Reads the next block header. Any unread payload of the previous
    /// block is skipped first. Returns `None` at a clean end of the file,
    /// after checking that the compressed stream is complete.
    pub fn next_block(&mut self) -> Result<Option<BlockHeader>> {
        self.end_block()?;
        if self.state == State::End {
            return Ok(None);
        }
        let offset = self.offset;
        let mut h = [0u8; 4];
        let n = read_full(&mut self.dec, &mut h).map_err(|e| stream_err(e, "block header"))?;
        if n == 0 {
            if self.state == State::ExpectAdb {
                return Err(Error::Format("no ADB block".into()));
            }
            self.dec.check_end()?;
            self.state = State::End;
            return Ok(None);
        }
        if n < 4 {
            return Err(Error::Format(format!("truncated block header at offset {offset}")));
        }
        let ts = u32::from_le_bytes(h);
        let (kind, raw, hdr_len, ext) = if ts >> 30 == 3 {
            let mut x = [0u8; 12];
            let n = read_full(&mut self.dec, &mut x).map_err(|e| stream_err(e, "block header"))?;
            if n != 12 {
                return Err(Error::Format(format!("truncated extended block header at offset {offset}")));
            }
            (ts & 0x3fff_ffff, u64::from_le_bytes(x[4..12].try_into().unwrap()), 16u64, true)
        } else {
            (ts >> 30, (ts & 0x3fff_ffff) as u64, 4u64, false)
        };
        self.offset += hdr_len;
        if raw < hdr_len {
            return Err(Error::Format(format!("block at offset {offset} is smaller than its header")));
        }
        let padded = raw
            .checked_add(7)
            .ok_or_else(|| Error::Format(format!("block size overflow at offset {offset}")))?
            & !7u64;
        let payload_len = raw - hdr_len;
        let next_state = match (kind, self.state) {
            (BLOCK_ADB, State::ExpectAdb) => {
                if !(MIN_ADB_PAYLOAD..=MAX_ADB_PAYLOAD).contains(&payload_len) {
                    return Err(Error::Format(format!("ADB block payload size {payload_len} out of range")));
                }
                let limit = adb_limit(self.schema);
                if payload_len > limit {
                    return Err(Error::Format(format!(
                        "ADB block of {payload_len} bytes is larger than catalyst accepts for schema '{}' ({limit})",
                        schema_name(self.schema)
                    )));
                }
                State::AfterAdb
            }
            (BLOCK_SIG, State::AfterAdb) => {
                if payload_len > MAX_SIG_PAYLOAD {
                    return Err(Error::Format(format!("SIG block too large ({payload_len} bytes)")));
                }
                State::AfterAdb
            }
            (BLOCK_DATA, State::AfterAdb) | (BLOCK_DATA, State::InData) => State::InData,
            (k, s) => {
                let what = match k {
                    BLOCK_ADB => "ADB".to_string(),
                    BLOCK_SIG => "SIG".to_string(),
                    BLOCK_DATA => "DATA".to_string(),
                    k => format!("type {k}"),
                };
                let at = match s {
                    State::ExpectAdb => "before the ADB block",
                    State::AfterAdb => "after the ADB block",
                    _ => "after a DATA block",
                };
                return Err(Error::Format(format!("unexpected {what} block {at} (offset {offset})")));
            }
        };
        self.state = next_state;
        self.in_block = true;
        self.remaining = payload_len;
        self.padding = (padded - raw) as u8;
        Ok(Some(BlockHeader { kind, payload_len, ext, offset }))
    }

    /// Reads up to `buf.len()` bytes of the current payload. Returns 0 at
    /// the end of the payload. A payload cut short by the end of the file
    /// is an error.
    pub fn read_payload(&mut self, buf: &mut [u8]) -> Result<usize> {
        if !self.in_block || self.remaining == 0 || buf.is_empty() {
            return Ok(0);
        }
        let want = (buf.len() as u64).min(self.remaining) as usize;
        let got = loop {
            match self.dec.read(&mut buf[..want]) {
                Ok(n) => break n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(stream_err(e, "block payload")),
            }
        };
        if got == 0 {
            return Err(Error::Format("block payload: unexpected end of file".into()));
        }
        self.remaining -= got as u64;
        self.offset += got as u64;
        Ok(got)
    }

    /// Reads exactly `buf.len()` payload bytes.
    pub fn read_payload_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        if (buf.len() as u64) > self.payload_remaining() {
            return Err(Error::Format("block payload shorter than expected".into()));
        }
        let mut n = 0;
        while n < buf.len() {
            n += self.read_payload(&mut buf[n..])?;
        }
        Ok(())
    }

    /// Reads the rest of the current payload into memory. Only ADB and SIG
    /// payloads, whose sizes are bounded by [`Container::next_block`],
    /// should be read this way.
    pub fn read_payload_vec(&mut self) -> Result<Vec<u8>> {
        let len = self.payload_remaining();
        if len > MAX_ADB_PAYLOAD {
            return Err(Error::Format("payload too large to load into memory".into()));
        }
        // grow as data arrives, so a lying size field costs no memory
        let mut v = Vec::with_capacity(len.min(1 << 20) as usize);
        let mut buf = vec![0u8; 64 * 1024];
        while (v.len() as u64) < len {
            let want = ((len - v.len() as u64) as usize).min(buf.len());
            let n = self.read_payload(&mut buf[..want])?;
            v.extend_from_slice(&buf[..n]);
        }
        Ok(v)
    }
}

/// Decompresses a whole file into memory (for tools and tests). The
/// result starts with the 8-byte file header ("ADB." + schema).
pub fn decompress_all(mut reader: impl Read) -> Result<(Compression, Vec<u8>)> {
    let mut raw = Vec::new();
    reader.read_to_end(&mut raw)?;
    if raw.len() < 4 || &raw[..3] != b"ADB" {
        return Err(Error::Format("not an apk v3 file (no ADB prefix)".into()));
    }
    if raw[3] == b'.' {
        return Ok((Compression::None, raw));
    }
    let mut out = Vec::new();
    let comp = match raw[3] {
        b'd' => {
            let mut z = Inflater::new(BufReader::new(&raw[4..]));
            z.read_to_end(&mut out).map_err(|e| stream_err(e, "decompress"))?;
            if !z.inner.fill_buf()?.is_empty() {
                return Err(Error::Format("trailing bytes after the deflate stream".into()));
            }
            Compression::Deflate { level: None }
        }
        b'c' if raw.len() >= 6 => {
            let br = BufReader::new(&raw[6..]);
            match raw[4] {
                1 => {
                    let mut z = Inflater::new(br);
                    z.read_to_end(&mut out).map_err(|e| stream_err(e, "decompress"))?;
                    if !z.inner.fill_buf()?.is_empty() {
                        return Err(Error::Format("trailing bytes after the deflate stream".into()));
                    }
                    Compression::Deflate { level: Some(raw[5]) }
                }
                2 => {
                    let mut z = zstd::stream::read::Decoder::with_buffer(br)?;
                    z.read_to_end(&mut out).map_err(|e| stream_err(e, "decompress"))?;
                    Compression::Zstd { level: raw[5] }
                }
                a => return Err(Error::Format(format!("unsupported compression algorithm {a}"))),
            }
        }
        _ => return Err(Error::Format("unknown compression prefix".into())),
    };
    Ok((comp, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    /// The worked example of §B7.10: an index whose root has only
    /// description = "x".
    pub(crate) fn example_index() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"ADB.indx");
        v.extend_from_slice(&[0x18, 0, 0, 0]);
        v.extend_from_slice(&[0, 0, 0, 0, 0x0c, 0, 0, 0xe0, 1, b'x', 0, 0, 2, 0, 0, 0, 8, 0, 0, 0x80]);
        v
    }

    fn deflate(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    fn blocks(data: &[u8]) -> Result<(Compression, u32, Vec<BlockHeader>)> {
        let mut c = Container::new(Cursor::new(data))?;
        let mut v = Vec::new();
        while let Some(h) = c.next_block()? {
            v.push(h);
        }
        Ok((c.compression(), c.schema(), v))
    }

    #[test]
    fn plain_example() {
        let (comp, schema, b) = blocks(&example_index()).unwrap();
        assert_eq!(comp, Compression::None);
        assert_eq!(schema, SCHEMA_INDEX);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].kind, BLOCK_ADB);
        assert_eq!(b[0].payload_len, 20);
    }

    #[test]
    fn deflate_and_zstd() {
        let plain = example_index();
        let mut d = b"ADBd".to_vec();
        d.extend(deflate(&plain));
        let (comp, _, b) = blocks(&d).unwrap();
        assert_eq!(comp, Compression::Deflate { level: None });
        assert_eq!(b.len(), 1);

        let mut c = b"ADBc\x01\x05".to_vec();
        c.extend(deflate(&plain));
        assert_eq!(blocks(&c).unwrap().0, Compression::Deflate { level: Some(5) });

        let mut z = b"ADBc\x02\x03".to_vec();
        z.extend(zstd::stream::encode_all(Cursor::new(&plain), 3).unwrap());
        assert_eq!(blocks(&z).unwrap().0, Compression::Zstd { level: 3 });
        assert_eq!(decompress_all(Cursor::new(&z)).unwrap().1, plain);
        assert_eq!(decompress_all(Cursor::new(&d)).unwrap().1, plain);

        // truncated deflate stream
        let t = &d[..d.len() - 2];
        assert_eq!(blocks(t).unwrap_err().kind(), "format");
        // trailing bytes after the deflate stream
        let mut tr = d.clone();
        tr.push(0);
        assert_eq!(blocks(&tr).unwrap_err().kind(), "format");
        // truncated zstd frame
        assert!(blocks(&z[..z.len() - 3]).is_err());
        // unknown algorithm
        assert!(blocks(b"ADBc\x03\x00abcd").is_err());
        assert!(blocks(b"ADBc\x00\x00ADB.indx").is_err());
        assert!(blocks(b"ADBx").is_err());
        assert!(blocks(b"AD").is_err());
    }

    #[test]
    fn framing_errors() {
        let good = example_index();
        // trailing zero bytes: decoded as an ADB block after the ADB block
        let mut t = good.clone();
        t.extend_from_slice(&[0; 8]);
        assert!(blocks(&t).is_err());
        // 1..3 stray bytes
        let mut t = good.clone();
        t.extend_from_slice(&[1, 2]);
        assert!(blocks(&t).is_err());
        // missing padding: a SIG block of 18 bytes (raw 22, padded 24)
        let mut t = good.clone();
        t.extend_from_slice(&(0x4000_0000u32 | 22).to_le_bytes());
        t.extend_from_slice(&[0u8; 18]);
        assert!(blocks(&t).is_err());
        t.extend_from_slice(&[0, 0]);
        assert_eq!(blocks(&t).unwrap().2.len(), 2);
        // DATA then SIG is out of order
        let mut t = good.clone();
        t.extend_from_slice(&(0x8000_0000u32 | 12).to_le_bytes());
        t.extend_from_slice(&[1, 0, 0, 0, 1, 0, 0, 0]);
        t.extend_from_slice(&[0, 0, 0, 0]);
        t.extend_from_slice(&(0x4000_0000u32 | 4).to_le_bytes());
        t.extend_from_slice(&[0, 0, 0, 0]);
        assert!(blocks(&t).is_err());
        // rawsize < header size
        let mut t = good.clone();
        t.extend_from_slice(&(0x4000_0000u32 | 2).to_le_bytes());
        t.extend_from_slice(&[0, 0, 0, 0]);
        assert!(blocks(&t).is_err());
        // EXT header with a size that overflows when padded
        let mut t = good.clone();
        t.extend_from_slice(&(0xc000_0002u32).to_le_bytes());
        t.extend_from_slice(&[0; 4]);
        t.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(blocks(&t).is_err());
        // EXT header with inner type 3
        let mut t = good.clone();
        t.extend_from_slice(&(0xc000_0003u32).to_le_bytes());
        t.extend_from_slice(&[0; 4]);
        t.extend_from_slice(&16u64.to_le_bytes());
        assert!(blocks(&t).is_err());
        // EXT header, legal (SIG of 0 payload)
        let mut t = good.clone();
        t.extend_from_slice(&(0xc000_0001u32).to_le_bytes());
        t.extend_from_slice(&[0; 4]);
        t.extend_from_slice(&16u64.to_le_bytes());
        let (_, _, b) = blocks(&t).unwrap();
        assert!(b[1].ext && b[1].kind == BLOCK_SIG && b[1].payload_len == 0);
        // first block not ADB
        let mut t = b"ADB.indx".to_vec();
        t.extend_from_slice(&(0x4000_0000u32 | 4).to_le_bytes());
        t.extend_from_slice(&[0; 4]);
        assert!(blocks(&t).is_err());
        // no blocks at all
        assert!(blocks(b"ADB.indx").is_err());
        // ADB payload below 16 bytes
        let mut t = b"ADB.indx".to_vec();
        t.extend_from_slice(&12u32.to_le_bytes());
        t.extend_from_slice(&[0; 8 + 4]);
        assert!(blocks(&t).is_err());
        // payload claims more than the file holds
        let mut t = good.clone();
        t.extend_from_slice(&(0x8000_0000u32 | 1000).to_le_bytes());
        t.extend_from_slice(&[0; 20]);
        let mut c = Container::new(Cursor::new(&t)).unwrap();
        c.next_block().unwrap();
        c.next_block().unwrap();
        assert!(c.next_block().is_err());
    }
}
