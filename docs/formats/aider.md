# Aider Chat History & Analytics Log Formats (`.aider.chat.history.md` & `--analytics-log`)

- **Agent:** Aider (`aider`, `Aider-AI/aider`)
- **Format generations:** Per-repository Markdown chat transcript (`.aider.chat.history.md`) and opt-in JSONL telemetry log (`--analytics-log` / `AIDER_ANALYTICS_LOG`)
- **Checked date:** `2026-10-31`
- **Verification scope:** Upstream `Aider-AI/aider` history writer (`aider/io.py`), token/cost summary formatter (`aider/coders/base_coder.py`), analytics event schema (`aider/analytics.py`), and official Aider documentation.

## Sources

- **[S1]** Aider Chat History & CLI Options Documentation (`.aider.chat.history.md`, `AIDER_CHAT_HISTORY_FILE`, `# aider chat started at`, `####` user prompts, `> ...` tool output): <https://aider.chat/docs/config/options.html> (`checked: 2026-10-31`).
- **[S2]** Aider-AI/aider `BaseCoder.show_send_output_tokens` & `InputOutput.append_chat_history` (`aider/coders/base_coder.py`, `aider/io.py` — `> Tokens: <sent> sent[, <write> cache write][, <hit> cache hit], <recv> received. Cost: $<msg> message, $<sess> session.` with `k`/`M` human-rounded counts): <https://github.com/Aider-AI/aider> (`checked: 2026-10-31`).
- **[S3]** Aider Analytics Documentation & `Analytics.event` JSONL Schema (`--analytics-log`, `AIDER_ANALYTICS_LOG`, `aider/analytics.py` — `event`, `properties.{main_model, weak_model, editor_model, edit_format, prompt_tokens, completion_tokens, total_tokens, cost, total_cost, version}`, `user_id`, `time`): <https://aider.chat/docs/more/analytics.html> (`checked: 2026-10-31`).

## Discovery & File Layout

| Location / Variable | Role | Source |
|---|---|---|
| `.aider.chat.history.md` | Default per-repository Markdown chat transcript written in the git root / working directory. | [S1], [S2] |
| `~/.aider/` | Optional user-level Aider directory searched up to bounded depth 2 for `.aider.chat.history.md` and `*.analytics.jsonl` / `analytics.jsonl` logs without walking `$HOME`. | [S1], [S3] |
| `AIDER_HOME` | Optional environment override for the user-level `~/.aider` directory. | [S1] |
| `AIDER_CHAT_HISTORY_FILE` | Explicit path override for `.aider.chat.history.md`. | [S1] |
| `AIDER_ANALYTICS_LOG` | Explicit path override for the opt-in `--analytics-log` JSONL file. | [S1], [S3] |

## Markdown Chat History Structural Markers (`.aider.chat.history.md`)

Aider appends human-readable Markdown transcripts via `InputOutput.append_chat_history`. Because token figures in `> Tokens:` lines use human-rounded `k`/`M` suffixes (`math_utils` / `format_tokens`), **every turn decoded from `.aider.chat.history.md` carries `Confidence::Estimated`** (`estimation_method = "aider_history_rounded"`), and all raw user/assistant text is measured in UTF-8 bytes and dropped immediately (`PLAN.md` §0 #4, §5, §10 T11).

| Marker | Construct | Semantics | Source |
|---|---|---|---|
| `# aider chat started at` | `markdown heading` | Session boundary banner `# aider chat started at YYYY-MM-DD HH:MM:SS` (normalised deterministically to UTC `YYYY-MM-DDTHH:MM:SSZ` per `ADR 001`). | [S1], [S2] |
| `> Aider v` | `markdown blockquote` | CLI version banner `> Aider v<version>` recorded in `ParseDiagnostics.cli_versions`. | [S1], [S2] |
| `> Main model:` | `markdown blockquote` | Active model banner `> Main model: <model> with <edit_format> edit format` updating the session's current model slug. | [S1], [S2] |
| `> Models:` | `markdown blockquote` | Legacy/summary model banner `> Models: <main_model>, weak model <weak_model>` updating the current model slug. | [S1], [S2] |
| `####` | `markdown heading` | User prompt line prefix (`#### <line>`). Consecutive `####` lines belong to one user prompt; `/clear` and `/reset` set `TurnHints.compaction = true` for the next turn, `/model <slug>` updates the active model, and all other prompt text is UTF-8 byte-measured into `BlockKind::UserPrompt` and dropped immediately. | [S1], [S2] |
| `> Tokens:` | `markdown blockquote` | Turn completion summary `> Tokens: <X> sent[, <W> cache write][, <R> cache hit], <Y> received.[ Cost: $<Z> message, $<S> session.]` emitting one `TurnRecord` with `Confidence::Estimated`. | [S2] |

## Analytics JSONL Field Reference (`--analytics-log`)

Aider's opt-in `--analytics-log` writes one JSON object per event (`launched`, `message_send`, `exit`, etc.). Because `message_send` records aggregate `prompt_tokens` and `completion_tokens` with **no cache read/write split** (`PLAN.md` §5), **every turn decoded from `--analytics-log` carries `Confidence::Estimated`** (`estimation_method = "aider_analytics_no_cache_split"`).

| Field Path | JSON Type | Semantics | Source |
|---|---|---|---|
| `event` | `string` | Analytics event discriminator (`"message_send"` emits a `TurnRecord`; lifecycle events such as `"launched"`, `"cli session"`, and `"exit"` increment `ignored_records`). | [S3] |
| `properties` | `object` | Event-specific payload map. | [S3] |
| `properties.main_model` | `string` | Primary model slug billed on `"message_send"` (`TurnRecord.model`). | [S3] |
| `properties.weak_model` | `string \| null` | Configured weak model slug for commit messages / summaries (metadata only). | [S3] |
| `properties.editor_model` | `string \| null` | Configured architect/editor model slug when applicable (metadata only). | [S3] |
| `properties.edit_format` | `string` | Active Aider edit format (`"diff"`, `"whole"`, `"udiff"`, etc.). | [S3] |
| `properties.prompt_tokens` | `integer` | Aggregate prompt tokens sent on `"message_send"` (`fresh_in = prompt_tokens`, `cache_write = 0`, `cache_read = 0`, `Confidence::Estimated`). | [S3] |
| `properties.completion_tokens` | `integer` | Completion output tokens received on `"message_send"` (`Usage.output`). | [S3] |
| `properties.total_tokens` | `integer` | Total tokens (`prompt_tokens + completion_tokens`) reported by LiteLLM. | [S3] |
| `properties.cost` | `number` | Per-message USD cost computed by Aider/LiteLLM (`agent_reported_cost_usd = Some(cost)` strictly when finite and `cost > 0.0`). | [S3] |
| `properties.total_cost` | `number` | Cumulative session USD cost up to this event (not summed, avoiding double-counting). | [S3] |
| `properties.version` | `string` | Aider CLI version string recorded in `ParseDiagnostics.cli_versions`. | [S3] |
| `properties.reason` | `string` | Exit reason on `"exit"` lifecycle events (ignored). | [S3] |
| `time` | `number \| string` | Unix epoch seconds (integer or float) or RFC 3339 timestamp converted to UTC `Timestamp`. | [S3] |
| `user_id` | `string` | Random anonymous Aider analytics UUID (irreversibly mixed into SHA-256 session ID derivation; never stored raw). | [S3] |

## Normalisation & Attribution Rules

1. **Rounded `k`/`M` token parsing & inclusive `sent` handling (`.aider.chat.history.md`)** ([S2]):
   - Suffixes `k`/`K` multiply by `1_000` and `m`/`M` multiply by `1_000_000` using exact decimal scaling (e.g. `12k -> 12_000`, `1.4k -> 1_400`, `850 -> 850`).
   - When `cache write` (`W`) and/or `cache hit` (`R`) are present alongside `<X> sent`, LiteLLM's `prompt_tokens` includes cached tokens (`X >= W + R`), so `fresh_in = X - W - R`, `cache_write = W`, `cache_read = R` (falling back to disjoint `fresh_in = X` if an older log ever reports `X < W + R`).
   - Every `.aider.chat.history.md` turn sets `Confidence::Estimated` (`estimation_method = Some("aider_history_rounded")`).
2. **Analytics `message_send` accounting (`--analytics-log`)** ([S3]):
   - Only `event == "message_send"` emits a `TurnRecord`; lifecycle events (`launched`, `exit`, etc.) increment `ParseDiagnostics.ignored_records`.
   - Every `message_send` turn sets `fresh_in = prompt_tokens`, `cache_write = 0`, `cache_read = 0`, `output = completion_tokens`, and `Confidence::Estimated` (`estimation_method = Some("aider_analytics_no_cache_split")`).
3. **Agent-reported cost (`PLAN.md` §6.2)** ([S2], [S3]):
   - Both `Cost: $<Z> message` in `.aider.chat.history.md` and `properties.cost` in `--analytics-log` populate `TurnRecord.agent_reported_cost_usd = Some(cost)` strictly when finite and `cost > 0.0`, mapping `$0.00` / `0` to `None` (`"unpriced"` unless user `pricing.toml` supplies rates).
