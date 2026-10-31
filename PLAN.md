# Kakeibo: Implementation Plan (v1)

*Kakeibo: a token budget diary for your coding agents.* (Formerly "Token Autopsy".)

> **Names:** display name **Kakeibo**, CLI binary and crate **`kakeibo`**, Homebrew formula **`kakeibo`**, optional menu bar cask **`kakeibo-bar`**, bundle id **`io.github.quiinthieu.kakeibo`**. They're defined once in `crates/kakeibo-core/src/product.rs` and mirrored in `macos/Config/Product.xcconfig`. **Don't hardcode the names anywhere else.** §1.4 has the clash check.

Kakeibo reads the session logs coding agents already write and explains, **per session and per turn**, where the tokens went (fixed overhead: system prompt, rules, skills, MCP schemas; resent history; tool output; retries; subagents; compaction), then flags root causes like *"resumed session re-sent ~960K tokens on its first request"*. **Local-only:** no network, transcripts never leave the machine, and only derived numbers are stored.

---

## Repository & workflow

- **Repo:** **`git@github.com:quiinthieu/kakeibo.git`**.
- **Bootstrap (first agent only):**
  ```sh
  git clone git@github.com:quiinthieu/kakeibo.git && cd kakeibo
  git checkout -b main
  # add PLAN.md, AGENTS.md (+ CLAUDE.md containing "@AGENTS.md"), .gitignore (target/, DerivedData/, .build/, .DS_Store)
  git add . && git commit -m "chore: add implementation plan and agent instructions"
  git push -u origin main
  ```
  Then make `main` the default branch and protect it (PR required, CI green).
- **Per task:** branch from the latest `main` as `task/TNN-short-slug` (e.g. `task/T04-claude-parser`). One task per branch. Use Conventional Commits (`feat(parse): …`, `fix(attrib): …`, `test:`, `chore:`, `docs:`). Open a PR titled `TNN: <task title>` with the acceptance criteria as a checklist. Squash-merge once CI passes, then delete the branch.
- **CI:** T01 adds `.github/workflows/ci.yml`: `cargo fmt --check`, `cargo clippy --workspace -- -D warnings`, `cargo test --workspace`, `cargo deny check`, `scripts/lint-privacy.sh`, `scripts/lint-fixtures.sh` on `ubuntu-latest` and `macos-latest`. From T15 it also runs `xcodebuild test` for `macos/`. T17 adds `release.yml` on `v*` tags.
- **Never** force-push `main`. Never commit secrets, signing or notarization credentials (use Actions secrets), **or any real transcript**.

## 0. Instructions for agents (read first)

1. **One task at a time, in order:** the lowest-numbered unchecked task in §10. Split an oversized task here first (`T06a`, `T06b`).
2. **Tests first.** Every PR leaves `cargo test --workspace` green. Parsers are built against synthetic fixtures (§9).
3. **`kakeibo-core` is pure:** no I/O, clock or env. Parsers turn bytes into `TurnRecord`s; core does attribution, flags and pricing math. This keeps it deterministic and shareable (§11).
4. **Privacy is a hard rule.** No network, ever: no telemetry, update pings, price fetches or crash reports. `scripts/lint-privacy.sh` fails CI on `reqwest`, `hyper`, `ureq`, `std::net`, `URLSession`. Parsers measure each block (bytes, estimated tokens), keep tool names and hashes, and **drop text immediately**.
5. **Agent logs are read-only.** Read-only file handles; SQLite with `mode=ro` + `PRAGMA query_only`. Only `kakeibo-index` writes, inside Kakeibo's cache/config dirs (plus `export --out`). Tests use a temp `HOME` (`TestEnv`).
6. **Log formats are undocumented internals.** Everything marked *(verify)* is an assumption. Check the installed agent and record URL, agent version and date in `docs/formats/<agent>.md` before implementing. Unknown record types are counted and skipped, never a crash.
7. **Never invent numbers.** Prices, context windows and thresholds live in `data/*.toml` with `source` + `checked`, or as empty placeholders. Every figure is labelled `measured` (log-reported) or `estimated` (with method).
8. **No new dependencies** beyond §3.3 without an ADR (`docs/decisions/NNN-*.md`). When a task is done, tick it in §10 and update `CHANGELOG.md`.

## 1. Goals, non-goals, landscape

### 1.1 Goals (v1)
- **G1 Autopsy:** per-category and per-turn breakdown of input (fresh / cache write / cache read) and output for any session.
- **G2 Root-cause flags:** deterministic rules that say *why* a session was expensive, with evidence.
- **G3 Agents:** Claude Code and Codex CLI in the MVP; Gemini CLI, opencode, Aider, Cursor (best effort) after.
- **G4 Surfaces:** CLI tables, a `ratatui` TUI, an optional macOS menu bar app; JSON/Markdown export.
- **G5 Cost as config:** API-equivalent cost from a user-editable pricing table.
- **G6** fast on GB-scale logs (§7); **G7** Homebrew install.

### 1.2 Non-goals (v1)
Live quota meters that poll provider APIs. Proxying traffic. Uploading transcripts. Editing agent config (Bento's job, §11). A Windows GUI. Claims about how quotas are metered: we report tokens and API-equivalent cost, not quota % we can't see.

### 1.3 Landscape (checked 2026-10-31)
Existing tools answer **"how much"** or **"how much is left"**. Kakeibo answers **"why, and which turn"**.
- **[ccusage](https://ccusage.com/guide/):** reference CLI; daily/session/5-hour-block totals across many agents. No in-session attribution. (Its dedupe once kept partial streaming output, [#938](https://github.com/ryoppippi/ccusage/issues/938), a trap we test for.)
- **[Tokenz](https://www.pardel.dev/2026/10/10/tokenz-claude-code-and-codex-usage-in-the-menu-bar):** Swift menu bar; Claude Code + Codex plan % and 1/7/30-day totals from local logs. Totals only.
- **[UsageBar](https://github.com/abdelrahmanmagdii/usage-tracker):** 7-provider quota meter that polls provider endpoints with local credentials. Live limits, not forensics.
- **[Usage Desk](https://usage-desk.nikhilkr.in/):** macOS quotas/resets/accounts via the CLIs; doesn't scan transcripts.
- **Pacer** ([EricAndrechek/Pacer](https://github.com/EricAndrechek/Pacer), [dkremsa/claude-pacer](https://github.com/dkremsa/claude-pacer)): burn-rate pacing and forecasts. Shows *how fast*, not *what burned it*.
- **[Claude-Code-Usage-Monitor](https://github.com/Maciek-roboblog/Claude-Code-Usage-Monitor/):** Python terminal burn-rate and limit monitor.
- **[CodexBar](https://github.com/steipete/CodexBar), [TokenBar](https://github.com/Nanako0129/TokenBar-Tauri), [Token Use](https://tokenuse.app/):** multi-provider trackers; breadth over depth.
- **[tokz](https://github.com/tokz-dev/cli/blob/main/README.md):** closest neighbour; splits each turn's bill across tools, cache hit rate, unused-MCP warning. No resent-vs-fresh split or cold-resume/cache-miss causes; fetches pricing online by default.
- **[CodeBurn](https://github.com/getagentseal/codeburn/blob/main/README.md):** cost by task type and a "one-shot rate" (retry = same file re-edited after a shell command). Prior art for our retry rule.
- **Bento-like static auditors** ([ctxweight](https://github.com/GonzaloPeriane/ctxweight), [claude-context-lint](https://github.com/skibidiskib/claude-context-lint), Claude Code [`/context`](https://code.claude.com/docs/en/commands), which can under-report MCP schemas, [#50061](https://github.com/anthropics/claude-code/issues/50061)): estimate overhead from config, not actual spend.

**How Kakeibo differs:** (1) **attribution per category and per turn**, reconciled to each request's measured prompt size; (2) **cache hits vs misses** shown separately, with the likely cause of each miss (cold resume, TTL expiry, prefix change); (3) **plain-words root-cause flags**. Motivating case: [anthropics/claude-code#100447](https://github.com/anthropics/claude-code/issues/100447), where the reporter's fresh Pro 5-hour window was gone ~4 minutes after auto-resuming a session at 961.8k/1M context (they report 142.6M cache-read tokens over 191 requests). Related: [#46047](https://github.com/anthropics/claude-code/issues/46047), [#49302](https://github.com/anthropics/claude-code/issues/49302). Kakeibo can't see Anthropic's meter, but it can show that each request resent the whole context and which turns did it.

### 1.4 Name clash check (2026-10-31)
- **Homebrew:** no `kakeibo` formula or cask (formulae.brew.sh API 404); `kakeibo-bar` free.
- **crates.io / npm / PyPI:** `kakeibo` free on all three. (`kkb` is taken on crates.io and PyPI by unrelated packages, so no `kkb` alias is shipped.)
- **GitHub:** "kakeibo" is a common Japanese word (household account book): ~3.8k small repos, mostly finance apps, none notable. Near-namesake in our space: [`Leif-Wang-021/token-kakeibo`](https://github.com/Leif-Wang-021/token-kakeibo), an unmaintained opencode Go usage viewer; the README says we're unrelated. `quiinthieu/kakeibo` is free.
- **Chosen:** binary/crate/formula **`kakeibo`** (`quiinthieu/tap/kakeibo`), cask **`kakeibo-bar`**, bundle id **`io.github.quiinthieu.kakeibo`**. README suggests `alias kkb=kakeibo`. Re-check before T17.

---

## 2. Glossary
**Turn:** one model API request with its usage record. **P(t):** total prompt of request t = fresh input + cache write + cache read. **Fixed overhead:** what's in P before any conversation. **Carry:** content added at turn k and resent in every later prompt. **Cold request:** mostly cache *write* where a *read* was expected. **Flag:** a deterministic finding with evidence. **Ledger:** the text-free stream of `TurnRecord`s.

## 3. Architecture

**Stack: Rust core, CLI and TUI, plus an optional thin Swift menu bar app.**
1. Rust: one static cross-platform binary that streams multi-GB JSONL fast (serde, memchr, rayon), plus `ratatui` and bundled read-only SQLite for opencode/Cursor.
2. The menu bar is a thin SwiftUI `MenuBarExtra` reading a versioned `snapshot.json`; no logic in Swift (Niwa's pattern).
3. Local-only by construction: network crates are linted out and text is dropped at parse time.

### 3.1 Crates and app
| Unit | Responsibility |
|---|---|
| `kakeibo-core` | Pure: `TurnRecord`, attribution, flag rules, pricing math, `product.rs`. **The shareable core (§11).** |
| `kakeibo-parse` | Per-agent discovery (`PathCatalog`) + streaming decoders → `TurnRecord`s; block sizing. Read-only. |
| `kakeibo-index` | Incremental cache (offsets, inode/mtime, derived records only), `config.toml`, `snapshot.json`. **Only writer.** |
| `kakeibo-report` | Table, Markdown, JSON, NDJSON renderers; TUI widgets. |
| `kakeibo` | Binary: clap CLI + ratatui TUI. |
| `macos/KakeiboBar` | Swift 6 `MenuBarExtra` (macOS 14+ *(verify)*). |

Graph: `kakeibo → {core, parse, index, report}`; `parse`, `index`, `report → core`; `core →` nothing. `DependencyGraphTests` checks it.

### 3.2 Flow
`discover → index (skip unchanged files, resume appended ones) → parse → dedupe → attribute → flag → price → report`. `--no-cache` must give identical results (tested).

### 3.3 Allowed dependencies
`serde`, `serde_json`, `toml`, `clap`, `ratatui`, `crossterm`, `directories`, `rusqlite` (bundled), `memchr`, `rayon`, `zstd` (only if Codex `.jsonl.zst` is confirmed), `thiserror`, `anyhow` (bin only), `jiff` or `time` (ADR), `sha2`, `tiktoken-rs` *or* an in-house heuristic (ADR in T06). Dev: `insta`, `proptest`, `assert_cmd`, `tempfile`, `criterion`. Swift: none.

## 4. Data model (`kakeibo-core`)
```rust
pub struct TurnRecord {               // one deduped API request; no text anywhere
    pub agent: AgentId, pub session_id: String, pub turn: u32, pub ts: Timestamp,
    pub model: Option<String>, pub cwd_hash: Option<String>,
    pub usage: Usage,                 // normalised (§5)
    pub added: Vec<ContentBlock>,     // content new since the previous request
    pub hints: TurnHints,             // resume_start, compaction, sidechain, api_error
}
pub struct Usage { pub fresh_in: u64, pub cache_write: u64, pub cache_read: u64,
                   pub output: u64, pub reasoning: Option<u64>, pub confidence: Confidence }
pub struct ContentBlock { pub kind: BlockKind, pub est_tokens: u64, pub bytes: u64,
                          pub tool: Option<String>, pub key_hash: Option<String> } // hash(tool+args)
pub enum Category { FixedOverhead, ResentHistory, ToolOutput, Retries, UserInput,
                    AssistantOutput, Subagents, Compaction, Unattributed }
```
`docs/ledger-schema.md` defines the NDJSON form (`"schema": "kakeibo.ledger/1"`), the contract Bento can consume.

## 5. Agent sources (all *(verify)*; record findings in `docs/formats/`)
**Normalisation:** Anthropic-style usage keeps fresh, cache-write and cache-read disjoint; OpenAI/Gemini-style `input` *includes* cached tokens ([comparison](https://agentminutes.dev/docs/token-comparison/)). Parsers map both to `fresh_in/cache_write/cache_read` (property-tested).

| Agent | Location | Usage fields & gotchas |
|---|---|---|
| **Claude Code** | `~/.claude/projects/<cwd-slug>/<session>.jsonl`, top level only ([bough](https://www.bough.run/docs/format/claude-code), [claude-dev.tools](https://claude-dev.tools/docs/jsonl-format)) | `assistant` lines: `message.usage.{input_tokens, output_tokens, cache_creation_input_tokens, cache_read_input_tokens}` + 5m/1h split, `message.model`, `cwd` ([notes](https://github.com/FullFran/claudeops-tui/blob/main/docs/jsonl-format.md)). Dedupe on `message.id`/`requestId`, keep **max output** ([ccusage#938](https://github.com/ryoppippi/ccusage/issues/938)). Tool results are `tool_result` blocks in `user` lines. Resume/compaction/sidechain markers, `CLAUDE_CONFIG_DIR` *(verify)*. |
| **Codex CLI** | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`, `archived_sessions/`, `CODEX_HOME` ([Token Use](https://tokenuse.app/docs/development/tools/codex/)) | `event_msg` `token_count`: `last_token_usage`/`total_token_usage {input_tokens, cached_input_tokens, output_tokens, reasoning_output_tokens}`, `rate_limits.primary.used_percent` ([codex#22354](https://github.com/openai/codex/issues/22354)); newer `token_usage_record` with `response_id` ([bough](https://www.bough.run/docs/format/codex)). Cached is **inside** input; totals are cumulative; never read record + event both. Forks carry parent history. `.jsonl.zst` *(verify)*. |
| **Gemini CLI** | `~/.gemini/tmp/<slug>/chats/session-*.jsonl` (legacy `.json`) ([docs](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/session-management.md), [schema](https://seanthegeek.net/doubleagent/analyzer/research/gemini-cli.html)) | `tokens {input, output, cached, thoughts, tool, total}` on `gemini` messages ([types](https://github.com/google-gemini/gemini-cli/blob/main/packages/core/src/services/chatRecordingTypes.ts)); cached inside input; apply `$rewindTo`/`$set`/`$patch`. |
| **opencode** | `~/.local/share/opencode/opencode.db` ([docs](https://opencode.ai/docs/troubleshooting/)); legacy `storage/` ([ccusage#966](https://github.com/ccusage/ccusage/issues/966)) | `message.data.tokens {input, output, reasoning, cache{read,write}}`, `cost`; tool I/O in `part`; `session.parent_id` = subagent ([notes](https://github.com/repowise-dev/skiplevel/blob/main/docs/schema-notes-opencode.md)). Live WAL DB: read-only + busy timeout. |
| **Aider** | `.aider.chat.history.md` per repo; opt-in `--analytics-log` ([options](https://aider.chat/docs/config/options.html), [analytics](https://aider.chat/docs/more/analytics.html)) | Rounded `Tokens: Nk sent, Nk received` lines; analytics `prompt_tokens`/`completion_tokens`/`cost` *(verify)*. No cache split: `estimated`, totals only. |
| **Cursor** (best effort) | `~/Library/Application Support/Cursor/User/globalStorage/state.vscdb`, `cursorDiskKV` ([session-viz](https://session-viz.com/blog/reading-cursors-chat-history/)) *(verify)* | `bubbleId:*` → `tokenCount.{inputTokens, outputTokens}`, often 0 ([forum](https://forum.cursor.com/t/cursordiskkv-table-records-always-show-0-for-tokencount/155984)): show "not recorded", never 0. |

## 6. Engine

### 6.1 Attribution
Per request we **measure** P(t), its fresh/write/read split and output; we **estimate** its contents:
1. **Fixed overhead** = P(1) − est(first user prompt); split further only with a Bento budget file (§11).
2. **Added content** = blocks new since t−1 (user prompt, prior assistant output, tool calls/results, subagent results), estimated and scaled so Σ ≈ P(t) − P(t−1). The remainder goes to `Unattributed`, never hidden.
3. **Carry:** a block stays in every later prompt until compaction or rewind; its carried cost is summed over later turns and split by how each was billed. This is how a 40K tool dump becomes a multi-million-token line.
4. **Category** follows block kind; `Retries` = repeated `key_hash` after a failure; `ResentHistory` = cache-read share of carried content. Category sums reconcile to measured totals (property test).

### 6.2 Cost
- `data/pricing.toml`: per model × class (`input`, `cache_write_5m`, `cache_write_1h`, `cache_read`, `output`, `reasoning`), plus `currency`, `source`, `checked`. **Shipped with empty values and source URLs** (Anthropic, OpenAI, Google pricing pages). `pricing check` lists models seen without a price; unpriced shows "unpriced", never $0.
- `pricing import <file>` reads a user-downloaded LiteLLM [`model_prices_and_context_window.json`](https://github.com/BerriAI/litellm/blob/main/model_prices_and_context_window.json) *(verify fields)*, so Kakeibo itself makes no network call. `data/models.toml` holds context windows with sources.
- Costs are **API-equivalent**: a value indicator on subscriptions, not a bill or a quota %. opencode's own `cost` is shown as "agent-reported".

### 6.3 Flags (`data/flags.toml`; thresholds tunable, defaults are guesses to calibrate)
| Id | Fires when | Evidence |
|---|---|---|
| `cold-resume` | first request after resume/fork or an idle gap > cache TTL has cache_write ≥ X% of P and P ≥ N | gap, P, tokens rewritten; link to #100447 |
| `near-full-context` | P ≥ Y% of window for ≥ K requests | turns, tokens resent |
| `tool-output-bloat` | a tool result ≥ N tokens carried ≥ K turns | tool, size, carried cost |
| `cache-miss-storm` | ≥ M mid-session requests with write ≫ read (model switch, edited rules, MCP change) | range, likely trigger |
| `retry-loop` | repeated `key_hash` after error, API retries, edit→test-fail→edit on one file | attempts, tokens |
| `duplicate-reads` | same file read ≥ R times with no edit between | count, carried cost |
| `heavy-overhead` | P(1) above threshold or user median | P(1), "try `bentoctx budget`" |
| `subagent-fanout` | subagents > S% of parent | per-subagent totals |
| `compaction-cost` | compaction request + rebuild | before/after |
| `quota-jump` (Codex) | `used_percent` rises ≥ Q points within ≤ J turns | turns in window |
Flags rank by tokens (or cost) and render as one sentence plus evidence; none claims a provider-side cause.

### 6.4 Surfaces
```
kakeibo                          # recent sessions: tokens, cost, top flag
kakeibo sessions [--agent A] [--since 7d] [--project PATH]
kakeibo autopsy <id|latest> [--turns] [--category C]
kakeibo flags [--since 7d]       kakeibo top --by category|tool|project|flag
kakeibo tui                      kakeibo export <id|--since> --format json|md|ndjson [--out F]
kakeibo pricing list|check|import <file>
kakeibo doctor                   # sources found, versions, unknown-record counts
kakeibo snapshot                 # snapshot.json for the menu bar
```
- **TUI:** session list → autopsy (stacked per-turn bars by category, cache read/write hatching) → flags with jump-to-turn. `NO_COLOR` honoured.
- **Menu bar (optional):** today's worst flag as a glyph; popover with top 3 flags; watches `snapshot.json` only.
- **Export:** JSON (`schemaVersion: 1`), Markdown (projects hashed unless `--show-paths`), NDJSON ledger.

## 7. Performance budget (targets for T16, not claims)
Bounded-memory streaming (lines > 16 MB skipped with a diagnostic), rayon across files, `memchr` pre-filter so only usage lines get a full parse, index resume from the last newline. Targets on the CI macOS runner, release build: 1 GB synthetic corpus cold ≤ 10 s, warm ≤ 300 ms, peak RSS ≤ 300 MB. If one is missed, file an issue; don't edit the number.

## 8. Repo layout
`crates/{kakeibo-core,kakeibo-parse,kakeibo-index,kakeibo-report,kakeibo}/` · `data/{pricing,models,flags}.toml` · `macos/` (XcodeGen, `Config/Product.xcconfig`) · `packaging/homebrew/` · `scripts/{lint-privacy.sh,lint-fixtures.sh,gen-fixtures/,bench.sh}` · `docs/{formats,decisions}/`, `docs/{ledger-schema,cli-schema,snapshot-schema,privacy,perf}.md` · `tests/fixtures/<agent>/<scenario>/` · `Cargo.toml`, `deny.toml`, `CHANGELOG.md`, `PLAN.md`, `AGENTS.md`, `CLAUDE.md`.

## 9. Test strategy
- **Synthetic fixtures only:** `scripts/gen-fixtures/` emits each agent's shape plus ground truth (`expected.json`). Scenarios: `basic`, `streaming-dupes`, `resume-cold` (large synthetic context resumed after a gap), `tool-bloat`, `retry-loop`, `cache-miss-storm`, `compaction`, `subagents`, `codex-dual-usage`, `gemini-rewind`, `opencode-wal`, `hostile` (truncated line, 20 MB line, unknown types, non-UTF-8). `lint-fixtures.sh` rejects real-looking home paths, emails and key-like strings.
- **Unit:** normalisation, attribution reconciliation, carry math, each flag (fires on its scenario, silent on `basic`).
- **Property (`proptest`):** categories sum to measured totals; dedupe idempotent; chunked/appended parsing equals one pass; cached equals uncached.
- **Golden (`insta`):** autopsy JSON/Markdown/table per scenario; TUI frames via `TestBackend`.
- **Privacy:** marker strings planted in fixture text must appear zero times in the index, snapshot and exports. Read-only proof: fixture tree `chmod -R a-w`, hashes unchanged.
- **macOS:** XCTest for snapshot decoding and the CLI wrapper. **Perf:** `criterion` + `bench.sh` on a generated 1 GB corpus.

## 10. Milestones & tasks
Each task fits one agent session. **M1 is the MVP**: autopsy and flags for Claude Code + Codex in the terminal.

### M0 Skeleton
- [x] **T01 Workspace & tooling.** §3.1 crates as stubs, `product.rs`, `rust-toolchain.toml`, `deny.toml`, `ci.yml`, both lints, `DependencyGraphTests`.
  *AC:* CI green on macOS + Linux; `--version` from `product.rs`; lints catch a planted `std::net` and a planted `/Users/alice`.
- [x] **T02 Fixture generator & harness.** `gen-fixtures` with ground truth for Claude/Codex `basic`, `streaming-dupes`, `codex-dual-usage`; `TestEnv`; insta; `docs/formats/{claude-code,codex}.md` from a real install (field names only).
  *AC:* byte-stable fixtures for a fixed seed; every used field cites a source.

### M1 MVP: Claude Code + Codex autopsy in the terminal
- [x] **T03 Core model & ledger.** §4 types, normalisation for both conventions, `ledger-schema.md`, NDJSON I/O.
  *AC:* round-trip tests; `fresh+write+read == P` property for both conventions.
- [x] **T04 Claude Code parser.** Discovery, streaming, dedupe (max output), block sizing with text dropped, tool names/`key_hash`, resume/compaction/sidechain hints *(verify)*.
  *AC:* `basic`/`streaming-dupes` match `expected.json` exactly; `hostile` → diagnostics, no panic; privacy grep clean.
- [x] **T05 Codex parser.** Rollouts (+ archived, `CODEX_HOME`), `session_meta`/`turn_context`, usage precedence, fork history, `rate_limits`.
  *AC:* `codex-dual-usage` not double counted; cumulative-only fixture gives correct deltas; cached never added to input.
- [x] **T06 Attribution engine.** Estimator ADR, scaling, carry, categories, `Unattributed`.
  *AC:* reconciliation property on all fixtures; `tool-bloat` carried cost equals a hand-computed value.
- [ ] **T07 Flags v1.** `cold-resume`, `near-full-context`, `tool-output-bloat`, `cache-miss-storm`, `heavy-overhead`.
  *AC:* each fires on its scenario with the right turn range, silent on `basic`; text golden.
- [ ] **T08 CLI & export (MVP release).** default view, `sessions`, `autopsy`, `flags`, `top`, `export`, `doctor`.
  *AC:* `assert_cmd` per subcommand; JSON matches `cli-schema.md`; Markdown pathless by default; `v0.1.0`.

### M2 Cost, more agents, scale
- [ ] **T09 Pricing as config.** Schemas with `source`/`checked`, empty defaults, `pricing list|check|import`, per-category/turn cost, 5m/1h write split.
  *AC:* no price literals in code (lint); unpriced never shows $0; import tested on a tiny synthetic LiteLLM-shaped file.
- [ ] **T10 Gemini CLI + opencode.** JSONL/JSON with rewind/patch and slug registry; read-only SQLite + legacy tree.
  *AC:* scenarios match ground truth; DB unchanged (hash + mtime) while another connection holds a write lock.
- [ ] **T11 Aider + Cursor (best effort).** History/analytics parsing; `state.vscdb` reader.
  *AC:* fixtures parse; zero Cursor counts render as "not recorded"; all labelled `estimated`.
- [ ] **T12 Incremental index & perf.** Index (ADR: SQLite vs files), offset resume, truncation detection, rayon, pre-filter.
  *AC:* cached == uncached (property); append re-reads only new bytes (counter); 1 GB bench in `docs/perf.md`.

### M3 Deeper flags & surfaces
- [ ] **T13 TUI.** List, autopsy bars, flags pane, filters, help.
  *AC:* goldens at 80×24 and 120×40; no colour-only cues; keyboard-only test.
- [ ] **T14 Flags v2.** `retry-loop`, `duplicate-reads`, `subagent-fanout`, `compaction-cost`, `quota-jump`.
  *AC:* each fires only on its scenario; `quota-jump` names the turns in the window.
- [ ] **T15 macOS menu bar.** `snapshot` + schema doc, XcodeGen app, glyph + top-3 popover, "Open in TUI", launch at login (`SMAppService` *(verify)*).
  *AC:* XCTests pass; helpful state when `kakeibo` is missing; no timers beyond the snapshot watch.

### M4 Polish & release
- [ ] **T16 Perf pass & Bento interop.** §7 targets measured; JSON Schema for the ledger that Bento can vendor; `--bento-budget <file>` reads a `bentoctx budget --json` export to split fixed overhead.
  *AC:* `perf.md` has measured values or filed issues; schema validation on every fixture; split appears only with a Bento file, labelled estimated.
- [ ] **T17 Packaging & Homebrew.** `release.yml` (macOS universal, Linux x86_64/arm64, checksums), notarised `KakeiboBar.app`, tap `quiinthieu/homebrew-tap` with formula `kakeibo` + cask `kakeibo-bar` (`depends_on formula:` *(verify)*). Re-run §1.4 first.
  *AC:* `brew audit --strict` and a source install pass in CI; tag dry run builds every artifact.
- [ ] **T18 Docs & v1.0.** README (screenshots from synthetic fixtures), `privacy.md`, uninstall guide, `v1.0.0`.
  *AC:* fresh-VM install→uninstall works; a full run opens zero network connections (`nettop` or sandbox profile).

## 11. Relationship to Bento
**Overlap.** Bento (`bentoctx`, Swift, macOS) audits what agents load *before* you type (inventory, pre-prompt budget, hygiene, fixes) and already plans `BentoLogIngest` with Claude dedupe and Codex `token_count` decoders for usage joins and calibration (Bento PLAN §5.2, §6.2, T11–T12). Kakeibo explains what happened *during* sessions. Shared ground: the **log parsers** and the **fixed-overhead number** (Bento estimates it from config; Kakeibo measures it as P(1)).

**Recommendation: a standalone product with a shared core contract, not a Bento module (for now).**
- Different job and reach: Kakeibo is a cross-platform forensic CLI (Linux/WSL users hit quota drains too); Bento is a notarised macOS GUI that edits config. Merging would make Kakeibo macOS-only or bloat Bento.
- Share through data, not code: `kakeibo.ledger/1` NDJSON is the core contract. Bento can shell out to `kakeibo export --format ndjson` when it's installed and skip its own decoders, or keep its Swift decoders and test them against **the same synthetic fixture corpus**. Either way, a format change is fixed in one place.
- Two-way link: `heavy-overhead` points to `bentoctx budget`; Kakeibo reads a Bento budget export to split fixed overhead (T16); Bento's calibration can use Kakeibo's measured P(1) per repo.
- Keep `kakeibo-core` pure and `kakeibo-parse` CLI-free so a UniFFI/C-ABI binding can expose them to Swift later. Revisit merging (ADR) once both MVPs ship.

## 12. Risks & mitigations
*Format churn:* `PathCatalog`, `docs/formats`, tolerant decoders, `doctor`. *Double counting:* per-agent dedupe + ground-truth fixtures. *Estimates read as facts:* `Confidence` everywhere. *Quota opacity:* no quota % except Codex's logged `used_percent`. *Privacy:* text dropped at parse, lints, privacy grep.

## 13. v2 backlog (don't implement in v1)
Copilot CLI (`~/.copilot/session-state/*/events.jsonl` per [ccusage](https://ccusage.com/guide/)), Amp, Kiro; opt-in Claude status-line hook saving `rate_limits`; live cold-resume warning; per-repo trends; Linux tray; UniFFI core binding for Bento.
