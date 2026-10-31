//! Tolerant, zero-text-retention streaming JSONL and `.jsonl.zst` decoder for OpenAI Codex CLI (`PLAN.md` §5, §7, §10 T05).

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader};
use std::mem;
use std::path::Path;

use kakeibo_core::{
    AgentId, BlockKind, Confidence, ContentBlock, InclusiveUsageInput, Timestamp, TurnHints,
    TurnRecord, Usage,
};
use serde::Deserialize;

use crate::catalog::open_readonly;
use crate::hash::{
    canonical_json_byte_len, estimate_tokens, hash_codex_tool_arguments, hash_cwd,
    measure_and_hash_tool_input,
};
use crate::line_reader::{
    BoundedLineReader, LineReadOutcome, ParseDiagnostic, ParseDiagnosticKind, ParseDiagnostics,
    DEFAULT_MAX_LINE_BYTES,
};

/// Zstandard frame magic header (`0xFD2FB528` little-endian).
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// Known non-turn top-level record types observed on `codex-cli 0.165.0` (`docs/formats/codex.md`).
const KNOWN_NON_TURN_TOP_TYPES: &[&str] = &[
    "compacted",
    "inter_agent_communication_metadata",
    "world_state",
];

fn is_safe_type_token(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 64
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn is_safe_version_token(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 64
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+' | b'_'))
}

/// Result of parsing a single Codex CLI rollout stream.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CodexParseOutput {
    pub records: Vec<TurnRecord>,
    pub diagnostics: ParseDiagnostics,
}

#[derive(Debug, Deserialize)]
struct RawEnvelope<'a> {
    #[serde(rename = "type")]
    record_type: Option<&'a str>,
    #[serde(default)]
    ordinal: Option<u64>,
    #[serde(default)]
    timestamp: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
struct RawSessionMetaLine {
    #[serde(default)]
    payload: Option<RawSessionMetaPayload>,
}

#[derive(Debug, Deserialize)]
struct RawSessionMetaPayload {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    cli_version: Option<String>,
    #[serde(default)]
    parent_thread_id: Option<String>,
    #[serde(default)]
    forked_from_id: Option<String>,
    #[serde(default)]
    subagent_history_start_ordinal: Option<u64>,
    #[serde(default)]
    source: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct RawTurnContextLine {
    #[serde(default)]
    payload: Option<RawTurnContextPayload>,
}

#[derive(Debug, Deserialize)]
struct RawTurnContextPayload {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawResponseItemLine {
    #[serde(default)]
    payload: Option<RawResponseItemPayload>,
}

#[derive(Debug, Deserialize)]
struct RawResponseItemPayload {
    #[serde(rename = "type")]
    item_type: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    content: Option<serde_json::Value>,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<serde_json::Value>,
    #[serde(default)]
    input: Option<String>,
    #[serde(default)]
    output: Option<serde_json::Value>,
    #[serde(default)]
    action: Option<serde_json::Value>,
    #[serde(default)]
    tools: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
struct RawCodexTokenUsage {
    input_tokens: u64,
    #[serde(default)]
    cached_input_tokens: u64,
    #[serde(default)]
    cache_write_input_tokens: u64,
    output_tokens: u64,
    #[serde(default)]
    reasoning_output_tokens: Option<u64>,
}

impl RawCodexTokenUsage {
    fn same_counts(self, other: Self) -> bool {
        self.input_tokens == other.input_tokens
            && self.cached_input_tokens == other.cached_input_tokens
            && self.cache_write_input_tokens == other.cache_write_input_tokens
            && self.output_tokens == other.output_tokens
            && self.reasoning_output_tokens == other.reasoning_output_tokens
    }
}

#[derive(Debug, Deserialize)]
struct RawTokenUsageRecordLine {
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    payload: Option<RawTokenUsageRecordPayload>,
}

#[derive(Debug, Deserialize)]
struct RawTokenUsageRecordPayload {
    #[serde(default)]
    response_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    usage: Option<RawCodexTokenUsage>,
    #[serde(default)]
    thread_token_usage: Option<RawCodexTokenUsage>,
}

#[derive(Debug, Deserialize)]
struct RawEventMsgLine {
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    payload: Option<RawEventMsgPayload>,
}

#[derive(Debug, Deserialize)]
struct RawEventMsgPayload {
    #[serde(rename = "type")]
    event_type: Option<String>,
    #[serde(default)]
    info: Option<RawTokenCountInfo>,
    #[serde(default)]
    rate_limits: Option<RawRateLimits>,
}

#[derive(Debug, Deserialize)]
struct RawTokenCountInfo {
    #[serde(default)]
    model_context_window: Option<u64>,
    #[serde(default)]
    last_token_usage: Option<RawCodexTokenUsage>,
    #[serde(default)]
    total_token_usage: Option<RawCodexTokenUsage>,
}

#[derive(Debug, Deserialize)]
struct RawRateLimits {
    #[serde(default)]
    primary: Option<RawRateLimitWindow>,
}

#[derive(Debug, Deserialize)]
struct RawRateLimitWindow {
    #[serde(default)]
    used_percent: Option<f64>,
}

#[derive(Debug)]
struct PendingTcFallback {
    ts: Timestamp,
    usage: Usage,
}

#[derive(Debug, Default)]
struct CodexSessionState {
    session_id: Option<String>,
    parent_session_id: Option<String>,
    current_model: Option<String>,
    current_cwd_hash: Option<String>,
    session_is_sidechain: bool,
    has_inherited_parent: bool,
    saw_first_turn_context: bool,
    fork_cutoff_ordinal: Option<u64>,

    pending_resume_start: bool,
    pending_compaction: bool,
    pending_api_error: bool,
    pending_rate_limit_used_percent: Option<f64>,
    pending_context_window_override: Option<u64>,
    pending_added_blocks: Vec<ContentBlock>,

    seen_item_ids: HashSet<String>,
    seen_response_ids: HashSet<String>,
    tool_registry: HashMap<String, (String, String)>,

    saw_any_tur: bool,
    last_turn_from_tc_upgradeable: bool,
    last_turn_open_for_trailing_tc: bool,
    pending_tc_fallback: Option<PendingTcFallback>,
    prev_total_usage: Option<RawCodexTokenUsage>,

    records: Vec<TurnRecord>,
}

impl CodexSessionState {
    fn is_inherited_history(&self, top_ordinal: Option<u64>) -> bool {
        let by_pre_ctx = self.has_inherited_parent && !self.saw_first_turn_context;
        let by_ordinal = match (self.fork_cutoff_ordinal, top_ordinal) {
            (Some(cutoff), Some(ord)) => ord < cutoff,
            _ => false,
        };
        by_pre_ctx || by_ordinal
    }

    fn flush_pending_tc_fallback(&mut self) {
        let Some(fb) = self.pending_tc_fallback.take() else {
            return;
        };
        let Some(sid) = self.session_id.clone() else {
            return;
        };
        self.emit_turn(sid, fb.ts, fb.usage, false, false);
    }

    fn emit_turn(
        &mut self,
        session_id: String,
        ts: Timestamp,
        usage: Usage,
        open_for_trailing_tc: bool,
        from_tc_upgradeable: bool,
    ) {
        let turn_num = u32::try_from(self.records.len() + 1).unwrap_or(u32::MAX);
        let hints = TurnHints {
            resume_start: mem::replace(&mut self.pending_resume_start, false),
            compaction: mem::replace(&mut self.pending_compaction, false),
            sidechain: self.session_is_sidechain,
            api_error: mem::replace(&mut self.pending_api_error, false),
            rate_limit_used_percent: self.pending_rate_limit_used_percent.take(),
            context_window_override: self.pending_context_window_override.take(),
        };
        let added = mem::take(&mut self.pending_added_blocks);

        self.records.push(TurnRecord {
            agent: AgentId::Codex,
            session_id,
            parent_session_id: self.parent_session_id.clone(),
            turn: turn_num,
            ts,
            model: self.current_model.clone(),
            cwd_hash: self.current_cwd_hash.clone(),
            usage,
            added,
            hints,
        });

        self.last_turn_open_for_trailing_tc = open_for_trailing_tc;
        self.last_turn_from_tc_upgradeable = from_tc_upgradeable;
    }
}

fn normalise_codex_usage(raw: RawCodexTokenUsage) -> Usage {
    let (usage, _was_clamped) = Usage::from_inclusive_clamped(InclusiveUsageInput {
        input_tokens: raw.input_tokens,
        cache_write_tokens: raw.cache_write_input_tokens,
        cache_read_tokens: raw.cached_input_tokens,
        output_tokens: raw.output_tokens,
        reasoning_tokens: raw.reasoning_output_tokens,
        confidence: Confidence::Measured,
        estimation_method: None,
        agent_reported_cost_usd: None,
    });
    usage
}

fn source_is_subagent(source: Option<&serde_json::Value>) -> bool {
    match source {
        Some(serde_json::Value::String(s)) => s == "subagent",
        Some(serde_json::Value::Object(map)) => map.contains_key("subagent"),
        _ => false,
    }
}

fn measure_content_value_bytes(val: &serde_json::Value) -> u64 {
    match val {
        serde_json::Value::Null => 0,
        serde_json::Value::String(s) => s.len() as u64,
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| {
                if let Some(text) = item.get("text").and_then(serde_json::Value::as_str) {
                    text.len() as u64
                } else {
                    canonical_json_byte_len(item)
                }
            })
            .sum(),
        other => canonical_json_byte_len(other),
    }
}

fn extract_response_item(
    payload: RawResponseItemPayload,
    is_inherited_history: bool,
    state: &mut CodexSessionState,
) {
    if let Some(ref item_id) = payload.id {
        if !item_id.is_empty() && !state.seen_item_ids.insert(item_id.clone()) {
            return;
        }
    }

    if !is_inherited_history {
        state.flush_pending_tc_fallback();
        state.last_turn_open_for_trailing_tc = false;
        state.last_turn_from_tc_upgradeable = false;
    }

    let Some(item_type) = payload.item_type.as_deref() else {
        return;
    };

    match item_type {
        "message" => {
            if is_inherited_history {
                return;
            }
            let kind = match payload.role.as_deref() {
                Some("assistant") => BlockKind::AssistantText,
                Some("developer" | "system") => BlockKind::SystemContext,
                _ => BlockKind::UserPrompt,
            };
            let Some(content) = payload.content else {
                return;
            };
            match content {
                serde_json::Value::String(s) => {
                    let bytes = s.len() as u64;
                    state.pending_added_blocks.push(ContentBlock {
                        kind,
                        est_tokens: estimate_tokens(bytes),
                        bytes,
                        tool: None,
                        key_hash: None,
                        is_error: false,
                    });
                }
                serde_json::Value::Array(parts) => {
                    for part in parts {
                        if let Some(text) = part.get("text").and_then(serde_json::Value::as_str) {
                            let bytes = text.len() as u64;
                            state.pending_added_blocks.push(ContentBlock {
                                kind,
                                est_tokens: estimate_tokens(bytes),
                                bytes,
                                tool: None,
                                key_hash: None,
                                is_error: false,
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        "reasoning" => {
            // Encrypted reasoning blobs and summary arrays are dropped immediately;
            // reasoning tokens are reported in `usage.reasoning` (`docs/formats/codex.md`).
        }
        "function_call" => {
            let tool_name = payload
                .name
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "unknown".to_string());
            let (bytes, key_hash) = match payload.arguments {
                Some(serde_json::Value::String(ref raw_args)) => {
                    hash_codex_tool_arguments(&tool_name, raw_args)
                }
                Some(ref val) => measure_and_hash_tool_input(&tool_name, val),
                None => hash_codex_tool_arguments(&tool_name, ""),
            };
            if let Some(call_id) = payload.call_id.filter(|s| !s.is_empty()) {
                state
                    .tool_registry
                    .insert(call_id, (tool_name.clone(), key_hash.clone()));
            }
            if !is_inherited_history {
                state.pending_added_blocks.push(ContentBlock {
                    kind: BlockKind::ToolUse,
                    est_tokens: estimate_tokens(bytes),
                    bytes,
                    tool: Some(tool_name),
                    key_hash: Some(key_hash),
                    is_error: false,
                });
            }
        }
        "custom_tool_call" => {
            let tool_name = payload
                .name
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "unknown".to_string());
            let raw_input = payload.input.as_deref().unwrap_or("");
            let (bytes, key_hash) = hash_codex_tool_arguments(&tool_name, raw_input);
            if let Some(call_id) = payload.call_id.filter(|s| !s.is_empty()) {
                state
                    .tool_registry
                    .insert(call_id, (tool_name.clone(), key_hash.clone()));
            }
            if !is_inherited_history {
                state.pending_added_blocks.push(ContentBlock {
                    kind: BlockKind::ToolUse,
                    est_tokens: estimate_tokens(bytes),
                    bytes,
                    tool: Some(tool_name),
                    key_hash: Some(key_hash),
                    is_error: false,
                });
            }
        }
        "web_search_call" => {
            let action_val = payload.action.unwrap_or(serde_json::Value::Null);
            let (bytes, key_hash) = measure_and_hash_tool_input("web_search", &action_val);
            if !is_inherited_history {
                state.pending_added_blocks.push(ContentBlock {
                    kind: BlockKind::ToolUse,
                    est_tokens: estimate_tokens(bytes),
                    bytes,
                    tool: Some("web_search".to_string()),
                    key_hash: Some(key_hash),
                    is_error: false,
                });
            }
        }
        "tool_search_call" => {
            let tool_name = "tool_search".to_string();
            let (bytes, key_hash) = match payload.arguments {
                Some(serde_json::Value::String(ref raw_args)) => {
                    hash_codex_tool_arguments(&tool_name, raw_args)
                }
                Some(ref val) => measure_and_hash_tool_input(&tool_name, val),
                None => hash_codex_tool_arguments(&tool_name, ""),
            };
            if let Some(call_id) = payload.call_id.filter(|s| !s.is_empty()) {
                state
                    .tool_registry
                    .insert(call_id, (tool_name.clone(), key_hash.clone()));
            }
            if !is_inherited_history {
                state.pending_added_blocks.push(ContentBlock {
                    kind: BlockKind::ToolUse,
                    est_tokens: estimate_tokens(bytes),
                    bytes,
                    tool: Some(tool_name),
                    key_hash: Some(key_hash),
                    is_error: false,
                });
            }
        }
        "function_call_output" | "custom_tool_call_output" | "tool_search_output" => {
            if is_inherited_history {
                return;
            }
            let (tool, key_hash) = payload
                .call_id
                .as_deref()
                .and_then(|cid| state.tool_registry.get(cid))
                .map(|(t, h)| (Some(t.clone()), Some(h.clone())))
                .unwrap_or_else(|| {
                    if item_type == "tool_search_output" {
                        (Some("tool_search".to_string()), None)
                    } else {
                        (None, None)
                    }
                });
            let bytes = if let Some(ref out_val) = payload.output {
                measure_content_value_bytes(out_val)
            } else if let Some(ref tools_val) = payload.tools {
                canonical_json_byte_len(tools_val)
            } else {
                0
            };
            let is_error = matches!(payload.status.as_deref(), Some("failed" | "error"));
            state.pending_added_blocks.push(ContentBlock {
                kind: BlockKind::ToolResult,
                est_tokens: estimate_tokens(bytes),
                bytes,
                tool,
                key_hash,
                is_error,
            });
        }
        "agent_message" => {
            if is_inherited_history {
                return;
            }
            let bytes = payload
                .content
                .as_ref()
                .map_or(0, measure_content_value_bytes);
            state.pending_added_blocks.push(ContentBlock {
                kind: BlockKind::SubagentResult,
                est_tokens: estimate_tokens(bytes),
                bytes,
                tool: None,
                key_hash: None,
                is_error: false,
            });
        }
        "compaction" => {
            if !is_inherited_history {
                state.pending_compaction = true;
            }
        }
        _ => {}
    }
}

/// Opens `path` strictly read-only (transparently decompressing `.jsonl.zst` or Zstandard
/// magic frames) and streams its turns in bounded memory.
pub fn parse_codex_file(path: &Path) -> io::Result<CodexParseOutput> {
    let file = open_readonly(path)?;
    let mut peek_reader = BufReader::new(file);
    let head = peek_reader.fill_buf()?;

    let has_zst_ext = path.extension().and_then(|ext| ext.to_str()) == Some("zst");
    let has_zst_magic = head.len() >= 4 && head[..4] == ZSTD_MAGIC;

    if has_zst_ext || has_zst_magic {
        match zstd::stream::read::Decoder::with_buffer(peek_reader) {
            Ok(decoder) => parse_codex_reader_inner(BufReader::new(decoder), true),
            Err(_) => {
                let mut out = CodexParseOutput::default();
                out.diagnostics.issues.push(ParseDiagnostic {
                    line_number: 1,
                    kind: ParseDiagnosticKind::DecompressionError,
                });
                Ok(out)
            }
        }
    } else {
        parse_codex_reader_inner(peek_reader, false)
    }
}

/// Streams Codex CLI rollout records from `reader` using a 16 MiB `BoundedLineReader`.
pub fn parse_codex_reader<R: BufRead>(reader: R) -> io::Result<CodexParseOutput> {
    parse_codex_reader_inner(reader, false)
}

fn parse_codex_reader_inner<R: BufRead>(
    reader: R,
    is_compressed: bool,
) -> io::Result<CodexParseOutput> {
    let mut line_reader = BoundedLineReader::with_max_line_bytes(reader, DEFAULT_MAX_LINE_BYTES);
    let mut diagnostics = ParseDiagnostics::default();
    let mut state = CodexSessionState::default();

    loop {
        let outcome = match line_reader.next_line() {
            Ok(Some(outcome)) => outcome,
            Ok(None) => break,
            Err(err) => {
                if is_compressed {
                    diagnostics.issues.push(ParseDiagnostic {
                        line_number: line_reader.line_number() + 1,
                        kind: ParseDiagnosticKind::DecompressionError,
                    });
                    break;
                }
                return Err(err);
            }
        };

        let (line_number, line) = match outcome {
            LineReadOutcome::Line { line_number, text } => (line_number, text),
            LineReadOutcome::Oversized {
                line_number,
                byte_len,
            } => {
                diagnostics.issues.push(ParseDiagnostic {
                    line_number,
                    kind: ParseDiagnosticKind::OversizedLine {
                        byte_len,
                        limit: line_reader.max_line_bytes(),
                    },
                });
                continue;
            }
            LineReadOutcome::InvalidUtf8 { line_number } => {
                diagnostics.issues.push(ParseDiagnostic {
                    line_number,
                    kind: ParseDiagnosticKind::InvalidUtf8,
                });
                continue;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let env: RawEnvelope<'_> = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => {
                diagnostics.issues.push(ParseDiagnostic {
                    line_number,
                    kind: ParseDiagnosticKind::MalformedJson,
                });
                continue;
            }
        };

        let Some(rec_type) = env.record_type else {
            diagnostics.ignored_records += 1;
            continue;
        };

        match rec_type {
            "session_meta" => {
                let raw: RawSessionMetaLine = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => {
                        diagnostics.issues.push(ParseDiagnostic {
                            line_number,
                            kind: ParseDiagnosticKind::MalformedJson,
                        });
                        continue;
                    }
                };
                let Some(payload) = raw.payload else {
                    continue;
                };

                if !state.records.is_empty() {
                    state.pending_resume_start = true;
                }

                if let Some(sid) = payload
                    .session_id
                    .filter(|s| !s.is_empty())
                    .or_else(|| payload.id.filter(|s| !s.is_empty()))
                {
                    state.session_id = Some(sid);
                }

                if let Some(ver) = payload.cli_version {
                    if is_safe_version_token(&ver) {
                        diagnostics.cli_versions.insert(ver);
                    }
                }

                if let Some(ref cwd) = payload.cwd {
                    if !cwd.is_empty() {
                        state.current_cwd_hash = Some(hash_cwd(cwd));
                    }
                }

                let is_subagent = source_is_subagent(payload.source.as_ref());
                if is_subagent {
                    state.session_is_sidechain = true;
                }

                let has_explicit_fork = payload
                    .forked_from_id
                    .as_deref()
                    .is_some_and(|s| !s.is_empty());
                let parent = payload
                    .forked_from_id
                    .filter(|s| !s.is_empty())
                    .or_else(|| payload.parent_thread_id.filter(|s| !s.is_empty()));

                if parent.is_some() {
                    state.parent_session_id = parent;
                    state.has_inherited_parent = true;
                    if has_explicit_fork || !is_subagent {
                        state.pending_resume_start = true;
                    }
                }

                if let Some(cutoff) = payload.subagent_history_start_ordinal {
                    state.fork_cutoff_ordinal = Some(cutoff);
                }
            }
            "turn_context" => {
                let raw: RawTurnContextLine = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => {
                        diagnostics.issues.push(ParseDiagnostic {
                            line_number,
                            kind: ParseDiagnosticKind::MalformedJson,
                        });
                        continue;
                    }
                };
                state.flush_pending_tc_fallback();
                state.saw_first_turn_context = true;
                state.last_turn_open_for_trailing_tc = false;
                state.last_turn_from_tc_upgradeable = false;

                if let Some(payload) = raw.payload {
                    if let Some(model) = payload.model.filter(|s| !s.is_empty()) {
                        state.current_model = Some(model);
                    }
                    if let Some(ref cwd) = payload.cwd {
                        if !cwd.is_empty() {
                            state.current_cwd_hash = Some(hash_cwd(cwd));
                        }
                    }
                }
            }
            "response_item" => {
                let raw: RawResponseItemLine = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => {
                        diagnostics.issues.push(ParseDiagnostic {
                            line_number,
                            kind: ParseDiagnosticKind::MalformedJson,
                        });
                        continue;
                    }
                };
                let Some(payload) = raw.payload else {
                    continue;
                };
                let inherited = state.is_inherited_history(env.ordinal);
                extract_response_item(payload, inherited, &mut state);
            }
            "token_usage_record" => {
                let raw: RawTokenUsageRecordLine = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => {
                        diagnostics.issues.push(ParseDiagnostic {
                            line_number,
                            kind: ParseDiagnosticKind::MalformedJson,
                        });
                        continue;
                    }
                };
                let Some(ts_str) = raw.timestamp.as_deref().or(env.timestamp) else {
                    diagnostics.issues.push(ParseDiagnostic {
                        line_number,
                        kind: ParseDiagnosticKind::MissingField { field: "timestamp" },
                    });
                    continue;
                };
                let Ok(ts) = Timestamp::parse_rfc3339(ts_str) else {
                    diagnostics.issues.push(ParseDiagnostic {
                        line_number,
                        kind: ParseDiagnosticKind::InvalidTimestamp,
                    });
                    continue;
                };
                let Some(payload) = raw.payload else {
                    diagnostics.issues.push(ParseDiagnostic {
                        line_number,
                        kind: ParseDiagnosticKind::MissingField { field: "payload" },
                    });
                    continue;
                };

                if let Some(ref resp_id) = payload.response_id {
                    if !resp_id.is_empty() && !state.seen_response_ids.insert(resp_id.clone()) {
                        continue;
                    }
                }

                let Some(raw_usage) = payload.usage else {
                    diagnostics.issues.push(ParseDiagnostic {
                        line_number,
                        kind: ParseDiagnosticKind::MissingField { field: "usage" },
                    });
                    continue;
                };

                if state.session_id.is_none() {
                    state.session_id = payload
                        .session_id
                        .filter(|s| !s.is_empty())
                        .or_else(|| payload.thread_id.filter(|s| !s.is_empty()));
                }
                let Some(sid) = state.session_id.clone() else {
                    diagnostics.issues.push(ParseDiagnostic {
                        line_number,
                        kind: ParseDiagnosticKind::MissingField {
                            field: "session_id",
                        },
                    });
                    continue;
                };

                state.saw_any_tur = true;
                if let Some(tot) = payload.thread_token_usage {
                    state.prev_total_usage = Some(tot);
                }

                let usage = normalise_codex_usage(raw_usage);

                // Case A (`tc -> tur` on Turn 1 before `saw_any_tur` was set): upgrade the
                // adjacent `tc`-emitted turn in-place without double-counting!
                if state.last_turn_from_tc_upgradeable {
                    if let Some(last) = state.records.last_mut() {
                        last.ts = ts;
                        last.usage = usage;
                    }
                    state.last_turn_from_tc_upgradeable = false;
                    state.last_turn_open_for_trailing_tc = false;
                    continue;
                }

                // Case B (`tc -> tur` after `saw_any_tur` was already true): consume buffered `tc`
                // rate_limits/window and clear `pending_tc_fallback`.
                let already_paired_with_tc = state.pending_tc_fallback.take().is_some();
                state.emit_turn(sid, ts, usage, !already_paired_with_tc, false);
            }
            "event_msg" => {
                let raw: RawEventMsgLine = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => {
                        diagnostics.issues.push(ParseDiagnostic {
                            line_number,
                            kind: ParseDiagnosticKind::MalformedJson,
                        });
                        continue;
                    }
                };
                let Some(payload) = raw.payload else {
                    diagnostics.ignored_records += 1;
                    continue;
                };
                let Some(ev_type) = payload.event_type.as_deref() else {
                    diagnostics.ignored_records += 1;
                    continue;
                };

                match ev_type {
                    "token_count" => {
                        let rl_pct = payload
                            .rate_limits
                            .and_then(|r| r.primary)
                            .and_then(|p| p.used_percent)
                            .filter(|v| v.is_finite());

                        let Some(info) = payload.info else {
                            // Rate-limit-only `token_count` (`info: null`).
                            if let Some(pct) = rl_pct {
                                if state.last_turn_open_for_trailing_tc {
                                    if let Some(last) = state.records.last_mut() {
                                        last.hints.rate_limit_used_percent = Some(pct);
                                    }
                                } else {
                                    state.pending_rate_limit_used_percent = Some(pct);
                                }
                            }
                            continue;
                        };

                        let ctx_win = info.model_context_window.filter(|&w| w > 0);

                        // Case 1 (`tur -> tc`): attach `rate_limits` & `model_context_window`
                        // to the immediately preceding `token_usage_record` turn.
                        if state.last_turn_open_for_trailing_tc {
                            if let Some(last) = state.records.last_mut() {
                                if rl_pct.is_some() {
                                    last.hints.rate_limit_used_percent = rl_pct;
                                }
                                if ctx_win.is_some() {
                                    last.hints.context_window_override = ctx_win;
                                }
                            }
                            if let Some(tot) = info.total_token_usage {
                                state.prev_total_usage = Some(tot);
                            }
                            state.last_turn_open_for_trailing_tc = false;
                            continue;
                        }

                        let Some(ts_str) = raw.timestamp.as_deref().or(env.timestamp) else {
                            diagnostics.issues.push(ParseDiagnostic {
                                line_number,
                                kind: ParseDiagnosticKind::MissingField { field: "timestamp" },
                            });
                            continue;
                        };
                        let Ok(ts) = Timestamp::parse_rfc3339(ts_str) else {
                            diagnostics.issues.push(ParseDiagnostic {
                                line_number,
                                kind: ParseDiagnosticKind::InvalidTimestamp,
                            });
                            continue;
                        };

                        // Suppress duplicate consecutive `tc -> tc` cumulative snapshots.
                        if let (Some(tot), Some(prev)) =
                            (info.total_token_usage, state.prev_total_usage)
                        {
                            if tot.same_counts(prev) {
                                if let Some(last) = state.records.last_mut() {
                                    if rl_pct.is_some() {
                                        last.hints.rate_limit_used_percent = rl_pct;
                                    }
                                    if ctx_win.is_some() {
                                        last.hints.context_window_override = ctx_win;
                                    }
                                }
                                continue;
                            }
                        }

                        // Resolve per-turn raw token counts (`last_token_usage` preferred,
                        // falling back to cumulative `total_token_usage` deltas).
                        let raw_turn_usage = if let Some(last) = info.last_token_usage {
                            if let Some(tot) = info.total_token_usage {
                                state.prev_total_usage = Some(tot);
                            }
                            last
                        } else if let Some(tot) = info.total_token_usage {
                            let delta = match state.prev_total_usage {
                                None => tot,
                                Some(prev) => {
                                    if tot.input_tokens < prev.input_tokens
                                        && tot.output_tokens <= prev.output_tokens
                                    {
                                        state.pending_resume_start = true;
                                        tot
                                    } else {
                                        RawCodexTokenUsage {
                                            input_tokens: tot
                                                .input_tokens
                                                .saturating_sub(prev.input_tokens),
                                            cached_input_tokens: tot
                                                .cached_input_tokens
                                                .saturating_sub(prev.cached_input_tokens),
                                            cache_write_input_tokens: tot
                                                .cache_write_input_tokens
                                                .saturating_sub(prev.cache_write_input_tokens),
                                            output_tokens: tot
                                                .output_tokens
                                                .saturating_sub(prev.output_tokens),
                                            reasoning_output_tokens: match (
                                                tot.reasoning_output_tokens,
                                                prev.reasoning_output_tokens,
                                            ) {
                                                (Some(t), Some(p)) => Some(t.saturating_sub(p)),
                                                (Some(t), None) => Some(t),
                                                (None, _) => None,
                                            },
                                        }
                                    }
                                }
                            };
                            state.prev_total_usage = Some(tot);
                            delta
                        } else {
                            diagnostics.issues.push(ParseDiagnostic {
                                line_number,
                                kind: ParseDiagnosticKind::MissingField { field: "usage" },
                            });
                            continue;
                        };

                        if rl_pct.is_some() {
                            state.pending_rate_limit_used_percent = rl_pct;
                        }
                        if ctx_win.is_some() {
                            state.pending_context_window_override = ctx_win;
                        }

                        let usage = normalise_codex_usage(raw_turn_usage);

                        // Case 2 (`tc -> tur` when `saw_any_tur` is already active): buffer
                        // fallback & wait for the paired `token_usage_record`.
                        if state.saw_any_tur {
                            state.pending_tc_fallback = Some(PendingTcFallback { ts, usage });
                            continue;
                        }

                        let Some(sid) = state.session_id.clone() else {
                            diagnostics.issues.push(ParseDiagnostic {
                                line_number,
                                kind: ParseDiagnosticKind::MissingField {
                                    field: "session_id",
                                },
                            });
                            continue;
                        };

                        state.emit_turn(sid, ts, usage, false, true);
                    }
                    "context_compacted" => {
                        state.pending_compaction = true;
                        diagnostics.ignored_records += 1;
                    }
                    "error" | "stream_error" => {
                        state.pending_api_error = true;
                        diagnostics.ignored_records += 1;
                    }
                    _ => {
                        diagnostics.ignored_records += 1;
                    }
                }
            }
            "compacted" => {
                state.pending_compaction = true;
                diagnostics.ignored_records += 1;
            }
            other if KNOWN_NON_TURN_TOP_TYPES.contains(&other) => {
                diagnostics.ignored_records += 1;
            }
            other => {
                diagnostics.ignored_records += 1;
                let safe_key = if is_safe_type_token(other) {
                    other.to_string()
                } else {
                    "<invalid-type>".to_string()
                };
                *diagnostics
                    .unknown_record_types
                    .entry(safe_key)
                    .or_insert(0) += 1;
            }
        }
    }

    state.flush_pending_tc_fallback();

    Ok(CodexParseOutput {
        records: state.records,
        diagnostics,
    })
}
