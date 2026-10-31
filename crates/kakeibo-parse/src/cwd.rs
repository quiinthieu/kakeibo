//! Opt-in read-only working-directory extraction used exclusively when the user
//! explicitly passes `kakeibo export --show-paths` (`PLAN.md` §0 #5, §6.4).
//!
//! Normal discovery, parsing, attribution, flags, NDJSON ledgers, and default
//! exports never call these helpers and drop raw `cwd` strings immediately.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

use serde::Deserialize;

use crate::catalog::open_readonly;
use crate::line_reader::{BoundedLineReader, LineReadOutcome, DEFAULT_MAX_LINE_BYTES};

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
