# Claude Code Session Log Format

- **Agent:** Claude Code (`claude`)
- **Verified local version:** `2.1.314 (Claude Code)`
- **Checked date:** `2026-10-31`
- **Verification scope:** Structural field names and JSON value types only (no transcript values or personal paths recorded).

## Sources

- **[S1]** Local `Claude Code 2.1.314` install structural schema inspection (`checked: 2026-10-31`).
- **[S2]** Bough — Claude Code JSONL format reference: <https://www.bough.run/docs/format/claude-code> (`checked: 2026-10-31`).
- **[S3]** claude-dev.tools — Claude Code JSONL format notes: <https://claude-dev.tools/docs/jsonl-format> (`checked: 2026-10-31`).
- **[S4]** FullFran/claudeops-tui — JSONL format documentation: <https://github.com/FullFran/claudeops-tui/blob/main/docs/jsonl-format.md> (`checked: 2026-10-31`).
- **[S5]** ryoppippi/ccusage #938 — Streaming duplicate `assistant` lines & partial `output_tokens` trap: <https://github.com/ryoppippi/ccusage/issues/938> (`checked: 2026-10-31`).
- **[S6]** AgentMinutes — Anthropic vs OpenAI token accounting comparison: <https://agentminutes.dev/docs/token-comparison/> (`checked: 2026-10-31`).

## Discovery & File Layout

| Location / Variable | Role | Source |
|---|---|---|
| `~/.claude/projects/<cwd-slug>/<session>.jsonl` | Primary top-level session transcript (UTF-8 NDJSON, one JSON object per line). `<cwd-slug>` replaces `/` with `-` (e.g. `-synthetic-workspace-alpha`). | [S1], [S2], [S3] |
| `~/.claude/projects/<cwd-slug>/<session>/subagents/*.jsonl` | Child subagent transcripts scoped to `<session>` (`isSidechain: true`). Top-level discovery must not blindly flatten these into parent sessions without attribution. | [S1], [S2] |
| `CLAUDE_CONFIG_DIR` | Optional environment override replacing `~/.claude` as the root configuration/projects directory. | [S1], [S2] |

## Observed Top-Level `type` Values (`2.1.314`)

Primary turn/content records:
- `user` — User prompt text, image paste metadata, or `tool_result` responses [S1], [S2], [S4].
- `assistant` — Model API response chunk(s) carrying `message.usage` and content blocks (`thinking`, `text`, `tool_use`) [S1], [S2], [S4].
- `system` — Lifecycle and boundary events (`subtype` values observed: `turn_duration`, `stop_hook_summary`, `away_summary`, `bridge_status`; plus documented `compact_boundary`, `api_error`) [S1], [S2], [S4].

Non-turn metadata/state records observed on `2.1.314` (counted and skipped cleanly by Kakeibo decoders, never a crash per `PLAN.md` §0 #6) [S1]:
- `agent-name`, `ai-title`, `api-request`, `api-request-blob`, `api-request-shape`, `atis-latch`, `attachment`, `bridge-session`, `cost-state`, `file-history-delta`, `file-history-snapshot`, `isolation-latch`, `last-prompt`, `mode`, `permission-mode`, `queue-operation`, `tools-used`, `summary`.

## Field Reference (Every Used Field Cites a Source)

Children inside opaque tool-argument maps (`message.content[].input.*`) and file backup maps (`snapshot.trackedFileBackups.*`) are tool-specific arbitrary JSON objects; parsers hash/measure their byte lengths and drop text immediately.

| Field Path | JSON Type | Semantics | Source |
|---|---|---|---|
| `type` | `string` | Top-level record discriminator (`"user"`, `"assistant"`, `"system"`, `"file-history-snapshot"`, etc.). | [S1], [S2], [S3] |
| `uuid` | `string` | Unique identifier for this JSONL event line. | [S1], [S2], [S4] |
| `parentUuid` | `string \| null` | Parent event `uuid` forming the conversation DAG (`null` at root or reset boundary). | [S1], [S2], [S4] |
| `sessionId` | `string` | Session UUID shared across lines in a session. | [S1], [S2], [S4] |
| `timestamp` | `string` | RFC 3339 / ISO-8601 UTC event timestamp. | [S1], [S2], [S4] |
| `cwd` | `string` | Working directory at the time of the event (hashed to `cwd_hash` at parse time; raw path dropped). | [S1], [S2], [S4] |
| `version` | `string` | Claude Code CLI version string that wrote the record. | [S1], [S2], [S4] |
| `isSidechain` | `boolean` | `true` for subagent / sidechain turns. | [S1], [S2], [S4] |
| `gitBranch` | `string` | Current Git branch recorded by Claude Code (dropped at parse time). | [S1], [S4] |
| `userType` | `string` | User classification marker (e.g. `"external"`). | [S1], [S4] |
| `isMeta` | `boolean` | `true` on synthetic/injected `user` or `system` meta lines. | [S1], [S4] |
| `sourceToolUseID` | `string` | Optional link on `user` tool-result records back to the originating `tool_use` ID. | [S1] |
| `requestId` | `string` | Anthropic API request identifier on `assistant` records; primary streaming deduplication key alongside `message.id`. | [S1], [S2], [S5] |
| `subtype` | `string` | Subtype discriminator on `system` records (e.g. `"turn_duration"`, `"compact_boundary"`). | [S1], [S2], [S4] |
| `durationMs` | `integer` | Turn wall-clock duration in milliseconds on `system` (`subtype == "turn_duration"`) records. | [S1] |
| `messageId` | `string` | Target message UUID on `file-history-snapshot` non-turn records. | [S1], [S3] |
| `isSnapshotUpdate` | `boolean` | Snapshot update flag on `file-history-snapshot` records. | [S1], [S3] |
| `snapshot` | `object` | File-backup state payload on `file-history-snapshot` records (skipped). | [S1], [S3] |
| `snapshot.messageId` | `string` | Message UUID associated with the file-history snapshot. | [S1], [S3] |
| `snapshot.timestamp` | `string` | RFC 3339 timestamp of the snapshot. | [S1], [S3] |
| `snapshot.trackedFileBackups` | `object` | Opaque map of tracked file backup descriptors (skipped). | [S1], [S3] |
| `message` | `object` | Anthropic Messages API payload embedded in `user` and `assistant` records. | [S1], [S2], [S4] |
| `message.id` | `string` | Anthropic message ID (e.g. `msg_...`) on `assistant` records; shared across streaming lines of one API response. | [S1], [S2], [S5] |
| `message.type` | `string` | `"message"` on `assistant` records. | [S1], [S4] |
| `message.role` | `string` | `"user"` or `"assistant"`. | [S1], [S2], [S4] |
| `message.model` | `string` | Model identifier used for the request (e.g. `"claude-sonnet-4-5"`). | [S1], [S2], [S4] |
| `message.stop_reason` | `string \| null` | Stop reason (`"end_turn"`, `"tool_use"`, `"max_tokens"`, or `null` on intermediate streaming lines). | [S1], [S4], [S5] |
| `message.stop_sequence` | `string \| null` | Stop sequence if matched, otherwise `null`. | [S1], [S4] |
| `message.content` | `string \| array` | Either a raw prompt string (on plain `user` lines) or an array of typed content blocks. | [S1], [S2], [S4] |
| `message.content[].type` | `string` | Block kind: `"text"`, `"thinking"`, `"tool_use"`, `"tool_result"`, or `"image"`. | [S1], [S2], [S4] |
| `message.content[].text` | `string` | Text payload of a `"text"` block (byte-measured and dropped immediately). | [S1], [S2], [S4] |
| `message.content[].thinking` | `string` | Thinking text payload of a `"thinking"` block (byte-measured and dropped immediately). | [S1], [S4] |
| `message.content[].signature` | `string` | Opaque cryptographic thinking block signature (dropped). | [S1], [S4] |
| `message.content[].id` | `string` | Tool invocation identifier (e.g. `toolu_...`) on `"tool_use"` blocks. | [S1], [S2], [S4] |
| `message.content[].name` | `string` | Tool name (e.g. `"Read"`, `"Bash"`) retained in `ContentBlock.tool`. | [S1], [S2], [S4] |
| `message.content[].input` | `object` | Tool call arguments JSON object; canonicalised with `name` into `key_hash = sha256(tool + canonical_args)`, measured, and dropped. | [S1], [S2], [S4] |
| `message.content[].tool_use_id` | `string` | Matching `tool_use` ID referenced by a `"tool_result"` block in a `user` record. | [S1], [S2], [S4] |
| `message.content[].is_error` | `boolean` | `true` when the tool execution failed (used for retry detection). | [S1], [S4] |
| `message.content[].content` | `string \| array` | Tool output body on `"tool_result"` blocks (string or nested content array; measured and dropped). | [S1], [S2], [S4] |
| `message.content[].content[].type` | `string` | Nested tool result block type (`"text"` or `"tool_reference"`). | [S1], [S4] |
| `message.content[].content[].text` | `string` | Nested tool result text payload (measured and dropped). | [S1], [S4] |
| `message.usage` | `object` | Anthropic token usage object on `assistant` records. | [S1], [S2], [S6] |
| `message.usage.input_tokens` | `integer` | **Disjoint** uncached fresh input tokens (`fresh_in`). Does **not** include cache write or cache read tokens. | [S1], [S2], [S6] |
| `message.usage.cache_creation_input_tokens` | `integer` | Total tokens written into prompt cache on this request (`cache_write`). | [S1], [S2], [S6] |
| `message.usage.cache_creation` | `object` | Breakdown of `cache_creation_input_tokens` by cache TTL tier. | [S1], [S2] |
| `message.usage.cache_creation.ephemeral_5m_input_tokens` | `integer` | 5-minute TTL cache-creation input tokens. | [S1], [S2] |
| `message.usage.cache_creation.ephemeral_1h_input_tokens` | `integer` | 1-hour TTL cache-creation input tokens. | [S1], [S2] |
| `message.usage.cache_read_input_tokens` | `integer` | Tokens read from prompt cache on this request (`cache_read`). | [S1], [S2], [S6] |
| `message.usage.output_tokens` | `integer` | Output tokens generated so far in this API response (partial on intermediate streaming lines; final on completed line). | [S1], [S5], [S6] |
| `message.usage.output_tokens_details` | `object` | Optional breakdown of `output_tokens` on newer Claude Code `2.1.x` releases. | [S1] |
| `message.usage.output_tokens_details.thinking_tokens` | `integer` | Optional thinking token share inside `output_tokens` (`Usage.reasoning`). | [S1] |
| `message.usage.service_tier` | `string` | Anthropic service tier label (e.g. `"standard"`). | [S1], [S4] |

## Normalisation & Deduplication Rules

1. **Disjoint prompt accounting** ([S1], [S6]):
   - `fresh_in = input_tokens`
   - `cache_write = cache_creation_input_tokens`
   - `cache_read = cache_read_input_tokens`
   - Total prompt `P = fresh_in + cache_write + cache_read`.
2. **Streaming duplicate consolidation** ([S1], [S5]):
   - Claude Code writes one `assistant` JSONL line per streamed content block (`thinking`, `text`, `tool_use`) for a single API call; all lines for that call share `(message.id, requestId)`.
   - Intermediate chunks may report partial `output_tokens` (`ccusage#938` [S5]). Decoders group by `(message.id, requestId)` (falling back to `message.id` or `requestId` if only one is present), keep **`max(output_tokens)`** (never first-seen and never the sum), and merge distinct content blocks in order.
