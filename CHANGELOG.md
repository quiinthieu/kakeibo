# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Rust workspace skeleton with `kakeibo-core`, `kakeibo-parse`, `kakeibo-index`, `kakeibo-report`, and `kakeibo` CLI crate stubs (T01).
- Single source of truth for product identifiers and CLI `--version` output in `kakeibo-core::product` (T01).
- Toolchain (`rust-toolchain.toml`), dependency policy (`deny.toml`), and Linux/macOS GitHub Actions CI workflow (`.github/workflows/ci.yml`) (T01).
- Zero-network privacy lint (`scripts/lint-privacy.sh`) and synthetic fixture sanitisation lint (`scripts/lint-fixtures.sh`) with automated verification tests (T01).
- Architectural crate-boundary and `kakeibo-core` purity enforcement tests (`DependencyGraphTests`) (T01).
