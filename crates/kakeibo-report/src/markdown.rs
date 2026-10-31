//! GitHub-Flavored Markdown session autopsy exporter (`PLAN.md` §6.4, §10 T08).
//!
//! Pathless by default: projects render as `proj:<cwd_hash>` unless `show_paths` is `true`.

use std::fmt::Write as _;

use kakeibo_core::{product, Category, Confidence};

use crate::flags::{format_tokens, render_flag_evidence, render_flag_sentence};
use crate::model::{format_share_pct, share_basis_points, SessionReport};

fn provenance_label(confidence: Confidence, method: Option<&str>) -> String {
    match (confidence, method) {
        (Confidence::Estimated, Some(m)) => format!("estimated: {m}"),
        (other, _) => other.as_str().to_string(),
    }
}

/// Renders one or more [`SessionReport`] items as deterministic GitHub-Flavored Markdown.
#[must_use]
pub fn render_markdown_export(sessions: &[SessionReport], show_paths: bool) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# {} Session Autopsy\n", product::DISPLAY_NAME);

    if sessions.is_empty() {
        out.push_str("No sessions matched.\n");
        return out;
    }

    for (idx, report) in sessions.iter().enumerate() {
        if idx > 0 {
            out.push_str("\n---\n\n");
        }

        let tot = report.totals();
        let total_tokens = tot.total_tokens();
        let models_label = if report.models.is_empty() {
            "unknown".to_string()
        } else {
            report
                .models
                .iter()
                .map(|m| format!("`{m}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };

        let _ = writeln!(
            out,
            "## Session `{}` (`{}`)\n",
            report.session_id, report.agent
        );
        let _ = writeln!(
            out,
            "- **Window:** `{}` to `{}` ({} turns)",
            report.started_at,
            report.ended_at,
            report.turn_count()
        );
        let _ = writeln!(out, "- **Models:** {models_label}");
        let _ = writeln!(out, "- **Project:** `{}`", report.project_label(show_paths));
        let _ = writeln!(
            out,
            "- **Totals:** {} prompt ({} fresh / {} write / {} read) + {} output = {} tokens ({})",
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
            "- **Fixed Overhead:** ~{} tokens (estimated: `{}`)",
            format_tokens(report.attribution.fixed_overhead_tokens),
            report.attribution.estimation_method,
        );
        let _ = writeln!(out, "- **Cost:** {}\n", report.cost.display_label());

        out.push_str("### Category Breakdown\n\n");
        out.push_str(
            "| Category | Total | Share | Fresh In | Cache Write | Cache Read | Output | Provenance |\n",
        );
        out.push_str("|---|---:|---:|---:|---:|---:|---:|---|\n");
        let cat_prov = provenance_label(
            report.attribution.category_confidence,
            Some(report.attribution.estimation_method),
        );
        for cat in Category::ALL {
            let split = report.attribution.categories.get(cat);
            let cat_total = split.total_tokens();
            let bps = share_basis_points(cat_total, total_tokens);
            let _ = writeln!(
                out,
                "| `{}` | {} | {} | {} | {} | {} | {} | {} |",
                cat,
                format_tokens(cat_total),
                format_share_pct(bps),
                format_tokens(split.fresh_in),
                format_tokens(split.cache_write),
                format_tokens(split.cache_read),
                format_tokens(split.output),
                cat_prov,
            );
        }

        out.push_str("\n### Root-Cause Flags\n\n");
        if report.flags.is_empty() {
            out.push_str("No flags.\n");
        } else {
            for finding in &report.flags {
                let prov = provenance_label(finding.confidence, finding.estimation_method);
                let sentence = render_flag_sentence(finding);
                let evidence = render_flag_evidence(finding);
                let _ = writeln!(
                    out,
                    "- **`{}`** (turns `{}`, {} tokens, {}): {}\n  - `{}`",
                    finding.id,
                    finding.turn_range,
                    format_tokens(finding.impact_tokens),
                    prov,
                    sentence,
                    evidence,
                );
            }
        }

        if !report.tools.is_empty() {
            out.push_str("\n### Tool Carry\n\n");
            out.push_str(
                "| Tool | Calls | Results | Errors | Bytes (measured) | Initial | Carried | Lifetime (estimated) |\n",
            );
            out.push_str("|---|---:|---:|---:|---:|---:|---:|---:|\n");
            for tool in &report.tools {
                let _ = writeln!(
                    out,
                    "| `{}` | {} | {} | {} | {} | {} | {} | {} |",
                    tool.tool,
                    tool.calls,
                    tool.results,
                    tool.errors,
                    format_tokens(tool.bytes),
                    format_tokens(tool.initial_tokens),
                    format_tokens(tool.carried_tokens),
                    format_tokens(tool.lifetime_tokens),
                );
            }
        }

        if !report.attribution.subagents.is_empty() {
            out.push_str("\n### Subagents\n\n");
            out.push_str("| Subagent Session | Turns | Prompt | Output | Total |\n");
            out.push_str("|---|---:|---:|---:|---:|\n");
            for sub in &report.attribution.subagents {
                let _ = writeln!(
                    out,
                    "| `{}` | {} | {} | {} | {} |",
                    sub.session_id,
                    sub.turns,
                    format_tokens(sub.totals.prompt_tokens()),
                    format_tokens(sub.totals.output),
                    format_tokens(sub.totals.total_tokens()),
                );
            }
        }

        out.push_str("\n### Per-Turn Breakdown\n\n");
        out.push_str(
            "| Turn | Timestamp | Model | Prompt (P) | Fresh In | Cache Write | Cache Read | Output | Unattributed Delta |\n",
        );
        out.push_str("|---:|---|---|---:|---:|---:|---:|---:|---:|\n");
        for (tr, ta) in report.turns.iter().zip(&report.attribution.turns) {
            let model_str = tr.model.as_deref().unwrap_or("unknown");
            let _ = writeln!(
                out,
                "| {} | `{}` | `{}` | {} | {} | {} | {} | {} | {} |",
                tr.turn,
                tr.ts,
                model_str,
                format_tokens(ta.measured.prompt_tokens()),
                format_tokens(ta.measured.fresh_in),
                format_tokens(ta.measured.cache_write),
                format_tokens(ta.measured.cache_read),
                format_tokens(ta.measured.output),
                format_tokens(ta.new_unattributed_prompt_tokens),
            );
        }
    }

    out
}
