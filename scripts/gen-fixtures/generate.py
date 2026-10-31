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
import hashlib
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


def _make_claude_text_helpers(
    rng: SplitMix64,
    records: list[dict[str, Any]],
    *,
    cwd: str,
    git_branch: str,
    session_id: str,
    version: str,
):
    def push_user_text(
        parent_uuid: str | None, ts: str, byte_len: int
    ) -> tuple[str, str]:
        u_uuid = rng.uuid_str()
        u_text = rng.synth_text(byte_len)
        records.append(
            {
                "cwd": cwd,
                "gitBranch": git_branch,
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
        model: str,
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
                "gitBranch": git_branch,
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

    return push_user_text, push_assistant_text


def gen_claude_resume_cold(rng: SplitMix64, seed: int) -> dict[str, bytes]:
    """Generates `claude-code/resume-cold` (`PLAN.md` §6.3, §9, §10 T07).

    4-turn session building ~122K prompt context, pausing 45 minutes (`2,700`s > `300`s TTL)
    across an `away_summary` resume boundary, rewriting `122,110` cold `cache_write` tokens
    on Turn 3 (`hints.resume_start == True`), then warming back up on Turn 4.
    """
    session_id = rng.uuid_str()
    cwd = "/synthetic/workspace/resume"
    log_rel = "projects/-synthetic-workspace-resume/session-resume-cold-0001.jsonl"
    model = "claude-sonnet-4-5"
    version = "2.1.314"

    records: list[dict[str, Any]] = []
    turns_gt: list[dict[str, Any]] = []
    push_user_text, push_assistant_text = _make_claude_text_helpers(
        rng,
        records,
        cwd=cwd,
        git_branch="task/synth-resume",
        session_id=session_id,
        version=version,
    )

    # Turn 1: P(1) = 12,030 (FixedOverhead = 12,000, u1 = 30).
    u1_uuid, u1_text = push_user_text(None, "2026-10-31T10:10:01Z", 120)
    t1_fresh, t1_cw, t1_cr, t1_out = 30, 12000, 0, 40
    a1_uuid, a1_text = push_assistant_text(
        u1_uuid, "2026-10-31T10:10:04Z", model, 160, t1_fresh, t1_cw, t1_cr, t1_out
    )
    turns_gt.append(
        {
            "added_blocks": [
                {"bytes": len(u1_text.encode("utf-8")), "kind": "user_prompt", "tool": None},
                {"bytes": len(a1_text.encode("utf-8")), "kind": "assistant_text", "tool": None},
            ],
            "hints": default_hints(),
            "model": model,
            "prompt_tokens": t1_fresh + t1_cw + t1_cr,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T10:10:04Z",
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

    # Turn 2: large 440,000B (110,000 est_tokens) user prompt builds P(2) = 122,070.
    u2_uuid, u2_text = push_user_text(a1_uuid, "2026-10-31T10:10:09Z", 440000)
    t2_fresh, t2_cw, t2_cr, t2_out = 40, 110000, 12030, 50
    a2_uuid, a2_text = push_assistant_text(
        u2_uuid, "2026-10-31T10:10:14Z", model, 200, t2_fresh, t2_cw, t2_cr, t2_out
    )
    turns_gt.append(
        {
            "added_blocks": [
                {"bytes": len(u2_text.encode("utf-8")), "kind": "user_prompt", "tool": None},
                {"bytes": len(a2_text.encode("utf-8")), "kind": "assistant_text", "tool": None},
            ],
            "hints": default_hints(),
            "model": model,
            "prompt_tokens": t2_fresh + t2_cw + t2_cr,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T10:10:14Z",
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

    # Non-turn `away_summary` system boundary before resumed Turn 3 (45m idle gap = 2,700s).
    sys_uuid = rng.uuid_str()
    records.append(
        {
            "cwd": cwd,
            "isMeta": False,
            "isSidechain": False,
            "parentUuid": a2_uuid,
            "sessionId": session_id,
            "subtype": "away_summary",
            "timestamp": "2026-10-31T10:55:10Z",
            "type": "system",
            "uuid": sys_uuid,
            "version": version,
        }
    )

    # Turn 3: cold resume after 2,700s gap -> rewrites 122,110 tokens into cache (P(3) = 122,150).
    u3_uuid, u3_text = push_user_text(sys_uuid, "2026-10-31T10:55:11Z", 120)
    t3_fresh, t3_cw, t3_cr, t3_out = 40, 122110, 0, 45
    a3_uuid, a3_text = push_assistant_text(
        u3_uuid, "2026-10-31T10:55:14Z", model, 180, t3_fresh, t3_cw, t3_cr, t3_out
    )
    t3_hints = default_hints()
    t3_hints["resume_start"] = True
    turns_gt.append(
        {
            "added_blocks": [
                {"bytes": len(u3_text.encode("utf-8")), "kind": "user_prompt", "tool": None},
                {"bytes": len(a3_text.encode("utf-8")), "kind": "assistant_text", "tool": None},
            ],
            "hints": t3_hints,
            "model": model,
            "prompt_tokens": t3_fresh + t3_cw + t3_cr,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T10:55:14Z",
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

    # Turn 4: 10s later, warm cache_read = 122,150.
    u4_uuid, u4_text = push_user_text(a3_uuid, "2026-10-31T10:55:20Z", 100)
    t4_fresh, t4_cw, t4_cr, t4_out = 70, 0, 122150, 40
    _, a4_text = push_assistant_text(
        u4_uuid, "2026-10-31T10:55:24Z", model, 160, t4_fresh, t4_cw, t4_cr, t4_out
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
            "ts": "2026-10-31T10:55:24Z",
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

    expected = {
        "agent": "claude-code",
        "log_files": [log_rel],
        "scenario": "resume-cold",
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
        f"claude-code/resume-cold/{log_rel}": dumps_jsonl(records),
        "claude-code/resume-cold/expected.json": dumps_pretty_json(expected),
    }


def gen_claude_near_full_context(rng: SplitMix64, seed: int) -> dict[str, bytes]:
    """Generates `claude-code/near-full-context` (`PLAN.md` §6.3, §10 T07).

    5-turn `claude-sonnet-4-5` (`200,000` window) session sustaining `86%..95%` of the
    context window across 4 consecutive requests on Turns `2..=5`.
    """
    session_id = rng.uuid_str()
    cwd = "/synthetic/workspace/nearfull"
    log_rel = "projects/-synthetic-workspace-nearfull/session-near-full-0001.jsonl"
    model = "claude-sonnet-4-5"
    version = "2.1.314"

    records: list[dict[str, Any]] = []
    turns_gt: list[dict[str, Any]] = []
    push_user_text, push_assistant_text = _make_claude_text_helpers(
        rng,
        records,
        cwd=cwd,
        git_branch="task/synth-nearfull",
        session_id=session_id,
        version=version,
    )

    turn_specs = [
        # (u_ts, u_bytes, a_ts, a_bytes, fresh_in, cache_write, cache_read, output)
        ("2026-10-31T11:00:01Z", 120, "2026-10-31T11:00:04Z", 160, 30, 14000, 0, 40),
        ("2026-10-31T11:00:10Z", 632000, "2026-10-31T11:00:15Z", 200, 40, 158000, 14030, 50),
        ("2026-10-31T11:00:20Z", 24000, "2026-10-31T11:00:25Z", 180, 50, 6000, 172070, 45),
        ("2026-10-31T11:00:30Z", 24000, "2026-10-31T11:00:35Z", 160, 45, 6000, 178120, 40),
        ("2026-10-31T11:00:40Z", 24000, "2026-10-31T11:00:45Z", 220, 40, 6000, 184165, 55),
    ]

    prev_uuid: str | None = None
    for idx, (u_ts, u_bytes, a_ts, a_bytes, fresh, cw, cr, out) in enumerate(
        turn_specs, start=1
    ):
        u_uuid, u_text = push_user_text(prev_uuid, u_ts, u_bytes)
        a_uuid, a_text = push_assistant_text(
            u_uuid, a_ts, model, a_bytes, fresh, cw, cr, out
        )
        prev_uuid = a_uuid
        turns_gt.append(
            {
                "added_blocks": [
                    {"bytes": len(u_text.encode("utf-8")), "kind": "user_prompt", "tool": None},
                    {"bytes": len(a_text.encode("utf-8")), "kind": "assistant_text", "tool": None},
                ],
                "hints": default_hints(),
                "model": model,
                "prompt_tokens": fresh + cw + cr,
                "rate_limit_used_percent": None,
                "ts": a_ts,
                "turn": idx,
                "usage": {
                    "cache_read": cr,
                    "cache_write": cw,
                    "confidence": "measured",
                    "fresh_in": fresh,
                    "output": out,
                    "reasoning": None,
                },
            }
        )

    expected = {
        "agent": "claude-code",
        "log_files": [log_rel],
        "scenario": "near-full-context",
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
        f"claude-code/near-full-context/{log_rel}": dumps_jsonl(records),
        "claude-code/near-full-context/expected.json": dumps_pretty_json(expected),
    }


def gen_claude_cache_miss_storm(rng: SplitMix64, seed: int) -> dict[str, bytes]:
    """Generates `claude-code/cache-miss-storm` (`PLAN.md` §6.3, §9, §10 T07).

    5-turn session switching models (`claude-sonnet-4-5` <-> `claude-opus-4-5`) across
    Turns `2..=4` (8s apart, well inside 5m TTL), causing 3 consecutive mid-session
    `cache_write >> cache_read` misses (`45,480` rewritten tokens) before stabilising on Turn 5.
    """
    session_id = rng.uuid_str()
    cwd = "/synthetic/workspace/storm"
    log_rel = "projects/-synthetic-workspace-storm/session-cache-miss-storm-0001.jsonl"
    version = "2.1.314"

    records: list[dict[str, Any]] = []
    turns_gt: list[dict[str, Any]] = []
    push_user_text, push_assistant_text = _make_claude_text_helpers(
        rng,
        records,
        cwd=cwd,
        git_branch="task/synth-storm",
        session_id=session_id,
        version=version,
    )

    turn_specs = [
        ("2026-10-31T12:00:01Z", 120, "2026-10-31T12:00:04Z", "claude-sonnet-4-5", 160, 30, 15000, 0, 40),
        ("2026-10-31T12:00:08Z", 160, "2026-10-31T12:00:12Z", "claude-opus-4-5", 200, 40, 15070, 0, 50),
        ("2026-10-31T12:00:16Z", 160, "2026-10-31T12:00:20Z", "claude-sonnet-4-5", 200, 40, 15160, 0, 50),
        ("2026-10-31T12:00:24Z", 160, "2026-10-31T12:00:28Z", "claude-opus-4-5", 200, 40, 15250, 0, 50),
        ("2026-10-31T12:00:32Z", 120, "2026-10-31T12:00:36Z", "claude-opus-4-5", 160, 30, 50, 15290, 40),
    ]

    prev_uuid: str | None = None
    for idx, (u_ts, u_bytes, a_ts, turn_model, a_bytes, fresh, cw, cr, out) in enumerate(
        turn_specs, start=1
    ):
        u_uuid, u_text = push_user_text(prev_uuid, u_ts, u_bytes)
        a_uuid, a_text = push_assistant_text(
            u_uuid, a_ts, turn_model, a_bytes, fresh, cw, cr, out
        )
        prev_uuid = a_uuid
        turns_gt.append(
            {
                "added_blocks": [
                    {"bytes": len(u_text.encode("utf-8")), "kind": "user_prompt", "tool": None},
                    {"bytes": len(a_text.encode("utf-8")), "kind": "assistant_text", "tool": None},
                ],
                "hints": default_hints(),
                "model": turn_model,
                "prompt_tokens": fresh + cw + cr,
                "rate_limit_used_percent": None,
                "ts": a_ts,
                "turn": idx,
                "usage": {
                    "cache_read": cr,
                    "cache_write": cw,
                    "confidence": "measured",
                    "fresh_in": fresh,
                    "output": out,
                    "reasoning": None,
                },
            }
        )

    expected = {
        "agent": "claude-code",
        "log_files": [log_rel],
        "scenario": "cache-miss-storm",
        "schema": "kakeibo.fixture-ground-truth/1",
        "seed": seed,
        "sessions": [
            {
                "ignored_records": 0,
                "model": "claude-sonnet-4-5",
                "session_id": session_id,
                "totals": compute_session_totals(turns_gt),
                "turns": turns_gt,
            }
        ],
    }

    return {
        f"claude-code/cache-miss-storm/{log_rel}": dumps_jsonl(records),
        "claude-code/cache-miss-storm/expected.json": dumps_pretty_json(expected),
    }


def gen_claude_heavy_overhead(rng: SplitMix64, seed: int) -> dict[str, bytes]:
    """Generates `claude-code/heavy-overhead` (`PLAN.md` §6.3, §10 T07).

    2-turn session starting with `P(1) = 48,400` (`48,376` fixed overhead + `24`-token
    first user prompt) on Turn 1, followed by a warm cache hit on Turn 2.
    """
    session_id = rng.uuid_str()
    cwd = "/synthetic/workspace/overhead"
    log_rel = "projects/-synthetic-workspace-overhead/session-heavy-overhead-0001.jsonl"
    model = "claude-sonnet-4-5"
    version = "2.1.314"

    records: list[dict[str, Any]] = []
    turns_gt: list[dict[str, Any]] = []
    push_user_text, push_assistant_text = _make_claude_text_helpers(
        rng,
        records,
        cwd=cwd,
        git_branch="task/synth-overhead",
        session_id=session_id,
        version=version,
    )

    # Turn 1: P(1) = 48,400 (FixedOverhead = 48,376, u1 = 24).
    u1_uuid, u1_text = push_user_text(None, "2026-10-31T13:00:01Z", 96)
    t1_fresh, t1_cw, t1_cr, t1_out = 24, 48376, 0, 40
    a1_uuid, a1_text = push_assistant_text(
        u1_uuid, "2026-10-31T13:00:04Z", model, 160, t1_fresh, t1_cw, t1_cr, t1_out
    )
    turns_gt.append(
        {
            "added_blocks": [
                {"bytes": len(u1_text.encode("utf-8")), "kind": "user_prompt", "tool": None},
                {"bytes": len(a1_text.encode("utf-8")), "kind": "assistant_text", "tool": None},
            ],
            "hints": default_hints(),
            "model": model,
            "prompt_tokens": t1_fresh + t1_cw + t1_cr,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T13:00:04Z",
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

    # Turn 2: warm cache_read = 48,400.
    u2_uuid, u2_text = push_user_text(a1_uuid, "2026-10-31T13:00:08Z", 120)
    t2_fresh, t2_cw, t2_cr, t2_out = 30, 40, 48400, 40
    _, a2_text = push_assistant_text(
        u2_uuid, "2026-10-31T13:00:12Z", model, 160, t2_fresh, t2_cw, t2_cr, t2_out
    )
    turns_gt.append(
        {
            "added_blocks": [
                {"bytes": len(u2_text.encode("utf-8")), "kind": "user_prompt", "tool": None},
                {"bytes": len(a2_text.encode("utf-8")), "kind": "assistant_text", "tool": None},
            ],
            "hints": default_hints(),
            "model": model,
            "prompt_tokens": t2_fresh + t2_cw + t2_cr,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T13:00:12Z",
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

    expected = {
        "agent": "claude-code",
        "log_files": [log_rel],
        "scenario": "heavy-overhead",
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
        f"claude-code/heavy-overhead/{log_rel}": dumps_jsonl(records),
        "claude-code/heavy-overhead/expected.json": dumps_pretty_json(expected),
    }


def gen_litellm_pricing_tiny(rng: SplitMix64, seed: int) -> dict[str, bytes]:
    """Synthetic tiny LiteLLM `model_prices_and_context_window.json` fixture (`PLAN.md` §6.2, T09)."""
    _ = seed
    unpriced_embed_slug = f"synth-embed-{rng.hex_id('m', 4)}"
    doc = {
        "anthropic/claude-opus-4-5": {
            "cache_creation_input_token_cost": 0.00000625,
            "cache_creation_input_token_cost_above_1hr": 0.00001,
            "cache_read_input_token_cost": 0.0000005,
            "input_cost_per_token": 0.000005,
            "litellm_provider": "anthropic",
            "max_input_tokens": 200000,
            "mode": "chat",
            "output_cost_per_token": 0.000025,
        },
        "claude-haiku-4-5": {
            "cache_creation_input_token_cost": 0.00000125,
            "cache_creation_input_token_cost_above_1hr": 0.000002,
            "cache_read_input_token_cost": 0.0000001,
            "input_cost_per_token": 0.000001,
            "litellm_provider": "anthropic",
            "max_input_tokens": 200000,
            "mode": "chat",
            "output_cost_per_token": 0.000005,
        },
        "claude-sonnet-4-5": {
            "cache_creation_input_token_cost": 0.0000025,
            "cache_creation_input_token_cost_above_1hr": 0.000004,
            "cache_read_input_token_cost": 0.0000002,
            "input_cost_per_token": 0.000002,
            "litellm_provider": "anthropic",
            "max_input_tokens": 200000,
            "mode": "chat",
            "output_cost_per_token": 0.00001,
        },
        "gpt-5-codex": {
            "cache_read_input_token_cost": 0.00000015,
            "input_cost_per_token": 0.0000015,
            "litellm_provider": "openai",
            "max_input_tokens": 400000,
            "mode": "chat",
            "output_cost_per_reasoning_token": 0.000008,
            "output_cost_per_token": 0.000006,
        },
        "sample_spec": {
            "cache_creation_input_token_cost": 0.0,
            "cache_read_input_token_cost": 0.0,
            "input_cost_per_token": 0.0,
            "litellm_provider": "sample_spec",
            "max_input_tokens": 0,
            "mode": "chat",
            "output_cost_per_token": 0.0,
        },
        unpriced_embed_slug: {
            "litellm_provider": "synthetic",
            "max_input_tokens": 8192,
            "mode": "embedding",
        },
    }
    return {
        "pricing/litellm-tiny.json": dumps_pretty_json(doc),
    }


def gen_gemini_rewind_patch(rng: SplitMix64, seed: int) -> dict[str, bytes]:
    """Synthetic Gemini CLI 0.41.2 JSONL session exercising `$patch`, same-ID upserts,
    `$rewindTo` truncation (target + subsequent messages), `$set`, and `projects.json` (`T10`).
    """
    session_id = rng.uuid_str()
    slug = "synth-gemini-alpha"
    cwd = "/synthetic/workspace/gemini-alpha"
    project_hash = hashlib.sha256(cwd.encode("utf-8")).hexdigest()
    log_rel = f"tmp/{slug}/chats/session-2026-10-31T09-00-{session_id[:8]}.jsonl"
    model = "gemini-2.5-pro"

    records: list[dict[str, Any]] = []
    turns_gt: list[dict[str, Any]] = []

    # 1. Conversation metadata header (`ignored_records += 1`).
    records.append(
        {
            "directories": [cwd],
            "kind": "main",
            "lastUpdated": "2026-10-31T09:00:00Z",
            "projectHash": project_hash,
            "sessionId": session_id,
            "startTime": "2026-10-31T09:00:00Z",
        }
    )

    # 2. Turn 1 user prompt (`PartListUnion` array form).
    u1_id = rng.uuid_str()
    u1_text = rng.synth_text(104)
    records.append(
        {
            "content": [{"text": u1_text}],
            "id": u1_id,
            "timestamp": "2026-10-31T09:00:01Z",
            "type": "user",
        }
    )

    # 3. Turn 1 initial `"gemini"` message with `thoughts` and `executing` tool call.
    g1_id = rng.uuid_str()
    th1_subj = "Inspect parser module"
    th1_desc = rng.synth_text(88)
    a1_text = rng.synth_text(120)
    call1_id = rng.hex_id("call", 12)
    tool1_args = {"file_path": "crates/kakeibo-parse/src/lib.rs"}
    tool1_args_bytes = len(
        json.dumps(tool1_args, sort_keys=True, separators=(",", ":")).encode("utf-8")
    )

    t1_cached = 6400
    t1_input = 8200 + rng.rand_range(0, 60)
    t1_tool = 140
    t1_out_body = 96
    t1_thoughts = 54

    records.append(
        {
            "content": a1_text,
            "id": g1_id,
            "model": model,
            "thoughts": [
                {
                    "description": th1_desc,
                    "subject": th1_subj,
                    "timestamp": "2026-10-31T09:00:02Z",
                }
            ],
            "timestamp": "2026-10-31T09:00:03Z",
            "tokens": {
                "cached": t1_cached,
                "input": t1_input,
                "output": t1_out_body,
                "thoughts": t1_thoughts,
                "tool": t1_tool,
                "total": t1_input + t1_tool + t1_out_body + t1_thoughts,
            },
            "toolCalls": [
                {
                    "args": tool1_args,
                    "displayName": "ReadFile",
                    "id": call1_id,
                    "name": "read_file",
                    "status": "executing",
                }
            ],
            "type": "gemini",
        }
    )

    # 4. RFC-6902 `$patch` attaching tool output (`result`) and `"success"` status to `g1_id`.
    tr1_text = rng.synth_text(312)
    records.append(
        {
            "$patch": [
                {
                    "op": "add",
                    "path": "/toolCalls/0/result",
                    "value": [
                        {
                            "functionResponse": {
                                "id": call1_id,
                                "name": "read_file",
                                "response": {"output": tr1_text},
                            }
                        }
                    ],
                },
                {
                    "op": "replace",
                    "path": "/toolCalls/0/status",
                    "value": "success",
                },
            ],
            "id": g1_id,
        }
    )

    t1_prompt = t1_input + t1_tool
    t1_fresh = t1_prompt - t1_cached
    turns_gt.append(
        {
            "added_blocks": [
                {"bytes": len(u1_text.encode("utf-8")), "kind": "user_prompt", "tool": None},
                {
                    "bytes": len(th1_subj.encode("utf-8")) + len(th1_desc.encode("utf-8")),
                    "kind": "assistant_thinking",
                    "tool": None,
                },
                {"bytes": len(a1_text.encode("utf-8")), "kind": "assistant_text", "tool": None},
                {"bytes": tool1_args_bytes, "kind": "tool_use", "tool": "read_file"},
            ],
            "hints": default_hints(),
            "model": model,
            "prompt_tokens": t1_prompt,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T09:00:03Z",
            "turn": 1,
            "usage": {
                "cache_read": t1_cached,
                "cache_write": 0,
                "confidence": "measured",
                "fresh_in": t1_fresh,
                "output": t1_out_body + t1_thoughts,
                "reasoning": t1_thoughts,
            },
        }
    )

    # 5 & 6. Abandoned user + gemini exchange later discarded by `$rewindTo`.
    u_abandoned_id = rng.uuid_str()
    records.append(
        {
            "content": rng.synth_text(84),
            "id": u_abandoned_id,
            "timestamp": "2026-10-31T09:00:05Z",
            "type": "user",
        }
    )
    g_abandoned_id = rng.uuid_str()
    records.append(
        {
            "content": rng.synth_text(160),
            "id": g_abandoned_id,
            "model": model,
            "timestamp": "2026-10-31T09:00:07Z",
            "tokens": {
                "cached": 4000,
                "input": 55000,
                "output": 900,
                "thoughts": 300,
                "tool": 0,
                "total": 56200,
            },
            "type": "gemini",
        }
    )

    # 7. `$rewindTo` removes `u_abandoned_id` AND `g_abandoned_id` (`ignored_records += 1`).
    records.append({"$rewindTo": u_abandoned_id})

    # 8. Non-turn `"info"` message (`ignored_records += 1`).
    records.append(
        {
            "content": rng.synth_text(64),
            "id": rng.uuid_str(),
            "timestamp": "2026-10-31T09:00:08Z",
            "type": "info",
        }
    )

    # 9 & 10. Turn 2 `"gemini"` message written first without tokens, then upserted by same ID.
    g2_id = rng.uuid_str()
    a2_draft = rng.synth_text(48)
    a2_final = rng.synth_text(176)
    records.append(
        {
            "content": a2_draft,
            "id": g2_id,
            "model": model,
            "timestamp": "2026-10-31T09:00:10Z",
            "type": "gemini",
        }
    )

    t2_cached = t1_prompt
    t2_input = t1_prompt + 420 + rng.rand_range(0, 40)
    t2_tool = 80
    t2_out_body = 132
    records.append(
        {
            "content": a2_final,
            "id": g2_id,
            "model": model,
            "timestamp": "2026-10-31T09:00:12Z",
            "tokens": {
                "cached": t2_cached,
                "input": t2_input,
                "output": t2_out_body,
                "thoughts": 0,
                "tool": t2_tool,
                "total": t2_input + t2_tool + t2_out_body,
            },
            "type": "gemini",
        }
    )

    # 11. `$set` metadata update (`ignored_records += 1`).
    records.append(
        {
            "$set": {
                "lastUpdated": "2026-10-31T09:00:12Z",
                "summary": rng.synth_text(40),
            }
        }
    )

    t2_prompt = t2_input + t2_tool
    t2_fresh = t2_prompt - t2_cached
    turns_gt.append(
        {
            "added_blocks": [
                {"bytes": len(tr1_text.encode("utf-8")), "kind": "tool_result", "tool": "read_file"},
                {"bytes": len(a2_final.encode("utf-8")), "kind": "assistant_text", "tool": None},
            ],
            "hints": default_hints(),
            "model": model,
            "prompt_tokens": t2_prompt,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T09:00:12Z",
            "turn": 2,
            "usage": {
                "cache_read": t2_cached,
                "cache_write": 0,
                "confidence": "measured",
                "fresh_in": t2_fresh,
                "output": t2_out_body,
                "reasoning": None,
            },
        }
    )

    projects_doc = {"projects": {cwd: slug}}
    expected = {
        "agent": "gemini-cli",
        "log_files": [log_rel],
        "scenario": "rewind-patch",
        "schema": "kakeibo.fixture-ground-truth/1",
        "seed": seed,
        "sessions": [
            {
                "ignored_records": 4,
                "model": model,
                "session_id": session_id,
                "totals": compute_session_totals(turns_gt),
                "turns": turns_gt,
            }
        ],
    }

    return {
        "gemini-cli/rewind-patch/projects.json": dumps_pretty_json(projects_doc),
        f"gemini-cli/rewind-patch/{log_rel}": dumps_jsonl(records),
        "gemini-cli/rewind-patch/expected.json": dumps_pretty_json(expected),
    }


def gen_gemini_legacy_json(rng: SplitMix64, seed: int) -> dict[str, bytes]:
    """Synthetic legacy Gemini CLI single-object `.json` `ConversationRecord`
    with `.project_root` marker and intermediate `type: "error"` message (`T10`).
    """
    session_id = rng.uuid_str()
    slug = "synth-gemini-legacy"
    cwd = "/synthetic/workspace/gemini-legacy"
    project_hash = hashlib.sha256(cwd.encode("utf-8")).hexdigest()
    log_rel = f"tmp/{slug}/chats/session-2026-10-31T10-00-{session_id[:8]}.json"
    model = "gemini-2.5-flash"

    u1_text = rng.synth_text(92)
    a1_text = rng.synth_text(108)
    call1_id = rng.hex_id("call", 12)
    tool1_args = {"command": "cargo test --lib"}
    tool1_args_bytes = len(
        json.dumps(tool1_args, sort_keys=True, separators=(",", ":")).encode("utf-8")
    )
    tr1_text = rng.synth_text(248)
    err_text = rng.synth_text(56)
    a2_text = rng.synth_text(144)

    t1_cached = 2400
    t1_input = 4100 + rng.rand_range(0, 50)
    t1_out = 78
    t1_thoughts = 32

    t2_cached = t1_input
    t2_input = t1_input + 340 + rng.rand_range(0, 30)
    t2_tool = 48
    t2_out = 104

    conversation_doc = {
        "kind": "main",
        "lastUpdated": "2026-10-31T10:00:10Z",
        "messages": [
            {
                "content": u1_text,
                "id": rng.uuid_str(),
                "timestamp": "2026-10-31T10:00:01Z",
                "type": "user",
            },
            {
                "content": a1_text,
                "id": rng.uuid_str(),
                "model": model,
                "timestamp": "2026-10-31T10:00:04Z",
                "tokens": {
                    "cached": t1_cached,
                    "input": t1_input,
                    "output": t1_out,
                    "thoughts": t1_thoughts,
                    "tool": 0,
                    "total": t1_input + t1_out + t1_thoughts,
                },
                "toolCalls": [
                    {
                        "args": tool1_args,
                        "id": call1_id,
                        "name": "run_shell_command",
                        "result": [
                            {
                                "functionResponse": {
                                    "id": call1_id,
                                    "name": "run_shell_command",
                                    "response": {"output": tr1_text},
                                }
                            }
                        ],
                        "resultDisplay": tr1_text,
                        "status": "success",
                    }
                ],
                "type": "gemini",
            },
            {
                "content": err_text,
                "id": rng.uuid_str(),
                "timestamp": "2026-10-31T10:00:06Z",
                "type": "error",
            },
            {
                "content": a2_text,
                "id": rng.uuid_str(),
                "model": model,
                "timestamp": "2026-10-31T10:00:10Z",
                "tokens": {
                    "cached": t2_cached,
                    "input": t2_input,
                    "output": t2_out,
                    "thoughts": 0,
                    "tool": t2_tool,
                    "total": t2_input + t2_tool + t2_out,
                },
                "type": "gemini",
            },
        ],
        "projectHash": project_hash,
        "sessionId": session_id,
        "startTime": "2026-10-31T10:00:00Z",
        "summary": rng.synth_text(36),
    }

    t2_hints = default_hints()
    t2_hints["api_error"] = True

    turns_gt = [
        {
            "added_blocks": [
                {"bytes": len(u1_text.encode("utf-8")), "kind": "user_prompt", "tool": None},
                {"bytes": len(a1_text.encode("utf-8")), "kind": "assistant_text", "tool": None},
                {"bytes": tool1_args_bytes, "kind": "tool_use", "tool": "run_shell_command"},
            ],
            "hints": default_hints(),
            "model": model,
            "prompt_tokens": t1_input,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T10:00:04Z",
            "turn": 1,
            "usage": {
                "cache_read": t1_cached,
                "cache_write": 0,
                "confidence": "measured",
                "fresh_in": t1_input - t1_cached,
                "output": t1_out + t1_thoughts,
                "reasoning": t1_thoughts,
            },
        },
        {
            "added_blocks": [
                {
                    "bytes": len(tr1_text.encode("utf-8")),
                    "kind": "tool_result",
                    "tool": "run_shell_command",
                },
                {"bytes": len(a2_text.encode("utf-8")), "kind": "assistant_text", "tool": None},
            ],
            "hints": t2_hints,
            "model": model,
            "prompt_tokens": t2_input + t2_tool,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T10:00:10Z",
            "turn": 2,
            "usage": {
                "cache_read": t2_cached,
                "cache_write": 0,
                "confidence": "measured",
                "fresh_in": (t2_input + t2_tool) - t2_cached,
                "output": t2_out,
                "reasoning": None,
            },
        },
    ]

    projects_doc = {"projects": {cwd: slug}}
    expected = {
        "agent": "gemini-cli",
        "log_files": [log_rel],
        "scenario": "legacy-json",
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
        "gemini-cli/legacy-json/projects.json": dumps_pretty_json(projects_doc),
        f"gemini-cli/legacy-json/tmp/{slug}/.project_root": (cwd + "\n").encode("utf-8"),
        f"gemini-cli/legacy-json/{log_rel}": dumps_pretty_json(conversation_doc),
        "gemini-cli/legacy-json/expected.json": dumps_pretty_json(expected),
    }


def gen_opencode_legacy_tree(rng: SplitMix64, seed: int) -> dict[str, bytes]:
    """Synthetic OpenCode legacy `storage/{session,message,part}/` JSON hierarchy
    exercising disjoint `tokens.cache.{read,write}`, `reasoning`, tool I/O, and zero `cost` (`T10`).
    """
    project_id = rng.hex_id("prj", 16)
    session_id = rng.hex_id("ses", 16)
    cwd = "/synthetic/workspace/opencode-alpha"
    model_id = "claude-sonnet-4-5"
    provider_id = "anthropic"
    version = "1.2.4"

    session_rel = f"storage/session/{project_id}/{session_id}.json"
    session_doc = {
        "directory": cwd,
        "id": session_id,
        "parentID": None,
        "projectID": project_id,
        "slug": "synth-opencode-alpha",
        "time": {"created": 1793437200000, "updated": 1793437212000},
        "title": rng.synth_text(32),
        "version": version,
    }

    u1_msg_id = f"msg_0001_{rng.hex_id('m', 8)}"
    u1_prt_id = f"prt_0001_{rng.hex_id('p', 8)}"
    u1_text = rng.synth_text(100)
    u1_msg_doc = {
        "agent": "build",
        "id": u1_msg_id,
        "model": {"modelID": model_id, "providerID": provider_id},
        "role": "user",
        "sessionID": session_id,
        "time": {"created": 1793437201000},
    }
    u1_prt_doc = {
        "id": u1_prt_id,
        "messageID": u1_msg_id,
        "sessionID": session_id,
        "text": u1_text,
        "type": "text",
    }

    a1_msg_id = f"msg_0002_{rng.hex_id('m', 8)}"
    a1_step_prt_id = f"prt_0001_{rng.hex_id('p', 8)}"
    a1_think_prt_id = f"prt_0002_{rng.hex_id('p', 8)}"
    a1_text_prt_id = f"prt_0003_{rng.hex_id('p', 8)}"
    a1_tool_prt_id = f"prt_0004_{rng.hex_id('p', 8)}"
    call1_id = rng.hex_id("call", 12)
    th1_text = rng.synth_text(80)
    a1_text = rng.synth_text(116)
    tool1_input = {"filePath": "/synthetic/workspace/opencode-alpha/src/main.rs"}
    tool1_input_bytes = len(
        json.dumps(tool1_input, sort_keys=True, separators=(",", ":")).encode("utf-8")
    )
    tr1_text = rng.synth_text(296)

    t1_fresh = 380 + rng.rand_range(0, 40)
    t1_cw = 8600
    t1_cr = 0
    t1_out_body = 92
    t1_reasoning = 44

    a1_msg_doc = {
        "agent": "build",
        "cost": 0,
        "finish": "tool-calls",
        "id": a1_msg_id,
        "mode": "build",
        "modelID": model_id,
        "parentID": u1_msg_id,
        "path": {"cwd": cwd, "root": cwd},
        "providerID": provider_id,
        "role": "assistant",
        "sessionID": session_id,
        "time": {"completed": 1793437204000, "created": 1793437202000},
        "tokens": {
            "cache": {"read": t1_cr, "write": t1_cw},
            "input": t1_fresh,
            "output": t1_out_body,
            "reasoning": t1_reasoning,
        },
    }
    a1_step_prt_doc = {
        "id": a1_step_prt_id,
        "messageID": a1_msg_id,
        "sessionID": session_id,
        "type": "step-start",
    }
    a1_think_prt_doc = {
        "id": a1_think_prt_id,
        "messageID": a1_msg_id,
        "sessionID": session_id,
        "text": th1_text,
        "type": "reasoning",
    }
    a1_text_prt_doc = {
        "id": a1_text_prt_id,
        "messageID": a1_msg_id,
        "sessionID": session_id,
        "text": a1_text,
        "type": "text",
    }
    a1_tool_prt_doc = {
        "callID": call1_id,
        "id": a1_tool_prt_id,
        "messageID": a1_msg_id,
        "sessionID": session_id,
        "state": {
            "input": tool1_input,
            "metadata": {"truncated": False},
            "output": tr1_text,
            "status": "completed",
            "time": {"end": 1793437205000, "start": 1793437204100},
            "title": "src/main.rs",
        },
        "tool": "read",
        "type": "tool",
    }

    a2_msg_id = f"msg_0003_{rng.hex_id('m', 8)}"
    a2_text_prt_id = f"prt_0001_{rng.hex_id('p', 8)}"
    a2_text = rng.synth_text(168)
    t2_fresh = 210 + rng.rand_range(0, 30)
    t2_cw = 340
    t2_cr = t1_fresh + t1_cw
    t2_out_body = 118

    a2_msg_doc = {
        "agent": "build",
        "cost": 0,
        "finish": "stop",
        "id": a2_msg_id,
        "mode": "build",
        "modelID": model_id,
        "parentID": u1_msg_id,
        "path": {"cwd": cwd, "root": cwd},
        "providerID": provider_id,
        "role": "assistant",
        "sessionID": session_id,
        "time": {"completed": 1793437212000, "created": 1793437206000},
        "tokens": {
            "cache": {"read": t2_cr, "write": t2_cw},
            "input": t2_fresh,
            "output": t2_out_body,
            "reasoning": 0,
        },
    }
    a2_text_prt_doc = {
        "id": a2_text_prt_id,
        "messageID": a2_msg_id,
        "sessionID": session_id,
        "text": a2_text,
        "type": "text",
    }

    turns_gt = [
        {
            "added_blocks": [
                {"bytes": len(u1_text.encode("utf-8")), "kind": "user_prompt", "tool": None},
                {
                    "bytes": len(th1_text.encode("utf-8")),
                    "kind": "assistant_thinking",
                    "tool": None,
                },
                {"bytes": len(a1_text.encode("utf-8")), "kind": "assistant_text", "tool": None},
                {"bytes": tool1_input_bytes, "kind": "tool_use", "tool": "read"},
            ],
            "hints": default_hints(),
            "model": model_id,
            "prompt_tokens": t1_fresh + t1_cw + t1_cr,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T09:00:04Z",
            "turn": 1,
            "usage": {
                "cache_read": t1_cr,
                "cache_write": t1_cw,
                "confidence": "measured",
                "fresh_in": t1_fresh,
                "output": t1_out_body + t1_reasoning,
                "reasoning": t1_reasoning,
            },
        },
        {
            "added_blocks": [
                {"bytes": len(tr1_text.encode("utf-8")), "kind": "tool_result", "tool": "read"},
                {"bytes": len(a2_text.encode("utf-8")), "kind": "assistant_text", "tool": None},
            ],
            "hints": default_hints(),
            "model": model_id,
            "prompt_tokens": t2_fresh + t2_cw + t2_cr,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T09:00:12Z",
            "turn": 2,
            "usage": {
                "cache_read": t2_cr,
                "cache_write": t2_cw,
                "confidence": "measured",
                "fresh_in": t2_fresh,
                "output": t2_out_body,
                "reasoning": None,
            },
        },
    ]

    expected = {
        "agent": "opencode",
        "log_files": [session_rel],
        "scenario": "legacy-tree",
        "schema": "kakeibo.fixture-ground-truth/1",
        "seed": seed,
        "sessions": [
            {
                "ignored_records": 0,
                "model": model_id,
                "session_id": session_id,
                "totals": compute_session_totals(turns_gt),
                "turns": turns_gt,
            }
        ],
    }

    base = "opencode/legacy-tree"
    return {
        f"{base}/{session_rel}": dumps_pretty_json(session_doc),
        f"{base}/storage/message/{session_id}/{u1_msg_id}.json": dumps_pretty_json(u1_msg_doc),
        f"{base}/storage/part/{u1_msg_id}/{u1_prt_id}.json": dumps_pretty_json(u1_prt_doc),
        f"{base}/storage/message/{session_id}/{a1_msg_id}.json": dumps_pretty_json(a1_msg_doc),
        f"{base}/storage/part/{a1_msg_id}/{a1_step_prt_id}.json": dumps_pretty_json(
            a1_step_prt_doc
        ),
        f"{base}/storage/part/{a1_msg_id}/{a1_think_prt_id}.json": dumps_pretty_json(
            a1_think_prt_doc
        ),
        f"{base}/storage/part/{a1_msg_id}/{a1_text_prt_id}.json": dumps_pretty_json(
            a1_text_prt_doc
        ),
        f"{base}/storage/part/{a1_msg_id}/{a1_tool_prt_id}.json": dumps_pretty_json(
            a1_tool_prt_doc
        ),
        f"{base}/storage/message/{session_id}/{a2_msg_id}.json": dumps_pretty_json(a2_msg_doc),
        f"{base}/storage/part/{a2_msg_id}/{a2_text_prt_id}.json": dumps_pretty_json(
            a2_text_prt_doc
        ),
        f"{base}/expected.json": dumps_pretty_json(expected),
    }


def gen_opencode_subagent_cost(rng: SplitMix64, seed: int) -> dict[str, bytes]:
    """Synthetic OpenCode primary + child subagent (`parentID`) scenario with
    agent-reported USD `cost` and compaction marker (`T10`).
    """
    project_id = rng.hex_id("prj", 16)
    parent_ses_id = f"ses_0001_{rng.hex_id('s', 12)}"
    child_ses_id = f"ses_0002_{rng.hex_id('s', 12)}"
    cwd = "/synthetic/workspace/opencode-beta"
    parent_model = "claude-opus-4-5"
    child_model = "claude-haiku-4-5"
    provider_id = "anthropic"
    version = "1.2.4"

    parent_rel = f"storage/session/{project_id}/{parent_ses_id}.json"
    child_rel = f"storage/session/{project_id}/{child_ses_id}.json"

    parent_ses_doc = {
        "directory": cwd,
        "id": parent_ses_id,
        "parentID": None,
        "projectID": project_id,
        "slug": "synth-opencode-parent",
        "time": {"created": 1793440800000, "updated": 1793440815000},
        "title": rng.synth_text(28),
        "version": version,
    }
    child_ses_doc = {
        "directory": cwd,
        "id": child_ses_id,
        "parentID": parent_ses_id,
        "projectID": project_id,
        "slug": "synth-opencode-child",
        "time": {"created": 1793440803000, "updated": 1793440807000},
        "title": rng.synth_text(28),
        "version": version,
    }

    # Parent Turn 1: user prompt -> assistant invokes `task` subagent tool (`cost: 0.0425`).
    p_u1_id = f"msg_0001_{rng.hex_id('m', 8)}"
    p_u1_prt_id = f"prt_0001_{rng.hex_id('p', 8)}"
    p_u1_text = rng.synth_text(112)
    p_u1_doc = {
        "agent": "build",
        "id": p_u1_id,
        "model": {"modelID": parent_model, "providerID": provider_id},
        "role": "user",
        "sessionID": parent_ses_id,
        "time": {"created": 1793440801000},
    }
    p_u1_prt_doc = {
        "id": p_u1_prt_id,
        "messageID": p_u1_id,
        "sessionID": parent_ses_id,
        "text": p_u1_text,
        "type": "text",
    }

    p_a1_id = f"msg_0002_{rng.hex_id('m', 8)}"
    p_a1_tool_prt_id = f"prt_0001_{rng.hex_id('p', 8)}"
    task_call_id = rng.hex_id("call", 12)
    sub_prompt = rng.synth_text(88)
    sub_result = rng.synth_text(184)
    task_input = {"description": "Audit SQLite flags", "prompt": sub_prompt}
    task_input_bytes = len(
        json.dumps(task_input, sort_keys=True, separators=(",", ":")).encode("utf-8")
    )

    p1_fresh = 460 + rng.rand_range(0, 30)
    p1_cw = 6200
    p1_cr = 1800
    p1_out = 86

    p_a1_doc = {
        "agent": "build",
        "cost": 0.0425,
        "finish": "tool-calls",
        "id": p_a1_id,
        "mode": "build",
        "modelID": parent_model,
        "parentID": p_u1_id,
        "path": {"cwd": cwd, "root": cwd},
        "providerID": provider_id,
        "role": "assistant",
        "sessionID": parent_ses_id,
        "time": {"completed": 1793440808000, "created": 1793440802000},
        "tokens": {
            "cache": {"read": p1_cr, "write": p1_cw},
            "input": p1_fresh,
            "output": p1_out,
            "reasoning": 0,
        },
    }
    p_a1_tool_prt_doc = {
        "callID": task_call_id,
        "id": p_a1_tool_prt_id,
        "messageID": p_a1_id,
        "sessionID": parent_ses_id,
        "state": {
            "input": task_input,
            "metadata": {"sessionId": child_ses_id},
            "output": sub_result,
            "status": "completed",
            "time": {"end": 1793440807500, "start": 1793440802500},
            "title": "Audit SQLite flags",
        },
        "tool": "task",
        "type": "tool",
    }

    # Compaction user marker before Parent Turn 2.
    p_u2_id = f"msg_0003_{rng.hex_id('m', 8)}"
    p_u2_prt_id = f"prt_0001_{rng.hex_id('p', 8)}"
    p_u2_doc = {
        "agent": "build",
        "id": p_u2_id,
        "model": {"modelID": parent_model, "providerID": provider_id},
        "role": "user",
        "sessionID": parent_ses_id,
        "time": {"created": 1793440810000},
    }
    p_u2_prt_doc = {
        "auto": True,
        "id": p_u2_prt_id,
        "messageID": p_u2_id,
        "sessionID": parent_ses_id,
        "type": "compaction",
    }

    # Parent Turn 2 (`cost: 0.0185`, `compaction: True`).
    p_a2_id = f"msg_0004_{rng.hex_id('m', 8)}"
    p_a2_prt_id = f"prt_0001_{rng.hex_id('p', 8)}"
    p_a2_text = rng.synth_text(152)
    p2_fresh = 310 + rng.rand_range(0, 25)
    p2_cw = 1400
    p2_cr = 2200
    p2_out = 108

    p_a2_doc = {
        "agent": "build",
        "cost": 0.0185,
        "finish": "stop",
        "id": p_a2_id,
        "mode": "build",
        "modelID": parent_model,
        "parentID": p_u2_id,
        "path": {"cwd": cwd, "root": cwd},
        "providerID": provider_id,
        "role": "assistant",
        "sessionID": parent_ses_id,
        "summary": True,
        "time": {"completed": 1793440815000, "created": 1793440811000},
        "tokens": {
            "cache": {"read": p2_cr, "write": p2_cw},
            "input": p2_fresh,
            "output": p2_out,
            "reasoning": 0,
        },
    }
    p_a2_prt_doc = {
        "id": p_a2_prt_id,
        "messageID": p_a2_id,
        "sessionID": parent_ses_id,
        "text": p_a2_text,
        "type": "text",
    }

    # Child subagent session (`parentID: parent_ses_id`, `cost: 0.0125`).
    c_u1_id = f"msg_0001_{rng.hex_id('m', 8)}"
    c_u1_prt_id = f"prt_0001_{rng.hex_id('p', 8)}"
    c_u1_doc = {
        "agent": "general",
        "id": c_u1_id,
        "model": {"modelID": child_model, "providerID": provider_id},
        "role": "user",
        "sessionID": child_ses_id,
        "time": {"created": 1793440803500},
    }
    c_u1_prt_doc = {
        "id": c_u1_prt_id,
        "messageID": c_u1_id,
        "sessionID": child_ses_id,
        "text": sub_prompt,
        "type": "text",
    }

    c_a1_id = f"msg_0002_{rng.hex_id('m', 8)}"
    c_a1_prt_id = f"prt_0001_{rng.hex_id('p', 8)}"
    c1_fresh = 240 + rng.rand_range(0, 20)
    c1_cw = 1900
    c1_cr = 0
    c1_out = 94

    c_a1_doc = {
        "agent": "general",
        "cost": 0.0125,
        "finish": "stop",
        "id": c_a1_id,
        "mode": "general",
        "modelID": child_model,
        "parentID": c_u1_id,
        "path": {"cwd": cwd, "root": cwd},
        "providerID": provider_id,
        "role": "assistant",
        "sessionID": child_ses_id,
        "time": {"completed": 1793440807000, "created": 1793440804000},
        "tokens": {
            "cache": {"read": c1_cr, "write": c1_cw},
            "input": c1_fresh,
            "output": c1_out,
            "reasoning": 0,
        },
    }
    c_a1_prt_doc = {
        "id": c_a1_prt_id,
        "messageID": c_a1_id,
        "sessionID": child_ses_id,
        "text": sub_result,
        "type": "text",
    }

    p2_hints = default_hints()
    p2_hints["compaction"] = True
    parent_turns_gt = [
        {
            "added_blocks": [
                {"bytes": len(p_u1_text.encode("utf-8")), "kind": "user_prompt", "tool": None},
                {"bytes": task_input_bytes, "kind": "tool_use", "tool": "task"},
            ],
            "hints": default_hints(),
            "model": parent_model,
            "prompt_tokens": p1_fresh + p1_cw + p1_cr,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T10:00:08Z",
            "turn": 1,
            "usage": {
                "cache_read": p1_cr,
                "cache_write": p1_cw,
                "confidence": "measured",
                "fresh_in": p1_fresh,
                "output": p1_out,
                "reasoning": None,
            },
        },
        {
            "added_blocks": [
                {"bytes": len(sub_result.encode("utf-8")), "kind": "tool_result", "tool": "task"},
                {"bytes": len(p_a2_text.encode("utf-8")), "kind": "assistant_text", "tool": None},
            ],
            "hints": p2_hints,
            "model": parent_model,
            "prompt_tokens": p2_fresh + p2_cw + p2_cr,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T10:00:15Z",
            "turn": 2,
            "usage": {
                "cache_read": p2_cr,
                "cache_write": p2_cw,
                "confidence": "measured",
                "fresh_in": p2_fresh,
                "output": p2_out,
                "reasoning": None,
            },
        },
    ]

    c1_hints = default_hints()
    c1_hints["sidechain"] = True
    child_turns_gt = [
        {
            "added_blocks": [
                {"bytes": len(sub_prompt.encode("utf-8")), "kind": "user_prompt", "tool": None},
                {"bytes": len(sub_result.encode("utf-8")), "kind": "assistant_text", "tool": None},
            ],
            "hints": c1_hints,
            "model": child_model,
            "prompt_tokens": c1_fresh + c1_cw + c1_cr,
            "rate_limit_used_percent": None,
            "ts": "2026-10-31T10:00:07Z",
            "turn": 1,
            "usage": {
                "cache_read": c1_cr,
                "cache_write": c1_cw,
                "confidence": "measured",
                "fresh_in": c1_fresh,
                "output": c1_out,
                "reasoning": None,
            },
        }
    ]

    expected = {
        "agent": "opencode",
        "log_files": [parent_rel, child_rel],
        "scenario": "subagent-cost",
        "schema": "kakeibo.fixture-ground-truth/1",
        "seed": seed,
        "sessions": [
            {
                "ignored_records": 0,
                "model": parent_model,
                "session_id": parent_ses_id,
                "totals": compute_session_totals(parent_turns_gt),
                "turns": parent_turns_gt,
            },
            {
                "ignored_records": 0,
                "model": child_model,
                "session_id": child_ses_id,
                "totals": compute_session_totals(child_turns_gt),
                "turns": child_turns_gt,
            },
        ],
    }

    base = "opencode/subagent-cost"
    return {
        f"{base}/{parent_rel}": dumps_pretty_json(parent_ses_doc),
        f"{base}/{child_rel}": dumps_pretty_json(child_ses_doc),
        f"{base}/storage/message/{parent_ses_id}/{p_u1_id}.json": dumps_pretty_json(p_u1_doc),
        f"{base}/storage/part/{p_u1_id}/{p_u1_prt_id}.json": dumps_pretty_json(p_u1_prt_doc),
        f"{base}/storage/message/{parent_ses_id}/{p_a1_id}.json": dumps_pretty_json(p_a1_doc),
        f"{base}/storage/part/{p_a1_id}/{p_a1_tool_prt_id}.json": dumps_pretty_json(
            p_a1_tool_prt_doc
        ),
        f"{base}/storage/message/{parent_ses_id}/{p_u2_id}.json": dumps_pretty_json(p_u2_doc),
        f"{base}/storage/part/{p_u2_id}/{p_u2_prt_id}.json": dumps_pretty_json(p_u2_prt_doc),
        f"{base}/storage/message/{parent_ses_id}/{p_a2_id}.json": dumps_pretty_json(p_a2_doc),
        f"{base}/storage/part/{p_a2_id}/{p_a2_prt_id}.json": dumps_pretty_json(p_a2_prt_doc),
        f"{base}/storage/message/{child_ses_id}/{c_u1_id}.json": dumps_pretty_json(c_u1_doc),
        f"{base}/storage/part/{c_u1_id}/{c_u1_prt_id}.json": dumps_pretty_json(c_u1_prt_doc),
        f"{base}/storage/message/{child_ses_id}/{c_a1_id}.json": dumps_pretty_json(c_a1_doc),
        f"{base}/storage/part/{c_a1_id}/{c_a1_prt_id}.json": dumps_pretty_json(c_a1_prt_doc),
        f"{base}/expected.json": dumps_pretty_json(expected),
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
        gen_claude_resume_cold,
        gen_claude_near_full_context,
        gen_claude_cache_miss_storm,
        gen_claude_heavy_overhead,
        gen_litellm_pricing_tiny,
        gen_gemini_rewind_patch,
        gen_gemini_legacy_json,
        gen_opencode_legacy_tree,
        gen_opencode_subagent_cost,
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
