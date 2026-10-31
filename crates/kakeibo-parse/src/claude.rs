//! Read-only streaming Claude Code JSONL decoder (`PLAN.md` §5, §7, §10 T04/T12, `docs/formats/claude-code.md`).

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

use kakeibo_core::{
    AgentId, BlockKind, CacheWriteSplit, ContentBlock, DisjointUsageInput, Timestamp, TurnHints,
    TurnRecord, Usage,
};
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};

use crate::catalog::open_readonly;
use crate::hash::{estimate_tokens, hash_block_content, hash_cwd, measure_and_hash_tool_input};
use crate::line_reader::{
    open_seeked_counting_reader, BoundedLineReader, IncrementalParseResult, LineReadOutcome,
    ParseDiagnostic, ParseDiagnosticKind, ParseDiagnostics, ReadCounter, DEFAULT_MAX_LINE_BYTES,
};
use crate::prefilter::{classify_claude_line, BorrowedClaudeEnvelope, ClaudeLineClass};

const CLAUDE_CHECKPOINT_SCHEMA_V1: u32 = 1;

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

/// Incrementally decodes or resumes a Claude Code JSONL file from `start_offset` and
/// an optional serialized checkpoint (`PLAN.md` §7, §10 T12).
pub fn parse_claude_file_incremental(
    path: &Path,
    options: &ClaudeParseOptions,
    start_offset: u64,
    prev_checkpoint_json: Option<&str>,
    counter: Option<&ReadCounter>,
) -> io::Result<IncrementalParseResult<ClaudeParseOutput>> {
    let parsed_checkpoint = if start_offset > 0 {
        prev_checkpoint_json
            .and_then(|raw| serde_json::from_str::<ClaudeStreamCheckpoint>(raw).ok())
            .filter(|cp| cp.schema_version == CLAUDE_CHECKPOINT_SCHEMA_V1)
    } else {
        None
    };

    let effective_offset = if parsed_checkpoint.is_some() {
        start_offset
    } else {
        0
    };

    let (reader, fell_back_to_zero) = open_seeked_counting_reader(path, effective_offset, counter)?;
    if fell_back_to_zero || effective_offset == 0 {
        parse_claude_reader_incremental(reader, options, 0, None)
    } else {
        parse_claude_reader_incremental(reader, options, effective_offset, parsed_checkpoint)
    }
}

#[derive(Deserialize)]
struct RawSystemLine<'a> {
    #[serde(rename = "sessionId", alias = "session_id", default, borrow)]
    session_id: Option<Cow<'a, str>>,
    #[serde(rename = "agentId", default, borrow)]
    agent_id: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    subtype: Option<Cow<'a, str>>,
}

/// Zero-allocation string byte-length extractor that tolerates non-string JSON values.
#[derive(Debug, Clone, Copy, Default)]
struct OptionalStringBytes(Option<u64>);

impl<'de> Deserialize<'de> for OptionalStringBytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct LenVisitor;

        impl<'de> Visitor<'de> for LenVisitor {
            type Value = OptionalStringBytes;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(OptionalStringBytes(Some(v.len() as u64)))
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(OptionalStringBytes(None))
            }

            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(OptionalStringBytes(None))
            }

            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(OptionalStringBytes(None))
            }

            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(OptionalStringBytes(None))
            }

            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(OptionalStringBytes(None))
            }

            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(OptionalStringBytes(None))
            }

            fn visit_some<D2: Deserializer<'de>>(self, d: D2) -> Result<Self::Value, D2::Error> {
                d.deserialize_any(self)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(OptionalStringBytes(None))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(OptionalStringBytes(None))
            }
        }

        deserializer.deserialize_any(LenVisitor)
    }
}

/// Lenient boolean extractor matching `Value::as_bool().unwrap_or(false)`.
#[derive(Debug, Clone, Copy, Default)]
struct LenientBool(bool);

impl<'de> Deserialize<'de> for LenientBool {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct BoolVisitor;

        impl<'de> Visitor<'de> for BoolVisitor {
            type Value = LenientBool;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }

            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(LenientBool(v))
            }

            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(LenientBool(false))
            }

            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(LenientBool(false))
            }

            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(LenientBool(false))
            }

            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(LenientBool(false))
            }

            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientBool(false))
            }

            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientBool(false))
            }

            fn visit_some<D2: Deserializer<'de>>(self, d: D2) -> Result<Self::Value, D2::Error> {
                d.deserialize_any(self)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(LenientBool(false))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(LenientBool(false))
            }
        }

        deserializer.deserialize_any(BoolVisitor)
    }
}

/// Zero-copy `tool_result.content` byte-length measurer (`0` heap allocations on string payloads).
#[derive(Debug, Clone, Copy, Default)]
struct MeasuredToolResultBody(u64);

impl<'de> Deserialize<'de> for MeasuredToolResultBody {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ToolBodyVisitor;

        impl<'de> Visitor<'de> for ToolBodyVisitor {
            type Value = MeasuredToolResultBody;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("tool_result content payload")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(MeasuredToolResultBody(v.len() as u64))
            }

            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(MeasuredToolResultBody(0))
            }

            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(MeasuredToolResultBody(0))
            }

            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
                let val = serde_json::Value::Bool(v);
                Ok(MeasuredToolResultBody(measure_tool_result_body(Some(&val))))
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                let val = serde_json::Value::from(v);
                Ok(MeasuredToolResultBody(measure_tool_result_body(Some(&val))))
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                let val = serde_json::Value::from(v);
                Ok(MeasuredToolResultBody(measure_tool_result_body(Some(&val))))
            }

            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                let val = serde_json::Value::from(v);
                Ok(MeasuredToolResultBody(measure_tool_result_body(Some(&val))))
            }

            fn visit_some<D2: Deserializer<'de>>(self, d: D2) -> Result<Self::Value, D2::Error> {
                d.deserialize_any(self)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut total_bytes: u64 = 0;
                while let Some(elem) = seq.next_element::<ToolResultArrayPart>()? {
                    total_bytes = total_bytes.saturating_add(elem.0);
                }
                Ok(MeasuredToolResultBody(total_bytes))
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                let val =
                    serde_json::Value::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(MeasuredToolResultBody(measure_tool_result_body(Some(&val))))
            }
        }

        deserializer.deserialize_any(ToolBodyVisitor)
    }
}

/// Element inside a multi-part `tool_result.content` array (`{"type":"text","text":"..."}` fast path).
struct ToolResultArrayPart(u64);

impl<'de> Deserialize<'de> for ToolResultArrayPart {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let val = serde_json::Value::deserialize(deserializer)?;
        let bytes = if let Some(text) = val.get("text").and_then(serde_json::Value::as_str) {
            text.len() as u64
        } else {
            measure_and_hash_tool_input("tool_result_item", &val).0
        };
        Ok(ToolResultArrayPart(bytes))
    }
}

#[derive(Debug)]
enum ParsedUserBlock {
    Text {
        bytes: u64,
    },
    ToolResult {
        tool_use_id: Option<String>,
        is_error: bool,
        bytes: u64,
    },
    Ignore,
}

impl<'de> Deserialize<'de> for ParsedUserBlock {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ItemVisitor;

        impl<'de> Visitor<'de> for ItemVisitor {
            type Value = ParsedUserBlock;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("user content array element")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut block_type: Option<String> = None;
                let mut text_bytes: Option<u64> = None;
                let mut tool_use_id: Option<String> = None;
                let mut is_error = false;
                let mut content_bytes: u64 = 0;

                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    match key.as_ref() {
                        "type" => {
                            block_type = map.next_value::<Option<String>>().unwrap_or(None);
                        }
                        "text" => {
                            text_bytes = map.next_value::<OptionalStringBytes>()?.0;
                        }
                        "tool_use_id" => {
                            tool_use_id = map.next_value::<Option<String>>().unwrap_or(None);
                        }
                        "is_error" => {
                            is_error = map.next_value::<LenientBool>()?.0;
                        }
                        "content" => {
                            content_bytes = map.next_value::<MeasuredToolResultBody>()?.0;
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }

                Ok(match block_type.as_deref() {
                    Some("text") => ParsedUserBlock::Text {
                        bytes: text_bytes.unwrap_or(0),
                    },
                    Some("tool_result") => ParsedUserBlock::ToolResult {
                        tool_use_id,
                        is_error,
                        bytes: content_bytes,
                    },
                    _ => ParsedUserBlock::Ignore,
                })
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(ParsedUserBlock::Ignore)
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(ParsedUserBlock::Ignore)
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(ParsedUserBlock::Ignore)
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(ParsedUserBlock::Ignore)
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(ParsedUserBlock::Ignore)
            }
            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(ParsedUserBlock::Ignore)
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(ParsedUserBlock::Ignore)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(ParsedUserBlock::Ignore)
            }
        }

        deserializer.deserialize_any(ItemVisitor)
    }
}

#[derive(Debug)]
enum MeasuredUserContent {
    PlainString(u64),
    Blocks(Vec<ParsedUserBlock>),
    Other,
}

impl<'de> Deserialize<'de> for MeasuredUserContent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UserContentVisitor;

        impl<'de> Visitor<'de> for UserContentVisitor {
            type Value = MeasuredUserContent;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("user message content")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(MeasuredUserContent::PlainString(v.len() as u64))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut blocks = Vec::new();
                while let Some(item) = seq.next_element::<ParsedUserBlock>()? {
                    if !matches!(item, ParsedUserBlock::Ignore) {
                        blocks.push(item);
                    }
                }
                Ok(MeasuredUserContent::Blocks(blocks))
            }

            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(MeasuredUserContent::Other)
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(MeasuredUserContent::Other)
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(MeasuredUserContent::Other)
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(MeasuredUserContent::Other)
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(MeasuredUserContent::Other)
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(MeasuredUserContent::Other)
            }
            fn visit_some<D2: Deserializer<'de>>(self, d: D2) -> Result<Self::Value, D2::Error> {
                d.deserialize_any(self)
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(MeasuredUserContent::Other)
            }
        }

        deserializer.deserialize_any(UserContentVisitor)
    }
}

#[derive(Deserialize)]
struct RawUserLine<'a> {
    #[serde(default, borrow)]
    uuid: Option<Cow<'a, str>>,
    #[serde(rename = "parentUuid", default)]
    parent_uuid: Option<Option<String>>,
    #[serde(rename = "sessionId", alias = "session_id", default, borrow)]
    session_id: Option<Cow<'a, str>>,
    #[serde(rename = "agentId", default, borrow)]
    agent_id: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    cwd: Option<Cow<'a, str>>,
    #[serde(rename = "isCompactSummary", default)]
    is_compact_summary: bool,
    #[serde(rename = "sourceToolUseID", default, borrow)]
    source_tool_use_id: Option<Cow<'a, str>>,
    message: Option<RawUserMessage>,
}

#[derive(Deserialize)]
struct RawUserMessage {
    content: Option<MeasuredUserContent>,
}

/// Zero-copy string length + SHA-256 prefix-8 hasher for `"text"` and `"thinking"` blocks.
struct HashedField<const IS_THINKING: bool> {
    bytes: u64,
    digest: [u8; 8],
}

impl<const IS_THINKING: bool> Default for HashedField<IS_THINKING> {
    fn default() -> Self {
        let tag = if IS_THINKING { "thinking" } else { "text" };
        Self {
            bytes: 0,
            digest: hash_block_content(tag, ""),
        }
    }
}

impl<'de, const IS_THINKING: bool> Deserialize<'de> for HashedField<IS_THINKING> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct HashVisitor<const T: bool>;

        impl<'de, const T: bool> Visitor<'de> for HashVisitor<T> {
            type Value = HashedField<T>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("assistant block string")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                let tag = if T { "thinking" } else { "text" };
                Ok(HashedField {
                    bytes: v.len() as u64,
                    digest: hash_block_content(tag, v),
                })
            }

            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(HashedField::default())
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(HashedField::default())
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(HashedField::default())
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(HashedField::default())
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(HashedField::default())
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(HashedField::default())
            }
            fn visit_some<D2: Deserializer<'de>>(self, d: D2) -> Result<Self::Value, D2::Error> {
                d.deserialize_any(self)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(HashedField::default())
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(HashedField::default())
            }
        }

        deserializer.deserialize_any(HashVisitor::<IS_THINKING>)
    }
}

enum ParsedAssistantItem {
    Thinking {
        bytes: u64,
        digest: [u8; 8],
    },
    Text {
        bytes: u64,
        digest: [u8; 8],
    },
    ToolUse {
        id: Option<String>,
        name: String,
        bytes: u64,
        key_hash: String,
    },
    Ignore,
}

impl<'de> Deserialize<'de> for ParsedAssistantItem {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct AsstItemVisitor;

        impl<'de> Visitor<'de> for AsstItemVisitor {
            type Value = ParsedAssistantItem;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("assistant content block")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut block_type: Option<String> = None;
                let mut thinking_field: Option<HashedField<true>> = None;
                let mut text_field: Option<HashedField<false>> = None;
                let mut id: Option<String> = None;
                let mut name: Option<String> = None;
                let mut input_val: Option<serde_json::Value> = None;

                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    match key.as_ref() {
                        "type" => {
                            block_type = map.next_value::<Option<String>>().unwrap_or(None);
                        }
                        "thinking" => {
                            thinking_field = Some(map.next_value::<HashedField<true>>()?);
                        }
                        "text" => {
                            text_field = Some(map.next_value::<HashedField<false>>()?);
                        }
                        "id" => {
                            id = map.next_value::<Option<String>>().unwrap_or(None);
                        }
                        "name" => {
                            name = map.next_value::<Option<String>>().unwrap_or(None);
                        }
                        "input" => {
                            input_val = Some(map.next_value::<serde_json::Value>()?);
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }

                Ok(match block_type.as_deref() {
                    Some("thinking") => {
                        let f = thinking_field.unwrap_or_default();
                        ParsedAssistantItem::Thinking {
                            bytes: f.bytes,
                            digest: f.digest,
                        }
                    }
                    Some("text") => {
                        let f = text_field.unwrap_or_default();
                        ParsedAssistantItem::Text {
                            bytes: f.bytes,
                            digest: f.digest,
                        }
                    }
                    Some("tool_use") => {
                        let tool_name = name.unwrap_or_else(|| "unknown".to_string());
                        let tool_id = id.filter(|s| !s.is_empty());
                        let empty_obj = serde_json::Value::Object(serde_json::Map::new());
                        let inp = input_val.as_ref().unwrap_or(&empty_obj);
                        let (bytes, key_hash) = measure_and_hash_tool_input(&tool_name, inp);
                        ParsedAssistantItem::ToolUse {
                            id: tool_id,
                            name: tool_name,
                            bytes,
                            key_hash,
                        }
                    }
                    _ => ParsedAssistantItem::Ignore,
                })
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(ParsedAssistantItem::Ignore)
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(ParsedAssistantItem::Ignore)
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(ParsedAssistantItem::Ignore)
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(ParsedAssistantItem::Ignore)
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(ParsedAssistantItem::Ignore)
            }
            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(ParsedAssistantItem::Ignore)
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(ParsedAssistantItem::Ignore)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(ParsedAssistantItem::Ignore)
            }
        }

        deserializer.deserialize_any(AsstItemVisitor)
    }
}

#[derive(Deserialize)]
struct RawAssistantLine<'a> {
    #[serde(default, borrow)]
    uuid: Option<Cow<'a, str>>,
    #[serde(rename = "sessionId", alias = "session_id", default, borrow)]
    session_id: Option<Cow<'a, str>>,
    #[serde(rename = "agentId", default, borrow)]
    agent_id: Option<Cow<'a, str>>,
    #[serde(rename = "requestId", default, borrow)]
    request_id: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    timestamp: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    cwd: Option<Cow<'a, str>>,
    #[serde(rename = "isSidechain", default)]
    is_sidechain: bool,
    #[serde(rename = "isApiErrorMessage", default)]
    is_api_error_message: bool,
    #[serde(rename = "apiError", default)]
    api_error: Option<IgnoredAny>,
    #[serde(default)]
    error: Option<IgnoredAny>,
    message: Option<RawAssistantMessage>,
}

#[derive(Deserialize)]
struct RawAssistantMessage {
    id: Option<String>,
    model: Option<String>,
    content: Option<Vec<ParsedAssistantItem>>,
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
enum BlockFingerprint {
    ToolUseId(String),
    ContentDigest {
        kind: BlockKind,
        digest: [u8; 8],
        bytes: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClaudeStreamCheckpoint {
    schema_version: u32,
    line_number: usize,
    diagnostics: ParseDiagnostics,
    session_order: Vec<String>,
    sessions: BTreeMap<String, SessionState>,
    last_active_session_id: Option<String>,
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

fn measure_tool_result_body(body: Option<&serde_json::Value>) -> u64 {
    let Some(val) = body else {
        return 0;
    };
    match val {
        serde_json::Value::String(s) => s.len() as u64,
        serde_json::Value::Array(items) => {
            let mut total_bytes: u64 = 0;
            for item in items {
                if let Some(text) = item.get("text").and_then(serde_json::Value::as_str) {
                    total_bytes = total_bytes.saturating_add(text.len() as u64);
                } else {
                    let (b, _) = measure_and_hash_tool_input("tool_result_item", item);
                    total_bytes = total_bytes.saturating_add(b);
                }
            }
            total_bytes
        }
        other => measure_and_hash_tool_input("tool_result_obj", other).0,
    }
}

fn extract_user_blocks(
    content: MeasuredUserContent,
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
        MeasuredUserContent::PlainString(bytes) => {
            out.push(ContentBlock {
                kind: text_kind,
                est_tokens: estimate_tokens(bytes),
                bytes,
                tool: None,
                key_hash: None,
                is_error: false,
            });
        }
        MeasuredUserContent::Blocks(items) => {
            for item in items {
                match item {
                    ParsedUserBlock::Text { bytes } => {
                        out.push(ContentBlock {
                            kind: text_kind,
                            est_tokens: estimate_tokens(bytes),
                            bytes,
                            tool: None,
                            key_hash: None,
                            is_error: false,
                        });
                    }
                    ParsedUserBlock::ToolResult {
                        tool_use_id,
                        is_error,
                        bytes,
                    } => {
                        let resolved_id = tool_use_id.as_deref().or(fallback_tool_use_id);
                        let (tool, key_hash) = resolved_id
                            .and_then(|id| tool_registry.get(id))
                            .map_or((None, None), |(name, kh)| {
                                (Some(name.clone()), Some(kh.clone()))
                            });
                        out.push(ContentBlock {
                            kind: BlockKind::ToolResult,
                            est_tokens: estimate_tokens(bytes),
                            bytes,
                            tool,
                            key_hash,
                            is_error,
                        });
                    }
                    ParsedUserBlock::Ignore => {}
                }
            }
        }
        MeasuredUserContent::Other => {}
    }
}

struct ExtractedAssistantBlock {
    fingerprint: BlockFingerprint,
    tool_use_id: Option<String>,
    block: ContentBlock,
}

fn extract_assistant_blocks(
    items: Vec<ParsedAssistantItem>,
    tool_registry: &mut BTreeMap<String, (String, String)>,
) -> Vec<ExtractedAssistantBlock> {
    let mut extracted = Vec::with_capacity(items.len());
    for item in items {
        match item {
            ParsedAssistantItem::Thinking { bytes, digest } => {
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
            ParsedAssistantItem::Text { bytes, digest } => {
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
            ParsedAssistantItem::ToolUse {
                id,
                name,
                bytes,
                key_hash,
            } => {
                if let Some(ref tool_id) = id {
                    tool_registry.insert(tool_id.clone(), (name.clone(), key_hash.clone()));
                }

                let fingerprint = match &id {
                    Some(tool_id) => BlockFingerprint::ToolUseId(tool_id.clone()),
                    None => BlockFingerprint::ContentDigest {
                        kind: BlockKind::ToolUse,
                        digest: hash_block_content(&name, &key_hash),
                        bytes,
                    },
                };

                extracted.push(ExtractedAssistantBlock {
                    fingerprint,
                    tool_use_id: id,
                    block: ContentBlock {
                        kind: BlockKind::ToolUse,
                        est_tokens: estimate_tokens(bytes),
                        bytes,
                        tool: Some(name),
                        key_hash: Some(key_hash),
                        is_error: false,
                    },
                });
            }
            ParsedAssistantItem::Ignore => {}
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

fn record_non_turn_type(other: &str, diagnostics: &mut ParseDiagnostics) {
    diagnostics.ignored_records += 1;
    if !KNOWN_NON_TURN_TYPES.contains(&other) {
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

fn materialize_claude_output(
    session_order: &[String],
    sessions: &BTreeMap<String, SessionState>,
    base_diagnostics: &ParseDiagnostics,
) -> ClaudeParseOutput {
    let mut diagnostics = base_diagnostics.clone();
    let mut records = Vec::new();

    for sid in session_order {
        let Some(state) = sessions.get(sid) else {
            continue;
        };
        let mut turn_ordinal: u32 = 0;
        for pending in &state.turns {
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

            let Ok(usage) = Usage::from_disjoint(disjoint) else {
                diagnostics.issues.push(ParseDiagnostic {
                    line_number: pending.line_number,
                    kind: ParseDiagnosticKind::UsageOverflow,
                });
                continue;
            };

            turn_ordinal = turn_ordinal.saturating_add(1);
            records.push(TurnRecord {
                agent: AgentId::ClaudeCode,
                session_id: state.session_id.clone(),
                parent_session_id: state.parent_session_id.clone(),
                turn: turn_ordinal,
                ts: pending.ts,
                model: pending.model.clone(),
                cwd_hash: pending.cwd_hash.clone(),
                usage,
                added: pending.added.clone(),
                hints: pending.hints.clone(),
            });
        }
    }

    ClaudeParseOutput {
        records,
        diagnostics,
    }
}

/// Streams a buffered Claude Code JSONL reader into deduplicated [`TurnRecord`] items.
pub fn parse_claude_reader<R: BufRead>(
    reader: R,
    options: &ClaudeParseOptions,
) -> io::Result<ClaudeParseOutput> {
    Ok(parse_claude_reader_incremental(reader, options, 0, None)?.output)
}

fn parse_claude_reader_incremental<R: BufRead>(
    reader: R,
    options: &ClaudeParseOptions,
    start_offset: u64,
    checkpoint: Option<ClaudeStreamCheckpoint>,
) -> io::Result<IncrementalParseResult<ClaudeParseOutput>> {
    let max_line = options.max_line_bytes.unwrap_or(DEFAULT_MAX_LINE_BYTES);
    let (start_line, mut diagnostics, mut session_order, mut sessions, mut last_active_session_id) =
        match checkpoint {
            Some(cp) => (
                cp.line_number,
                cp.diagnostics,
                cp.session_order,
                cp.sessions,
                cp.last_active_session_id,
            ),
            None => (
                0,
                ParseDiagnostics::default(),
                Vec::new(),
                BTreeMap::new(),
                None,
            ),
        };

    let mut line_reader =
        BoundedLineReader::with_offset_and_line(reader, max_line, start_offset, start_line);
    let mut trailing_partial_snapshot: Option<(u64, ClaudeStreamCheckpoint)> = None;

    while let Some((meta, outcome)) = line_reader.next_line_with_meta()? {
        if !meta.ended_with_newline && trailing_partial_snapshot.is_none() {
            trailing_partial_snapshot = Some((
                meta.offset_before,
                ClaudeStreamCheckpoint {
                    schema_version: CLAUDE_CHECKPOINT_SCHEMA_V1,
                    line_number: meta.line_number.saturating_sub(1),
                    diagnostics: diagnostics.clone(),
                    session_order: session_order.clone(),
                    sessions: sessions.clone(),
                    last_active_session_id: last_active_session_id.clone(),
                },
            ));
        }

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

        let line_class = classify_claude_line(trimmed);
        let envelope: BorrowedClaudeEnvelope<'_> = match serde_json::from_str(trimmed) {
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
                diagnostics.cli_versions.insert(ver.to_string());
            }
        }

        let Some(rec_type) = envelope.record_type.as_deref() else {
            diagnostics.ignored_records += 1;
            continue;
        };

        if line_class == ClaudeLineClass::NonTurnFastPath {
            record_non_turn_type(rec_type, &mut diagnostics);
            continue;
        }

        match rec_type {
            "user" => {
                let raw: RawUserLine<'_> = match serde_json::from_str(trimmed) {
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
                    if !uuid.is_empty() && !state.seen_event_uuids.insert(uuid.to_string()) {
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
                    if let Some(content) = msg.content {
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
                let raw: RawAssistantLine<'_> = match serde_json::from_str(trimmed) {
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
                    if !uuid.is_empty() && !state.seen_event_uuids.insert(uuid.to_string()) {
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
                    msg.content.unwrap_or_default(),
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
                if let Ok(sys) = serde_json::from_str::<RawSystemLine<'_>>(trimmed) {
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
            other => {
                record_non_turn_type(other, &mut diagnostics);
            }
        }
    }

    let output = materialize_claude_output(&session_order, &sessions, &diagnostics);
    let (committed_offset, committed_checkpoint) = match trailing_partial_snapshot {
        Some(snap) => snap,
        None => (
            line_reader.bytes_consumed(),
            ClaudeStreamCheckpoint {
                schema_version: CLAUDE_CHECKPOINT_SCHEMA_V1,
                line_number: line_reader.line_number(),
                diagnostics,
                session_order,
                sessions,
                last_active_session_id,
            },
        ),
    };

    let checkpoint_json = serde_json::to_string(&committed_checkpoint).ok();
    Ok(IncrementalParseResult {
        output,
        committed_offset,
        checkpoint_json,
    })
}
