//! Minimal safe Rust wrapper over Valence's system `libsqlite3`.
//!
//! No external crates: binds directly to the C API via `libc`, linked by
//! `build.rs` through `pkg-config sqlite3`.

use std::ffi::{CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr;

use libc::{c_char, c_int, c_void};

use crate::error::{Error, Result};

#[repr(C)]
struct Sqlite3 {
    _private: [u8; 0],
}

#[repr(C)]
struct Sqlite3Stmt {
    _private: [u8; 0],
}

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_NULL: c_int = 5;

const SQLITE_OPEN_READONLY: c_int = 0x0000_0001;
const SQLITE_OPEN_READWRITE: c_int = 0x0000_0002;
const SQLITE_OPEN_CREATE: c_int = 0x0000_0004;
const SQLITE_OPEN_NOFOLLOW: c_int = 0x0100_0000;

type Destructor = Option<unsafe extern "C" fn(*mut c_void)>;

#[inline]
fn sqlite_transient() -> Destructor {
    // SQLITE_TRANSIENT is ((sqlite3_destructor_type)-1) in sqlite3.h.
    unsafe { std::mem::transmute::<isize, Destructor>(-1) }
}

extern "C" {
    fn sqlite3_open_v2(
        filename: *const c_char,
        pp_db: *mut *mut Sqlite3,
        flags: c_int,
        z_vfs: *const c_char,
    ) -> c_int;
    fn sqlite3_close_v2(db: *mut Sqlite3) -> c_int;
    fn sqlite3_errmsg(db: *mut Sqlite3) -> *const c_char;
    fn sqlite3_busy_timeout(db: *mut Sqlite3, ms: c_int) -> c_int;
    fn sqlite3_exec(
        db: *mut Sqlite3,
        sql: *const c_char,
        callback: Option<unsafe extern "C" fn(*mut c_void, c_int, *mut *mut c_char, *mut *mut c_char) -> c_int>,
        arg: *mut c_void,
        errmsg: *mut *mut c_char,
    ) -> c_int;
    fn sqlite3_free(ptr: *mut c_void);
    fn sqlite3_prepare_v2(
        db: *mut Sqlite3,
        z_sql: *const c_char,
        n_byte: c_int,
        pp_stmt: *mut *mut Sqlite3Stmt,
        pz_tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_step(stmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_reset(stmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_clear_bindings(stmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_finalize(stmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_last_insert_rowid(db: *mut Sqlite3) -> i64;

    fn sqlite3_bind_null(stmt: *mut Sqlite3Stmt, idx: c_int) -> c_int;
    fn sqlite3_bind_int64(stmt: *mut Sqlite3Stmt, idx: c_int, val: i64) -> c_int;
    fn sqlite3_bind_text(
        stmt: *mut Sqlite3Stmt,
        idx: c_int,
        val: *const c_char,
        n_bytes: c_int,
        destructor: Destructor,
    ) -> c_int;
    fn sqlite3_bind_blob(
        stmt: *mut Sqlite3Stmt,
        idx: c_int,
        val: *const c_void,
        n_bytes: c_int,
        destructor: Destructor,
    ) -> c_int;

    fn sqlite3_column_type(stmt: *mut Sqlite3Stmt, col: c_int) -> c_int;
    fn sqlite3_column_int64(stmt: *mut Sqlite3Stmt, col: c_int) -> i64;
    fn sqlite3_column_text(stmt: *mut Sqlite3Stmt, col: c_int) -> *const u8;
    fn sqlite3_column_blob(stmt: *mut Sqlite3Stmt, col: c_int) -> *const c_void;
    fn sqlite3_column_bytes(stmt: *mut Sqlite3Stmt, col: c_int) -> c_int;
}

/// An open SQLite database connection.
pub struct Connection {
    raw: *mut Sqlite3,
}

// Connection is only accessed by one thread at a time (or behind &mut).
unsafe impl Send for Connection {}

impl Connection {
    /// Opens (or creates) a database for reading and writing.
    /// Uses `SQLITE_OPEN_NOFOLLOW` so symlinks are never followed.
    pub fn open_rw(path: &Path) -> Result<Self> {
        let c = Self::open_with_flags(
            path,
            SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_NOFOLLOW,
        )?;
        // DELETE journal + FULL sync: leaves no -wal/-shm sidecar files so
        // read-only mounts (live squashfs ISOs) can always open state.db
        // cleanly, while keeping full crash durability inside transactions.
        c.exec_batch(
            "PRAGMA foreign_keys = ON;\
             PRAGMA journal_mode = DELETE;\
             PRAGMA synchronous = FULL;",
        )?;
        Ok(c)
    }

    /// Opens an existing database read-only (works on read-only mounts).
    pub fn open_ro(path: &Path) -> Result<Self> {
        let c = Self::open_with_flags(path, SQLITE_OPEN_READONLY | SQLITE_OPEN_NOFOLLOW)?;
        c.exec_batch("PRAGMA query_only = ON;")?;
        Ok(c)
    }

    fn open_with_flags(path: &Path, flags: c_int) -> Result<Self> {
        let cpath = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| Error::Msg(format!("sqlite path contains NUL: {}", path.display())))?;
        let mut raw: *mut Sqlite3 = ptr::null_mut();
        let rc = unsafe { sqlite3_open_v2(cpath.as_ptr(), &mut raw, flags, ptr::null()) };
        if rc != SQLITE_OK {
            let msg = if !raw.is_null() {
                let s = unsafe { CStr::from_ptr(sqlite3_errmsg(raw)).to_string_lossy().into_owned() };
                unsafe { sqlite3_close_v2(raw) };
                s
            } else {
                format!("sqlite3_open_v2 error {rc}")
            };
            return Err(Error::Msg(format!("{}: {msg}", path.display())));
        }
        unsafe { sqlite3_busy_timeout(raw, 5000) };
        Ok(Connection { raw })
    }

    fn last_error(&self, context: &str) -> Error {
        let msg = unsafe { CStr::from_ptr(sqlite3_errmsg(self.raw)).to_string_lossy().into_owned() };
        Error::Msg(format!("sqlite ({context}): {msg}"))
    }

    /// Executes one or more semicolon-separated SQL statements with no result rows.
    pub fn exec_batch(&self, sql: &str) -> Result<()> {
        let csql = CString::new(sql).map_err(|_| Error::Msg("sqlite: SQL contains NUL".into()))?;
        let mut errmsg: *mut c_char = ptr::null_mut();
        let rc = unsafe { sqlite3_exec(self.raw, csql.as_ptr(), None, ptr::null_mut(), &mut errmsg) };
        if rc != SQLITE_OK {
            let msg = if !errmsg.is_null() {
                let s = unsafe { CStr::from_ptr(errmsg).to_string_lossy().into_owned() };
                unsafe { sqlite3_free(errmsg as *mut c_void) };
                s
            } else {
                unsafe { CStr::from_ptr(sqlite3_errmsg(self.raw)).to_string_lossy().into_owned() }
            };
            return Err(Error::Msg(format!("sqlite: {msg}")));
        }
        Ok(())
    }

    /// Prepares a single SQL statement.
    pub fn prepare(&self, sql: &str) -> Result<Statement<'_>> {
        let mut raw: *mut Sqlite3Stmt = ptr::null_mut();
        let rc = unsafe {
            sqlite3_prepare_v2(
                self.raw,
                sql.as_ptr() as *const c_char,
                sql.len() as c_int,
                &mut raw,
                ptr::null_mut(),
            )
        };
        if rc != SQLITE_OK || raw.is_null() {
            return Err(self.last_error("prepare"));
        }
        Ok(Statement { conn: self, raw })
    }

    /// Runs `f` inside an `IMMEDIATE` transaction, rolling back on error.
    pub fn transaction<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        self.exec_batch("BEGIN IMMEDIATE;")?;
        match f(self) {
            Ok(v) => {
                self.exec_batch("COMMIT;")?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.exec_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    pub fn last_insert_rowid(&self) -> i64 {
        unsafe { sqlite3_last_insert_rowid(self.raw) }
    }

    pub fn pragma_i64(&self, pragma: &str) -> Result<i64> {
        let mut stmt = self.prepare(&format!("PRAGMA {pragma};"))?;
        if stmt.step_row()? {
            Ok(stmt.col_i64(0))
        } else {
            Ok(0)
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { sqlite3_close_v2(self.raw) };
            self.raw = ptr::null_mut();
        }
    }
}

/// A prepared SQLite statement tied to the lifetime of its [`Connection`].
pub struct Statement<'conn> {
    conn: &'conn Connection,
    raw: *mut Sqlite3Stmt,
}

impl<'conn> Statement<'conn> {
    pub fn reset(&mut self) {
        unsafe {
            sqlite3_reset(self.raw);
            sqlite3_clear_bindings(self.raw);
        }
    }

    pub fn bind_i64(&mut self, idx: usize, val: i64) -> Result<()> {
        let rc = unsafe { sqlite3_bind_int64(self.raw, idx as c_int, val) };
        if rc != SQLITE_OK {
            return Err(self.conn.last_error("bind_i64"));
        }
        Ok(())
    }

    pub fn bind_opt_i64(&mut self, idx: usize, val: Option<i64>) -> Result<()> {
        match val {
            Some(v) => self.bind_i64(idx, v),
            None => self.bind_null(idx),
        }
    }

    pub fn bind_null(&mut self, idx: usize) -> Result<()> {
        let rc = unsafe { sqlite3_bind_null(self.raw, idx as c_int) };
        if rc != SQLITE_OK {
            return Err(self.conn.last_error("bind_null"));
        }
        Ok(())
    }

    pub fn bind_text(&mut self, idx: usize, val: &str) -> Result<()> {
        // Non-null pointer even when empty so SQLite stores '' rather than NULL.
        let (ptr, dest) = if val.is_empty() {
            (b"".as_ptr() as *const c_char, None)
        } else {
            (val.as_ptr() as *const c_char, sqlite_transient())
        };
        let rc = unsafe { sqlite3_bind_text(self.raw, idx as c_int, ptr, val.len() as c_int, dest) };
        if rc != SQLITE_OK {
            return Err(self.conn.last_error("bind_text"));
        }
        Ok(())
    }

    pub fn bind_opt_text(&mut self, idx: usize, val: Option<&str>) -> Result<()> {
        match val {
            Some(s) => self.bind_text(idx, s),
            None => self.bind_null(idx),
        }
    }

    pub fn bind_blob(&mut self, idx: usize, val: &[u8]) -> Result<()> {
        // Non-null pointer even when empty so SQLite stores X'' rather than NULL.
        let (ptr, dest) = if val.is_empty() {
            (b"".as_ptr() as *const c_void, None)
        } else {
            (val.as_ptr() as *const c_void, sqlite_transient())
        };
        let rc = unsafe { sqlite3_bind_blob(self.raw, idx as c_int, ptr, val.len() as c_int, dest) };
        if rc != SQLITE_OK {
            return Err(self.conn.last_error("bind_blob"));
        }
        Ok(())
    }

    pub fn bind_opt_blob(&mut self, idx: usize, val: Option<&[u8]>) -> Result<()> {
        match val {
            Some(b) => self.bind_blob(idx, b),
            None => self.bind_null(idx),
        }
    }

    /// Executes a non-returning statement (INSERT/UPDATE/DELETE) and resets it.
    pub fn execute(&mut self) -> Result<()> {
        let rc = unsafe { sqlite3_step(self.raw) };
        if rc != SQLITE_DONE && rc != SQLITE_ROW {
            let err = self.conn.last_error("step");
            self.reset();
            return Err(err);
        }
        self.reset();
        Ok(())
    }

    /// Advances to the next result row; returns `Ok(true)` if a row is ready,
    /// `Ok(false)` when done.
    pub fn step_row(&mut self) -> Result<bool> {
        match unsafe { sqlite3_step(self.raw) } {
            SQLITE_ROW => Ok(true),
            SQLITE_DONE => Ok(false),
            _ => Err(self.conn.last_error("step")),
        }
    }

    pub fn is_null(&self, col: usize) -> bool {
        unsafe { sqlite3_column_type(self.raw, col as c_int) == SQLITE_NULL }
    }

    pub fn col_i64(&self, col: usize) -> i64 {
        unsafe { sqlite3_column_int64(self.raw, col as c_int) }
    }

    pub fn col_opt_i64(&self, col: usize) -> Option<i64> {
        if self.is_null(col) {
            None
        } else {
            Some(self.col_i64(col))
        }
    }

    pub fn col_text(&self, col: usize) -> String {
        let len = unsafe { sqlite3_column_bytes(self.raw, col as c_int) };
        if len <= 0 {
            return String::new();
        }
        let ptr = unsafe { sqlite3_column_text(self.raw, col as c_int) };
        if ptr.is_null() {
            return String::new();
        }
        let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
        String::from_utf8_lossy(slice).into_owned()
    }

    pub fn col_opt_text(&self, col: usize) -> Option<String> {
        if self.is_null(col) {
            None
        } else {
            Some(self.col_text(col))
        }
    }

    pub fn col_blob(&self, col: usize) -> Vec<u8> {
        let len = unsafe { sqlite3_column_bytes(self.raw, col as c_int) };
        if len <= 0 {
            return Vec::new();
        }
        let ptr = unsafe { sqlite3_column_blob(self.raw, col as c_int) } as *const u8;
        if ptr.is_null() {
            return Vec::new();
        }
        unsafe { std::slice::from_raw_parts(ptr, len as usize) }.to_vec()
    }

    pub fn col_opt_blob(&self, col: usize) -> Option<Vec<u8>> {
        if self.is_null(col) {
            None
        } else {
            Some(self.col_blob(col))
        }
    }
}

impl Drop for Statement<'_> {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { sqlite3_finalize(self.raw) };
            self.raw = ptr::null_mut();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn roundtrip_types_and_transactions() {
        let dir = std::env::temp_dir().join(format!("vpm-sqlite-unit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");

        let conn = Connection::open_rw(&path).unwrap();
        conn.exec_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, s TEXT, b BLOB, n INTEGER);").unwrap();
        conn.transaction(|c| {
            let mut st = c.prepare("INSERT INTO t (s, b, n) VALUES (?1, ?2, ?3)").unwrap();
            st.bind_text(1, "").unwrap();
            st.bind_blob(2, b"").unwrap();
            st.bind_opt_i64(3, None).unwrap();
            st.execute().unwrap();

            st.bind_text(1, "hello").unwrap();
            st.bind_blob(2, b"\x00\xffraw").unwrap();
            st.bind_i64(3, 42).unwrap();
            st.execute().unwrap();
            Ok(())
        })
        .unwrap();

        // Rollback check
        let r: Result<()> = conn.transaction(|c| {
            c.exec_batch("INSERT INTO t (s, b, n) VALUES ('gone', X'01', 99);")?;
            Err(Error::Msg("abort".into()))
        });
        assert!(r.is_err());

        drop(conn);

        // Read-only open verifies persistence and rollback
        let ro = Connection::open_ro(&path).unwrap();
        let mut st = ro.prepare("SELECT s, b, n FROM t ORDER BY id").unwrap();
        assert!(st.step_row().unwrap());
        assert_eq!(st.col_opt_text(0), Some(String::new()));
        assert_eq!(st.col_opt_blob(1), Some(Vec::new()));
        assert_eq!(st.col_opt_i64(2), None);

        assert!(st.step_row().unwrap());
        assert_eq!(st.col_text(0), "hello");
        assert_eq!(st.col_blob(1), b"\x00\xffraw");
        assert_eq!(st.col_opt_i64(2), Some(42));

        assert!(!st.step_row().unwrap());

        let _ = fs::remove_dir_all(&dir);
    }
}
