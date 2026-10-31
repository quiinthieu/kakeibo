//! Read-only per-agent log discovery and streaming decoders for Kakeibo.

pub mod aider;
pub mod catalog;
pub mod claude;
pub mod codex;
pub mod cursor;
pub mod cwd;
pub mod gemini;
pub mod hash;
pub mod line_reader;
pub mod opencode;
pub mod sqlite;

pub use aider::{
    parse_aider_analytics_file, parse_aider_analytics_reader, parse_aider_history_file,
    parse_aider_history_reader, AiderParseOutput, AiderSessionBatch,
};
pub use catalog::{open_readonly, DiscoveredLog, DiscoveredSubagentLog, PathCatalog};
pub use claude::{
    parse_claude_file, parse_claude_file_with_options, parse_claude_reader, ClaudeParseOptions,
    ClaudeParseOutput,
};
pub use codex::{parse_codex_file, parse_codex_reader, CodexParseOutput};
pub use cursor::{parse_cursor_vscdb, CursorParseOutput, CursorSessionBatch};
pub use cwd::{
    extract_aider_session_cwds, extract_claude_session_cwds, extract_codex_session_cwds,
    extract_cursor_session_cwds, extract_gemini_session_cwds, extract_opencode_session_cwds,
};
pub use gemini::{
    parse_gemini_file, parse_gemini_file_with_options, parse_gemini_reader, GeminiParseOptions,
    GeminiParseOutput,
};
pub use line_reader::{
    BoundedLineReader, LineReadOutcome, ParseDiagnostic, ParseDiagnosticKind, ParseDiagnostics,
    DEFAULT_MAX_LINE_BYTES,
};
pub use opencode::{
    parse_opencode_legacy_storage, parse_opencode_root, parse_opencode_sqlite, OpencodeParseOutput,
    OpencodeSessionBatch,
};
pub use sqlite::open_sqlite_readonly;
