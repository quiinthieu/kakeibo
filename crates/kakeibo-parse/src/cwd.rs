//! Opt-in read-only working-directory extraction used exclusively when the user
//! explicitly passes `kakeibo export --show-paths` (`PLAN.md` §0 #5, §6.4).
//!
//! Normal discovery, parsing, attribution, flags, NDJSON ledgers, and default
//! exports never call these helpers and drop raw `cwd` strings immediately.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;

use serde::Deserialize;

use crate::aider::{parse_aider_analytics_file, parse_aider_history_file};
use crate::catalog::open_readonly;
use crate::gemini::resolve_gemini_raw_cwd;
use crate::line_reader::{BoundedLineReader, LineReadOutcome, DEFAULT_MAX_LINE_BYTES};
use crate::opencode::{
    collect_json_files_recursive, open_sqlite_readonly, sqlite_err_to_io, table_columns,
    table_exists,
};

const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

#[derive(Deserialize)]
struct MinimalClaudeCwdLine {
    #[serde(rename = "sessionId", alias = "session_id")]
    session_id: Option<String>,
    cwd: Option<String>,
}

#[derive(Deserialize)]
struct MinimalCodexLine {
    #[serde(rename = "type")]
    record_type: Option<String>,
    payload: Option<MinimalCodexPayload>,
}

#[derive(Deserialize)]
struct MinimalCodexPayload {
    id: Option<String>,
    session_id: Option<String>,
    cwd: Option<String>,
}

/// Streams a Claude Code `.jsonl` file read-only to map `session_id -> raw_cwd`
/// strictly for opt-in `export --show-paths`.
pub fn extract_claude_session_cwds(path: &Path) -> io::Result<BTreeMap<String, String>> {
    let file = open_readonly(path)?;
    let mut reader =
        BoundedLineReader::with_max_line_bytes(BufReader::new(file), DEFAULT_MAX_LINE_BYTES);
    let mut out = BTreeMap::new();
    let mut last_sid: Option<String> = None;

    while let Some(outcome) = reader.next_line()? {
        let LineReadOutcome::Line { text, .. } = outcome else {
            continue;
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<MinimalClaudeCwdLine>(trimmed) else {
            continue;
        };
        let sid = parsed
            .session_id
            .filter(|s| !s.is_empty())
            .or_else(|| last_sid.clone());
        if let Some(sid) = sid {
            last_sid = Some(sid.clone());
            if let Some(cwd) = parsed.cwd.filter(|s| !s.is_empty()) {
                out.insert(sid, cwd);
            }
        }
    }

    Ok(out)
}

fn read_codex_cwds<R: BufRead>(
    reader: R,
    is_compressed: bool,
) -> io::Result<BTreeMap<String, String>> {
    let mut line_reader = BoundedLineReader::with_max_line_bytes(reader, DEFAULT_MAX_LINE_BYTES);
    let mut out = BTreeMap::new();
    let mut current_sid: Option<String> = None;
    let mut pending_cwd: Option<String> = None;

    loop {
        let outcome = match line_reader.next_line() {
            Ok(Some(o)) => o,
            Ok(None) => break,
            Err(_) if is_compressed => break,
            Err(err) => return Err(err),
        };
        let LineReadOutcome::Line { text, .. } = outcome else {
            continue;
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<MinimalCodexLine>(trimmed) else {
            continue;
        };
        let Some(rec_type) = parsed.record_type.as_deref() else {
            continue;
        };
        if !matches!(rec_type, "session_meta" | "turn_context") {
            continue;
        }
        let Some(payload) = parsed.payload else {
            continue;
        };
        if let Some(sid) = payload
            .session_id
            .filter(|s| !s.is_empty())
            .or_else(|| payload.id.filter(|s| !s.is_empty()))
        {
            current_sid = Some(sid);
        }
        if let Some(cwd) = payload.cwd.filter(|s| !s.is_empty()) {
            pending_cwd = Some(cwd);
        }
        if let (Some(ref sid), Some(ref cwd)) = (&current_sid, &pending_cwd) {
            out.insert(sid.clone(), cwd.clone());
        }
    }

    Ok(out)
}

/// Streams a Codex CLI `.jsonl` or `.jsonl.zst` rollout read-only to map
/// `session_id -> raw_cwd` strictly for opt-in `export --show-paths`.
pub fn extract_codex_session_cwds(path: &Path) -> io::Result<BTreeMap<String, String>> {
    let file = open_readonly(path)?;
    let mut peek_reader = BufReader::new(file);
    let head = peek_reader.fill_buf()?;

    let has_zst_ext = path.extension().and_then(|ext| ext.to_str()) == Some("zst");
    let has_zst_magic = head.len() >= 4 && head[..4] == ZSTD_MAGIC;

    if has_zst_ext || has_zst_magic {
        match zstd::stream::read::Decoder::with_buffer(peek_reader) {
            Ok(decoder) => read_codex_cwds(BufReader::new(decoder), true),
            Err(_) => Ok(BTreeMap::new()),
        }
    } else {
        read_codex_cwds(peek_reader, false)
    }
}

#[derive(Deserialize)]
struct MinimalGeminiMeta {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(default)]
    directories: Vec<String>,
    #[serde(rename = "$set")]
    set_meta: Option<MinimalGeminiSetMeta>,
}

#[derive(Deserialize)]
struct MinimalGeminiSetMeta {
    #[serde(default)]
    directories: Vec<String>,
}

/// Reads a Gemini CLI `.jsonl` or `.json` file read-only along with the preloaded
/// `slug -> raw_cwd` registry to map `session_id -> raw_cwd` strictly for opt-in
/// `export --show-paths`.
pub fn extract_gemini_session_cwds(
    path: &Path,
    slug_registry: &BTreeMap<String, String>,
) -> io::Result<BTreeMap<String, String>> {
    let mut session_id: Option<String> = None;
    let mut fallback_dir: Option<String> = None;

    if path.extension().and_then(|e| e.to_str()) == Some("json") {
        let file = open_readonly(path)?;
        let mut raw = String::new();
        file.take(16 * 1024 * 1024).read_to_string(&mut raw)?;
        if let Ok(parsed) = serde_json::from_str::<MinimalGeminiMeta>(&raw) {
            session_id = parsed.session_id.filter(|s| !s.trim().is_empty());
            fallback_dir = parsed
                .directories
                .into_iter()
                .find(|d| !d.trim().is_empty());
        }
    } else {
        let file = open_readonly(path)?;
        let mut reader =
            BoundedLineReader::with_max_line_bytes(BufReader::new(file), DEFAULT_MAX_LINE_BYTES);
        while let Some(outcome) = reader.next_line()? {
            let LineReadOutcome::Line { text, .. } = outcome else {
                continue;
            };
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            };
            let Ok(parsed) = serde_json::from_str::<MinimalGeminiMeta>(trimmed) else {
                continue;
            };
            if session_id.is_none() {
                session_id = parsed.session_id.filter(|s| !s.trim().is_empty());
            }
            if fallback_dir.is_none() {
                fallback_dir = parsed
                    .directories
                    .into_iter()
                    .chain(parsed.set_meta.map(|s| s.directories).unwrap_or_default())
                    .find(|d| !d.trim().is_empty());
            }
            if session_id.is_some() && fallback_dir.is_some() {
                break;
            }
        }
    }

    let sid = session_id.or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    });
    let raw_cwd = resolve_gemini_raw_cwd(Some(path), Some(slug_registry)).or(fallback_dir);

    let mut out = BTreeMap::new();
    if let (Some(sid), Some(cwd)) = (sid, raw_cwd) {
        out.insert(sid, cwd);
    }
    Ok(out)
}

#[derive(Deserialize)]
struct MinimalOpencodeSessionCwd {
    id: Option<String>,
    directory: Option<String>,
}

/// Reads `<opencode_root>/opencode.db` and legacy `<opencode_root>/storage/session/**/*.json`
/// strictly read-only to map `session_id -> raw_cwd` for opt-in `export --show-paths`.
///
/// Entries from `opencode.db` take precedence over duplicate `session_id`s in `storage/`.
pub fn extract_opencode_session_cwds(opencode_root: &Path) -> io::Result<BTreeMap<String, String>> {
    if !opencode_root.is_dir() {
        return Ok(BTreeMap::new());
    }

    let mut out = BTreeMap::new();

    let storage_session_dir = opencode_root.join("storage").join("session");
    if storage_session_dir.is_dir() {
        let mut session_files = Vec::new();
        collect_json_files_recursive(&storage_session_dir, &mut session_files)?;
        session_files.sort();

        for ses_file in session_files {
            let Ok(file) = open_readonly(&ses_file) else {
                continue;
            };
            let mut raw = String::new();
            if file
                .take(16 * 1024 * 1024)
                .read_to_string(&mut raw)
                .is_err()
            {
                continue;
            }
            let Ok(parsed) = serde_json::from_str::<MinimalOpencodeSessionCwd>(&raw) else {
                continue;
            };
            let sid = parsed.id.filter(|s| !s.trim().is_empty()).or_else(|| {
                ses_file
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            });
            let dir = parsed
                .directory
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            if let (Some(sid), Some(cwd)) = (sid, dir) {
                out.insert(sid, cwd);
            }
        }
    }

    let db_path = opencode_root.join("opencode.db");
    if db_path.is_file() {
        let conn = open_sqlite_readonly(&db_path).map_err(sqlite_err_to_io)?;
        if table_exists(&conn, "session").map_err(sqlite_err_to_io)? {
            let cols = table_columns(&conn, "session").map_err(sqlite_err_to_io)?;
            let dir_expr = if cols.contains("directory") {
                "directory"
            } else {
                "NULL"
            };
            let data_expr = if cols.contains("data") {
                "data"
            } else {
                "NULL"
            };
            let sql = format!("SELECT id, {dir_expr}, {data_expr} FROM session ORDER BY id ASC");
            let mut stmt = conn.prepare(&sql).map_err(sqlite_err_to_io)?;
            let mut rows = stmt.query([]).map_err(sqlite_err_to_io)?;
            while let Some(row) = rows.next().map_err(sqlite_err_to_io)? {
                let id: String = row.get(0).map_err(sqlite_err_to_io)?;
                let mut dir: Option<String> = row.get(1).ok().flatten();
                let raw_data: Option<String> = row.get(2).ok().flatten();
                if dir.is_none() {
                    if let Some(data_str) = raw_data {
                        if let Ok(parsed) =
                            serde_json::from_str::<MinimalOpencodeSessionCwd>(&data_str)
                        {
                            dir = parsed.directory;
                        }
                    }
                }
                if let Some(cwd) = dir.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
                    out.insert(id, cwd);
                }
            }
        }
    }

    Ok(out)
}

/// Resolves `session_id -> raw_cwd` for an Aider `.aider.chat.history.md` or
/// `--analytics-log` JSONL file strictly for opt-in `export --show-paths`.
pub fn extract_aider_session_cwds(path: &Path) -> io::Result<BTreeMap<String, String>> {
    let Some(parent_cwd) = path
        .parent()
        .and_then(Path::to_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    else {
        return Ok(BTreeMap::new());
    };

    let parsed = if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
        parse_aider_analytics_file(path)?
    } else {
        parse_aider_history_file(path)?
    };

    let mut out = BTreeMap::new();
    for batch in parsed.sessions {
        out.insert(batch.session_id, parent_cwd.clone());
    }
    Ok(out)
}
