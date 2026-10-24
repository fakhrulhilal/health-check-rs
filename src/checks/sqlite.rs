//! SQLite via raw `libsqlite3-sys` FFI (feature `sqlite`; `sqlite-bundled` compiles SQLite
//! from source instead of linking the system library). Equivalent of `AddSqlite`.
//!
//! Opens the database **read-only** (a missing file is reported, never created), runs a
//! query that reads the schema page, and closes. That catches a missing file, a file that
//! isn't a database, permission problems and a lock held longer than the busy timeout.
//!
//! Accepts a path, a `file:` URI, or a Microsoft.Data.Sqlite connection string
//! (`Data Source=app.db;...`; other keys are ignored).
//!
//! No rusqlite: the whole check is open → exec → close, so the dependency is just the
//! C bindings. SQLite calls block, so they run on Tokio's blocking pool.

use std::borrow::Cow;
use std::ffi::{CStr, CString, c_char, c_int};
use std::ptr;
use std::time::Duration;

use async_trait::async_trait;
use libsqlite3_sys as ffi;

use crate::{BoxError, HealthCheck, HealthCheckContext, HealthCheckResult};

pub struct SqliteCheck {
    path: CString,
    query: Cow<'static, str>,
    busy_timeout: Duration,
}

impl SqliteCheck {
    /// `app.db`, `/var/lib/app/app.db`, `file:app.db?mode=ro`, or `Data Source=app.db;Cache=Shared`.
    pub fn new(path_or_connection_string: &str) -> Result<Self, BoxError> {
        let path = parse_data_source(path_or_connection_string)?;
        Ok(Self {
            path: CString::new(path).map_err(|_| "path contains a NUL byte")?,
            query: Cow::Borrowed("SELECT count(*) FROM sqlite_master"),
            busy_timeout: Duration::from_secs(1),
        })
    }

    /// Replace the default `SELECT count(*) FROM sqlite_master`
    /// (e.g. `PRAGMA quick_check`, which scans the whole file).
    pub fn with_query(mut self, query: impl Into<Cow<'static, str>>) -> Self {
        self.query = query.into();
        self
    }

    /// How long to wait on a locked database before failing. Default 1 s.
    pub fn busy_timeout(mut self, timeout: Duration) -> Self {
        self.busy_timeout = timeout;
        self
    }
}

#[async_trait]
impl HealthCheck for SqliteCheck {
    async fn check(&self, _ctx: &HealthCheckContext) -> Result<HealthCheckResult, BoxError> {
        let path = self.path.clone();
        let query = CString::new(self.query.as_bytes()).map_err(|_| "query contains a NUL byte")?;
        let busy_ms = c_int::try_from(self.busy_timeout.as_millis()).unwrap_or(c_int::MAX);
        tokio::task::spawn_blocking(move || probe(&path, &query, busy_ms)).await??;
        Ok(HealthCheckResult::healthy())
    }
}

/// Closes the handle on every path out of `probe`.
struct Db(*mut ffi::sqlite3);

impl Drop for Db {
    fn drop(&mut self) {
        // SAFETY: handle came from sqlite3_open_v2 (null is a no-op); sqlite3_exec already
        // finalized its statements, so plain close succeeds.
        unsafe { ffi::sqlite3_close(self.0) };
    }
}

fn probe(path: &CStr, query: &CStr, busy_ms: c_int) -> Result<(), BoxError> {
    let mut db = Db(ptr::null_mut());
    let flags = ffi::SQLITE_OPEN_READONLY | ffi::SQLITE_OPEN_URI | ffi::SQLITE_OPEN_NOMUTEX;
    // SAFETY: valid C strings; out-pointer is a local; the handle is closed by `Db`.
    unsafe {
        let rc = ffi::sqlite3_open_v2(path.as_ptr(), &mut db.0, flags, ptr::null());
        if rc != ffi::SQLITE_OK {
            return Err(error(db.0, rc, "open"));
        }
        ffi::sqlite3_busy_timeout(db.0, busy_ms);
        let mut errmsg: *mut c_char = ptr::null_mut();
        let rc = ffi::sqlite3_exec(db.0, query.as_ptr(), None, ptr::null_mut(), &mut errmsg);
        if rc != ffi::SQLITE_OK {
            let message = if errmsg.is_null() {
                None
            } else {
                let m = CStr::from_ptr(errmsg).to_string_lossy().into_owned();
                ffi::sqlite3_free(errmsg.cast());
                Some(m)
            };
            return Err(match message {
                Some(m) => format!("query failed: {m} (code {rc})").into(),
                None => error(db.0, rc, "query"),
            });
        }
    }
    Ok(())
}

/// # Safety
/// `db` must be null or a handle from `sqlite3_open_v2`.
unsafe fn error(db: *mut ffi::sqlite3, rc: c_int, what: &str) -> BoxError {
    let message = if db.is_null() {
        // SAFETY: sqlite3_errstr returns a static string for any code.
        unsafe { CStr::from_ptr(ffi::sqlite3_errstr(rc)) }
    } else {
        // SAFETY: valid handle; string lives until the next call on it.
        unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(db)) }
    };
    format!("{what} failed: {} (code {rc})", message.to_string_lossy()).into()
}

/// Path, `file:` URI, or Microsoft.Data.Sqlite `Data Source=...` connection string.
fn parse_data_source(s: &str) -> Result<String, BoxError> {
    let s = s.trim();
    if !s.contains('=') || s.starts_with("file:") {
        return if s.is_empty() {
            Err("empty SQLite path".into())
        } else {
            Ok(s.to_owned())
        };
    }
    for part in s.split(';') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let key: String = key
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        if matches!(key.as_str(), "datasource" | "filename" | "data source") {
            let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
            if !value.is_empty() {
                return Ok(value.to_owned());
            }
        }
    }
    Err("connection string has no Data Source".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_source_forms() {
        assert_eq!(parse_data_source("app.db").unwrap(), "app.db");
        assert_eq!(
            parse_data_source("file:app.db?mode=ro").unwrap(),
            "file:app.db?mode=ro"
        );
        assert_eq!(
            parse_data_source("Data Source=/var/app.db;Cache=Shared").unwrap(),
            "/var/app.db"
        );
        assert_eq!(
            parse_data_source("Mode=ReadOnly;Filename='x y.db'").unwrap(),
            "x y.db"
        );
        assert!(parse_data_source("Mode=ReadOnly").is_err());
        assert!(parse_data_source("  ").is_err());
    }
}
