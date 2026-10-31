# Gemini CLI Session Transcript Format

- **Agent:** Google Gemini CLI (`gemini-cli`)
- **Verified local version:** `@google/gemini-cli 0.41.2`
- **Checked date:** `2026-10-31`
- **Verification scope:** Structural TypeScript/JS bundle types (`chatRecordingService` / `chatRecordingTypes` / `ProjectRegistry`) and boolean algebraic invariants only (no user transcripts inspected or recorded).

## Sources

- **[S1]** Local `@google/gemini-cli 0.41.2` bundle structural schema inspection (`ProjectRegistry`, `ChatRecordingService`, `loadConversationRecord`, `rewindTo`, `TokensSummary`, `ToolCallRecord`, `ThoughtSummary`, `checked: 2026-10-31`).
- **[S2]** google-gemini/gemini-cli — Session Management & Chat Recording docs and source (`packages/core/src/services/chatRecordingService.ts`, `chatRecordingTypes.ts`): <https://github.com/google-gemini/gemini-cli> (`checked: 2026-10-31`).
- **[S3]** Sean Whalen — *Where Gemini CLI Stores Session History* (`projects.json`, `.project_root`, `tmp/<slug>/chats/session-*.jsonl` and legacy `.json`): <https://seanthegeek.net/2402/where-gemini-cli-stores-session-history/> (`checked: 2026-10-31`).
- **[S4]** Google GenAI SDK `GenerateContentResponseUsageMetadata` (`promptTokenCount`, `candidatesTokenCount`, `cachedContentTokenCount`, `thoughtsTokenCount`, `toolUsePromptTokenCount`, `totalTokenCount`): <https://ai.google.dev/api/generate-content#UsageMetadata> (`checked: 2026-10-31`).

## Discovery & File Layout

| Location / Variable | Role | Source |
|---|---|---|
| `~/.gemini/projects.json` | Maps absolute project root paths to `<slug>` directory names (`{"projects": {"<abs_path>": "<slug>"}}`). | [S1], [S2], [S3] |
| `~/.gemini/tmp/<slug>/.project_root` | Per-slug plaintext marker containing the absolute project root path (`cwd`). | [S1], [S2], [S3] |
| `~/.gemini/tmp/<slug>/chats/session-*.jsonl` | Active append-only NDJSON session transcripts (`0.41.2+` default). | [S1], [S2], [S3] |
| `~/.gemini/tmp/<slug>/chats/session-*.json` | Legacy single-object `ConversationRecord` JSON session files; migrated to `<same-stem>.jsonl` when resumed in `0.41.2+`. | [S1], [S2], [S3] |
| `~/.gemini/tmp/<slug>/chats/<parent_session_id>/*.jsonl` | Subagent transcripts (`kind: "subagent"`) nested under their parent `sessionId` directory (also `.json` on older builds). | [S1], [S2] |
| `GEMINI_CLI_HOME` | Optional home override (`$GEMINI_CLI_HOME/.gemini`, or direct `.gemini` root if `tmp/` / `projects.json` lives directly inside it). | [S1], [S2] |

## JSONL State Reconstruction (`loadConversationRecord`)

Gemini CLI `0.41.2` records append-only NDJSON lines that reconstruct a logical `ConversationRecord` before turn extraction [S1], [S2]:

1. **Metadata header line:** `{ sessionId, projectHash, startTime, lastUpdated, kind?, directories?, summary? }` initializes session metadata (`ignored_records += 1`).
2. **`$set` metadata update:** `{ "$set": { "lastUpdated": "...", "summary": "..." } }` merges top-level session metadata (`ignored_records += 1`).
3. **`MessageRecord` insertion & same-ID upsert:** Lines carrying `type` (`"user"`, `"gemini"`, `"info"`, `"warning"`, `"error"`) and `id` insert into an ordered message list, or **replace the existing message with the same `id` in place** (preserving original insertion position when `tokens` or `toolCalls[].result` arrive) [S1].
4. **`$patch` partial message update:** Supports both RFC-6902 JSON Patch arrays (`{ "id": "<msg_id>", "$patch": [{ "op": "add" | "replace" | "remove", "path": "/...", "value": ... }] }`) and object-merge patches (`{ "id": "<msg_id>", "$patch": { ... } }` or `{ "$patch": { "id": "<msg_id>", ... } }`) targeting `id` (or the latest message when `id` is omitted) [S1], [S2].
5. **`$rewindTo` truncation (`rewindTo(messageId)`):** `{ "$rewindTo": "<message_id>" }` rewinds the conversation to the state **immediately before `<message_id>`** — removing the message with `id == <message_id>` **and all messages inserted after it** (or clearing all prior messages if `<message_id>` is not found in the active list) (`ignored_records += 1`) [S1], [S2].

To uphold Kakeibo's zero-text-retention invariant (`PLAN.md` §0 #4), every message line or `$patch` value is reduced immediately to byte lengths, tool names, and 16-hex SHA-256 `key_hash` / `cwd_hash` prefixes; raw prompt, thought, assistant, and tool-output strings are never stored across lines.

## Field Reference (Every Used Field Cites a Source)

Children inside dynamic slug-registry path keys (`projects.*`) and arbitrary tool call argument objects (`toolCalls[].args.*`, `messages[].toolCalls[].args.*`, `$patch.toolCalls[].args.*`) are hashed/measured without retaining keys or text.

| Field Path | JSON Type | Semantics | Source |
|---|---|---|---|
| `projects` | `object` | Map of `<absolute_project_path> -> <slug>` in `~/.gemini/projects.json` (used solely to hash `cwd` into `cwd_hash`, or resolved on explicit `export --show-paths`). | [S1], [S2], [S3] |
| `sessionId` | `string` | Unique session UUID on JSONL header or legacy `.json` `ConversationRecord`. | [S1], [S2], [S3] |
| `projectHash` | `string` | 64-hex SHA-256 hash of the canonical project root directory. | [S1], [S2], [S3] |
| `startTime` | `string` | Session start RFC 3339 / ISO-8601 UTC timestamp. | [S1], [S2] |
| `lastUpdated` | `string` | Most recent update RFC 3339 / ISO-8601 UTC timestamp. | [S1], [S2] |
| `kind` | `string` | Transcript kind (`"main"` or `"subagent"`; `"subagent"` sets `TurnHints.sidechain = true`). | [S1], [S2] |
| `directories` | `array` | Optional workspace root path array on metadata header (first entry hashed into `cwd_hash` when slug registry is absent; raw paths dropped). | [S1] |
| `summary` | `string` | Optional conversation summary string on metadata header or legacy `.json` (dropped immediately). | [S1], [S2] |
| `$set` | `object` | Top-level metadata merge envelope in JSONL transcripts. | [S1], [S2] |
| `$set.lastUpdated` | `string` | Updated RFC 3339 timestamp inside `$set`. | [S1], [S2] |
| `$set.summary` | `string` | Updated conversation summary inside `$set` (dropped immediately). | [S1], [S2] |
| `$set.directories` | `array` | Updated workspace directories array inside `$set` (hashed for `cwd_hash`, raw paths dropped). | [S1] |
| `$rewindTo` | `string` | Target `MessageRecord.id`; removes that message and all subsequent messages. | [S1], [S2] |
| `$patch` | `array \| object` | Partial update applied to an existing `MessageRecord` (RFC-6902 patch array or partial fields object). | [S1], [S2] |
| `$patch[].op` | `string` | RFC-6902 operation (`"add"`, `"replace"`, `"remove"`). | [S2] |
| `$patch[].path` | `string` | RFC-6902 JSON Pointer target path (e.g. `"/toolCalls/0/result"`, `"/toolCalls/0/status"`, `"/tokens"`). | [S2] |
| `$patch[].value` | `any` | Replacement/added value for RFC-6902 patch operation (measured and dropped immediately). | [S2] |
| `$patch[].value[].functionResponse` | `object` | Google GenAI `FunctionResponse` part wrapper inside a patched tool result array. | [S1], [S2] |
| `$patch[].value[].functionResponse.id` | `string` | Tool call correlation ID on `FunctionResponse`. | [S1], [S2] |
| `$patch[].value[].functionResponse.name` | `string` | Tool name on `FunctionResponse`. | [S1], [S2] |
| `$patch[].value[].functionResponse.response` | `object` | Structured tool response payload on `FunctionResponse`. | [S1], [S2] |
| `$patch[].value[].functionResponse.response.output` | `string` | Tool stdout/text output string (UTF-8 byte-measured and dropped immediately). | [S1], [S2] |
| `$patch.id` | `string` | Optional target message ID inside object-form `$patch`. | [S2] |
| `$patch.content` | `string \| array` | Patched message content (measured and dropped immediately). | [S1], [S2] |
| `$patch.tokens` | `object` | Patched `TokensSummary` object on a `"gemini"` message. | [S1], [S2] |
| `$patch.tokens.input` | `integer` | Patched `promptTokenCount`. | [S1], [S4] |
| `$patch.tokens.output` | `integer` | Patched `candidatesTokenCount`. | [S1], [S4] |
| `$patch.tokens.cached` | `integer` | Patched `cachedContentTokenCount` (inclusive inside `input + tool`). | [S1], [S4] |
| `$patch.tokens.thoughts` | `integer` | Patched `thoughtsTokenCount` (disjoint from `output`). | [S1], [S4] |
| `$patch.tokens.tool` | `integer` | Patched `toolUsePromptTokenCount`. | [S1], [S4] |
| `$patch.tokens.total` | `integer` | Patched `totalTokenCount`. | [S1], [S4] |
| `$patch.toolCalls` | `array` | Patched `ToolCallRecord` array on a `"gemini"` message. | [S1], [S2] |
| `$patch.toolCalls[].id` | `string` | Tool call ID inside object-form `$patch`. | [S1], [S2] |
| `$patch.toolCalls[].name` | `string` | Tool name inside object-form `$patch`. | [S1], [S2] |
| `$patch.toolCalls[].args` | `object` | Tool arguments inside object-form `$patch` (measured/hashed and dropped). | [S1], [S2] |
| `$patch.toolCalls[].result` | `array \| object \| string` | Tool execution output inside object-form `$patch` (measured and dropped). | [S1], [S2] |
| `$patch.toolCalls[].resultDisplay` | `string \| object` | Display output inside object-form `$patch` (fallback measurement, dropped). | [S1], [S2] |
| `$patch.toolCalls[].status` | `string` | Tool execution status inside object-form `$patch`. | [S1], [S2] |
| `id` | `string` | Unique message identifier within the session (`MessageRecord.id` or `$patch` target). | [S1], [S2] |
| `timestamp` | `string` | Message RFC 3339 / ISO-8601 UTC timestamp. | [S1], [S2] |
| `type` | `string` | Message discriminator (`"user"`, `"gemini"`, `"info"`, `"warning"`, `"error"`). | [S1], [S2] |
| `content` | `string \| array` | Message content (`PartListUnion` string or part array; UTF-8 byte-measured and dropped immediately). | [S1], [S2] |
| `content[].text` | `string` | Text segment inside `PartListUnion` array `content` (byte-measured and dropped). | [S1], [S2] |
| `model` | `string` | Gemini model slug on `type == "gemini"` messages (e.g. `"gemini-2.5-pro"`). | [S1], [S2] |
| `thoughts` | `array` | Array of `ThoughtSummary` items on `type == "gemini"` messages. | [S1], [S2] |
| `thoughts[].subject` | `string` | Short thought heading (`bytes = len(subject) + len(description)` for `AssistantThinking`; text dropped). | [S1], [S2] |
| `thoughts[].description` | `string` | Thought summary body (byte-measured and dropped immediately). | [S1], [S2] |
| `thoughts[].timestamp` | `string` | Thought timestamp metadata. | [S1], [S2] |
| `tokens` | `object` | Per-API-call `TokensSummary` attached to `type == "gemini"` messages. | [S1], [S2], [S4] |
| `tokens.input` | `integer` | `promptTokenCount` — **includes** `tokens.cached`. | [S1], [S4] |
| `tokens.output` | `integer` | `candidatesTokenCount` — **excludes** `tokens.thoughts`. | [S1], [S4] |
| `tokens.cached` | `integer` | `cachedContentTokenCount` — cached prompt tokens read (`cache_read`), **inclusive inside `input + tool`**. | [S1], [S4] |
| `tokens.thoughts` | `integer` | `thoughtsTokenCount` — thinking tokens generated (added to `Usage.output` and stored in `Usage.reasoning`). | [S1], [S4] |
| `tokens.tool` | `integer` | `toolUsePromptTokenCount` — tool-use prompt tokens billed alongside `tokens.input` (`P = input + tool`). | [S1], [S4] |
| `tokens.total` | `integer` | `totalTokenCount == (input + tool) + (output + thoughts)`. | [S1], [S4] |
| `toolCalls` | `array` | `ToolCallRecord` invocations emitted by a `type == "gemini"` message. | [S1], [S2] |
| `toolCalls[].id` | `string` | Tool call identifier. | [S1], [S2] |
| `toolCalls[].name` | `string` | Canonical tool identifier (retained in `ContentBlock.tool`). | [S1], [S2] |
| `toolCalls[].displayName` | `string` | Human-friendly UI label for the tool (dropped). | [S1], [S2] |
| `toolCalls[].args` | `object` | Tool input object (canonical JSON byte-measured and hashed into `key_hash`; dropped). | [S1], [S2] |
| `toolCalls[].result` | `array \| object \| string` | `PartListUnion` tool output fed into the next `"gemini"` turn's prompt (`BlockKind::ToolResult`; dropped). | [S1], [S2] |
| `toolCalls[].result[].functionResponse` | `object` | Google GenAI `FunctionResponse` part wrapper in `toolCalls[].result`. | [S1], [S2] |
| `toolCalls[].result[].functionResponse.id` | `string` | Tool call correlation ID on `FunctionResponse`. | [S1], [S2] |
| `toolCalls[].result[].functionResponse.name` | `string` | Tool name on `FunctionResponse`. | [S1], [S2] |
| `toolCalls[].result[].functionResponse.response` | `object` | Tool response payload object on `FunctionResponse`. | [S1], [S2] |
| `toolCalls[].result[].functionResponse.response.output` | `string` | Tool stdout/text output string (byte-measured and dropped). | [S1], [S2] |
| `toolCalls[].resultDisplay` | `string \| object` | Formatted display output (used only as byte-length fallback if `result` is absent; dropped). | [S1], [S2] |
| `toolCalls[].status` | `string` | Tool execution status (`"executing"`, `"success"`, `"error"`, `"cancelled"`). | [S1], [S2] |
| `messages` | `array` | Ordered `MessageRecord` list on legacy single-object `session-*.json` files. | [S1], [S2], [S3] |
| `messages[].id` | `string` | Message ID inside legacy `.json` `messages[]`. | [S1], [S2] |
| `messages[].timestamp` | `string` | Message RFC 3339 timestamp inside legacy `.json` `messages[]`. | [S1], [S2] |
| `messages[].type` | `string` | Message type (`"user"`, `"gemini"`, `"info"`, `"warning"`, `"error"`) inside legacy `.json`. | [S1], [S2] |
| `messages[].content` | `string \| array` | Message content inside legacy `.json` (measured and dropped). | [S1], [S2] |
| `messages[].content[].text` | `string` | Text part inside legacy `.json` `messages[].content` array (measured and dropped). | [S1], [S2] |
| `messages[].model` | `string` | Gemini model slug inside legacy `.json`. | [S1], [S2] |
| `messages[].thoughts` | `array` | `ThoughtSummary` array inside legacy `.json`. | [S1], [S2] |
| `messages[].thoughts[].subject` | `string` | Thought subject inside legacy `.json` (measured and dropped). | [S1], [S2] |
| `messages[].thoughts[].description` | `string` | Thought description inside legacy `.json` (measured and dropped). | [S1], [S2] |
| `messages[].thoughts[].timestamp` | `string` | Thought timestamp inside legacy `.json`. | [S1], [S2] |
| `messages[].tokens` | `object` | `TokensSummary` object inside legacy `.json`. | [S1], [S2], [S4] |
| `messages[].tokens.input` | `integer` | Inclusive `promptTokenCount` inside legacy `.json`. | [S1], [S4] |
| `messages[].tokens.output` | `integer` | `candidatesTokenCount` inside legacy `.json`. | [S1], [S4] |
| `messages[].tokens.cached` | `integer` | Inclusive `cachedContentTokenCount` inside legacy `.json`. | [S1], [S4] |
| `messages[].tokens.thoughts` | `integer` | `thoughtsTokenCount` inside legacy `.json`. | [S1], [S4] |
| `messages[].tokens.tool` | `integer` | `toolUsePromptTokenCount` inside legacy `.json`. | [S1], [S4] |
| `messages[].tokens.total` | `integer` | `totalTokenCount` inside legacy `.json`. | [S1], [S4] |
| `messages[].toolCalls` | `array` | `ToolCallRecord` array inside legacy `.json`. | [S1], [S2] |
| `messages[].toolCalls[].id` | `string` | Tool call ID inside legacy `.json`. | [S1], [S2] |
| `messages[].toolCalls[].name` | `string` | Tool name inside legacy `.json`. | [S1], [S2] |
| `messages[].toolCalls[].displayName` | `string` | Tool display name inside legacy `.json` (dropped). | [S1], [S2] |
| `messages[].toolCalls[].args` | `object` | Tool arguments inside legacy `.json` (measured/hashed and dropped). | [S1], [S2] |
| `messages[].toolCalls[].result` | `array \| object \| string` | Tool execution output inside legacy `.json` (measured and dropped). | [S1], [S2] |
| `messages[].toolCalls[].result[].functionResponse` | `object` | `FunctionResponse` wrapper inside legacy `.json`. | [S1], [S2] |
| `messages[].toolCalls[].result[].functionResponse.id` | `string` | Tool call ID on `FunctionResponse` inside legacy `.json`. | [S1], [S2] |
| `messages[].toolCalls[].result[].functionResponse.name` | `string` | Tool name on `FunctionResponse` inside legacy `.json`. | [S1], [S2] |
| `messages[].toolCalls[].result[].functionResponse.response` | `object` | Response payload on `FunctionResponse` inside legacy `.json`. | [S1], [S2] |
| `messages[].toolCalls[].result[].functionResponse.response.output` | `string` | Tool output text inside legacy `.json` (measured and dropped). | [S1], [S2] |
| `messages[].toolCalls[].resultDisplay` | `string \| object` | Display output inside legacy `.json` (fallback measurement, dropped). | [S1], [S2] |
| `messages[].toolCalls[].status` | `string` | Tool status inside legacy `.json`. | [S1], [S2] |

## Normalisation & Attribution Block Ordering

1. **Inclusive prompt + disjoint thoughts normalisation** ([S1], [S4]):
   - Total prompt tokens `P = tokens.input + tokens.tool`.
   - `tokens.cached` is **included inside** `P` (`cache_read = min(tokens.cached, P)`, `cache_write = 0`, `fresh_in = P - cache_read`), normalised via `Usage::from_inclusive_clamped`.
   - `tokens.thoughts` is **disjoint from** `tokens.output` (`candidatesTokenCount`): `Usage.output = tokens.output + tokens.thoughts`, `Usage.reasoning = (tokens.thoughts > 0).then_some(tokens.thoughts)`.
   - Thus `P + Usage.output == (input + tool) + (output + thoughts) == tokens.total`.
2. **Causal tool-result placement (`PLAN.md` §6.1)**:
   - On `"gemini"` turn `t`, `added` blocks include any pending `ToolResult` blocks produced by turn `t - 1`'s `toolCalls`, any `"user"` prompt blocks preceding turn `t`, plus turn `t`'s `thoughts` (`AssistantThinking`), `content` (`AssistantText`), and `toolCalls[].args` (`ToolUse`).
   - Turn `t`'s `toolCalls[].result` outputs enter the model's prompt on turn `t + 1`, so they are queued as `BlockKind::ToolResult` for turn `t + 1`.
3. **Migration deduplication (`.jsonl` vs `.json`)** ([S1]):
   - Resuming a legacy `session-<stem>.json` in `0.41.2+` writes `session-<stem>.jsonl` alongside the legacy `.json`. Discovery prefers `<stem>.jsonl` whenever both extensions share the same directory and stem so migrated sessions are never double-counted.
