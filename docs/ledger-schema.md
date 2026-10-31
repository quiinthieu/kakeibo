# Kakeibo Ledger Schema (`kakeibo.ledger/1`)

`kakeibo.ledger/1` is the text-free Newline-Delimited JSON (NDJSON) stream of normalised [`TurnRecord`](../crates/kakeibo-core/src/model.rs) items emitted by `kakeibo export --format ndjson` and consumed by downstream tools such as Bento (`PLAN.md` §4, §11).

---

## 1. Framing & Encoding

- **Character encoding:** UTF-8 (no BOM).
- **Line delimiter:** LF (`\n`, `0x0A`). Readers also tolerate CRLF (`\r\n`) and ignore empty or whitespace-only lines.
- **One turn per line:** Each non-empty line is a single compact JSON object (RFC 8259) representing **one deduplicated model API request** (`TurnRecord`). No raw string inside a record contains an unescaped newline.
- **Required schema tag:** Every line carries `"schema": "kakeibo.ledger/1"`.
  - Readers **must** reject records whose `"schema"` field is missing or differs from `"kakeibo.ledger/1"`.
  - Readers **must** ignore unknown additive JSON keys within `"kakeibo.ledger/1"` so minor non-breaking additions remain forward-compatible.
- **Finite numbers only:** Floating-point fields (`rate_limit_used_percent`, `agent_reported_cost_usd`) must be finite IEEE-754 numbers (`NaN` and `±Infinity` are invalid).

---

## 2. Privacy Contract

A `kakeibo.ledger/1` stream **never** contains:

- Prompt text, assistant responses, chain-of-thought text, or tool arguments/outputs.
- Unhashed local filesystem paths, usernames, hostnames, git branch names, or environment variables.

Only byte lengths, token counts, tool names (e.g. `"Read"`, `"Bash"`, `"apply_patch"`), timestamps, model identifiers, and irreversible cryptographic digests (`cwd_hash`, `key_hash`) appear on the wire.

---

## 3. Top-Level Record Fields (`TurnRecord`)

| Field | JSON Type | Required | Description |
|---|---|---|---|
| `schema` | `string` | yes | Always `"kakeibo.ledger/1"`. |
| `agent` | `string` (`AgentId`) | yes | Source coding agent: `"claude-code"`, `"codex"`, `"gemini-cli"`, `"opencode"`, `"aider"`, or `"cursor"`. |
| `session_id` | `string` | yes | Agent-assigned session/thread identifier. |
| `parent_session_id` | `string` | no | Parent session identifier when this session is a subagent or fork (omitted when absent). |
| `turn` | `integer` (`u32`) | yes | 1-based ordinal of the deduplicated API request within the session. |
| `ts` | `string` (`Timestamp`) | yes | Canonical UTC RFC 3339 timestamp ending in `Z` (e.g. `"2026-10-31T09:00:04Z"` or `"2026-10-31T09:00:04.125Z"`). |
| `model` | `string` | no | Model identifier reported on the turn (omitted when unknown). |
| `cwd_hash` | `string` | no | Irreversible hex digest of the session working directory (omitted when unknown). |
| `usage` | `object` (`Usage`) | yes | Normalised token accounting for this API request (§4). |
| `added` | `array<ContentBlock>` | yes | Content blocks newly introduced since the previous turn (§5). May be empty (`[]`). |
| `hints` | `object` (`TurnHints`) | yes | Structural turn hints for attribution and root-cause flags (§6). |

---

## 4. `Usage` Object & Token Normalisation Invariant

Agents log prompt caching under two incompatible conventions (`PLAN.md` §5):

1. **Disjoint convention** (Anthropic / Claude Code, opencode): `input_tokens` counts **only** uncached fresh tokens; total prompt is `input_tokens + cache_write + cache_read`.
2. **Inclusive convention** (OpenAI / Codex CLI, Gemini CLI): `input_tokens` **includes** cached tokens; fresh input is `input_tokens - cache_read - cache_write`, and total prompt is `input_tokens`.

`kakeibo.ledger/1` normalises **every** agent into disjoint prompt components so the following invariant holds on every record:

```text
P(t) = usage.fresh_in + usage.cache_write + usage.cache_read
```

| Field | JSON Type | Required | Description |
|---|---|---|---|
| `fresh_in` | `integer` (`u64`) | yes | Prompt tokens billed as uncached fresh input. |
| `cache_write` | `integer` (`u64`) | yes | Prompt tokens written into the provider prompt cache on this turn. |
| `cache_read` | `integer` (`u64`) | yes | Prompt tokens served from the provider prompt cache on this turn. |
| `output` | `integer` (`u64`) | yes | Generated completion/output tokens (for OpenAI/Codex, includes `reasoning` per verified schema). |
| `reasoning` | `integer` (`u64`) | no | Reasoning/thinking output tokens when reported separately by the agent (omitted when absent). |
| `confidence` | `string` (`Confidence`) | yes | `"measured"` (log-reported token counts), `"estimated"` (heuristic/rounded), or `"not_recorded"` (e.g. Cursor zero-count records). |
| `estimation_method` | `string` | no | Identifier describing how counts were estimated when `confidence == "estimated"` (omitted otherwise). |
| `cache_write_split` | `object` | no | Optional Anthropic TTL breakdown `{ "ephemeral_5m": u64, "ephemeral_1h": u64 }` when reported. |
| `agent_reported_cost_usd` | `number` (`f64`) | no | Optional finite USD figure logged directly by the agent (e.g. opencode/Aider); never treated as Kakeibo's own pricing calculation. |

---

## 5. `ContentBlock` Object (`added[]`)

Each entry represents a single content block added to the conversation between turn `t - 1` and turn `t`, measured before dropping its text.

| Field | JSON Type | Required | Description |
|---|---|---|---|
| `kind` | `string` (`BlockKind`) | yes | `"user_prompt"`, `"assistant_text"`, `"assistant_thinking"`, `"tool_use"`, `"tool_result"`, `"subagent_result"`, `"compaction_summary"`, or `"system_context"`. |
| `est_tokens` | `integer` (`u64`) | yes | Estimated token size of the block prior to prompt reconciliation scaling. |
| `bytes` | `integer` (`u64`) | yes | Raw UTF-8 byte length of the dropped block payload. |
| `tool` | `string` | no | Tool identifier on `tool_use` / `tool_result` blocks (e.g. `"Read"`, `"Bash"`, `"apply_patch"`). |
| `key_hash` | `string` | no | Irreversible hex digest of `(tool_name, canonical_arguments)` used by `duplicate-reads` and `retry-loop` detectors. |
| `is_error` | `boolean` | no | Defaults to `false` (omitted when `false`); `true` when a `tool_result` reported an execution failure. |

---

## 6. `TurnHints` Object (`hints`)

| Field | JSON Type | Required | Description |
|---|---|---|---|
| `resume_start` | `boolean` | no (default `false`) | `true` when this turn is the first request after resuming or forking a prior session. |
| `compaction` | `boolean` | no (default `false`) | `true` when context compaction occurred immediately before or on this turn. |
| `sidechain` | `boolean` | no (default `false`) | `true` when the request belongs to a sidechain / subagent branch (`isSidechain`). |
| `api_error` | `boolean` | no (default `false`) | `true` when the log records an API retry or transport/provider error on this turn. |
| `rate_limit_used_percent` | `number` (`f64`) | no | Logged primary quota window percentage (currently Codex `rate_limits.primary.used_percent`, `0.0..=100.0`). |
| `context_window_override` | `integer` (`u64`) | no | Agent-reported model context window size when present in the log (`model_context_window`). |

---

## 7. Attribution Categories (`Category`)

`TurnRecord` lines carry raw normalised observations. When `kakeibo-core` attributes `P(t)` across categories (T06) or exports category breakdowns, it uses the canonical `snake_case` `Category` enum names:

- `"fixed_overhead"`
- `"resent_history"`
- `"tool_output"`
- `"retries"`
- `"user_input"`
- `"assistant_output"`
- `"subagents"`
- `"compaction"`
- `"unattributed"`

---

## 8. Synthetic Examples

Two consecutive lines (one Claude Code turn with 5m/1h cache write split, one Codex CLI turn with `rate_limit_used_percent`):

```json
{"schema":"kakeibo.ledger/1","agent":"claude-code","session_id":"57e1faba-6510-4204-b4ab-d143feb24055","turn":1,"ts":"2026-10-31T09:00:04Z","model":"claude-sonnet-4-5","cwd_hash":"a9f1e4c0b2d87136","usage":{"fresh_in":332,"cache_write":9400,"cache_read":0,"output":84,"confidence":"measured","cache_write_split":{"ephemeral_5m":9400,"ephemeral_1h":0}},"added":[{"kind":"user_prompt","est_tokens":24,"bytes":96},{"kind":"assistant_text","est_tokens":28,"bytes":112},{"kind":"tool_use","est_tokens":14,"bytes":53,"tool":"Read","key_hash":"4c8e21b90f3d7a12"}],"hints":{"resume_start":false,"compaction":false,"sidechain":false,"api_error":false}}
{"schema":"kakeibo.ledger/1","agent":"codex","session_id":"3304d23d-b2a8-4503-baed-7332b91656d8","turn":1,"ts":"2026-10-31T10:00:03Z","model":"gpt-5-codex","cwd_hash":"7b3d10c8e5a24f91","usage":{"fresh_in":3100,"cache_write":0,"cache_read":8960,"output":190,"reasoning":92,"confidence":"measured"},"added":[{"kind":"user_prompt","est_tokens":34,"bytes":136},{"kind":"tool_use","est_tokens":53,"bytes":210,"tool":"apply_patch","key_hash":"9e01a74c6b2f8d55"}],"hints":{"resume_start":false,"compaction":false,"sidechain":false,"api_error":false,"rate_limit_used_percent":11.0,"context_window_override":272000}}
```
