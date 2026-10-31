# OpenCode Session Storage Format (`opencode.db` & Legacy `storage/`)

- **Agent:** OpenCode (`opencode`, `anomalyco/opencode` / `sst/opencode`)
- **Format generations:** Modern SQLite WAL database (`opencode.db`, `v1.2+`) and legacy JSON file hierarchy (`storage/{session,message,part}/`, `< v1.2`)
- **Checked date:** `2026-10-31`
- **Verification scope:** Published upstream schema definitions (`session.sql.ts`, `message-v2.ts`), troubleshooting docs, and community parser references.

## Sources

- **[S1]** OpenCode Troubleshooting & Storage Layout Documentation (`~/.local/share/opencode/opencode.db` and `~/.local/share/opencode/storage/`): <https://opencode.ai/docs/troubleshooting/> (`checked: 2026-10-31`).
- **[S2]** anomalyco/opencode `Session`, `MessageV2`, and `Part` schema (`packages/opencode/src/session/session.sql.ts`, `message-v2.ts`, `Session.getUsage` disjoint `input + output + reasoning + cache.read + cache.write`): <https://github.com/anomalyco/opencode> (`checked: 2026-10-31`).
- **[S3]** ryoppippi/ccusage #966 — OpenCode SQLite (`session`, `message`, `part`) and legacy `storage/` token & `cost` schema analysis: <https://github.com/ryoppippi/ccusage/issues/966> (`checked: 2026-10-31`).
- **[S4]** repowise-dev/skiplevel — OpenCode SQLite relational table columns & `data` JSON contract: <https://github.com/repowise-dev/skiplevel> (`checked: 2026-10-31`).

## Discovery & File Layout

| Location / Variable | Role | Source |
|---|---|---|
| `~/.local/share/opencode/opencode.db` | Modern SQLite database (WAL mode) containing `project`, `session`, `message`, and `part` tables (`${XDG_DATA_HOME:-~/.local/share}/opencode/opencode.db`). | [S1], [S2], [S3], [S4] |
| `~/.local/share/opencode/storage/session/<project_id>/<session_id>.json` | Legacy JSON session metadata records (`ses_*.json`). | [S1], [S2], [S3] |
| `~/.local/share/opencode/storage/message/<session_id>/<message_id>.json` | Legacy JSON message records (`msg_*.json`) belonging to `<session_id>`. | [S1], [S2], [S3] |
| `~/.local/share/opencode/storage/part/<message_id>/<part_id>.json` | Legacy JSON message parts (`prt_*.json`) belonging to `<message_id>`. | [S1], [S2], [S3] |
| `OPENCODE_DATA_DIR` | Optional environment override pointing directly to the `opencode` data root directory (overrides `XDG_DATA_HOME`). | [S1], [S2] |

## Read-Only SQLite Access & Legacy Coexistence

1. **Strictly read-only WAL-compatible SQLite access (`PLAN.md` §0 #3, §5, §10 T10):**
   - `opencode.db` is opened via SQLite URI `file:<percent-encoded-path>?mode=ro` with `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX | SQLITE_OPEN_URI`, `busy_timeout = 2500ms`, and `PRAGMA query_only = ON`.
   - Under WAL mode, `mode=ro` reads committed database and `-wal` frames while another process holds an active `BEGIN IMMEDIATE` write lock, **never modifying `opencode.db` bytes (SHA-256) or `mtime`**.
   - Only if `mode=ro` fails with `SQLITE_CANTOPEN` or `SQLITE_READONLY` (e.g. a `0555` read-only parent directory where SQLite cannot map `-shm` and no `-wal` sidecar exists), Kakeibo falls back to `file:<percent-encoded-path>?mode=ro&immutable=1` + `PRAGMA query_only = ON`.
2. **SQLite relational tables** ([S2], [S3], [S4]):
   - `session`: `id TEXT PRIMARY KEY`, `project_id TEXT`, `parent_id TEXT`, `slug TEXT`, `directory TEXT`, `title TEXT`, `version TEXT`, `time_created INTEGER`, `time_updated INTEGER` (plus optional `data TEXT` JSON on transitional builds).
   - `message`: `id TEXT PRIMARY KEY`, `session_id TEXT`, `time_created INTEGER`, `time_updated INTEGER`, `data TEXT` (JSON matching `MessageV2.Info` below, optionally without redundant `id`/`sessionID`).
   - `part`: `id TEXT PRIMARY KEY`, `message_id TEXT`, `session_id TEXT`, `time_created INTEGER`, `time_updated INTEGER`, `data TEXT` (JSON matching `MessageV2.Part` below, optionally without redundant `id`/`messageID`/`sessionID`).
3. **Deduplication across `opencode.db` and `storage/`** ([S1], [S3]):
   - Migrated installs frequently retain legacy `storage/` alongside `opencode.db`. Any `session_id` present in `opencode.db` takes precedence over the same `session_id` in `storage/`.

## Field Reference (Every Used Field Cites a Source)

Children inside arbitrary tool input objects (`state.input.*`) and tool execution metadata (`state.metadata.*`) are canonical-JSON measured/hashed and dropped immediately without retaining raw keys or text.

| Field Path | JSON Type | Semantics | Source |
|---|---|---|---|
| `id` | `string` | Primary identifier (`ses_*` on session, `msg_*` on message, `prt_*` on part). | [S2], [S3], [S4] |
| `projectID` | `string` | Project identifier on legacy `session` JSON (`project_id` column in SQLite). | [S2], [S3], [S4] |
| `parentID` | `string \| null` | Parent `ses_*` ID on child subagent `session` JSON (`parent_id` column in SQLite; sets `TurnHints.sidechain = true` and `TurnRecord.parent_session_id`), or parent user `msg_*` ID on `assistant` message JSON. | [S2], [S3], [S4] |
| `slug` | `string` | Short session slug on `session` metadata (ignored). | [S2], [S4] |
| `directory` | `string` | Session workspace directory (`cwd`, hashed to 16-hex `cwd_hash`; raw path dropped). | [S2], [S3], [S4] |
| `title` | `string` | Session display title (dropped immediately). | [S2], [S3], [S4] |
| `version` | `string` | OpenCode CLI version string on `session` (recorded in `ParseDiagnostics.cli_versions`). | [S2], [S4] |
| `sessionID` | `string` | Owning `ses_*` identifier on legacy `message` and `part` JSON (`session_id` column in SQLite). | [S2], [S3] |
| `messageID` | `string` | Owning `msg_*` identifier on legacy `part` JSON (`message_id` column in SQLite). | [S2], [S3] |
| `role` | `string` | Message role in `message` JSON (`"user"` or `"assistant"`). | [S2], [S3], [S4] |
| `agent` | `string` | OpenCode agent profile name on `message` (e.g. `"build"`, `"plan"`, `"general"`). | [S2], [S3] |
| `mode` | `string` | Execution mode label on `assistant` `message` JSON. | [S2] |
| `model` | `object` | Selected `{ providerID, modelID }` object on `user` `message` JSON. | [S2] |
| `model.providerID` | `string` | Provider identifier on `user` `message.model`. | [S2] |
| `model.modelID` | `string` | Model slug on `user` `message.model`. | [S2] |
| `modelID` | `string` | Model slug billed on `assistant` `message` JSON (`TurnRecord.model`, e.g. `"claude-sonnet-4-5"`). | [S2], [S3], [S4] |
| `providerID` | `string` | Provider identifier on `assistant` `message` JSON (e.g. `"anthropic"`, `"openai"`). | [S2], [S3], [S4] |
| `cost` | `number` | Per-step USD cost computed by OpenCode (`agent_reported_cost_usd = Some(cost)` strictly when finite and `cost > 0.0`; zero-initialized `0` on unpriced/subscription runs maps to `None`). | [S2], [S3], [S4] |
| `finish` | `string` | Step finish reason on `assistant` `message` JSON (`"tool-calls"`, `"stop"`, etc.). | [S2] |
| `summary` | `boolean` | Marks an `assistant` compaction summary message (`TurnHints.compaction = true`). | [S2] |
| `error` | `object \| null` | Structured API/provider error on `assistant` `message` JSON (`TurnHints.api_error = true`; error message text dropped). | [S2] |
| `path` | `object` | Working/root paths attached to `assistant` `message` JSON. | [S2] |
| `path.cwd` | `string` | Per-message working directory fallback (hashed to `cwd_hash`; raw path dropped). | [S2] |
| `path.root` | `string` | Project root directory on `assistant` `message.path` (dropped). | [S2] |
| `time` | `object` | Epoch-millisecond lifecycle timestamps on `session` and `message` JSON. | [S2], [S3], [S4] |
| `time.created` | `integer` | Creation timestamp in Unix epoch milliseconds. | [S2], [S3], [S4] |
| `time.updated` | `integer` | Last-update timestamp in Unix epoch milliseconds on `session` JSON. | [S2], [S4] |
| `time.completed` | `integer` | Step completion timestamp in Unix epoch milliseconds on `assistant` `message` JSON (preferred for `TurnRecord.ts`). | [S2], [S3] |
| `tokens` | `object` | Disjoint token counts on `assistant` `message` JSON. | [S2], [S3], [S4] |
| `tokens.input` | `integer` | Uncached fresh input tokens (`fresh_in` — **disjoint** from `tokens.cache.read` and `tokens.cache.write`). | [S2], [S3] |
| `tokens.output` | `integer` | Non-reasoning output tokens (**disjoint** from `tokens.reasoning`). | [S2], [S3] |
| `tokens.reasoning` | `integer` | Reasoning/thinking output tokens (`Usage.output = tokens.output + tokens.reasoning`, `Usage.reasoning = Some(reasoning)` when `> 0`). | [S2], [S3] |
| `tokens.cache` | `object` | Prompt cache read/write breakdown on `assistant` `message.tokens`. | [S2], [S3], [S4] |
| `tokens.cache.read` | `integer` | Cached prompt tokens read (`cache_read`). | [S2], [S3], [S4] |
| `tokens.cache.write` | `integer` | Prompt tokens written to cache (`cache_write`, `CacheWriteSplit::UnknownTtl`). | [S2], [S3], [S4] |
| `type` | `string` | `Part` discriminator (`"text"`, `"reasoning"`, `"tool"`, `"compaction"`, `"subtask"`, `"retry"`, `"step-start"`, `"step-finish"`, `"patch"`, `"snapshot"`, `"file"`, `"agent"`). | [S2], [S3], [S4] |
| `text` | `string` | Body on `"text"` or `"reasoning"` parts (UTF-8 byte-measured and dropped immediately). | [S2], [S4] |
| `auto` | `boolean` | Automatic vs manual compaction flag on `type == "compaction"` parts. | [S2] |
| `callID` | `string` | Tool call correlation identifier on `type == "tool"` parts. | [S2], [S4] |
| `tool` | `string` | Tool name on `type == "tool"` parts (retained in `ContentBlock.tool`). | [S2], [S4] |
| `state` | `object` | Lifecycle state container (`pending`, `running`, `completed`, `error`) on `type == "tool"` parts. | [S2], [S4] |
| `state.status` | `string` | Tool execution status (`"pending"`, `"running"`, `"completed"`, `"error"`). | [S2], [S4] |
| `state.input` | `object` | Tool call arguments object (canonical JSON byte-measured and hashed into `key_hash`; dropped). | [S2], [S4] |
| `state.output` | `string \| object` | Completed tool output (byte-measured and queued as `BlockKind::ToolResult` for the next assistant turn; dropped). | [S2], [S4] |
| `state.error` | `string` | Tool error string when `state.status == "error"` (byte-measured as `BlockKind::ToolResult` fallback; dropped). | [S2] |
| `state.title` | `string` | Short tool UI title (dropped immediately). | [S2] |
| `state.metadata` | `object` | Opaque tool-specific UI metadata (dropped immediately). | [S2] |
| `state.time` | `object` | Tool execution start/end epoch-millisecond timestamps. | [S2] |
| `state.time.start` | `integer` | Tool execution start in Unix epoch milliseconds. | [S2] |
| `state.time.end` | `integer` | Tool execution end in Unix epoch milliseconds. | [S2] |

## Normalisation & Attribution Rules

1. **Disjoint token accounting** ([S2], [S3]):
   - OpenCode's `Session.getUsage` normalises every provider into disjoint buckets satisfying `total = input + output + reasoning + cache.read + cache.write`.
   - Kakeibo maps OpenCode `tokens` via `Usage::from_disjoint`:
     - `fresh_in = tokens.input`
     - `cache_write = tokens.cache.write` (`CacheWriteSplit::UnknownTtl`)
     - `cache_read = tokens.cache.read`
     - `prompt_tokens = fresh_in + cache_write + cache_read`
     - `output = tokens.output + tokens.reasoning`
     - `reasoning = (tokens.reasoning > 0).then_some(tokens.reasoning)`
2. **Agent-reported cost (`PLAN.md` §6.2 & T09/T10)** ([S2], [S3]):
   - OpenCode zero-initializes `cost: 0` on subscription, OAuth, and unpriced local-model turns.
   - Kakeibo sets `TurnRecord.agent_reported_cost_usd = Some(cost)` strictly when `cost.is_finite() && cost > 0.0`, preserving `"unpriced"` (never `"$0"`) whenever `cost == 0` and no user `pricing.toml` entry matches.
3. **Causal tool-result placement & subagents** ([S2]):
   - Each `"assistant"` message emits `ToolUse` blocks for its `"tool"` parts (`state.input`) and queues their `state.output` / `state.error` payloads as `BlockKind::ToolResult` for the next `"assistant"` step in the session, matching the API call where the tool output first enters `P(t)`.
   - Child sessions with `parent_id` / `parentID` set emit `TurnRecord.parent_session_id = Some(parent_id)` and `TurnHints.sidechain = true` so `kakeibo-core::attrib` folds child token spend into the parent session's `Subagents` category without double-counting.
