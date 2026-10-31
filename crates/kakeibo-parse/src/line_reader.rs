//! Bounded-memory streaming line reader and text-free parse diagnostics (`PLAN.md` §7, §9).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead};

/// Default per-line streaming cap (`16 MiB` per `PLAN.md` §7).
pub const DEFAULT_MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Privacy-safe classification of a non-fatal streaming parse diagnostic.
///
/// Never stores raw line text or filesystem paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseDiagnosticKind {
    /// Line exceeded `limit` bytes (`> 16 MiB` by default) and was drained & skipped.
    OversizedLine { byte_len: u64, limit: usize },
    /// Line contained invalid UTF-8 bytes and was skipped.
    InvalidUtf8,
    /// Line was truncated or syntactically invalid JSON.
    MalformedJson,
    /// A turn record omitted a required field.
    MissingField { field: &'static str },
    /// A timestamp field was not valid RFC 3339.
    InvalidTimestamp,
    /// Token usage counts overflowed `u64` during normalisation.
    UsageOverflow,
}

/// A single 1-based line diagnostic emitted by a tolerant agent decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDiagnostic {
    pub line_number: usize,
    pub kind: ParseDiagnosticKind,
}

/// Aggregated non-fatal diagnostics and metadata collected while streaming a log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseDiagnostics {
    /// Count of non-turn metadata, system, error, or unknown JSONL records skipped cleanly.
    pub ignored_records: u64,
    /// Counts of unrecognised top-level record `type` identifiers (`kakeibo doctor`).
    pub unknown_record_types: BTreeMap<String, u64>,
    /// Distinct agent CLI version strings observed in the stream (`kakeibo doctor`).
    pub cli_versions: BTreeSet<String>,
    /// Line-level structural or encoding issues encountered and skipped without panicking.
    pub issues: Vec<ParseDiagnostic>,
}

/// Outcome of reading one physical `\n`-delimited line from [`BoundedLineReader`].
#[derive(Debug, PartialEq, Eq)]
pub enum LineReadOutcome<'a> {
    /// Valid UTF-8 line within the byte cap (`\r` and `\n` stripped).
    Line { line_number: usize, text: &'a str },
    /// Line exceeded `max_line_bytes` and was drained in bounded memory.
    Oversized { line_number: usize, byte_len: u64 },
    /// Line bytes were within `max_line_bytes` but failed UTF-8 validation.
    InvalidUtf8 { line_number: usize },
}

/// Streaming line reader using `memchr` that enforces a hard per-line byte ceiling.
#[derive(Debug)]
pub struct BoundedLineReader<R> {
    reader: R,
    buf: Vec<u8>,
    max_line_bytes: usize,
    line_number: usize,
}

impl<R: BufRead> BoundedLineReader<R> {
    /// Creates a bounded line reader using [`DEFAULT_MAX_LINE_BYTES`] (`16 MiB`).
    #[must_use]
    pub fn new(reader: R) -> Self {
        Self::with_max_line_bytes(reader, DEFAULT_MAX_LINE_BYTES)
    }

    /// Creates a bounded line reader with a custom `max_line_bytes` ceiling.
    #[must_use]
    pub fn with_max_line_bytes(reader: R, max_line_bytes: usize) -> Self {
        Self {
            reader,
            buf: Vec::with_capacity(8 * 1024),
            max_line_bytes,
            line_number: 0,
        }
    }

    /// Configured maximum byte length for a single line.
    #[must_use]
    pub const fn max_line_bytes(&self) -> usize {
        self.max_line_bytes
    }

    /// Most recently read 1-based physical line number (`0` before start).
    #[must_use]
    pub const fn line_number(&self) -> usize {
        self.line_number
    }

    /// Advances to the next physical line, returning `Ok(None)` at EOF.
    pub fn next_line(&mut self) -> io::Result<Option<LineReadOutcome<'_>>> {
        self.buf.clear();
        let mut total_bytes: u64 = 0;
        let mut oversized = false;
        let mut saw_any = false;

        loop {
            let available = self.reader.fill_buf()?;
            if available.is_empty() {
                if !saw_any {
                    return Ok(None);
                }
                break;
            }
            saw_any = true;

            if let Some(nl_pos) = memchr::memchr(b'\n', available) {
                let slice = &available[..nl_pos];
                total_bytes += slice.len() as u64;
                if !oversized {
                    if self.buf.len() + slice.len() <= self.max_line_bytes {
                        self.buf.extend_from_slice(slice);
                    } else {
                        oversized = true;
                        self.buf.clear();
                    }
                }
                self.reader.consume(nl_pos + 1);
                break;
            }

            let chunk_len = available.len();
            total_bytes += chunk_len as u64;
            if !oversized {
                if self.buf.len() + chunk_len <= self.max_line_bytes {
                    self.buf.extend_from_slice(available);
                } else {
                    oversized = true;
                    self.buf.clear();
                }
            }
            self.reader.consume(chunk_len);
        }

        self.line_number += 1;
        let line_number = self.line_number;

        if oversized {
            return Ok(Some(LineReadOutcome::Oversized {
                line_number,
                byte_len: total_bytes,
            }));
        }

        if self.buf.last() == Some(&b'\r') {
            self.buf.pop();
        }

        match std::str::from_utf8(&self.buf) {
            Ok(text) => Ok(Some(LineReadOutcome::Line { line_number, text })),
            Err(_) => Ok(Some(LineReadOutcome::InvalidUtf8 { line_number })),
        }
    }
}
