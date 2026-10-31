#!/usr/bin/env bash
# Synthetic fixture sanitisation lint (`PLAN.md` §9 & T01).
#
# Rejects real-looking home directory paths (e.g. `/Users/alice`, `/home/bob`),
# real-looking email addresses, and API key / secret strings in `tests/fixtures/`.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

if [ "$#" -ge 1 ]; then
    FIXTURES_DIR="$1"
    if [ ! -e "$FIXTURES_DIR" ]; then
        echo "lint-fixtures: path not found: $FIXTURES_DIR" >&2
        exit 2
    fi
else
    FIXTURES_DIR="$REPO_ROOT/tests/fixtures"
    if [ ! -d "$FIXTURES_DIR" ]; then
        echo "lint-fixtures: ok (no tests/fixtures directory yet)"
        exit 0
    fi
fi

FAIL=0
FILE_COUNT=0

HOME_PATH_REGEX='(/Users/[A-Za-z0-9._-]+|/home/[A-Za-z0-9._-]+|[A-Za-z]:(\\+|/)Users(\\+|/)[A-Za-z0-9._-]+)'
EMAIL_REGEX='[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}'
ALLOWED_EMAIL_DOMAIN_REGEX='@(example\.(com|org|net)|([A-Za-z0-9.-]+\.)?(invalid|test|localhost))$'
SECRET_KEY_REGEX='(sk-ant-[A-Za-z0-9_-]{8,}|sk-(proj|live)-[A-Za-z0-9_-]{8,}|sk-[A-Za-z0-9]{20,}|(ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{16,}|github_pat_[A-Za-z0-9_]{16,}|AIza[0-9A-Za-z_-]{20,}|AKIA[0-9A-Z]{16}|-----BEGIN [A-Z ]*PRIVATE KEY-----)'

while IFS= read -r -d '' file; do
    FILE_COUNT=$((FILE_COUNT + 1))

    # 1. Real-looking home paths
    if grep -a -nE "$HOME_PATH_REGEX" "$file" >&2; then
        echo "lint-fixtures: real-looking home path in $file" >&2
        FAIL=1
    fi

    # 2. Real-looking email addresses (excluding RFC 2606 reserved domains)
    EMAIL_MATCHES="$(grep -a -nEo "$EMAIL_REGEX" "$file" | grep -vE "$ALLOWED_EMAIL_DOMAIN_REGEX" || true)"
    if [ -n "$EMAIL_MATCHES" ]; then
        echo "$EMAIL_MATCHES" >&2
        echo "lint-fixtures: real-looking email address in $file" >&2
        FAIL=1
    fi

    # 3. API keys and private key headers
    if grep -a -nE "$SECRET_KEY_REGEX" "$file" >&2; then
        echo "lint-fixtures: secret/key-like string in $file" >&2
        FAIL=1
    fi
done < <(find "$FIXTURES_DIR" -type f -not -path '*/.git/*' -print0)

if [ "$FAIL" -ne 0 ]; then
    echo "lint-fixtures: FAILED (synthetic fixtures must not contain personal paths, emails, or keys)" >&2
    exit 1
fi

echo "lint-fixtures: ok ($FILE_COUNT files checked)"
