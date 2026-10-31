//! Normalised token usage and dual-convention constructors (`PLAN.md` §4, §5).

use serde::{Deserialize, Serialize};

use crate::model::Confidence;

/// Anthropic-style prompt cache creation TTL breakdown (`5m` vs `1h` ephemeral tiers).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheWriteSplit {
    pub ephemeral_5m: u64,
    pub ephemeral_1h: u64,
}

/// Errors returned when normalising raw agent token usage.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum UsageError {
    /// Summing disjoint prompt components overflowed `u64`.
    #[error(
        "prompt token sum overflowed u64 (input={input_tokens}, cache_write={cache_write_tokens}, cache_read={cache_read_tokens})"
    )]
    PromptOverflow {
        input_tokens: u64,
        cache_write_tokens: u64,
        cache_read_tokens: u64,
    },
    /// Inclusive `input_tokens` was smaller than `cache_write_tokens + cache_read_tokens`.
    #[error(
        "cached tokens ({cache_write_tokens} write + {cache_read_tokens} read) exceed inclusive input_tokens ({input_tokens})"
    )]
    CachedExceedsInput {
        input_tokens: u64,
        cache_write_tokens: u64,
        cache_read_tokens: u64,
    },
    /// Optional agent-reported USD cost was `NaN` or infinite.
    #[error("agent_reported_cost_usd must be finite, got {value}")]
    NonFiniteCost { value: f64 },
}

/// Raw usage fields for agents where `input_tokens` is **disjoint** from cached
/// tokens (Anthropic / Claude Code, opencode).
///
/// Total prompt size is `P = input_tokens + cache_write_tokens + cache_read_tokens`.
#[derive(Debug, Clone, PartialEq)]
pub struct DisjointUsageInput {
    pub input_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_read_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: Option<u64>,
    pub confidence: Confidence,
    pub estimation_method: Option<String>,
    pub cache_write_split: Option<CacheWriteSplit>,
    pub agent_reported_cost_usd: Option<f64>,
}

impl DisjointUsageInput {
    /// Convenience constructor for measured disjoint counts with no optional metadata.
    #[must_use]
    pub const fn measured(
        input_tokens: u64,
        cache_write_tokens: u64,
        cache_read_tokens: u64,
        output_tokens: u64,
    ) -> Self {
        Self {
            input_tokens,
            cache_write_tokens,
            cache_read_tokens,
            output_tokens,
            reasoning_tokens: None,
            confidence: Confidence::Measured,
            estimation_method: None,
            cache_write_split: None,
            agent_reported_cost_usd: None,
        }
    }
}

/// Raw usage fields for agents where `input_tokens` **includes** cached tokens
/// (OpenAI / Codex CLI, Gemini CLI).
///
/// Total prompt size is `P = input_tokens`, and
/// `fresh_in = input_tokens - cache_write_tokens - cache_read_tokens`.
#[derive(Debug, Clone, PartialEq)]
pub struct InclusiveUsageInput {
    pub input_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_read_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: Option<u64>,
    pub confidence: Confidence,
    pub estimation_method: Option<String>,
    pub agent_reported_cost_usd: Option<f64>,
}

impl InclusiveUsageInput {
    /// Convenience constructor for measured inclusive counts with no optional metadata.
    #[must_use]
    pub const fn measured(
        input_tokens: u64,
        cache_write_tokens: u64,
        cache_read_tokens: u64,
        output_tokens: u64,
    ) -> Self {
        Self {
            input_tokens,
            cache_write_tokens,
            cache_read_tokens,
            output_tokens,
            reasoning_tokens: None,
            confidence: Confidence::Measured,
            estimation_method: None,
            agent_reported_cost_usd: None,
        }
    }
}

fn validate_finite_cost(cost: Option<f64>) -> Result<(), UsageError> {
    if let Some(value) = cost {
        if !value.is_finite() {
            return Err(UsageError::NonFiniteCost { value });
        }
    }
    Ok(())
}

/// Normalised per-turn token usage (`PLAN.md` §4, §5).
///
/// Always satisfies `P(t) = fresh_in + cache_write + cache_read`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub fresh_in: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub output: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    pub confidence: Confidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimation_method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_split: Option<CacheWriteSplit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_reported_cost_usd: Option<f64>,
}

impl Usage {
    /// Total prompt size `P(t) = fresh_in + cache_write + cache_read` (saturating on `u64`).
    #[must_use]
    pub const fn prompt_tokens(&self) -> u64 {
        self.fresh_in
            .saturating_add(self.cache_write)
            .saturating_add(self.cache_read)
    }

    /// Checked total prompt size `P(t) = fresh_in + cache_write + cache_read`.
    #[must_use]
    pub const fn prompt_tokens_checked(&self) -> Option<u64> {
        match self.fresh_in.checked_add(self.cache_write) {
            Some(partial) => partial.checked_add(self.cache_read),
            None => None,
        }
    }

    /// Normalises Anthropic/opencode-style disjoint usage counts.
    pub fn from_disjoint(input: DisjointUsageInput) -> Result<Self, UsageError> {
        validate_finite_cost(input.agent_reported_cost_usd)?;
        input
            .input_tokens
            .checked_add(input.cache_write_tokens)
            .and_then(|partial| partial.checked_add(input.cache_read_tokens))
            .ok_or(UsageError::PromptOverflow {
                input_tokens: input.input_tokens,
                cache_write_tokens: input.cache_write_tokens,
                cache_read_tokens: input.cache_read_tokens,
            })?;

        Ok(Self {
            fresh_in: input.input_tokens,
            cache_write: input.cache_write_tokens,
            cache_read: input.cache_read_tokens,
            output: input.output_tokens,
            reasoning: input.reasoning_tokens,
            confidence: input.confidence,
            estimation_method: input.estimation_method,
            cache_write_split: input.cache_write_split,
            agent_reported_cost_usd: input.agent_reported_cost_usd,
        })
    }

    /// Normalises OpenAI/Codex/Gemini-style inclusive usage counts, rejecting
    /// records where cached tokens exceed `input_tokens`.
    pub fn from_inclusive_checked(input: InclusiveUsageInput) -> Result<Self, UsageError> {
        validate_finite_cost(input.agent_reported_cost_usd)?;
        let cached_total = input
            .cache_write_tokens
            .checked_add(input.cache_read_tokens)
            .ok_or(UsageError::CachedExceedsInput {
                input_tokens: input.input_tokens,
                cache_write_tokens: input.cache_write_tokens,
                cache_read_tokens: input.cache_read_tokens,
            })?;
        let fresh_in =
            input
                .input_tokens
                .checked_sub(cached_total)
                .ok_or(UsageError::CachedExceedsInput {
                    input_tokens: input.input_tokens,
                    cache_write_tokens: input.cache_write_tokens,
                    cache_read_tokens: input.cache_read_tokens,
                })?;

        Ok(Self {
            fresh_in,
            cache_write: input.cache_write_tokens,
            cache_read: input.cache_read_tokens,
            output: input.output_tokens,
            reasoning: input.reasoning_tokens,
            confidence: input.confidence,
            estimation_method: input.estimation_method,
            cache_write_split: None,
            agent_reported_cost_usd: input.agent_reported_cost_usd,
        })
    }

    /// Tolerant inclusive-convention constructor for malformed logs (`PLAN.md` §0 #6).
    ///
    /// Clamps `cache_read` and `cache_write` so `fresh_in + cache_write + cache_read == input_tokens`
    /// always holds, returning `(usage, was_clamped)`.
    #[must_use]
    pub fn from_inclusive_clamped(input: InclusiveUsageInput) -> (Self, bool) {
        let was_clamped = input
            .cache_read_tokens
            .checked_add(input.cache_write_tokens)
            .is_none_or(|sum| sum > input.input_tokens);

        let cache_read = input.cache_read_tokens.min(input.input_tokens);
        let remaining_after_read = input.input_tokens - cache_read;
        let cache_write = input.cache_write_tokens.min(remaining_after_read);
        let fresh_in = remaining_after_read - cache_write;

        let usage = Self {
            fresh_in,
            cache_write,
            cache_read,
            output: input.output_tokens,
            reasoning: input.reasoning_tokens,
            confidence: input.confidence,
            estimation_method: input.estimation_method,
            cache_write_split: None,
            agent_reported_cost_usd: input.agent_reported_cost_usd.filter(|v| v.is_finite()),
        };

        (usage, was_clamped)
    }
}
