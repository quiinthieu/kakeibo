//! Pure API-equivalent pricing schemas, `5m`/`1h` prompt-cache write split math,
//! per-turn & per-category cost reconciliation, and offline LiteLLM JSON import
//! (`PLAN.md` §0 #3 & #7, §6.2, §10 T09).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::attrib::{CategoryBreakdown, SessionAttribution};
use crate::model::{Category, Confidence, TurnRecord};
use crate::usage::Usage;

const BUNDLED_PRICING_TOML: &str = include_str!("../../../data/pricing.toml");

/// Expected `schema_version` in `data/pricing.toml` and user `pricing.toml`.
pub const PRICING_SCHEMA_VERSION: u32 = 1;

/// Provenance `source` recorded on entries imported via `kakeibo pricing import`.
pub const LITELLM_IMPORT_SOURCE: &str = "litellm:model_prices_and_context_window.json";

const TOKENS_PER_MILLION: f64 = 1_000_000.0;
const IMPORT_ROUND_SCALE: f64 = 1_000_000_000.0;

fn default_currency() -> String {
    "USD".to_string()
}

/// Errors returned when parsing, validating, serializing, or importing [`PricingCatalog`].
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PricingError {
    /// Failed to parse or serialize TOML.
    #[error("pricing TOML error: {0}")]
    Toml(String),
    /// Failed to parse LiteLLM JSON.
    #[error("LiteLLM pricing JSON error: {0}")]
    Json(String),
    /// Unsupported `schema_version`.
    #[error("unsupported pricing schema_version {found} (expected {expected})")]
    UnsupportedSchemaVersion { found: u32, expected: u32 },
    /// Required provenance metadata (`currency`, `source`, or `checked`) was empty.
    #[error("pricing entry `{entry}` is missing non-empty `{field}` provenance")]
    MissingProvenance { entry: String, field: &'static str },
    /// A configured rate was negative, `NaN`, or infinite.
    #[error("invalid rate `{field}` on `{entry}`: {reason}")]
    InvalidRate {
        entry: String,
        field: &'static str,
        reason: &'static str,
    },
}

/// Denominator unit for numeric rates stored in a [`PricingCatalog`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RateUnit {
    /// Amounts are expressed per `1,000,000` tokens (default human-editable TOML unit).
    #[default]
    #[serde(rename = "per_1m_tokens")]
    Per1MTokens,
    /// Amounts are expressed per single token.
    #[serde(rename = "per_token")]
    PerToken,
}

impl RateUnit {
    /// Canonical `snake_case` wire identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Per1MTokens => "per_1m_tokens",
            Self::PerToken => "per_token",
        }
    }

    /// Token denominator converting a rate into currency-per-single-token.
    #[must_use]
    pub const fn tokens_denominator(self) -> f64 {
        match self {
            Self::Per1MTokens => TOKENS_PER_MILLION,
            Self::PerToken => 1.0,
        }
    }
}

impl fmt::Display for RateUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Cost availability status for a session (`PLAN.md` §0 #7, §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostStatus {
    /// No configured user price matched and no agent-reported cost was logged; never shown as `$0`.
    Unpriced,
    /// Every turn in the session (including rolled-up subagents) matched a configured price in one currency.
    Configured,
    /// At least one turn matched a configured price, while one or more turns remained unpriced.
    PartialConfigured,
    /// No configured user price matched, and the coding agent logged its own cost figure (`agent_reported_cost_usd`).
    AgentReported,
}

impl CostStatus {
    /// Canonical `snake_case` wire label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unpriced => "unpriced",
            Self::Configured => "configured",
            Self::PartialConfigured => "partial_configured",
            Self::AgentReported => "agent_reported",
        }
    }
}

impl fmt::Display for CostStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Per-model × token-class API-equivalent rate entry (`PLAN.md` §6.2).
///
/// Bundled placeholder entries in `data/pricing.toml` leave all six numeric
/// fields as `None` while recording `currency`, `source`, and `checked`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelPriceEntry {
    #[serde(default = "default_currency")]
    pub currency: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_5m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_1h: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<f64>,
    pub source: String,
    pub checked: String,
}

fn validate_optional_non_negative(
    entry_name: &str,
    field: &'static str,
    val: Option<f64>,
) -> Result<(), PricingError> {
    if let Some(v) = val {
        if !v.is_finite() {
            return Err(PricingError::InvalidRate {
                entry: entry_name.to_string(),
                field,
                reason: "must be finite",
            });
        }
        if v < 0.0 {
            return Err(PricingError::InvalidRate {
                entry: entry_name.to_string(),
                field,
                reason: "must be >= 0.0",
            });
        }
    }
    Ok(())
}

fn convert_rate(val: Option<f64>, from: RateUnit, to: RateUnit) -> Option<f64> {
    let v = val?;
    if from == to {
        return Some(v);
    }
    let per_tok = v / from.tokens_denominator();
    let scaled = per_tok * to.tokens_denominator();
    Some((scaled * IMPORT_ROUND_SCALE).round() / IMPORT_ROUND_SCALE)
}

impl ModelPriceEntry {
    /// Returns `true` when at least `input` and `output` rates are configured.
    ///
    /// Bundled placeholder entries (`input: None, output: None`) return `false`
    /// so unpriced models never evaluate to `$0` (`PLAN.md` §6.2).
    #[must_use]
    pub const fn is_priced(&self) -> bool {
        self.input.is_some() && self.output.is_some()
    }

    /// Converts all configured rates from `from_unit` into `to_unit`.
    #[must_use]
    pub fn converted(&self, from_unit: RateUnit, to_unit: RateUnit) -> Self {
        Self {
            currency: self.currency.trim().to_ascii_uppercase(),
            input: convert_rate(self.input, from_unit, to_unit),
            cache_write_5m: convert_rate(self.cache_write_5m, from_unit, to_unit),
            cache_write_1h: convert_rate(self.cache_write_1h, from_unit, to_unit),
            cache_read: convert_rate(self.cache_read, from_unit, to_unit),
            output: convert_rate(self.output, from_unit, to_unit),
            reasoning: convert_rate(self.reasoning, from_unit, to_unit),
            source: self.source.clone(),
            checked: self.checked.clone(),
        }
    }

    fn validate(&self, name: &str) -> Result<(), PricingError> {
        if self.currency.trim().is_empty() {
            return Err(PricingError::MissingProvenance {
                entry: name.to_string(),
                field: "currency",
            });
        }
        if self.source.trim().is_empty() {
            return Err(PricingError::MissingProvenance {
                entry: name.to_string(),
                field: "source",
            });
        }
        if self.checked.trim().is_empty() {
            return Err(PricingError::MissingProvenance {
                entry: name.to_string(),
                field: "checked",
            });
        }
        validate_optional_non_negative(name, "input", self.input)?;
        validate_optional_non_negative(name, "cache_write_5m", self.cache_write_5m)?;
        validate_optional_non_negative(name, "cache_write_1h", self.cache_write_1h)?;
        validate_optional_non_negative(name, "cache_read", self.cache_read)?;
        validate_optional_non_negative(name, "output", self.output)?;
        validate_optional_non_negative(name, "reasoning", self.reasoning)?;
        Ok(())
    }
}

/// Validated catalog of model pricing entries (`data/pricing.toml` + user overlay).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PricingCatalog {
    pub schema_version: u32,
    #[serde(default)]
    pub rate_unit: RateUnit,
    #[serde(default)]
    pub models: BTreeMap<String, ModelPriceEntry>,
}

fn is_preferred_vendor_prefix(key: &str) -> bool {
    key.starts_with("anthropic/")
        || key.starts_with("openai/")
        || key.starts_with("gemini/")
        || key.starts_with("google/")
}

impl PricingCatalog {
    /// Parses and validates the bundled `data/pricing.toml` catalog (shipped with empty rate defaults).
    #[must_use]
    pub fn bundled() -> Self {
        Self::from_toml_str(BUNDLED_PRICING_TOML)
            .expect("bundled data/pricing.toml must be valid TOML with provenance")
    }

    /// Creates an empty valid overlay catalog (`schema_version = 1`, `rate_unit = Per1MTokens`).
    #[must_use]
    pub const fn empty_overlay() -> Self {
        Self {
            schema_version: PRICING_SCHEMA_VERSION,
            rate_unit: RateUnit::Per1MTokens,
            models: BTreeMap::new(),
        }
    }

    /// Parses and validates a `pricing.toml` document.
    pub fn from_toml_str(raw: &str) -> Result<Self, PricingError> {
        let mut catalog: Self =
            toml::from_str(raw).map_err(|err| PricingError::Toml(err.to_string()))?;
        if catalog.schema_version != PRICING_SCHEMA_VERSION {
            return Err(PricingError::UnsupportedSchemaVersion {
                found: catalog.schema_version,
                expected: PRICING_SCHEMA_VERSION,
            });
        }
        for (name, entry) in &mut catalog.models {
            entry.validate(name)?;
            entry.currency = entry.currency.trim().to_ascii_uppercase();
            entry.source = entry.source.trim().to_string();
            entry.checked = entry.checked.trim().to_string();
        }
        Ok(catalog)
    }

    /// Serializes this catalog to canonical pretty TOML (`rate_unit = "per_1m_tokens"`).
    pub fn to_toml_string(&self) -> Result<String, PricingError> {
        let normalised = if self.rate_unit == RateUnit::Per1MTokens {
            self.clone()
        } else {
            let mut models = BTreeMap::new();
            for (k, v) in &self.models {
                models.insert(
                    k.clone(),
                    v.converted(self.rate_unit, RateUnit::Per1MTokens),
                );
            }
            Self {
                schema_version: self.schema_version,
                rate_unit: RateUnit::Per1MTokens,
                models,
            }
        };
        toml::to_string_pretty(&normalised).map_err(|err| PricingError::Toml(err.to_string()))
    }

    /// Merges `overlay` entries onto `self`, converting `overlay`'s `rate_unit` into `self.rate_unit`.
    pub fn merge_overlay(&mut self, overlay: &Self) {
        for (key, entry) in &overlay.models {
            self.models.insert(
                key.clone(),
                entry.converted(overlay.rate_unit, self.rate_unit),
            );
        }
    }

    /// Resolves `model` to the best-matching **priced** (`is_priced() == true`) entry.
    ///
    /// Returns `None` when `model` is missing or only matches an unpriced placeholder.
    #[must_use]
    pub fn resolve_priced(&self, model: Option<&str>) -> Option<(&str, &ModelPriceEntry)> {
        self.resolve_internal(model, true)
    }

    /// Resolves `model` to the best-matching entry whether priced or unpriced placeholder.
    #[must_use]
    pub fn resolve_any(&self, model: Option<&str>) -> Option<(&str, &ModelPriceEntry)> {
        self.resolve_internal(model, false)
    }

    fn resolve_internal(
        &self,
        model: Option<&str>,
        require_priced: bool,
    ) -> Option<(&str, &ModelPriceEntry)> {
        let query = model?.trim();
        if query.is_empty() {
            return None;
        }
        let bare_query = query.split_once('/').map_or(query, |(_, rest)| rest);

        // Candidate score tuple `(tier, bare_len, preferred_rank, is_bare_key)` — higher wins.
        type CandidateScore = (u8, usize, u8, u8);
        let mut best: Option<(CandidateScore, &str, &ModelPriceEntry)> = None;

        for (key, entry) in &self.models {
            if require_priced && !entry.is_priced() {
                continue;
            }
            let key_str = key.as_str();
            let bare_key = key_str.split_once('/').map_or(key_str, |(_, rest)| rest);

            let tier = if key_str == query {
                4_u8
            } else if bare_key == bare_query {
                3_u8
            } else if query
                .strip_prefix(key_str)
                .is_some_and(|rest| rest.starts_with(['-', '@', ':']))
            {
                2_u8
            } else if bare_query
                .strip_prefix(bare_key)
                .is_some_and(|rest| rest.starts_with(['-', '@', ':']))
            {
                1_u8
            } else {
                continue;
            };

            let is_bare_key = u8::from(!key_str.contains('/'));
            let preferred_rank = if is_bare_key == 1 {
                2_u8
            } else if is_preferred_vendor_prefix(key_str) {
                1_u8
            } else {
                0_u8
            };
            let score = (tier, bare_key.len(), preferred_rank, is_bare_key);

            let should_replace = match best {
                None => true,
                Some((best_score, best_key, _)) => {
                    score > best_score || (score == best_score && key_str < best_key)
                }
            };
            if should_replace {
                best = Some((score, key_str, entry));
            }
        }

        best.map(|(_, k, e)| (k, e))
    }
}

/// Summary returned by [`import_litellm_json_str`].
#[derive(Debug, Clone, PartialEq)]
pub struct LiteLlmImportResult {
    pub catalog: PricingCatalog,
    pub imported_models: usize,
    pub skipped_entries: usize,
}

fn extract_per_tok_field(
    obj: &serde_json::Map<String, serde_json::Value>,
    model_key: &str,
    json_field: &'static str,
) -> Result<Option<f64>, PricingError> {
    let Some(val) = obj.get(json_field) else {
        return Ok(None);
    };
    if val.is_null() {
        return Ok(None);
    }
    let Some(num) = val.as_f64() else {
        return Ok(None);
    };
    if !num.is_finite() || num < 0.0 {
        return Err(PricingError::InvalidRate {
            entry: model_key.to_string(),
            field: json_field,
            reason: "must be finite and >= 0.0",
        });
    }
    let per_1m = ((num * TOKENS_PER_MILLION) * IMPORT_ROUND_SCALE).round() / IMPORT_ROUND_SCALE;
    Ok(Some(per_1m))
}

/// Converts a local LiteLLM `model_prices_and_context_window.json` payload into a
/// validated [`PricingCatalog`] (`rate_unit = "per_1m_tokens"`, `PLAN.md` §6.2, `docs/formats/litellm-pricing.md`).
pub fn import_litellm_json_str(
    raw_json: &str,
    checked_date: &str,
) -> Result<LiteLlmImportResult, PricingError> {
    let checked = checked_date.trim();
    if checked.is_empty() {
        return Err(PricingError::MissingProvenance {
            entry: "<litellm-import>".to_string(),
            field: "checked",
        });
    }

    let root: BTreeMap<String, serde_json::Value> =
        serde_json::from_str(raw_json).map_err(|err| PricingError::Json(err.to_string()))?;

    let mut models = BTreeMap::new();
    let mut skipped_entries = 0_usize;

    for (model_key, raw_entry) in root {
        let trimmed_key = model_key.trim();
        if trimmed_key.is_empty() || trimmed_key == "sample_spec" {
            skipped_entries += 1;
            continue;
        }
        let Some(obj) = raw_entry.as_object() else {
            skipped_entries += 1;
            continue;
        };

        let input = extract_per_tok_field(obj, trimmed_key, "input_cost_per_token")?;
        let output = extract_per_tok_field(obj, trimmed_key, "output_cost_per_token")?;
        let (Some(in_val), Some(out_val)) = (input, output) else {
            skipped_entries += 1;
            continue;
        };
        // Skip zero-rate documentation stubs so unpriced models never masquerade as `$0`.
        if in_val == 0.0 && out_val == 0.0 {
            skipped_entries += 1;
            continue;
        }

        let w5m = extract_per_tok_field(obj, trimmed_key, "cache_creation_input_token_cost")?;
        let w1h = extract_per_tok_field(
            obj,
            trimmed_key,
            "cache_creation_input_token_cost_above_1hr",
        )?;
        let read = extract_per_tok_field(obj, trimmed_key, "cache_read_input_token_cost")?;
        let reason = extract_per_tok_field(obj, trimmed_key, "output_cost_per_reasoning_token")?;

        models.insert(
            trimmed_key.to_string(),
            ModelPriceEntry {
                currency: default_currency(),
                input: Some(in_val),
                cache_write_5m: w5m,
                cache_write_1h: w1h,
                cache_read: read,
                output: Some(out_val),
                reasoning: reason,
                source: LITELLM_IMPORT_SOURCE.to_string(),
                checked: checked.to_string(),
            },
        );
    }

    let imported_models = models.len();
    Ok(LiteLlmImportResult {
        catalog: PricingCatalog {
            schema_version: PRICING_SCHEMA_VERSION,
            rate_unit: RateUnit::Per1MTokens,
            models,
        },
        imported_models,
        skipped_entries,
    })
}

/// Normalised `5m` vs `1h` prompt-cache creation token partition for one turn.
///
/// Always satisfies `tokens_5m + tokens_1h == usage.cache_write`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheWriteTierTokens {
    pub tokens_5m: u64,
    pub tokens_1h: u64,
}

impl CacheWriteTierTokens {
    /// Partitions `usage.cache_write` across `5m` and `1h` tiers using `usage.cache_write_split`.
    #[must_use]
    pub fn from_usage(usage: &Usage) -> Self {
        match usage.cache_write_split {
            Some(split) => {
                let tokens_1h = split.ephemeral_1h.min(usage.cache_write);
                let tokens_5m = usage.cache_write - tokens_1h;
                Self {
                    tokens_5m,
                    tokens_1h,
                }
            }
            None => Self {
                tokens_5m: usage.cache_write,
                tokens_1h: 0,
            },
        }
    }
}

/// API-equivalent cost breakdown for a single priced turn (`PLAN.md` §6.2, §10 T09).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnCostBreakdown {
    pub turn: u32,
    pub model: String,
    pub matched_pricing_key: String,
    pub currency: String,
    pub write_tiers: CacheWriteTierTokens,
    pub fresh_in_cost: f64,
    pub cache_write_5m_cost: f64,
    pub cache_write_1h_cost: f64,
    pub cache_read_cost: f64,
    pub output_cost: f64,
    pub reasoning_cost: f64,
    pub total_cost: f64,
    pub category_costs: BTreeMap<Category, f64>,
}

/// Session-wide cost attribution across turns, categories, and rolled-up subagents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionCostAttribution {
    pub status: CostStatus,
    pub currency: Option<String>,
    pub total_cost: Option<f64>,
    pub priced_turns: u32,
    pub unpriced_turns: u32,
    pub unpriced_models: Vec<String>,
    pub category_costs: Option<BTreeMap<Category, f64>>,
    pub turn_costs: Vec<Option<TurnCostBreakdown>>,
}

struct ComputedUsageCost {
    matched_key: String,
    currency: String,
    write_tiers: CacheWriteTierTokens,
    per_tok_in: f64,
    per_tok_read: f64,
    fresh_in_cost: f64,
    cache_write_5m_cost: f64,
    cache_write_1h_cost: f64,
    cache_read_cost: f64,
    output_cost: f64,
    reasoning_cost: f64,
    total_cost: f64,
}

fn compute_usage_cost(
    model: Option<&str>,
    usage: &Usage,
    catalog: &PricingCatalog,
) -> Option<ComputedUsageCost> {
    if usage.confidence == Confidence::NotRecorded {
        return None;
    }
    let (matched_key, entry) = catalog.resolve_priced(model)?;
    let denom = catalog.rate_unit.tokens_denominator();

    let per_tok_in = entry.input? / denom;
    let per_tok_out = entry.output? / denom;
    let per_tok_w5m = entry.cache_write_5m.map_or(per_tok_in, |v| v / denom);
    let per_tok_w1h = entry
        .cache_write_1h
        .or(entry.cache_write_5m)
        .map_or(per_tok_in, |v| v / denom);
    let per_tok_read = entry.cache_read.map_or(per_tok_in, |v| v / denom);

    let write_tiers = CacheWriteTierTokens::from_usage(usage);
    let fresh_in_cost = (usage.fresh_in as f64) * per_tok_in;
    let cache_write_5m_cost = (write_tiers.tokens_5m as f64) * per_tok_w5m;
    let cache_write_1h_cost = (write_tiers.tokens_1h as f64) * per_tok_w1h;
    let cache_read_cost = (usage.cache_read as f64) * per_tok_read;

    let (output_cost, reasoning_cost) = match (usage.reasoning, entry.reasoning) {
        (Some(r_tokens), Some(raw_reason)) => {
            let per_tok_reason = raw_reason / denom;
            let r_in_out = r_tokens.min(usage.output);
            let regular_out = usage.output - r_in_out;
            let extra_r = r_tokens.saturating_sub(usage.output);
            (
                (regular_out as f64) * per_tok_out,
                ((r_in_out + extra_r) as f64) * per_tok_reason,
            )
        }
        _ => ((usage.output as f64) * per_tok_out, 0.0),
    };

    let total_cost = fresh_in_cost
        + cache_write_5m_cost
        + cache_write_1h_cost
        + cache_read_cost
        + output_cost
        + reasoning_cost;

    Some(ComputedUsageCost {
        matched_key: matched_key.to_string(),
        currency: entry.currency.clone(),
        write_tiers,
        per_tok_in,
        per_tok_read,
        fresh_in_cost,
        cache_write_5m_cost,
        cache_write_1h_cost,
        cache_read_cost,
        output_cost,
        reasoning_cost,
        total_cost,
    })
}

fn apportion_turn_categories(
    usage: &Usage,
    categories: &CategoryBreakdown,
    computed: &ComputedUsageCost,
) -> BTreeMap<Category, f64> {
    let write_total_cost = computed.cache_write_5m_cost + computed.cache_write_1h_cost;
    let out_and_reason_cost = computed.output_cost + computed.reasoning_cost;

    let mut out = BTreeMap::new();
    for cat in Category::ALL {
        let split = categories.get(cat);
        let c_fresh = (split.fresh_in as f64) * computed.per_tok_in;
        let c_write = if usage.cache_write > 0 {
            write_total_cost * ((split.cache_write as f64) / (usage.cache_write as f64))
        } else {
            0.0
        };
        let c_read = (split.cache_read as f64) * computed.per_tok_read;
        let c_out = if usage.output > 0 {
            out_and_reason_cost * ((split.output as f64) / (usage.output as f64))
        } else if cat == Category::AssistantOutput {
            out_and_reason_cost
        } else {
            0.0
        };
        out.insert(cat, c_fresh + c_write + c_read + c_out);
    }
    out
}

/// Computes per-turn and per-category API-equivalent costs for a primary session and
/// its rolled-up child subagent sessions (`PLAN.md` §6.2, §10 T09).
#[must_use]
pub fn price_session(
    turns: &[TurnRecord],
    subagents: &[&[TurnRecord]],
    attribution: &SessionAttribution,
    catalog: &PricingCatalog,
) -> SessionCostAttribution {
    let mut turn_costs = Vec::with_capacity(turns.len());
    let mut session_categories: BTreeMap<Category, f64> =
        Category::ALL.into_iter().map(|c| (c, 0.0)).collect();

    let mut priced_turns = 0_u32;
    let mut unpriced_turns = 0_u32;
    let mut unpriced_models = BTreeSet::new();
    let mut currencies = BTreeSet::new();
    let mut configured_sum = 0.0_f64;

    let mut reported_sum = 0.0_f64;
    let mut saw_reported_cost = false;

    for (idx, tr) in turns.iter().enumerate() {
        if let Some(rep) = tr
            .usage
            .agent_reported_cost_usd
            .filter(|v| v.is_finite() && *v >= 0.0)
        {
            reported_sum += rep;
            saw_reported_cost = true;
        }

        let fallback_cats = CategoryBreakdown::new();
        let turn_cats = attribution
            .turns
            .get(idx)
            .map_or(&fallback_cats, |ta| &ta.categories);

        if let Some(computed) = compute_usage_cost(tr.model.as_deref(), &tr.usage, catalog) {
            priced_turns = priced_turns.saturating_add(1);
            currencies.insert(computed.currency.clone());
            configured_sum += computed.total_cost;

            let cat_costs = apportion_turn_categories(&tr.usage, turn_cats, &computed);
            for (cat, val) in &cat_costs {
                if let Some(acc) = session_categories.get_mut(cat) {
                    *acc += *val;
                }
            }

            turn_costs.push(Some(TurnCostBreakdown {
                turn: tr.turn,
                model: tr.model.clone().unwrap_or_default(),
                matched_pricing_key: computed.matched_key,
                currency: computed.currency,
                write_tiers: computed.write_tiers,
                fresh_in_cost: computed.fresh_in_cost,
                cache_write_5m_cost: computed.cache_write_5m_cost,
                cache_write_1h_cost: computed.cache_write_1h_cost,
                cache_read_cost: computed.cache_read_cost,
                output_cost: computed.output_cost,
                reasoning_cost: computed.reasoning_cost,
                total_cost: computed.total_cost,
                category_costs: cat_costs,
            }));
        } else {
            unpriced_turns = unpriced_turns.saturating_add(1);
            if let Some(m) = tr.model.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
                unpriced_models.insert(m.to_string());
            }
            turn_costs.push(None);
        }
    }

    for sub_session in subagents {
        for sub_tr in *sub_session {
            if let Some(rep) = sub_tr
                .usage
                .agent_reported_cost_usd
                .filter(|v| v.is_finite() && *v >= 0.0)
            {
                reported_sum += rep;
                saw_reported_cost = true;
            }

            if let Some(computed) =
                compute_usage_cost(sub_tr.model.as_deref(), &sub_tr.usage, catalog)
            {
                priced_turns = priced_turns.saturating_add(1);
                currencies.insert(computed.currency);
                configured_sum += computed.total_cost;
                if let Some(sub_acc) = session_categories.get_mut(&Category::Subagents) {
                    *sub_acc += computed.total_cost;
                }
            } else {
                unpriced_turns = unpriced_turns.saturating_add(1);
                if let Some(m) = sub_tr
                    .model
                    .as_deref()
                    .map(str::trim)
                    .filter(|m| !m.is_empty())
                {
                    unpriced_models.insert(m.to_string());
                }
            }
        }
    }

    let unpriced_models_vec: Vec<String> = unpriced_models.into_iter().collect();

    if priced_turns > 0 && currencies.len() == 1 {
        let currency = currencies.into_iter().next();
        if unpriced_turns == 0 {
            return SessionCostAttribution {
                status: CostStatus::Configured,
                currency,
                total_cost: Some(configured_sum),
                priced_turns,
                unpriced_turns,
                unpriced_models: unpriced_models_vec,
                category_costs: Some(session_categories),
                turn_costs,
            };
        }
        return SessionCostAttribution {
            status: CostStatus::PartialConfigured,
            currency,
            total_cost: Some(configured_sum),
            priced_turns,
            unpriced_turns,
            unpriced_models: unpriced_models_vec,
            category_costs: None,
            turn_costs,
        };
    }

    if priced_turns == 0 && saw_reported_cost {
        return SessionCostAttribution {
            status: CostStatus::AgentReported,
            currency: Some(default_currency()),
            total_cost: Some(reported_sum),
            priced_turns: 0,
            unpriced_turns,
            unpriced_models: unpriced_models_vec,
            category_costs: None,
            turn_costs,
        };
    }

    SessionCostAttribution {
        status: CostStatus::Unpriced,
        currency: None,
        total_cost: None,
        priced_turns,
        unpriced_turns,
        unpriced_models: unpriced_models_vec,
        category_costs: None,
        turn_costs,
    }
}
