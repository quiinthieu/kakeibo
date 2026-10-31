//! Read-only OpenCode SQLite (`opencode.db`) and legacy `storage/` JSON tree decoder
//! (`PLAN.md` §0 #3–#5, §5, §6.2, §10 T10, `docs/formats/opencode.md`).
//!
//! Opens `opencode.db` strictly read-only via SQLite URI `mode=ro` + `PRAGMA query_only = ON`
//! (with `immutable=1` fallback only when `mode=ro` fails with `SQLITE_CANTOPEN` / `SQLITE_READONLY`
//! on a read-only parent directory), preserving DB bytes (SHA-256) and `mtime` even while
//! another process holds an active WAL write lock.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read};
use std::mem;
use std::path::{Path, PathBuf};

use kakeibo_core::{
    AgentId, BlockKind, Confidence, ContentBlock, DisjointUsageInput, Timestamp, TurnHints,
    TurnRecord, Usage,
};
use serde_json::{Map, Value};

use crate::catalog::open_readonly;
use crate::hash::{
    canonical_json_byte_len, estimate_tokens, hash_cwd, measure_and_hash_tool_input,
};
use crate::line_reader::{ParseDiagnostic, ParseDiagnosticKind, ParseDiagnostics};
pub use crate::sqlite::open_sqlite_readonly;
use crate::sqlite::{sqlite_err_to_io, table_columns, table_exists};

const MAX_OPENCODE_JSON_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Decoded [`TurnRecord`] batch for a single OpenCode session (`primary` or `subagent`).
#[derive(Debug, Clone, PartialEq)]
pub struct OpencodeSessionBatch {
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub records: Vec<TurnRecord>,
}

/// Decoded sessions and non-fatal diagnostics produced from an OpenCode data root,
/// SQLite database, or legacy `storage/` tree.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OpencodeParseOutput {
    pub sessions: Vec<OpencodeSessionBatch>,
    pub diagnostics: ParseDiagnostics,
}

impl OpencodeParseOutput {
    /// Flattens all session batches into a single [`TurnRecord`] vector.
    #[must_use]
    pub fn all_records(&self) -> Vec<TurnRecord> {
        let total: usize = self.sessions.iter().map(|s| s.records.len()).sum();
        let mut out = Vec::with_capacity(total);
        for batch in &self.sessions {
            out.extend(batch.records.iter().cloned());
        }
        out
    }
}

fn is_safe_version_token(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 64
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+' | b'_'))
}

fn is_safe_type_token(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 64
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn timestamp_from_millis(ms: i64) -> Option<Timestamp> {
    let nanos = i128::from(ms).checked_mul(1_000_000)?;
    Timestamp::from_unix_timestamp_nanos(nanos).ok()
}

#[derive(Debug, Clone)]
struct CompactSessionMeta {
    id: String,
    parent_session_id: Option<String>,
    cwd_hash: Option<String>,
    created_ms: i64,
    created_ts: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, Default)]
struct RawOpencodeTokens {
    input: u64,
    output: u64,
    reasoning: u64,
    cache_read: u64,
    cache_write: u64,
}

impl RawOpencodeTokens {
    fn from_value(val: &Value) -> Option<Self> {
        let obj = val.as_object()?;
        let cache_obj = obj.get("cache").and_then(Value::as_object);
        Some(Self {
            input: obj.get("input").and_then(Value::as_u64).unwrap_or(0),
            output: obj.get("output").and_then(Value::as_u64).unwrap_or(0),
            reasoning: obj.get("reasoning").and_then(Value::as_u64).unwrap_or(0),
            cache_read: cache_obj
                .and_then(|c| c.get("read"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_write: cache_obj
                .and_then(|c| c.get("write"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
        })
    }

    const fn is_all_zero(self) -> bool {
        self.input == 0
            && self.output == 0
            && self.reasoning == 0
            && self.cache_read == 0
            && self.cache_write == 0
    }
}

#[derive(Debug, Clone)]
enum CompactOpencodePartKind {
    Text {
        bytes: u64,
    },
    Reasoning {
        bytes: u64,
    },
    Tool {
        tool_name: String,
        args_bytes: u64,
        key_hash: String,
        result_bytes: Option<u64>,
        is_error: bool,
    },
    Compaction {
        summary_bytes: Option<u64>,
    },
    Subtask {
        prompt_bytes: Option<u64>,
    },
    Retry,
    LifecycleIgnored,
}

#[derive(Debug, Clone)]
struct CompactOpencodePart {
    sort_ms: i64,
    id: String,
    kind: CompactOpencodePartKind,
}

#[derive(Debug, Clone)]
struct CompactOpencodeMessage {
    sort_ms: i64,
    id: String,
    role: String,
    model_id: Option<String>,
    user_model_id: Option<String>,
    path_cwd_hash: Option<String>,
    created_ts: Option<Timestamp>,
    completed_ts: Option<Timestamp>,
    tokens: Option<RawOpencodeTokens>,
    cost: Option<f64>,
    is_summary: bool,
    has_error: bool,
    parts: Vec<CompactOpencodePart>,
}

fn non_zero(v: u64) -> Option<u64> {
    (v > 0).then_some(v)
}

fn parse_compact_part(
    part_id_fallback: &str,
    sort_ms_fallback: i64,
    obj: &Map<String, Value>,
    diagnostics: &mut ParseDiagnostics,
) -> CompactOpencodePart {
    let id = obj
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(part_id_fallback)
        .to_string();
    let part_type = obj.get("type").and_then(Value::as_str).unwrap_or("");

    let kind = match part_type {
        "text" => {
            let bytes = obj
                .get("text")
                .and_then(Value::as_str)
                .map_or(0, |s| s.len() as u64);
            CompactOpencodePartKind::Text { bytes }
        }
        "reasoning" => {
            let bytes = obj
                .get("text")
                .and_then(Value::as_str)
                .map_or(0, |s| s.len() as u64);
            CompactOpencodePartKind::Reasoning { bytes }
        }
        "tool" => {
            let tool_name = obj
                .get("tool")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("unknown_tool")
                .to_string();
            let state_obj = obj.get("state").and_then(Value::as_object);
            let empty_input = Value::Object(Map::new());
            let input_val = state_obj
                .and_then(|st| st.get("input"))
                .unwrap_or(&empty_input);
            let (args_bytes, key_hash) = measure_and_hash_tool_input(&tool_name, input_val);

            let status = state_obj
                .and_then(|st| st.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let is_error = status == "error";

            let result_bytes = state_obj
                .and_then(|st| st.get("output"))
                .and_then(|out_val| match out_val {
                    Value::Null => None,
                    Value::String(s) => non_zero(s.len() as u64),
                    other => non_zero(canonical_json_byte_len(other)),
                })
                .or_else(|| {
                    state_obj
                        .and_then(|st| st.get("error"))
                        .and_then(Value::as_str)
                        .and_then(|s| non_zero(s.len() as u64))
                });

            CompactOpencodePartKind::Tool {
                tool_name,
                args_bytes,
                key_hash,
                result_bytes,
                is_error,
            }
        }
        "compaction" => {
            let summary_bytes = obj
                .get("text")
                .and_then(Value::as_str)
                .and_then(|s| non_zero(s.len() as u64));
            CompactOpencodePartKind::Compaction { summary_bytes }
        }
        "subtask" => {
            let prompt_bytes = obj
                .get("prompt")
                .or_else(|| obj.get("text"))
                .and_then(Value::as_str)
                .and_then(|s| non_zero(s.len() as u64));
            CompactOpencodePartKind::Subtask { prompt_bytes }
        }
        "retry" => CompactOpencodePartKind::Retry,
        "step-start" | "step-finish" | "patch" | "snapshot" | "file" | "agent" | "" => {
            CompactOpencodePartKind::LifecycleIgnored
        }
        other => {
            let safe_key = if is_safe_type_token(other) {
                format!("part:{other}")
            } else {
                "part:<redacted>".to_string()
            };
            *diagnostics
                .unknown_record_types
                .entry(safe_key)
                .or_insert(0) += 1;
            CompactOpencodePartKind::LifecycleIgnored
        }
    };

    CompactOpencodePart {
        sort_ms: sort_ms_fallback,
        id,
        kind,
    }
}

fn parse_compact_message(
    msg_id_fallback: &str,
    created_ms_fallback: Option<i64>,
    updated_ms_fallback: Option<i64>,
    obj: &Map<String, Value>,
) -> CompactOpencodeMessage {
    let id = obj
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(msg_id_fallback)
        .to_string();
    let role = obj
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let time_obj = obj.get("time").and_then(Value::as_object);
    let json_created = time_obj
        .and_then(|t| t.get("created"))
        .and_then(Value::as_i64);
    let json_completed = time_obj
        .and_then(|t| t.get("completed"))
        .and_then(Value::as_i64);

    let sort_ms = json_created.or(created_ms_fallback).unwrap_or(0);
    let created_ts = json_created
        .or(created_ms_fallback)
        .and_then(timestamp_from_millis);
    let completed_ts = json_completed
        .or(updated_ms_fallback)
        .and_then(timestamp_from_millis);

    let model_id = obj
        .get("modelID")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let user_model_id = obj
        .get("model")
        .and_then(Value::as_object)
        .and_then(|m| m.get("modelID"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let path_cwd_hash = obj
        .get("path")
        .and_then(Value::as_object)
        .and_then(|p| p.get("cwd"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(hash_cwd);

    let tokens = obj.get("tokens").and_then(RawOpencodeTokens::from_value);
    let cost = obj.get("cost").and_then(Value::as_f64);
    let is_summary = obj.get("summary").and_then(Value::as_bool).unwrap_or(false);
    let has_error = obj.get("error").is_some_and(|v| !v.is_null());

    CompactOpencodeMessage {
        sort_ms,
        id,
        role,
        model_id,
        user_model_id,
        path_cwd_hash,
        created_ts,
        completed_ts,
        tokens,
        cost,
        is_summary,
        has_error,
        parts: Vec::new(),
    }
}

fn build_opencode_session_turns(
    session: &CompactSessionMeta,
    mut messages: Vec<CompactOpencodeMessage>,
    diagnostics: &mut ParseDiagnostics,
) -> OpencodeSessionBatch {
    messages.sort_by(|a, b| a.sort_ms.cmp(&b.sort_ms).then_with(|| a.id.cmp(&b.id)));

    let sidechain = session.parent_session_id.is_some();
    let mut records = Vec::new();
    let mut pending_input_blocks: Vec<ContentBlock> = Vec::new();
    let mut pending_compaction = false;
    let mut pending_api_error = false;
    let mut last_ts: Option<Timestamp> = session.created_ts;
    let mut last_model: Option<String> = None;
    let mut turn_num: u32 = 0;

    for mut msg in messages {
        msg.parts
            .sort_by(|a, b| a.sort_ms.cmp(&b.sort_ms).then_with(|| a.id.cmp(&b.id)));

        match msg.role.as_str() {
            "user" => {
                if msg.user_model_id.is_some() {
                    last_model = msg.user_model_id;
                }
                if msg.created_ts.is_some() {
                    last_ts = msg.created_ts;
                }
                for part in msg.parts {
                    match part.kind {
                        CompactOpencodePartKind::Text { bytes } if bytes > 0 => {
                            pending_input_blocks.push(ContentBlock {
                                kind: BlockKind::UserPrompt,
                                est_tokens: estimate_tokens(bytes),
                                bytes,
                                tool: None,
                                key_hash: None,
                                is_error: false,
                            });
                        }
                        CompactOpencodePartKind::Subtask {
                            prompt_bytes: Some(bytes),
                        } if bytes > 0 => {
                            pending_input_blocks.push(ContentBlock {
                                kind: BlockKind::UserPrompt,
                                est_tokens: estimate_tokens(bytes),
                                bytes,
                                tool: None,
                                key_hash: None,
                                is_error: false,
                            });
                        }
                        CompactOpencodePartKind::Compaction { summary_bytes } => {
                            pending_compaction = true;
                            if let Some(bytes) = summary_bytes {
                                pending_input_blocks.push(ContentBlock {
                                    kind: BlockKind::CompactionSummary,
                                    est_tokens: estimate_tokens(bytes),
                                    bytes,
                                    tool: None,
                                    key_hash: None,
                                    is_error: false,
                                });
                            }
                        }
                        CompactOpencodePartKind::Retry => {
                            pending_api_error = true;
                        }
                        _ => {}
                    }
                }
            }
            "assistant" => {
                if msg.is_summary {
                    pending_compaction = true;
                }
                if msg.has_error {
                    pending_api_error = true;
                }

                let Some(tok) = msg.tokens else {
                    diagnostics.ignored_records += 1;
                    continue;
                };
                if tok.is_all_zero() {
                    diagnostics.ignored_records += 1;
                    continue;
                }

                let Some(ts) = msg.completed_ts.or(msg.created_ts).or(last_ts) else {
                    diagnostics.issues.push(ParseDiagnostic {
                        line_number: 1,
                        kind: ParseDiagnosticKind::MissingField { field: "time" },
                    });
                    continue;
                };
                last_ts = Some(ts);

                let model = msg
                    .model_id
                    .or(msg.user_model_id)
                    .or_else(|| last_model.clone());
                if model.is_some() {
                    last_model.clone_from(&model);
                }

                let agent_reported_cost_usd = msg.cost.filter(|c| c.is_finite() && *c > 0.0);
                let output_total = tok.output.saturating_add(tok.reasoning);
                let reasoning_tokens = (tok.reasoning > 0).then_some(tok.reasoning);

                let usage = match Usage::from_disjoint(DisjointUsageInput {
                    input_tokens: tok.input,
                    cache_write_tokens: tok.cache_write,
                    cache_read_tokens: tok.cache_read,
                    output_tokens: output_total,
                    reasoning_tokens,
                    confidence: Confidence::Measured,
                    estimation_method: None,
                    cache_write_split: None,
                    agent_reported_cost_usd,
                }) {
                    Ok(u) => u,
                    Err(_) => {
                        diagnostics.issues.push(ParseDiagnostic {
                            line_number: 1,
                            kind: ParseDiagnosticKind::UsageOverflow,
                        });
                        continue;
                    }
                };

                let mut added = mem::take(&mut pending_input_blocks);
                for part in msg.parts {
                    match part.kind {
                        CompactOpencodePartKind::Reasoning { bytes } if bytes > 0 => {
                            added.push(ContentBlock {
                                kind: BlockKind::AssistantThinking,
                                est_tokens: estimate_tokens(bytes),
                                bytes,
                                tool: None,
                                key_hash: None,
                                is_error: false,
                            });
                        }
                        CompactOpencodePartKind::Text { bytes } if bytes > 0 => {
                            added.push(ContentBlock {
                                kind: BlockKind::AssistantText,
                                est_tokens: estimate_tokens(bytes),
                                bytes,
                                tool: None,
                                key_hash: None,
                                is_error: false,
                            });
                        }
                        CompactOpencodePartKind::Tool {
                            tool_name,
                            args_bytes,
                            key_hash,
                            result_bytes,
                            is_error,
                        } => {
                            added.push(ContentBlock {
                                kind: BlockKind::ToolUse,
                                est_tokens: estimate_tokens(args_bytes),
                                bytes: args_bytes,
                                tool: Some(tool_name.clone()),
                                key_hash: Some(key_hash),
                                is_error: false,
                            });
                            if let Some(res_bytes) = result_bytes {
                                pending_input_blocks.push(ContentBlock {
                                    kind: BlockKind::ToolResult,
                                    est_tokens: estimate_tokens(res_bytes),
                                    bytes: res_bytes,
                                    tool: Some(tool_name),
                                    key_hash: None,
                                    is_error,
                                });
                            }
                        }
                        CompactOpencodePartKind::Compaction { summary_bytes } => {
                            pending_compaction = true;
                            if let Some(bytes) = summary_bytes {
                                added.push(ContentBlock {
                                    kind: BlockKind::CompactionSummary,
                                    est_tokens: estimate_tokens(bytes),
                                    bytes,
                                    tool: None,
                                    key_hash: None,
                                    is_error: false,
                                });
                            }
                        }
                        CompactOpencodePartKind::Retry => {
                            pending_api_error = true;
                        }
                        _ => {}
                    }
                }

                turn_num = turn_num.saturating_add(1);
                let cwd_hash = session.cwd_hash.clone().or(msg.path_cwd_hash);
                let hints = TurnHints {
                    resume_start: false,
                    compaction: mem::take(&mut pending_compaction),
                    sidechain,
                    api_error: mem::take(&mut pending_api_error),
                    rate_limit_used_percent: None,
                    context_window_override: None,
                };

                records.push(TurnRecord {
                    agent: AgentId::Opencode,
                    session_id: session.id.clone(),
                    parent_session_id: session.parent_session_id.clone(),
                    turn: turn_num,
                    ts,
                    model,
                    cwd_hash,
                    usage,
                    added,
                    hints,
                });
            }
            other => {
                diagnostics.ignored_records += 1;
                if !other.is_empty() {
                    let safe_key = if is_safe_type_token(other) {
                        format!("role:{other}")
                    } else {
                        "role:<redacted>".to_string()
                    };
                    *diagnostics
                        .unknown_record_types
                        .entry(safe_key)
                        .or_insert(0) += 1;
                }
            }
        }
    }

    OpencodeSessionBatch {
        session_id: session.id.clone(),
        parent_session_id: session.parent_session_id.clone(),
        records,
    }
}

/// Decodes OpenCode sessions, messages, and parts from `opencode.db` strictly read-only.
pub fn parse_opencode_sqlite(db_path: &Path) -> io::Result<OpencodeParseOutput> {
    if !db_path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "opencode.db does not exist",
        ));
    }

    let conn = open_sqlite_readonly(db_path).map_err(sqlite_err_to_io)?;
    if !table_exists(&conn, "session").map_err(sqlite_err_to_io)?
        || !table_exists(&conn, "message").map_err(sqlite_err_to_io)?
    {
        return Ok(OpencodeParseOutput::default());
    }

    let ses_cols = table_columns(&conn, "session").map_err(sqlite_err_to_io)?;
    let has_parent = ses_cols.contains("parent_id");
    let has_dir = ses_cols.contains("directory");
    let has_ver = ses_cols.contains("version");
    let has_created = ses_cols.contains("time_created");
    let has_ses_data = ses_cols.contains("data");

    let parent_expr = if has_parent { "parent_id" } else { "NULL" };
    let dir_expr = if has_dir { "directory" } else { "NULL" };
    let ver_expr = if has_ver { "version" } else { "NULL" };
    let created_expr = if has_created { "time_created" } else { "0" };
    let data_expr = if has_ses_data { "data" } else { "NULL" };
    let order_clause = if has_created {
        "ORDER BY time_created ASC, id ASC"
    } else {
        "ORDER BY id ASC"
    };

    let ses_sql = format!(
        "SELECT id, {parent_expr}, {dir_expr}, {ver_expr}, {created_expr}, {data_expr} FROM session {order_clause}"
    );

    let mut diagnostics = ParseDiagnostics::default();
    let mut sessions_meta = Vec::new();
    {
        let mut ses_stmt = conn.prepare(&ses_sql).map_err(sqlite_err_to_io)?;
        let mut rows = ses_stmt.query([]).map_err(sqlite_err_to_io)?;
        while let Some(row) = rows.next().map_err(sqlite_err_to_io)? {
            let id: String = row.get(0).map_err(sqlite_err_to_io)?;
            let mut parent_id: Option<String> = row.get(1).ok().flatten();
            let mut directory: Option<String> = row.get(2).ok().flatten();
            let mut version: Option<String> = row.get(3).ok().flatten();
            let mut created_ms: i64 = row.get::<_, Option<i64>>(4).ok().flatten().unwrap_or(0);
            let raw_data: Option<String> = row.get(5).ok().flatten();

            if let Some(data_str) = raw_data {
                if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&data_str) {
                    if parent_id.is_none() {
                        parent_id = obj
                            .get("parentID")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                    }
                    if directory.is_none() {
                        directory = obj
                            .get("directory")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                    }
                    if version.is_none() {
                        version = obj
                            .get("version")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                    }
                    if created_ms == 0 {
                        created_ms = obj
                            .get("time")
                            .and_then(Value::as_object)
                            .and_then(|t| t.get("created"))
                            .and_then(Value::as_i64)
                            .unwrap_or(0);
                    }
                }
            }

            if let Some(ver) = version.as_deref().map(str::trim) {
                if is_safe_version_token(ver) {
                    diagnostics.cli_versions.insert(ver.to_string());
                }
            }

            let parent_session_id = parent_id
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let cwd_hash = directory
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(hash_cwd);

            sessions_meta.push(CompactSessionMeta {
                id,
                parent_session_id,
                cwd_hash,
                created_ms,
                created_ts: timestamp_from_millis(created_ms),
            });
        }
    }

    let has_parts_table = table_exists(&conn, "part").map_err(sqlite_err_to_io)?;
    let msg_cols = table_columns(&conn, "message").map_err(sqlite_err_to_io)?;
    let msg_created_expr = if msg_cols.contains("time_created") {
        "time_created"
    } else {
        "NULL"
    };
    let msg_updated_expr = if msg_cols.contains("time_updated") {
        "time_updated"
    } else {
        "NULL"
    };
    let msg_order = if msg_cols.contains("time_created") {
        "ORDER BY time_created ASC, id ASC"
    } else {
        "ORDER BY id ASC"
    };
    let msg_sql = format!(
        "SELECT id, {msg_created_expr}, {msg_updated_expr}, data FROM message WHERE session_id = ?1 {msg_order}"
    );
    let mut msg_stmt = conn.prepare(&msg_sql).map_err(sqlite_err_to_io)?;

    let mut prt_stmt = if has_parts_table {
        let prt_cols = table_columns(&conn, "part").map_err(sqlite_err_to_io)?;
        let prt_created_expr = if prt_cols.contains("time_created") {
            "time_created"
        } else {
            "0"
        };
        let prt_order = if prt_cols.contains("time_created") {
            "ORDER BY time_created ASC, id ASC"
        } else {
            "ORDER BY id ASC"
        };
        let prt_sql = format!(
            "SELECT id, {prt_created_expr}, data FROM part WHERE message_id = ?1 {prt_order}"
        );
        Some(conn.prepare(&prt_sql).map_err(sqlite_err_to_io)?)
    } else {
        None
    };

    let mut batches = Vec::with_capacity(sessions_meta.len());
    for ses in sessions_meta {
        let mut msg_rows = msg_stmt.query([&ses.id]).map_err(sqlite_err_to_io)?;
        let mut messages = Vec::new();

        while let Some(m_row) = msg_rows.next().map_err(sqlite_err_to_io)? {
            let mid: String = m_row.get(0).map_err(sqlite_err_to_io)?;
            let m_created: Option<i64> = m_row.get(1).ok().flatten();
            let m_updated: Option<i64> = m_row.get(2).ok().flatten();
            let raw_data: String = match m_row.get(3) {
                Ok(s) => s,
                Err(_) => {
                    diagnostics.ignored_records += 1;
                    continue;
                }
            };

            let Ok(Value::Object(msg_obj)) = serde_json::from_str::<Value>(&raw_data) else {
                diagnostics.ignored_records += 1;
                diagnostics.issues.push(ParseDiagnostic {
                    line_number: 1,
                    kind: ParseDiagnosticKind::MalformedJson,
                });
                continue;
            };

            let mut compact_msg = parse_compact_message(&mid, m_created, m_updated, &msg_obj);
            drop(msg_obj);
            drop(raw_data);

            if let Some(ref mut p_stmt) = prt_stmt {
                let mut prt_rows = p_stmt.query([&compact_msg.id]).map_err(sqlite_err_to_io)?;
                while let Some(p_row) = prt_rows.next().map_err(sqlite_err_to_io)? {
                    let pid: String = p_row.get(0).map_err(sqlite_err_to_io)?;
                    let p_created: i64 = p_row.get::<_, Option<i64>>(1).ok().flatten().unwrap_or(0);
                    let Ok(p_data) = p_row.get::<_, String>(2) else {
                        continue;
                    };
                    match serde_json::from_str::<Value>(&p_data) {
                        Ok(Value::Object(prt_obj)) => {
                            let part =
                                parse_compact_part(&pid, p_created, &prt_obj, &mut diagnostics);
                            compact_msg.parts.push(part);
                        }
                        Ok(_) | Err(_) => {
                            diagnostics.issues.push(ParseDiagnostic {
                                line_number: 1,
                                kind: ParseDiagnosticKind::MalformedJson,
                            });
                        }
                    }
                }
            }

            messages.push(compact_msg);
        }

        batches.push(build_opencode_session_turns(
            &ses,
            messages,
            &mut diagnostics,
        ));
    }

    Ok(OpencodeParseOutput {
        sessions: batches,
        diagnostics,
    })
}

fn read_json_object_readonly(
    path: &Path,
    diagnostics: &mut ParseDiagnostics,
) -> io::Result<Option<Map<String, Value>>> {
    let file = open_readonly(path)?;
    let mut raw_bytes = Vec::new();
    file.take(MAX_OPENCODE_JSON_FILE_BYTES + 1)
        .read_to_end(&mut raw_bytes)?;

    if raw_bytes.len() as u64 > MAX_OPENCODE_JSON_FILE_BYTES {
        diagnostics.issues.push(ParseDiagnostic {
            line_number: 1,
            kind: ParseDiagnosticKind::OversizedLine {
                byte_len: raw_bytes.len() as u64,
                limit: usize::try_from(MAX_OPENCODE_JSON_FILE_BYTES).unwrap_or(usize::MAX),
            },
        });
        raw_bytes.clear();
        return Ok(None);
    }

    let Ok(raw_str) = std::str::from_utf8(&raw_bytes) else {
        diagnostics.issues.push(ParseDiagnostic {
            line_number: 1,
            kind: ParseDiagnosticKind::InvalidUtf8,
        });
        raw_bytes.clear();
        return Ok(None);
    };

    let parsed = match serde_json::from_str::<Value>(raw_str) {
        Ok(Value::Object(map)) => Some(map),
        Ok(_) | Err(_) => {
            diagnostics.ignored_records += 1;
            diagnostics.issues.push(ParseDiagnostic {
                line_number: 1,
                kind: ParseDiagnosticKind::MalformedJson,
            });
            None
        }
    };
    raw_bytes.clear();
    Ok(parsed)
}

pub(crate) fn collect_json_files_recursive(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_json_files_recursive(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
            out.push(path);
        }
    }
    Ok(())
}

/// Decodes OpenCode sessions from the legacy `<opencode_root>/storage/{session,message,part}/`
/// JSON file tree strictly read-only.
pub fn parse_opencode_legacy_storage(storage_dir: &Path) -> io::Result<OpencodeParseOutput> {
    let session_root = storage_dir.join("session");
    if !session_root.is_dir() {
        return Ok(OpencodeParseOutput::default());
    }

    let mut session_files = Vec::new();
    collect_json_files_recursive(&session_root, &mut session_files)?;
    session_files.sort();

    let mut diagnostics = ParseDiagnostics::default();
    let mut sessions_meta = Vec::with_capacity(session_files.len());

    for ses_file in session_files {
        let Some(obj) = read_json_object_readonly(&ses_file, &mut diagnostics)? else {
            continue;
        };
        let fallback_id = ses_file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        let id = obj
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(fallback_id)
            .to_string();
        let parent_session_id = obj
            .get("parentID")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let cwd_hash = obj
            .get("directory")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(hash_cwd);
        if let Some(ver) = obj.get("version").and_then(Value::as_str).map(str::trim) {
            if is_safe_version_token(ver) {
                diagnostics.cli_versions.insert(ver.to_string());
            }
        }
        let created_ms = obj
            .get("time")
            .and_then(Value::as_object)
            .and_then(|t| t.get("created"))
            .and_then(Value::as_i64)
            .unwrap_or(0);

        sessions_meta.push(CompactSessionMeta {
            id,
            parent_session_id,
            cwd_hash,
            created_ms,
            created_ts: timestamp_from_millis(created_ms),
        });
    }

    sessions_meta.sort_by(|a, b| {
        a.created_ms
            .cmp(&b.created_ms)
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut batches = Vec::with_capacity(sessions_meta.len());
    for ses in sessions_meta {
        let msg_dir = storage_dir.join("message").join(&ses.id);
        let mut msg_files = Vec::new();
        collect_json_files_recursive(&msg_dir, &mut msg_files)?;
        msg_files.sort();

        let mut messages = Vec::with_capacity(msg_files.len());
        for msg_file in msg_files {
            let Some(msg_obj) = read_json_object_readonly(&msg_file, &mut diagnostics)? else {
                continue;
            };
            let fallback_mid = msg_file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("msg");
            let mut compact_msg = parse_compact_message(fallback_mid, None, None, &msg_obj);
            drop(msg_obj);

            let prt_dir = storage_dir.join("part").join(&compact_msg.id);
            let mut prt_files = Vec::new();
            collect_json_files_recursive(&prt_dir, &mut prt_files)?;
            prt_files.sort();

            for (idx, prt_file) in prt_files.into_iter().enumerate() {
                let Some(prt_obj) = read_json_object_readonly(&prt_file, &mut diagnostics)? else {
                    continue;
                };
                let fallback_pid = prt_file
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("prt");
                let sort_ms = compact_msg
                    .sort_ms
                    .saturating_add(i64::try_from(idx).unwrap_or(0));
                let part = parse_compact_part(fallback_pid, sort_ms, &prt_obj, &mut diagnostics);
                compact_msg.parts.push(part);
            }

            messages.push(compact_msg);
        }

        batches.push(build_opencode_session_turns(
            &ses,
            messages,
            &mut diagnostics,
        ));
    }

    Ok(OpencodeParseOutput {
        sessions: batches,
        diagnostics,
    })
}

/// Decodes OpenCode sessions from `<opencode_root>` across both `opencode.db` and
/// legacy `storage/`, deduplicating shared `session_id`s in favor of `opencode.db`.
pub fn parse_opencode_root(opencode_root: &Path) -> io::Result<OpencodeParseOutput> {
    if !opencode_root.is_dir() {
        return Ok(OpencodeParseOutput::default());
    }

    let db_path = opencode_root.join("opencode.db");
    let storage_dir = opencode_root.join("storage");

    let mut out = if db_path.is_file() {
        parse_opencode_sqlite(&db_path)?
    } else {
        OpencodeParseOutput::default()
    };

    if storage_dir.is_dir() {
        let seen_ids: BTreeSet<String> =
            out.sessions.iter().map(|b| b.session_id.clone()).collect();
        let legacy = parse_opencode_legacy_storage(&storage_dir)?;
        for batch in legacy.sessions {
            if !seen_ids.contains(&batch.session_id) {
                out.sessions.push(batch);
            }
        }
        out.diagnostics.ignored_records = out
            .diagnostics
            .ignored_records
            .saturating_add(legacy.diagnostics.ignored_records);
        out.diagnostics
            .cli_versions
            .extend(legacy.diagnostics.cli_versions);
        for (k, v) in legacy.diagnostics.unknown_record_types {
            *out.diagnostics.unknown_record_types.entry(k).or_insert(0) += v;
        }
        out.diagnostics.issues.extend(legacy.diagnostics.issues);
    }

    Ok(out)
}
