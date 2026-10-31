//! Read-only streaming Claude Code JSONL decoder (`PLAN.md` §5, §10 T04, `docs/formats/claude-code.md`).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, BufReader};
use std::path::Path;

use kakeibo_core::{
    AgentId, BlockKind, CacheWriteSplit, ContentBlock, DisjointUsageInput, Timestamp, TurnHints,
    TurnRecord, Usage,
};
use serde::Deserialize;

use crate::catalog::open_readonly;
use crate::hash::{estimate_tokens, hash_block_content, hash_cwd, measure_and_hash_tool_input};
use crate::line_reader::{
    BoundedLineReader, LineReadOutcome, ParseDiagnostic, ParseDiagnosticKind, ParseDiagnostics,
    DEFAULT_MAX_LINE_BYTES,
};

/// Known non-turn record types observed on `Claude Code 2.1.314` (`docs/formats/claude-code.md`).
const KNOWN_NON_TURN_TYPES: &[&str] = &[
    "agent-name",
    "ai-title",
    "api-request",
    "api-request-blob",
    "api-request-shape",
    "atis-latch",
    "attachment",
    "bridge-session",
    "cost-state",
    "file-history-delta",
    "file-history-snapshot",
    "isolation-latch",
    "last-prompt",
    "mode",
    "permission-mode",
    "queue-operation",
    "summary",
    "tools-used",
];

/// Options controlling a single Claude Code JSONL stream decode.
#[derive(Debug, Clone, Default)]
pub struct ClaudeParseOptions {
    /// Parent session identifier when decoding `<session>/subagents/*.jsonl`.
    pub parent_session_id: Option<String>,
    /// Optional override for the per-line byte ceiling (`16 MiB` when `None`).
    pub max_line_bytes: Option<usize>,
}

/// Deduplicated [`TurnRecord`] items and non-fatal diagnostics produced from a
/// Claude Code JSONL stream.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeParseOutput {
    pub records: Vec<TurnRecord>,
    pub diagnostics: ParseDiagnostics,
}

/// Opens `path` strictly read-only and decodes Claude Code [`TurnRecord`] items.
pub fn parse_claude_file(path: &Path) -> io::Result<ClaudeParseOutput> {
    parse_claude_file_with_options(path, &ClaudeParseOptions::default())
}

/// Opens `path` strictly read-only and decodes Claude Code [`TurnRecord`] items
/// with custom [`ClaudeParseOptions`].
pub fn parse_claude_file_with_options(
    path: &Path,
    options: &ClaudeParseOptions,
) -> io::Result<ClaudeParseOutput> {
    let file = open_readonly(path)?;
    let reader = BufReader::new(file);
    parse_claude_reader(reader, options)
}

#[derive(Deserialize)]
struct TypeEnvelope {
    #[serde(rename = "type")]
    record_type: Option<String>,
    version: Option<String>,
}

#[derive(Deserialize)]
struct RawSystemLine {
    #[serde(rename = "sessionId", alias = "session_id")]
    session_id: Option<String>,
    #[serde(rename = "agentId")]
    agent_id: Option<String>,
    subtype: Option<String>,
}

#[derive(Deserialize)]
struct RawUserLine {
    uuid: Option<String>,
    #[serde(rename = "parentUuid", default)]
    parent_uuid: Option<Option<String>>,
    #[serde(rename = "sessionId", alias = "session_id")]
    session_id: Option<String>,
    #[serde(rename = "agentId")]
    agent_id: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "isCompactSummary", default)]
    is_compact_summary: bool,
    #[serde(rename = "sourceToolUseID")]
    source_tool_use_id: Option<String>,
    message: Option<RawUserMessage>,
}

#[derive(Deserialize)]
struct RawUserMessage {
    content: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawAssistantLine {
    uuid: Option<String>,
    #[serde(rename = "sessionId", alias = "session_id")]
    session_id: Option<String>,
    #[serde(rename = "agentId")]
    agent_id: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    timestamp: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "isSidechain", default)]
    is_sidechain: bool,
    #[serde(rename = "isApiErrorMessage", default)]
    is_api_error_message: bool,
    #[serde(rename = "apiError")]
    api_error: Option<serde_json::Value>,
    error: Option<serde_json::Value>,
    message: Option<RawAssistantMessage>,
}

#[derive(Deserialize)]
struct RawAssistantMessage {
    id: Option<String>,
    model: Option<String>,
    content: Option<Vec<serde_json::Value>>,
    usage: Option<RawClaudeUsage>,
}

#[derive(Deserialize)]
struct RawClaudeUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    cache_creation: Option<RawCacheCreation>,
    output_tokens_details: Option<RawOutputTokensDetails>,
}

#[derive(Deserialize)]
struct RawCacheCreation {
    #[serde(default)]
    ephemeral_5m_input_tokens: u64,
    #[serde(default)]
    ephemeral_1h_input_tokens: u64,
}

#[derive(Deserialize)]
struct RawOutputTokensDetails {
    thinking_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum BlockFingerprint {
    ToolUseId(String),
    ContentDigest {
        kind: BlockKind,
        digest: [u8; 8],
        bytes: u64,
    },
}

struct PendingTurn {
    line_number: usize,
    ts: Timestamp,
    model: Option<String>,
    cwd_hash: Option<String>,
    raw_input: u64,
    raw_cache_write: u64,
    raw_cache_read: u64,
    raw_output: u64,
    thinking_tokens: Option<u64>,
    has_cache_split: bool,
    ephemeral_5m: u64,
    ephemeral_1h: u64,
    added: Vec<ContentBlock>,
    seen_blocks: BTreeSet<BlockFingerprint>,
    tool_index_by_id: BTreeMap<String, usize>,
    hints: TurnHints,
}

struct SessionState {
    session_id: String,
    parent_session_id: Option<String>,
    last_cwd_hash: Option<String>,
    seen_event_uuids: BTreeSet<String>,
    tool_registry: BTreeMap<String, (String, String)>,
    pending_user_blocks: Vec<ContentBlock>,
    pending_resume_start: bool,
    pending_compaction: bool,
    pending_api_error: bool,
    turns: Vec<PendingTurn>,
    turn_index_by_key: BTreeMap<String, usize>,
}

impl SessionState {
    fn new(session_id: String, parent_session_id: Option<String>) -> Self {
        Self {
            session_id,
            parent_session_id,
            last_cwd_hash: None,
            seen_event_uuids: BTreeSet::new(),
            tool_registry: BTreeMap::new(),
            pending_user_blocks: Vec::new(),
            pending_resume_start: false,
            pending_compaction: false,
            pending_api_error: false,
            turns: Vec::new(),
            turn_index_by_key: BTreeMap::new(),
        }
    }
}

fn is_safe_type_token(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 48
        && raw
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
}

fn is_safe_version_token(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 32
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
}

fn resolve_session_id(
    raw_session_id: Option<&str>,
    agent_id: Option<&str>,
    options: &ClaudeParseOptions,
) -> Option<String> {
    if let Some(parent_id) = &options.parent_session_id {
        let suffix = agent_id
            .filter(|id| !id.is_empty())
            .map_or_else(|| "subagent".to_string(), |id| format!("subagent:{id}"));
        return Some(format!("{parent_id}:{suffix}"));
    }
    raw_session_id
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn measure_tool_result_body(body: Option<&serde_json::Value>) -> (u64, [u8; 8]) {
    let Some(val) = body else {
        return (0, hash_block_content("tool_result", ""));
    };
    match val {
        serde_json::Value::String(s) => (s.len() as u64, hash_block_content("tool_result", s)),
        serde_json::Value::Array(items) => {
            let mut total_bytes: u64 = 0;
            let mut combined_hash = [0_u8; 8];
            for item in items {
                if let Some(text) = item.get("text").and_then(serde_json::Value::as_str) {
                    total_bytes = total_bytes.saturating_add(text.len() as u64);
                    let h = hash_block_content("tool_result_part", text);
                    for (dst, src) in combined_hash.iter_mut().zip(h.iter()) {
                        *dst ^= *src;
                    }
                } else {
                    let (b, _) = measure_and_hash_tool_input("tool_result_item", item);
                    total_bytes = total_bytes.saturating_add(b);
                }
            }
            (total_bytes, combined_hash)
        }
        other => {
            let (b, kh) = measure_and_hash_tool_input("tool_result_obj", other);
            (b, hash_block_content("tool_result_obj", &kh))
        }
    }
}

fn extract_user_blocks(
    content: &serde_json::Value,
    is_compact_summary: bool,
    fallback_tool_use_id: Option<&str>,
    tool_registry: &BTreeMap<String, (String, String)>,
    out: &mut Vec<ContentBlock>,
) {
    let text_kind = if is_compact_summary {
        BlockKind::CompactionSummary
    } else {
        BlockKind::UserPrompt
    };

    match content {
        serde_json::Value::String(s) => {
            let bytes = s.len() as u64;
            out.push(ContentBlock {
                kind: text_kind,
                est_tokens: estimate_tokens(bytes),
                bytes,
                tool: None,
                key_hash: None,
                is_error: false,
            });
        }
        serde_json::Value::Array(items) => {
            for item in items {
                let Some(block_type) = item.get("type").and_then(serde_json::Value::as_str) else {
                    continue;
                };
                match block_type {
                    "text" => {
                        let bytes = item
                            .get("text")
                            .and_then(serde_json::Value::as_str)
                            .map_or(0, |s| s.len() as u64);
                        out.push(ContentBlock {
                            kind: text_kind,
                            est_tokens: estimate_tokens(bytes),
                            bytes,
                            tool: None,
                            key_hash: None,
                            is_error: false,
                        });
                    }
                    "tool_result" => {
                        let tool_use_id = item
                            .get("tool_use_id")
                            .and_then(serde_json::Value::as_str)
                            .or(fallback_tool_use_id);
                        let (tool, key_hash) = tool_use_id
                            .and_then(|id| tool_registry.get(id))
                            .map_or((None, None), |(name, kh)| {
                                (Some(name.clone()), Some(kh.clone()))
                            });
                        let is_error = item
                            .get("is_error")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false);
                        let (bytes, _) = measure_tool_result_body(item.get("content"));
                        out.push(ContentBlock {
                            kind: BlockKind::ToolResult,
                            est_tokens: estimate_tokens(bytes),
                            bytes,
                            tool,
                            key_hash,
                            is_error,
                        });
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

struct ExtractedAssistantBlock {
    fingerprint: BlockFingerprint,
    tool_use_id: Option<String>,
    block: ContentBlock,
}

fn extract_assistant_blocks(
    items: &[serde_json::Value],
    tool_registry: &mut BTreeMap<String, (String, String)>,
) -> Vec<ExtractedAssistantBlock> {
    let mut extracted = Vec::with_capacity(items.len());
    for item in items {
        let Some(block_type) = item.get("type").and_then(serde_json::Value::as_str) else {
            continue;
        };
        match block_type {
            "thinking" => {
                let text = item
                    .get("thinking")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let bytes = text.len() as u64;
                let digest = hash_block_content("thinking", text);
                extracted.push(ExtractedAssistantBlock {
                    fingerprint: BlockFingerprint::ContentDigest {
                        kind: BlockKind::AssistantThinking,
                        digest,
                        bytes,
                    },
                    tool_use_id: None,
                    block: ContentBlock {
                        kind: BlockKind::AssistantThinking,
                        est_tokens: estimate_tokens(bytes),
                        bytes,
                        tool: None,
                        key_hash: None,
                        is_error: false,
                    },
                });
            }
            "text" => {
                let text = item
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let bytes = text.len() as u64;
                let digest = hash_block_content("text", text);
                extracted.push(ExtractedAssistantBlock {
                    fingerprint: BlockFingerprint::ContentDigest {
                        kind: BlockKind::AssistantText,
                        digest,
                        bytes,
                    },
                    tool_use_id: None,
                    block: ContentBlock {
                        kind: BlockKind::AssistantText,
                        est_tokens: estimate_tokens(bytes),
                        bytes,
                        tool: None,
                        key_hash: None,
                        is_error: false,
                    },
                });
            }
            "tool_use" => {
                let tool_name = item
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let tool_id = item
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let empty_obj = serde_json::Value::Object(serde_json::Map::new());
                let input_val = item.get("input").unwrap_or(&empty_obj);
                let (bytes, key_hash) = measure_and_hash_tool_input(&tool_name, input_val);

                if let Some(ref id) = tool_id {
                    tool_registry.insert(id.clone(), (tool_name.clone(), key_hash.clone()));
                }

                let fingerprint = match &tool_id {
                    Some(id) => BlockFingerprint::ToolUseId(id.clone()),
                    None => BlockFingerprint::ContentDigest {
                        kind: BlockKind::ToolUse,
                        digest: hash_block_content(&tool_name, &key_hash),
                        bytes,
                    },
                };

                extracted.push(ExtractedAssistantBlock {
                    fingerprint,
                    tool_use_id: tool_id,
                    block: ContentBlock {
                        kind: BlockKind::ToolUse,
                        est_tokens: estimate_tokens(bytes),
                        bytes,
                        tool: Some(tool_name),
                        key_hash: Some(key_hash),
                        is_error: false,
                    },
                });
            }
            _ => {}
        }
    }
    extracted
}

fn merge_assistant_blocks(turn: &mut PendingTurn, incoming: Vec<ExtractedAssistantBlock>) {
    for item in incoming {
        if let Some(ref tool_id) = item.tool_use_id {
            if let Some(&existing_idx) = turn.tool_index_by_id.get(tool_id) {
                if item.block.bytes >= turn.added[existing_idx].bytes {
                    turn.added[existing_idx] = item.block;
                }
                continue;
            }
            let idx = turn.added.len();
            turn.tool_index_by_id.insert(tool_id.clone(), idx);
            turn.seen_blocks.insert(item.fingerprint);
            turn.added.push(item.block);
        } else if turn.seen_blocks.insert(item.fingerprint) {
            turn.added.push(item.block);
        }
    }
}

/// Streams a buffered Claude Code JSONL reader into deduplicated [`TurnRecord`] items.
pub fn parse_claude_reader<R: BufRead>(
    reader: R,
    options: &ClaudeParseOptions,
) -> io::Result<ClaudeParseOutput> {
    let max_line = options.max_line_bytes.unwrap_or(DEFAULT_MAX_LINE_BYTES);
    let mut line_reader = BoundedLineReader::with_max_line_bytes(reader, max_line);

    let mut diagnostics = ParseDiagnostics::default();
    let mut session_order: Vec<String> = Vec::new();
    let mut sessions: BTreeMap<String, SessionState> = BTreeMap::new();
    let mut last_active_session_id: Option<String> = None;

    while let Some(outcome) = line_reader.next_line()? {
        let (line_number, text) = match outcome {
            LineReadOutcome::Line { line_number, text } => (line_number, text),
            LineReadOutcome::Oversized {
                line_number,
                byte_len,
            } => {
                diagnostics.issues.push(ParseDiagnostic {
                    line_number,
                    kind: ParseDiagnosticKind::OversizedLine {
                        byte_len,
                        limit: max_line,
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

        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }

        let envelope: TypeEnvelope = match serde_json::from_str(trimmed) {
            Ok(env) => env,
            Err(_) => {
                diagnostics.issues.push(ParseDiagnostic {
                    line_number,
                    kind: ParseDiagnosticKind::MalformedJson,
                });
                continue;
            }
        };

        if let Some(ref ver) = envelope.version {
            if is_safe_version_token(ver) {
                diagnostics.cli_versions.insert(ver.clone());
            }
        }

        let Some(rec_type) = envelope.record_type.as_deref() else {
            diagnostics.ignored_records += 1;
            continue;
        };

        match rec_type {
            "user" => {
                let raw: RawUserLine = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => {
                        diagnostics.issues.push(ParseDiagnostic {
                            line_number,
                            kind: ParseDiagnosticKind::MalformedJson,
                        });
                        continue;
                    }
                };

                let Some(sid) =
                    resolve_session_id(raw.session_id.as_deref(), raw.agent_id.as_deref(), options)
                        .or_else(|| last_active_session_id.clone())
                else {
                    diagnostics.issues.push(ParseDiagnostic {
                        line_number,
                        kind: ParseDiagnosticKind::MissingField { field: "sessionId" },
                    });
                    continue;
                };

                let is_fork_switch = last_active_session_id
                    .as_deref()
                    .is_some_and(|prev| prev != sid);
                last_active_session_id = Some(sid.clone());

                let state = sessions.entry(sid.clone()).or_insert_with(|| {
                    session_order.push(sid.clone());
                    SessionState::new(sid, options.parent_session_id.clone())
                });

                if let Some(ref uuid) = raw.uuid {
                    if !uuid.is_empty() && !state.seen_event_uuids.insert(uuid.clone()) {
                        continue;
                    }
                }

                if let Some(ref cwd) = raw.cwd {
                    if !cwd.is_empty() {
                        state.last_cwd_hash = Some(hash_cwd(cwd));
                    }
                }

                if raw.is_compact_summary {
                    state.pending_compaction = true;
                } else if is_fork_switch
                    || (!state.turns.is_empty()
                        && !state.pending_compaction
                        && matches!(raw.parent_uuid, Some(None)))
                {
                    state.pending_resume_start = true;
                }

                if let Some(msg) = raw.message {
                    if let Some(ref content) = msg.content {
                        extract_user_blocks(
                            content,
                            raw.is_compact_summary,
                            raw.source_tool_use_id.as_deref(),
                            &state.tool_registry,
                            &mut state.pending_user_blocks,
                        );
                    }
                }
            }
            "assistant" => {
                let raw: RawAssistantLine = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => {
                        diagnostics.issues.push(ParseDiagnostic {
                            line_number,
                            kind: ParseDiagnosticKind::MalformedJson,
                        });
                        continue;
                    }
                };

                let Some(sid) =
                    resolve_session_id(raw.session_id.as_deref(), raw.agent_id.as_deref(), options)
                        .or_else(|| last_active_session_id.clone())
                else {
                    diagnostics.issues.push(ParseDiagnostic {
                        line_number,
                        kind: ParseDiagnosticKind::MissingField { field: "sessionId" },
                    });
                    continue;
                };
                last_active_session_id = Some(sid.clone());

                let state = sessions.entry(sid.clone()).or_insert_with(|| {
                    session_order.push(sid.clone());
                    SessionState::new(sid, options.parent_session_id.clone())
                });

                if let Some(ref uuid) = raw.uuid {
                    if !uuid.is_empty() && !state.seen_event_uuids.insert(uuid.clone()) {
                        continue;
                    }
                }

                let is_error_line =
                    raw.is_api_error_message || raw.api_error.is_some() || raw.error.is_some();

                let Some(msg) = raw.message else {
                    if is_error_line {
                        state.pending_api_error = true;
                        diagnostics.ignored_records += 1;
                    } else {
                        diagnostics.issues.push(ParseDiagnostic {
                            line_number,
                            kind: ParseDiagnosticKind::MissingField { field: "message" },
                        });
                    }
                    continue;
                };

                let Some(raw_usage) = msg.usage else {
                    if is_error_line {
                        state.pending_api_error = true;
                        diagnostics.ignored_records += 1;
                    } else {
                        diagnostics.issues.push(ParseDiagnostic {
                            line_number,
                            kind: ParseDiagnosticKind::MissingField {
                                field: "message.usage",
                            },
                        });
                    }
                    continue;
                };

                // Verified Claude Code 2.1.314: synthetic local API error lines carry
                // zeroed usage counts and must mark the subsequent retried request rather
                // than emitting a bogus 0-token turn.
                if is_error_line
                    && raw_usage.input_tokens == 0
                    && raw_usage.cache_creation_input_tokens == 0
                    && raw_usage.cache_read_input_tokens == 0
                    && raw_usage.output_tokens == 0
                {
                    state.pending_api_error = true;
                    diagnostics.ignored_records += 1;
                    continue;
                }

                let Some(ts_str) = raw.timestamp.as_deref() else {
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

                let cwd_hash = raw
                    .cwd
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(hash_cwd)
                    .or_else(|| state.last_cwd_hash.clone());
                if cwd_hash.is_some() {
                    state.last_cwd_hash.clone_from(&cwd_hash);
                }

                let request_key = match (
                    msg.id.as_deref().filter(|s| !s.is_empty()),
                    raw.request_id.as_deref().filter(|s| !s.is_empty()),
                ) {
                    (Some(mid), Some(rid)) => format!("{mid}\0{rid}"),
                    (Some(mid), None) => format!("m:{mid}"),
                    (None, Some(rid)) => format!("r:{rid}"),
                    (None, None) => match raw.uuid.as_deref().filter(|s| !s.is_empty()) {
                        Some(u) => format!("u:{u}"),
                        None => format!("line:{line_number}"),
                    },
                };

                let extracted_blocks = extract_assistant_blocks(
                    msg.content.as_deref().unwrap_or(&[]),
                    &mut state.tool_registry,
                );
                let thinking = raw_usage
                    .output_tokens_details
                    .and_then(|d| d.thinking_tokens);
                let (has_split, eph_5m, eph_1h) = match raw_usage.cache_creation {
                    Some(cc) => (
                        true,
                        cc.ephemeral_5m_input_tokens,
                        cc.ephemeral_1h_input_tokens,
                    ),
                    None => (false, 0, 0),
                };

                if let Some(&turn_idx) = state.turn_index_by_key.get(&request_key) {
                    let pending = &mut state.turns[turn_idx];
                    if ts > pending.ts {
                        pending.ts = ts;
                    }
                    if pending.model.is_none() {
                        pending.model = msg.model;
                    }
                    if pending.cwd_hash.is_none() {
                        pending.cwd_hash = cwd_hash;
                    }
                    pending.raw_input = pending.raw_input.max(raw_usage.input_tokens);
                    pending.raw_cache_write = pending
                        .raw_cache_write
                        .max(raw_usage.cache_creation_input_tokens);
                    pending.raw_cache_read = pending
                        .raw_cache_read
                        .max(raw_usage.cache_read_input_tokens);
                    pending.raw_output = pending.raw_output.max(raw_usage.output_tokens);
                    pending.thinking_tokens = match (pending.thinking_tokens, thinking) {
                        (Some(a), Some(b)) => Some(a.max(b)),
                        (Some(a), None) => Some(a),
                        (None, Some(b)) => Some(b),
                        (None, None) => None,
                    };
                    if has_split {
                        pending.has_cache_split = true;
                        pending.ephemeral_5m = pending.ephemeral_5m.max(eph_5m);
                        pending.ephemeral_1h = pending.ephemeral_1h.max(eph_1h);
                    }
                    pending.hints.sidechain |=
                        raw.is_sidechain || options.parent_session_id.is_some();
                    pending.hints.api_error |= is_error_line;
                    merge_assistant_blocks(pending, extracted_blocks);
                } else {
                    let initial_user_blocks = std::mem::take(&mut state.pending_user_blocks);
                    let hints = TurnHints {
                        resume_start: std::mem::take(&mut state.pending_resume_start),
                        compaction: std::mem::take(&mut state.pending_compaction),
                        sidechain: raw.is_sidechain || options.parent_session_id.is_some(),
                        api_error: std::mem::take(&mut state.pending_api_error) || is_error_line,
                        rate_limit_used_percent: None,
                        context_window_override: None,
                    };

                    let mut pending = PendingTurn {
                        line_number,
                        ts,
                        model: msg.model,
                        cwd_hash,
                        raw_input: raw_usage.input_tokens,
                        raw_cache_write: raw_usage.cache_creation_input_tokens,
                        raw_cache_read: raw_usage.cache_read_input_tokens,
                        raw_output: raw_usage.output_tokens,
                        thinking_tokens: thinking,
                        has_cache_split: has_split,
                        ephemeral_5m: eph_5m,
                        ephemeral_1h: eph_1h,
                        added: initial_user_blocks,
                        seen_blocks: BTreeSet::new(),
                        tool_index_by_id: BTreeMap::new(),
                        hints,
                    };
                    merge_assistant_blocks(&mut pending, extracted_blocks);

                    let idx = state.turns.len();
                    state.turn_index_by_key.insert(request_key, idx);
                    state.turns.push(pending);
                }
            }
            "system" => {
                diagnostics.ignored_records += 1;
                if let Ok(sys) = serde_json::from_str::<RawSystemLine>(trimmed) {
                    if let Some(sid) = resolve_session_id(
                        sys.session_id.as_deref(),
                        sys.agent_id.as_deref(),
                        options,
                    )
                    .or_else(|| last_active_session_id.clone())
                    {
                        last_active_session_id = Some(sid.clone());
                        let state = sessions.entry(sid.clone()).or_insert_with(|| {
                            session_order.push(sid.clone());
                            SessionState::new(sid, options.parent_session_id.clone())
                        });
                        match sys.subtype.as_deref() {
                            Some("compact_boundary") => state.pending_compaction = true,
                            Some("away_summary") => state.pending_resume_start = true,
                            Some("api_error") => state.pending_api_error = true,
                            _ => {}
                        }
                    }
                }
            }
            other if KNOWN_NON_TURN_TYPES.contains(&other) => {
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

    let mut records = Vec::new();
    for sid in session_order {
        let Some(state) = sessions.remove(&sid) else {
            continue;
        };
        let mut turn_ordinal: u32 = 0;
        for pending in state.turns {
            let mut disjoint = DisjointUsageInput::measured(
                pending.raw_input,
                pending.raw_cache_write,
                pending.raw_cache_read,
                pending.raw_output,
            );
            disjoint.reasoning_tokens = pending.thinking_tokens;
            if pending.has_cache_split {
                disjoint.cache_write_split = Some(CacheWriteSplit {
                    ephemeral_5m: pending.ephemeral_5m,
                    ephemeral_1h: pending.ephemeral_1h,
                });
            }

            let usage = match Usage::from_disjoint(disjoint) {
                Ok(u) => u,
                Err(_) => {
                    diagnostics.issues.push(ParseDiagnostic {
                        line_number: pending.line_number,
                        kind: ParseDiagnosticKind::UsageOverflow,
                    });
                    continue;
                }
            };

            turn_ordinal = turn_ordinal.saturating_add(1);
            records.push(TurnRecord {
                agent: AgentId::ClaudeCode,
                session_id: state.session_id.clone(),
                parent_session_id: state.parent_session_id.clone(),
                turn: turn_ordinal,
                ts: pending.ts,
                model: pending.model,
                cwd_hash: pending.cwd_hash,
                usage,
                added: pending.added,
                hints: pending.hints,
            });
        }
    }

    Ok(ClaudeParseOutput {
        records,
        diagnostics,
    })
}
