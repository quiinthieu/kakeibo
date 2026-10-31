# Cursor Global State SQLite Format (`state.vscdb` — Best Effort)

- **Agent:** Cursor (`cursor`, Anysphere Cursor IDE)
- **Format generation:** VS Code / Electron global storage SQLite database (`state.vscdb`, `cursorDiskKV` table with `composerData:<composerId>` and `bubbleId:<composerId>:<bubbleId>` entries)
- **Checked date:** `2026-10-31`
- **Verification scope:** Published community reverse-engineering specifications of `cursorDiskKV` (`composerData:*` and `bubbleId:*`) and Cursor Forum reports on zeroed local `tokenCount` fields. Because Cursor's local SQLite schema is undocumented and unversioned by Anysphere, support is explicitly **best effort** (`PLAN.md` §5, §10 T11).

## Sources

- **[S1]** Session-Viz Engineering Guide — *Reading Cursor's Chat History* (`state.vscdb` platform paths, `cursorDiskKV` table, `composerData:<composerId>`, `fullConversationHeadersOnly`, `bubbleId:<composerId>:<bubbleId>`, `type` `1`/`2`, `toolFormerData`, `thinking`, `tokenCount.{inputTokens,outputTokens}`): <https://www.session-viz.com/blog/reading-cursors-chat-history/> (`checked: 2026-10-31`).
- **[S2]** Cursor Community Forum `#155984` — Local `state.vscdb` `bubbleId:*` `tokenCount: { "inputTokens": 0, "outputTokens": 0 }` behaviour on server-counted Cursor versions: <https://forum.cursor.com/t/where-are-chat-token-counts-stored-locally/155984> (`checked: 2026-10-31`).
- **[S3]** Tarek-Rolland/cursor-chat-export & SQLite `ItemTable` / `cursorDiskKV` layout reference (`key TEXT PRIMARY KEY, value TEXT | BLOB` UTF-8 JSON payloads): <https://github.com/Tarek-Rolland/cursor-chat-export> (`checked: 2026-10-31`).

## Discovery & File Layout

| Location / Variable | Role | Source |
|---|---|---|
| `~/Library/Application Support/Cursor/User/globalStorage/state.vscdb` | macOS Cursor global storage SQLite database containing `cursorDiskKV`. | [S1], [S3] |
| `~/.config/Cursor/User/globalStorage/state.vscdb` | Linux Cursor global storage SQLite database (`${XDG_CONFIG_HOME:-~/.config}/Cursor/User/globalStorage/state.vscdb`). | [S1], [S3] |
| `%APPDATA%/Cursor/User/globalStorage/state.vscdb` | Windows Cursor global storage SQLite database. | [S1], [S3] |
| `CURSOR_STATE_DB` | Optional explicit file path override pointing directly to a `state.vscdb` file. | [S1] |
| `CURSOR_GLOBAL_STORAGE_DIR` | Optional directory override containing `state.vscdb`. | [S1] |

## Read-Only SQLite Access

1. **Strictly read-only WAL-compatible SQLite access (`PLAN.md` §0 #3, §5, §10 T11):**
   - `state.vscdb` is opened via `kakeibo-parse::sqlite::open_sqlite_readonly` using `file:<percent-encoded-path>?mode=ro` (`SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX | SQLITE_OPEN_URI`), `busy_timeout = 2500ms`, and `PRAGMA query_only = ON`, falling back to `mode=ro&immutable=1` only when `mode=ro` fails with `SQLITE_CANTOPEN` or `SQLITE_READONLY` (e.g. a `0555` read-only parent directory without a `-wal` sidecar).
   - Kakeibo never modifies `state.vscdb` bytes (SHA-256) or `mtime`, even while Cursor holds an active WAL write transaction.
2. **`cursorDiskKV` table schema** ([S1], [S3]):
   - Table `cursorDiskKV (key TEXT PRIMARY KEY, value TEXT | BLOB)` stores UTF-8 JSON values keyed by prefix:
     - `composerData:<composerId>`: composer thread metadata and ordered bubble headers.
     - `bubbleId:<composerId>:<bubbleId>`: individual user (`type == 1`) and assistant/agent (`type == 2`) bubbles.
     - Unrelated IDE keys in `cursorDiskKV` (e.g. `workbench.panel.*`, `inlineDiffs:*`) are skipped and counted in `ParseDiagnostics.ignored_records`.

## Field Reference (Every Used Field Cites a Source)

Children inside arbitrary tool parameter objects (`toolFormerData.params.*`) are canonical-JSON measured/hashed and dropped immediately without retaining raw keys or text.

| Field Path | JSON Type | Semantics | Source |
|---|---|---|---|
| `_v` | `integer` | Internal Cursor schema version tag on `composerData:*` / `bubbleId:*` JSON objects. | [S1], [S3] |
| `composerId` | `string` | Composer thread identifier (`TurnRecord.session_id`). | [S1], [S3] |
| `createdAt` | `integer \| string` | Creation timestamp in Unix epoch milliseconds (or RFC 3339 string) on `composerData:*` and `bubbleId:*`. | [S1], [S3] |
| `lastUpdatedAt` | `integer \| string` | Last-updated timestamp in Unix epoch milliseconds on `composerData:*`. | [S1] |
| `name` | `string` | Composer thread title on `composerData:*` (dropped immediately). | [S1], [S3] |
| `workspaceUri` | `string` | Workspace `file://` URI or path on `composerData:*` (normalised and hashed to `cwd_hash`; raw path dropped unless `--show-paths` is explicitly requested). | [S1] |
| `modelConfig` | `object` | Default model configuration object on `composerData:*`. | [S1] |
| `modelConfig.modelName` | `string` | Fallback model slug for bubbles in the composer when `bubble.modelInfo.modelName` is absent. | [S1] |
| `fullConversationHeadersOnly` | `array<object>` | Canonical chronological sequence of `{ bubbleId, type }` headers in the composer thread. | [S1] |
| `fullConversationHeadersOnly[].bubbleId` | `string` | Referenced `bubbleId` within `bubbleId:<composerId>:<bubbleId>`. | [S1] |
| `fullConversationHeadersOnly[].type` | `integer` | Bubble role code (`1` = user, `2` = assistant/agent). | [S1] |
| `bubbleId` | `string` | Unique bubble identifier on `bubbleId:<composerId>:<bubbleId>` payloads. | [S1], [S3] |
| `type` | `integer` | Bubble role discriminator (`1` = user prompt, `2` = assistant/agent turn). | [S1], [S3] |
| `text` | `string` | Primary message prose on user (`BlockKind::UserPrompt`) or assistant (`BlockKind::AssistantText`) bubbles; UTF-8 byte-measured and dropped immediately. | [S1], [S3] |
| `thinking` | `object \| string` | Reasoning/thinking block on assistant bubbles (`BlockKind::AssistantThinking`); UTF-8 byte-measured and dropped immediately. | [S1] |
| `thinking.text` | `string` | Thinking prose inside structured `thinking` objects; UTF-8 byte-measured and dropped immediately. | [S1] |
| `modelInfo` | `object` | Per-bubble model metadata on assistant bubbles. | [S1] |
| `modelInfo.modelName` | `string` | Model slug billed on the assistant bubble (`TurnRecord.model`). | [S1] |
| `tokenCount` | `object \| null` | Optional local token summary `{ inputTokens, outputTokens }` on `bubbleId:*`. | [S1], [S2] |
| `tokenCount.inputTokens` | `integer` | Aggregate input tokens reported on the bubble (`0` on many recent Cursor builds). | [S1], [S2] |
| `tokenCount.outputTokens` | `integer` | Output tokens reported on the bubble (`0` on many recent Cursor builds). | [S1], [S2] |
| `toolFormerData` | `object` | Agent tool invocation and result payload attached to an assistant bubble. | [S1] |
| `toolFormerData.toolCallId` | `string` | Tool invocation identifier on `toolFormerData`. | [S1] |
| `toolFormerData.name` | `string` | Tool name retained in `ContentBlock.tool` (e.g. `"read_file"`, `"edit_file"`, `"run_terminal_cmd"`). | [S1] |
| `toolFormerData.status` | `string` | Tool execution status (`"completed"`, `"error"`, `"cancelled"`, etc.). | [S1] |
| `toolFormerData.params` | `object \| string` | Tool call parameters (canonical JSON byte-measured and hashed into `BlockKind::ToolUse` `key_hash`; dropped immediately). | [S1] |
| `toolFormerData.rawArgs` | `string` | Raw JSON argument string fallback when `params` is absent (measured/hashed and dropped immediately). | [S1] |
| `toolFormerData.result` | `string \| object` | Tool result payload (UTF-8 / canonical-JSON byte-measured and queued as `BlockKind::ToolResult` for the next assistant bubble; dropped immediately). | [S1] |
| `pinned` | `boolean` | Non-chat UI state field on unrelated `workbench.panel.*` rows in `cursorDiskKV` (ignored). | [S3] |

## Normalisation & Honest `"not recorded"` Rules

1. **Zero or missing `tokenCount` -> `Confidence::NotRecorded` (`PLAN.md` §0 #6, §5, §10 T11)** ([S1], [S2]):
   - Recent Cursor versions frequently compute token usage server-side and write `tokenCount: { "inputTokens": 0, "outputTokens": 0 }` (or omit `tokenCount`) in local `state.vscdb`.
   - Whenever `tokenCount` is absent/null **or** `inputTokens == 0 && outputTokens == 0`, Kakeibo emits `Usage::not_recorded()` (`fresh_in: 0, cache_write: 0, cache_read: 0, output: 0, confidence: Confidence::NotRecorded`).
   - Human-readable CLI tables (`sessions`, `autopsy [--turns]`, `top --by project`) and Markdown exports (`export --format md`) **must render `"not recorded"` (never `"0"` or `"0/0/0"`)**, and corpus statistics (`compute_corpus_median_p1`) exclude `Confidence::NotRecorded` turns so zeroed Cursor logs never skew `heavy-overhead`.
2. **Positive `tokenCount` -> `Confidence::Estimated` (`PLAN.md` §5, §10 T11)** ([S1]):
   - Whenever `inputTokens > 0 || outputTokens > 0`, Kakeibo emits `Usage::from_disjoint` with `fresh_in = inputTokens`, `cache_write = 0`, `cache_read = 0`, `output = outputTokens`, and **`Confidence::Estimated`** (`estimation_method = Some("cursor_bubble_token_count")`), because Cursor provides no cache read/write split.
3. **Bubble ordering & causal tool-result placement** ([S1]):
   - When `composerData.fullConversationHeadersOnly` is non-empty, bubbles are traversed in that exact header order (resolving `bubbleId:<composerId>:<bubbleId>`); any orphan `bubbleId:<composerId>:*` rows not listed in `fullConversationHeadersOnly` (or composers lacking `composerData:*`) fall back to `(createdAt, bubble_key)` order.
   - `type == 1` user bubbles queue `BlockKind::UserPrompt` for the next `type == 2` assistant bubble; `toolFormerData` on a `type == 2` bubble emits `BlockKind::ToolUse` on that turn and queues `toolFormerData.result` as `BlockKind::ToolResult` for the subsequent `type == 2` bubble.
