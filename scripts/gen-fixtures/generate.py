#!/usr/bin/env python3
"""Deterministic synthetic fixture generator for Kakeibo (`PLAN.md` §9 & T02).

Uses an in-house 64-bit SplitMix64 PRNG so `--seed <N>` emits byte-for-byte
identical JSONL logs and `expected.json` ground-truth manifests across Python
versions, macOS, and Linux.

Usage:
    python3 scripts/gen-fixtures/generate.py --seed 42 --out tests/fixtures
    python3 scripts/gen-fixtures/generate.py --seed 42 --check
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

MASK64 = (1 << 64) - 1

SYNTH_WORDS = (
    "alpha",
    "anchor",
    "archive",
    "Attribution",
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
    "locale",
    "manifest",
    "marker",
    "matrix",
    "meadow",
    "metric",
    "module",
    "needle",
    "offset",
    "orbit",
    "packet",
    "parser",
    "pattern",
    "pebble",
    "pocket",
    "prefix",
    "prism",
    "prompt",
    "quartz",
    "record",
    "relay",
    "report",
    "ripple",
    "router",
    "sample",
    "schema",
    "sector",
    "signal",
    "sketch",
    "socket",
    "source",
    "stream",
    "stride",
    "summit",
    "symbol",
    "syntax",
    "tablet",
    "target",
    "tensor",
    "thread",
    "ticket",
    "timber",
    "token",
    "tracer",
    "tunnel",
    "vector",
    "vessel",
    "vertex",
    "window",
    "worker",
    "zenith",
)


class SplitMix64:
    """Portable 64-bit SplitMix64 PRNG with platform-independent output."""

    def __init__(self, seed: int) -> None:
        self.state = seed & MASK64

    def next_u64(self) -> int:
        self.state = (self.state + 0x9E3779B97F4A7C15) & MASK64
        z = self.state
        z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & MASK64
        z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & MASK64
        return (z ^ (z >> 31)) & MASK64

    def rand_range(self, low: int, high: int) -> int:
        span = high - low + 1
        return low + (self.next_u64() % span)

    def hex_id(self, prefix: str, hex_chars: int = 16) -> str:
        chunks: list[str] = []
        while sum(len(c) for c in chunks) < hex_chars:
            chunks.append(f"{self.next_u64():016x}")
        return f"{prefix}_{''.join(chunks)[:hex_chars]}"

    def uuid_str(self) -> str:
        a = self.next_u64()
        b = self.next_u64()
        p1 = f"{(a >> 32) & 0xFFFFFFFF:08x}"
        p2 = f"{(a >> 16) & 0xFFFF:04x}"
        p3 = f"{0x4000 | (a & 0x0FFF):04x}"
        p4 = f"{0x8000 | ((b >> 48) & 0x3FFF):04x}"
        p5 = f"{b & 0xFFFFFFFFFFFF:012x}"
        return f"{p1}-{p2}-{p3}-{p4}-{p5}"

    def synth_text(self, target_bytes: int) -> str:
        """Generates deterministic synthetic ASCII prose of exact UTF-8 byte length."""
        assert target_bytes >= 8
        parts: list[str] = []
        total = 0
        while total < target_bytes:
            word = SYNTH_WORDS[self.next_u64() % len(SYNTH_WORDS)]
            sep = " " if parts else ""
            parts.append(sep + word)
            total += len(sep) + len(word)
        joined = "".join(parts)
        if len(joined) > target_bytes:
            trimmed = joined[: target_bytes - 1] + "."
            return trimmed
        return joined + ("." * (target_bytes - len(joined)))


def dumps_jsonl(records: list[dict[str, Any]]) -> bytes:
    lines = [
        json.dumps(rec, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
        for rec in records
    ]
    return ("\n".join(lines) + "\n").encode("utf-8")


def dumps_pretty_json(obj: dict[str, Any]) -> bytes:
    return (
        json.dumps(obj, sort_keys=True, indent=2, ensure_ascii=True) + "\n"
    ).encode("utf-8")


def default_hints() -> dict[str, bool]:
    return {
        "api_error": False,
        "compaction": False,
        "resume_start": False,
        "sidechain": False,
    }


def compute_session_totals(turns: list[dict[str, Any]]) -> dict[str, int]:
    return {
        "cache_read": sum(t["usage"]["cache_read"] for t in turns),
        "cache_write": sum(t["usage"]["cache_write"] for t in turns),
        "fresh_in": sum(t["usage"]["fresh_in"] for t in turns),
        "output": sum(t["usage"]["output"] for t in turns),
        "prompt_tokens": sum(t["prompt_tokens"] for t in turns),
        "reasoning": sum(t["usage"]["reasoning"] or 0 for t in turns),
        "turns": len(turns),
    }


def gen_claude_basic(rng: SplitMix64, seed: int) -> dict[str, bytes]:
    session_id = rng.uuid_str()
    cwd = "/synthetic/workspace/alpha"
    log_rel = "projects/-synthetic-workspace-alpha/session-basic-0001.jsonl"
    model = "claude-sonnet-4-5"
    version = "2.1.314"

    records: list[dict[str, Any]] = []
    turns_gt: list[dict[str, Any]] = []

    # Turn 1: user plain string prompt -> assistant text + Read tool_use
    u1_uuid = rng.uuid_str()
    u1_text = rng.synth_text(96)
    records.append(
        {
            "cwd": cwd,
            "gitBranch": "task/synth-alpha",
            "isMeta": False,
            "isSidechain": False,
            "message": {"content": u1_text, "role": "user"},
            "parentUuid": None,
            "sessionId": session_id,
            "timestamp": "2026-10-31T09:00:01Z",
            "type": "user",
            "userType": "external",
            "uuid": u1_uuid,
            "version": version,
        }
    )

    a1_uuid = rng.uuid_str()
    a1_msg_id = rng.hex_id("msg", 16)
    a1_req_id = rng.hex_id("req", 16)
    a1_text = rng.synth_text(112)
    tool1_id = rng.hex_id("toolu", 16)
    tool1_input = {"file_path": "/synthetic/workspace/alpha/src/lib.rs"}
    tool1_input_bytes = len(
        json.dumps(tool1_input, sort_keys=True, separators=(",", ":")).encode("utf-8")
    )

    t1_fresh = 320 + rng.rand_range(0, 40)
    t1_cw_5m = 9400
    t1_cw_1h = 0
    t1_cw = t1_cw_5m + t1_cw_1h
    t1_cr = 0
    t1_out = 84

    records.append(
        {
            "cwd": cwd,
            "gitBranch": "task/synth-alpha",
            "isSidechain": False,
            "message": {
                "content": [
                    {"text": a1_text, "type": "text"},
                    {
                        "id": tool1_id,
                        "input": tool1_input,
                        "name": "Read",
                        "type": "tool_use",
                    },
                ],
                "id": a1_msg_id,
                "model": model,
                "role": "assistant",
                "stop_reason": "tool_use",
                "stop_sequence": None,
                "type": "message",
                "usage": {
                    "cache_creation": {
                        "ephemeral_1h_input_tokens": t1_cw_1h,
                        "ephemeral_5m_input_tokens": t1_cw_5m,
                    },
                    "cache_creation_input_tokens": t1_cw,
                    "cache_read_input_tokens": t1_cr,
                    "input_tokens": t1_fresh,
                    "output_tokens": t1_out,
                    "service_tier": "standard",
                },
            },
            "parentUuid": u1_uuid,
            "requestId": a1_req_id,
            "sessionId": session_id,
            "timestamp": "2026-10-31T09:00:04Z",
            "type": "assistant",
            "userType": "external",
            "uuid": a1_uuid,
            "version": version,
        }
    )

    turns_gt.append(
        {
            "added_blocks": [
                {"bytes": len(u1_text.encode("utf-8")), "kind": "user_prompt", "tool": None},
                {"bytes": len(a1_text.encode("utf-8")), "kind": "assistant_text", "tool": None},
                {"bytes": tool1_input_bytes, "kind": "tool_use", "tool": "Read"},
            ],
            "hints": default_hints(),
            "model": model,
            "prompt_tokens": t1_fresh + t1_cw + t1_cr,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T09:00:04Z",
            "turn": 1,
            "usage": {
                "cache_read": t1_cr,
                "cache_write": t1_cw,
                "confidence": "measured",
                "fresh_in": t1_fresh,
                "output": t1_out,
                "reasoning": None,
            },
        }
    )

    # Non-turn record #1: file-history-snapshot (must be counted and skipped)
    records.append(
        {
            "isSnapshotUpdate": False,
            "messageId": u1_uuid,
            "snapshot": {
                "messageId": u1_uuid,
                "timestamp": "2026-10-31T09:00:04Z",
                "trackedFileBackups": {},
            },
            "type": "file-history-snapshot",
        }
    )

    # Turn 2: user tool_result -> assistant Bash tool_use
    u2_uuid = rng.uuid_str()
    tr1_text = rng.synth_text(380)
    records.append(
        {
            "cwd": cwd,
            "gitBranch": "task/synth-alpha",
            "isMeta": False,
            "isSidechain": False,
            "message": {
                "content": [
                    {
                        "content": tr1_text,
                        "is_error": False,
                        "tool_use_id": tool1_id,
                        "type": "tool_result",
                    }
                ],
                "role": "user",
            },
            "parentUuid": a1_uuid,
            "sessionId": session_id,
            "sourceToolUseID": tool1_id,
            "timestamp": "2026-10-31T09:00:05Z",
            "type": "user",
            "userType": "external",
            "uuid": u2_uuid,
            "version": version,
        }
    )

    a2_uuid = rng.uuid_str()
    a2_msg_id = rng.hex_id("msg", 16)
    a2_req_id = rng.hex_id("req", 16)
    tool2_id = rng.hex_id("toolu", 16)
    tool2_input = {"command": "cargo test --workspace", "description": "Run unit tests"}
    tool2_input_bytes = len(
        json.dumps(tool2_input, sort_keys=True, separators=(",", ":")).encode("utf-8")
    )

    t2_fresh = 195 + rng.rand_range(0, 30)
    t2_cw = 404
    t2_cr = t1_cw + t1_fresh
    t2_out = 62

    records.append(
        {
            "cwd": cwd,
            "gitBranch": "task/synth-alpha",
            "isSidechain": False,
            "message": {
                "content": [
                    {
                        "id": tool2_id,
                        "input": tool2_input,
                        "name": "Bash",
                        "type": "tool_use",
                    }
                ],
                "id": a2_msg_id,
                "model": model,
                "role": "assistant",
                "stop_reason": "tool_use",
                "stop_sequence": None,
                "type": "message",
                "usage": {
                    "cache_creation": {
                        "ephemeral_1h_input_tokens": 0,
                        "ephemeral_5m_input_tokens": t2_cw,
                    },
                    "cache_creation_input_tokens": t2_cw,
                    "cache_read_input_tokens": t2_cr,
                    "input_tokens": t2_fresh,
                    "output_tokens": t2_out,
                    "service_tier": "standard",
                },
            },
            "parentUuid": u2_uuid,
            "requestId": a2_req_id,
            "sessionId": session_id,
            "timestamp": "2026-10-31T09:00:09Z",
            "type": "assistant",
            "userType": "external",
            "uuid": a2_uuid,
            "version": version,
        }
    )

    turns_gt.append(
        {
            "added_blocks": [
                {"bytes": len(tr1_text.encode("utf-8")), "kind": "tool_result", "tool": "Read"},
                {"bytes": tool2_input_bytes, "kind": "tool_use", "tool": "Bash"},
            ],
            "hints": default_hints(),
            "model": model,
            "prompt_tokens": t2_fresh + t2_cw + t2_cr,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T09:00:09Z",
            "turn": 2,
            "usage": {
                "cache_read": t2_cr,
                "cache_write": t2_cw,
                "confidence": "measured",
                "fresh_in": t2_fresh,
                "output": t2_out,
                "reasoning": None,
            },
        }
    )

    # Turn 3: user nested array tool_result -> assistant final answer + thinking details
    u3_uuid = rng.uuid_str()
    tr2_text = rng.synth_text(220)
    records.append(
        {
            "cwd": cwd,
            "gitBranch": "task/synth-alpha",
            "isMeta": False,
            "isSidechain": False,
            "message": {
                "content": [
                    {
                        "content": [{"text": tr2_text, "type": "text"}],
                        "is_error": False,
                        "tool_use_id": tool2_id,
                        "type": "tool_result",
                    }
                ],
                "role": "user",
            },
            "parentUuid": a2_uuid,
            "sessionId": session_id,
            "sourceToolUseID": tool2_id,
            "timestamp": "2026-10-31T09:00:12Z",
            "type": "user",
            "userType": "external",
            "uuid": u3_uuid,
            "version": version,
        }
    )

    a3_uuid = rng.uuid_str()
    a3_msg_id = rng.hex_id("msg", 16)
    a3_req_id = rng.hex_id("req", 16)
    a3_text = rng.synth_text(180)
    t3_fresh = 140 + rng.rand_range(0, 25)
    t3_cw = 260
    t3_cr = t2_cr + t2_cw
    t3_out = 136
    t3_think = 40

    records.append(
        {
            "cwd": cwd,
            "gitBranch": "task/synth-alpha",
            "isSidechain": False,
            "message": {
                "content": [{"text": a3_text, "type": "text"}],
                "id": a3_msg_id,
                "model": model,
                "role": "assistant",
                "stop_reason": "end_turn",
                "stop_sequence": None,
                "type": "message",
                "usage": {
                    "cache_creation": {
                        "ephemeral_1h_input_tokens": 0,
                        "ephemeral_5m_input_tokens": t3_cw,
                    },
                    "cache_creation_input_tokens": t3_cw,
                    "cache_read_input_tokens": t3_cr,
                    "input_tokens": t3_fresh,
                    "output_tokens": t3_out,
                    "output_tokens_details": {"thinking_tokens": t3_think},
                    "service_tier": "standard",
                },
            },
            "parentUuid": u3_uuid,
            "requestId": a3_req_id,
            "sessionId": session_id,
            "timestamp": "2026-10-31T09:00:16Z",
            "type": "assistant",
            "userType": "external",
            "uuid": a3_uuid,
            "version": version,
        }
    )

    # Non-turn record #2: system turn_duration (skipped as turn)
    records.append(
        {
            "cwd": cwd,
            "durationMs": 15200,
            "isMeta": False,
            "isSidechain": False,
            "parentUuid": a3_uuid,
            "sessionId": session_id,
            "subtype": "turn_duration",
            "timestamp": "2026-10-31T09:00:16Z",
            "type": "system",
            "uuid": rng.uuid_str(),
            "version": version,
        }
    )

    turns_gt.append(
        {
            "added_blocks": [
                {"bytes": len(tr2_text.encode("utf-8")), "kind": "tool_result", "tool": "Bash"},
                {"bytes": len(a3_text.encode("utf-8")), "kind": "assistant_text", "tool": None},
            ],
            "hints": default_hints(),
            "model": model,
            "prompt_tokens": t3_fresh + t3_cw + t3_cr,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T09:00:16Z",
            "turn": 3,
            "usage": {
                "cache_read": t3_cr,
                "cache_write": t3_cw,
                "confidence": "measured",
                "fresh_in": t3_fresh,
                "output": t3_out,
                "reasoning": t3_think,
            },
        }
    )

    expected = {
        "agent": "claude-code",
        "log_files": [log_rel],
        "scenario": "basic",
        "schema": "kakeibo.fixture-ground-truth/1",
        "seed": seed,
        "sessions": [
            {
                "ignored_records": 2,
                "model": model,
                "session_id": session_id,
                "totals": compute_session_totals(turns_gt),
                "turns": turns_gt,
            }
        ],
    }

    return {
        f"claude-code/basic/{log_rel}": dumps_jsonl(records),
        "claude-code/basic/expected.json": dumps_pretty_json(expected),
    }


def gen_claude_streaming_dupes(rng: SplitMix64, seed: int) -> dict[str, bytes]:
    """Generates multi-line streaming `assistant` chunks sharing `(message.id, requestId)`.

    Exercises the `ccusage#938` trap: early streaming lines carry partial
    `output_tokens` (`5`, `42`) while the final line carries `95` (`max = 95`).
    """
    session_id = rng.uuid_str()
    cwd = "/synthetic/workspace/alpha"
    log_rel = "projects/-synthetic-workspace-alpha/session-dupes-0002.jsonl"
    model = "claude-sonnet-4-5"
    version = "2.1.314"

    records: list[dict[str, Any]] = []
    turns_gt: list[dict[str, Any]] = []

    # Turn 1: user prompt -> 3 streaming assistant lines sharing (msg1_id, req1_id)
    u1_uuid = rng.uuid_str()
    u1_text = rng.synth_text(128)
    records.append(
        {
            "cwd": cwd,
            "gitBranch": "task/synth-dupes",
            "isMeta": False,
            "isSidechain": False,
            "message": {"content": [{"text": u1_text, "type": "text"}], "role": "user"},
            "parentUuid": None,
            "sessionId": session_id,
            "timestamp": "2026-10-31T09:15:00Z",
            "type": "user",
            "userType": "external",
            "uuid": u1_uuid,
            "version": version,
        }
    )

    msg1_id = rng.hex_id("msg", 16)
    req1_id = rng.hex_id("req", 16)
    think1_text = rng.synth_text(96)
    text1_body = rng.synth_text(140)
    tool1_id = rng.hex_id("toolu", 16)
    tool1_input = {"file_path": "/synthetic/workspace/alpha/Cargo.toml"}
    tool1_input_bytes = len(
        json.dumps(tool1_input, sort_keys=True, separators=(",", ":")).encode("utf-8")
    )

    t1_fresh = 410 + rng.rand_range(0, 30)
    t1_cw = 11200
    t1_cr = 0
    # Streaming chunks report partial output_tokens 5 -> 42 -> 95; deduped max is 95.
    chunk_specs = [
        (
            "2026-10-31T09:15:02Z",
            None,
            5,
            [
                {
                    "signature": rng.hex_id("sig", 24),
                    "thinking": think1_text,
                    "type": "thinking",
                }
            ],
        ),
        (
            "2026-10-31T09:15:03Z",
            None,
            42,
            [{"text": text1_body, "type": "text"}],
        ),
        (
            "2026-10-31T09:15:04Z",
            "tool_use",
            95,
            [
                {
                    "id": tool1_id,
                    "input": tool1_input,
                    "name": "Read",
                    "type": "tool_use",
                }
            ],
        ),
    ]

    prev_uuid = u1_uuid
    for ts, stop_reason, partial_out, content_blocks in chunk_specs:
        cur_uuid = rng.uuid_str()
        records.append(
            {
                "cwd": cwd,
                "gitBranch": "task/synth-dupes",
                "isSidechain": False,
                "message": {
                    "content": content_blocks,
                    "id": msg1_id,
                    "model": model,
                    "role": "assistant",
                    "stop_reason": stop_reason,
                    "stop_sequence": None,
                    "type": "message",
                    "usage": {
                        "cache_creation": {
                            "ephemeral_1h_input_tokens": 0,
                            "ephemeral_5m_input_tokens": t1_cw,
                        },
                        "cache_creation_input_tokens": t1_cw,
                        "cache_read_input_tokens": t1_cr,
                        "input_tokens": t1_fresh,
                        "output_tokens": partial_out,
                        "service_tier": "standard",
                    },
                },
                "parentUuid": prev_uuid,
                "requestId": req1_id,
                "sessionId": session_id,
                "timestamp": ts,
                "type": "assistant",
                "userType": "external",
                "uuid": cur_uuid,
                "version": version,
            }
        )
        prev_uuid = cur_uuid

    turns_gt.append(
        {
            "added_blocks": [
                {"bytes": len(u1_text.encode("utf-8")), "kind": "user_prompt", "tool": None},
                {
                    "bytes": len(think1_text.encode("utf-8")),
                    "kind": "assistant_thinking",
                    "tool": None,
                },
                {
                    "bytes": len(text1_body.encode("utf-8")),
                    "kind": "assistant_text",
                    "tool": None,
                },
                {"bytes": tool1_input_bytes, "kind": "tool_use", "tool": "Read"},
            ],
            "hints": default_hints(),
            "model": model,
            "prompt_tokens": t1_fresh + t1_cw + t1_cr,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T09:15:04Z",
            "turn": 1,
            "usage": {
                "cache_read": t1_cr,
                "cache_write": t1_cw,
                "confidence": "measured",
                "fresh_in": t1_fresh,
                "output": 95,
                "reasoning": None,
            },
        }
    )

    # Turn 2: user tool_result -> 2 duplicate streaming assistant lines with final output 110
    u2_uuid = rng.uuid_str()
    tr1_text = rng.synth_text(256)
    records.append(
        {
            "cwd": cwd,
            "gitBranch": "task/synth-dupes",
            "isMeta": False,
            "isSidechain": False,
            "message": {
                "content": [
                    {
                        "content": tr1_text,
                        "is_error": False,
                        "tool_use_id": tool1_id,
                        "type": "tool_result",
                    }
                ],
                "role": "user",
            },
            "parentUuid": prev_uuid,
            "sessionId": session_id,
            "sourceToolUseID": tool1_id,
            "timestamp": "2026-10-31T09:15:05Z",
            "type": "user",
            "userType": "external",
            "uuid": u2_uuid,
            "version": version,
        }
    )

    msg2_id = rng.hex_id("msg", 16)
    req2_id = rng.hex_id("req", 16)
    think2_text = rng.synth_text(72)
    text2_body = rng.synth_text(192)
    t2_fresh = 180 + rng.rand_range(0, 20)
    t2_cw = 310
    t2_cr = t1_cw + t1_fresh

    prev_uuid = u2_uuid
    for ts, stop_reason, partial_out, content_blocks in [
        (
            "2026-10-31T09:15:07Z",
            None,
            24,
            [
                {
                    "signature": rng.hex_id("sig", 24),
                    "thinking": think2_text,
                    "type": "thinking",
                }
            ],
        ),
        (
            "2026-10-31T09:15:08Z",
            "end_turn",
            110,
            [{"text": text2_body, "type": "text"}],
        ),
    ]:
        cur_uuid = rng.uuid_str()
        records.append(
            {
                "cwd": cwd,
                "gitBranch": "task/synth-dupes",
                "isSidechain": False,
                "message": {
                    "content": content_blocks,
                    "id": msg2_id,
                    "model": model,
                    "role": "assistant",
                    "stop_reason": stop_reason,
                    "stop_sequence": None,
                    "type": "message",
                    "usage": {
                        "cache_creation": {
                            "ephemeral_1h_input_tokens": 0,
                            "ephemeral_5m_input_tokens": t2_cw,
                        },
                        "cache_creation_input_tokens": t2_cw,
                        "cache_read_input_tokens": t2_cr,
                        "input_tokens": t2_fresh,
                        "output_tokens": partial_out,
                        "service_tier": "standard",
                    },
                },
                "parentUuid": prev_uuid,
                "requestId": req2_id,
                "sessionId": session_id,
                "timestamp": ts,
                "type": "assistant",
                "userType": "external",
                "uuid": cur_uuid,
                "version": version,
            }
        )
        prev_uuid = cur_uuid

    turns_gt.append(
        {
            "added_blocks": [
                {"bytes": len(tr1_text.encode("utf-8")), "kind": "tool_result", "tool": "Read"},
                {
                    "bytes": len(think2_text.encode("utf-8")),
                    "kind": "assistant_thinking",
                    "tool": None,
                },
                {
                    "bytes": len(text2_body.encode("utf-8")),
                    "kind": "assistant_text",
                    "tool": None,
                },
            ],
            "hints": default_hints(),
            "model": model,
            "prompt_tokens": t2_fresh + t2_cw + t2_cr,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T09:15:08Z",
            "turn": 2,
            "usage": {
                "cache_read": t2_cr,
                "cache_write": t2_cw,
                "confidence": "measured",
                "fresh_in": t2_fresh,
                "output": 110,
                "reasoning": None,
            },
        }
    )

    expected = {
        "agent": "claude-code",
        "log_files": [log_rel],
        "scenario": "streaming-dupes",
        "schema": "kakeibo.fixture-ground-truth/1",
        "seed": seed,
        "sessions": [
            {
                "ignored_records": 0,
                "model": model,
                "session_id": session_id,
                "totals": compute_session_totals(turns_gt),
                "turns": turns_gt,
            }
        ],
    }

    return {
        f"claude-code/streaming-dupes/{log_rel}": dumps_jsonl(records),
        "claude-code/streaming-dupes/expected.json": dumps_pretty_json(expected),
    }


def make_codex_usage_dict(
    fresh_in: int, cache_read: int, cache_write: int, output: int, reasoning: int
) -> dict[str, int]:
    """Builds an OpenAI/Codex usage object where `input_tokens` INCLUDES cached tokens."""
    inclusive_input = fresh_in + cache_read + cache_write
    return {
        "cache_write_input_tokens": cache_write,
        "cached_input_tokens": cache_read,
        "input_tokens": inclusive_input,
        "output_tokens": output,
        "reasoning_output_tokens": reasoning,
        "total_tokens": inclusive_input + output,
    }


def sum_codex_usage(usages: list[dict[str, int]]) -> dict[str, int]:
    keys = (
        "cache_write_input_tokens",
        "cached_input_tokens",
        "input_tokens",
        "output_tokens",
        "reasoning_output_tokens",
        "total_tokens",
    )
    return {k: sum(u[k] for u in usages) for k in keys}


def gen_codex_basic(rng: SplitMix64, seed: int) -> dict[str, bytes]:
    """Generates a Codex CLI rollout using `event_msg/token_count` (without `token_usage_record`)."""
    session_id = rng.uuid_str()
    cwd = "/synthetic/workspace/beta"
    log_rel = "sessions/2026/10/31/rollout-2026-10-31T09-30-00-synth-codex-0001.jsonl"
    model = "gpt-5-codex"

    records: list[dict[str, Any]] = []
    turns_gt: list[dict[str, Any]] = []
    cumulative_usages: list[dict[str, int]] = []
    ordinal = 0

    def push(ts: str, rec_type: str, payload: dict[str, Any]) -> None:
        nonlocal ordinal
        records.append(
            {
                "ordinal": ordinal,
                "payload": payload,
                "timestamp": ts,
                "type": rec_type,
            }
        )
        ordinal += 1

    push(
        "2026-10-31T09:30:00Z",
        "session_meta",
        {
            "cli_version": "0.165.0",
            "cwd": cwd,
            "id": session_id,
            "model_provider": "openai",
            "originator": "codex_cli_rs",
            "parent_thread_id": None,
            "session_id": session_id,
            "source": "cli",
            "timestamp": "2026-10-31T09:30:00Z",
        },
    )

    turn_id = rng.uuid_str()
    push(
        "2026-10-31T09:30:01Z",
        "turn_context",
        {
            "approval_policy": "on-request",
            "cwd": cwd,
            "effort": "high",
            "model": model,
            "root_turn_id": turn_id,
            "summary": "auto",
            "turn_id": turn_id,
        },
    )

    # Turn 1: user prompt -> reasoning + shell function_call -> token_count
    u1_text = rng.synth_text(104)
    push(
        "2026-10-31T09:30:01Z",
        "response_item",
        {
            "content": [{"text": u1_text, "type": "input_text"}],
            "id": rng.hex_id("msg", 16),
            "role": "user",
            "status": "completed",
            "type": "message",
        },
    )
    push(
        "2026-10-31T09:30:03Z",
        "response_item",
        {
            "encrypted_content": rng.hex_id("enc", 32),
            "id": rng.hex_id("rs", 16),
            "summary": [],
            "type": "reasoning",
        },
    )

    call1_id = rng.hex_id("call", 16)
    call1_args = json.dumps(
        {"cmd": ["cargo", "check", "--workspace"]},
        sort_keys=True,
        separators=(",", ":"),
    )
    push(
        "2026-10-31T09:30:04Z",
        "response_item",
        {
            "arguments": call1_args,
            "call_id": call1_id,
            "id": rng.hex_id("fc", 16),
            "name": "exec_command",
            "status": "completed",
            "type": "function_call",
        },
    )

    u_t1 = make_codex_usage_dict(
        fresh_in=2600 + rng.rand_range(0, 50),
        cache_read=6400,
        cache_write=0,
        output=140,
        reasoning=64,
    )
    cumulative_usages.append(u_t1)
    push(
        "2026-10-31T09:30:04Z",
        "event_msg",
        {
            "info": {
                "last_token_usage": u_t1,
                "model_context_window": 272000,
                "total_token_usage": sum_codex_usage(cumulative_usages),
            },
            "rate_limits": {
                "limit_id": "codex",
                "plan_type": "plus",
                "primary": {
                    "resets_at": 1793440800,
                    "used_percent": 6.5,
                    "window_minutes": 300,
                },
                "secondary": None,
            },
            "type": "token_count",
        },
    )

    turns_gt.append(
        {
            "added_blocks": [
                {"bytes": len(u1_text.encode("utf-8")), "kind": "user_prompt", "tool": None},
                {
                    "bytes": len(call1_args.encode("utf-8")),
                    "kind": "tool_use",
                    "tool": "exec_command",
                },
            ],
            "hints": default_hints(),
            "model": model,
            "prompt_tokens": u_t1["input_tokens"],
            "rate_limit_used_percent": 6.5,
            "ts": "2026-10-31T09:30:04Z",
            "turn": 1,
            "usage": {
                "cache_read": u_t1["cached_input_tokens"],
                "cache_write": u_t1["cache_write_input_tokens"],
                "confidence": "measured",
                "fresh_in": u_t1["input_tokens"]
                - u_t1["cached_input_tokens"]
                - u_t1["cache_write_input_tokens"],
                "output": u_t1["output_tokens"],
                "reasoning": u_t1["reasoning_output_tokens"],
            },
        }
    )

    # Turn 2: function_call_output -> assistant message -> token_count
    out1_text = rng.synth_text(310)
    push(
        "2026-10-31T09:30:05Z",
        "response_item",
        {
            "call_id": call1_id,
            "id": rng.hex_id("fco", 16),
            "output": out1_text,
            "type": "function_call_output",
        },
    )

    a2_text = rng.synth_text(164)
    push(
        "2026-10-31T09:30:08Z",
        "response_item",
        {
            "content": [{"text": a2_text, "type": "output_text"}],
            "id": rng.hex_id("msg", 16),
            "role": "assistant",
            "status": "completed",
            "type": "message",
        },
    )

    u_t2 = make_codex_usage_dict(
        fresh_in=480 + rng.rand_range(0, 30),
        cache_read=u_t1["input_tokens"],
        cache_write=0,
        output=175,
        reasoning=80,
    )
    cumulative_usages.append(u_t2)
    push(
        "2026-10-31T09:30:08Z",
        "event_msg",
        {
            "info": {
                "last_token_usage": u_t2,
                "model_context_window": 272000,
                "total_token_usage": sum_codex_usage(cumulative_usages),
            },
            "rate_limits": {
                "limit_id": "codex",
                "plan_type": "plus",
                "primary": {
                    "resets_at": 1793440800,
                    "used_percent": 7.25,
                    "window_minutes": 300,
                },
                "secondary": None,
            },
            "type": "token_count",
        },
    )

    turns_gt.append(
        {
            "added_blocks": [
                {
                    "bytes": len(out1_text.encode("utf-8")),
                    "kind": "tool_result",
                    "tool": "exec_command",
                },
                {"bytes": len(a2_text.encode("utf-8")), "kind": "assistant_text", "tool": None},
            ],
            "hints": default_hints(),
            "model": model,
            "prompt_tokens": u_t2["input_tokens"],
            "rate_limit_used_percent": 7.25,
            "ts": "2026-10-31T09:30:08Z",
            "turn": 2,
            "usage": {
                "cache_read": u_t2["cached_input_tokens"],
                "cache_write": u_t2["cache_write_input_tokens"],
                "confidence": "measured",
                "fresh_in": u_t2["input_tokens"]
                - u_t2["cached_input_tokens"]
                - u_t2["cache_write_input_tokens"],
                "output": u_t2["output_tokens"],
                "reasoning": u_t2["reasoning_output_tokens"],
            },
        }
    )

    expected = {
        "agent": "codex",
        "log_files": [log_rel],
        "scenario": "basic",
        "schema": "kakeibo.fixture-ground-truth/1",
        "seed": seed,
        "sessions": [
            {
                "ignored_records": 0,
                "model": model,
                "session_id": session_id,
                "totals": compute_session_totals(turns_gt),
                "turns": turns_gt,
            }
        ],
    }

    return {
        f"codex/basic/{log_rel}": dumps_jsonl(records),
        "codex/basic/expected.json": dumps_pretty_json(expected),
    }


def gen_codex_dual_usage(rng: SplitMix64, seed: int) -> dict[str, bytes]:
    """Generates a `codex-cli 0.165.0` rollout emitting BOTH `token_usage_record`
    and `event_msg/token_count` on every turn, plus a non-turn `world_state` record.

    Ground truth verifies turns are NOT double-counted and cached input is NOT
    added on top of `input_tokens`.
    """
    session_id = rng.uuid_str()
    cwd = "/synthetic/workspace/beta"
    log_rel = "sessions/2026/10/31/rollout-2026-10-31T10-00-00-synth-codex-0002.jsonl"
    model = "gpt-5-codex"

    records: list[dict[str, Any]] = []
    turns_gt: list[dict[str, Any]] = []
    cumulative_usages: list[dict[str, int]] = []
    ordinal = 0

    def push(ts: str, rec_type: str, payload: dict[str, Any]) -> None:
        nonlocal ordinal
        records.append(
            {
                "ordinal": ordinal,
                "payload": payload,
                "timestamp": ts,
                "type": rec_type,
            }
        )
        ordinal += 1

    push(
        "2026-10-31T10:00:00Z",
        "session_meta",
        {
            "cli_version": "0.165.0",
            "cwd": cwd,
            "id": session_id,
            "model_provider": "openai",
            "originator": "codex_cli_rs",
            "parent_thread_id": None,
            "session_id": session_id,
            "source": "cli",
            "timestamp": "2026-10-31T10:00:00Z",
        },
    )

    # Non-turn record observed in 0.165.0 (`world_state` -> skipped cleanly, ignored_records = 1)
    push(
        "2026-10-31T10:00:00Z",
        "world_state",
        {"full": True, "state": {"synthetic_mode": "default"}},
    )

    turn_id = rng.uuid_str()
    push(
        "2026-10-31T10:00:01Z",
        "turn_context",
        {
            "approval_policy": "never",
            "cwd": cwd,
            "effort": "high",
            "model": model,
            "root_turn_id": turn_id,
            "summary": "auto",
            "turn_id": turn_id,
        },
    )

    # Turn 1: user message -> custom_tool_call (`apply_patch`) -> BOTH token_usage_record & event_msg/token_count
    u1_text = rng.synth_text(136)
    push(
        "2026-10-31T10:00:01Z",
        "response_item",
        {
            "content": [{"text": u1_text, "type": "input_text"}],
            "id": rng.hex_id("msg", 16),
            "role": "user",
            "status": "completed",
            "type": "message",
        },
    )

    call1_id = rng.hex_id("call", 16)
    patch_input = rng.synth_text(210)
    push(
        "2026-10-31T10:00:03Z",
        "response_item",
        {
            "call_id": call1_id,
            "id": rng.hex_id("ctc", 16),
            "input": patch_input,
            "name": "apply_patch",
            "status": "completed",
            "type": "custom_tool_call",
        },
    )

    u_t1 = make_codex_usage_dict(
        fresh_in=3100 + rng.rand_range(0, 60),
        cache_read=8960,
        cache_write=0,
        output=190,
        reasoning=92,
    )
    cumulative_usages.append(u_t1)
    total_t1 = sum_codex_usage(cumulative_usages)

    push(
        "2026-10-31T10:00:03Z",
        "token_usage_record",
        {
            "response_id": rng.hex_id("resp", 18),
            "root_turn_id": turn_id,
            "session_id": session_id,
            "thread_id": session_id,
            "thread_token_usage": total_t1,
            "turn_id": turn_id,
            "turn_token_usage": total_t1,
            "usage": u_t1,
        },
    )
    push(
        "2026-10-31T10:00:03Z",
        "event_msg",
        {
            "info": {
                "last_token_usage": u_t1,
                "model_context_window": 272000,
                "total_token_usage": total_t1,
            },
            "rate_limits": {
                "limit_id": "codex",
                "plan_type": "pro",
                "primary": {
                    "resets_at": 1793444400,
                    "used_percent": 11.0,
                    "window_minutes": 300,
                },
                "secondary": None,
            },
            "type": "token_count",
        },
    )

    turns_gt.append(
        {
            "added_blocks": [
                {"bytes": len(u1_text.encode("utf-8")), "kind": "user_prompt", "tool": None},
                {
                    "bytes": len(patch_input.encode("utf-8")),
                    "kind": "tool_use",
                    "tool": "apply_patch",
                },
            ],
            "hints": default_hints(),
            "model": model,
            "prompt_tokens": u_t1["input_tokens"],
            "rate_limit_used_percent": 11.0,
            "ts": "2026-10-31T10:00:03Z",
            "turn": 1,
            "usage": {
                "cache_read": u_t1["cached_input_tokens"],
                "cache_write": u_t1["cache_write_input_tokens"],
                "confidence": "measured",
                "fresh_in": u_t1["input_tokens"]
                - u_t1["cached_input_tokens"]
                - u_t1["cache_write_input_tokens"],
                "output": u_t1["output_tokens"],
                "reasoning": u_t1["reasoning_output_tokens"],
            },
        }
    )

    # Turn 2: custom_tool_call_output -> assistant message -> BOTH token_usage_record & event_msg/token_count
    patch_out_text = rng.synth_text(148)
    push(
        "2026-10-31T10:00:04Z",
        "response_item",
        {
            "call_id": call1_id,
            "id": rng.hex_id("ctco", 16),
            "output": [{"text": patch_out_text, "type": "input_text"}],
            "type": "custom_tool_call_output",
        },
    )

    a2_text = rng.synth_text(204)
    push(
        "2026-10-31T10:00:07Z",
        "response_item",
        {
            "content": [{"text": a2_text, "type": "output_text"}],
            "id": rng.hex_id("msg", 16),
            "role": "assistant",
            "status": "completed",
            "type": "message",
        },
    )

    u_t2 = make_codex_usage_dict(
        fresh_in=520 + rng.rand_range(0, 40),
        cache_read=u_t1["input_tokens"],
        cache_write=0,
        output=210,
        reasoning=96,
    )
    cumulative_usages.append(u_t2)
    total_t2 = sum_codex_usage(cumulative_usages)

    push(
        "2026-10-31T10:00:07Z",
        "token_usage_record",
        {
            "response_id": rng.hex_id("resp", 18),
            "root_turn_id": turn_id,
            "session_id": session_id,
            "thread_id": session_id,
            "thread_token_usage": total_t2,
            "turn_id": turn_id,
            "turn_token_usage": total_t2,
            "usage": u_t2,
        },
    )
    push(
        "2026-10-31T10:00:07Z",
        "event_msg",
        {
            "info": {
                "last_token_usage": u_t2,
                "model_context_window": 272000,
                "total_token_usage": total_t2,
            },
            "rate_limits": {
                "limit_id": "codex",
                "plan_type": "pro",
                "primary": {
                    "resets_at": 1793444400,
                    "used_percent": 12.25,
                    "window_minutes": 300,
                },
                "secondary": None,
            },
            "type": "token_count",
        },
    )

    turns_gt.append(
        {
            "added_blocks": [
                {
                    "bytes": len(patch_out_text.encode("utf-8")),
                    "kind": "tool_result",
                    "tool": "apply_patch",
                },
                {"bytes": len(a2_text.encode("utf-8")), "kind": "assistant_text", "tool": None},
            ],
            "hints": default_hints(),
            "model": model,
            "prompt_tokens": u_t2["input_tokens"],
            "rate_limit_used_percent": 12.25,
            "ts": "2026-10-31T10:00:07Z",
            "turn": 2,
            "usage": {
                "cache_read": u_t2["cached_input_tokens"],
                "cache_write": u_t2["cache_write_input_tokens"],
                "confidence": "measured",
                "fresh_in": u_t2["input_tokens"]
                - u_t2["cached_input_tokens"]
                - u_t2["cache_write_input_tokens"],
                "output": u_t2["output_tokens"],
                "reasoning": u_t2["reasoning_output_tokens"],
            },
        }
    )

    expected = {
        "agent": "codex",
        "log_files": [log_rel],
        "scenario": "codex-dual-usage",
        "schema": "kakeibo.fixture-ground-truth/1",
        "seed": seed,
        "sessions": [
            {
                "ignored_records": 1,
                "model": model,
                "session_id": session_id,
                "totals": compute_session_totals(turns_gt),
                "turns": turns_gt,
            }
        ],
    }

    return {
        f"codex/codex-dual-usage/{log_rel}": dumps_jsonl(records),
        "codex/codex-dual-usage/expected.json": dumps_pretty_json(expected),
    }


def gen_claude_tool_bloat(rng: SplitMix64, seed: int) -> dict[str, bytes]:
    """5-turn Claude Code session ingesting a 160 KB (40,000-token) Bash tool_result
    on Turn 2 and carrying it across Turns 3 (cache_read), 4 (cold cache_write miss),
    and 5 (cache_read) -> exact hand-computed carried cost = 120,000 prompt tokens
    (80,000 cache_read + 40,000 cache_write; lifetime = 160,000).
    """
    session_id = rng.uuid_str()
    cwd = "/synthetic/workspace/bloat"
    log_rel = "projects/-synthetic-workspace-bloat/session-tool-bloat-0001.jsonl"
    model = "claude-sonnet-4-5"
    version = "2.1.314"

    records: list[dict[str, Any]] = []
    turns_gt: list[dict[str, Any]] = []

    def push_user_text(
        parent_uuid: str | None, ts: str, byte_len: int
    ) -> tuple[str, str]:
        u_uuid = rng.uuid_str()
        u_text = rng.synth_text(byte_len)
        records.append(
            {
                "cwd": cwd,
                "gitBranch": "task/synth-bloat",
                "isMeta": False,
                "isSidechain": False,
                "message": {"content": u_text, "role": "user"},
                "parentUuid": parent_uuid,
                "sessionId": session_id,
                "timestamp": ts,
                "type": "user",
                "userType": "external",
                "uuid": u_uuid,
                "version": version,
            }
        )
        return u_uuid, u_text

    def push_assistant_text(
        parent_uuid: str,
        ts: str,
        byte_len: int,
        fresh_in: int,
        cache_write: int,
        cache_read: int,
        output: int,
    ) -> tuple[str, str]:
        a_uuid = rng.uuid_str()
        a_msg_id = rng.hex_id("msg", 16)
        a_req_id = rng.hex_id("req", 16)
        a_text = rng.synth_text(byte_len)
        records.append(
            {
                "cwd": cwd,
                "gitBranch": "task/synth-bloat",
                "isSidechain": False,
                "message": {
                    "content": [{"text": a_text, "type": "text"}],
                    "id": a_msg_id,
                    "model": model,
                    "role": "assistant",
                    "stop_reason": "end_turn",
                    "stop_sequence": None,
                    "type": "message",
                    "usage": {
                        "cache_creation": {
                            "ephemeral_1h_input_tokens": 0,
                            "ephemeral_5m_input_tokens": cache_write,
                        },
                        "cache_creation_input_tokens": cache_write,
                        "cache_read_input_tokens": cache_read,
                        "input_tokens": fresh_in,
                        "output_tokens": output,
                        "service_tier": "standard",
                    },
                },
                "parentUuid": parent_uuid,
                "requestId": a_req_id,
                "sessionId": session_id,
                "timestamp": ts,
                "type": "assistant",
                "userType": "external",
                "uuid": a_uuid,
                "version": version,
            }
        )
        return a_uuid, a_text

    # Turn 1: P(1) = 10,020 (FixedOverhead = 10,000 cache_write, u1 = 20 fresh_in).
    # Assistant emits 120B text (30 est_tokens) + 80B Bash tool_use (20 est_tokens).
    u1_uuid, u1_text = push_user_text(None, "2026-10-31T09:10:01Z", 80)
    a1_uuid = rng.uuid_str()
    a1_msg_id = rng.hex_id("msg", 16)
    a1_req_id = rng.hex_id("req", 16)
    a1_text = rng.synth_text(120)
    tool1_id = rng.hex_id("toolu", 16)
    tool1_input = {
        "command": "cat /synthetic/workspace/bloat/build.log",
        "description": "Read logs",
    }
    tool1_input_bytes = len(
        json.dumps(tool1_input, sort_keys=True, separators=(",", ":")).encode("utf-8")
    )
    assert tool1_input_bytes == 80

    t1_fresh, t1_cw, t1_cr, t1_out = 20, 10000, 0, 50
    records.append(
        {
            "cwd": cwd,
            "gitBranch": "task/synth-bloat",
            "isSidechain": False,
            "message": {
                "content": [
                    {"text": a1_text, "type": "text"},
                    {
                        "id": tool1_id,
                        "input": tool1_input,
                        "name": "Bash",
                        "type": "tool_use",
                    },
                ],
                "id": a1_msg_id,
                "model": model,
                "role": "assistant",
                "stop_reason": "tool_use",
                "stop_sequence": None,
                "type": "message",
                "usage": {
                    "cache_creation": {
                        "ephemeral_1h_input_tokens": 0,
                        "ephemeral_5m_input_tokens": t1_cw,
                    },
                    "cache_creation_input_tokens": t1_cw,
                    "cache_read_input_tokens": t1_cr,
                    "input_tokens": t1_fresh,
                    "output_tokens": t1_out,
                    "service_tier": "standard",
                },
            },
            "parentUuid": u1_uuid,
            "requestId": a1_req_id,
            "sessionId": session_id,
            "timestamp": "2026-10-31T09:10:04Z",
            "type": "assistant",
            "userType": "external",
            "uuid": a1_uuid,
            "version": version,
        }
    )
    turns_gt.append(
        {
            "added_blocks": [
                {"bytes": len(u1_text.encode("utf-8")), "kind": "user_prompt", "tool": None},
                {"bytes": len(a1_text.encode("utf-8")), "kind": "assistant_text", "tool": None},
                {"bytes": tool1_input_bytes, "kind": "tool_use", "tool": "Bash"},
            ],
            "hints": default_hints(),
            "model": model,
            "prompt_tokens": t1_fresh + t1_cw + t1_cr,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T09:10:04Z",
            "turn": 1,
            "usage": {
                "cache_read": t1_cr,
                "cache_write": t1_cw,
                "confidence": "measured",
                "fresh_in": t1_fresh,
                "output": t1_out,
                "reasoning": None,
            },
        }
    )

    # Turn 2: 160,000-byte (40,000 est_tokens) Bash tool_result enters prompt alongside
    # Turn 1's assistant_text (30) + tool_use (20) => delta P(2) = 40,050 cache_write.
    u2_uuid = rng.uuid_str()
    tr1_bloat_text = rng.synth_text(160000)
    records.append(
        {
            "cwd": cwd,
            "gitBranch": "task/synth-bloat",
            "isMeta": False,
            "isSidechain": False,
            "message": {
                "content": [
                    {
                        "content": tr1_bloat_text,
                        "is_error": False,
                        "tool_use_id": tool1_id,
                        "type": "tool_result",
                    }
                ],
                "role": "user",
            },
            "parentUuid": a1_uuid,
            "sessionId": session_id,
            "sourceToolUseID": tool1_id,
            "timestamp": "2026-10-31T09:10:06Z",
            "type": "user",
            "userType": "external",
            "uuid": u2_uuid,
            "version": version,
        }
    )
    t2_fresh, t2_cw, t2_cr, t2_out = 0, 40050, 10020, 20
    a2_uuid, a2_text = push_assistant_text(
        u2_uuid, "2026-10-31T09:10:12Z", 80, t2_fresh, t2_cw, t2_cr, t2_out
    )
    turns_gt.append(
        {
            "added_blocks": [
                {
                    "bytes": len(tr1_bloat_text.encode("utf-8")),
                    "kind": "tool_result",
                    "tool": "Bash",
                },
                {"bytes": len(a2_text.encode("utf-8")), "kind": "assistant_text", "tool": None},
            ],
            "hints": default_hints(),
            "model": model,
            "prompt_tokens": t2_fresh + t2_cw + t2_cr,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T09:10:12Z",
            "turn": 2,
            "usage": {
                "cache_read": t2_cr,
                "cache_write": t2_cw,
                "confidence": "measured",
                "fresh_in": t2_fresh,
                "output": t2_out,
                "reasoning": None,
            },
        }
    )

    # Turn 3 (carry #1): warm cache_read = 50,070 (carries 40,000 cache_read for Bash tool_result).
    u3_uuid, u3_text = push_user_text(a2_uuid, "2026-10-31T09:10:16Z", 120)
    t3_fresh, t3_cw, t3_cr, t3_out = 20, 30, 50070, 20
    a3_uuid, a3_text = push_assistant_text(
        u3_uuid, "2026-10-31T09:10:20Z", 80, t3_fresh, t3_cw, t3_cr, t3_out
    )
    turns_gt.append(
        {
            "added_blocks": [
                {"bytes": len(u3_text.encode("utf-8")), "kind": "user_prompt", "tool": None},
                {"bytes": len(a3_text.encode("utf-8")), "kind": "assistant_text", "tool": None},
            ],
            "hints": default_hints(),
            "model": model,
            "prompt_tokens": t3_fresh + t3_cw + t3_cr,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T09:10:20Z",
            "turn": 3,
            "usage": {
                "cache_read": t3_cr,
                "cache_write": t3_cw,
                "confidence": "measured",
                "fresh_in": t3_fresh,
                "output": t3_out,
                "reasoning": None,
            },
        }
    )

    # Turn 4 (carry #2): 12m idle gap expires 5m TTL -> cold cache_write = 50,120
    # (carries 40,000 cache_write for Bash tool_result).
    u4_uuid, u4_text = push_user_text(a3_uuid, "2026-10-31T09:22:25Z", 120)
    t4_fresh, t4_cw, t4_cr, t4_out = 50, 50120, 0, 20
    a4_uuid, a4_text = push_assistant_text(
        u4_uuid, "2026-10-31T09:22:30Z", 80, t4_fresh, t4_cw, t4_cr, t4_out
    )
    turns_gt.append(
        {
            "added_blocks": [
                {"bytes": len(u4_text.encode("utf-8")), "kind": "user_prompt", "tool": None},
                {"bytes": len(a4_text.encode("utf-8")), "kind": "assistant_text", "tool": None},
            ],
            "hints": default_hints(),
            "model": model,
            "prompt_tokens": t4_fresh + t4_cw + t4_cr,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T09:22:30Z",
            "turn": 4,
            "usage": {
                "cache_read": t4_cr,
                "cache_write": t4_cw,
                "confidence": "measured",
                "fresh_in": t4_fresh,
                "output": t4_out,
                "reasoning": None,
            },
        }
    )

    # Turn 5 (carry #3): warm cache_read = 50,170 (carries 40,000 cache_read for Bash tool_result).
    u5_uuid, u5_text = push_user_text(a4_uuid, "2026-10-31T09:22:36Z", 80)
    t5_fresh, t5_cw, t5_cr, t5_out = 40, 0, 50170, 25
    _, a5_text = push_assistant_text(
        u5_uuid, "2026-10-31T09:22:42Z", 100, t5_fresh, t5_cw, t5_cr, t5_out
    )
    turns_gt.append(
        {
            "added_blocks": [
                {"bytes": len(u5_text.encode("utf-8")), "kind": "user_prompt", "tool": None},
                {"bytes": len(a5_text.encode("utf-8")), "kind": "assistant_text", "tool": None},
            ],
            "hints": default_hints(),
            "model": model,
            "prompt_tokens": t5_fresh + t5_cw + t5_cr,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T09:22:42Z",
            "turn": 5,
            "usage": {
                "cache_read": t5_cr,
                "cache_write": t5_cw,
                "confidence": "measured",
                "fresh_in": t5_fresh,
                "output": t5_out,
                "reasoning": None,
            },
        }
    )

    expected = {
        "agent": "claude-code",
        "log_files": [log_rel],
        "scenario": "tool-bloat",
        "schema": "kakeibo.fixture-ground-truth/1",
        "seed": seed,
        "sessions": [
            {
                "ignored_records": 0,
                "model": model,
                "session_id": session_id,
                "totals": compute_session_totals(turns_gt),
                "turns": turns_gt,
            }
        ],
    }

    return {
        f"claude-code/tool-bloat/{log_rel}": dumps_jsonl(records),
        "claude-code/tool-bloat/expected.json": dumps_pretty_json(expected),
    }


def generate_all(seed: int) -> dict[str, bytes]:
    rng = SplitMix64(seed)
    files: dict[str, bytes] = {}
    for builder in (
        gen_claude_basic,
        gen_claude_streaming_dupes,
        gen_codex_basic,
        gen_codex_dual_usage,
        gen_claude_tool_bloat,
    ):
        # Derive an independent deterministic sub-seed per scenario so adding new
        # scenarios in later tasks never shifts existing scenario bytes!
        scenario_rng = SplitMix64(rng.next_u64())
        files.update(builder(scenario_rng, seed))
    return dict(sorted(files.items()))


def collect_existing(out_dir: Path) -> dict[str, bytes]:
    if not out_dir.is_dir():
        return {}
    found: dict[str, bytes] = {}
    for path in sorted(out_dir.rglob("*")):
        if path.is_file():
            rel = path.relative_to(out_dir).as_posix()
            found[rel] = path.read_bytes()
    return found


def main() -> int:
    repo_root = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description="Generate deterministic synthetic Kakeibo fixtures.")
    parser.add_argument("--seed", type=int, default=42, help="Deterministic 64-bit PRNG seed (default: 42)")
    parser.add_argument(
        "--out",
        type=Path,
        default=repo_root / "tests" / "fixtures",
        help="Target fixture directory (default: tests/fixtures)",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="Verify target directory matches generated bytes without modifying disk",
    )
    args = parser.parse_args()

    expected_files = generate_all(args.seed)

    if args.check:
        actual_files = collect_existing(args.out)
        if actual_files != expected_files:
            missing = sorted(set(expected_files) - set(actual_files))
            extra = sorted(set(actual_files) - set(expected_files))
            changed = sorted(
                k for k in (set(expected_files) & set(actual_files)) if expected_files[k] != actual_files[k]
            )
            print(
                f"gen-fixtures --check FAILED (missing={missing}, extra={extra}, changed={changed})",
                file=sys.stderr,
            )
            return 1
        print(f"gen-fixtures: ok ({len(expected_files)} files byte-identical for seed={args.seed})")
        return 0

    for rel, data in expected_files.items():
        dst = args.out / rel
        if dst.is_file() and dst.read_bytes() == data:
            continue
        dst.parent.mkdir(parents=True, exist_ok=True)
        dst.write_bytes(data)

    print(f"gen-fixtures: wrote {len(expected_files)} files to {args.out} (seed={args.seed})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
