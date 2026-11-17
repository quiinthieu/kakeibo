#!/usr/bin/env python3
"""Deterministic 1 GiB synthetic JSONL benchmark corpus generator (`PLAN.md` §7, §9, §10 T12).

Generates a byte-exact multi-session Claude Code + Codex CLI synthetic corpus
inside an isolated temporary `HOME` directory (never inside the repo or real `$HOME`).
Uses SplitMix64 deterministic blocks with realistic streaming duplicate chunks,
large tool outputs, non-turn metadata/delta records, and prompt-cache carry.
Contains zero personal paths, emails, or key-like strings.
"""

from __future__ import annotations

import argparse
import io
import json
from pathlib import Path

MASK64 = (1 << 64) - 1
DEFAULT_TARGET_BYTES = 1024 * 1024 * 1024  # 1 GiB (1,073,741,824 bytes)

SYNTH_WORDS = (
    "alpha",
    "anchor",
    "archive",
    "balance",
    "beacon",
    "block",
    "budget",
    "cache",
    "catalog",
    "canvas",
    "checkpoint",
    "cipher",
    "circuit",
    "compass",
    "context",
    "counter",
    "cursor",
    "delta",
    "diary",
    "digest",
    "engine",
    "entry",
    "epoch",
    "factor",
    "filter",
    "frame",
    "garden",
    "graph",
    "harbor",
    "header",
    "horizon",
    "index",
    "journal",
    "kernel",
    "lantern",
    "ledger",
    "manifest",
    "marker",
    "matrix",
    "metric",
    "module",
    "offset",
    "orbit",
    "packet",
    "parser",
    "pattern",
    "prefix",
    "prism",
    "quartz",
    "record",
    "relay",
    "report",
    "router",
    "sample",
    "schema",
    "sector",
    "signal",
    "source",
    "stream",
    "stride",
    "symbol",
    "syntax",
    "target",
    "tensor",
)


class SplitMix64:
    def __init__(self, seed: int) -> None:
        self.state = seed & MASK64

    def next_u64(self) -> int:
        self.state = (self.state + 0x9E3779B97F4A7C15) & MASK64
        z = self.state
        z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & MASK64
        z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & MASK64
        return (z ^ (z >> 31)) & MASK64


def make_payload_pool(rng: SplitMix64, size: int, count: int = 8) -> list[str]:
    pool: list[str] = []
    for idx in range(count):
        words = [SYNTH_WORDS[rng.next_u64() % len(SYNTH_WORDS)] for _ in range(96)]
        unit = f"/* synth_block_{idx:02d} */ " + " ".join(words) + " "
        repeats = (size // len(unit)) + 2
        pool.append((unit * repeats)[:size])
    return pool


def write_claude_session_file(
    path: Path,
    session_idx: int,
    target_file_bytes: int,
    tool_blobs: list[str],
    snap_blobs: list[str],
    reply_blobs: list[str],
) -> tuple[int, int]:
    sid = f"claude-bench-{session_idx:04d}"
    cwd = f"/workspace/synth-bench/claude-repo-{session_idx % 8:02d}"
    written = 0
    turns = 0
    turn_idx = 0

    with io.open(path, "wb", buffering=4 * 1024 * 1024) as f:
        while written < target_file_bytes:
            turn_idx += 1
            turns += 1
            mm = (turn_idx // 60) % 60
            ss = turn_idx % 60
            hh = 10 + ((turn_idx // 3600) % 10)
            ts = f"2026-10-31T{hh:02d}:{mm:02d}:{ss:02d}Z"

            snap = snap_blobs[(session_idx + turn_idx) % len(snap_blobs)]
            tool_out = tool_blobs[(session_idx * 3 + turn_idx) % len(tool_blobs)]
            reply = reply_blobs[(session_idx + turn_idx * 5) % len(reply_blobs)]

            cache_write = 12_500 if turn_idx == 1 else (9_000 if turn_idx % 17 == 0 else 250)
            cache_read = 0 if turn_idx == 1 else min(185_000, 12_000 + turn_idx * 2_400)
            fresh_in = 850 + (turn_idx % 7) * 120
            out_partial = 35
            out_final = 180 + (turn_idx % 11) * 25

            lines = (
                f'{{"type":"file-history-snapshot","messageId":"snap-{session_idx}-{turn_idx}","snapshot":{{"digest":"{snap}"}}}}\n'
                f'{{"type":"progress","sessionId":"{sid}","data":{{"step":{turn_idx},"note":"{snap[:512]}"}}}}\n'
                f'{{"type":"user","uuid":"u-{session_idx}-{turn_idx}","sessionId":"{sid}","version":"2.1.314","cwd":"{cwd}","message":{{"role":"user","content":[{{"type":"text","text":"analyse module turn {turn_idx}"}},{{"type":"tool_result","tool_use_id":"toolu-{session_idx}-{turn_idx - 1}","content":"{tool_out}"}}]}}}}\n'
                f'{{"type":"assistant","uuid":"a1-{session_idx}-{turn_idx}","sessionId":"{sid}","requestId":"req-{session_idx}-{turn_idx}","timestamp":"{ts}","version":"2.1.314","cwd":"{cwd}","message":{{"id":"msg-{session_idx}-{turn_idx}","model":"claude-sonnet-4-5","content":[{{"type":"thinking","thinking":"{reply[:512]}"}}],"usage":{{"input_tokens":{fresh_in},"cache_creation_input_tokens":{cache_write},"cache_read_input_tokens":{cache_read},"output_tokens":{out_partial}}}}}}}\n'
                f'{{"type":"assistant","uuid":"a2-{session_idx}-{turn_idx}","sessionId":"{sid}","requestId":"req-{session_idx}-{turn_idx}","timestamp":"{ts}","version":"2.1.314","cwd":"{cwd}","message":{{"id":"msg-{session_idx}-{turn_idx}","model":"claude-sonnet-4-5","content":[{{"type":"thinking","thinking":"{reply[:512]}"}},{{"type":"text","text":"{reply}"}},{{"type":"tool_use","id":"toolu-{session_idx}-{turn_idx}","name":"Read","input":{{"file_path":"crates/mod_{turn_idx % 12}.rs"}}}}],"usage":{{"input_tokens":{fresh_in},"cache_creation_input_tokens":{cache_write},"cache_read_input_tokens":{cache_read},"output_tokens":{out_final}}}}}}}\n'
            ).encode("utf-8")

            f.write(lines)
            written += len(lines)

    return written, turns


def write_codex_session_file(
    path: Path,
    session_idx: int,
    target_file_bytes: int,
    tool_blobs: list[str],
    delta_blobs: list[str],
    reply_blobs: list[str],
) -> tuple[int, int]:
    sid = f"codex-bench-{session_idx:04d}"
    cwd = f"/workspace/synth-bench/codex-repo-{session_idx % 8:02d}"
    written = 0
    turns = 0
    turn_idx = 0
    cum_in = 0
    cum_cached = 0
    cum_out = 0

    with io.open(path, "wb", buffering=4 * 1024 * 1024) as f:
        header = (
            f'{{"timestamp":"2026-10-31T10:00:00Z","type":"session_meta","payload":{{"id":"{sid}","cwd":"{cwd}","cli_version":"0.165.0"}}}}\n'
            f'{{"timestamp":"2026-10-31T10:00:00Z","type":"turn_context","payload":{{"cwd":"{cwd}","model":"gpt-5-codex"}}}}\n'
        ).encode("utf-8")
        f.write(header)
        written += len(header)

        while written < target_file_bytes:
            turn_idx += 1
            turns += 1
            mm = (turn_idx // 60) % 60
            ss = turn_idx % 60
            hh = 10 + ((turn_idx // 3600) % 10)
            ts = f"2026-10-31T{hh:02d}:{mm:02d}:{ss:02d}Z"

            delta = delta_blobs[(session_idx + turn_idx) % len(delta_blobs)]
            tool_out = tool_blobs[(session_idx * 5 + turn_idx) % len(tool_blobs)]
            reply = reply_blobs[(session_idx + turn_idx * 3) % len(reply_blobs)]

            cached = 0 if turn_idx == 1 else min(190_000, 10_000 + turn_idx * 2_200)
            inp = cached + 1_100 + (turn_idx % 9) * 90
            out = 160 + (turn_idx % 13) * 20
            cum_in += inp
            cum_cached += cached
            cum_out += out
            used_pct = min(98.5, round(5.0 + turn_idx * 0.45, 2))

            lines = (
                f'{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"agent_message_delta","delta":"{delta}"}}}}\n'
                f'{{"timestamp":"{ts}","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"inspect benchmark step {turn_idx}"}}]}}}}\n'
                f'{{"timestamp":"{ts}","type":"response_item","payload":{{"type":"function_call","name":"exec_command","call_id":"call-{session_idx}-{turn_idx}","arguments":"{{\\"cmd\\":\\"cargo test --lib mod_{turn_idx % 9}\\"}}"}}}}\n'
                f'{{"timestamp":"{ts}","type":"response_item","payload":{{"type":"function_call_output","call_id":"call-{session_idx}-{turn_idx}","output":"{tool_out}"}}}}\n'
                f'{{"timestamp":"{ts}","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"{reply}"}}]}}}}\n'
                f'{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"model_context_window":272000,"last_token_usage":{{"input_tokens":{inp},"cached_input_tokens":{cached},"output_tokens":{out},"reasoning_output_tokens":30,"total_tokens":{inp + out}}},"total_token_usage":{{"input_tokens":{cum_in},"cached_input_tokens":{cum_cached},"output_tokens":{cum_out},"reasoning_output_tokens":{turns * 30},"total_tokens":{cum_in + cum_out}}}}},"rate_limits":{{"primary":{{"used_percent":{used_pct}}}}}}}}}\n'
                f'{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"turn_completed","turn_id":"turn-{session_idx}-{turn_idx}","model":"gpt-5-codex","usage":{{"input_tokens":{inp},"cached_input_tokens":{cached},"output_tokens":{out},"reasoning_output_tokens":30}}}}}}\n'
            ).encode("utf-8")

            f.write(lines)
            written += len(lines)

    return written, turns


def main() -> None:
    parser = argparse.ArgumentParser(description="Generate deterministic synthetic JSONL benchmark corpus.")
    parser.add_argument("--home", required=True, type=Path, help="Isolated benchmark HOME directory.")
    parser.add_argument("--bytes", type=int, default=DEFAULT_TARGET_BYTES, help="Exact target corpus bytes.")
    parser.add_argument("--files", type=int, default=64, help="Number of session log files to distribute across.")
    parser.add_argument("--seed", type=int, default=42, help="Deterministic SplitMix64 seed.")
    args = parser.parse_args()

    target_bytes = args.bytes
    num_files = max(2, args.files)
    rng = SplitMix64(args.seed)

    claude_root = args.home / ".claude" / "projects"
    codex_root = args.home / ".codex" / "sessions" / "2026" / "10" / "31"
    codex_root.mkdir(parents=True, exist_ok=True)

    # Pre-build deterministic synthetic text pools (24 KiB tool outputs, 6 KiB snapshots/deltas, 2 KiB replies).
    tool_blobs = make_payload_pool(rng, 24 * 1024, count=8)
    snap_blobs = make_payload_pool(rng, 6 * 1024, count=8)
    reply_blobs = make_payload_pool(rng, 2 * 1024, count=8)

    total_written = 0
    total_turns = 0
    claude_files = 0
    codex_files = 0
    last_claude_path: Path | None = None
    last_claude_sid: str = ""

    # Reserve up to 128 KiB slack on the final file so we can land on `target_bytes` to the exact byte.
    per_file_target = max(65_536, (target_bytes - 131_072) // num_files)

    for idx in range(num_files):
        remaining = target_bytes - total_written
        if remaining <= 0:
            break

        is_last = idx == num_files - 1
        budget = max(4_096, remaining - 98_304) if is_last else min(per_file_target, max(4_096, remaining // (num_files - idx)))

        if idx % 2 == 0 or is_last:
            proj_dir = claude_root / f"synth-proj-{idx % 8:02d}"
            proj_dir.mkdir(parents=True, exist_ok=True)
            path = proj_dir / f"claude-bench-{idx:04d}.jsonl"
            file_bytes, turns = write_claude_session_file(
                path, idx, budget, tool_blobs, snap_blobs, reply_blobs
            )
            claude_files += 1
            last_claude_path = path
            last_claude_sid = f"claude-bench-{idx:04d}"
        else:
            path = codex_root / f"rollout-2026-10-31T10-00-00-codex-bench-{idx:04d}.jsonl"
            file_bytes, turns = write_codex_session_file(
                path, idx, budget, tool_blobs, snap_blobs, reply_blobs
            )
            codex_files += 1

        total_written += file_bytes
        total_turns += turns

    assert last_claude_path is not None
    # Pad or trim cleanly at record boundary on `last_claude_path` to hit `target_bytes` exactly if below target.
    if total_written < target_bytes:
        deficit = target_bytes - total_written
        prefix = b'{"type":"file-history-snapshot","messageId":"snap-pad","snapshot":{"pad":"'
        suffix = b'"}}\n'
        min_rec = len(prefix) + len(suffix)
        with io.open(last_claude_path, "ab") as f:
            while deficit >= min_rec * 2 or deficit == min_rec:
                chunk_total = deficit if deficit <= 1_048_576 else (deficit - min_rec)
                if deficit - chunk_total < min_rec and chunk_total != deficit:
                    chunk_total = deficit - min_rec
                pad_len = chunk_total - min_rec
                rec = prefix + (b"p" * pad_len) + suffix
                f.write(rec)
                total_written += len(rec)
                deficit -= len(rec)
            if deficit >= min_rec:
                pad_len = deficit - min_rec
                rec = prefix + (b"p" * pad_len) + suffix
                f.write(rec)
                total_written += len(rec)

    summary = {
        "total_bytes": total_written,
        "target_bytes": target_bytes,
        "files": claude_files + codex_files,
        "claude_files": claude_files,
        "codex_files": codex_files,
        "total_turns": total_turns,
        "append_target": str(last_claude_path),
        "append_session_id": last_claude_sid,
    }
    print(json.dumps(summary))


if __name__ == "__main__":
    main()
