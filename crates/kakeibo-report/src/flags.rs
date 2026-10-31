//! Plain-words root-cause flag sentence and evidence text renderers (`PLAN.md` §0 #7, §6.3).

use kakeibo_core::flags::{ColdResumeTrigger, FlagEvidence, FlagFinding, TurnRange};
use kakeibo_core::{Confidence, ESTIMATION_METHOD_SCALED_BYTES_DIV4};

/// Formats an integer token or byte figure with deterministic ASCII comma thousands separators.
#[must_use]
pub fn format_tokens(n: u64) -> String {
    let digits = n.to_string();
    let len = digits.len();
    if len <= 3 {
        return digits;
    }
    let mut out = String::with_capacity(len + (len - 1) / 3);
    for (idx, ch) in digits.chars().enumerate() {
        if idx > 0 && (len - idx).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Formats elapsed whole seconds into a compact human-readable duration (`45m`, `12m 10s`, `1h 5m`).
#[must_use]
pub fn format_duration_seconds(seconds: u64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    let mut parts = Vec::with_capacity(3);
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if secs > 0 {
        parts.push(format!("{secs}s"));
    }
    parts.join(" ")
}

fn format_turn_phrase(range: TurnRange) -> String {
    if range.is_single() {
        format!("turn {}", range.start)
    } else {
        format!("turns {range}")
    }
}

fn format_provenance(confidence: Confidence, estimation_method: Option<&str>) -> String {
    match (confidence, estimation_method) {
        (Confidence::Estimated, Some(method)) => format!("estimated: {method}"),
        (other, _) => other.as_str().to_string(),
    }
}

/// Renders the one-sentence plain-words explanation for `finding` (`PLAN.md` §6.3).
///
/// Never claims a provider-side cause.
#[must_use]
pub fn render_flag_sentence(finding: &FlagFinding) -> String {
    let turn_phrase = format_turn_phrase(finding.turn_range);
    match &finding.evidence {
        FlagEvidence::ColdResume {
            trigger,
            gap_seconds,
            prompt_tokens,
            rewritten_tokens,
            cache_write_pct,
            ..
        } => {
            let rewritten = format_tokens(*rewritten_tokens);
            let prompt = format_tokens(*prompt_tokens);
            match (trigger, gap_seconds) {
                (ColdResumeTrigger::ResumeAfterIdleGap, Some(gap)) => {
                    let dur = format_duration_seconds(*gap);
                    format!(
                        "Resumed request on {turn_phrase} after a {dur} idle gap rewrote {rewritten} of {prompt} prompt tokens ({cache_write_pct}%) into cache."
                    )
                }
                (ColdResumeTrigger::IdleGap, Some(gap)) => {
                    let dur = format_duration_seconds(*gap);
                    format!(
                        "Request on {turn_phrase} after a {dur} idle gap rewrote {rewritten} of {prompt} prompt tokens ({cache_write_pct}%) into cache."
                    )
                }
                _ => format!(
                    "Resumed request on {turn_phrase} rewrote {rewritten} of {prompt} prompt tokens ({cache_write_pct}%) into cache."
                ),
            }
        }
        FlagEvidence::NearFullContext {
            request_count,
            context_window_tokens,
            peak_window_pct,
            tokens_resent,
            ..
        } => {
            let window = format_tokens(*context_window_tokens);
            let resent = format_tokens(*tokens_resent);
            format!(
                "{request_count} consecutive requests on {turn_phrase} ran at up to {peak_window_pct}% of the {window}-token context window, resending {resent} prompt tokens."
            )
        }
        FlagEvidence::ToolOutputBloat {
            tool,
            source_turn,
            size_tokens,
            carried_turns,
            carried_tokens,
            ..
        } => {
            let size = format_tokens(*size_tokens);
            let carried = format_tokens(*carried_tokens);
            format!(
                "{tool} tool output from turn {source_turn} (~{size} tokens) was carried across {carried_turns} later requests ({turn_phrase}) for ~{carried} carried tokens."
            )
        }
        FlagEvidence::CacheMissStorm {
            miss_requests,
            rewritten_tokens,
            likely_trigger,
            ..
        } => {
            let rewritten = format_tokens(*rewritten_tokens);
            let trigger = likely_trigger.label();
            format!(
                "{miss_requests} mid-session requests on {turn_phrase} rewrote {rewritten} prompt tokens into cache instead of hitting cache (likely {trigger})."
            )
        }
        FlagEvidence::HeavyOverhead {
            first_prompt_tokens,
            fixed_overhead_tokens,
            ..
        } => {
            let p1 = format_tokens(*first_prompt_tokens);
            let fixed = format_tokens(*fixed_overhead_tokens);
            format!(
                "First request on {turn_phrase} started with {p1} prompt tokens before conversation growth (~{fixed} fixed overhead tokens)."
            )
        }
    }
}

/// Renders the structured, provenance-labelled evidence line for `finding` (`PLAN.md` §0 #7, §6.3).
#[must_use]
pub fn render_flag_evidence(finding: &FlagFinding) -> String {
    match &finding.evidence {
        FlagEvidence::ColdResume {
            trigger,
            gap_seconds,
            prompt_tokens,
            rewritten_tokens,
            cache_write_pct,
            reference_url,
        } => {
            let gap_part = match gap_seconds {
                Some(secs) => format!(
                    "gap={} ({}s), ",
                    format_duration_seconds(*secs),
                    format_tokens(*secs)
                ),
                None => "gap=n/a, ".to_string(),
            };
            format!(
                "evidence: {gap_part}P={} (measured), rewritten={} (measured, {cache_write_pct}%), trigger={}; see {reference_url}",
                format_tokens(*prompt_tokens),
                format_tokens(*rewritten_tokens),
                trigger.as_str(),
            )
        }
        FlagEvidence::NearFullContext {
            model,
            request_count,
            context_window_tokens,
            window_source,
            peak_prompt_tokens,
            peak_window_pct,
            tokens_resent,
            cache_read_tokens,
        } => {
            let model_label = model.as_deref().unwrap_or("unknown");
            format!(
                "evidence: turns={} ({request_count} requests), model={model_label}, window={} ({}), peak_P={} (measured, {peak_window_pct}%), tokens_resent={} (measured), cache_read={} (measured)",
                finding.turn_range,
                format_tokens(*context_window_tokens),
                window_source.as_str(),
                format_tokens(*peak_prompt_tokens),
                format_tokens(*tokens_resent),
                format_tokens(*cache_read_tokens),
            )
        }
        FlagEvidence::ToolOutputBloat {
            tool,
            bytes,
            size_tokens,
            carried_turns,
            carried_tokens,
            carried_split,
            ..
        } => {
            let method = finding
                .estimation_method
                .unwrap_or(ESTIMATION_METHOD_SCALED_BYTES_DIV4);
            format!(
                "evidence: tool={tool}, turns={}, bytes={} (measured), size={} (estimated: {method}), carried_turns={carried_turns}, carried_cost={} (estimated: {method}; cache_read={}, cache_write={}, fresh_in={})",
                finding.turn_range,
                format_tokens(*bytes),
                format_tokens(*size_tokens),
                format_tokens(*carried_tokens),
                format_tokens(carried_split.cache_read),
                format_tokens(carried_split.cache_write),
                format_tokens(carried_split.fresh_in),
            )
        }
        FlagEvidence::CacheMissStorm {
            miss_requests,
            rewritten_tokens,
            cache_read_tokens,
            likely_trigger,
        } => {
            format!(
                "evidence: turns={} ({miss_requests} requests), rewritten={} (measured), cache_read={} (measured), likely_trigger={}",
                finding.turn_range,
                format_tokens(*rewritten_tokens),
                format_tokens(*cache_read_tokens),
                likely_trigger.label(),
            )
        }
        FlagEvidence::HeavyOverhead {
            first_prompt_tokens,
            fixed_overhead_tokens,
            user_median_p1,
            remediation_hint,
            ..
        } => {
            let median_part = match user_median_p1 {
                Some(med) => format!(", user_median_P(1)={} (measured)", format_tokens(*med)),
                None => String::new(),
            };
            format!(
                "evidence: turn={}, P(1)={} (measured), fixed_overhead={} (estimated: {}){median_part}; {remediation_hint}",
                finding.turn_range,
                format_tokens(*first_prompt_tokens),
                format_tokens(*fixed_overhead_tokens),
                ESTIMATION_METHOD_SCALED_BYTES_DIV4,
            )
        }
    }
}

/// Renders one [`FlagFinding`] block (header, one-sentence summary, and evidence line).
#[must_use]
pub fn render_flag_finding(finding: &FlagFinding) -> String {
    let prov = format_provenance(finding.confidence, finding.estimation_method);
    let impact = format_tokens(finding.impact_tokens);
    let sentence = render_flag_sentence(finding);
    let evidence = render_flag_evidence(finding);
    format!(
        "[{}] turns={} ({impact} tokens, {prov})\n  {sentence}\n  {evidence}",
        finding.id, finding.turn_range,
    )
}

/// Renders a ranked slice of [`FlagFinding`] items as deterministic plain text.
#[must_use]
pub fn render_flags_text(findings: &[FlagFinding]) -> String {
    if findings.is_empty() {
        return "No flags.\n".to_string();
    }
    let blocks: Vec<String> = findings.iter().map(render_flag_finding).collect();
    let mut out = blocks.join("\n\n");
    out.push('\n');
    out
}
