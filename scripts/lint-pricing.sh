#!/usr/bin/env bash
# No-hardcoded-price-literals static lint (`PLAN.md` §0 #7, §6.2, §10 T09).
#
# Enforces two invariants:
#   1. Bundled `data/pricing.toml` ships with empty rate placeholders (zero numeric
#      assignments to `input`, `cache_write_5m`, `cache_write_1h`, `cache_read`,
#      `output`, or `reasoning`).
#   2. Production Rust/Swift source files (`crates/*/src/**/*.rs`, `macos/**/*.swift`)
#      contain no hardcoded per-token or per-model price literals.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TARGET_ROOT="${1:-$REPO_ROOT}"

if [ ! -d "$TARGET_ROOT" ]; then
    echo "lint-pricing: directory not found: $TARGET_ROOT" >&2
    exit 2
fi

FAIL=0

# 1. Verify bundled `data/pricing.toml` (when present) ships with empty rate defaults.
PRICING_TOML="$TARGET_ROOT/data/pricing.toml"
BUNDLED_RATE_ASSIGN_REGEX='^[[:space:]]*(input|cache_write_5m|cache_write_1h|cache_read|output|reasoning)[[:space:]]*='

if [ -f "$PRICING_TOML" ]; then
    if grep -nE "$BUNDLED_RATE_ASSIGN_REGEX" "$PRICING_TOML" >&2; then
        echo "lint-pricing: bundled $PRICING_TOML must ship with empty rate placeholders (no numeric price defaults)" >&2
        FAIL=1
    fi
fi

# 2. Collect production source roots (`crates/*/src` and `macos/`).
SEARCH_DIRS=()
if [ -d "$TARGET_ROOT/crates" ]; then
    while IFS= read -r -d '' crate_src; do
        SEARCH_DIRS+=("$crate_src")
    done < <(find "$TARGET_ROOT/crates" -mindepth 2 -maxdepth 2 -type d -name 'src' -print0)
fi
if [ -d "$TARGET_ROOT/macos" ]; then
    SEARCH_DIRS+=("$TARGET_ROOT/macos")
fi
if [ "${#SEARCH_DIRS[@]}" -eq 0 ] && [ "$TARGET_ROOT" != "$REPO_ROOT" ] && [ ! -d "$TARGET_ROOT/data" ]; then
    SEARCH_DIRS+=("$TARGET_ROOT")
fi

# Per-token scientific literals (e.g. `3e-6`, `1.5e-05`, `7.5e-5`) and small decimal per-token prices (`0.000003`).
PER_TOKEN_LITERAL_REGEX='(^|[^A-Za-z0-9_])([0-9]+(\.[0-9]+)?[eE]-0?[4-8]|0\.000[0-9]+)([^0-9]|$)'

# Inline float literal assignments to rate variables / struct fields.
RATE_ASSIGN_REGEX='(input_cost_per_token|output_cost_per_token|cache_creation_input_token_cost|cache_creation_input_token_cost_above_1hr|cache_read_input_token_cost|output_cost_per_reasoning_token|price_per_mtok|usd_per_mtok|cost_per_mtok|input_rate|output_rate|cache_write_5m|cache_write_1h|cache_read_rate|reasoning_rate)[[:space:]]*(:[[:space:]]*f64[[:space:]]*=|[:=])[[:space:]]*(Some\()?[0-9]+\.[0-9]+'

# Model-slug match arms mapping directly to numeric price literals.
MODEL_PRICE_ARM_REGEX='"(claude-|gpt-|gemini-|o[134]-)[^"]*"[[:space:]]*=>[[:space:]]*\(?[0-9]+\.[0-9]+'

if [ "${#SEARCH_DIRS[@]}" -gt 0 ]; then
    while IFS= read -r -d '' src_file; do
        if grep -nE "$PER_TOKEN_LITERAL_REGEX" "$src_file" >&2; then
            echo "lint-pricing: hardcoded per-token price literal in $src_file" >&2
            FAIL=1
        fi
        if grep -nE "$RATE_ASSIGN_REGEX" "$src_file" >&2; then
            echo "lint-pricing: hardcoded rate literal assignment in $src_file" >&2
            FAIL=1
        fi
        if grep -nE "$MODEL_PRICE_ARM_REGEX" "$src_file" >&2; then
            echo "lint-pricing: hardcoded model price mapping in $src_file" >&2
            FAIL=1
        fi
    done < <(find "${SEARCH_DIRS[@]}" -type f \( -name '*.rs' -o -name '*.swift' \) -not -path '*/target/*' -not -path '*/.git/*' -print0)
fi

if [ "$FAIL" -ne 0 ]; then
    echo "lint-pricing: FAILED (prices must live in user config, never as code literals or non-empty shipped defaults)" >&2
    exit 1
fi

echo "lint-pricing: ok"
