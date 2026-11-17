#!/usr/bin/env bash
# End-to-end 1 GiB release benchmark harness (`PLAN.md` §7, §8, §9, §10 T12).
#
# Generates a deterministic temporary 1 GiB (`1,073,741,824`-byte) synthetic JSONL
# corpus across Claude Code and Codex CLI in an isolated temp `HOME`, builds a
# `--release` `kakeibo` binary, measures cold uncached, cold index-build, warm
# cached, and incremental `+1 turn` append wall-clock latency + kernel peak RSS,
# verifies byte-identical output parity (`cached == uncached`), and cleans up.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

CORPUS_BYTES=1073741824
CORPUS_FILES=64
SEED=42
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
CARGO_EXTRA_ARGS=()

while [ "$#" -gt 0 ]; do
    case "$1" in
        --bytes)
            CORPUS_BYTES="$2"
            shift 2
            ;;
        --files)
            CORPUS_FILES="$2"
            shift 2
            ;;
        --seed)
            SEED="$2"
            shift 2
            ;;
        --target-dir)
            TARGET_DIR="$2"
            shift 2
            ;;
        --offline)
            CARGO_EXTRA_ARGS+=("--offline")
            shift
            ;;
        *)
            echo "bench.sh: unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

WORK_DIR="$(mktemp -d /tmp/kakeibo-bench.XXXXXX)"
cleanup() {
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT

BENCH_HOME="$WORK_DIR/home"
BENCH_CONFIG="$WORK_DIR/xdg_config"
BENCH_CACHE="$WORK_DIR/xdg_cache"
BENCH_DATA="$WORK_DIR/xdg_data"
mkdir -p "$BENCH_HOME" "$BENCH_CONFIG" "$BENCH_CACHE" "$BENCH_DATA"

echo "==> Building release binary (target-dir: $TARGET_DIR)..."
cargo build --manifest-path "$REPO_ROOT/Cargo.toml" --release --locked "${CARGO_EXTRA_ARGS[@]}" --target-dir "$TARGET_DIR" --bin kakeibo

BIN_PATH="$TARGET_DIR/release/kakeibo"
if [ ! -x "$BIN_PATH" ]; then
    echo "bench.sh: expected release binary not found at $BIN_PATH" >&2
    exit 1
fi

echo "==> Generating deterministic synthetic corpus ($CORPUS_BYTES bytes, $CORPUS_FILES files, seed=$SEED)..."
CORPUS_JSON="$(python3 "$REPO_ROOT/scripts/gen-fixtures/gen_bench_corpus.py" \
    --home "$BENCH_HOME" \
    --bytes "$CORPUS_BYTES" \
    --files "$CORPUS_FILES" \
    --seed "$SEED")"
echo "    $CORPUS_JSON"

APPEND_TARGET="$(python3 -c 'import json, sys; print(json.loads(sys.argv[1])["append_target"])' "$CORPUS_JSON")"
APPEND_SID="$(python3 -c 'import json, sys; print(json.loads(sys.argv[1])["append_session_id"])' "$CORPUS_JSON")"

run_measured() {
    local label="$1"
    local out_file="$2"
    shift 2

    python3 - "$label" "$out_file" "$BENCH_HOME" "$BENCH_CONFIG" "$BENCH_CACHE" "$BENCH_DATA" "$WORK_DIR" "$BIN_PATH" "$@" <<'PY'
import json
import os
import platform
import re
import subprocess
import sys
import time

label, out_file, home, cfg, cache, data, work_dir, bin_path, *cli_args = sys.argv[1:]

env = {
    "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
    "HOME": home,
    "USERPROFILE": home,
    "XDG_CONFIG_HOME": cfg,
    "XDG_CACHE_HOME": cache,
    "XDG_DATA_HOME": data,
    "NO_COLOR": "1",
    "TZ": "UTC",
}

is_macos = platform.system() == "Darwin"
time_cmd = ["/usr/bin/time", "-l"] if is_macos else ["/usr/bin/time", "-v"]
full_cmd = time_cmd + [bin_path] + cli_args

t0 = time.perf_counter()
proc = subprocess.run(
    full_cmd,
    cwd=work_dir,
    env=env,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    check=True,
)
elapsed_ms = (time.perf_counter() - t0) * 1000.0

with open(out_file, "wb") as f:
    f.write(proc.stdout)

stderr_text = proc.stderr.decode("utf-8", errors="replace")
peak_rss_bytes = 0
if is_macos:
    # macOS `/usr/bin/time -l` reports `maximum resident set size` in bytes.
    m = re.search(r"(\d+)\s+maximum resident set size", stderr_text)
    if m:
        peak_rss_bytes = int(m.group(1))
else:
    # GNU `/usr/bin/time -v` reports `Maximum resident set size (kbytes)` in KiB.
    m = re.search(r"Maximum resident set size \(kbytes\):\s*(\d+)", stderr_text)
    if m:
        peak_rss_bytes = int(m.group(1)) * 1024

result = {
    "label": label,
    "elapsed_ms": round(elapsed_ms, 2),
    "elapsed_s": round(elapsed_ms / 1000.0, 3),
    "peak_rss_bytes": peak_rss_bytes,
    "peak_rss_mb": round(peak_rss_bytes / 1_000_000.0, 2),
    "peak_rss_mib": round(peak_rss_bytes / (1024.0 * 1024.0), 2),
}
print("    " + json.dumps(result))
PY
}

echo "==> Running benchmark passes..."
run_measured "cold_no_cache" "$WORK_DIR/out_nocache.txt" --no-cache sessions
run_measured "cold_index_build" "$WORK_DIR/out_cold_cache.txt" sessions
run_measured "warm_cached_1" "$WORK_DIR/out_warm_1.txt" sessions
run_measured "warm_cached_2" "$WORK_DIR/out_warm_2.txt" sessions
run_measured "warm_cached_3" "$WORK_DIR/out_warm_3.txt" sessions

diff -u "$WORK_DIR/out_nocache.txt" "$WORK_DIR/out_cold_cache.txt"
diff -u "$WORK_DIR/out_nocache.txt" "$WORK_DIR/out_warm_1.txt"
diff -u "$WORK_DIR/out_nocache.txt" "$WORK_DIR/out_warm_3.txt"

INDEX_DB="$BENCH_CACHE/kakeibo/index.sqlite"
if [ -f "$INDEX_DB" ]; then
    INDEX_BYTES="$(wc -c < "$INDEX_DB" | tr -d ' ')"
    echo "    index.sqlite size: ${INDEX_BYTES} bytes"
fi

# Append 1 new turn (~640 bytes) to the active session log and measure incremental resume.
printf '{"type":"assistant","uuid":"a-bench-append-1","sessionId":"%s","requestId":"req-bench-append-1","timestamp":"2026-10-31T23:59:59Z","version":"2.1.314","cwd":"/workspace/synth-bench/claude-repo-07","message":{"id":"msg-bench-append-1","model":"claude-sonnet-4-5","content":[{"type":"text","text":"incremental append verification turn"}],"usage":{"input_tokens":640,"cache_creation_input_tokens":0,"cache_read_input_tokens":142000,"output_tokens":95}}}\n' "$APPEND_SID" >> "$APPEND_TARGET"

run_measured "incremental_append_1_turn" "$WORK_DIR/out_append_cached.txt" sessions
run_measured "post_append_no_cache_verify" "$WORK_DIR/out_append_nocache.txt" --no-cache sessions
diff -u "$WORK_DIR/out_append_nocache.txt" "$WORK_DIR/out_append_cached.txt"

echo "==> Parity verified (cached == uncached before and after append). Benchmark complete."
