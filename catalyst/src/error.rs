use std::fmt;
use std::io;

/// Errors from reading and verifying apk v3 files.
#[derive(Debug)]
pub enum Error {
    /// An I/O error from the underlying reader or the file system.
    Io(io::Error),
    /// The container is malformed: compression prefix or stream, file
    /// header, block framing, block order, truncation, trailing bytes.
    Format(String),
    /// The ADB values are malformed or do not fit the schema: bad offsets,
    /// wrong value types, unsafe or invalid paths, bad file entries,
    /// DATA blocks that do not match the file tree.
    Schema(String),
    /// No trust could be established: unsigned, no signature from a trusted
    /// key, or a bad signature.
    Untrusted(String),
    /// A digest did not match: the package identity from the index, or a
    /// file's content against its stored hash.
    Integrity(String),
    /// A key could not be loaded.
    Key(String),
    /// An error reported by OpenSSL.
    Crypto(openssl::error::ErrorStack),
    /// Anything else (resolution, downloads, scripts, the database),
    /// printed as is.
    Msg(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// A short name for the error class, for messages and tests.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::Io(_) => "io",
            Error::Format(_) => "format",
            Error::Schema(_) => "schema",
            Error::Untrusted(_) => "untrusted",
            Error::Integrity(_) => "integrity",
            Error::Key(_) => "key",
            Error::Crypto(_) => "crypto",
            Error::Msg(_) => "error",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Format(m) => write!(f, "malformed file: {m}"),
            Error::Schema(m) => write!(f, "invalid content: {m}"),
            Error::Untrusted(m) => write!(f, "untrusted: {m}"),
            Error::Integrity(m) => write!(f, "integrity check failed: {m}"),
            Error::Key(m) => write!(f, "key error: {m}"),
            Error::Crypto(e) => write!(f, "crypto error: {e}"),
            Error::Msg(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Crypto(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<openssl::error::ErrorStack> for Error {
    fn from(e: openssl::error::ErrorStack) -> Self {
        Error::Crypto(e)
    }
}
