//! WAL-mode SQLite incremental derived-record index (`<cache_dir>/index.sqlite`,
//! `PLAN.md` §0 #5, §3.1, §7, §10 T12, `docs/decisions/003-incremental-index-storage.md`).
//!
//! Privacy invariants:
//! - Source filesystem paths are irreversibly hashed into `source_id` (`SHA-256`) and never stored raw.
//! - Only text-free, path-free [`TurnRecord`]s, checkpoints, and aggregate diagnostic counts are persisted.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kakeibo_core::{product, AgentId, TurnRecord};
use rusqlite::{params, Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::paths::AppPaths;

/// Current incremental index SQLite schema version.
pub const INDEX_SCHEMA_VERSION: &str = "1";

/// Computes a deterministic, irreversible 64-hex-char SHA-256 `source_id` for a discovered log
/// without storing any raw filesystem path in `<cache_dir>/index.sqlite`.
#[must_use]
pub fn hash_source_id(
    agent: AgentId,
    source_kind: &str,
    parent_session_id: Option<&str>,
    path: &Path,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(product::BINARY_NAME.as_bytes());
    hasher.update(b":src:v1:");
    hasher.update(agent.as_str().as_bytes());
    hasher.update(b":");
    hasher.update(source_kind.as_bytes());
    hasher.update(b":");
    if let Some(parent) = parent_session_id {
        hasher.update(parent.as_bytes());
    }
    hasher.update(b":");
    hasher.update(path.as_os_str().as_encoded_bytes());
    let digest = hasher.finalize();

    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

fn stat_mtime_parts(meta: &fs::Metadata) -> (u64, u64, u64, i64, i64) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (
            meta.dev(),
            meta.ino(),
            meta.len(),
            meta.mtime(),
            meta.mtime_nsec(),
        )
    }
    #[cfg(not(unix))]
    {
        use std::time::UNIX_EPOCH;
        let (sec, nsec) = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or((0_i64, 0_i64), |d| {
                (
                    i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
                    i64::from(d.subsec_nanos()),
                )
            });
        (0, 0, meta.len(), sec, nsec)
    }
}

fn collect_sorted_json_files(
    root: &Path,
    current: &Path,
    out: &mut Vec<(PathBuf, PathBuf)>,
) -> io::Result<()> {
    if !current.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(current)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_sorted_json_files(root, &path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            out.push((rel, path));
        }
    }
    Ok(())
}

/// Filesystem `stat` fingerprint for detecting unchanged files, appends, truncations,
/// inode rotations, and companion SQLite `-wal` or registry mutations with zero file opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFingerprint {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_sec: i64,
    pub mtime_nsec: i64,
    pub aux_size: u64,
    pub aux_mtime_sec: i64,
    pub aux_mtime_nsec: i64,
}

impl SourceFingerprint {
    /// Captures `stat` metadata for `path` plus optional auxiliary file `aux_path`
    /// (such as Gemini's `projects.json` or a SQLite `<db>-wal`).
    pub fn from_file_with_aux(path: &Path, aux_path: Option<&Path>) -> io::Result<Self> {
        let meta = fs::metadata(path)?;
        let (dev, ino, size, mtime_sec, mtime_nsec) = stat_mtime_parts(&meta);
        let (aux_size, aux_mtime_sec, aux_mtime_nsec) = aux_path
            .and_then(|p| fs::metadata(p).ok())
            .map(|aux_meta| {
                let (_, _, s, sec, nsec) = stat_mtime_parts(&aux_meta);
                (s, sec, nsec)
            })
            .unwrap_or((0, 0, 0));

        Ok(Self {
            dev,
            ino,
            size,
            mtime_sec,
            mtime_nsec,
            aux_size,
            aux_mtime_sec,
            aux_mtime_nsec,
        })
    }

    /// Captures `stat` metadata for a SQLite database file and its companion `<db>-wal` file.
    pub fn from_sqlite_db(db_path: &Path) -> io::Result<Self> {
        let mut wal_os: OsString = db_path.as_os_str().to_os_string();
        wal_os.push("-wal");
        let wal_path = PathBuf::from(wal_os);
        Self::from_file_with_aux(db_path, Some(&wal_path))
    }

    /// Captures an aggregate `stat` digest across all `.json` files in a legacy directory tree
    /// (used for OpenCode's legacy `storage/` tree) without opening any file.
    pub fn from_json_tree(root_dir: &Path) -> io::Result<Self> {
        let mut files = Vec::new();
        collect_sorted_json_files(root_dir, root_dir, &mut files)?;
        files.sort_by(|a, b| a.0.cmp(&b.0));

        let mut hasher = Sha256::new();
        let mut total_bytes: u64 = 0;
        for (rel, full) in &files {
            let meta = fs::metadata(full)?;
            let (dev, ino, size, sec, nsec) = stat_mtime_parts(&meta);
            total_bytes = total_bytes.saturating_add(size);
            hasher.update(rel.as_os_str().as_encoded_bytes());
            hasher.update([0]);
            hasher.update(dev.to_le_bytes());
            hasher.update(ino.to_le_bytes());
            hasher.update(size.to_le_bytes());
            hasher.update(sec.to_le_bytes());
            hasher.update(nsec.to_le_bytes());
        }
        let digest = hasher.finalize();

        let ino = u64::from_le_bytes(digest[0..8].try_into().expect("8-byte slice"));
        let mtime_sec = i64::from_le_bytes(digest[8..16].try_into().expect("8-byte slice"));
        let mtime_nsec = i64::from_le_bytes(digest[16..24].try_into().expect("8-byte slice"));
        let aux_mtime_sec = i64::from_le_bytes(digest[24..32].try_into().expect("8-byte slice"));

        Ok(Self {
            dev: 0,
            ino,
            size: total_bytes,
            mtime_sec,
            mtime_nsec,
            aux_size: files.len() as u64,
            aux_mtime_sec,
            aux_mtime_nsec: 0,
        })
    }
}

/// Cached text-free [`TurnRecord`] batch for one session emitted by an indexed source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CachedSessionTurns {
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub records: Vec<TurnRecord>,
}

/// Cached text-free, path-free diagnostic summary for one indexed source (`doctor`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedDiagnostics {
    pub cli_versions: Vec<String>,
    pub ignored_records: u64,
    pub unknown_record_types: BTreeMap<String, u64>,
    pub parse_issues: usize,
}

/// Complete materialized record persisted for a single discovered source in `indexed_sources`.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexedSourceRecord {
    pub source_id: String,
    pub agent: AgentId,
    pub source_kind: String,
    pub fingerprint: SourceFingerprint,
    pub committed_offset: u64,
    pub checkpoint_json: Option<String>,
    pub sessions: Vec<CachedSessionTurns>,
    pub diagnostics: CachedDiagnostics,
}

/// Raw pre-deserialization row loaded in bulk from `indexed_sources` so `rayon` workers
/// can deserialize `sessions_json` and `diagnostics_json` concurrently across cores.
#[derive(Debug, Clone)]
pub struct RawIndexedRow {
    pub source_id: String,
    pub agent_slug: String,
    pub source_kind: String,
    pub fingerprint: SourceFingerprint,
    pub committed_offset: u64,
    pub checkpoint_json: Option<String>,
    pub sessions_json: String,
    pub diagnostics_json: String,
}

impl RawIndexedRow {
    /// Materializes [`CachedSessionTurns`] and [`CachedDiagnostics`] from stored JSON columns.
    pub fn decode_payloads(&self) -> Option<(Vec<CachedSessionTurns>, CachedDiagnostics)> {
        let sessions: Vec<CachedSessionTurns> = serde_json::from_str(&self.sessions_json).ok()?;
        let diagnostics: CachedDiagnostics = serde_json::from_str(&self.diagnostics_json).ok()?;
        Some((sessions, diagnostics))
    }
}

/// Classification of a discovered source against the [`IndexSnapshot`].
#[derive(Debug, Clone)]
pub enum SourceLookupOutcome<'a> {
    /// `stat` fingerprint matches identically: zero file opens, zero bytes read.
    Unchanged(&'a RawIndexedRow),
    /// Same `(dev, ino)` and auxiliary fingerprint, file strictly grew past `last_seen_size`,
    /// and a valid text-free checkpoint exists at `committed_offset` (`<= last_seen_size`).
    ResumeFromOffset {
        committed_offset: u64,
        checkpoint_json: &'a str,
    },
    /// Cold miss, truncation (`size < last_seen_size`), inode rotation, in-place rewrite,
    /// auxiliary file change, or non-resumable format: parse from byte `0`.
    FullReparse,
}

/// Thread-safe (`Send + Sync`) read snapshot of `indexed_sources` for concurrent `rayon` lookup.
#[derive(Debug, Default)]
pub struct IndexSnapshot {
    rows: HashMap<String, RawIndexedRow>,
}

impl IndexSnapshot {
    /// Classifies `source_id` with current `fp` into `Unchanged`, `ResumeFromOffset`, or `FullReparse`.
    #[must_use]
    pub fn classify<'a>(
        &'a self,
        source_id: &str,
        fp: &SourceFingerprint,
        is_appendable: bool,
    ) -> SourceLookupOutcome<'a> {
        let Some(old) = self.rows.get(source_id) else {
            return SourceLookupOutcome::FullReparse;
        };

        if &old.fingerprint == fp {
            return SourceLookupOutcome::Unchanged(old);
        }

        let same_inode = fp.dev == old.fingerprint.dev && fp.ino == old.fingerprint.ino;
        let same_aux = fp.aux_size == old.fingerprint.aux_size
            && fp.aux_mtime_sec == old.fingerprint.aux_mtime_sec
            && fp.aux_mtime_nsec == old.fingerprint.aux_mtime_nsec;
        let strictly_appended = fp.size > old.fingerprint.last_seen_size_alias()
            && old.committed_offset <= old.fingerprint.size;

        if is_appendable && same_inode && same_aux && strictly_appended {
            if let Some(cp) = old.checkpoint_json.as_deref() {
                return SourceLookupOutcome::ResumeFromOffset {
                    committed_offset: old.committed_offset,
                    checkpoint_json: cp,
                };
            }
        }

        SourceLookupOutcome::FullReparse
    }

    /// Returns stale `source_id`s present in this snapshot that are no longer in `active_ids`.
    #[must_use]
    pub fn stale_source_ids(&self, active_ids: &BTreeSet<String>) -> Vec<String> {
        self.rows
            .keys()
            .filter(|id| !active_ids.contains(*id))
            .cloned()
            .collect()
    }
}

impl SourceFingerprint {
    const fn last_seen_size_alias(&self) -> u64 {
        self.size
    }
}

/// Scope for pruning deleted source rows from `indexed_sources`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneScope {
    AllAgents,
    SingleAgent(AgentId),
}

/// Sole-writer WAL-mode SQLite handle for `<cache_dir>/index.sqlite` (`PLAN.md` §3.1).
pub struct IndexStore {
    conn: Connection,
}

impl IndexStore {
    /// Opens `<cache_dir>/index.sqlite`, transparently recreating it inside `<cache_dir>`
    /// if the file is corrupt or unreadable (`ADR 003`).
    pub fn open_or_rebuild(paths: &AppPaths) -> io::Result<Self> {
        fs::create_dir_all(paths.cache_dir())?;
        let db_path = paths.index_db_path();

        match Self::try_open_and_init(&db_path) {
            Ok(store) => Ok(store),
            Err(_) => {
                Self::remove_db_files(paths);
                Self::try_open_and_init(&db_path).map_err(|e| io::Error::other(e.to_string()))
            }
        }
    }

    fn remove_db_files(paths: &AppPaths) {
        let base = paths.index_db_path();
        let _ = fs::remove_file(&base);
        for suffix in ["-wal", "-shm"] {
            let mut os = base.as_os_str().to_os_string();
            os.push(suffix);
            let _ = fs::remove_file(PathBuf::from(os));
        }
    }

    fn try_open_and_init(db_path: &Path) -> rusqlite::Result<Self> {
        let mut conn = Connection::open(db_path)?;
        conn.busy_timeout(Duration::from_millis(5_000))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;
             CREATE TABLE IF NOT EXISTS index_meta (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS indexed_sources (
                 source_id        TEXT PRIMARY KEY,
                 agent            TEXT NOT NULL,
                 source_kind      TEXT NOT NULL,
                 dev              INTEGER NOT NULL,
                 ino              INTEGER NOT NULL,
                 last_seen_size   INTEGER NOT NULL,
                 mtime_sec        INTEGER NOT NULL,
                 mtime_nsec       INTEGER NOT NULL,
                 aux_size         INTEGER NOT NULL,
                 aux_mtime_sec    INTEGER NOT NULL,
                 aux_mtime_nsec   INTEGER NOT NULL,
                 committed_offset INTEGER NOT NULL,
                 checkpoint_json  TEXT,
                 sessions_json    TEXT NOT NULL,
                 diagnostics_json TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_indexed_sources_agent
                 ON indexed_sources(agent);",
        )?;

        // Probe table readability and verify schema + binary version match.
        let _: i64 = conn.query_row("SELECT COUNT(*) FROM indexed_sources;", [], |r| r.get(0))?;

        let stored_schema: Option<String> = conn
            .query_row(
                "SELECT value FROM index_meta WHERE key = 'index_schema_version';",
                [],
                |r| r.get(0),
            )
            .ok();
        let stored_version: Option<String> = conn
            .query_row(
                "SELECT value FROM index_meta WHERE key = 'binary_version';",
                [],
                |r| r.get(0),
            )
            .ok();

        if stored_schema.as_deref() != Some(INDEX_SCHEMA_VERSION)
            || stored_version.as_deref() != Some(product::VERSION)
        {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute("DELETE FROM indexed_sources;", [])?;
            tx.execute(
                "INSERT INTO index_meta (key, value) VALUES ('index_schema_version', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value;",
                params![INDEX_SCHEMA_VERSION],
            )?;
            tx.execute(
                "INSERT INTO index_meta (key, value) VALUES ('binary_version', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value;",
                params![product::VERSION],
            )?;
            tx.commit()?;
        }

        Ok(Self { conn })
    }

    /// Loads an immutable [`IndexSnapshot`] for `scope` in a single sequential SQL query.
    pub fn load_snapshot(&self, scope: PruneScope) -> io::Result<IndexSnapshot> {
        let mut rows = HashMap::new();
        let sql_all = "SELECT source_id, agent, source_kind, dev, ino, last_seen_size,
                              mtime_sec, mtime_nsec, aux_size, aux_mtime_sec, aux_mtime_nsec,
                              committed_offset, checkpoint_json, sessions_json, diagnostics_json
                       FROM indexed_sources;";
        let sql_one = "SELECT source_id, agent, source_kind, dev, ino, last_seen_size,
                              mtime_sec, mtime_nsec, aux_size, aux_mtime_sec, aux_mtime_nsec,
                              committed_offset, checkpoint_json, sessions_json, diagnostics_json
                       FROM indexed_sources
                       WHERE agent = ?1;";

        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<RawIndexedRow> {
            let dev_i: i64 = row.get(3)?;
            let ino_i: i64 = row.get(4)?;
            let size_i: i64 = row.get(5)?;
            let aux_size_i: i64 = row.get(8)?;
            let committed_i: i64 = row.get(11)?;
            Ok(RawIndexedRow {
                source_id: row.get(0)?,
                agent_slug: row.get(1)?,
                source_kind: row.get(2)?,
                fingerprint: SourceFingerprint {
                    dev: dev_i as u64,
                    ino: ino_i as u64,
                    size: size_i as u64,
                    mtime_sec: row.get(6)?,
                    mtime_nsec: row.get(7)?,
                    aux_size: aux_size_i as u64,
                    aux_mtime_sec: row.get(9)?,
                    aux_mtime_nsec: row.get(10)?,
                },
                committed_offset: committed_i as u64,
                checkpoint_json: row.get(12)?,
                sessions_json: row.get(13)?,
                diagnostics_json: row.get(14)?,
            })
        };

        let result: rusqlite::Result<()> = (|| match scope {
            PruneScope::AllAgents => {
                let mut stmt = self.conn.prepare(sql_all)?;
                let iter = stmt.query_map([], map_row)?;
                for item in iter {
                    let r = item?;
                    rows.insert(r.source_id.clone(), r);
                }
                Ok(())
            }
            PruneScope::SingleAgent(agent) => {
                let mut stmt = self.conn.prepare(sql_one)?;
                let iter = stmt.query_map(params![agent.as_str()], map_row)?;
                for item in iter {
                    let r = item?;
                    rows.insert(r.source_id.clone(), r);
                }
                Ok(())
            }
        })();

        result.map_err(|e| io::Error::other(e.to_string()))?;
        Ok(IndexSnapshot { rows })
    }

    /// Commits newly parsed/resumed `upserts` and prunes `stale_source_ids` atomically
    /// in a single `IMMEDIATE` transaction. No-op if both slices are empty.
    pub fn commit_updates(
        &mut self,
        upserts: &[IndexedSourceRecord],
        stale_source_ids: &[String],
    ) -> io::Result<()> {
        if upserts.is_empty() && stale_source_ids.is_empty() {
            return Ok(());
        }

        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| io::Error::other(e.to_string()))?;

        if !upserts.is_empty() {
            let mut upsert_stmt = tx
                .prepare(
                    "INSERT INTO indexed_sources (
                         source_id, agent, source_kind, dev, ino, last_seen_size,
                         mtime_sec, mtime_nsec, aux_size, aux_mtime_sec, aux_mtime_nsec,
                         committed_offset, checkpoint_json, sessions_json, diagnostics_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                     ON CONFLICT(source_id) DO UPDATE SET
                         agent            = excluded.agent,
                         source_kind      = excluded.source_kind,
                         dev              = excluded.dev,
                         ino              = excluded.ino,
                         last_seen_size   = excluded.last_seen_size,
                         mtime_sec        = excluded.mtime_sec,
                         mtime_nsec       = excluded.mtime_nsec,
                         aux_size         = excluded.aux_size,
                         aux_mtime_sec    = excluded.aux_mtime_sec,
                         aux_mtime_nsec   = excluded.aux_mtime_nsec,
                         committed_offset = excluded.committed_offset,
                         checkpoint_json  = excluded.checkpoint_json,
                         sessions_json    = excluded.sessions_json,
                         diagnostics_json = excluded.diagnostics_json;",
                )
                .map_err(|e| io::Error::other(e.to_string()))?;

            for rec in upserts {
                let sessions_json = serde_json::to_string(&rec.sessions)
                    .map_err(|e| io::Error::other(e.to_string()))?;
                let diagnostics_json = serde_json::to_string(&rec.diagnostics)
                    .map_err(|e| io::Error::other(e.to_string()))?;
                upsert_stmt
                    .execute(params![
                        rec.source_id,
                        rec.agent.as_str(),
                        rec.source_kind,
                        rec.fingerprint.dev as i64,
                        rec.fingerprint.ino as i64,
                        rec.fingerprint.size as i64,
                        rec.fingerprint.mtime_sec,
                        rec.fingerprint.mtime_nsec,
                        rec.fingerprint.aux_size as i64,
                        rec.fingerprint.aux_mtime_sec,
                        rec.fingerprint.aux_mtime_nsec,
                        rec.committed_offset as i64,
                        rec.checkpoint_json,
                        sessions_json,
                        diagnostics_json,
                    ])
                    .map_err(|e| io::Error::other(e.to_string()))?;
            }
        }

        if !stale_source_ids.is_empty() {
            let mut del_stmt = tx
                .prepare("DELETE FROM indexed_sources WHERE source_id = ?1;")
                .map_err(|e| io::Error::other(e.to_string()))?;
            for sid in stale_source_ids {
                del_stmt
                    .execute(params![sid])
                    .map_err(|e| io::Error::other(e.to_string()))?;
            }
        }

        tx.commit().map_err(|e| io::Error::other(e.to_string()))?;
        Ok(())
    }
}
