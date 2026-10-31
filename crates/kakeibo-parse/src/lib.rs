//! Read-only per-agent log discovery and streaming decoders for Kakeibo.

pub mod catalog;
pub mod claude;
pub mod codex;
pub mod hash;
pub mod line_reader;

pub use catalog::{open_readonly, DiscoveredLog, DiscoveredSubagentLog, PathCatalog};
pub use claude::{
    parse_claude_file, parse_claude_file_with_options, parse_claude_reader, ClaudeParseOptions,
    ClaudeParseOutput,
};
pub use codex::{parse_codex_file, parse_codex_reader, CodexParseOutput};
pub use line_reader::{
    BoundedLineReader, LineReadOutcome, ParseDiagnostic, ParseDiagnosticKind, ParseDiagnostics,
    DEFAULT_MAX_LINE_BYTES,
};
