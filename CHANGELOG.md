# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- OpenAI Codex CLI streaming `.jsonl` and `.jsonl.zst` rollout parser (`kakeibo-parse::codex`) with bidirectional `token_usage_record` / `event_msg(token_count)` dual-usage consolidation, `response_id` and duplicate cumulative-snapshot deduplication, cumulative-only `total_token_usage` delta fallback, inclusive cache-token normalisation, fork/subagent inherited parent-history filtering, `rate_limits.primary.used_percent` and `model_context_window` extraction, and immediate raw-text dropping (T05).
- Read-only Codex CLI rollout discovery (`PathCatalog::discover_codex_rollouts` across `sessions/**` and `archived_sessions/**` with `CODEX_HOME` override) and graceful Zstandard decompression-error diagnostics (`ParseDiagnosticKind::DecompressionError`) (T05).
- Verified `codex-cli 0.165.0` fork/subagent (`forked_from_id`, `subagent_history_start_ordinal`), compaction (`compacted`, `context_compacted`), and bidirectional `tur <-> tc` dual-usage schema documentation in `docs/formats/codex.md` (T05).
- Claude Code streaming JSONL parser (`kakeibo-parse::claude`) with `(message.id, requestId)` consolidation (`max(output_tokens)`), event-UUID and block-fingerprint deduplication, immediate raw-text dropping, canonical tool-input hashing (`key_hash`), and verified `resume_start`, `compaction`, `sidechain`, and `api_error` hints (T04).
- Read-only Claude Code log discovery (`PathCatalog::discover_claude_top_level` and `discover_claude_subagents` with `CLAUDE_CONFIG_DIR` override) and `memchr`-backed 16 MiB `BoundedLineReader` emitting structured `ParseDiagnostics` on oversized, non-UTF-8, malformed, or truncated lines (T04).
- Verified `Claude Code 2.1.314` compaction (`compact_boundary`, `isCompactSummary`), resume (`away_summary`, root-parent reset, fork switch), API-error (`isApiErrorMessage`), and `<session>/subagents/*.jsonl` schema documentation in `docs/formats/claude-code.md` (T04).
- Pure domain model in `kakeibo-core` (`TurnRecord`, `Usage`, `ContentBlock`, `TurnHints`, `Category`, `Confidence`, `AgentId`, `BlockKind`, and UTC-normalised `Timestamp` backed by `time` per ADR 001) (T03).
- Dual-convention token-usage normalisation (`Usage::from_disjoint`, `Usage::from_inclusive_checked`, `Usage::from_inclusive_clamped`) with `proptest` verification of `fresh_in + cache_write + cache_read == P` across both conventions (T03).
- `kakeibo.ledger/1` NDJSON specification (`docs/ledger-schema.md`), pure in-memory codec (`kakeibo-core::ledger`), and bounded-memory streaming `BufRead`/`Write` NDJSON reader/writer (`kakeibo-report::ndjson`) with round-trip and golden snapshot tests (T03).
- Field-name-only format documentation verified against installed `Claude Code 2.1.314` (`docs/formats/claude-code.md`) and `codex-cli 0.165.0` (`docs/formats/codex.md`) with per-field source citations (T02).
- Deterministic SplitMix64 synthetic fixture generator (`scripts/gen-fixtures/generate.py`) and ground-truth `expected.json` corpus for Claude Code (`basic`, `streaming-dupes`) and Codex CLI (`basic`, `codex-dual-usage`) (T02).
- Isolated temp-`HOME` test harness (`TestEnv` + `ReadOnlyGuard`), automated field-citation and seed-determinism tests, and `insta` golden snapshots (T02).
- Rust workspace skeleton with `kakeibo-core`, `kakeibo-parse`, `kakeibo-index`, `kakeibo-report`, and `kakeibo` CLI crate stubs (T01).
- Single source of truth for product identifiers and CLI `--version` output in `kakeibo-core::product` (T01).
- Toolchain (`rust-toolchain.toml`), dependency policy (`deny.toml`), and Linux/macOS GitHub Actions CI workflow (`.github/workflows/ci.yml`) (T01).
- Zero-network privacy lint (`scripts/lint-privacy.sh`) and synthetic fixture sanitisation lint (`scripts/lint-fixtures.sh`) with automated verification tests (T01).
- Architectural crate-boundary and `kakeibo-core` purity enforcement tests (`DependencyGraphTests`) (T01).
