//! Read-only discovery, subagent linking, attribution, flag evaluation, and filtering pipeline.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;

use kakeibo_core::attrib::attribute_session_with_subagents;
use kakeibo_core::flags::{evaluate_flags_v1, FlagsConfig, ModelCatalog};
use kakeibo_core::{AgentId, TurnRecord};
use kakeibo_parse::hash::hash_cwd;
use kakeibo_parse::{
    extract_claude_session_cwds, extract_codex_session_cwds, parse_claude_file,
    parse_claude_file_with_options, parse_codex_file, ClaudeParseOptions, PathCatalog,
};
use kakeibo_report::{sort_session_reports, DoctorReport, DoctorSourceSummary, SessionReport};

type SessionKey = (AgentId, String);

fn dedupe_and_renumber_turns(mut turns: Vec<TurnRecord>) -> Vec<TurnRecord> {
    if turns.len() <= 1 {
        if let Some(first) = turns.first_mut() {
            first.turn = 1;
        }
        return turns;
    }

    turns.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.turn.cmp(&b.turn)));

    // Guard against duplicate files (e.g. an identical rollout present in both
    // `sessions/` and `archived_sessions/`).
    let mut deduped: Vec<TurnRecord> = Vec::with_capacity(turns.len());
    for tr in turns {
        let is_exact_dup = deduped.last().is_some_and(|prev| {
            prev.ts == tr.ts
                && prev.turn == tr.turn
                && prev.usage == tr.usage
                && prev.added == tr.added
        });
        if !is_exact_dup {
            deduped.push(tr);
        }
    }

    for (idx, tr) in deduped.iter_mut().enumerate() {
        tr.turn = u32::try_from(idx + 1).unwrap_or(u32::MAX);
    }
    deduped
}

fn compute_corpus_median_p1(
    primary_sessions: &BTreeMap<SessionKey, Vec<TurnRecord>>,
) -> Option<u64> {
    let mut p1s: Vec<u64> = primary_sessions
        .values()
        .filter_map(|turns| {
            turns
                .iter()
                .find(|tr| !tr.hints.sidechain)
                .map(|tr| tr.usage.prompt_tokens())
        })
        .collect();

    if p1s.len() < 3 {
        return None;
    }
    p1s.sort_unstable();
    let mid = p1s.len() / 2;
    if p1s.len().is_multiple_of(2) {
        Some((p1s[mid - 1] + p1s[mid]) / 2)
    } else {
        Some(p1s[mid])
    }
}

/// Discovers, decodes, links subagents, attributes, and evaluates v1 flags across
/// local read-only agent logs (`PLAN.md` §3.2).
pub fn load_session_reports(
    catalog: &PathCatalog,
    agent_filter: Option<AgentId>,
    capture_raw_paths: bool,
) -> io::Result<Vec<SessionReport>> {
    let mut primary_turns: BTreeMap<SessionKey, Vec<TurnRecord>> = BTreeMap::new();
    let mut subagents_by_parent: BTreeMap<SessionKey, BTreeMap<String, Vec<TurnRecord>>> =
        BTreeMap::new();
    let mut raw_cwds: BTreeMap<SessionKey, String> = BTreeMap::new();

    if agent_filter.is_none_or(|a| a == AgentId::ClaudeCode) {
        for log in catalog.discover_claude_top_level()? {
            let parsed = parse_claude_file(&log.path)?;
            for tr in parsed.records {
                primary_turns
                    .entry((AgentId::ClaudeCode, tr.session_id.clone()))
                    .or_default()
                    .push(tr);
            }
            if capture_raw_paths {
                for (sid, cwd) in extract_claude_session_cwds(&log.path)? {
                    raw_cwds.insert((AgentId::ClaudeCode, sid), cwd);
                }
            }
        }

        for sub in catalog.discover_claude_subagents()? {
            let opts = ClaudeParseOptions {
                parent_session_id: Some(sub.parent_session_id.clone()),
                max_line_bytes: None,
            };
            let parsed = parse_claude_file_with_options(&sub.path, &opts)?;
            for tr in parsed.records {
                subagents_by_parent
                    .entry((AgentId::ClaudeCode, sub.parent_session_id.clone()))
                    .or_default()
                    .entry(tr.session_id.clone())
                    .or_default()
                    .push(tr);
            }
        }
    }

    if agent_filter.is_none_or(|a| a == AgentId::Codex) {
        let mut codex_candidates: BTreeMap<String, Vec<TurnRecord>> = BTreeMap::new();
        for log in catalog.discover_codex_rollouts()? {
            let parsed = parse_codex_file(&log.path)?;
            for tr in parsed.records {
                codex_candidates
                    .entry(tr.session_id.clone())
                    .or_default()
                    .push(tr);
            }
            if capture_raw_paths {
                for (sid, cwd) in extract_codex_session_cwds(&log.path)? {
                    raw_cwds.insert((AgentId::Codex, sid), cwd);
                }
            }
        }

        // Partition pure Codex sidechain subagent rollouts whose parent session exists in the corpus.
        let known_codex_ids: BTreeSet<String> = codex_candidates.keys().cloned().collect();
        for (sid, turns) in codex_candidates {
            let subagent_parent = turns
                .first()
                .filter(|_| turns.iter().all(|t| t.hints.sidechain))
                .and_then(|first| first.parent_session_id.as_ref())
                .filter(|parent_id| known_codex_ids.contains(*parent_id))
                .cloned();

            if let Some(parent_id) = subagent_parent {
                subagents_by_parent
                    .entry((AgentId::Codex, parent_id))
                    .or_default()
                    .entry(sid)
                    .or_default()
                    .extend(turns);
            } else {
                primary_turns
                    .entry((AgentId::Codex, sid))
                    .or_default()
                    .extend(turns);
            }
        }
    }

    for turns in primary_turns.values_mut() {
        let taken = std::mem::take(turns);
        *turns = dedupe_and_renumber_turns(taken);
    }

    let user_median_p1 = compute_corpus_median_p1(&primary_turns);
    let flags_cfg = FlagsConfig::bundled();
    let models = ModelCatalog::bundled();

    let mut reports = Vec::with_capacity(primary_turns.len());
    for ((agent, sid), turns) in primary_turns {
        let child_vecs: Vec<Vec<TurnRecord>> = subagents_by_parent
            .remove(&(agent, sid.clone()))
            .map(|m| m.into_values().map(dedupe_and_renumber_turns).collect())
            .unwrap_or_default();
        let child_slices: Vec<&[TurnRecord]> = child_vecs.iter().map(Vec::as_slice).collect();

        let attribution = attribute_session_with_subagents(&turns, &child_slices);
        let flags = evaluate_flags_v1(&turns, &attribution, &flags_cfg, &models, user_median_p1);
        let project_path = if capture_raw_paths {
            raw_cwds.remove(&(agent, sid.clone()))
        } else {
            None
        };

        if let Some(report) = SessionReport::build(turns, attribution, flags, project_path) {
            reports.push(report);
        }
    }

    sort_session_reports(&mut reports);
    Ok(reports)
}

/// Matches a [`SessionReport`] against `--project <PATH_OR_HASH>` without storing raw paths in `TurnRecord`.
#[must_use]
pub fn matches_project_filter(report: &SessionReport, query: &str) -> bool {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return true;
    }
    let Some(ref session_hash) = report.project_hash else {
        return false;
    };

    if *session_hash == hash_cwd(trimmed) {
        return true;
    }

    let stripped_slash = trimmed.trim_end_matches(['/', '\\']);
    if !stripped_slash.is_empty() && *session_hash == hash_cwd(stripped_slash) {
        return true;
    }

    if fs::canonicalize(trimmed)
        .ok()
        .and_then(|canon| canon.to_str().map(hash_cwd))
        .is_some_and(|h| *session_hash == h)
    {
        return true;
    }

    let hex_query = trimmed
        .strip_prefix("proj:")
        .unwrap_or(trimmed)
        .to_ascii_lowercase();
    hex_query.len() >= 4
        && hex_query.len() <= 16
        && hex_query.chars().all(|c| c.is_ascii_hexdigit())
        && session_hash.starts_with(&hex_query)
}

/// Resolves `<id|prefix|latest>` against a newest-first sorted slice of [`SessionReport`]s.
pub fn select_session<'a>(
    sessions: &'a [SessionReport],
    selector: &str,
) -> Result<&'a SessionReport, String> {
    if sessions.is_empty() {
        return Err("no sessions found".to_string());
    }
    if selector == "latest" {
        return Ok(&sessions[0]);
    }
    if let Some(exact) = sessions.iter().find(|s| s.session_id == selector) {
        return Ok(exact);
    }

    let matches: Vec<&SessionReport> = sessions
        .iter()
        .filter(|s| s.session_id.starts_with(selector))
        .collect();
    match matches.len() {
        0 => Err(format!("no session matching `{selector}`")),
        1 => Ok(matches[0]),
        count => Err(format!(
            "ambiguous session prefix `{selector}` matched {count} sessions"
        )),
    }
}

/// Runs read-only discovery and diagnostic inspection for `kakeibo doctor` (`PLAN.md` §6.4).
pub fn run_doctor(catalog: &PathCatalog) -> io::Result<DoctorReport> {
    let mut sources = Vec::with_capacity(2);

    // 1. Claude Code
    let claude_root_exists = catalog.claude_root().is_some_and(|p| p.is_dir());
    let claude_top = catalog.discover_claude_top_level()?;
    let claude_subs = catalog.discover_claude_subagents()?;
    let mut claude_sessions = BTreeSet::new();
    let mut claude_turns = 0_usize;
    let mut claude_versions = BTreeSet::new();
    let mut claude_ignored = 0_u64;
    let mut claude_unknown: BTreeMap<String, u64> = BTreeMap::new();
    let mut claude_issues = 0_usize;

    for log in &claude_top {
        let parsed = parse_claude_file(&log.path)?;
        claude_turns += parsed.records.len();
        for tr in &parsed.records {
            claude_sessions.insert(tr.session_id.clone());
        }
        claude_versions.extend(parsed.diagnostics.cli_versions);
        claude_ignored = claude_ignored.saturating_add(parsed.diagnostics.ignored_records);
        claude_issues += parsed.diagnostics.issues.len();
        for (k, v) in parsed.diagnostics.unknown_record_types {
            *claude_unknown.entry(k).or_insert(0) += v;
        }
    }
    for sub in &claude_subs {
        let opts = ClaudeParseOptions {
            parent_session_id: Some(sub.parent_session_id.clone()),
            max_line_bytes: None,
        };
        let parsed = parse_claude_file_with_options(&sub.path, &opts)?;
        claude_turns += parsed.records.len();
        claude_versions.extend(parsed.diagnostics.cli_versions);
        claude_ignored = claude_ignored.saturating_add(parsed.diagnostics.ignored_records);
        claude_issues += parsed.diagnostics.issues.len();
        for (k, v) in parsed.diagnostics.unknown_record_types {
            *claude_unknown.entry(k).or_insert(0) += v;
        }
    }

    sources.push(DoctorSourceSummary {
        agent: AgentId::ClaudeCode,
        root_exists: claude_root_exists,
        log_files: claude_top.len(),
        subagent_files: claude_subs.len(),
        sessions: claude_sessions.len(),
        turns: claude_turns,
        cli_versions: claude_versions.into_iter().collect(),
        ignored_records: claude_ignored,
        unknown_record_types: claude_unknown,
        parse_issues: claude_issues,
    });

    // 2. Codex CLI
    let codex_root_exists = catalog.codex_root().is_some_and(|p| p.is_dir());
    let codex_logs = catalog.discover_codex_rollouts()?;
    let mut codex_sessions = BTreeSet::new();
    let mut codex_turns = 0_usize;
    let mut codex_versions = BTreeSet::new();
    let mut codex_ignored = 0_u64;
    let mut codex_unknown: BTreeMap<String, u64> = BTreeMap::new();
    let mut codex_issues = 0_usize;

    for log in &codex_logs {
        let parsed = parse_codex_file(&log.path)?;
        codex_turns += parsed.records.len();
        for tr in &parsed.records {
            codex_sessions.insert(tr.session_id.clone());
        }
        codex_versions.extend(parsed.diagnostics.cli_versions);
        codex_ignored = codex_ignored.saturating_add(parsed.diagnostics.ignored_records);
        codex_issues += parsed.diagnostics.issues.len();
        for (k, v) in parsed.diagnostics.unknown_record_types {
            *codex_unknown.entry(k).or_insert(0) += v;
        }
    }

    sources.push(DoctorSourceSummary {
        agent: AgentId::Codex,
        root_exists: codex_root_exists,
        log_files: codex_logs.len(),
        subagent_files: 0,
        sessions: codex_sessions.len(),
        turns: codex_turns,
        cli_versions: codex_versions.into_iter().collect(),
        ignored_records: codex_ignored,
        unknown_record_types: codex_unknown,
        parse_issues: codex_issues,
    });

    Ok(DoctorReport { sources })
}
