//! Strictly read-only WAL-compatible SQLite connection helpers shared by
//! OpenCode (`opencode.db`) and Cursor (`state.vscdb`) decoders (`PLAN.md` §0 #3, §5).

use std::collections::BTreeSet;
use std::io;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, ErrorCode, OpenFlags};

/// Percent-encodes a filesystem path into a SQLite RFC 3986 `file:` URI.
#[must_use]
pub(crate) fn sqlite_file_uri(path: &Path, query: &str) -> String {
    let raw = path.to_string_lossy();
    let mut encoded = String::with_capacity(raw.len() + 24);
    for ch in raw.chars() {
        match ch {
            '\\' => encoded.push('/'),
            '%' => encoded.push_str("%25"),
            '?' => encoded.push_str("%3f"),
            '#' => encoded.push_str("%23"),
            ' ' => encoded.push_str("%20"),
            other => encoded.push(other),
        }
    }
    if encoded.starts_with('/') {
        format!("file://{encoded}?{query}")
    } else {
        format!("file:///{encoded}?{query}")
    }
}

fn is_readonly_or_cantopen(err: &rusqlite::Error) -> bool {
    matches!(
        err.sqlite_error_code(),
        Some(ErrorCode::CannotOpen | ErrorCode::ReadOnly)
    )
}

fn configure_and_probe_readonly(conn: Connection) -> Result<Connection, rusqlite::Error> {
    let _ = conn.busy_timeout(Duration::from_millis(2_500));
    conn.execute_batch("PRAGMA query_only = ON;")?;
    // Eagerly read database header so lazy WAL `-shm` permission failures surface immediately.
    let _: i64 = conn.query_row("PRAGMA schema_version;", [], |row| row.get(0))?;
    Ok(conn)
}

/// Opens a SQLite database strictly read-only without modifying database bytes or `mtime`.
///
/// Uses `mode=ro` + `busy_timeout` + `PRAGMA query_only = ON` so live WAL databases
/// can be read concurrently while another process holds a `BEGIN IMMEDIATE` write lock,
/// falling back to `mode=ro&immutable=1` only when `mode=ro` fails with `SQLITE_CANTOPEN`
/// or `SQLITE_READONLY` (e.g. a `0555` read-only parent directory without `-shm`).
pub fn open_sqlite_readonly(db_path: &Path) -> Result<Connection, rusqlite::Error> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_URI;

    let uri_ro = sqlite_file_uri(db_path, "mode=ro");
    match Connection::open_with_flags(&uri_ro, flags).and_then(configure_and_probe_readonly) {
        Ok(conn) => Ok(conn),
        Err(err) if is_readonly_or_cantopen(&err) => {
            let uri_immutable = sqlite_file_uri(db_path, "mode=ro&immutable=1");
            let conn = Connection::open_with_flags(&uri_immutable, flags)?;
            let _ = conn.busy_timeout(Duration::from_millis(2_500));
            conn.execute_batch("PRAGMA query_only = ON;")?;
            Ok(conn)
        }
        Err(err) => Err(err),
    }
}

pub(crate) fn table_exists(conn: &Connection, table_name: &str) -> Result<bool, rusqlite::Error> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table_name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

pub(crate) fn table_columns(
    conn: &Connection,
    table_name: &str,
) -> Result<BTreeSet<String>, rusqlite::Error> {
    let pragma = format!("PRAGMA table_info({table_name})");
    let mut stmt = conn.prepare(&pragma)?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let mut cols = BTreeSet::new();
    for col in rows {
        cols.insert(col?);
    }
    Ok(cols)
}

pub(crate) fn sqlite_err_to_io(err: rusqlite::Error) -> io::Error {
    io::Error::other(format!("sqlite read-only error: {err}"))
}
