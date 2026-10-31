# Kakeibo CLI JSON Export Schema (`schemaVersion: 1`)

This document specifies the structured UTF-8 JSON document emitted by:

```sh
kakeibo export <id|latest> --format json [--out <file>] [--show-paths]
kakeibo export --since <window> --format json [--out <file>] [--show-paths]
```

(`PLAN.md` §6.4, §8, §10 T08).

For the streaming turn-level ledger (`"schema": "kakeibo.ledger/1"`) emitted by `kakeibo export ... --format ndjson`, see [`docs/ledger-schema.md`](./ledger-schema.md).

---

## 1. Compatibility & Privacy Contract

1. **Version tag:** Every document carries `"schemaVersion": 1` at the root object.
   - Consumers **must** check `schemaVersion == 1` before reading `sessions` and **must** ignore unknown additive JSON keys within `schemaVersion: 1` objects.
   - Breaking removals, type changes, or semantic redefinitions increment `schemaVersion`.
2. **Text-free & pathless by default (`PLAN.md` §0 #4, §6.4):**
   - No prompt text, assistant completion text, chain-of-thought text, or raw tool arguments/outputs ever appear in a JSON export.
   - By default (`--show-paths` omitted), `project_path` is **omitted** from every session object; projects are identified solely by their irreversible 16-hex-char SHA-256 prefix `project_hash` (`SHA-256("cwd\0" + cwd)[..8]`).
   - Only when `--show-paths` is explicitly passed on the command line may `project_path` be populated with the session's observed working directory.
3. **Provenance & honest pricing (`PLAN.md` §0 #7, §6.2):**
   - Every token aggregate carries an explicit `confidence` (`"measured"`, `"estimated"`, or `"not_recorded"`) and, when `"estimated"`, an `estimation_method` identifier (e.g. `"utf8_bytes_div4_ceil_scaled"` per [`ADR 002`](./decisions/002-token-estimator.md)).
   - Unpriced sessions emit `"cost": { "status": "unpriced", "usd": null }` — never `0` or `0.0`.
4. **Finite numbers only:** All JSON numbers are integers (`u32` / `u64`) or finite IEEE-754 floats (`NaN` and `±Infinity` never appear).

---

## 2. Root Document (`CliExportDocumentV1`)

| Field | JSON Type | Required | Description |
|---|---|---|---|
| `schemaVersion` | `integer` (`u32`) | yes | Always `1` (`kakeibo_core::product::CLI_EXPORT_SCHEMA_VERSION`). |
| `generator` | `object` | yes | `{ "name": string, "version": string }` identifying the `kakeibo` binary and semantic version. |
| `session_count` | `integer` (`usize`) | yes | Number of items in `sessions` (`sessions.len()`). |
| `sessions` | `array<SessionExportV1>` | yes | Matching sessions ordered deterministically by `ended_at` descending, tie-broken by `session_id` ascending. |

---

## 3. Session Object (`SessionExportV1`)

| Field | JSON Type | Required | Description |
|---|---|---|---|
| `agent` | `string` (`AgentId`) | yes | Source coding agent: `"claude-code"`, `"codex"`, `"gemini-cli"`, `"opencode"`, `"aider"`, or `"cursor"`. |
| `session_id` | `string` | yes | Agent-assigned primary session/thread identifier. |
| `turn_count` | `integer` (`u32`) | yes | Number of deduplicated API requests in `turns`. |
| `started_at` | `string` (`Timestamp`) | yes | Canonical UTC RFC 3339 timestamp (`Z`) of the earliest turn in the session. |
| `ended_at` | `string` (`Timestamp`) | yes | Canonical UTC RFC 3339 timestamp (`Z`) of the latest turn in the session. |
| `models` | `array<string>` | yes | Sorted distinct model identifiers observed across the session's turns. |
| `project_hash` | `string \| null` | yes | 16-hex-char SHA-256 `cwd_hash` digest of the session working directory, or `null` when unknown. |
| `project_path` | `string` | no | Raw working directory string; **omitted by default** and included only when `--show-paths` is passed. |
| `cost` | `object` (`CostExportV1`) | yes | Cost status and optional USD value (§4). |
| `totals` | `object` (`TotalsExportV1`) | yes | Reconciled session-wide token totals (§5). |
| `fixed_overhead` | `object` (`FixedOverheadExportV1`) | yes | Estimated pre-conversation prompt overhead `P(1) - est(first_user_prompt)` (§5). |
| `categories` | `object` (`CategoriesExportV1`) | yes | Reconciled 9-category breakdown summing exactly to `totals` (§6). |
| `flags` | `array<FlagExportV1>` | yes | Ranked deterministic root-cause findings (§7). Empty (`[]`) when no flags fire. |
| `tools` | `array<ToolSpendExportV1>` | yes | Per-tool token carry leaderboard (§8). Empty (`[]`) when no tools were invoked. |
| `subagents` | `array<SubagentSessionSummary>` | yes | Child subagent session rollups folded into `categories.breakdown.subagents` (§9). |
| `turns` | `array<TurnExportV1>` | yes | Chronological per-request measured and attributed token splits (§10). |

---

## 4. `CostExportV1` (`cost`)

| Field | JSON Type | Required | Description |
|---|---|---|---|
| `status` | `string` | yes | `"unpriced"` (no user pricing table entry applied), `"configured"` (every turn in the session matched a configured price), `"partial_configured"` (some turns matched a configured price while one or more turns remained unpriced), or `"agent_reported"` (cost logged directly by the agent when unconfigured). |
| `currency` | `string` | no | ISO currency code (e.g. `"USD"`) when `status` is `"configured"` or `"partial_configured"`; omitted when `"unpriced"`. |
| `usd` | `number \| null` | yes | `null` when `status == "unpriced"` (or non-USD currency); finite USD sum when priced in `"USD"` or `"agent_reported"`. |

---

## 5. `TotalsExportV1` (`totals`) & `FixedOverheadExportV1` (`fixed_overhead`)

### `totals`
| Field | JSON Type | Required | Description |
|---|---|---|---|
| `prompt_tokens` | `integer` (`u64`) | yes | `fresh_in + cache_write + cache_read`. |
| `total_tokens` | `integer` (`u64`) | yes | `prompt_tokens + output`. |
| `fresh_in` | `integer` (`u64`) | yes | Disjoint uncached fresh input tokens across the session (including rolled-up subagents). |
| `cache_write` | `integer` (`u64`) | yes | Prompt cache creation/write tokens across the session. |
| `cache_read` | `integer` (`u64`) | yes | Prompt cache hit/read tokens across the session. |
| `output` | `integer` (`u64`) | yes | Completion/output tokens across the session. |
| `confidence` | `string` (`Confidence`) | yes | `"measured"`, `"estimated"`, or `"not_recorded"`. |

### `fixed_overhead`
| Field | JSON Type | Required | Description |
|---|---|---|---|
| `tokens` | `integer` (`u64`) | yes | Estimated fixed pre-conversation prompt tokens on the first main-chain turn. |
| `confidence` | `string` (`Confidence`) | yes | `"estimated"`. |
| `estimation_method` | `string` | yes | Token estimator method identifier (`"utf8_bytes_div4_ceil_scaled"`). |

---

## 6. `CategoriesExportV1` (`categories`)

| Field | JSON Type | Required | Description |
|---|---|---|---|
| `confidence` | `string` (`Confidence`) | yes | Confidence of category attribution (`"estimated"` or `"not_recorded"`). |
| `estimation_method` | `string` | yes | Estimator and reconciliation method (`"utf8_bytes_div4_ceil_scaled"`). |
| `breakdown` | `object` | yes | Map containing all nine canonical `snake_case` category keys (`fixed_overhead`, `resent_history`, `tool_output`, `retries`, `user_input`, `assistant_output`, `subagents`, `compaction`, `unattributed`), each mapping to a `CategoryBucketExportV1`. |

### `CategoryBucketExportV1`
| Field | JSON Type | Required | Description |
|---|---|---|---|
| `prompt_tokens` | `integer` (`u64`) | yes | `fresh_in + cache_write + cache_read` attributed to this category. |
| `total_tokens` | `integer` (`u64`) | yes | `prompt_tokens + output` attributed to this category. |
| `fresh_in` | `integer` (`u64`) | yes | Fresh input tokens attributed to this category. |
| `cache_write` | `integer` (`u64`) | yes | Cache write tokens attributed to this category. |
| `cache_read` | `integer` (`u64`) | yes | Cache read tokens attributed to this category. |
| `output` | `integer` (`u64`) | yes | Output tokens attributed to this category. |
| `share_bps` | `integer` (`u64`) | yes | Share of `totals.total_tokens` in integer basis points (`0..=10000`, `floor(total_tokens * 10000 / session_total_tokens)`). |
| `cost_usd` | `number` (`f64`) | no | Reconciled API-equivalent USD cost attributed to this category when `cost.status == "configured"` (`currency == "USD"`); omitted when unpriced. |

**Reconciliation invariant:** Across all nine keys in `categories.breakdown`, the component-wise sums of `fresh_in`, `cache_write`, `cache_read`, and `output` equal `totals.fresh_in`, `totals.cache_write`, `totals.cache_read`, and `totals.output` exactly.

---

## 7. `FlagExportV1` (`flags[]`)

Items are ranked by `impact_tokens` descending, tie-broken by `turn_range` ascending, then `id` ascending.

| Field | JSON Type | Required | Description |
|---|---|---|---|
| `id` | `string` (`FlagId`) | yes | `"cold-resume"`, `"near-full-context"`, `"tool-output-bloat"`, `"cache-miss-storm"`, or `"heavy-overhead"` (v2 flags add additive slugs in T14). |
| `turn_range` | `object` (`TurnRange`) | yes | `{ "start": u32, "end": u32 }` inclusive 1-based turn span. |
| `impact_tokens` | `integer` (`u64`) | yes | Primary token impact used to rank the finding. |
| `confidence` | `string` (`Confidence`) | yes | `"measured"` or `"estimated"`. |
| `estimation_method` | `string` | no | Present when `confidence == "estimated"`; omitted otherwise. |
| `sentence` | `string` | yes | Plain-words one-sentence summary (`kakeibo_report::flags::render_flag_sentence`). |
| `evidence_text` | `string` | yes | Provenance-labelled evidence line (`kakeibo_report::flags::render_flag_evidence`). |
| `evidence` | `object` (`FlagEvidence`) | yes | Structured evidence payload tagged with `"kind"` (`"cold-resume"`, `"near-full-context"`, `"tool-output-bloat"`, `"cache-miss-storm"`, `"heavy-overhead"`). |

---

## 8. `ToolSpendExportV1` (`tools[]`)

Aggregates carried `ToolUse` and `ToolResult` blocks by tool name, ranked by `lifetime_tokens` descending, tie-broken by `tool` ascending.

| Field | JSON Type | Required | Description |
|---|---|---|---|
| `tool` | `string` | yes | Tool identifier (e.g. `"Bash"`, `"Read"`, `"exec_command"`). |
| `calls` | `integer` (`u32`) | yes | Count of `tool_use` blocks observed for this tool. |
| `results` | `integer` (`u32`) | yes | Count of `tool_result` blocks observed for this tool. |
| `errors` | `integer` (`u32`) | yes | Count of `is_error == true` blocks observed for this tool. |
| `bytes` | `integer` (`u64`) | yes | Sum of measured UTF-8 byte lengths across this tool's blocks. |
| `initial_tokens` | `integer` (`u64`) | yes | Prompt tokens attributed on the turn each block first entered `P(t)`. |
| `carried_tokens` | `integer` (`u64`) | yes | Cumulative prompt tokens attributed to re-sending this tool's blocks on later turns. |
| `lifetime_tokens` | `integer` (`u64`) | yes | `initial_tokens + carried_tokens`. |
| `confidence` | `string` (`Confidence`) | yes | `"estimated"`. |
| `estimation_method` | `string` | yes | `"utf8_bytes_div4_ceil_scaled"`. |

---

## 9. `SubagentSessionSummary` (`subagents[]`)

| Field | JSON Type | Required | Description |
|---|---|---|---|
| `session_id` | `string` | yes | Child subagent session identifier. |
| `turns` | `integer` (`u32`) | yes | Number of turns in the child subagent session. |
| `totals` | `object` (`TokenSplit`) | yes | `{ "fresh_in": u64, "cache_write": u64, "cache_read": u64, "output": u64 }`. |

---

## 10. `TurnExportV1` (`turns[]`)

| Field | JSON Type | Required | Description |
|---|---|---|---|
| `turn` | `integer` (`u32`) | yes | 1-based turn ordinal within the session. |
| `ts` | `string` (`Timestamp`) | yes | Canonical UTC RFC 3339 timestamp (`Z`). |
| `model` | `string \| null` | yes | Model identifier on this turn, or `null` when unknown. |
| `prompt_tokens` | `integer` (`u64`) | yes | `measured.fresh_in + measured.cache_write + measured.cache_read`. |
| `measured` | `object` (`TokenSplit`) | yes | `{ "fresh_in": u64, "cache_write": u64, "cache_read": u64, "output": u64 }`. |
| `measured_confidence` | `string` (`Confidence`) | yes | Provenance confidence of `measured`. |
| `cache_write_5m_tokens` | `integer` (`u64`) | no | 5-minute TTL prompt-cache write tokens on this turn when priced; omitted when unpriced. |
| `cache_write_1h_tokens` | `integer` (`u64`) | no | 1-hour TTL prompt-cache write tokens on this turn when priced; omitted when unpriced. |
| `cost_usd` | `number` (`f64`) | no | API-equivalent USD cost of this turn when priced (`currency == "USD"`); omitted when unpriced. |
| `categories` | `object` (`CategoryBreakdown`) | yes | Map of all 9 `Category` keys to `TokenSplit`, reconciling exactly to `measured`. |
| `category_confidence` | `string` (`Confidence`) | yes | Provenance confidence of `categories`. |
| `estimation_method` | `string` | yes | `"utf8_bytes_div4_ceil_scaled"`. |
| `new_unattributed_prompt_tokens` | `integer` (`u64`) | yes | Prompt growth on this turn exceeding estimated added-block upper bounds. |
| `hints` | `object` (`TurnHints`) | yes | Structural hints (`resume_start`, `compaction`, `sidechain`, `api_error`, optional `rate_limit_used_percent`, optional `context_window_override`). |
