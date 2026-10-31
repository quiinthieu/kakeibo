#!/usr/bin/env bash
# Privacy & zero-network static lint (`PLAN.md` §0 #4).
#
# Fails if any Rust/Swift source references networking APIs (`std::net`,
# `tokio::net`, `URLSession`, etc.) or if any Cargo manifest/lockfile pulls in
# banned network crates (`reqwest`, `hyper`, `ureq`, `curl`, etc.).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TARGET_ROOT="${1:-$REPO_ROOT}"

if [ ! -d "$TARGET_ROOT" ]; then
    echo "lint-privacy: directory not found: $TARGET_ROOT" >&2
    exit 2
fi

FAIL=0

# 1. Scan Rust and Swift source files under crates/ and macos/ (or TARGET_ROOT if custom dir).
SEARCH_DIRS=()
if [ -d "$TARGET_ROOT/crates" ]; then
    SEARCH_DIRS+=("$TARGET_ROOT/crates")
fi
if [ -d "$TARGET_ROOT/macos" ]; then
    SEARCH_DIRS+=("$TARGET_ROOT/macos")
fi
if [ "${#SEARCH_DIRS[@]}" -eq 0 ] && [ "$TARGET_ROOT" != "$REPO_ROOT" ]; then
    SEARCH_DIRS+=("$TARGET_ROOT")
fi

FORBIDDEN_CODE_REGEX='(std::net|tokio::net|async_std::net|smol::net|reqwest|hyper|ureq|URLSession|NSURLSession|NWConnection|CFNetwork)'

if [ "${#SEARCH_DIRS[@]}" -gt 0 ]; then
    while IFS= read -r -d '' src_file; do
        if grep -nE "$FORBIDDEN_CODE_REGEX" "$src_file" >&2; then
            echo "lint-privacy: forbidden network symbol in $src_file" >&2
            FAIL=1
        fi
    done < <(find "${SEARCH_DIRS[@]}" -type f \( -name '*.rs' -o -name '*.swift' \) -not -path '*/target/*' -not -path '*/.git/*' -print0)
fi

# 2. Scan Cargo.toml manifests for banned network crate dependencies.
BANNED_CRATES_TOML_REGEX='^[[:space:]]*(reqwest|hyper|ureq|curl|curl-sys|surf|isahc|attohttpc|minreq|h2|tungstenite|tokio-tungstenite)[[:space:]]*[=.]'

while IFS= read -r -d '' manifest; do
    if grep -nE "$BANNED_CRATES_TOML_REGEX" "$manifest" >&2; then
        echo "lint-privacy: banned network dependency in $manifest" >&2
        FAIL=1
    fi
done < <(find "$TARGET_ROOT" -type f -name 'Cargo.toml' -not -path '*/target/*' -not -path '*/.git/*' -print0)

# 3. Scan Cargo.lock for banned network packages anywhere in the transitive graph.
BANNED_CRATES_LOCK_REGEX='^name = "(reqwest|hyper|ureq|curl|curl-sys|surf|isahc|attohttpc|minreq|h2|tungstenite|tokio-tungstenite)"$'

while IFS= read -r -d '' lockfile; do
    if grep -nE "$BANNED_CRATES_LOCK_REGEX" "$lockfile" >&2; then
        echo "lint-privacy: banned network crate resolved in $lockfile" >&2
        FAIL=1
    fi
done < <(find "$TARGET_ROOT" -type f -name 'Cargo.lock' -not -path '*/target/*' -not -path '*/.git/*' -print0)

if [ "$FAIL" -ne 0 ]; then
    echo "lint-privacy: FAILED (network usage is strictly forbidden)" >&2
    exit 1
fi

echo "lint-privacy: ok"
