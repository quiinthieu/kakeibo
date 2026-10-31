//! Streaming `kakeibo.ledger/1` NDJSON reader and writer (`PLAN.md` §3.1, §4, §10 T03).
//!
//! Wraps the pure line codec in [`kakeibo_core::ledger`] with bounded-memory
//! `std::io::BufRead` and `std::io::Write` streaming adapters.

use std::io::{self, BufRead, Write};

use kakeibo_core::ledger::{decode_line, encode_line, LedgerError};
use kakeibo_core::TurnRecord;

/// Errors returned by streaming NDJSON ledger I/O operations.
#[derive(Debug, thiserror::Error)]
pub enum NdjsonError {
    /// Underlying reader or writer failed.
    #[error("NDJSON I/O error{}: {source}", format_line_suffix(*.line))]
    Io {
        line: Option<usize>,
        #[source]
        source: io::Error,
    },
    /// Wire schema or JSON validation failed.
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}

impl NdjsonError {
    /// Returns the 1-based physical line number associated with this error, if known.
    #[must_use]
    pub const fn line(&self) -> Option<usize> {
        match self {
            Self::Io { line, .. } => *line,
            Self::Ledger(
                LedgerError::MissingSchema { line }
                | LedgerError::UnsupportedSchema { line, .. }
                | LedgerError::Json { line, .. },
            ) => *line,
            Self::Ledger(LedgerError::NonFiniteNumber { .. }) => None,
        }
    }
}

fn format_line_suffix(line: Option<usize>) -> String {
    match line {
        Some(n) => format!(" at line {n}"),
        None => String::new(),
    }
}

/// Writes a single [`TurnRecord`] as a `\n`-terminated `kakeibo.ledger/1` JSON line.
pub fn write_record<W: Write>(writer: &mut W, record: &TurnRecord) -> Result<(), NdjsonError> {
    let line = encode_line(record)?;
    writer
        .write_all(line.as_bytes())
        .and_then(|()| writer.write_all(b"\n"))
        .map_err(|source| NdjsonError::Io { line: None, source })
}

/// Writes an iterator of [`TurnRecord`] items as `\n`-terminated NDJSON lines
/// and returns the number of records written.
pub fn write_ledger<'a, W, I>(writer: &mut W, records: I) -> Result<usize, NdjsonError>
where
    W: Write,
    I: IntoIterator<Item = &'a TurnRecord>,
{
    let mut count = 0_usize;
    for record in records {
        let line_no = count + 1;
        let line = encode_line(record)?;
        writer
            .write_all(line.as_bytes())
            .and_then(|()| writer.write_all(b"\n"))
            .map_err(|source| NdjsonError::Io {
                line: Some(line_no),
                source,
            })?;
        count += 1;
    }
    Ok(count)
}

/// Bounded-memory streaming iterator decoding [`TurnRecord`] lines from a [`BufRead`].
#[derive(Debug)]
pub struct LedgerReader<R> {
    reader: R,
    line_buf: String,
    line_number: usize,
    finished: bool,
}

impl<R: BufRead> LedgerReader<R> {
    /// Wraps a buffered reader as a streaming `kakeibo.ledger/1` iterator.
    #[must_use]
    pub const fn new(reader: R) -> Self {
        Self {
            reader,
            line_buf: String::new(),
            line_number: 0,
            finished: false,
        }
    }

    /// Returns the 1-based physical line number most recently read (`0` before start).
    #[must_use]
    pub const fn line_number(&self) -> usize {
        self.line_number
    }
}

impl<R: BufRead> Iterator for LedgerReader<R> {
    type Item = Result<TurnRecord, NdjsonError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }

        loop {
            self.line_buf.clear();
            let next_line_no = self.line_number + 1;
            match self.reader.read_line(&mut self.line_buf) {
                Ok(0) => {
                    self.finished = true;
                    return None;
                }
                Ok(_) => {
                    self.line_number = next_line_no;
                    let trimmed = self.line_buf.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let result = decode_line(trimmed)
                        .map_err(|err| NdjsonError::Ledger(err.with_line(self.line_number)));
                    return Some(result);
                }
                Err(source) => {
                    self.line_number = next_line_no;
                    self.finished = true;
                    return Some(Err(NdjsonError::Io {
                        line: Some(self.line_number),
                        source,
                    }));
                }
            }
        }
    }
}

/// Reads all [`TurnRecord`] items from a buffered `kakeibo.ledger/1` NDJSON stream.
pub fn read_ledger<R: BufRead>(reader: R) -> Result<Vec<TurnRecord>, NdjsonError> {
    LedgerReader::new(reader).collect()
}
