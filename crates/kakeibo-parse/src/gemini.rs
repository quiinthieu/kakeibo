//! Read-only streaming Gemini CLI JSONL/JSON decoder (`PLAN.md` §5, §10 T10, `docs/formats/gemini-cli.md`).
//!
//! Reconstructs `ConversationRecord` state across `$set`, `$patch` (object-merge and
//! RFC-6902 array), same-ID message upserts, and `$rewindTo` while reducing every
//! message immediately to byte counts, tool names, and SHA-256 hashes (zero raw text retention).

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read};
use std::mem;
use std::path::{Path, PathBuf};

use kakeibo_core::{
    AgentId, BlockKind, Confidence, ContentBlock, InclusiveUsageInput, Timestamp, TurnHints,
    TurnRecord, Usage,
};
use serde_json::{Map, Value};

use crate::catalog::{open_readonly, read_gemini_slug_registry_at};
use crate::hash::{
    canonical_json_byte_len, estimate_tokens, hash_cwd, measure_and_hash_tool_input,
};
use crate::line_reader::{
    BoundedLineReader, LineReadOutcome, ParseDiagnostic, ParseDiagnosticKind, ParseDiagnostics,
    DEFAULT_MAX_LINE_BYTES,
};

/// Upper bound when reading a legacy single-object `session-*.json` file (`64 MiB`).
const MAX_LEGACY_JSON_BYTES: u64 = 64 * 1024 * 1024;

/// Options controlling a Gemini CLI transcript decode.
#[derive(Debug, Clone, Default)]
pub struct GeminiParseOptions {
    /// Parent `sessionId` when decoding `<gemini_root>/tmp/<slug>/chats/<parent_id>/*.jsonl`.
    pub parent_session_id: Option<String>,
    /// Optional preloaded `slug -> raw_cwd` map from [`crate::PathCatalog::load_gemini_slug_registry`].
    pub slug_registry: Option<BTreeMap<String, String>>,
    /// Optional override for the per-line byte ceiling (`16 MiB` when `None`).
    pub max_line_bytes: Option<usize>,
}

/// Deduplicated [`TurnRecord`] items and non-fatal diagnostics produced from a
/// Gemini CLI `.jsonl` or legacy `.json` transcript.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GeminiParseOutput {
    pub records: Vec<TurnRecord>,
    pub diagnostics: ParseDiagnostics,
}

/// Opens `path` strictly read-only and decodes Gemini CLI [`TurnRecord`] items.
pub fn parse_gemini_file(path: &Path) -> io::Result<GeminiParseOutput> {
    parse_gemini_file_with_options(path, &GeminiParseOptions::default())
}

/// Opens `path` strictly read-only and decodes Gemini CLI [`TurnRecord`] items
/// with custom [`GeminiParseOptions`].
pub fn parse_gemini_file_with_options(
    path: &Path,
    options: &GeminiParseOptions,
) -> io::Result<GeminiParseOutput> {
    let file = open_readonly(path)?;
    let reader = BufReader::new(file);
    parse_gemini_reader(reader, Some(path), options)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GeminiMsgType {
    User,
    Gemini,
    Info,
    Warning,
    Error,
}

impl GeminiMsgType {
    fn from_raw(raw: &str) -> Option<Self> {
        match raw {
            "user" => Some(Self::User),
            "gemini" => Some(Self::Gemini),
            "info" => Some(Self::Info),
            "warning" => Some(Self::Warning),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RawGeminiTokens {
    input: u64,
    output: u64,
    cached: u64,
    thoughts: u64,
    tool: u64,
}

impl RawGeminiTokens {
    fn from_value(val: &Value) -> Option<Self> {
        let obj = val.as_object()?;
        Some(Self {
            input: obj.get("input").and_then(Value::as_u64).unwrap_or(0),
            output: obj.get("output").and_then(Value::as_u64).unwrap_or(0),
            cached: obj.get("cached").and_then(Value::as_u64).unwrap_or(0),
            thoughts: obj.get("thoughts").and_then(Value::as_u64).unwrap_or(0),
            tool: obj.get("tool").and_then(Value::as_u64).unwrap_or(0),
        })
    }

    fn merge_from_object(&mut self, obj: &Map<String, Value>) {
        if let Some(v) = obj.get("input").and_then(Value::as_u64) {
            self.input = v;
        }
        if let Some(v) = obj.get("output").and_then(Value::as_u64) {
            self.output = v;
        }
        if let Some(v) = obj.get("cached").and_then(Value::as_u64) {
            self.cached = v;
        }
        if let Some(v) = obj.get("thoughts").and_then(Value::as_u64) {
            self.thoughts = v;
        }
        if let Some(v) = obj.get("tool").and_then(Value::as_u64) {
            self.tool = v;
        }
    }
}

#[derive(Debug, Clone)]
struct CompactGeminiToolCall {
    id: String,
    name: String,
    args_bytes: u64,
    key_hash: String,
    result_bytes: Option<u64>,
    is_error: bool,
}

#[derive(Debug, Clone)]
struct CompactGeminiMessage {
    id: String,
    msg_type: GeminiMsgType,
    timestamp: Option<Timestamp>,
    model: Option<String>,
    content_bytes: Option<u64>,
    thought_blocks: Vec<u64>,
    tool_calls: Vec<CompactGeminiToolCall>,
    tokens: Option<RawGeminiTokens>,
    mentions_compaction: bool,
}

#[derive(Debug, Default)]
struct GeminiConversationMeta {
    session_id: Option<String>,
    start_time: Option<Timestamp>,
    last_updated: Option<Timestamp>,
    kind: Option<String>,
    /// Immediate `hash_cwd` of the first workspace directory listed on metadata,
    /// avoiding retention of raw path strings during streaming decode.
    fallback_cwd_hash: Option<String>,
}

fn non_zero(bytes: u64) -> Option<u64> {
    (bytes > 0).then_some(bytes)
}

/// Measures UTF-8 payload bytes of a Gemini `PartListUnion` (`string`, `Part`, or `Part[]`)
/// without retaining any raw text.
fn measure_gemini_part_union(val: &Value) -> u64 {
    match val {
        Value::Null => 0,
        Value::String(s) => s.len() as u64,
        Value::Array(items) => items.iter().fold(0_u64, |acc, item| {
            acc.saturating_add(measure_gemini_part_union(item))
        }),
        Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(Value::as_str) {
                text.len() as u64
            } else {
                canonical_json_byte_len(val)
            }
        }
        Value::Bool(_) | Value::Number(_) => canonical_json_byte_len(val),
    }
}

fn content_mentions_compaction(val: &Value) -> bool {
    match val {
        Value::String(s) => {
            let lower = s.to_ascii_lowercase();
            lower.contains("compact") || lower.contains("compress")
        }
        Value::Array(items) => items.iter().any(content_mentions_compaction),
        Value::Object(map) => map.get("text").is_some_and(content_mentions_compaction),
        _ => false,
    }
}

/// Measures UTF-8 payload bytes of a Gemini `ToolCallRecord.result` (`PartListUnion`)
/// unwrapping `functionResponse.response.{output,error}` strings when present.
fn measure_gemini_tool_result(val: &Value) -> u64 {
    match val {
        Value::Null => 0,
        Value::String(s) => s.len() as u64,
        Value::Array(items) => items.iter().fold(0_u64, |acc, item| {
            acc.saturating_add(measure_gemini_tool_result(item))
        }),
        Value::Object(map) => {
            if let Some(fr_obj) = map.get("functionResponse").and_then(Value::as_object) {
                if let Some(resp_val) = fr_obj.get("response") {
                    if let Some(resp_map) = resp_val.as_object() {
                        if resp_map.len() == 1 {
                            if let Some(out_str) = resp_map
                                .get("output")
                                .or_else(|| resp_map.get("error"))
                                .and_then(Value::as_str)
                            {
                                return out_str.len() as u64;
                            }
                        }
                    }
                    return canonical_json_byte_len(resp_val);
                }
            }
            if let Some(text) = map.get("text").and_then(Value::as_str) {
                text.len() as u64
            } else {
                canonical_json_byte_len(val)
            }
        }
        Value::Bool(_) | Value::Number(_) => canonical_json_byte_len(val),
    }
}

fn measure_thought_item(val: &Value) -> u64 {
    match val {
        Value::String(s) => s.len() as u64,
        Value::Object(map) => {
            let subj = map.get("subject").and_then(Value::as_str).unwrap_or("");
            let desc = map.get("description").and_then(Value::as_str).unwrap_or("");
            (subj.len() as u64).saturating_add(desc.len() as u64)
        }
        _ => 0,
    }
}

fn measure_thoughts_array(val: &Value) -> Vec<u64> {
    let Some(arr) = val.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .map(measure_thought_item)
        .filter(|&b| b > 0)
        .collect()
}

fn parse_compact_tool_call(val: &Value) -> Option<CompactGeminiToolCall> {
    let obj = val.as_object()?;
    let id = obj
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let name = obj
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown_tool")
        .to_string();
    let empty_args = Value::Object(Map::new());
    let args_val = obj.get("args").unwrap_or(&empty_args);
    let (args_bytes, key_hash) = measure_and_hash_tool_input(&name, args_val);

    let result_bytes = obj
        .get("result")
        .and_then(|r| non_zero(measure_gemini_tool_result(r)))
        .or_else(|| {
            obj.get("resultDisplay")
                .and_then(|rd| non_zero(measure_gemini_part_union(rd)))
        });

    let is_error = obj
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|st| st == "error");

    Some(CompactGeminiToolCall {
        id,
        name,
        args_bytes,
        key_hash,
        result_bytes,
        is_error,
    })
}

fn merge_tool_call_from_object(target: &mut CompactGeminiToolCall, obj: &Map<String, Value>) {
    if let Some(name) = obj
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        target.name = name.to_string();
    }
    if let Some(args_val) = obj.get("args") {
        let (bytes, hash) = measure_and_hash_tool_input(&target.name, args_val);
        target.args_bytes = bytes;
        target.key_hash = hash;
    }
    if let Some(res_val) = obj.get("result") {
        target.result_bytes = non_zero(measure_gemini_tool_result(res_val));
    } else if target.result_bytes.is_none() {
        if let Some(rd_val) = obj.get("resultDisplay") {
            target.result_bytes = non_zero(measure_gemini_part_union(rd_val));
        }
    }
    if let Some(status) = obj.get("status").and_then(Value::as_str) {
        target.is_error = status == "error";
    }
}

fn is_safe_type_token(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 64
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'$'))
}

#[derive(Default)]
struct GeminiReconstructor {
    meta: GeminiConversationMeta,
    messages: Vec<CompactGeminiMessage>,
    diagnostics: ParseDiagnostics,
}

impl GeminiReconstructor {
    fn merge_meta_object(&mut self, obj: &Map<String, Value>, line_number: usize) {
        if let Some(sid) = obj
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            self.meta.session_id = Some(sid.to_string());
        }
        if let Some(st_raw) = obj.get("startTime").and_then(Value::as_str) {
            match Timestamp::parse_rfc3339(st_raw) {
                Ok(ts) => self.meta.start_time = Some(ts),
                Err(_) => self.diagnostics.issues.push(ParseDiagnostic {
                    line_number,
                    kind: ParseDiagnosticKind::InvalidTimestamp,
                }),
            }
        }
        if let Some(lu_raw) = obj.get("lastUpdated").and_then(Value::as_str) {
            match Timestamp::parse_rfc3339(lu_raw) {
                Ok(ts) => self.meta.last_updated = Some(ts),
                Err(_) => self.diagnostics.issues.push(ParseDiagnostic {
                    line_number,
                    kind: ParseDiagnosticKind::InvalidTimestamp,
                }),
            }
        }
        if let Some(kind) = obj
            .get("kind")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            self.meta.kind = Some(kind.to_string());
        }
        if let Some(dirs) = obj.get("directories").and_then(Value::as_array) {
            if let Some(first_dir) = dirs
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .find(|s| !s.is_empty())
            {
                self.meta.fallback_cwd_hash = Some(hash_cwd(first_dir));
            }
        }
    }

    fn parse_message_object(
        &mut self,
        obj: &Map<String, Value>,
        msg_type: GeminiMsgType,
        line_number: usize,
    ) -> CompactGeminiMessage {
        let id = obj
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let timestamp = match obj.get("timestamp").and_then(Value::as_str) {
            Some(ts_raw) => match Timestamp::parse_rfc3339(ts_raw) {
                Ok(ts) => Some(ts),
                Err(_) => {
                    self.diagnostics.issues.push(ParseDiagnostic {
                        line_number,
                        kind: ParseDiagnosticKind::InvalidTimestamp,
                    });
                    None
                }
            },
            None => None,
        };
        let model = obj
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let content_bytes = obj
            .get("content")
            .and_then(|c| non_zero(measure_gemini_part_union(c)));
        let mentions_compaction = matches!(msg_type, GeminiMsgType::Info)
            && obj.get("content").is_some_and(content_mentions_compaction);
        let thought_blocks = obj
            .get("thoughts")
            .map(measure_thoughts_array)
            .unwrap_or_default();
        let tool_calls = obj
            .get("toolCalls")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(parse_compact_tool_call).collect())
            .unwrap_or_default();
        let tokens = obj.get("tokens").and_then(RawGeminiTokens::from_value);

        CompactGeminiMessage {
            id,
            msg_type,
            timestamp,
            model,
            content_bytes,
            thought_blocks,
            tool_calls,
            tokens,
            mentions_compaction,
        }
    }

    fn upsert_message(&mut self, msg: CompactGeminiMessage) {
        if !msg.id.is_empty() {
            if let Some(pos) = self.messages.iter().position(|m| m.id == msg.id) {
                self.messages[pos] = msg;
                return;
            }
        }
        self.messages.push(msg);
    }

    fn apply_rewind(&mut self, target_id: &str) {
        if let Some(pos) = self.messages.iter().position(|m| m.id == target_id) {
            self.messages.truncate(pos);
        } else {
            self.messages.clear();
        }
    }

    fn find_patch_target_mut(
        &mut self,
        target_id: Option<&str>,
    ) -> Option<&mut CompactGeminiMessage> {
        if let Some(id) = target_id.filter(|s| !s.is_empty()) {
            self.messages.iter_mut().find(|m| m.id == id)
        } else {
            self.messages.last_mut()
        }
    }

    fn apply_object_patch(
        &mut self,
        outer_id: Option<&str>,
        patch_obj: &Map<String, Value>,
        line_number: usize,
    ) {
        let target_id = outer_id.or_else(|| patch_obj.get("id").and_then(Value::as_str));
        let mut invalid_ts = false;
        let Some(target) = self.find_patch_target_mut(target_id) else {
            self.diagnostics.ignored_records += 1;
            return;
        };

        if let Some(ts_raw) = patch_obj.get("timestamp").and_then(Value::as_str) {
            match Timestamp::parse_rfc3339(ts_raw) {
                Ok(ts) => target.timestamp = Some(ts),
                Err(_) => invalid_ts = true,
            }
        }
        if let Some(model) = patch_obj
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            target.model = Some(model.to_string());
        }
        if let Some(content_val) = patch_obj.get("content") {
            target.content_bytes = non_zero(measure_gemini_part_union(content_val));
            if matches!(target.msg_type, GeminiMsgType::Info)
                && content_mentions_compaction(content_val)
            {
                target.mentions_compaction = true;
            }
        }
        if let Some(thoughts_val) = patch_obj.get("thoughts") {
            target.thought_blocks = measure_thoughts_array(thoughts_val);
        }
        if let Some(tok_obj) = patch_obj.get("tokens").and_then(Value::as_object) {
            target
                .tokens
                .get_or_insert_with(RawGeminiTokens::default)
                .merge_from_object(tok_obj);
        }
        if let Some(tc_arr) = patch_obj.get("toolCalls").and_then(Value::as_array) {
            for item in tc_arr {
                let Some(tc_obj) = item.as_object() else {
                    continue;
                };
                let item_id = tc_obj.get("id").and_then(Value::as_str).unwrap_or("");
                if !item_id.is_empty() {
                    if let Some(existing) = target.tool_calls.iter_mut().find(|c| c.id == item_id) {
                        merge_tool_call_from_object(existing, tc_obj);
                        continue;
                    }
                }
                if let Some(new_tc) = parse_compact_tool_call(item) {
                    target.tool_calls.push(new_tc);
                }
            }
        }

        if invalid_ts {
            self.diagnostics.issues.push(ParseDiagnostic {
                line_number,
                kind: ParseDiagnosticKind::InvalidTimestamp,
            });
        }
    }

    fn apply_rfc6902_patch(&mut self, outer_id: Option<&str>, ops: &[Value], line_number: usize) {
        let mut invalid_ts = false;
        let Some(target) = self.find_patch_target_mut(outer_id) else {
            self.diagnostics.ignored_records += 1;
            return;
        };

        for op_val in ops {
            let Some(op_obj) = op_val.as_object() else {
                continue;
            };
            let op = op_obj.get("op").and_then(Value::as_str).unwrap_or("");
            let path = op_obj.get("path").and_then(Value::as_str).unwrap_or("");
            let val = op_obj.get("value").unwrap_or(&Value::Null);

            let segments: Vec<&str> = path
                .strip_prefix('/')
                .map(|rest| rest.split('/').collect())
                .unwrap_or_default();

            match segments.as_slice() {
                ["content"] => {
                    if op == "remove" {
                        target.content_bytes = None;
                    } else {
                        target.content_bytes = non_zero(measure_gemini_part_union(val));
                    }
                }
                ["model"] => {
                    if op == "remove" {
                        target.model = None;
                    } else if let Some(m) = val.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                        target.model = Some(m.to_string());
                    }
                }
                ["timestamp"] => {
                    if op == "remove" {
                        target.timestamp = None;
                    } else if let Some(s) = val.as_str() {
                        match Timestamp::parse_rfc3339(s) {
                            Ok(ts) => target.timestamp = Some(ts),
                            Err(_) => invalid_ts = true,
                        }
                    }
                }
                ["tokens"] => {
                    if op == "remove" {
                        target.tokens = None;
                    } else {
                        target.tokens = RawGeminiTokens::from_value(val);
                    }
                }
                ["tokens", field] => {
                    let v = if op == "remove" {
                        0
                    } else {
                        val.as_u64().unwrap_or(0)
                    };
                    let tok = target.tokens.get_or_insert_with(RawGeminiTokens::default);
                    match *field {
                        "input" => tok.input = v,
                        "output" => tok.output = v,
                        "cached" => tok.cached = v,
                        "thoughts" => tok.thoughts = v,
                        "tool" => tok.tool = v,
                        _ => {}
                    }
                }
                ["thoughts"] => {
                    if op == "remove" {
                        target.thought_blocks.clear();
                    } else {
                        target.thought_blocks = measure_thoughts_array(val);
                    }
                }
                ["thoughts", "-"] if op == "add" => {
                    let b = measure_thought_item(val);
                    if b > 0 {
                        target.thought_blocks.push(b);
                    }
                }
                ["thoughts", idx_str] => {
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        if op == "remove" && idx < target.thought_blocks.len() {
                            target.thought_blocks.remove(idx);
                        } else if idx < target.thought_blocks.len() {
                            target.thought_blocks[idx] = measure_thought_item(val);
                        } else if op == "add" && idx == target.thought_blocks.len() {
                            let b = measure_thought_item(val);
                            if b > 0 {
                                target.thought_blocks.push(b);
                            }
                        }
                    }
                }
                ["toolCalls"] => {
                    if op == "remove" {
                        target.tool_calls.clear();
                    } else if let Some(arr) = val.as_array() {
                        target.tool_calls =
                            arr.iter().filter_map(parse_compact_tool_call).collect();
                    }
                }
                ["toolCalls", "-"] if op == "add" => {
                    if let Some(tc) = parse_compact_tool_call(val) {
                        target.tool_calls.push(tc);
                    }
                }
                ["toolCalls", idx_str] => {
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        if op == "remove" && idx < target.tool_calls.len() {
                            target.tool_calls.remove(idx);
                        } else if let Some(tc) = parse_compact_tool_call(val) {
                            if op == "replace" && idx < target.tool_calls.len() {
                                target.tool_calls[idx] = tc;
                            } else if op == "add" && idx <= target.tool_calls.len() {
                                target.tool_calls.insert(idx, tc);
                            }
                        }
                    }
                }
                ["toolCalls", idx_str, field] => {
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        if let Some(tc) = target.tool_calls.get_mut(idx) {
                            match *field {
                                "name" => {
                                    if let Some(n) =
                                        val.as_str().map(str::trim).filter(|s| !s.is_empty())
                                    {
                                        tc.name = n.to_string();
                                    }
                                }
                                "args" => {
                                    let (bytes, hash) = measure_and_hash_tool_input(&tc.name, val);
                                    tc.args_bytes = bytes;
                                    tc.key_hash = hash;
                                }
                                "result" => {
                                    tc.result_bytes = if op == "remove" {
                                        None
                                    } else {
                                        non_zero(measure_gemini_tool_result(val))
                                    };
                                }
                                "resultDisplay" => {
                                    if tc.result_bytes.is_none() && op != "remove" {
                                        tc.result_bytes = non_zero(measure_gemini_part_union(val));
                                    }
                                }
                                "status" => {
                                    tc.is_error = op != "remove"
                                        && val.as_str().is_some_and(|st| st == "error");
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        if invalid_ts {
            self.diagnostics.issues.push(ParseDiagnostic {
                line_number,
                kind: ParseDiagnosticKind::InvalidTimestamp,
            });
        }
    }

    fn process_record_object(
        &mut self,
        obj: &Map<String, Value>,
        line_number: usize,
        is_legacy_root: bool,
    ) {
        if let Some(rewind_id) = obj.get("$rewindTo").and_then(Value::as_str) {
            self.diagnostics.ignored_records += 1;
            self.apply_rewind(rewind_id);
            return;
        }

        if let Some(set_obj) = obj.get("$set").and_then(Value::as_object) {
            self.diagnostics.ignored_records += 1;
            self.merge_meta_object(set_obj, line_number);
            return;
        }

        if let Some(patch_val) = obj.get("$patch") {
            let outer_id = obj.get("id").and_then(Value::as_str);
            match patch_val {
                Value::Object(patch_obj) => {
                    self.apply_object_patch(outer_id, patch_obj, line_number);
                }
                Value::Array(ops) => {
                    self.apply_rfc6902_patch(outer_id, ops, line_number);
                }
                _ => {
                    self.diagnostics.ignored_records += 1;
                }
            }
            return;
        }

        if let Some(messages_arr) = obj.get("messages").and_then(Value::as_array) {
            self.merge_meta_object(obj, line_number);
            if !is_legacy_root {
                // Single-line legacy `ConversationRecord` passed through JSONL mode.
            }
            for item in messages_arr {
                if let Some(msg_obj) = item.as_object() {
                    self.process_record_object(msg_obj, line_number, false);
                } else {
                    self.diagnostics.ignored_records += 1;
                }
            }
            return;
        }

        if let Some(raw_type) = obj.get("type").and_then(Value::as_str) {
            if let Some(msg_type) = GeminiMsgType::from_raw(raw_type) {
                let msg = self.parse_message_object(obj, msg_type, line_number);
                self.upsert_message(msg);
            } else {
                self.diagnostics.ignored_records += 1;
                let safe_key = if is_safe_type_token(raw_type) {
                    raw_type
                } else {
                    "<redacted>"
                };
                *self
                    .diagnostics
                    .unknown_record_types
                    .entry(safe_key.to_string())
                    .or_insert(0) += 1;
            }
            return;
        }

        if obj.contains_key("sessionId")
            || obj.contains_key("projectHash")
            || obj.contains_key("startTime")
            || obj.contains_key("lastUpdated")
            || obj.contains_key("kind")
            || obj.contains_key("directories")
        {
            if !is_legacy_root {
                self.diagnostics.ignored_records += 1;
            }
            self.merge_meta_object(obj, line_number);
            return;
        }

        self.diagnostics.ignored_records += 1;
        *self
            .diagnostics
            .unknown_record_types
            .entry("<untyped>".to_string())
            .or_insert(0) += 1;
    }

    fn finish(
        mut self,
        source_path: Option<&Path>,
        options: &GeminiParseOptions,
    ) -> GeminiParseOutput {
        let session_id = self
            .meta
            .session_id
            .clone()
            .or_else(|| {
                source_path
                    .and_then(Path::file_stem)
                    .and_then(|s| s.to_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "unknown-gemini-session".to_string());

        let cwd_hash = resolve_gemini_raw_cwd(source_path, options.slug_registry.as_ref())
            .map(|raw_cwd| hash_cwd(&raw_cwd))
            .or_else(|| self.meta.fallback_cwd_hash.clone());

        let parent_session_id = options
            .parent_session_id
            .clone()
            .or_else(|| extract_gemini_subagent_parent_from_path(source_path));
        let sidechain =
            parent_session_id.is_some() || self.meta.kind.as_deref() == Some("subagent");

        let mut records = Vec::new();
        let mut pending_input_blocks: Vec<ContentBlock> = Vec::new();
        let mut pending_api_error = false;
        let mut pending_compaction = false;
        let mut last_ts: Option<Timestamp> = self.meta.start_time;
        let mut last_model: Option<String> = None;
        let mut turn_num: u32 = 0;

        for msg in self.messages {
            match msg.msg_type {
                GeminiMsgType::User => {
                    if let Some(bytes) = msg.content_bytes {
                        pending_input_blocks.push(ContentBlock {
                            kind: BlockKind::UserPrompt,
                            est_tokens: estimate_tokens(bytes),
                            bytes,
                            tool: None,
                            key_hash: None,
                            is_error: false,
                        });
                    }
                    if msg.timestamp.is_some() {
                        last_ts = msg.timestamp;
                    }
                }
                GeminiMsgType::Error => {
                    self.diagnostics.ignored_records += 1;
                    pending_api_error = true;
                }
                GeminiMsgType::Info | GeminiMsgType::Warning => {
                    self.diagnostics.ignored_records += 1;
                    if msg.mentions_compaction {
                        pending_compaction = true;
                    }
                }
                GeminiMsgType::Gemini => {
                    let Some(tok) = msg.tokens else {
                        self.diagnostics.ignored_records += 1;
                        continue;
                    };
                    let Some(ts) = msg
                        .timestamp
                        .or(last_ts)
                        .or(self.meta.last_updated)
                        .or(self.meta.start_time)
                    else {
                        self.diagnostics.issues.push(ParseDiagnostic {
                            line_number: 1,
                            kind: ParseDiagnosticKind::MissingField { field: "timestamp" },
                        });
                        continue;
                    };
                    last_ts = Some(ts);

                    let model = msg.model.or_else(|| last_model.clone());
                    if model.is_some() {
                        last_model.clone_from(&model);
                    }

                    let prompt_total = tok.input.saturating_add(tok.tool);
                    let output_total = tok.output.saturating_add(tok.thoughts);
                    let reasoning_tokens = (tok.thoughts > 0).then_some(tok.thoughts);
                    let (usage, _was_clamped) =
                        Usage::from_inclusive_clamped(InclusiveUsageInput {
                            input_tokens: prompt_total,
                            cache_write_tokens: 0,
                            cache_read_tokens: tok.cached,
                            output_tokens: output_total,
                            reasoning_tokens,
                            confidence: Confidence::Measured,
                            estimation_method: None,
                            agent_reported_cost_usd: None,
                        });

                    let mut added = mem::take(&mut pending_input_blocks);
                    for th_bytes in msg.thought_blocks {
                        if th_bytes > 0 {
                            added.push(ContentBlock {
                                kind: BlockKind::AssistantThinking,
                                est_tokens: estimate_tokens(th_bytes),
                                bytes: th_bytes,
                                tool: None,
                                key_hash: None,
                                is_error: false,
                            });
                        }
                    }
                    if let Some(text_bytes) = msg.content_bytes {
                        added.push(ContentBlock {
                            kind: BlockKind::AssistantText,
                            est_tokens: estimate_tokens(text_bytes),
                            bytes: text_bytes,
                            tool: None,
                            key_hash: None,
                            is_error: false,
                        });
                    }
                    for tc in msg.tool_calls {
                        added.push(ContentBlock {
                            kind: BlockKind::ToolUse,
                            est_tokens: estimate_tokens(tc.args_bytes),
                            bytes: tc.args_bytes,
                            tool: Some(tc.name.clone()),
                            key_hash: Some(tc.key_hash),
                            is_error: false,
                        });
                        if let Some(res_bytes) = tc.result_bytes {
                            pending_input_blocks.push(ContentBlock {
                                kind: BlockKind::ToolResult,
                                est_tokens: estimate_tokens(res_bytes),
                                bytes: res_bytes,
                                tool: Some(tc.name),
                                key_hash: None,
                                is_error: tc.is_error,
                            });
                        }
                    }

                    turn_num = turn_num.saturating_add(1);
                    let hints = TurnHints {
                        resume_start: false,
                        compaction: mem::take(&mut pending_compaction),
                        sidechain,
                        api_error: mem::take(&mut pending_api_error),
                        rate_limit_used_percent: None,
                        context_window_override: None,
                    };

                    records.push(TurnRecord {
                        agent: AgentId::GeminiCli,
                        session_id: session_id.clone(),
                        parent_session_id: parent_session_id.clone(),
                        turn: turn_num,
                        ts,
                        model,
                        cwd_hash: cwd_hash.clone(),
                        usage,
                        added,
                        hints,
                    });
                }
            }
        }

        GeminiParseOutput {
            records,
            diagnostics: self.diagnostics,
        }
    }
}

/// Extracts `(gemini_root, slug_dir, slug)` when `path` lies inside
/// `<gemini_root>/tmp/<slug>/chats/...`.
pub(crate) fn extract_gemini_slug_context(
    path: &Path,
) -> Option<(Option<PathBuf>, PathBuf, String)> {
    for ancestor in path.ancestors() {
        if ancestor.file_name().and_then(|n| n.to_str()) == Some("chats") {
            let slug_dir = ancestor.parent()?.to_path_buf();
            let slug = slug_dir.file_name()?.to_str()?.to_string();
            let gemini_root = slug_dir
                .parent()
                .filter(|p| p.file_name().and_then(|n| n.to_str()) == Some("tmp"))
                .and_then(Path::parent)
                .map(Path::to_path_buf);
            return Some((gemini_root, slug_dir, slug));
        }
    }
    None
}

/// Extracts `<parent_session_id>` if `source_path` is nested inside
/// `.../chats/<parent_session_id>/<subagent_file>`.
fn extract_gemini_subagent_parent_from_path(source_path: Option<&Path>) -> Option<String> {
    let path = source_path?;
    let parent_dir = path.parent()?;
    let grandparent = parent_dir.parent()?;
    if grandparent.file_name().and_then(|n| n.to_str()) == Some("chats") {
        parent_dir
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|s| !s.is_empty() && !s.starts_with('.'))
            .map(str::to_string)
    } else {
        None
    }
}

/// Resolves the raw project directory for a Gemini CLI transcript path using
/// `slug_registry`, `<slug_dir>/.project_root`, or `<gemini_root>/projects.json`.
pub(crate) fn resolve_gemini_raw_cwd(
    source_path: Option<&Path>,
    slug_registry: Option<&BTreeMap<String, String>>,
) -> Option<String> {
    let path = source_path?;
    let (gemini_root, slug_dir, slug) = extract_gemini_slug_context(path)?;

    if let Some(reg) = slug_registry {
        if let Some(cwd) = reg.get(&slug) {
            if !cwd.is_empty() {
                return Some(cwd.clone());
            }
        }
    }

    let marker = slug_dir.join(".project_root");
    if marker.is_file() {
        if let Ok(file) = open_readonly(&marker) {
            let mut raw = String::new();
            if file.take(16 * 1024).read_to_string(&mut raw).is_ok() {
                let trimmed = raw.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }

    if let Some(g_root) = gemini_root {
        if let Ok(reg) = read_gemini_slug_registry_at(&g_root) {
            if let Some(cwd) = reg.get(&slug) {
                if !cwd.is_empty() {
                    return Some(cwd.clone());
                }
            }
        }
    }

    None
}

fn parse_legacy_gemini_json<R: Read>(
    reader: R,
    source_path: Option<&Path>,
    options: &GeminiParseOptions,
) -> io::Result<GeminiParseOutput> {
    let mut raw_bytes = Vec::new();
    reader
        .take(MAX_LEGACY_JSON_BYTES + 1)
        .read_to_end(&mut raw_bytes)?;

    let mut recon = GeminiReconstructor::default();
    if raw_bytes.len() as u64 > MAX_LEGACY_JSON_BYTES {
        recon.diagnostics.issues.push(ParseDiagnostic {
            line_number: 1,
            kind: ParseDiagnosticKind::OversizedLine {
                byte_len: raw_bytes.len() as u64,
                limit: usize::try_from(MAX_LEGACY_JSON_BYTES).unwrap_or(usize::MAX),
            },
        });
        raw_bytes.clear();
        return Ok(recon.finish(source_path, options));
    }

    let Ok(raw_text) = std::str::from_utf8(&raw_bytes) else {
        recon.diagnostics.issues.push(ParseDiagnostic {
            line_number: 1,
            kind: ParseDiagnosticKind::InvalidUtf8,
        });
        raw_bytes.clear();
        return Ok(recon.finish(source_path, options));
    };

    match serde_json::from_str::<Value>(raw_text) {
        Ok(Value::Object(obj)) => {
            recon.process_record_object(&obj, 1, true);
        }
        Ok(_) | Err(_) => {
            recon.diagnostics.issues.push(ParseDiagnostic {
                line_number: 1,
                kind: ParseDiagnosticKind::MalformedJson,
            });
        }
    }
    raw_bytes.clear();

    Ok(recon.finish(source_path, options))
}

/// Decodes a Gemini CLI `.jsonl` or legacy `.json` stream from any [`BufRead`]
/// in bounded memory with zero raw text retention.
pub fn parse_gemini_reader<R: BufRead>(
    reader: R,
    source_path: Option<&Path>,
    options: &GeminiParseOptions,
) -> io::Result<GeminiParseOutput> {
    let is_json_ext = source_path
        .and_then(Path::extension)
        .and_then(|e| e.to_str())
        == Some("json");
    if is_json_ext {
        return parse_legacy_gemini_json(reader, source_path, options);
    }

    let max_line = options.max_line_bytes.unwrap_or(DEFAULT_MAX_LINE_BYTES);
    let mut line_reader = BoundedLineReader::with_max_line_bytes(reader, max_line);
    let mut recon = GeminiReconstructor::default();

    while let Some(outcome) = line_reader.next_line()? {
        match outcome {
            LineReadOutcome::Oversized {
                line_number,
                byte_len,
            } => {
                recon.diagnostics.issues.push(ParseDiagnostic {
                    line_number,
                    kind: ParseDiagnosticKind::OversizedLine {
                        byte_len,
                        limit: max_line,
                    },
                });
            }
            LineReadOutcome::InvalidUtf8 { line_number } => {
                recon.diagnostics.issues.push(ParseDiagnostic {
                    line_number,
                    kind: ParseDiagnosticKind::InvalidUtf8,
                });
            }
            LineReadOutcome::Line { line_number, text } => {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(trimmed) {
                    Ok(Value::Object(obj)) => {
                        recon.process_record_object(&obj, line_number, false);
                    }
                    Ok(_) | Err(_) => {
                        recon.diagnostics.issues.push(ParseDiagnostic {
                            line_number,
                            kind: ParseDiagnosticKind::MalformedJson,
                        });
                    }
                }
            }
        }
    }

    Ok(recon.finish(source_path, options))
}
