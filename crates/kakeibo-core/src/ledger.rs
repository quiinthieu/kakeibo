//! Pure in-memory `kakeibo.ledger/1` NDJSON codec (`docs/ledger-schema.md`).
//!
//! Performs zero I/O so `kakeibo-core` stays deterministic and FFI-shareable.
//! Streaming `BufRead` / `Write` adapters live in `kakeibo-report::ndjson`.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::model::TurnRecord;
pub use crate::product::LEDGER_SCHEMA_V1 as LEDGER_SCHEMA;

/// Errors returned while encoding or decoding `kakeibo.ledger/1` NDJSON records.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LedgerError {
    /// A JSON object omitted the required `"schema"` string field.
    #[error("missing required `schema` field{}", format_line_suffix(*.line))]
    MissingSchema { line: Option<usize> },
    /// A JSON object carried an unrecognised `"schema"` version tag.
    #[error(
        "unsupported ledger schema `{found}` (expected `{expected}`){}",
        format_line_suffix(*.line)
    )]
    UnsupportedSchema {
        found: String,
        expected: &'static str,
        line: Option<usize>,
    },
    /// A floating-point field contained `NaN` or `±Infinity`.
    #[error("field `{field}` must be a finite number, got {value}")]
    NonFiniteNumber { field: &'static str, value: f64 },
    /// JSON syntax or field type validation error.
    #[error("invalid ledger JSON{}: {message}", format_line_suffix(*.line))]
    Json {
        line: Option<usize>,
        message: String,
    },
}

impl LedgerError {
    /// Attaches a 1-based line number if one is not already recorded.
    #[must_use]
    pub fn with_line(self, line_number: usize) -> Self {
        match self {
            Self::MissingSchema { line: None } => Self::MissingSchema {
                line: Some(line_number),
            },
            Self::UnsupportedSchema {
                found,
                expected,
                line: None,
            } => Self::UnsupportedSchema {
                found,
                expected,
                line: Some(line_number),
            },
            Self::Json {
                line: None,
                message,
            } => Self::Json {
                line: Some(line_number),
                message,
            },
            other => other,
        }
    }
}

fn format_line_suffix(line: Option<usize>) -> String {
    match line {
        Some(n) => format!(" at line {n}"),
        None => String::new(),
    }
}

#[derive(Serialize)]
struct OutboundLedgerLine<'a> {
    schema: &'static str,
    #[serde(flatten)]
    record: &'a TurnRecord,
}

#[derive(Deserialize)]
struct SchemaHeader<'a> {
    #[serde(borrow)]
    schema: Option<Cow<'a, str>>,
}

fn validate_record_numbers(record: &TurnRecord) -> Result<(), LedgerError> {
    if let Some(cost) = record.usage.agent_reported_cost_usd {
        if !cost.is_finite() {
            return Err(LedgerError::NonFiniteNumber {
                field: "usage.agent_reported_cost_usd",
                value: cost,
            });
        }
    }
    if let Some(pct) = record.hints.rate_limit_used_percent {
        if !pct.is_finite() {
            return Err(LedgerError::NonFiniteNumber {
                field: "hints.rate_limit_used_percent",
                value: pct,
            });
        }
    }
    Ok(())
}

/// Encodes a single [`TurnRecord`] as one compact `kakeibo.ledger/1` JSON line
/// (without a trailing newline).
pub fn encode_line(record: &TurnRecord) -> Result<String, LedgerError> {
    validate_record_numbers(record)?;
    let envelope = OutboundLedgerLine {
        schema: LEDGER_SCHEMA,
        record,
    };
    serde_json::to_string(&envelope).map_err(|err| LedgerError::Json {
        line: None,
        message: err.to_string(),
    })
}

/// Decodes a single `kakeibo.ledger/1` JSON line into a [`TurnRecord`].
pub fn decode_line(line: &str) -> Result<TurnRecord, LedgerError> {
    let trimmed = line.trim();
    let header: SchemaHeader<'_> =
        serde_json::from_str(trimmed).map_err(|err| LedgerError::Json {
            line: None,
            message: err.to_string(),
        })?;

    match header.schema.as_deref() {
        None => return Err(LedgerError::MissingSchema { line: None }),
        Some(found) if found != LEDGER_SCHEMA => {
            return Err(LedgerError::UnsupportedSchema {
                found: found.to_string(),
                expected: LEDGER_SCHEMA,
                line: None,
            });
        }
        Some(_) => {}
    }

    let record: TurnRecord = serde_json::from_str(trimmed).map_err(|err| LedgerError::Json {
        line: None,
        message: err.to_string(),
    })?;
    validate_record_numbers(&record)?;
    Ok(record)
}

/// Encodes a sequence of [`TurnRecord`] items as `\n`-terminated NDJSON.
pub fn encode_ndjson<'a, I>(records: I) -> Result<String, LedgerError>
where
    I: IntoIterator<Item = &'a TurnRecord>,
{
    let mut out = String::new();
    for record in records {
        let line = encode_line(record)?;
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}

/// Decodes a multi-line `kakeibo.ledger/1` NDJSON buffer, ignoring blank lines
/// and reporting 1-based line numbers on failure.
pub fn decode_ndjson(ndjson: &str) -> Result<Vec<TurnRecord>, LedgerError> {
    let mut records = Vec::new();
    for (idx, raw_line) in ndjson.split('\n').enumerate() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let line_number = idx + 1;
        let record = decode_line(trimmed).map_err(|err| err.with_line(line_number))?;
        records.push(record);
    }
    Ok(records)
}
