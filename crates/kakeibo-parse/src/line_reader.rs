//! Bounded-memory streaming line reader, atomic read-byte counters, and text-free parse diagnostics (`PLAN.md` §7, §9).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Deserializer, Serialize};

use crate::catalog::open_readonly;

/// Default per-line streaming cap (`16 MiB` per `PLAN.md` §7).
pub const DEFAULT_MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

fn intern_diagnostic_field(raw: &str) -> &'static str {
    match raw {
        "timestamp" => "timestamp",
        "time" => "time",
        "sessionId" => "sessionId",
        "session_id" => "session_id",
        "message" => "message",
        "message.usage" => "message.usage",
        "payload" => "payload",
        "usage" => "usage",
        _ => "<unknown>",
    }
}

/// Privacy-safe classification of a non-fatal streaming parse diagnostic.
///
/// Never stores raw line text or filesystem paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
    /// Compressed rollout stream encountered a corrupt or truncated Zstandard frame.
    DecompressionError,
}

impl<'de> Deserialize<'de> for ParseDiagnosticKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        enum RawParseDiagnosticKind {
            OversizedLine { byte_len: u64, limit: usize },
            InvalidUtf8,
            MalformedJson,
            MissingField { field: String },
            InvalidTimestamp,
            UsageOverflow,
            DecompressionError,
        }

        let raw = RawParseDiagnosticKind::deserialize(deserializer)?;
        Ok(match raw {
            RawParseDiagnosticKind::OversizedLine { byte_len, limit } => {
                Self::OversizedLine { byte_len, limit }
            }
            RawParseDiagnosticKind::InvalidUtf8 => Self::InvalidUtf8,
            RawParseDiagnosticKind::MalformedJson => Self::MalformedJson,
            RawParseDiagnosticKind::MissingField { field } => Self::MissingField {
                field: intern_diagnostic_field(&field),
            },
            RawParseDiagnosticKind::InvalidTimestamp => Self::InvalidTimestamp,
            RawParseDiagnosticKind::UsageOverflow => Self::UsageOverflow,
            RawParseDiagnosticKind::DecompressionError => Self::DecompressionError,
        })
    }
}

/// A single 1-based line diagnostic emitted by a tolerant agent decoder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseDiagnostic {
    pub line_number: usize,
    pub kind: ParseDiagnosticKind,
}

/// Aggregated non-fatal diagnostics and metadata collected while streaming a log.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseDiagnostics {
    /// Count of non-turn metadata, system, error, or unknown JSONL records skipped cleanly.
    pub ignored_records: u64,
    /// Counts of unrecognised top-level record `type` identifiers (`doctor`).
    pub unknown_record_types: BTreeMap<String, u64>,
    /// Distinct agent CLI version strings observed in the stream (`doctor`).
    pub cli_versions: BTreeSet<String>,
    /// Line-level structural or encoding issues encountered and skipped without panicking.
    pub issues: Vec<ParseDiagnostic>,
}

/// Result of decoding or resuming a line-oriented log stream from a byte offset (`PLAN.md` §7, §10 T12).
#[derive(Debug, Clone, PartialEq)]
pub struct IncrementalParseResult<Out> {
    /// Complete parse output reflecting the stream up to EOF (`last_seen_size`),
    /// including any non-`\n`-terminated trailing line.
    pub output: Out,
    /// Byte offset immediately after the last `\n` committed into `checkpoint_json`
    /// (`0` when the source format is non-resumable, such as `.jsonl.zst` or legacy `.json`).
    pub committed_offset: u64,
    /// Opaque, versioned, text-free, path-free JSON checkpoint at `committed_offset`,
    /// or `None` when the source format is non-resumable.
    pub checkpoint_json: Option<String>,
}

/// Thread-safe atomic counter recording files opened and physical bytes read from agent logs.
///
/// Used by T12 acceptance tests to prove warm unchanged runs read `0` bytes and appended
/// logs read only the newly appended bytes (`PLAN.md` §10 T12).
#[derive(Debug, Default)]
pub struct ReadCounter {
    files_opened: AtomicU64,
    bytes_read: AtomicU64,
}

impl ReadCounter {
    /// Creates a zeroed [`ReadCounter`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            files_opened: AtomicU64::new(0),
            bytes_read: AtomicU64::new(0),
        }
    }

    /// Increments the count of opened agent source files by `1`.
    pub fn record_file_open(&self) {
        self.files_opened.fetch_add(1, Ordering::Relaxed);
    }

    /// Adds `n` physical bytes read from an agent source file.
    pub fn record_bytes_read(&self, n: u64) {
        self.bytes_read.fetch_add(n, Ordering::Relaxed);
    }

    /// Total number of agent source files opened since creation or last [`Self::reset`].
    #[must_use]
    pub fn files_opened(&self) -> u64 {
        self.files_opened.load(Ordering::Relaxed)
    }

    /// Total physical bytes read from agent source files since creation or last [`Self::reset`].
    #[must_use]
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read.load(Ordering::Relaxed)
    }

    /// Resets both `files_opened` and `bytes_read` to `0`.
    pub fn reset(&self) {
        self.files_opened.store(0, Ordering::Relaxed);
        self.bytes_read.store(0, Ordering::Relaxed);
    }
}

/// [`Read`] adapter that increments an optional [`ReadCounter`] on every physical read.
#[derive(Debug)]
pub struct CountingReader<'a, R> {
    inner: R,
    counter: Option<&'a ReadCounter>,
}

impl<'a, R> CountingReader<'a, R> {
    /// Wraps `inner` and records every byte read into `counter` when `Some`.
    #[must_use]
    pub const fn new(inner: R, counter: Option<&'a ReadCounter>) -> Self {
        Self { inner, counter }
    }
}

impl<R: Read> Read for CountingReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            if let Some(counter) = self.counter {
                counter.record_bytes_read(n as u64);
            }
        }
        Ok(n)
    }
}

/// Opens `path` strictly read-only, seeks to `start_offset`, and wraps it in a buffered
/// [`CountingReader`].
///
/// Returns `(reader, fell_back_to_zero)`. If the byte at `start_offset > 0` is `\0`
/// (indicating a POSIX sparse zero hole after `ftruncate(0)` + write past `start_offset`),
/// transparently re-opens from offset `0` and sets `fell_back_to_zero = true`.
pub(crate) fn open_seeked_counting_reader<'a>(
    path: &Path,
    start_offset: u64,
    counter: Option<&'a ReadCounter>,
) -> io::Result<(BufReader<CountingReader<'a, File>>, bool)> {
    let mut file = open_readonly(path)?;
    if let Some(c) = counter {
        c.record_file_open();
    }
    if start_offset == 0 {
        return Ok((BufReader::new(CountingReader::new(file, counter)), false));
    }

    file.seek(SeekFrom::Start(start_offset))?;
    let mut reader = BufReader::new(CountingReader::new(file, counter));
    let peek = reader.fill_buf()?;
    if peek.first() == Some(&0) {
        let fresh_file = open_readonly(path)?;
        return Ok((
            BufReader::new(CountingReader::new(fresh_file, counter)),
            true,
        ));
    }

    Ok((reader, false))
}

/// Physical byte-offset metadata accompanying a [`LineReadOutcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineMeta {
    pub line_number: usize,
    pub offset_before: u64,
    pub offset_after: u64,
    pub ended_with_newline: bool,
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

/// Streaming line reader using `memchr` that enforces a hard per-line byte ceiling
/// and tracks exact byte offsets for incremental append resume.
#[derive(Debug)]
pub struct BoundedLineReader<R> {
    reader: R,
    buf: Vec<u8>,
    max_line_bytes: usize,
    line_number: usize,
    bytes_consumed: u64,
    offset_before_last_line: u64,
    last_line_ended_with_newline: bool,
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
        Self::with_offset_and_line(reader, max_line_bytes, 0, 0)
    }

    /// Creates a bounded line reader resumed from `start_offset` and `start_line_number`.
    #[must_use]
    pub fn with_offset_and_line(
        reader: R,
        max_line_bytes: usize,
        start_offset: u64,
        start_line_number: usize,
    ) -> Self {
        Self {
            reader,
            buf: Vec::with_capacity(8 * 1024),
            max_line_bytes,
            line_number: start_line_number,
            bytes_consumed: start_offset,
            offset_before_last_line: start_offset,
            last_line_ended_with_newline: true,
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

    /// Exact physical byte offset after the most recently read line (including `start_offset`).
    #[must_use]
    pub const fn bytes_consumed(&self) -> u64 {
        self.bytes_consumed
    }

    /// Exact physical byte offset immediately before the most recently read line.
    #[must_use]
    pub const fn offset_before_last_line(&self) -> u64 {
        self.offset_before_last_line
    }

    /// Whether the most recently returned line ended with a physical `\n` byte (`true` before start).
    #[must_use]
    pub const fn last_line_ended_with_newline(&self) -> bool {
        self.last_line_ended_with_newline
    }

    /// Advances to the next physical line, returning `Ok(None)` at EOF.
    pub fn next_line(&mut self) -> io::Result<Option<LineReadOutcome<'_>>> {
        Ok(self.next_line_with_meta()?.map(|(_, outcome)| outcome))
    }

    /// Advances to the next physical line along with [`LineMeta`] byte offsets.
    pub fn next_line_with_meta(&mut self) -> io::Result<Option<(LineMeta, LineReadOutcome<'_>)>> {
        self.buf.clear();
        let line_start_offset = self.bytes_consumed;
        let mut payload_bytes: u64 = 0;
        let mut physical_bytes: u64 = 0;
        let mut ended_with_newline = false;
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
                payload_bytes += slice.len() as u64;
                physical_bytes += (nl_pos + 1) as u64;
                ended_with_newline = true;
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
            payload_bytes += chunk_len as u64;
            physical_bytes += chunk_len as u64;
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

        self.offset_before_last_line = line_start_offset;
        self.bytes_consumed = line_start_offset + physical_bytes;
        self.last_line_ended_with_newline = ended_with_newline;
        self.line_number += 1;
        let line_number = self.line_number;
        let meta = LineMeta {
            line_number,
            offset_before: self.offset_before_last_line,
            offset_after: self.bytes_consumed,
            ended_with_newline,
        };

        if oversized {
            return Ok(Some((
                meta,
                LineReadOutcome::Oversized {
                    line_number,
                    byte_len: payload_bytes,
                },
            )));
        }

        if self.buf.last() == Some(&b'\r') {
            self.buf.pop();
        }

        match std::str::from_utf8(&self.buf) {
            Ok(text) => Ok(Some((meta, LineReadOutcome::Line { line_number, text }))),
            Err(_) => Ok(Some((meta, LineReadOutcome::InvalidUtf8 { line_number }))),
        }
    }
}
