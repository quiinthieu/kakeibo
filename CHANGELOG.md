# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Pure API-equivalent pricing engine (`kakeibo-core::pricing`: `PricingCatalog`, `ModelPriceEntry`, `RateUnit`, `CostStatus`, `CacheWriteTierTokens`, `TurnCostBreakdown`, `SessionCostAttribution`, `price_session`, `import_litellm_json_str`) supporting `5m`/`1h` prompt-cache write tier splits, reasoning-token rate splits, exact per-turn and per-category cost reconciliation, offline LiteLLM `model_prices_and_context_window.json` import, and bundled empty-rate placeholder defaults (`data/pricing.toml`, `docs/formats/litellm-pricing.md`) (T09).
- Sole-writer user pricing overlay store (`kakeibo-index::{AppPaths, load_effective_pricing, save_user_pricing}`) with atomic write-and-rename inside Kakeibo's own config directory, `kakeibo pricing list | check | import <file>` CLI subcommands, per-turn and per-category cost rendering across terminal tables, Markdown reports, and `schemaVersion: 1` JSON exports (never displaying `$0` for unpriced models), and CI static price-literal enforcement (`scripts/lint-pricing.sh`, `tests/fixtures/pricing/litellm-tiny.json`) (T09).

## [0.1.0] - 2026-10-31

### Added
- MVP terminal CLI (`kakeibo`) covering the default recent-sessions view, `sessions [--agent] [--since] [--project]`, `autopsy <id|latest> [--turns] [--category]`, `flags [--since]`, `top --by category|tool|project|flag [--since]`, `export <id|--since> --format json|md|ndjson [--out] [--show-paths]`, `doctor`, and `--no-cache`, with read-only Claude Code + Codex subagent rollup and corpus `user_median_p1` computation (T08).
- Versioned CLI JSON export v1 specification (`docs/cli-schema.md`, `schemaVersion: 1`), structured JSON/Markdown/table/doctor report builders (`kakeibo-report::{model,json_export,markdown,table}`), opt-in `--show-paths` raw working-directory scan (`kakeibo-parse::cwd`, omitted by default), explicit `"unpriced"` cost rendering (never `$0`), and `assert_cmd` + `insta` golden CLI integration suite (T08).
- Pure v1 root-cause flag evaluation engine (`kakeibo-core::flags`: `evaluate_flags_v1`, `evaluate_flags_v1_default`, `FlagId`, `TurnRange`, `FlagEvidence`, `FlagFinding`, `FlagsConfig`, `ModelCatalog`) covering `cold-resume`, `near-full-context`, `tool-output-bloat`, `cache-miss-storm`, and `heavy-overhead` with `u128` threshold evaluation, deterministic impact ranking, and provenance-validated `data/flags.toml` and `data/models.toml` configuration tables (T07).
- Plain-words single-sentence and provenance-labelled evidence text renderer (`kakeibo-report::flags`: `render_flag_sentence`, `render_flag_evidence`, `render_flag_finding`, `render_flags_text`) with neutral client-side wording and `insta` text goldens (T07).
- Deterministic synthetic fixture scenarios for `claude-code/{resume-cold,near-full-context,cache-miss-storm,heavy-overhead}` in `scripts/gen-fixtures/generate.py` alongside `tool-bloat` and `basic` silence tests (T07).
- Pure per-turn and per-session token attribution and carry engine (`kakeibo-core::attrib`: `attribute_session`, `attribute_session_with_subagents`, `TokenSplit`, `CategoryBreakdown`, `CarriedBlockRecord`, `TurnAttribution`, `SessionAttribution`, `SubagentSessionSummary`) using `u128` capped Largest-Remainder (Hamilton) integer apportionment across all 9 `Category` variants with compaction eviction, retry `key_hash` tracking, subagent rollup, and exact 4-component measured reconciliation (T06).
- Pure UTF-8 byte-ceiling block token estimator ADR (`docs/decisions/002-token-estimator.md`) and unified `kakeibo_core::attrib::estimate_tokens_from_bytes` (`ESTIMATION_METHOD_SCALED_BYTES_DIV4`) shared across `kakeibo-core` and `kakeibo-parse` (T06).
- Deterministic 5-turn `claude-code/tool-bloat` synthetic fixture in `scripts/gen-fixtures/generate.py`, `proptest` category-reconciliation property suite (`attribution_proptest`), and end-to-end fixture + NDJSON round-trip attribution snapshot tests (`fixture_attribution`) (T06).
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
