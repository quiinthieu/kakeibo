# ADR 002: Pure Byte-Ceiling Block Token Estimator with Turn-Delta Reconciliation

- **Status:** Accepted
- **Date:** 2026-10-31
- **Task:** T06 (`Attribution engine`)
- **References:** `PLAN.md` §0 #3, §0 #4, §0 #7, §0 #8, §3.3, §4, §6.1, §7, §11

## Context

`PLAN.md` §3.3 requires an ADR in Task T06 selecting between `tiktoken-rs` and an in-house heuristic for estimating individual `ContentBlock` token sizes when attributing per-turn token usage across the nine `Category` variants (`FixedOverhead`, `ResentHistory`, `ToolOutput`, `Retries`, `UserInput`, `AssistantOutput`, `Subagents`, `Compaction`, `Unattributed`).

Per-turn API usage records report **measured** request totals (`fresh_in`, `cache_write`, `cache_read`, and `output`), whereas individual blocks inside a turn (`UserPrompt`, `ToolResult`, `ToolUse`, `AssistantText`, `AssistantThinking`, `SubagentResult`, `CompactionSummary`, `SystemContext`) do not carry provider-reported per-block token counts.

Any block estimator chosen for Kakeibo must satisfy five constraints simultaneously:

1. **Immediate text drop & ledger parity (`PLAN.md` §0 #4, §4, §11):** Parsers measure each block at decode time and drop raw transcript text immediately. Shared `kakeibo.ledger/1` NDJSON streams retain only `(kind, bytes, est_tokens, tool, key_hash, is_error)`. Attribution in `kakeibo-core` must produce identical results on live `TurnRecord`s and reloaded NDJSON ledgers without access to raw text.
2. **Purity (`PLAN.md` §0 #3):** `kakeibo-core` performs no I/O, reads no clock or environment, and must execute deterministically across macOS and Linux without floating-point rounding drift.
3. **Multi-provider neutrality (`PLAN.md` §1.1, §5):** Kakeibo analyses Claude Code (Anthropic tokenizer), Codex CLI (OpenAI `o200k_base` / GPT-5 BPE), Gemini CLI (SentencePiece / Gemma-family vocabulary), opencode, Aider, and Cursor. No single provider vocabulary matches all six sources.
4. **Streaming throughput (`PLAN.md` §7):** Cold scans target `<= 10 s` and `<= 300 MB` RSS across a 1 GB JSONL corpus; running backtracking regular-expression pre-tokenisation over multi-megabyte tool dumps on every line works directly against the streaming budget.
5. **Exact measured reconciliation (`PLAN.md` §6.1, §9):** Summed across `Category::ALL`, attributed `(fresh_in, cache_write, cache_read, output)` must reconcile identically to each turn's measured `TurnRecord.usage`, routing unexplained prompt jumps to `Category::Unattributed` rather than hiding them.

## Decision

We select an **in-house pure byte-ceiling heuristic with turn-delta Hamilton integer reconciliation** in `kakeibo-core::attrib`, adding **zero** external crates:

1. **Block pre-estimate (`estimate_tokens_from_bytes`):**
   ```rust
   pub const fn estimate_tokens_from_bytes(bytes: u64) -> u64 {
       bytes.div_ceil(4)
   }
   ```
   Both `kakeibo-parse` and `kakeibo-core` share this single `const fn` (`ceil(utf8_bytes / 4)`), handling dense English prose, JSON, and source code (~3.5–4.2 UTF-8 bytes/token) in $O(1)$ time after raw text is dropped.

2. **Turn-delta proportional scaling with physical UTF-8 byte ceiling:**
   On turn $t \ge 2$, newly entering prompt blocks $B_{\text{new}}(t) = \text{output\_blocks}(t - 1) \cup \text{input\_blocks}(t)$ are scaled against measured prompt growth $\Delta P(t) = \max(0, P(t) - P_{\text{carried}}(t - 1))$ using pure `u128` Largest-Remainder (Hamilton) integer apportionment, capped at each block's physical byte ceiling:
   $$w(b) \le \max(\text{bytes}(b), \text{est\_tokens}(b))$$
   Because modern byte-level BPE tokenizers encode at most one token per UTF-8 byte (`tokens(text) <= utf8_bytes(text)`), capping scaled prompt tokens at UTF-8 byte length prevents a tiny block (such as a 2-byte `"ok"` user prompt) from absorbing an unlogged 50,000-token MCP schema or system-context injection.

3. **Transparent `Category::Unattributed` residual:**
   Whenever $\Delta P(t) > \sum_{b \in B_{\text{new}}(t)} \max(\text{bytes}(b), \text{est\_tokens}(b))$ (or when $B_{\text{new}}(t)$ is empty while $P(t)$ grows), the exact residual $\Delta P(t) - \sum w(b)$ is emitted as an explicit `Category::Unattributed` prompt segment.

4. **Deterministic `u128` Hamilton billing apportionment & provenance:**
   Prompt billing tiers are apportioned across active prompt segments in prefix-cache order (`cache_read` to carried prefix segments first, then `cache_write`, then `fresh_in`) via `u128` integer division and deterministic remainder tie-breaking (`remainder DESC, spare_capacity DESC, segment_index ASC`). Every attributed breakdown records `Confidence::Estimated` paired with `ESTIMATION_METHOD_SCALED_BYTES_DIV4 = "utf8_bytes_div4_ceil_scaled"`, while turn and session totals retain `TurnRecord.usage.confidence` (`Measured` when log-reported).

## Alternatives Considered

### `tiktoken-rs`

- **Pros:** Exact BPE token counts for OpenAI `cl100k_base` / `o200k_base` models when raw text is available at parse time.
- **Cons:**
  - Pulls in embedded multi-megabyte BPE rank assets and `fancy-regex` backtracking pre-tokenisers, increasing binary size, compile time, and per-line CPU cost on multi-megabyte `tool_result` lines (`PLAN.md` §7).
  - Still an approximation for Anthropic (Claude Code) and Google (Gemini CLI) models, which do not publish OpenAI-identical BPE vocabularies, yet creates a false appearance of exactness before turn-delta scaling.
  - Cannot re-run inside `kakeibo-core` on text-free `kakeibo.ledger/1` NDJSON files because raw text has already been discarded at parse time.

### Uncapped proportional scaling (`est_tokens` without byte ceiling)

- **Pros:** Simpler single-step scaling formula.
- **Cons:** Violates physical tokenisation bounds whenever an agent injects unlogged prompt content mid-session (e.g. lazy-loaded MCP tool schemas or dynamic system reminders): a 4-byte `"ship"` prompt on a turn where $P(t)$ jumps by `+40,000` tokens would wrongly attribute all `40,000` tokens to `Category::UserInput` instead of `Category::Unattributed`.

## Consequences

- `kakeibo-core` remains 100% pure, fast, and dependency-minimal (`Cargo.toml` unchanged).
- `kakeibo-parse::hash::estimate_tokens` delegates directly to `kakeibo_core::attrib::estimate_tokens_from_bytes`, guaranteeing a single estimator definition across parsing and attribution.
- Per-turn and per-session category sums reconcile identically to `TurnRecord.usage` across all 4 billing components (`fresh_in`, `cache_write`, `cache_read`, `output`), verified by `proptest` and synthetic ground-truth fixtures.
