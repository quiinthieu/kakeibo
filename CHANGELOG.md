# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Field-name-only format documentation verified against installed `Claude Code 2.1.314` (`docs/formats/claude-code.md`) and `codex-cli 0.165.0` (`docs/formats/codex.md`) with per-field source citations (T02).
- Deterministic SplitMix64 synthetic fixture generator (`scripts/gen-fixtures/generate.py`) and ground-truth `expected.json` corpus for Claude Code (`basic`, `streaming-dupes`) and Codex CLI (`basic`, `codex-dual-usage`) (T02).
- Isolated temp-`HOME` test harness (`TestEnv` + `ReadOnlyGuard`), automated field-citation and seed-determinism tests, and `insta` golden snapshots (T02).
- Rust workspace skeleton with `kakeibo-core`, `kakeibo-parse`, `kakeibo-index`, `kakeibo-report`, and `kakeibo` CLI crate stubs (T01).
- Single source of truth for product identifiers and CLI `--version` output in `kakeibo-core::product` (T01).
- Toolchain (`rust-toolchain.toml`), dependency policy (`deny.toml`), and Linux/macOS GitHub Actions CI workflow (`.github/workflows/ci.yml`) (T01).
- Zero-network privacy lint (`scripts/lint-privacy.sh`) and synthetic fixture sanitisation lint (`scripts/lint-fixtures.sh`) with automated verification tests (T01).
- Architectural crate-boundary and `kakeibo-core` purity enforcement tests (`DependencyGraphTests`) (T01).
