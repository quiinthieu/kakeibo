//! Versioned JSON export renderer (`schemaVersion: 1`, `docs/cli-schema.md`).

use std::collections::BTreeMap;
use std::io::Write;

use kakeibo_core::attrib::{CategoryBreakdown, SubagentSessionSummary, TokenSplit};
use kakeibo_core::flags::{FlagEvidence, FlagId, TurnRange};
use kakeibo_core::{product, AgentId, Category, Confidence, Timestamp, TurnHints};
use serde::Serialize;

use crate::flags::{render_flag_evidence, render_flag_sentence};
use crate::model::{share_basis_points, CostStatus, SessionReport};

/// Root JSON document emitted by `kakeibo export ... --format json` (`docs/cli-schema.md` §2).
#[derive(Debug, Clone, Serialize)]
pub struct CliExportDocumentV1 {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub generator: GeneratorExportV1,
    pub session_count: usize,
    pub sessions: Vec<SessionExportV1>,
}

/// Generator metadata (`product::BINARY_NAME` and `product::VERSION`).
#[derive(Debug, Clone, Serialize)]
pub struct GeneratorExportV1 {
    pub name: String,
    pub version: String,
}

/// Per-session JSON export object (`docs/cli-schema.md` §3).
#[derive(Debug, Clone, Serialize)]
pub struct SessionExportV1 {
    pub agent: AgentId,
    pub session_id: String,
    pub turn_count: u32,
    pub started_at: Timestamp,
    pub ended_at: Timestamp,
    pub models: Vec<String>,
    pub project_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
    pub cost: CostExportV1,
    pub totals: TotalsExportV1,
    pub fixed_overhead: FixedOverheadExportV1,
    pub categories: CategoriesExportV1,
    pub flags: Vec<FlagExportV1>,
    pub tools: Vec<ToolSpendExportV1>,
    pub subagents: Vec<SubagentSessionSummary>,
    pub turns: Vec<TurnExportV1>,
}

/// Cost summary object (`docs/cli-schema.md` §4).
#[derive(Debug, Clone, Serialize)]
pub struct CostExportV1 {
    pub status: CostStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    pub usd: Option<f64>,
}

/// Reconciled session-wide token totals (`docs/cli-schema.md` §5).
#[derive(Debug, Clone, Serialize)]
pub struct TotalsExportV1 {
    pub prompt_tokens: u64,
    pub total_tokens: u64,
    pub fresh_in: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub output: u64,
    pub confidence: Confidence,
}

/// Estimated pre-conversation fixed prompt overhead (`docs/cli-schema.md` §5).
#[derive(Debug, Clone, Serialize)]
pub struct FixedOverheadExportV1 {
    pub tokens: u64,
    pub confidence: Confidence,
    pub estimation_method: &'static str,
}

/// Nine-category attributed breakdown (`docs/cli-schema.md` §6).
#[derive(Debug, Clone, Serialize)]
pub struct CategoriesExportV1 {
    pub confidence: Confidence,
    pub estimation_method: &'static str,
    pub breakdown: BTreeMap<Category, CategoryBucketExportV1>,
}

/// Per-category token split, share basis points, and optional API-equivalent USD cost (`docs/cli-schema.md` §6).
#[derive(Debug, Clone, Serialize)]
pub struct CategoryBucketExportV1 {
    pub prompt_tokens: u64,
    pub total_tokens: u64,
    pub fresh_in: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub output: u64,
    pub share_bps: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// Ranked root-cause flag finding with plain-words sentence, evidence text, and structured payload (`docs/cli-schema.md` §7).
#[derive(Debug, Clone, Serialize)]
pub struct FlagExportV1 {
    pub id: FlagId,
    pub turn_range: TurnRange,
    pub impact_tokens: u64,
    pub confidence: Confidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimation_method: Option<&'static str>,
    pub sentence: String,
    pub evidence_text: String,
    pub evidence: FlagEvidence,
}

/// Per-tool carry summary (`docs/cli-schema.md` §8).
#[derive(Debug, Clone, Serialize)]
pub struct ToolSpendExportV1 {
    pub tool: String,
    pub calls: u32,
    pub results: u32,
    pub errors: u32,
    pub bytes: u64,
    pub initial_tokens: u64,
    pub carried_tokens: u64,
    pub lifetime_tokens: u64,
    pub confidence: Confidence,
    pub estimation_method: &'static str,
}

/// Per-turn measured and attributed breakdown (`docs/cli-schema.md` §10).
#[derive(Debug, Clone, Serialize)]
pub struct TurnExportV1 {
    pub turn: u32,
    pub ts: Timestamp,
    pub model: Option<String>,
    pub prompt_tokens: u64,
    pub measured: TokenSplit,
    pub measured_confidence: Confidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_5m_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_1h_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    pub categories: CategoryBreakdown,
    pub category_confidence: Confidence,
    pub estimation_method: &'static str,
    pub new_unattributed_prompt_tokens: u64,
    pub hints: TurnHints,
}

/// Builds the structured [`CliExportDocumentV1`] envelope for `sessions`.
#[must_use]
pub fn build_json_export_v1(sessions: &[SessionReport], show_paths: bool) -> CliExportDocumentV1 {
    let mut exported_sessions = Vec::with_capacity(sessions.len());

    for report in sessions {
        let tot = report.totals();
        let total_tokens = tot.total_tokens();
        let session_is_usd = report
            .cost_attribution
            .currency
            .as_deref()
            .is_some_and(|c| c.eq_ignore_ascii_case("USD"));

        let mut breakdown = BTreeMap::new();
        for cat in Category::ALL {
            let split = report.attribution.categories.get(cat);
            let cat_total = split.total_tokens();
            let cost_usd = report
                .cost_attribution
                .category_costs
                .as_ref()
                .filter(|_| session_is_usd)
                .and_then(|m| m.get(&cat).copied());
            breakdown.insert(
                cat,
                CategoryBucketExportV1 {
                    prompt_tokens: split.prompt_tokens(),
                    total_tokens: cat_total,
                    fresh_in: split.fresh_in,
                    cache_write: split.cache_write,
                    cache_read: split.cache_read,
                    output: split.output,
                    share_bps: share_basis_points(cat_total, total_tokens),
                    cost_usd,
                },
            );
        }

        let flags = report
            .flags
            .iter()
            .map(|f| FlagExportV1 {
                id: f.id,
                turn_range: f.turn_range,
                impact_tokens: f.impact_tokens,
                confidence: f.confidence,
                estimation_method: f.estimation_method,
                sentence: render_flag_sentence(f),
                evidence_text: render_flag_evidence(f),
                evidence: f.evidence.clone(),
            })
            .collect();

        let tools = report
            .tools
            .iter()
            .map(|t| ToolSpendExportV1 {
                tool: t.tool.clone(),
                calls: t.calls,
                results: t.results,
                errors: t.errors,
                bytes: t.bytes,
                initial_tokens: t.initial_tokens,
                carried_tokens: t.carried_tokens,
                lifetime_tokens: t.lifetime_tokens,
                confidence: t.confidence,
                estimation_method: t.estimation_method,
            })
            .collect();

        let turns = report
            .turns
            .iter()
            .zip(&report.attribution.turns)
            .enumerate()
            .map(|(idx, (tr, ta))| {
                let tc = report
                    .cost_attribution
                    .turn_costs
                    .get(idx)
                    .and_then(Option::as_ref);
                let turn_cost_usd = tc
                    .filter(|c| c.currency.eq_ignore_ascii_case("USD"))
                    .map(|c| c.total_cost);
                let (w5m, w1h) = match tc {
                    Some(c) => (Some(c.write_tiers.tokens_5m), Some(c.write_tiers.tokens_1h)),
                    None => (None, None),
                };

                TurnExportV1 {
                    turn: tr.turn,
                    ts: tr.ts,
                    model: tr.model.clone(),
                    prompt_tokens: ta.measured.prompt_tokens(),
                    measured: ta.measured,
                    measured_confidence: ta.measured_confidence,
                    cache_write_5m_tokens: w5m,
                    cache_write_1h_tokens: w1h,
                    cost_usd: turn_cost_usd,
                    categories: ta.categories,
                    category_confidence: ta.category_confidence,
                    estimation_method: ta.estimation_method,
                    new_unattributed_prompt_tokens: ta.new_unattributed_prompt_tokens,
                    hints: tr.hints.clone(),
                }
            })
            .collect();

        let export_currency = match report.cost.status {
            CostStatus::Configured | CostStatus::PartialConfigured => report.cost.currency.clone(),
            CostStatus::Unpriced | CostStatus::AgentReported => None,
        };

        exported_sessions.push(SessionExportV1 {
            agent: report.agent,
            session_id: report.session_id.clone(),
            turn_count: report.turn_count(),
            started_at: report.started_at,
            ended_at: report.ended_at,
            models: report.models.clone(),
            project_hash: report.project_hash.clone(),
            project_path: if show_paths {
                report.project_path.clone()
            } else {
                None
            },
            cost: CostExportV1 {
                status: report.cost.status,
                currency: export_currency,
                usd: report.cost.usd,
            },
            totals: TotalsExportV1 {
                prompt_tokens: tot.prompt_tokens(),
                total_tokens,
                fresh_in: tot.fresh_in,
                cache_write: tot.cache_write,
                cache_read: tot.cache_read,
                output: tot.output,
                confidence: report.attribution.totals_confidence,
            },
            fixed_overhead: FixedOverheadExportV1 {
                tokens: report.attribution.fixed_overhead_tokens,
                confidence: Confidence::Estimated,
                estimation_method: report.attribution.estimation_method,
            },
            categories: CategoriesExportV1 {
                confidence: report.attribution.category_confidence,
                estimation_method: report.attribution.estimation_method,
                breakdown,
            },
            flags,
            tools,
            subagents: report.attribution.subagents.clone(),
            turns,
        });
    }

    CliExportDocumentV1 {
        schema_version: product::CLI_EXPORT_SCHEMA_VERSION,
        generator: GeneratorExportV1 {
            name: product::BINARY_NAME.to_string(),
            version: product::VERSION.to_string(),
        },
        session_count: exported_sessions.len(),
        sessions: exported_sessions,
    }
}

/// Serialises `sessions` as a pretty-printed `\n`-terminated UTF-8 JSON string (`schemaVersion: 1`).
pub fn render_json_export_v1(
    sessions: &[SessionReport],
    show_paths: bool,
) -> Result<String, serde_json::Error> {
    let doc = build_json_export_v1(sessions, show_paths);
    let mut out = serde_json::to_string_pretty(&doc)?;
    out.push('\n');
    Ok(out)
}

/// Writes `sessions` as a pretty-printed `\n`-terminated UTF-8 JSON document to `writer`.
pub fn write_json_export_v1<W: Write>(
    writer: &mut W,
    sessions: &[SessionReport],
    show_paths: bool,
) -> std::io::Result<()> {
    let rendered = render_json_export_v1(sessions, show_paths).map_err(std::io::Error::other)?;
    writer.write_all(rendered.as_bytes())
}
