//! Deterministic aligned plain-text CLI table renderers (`PLAN.md` §6.4).
//!
//! Uses pure ASCII alignment with zero ANSI colour-only cues so output is
//! universally readable across terminals, `NO_COLOR=1`, pipes, and `insta` goldens.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use kakeibo_core::attrib::{CategoryBreakdown, TokenSplit};
use kakeibo_core::flags::{FlagFinding, FlagId};
use kakeibo_core::pricing::{CostStatus, PricingCatalog, RateUnit};
use kakeibo_core::{product, Category, Confidence, TurnHints};

use crate::flags::{format_tokens, render_flag_evidence, render_flag_sentence, render_flags_text};
use crate::model::{
    format_currency_amount, format_share_pct, share_basis_points, DoctorReport,
    ObservedModelPriceStatus, SessionReport, TopDimension,
};

fn format_aligned_table(headers: &[&str], right_align: &[bool], rows: &[Vec<String>]) -> String {
    let cols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (idx, cell) in row.iter().enumerate().take(cols) {
            if cell.len() > widths[idx] {
                widths[idx] = cell.len();
            }
        }
    }

    let mut out = String::new();
    append_table_row(
        &mut out,
        &headers.iter().map(|s| (*s).to_string()).collect::<Vec<_>>(),
        &widths,
        right_align,
    );
    let divider: Vec<String> = widths.iter().map(|&w| "-".repeat(w)).collect();
    append_table_row(&mut out, &divider, &widths, right_align);
    for row in rows {
        append_table_row(&mut out, row, &widths, right_align);
    }
    out
}

fn append_table_row(out: &mut String, cells: &[String], widths: &[usize], right_align: &[bool]) {
    let last_col = widths.len().saturating_sub(1);
    for (idx, width) in widths.iter().copied().enumerate() {
        if idx > 0 {
            out.push_str("  ");
        }
        let cell = cells.get(idx).map_or("", String::as_str);
        let is_right = right_align.get(idx).copied().unwrap_or(false);
        if is_right {
            let _ = write!(out, "{cell:>width$}");
        } else if idx == last_col {
            out.push_str(cell);
        } else {
            let _ = write!(out, "{cell:<width$}");
        }
    }
    out.push('\n');
}

fn format_hints(hints: &TurnHints) -> String {
    let mut tags = Vec::new();
    if hints.resume_start {
        tags.push("resume".to_string());
    }
    if hints.compaction {
        tags.push("compact".to_string());
    }
    if hints.sidechain {
        tags.push("sidechain".to_string());
    }
    if hints.api_error {
        tags.push("api_error".to_string());
    }
    if let Some(pct) = hints.rate_limit_used_percent {
        tags.push(format!("quota={pct:.1}%"));
    }
    if tags.is_empty() {
        "-".to_string()
    } else {
        tags.join(",")
    }
}

fn provenance_str(confidence: Confidence, method: Option<&str>) -> String {
    match (confidence, method) {
        (Confidence::Estimated, Some(m)) => format!("estimated: {m}"),
        (other, _) => other.as_str().to_string(),
    }
}

/// Renders the session list table used by `kakeibo` (default view) and `kakeibo sessions`.
#[must_use]
pub fn render_sessions_table(sessions: &[SessionReport]) -> String {
    if sessions.is_empty() {
        return "No sessions found.\n".to_string();
    }

    let headers = [
        "SESSION",
        "AGENT",
        "LAST_ACTIVE",
        "TURNS",
        "PROMPT",
        "FRESH/WRITE/READ",
        "OUT",
        "COST",
        "TOP_FLAG",
    ];
    let right_align = [false, false, false, true, true, false, true, false, false];

    let rows: Vec<Vec<String>> = sessions
        .iter()
        .map(|s| {
            let tot = s.totals();
            let top_flag = s
                .flags
                .first()
                .map_or_else(|| "none".to_string(), |f| f.id.to_string());
            vec![
                s.short_id().to_string(),
                s.agent.to_string(),
                s.ended_at.to_string(),
                s.turn_count().to_string(),
                format_tokens(tot.prompt_tokens()),
                format!(
                    "{}/{}/{}",
                    format_tokens(tot.fresh_in),
                    format_tokens(tot.cache_write),
                    format_tokens(tot.cache_read),
                ),
                format_tokens(tot.output),
                s.cost.display_label(),
                top_flag,
            ]
        })
        .collect();

    format_aligned_table(&headers, &right_align, &rows)
}

/// Renders a full terminal autopsy for one [`SessionReport`] (`kakeibo autopsy`).
#[must_use]
pub fn render_autopsy_report(
    report: &SessionReport,
    show_turns: bool,
    category_filter: Option<Category>,
) -> String {
    let mut out = String::new();
    let tot = report.totals();
    let total_tokens = tot.total_tokens();
    let models_str = if report.models.is_empty() {
        "unknown".to_string()
    } else {
        report.models.join(", ")
    };

    let _ = writeln!(
        out,
        "Session:        {} ({})",
        report.session_id, report.agent
    );
    let _ = writeln!(
        out,
        "Window:         {} .. {} ({} turns)",
        report.started_at,
        report.ended_at,
        report.turn_count()
    );
    let _ = writeln!(out, "Models:         {models_str}");
    let _ = writeln!(out, "Project:        {}", report.project_label(false));
    let _ = writeln!(
        out,
        "Totals:         {} prompt ({} fresh / {} write / {} read) + {} output = {} tokens ({})",
        format_tokens(tot.prompt_tokens()),
        format_tokens(tot.fresh_in),
        format_tokens(tot.cache_write),
        format_tokens(tot.cache_read),
        format_tokens(tot.output),
        format_tokens(total_tokens),
        report.attribution.totals_confidence,
    );
    let _ = writeln!(
        out,
        "Fixed overhead: {} tokens (estimated: {})",
        format_tokens(report.attribution.fixed_overhead_tokens),
        report.attribution.estimation_method,
    );
    let _ = writeln!(out, "Cost:           {}", report.cost.display_label());

    let cat_prov = provenance_str(
        report.attribution.category_confidence,
        Some(report.attribution.estimation_method),
    );
    let _ = writeln!(out, "\nCategories ({cat_prov}):");
    let categories_to_show: Vec<Category> = match category_filter {
        Some(single) => vec![single],
        None => Category::ALL.to_vec(),
    };
    if let Some(ref cat_costs) = report.cost_attribution.category_costs {
        let curr = report.cost.currency.as_deref().unwrap_or("USD");
        let cat_headers = [
            "CATEGORY", "TOTAL", "SHARE", "FRESH", "WRITE", "READ", "OUT", "COST",
        ];
        let cat_align = [false, true, true, true, true, true, true, true];
        let cat_rows: Vec<Vec<String>> = categories_to_show
            .into_iter()
            .map(|cat| {
                let split = report.attribution.categories.get(cat);
                let cat_tot = split.total_tokens();
                let bps = share_basis_points(cat_tot, total_tokens);
                let cat_val = cat_costs.get(&cat).copied().unwrap_or(0.0);
                vec![
                    cat.to_string(),
                    format_tokens(cat_tot),
                    format_share_pct(bps),
                    format_tokens(split.fresh_in),
                    format_tokens(split.cache_write),
                    format_tokens(split.cache_read),
                    format_tokens(split.output),
                    format_currency_amount(curr, cat_val),
                ]
            })
            .collect();
        out.push_str(&format_aligned_table(&cat_headers, &cat_align, &cat_rows));
    } else {
        let cat_headers = [
            "CATEGORY", "TOTAL", "SHARE", "FRESH", "WRITE", "READ", "OUT",
        ];
        let cat_align = [false, true, true, true, true, true, true];
        let cat_rows: Vec<Vec<String>> = categories_to_show
            .into_iter()
            .map(|cat| {
                let split = report.attribution.categories.get(cat);
                let cat_tot = split.total_tokens();
                let bps = share_basis_points(cat_tot, total_tokens);
                vec![
                    cat.to_string(),
                    format_tokens(cat_tot),
                    format_share_pct(bps),
                    format_tokens(split.fresh_in),
                    format_tokens(split.cache_write),
                    format_tokens(split.cache_read),
                    format_tokens(split.output),
                ]
            })
            .collect();
        out.push_str(&format_aligned_table(&cat_headers, &cat_align, &cat_rows));
    }

    out.push_str("\nFlags:\n");
    out.push_str(&render_flags_text(&report.flags));

    let show_tools = !report.tools.is_empty()
        && category_filter.is_none_or(|c| matches!(c, Category::ToolOutput | Category::Retries));
    if show_tools {
        let _ = writeln!(
            out,
            "\nTools (estimated: {}):",
            report.attribution.estimation_method
        );
        let tool_headers = [
            "TOOL", "CALLS", "RESULTS", "ERRORS", "BYTES", "INITIAL", "CARRIED", "LIFETIME",
        ];
        let tool_align = [false, true, true, true, true, true, true, true];
        let tool_rows: Vec<Vec<String>> = report
            .tools
            .iter()
            .map(|t| {
                vec![
                    t.tool.clone(),
                    t.calls.to_string(),
                    t.results.to_string(),
                    t.errors.to_string(),
                    format_tokens(t.bytes),
                    format_tokens(t.initial_tokens),
                    format_tokens(t.carried_tokens),
                    format_tokens(t.lifetime_tokens),
                ]
            })
            .collect();
        out.push_str(&format_aligned_table(
            &tool_headers,
            &tool_align,
            &tool_rows,
        ));
    }

    if !report.attribution.subagents.is_empty()
        && category_filter.is_none_or(|c| c == Category::Subagents)
    {
        out.push_str("\nSubagents:\n");
        let sub_headers = ["SUBAGENT", "TURNS", "PROMPT", "OUT", "TOTAL"];
        let sub_align = [false, true, true, true, true];
        let sub_rows: Vec<Vec<String>> = report
            .attribution
            .subagents
            .iter()
            .map(|sub| {
                vec![
                    sub.session_id.clone(),
                    sub.turns.to_string(),
                    format_tokens(sub.totals.prompt_tokens()),
                    format_tokens(sub.totals.output),
                    format_tokens(sub.totals.total_tokens()),
                ]
            })
            .collect();
        out.push_str(&format_aligned_table(&sub_headers, &sub_align, &sub_rows));
    }

    if show_turns {
        out.push_str("\nTurns (measured):\n");
        if report.has_any_priced_turn() {
            let turn_headers = [
                "TURN",
                "TIMESTAMP",
                "MODEL",
                "PROMPT",
                "FRESH",
                "WRITE",
                "READ",
                "OUT",
                "COST",
                "HINTS",
            ];
            let turn_align = [
                true, false, false, true, true, true, true, true, true, false,
            ];
            let turn_rows: Vec<Vec<String>> = report
                .turns
                .iter()
                .zip(&report.attribution.turns)
                .enumerate()
                .map(|(idx, (tr, ta))| {
                    let display_split = match category_filter {
                        Some(cat) => ta.categories.get(cat),
                        None => ta.measured,
                    };
                    let cost_cell = match report
                        .cost_attribution
                        .turn_costs
                        .get(idx)
                        .and_then(Option::as_ref)
                    {
                        Some(tc) => {
                            let val = match category_filter {
                                Some(cat) => tc.category_costs.get(&cat).copied().unwrap_or(0.0),
                                None => tc.total_cost,
                            };
                            format_currency_amount(&tc.currency, val)
                        }
                        None => "unpriced".to_string(),
                    };
                    vec![
                        tr.turn.to_string(),
                        tr.ts.to_string(),
                        tr.model.as_deref().unwrap_or("unknown").to_string(),
                        format_tokens(display_split.prompt_tokens()),
                        format_tokens(display_split.fresh_in),
                        format_tokens(display_split.cache_write),
                        format_tokens(display_split.cache_read),
                        format_tokens(display_split.output),
                        cost_cell,
                        format_hints(&tr.hints),
                    ]
                })
                .collect();
            out.push_str(&format_aligned_table(
                &turn_headers,
                &turn_align,
                &turn_rows,
            ));
        } else {
            let turn_headers = [
                "TURN",
                "TIMESTAMP",
                "MODEL",
                "PROMPT",
                "FRESH",
                "WRITE",
                "READ",
                "OUT",
                "HINTS",
            ];
            let turn_align = [true, false, false, true, true, true, true, true, false];
            let turn_rows: Vec<Vec<String>> = report
                .turns
                .iter()
                .zip(&report.attribution.turns)
                .map(|(tr, ta)| {
                    let display_split = match category_filter {
                        Some(cat) => ta.categories.get(cat),
                        None => ta.measured,
                    };
                    vec![
                        tr.turn.to_string(),
                        tr.ts.to_string(),
                        tr.model.as_deref().unwrap_or("unknown").to_string(),
                        format_tokens(display_split.prompt_tokens()),
                        format_tokens(display_split.fresh_in),
                        format_tokens(display_split.cache_write),
                        format_tokens(display_split.cache_read),
                        format_tokens(display_split.output),
                        format_hints(&tr.hints),
                    ]
                })
                .collect();
            out.push_str(&format_aligned_table(
                &turn_headers,
                &turn_align,
                &turn_rows,
            ));
        }
    }

    out
}

/// Renders ranked root-cause flags across multiple sessions (`kakeibo flags`).
#[must_use]
pub fn render_cross_session_flags(sessions: &[SessionReport]) -> String {
    let mut all: Vec<(&SessionReport, &FlagFinding)> = sessions
        .iter()
        .flat_map(|s| s.flags.iter().map(move |f| (s, f)))
        .collect();

    if all.is_empty() {
        return "No flags.\n".to_string();
    }

    all.sort_by(|a, b| {
        b.1.impact_tokens
            .cmp(&a.1.impact_tokens)
            .then_with(|| b.0.ended_at.cmp(&a.0.ended_at))
            .then_with(|| a.0.session_id.cmp(&b.0.session_id))
            .then_with(|| a.1.turn_range.cmp(&b.1.turn_range))
            .then_with(|| a.1.id.cmp(&b.1.id))
    });

    let blocks: Vec<String> = all
        .into_iter()
        .map(|(sess, finding)| {
            let prov = provenance_str(finding.confidence, finding.estimation_method);
            let impact = format_tokens(finding.impact_tokens);
            let sentence = render_flag_sentence(finding);
            let evidence = render_flag_evidence(finding);
            format!(
                "[{}] session={} ({}) turns={} ({impact} tokens, {prov})\n  {sentence}\n  {evidence}",
                finding.id,
                sess.short_id(),
                sess.agent,
                finding.turn_range,
            )
        })
        .collect();

    let mut out = blocks.join("\n\n");
    out.push('\n');
    out
}

/// Renders the `kakeibo top --by <category|tool|project|flag>` cross-session leaderboard.
#[must_use]
pub fn render_top_table(sessions: &[SessionReport], by: TopDimension) -> String {
    if sessions.is_empty() {
        return "No sessions found.\n".to_string();
    }

    match by {
        TopDimension::Category => render_top_by_category(sessions),
        TopDimension::Tool => render_top_by_tool(sessions),
        TopDimension::Project => render_top_by_project(sessions),
        TopDimension::Flag => render_top_by_flag(sessions),
    }
}

fn render_top_by_category(sessions: &[SessionReport]) -> String {
    let mut totals = CategoryBreakdown::new();
    let mut session_counts = [0_usize; Category::ALL.len()];
    let mut grand_total = 0_u64;
    let method = sessions[0].attribution.estimation_method;

    for s in sessions {
        totals.add_breakdown(&s.attribution.categories);
        grand_total = grand_total.saturating_add(s.totals().total_tokens());
        for (idx, cat) in Category::ALL.into_iter().enumerate() {
            if !s.attribution.categories.get(cat).is_zero() {
                session_counts[idx] += 1;
            }
        }
    }

    let mut items: Vec<(Category, usize, TokenSplit)> = Category::ALL
        .into_iter()
        .enumerate()
        .map(|(idx, cat)| (cat, session_counts[idx], totals.get(cat)))
        .collect();
    items.sort_by(|a, b| {
        b.2.total_tokens()
            .cmp(&a.2.total_tokens())
            .then_with(|| a.0.as_str().cmp(b.0.as_str()))
    });

    let headers = [
        "CATEGORY",
        "SESSIONS",
        "TOTAL",
        "SHARE",
        "FRESH",
        "WRITE",
        "READ",
        "OUT",
        "PROVENANCE",
    ];
    let align = [false, true, true, true, true, true, true, true, false];
    let prov = format!("estimated: {method}");
    let rows: Vec<Vec<String>> = items
        .into_iter()
        .map(|(cat, count, split)| {
            let cat_tot = split.total_tokens();
            let bps = share_basis_points(cat_tot, grand_total);
            vec![
                cat.to_string(),
                count.to_string(),
                format_tokens(cat_tot),
                format_share_pct(bps),
                format_tokens(split.fresh_in),
                format_tokens(split.cache_write),
                format_tokens(split.cache_read),
                format_tokens(split.output),
                prov.clone(),
            ]
        })
        .collect();

    format_aligned_table(&headers, &align, &rows)
}

#[derive(Default)]
struct CrossToolAgg {
    sessions: BTreeSet<String>,
    calls: u32,
    results: u32,
    bytes: u64,
    carried_tokens: u64,
    lifetime_tokens: u64,
}

fn render_top_by_tool(sessions: &[SessionReport]) -> String {
    let mut map: BTreeMap<String, CrossToolAgg> = BTreeMap::new();
    let method = sessions[0].attribution.estimation_method;

    for s in sessions {
        for t in &s.tools {
            let agg = map.entry(t.tool.clone()).or_default();
            agg.sessions.insert(format!("{}:{}", s.agent, s.session_id));
            agg.calls = agg.calls.saturating_add(t.calls);
            agg.results = agg.results.saturating_add(t.results);
            agg.bytes = agg.bytes.saturating_add(t.bytes);
            agg.carried_tokens = agg.carried_tokens.saturating_add(t.carried_tokens);
            agg.lifetime_tokens = agg.lifetime_tokens.saturating_add(t.lifetime_tokens);
        }
    }

    if map.is_empty() {
        return "No tool usage recorded.\n".to_string();
    }

    let mut list: Vec<(String, CrossToolAgg)> = map.into_iter().collect();
    list.sort_by(|a, b| {
        b.1.lifetime_tokens
            .cmp(&a.1.lifetime_tokens)
            .then_with(|| a.0.cmp(&b.0))
    });

    let headers = [
        "TOOL",
        "SESSIONS",
        "CALLS",
        "RESULTS",
        "BYTES",
        "CARRIED",
        "LIFETIME",
        "PROVENANCE",
    ];
    let align = [false, true, true, true, true, true, true, false];
    let prov = format!("estimated: {method}");
    let rows: Vec<Vec<String>> = list
        .into_iter()
        .map(|(tool, agg)| {
            vec![
                tool,
                agg.sessions.len().to_string(),
                agg.calls.to_string(),
                agg.results.to_string(),
                format_tokens(agg.bytes),
                format_tokens(agg.carried_tokens),
                format_tokens(agg.lifetime_tokens),
                prov.clone(),
            ]
        })
        .collect();

    format_aligned_table(&headers, &align, &rows)
}

#[derive(Default)]
struct CrossProjectAgg {
    sessions: usize,
    turns: u32,
    totals: TokenSplit,
    flags: usize,
    configured_sessions: usize,
    partial_or_unpriced_sessions: usize,
    agent_reported_sessions: usize,
    currencies: BTreeSet<String>,
    cost_sum: f64,
}

impl CrossProjectAgg {
    fn cost_label(&self) -> String {
        if self.currencies.len() != 1 {
            return "unpriced".to_string();
        }
        let curr = self.currencies.iter().next().map_or("USD", String::as_str);
        if self.configured_sessions > 0
            && self.partial_or_unpriced_sessions == 0
            && self.agent_reported_sessions == 0
        {
            format!(
                "{} (API-equiv)",
                format_currency_amount(curr, self.cost_sum)
            )
        } else if self.configured_sessions > 0 {
            format!(
                "{} (partial, API-equiv)",
                format_currency_amount(curr, self.cost_sum)
            )
        } else if self.agent_reported_sessions == self.sessions {
            format!(
                "{} (agent-reported)",
                format_currency_amount(curr, self.cost_sum)
            )
        } else if self.agent_reported_sessions > 0 {
            format!(
                "{} (partial, agent-reported)",
                format_currency_amount(curr, self.cost_sum)
            )
        } else {
            "unpriced".to_string()
        }
    }
}

fn render_top_by_project(sessions: &[SessionReport]) -> String {
    let mut map: BTreeMap<String, CrossProjectAgg> = BTreeMap::new();
    for s in sessions {
        let key = s.project_label(false);
        let agg = map.entry(key).or_default();
        agg.sessions += 1;
        agg.turns = agg.turns.saturating_add(s.turn_count());
        agg.totals.add_assign(s.totals());
        agg.flags += s.flags.len();
        let curr = s.cost.currency.clone().unwrap_or_else(|| "USD".to_string());
        match (s.cost.status, s.cost.amount) {
            (CostStatus::Configured, Some(v)) => {
                agg.configured_sessions += 1;
                agg.cost_sum += v;
                agg.currencies.insert(curr);
            }
            (CostStatus::PartialConfigured, Some(v)) => {
                agg.configured_sessions += 1;
                agg.partial_or_unpriced_sessions += 1;
                agg.cost_sum += v;
                agg.currencies.insert(curr);
            }
            (CostStatus::AgentReported, Some(v)) => {
                agg.agent_reported_sessions += 1;
                agg.cost_sum += v;
                agg.currencies.insert(curr);
            }
            _ => {
                agg.partial_or_unpriced_sessions += 1;
            }
        }
    }

    let mut list: Vec<(String, CrossProjectAgg)> = map.into_iter().collect();
    list.sort_by(|a, b| {
        b.1.totals
            .total_tokens()
            .cmp(&a.1.totals.total_tokens())
            .then_with(|| a.0.cmp(&b.0))
    });

    let headers = [
        "PROJECT", "SESSIONS", "TURNS", "PROMPT", "OUT", "TOTAL", "FLAGS", "COST",
    ];
    let align = [false, true, true, true, true, true, true, false];
    let rows: Vec<Vec<String>> = list
        .into_iter()
        .map(|(proj, agg)| {
            let cost_label = agg.cost_label();
            vec![
                proj,
                agg.sessions.to_string(),
                agg.turns.to_string(),
                format_tokens(agg.totals.prompt_tokens()),
                format_tokens(agg.totals.output),
                format_tokens(agg.totals.total_tokens()),
                agg.flags.to_string(),
                cost_label,
            ]
        })
        .collect();

    format_aligned_table(&headers, &align, &rows)
}

#[derive(Default)]
struct CrossFlagAgg {
    findings: usize,
    sessions: BTreeSet<String>,
    impact_tokens: u64,
}

fn render_top_by_flag(sessions: &[SessionReport]) -> String {
    let mut map: BTreeMap<FlagId, CrossFlagAgg> = BTreeMap::new();
    for s in sessions {
        for f in &s.flags {
            let agg = map.entry(f.id).or_default();
            agg.findings += 1;
            agg.sessions.insert(format!("{}:{}", s.agent, s.session_id));
            agg.impact_tokens = agg.impact_tokens.saturating_add(f.impact_tokens);
        }
    }

    if map.is_empty() {
        return "No flags.\n".to_string();
    }

    let mut list: Vec<(FlagId, CrossFlagAgg)> = map.into_iter().collect();
    list.sort_by(|a, b| {
        b.1.impact_tokens
            .cmp(&a.1.impact_tokens)
            .then_with(|| b.1.findings.cmp(&a.1.findings))
            .then_with(|| a.0.cmp(&b.0))
    });

    let headers = ["FLAG", "FINDINGS", "SESSIONS", "IMPACT_TOKENS"];
    let align = [false, true, true, true];
    let rows: Vec<Vec<String>> = list
        .into_iter()
        .map(|(id, agg)| {
            vec![
                id.to_string(),
                agg.findings.to_string(),
                agg.sessions.len().to_string(),
                format_tokens(agg.impact_tokens),
            ]
        })
        .collect();

    format_aligned_table(&headers, &align, &rows)
}

/// Renders the `kakeibo doctor` diagnostic report (`PLAN.md` §6.4).
#[must_use]
pub fn render_doctor_report(report: &DoctorReport) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} doctor ({} {})\n",
        product::DISPLAY_NAME,
        product::BINARY_NAME,
        product::VERSION
    );

    let headers = [
        "SOURCE",
        "STATUS",
        "FILES",
        "SUBAGENTS",
        "SESSIONS",
        "TURNS",
        "VERSIONS",
        "IGNORED",
        "ISSUES",
    ];
    let align = [false, false, true, true, true, true, false, true, true];
    let rows: Vec<Vec<String>> = report
        .sources
        .iter()
        .map(|src| {
            let status = if src.root_exists { "found" } else { "missing" };
            let versions = if src.cli_versions.is_empty() {
                "-".to_string()
            } else {
                src.cli_versions.join(",")
            };
            vec![
                src.agent.to_string(),
                status.to_string(),
                src.log_files.to_string(),
                src.subagent_files.to_string(),
                src.sessions.to_string(),
                src.turns.to_string(),
                versions,
                src.ignored_records.to_string(),
                src.parse_issues.to_string(),
            ]
        })
        .collect();
    out.push_str(&format_aligned_table(&headers, &align, &rows));

    let mut unknown_lines = Vec::new();
    for src in &report.sources {
        for (rec_type, count) in &src.unknown_record_types {
            unknown_lines.push(format!("  {} {}: {}", src.agent, rec_type, count));
        }
    }

    if unknown_lines.is_empty() {
        out.push_str("\nUnknown record types: none\n");
    } else {
        out.push_str("\nUnknown record types:\n");
        for line in unknown_lines {
            out.push_str(&line);
            out.push('\n');
        }
    }

    out
}

fn format_opt_rate_per_1m(val: Option<f64>) -> String {
    let Some(v) = val else {
        return "-".to_string();
    };
    let formatted = format!("{v:.4}");
    let trimmed = formatted.trim_end_matches('0');
    if let Some(int_part) = trimmed.strip_suffix('.') {
        format!("{int_part}.00")
    } else if trimmed
        .split_once('.')
        .is_some_and(|(_, frac)| frac.len() == 1)
    {
        format!("{trimmed}0")
    } else {
        trimmed.to_string()
    }
}

/// Renders the effective pricing catalog table (`pricing list`, `PLAN.md` §6.4, §10 T09).
#[must_use]
pub fn render_pricing_list(catalog: &PricingCatalog, has_user_overlay: bool) -> String {
    let mut out = String::new();
    let overlay_str = if has_user_overlay { "yes" } else { "no" };
    let _ = writeln!(
        out,
        "Pricing catalog: schema={} unit={} user_overlay={overlay_str}\n",
        catalog.schema_version,
        RateUnit::Per1MTokens,
    );

    if catalog.models.is_empty() {
        out.push_str("No pricing entries configured.\n");
        return out;
    }

    let headers = [
        "MODEL",
        "STATUS",
        "CURR",
        "INPUT/1M",
        "WRITE_5M/1M",
        "WRITE_1H/1M",
        "READ/1M",
        "OUTPUT/1M",
        "REASON/1M",
        "CHECKED",
        "SOURCE",
    ];
    let align = [
        false, false, false, true, true, true, true, true, true, false, false,
    ];
    let rows: Vec<Vec<String>> = catalog
        .models
        .iter()
        .map(|(key, raw_entry)| {
            let entry = raw_entry.converted(catalog.rate_unit, RateUnit::Per1MTokens);
            let status = if entry.is_priced() {
                "configured"
            } else {
                "unpriced"
            };
            vec![
                key.clone(),
                status.to_string(),
                entry.currency,
                format_opt_rate_per_1m(entry.input),
                format_opt_rate_per_1m(entry.cache_write_5m),
                format_opt_rate_per_1m(entry.cache_write_1h),
                format_opt_rate_per_1m(entry.cache_read),
                format_opt_rate_per_1m(entry.output),
                format_opt_rate_per_1m(entry.reasoning),
                entry.checked,
                entry.source,
            ]
        })
        .collect();
    out.push_str(&format_aligned_table(&headers, &align, &rows));
    out
}

/// Renders observed session models and their pricing resolution status (`pricing check`, `PLAN.md` §6.4, §10 T09).
#[must_use]
pub fn render_pricing_check(observed: &[ObservedModelPriceStatus]) -> String {
    if observed.is_empty() {
        return "No models observed in discovered session logs.\n".to_string();
    }

    let headers = [
        "MODEL",
        "STATUS",
        "MATCHED_KEY",
        "SESSIONS",
        "TURNS",
        "PROMPT",
        "OUT",
    ];
    let align = [false, false, false, true, true, true, true];
    let mut unpriced_count = 0_usize;
    let rows: Vec<Vec<String>> = observed
        .iter()
        .map(|item| {
            let status = if item.is_priced {
                "configured"
            } else {
                unpriced_count += 1;
                "unpriced"
            };
            vec![
                item.model.clone(),
                status.to_string(),
                item.matched_key.clone().unwrap_or_else(|| "-".to_string()),
                item.sessions.to_string(),
                item.turns.to_string(),
                format_tokens(item.prompt_tokens),
                format_tokens(item.output_tokens),
            ]
        })
        .collect();

    let mut out = format_aligned_table(&headers, &align, &rows);
    if unpriced_count == 0 {
        let _ = writeln!(
            out,
            "\nAll {} observed model(s) have configured pricing.",
            observed.len()
        );
    } else {
        let _ = writeln!(
            out,
            "\n{unpriced_count} of {} observed model(s) unpriced. Run `{} pricing import <litellm.json>` to import local rates.",
            observed.len(),
            product::BINARY_NAME
        );
    }
    out
}
