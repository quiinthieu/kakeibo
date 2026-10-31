//! Pure per-turn and per-session token attribution and carry engine (`PLAN.md` §6.1, `ADR 002`).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::model::{BlockKind, Category, Confidence, ContentBlock, TurnRecord};
use crate::usage::Usage;

/// Identifier recorded on attributed category breakdowns (`ADR 002`).
pub const ESTIMATION_METHOD_SCALED_BYTES_DIV4: &str = "utf8_bytes_div4_ceil_scaled";

/// Deterministic pure UTF-8 byte-to-token pre-estimator (`ceil(bytes / 4)`, `ADR 002`).
#[must_use]
pub const fn estimate_tokens_from_bytes(bytes: u64) -> u64 {
    bytes.div_ceil(4)
}

/// Disjoint four-component token split preserving prompt cache tiers and output (`PLAN.md` §4, §6.1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSplit {
    pub fresh_in: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub output: u64,
}

impl TokenSplit {
    /// All-zero token split.
    pub const ZERO: Self = Self {
        fresh_in: 0,
        cache_write: 0,
        cache_read: 0,
        output: 0,
    };

    /// Extracts the four normalised billing components from [`Usage`].
    #[must_use]
    pub const fn from_usage(usage: &Usage) -> Self {
        Self {
            fresh_in: usage.fresh_in,
            cache_write: usage.cache_write,
            cache_read: usage.cache_read,
            output: usage.output,
        }
    }

    /// Total prompt tokens `P = fresh_in + cache_write + cache_read` (saturating on `u64`).
    #[must_use]
    pub const fn prompt_tokens(self) -> u64 {
        self.fresh_in
            .saturating_add(self.cache_write)
            .saturating_add(self.cache_read)
    }

    /// Total prompt + output tokens (saturating on `u64`).
    #[must_use]
    pub const fn total_tokens(self) -> u64 {
        self.prompt_tokens().saturating_add(self.output)
    }

    /// Returns `true` when all four components are zero.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.fresh_in == 0 && self.cache_write == 0 && self.cache_read == 0 && self.output == 0
    }

    /// Component-wise saturating sum of two splits.
    #[must_use]
    pub const fn saturating_add(self, rhs: Self) -> Self {
        Self {
            fresh_in: self.fresh_in.saturating_add(rhs.fresh_in),
            cache_write: self.cache_write.saturating_add(rhs.cache_write),
            cache_read: self.cache_read.saturating_add(rhs.cache_read),
            output: self.output.saturating_add(rhs.output),
        }
    }

    /// Accumulates `rhs` into `self` using saturating integer addition.
    pub fn add_assign(&mut self, rhs: Self) {
        *self = self.saturating_add(rhs);
    }
}

const fn category_index(category: Category) -> usize {
    match category {
        Category::FixedOverhead => 0,
        Category::ResentHistory => 1,
        Category::ToolOutput => 2,
        Category::Retries => 3,
        Category::UserInput => 4,
        Category::AssistantOutput => 5,
        Category::Subagents => 6,
        Category::Compaction => 7,
        Category::Unattributed => 8,
    }
}

/// Fixed-size nine-category token breakdown indexed by [`Category`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CategoryBreakdown {
    buckets: [TokenSplit; Category::ALL.len()],
}

impl CategoryBreakdown {
    /// Creates an all-zero breakdown across [`Category::ALL`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buckets: [TokenSplit::ZERO; Category::ALL.len()],
        }
    }

    /// Returns the [`TokenSplit`] attributed to `category`.
    #[must_use]
    pub const fn get(&self, category: Category) -> TokenSplit {
        self.buckets[category_index(category)]
    }

    /// Returns a mutable reference to `category`'s [`TokenSplit`].
    pub fn get_mut(&mut self, category: Category) -> &mut TokenSplit {
        &mut self.buckets[category_index(category)]
    }

    /// Adds `split` into `category`.
    pub fn add(&mut self, category: Category, split: TokenSplit) {
        self.buckets[category_index(category)].add_assign(split);
    }

    /// Accumulates every bucket from `other` into `self`.
    pub fn add_breakdown(&mut self, other: &Self) {
        for cat in Category::ALL {
            self.add(cat, other.get(cat));
        }
    }

    /// Iterates over all nine `(Category, TokenSplit)` pairs in [`Category::ALL`] order.
    pub fn iter(&self) -> impl Iterator<Item = (Category, TokenSplit)> + '_ {
        Category::ALL.into_iter().map(|cat| (cat, self.get(cat)))
    }

    /// Component-wise sum across all nine categories.
    #[must_use]
    pub fn sum(&self) -> TokenSplit {
        let mut total = TokenSplit::ZERO;
        for cat in Category::ALL {
            total.add_assign(self.get(cat));
        }
        total
    }
}

impl Serialize for CategoryBreakdown {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let map: BTreeMap<Category, TokenSplit> = self.iter().collect();
        map.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CategoryBreakdown {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let map = BTreeMap::<Category, TokenSplit>::deserialize(deserializer)?;
        let mut out = Self::new();
        for (cat, split) in map {
            *out.get_mut(cat) = split;
        }
        Ok(out)
    }
}

/// Per-block lifecycle and cumulative carry ledger (`PLAN.md` §6.1 #3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarriedBlockRecord {
    /// Monotonic 0-based index in [`SessionAttribution::carried_blocks`].
    pub block_id: usize,
    /// `TurnRecord.turn` on which this block appeared in `TurnRecord.added`.
    pub source_turn: u32,
    /// `TurnRecord.turn` on which this block first entered prompt `P(t)`,
    /// or `None` if it was generated as final-turn output or compacted before entering `P`.
    pub born_turn: Option<u32>,
    /// `TurnRecord.turn` before which this block was evicted by compaction or prompt shrink.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evicted_before_turn: Option<u32>,
    pub kind: BlockKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_hash: Option<String>,
    pub is_error: bool,
    pub is_retry: bool,
    pub bytes: u64,
    pub est_tokens: u64,
    /// Scaled prompt tokens assigned when entering `P(born_turn)` (`<= max(bytes, est_tokens)`).
    pub scaled_prompt_tokens: u64,
    /// Billing split on `born_turn`.
    pub initial_split: TokenSplit,
    /// Number of later turns (`t > born_turn` prior to eviction) that re-sent this block.
    pub carried_turns: u32,
    /// Cumulative prompt tokens billed to re-send this block across all `carried_turns`.
    pub carried_split: TokenSplit,
}

impl CarriedBlockRecord {
    /// Total prompt tokens paid to re-send this block after `born_turn`.
    #[must_use]
    pub const fn carried_prompt_tokens(&self) -> u64 {
        self.carried_split.prompt_tokens()
    }

    /// Initial + carried billing split across the block's full lifetime in context.
    #[must_use]
    pub const fn lifetime_split(&self) -> TokenSplit {
        self.initial_split.saturating_add(self.carried_split)
    }

    /// Initial + carried prompt tokens across the block's full lifetime in context.
    #[must_use]
    pub const fn lifetime_prompt_tokens(&self) -> u64 {
        self.lifetime_split().prompt_tokens()
    }
}

/// Reconciled token attribution for one model API request (`PLAN.md` §6.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnAttribution {
    pub turn: u32,
    pub measured: TokenSplit,
    pub measured_confidence: Confidence,
    pub categories: CategoryBreakdown,
    pub category_confidence: Confidence,
    pub estimation_method: &'static str,
    pub new_unattributed_prompt_tokens: u64,
}

/// Rollup summary for one child subagent session folded into [`Category::Subagents`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentSessionSummary {
    pub session_id: String,
    pub turns: u32,
    pub totals: TokenSplit,
}

/// Complete session attribution, carry ledger, and subagent rollup (`PLAN.md` §6.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAttribution {
    pub fixed_overhead_tokens: u64,
    pub totals: TokenSplit,
    pub totals_confidence: Confidence,
    pub categories: CategoryBreakdown,
    pub category_confidence: Confidence,
    pub estimation_method: &'static str,
    pub turns: Vec<TurnAttribution>,
    pub carried_blocks: Vec<CarriedBlockRecord>,
    pub subagents: Vec<SubagentSessionSummary>,
}

const fn is_prompt_input_block(kind: BlockKind) -> bool {
    matches!(
        kind,
        BlockKind::UserPrompt
            | BlockKind::ToolResult
            | BlockKind::SubagentResult
            | BlockKind::CompactionSummary
            | BlockKind::SystemContext
    )
}

const fn merge_confidence(a: Confidence, b: Confidence) -> Confidence {
    match (a, b) {
        (Confidence::NotRecorded, _) | (_, Confidence::NotRecorded) => Confidence::NotRecorded,
        (Confidence::Estimated, _) | (_, Confidence::Estimated) => Confidence::Estimated,
        (Confidence::Measured, Confidence::Measured) => Confidence::Measured,
    }
}

const fn category_confidence_for(measured_conf: Confidence) -> Confidence {
    match measured_conf {
        Confidence::NotRecorded => Confidence::NotRecorded,
        Confidence::Measured | Confidence::Estimated => Confidence::Estimated,
    }
}

const fn block_base_category(blk: &CarriedBlockRecord) -> Category {
    if blk.is_retry {
        return Category::Retries;
    }
    match blk.kind {
        BlockKind::UserPrompt => Category::UserInput,
        BlockKind::AssistantText | BlockKind::AssistantThinking => Category::AssistantOutput,
        BlockKind::ToolUse | BlockKind::ToolResult => Category::ToolOutput,
        BlockKind::SubagentResult => Category::Subagents,
        BlockKind::CompactionSummary => Category::Compaction,
        BlockKind::SystemContext => Category::FixedOverhead,
    }
}

/// Pure `u128` Largest-Remainder (Hamilton) integer apportionment of `total` across `weights`.
///
/// Always returns a vector of length `weights.len()` summing identically to `total`
/// (when `weights` is non-empty). Zero-sum weights fall back to uniform weights.
fn apportion_hamilton(total: u64, weights: &[u64]) -> Vec<u64> {
    let n = weights.len();
    if n == 0 {
        return Vec::new();
    }
    if total == 0 {
        return vec![0; n];
    }

    let raw_sum: u128 = weights.iter().map(|&w| u128::from(w)).sum();
    let use_uniform = raw_sum == 0;
    let denom: u128 = if use_uniform { n as u128 } else { raw_sum };
    let total_u128 = u128::from(total);

    let mut quotas = vec![0_u64; n];
    let mut remainders: Vec<(u128, u64, usize)> = Vec::with_capacity(n);
    let mut assigned: u64 = 0;

    for (idx, &w) in weights.iter().enumerate() {
        let eff_w: u128 = if use_uniform { 1 } else { u128::from(w) };
        let prod = total_u128 * eff_w;
        let q = u64::try_from(prod / denom).expect("quotient bounded by u64 total");
        let rem = prod % denom;
        quotas[idx] = q;
        assigned = assigned.saturating_add(q);
        remainders.push((rem, w, idx));
    }

    let leftover = usize::try_from(total - assigned).expect("leftover bounded by n");
    if leftover > 0 {
        // Deterministic tie-breaking: largest remainder, largest weight, lowest index.
        remainders.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.2.cmp(&b.2))
        });
        for &(_, _, idx) in remainders.iter().take(leftover) {
            quotas[idx] += 1;
        }
    }

    quotas
}

/// Iterative capped Hamilton apportionment (discrete water-filling) using pure `u128` arithmetic.
///
/// Guarantees:
/// - `0 <= alloc[i] <= caps[i]` for every `i`, and
/// - `alloc.iter().sum::<u64>() == total.min(caps.iter().sum::<u64>())`.
fn apportion_capped_hamilton(total: u64, weights: &[u64], caps: &[u64]) -> Vec<u64> {
    debug_assert_eq!(weights.len(), caps.len());
    let n = weights.len();
    if n == 0 {
        return Vec::new();
    }

    let total_cap = caps.iter().copied().fold(0_u64, u64::saturating_add);
    let mut rem_target = total.min(total_cap);
    let mut alloc = vec![0_u64; n];
    if rem_target == 0 {
        return alloc;
    }

    let mut active: Vec<bool> = caps.iter().map(|&c| c > 0).collect();

    loop {
        if rem_target == 0 {
            break;
        }

        let active_indices: Vec<usize> = active
            .iter()
            .enumerate()
            .filter_map(|(i, &is_act)| is_act.then_some(i))
            .collect();
        if active_indices.is_empty() {
            break;
        }

        let sub_weights: Vec<u64> = active_indices.iter().map(|&i| weights[i]).collect();
        let deltas = apportion_hamilton(rem_target, &sub_weights);

        let mut any_saturated = false;
        for (&idx, &delta) in active_indices.iter().zip(deltas.iter()) {
            let rem_cap = caps[idx] - alloc[idx];
            if delta >= rem_cap {
                alloc[idx] = caps[idx];
                rem_target -= rem_cap;
                active[idx] = false;
                any_saturated = true;
            }
        }

        if !any_saturated {
            for (&idx, &delta) in active_indices.iter().zip(deltas.iter()) {
                alloc[idx] += delta;
            }
            break;
        }
    }

    alloc
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegmentSource {
    FixedOverhead,
    Block { block_id: usize },
    Unattributed,
}

#[derive(Debug, Clone, Copy)]
struct ActiveSegment {
    source: SegmentSource,
    is_prefix: bool,
    current_tokens: u64,
}

fn allocate_billing_tier(
    remaining_tier_budget: &mut u64,
    indices: &[usize],
    rem_caps: &mut [u64],
) -> Vec<(usize, u64)> {
    if *remaining_tier_budget == 0 || indices.is_empty() {
        return Vec::new();
    }
    let tier_caps: Vec<u64> = indices.iter().map(|&i| rem_caps[i]).collect();
    let tier_cap_sum = tier_caps.iter().copied().fold(0_u64, u64::saturating_add);
    let take = (*remaining_tier_budget).min(tier_cap_sum);
    if take == 0 {
        return Vec::new();
    }
    *remaining_tier_budget -= take;

    let allocs = apportion_capped_hamilton(take, &tier_caps, &tier_caps);
    let mut out = Vec::with_capacity(indices.len());
    for (&seg_idx, &tok) in indices.iter().zip(allocs.iter()) {
        rem_caps[seg_idx] -= tok;
        out.push((seg_idx, tok));
    }
    out
}

fn register_block(
    carried_blocks: &mut Vec<CarriedBlockRecord>,
    source_turn: u32,
    blk: &ContentBlock,
    is_retry: bool,
) -> usize {
    let block_id = carried_blocks.len();
    carried_blocks.push(CarriedBlockRecord {
        block_id,
        source_turn,
        born_turn: None,
        evicted_before_turn: None,
        kind: blk.kind,
        tool: blk.tool.clone(),
        key_hash: blk.key_hash.clone(),
        is_error: blk.is_error,
        is_retry,
        bytes: blk.bytes,
        est_tokens: blk.est_tokens,
        scaled_prompt_tokens: 0,
        initial_split: TokenSplit::ZERO,
        carried_turns: 0,
        carried_split: TokenSplit::ZERO,
    });
    block_id
}

/// Attributes a single session's [`TurnRecord`] stream across [`Category::ALL`].
#[must_use]
pub fn attribute_session(turns: &[TurnRecord]) -> SessionAttribution {
    attribute_session_with_subagents(turns, &[])
}

/// Attributes `parent_turns` and folds any child `subagent_sessions` into [`Category::Subagents`].
#[must_use]
pub fn attribute_session_with_subagents(
    parent_turns: &[TurnRecord],
    subagent_sessions: &[&[TurnRecord]],
) -> SessionAttribution {
    let mut fixed_overhead_tokens: u64 = 0;
    let mut saw_first_main_turn = false;
    let mut active_segments: Vec<ActiveSegment> = Vec::new();
    let mut pending_output_blocks: Vec<usize> = Vec::new();
    let mut failed_key_hashes: BTreeSet<String> = BTreeSet::new();

    let mut carried_blocks: Vec<CarriedBlockRecord> = Vec::new();
    let mut turn_attributions: Vec<TurnAttribution> = Vec::with_capacity(parent_turns.len());
    let mut session_categories = CategoryBreakdown::new();
    let mut session_totals = TokenSplit::ZERO;
    let mut overall_confidence = Confidence::Measured;

    for turn in parent_turns {
        let measured = TokenSplit::from_usage(&turn.usage);
        let measured_confidence = turn.usage.confidence;
        overall_confidence = merge_confidence(overall_confidence, measured_confidence);
        let cat_confidence = category_confidence_for(measured_confidence);

        // Inline sidechain turns execute in a subagent context without mutating parent carry.
        if turn.hints.sidechain {
            let mut cats = CategoryBreakdown::new();
            cats.add(Category::Subagents, measured);
            session_categories.add_breakdown(&cats);
            session_totals.add_assign(measured);
            turn_attributions.push(TurnAttribution {
                turn: turn.turn,
                measured,
                measured_confidence,
                categories: cats,
                category_confidence: cat_confidence,
                estimation_method: ESTIMATION_METHOD_SCALED_BYTES_DIV4,
                new_unattributed_prompt_tokens: 0,
            });
            continue;
        }

        // Partition `turn.added` into prompt-input blocks (present in `P(turn)`) and
        // response-output blocks (billed in `O(turn)`, entering `P` on the next main-chain turn).
        let mut input_block_ids = Vec::new();
        let mut output_block_ids = Vec::new();

        for blk in &turn.added {
            if is_prompt_input_block(blk.kind) {
                let is_retry = if blk.kind == BlockKind::ToolResult {
                    let was_failed = blk
                        .key_hash
                        .as_ref()
                        .is_some_and(|h| failed_key_hashes.contains(h));
                    if let Some(ref key) = blk.key_hash {
                        if blk.is_error {
                            failed_key_hashes.insert(key.clone());
                        } else {
                            failed_key_hashes.remove(key);
                        }
                    }
                    was_failed
                } else {
                    false
                };
                let id = register_block(&mut carried_blocks, turn.turn, blk, is_retry);
                input_block_ids.push(id);
            } else {
                let is_retry = turn.hints.api_error
                    || (blk.kind == BlockKind::ToolUse
                        && blk
                            .key_hash
                            .as_ref()
                            .is_some_and(|h| failed_key_hashes.contains(h)));
                let id = register_block(&mut carried_blocks, turn.turn, blk, is_retry);
                output_block_ids.push(id);
            }
        }

        let prompt_total = measured.prompt_tokens();
        let is_compaction = turn.hints.compaction
            || input_block_ids
                .iter()
                .any(|&id| carried_blocks[id].kind == BlockKind::CompactionSummary);
        let mut new_unattributed_prompt_tokens: u64 = 0;

        if !saw_first_main_turn {
            saw_first_main_turn = true;
            let in_weights: Vec<u64> = input_block_ids
                .iter()
                .map(|&id| carried_blocks[id].est_tokens)
                .collect();
            let alloc = apportion_capped_hamilton(prompt_total, &in_weights, &in_weights);
            let alloc_sum: u64 = alloc.iter().sum();
            let unlogged_fixed = prompt_total - alloc_sum;

            let mut sys_ctx_tokens: u64 = 0;
            active_segments.push(ActiveSegment {
                source: SegmentSource::FixedOverhead,
                is_prefix: true,
                current_tokens: unlogged_fixed,
            });
            for (&id, &tok) in input_block_ids.iter().zip(alloc.iter()) {
                carried_blocks[id].born_turn = Some(turn.turn);
                carried_blocks[id].scaled_prompt_tokens = tok;
                if carried_blocks[id].kind == BlockKind::SystemContext {
                    sys_ctx_tokens = sys_ctx_tokens.saturating_add(tok);
                }
                active_segments.push(ActiveSegment {
                    source: SegmentSource::Block { block_id: id },
                    is_prefix: false,
                    current_tokens: tok,
                });
            }
            fixed_overhead_tokens = unlogged_fixed.saturating_add(sys_ctx_tokens);
        } else if is_compaction {
            // Prior turn's response outputs were summarised before entering `P(turn)`.
            for id in pending_output_blocks.drain(..) {
                carried_blocks[id].evicted_before_turn = Some(turn.turn);
            }
            for seg in &active_segments {
                if let SegmentSource::Block { block_id } = seg.source {
                    if seg.current_tokens > 0 {
                        carried_blocks[block_id].evicted_before_turn = Some(turn.turn);
                    }
                }
            }
            active_segments.retain(|seg| matches!(seg.source, SegmentSource::FixedOverhead));
            for seg in &mut active_segments {
                seg.is_prefix = true;
            }
            if active_segments.is_empty() {
                active_segments.push(ActiveSegment {
                    source: SegmentSource::FixedOverhead,
                    is_prefix: true,
                    current_tokens: 0,
                });
            }

            let fixed_cap: u64 = active_segments.iter().map(|s| s.current_tokens).sum();
            let in_weights: Vec<u64> = input_block_ids
                .iter()
                .map(|&id| carried_blocks[id].est_tokens)
                .collect();
            let in_caps: Vec<u64> = input_block_ids
                .iter()
                .map(|&id| carried_blocks[id].bytes.max(carried_blocks[id].est_tokens))
                .collect();

            if prompt_total >= fixed_cap {
                let delta_p = prompt_total - fixed_cap;
                let alloc = apportion_capped_hamilton(delta_p, &in_weights, &in_caps);
                let alloc_sum: u64 = alloc.iter().sum();
                for (&id, &tok) in input_block_ids.iter().zip(alloc.iter()) {
                    carried_blocks[id].born_turn = Some(turn.turn);
                    carried_blocks[id].scaled_prompt_tokens = tok;
                    active_segments.push(ActiveSegment {
                        source: SegmentSource::Block { block_id: id },
                        is_prefix: false,
                        current_tokens: tok,
                    });
                }
                let unattrib = delta_p - alloc_sum;
                if unattrib > 0 {
                    new_unattributed_prompt_tokens = unattrib;
                    active_segments.push(ActiveSegment {
                        source: SegmentSource::Unattributed,
                        is_prefix: false,
                        current_tokens: unattrib,
                    });
                }
            } else {
                let mut all_weights = Vec::with_capacity(1 + in_weights.len());
                let mut all_caps = Vec::with_capacity(1 + in_caps.len());
                all_weights.push(fixed_cap);
                all_weights.extend_from_slice(&in_weights);
                all_caps.push(fixed_cap);
                all_caps.extend_from_slice(&in_caps);

                let alloc = apportion_capped_hamilton(prompt_total, &all_weights, &all_caps);
                active_segments[0].current_tokens = alloc[0];
                for (&id, &tok) in input_block_ids.iter().zip(&alloc[1..]) {
                    carried_blocks[id].born_turn = Some(turn.turn);
                    carried_blocks[id].scaled_prompt_tokens = tok;
                    active_segments.push(ActiveSegment {
                        source: SegmentSource::Block { block_id: id },
                        is_prefix: false,
                        current_tokens: tok,
                    });
                }
            }
        } else {
            for seg in &mut active_segments {
                seg.is_prefix = true;
            }
            let carried_cap: u64 = active_segments.iter().map(|s| s.current_tokens).sum();
            let new_block_ids: Vec<usize> = pending_output_blocks
                .drain(..)
                .chain(input_block_ids)
                .collect();
            let new_weights: Vec<u64> = new_block_ids
                .iter()
                .map(|&id| carried_blocks[id].est_tokens)
                .collect();
            let new_caps: Vec<u64> = new_block_ids
                .iter()
                .map(|&id| carried_blocks[id].bytes.max(carried_blocks[id].est_tokens))
                .collect();

            if prompt_total >= carried_cap {
                let delta_p = prompt_total - carried_cap;
                let alloc = apportion_capped_hamilton(delta_p, &new_weights, &new_caps);
                let alloc_sum: u64 = alloc.iter().sum();

                for (&id, &tok) in new_block_ids.iter().zip(alloc.iter()) {
                    carried_blocks[id].born_turn = Some(turn.turn);
                    carried_blocks[id].scaled_prompt_tokens = tok;
                    active_segments.push(ActiveSegment {
                        source: SegmentSource::Block { block_id: id },
                        is_prefix: false,
                        current_tokens: tok,
                    });
                }

                let unattrib = delta_p - alloc_sum;
                if unattrib > 0 {
                    new_unattributed_prompt_tokens = unattrib;
                    active_segments.push(ActiveSegment {
                        source: SegmentSource::Unattributed,
                        is_prefix: false,
                        current_tokens: unattrib,
                    });
                }
            } else {
                // Unmarked prompt shrink (`P(t) < P_carried(t-1)`): fit active + new segments to `P(t)`.
                let old_len = active_segments.len();
                let mut all_weights = Vec::with_capacity(old_len + new_weights.len());
                let mut all_caps = Vec::with_capacity(old_len + new_caps.len());
                for seg in &active_segments {
                    all_weights.push(seg.current_tokens);
                    all_caps.push(seg.current_tokens);
                }
                all_weights.extend_from_slice(&new_weights);
                all_caps.extend_from_slice(&new_caps);

                let alloc = apportion_capped_hamilton(prompt_total, &all_weights, &all_caps);
                for (seg, &tok) in active_segments.iter_mut().zip(&alloc[..old_len]) {
                    if tok == 0 && seg.current_tokens > 0 {
                        if let SegmentSource::Block { block_id } = seg.source {
                            carried_blocks[block_id].evicted_before_turn = Some(turn.turn);
                        }
                    }
                    seg.current_tokens = tok;
                }
                for (&id, &tok) in new_block_ids.iter().zip(&alloc[old_len..]) {
                    carried_blocks[id].born_turn = Some(turn.turn);
                    carried_blocks[id].scaled_prompt_tokens = tok;
                    active_segments.push(ActiveSegment {
                        source: SegmentSource::Block { block_id: id },
                        is_prefix: false,
                        current_tokens: tok,
                    });
                }
            }
        }

        debug_assert_eq!(
            active_segments
                .iter()
                .map(|s| s.current_tokens)
                .sum::<u64>(),
            prompt_total,
            "active prompt segments must sum identically to P(t)"
        );

        // Two-tier prefix-first Hamilton billing apportionment (`cache_read -> cache_write -> fresh_in`).
        let prefix_indices: Vec<usize> = active_segments
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.is_prefix.then_some(i))
            .collect();
        let suffix_indices: Vec<usize> = active_segments
            .iter()
            .enumerate()
            .filter_map(|(i, s)| (!s.is_prefix).then_some(i))
            .collect();

        let mut rem_caps: Vec<u64> = active_segments.iter().map(|s| s.current_tokens).collect();
        let mut seg_splits = vec![TokenSplit::ZERO; active_segments.len()];

        let mut rem_read = measured.cache_read;
        for (idx, tok) in allocate_billing_tier(&mut rem_read, &prefix_indices, &mut rem_caps) {
            seg_splits[idx].cache_read += tok;
        }
        for (idx, tok) in allocate_billing_tier(&mut rem_read, &suffix_indices, &mut rem_caps) {
            seg_splits[idx].cache_read += tok;
        }

        let mut rem_write = measured.cache_write;
        for (idx, tok) in allocate_billing_tier(&mut rem_write, &prefix_indices, &mut rem_caps) {
            seg_splits[idx].cache_write += tok;
        }
        for (idx, tok) in allocate_billing_tier(&mut rem_write, &suffix_indices, &mut rem_caps) {
            seg_splits[idx].cache_write += tok;
        }

        let mut rem_fresh = measured.fresh_in;
        for (idx, tok) in allocate_billing_tier(&mut rem_fresh, &prefix_indices, &mut rem_caps) {
            seg_splits[idx].fresh_in += tok;
        }
        for (idx, tok) in allocate_billing_tier(&mut rem_fresh, &suffix_indices, &mut rem_caps) {
            seg_splits[idx].fresh_in += tok;
        }

        debug_assert_eq!(rem_read, 0);
        debug_assert_eq!(rem_write, 0);
        debug_assert_eq!(rem_fresh, 0);

        // Route prompt segment splits to categories and update `carried_blocks`.
        let mut turn_categories = CategoryBreakdown::new();
        for (seg, split) in active_segments.iter().zip(seg_splits) {
            match seg.source {
                SegmentSource::FixedOverhead => {
                    turn_categories.add(Category::FixedOverhead, split);
                }
                SegmentSource::Unattributed => {
                    if !seg.is_prefix {
                        turn_categories.add(Category::Unattributed, split);
                    } else {
                        turn_categories.add(
                            Category::ResentHistory,
                            TokenSplit {
                                fresh_in: 0,
                                cache_write: 0,
                                cache_read: split.cache_read,
                                output: 0,
                            },
                        );
                        turn_categories.add(
                            Category::Unattributed,
                            TokenSplit {
                                fresh_in: split.fresh_in,
                                cache_write: split.cache_write,
                                cache_read: 0,
                                output: 0,
                            },
                        );
                    }
                }
                SegmentSource::Block { block_id } => {
                    let base_cat = block_base_category(&carried_blocks[block_id]);
                    if !seg.is_prefix {
                        carried_blocks[block_id].initial_split = split;
                        turn_categories.add(base_cat, split);
                    } else {
                        if seg.current_tokens > 0 {
                            carried_blocks[block_id].carried_turns =
                                carried_blocks[block_id].carried_turns.saturating_add(1);
                            carried_blocks[block_id].carried_split.add_assign(split);
                        }
                        if carried_blocks[block_id].kind == BlockKind::SystemContext {
                            turn_categories.add(Category::FixedOverhead, split);
                        } else {
                            turn_categories.add(
                                Category::ResentHistory,
                                TokenSplit {
                                    fresh_in: 0,
                                    cache_write: 0,
                                    cache_read: split.cache_read,
                                    output: 0,
                                },
                            );
                            turn_categories.add(
                                base_cat,
                                TokenSplit {
                                    fresh_in: split.fresh_in,
                                    cache_write: split.cache_write,
                                    cache_read: 0,
                                    output: 0,
                                },
                            );
                        }
                    }
                }
            }
        }

        // Drop zero-token non-fixed segments so zero-sized blocks never accrue `carried_turns`.
        active_segments.retain(|seg| {
            seg.current_tokens > 0 || matches!(seg.source, SegmentSource::FixedOverhead)
        });

        // Route response output `O(t)` across `AssistantOutput` / `Retries`.
        if measured.output > 0 {
            if turn.hints.api_error {
                turn_categories.add(
                    Category::Retries,
                    TokenSplit {
                        output: measured.output,
                        ..TokenSplit::ZERO
                    },
                );
            } else if output_block_ids.is_empty() {
                turn_categories.add(
                    Category::AssistantOutput,
                    TokenSplit {
                        output: measured.output,
                        ..TokenSplit::ZERO
                    },
                );
            } else {
                let out_weights: Vec<u64> = output_block_ids
                    .iter()
                    .map(|&id| carried_blocks[id].est_tokens)
                    .collect();
                let out_allocs = apportion_hamilton(measured.output, &out_weights);
                for (&id, out_tok) in output_block_ids.iter().zip(out_allocs) {
                    let cat = if carried_blocks[id].is_retry {
                        Category::Retries
                    } else {
                        Category::AssistantOutput
                    };
                    turn_categories.add(
                        cat,
                        TokenSplit {
                            output: out_tok,
                            ..TokenSplit::ZERO
                        },
                    );
                }
            }
        }

        pending_output_blocks = output_block_ids;

        debug_assert_eq!(
            turn_categories.sum(),
            measured,
            "turn category sum must reconcile identically to measured usage"
        );

        session_categories.add_breakdown(&turn_categories);
        session_totals.add_assign(measured);
        turn_attributions.push(TurnAttribution {
            turn: turn.turn,
            measured,
            measured_confidence,
            categories: turn_categories,
            category_confidence: cat_confidence,
            estimation_method: ESTIMATION_METHOD_SCALED_BYTES_DIV4,
            new_unattributed_prompt_tokens,
        });
    }

    // Fold child subagent sessions into `Category::Subagents` and `session_totals`.
    let mut subagent_summaries = Vec::with_capacity(subagent_sessions.len());
    for child_turns in subagent_sessions {
        if child_turns.is_empty() {
            continue;
        }
        let mut child_totals = TokenSplit::ZERO;
        for tr in *child_turns {
            overall_confidence = merge_confidence(overall_confidence, tr.usage.confidence);
            child_totals.add_assign(TokenSplit::from_usage(&tr.usage));
        }
        session_categories.add(Category::Subagents, child_totals);
        session_totals.add_assign(child_totals);
        subagent_summaries.push(SubagentSessionSummary {
            session_id: child_turns[0].session_id.clone(),
            turns: u32::try_from(child_turns.len()).unwrap_or(u32::MAX),
            totals: child_totals,
        });
    }

    let session_category_confidence = if parent_turns.is_empty() && subagent_summaries.is_empty() {
        Confidence::Measured
    } else {
        category_confidence_for(overall_confidence)
    };

    SessionAttribution {
        fixed_overhead_tokens,
        totals: session_totals,
        totals_confidence: overall_confidence,
        categories: session_categories,
        category_confidence: session_category_confidence,
        estimation_method: ESTIMATION_METHOD_SCALED_BYTES_DIV4,
        turns: turn_attributions,
        carried_blocks,
        subagents: subagent_summaries,
    }
}
