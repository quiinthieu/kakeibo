//! Strictly read-only Cursor global storage SQLite (`state.vscdb`) `cursorDiskKV`
//! decoder (`PLAN.md` §0 #3–#6, §5, §10 T11, `docs/formats/cursor.md`).
//!
//! Because Cursor's local SQLite schema is undocumented and unversioned, support is
//! best effort:
//! - Zero or missing `tokenCount` (`inputTokens == 0 && outputTokens == 0`) emits
//!   [`Usage::not_recorded`] ([`Confidence::NotRecorded`]) so human-readable views
//!   render `"not recorded"` (never `"0"`).
//! - Positive `tokenCount` (`inputTokens > 0 || outputTokens > 0`) emits
//!   [`Confidence::Estimated`] (`estimation_method = "cursor_bubble_token_count"`).
//! - All raw prompt, response, thinking, and tool text is measured/hashed and dropped
//!   immediately.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::mem;
use std::path::Path;

use kakeibo_core::{
    AgentId, BlockKind, Confidence, ContentBlock, DisjointUsageInput, Timestamp, TurnHints,
    TurnRecord, Usage,
};
use rusqlite::types::ValueRef;
use serde_json::{Map, Value};

use crate::hash::{
    canonical_json_byte_len, estimate_tokens, hash_codex_tool_arguments, hash_cwd,
    measure_and_hash_tool_input,
};
use crate::line_reader::{ParseDiagnostic, ParseDiagnosticKind, ParseDiagnostics};
use crate::sqlite::{open_sqlite_readonly, sqlite_err_to_io, table_exists};

const EST_METHOD_CURSOR_BUBBLE: &str = "cursor_bubble_token_count";

/// Decoded [`TurnRecord`] batch for a single Cursor composer thread.
#[derive(Debug, Clone, PartialEq)]
pub struct CursorSessionBatch {
    pub session_id: String,
    pub records: Vec<TurnRecord>,
}

/// Decoded composer sessions and non-fatal diagnostics produced from a Cursor `state.vscdb`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CursorParseOutput {
    pub sessions: Vec<CursorSessionBatch>,
    pub diagnostics: ParseDiagnostics,
}

impl CursorParseOutput {
    /// Flattens all composer session batches into a single [`TurnRecord`] vector.
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

fn unix_epoch_ts() -> Timestamp {
    Timestamp::from_unix_timestamp(0).expect("unix epoch timestamp")
}

fn timestamp_from_millis(ms: i64) -> Option<Timestamp> {
    let nanos = i128::from(ms).checked_mul(1_000_000)?;
    Timestamp::from_unix_timestamp_nanos(nanos).ok()
}

fn parse_flexible_timestamp(val: Option<&Value>) -> (i64, Option<Timestamp>) {
    let Some(val) = val else {
        return (0, None);
    };
    match val {
        Value::Number(num) => {
            if let Some(ms) = num.as_i64() {
                (ms, timestamp_from_millis(ms))
            } else if let Some(ms_f) = num.as_f64() {
                if ms_f.is_finite() {
                    let ms = ms_f as i64;
                    (ms, timestamp_from_millis(ms))
                } else {
                    (0, None)
                }
            } else {
                (0, None)
            }
        }
        Value::String(s) => {
            let trimmed = s.trim();
            if let Ok(ts) = Timestamp::parse_rfc3339(trimmed) {
                let ms = ts.unix_timestamp().saturating_mul(1_000);
                (ms, Some(ts))
            } else if let Ok(ms) = trimmed.parse::<i64>() {
                (ms, timestamp_from_millis(ms))
            } else {
                (0, None)
            }
        }
        _ => (0, None),
    }
}

/// Normalises a `file://` URI or plain workspace path string into a clean filesystem path.
pub(crate) fn normalise_cursor_workspace_uri(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let stripped = if let Some(after_scheme) = trimmed.strip_prefix("file://") {
        let bytes = after_scheme.as_bytes();
        // Handle Windows `file:///C:/...` -> `C:/...` while keeping Unix `/path/...`.
        if bytes.len() >= 3 && bytes[0] == b'/' && bytes[2] == b':' {
            &after_scheme[1..]
        } else {
            after_scheme
        }
    } else {
        trimmed
    };
    let clean = stripped.trim();
    (!clean.is_empty()).then(|| clean.to_string())
}

fn extract_raw_workspace_from_composer(obj: &Map<String, Value>) -> Option<String> {
    for key in ["workspaceUri", "workspacePath", "cwd"] {
        if let Some(s) = obj.get(key).and_then(Value::as_str) {
            if let Some(norm) = normalise_cursor_workspace_uri(s) {
                return Some(norm);
            }
        }
    }
    None
}

#[derive(Debug, Default)]
struct CompactComposerMeta {
    created_ms: i64,
    created_ts: Option<Timestamp>,
    default_model: Option<String>,
    cwd_hash: Option<String>,
    header_order: Vec<String>,
}

#[derive(Debug)]
struct CompactToolInvocation {
    name: String,
    args_bytes: u64,
    key_hash: String,
    result_bytes: Option<u64>,
    is_error: bool,
}

#[derive(Debug)]
struct CompactBubble {
    bubble_id: String,
    bubble_type: u64,
    created_ms: i64,
    created_ts: Option<Timestamp>,
    model: Option<String>,
    text_bytes: u64,
    thinking_bytes: u64,
    tool: Option<CompactToolInvocation>,
    input_tokens: u64,
    output_tokens: u64,
}

fn parse_composer_meta(obj: &Map<String, Value>) -> CompactComposerMeta {
    let (created_ms, created_ts) =
        parse_flexible_timestamp(obj.get("createdAt").or_else(|| obj.get("lastUpdatedAt")));
    let default_model = obj
        .get("modelConfig")
        .and_then(Value::as_object)
        .and_then(|mc| mc.get("modelName"))
        .or_else(|| obj.get("model"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let cwd_hash = extract_raw_workspace_from_composer(obj).map(|cwd| hash_cwd(&cwd));

    let mut header_order = Vec::new();
    if let Some(headers) = obj
        .get("fullConversationHeadersOnly")
        .and_then(Value::as_array)
    {
        for item in headers {
            if let Some(bid) = item
                .as_object()
                .and_then(|m| m.get("bubbleId"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                header_order.push(bid.to_string());
            }
        }
    }

    CompactComposerMeta {
        created_ms,
        created_ts,
        default_model,
        cwd_hash,
        header_order,
    }
}

fn parse_compact_tool(tfd: &Map<String, Value>) -> Option<CompactToolInvocation> {
    let name_opt = tfd
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let has_payload =
        tfd.contains_key("params") || tfd.contains_key("rawArgs") || tfd.contains_key("result");
    if name_opt.is_none() && !has_payload {
        return None;
    }
    let name = name_opt.unwrap_or("unknown").to_string();

    let (args_bytes, key_hash) = match tfd.get("params") {
        Some(v @ (Value::Object(_) | Value::Array(_))) => measure_and_hash_tool_input(&name, v),
        Some(Value::String(s)) if !s.is_empty() => hash_codex_tool_arguments(&name, s),
        _ => match tfd.get("rawArgs").and_then(Value::as_str) {
            Some(raw) if !raw.is_empty() => hash_codex_tool_arguments(&name, raw),
            _ => measure_and_hash_tool_input(&name, &Value::Object(Map::new())),
        },
    };

    let result_bytes = match tfd.get("result") {
        Some(Value::String(s)) if !s.is_empty() => Some(s.len() as u64),
        Some(v @ (Value::Object(_) | Value::Array(_))) => Some(canonical_json_byte_len(v)),
        _ => None,
    };

    let is_error = tfd
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|st| matches!(st, "error" | "failed"));

    Some(CompactToolInvocation {
        name,
        args_bytes,
        key_hash,
        result_bytes,
        is_error,
    })
}

fn parse_compact_bubble(bubble_id: &str, obj: &Map<String, Value>) -> CompactBubble {
    let bubble_type = obj.get("type").and_then(Value::as_u64).unwrap_or(0);
    let (created_ms, created_ts) = parse_flexible_timestamp(obj.get("createdAt"));

    let model = obj
        .get("modelInfo")
        .and_then(Value::as_object)
        .and_then(|mi| mi.get("modelName"))
        .or_else(|| obj.get("modelName"))
        .or_else(|| obj.get("model"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let text_bytes = obj
        .get("text")
        .or_else(|| obj.get("rawText"))
        .and_then(Value::as_str)
        .map_or(0, |s| s.len() as u64);

    let thinking_bytes = match obj.get("thinking") {
        Some(Value::String(s)) => s.len() as u64,
        Some(Value::Object(m)) => m
            .get("text")
            .and_then(Value::as_str)
            .map_or(0, |s| s.len() as u64),
        _ => 0,
    };

    let tool = obj
        .get("toolFormerData")
        .and_then(Value::as_object)
        .and_then(parse_compact_tool);

    let (input_tokens, output_tokens) = match obj.get("tokenCount").and_then(Value::as_object) {
        Some(tc) => (
            tc.get("inputTokens").and_then(Value::as_u64).unwrap_or(0),
            tc.get("outputTokens").and_then(Value::as_u64).unwrap_or(0),
        ),
        None => (0, 0),
    };

    CompactBubble {
        bubble_id: bubble_id.to_string(),
        bubble_type,
        created_ms,
        created_ts,
        model,
        text_bytes,
        thinking_bytes,
        tool,
        input_tokens,
        output_tokens,
    }
}

fn build_composer_batch(
    composer_id: String,
    meta: CompactComposerMeta,
    mut bubbles: BTreeMap<String, CompactBubble>,
    diagnostics: &mut ParseDiagnostics,
) -> Option<CursorSessionBatch> {
    let mut ordered: Vec<CompactBubble> = Vec::with_capacity(bubbles.len());
    for header_bid in &meta.header_order {
        if let Some(b) = bubbles.remove(header_bid) {
            ordered.push(b);
        }
    }
    if !bubbles.is_empty() {
        let mut remaining: Vec<CompactBubble> = bubbles.into_values().collect();
        remaining.sort_by(|a, b| {
            a.created_ms
                .cmp(&b.created_ms)
                .then_with(|| a.bubble_id.cmp(&b.bubble_id))
        });
        ordered.extend(remaining);
    }

    let mut records = Vec::new();
    let mut pending_input_blocks: Vec<ContentBlock> = Vec::new();
    let mut last_ts = meta.created_ts.unwrap_or_else(unix_epoch_ts);
    let mut turn_num: u32 = 0;

    for bubble in ordered {
        if let Some(bts) = bubble.created_ts {
            if bts >= last_ts {
                last_ts = bts;
            }
        }
        let ts = bubble.created_ts.unwrap_or(last_ts);

        match bubble.bubble_type {
            1 => {
                if bubble.text_bytes > 0 {
                    pending_input_blocks.push(ContentBlock {
                        kind: BlockKind::UserPrompt,
                        tool: None,
                        bytes: bubble.text_bytes,
                        est_tokens: estimate_tokens(bubble.text_bytes),
                        key_hash: None,
                        is_error: false,
                    });
                }
            }
            2 => {
                let has_tokens = bubble.input_tokens > 0 || bubble.output_tokens > 0;
                let has_assistant_body =
                    bubble.text_bytes > 0 || bubble.thinking_bytes > 0 || bubble.tool.is_some();
                if !has_tokens && !has_assistant_body && pending_input_blocks.is_empty() {
                    continue;
                }

                let usage = if has_tokens {
                    match Usage::from_disjoint(DisjointUsageInput {
                        input_tokens: bubble.input_tokens,
                        cache_write_tokens: 0,
                        cache_read_tokens: 0,
                        output_tokens: bubble.output_tokens,
                        reasoning_tokens: None,
                        confidence: Confidence::Estimated,
                        estimation_method: Some(EST_METHOD_CURSOR_BUBBLE.to_string()),
                        cache_write_split: None,
                        agent_reported_cost_usd: None,
                    }) {
                        Ok(u) => u,
                        Err(_) => {
                            diagnostics.issues.push(ParseDiagnostic {
                                line_number: 0,
                                kind: ParseDiagnosticKind::UsageOverflow,
                            });
                            continue;
                        }
                    }
                } else {
                    Usage {
                        fresh_in: 0,
                        cache_write: 0,
                        cache_read: 0,
                        output: 0,
                        reasoning: None,
                        confidence: Confidence::NotRecorded,
                        estimation_method: None,
                        cache_write_split: None,
                        agent_reported_cost_usd: None,
                    }
                };

                let mut added = mem::take(&mut pending_input_blocks);
                if bubble.thinking_bytes > 0 {
                    added.push(ContentBlock {
                        kind: BlockKind::AssistantThinking,
                        tool: None,
                        bytes: bubble.thinking_bytes,
                        est_tokens: estimate_tokens(bubble.thinking_bytes),
                        key_hash: None,
                        is_error: false,
                    });
                }
                if bubble.text_bytes > 0 {
                    added.push(ContentBlock {
                        kind: BlockKind::AssistantText,
                        tool: None,
                        bytes: bubble.text_bytes,
                        est_tokens: estimate_tokens(bubble.text_bytes),
                        key_hash: None,
                        is_error: false,
                    });
                }
                if let Some(tool) = bubble.tool {
                    added.push(ContentBlock {
                        kind: BlockKind::ToolUse,
                        tool: Some(tool.name.clone()),
                        bytes: tool.args_bytes,
                        est_tokens: estimate_tokens(tool.args_bytes),
                        key_hash: Some(tool.key_hash),
                        is_error: false,
                    });
                    if let Some(res_bytes) = tool.result_bytes {
                        pending_input_blocks.push(ContentBlock {
                            kind: BlockKind::ToolResult,
                            tool: Some(tool.name),
                            bytes: res_bytes,
                            est_tokens: estimate_tokens(res_bytes),
                            key_hash: None,
                            is_error: tool.is_error,
                        });
                    }
                }

                turn_num = turn_num.saturating_add(1);
                let model = bubble.model.or_else(|| meta.default_model.clone());

                records.push(TurnRecord {
                    agent: AgentId::Cursor,
                    session_id: composer_id.clone(),
                    parent_session_id: None,
                    turn: turn_num,
                    ts,
                    model,
                    cwd_hash: meta.cwd_hash.clone(),
                    usage,
                    added,
                    hints: TurnHints::default(),
                });
            }
            _ => {
                diagnostics.ignored_records += 1;
            }
        }
    }

    (!records.is_empty()).then_some(CursorSessionBatch {
        session_id: composer_id,
        records,
    })
}

fn value_ref_bytes<'a>(vref: ValueRef<'a>) -> Option<&'a [u8]> {
    match vref {
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => Some(bytes),
        ValueRef::Null | ValueRef::Integer(_) | ValueRef::Real(_) => None,
    }
}

/// Decodes Cursor composer threads and bubbles from a `state.vscdb` SQLite database
/// strictly read-only (`PLAN.md` §5, §10 T11).
pub fn parse_cursor_vscdb(db_path: &Path) -> io::Result<CursorParseOutput> {
    if !db_path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Cursor state.vscdb does not exist",
        ));
    }

    let conn = open_sqlite_readonly(db_path).map_err(sqlite_err_to_io)?;
    if !table_exists(&conn, "cursorDiskKV").map_err(sqlite_err_to_io)? {
        return Ok(CursorParseOutput::default());
    }

    let mut output = CursorParseOutput::default();
    let mut composers: BTreeMap<String, CompactComposerMeta> = BTreeMap::new();
    let mut bubbles_by_composer: BTreeMap<String, BTreeMap<String, CompactBubble>> =
        BTreeMap::new();

    let mut stmt = conn
        .prepare("SELECT key, value FROM cursorDiskKV")
        .map_err(sqlite_err_to_io)?;
    let mut rows = stmt.query([]).map_err(sqlite_err_to_io)?;
    let mut row_num: usize = 0;

    while let Some(row) = rows.next().map_err(sqlite_err_to_io)? {
        row_num += 1;
        let Ok(key_ref) = row.get_ref(0) else {
            output.diagnostics.ignored_records += 1;
            continue;
        };
        let Some(key_bytes) = value_ref_bytes(key_ref) else {
            output.diagnostics.ignored_records += 1;
            continue;
        };
        let Ok(key) = std::str::from_utf8(key_bytes) else {
            output.diagnostics.issues.push(ParseDiagnostic {
                line_number: row_num,
                kind: ParseDiagnosticKind::InvalidUtf8,
            });
            continue;
        };

        if let Some(raw_comp_id) = key.strip_prefix("composerData:") {
            let comp_key_id = raw_comp_id.trim();
            if comp_key_id.is_empty() {
                output.diagnostics.ignored_records += 1;
                continue;
            }
            let Some(val_bytes) = row.get_ref(1).ok().and_then(value_ref_bytes) else {
                output.diagnostics.ignored_records += 1;
                continue;
            };
            let Ok(val) = serde_json::from_slice::<Value>(val_bytes) else {
                output.diagnostics.issues.push(ParseDiagnostic {
                    line_number: row_num,
                    kind: ParseDiagnosticKind::MalformedJson,
                });
                continue;
            };
            let Some(obj) = val.as_object() else {
                output.diagnostics.ignored_records += 1;
                continue;
            };
            let comp_id = obj
                .get("composerId")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(comp_key_id)
                .to_string();
            composers.insert(comp_id, parse_composer_meta(obj));
            continue;
        }

        if let Some(rest) = key.strip_prefix("bubbleId:") {
            let Some((comp_id, bubble_id)) = rest.split_once(':') else {
                output.diagnostics.ignored_records += 1;
                continue;
            };
            let comp_id = comp_id.trim();
            let bubble_id = bubble_id.trim();
            if comp_id.is_empty() || bubble_id.is_empty() {
                output.diagnostics.ignored_records += 1;
                continue;
            }
            let Some(val_bytes) = row.get_ref(1).ok().and_then(value_ref_bytes) else {
                output.diagnostics.ignored_records += 1;
                continue;
            };
            let Ok(val) = serde_json::from_slice::<Value>(val_bytes) else {
                output.diagnostics.issues.push(ParseDiagnostic {
                    line_number: row_num,
                    kind: ParseDiagnosticKind::MalformedJson,
                });
                continue;
            };
            let Some(obj) = val.as_object() else {
                output.diagnostics.ignored_records += 1;
                continue;
            };
            let compact = parse_compact_bubble(bubble_id, obj);
            bubbles_by_composer
                .entry(comp_id.to_string())
                .or_default()
                .insert(bubble_id.to_string(), compact);
            continue;
        }

        output.diagnostics.ignored_records += 1;
    }

    let mut all_composer_ids: BTreeSet<String> = composers.keys().cloned().collect();
    all_composer_ids.extend(bubbles_by_composer.keys().cloned());

    let mut sortable: Vec<(i64, String, CursorSessionBatch)> = Vec::new();
    for cid in all_composer_ids {
        let meta = composers.remove(&cid).unwrap_or_default();
        let created_ms = meta.created_ms;
        let bubbles = bubbles_by_composer.remove(&cid).unwrap_or_default();
        if let Some(batch) =
            build_composer_batch(cid.clone(), meta, bubbles, &mut output.diagnostics)
        {
            sortable.push((created_ms, cid, batch));
        }
    }
    sortable.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    output.sessions = sortable.into_iter().map(|(_, _, b)| b).collect();

    Ok(output)
}

/// Reads `composerData:*` rows from `state.vscdb` strictly read-only to map
/// `composer_id -> raw_cwd` exclusively for opt-in `export --show-paths`.
pub(crate) fn extract_cursor_cwds_from_vscdb(
    db_path: &Path,
) -> io::Result<BTreeMap<String, String>> {
    if !db_path.is_file() {
        return Ok(BTreeMap::new());
    }
    let conn = open_sqlite_readonly(db_path).map_err(sqlite_err_to_io)?;
    if !table_exists(&conn, "cursorDiskKV").map_err(sqlite_err_to_io)? {
        return Ok(BTreeMap::new());
    }

    let mut out = BTreeMap::new();
    let mut stmt = conn
        .prepare("SELECT key, value FROM cursorDiskKV WHERE key LIKE 'composerData:%'")
        .map_err(sqlite_err_to_io)?;
    let mut rows = stmt.query([]).map_err(sqlite_err_to_io)?;

    while let Some(row) = rows.next().map_err(sqlite_err_to_io)? {
        let Some(key_bytes) = row.get_ref(0).ok().and_then(value_ref_bytes) else {
            continue;
        };
        let Ok(key) = std::str::from_utf8(key_bytes) else {
            continue;
        };
        let Some(raw_comp_id) = key.strip_prefix("composerData:") else {
            continue;
        };
        let Some(val_bytes) = row.get_ref(1).ok().and_then(value_ref_bytes) else {
            continue;
        };
        let Ok(val) = serde_json::from_slice::<Value>(val_bytes) else {
            continue;
        };
        let Some(obj) = val.as_object() else {
            continue;
        };
        let comp_id = obj
            .get("composerId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| raw_comp_id.trim())
            .to_string();
        if !comp_id.is_empty() {
            if let Some(cwd) = extract_raw_workspace_from_composer(obj) {
                out.insert(comp_id, cwd);
            }
        }
    }

    Ok(out)
}
