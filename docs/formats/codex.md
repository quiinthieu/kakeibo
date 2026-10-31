# Codex CLI Session Rollout Format

- **Agent:** OpenAI Codex CLI (`codex`)
- **Verified local version:** `codex-cli 0.165.0`
- **Checked date:** `2026-10-31`
- **Verification scope:** Structural field names, JSON value types, and boolean algebraic invariants only (no transcript values or personal paths recorded).

## Sources

- **[S1]** Local `codex-cli 0.165.0` install structural schema & algebraic invariant inspection (`checked: 2026-10-31`).
- **[S2]** Token Use — Codex CLI rollout format guide: <https://tokenuse.app/docs/development/tools/codex/> (`checked: 2026-10-31`).
- **[S3]** openai/codex #22354 — `token_count` `last_token_usage` / `total_token_usage` & `rate_limits`: <https://github.com/openai/codex/issues/22354> (`checked: 2026-10-31`).
- **[S4]** Bough — Codex format & `token_usage_record` reference: <https://www.bough.run/docs/format/codex> (`checked: 2026-10-31`).
- **[S5]** AgentMinutes — OpenAI inclusive `cached_input_tokens` accounting comparison: <https://agentminutes.dev/docs/token-comparison/> (`checked: 2026-10-31`).

## Discovery & File Layout

| Location / Variable | Role | Source |
|---|---|---|
| `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` | Active uncompressed session rollout logs (UTF-8 NDJSON). | [S1], [S2] |
| `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl.zst` | Zstandard-compressed rollout logs (**confirmed present on disk in `codex-cli 0.165.0`**). | [S1] |
| `~/.codex/archived_sessions/` | Archived rollout logs (`rollout-*.jsonl` / `.jsonl.zst`). | [S1], [S2] |
| `CODEX_HOME` | Optional environment override replacing `~/.codex` as the root directory. | [S1], [S2] |

## Observed Top-Level `type` Values (`0.165.0`)

- `session_meta` — Session header with `session_id`, `cwd`, `cli_version`, `parent_thread_id`, `forked_from_id`, `subagent_history_start_ordinal`, and `source` [S1], [S2], [S4].
- `turn_context` — Per-turn active model, `turn_id`, `cwd`, `effort`, and sandbox settings [S1], [S2], [S4].
- `response_item` — Conversation and tool items (`payload.type` in `message`, `reasoning`, `function_call`, `function_call_output`, `custom_tool_call`, `custom_tool_call_output`, `compaction`, `web_search_call`, `tool_search_call`, `tool_search_output`, `agent_message`, `additional_tools`) [S1], [S2], [S4].
- `event_msg` — Runtime events (`payload.type == "token_count"` carries usage and `rate_limits`; `payload.type == "context_compacted"` marks compaction; `payload.type` in `"error"`, `"stream_error"` marks API errors; other observed `payload.type` values `task_started`, `task_complete`, `item_completed`, `thread_settings_applied`, `agent_message`, `agent_reasoning`, `user_message`, `mcp_tool_call_end`, `patch_apply_end`, `sub_agent_activity`, `thread_goal_updated`, `turn_aborted`, `web_search_end` are counted and skipped cleanly) [S1], [S2], [S3].
- `token_usage_record` — Per-API-response usage record with `response_id`, `usage`, `turn_token_usage`, and `thread_token_usage` (**emitted alongside `event_msg/token_count` in `0.165.0` in both `tur -> tc` and `tc -> tur` orders**) [S1], [S4].
- `compacted` — Context window compaction checkpoint (marks `TurnHints.compaction = true` on the next turn; internal replacement/retained history payloads dropped immediately) [S1].
- `inter_agent_communication_metadata` — Multi-agent coordination metadata (non-turn record; counted and skipped cleanly) [S1].
- `world_state` — Internal environment/instruction state snapshot (non-turn record; counted and skipped cleanly) [S1].

## Field Reference (Every Used Field Cites a Source)

Children inside opaque state snapshots (`payload.state.*`) are skipped without retaining text.

| Field Path | JSON Type | Semantics | Source |
|---|---|---|---|
| `timestamp` | `string` | Top-level RFC 3339 / ISO-8601 UTC event timestamp. | [S1], [S2], [S4] |
| `type` | `string` | Top-level record discriminator (`"session_meta"`, `"turn_context"`, `"response_item"`, `"event_msg"`, `"token_usage_record"`, `"world_state"`). | [S1], [S2], [S4] |
| `ordinal` | `integer` | Monotonic zero-based record sequence index within the rollout file (`0.165.0`). | [S1] |
| `payload` | `object` | Record body interpreted according to top-level `type`. | [S1], [S2], [S4] |
| `payload.id` | `string` | Session/thread ID (on `session_meta`) or response item ID (on `response_item`). | [S1], [S2], [S4] |
| `payload.session_id` | `string` | Session UUID on `session_meta` and `token_usage_record`. | [S1], [S4] |
| `payload.timestamp` | `string` | Session start RFC 3339 timestamp on `session_meta`. | [S1], [S2] |
| `payload.cwd` | `string` | Working directory on `session_meta` and `turn_context` (hashed to `cwd_hash`; raw path dropped). | [S1], [S2], [S4] |
| `payload.originator` | `string` | Client identifier on `session_meta` (e.g. `"codex_cli_rs"`). | [S1], [S2] |
| `payload.cli_version` | `string` | Codex CLI version string on `session_meta`. | [S1], [S2] |
| `payload.model_provider` | `string` | Model provider slug on `session_meta` (e.g. `"openai"`). | [S1], [S2] |
| `payload.source` | `string \| object` | Session origin (`"cli"`, `"vscode"`, or `{"subagent": ...}`) on `session_meta` (`"subagent"` sets `TurnHints.sidechain = true`). | [S1], [S4] |
| `payload.parent_thread_id` | `string \| null` | Parent thread UUID when session was forked or spawned from another thread (`TurnRecord.parent_session_id`). | [S1], [S4] |
| `payload.forked_from_id` | `string \| null` | Explicit source thread UUID on forked sessions (`TurnRecord.parent_session_id`, sets `TurnHints.resume_start = true`). | [S1] |
| `payload.subagent_history_start_ordinal` | `integer` | Zero-based ordinal cutoff on `session_meta`; `response_item`s with `ordinal < subagent_history_start_ordinal` are inherited parent history. | [S1] |
| `payload.turn_id` | `string` | Logical turn identifier on `turn_context` and `token_usage_record`. | [S1], [S4] |
| `payload.root_turn_id` | `string` | Root turn identifier on `turn_context` and `token_usage_record`. | [S1] |
| `payload.model` | `string` | Active model identifier on `turn_context` (e.g. `"gpt-5-codex"`). | [S1], [S2], [S4] |
| `payload.effort` | `string` | Reasoning effort setting on `turn_context` (e.g. `"high"`). | [S1], [S4] |
| `payload.approval_policy` | `string` | Approval mode on `turn_context` (metadata, skipped). | [S1], [S2] |
| `payload.summary` | `string \| array` | Summary setting string on `turn_context` or summary array on `response_item/reasoning` (dropped). | [S1], [S2] |
| `payload.type` | `string` | Inner discriminator on `response_item` (`"message"`, `"reasoning"`, `"function_call"`, `"function_call_output"`, `"custom_tool_call"`, `"custom_tool_call_output"`) and `event_msg` (`"token_count"`). | [S1], [S2], [S4] |
| `payload.role` | `string` | Message role on `response_item/message` (`"user"`, `"assistant"`, `"developer"`). | [S1], [S2], [S4] |
| `payload.status` | `string` | Item completion status (`"completed"`, etc.) on `response_item`. | [S1] |
| `payload.content` | `array` | Typed content parts on `response_item/message`. | [S1], [S2], [S4] |
| `payload.content[].type` | `string` | Content part kind (`"input_text"` or `"output_text"`). | [S1], [S2], [S4] |
| `payload.content[].text` | `string` | Text body (byte-measured and dropped immediately). | [S1], [S2], [S4] |
| `payload.encrypted_content` | `string` | Encrypted reasoning token blob on `response_item/reasoning` (dropped immediately). | [S1], [S4] |
| `payload.call_id` | `string` | Tool call correlation ID linking `function_call`/`custom_tool_call` to its `*_output`. | [S1], [S2], [S4] |
| `payload.name` | `string` | Tool name on `function_call` / `custom_tool_call` (retained in `ContentBlock.tool`). | [S1], [S2], [S4] |
| `payload.arguments` | `string` | JSON-encoded arguments string on `function_call` (hashed with `name` into `key_hash`, measured, and dropped). | [S1], [S2], [S4] |
| `payload.input` | `string` | Raw tool input string on `custom_tool_call` (hashed with `name` into `key_hash`, measured, and dropped). | [S1] |
| `payload.output` | `string \| array` | Tool result string on `function_call_output` or content array on `custom_tool_call_output` (measured and dropped). | [S1], [S2], [S4] |
| `payload.output[].type` | `string` | `"input_text"` on `custom_tool_call_output` items. | [S1] |
| `payload.output[].text` | `string` | Tool result text on `custom_tool_call_output` items (measured and dropped). | [S1] |
| `payload.action` | `object` | Structured search query action on `response_item/web_search_call` (canonical JSON measured/hashed, text dropped). | [S1] |
| `payload.tools` | `array` | Discovered tool definitions on `response_item/tool_search_output` (canonical JSON measured, text dropped). | [S1] |
| `payload.author` | `string` | Sender agent role on `response_item/agent_message` (dropped). | [S1] |
| `payload.recipient` | `string` | Recipient agent role on `response_item/agent_message` (dropped). | [S1] |
| `payload.info` | `object \| null` | Token usage container on `event_msg` (`payload.type == "token_count"`). | [S1], [S2], [S3] |
| `payload.info.model_context_window` | `integer` | Agent-reported model context window size in tokens. | [S1], [S2], [S3] |
| `payload.info.last_token_usage` | `object` | Per-request token counts for the most recent API call on `event_msg/token_count`. | [S1], [S2], [S3] |
| `payload.info.last_token_usage.input_tokens` | `integer` | **Inclusive** total prompt tokens (`P = fresh_in + cache_read + cache_write`). | [S1], [S3], [S5] |
| `payload.info.last_token_usage.cached_input_tokens` | `integer` | Cached prompt tokens read (`cache_read`), **included inside `input_tokens`**. | [S1], [S3], [S5] |
| `payload.info.last_token_usage.cache_write_input_tokens` | `integer` | Cache-write input tokens (`cache_write`), **included inside `input_tokens`** (`0.165.0`). | [S1] |
| `payload.info.last_token_usage.output_tokens` | `integer` | Total output tokens for this call (includes `reasoning_output_tokens`). | [S1], [S3], [S5] |
| `payload.info.last_token_usage.reasoning_output_tokens` | `integer` | Reasoning token share within `output_tokens` (`Usage.reasoning`). | [S1], [S3], [S5] |
| `payload.info.last_token_usage.total_tokens` | `integer` | `input_tokens + output_tokens` (verified invariant across 139 local records). | [S1], [S3] |
| `payload.info.total_token_usage` | `object` | Cumulative session-wide token totals up to this event. | [S1], [S2], [S3] |
| `payload.info.total_token_usage.input_tokens` | `integer` | Cumulative inclusive input tokens. | [S1], [S3] |
| `payload.info.total_token_usage.cached_input_tokens` | `integer` | Cumulative cached input tokens. | [S1], [S3] |
| `payload.info.total_token_usage.cache_write_input_tokens` | `integer` | Cumulative cache-write input tokens (`0.165.0`). | [S1] |
| `payload.info.total_token_usage.output_tokens` | `integer` | Cumulative output tokens. | [S1], [S3] |
| `payload.info.total_token_usage.reasoning_output_tokens` | `integer` | Cumulative reasoning output tokens. | [S1], [S3] |
| `payload.info.total_token_usage.total_tokens` | `integer` | Cumulative `input_tokens + output_tokens`. | [S1], [S3] |
| `payload.rate_limits` | `object \| null` | Rate-limit telemetry attached to `event_msg/token_count`. | [S1], [S3] |
| `payload.rate_limits.limit_id` | `string` | Rate-limit bucket identifier. | [S1] |
| `payload.rate_limits.plan_type` | `string` | Plan tier label reported in rate-limit snapshot. | [S1], [S3] |
| `payload.rate_limits.primary` | `object \| null` | Primary quota window snapshot (drives Codex `quota-jump` flag). | [S1], [S3] |
| `payload.rate_limits.primary.used_percent` | `number` | Logged primary window utilisation percentage (`0.0..=100.0`). | [S1], [S3] |
| `payload.rate_limits.primary.window_minutes` | `integer` | Primary rate-limit window length in minutes. | [S1], [S3] |
| `payload.rate_limits.primary.resets_at` | `integer` | Unix epoch seconds when the primary window resets. | [S1], [S3] |
| `payload.rate_limits.secondary` | `object \| null` | Optional secondary rate-limit window snapshot. | [S1], [S3] |
| `payload.response_id` | `string` | Unique OpenAI API response ID on `token_usage_record` (primary deduplication key). | [S1], [S4] |
| `payload.thread_id` | `string` | Thread UUID on `token_usage_record`. | [S1] |
| `payload.usage` | `object` | Per-response token usage object on `token_usage_record`. | [S1], [S4] |
| `payload.usage.input_tokens` | `integer` | Inclusive prompt tokens (`P`) for this response. | [S1], [S4], [S5] |
| `payload.usage.cached_input_tokens` | `integer` | Cached input tokens (`cache_read`) inside `input_tokens`. | [S1], [S4], [S5] |
| `payload.usage.cache_write_input_tokens` | `integer` | Cache-write input tokens (`cache_write`) inside `input_tokens`. | [S1] |
| `payload.usage.output_tokens` | `integer` | Output tokens for this response. | [S1], [S4] |
| `payload.usage.reasoning_output_tokens` | `integer` | Reasoning output tokens inside `output_tokens`. | [S1], [S4] |
| `payload.usage.total_tokens` | `integer` | `input_tokens + output_tokens`. | [S1], [S4] |
| `payload.turn_token_usage` | `object` | Cumulative token usage within the current logical `turn_id` on `token_usage_record`. | [S1] |
| `payload.turn_token_usage.input_tokens` | `integer` | Turn-cumulative inclusive input tokens. | [S1] |
| `payload.turn_token_usage.cached_input_tokens` | `integer` | Turn-cumulative cached input tokens. | [S1] |
| `payload.turn_token_usage.cache_write_input_tokens` | `integer` | Turn-cumulative cache-write input tokens. | [S1] |
| `payload.turn_token_usage.output_tokens` | `integer` | Turn-cumulative output tokens. | [S1] |
| `payload.turn_token_usage.reasoning_output_tokens` | `integer` | Turn-cumulative reasoning output tokens. | [S1] |
| `payload.turn_token_usage.total_tokens` | `integer` | Turn-cumulative total tokens. | [S1] |
| `payload.thread_token_usage` | `object` | Cumulative thread-wide token usage on `token_usage_record`. | [S1] |
| `payload.thread_token_usage.input_tokens` | `integer` | Thread-cumulative inclusive input tokens. | [S1] |
| `payload.thread_token_usage.cached_input_tokens` | `integer` | Thread-cumulative cached input tokens. | [S1] |
| `payload.thread_token_usage.cache_write_input_tokens` | `integer` | Thread-cumulative cache-write input tokens. | [S1] |
| `payload.thread_token_usage.output_tokens` | `integer` | Thread-cumulative output tokens. | [S1] |
| `payload.thread_token_usage.reasoning_output_tokens` | `integer` | Thread-cumulative reasoning output tokens. | [S1] |
| `payload.thread_token_usage.total_tokens` | `integer` | Thread-cumulative total tokens. | [S1] |
| `payload.full` | `boolean` | Full-snapshot flag on non-turn `world_state` records (skipped). | [S1] |
| `payload.state` | `object` | Opaque internal environment snapshot on `world_state` records (skipped). | [S1] |

## Normalisation & Dual-Usage Precedence Rules

1. **Inclusive prompt accounting** ([S1], [S5] — verified across 139 local `0.165.0` records):
   - `input_tokens >= cached_input_tokens + cache_write_input_tokens`
   - `cache_read = cached_input_tokens`
   - `cache_write = cache_write_input_tokens` (defaults to `0` on older rollouts without the field)
   - `fresh_in = input_tokens - cache_read - cache_write` (saturating at `0` on malformed input)
   - Total prompt `P = fresh_in + cache_write + cache_read == input_tokens` (cached tokens are **never** added on top of `input_tokens`).
   - `output = output_tokens`, `reasoning = Some(reasoning_output_tokens)` (note: `reasoning_output_tokens <= output_tokens`, and `total_tokens == input_tokens + output_tokens`).
2. **Dual-usage deduplication (`codex-dual-usage`)** ([S1], [S3], [S4]):
   - `codex-cli 0.165.0` emits **both** `token_usage_record` (keyed by `response_id`, carrying `payload.usage`) and `event_msg` (`payload.type == "token_count"`, carrying `last_token_usage`, `total_token_usage`, `model_context_window`, and `rate_limits`) for every API call — observed in both `token_usage_record -> token_count` (`1,784` pairs) and `token_count -> token_usage_record` (`37` pairs) orders with zero intervening `response_item`s.
   - **Precedence rule:** When a rollout contains `token_usage_record`, use `token_usage_record` (deduplicated by `response_id`) as the authoritative token source and merge `rate_limits` / `model_context_window` from its paired adjacent `event_msg/token_count` — **never** sum or emit both!
   - **Fallback for older rollouts without `token_usage_record`:** Use `event_msg/token_count` (`last_token_usage` when present, suppressing consecutive duplicate `total_token_usage` snapshots; or per-field deltas of monotonic `total_token_usage` when `last_token_usage` is omitted).
3. **Fork & subagent inherited history** ([S1], [S4]):
   - Across `60` verified forked/subagent `0.165.0` rollouts (`forked_from_id` or `parent_thread_id` set), replayed parent `response_item`s appear prior to the child's first `turn_context` (and at `ordinal < subagent_history_start_ordinal` when present), with zero usage events before the first `turn_context`.
   - Parsers index tool `call_id`s from replayed parent items so cross-boundary tool outputs resolve tool names, but exclude replayed parent `response_item`s from the child's Turn 1 `added` blocks.
