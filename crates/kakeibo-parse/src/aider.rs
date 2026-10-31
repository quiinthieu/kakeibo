//! Read-only streaming Aider `.aider.chat.history.md` and `--analytics-log` JSONL
//! decoders (`PLAN.md` §0 #4–#6, §5, §6.2, §10 T11, `docs/formats/aider.md`).
//!
//! All user prompts and assistant responses are reduced immediately to UTF-8 byte
//! lengths and dropped. Because `.aider.chat.history.md` rounds token figures (`k`/`M`)
//! and `--analytics-log` omits prompt-cache read/write splits, every decoded Aider
//! turn carries [`Confidence::Estimated`].

use std::io::{self, BufRead, BufReader};
use std::mem;
use std::path::Path;

use kakeibo_core::{
    AgentId, BlockKind, Confidence, ContentBlock, DisjointUsageInput, Timestamp, TurnHints,
    TurnRecord, Usage,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::catalog::open_readonly;
use crate::hash::{estimate_tokens, hash_cwd};
use crate::line_reader::{
    open_seeked_counting_reader, BoundedLineReader, IncrementalParseResult, LineReadOutcome,
    ParseDiagnostic, ParseDiagnosticKind, ParseDiagnostics, ReadCounter, DEFAULT_MAX_LINE_BYTES,
};
use crate::prefilter::{classify_aider_analytics_line, AiderAnalyticsClass, BorrowedAiderEnvelope};

const AIDER_HISTORY_CHECKPOINT_SCHEMA_V1: u32 = 1;
const AIDER_ANALYTICS_CHECKPOINT_SCHEMA_V1: u32 = 1;
const EST_METHOD_HISTORY: &str = "aider_history_rounded";
const EST_METHOD_ANALYTICS: &str = "aider_analytics_no_cache_split";

/// Decoded [`TurnRecord`] batch and per-session diagnostics for a single Aider session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiderSessionBatch {
    pub session_id: String,
    pub records: Vec<TurnRecord>,
    pub diagnostics: ParseDiagnostics,
}

/// Decoded sessions and stream-level diagnostics produced from an Aider history or
/// analytics file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AiderParseOutput {
    pub sessions: Vec<AiderSessionBatch>,
    pub diagnostics: ParseDiagnostics,
}

impl AiderParseOutput {
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

fn hex_encode_prefix8(digest: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(16);
    for &byte in &digest[..8] {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0F)]));
    }
    out
}

fn is_safe_version_token(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 48
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+' | b'_'))
}

fn derive_history_session_id(
    started_ts: &str,
    ordinal: usize,
    model: &str,
    fresh_in: u64,
    cache_write: u64,
    cache_read: u64,
    output: u64,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"aider-history\0");
    hasher.update(started_ts.as_bytes());
    hasher.update(b"\0");
    hasher.update(ordinal.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(format!("{model}:{fresh_in}:{cache_write}:{cache_read}:{output}").as_bytes());
    let digest = hasher.finalize();
    format!("aider_hist_{}", hex_encode_prefix8(&digest))
}

fn derive_analytics_session_id(
    started_ts: &str,
    ordinal: usize,
    user_id: &str,
    model: &str,
    prompt_tokens: u64,
    output: u64,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"aider-analytics\0");
    hasher.update(started_ts.as_bytes());
    hasher.update(b"\0");
    hasher.update(ordinal.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(user_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(format!("{model}:{prompt_tokens}:{output}").as_bytes());
    let digest = hasher.finalize();
    format!("aider_ana_{}", hex_encode_prefix8(&digest))
}

fn unix_epoch_ts() -> Timestamp {
    Timestamp::from_unix_timestamp(0).expect("unix epoch timestamp")
}

/// Parses `# aider chat started at YYYY-MM-DD HH:MM:SS` into a UTC [`Timestamp`].
fn parse_aider_banner_timestamp(raw: &str) -> Option<Timestamp> {
    let trimmed = raw.trim();
    if let Ok(ts) = Timestamp::parse_rfc3339(trimmed) {
        return Some(ts);
    }
    if let Some((date_part, time_part)) = trimmed.split_once(' ') {
        let time_clean = time_part.trim();
        let candidate = if time_clean.ends_with('Z') || time_clean.contains('+') {
            format!("{date_part}T{time_clean}")
        } else {
            format!("{date_part}T{time_clean}Z")
        };
        if let Ok(ts) = Timestamp::parse_rfc3339(&candidate) {
            return Some(ts);
        }
    }
    None
}

/// Parses an Aider token count token such as `"12k"`, `"9.5k"`, `"1.25M"`, `"1,240"`, or `"850"`
/// using exact integer decimal scaling.
fn parse_aider_token_count(raw: &str) -> Option<u64> {
    let cleaned: String = raw.trim().chars().filter(|&c| c != ',').collect();
    if cleaned.is_empty() {
        return None;
    }

    let (num_str, scale_digits): (&str, u32) = if let Some(stem) = cleaned.strip_suffix(['k', 'K'])
    {
        (stem, 3)
    } else if let Some(stem) = cleaned.strip_suffix(['m', 'M']) {
        (stem, 6)
    } else {
        (cleaned.as_str(), 0)
    };

    if num_str.is_empty() {
        return None;
    }

    let scale = 10_u128.pow(scale_digits);
    if let Some((whole_str, frac_str)) = num_str.split_once('.') {
        if scale_digits == 0
            || whole_str.is_empty()
            || frac_str.is_empty()
            || !whole_str.bytes().all(|b| b.is_ascii_digit())
            || !frac_str.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let whole: u128 = whole_str.parse().ok()?;
        let base = whole.checked_mul(scale)?;
        let sd = scale_digits as usize;
        let frac_scaled = if frac_str.len() <= sd {
            let mut padded = String::with_capacity(sd);
            padded.push_str(frac_str);
            for _ in 0..(sd - frac_str.len()) {
                padded.push('0');
            }
            padded.parse::<u128>().ok()?
        } else {
            let head: u128 = frac_str[..sd].parse().ok()?;
            let round_up = u128::from(frac_str.as_bytes()[sd] >= b'5');
            head.checked_add(round_up)?
        };
        u64::try_from(base.checked_add(frac_scaled)?).ok()
    } else {
        if !num_str.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let whole: u128 = num_str.parse().ok()?;
        u64::try_from(whole.checked_mul(scale)?).ok()
    }
}

#[derive(Debug, Clone, Copy)]
struct ParsedTokensSummary {
    sent: u64,
    cache_write: u64,
    cache_read: u64,
    received: u64,
    message_cost_usd: Option<f64>,
}

fn parse_tokens_summary_line(after_prefix: &str) -> Option<ParsedTokensSummary> {
    let (tokens_part, cost_part_opt) = match after_prefix.split_once("Cost:") {
        Some((lhs, rhs)) => (lhs.trim(), Some(rhs.trim())),
        None => (after_prefix.trim(), None),
    };
    let tokens_trimmed = tokens_part.trim_end_matches('.').trim();
    if tokens_trimmed.is_empty() {
        return None;
    }

    let mut sent: Option<u64> = None;
    let mut cache_write: u64 = 0;
    let mut cache_read: u64 = 0;
    let mut received: Option<u64> = None;

    for raw_clause in tokens_trimmed.split(", ") {
        let clause = raw_clause.trim().trim_end_matches('.').trim();
        if let Some(num) = clause.strip_suffix(" sent") {
            sent = Some(parse_aider_token_count(num)?);
        } else if let Some(num) = clause.strip_suffix(" cache write") {
            cache_write = parse_aider_token_count(num)?;
        } else if let Some(num) = clause.strip_suffix(" cache hit") {
            cache_read = parse_aider_token_count(num)?;
        } else {
            let num = clause.strip_suffix(" received")?;
            received = Some(parse_aider_token_count(num)?);
        }
    }

    let sent = sent?;
    let received = received?;

    let mut message_cost_usd: Option<f64> = None;
    if let Some(cost_part) = cost_part_opt {
        for raw_seg in cost_part.split(',') {
            let seg = raw_seg.trim().trim_end_matches('.').trim();
            if let Some(amount_raw) = seg.strip_suffix(" message") {
                let amount_clean = amount_raw
                    .trim()
                    .strip_prefix('$')
                    .unwrap_or(amount_raw.trim());
                if let Ok(val) = amount_clean.parse::<f64>() {
                    if val.is_finite() && val > 0.0 {
                        message_cost_usd = Some(val);
                    }
                }
            }
        }
    }

    Some(ParsedTokensSummary {
        sent,
        cache_write,
        cache_read,
        received,
        message_cost_usd,
    })
}

fn parse_main_model_slug(after_prefix: &str) -> Option<String> {
    let token = after_prefix
        .split_whitespace()
        .next()?
        .trim_end_matches(',')
        .trim();
    (!token.is_empty()).then(|| token.to_string())
}

fn cwd_hash_from_parent(path: &Path) -> Option<String> {
    path.parent()
        .and_then(Path::to_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(hash_cwd)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingTurnAccumulator {
    in_prompt_block: bool,
    saw_prompt_line: bool,
    user_prompt_bytes: u64,
    saw_asst_line: bool,
    asst_bytes: u64,
    pending_blank_newlines: u64,
}

impl PendingTurnAccumulator {
    const fn new() -> Self {
        Self {
            in_prompt_block: false,
            saw_prompt_line: false,
            user_prompt_bytes: 0,
            saw_asst_line: false,
            asst_bytes: 0,
            pending_blank_newlines: 0,
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn push_prompt_line(&mut self, payload: &str) {
        if !self.in_prompt_block && (self.saw_prompt_line || self.saw_asst_line) {
            // A fresh user prompt started before any `> Tokens:` line (e.g. cancelled prompt);
            // replace the abandoned prompt accumulator with the new prompt.
            self.user_prompt_bytes = 0;
            self.saw_prompt_line = false;
            self.asst_bytes = 0;
            self.saw_asst_line = false;
            self.pending_blank_newlines = 0;
        }
        if self.saw_prompt_line {
            self.user_prompt_bytes = self.user_prompt_bytes.saturating_add(1);
        }
        self.user_prompt_bytes = self.user_prompt_bytes.saturating_add(payload.len() as u64);
        self.saw_prompt_line = true;
        self.in_prompt_block = true;
    }

    fn push_assistant_or_blank(&mut self, line: &str) {
        self.in_prompt_block = false;
        if line.is_empty() {
            if self.saw_asst_line {
                self.pending_blank_newlines = self.pending_blank_newlines.saturating_add(1);
            }
            return;
        }
        if self.saw_asst_line {
            self.asst_bytes = self
                .asst_bytes
                .saturating_add(1)
                .saturating_add(self.pending_blank_newlines);
            self.pending_blank_newlines = 0;
        }
        self.asst_bytes = self.asst_bytes.saturating_add(line.len() as u64);
        self.saw_asst_line = true;
    }

    fn take_blocks(&mut self) -> Vec<ContentBlock> {
        let mut blocks = Vec::new();
        if self.saw_prompt_line && self.user_prompt_bytes > 0 {
            let bytes = self.user_prompt_bytes;
            blocks.push(ContentBlock {
                kind: BlockKind::UserPrompt,
                tool: None,
                bytes,
                est_tokens: estimate_tokens(bytes),
                key_hash: None,
                is_error: false,
            });
        }
        if self.saw_asst_line && self.asst_bytes > 0 {
            let bytes = self.asst_bytes;
            blocks.push(ContentBlock {
                kind: BlockKind::AssistantText,
                tool: None,
                bytes,
                est_tokens: estimate_tokens(bytes),
                key_hash: None,
                is_error: false,
            });
        }
        self.reset();
        blocks
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenHistorySession {
    ordinal: usize,
    started_ts: Timestamp,
    started_ts_str: String,
    session_id: Option<String>,
    current_model: Option<String>,
    pending_compaction: bool,
    turn_accum: PendingTurnAccumulator,
    records: Vec<TurnRecord>,
    diagnostics: ParseDiagnostics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AiderHistoryCheckpoint {
    schema_version: u32,
    line_number: usize,
    completed_sessions: Vec<AiderSessionBatch>,
    diagnostics: ParseDiagnostics,
    next_ordinal: usize,
    current: Option<OpenHistorySession>,
}

impl OpenHistorySession {
    fn new(ordinal: usize, started_ts: Timestamp) -> Self {
        let started_ts_str = started_ts.to_string();
        Self {
            ordinal,
            started_ts,
            started_ts_str,
            session_id: None,
            current_model: None,
            pending_compaction: false,
            turn_accum: PendingTurnAccumulator::new(),
            records: Vec::new(),
            diagnostics: ParseDiagnostics::default(),
        }
    }

    fn to_batch(&self) -> Option<AiderSessionBatch> {
        let session_id = self.session_id.clone()?;
        if self.records.is_empty() {
            return None;
        }
        Some(AiderSessionBatch {
            session_id,
            records: self.records.clone(),
            diagnostics: self.diagnostics.clone(),
        })
    }

    fn into_batch(self) -> Option<AiderSessionBatch> {
        let session_id = self.session_id?;
        if self.records.is_empty() {
            return None;
        }
        Some(AiderSessionBatch {
            session_id,
            records: self.records,
            diagnostics: self.diagnostics,
        })
    }
}

/// Opens an Aider `.aider.chat.history.md` file strictly read-only and decodes its
/// sessions into [`TurnRecord`] batches (`Confidence::Estimated`).
pub fn parse_aider_history_file(path: &Path) -> io::Result<AiderParseOutput> {
    let file = open_readonly(path)?;
    let cwd_hash = cwd_hash_from_parent(path);
    parse_aider_history_reader(BufReader::new(file), cwd_hash)
}

/// Incrementally decodes or resumes an Aider `.aider.chat.history.md` file from
/// `start_offset` and an optional serialized checkpoint (`PLAN.md` §7, §10 T12).
pub fn parse_aider_history_file_incremental(
    path: &Path,
    start_offset: u64,
    prev_checkpoint_json: Option<&str>,
    counter: Option<&ReadCounter>,
) -> io::Result<IncrementalParseResult<AiderParseOutput>> {
    let cwd_hash = cwd_hash_from_parent(path);
    let parsed_checkpoint = if start_offset > 0 {
        prev_checkpoint_json
            .and_then(|raw| serde_json::from_str::<AiderHistoryCheckpoint>(raw).ok())
            .filter(|cp| cp.schema_version == AIDER_HISTORY_CHECKPOINT_SCHEMA_V1)
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
        parse_aider_history_reader_incremental(reader, cwd_hash, 0, None)
    } else {
        parse_aider_history_reader_incremental(
            reader,
            cwd_hash,
            effective_offset,
            parsed_checkpoint,
        )
    }
}

/// Streams an Aider `.aider.chat.history.md` reader in bounded memory, dropping all
/// prompt and response prose immediately.
pub fn parse_aider_history_reader<R: BufRead>(
    reader: R,
    cwd_hash: Option<String>,
) -> io::Result<AiderParseOutput> {
    Ok(parse_aider_history_reader_incremental(reader, cwd_hash, 0, None)?.output)
}

fn parse_aider_history_reader_incremental<R: BufRead>(
    reader: R,
    cwd_hash: Option<String>,
    start_offset: u64,
    checkpoint: Option<AiderHistoryCheckpoint>,
) -> io::Result<IncrementalParseResult<AiderParseOutput>> {
    let (start_line, mut completed_sessions, mut diagnostics, mut next_ordinal, mut current) =
        match checkpoint {
            Some(cp) => (
                cp.line_number,
                cp.completed_sessions,
                cp.diagnostics,
                cp.next_ordinal,
                cp.current,
            ),
            None => (0, Vec::new(), ParseDiagnostics::default(), 1, None),
        };

    let mut line_reader = BoundedLineReader::with_offset_and_line(
        reader,
        DEFAULT_MAX_LINE_BYTES,
        start_offset,
        start_line,
    );
    let mut trailing_partial_snapshot: Option<(u64, AiderHistoryCheckpoint)> = None;

    while let Some((meta, outcome)) = line_reader.next_line_with_meta()? {
        if !meta.ended_with_newline && trailing_partial_snapshot.is_none() {
            trailing_partial_snapshot = Some((
                meta.offset_before,
                AiderHistoryCheckpoint {
                    schema_version: AIDER_HISTORY_CHECKPOINT_SCHEMA_V1,
                    line_number: meta.line_number.saturating_sub(1),
                    completed_sessions: completed_sessions.clone(),
                    diagnostics: diagnostics.clone(),
                    next_ordinal,
                    current: current.clone(),
                },
            ));
        }

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
                        limit: DEFAULT_MAX_LINE_BYTES,
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

        if let Some(banner_rest) = line.strip_prefix("# aider chat started at") {
            if let Some(prev) = current.take() {
                if let Some(batch) = prev.into_batch() {
                    completed_sessions.push(batch);
                }
            }
            let ts = parse_aider_banner_timestamp(banner_rest).unwrap_or_else(unix_epoch_ts);
            current = Some(OpenHistorySession::new(next_ordinal, ts));
            next_ordinal += 1;
            continue;
        }

        let session = current.get_or_insert_with(|| {
            let ord = next_ordinal;
            next_ordinal += 1;
            OpenHistorySession::new(ord, unix_epoch_ts())
        });

        if let Some(prompt_rest) = line.strip_prefix("####") {
            let payload = prompt_rest.strip_prefix(' ').unwrap_or(prompt_rest);
            let cmd_trimmed = payload.trim();
            if matches!(cmd_trimmed, "/clear" | "/reset") {
                session.pending_compaction = true;
                session.turn_accum.reset();
                continue;
            }
            if let Some(model_arg) = cmd_trimmed.strip_prefix("/model ") {
                if let Some(slug) = parse_main_model_slug(model_arg) {
                    session.current_model = Some(slug);
                }
                session.turn_accum.reset();
                continue;
            }
            session.turn_accum.push_prompt_line(payload);
            continue;
        }

        if let Some(meta_rest) = line.strip_prefix('>') {
            session.turn_accum.in_prompt_block = false;
            let meta = meta_rest.trim();

            if let Some(ver_rest) = meta.strip_prefix("Aider v") {
                let ver_token = ver_rest.split_whitespace().next().unwrap_or("");
                if is_safe_version_token(ver_token) {
                    diagnostics.cli_versions.insert(ver_token.to_string());
                    session
                        .diagnostics
                        .cli_versions
                        .insert(ver_token.to_string());
                }
                continue;
            }

            if let Some(model_rest) = meta.strip_prefix("Main model:") {
                if let Some(slug) = parse_main_model_slug(model_rest) {
                    session.current_model = Some(slug);
                }
                continue;
            }

            if let Some(models_rest) = meta.strip_prefix("Models:") {
                if let Some(slug) = parse_main_model_slug(models_rest) {
                    session.current_model = Some(slug);
                }
                continue;
            }

            if matches!(meta, "/clear" | "/reset") {
                session.pending_compaction = true;
                session.turn_accum.reset();
                continue;
            }

            if let Some(model_arg) = meta.strip_prefix("/model ") {
                if let Some(slug) = parse_main_model_slug(model_arg) {
                    session.current_model = Some(slug);
                }
                continue;
            }

            if let Some(tokens_rest) = meta.strip_prefix("Tokens:") {
                let Some(parsed_tok) = parse_tokens_summary_line(tokens_rest) else {
                    let diag = ParseDiagnostic {
                        line_number,
                        kind: ParseDiagnosticKind::MalformedJson,
                    };
                    session.diagnostics.issues.push(diag.clone());
                    diagnostics.issues.push(diag);
                    session.turn_accum.reset();
                    continue;
                };

                let cached_sum = parsed_tok.cache_write.saturating_add(parsed_tok.cache_read);
                let fresh_in = if parsed_tok.sent >= cached_sum {
                    parsed_tok.sent - cached_sum
                } else {
                    parsed_tok.sent
                };

                let Ok(usage) = Usage::from_disjoint(DisjointUsageInput {
                    input_tokens: fresh_in,
                    cache_write_tokens: parsed_tok.cache_write,
                    cache_read_tokens: parsed_tok.cache_read,
                    output_tokens: parsed_tok.received,
                    reasoning_tokens: None,
                    confidence: Confidence::Estimated,
                    estimation_method: Some(EST_METHOD_HISTORY.to_string()),
                    cache_write_split: None,
                    agent_reported_cost_usd: parsed_tok.message_cost_usd,
                }) else {
                    let diag = ParseDiagnostic {
                        line_number,
                        kind: ParseDiagnosticKind::UsageOverflow,
                    };
                    session.diagnostics.issues.push(diag.clone());
                    diagnostics.issues.push(diag);
                    session.turn_accum.reset();
                    continue;
                };

                let model_slug = session.current_model.as_deref().unwrap_or("unknown");
                let session_id = match &session.session_id {
                    Some(existing) => existing.clone(),
                    None => {
                        let derived = derive_history_session_id(
                            &session.started_ts_str,
                            session.ordinal,
                            model_slug,
                            usage.fresh_in,
                            usage.cache_write,
                            usage.cache_read,
                            usage.output,
                        );
                        session.session_id = Some(derived.clone());
                        derived
                    }
                };

                let turn_num = u32::try_from(session.records.len() + 1).unwrap_or(u32::MAX);
                let added = session.turn_accum.take_blocks();
                let compaction = mem::replace(&mut session.pending_compaction, false);

                session.records.push(TurnRecord {
                    agent: AgentId::Aider,
                    session_id,
                    parent_session_id: None,
                    turn: turn_num,
                    ts: session.started_ts,
                    model: session.current_model.clone(),
                    cwd_hash: cwd_hash.clone(),
                    usage,
                    added,
                    hints: TurnHints {
                        compaction,
                        ..TurnHints::default()
                    },
                });
            }
            continue;
        }

        session.turn_accum.push_assistant_or_blank(line);
    }

    let mut sessions_out = completed_sessions.clone();
    if let Some(ref curr) = current {
        if let Some(batch) = curr.to_batch() {
            sessions_out.push(batch);
        }
    }
    let output = AiderParseOutput {
        sessions: sessions_out,
        diagnostics: diagnostics.clone(),
    };

    let (committed_offset, committed_checkpoint) = match trailing_partial_snapshot {
        Some(snap) => snap,
        None => (
            line_reader.bytes_consumed(),
            AiderHistoryCheckpoint {
                schema_version: AIDER_HISTORY_CHECKPOINT_SCHEMA_V1,
                line_number: line_reader.line_number(),
                completed_sessions,
                diagnostics,
                next_ordinal,
                current,
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

#[derive(Deserialize)]
struct RawAnalyticsEvent {
    event: Option<String>,
    #[serde(default)]
    properties: RawAnalyticsProperties,
    time: Option<serde_json::Value>,
    user_id: Option<String>,
}

#[derive(Default, Deserialize)]
struct RawAnalyticsProperties {
    main_model: Option<String>,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    cost: Option<f64>,
    version: Option<String>,
}

fn parse_analytics_timestamp(val: Option<&serde_json::Value>) -> Option<Timestamp> {
    let val = val?;
    match val {
        serde_json::Value::String(s) => Timestamp::parse_rfc3339(s.trim()).ok(),
        serde_json::Value::Number(num) => {
            if let Some(secs) = num.as_i64() {
                Timestamp::from_unix_timestamp(secs).ok()
            } else if let Some(secs_f) = num.as_f64() {
                if secs_f.is_finite() {
                    let nanos = (secs_f * 1_000_000_000.0) as i128;
                    Timestamp::from_unix_timestamp_nanos(nanos).ok()
                } else {
                    None
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAnalyticsSession {
    ordinal: usize,
    started_ts: Option<Timestamp>,
    /// Transient `user_id` retained in memory only until the first `message_send`
    /// derives the irreversible `session_id`; never serialized to checkpoints.
    #[serde(skip)]
    user_id: String,
    session_id: Option<String>,
    records: Vec<TurnRecord>,
    diagnostics: ParseDiagnostics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AiderAnalyticsCheckpoint {
    schema_version: u32,
    line_number: usize,
    completed_sessions: Vec<AiderSessionBatch>,
    diagnostics: ParseDiagnostics,
    next_ordinal: usize,
    current: Option<OpenAnalyticsSession>,
}

impl OpenAnalyticsSession {
    fn new(ordinal: usize) -> Self {
        Self {
            ordinal,
            started_ts: None,
            user_id: String::new(),
            session_id: None,
            records: Vec::new(),
            diagnostics: ParseDiagnostics::default(),
        }
    }

    fn to_batch(&self) -> Option<AiderSessionBatch> {
        let session_id = self.session_id.clone()?;
        if self.records.is_empty() {
            return None;
        }
        Some(AiderSessionBatch {
            session_id,
            records: self.records.clone(),
            diagnostics: self.diagnostics.clone(),
        })
    }

    fn into_batch(self) -> Option<AiderSessionBatch> {
        let session_id = self.session_id?;
        if self.records.is_empty() {
            return None;
        }
        Some(AiderSessionBatch {
            session_id,
            records: self.records,
            diagnostics: self.diagnostics,
        })
    }
}

/// Opens an opt-in Aider `--analytics-log` JSONL file strictly read-only and decodes
/// `"message_send"` events into [`TurnRecord`] batches (`Confidence::Estimated`).
pub fn parse_aider_analytics_file(path: &Path) -> io::Result<AiderParseOutput> {
    let file = open_readonly(path)?;
    let cwd_hash = cwd_hash_from_parent(path);
    parse_aider_analytics_reader(BufReader::new(file), cwd_hash)
}

/// Incrementally decodes or resumes an opt-in Aider `--analytics-log` JSONL file from
/// `start_offset` and an optional serialized checkpoint (`PLAN.md` §7, §10 T12).
pub fn parse_aider_analytics_file_incremental(
    path: &Path,
    start_offset: u64,
    prev_checkpoint_json: Option<&str>,
    counter: Option<&ReadCounter>,
) -> io::Result<IncrementalParseResult<AiderParseOutput>> {
    let cwd_hash = cwd_hash_from_parent(path);
    let parsed_checkpoint = if start_offset > 0 {
        prev_checkpoint_json
            .and_then(|raw| serde_json::from_str::<AiderAnalyticsCheckpoint>(raw).ok())
            .filter(|cp| cp.schema_version == AIDER_ANALYTICS_CHECKPOINT_SCHEMA_V1)
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
        parse_aider_analytics_reader_incremental(reader, cwd_hash, 0, None)
    } else {
        parse_aider_analytics_reader_incremental(
            reader,
            cwd_hash,
            effective_offset,
            parsed_checkpoint,
        )
    }
}

/// Streams an opt-in Aider `--analytics-log` JSONL reader in bounded memory.
pub fn parse_aider_analytics_reader<R: BufRead>(
    reader: R,
    cwd_hash: Option<String>,
) -> io::Result<AiderParseOutput> {
    Ok(parse_aider_analytics_reader_incremental(reader, cwd_hash, 0, None)?.output)
}

fn parse_aider_analytics_reader_incremental<R: BufRead>(
    reader: R,
    cwd_hash: Option<String>,
    start_offset: u64,
    checkpoint: Option<AiderAnalyticsCheckpoint>,
) -> io::Result<IncrementalParseResult<AiderParseOutput>> {
    let (start_line, mut completed_sessions, mut diagnostics, mut next_ordinal, mut current) =
        match checkpoint {
            Some(cp) => (
                cp.line_number,
                cp.completed_sessions,
                cp.diagnostics,
                cp.next_ordinal,
                cp.current,
            ),
            None => (0, Vec::new(), ParseDiagnostics::default(), 1, None),
        };

    let mut line_reader = BoundedLineReader::with_offset_and_line(
        reader,
        DEFAULT_MAX_LINE_BYTES,
        start_offset,
        start_line,
    );
    let mut trailing_partial_snapshot: Option<(u64, AiderAnalyticsCheckpoint)> = None;
    let mut unmaterialized_launch_snapshot: Option<(u64, AiderAnalyticsCheckpoint)> = None;

    while let Some((meta, outcome)) = line_reader.next_line_with_meta()? {
        if !meta.ended_with_newline && trailing_partial_snapshot.is_none() {
            trailing_partial_snapshot = Some((
                meta.offset_before,
                AiderAnalyticsCheckpoint {
                    schema_version: AIDER_ANALYTICS_CHECKPOINT_SCHEMA_V1,
                    line_number: meta.line_number.saturating_sub(1),
                    completed_sessions: completed_sessions.clone(),
                    diagnostics: diagnostics.clone(),
                    next_ordinal,
                    current: current.clone(),
                },
            ));
        }

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
                        limit: DEFAULT_MAX_LINE_BYTES,
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

        if classify_aider_analytics_line(trimmed) == AiderAnalyticsClass::NonTurnFastPath {
            let Ok(env) = serde_json::from_str::<BorrowedAiderEnvelope<'_>>(trimmed) else {
                diagnostics.issues.push(ParseDiagnostic {
                    line_number,
                    kind: ParseDiagnosticKind::MalformedJson,
                });
                continue;
            };
            if let Some(ver) = env.properties.version.as_deref() {
                let ver_trimmed = ver.trim();
                if is_safe_version_token(ver_trimmed) {
                    diagnostics.cli_versions.insert(ver_trimmed.to_string());
                }
            }
            let _ = env.event;
            diagnostics.ignored_records += 1;
            if let Some(ref mut sess) = current {
                sess.diagnostics.ignored_records += 1;
            }
            continue;
        }

        let Ok(parsed) = serde_json::from_str::<RawAnalyticsEvent>(trimmed) else {
            diagnostics.issues.push(ParseDiagnostic {
                line_number,
                kind: ParseDiagnosticKind::MalformedJson,
            });
            continue;
        };

        if let Some(ver) = parsed.properties.version.as_deref() {
            let ver_trimmed = ver.trim();
            if is_safe_version_token(ver_trimmed) {
                diagnostics.cli_versions.insert(ver_trimmed.to_string());
            }
        }

        let Some(event_name) = parsed.event.as_deref() else {
            diagnostics.ignored_records += 1;
            if let Some(ref mut sess) = current {
                sess.diagnostics.ignored_records += 1;
            }
            continue;
        };

        let event_ts = parse_analytics_timestamp(parsed.time.as_ref());

        if event_name == "launched" {
            if let Some(prev) = current.take() {
                if let Some(batch) = prev.into_batch() {
                    completed_sessions.push(batch);
                }
            }
            unmaterialized_launch_snapshot = Some((
                meta.offset_before,
                AiderAnalyticsCheckpoint {
                    schema_version: AIDER_ANALYTICS_CHECKPOINT_SCHEMA_V1,
                    line_number: meta.line_number.saturating_sub(1),
                    completed_sessions: completed_sessions.clone(),
                    diagnostics: diagnostics.clone(),
                    next_ordinal,
                    current: None,
                },
            ));
            let mut sess = OpenAnalyticsSession::new(next_ordinal);
            next_ordinal += 1;
            sess.started_ts = event_ts;
            if let Some(uid) = parsed
                .user_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                sess.user_id = uid.to_string();
            }
            sess.diagnostics.ignored_records += 1;
            diagnostics.ignored_records += 1;
            current = Some(sess);
            continue;
        }

        if event_name != "message_send" {
            diagnostics.ignored_records += 1;
            if let Some(ref mut sess) = current {
                sess.diagnostics.ignored_records += 1;
            }
            continue;
        }

        let session = current.get_or_insert_with(|| {
            let ord = next_ordinal;
            next_ordinal += 1;
            OpenAnalyticsSession::new(ord)
        });

        if session.started_ts.is_none() {
            session.started_ts = event_ts;
        }
        if session.session_id.is_none() && session.user_id.is_empty() {
            if let Some(uid) = parsed
                .user_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                session.user_id = uid.to_string();
            }
        }
        if let Some(ver) = parsed.properties.version.as_deref() {
            let ver_trimmed = ver.trim();
            if is_safe_version_token(ver_trimmed) {
                session
                    .diagnostics
                    .cli_versions
                    .insert(ver_trimmed.to_string());
            }
        }

        let prompt_tokens = parsed.properties.prompt_tokens.unwrap_or(0);
        let completion_tokens = parsed.properties.completion_tokens.unwrap_or(0);
        let cost_usd = parsed.properties.cost.filter(|c| c.is_finite() && *c > 0.0);

        let Ok(usage) = Usage::from_disjoint(DisjointUsageInput {
            input_tokens: prompt_tokens,
            cache_write_tokens: 0,
            cache_read_tokens: 0,
            output_tokens: completion_tokens,
            reasoning_tokens: None,
            confidence: Confidence::Estimated,
            estimation_method: Some(EST_METHOD_ANALYTICS.to_string()),
            cache_write_split: None,
            agent_reported_cost_usd: cost_usd,
        }) else {
            let diag = ParseDiagnostic {
                line_number,
                kind: ParseDiagnosticKind::UsageOverflow,
            };
            session.diagnostics.issues.push(diag.clone());
            diagnostics.issues.push(diag);
            continue;
        };

        let ts = event_ts
            .or(session.started_ts)
            .unwrap_or_else(unix_epoch_ts);
        let started_ts_str = session.started_ts.unwrap_or(ts).to_string();
        let model_opt = parsed
            .properties
            .main_model
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty());
        let model_slug = model_opt.as_deref().unwrap_or("unknown");

        let session_id = match &session.session_id {
            Some(existing) => existing.clone(),
            None => {
                let derived = derive_analytics_session_id(
                    &started_ts_str,
                    session.ordinal,
                    &session.user_id,
                    model_slug,
                    prompt_tokens,
                    completion_tokens,
                );
                session.session_id = Some(derived.clone());
                session.user_id.clear();
                unmaterialized_launch_snapshot = None;
                derived
            }
        };

        let turn_num = u32::try_from(session.records.len() + 1).unwrap_or(u32::MAX);
        session.records.push(TurnRecord {
            agent: AgentId::Aider,
            session_id,
            parent_session_id: None,
            turn: turn_num,
            ts,
            model: model_opt,
            cwd_hash: cwd_hash.clone(),
            usage,
            added: Vec::new(),
            hints: TurnHints::default(),
        });
    }

    let mut sessions_out = completed_sessions.clone();
    if let Some(ref curr) = current {
        if let Some(batch) = curr.to_batch() {
            sessions_out.push(batch);
        }
    }
    let output = AiderParseOutput {
        sessions: sessions_out,
        diagnostics: diagnostics.clone(),
    };

    let chosen_snapshot = match (unmaterialized_launch_snapshot, trailing_partial_snapshot) {
        (Some(launch_snap), Some(partial_snap)) => {
            if launch_snap.0 <= partial_snap.0 {
                Some(launch_snap)
            } else {
                Some(partial_snap)
            }
        }
        (Some(launch_snap), None) => Some(launch_snap),
        (None, Some(partial_snap)) => Some(partial_snap),
        (None, None) => None,
    };

    let (committed_offset, committed_checkpoint) = match chosen_snapshot {
        Some(snap) => snap,
        None => (
            line_reader.bytes_consumed(),
            AiderAnalyticsCheckpoint {
                schema_version: AIDER_ANALYTICS_CHECKPOINT_SCHEMA_V1,
                line_number: line_reader.line_number(),
                completed_sessions,
                diagnostics,
                next_ordinal,
                current,
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
