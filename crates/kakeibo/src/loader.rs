//! Read-only discovery, incremental SQLite index synchronisation, `rayon` parallel
//! decoding/attribution, subagent linking, flag evaluation, and filtering pipeline
//! (`PLAN.md` §3.1–§3.3, §6.2, §7, §10 T12).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kakeibo_core::attrib::attribute_session_with_subagents;
use kakeibo_core::flags::{evaluate_flags_v1, FlagsConfig, ModelCatalog};
use kakeibo_core::pricing::PricingCatalog;
use kakeibo_core::{AgentId, Confidence, Timestamp, TurnRecord};
use kakeibo_index::{
    hash_source_id, AppPaths, CachedDiagnostics, CachedSessionTurns, IndexSnapshot, IndexStore,
    IndexedSourceRecord, PruneScope, SourceFingerprint, SourceLookupOutcome,
};
use kakeibo_parse::hash::hash_cwd;
use kakeibo_parse::{
    extract_aider_session_cwds, extract_claude_session_cwds, extract_codex_session_cwds,
    extract_cursor_session_cwds, extract_gemini_session_cwds, extract_opencode_session_cwds,
    parse_aider_analytics_file_incremental, parse_aider_history_file_incremental,
    parse_claude_file_incremental, parse_codex_file_incremental, parse_cursor_vscdb,
    parse_gemini_file_incremental, parse_opencode_legacy_storage, parse_opencode_sqlite,
    ClaudeParseOptions, GeminiParseOptions, ParseDiagnostics, PathCatalog, ReadCounter,
};
use kakeibo_report::{
    sort_session_reports, DoctorReport, DoctorSourceSummary, ObservedModelPriceStatus,
    SessionReport,
};
use rayon::prelude::*;

type SessionKey = (AgentId, String);
type SubagentMap = BTreeMap<SessionKey, BTreeMap<String, Vec<TurnRecord>>>;

/// Controls whether discovery uses `<cache_dir>/index.sqlite` or decodes raw logs directly (`--no-cache`).
#[derive(Debug, Clone, Copy)]
pub enum IndexMode<'a> {
    /// Bypasses `kakeibo-index` completely (`--no-cache`): neither reads nor writes `<cache_dir>/index.sqlite`.
    Uncached,
    /// Reads and updates `<cache_dir>/index.sqlite` via `kakeibo-index`.
    Cached(&'a AppPaths),
}

struct DiscoveredCorpus {
    primary_turns: BTreeMap<SessionKey, Vec<TurnRecord>>,
    subagents_by_parent: SubagentMap,
    raw_cwds: BTreeMap<SessionKey, String>,
}

#[derive(Debug, Clone)]
enum SourceTaskKind {
    ClaudeTopLevel {
        path: PathBuf,
    },
    ClaudeSubagent {
        path: PathBuf,
        parent_session_id: String,
    },
    CodexRollout {
        path: PathBuf,
    },
    GeminiTopLevel {
        path: PathBuf,
        projects_json: Option<PathBuf>,
        slug_registry: Arc<BTreeMap<String, String>>,
    },
    GeminiSubagent {
        path: PathBuf,
        parent_session_id: String,
        projects_json: Option<PathBuf>,
        slug_registry: Arc<BTreeMap<String, String>>,
    },
    OpencodeSqlite {
        db_path: PathBuf,
    },
    OpencodeLegacy {
        storage_dir: PathBuf,
    },
    AiderHistory {
        path: PathBuf,
    },
    AiderAnalytics {
        path: PathBuf,
    },
    CursorVscdb {
        db_path: PathBuf,
    },
}

impl SourceTaskKind {
    fn agent(&self) -> AgentId {
        match self {
            Self::ClaudeTopLevel { .. } | Self::ClaudeSubagent { .. } => AgentId::ClaudeCode,
            Self::CodexRollout { .. } => AgentId::Codex,
            Self::GeminiTopLevel { .. } | Self::GeminiSubagent { .. } => AgentId::GeminiCli,
            Self::OpencodeSqlite { .. } | Self::OpencodeLegacy { .. } => AgentId::Opencode,
            Self::AiderHistory { .. } | Self::AiderAnalytics { .. } => AgentId::Aider,
            Self::CursorVscdb { .. } => AgentId::Cursor,
        }
    }

    fn source_kind_slug(&self) -> &'static str {
        match self {
            Self::ClaudeTopLevel { .. }
            | Self::CodexRollout { .. }
            | Self::GeminiTopLevel { .. } => "primary_file",
            Self::ClaudeSubagent { .. } | Self::GeminiSubagent { .. } => "subagent_file",
            Self::OpencodeSqlite { .. } => "opencode_db",
            Self::OpencodeLegacy { .. } => "opencode_legacy",
            Self::AiderHistory { .. } => "aider_history",
            Self::AiderAnalytics { .. } => "aider_analytics",
            Self::CursorVscdb { .. } => "cursor_vscdb",
        }
    }

    fn parent_session_id(&self) -> Option<&str> {
        match self {
            Self::ClaudeSubagent {
                parent_session_id, ..
            }
            | Self::GeminiSubagent {
                parent_session_id, ..
            } => Some(parent_session_id.as_str()),
            _ => None,
        }
    }

    fn primary_path(&self) -> &Path {
        match self {
            Self::ClaudeTopLevel { path }
            | Self::ClaudeSubagent { path, .. }
            | Self::CodexRollout { path }
            | Self::GeminiTopLevel { path, .. }
            | Self::GeminiSubagent { path, .. }
            | Self::AiderHistory { path }
            | Self::AiderAnalytics { path } => path.as_path(),
            Self::OpencodeSqlite { db_path } | Self::CursorVscdb { db_path } => db_path.as_path(),
            Self::OpencodeLegacy { storage_dir } => storage_dir.as_path(),
        }
    }

    fn is_appendable(&self) -> bool {
        match self {
            Self::ClaudeTopLevel { path }
            | Self::ClaudeSubagent { path, .. }
            | Self::CodexRollout { path } => {
                path.extension().and_then(|e| e.to_str()) != Some("zst")
            }
            Self::GeminiTopLevel { path, .. } | Self::GeminiSubagent { path, .. } => {
                path.extension().and_then(|e| e.to_str()) != Some("json")
            }
            Self::AiderHistory { .. } | Self::AiderAnalytics { .. } => true,
            Self::OpencodeSqlite { .. }
            | Self::OpencodeLegacy { .. }
            | Self::CursorVscdb { .. } => false,
        }
    }

    fn fingerprint(&self) -> io::Result<SourceFingerprint> {
        match self {
            Self::ClaudeTopLevel { path }
            | Self::ClaudeSubagent { path, .. }
            | Self::CodexRollout { path }
            | Self::AiderHistory { path }
            | Self::AiderAnalytics { path } => SourceFingerprint::from_file_with_aux(path, None),
            Self::GeminiTopLevel {
                path,
                projects_json,
                ..
            }
            | Self::GeminiSubagent {
                path,
                projects_json,
                ..
            } => SourceFingerprint::from_file_with_aux(path, projects_json.as_deref()),
            Self::OpencodeSqlite { db_path } | Self::CursorVscdb { db_path } => {
                SourceFingerprint::from_sqlite_db(db_path)
            }
            Self::OpencodeLegacy { storage_dir } => SourceFingerprint::from_json_tree(storage_dir),
        }
    }
}

struct ResolvedSource {
    task: SourceTaskKind,
    sessions: Vec<CachedSessionTurns>,
    diagnostics: CachedDiagnostics,
    dirty_upsert: Option<IndexedSourceRecord>,
}

fn group_flat_records(records: Vec<TurnRecord>) -> Vec<CachedSessionTurns> {
    let mut order: Vec<String> = Vec::new();
    let mut grouped: BTreeMap<String, (Option<String>, Vec<TurnRecord>)> = BTreeMap::new();
    for tr in records {
        let entry = grouped.entry(tr.session_id.clone()).or_insert_with(|| {
            order.push(tr.session_id.clone());
            (tr.parent_session_id.clone(), Vec::new())
        });
        if entry.0.is_none() && tr.parent_session_id.is_some() {
            entry.0.clone_from(&tr.parent_session_id);
        }
        entry.1.push(tr);
    }

    order
        .into_iter()
        .filter_map(|sid| {
            grouped
                .remove(&sid)
                .map(|(parent_session_id, records)| CachedSessionTurns {
                    session_id: sid,
                    parent_session_id,
                    records,
                })
        })
        .collect()
}

fn to_cached_diagnostics(diag: ParseDiagnostics) -> CachedDiagnostics {
    CachedDiagnostics {
        cli_versions: diag.cli_versions.into_iter().collect(),
        ignored_records: diag.ignored_records,
        unknown_record_types: diag.unknown_record_types,
        parse_issues: diag.issues.len(),
    }
}

fn merge_cached_diagnostics(
    cli_versions: &mut BTreeSet<String>,
    ignored_records: &mut u64,
    unknown_record_types: &mut BTreeMap<String, u64>,
    parse_issues: &mut usize,
    diag: &CachedDiagnostics,
) {
    cli_versions.extend(diag.cli_versions.iter().cloned());
    *ignored_records = ignored_records.saturating_add(diag.ignored_records);
    *parse_issues += diag.parse_issues;
    for (k, v) in &diag.unknown_record_types {
        *unknown_record_types.entry(k.clone()).or_insert(0) += *v;
    }
}

fn decode_task_from_offset(
    task: &SourceTaskKind,
    start_offset: u64,
    checkpoint_json: Option<&str>,
    fp_size: u64,
    counter: Option<&ReadCounter>,
) -> io::Result<(
    Vec<CachedSessionTurns>,
    CachedDiagnostics,
    u64,
    Option<String>,
)> {
    match task {
        SourceTaskKind::ClaudeTopLevel { path } => {
            let opts = ClaudeParseOptions::default();
            let inc =
                parse_claude_file_incremental(path, &opts, start_offset, checkpoint_json, counter)?;
            Ok((
                group_flat_records(inc.output.records),
                to_cached_diagnostics(inc.output.diagnostics),
                inc.committed_offset,
                inc.checkpoint_json,
            ))
        }
        SourceTaskKind::ClaudeSubagent {
            path,
            parent_session_id,
        } => {
            let opts = ClaudeParseOptions {
                parent_session_id: Some(parent_session_id.clone()),
                max_line_bytes: None,
            };
            let inc =
                parse_claude_file_incremental(path, &opts, start_offset, checkpoint_json, counter)?;
            Ok((
                group_flat_records(inc.output.records),
                to_cached_diagnostics(inc.output.diagnostics),
                inc.committed_offset,
                inc.checkpoint_json,
            ))
        }
        SourceTaskKind::CodexRollout { path } => {
            let inc = parse_codex_file_incremental(path, start_offset, checkpoint_json, counter)?;
            Ok((
                group_flat_records(inc.output.records),
                to_cached_diagnostics(inc.output.diagnostics),
                inc.committed_offset,
                inc.checkpoint_json,
            ))
        }
        SourceTaskKind::GeminiTopLevel {
            path,
            slug_registry,
            ..
        } => {
            let opts = GeminiParseOptions {
                parent_session_id: None,
                slug_registry: Some((**slug_registry).clone()),
                max_line_bytes: None,
            };
            let inc =
                parse_gemini_file_incremental(path, &opts, start_offset, checkpoint_json, counter)?;
            Ok((
                group_flat_records(inc.output.records),
                to_cached_diagnostics(inc.output.diagnostics),
                inc.committed_offset,
                inc.checkpoint_json,
            ))
        }
        SourceTaskKind::GeminiSubagent {
            path,
            parent_session_id,
            slug_registry,
            ..
        } => {
            let opts = GeminiParseOptions {
                parent_session_id: Some(parent_session_id.clone()),
                slug_registry: Some((**slug_registry).clone()),
                max_line_bytes: None,
            };
            let inc =
                parse_gemini_file_incremental(path, &opts, start_offset, checkpoint_json, counter)?;
            Ok((
                group_flat_records(inc.output.records),
                to_cached_diagnostics(inc.output.diagnostics),
                inc.committed_offset,
                inc.checkpoint_json,
            ))
        }
        SourceTaskKind::OpencodeSqlite { db_path } => {
            if let Some(c) = counter {
                c.record_file_open();
                c.record_bytes_read(fp_size);
            }
            let parsed = parse_opencode_sqlite(db_path)?;
            let sessions = parsed
                .sessions
                .into_iter()
                .map(|b| CachedSessionTurns {
                    session_id: b.session_id,
                    parent_session_id: b.parent_session_id,
                    records: b.records,
                })
                .collect();
            Ok((sessions, to_cached_diagnostics(parsed.diagnostics), 0, None))
        }
        SourceTaskKind::OpencodeLegacy { storage_dir } => {
            if let Some(c) = counter {
                c.record_file_open();
                c.record_bytes_read(fp_size);
            }
            let parsed = parse_opencode_legacy_storage(storage_dir)?;
            let sessions = parsed
                .sessions
                .into_iter()
                .map(|b| CachedSessionTurns {
                    session_id: b.session_id,
                    parent_session_id: b.parent_session_id,
                    records: b.records,
                })
                .collect();
            Ok((sessions, to_cached_diagnostics(parsed.diagnostics), 0, None))
        }
        SourceTaskKind::AiderHistory { path } => {
            let inc =
                parse_aider_history_file_incremental(path, start_offset, checkpoint_json, counter)?;
            let sessions = inc
                .output
                .sessions
                .into_iter()
                .map(|b| CachedSessionTurns {
                    session_id: b.session_id,
                    parent_session_id: None,
                    records: b.records,
                })
                .collect();
            Ok((
                sessions,
                to_cached_diagnostics(inc.output.diagnostics),
                inc.committed_offset,
                inc.checkpoint_json,
            ))
        }
        SourceTaskKind::AiderAnalytics { path } => {
            let inc = parse_aider_analytics_file_incremental(
                path,
                start_offset,
                checkpoint_json,
                counter,
            )?;
            let sessions = inc
                .output
                .sessions
                .into_iter()
                .map(|b| CachedSessionTurns {
                    session_id: b.session_id,
                    parent_session_id: None,
                    records: b.records,
                })
                .collect();
            Ok((
                sessions,
                to_cached_diagnostics(inc.output.diagnostics),
                inc.committed_offset,
                inc.checkpoint_json,
            ))
        }
        SourceTaskKind::CursorVscdb { db_path } => {
            if let Some(c) = counter {
                c.record_file_open();
                c.record_bytes_read(fp_size);
            }
            let parsed = parse_cursor_vscdb(db_path)?;
            let sessions = parsed
                .sessions
                .into_iter()
                .map(|b| CachedSessionTurns {
                    session_id: b.session_id,
                    parent_session_id: None,
                    records: b.records,
                })
                .collect();
            Ok((sessions, to_cached_diagnostics(parsed.diagnostics), 0, None))
        }
    }
}

fn resolve_one_source_task(
    task: SourceTaskKind,
    source_id: String,
    snapshot: Option<&IndexSnapshot>,
    counter: Option<&ReadCounter>,
) -> io::Result<ResolvedSource> {
    let Some(snap) = snapshot else {
        // `--no-cache`: decode directly from offset 0 without touching IndexStore.
        let fp_size = fs::metadata(task.primary_path()).map_or(0, |m| m.len());
        let (sessions, diagnostics, _, _) =
            decode_task_from_offset(&task, 0, None, fp_size, counter)?;
        return Ok(ResolvedSource {
            task,
            sessions,
            diagnostics,
            dirty_upsert: None,
        });
    };

    let fp = task.fingerprint()?;
    match snap.classify(&source_id, &fp, task.is_appendable()) {
        SourceLookupOutcome::Unchanged(raw_row) => {
            if let Some((sessions, diagnostics)) = raw_row.decode_payloads() {
                return Ok(ResolvedSource {
                    task,
                    sessions,
                    diagnostics,
                    dirty_upsert: None,
                });
            }
            // Corrupt JSON payload inside an individual row -> self-heal from offset 0.
            let (sessions, diagnostics, committed_offset, checkpoint_json) =
                decode_task_from_offset(&task, 0, None, fp.size, counter)?;
            let rec = IndexedSourceRecord {
                source_id,
                agent: task.agent(),
                source_kind: task.source_kind_slug().to_string(),
                fingerprint: fp,
                committed_offset,
                checkpoint_json,
                sessions: sessions.clone(),
                diagnostics: diagnostics.clone(),
            };
            Ok(ResolvedSource {
                task,
                sessions,
                diagnostics,
                dirty_upsert: Some(rec),
            })
        }
        SourceLookupOutcome::ResumeFromOffset {
            committed_offset: resume_offset,
            checkpoint_json: resume_cp,
        } => {
            let (sessions, diagnostics, committed_offset, checkpoint_json) =
                decode_task_from_offset(&task, resume_offset, Some(resume_cp), fp.size, counter)?;
            let rec = IndexedSourceRecord {
                source_id,
                agent: task.agent(),
                source_kind: task.source_kind_slug().to_string(),
                fingerprint: fp,
                committed_offset,
                checkpoint_json,
                sessions: sessions.clone(),
                diagnostics: diagnostics.clone(),
            };
            Ok(ResolvedSource {
                task,
                sessions,
                diagnostics,
                dirty_upsert: Some(rec),
            })
        }
        SourceLookupOutcome::FullReparse => {
            let (sessions, diagnostics, committed_offset, checkpoint_json) =
                decode_task_from_offset(&task, 0, None, fp.size, counter)?;
            let rec = IndexedSourceRecord {
                source_id,
                agent: task.agent(),
                source_kind: task.source_kind_slug().to_string(),
                fingerprint: fp,
                committed_offset,
                checkpoint_json,
                sessions: sessions.clone(),
                diagnostics: diagnostics.clone(),
            };
            Ok(ResolvedSource {
                task,
                sessions,
                diagnostics,
                dirty_upsert: Some(rec),
            })
        }
    }
}

struct EnumeratedTasks {
    tasks: Vec<SourceTaskKind>,
    gemini_slug_registry: Arc<BTreeMap<String, String>>,
}

fn enumerate_source_tasks(
    catalog: &PathCatalog,
    agent_filter: Option<AgentId>,
) -> io::Result<EnumeratedTasks> {
    let mut tasks = Vec::new();
    let mut gemini_slug_registry = Arc::new(BTreeMap::new());

    if agent_filter.is_none_or(|a| a == AgentId::ClaudeCode) {
        for log in catalog.discover_claude_top_level()? {
            tasks.push(SourceTaskKind::ClaudeTopLevel { path: log.path });
        }
        for sub in catalog.discover_claude_subagents()? {
            tasks.push(SourceTaskKind::ClaudeSubagent {
                path: sub.path,
                parent_session_id: sub.parent_session_id,
            });
        }
    }

    if agent_filter.is_none_or(|a| a == AgentId::Codex) {
        for log in catalog.discover_codex_rollouts()? {
            tasks.push(SourceTaskKind::CodexRollout { path: log.path });
        }
    }

    if agent_filter.is_none_or(|a| a == AgentId::GeminiCli) {
        let registry = Arc::new(catalog.load_gemini_slug_registry()?);
        let projects_json = catalog
            .gemini_root()
            .map(|r| r.join("projects.json"))
            .filter(|p| p.is_file());
        for log in catalog.discover_gemini_top_level()? {
            tasks.push(SourceTaskKind::GeminiTopLevel {
                path: log.path,
                projects_json: projects_json.clone(),
                slug_registry: Arc::clone(&registry),
            });
        }
        for sub in catalog.discover_gemini_subagents()? {
            tasks.push(SourceTaskKind::GeminiSubagent {
                path: sub.path,
                parent_session_id: sub.parent_session_id,
                projects_json: projects_json.clone(),
                slug_registry: Arc::clone(&registry),
            });
        }
        gemini_slug_registry = registry;
    }

    if agent_filter.is_none_or(|a| a == AgentId::Opencode) {
        if let Some(root) = catalog.opencode_root().filter(|p| p.is_dir()) {
            let db_path = root.join("opencode.db");
            if db_path.is_file() {
                tasks.push(SourceTaskKind::OpencodeSqlite { db_path });
            }
            let storage_dir = root.join("storage");
            if storage_dir.is_dir() {
                tasks.push(SourceTaskKind::OpencodeLegacy { storage_dir });
            }
        }
    }

    if agent_filter.is_none_or(|a| a == AgentId::Aider) {
        for log in catalog.discover_aider_histories()? {
            tasks.push(SourceTaskKind::AiderHistory { path: log.path });
        }
        for log in catalog.discover_aider_analytics_logs()? {
            tasks.push(SourceTaskKind::AiderAnalytics { path: log.path });
        }
    }

    if agent_filter.is_none_or(|a| a == AgentId::Cursor) {
        for db in catalog.discover_cursor_state_dbs()? {
            tasks.push(SourceTaskKind::CursorVscdb { db_path: db.path });
        }
    }

    Ok(EnumeratedTasks {
        tasks,
        gemini_slug_registry,
    })
}

struct SyncedSources {
    resolved: Vec<ResolvedSource>,
    gemini_slug_registry: Arc<BTreeMap<String, String>>,
}

fn sync_agent_sources(
    catalog: &PathCatalog,
    agent_filter: Option<AgentId>,
    mode: IndexMode<'_>,
    counter: Option<&ReadCounter>,
) -> io::Result<SyncedSources> {
    let enumerated = enumerate_source_tasks(catalog, agent_filter)?;
    let scope = match agent_filter {
        Some(agent) => PruneScope::SingleAgent(agent),
        None => PruneScope::AllAgents,
    };

    let mut store_opt: Option<IndexStore> = match mode {
        IndexMode::Uncached => None,
        IndexMode::Cached(paths) => IndexStore::open_or_rebuild(paths).ok(),
    };
    let snapshot_opt: Option<IndexSnapshot> = match store_opt.as_ref() {
        Some(store) => store.load_snapshot(scope).ok(),
        None => None,
    };

    let tasks_with_ids: Vec<(SourceTaskKind, String)> = enumerated
        .tasks
        .into_iter()
        .map(|t| {
            let sid = hash_source_id(
                t.agent(),
                t.source_kind_slug(),
                t.parent_session_id(),
                t.primary_path(),
            );
            (t, sid)
        })
        .collect();

    let active_ids: BTreeSet<String> = tasks_with_ids.iter().map(|(_, id)| id.clone()).collect();

    let resolved: Vec<ResolvedSource> = tasks_with_ids
        .into_par_iter()
        .map(|(task, sid)| resolve_one_source_task(task, sid, snapshot_opt.as_ref(), counter))
        .collect::<io::Result<Vec<ResolvedSource>>>()?;

    if let (Some(ref mut store), Some(ref snap)) = (&mut store_opt, &snapshot_opt) {
        let upserts: Vec<IndexedSourceRecord> = resolved
            .iter()
            .filter_map(|r| r.dirty_upsert.clone())
            .collect();
        let stale = snap.stale_source_ids(&active_ids);
        // Best-effort cache write: if `<cache_dir>` becomes unwritable mid-run,
        // user commands still succeed with the freshly decoded records.
        let _ = store.commit_updates(&upserts, &stale);
    }

    Ok(SyncedSources {
        resolved,
        gemini_slug_registry: enumerated.gemini_slug_registry,
    })
}

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
                .find(|tr| !tr.hints.sidechain && tr.usage.confidence != Confidence::NotRecorded)
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

fn combine_opencode_batches(
    db_batches: Vec<CachedSessionTurns>,
    legacy_batches: Vec<CachedSessionTurns>,
) -> Vec<CachedSessionTurns> {
    let seen_ids: BTreeSet<String> = db_batches.iter().map(|b| b.session_id.clone()).collect();
    let mut combined = db_batches;
    for batch in legacy_batches {
        if !seen_ids.contains(&batch.session_id) {
            combined.push(batch);
        }
    }
    combined
}

fn discover_raw_sessions(
    catalog: &PathCatalog,
    agent_filter: Option<AgentId>,
    capture_raw_paths: bool,
    mode: IndexMode<'_>,
    counter: Option<&ReadCounter>,
) -> io::Result<DiscoveredCorpus> {
    let SyncedSources {
        resolved,
        gemini_slug_registry,
    } = sync_agent_sources(catalog, agent_filter, mode, counter)?;

    let mut primary_turns: BTreeMap<SessionKey, Vec<TurnRecord>> = BTreeMap::new();
    let mut subagents_by_parent: SubagentMap = BTreeMap::new();
    let mut raw_cwds: BTreeMap<SessionKey, String> = BTreeMap::new();

    let mut codex_candidates: BTreeMap<String, Vec<TurnRecord>> = BTreeMap::new();
    let mut opencode_db_batches: Vec<CachedSessionTurns> = Vec::new();
    let mut opencode_legacy_batches: Vec<CachedSessionTurns> = Vec::new();

    for src in resolved {
        match src.task {
            SourceTaskKind::ClaudeTopLevel { path } => {
                for batch in src.sessions {
                    primary_turns
                        .entry((AgentId::ClaudeCode, batch.session_id))
                        .or_default()
                        .extend(batch.records);
                }
                if capture_raw_paths {
                    for (sid, cwd) in extract_claude_session_cwds(&path)? {
                        raw_cwds.insert((AgentId::ClaudeCode, sid), cwd);
                    }
                }
            }
            SourceTaskKind::ClaudeSubagent {
                parent_session_id, ..
            } => {
                for batch in src.sessions {
                    subagents_by_parent
                        .entry((AgentId::ClaudeCode, parent_session_id.clone()))
                        .or_default()
                        .entry(batch.session_id)
                        .or_default()
                        .extend(batch.records);
                }
            }
            SourceTaskKind::CodexRollout { path } => {
                for batch in src.sessions {
                    codex_candidates
                        .entry(batch.session_id)
                        .or_default()
                        .extend(batch.records);
                }
                if capture_raw_paths {
                    for (sid, cwd) in extract_codex_session_cwds(&path)? {
                        raw_cwds.insert((AgentId::Codex, sid), cwd);
                    }
                }
            }
            SourceTaskKind::GeminiTopLevel { path, .. } => {
                for batch in src.sessions {
                    primary_turns
                        .entry((AgentId::GeminiCli, batch.session_id))
                        .or_default()
                        .extend(batch.records);
                }
                if capture_raw_paths {
                    for (sid, cwd) in extract_gemini_session_cwds(&path, &gemini_slug_registry)? {
                        raw_cwds.insert((AgentId::GeminiCli, sid), cwd);
                    }
                }
            }
            SourceTaskKind::GeminiSubagent {
                parent_session_id, ..
            } => {
                for batch in src.sessions {
                    subagents_by_parent
                        .entry((AgentId::GeminiCli, parent_session_id.clone()))
                        .or_default()
                        .entry(batch.session_id)
                        .or_default()
                        .extend(batch.records);
                }
            }
            SourceTaskKind::OpencodeSqlite { .. } => {
                opencode_db_batches.extend(src.sessions);
            }
            SourceTaskKind::OpencodeLegacy { .. } => {
                opencode_legacy_batches.extend(src.sessions);
            }
            SourceTaskKind::AiderHistory { path } | SourceTaskKind::AiderAnalytics { path } => {
                for batch in src.sessions {
                    if batch.records.is_empty() {
                        continue;
                    }
                    primary_turns
                        .entry((AgentId::Aider, batch.session_id))
                        .or_default()
                        .extend(batch.records);
                }
                if capture_raw_paths {
                    for (sid, cwd) in extract_aider_session_cwds(&path)? {
                        raw_cwds.insert((AgentId::Aider, sid), cwd);
                    }
                }
            }
            SourceTaskKind::CursorVscdb { db_path } => {
                for batch in src.sessions {
                    if batch.records.is_empty() {
                        continue;
                    }
                    primary_turns
                        .entry((AgentId::Cursor, batch.session_id))
                        .or_default()
                        .extend(batch.records);
                }
                if capture_raw_paths {
                    for (sid, cwd) in extract_cursor_session_cwds(&db_path)? {
                        raw_cwds.insert((AgentId::Cursor, sid), cwd);
                    }
                }
            }
        }
    }

    if !codex_candidates.is_empty() {
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

    if !opencode_db_batches.is_empty() || !opencode_legacy_batches.is_empty() {
        let combined = combine_opencode_batches(opencode_db_batches, opencode_legacy_batches);
        let parent_map: BTreeMap<String, Option<String>> = combined
            .iter()
            .filter(|b| !b.records.is_empty())
            .map(|b| (b.session_id.clone(), b.parent_session_id.clone()))
            .collect();

        for batch in combined {
            if batch.records.is_empty() {
                continue;
            }
            if let Some(root_parent) = resolve_opencode_root_parent(&batch.session_id, &parent_map)
            {
                subagents_by_parent
                    .entry((AgentId::Opencode, root_parent))
                    .or_default()
                    .entry(batch.session_id)
                    .or_default()
                    .extend(batch.records);
            } else {
                primary_turns
                    .entry((AgentId::Opencode, batch.session_id))
                    .or_default()
                    .extend(batch.records);
            }
        }

        if capture_raw_paths {
            if let Some(root) = catalog.opencode_root().filter(|p| p.is_dir()) {
                for (sid, cwd) in extract_opencode_session_cwds(&root)? {
                    raw_cwds.insert((AgentId::Opencode, sid), cwd);
                }
            }
        }
    }

    for turns in primary_turns.values_mut() {
        let taken = std::mem::take(turns);
        *turns = dedupe_and_renumber_turns(taken);
    }

    Ok(DiscoveredCorpus {
        primary_turns,
        subagents_by_parent,
        raw_cwds,
    })
}

/// Discovers, decodes (uncached), links subagents, attributes, evaluates v1 flags, and prices
/// sessions across local read-only agent logs (`PLAN.md` §3.2, §6.2).
pub fn load_session_reports(
    catalog: &PathCatalog,
    pricing: &PricingCatalog,
    agent_filter: Option<AgentId>,
    capture_raw_paths: bool,
) -> io::Result<Vec<SessionReport>> {
    load_session_reports_with_options(
        catalog,
        pricing,
        agent_filter,
        capture_raw_paths,
        IndexMode::Uncached,
        None,
    )
}

/// Discovers, decodes (via [`IndexMode`]), links subagents, attributes, evaluates v1 flags,
/// and prices sessions in parallel via `rayon` (`PLAN.md` §3.1–§3.3, §7, §10 T12).
pub fn load_session_reports_with_options(
    catalog: &PathCatalog,
    pricing: &PricingCatalog,
    agent_filter: Option<AgentId>,
    capture_raw_paths: bool,
    mode: IndexMode<'_>,
    counter: Option<&ReadCounter>,
) -> io::Result<Vec<SessionReport>> {
    let mut corpus =
        discover_raw_sessions(catalog, agent_filter, capture_raw_paths, mode, counter)?;

    let user_median_p1 = compute_corpus_median_p1(&corpus.primary_turns);
    let flags_cfg = FlagsConfig::bundled();
    let models = ModelCatalog::bundled();

    struct SessionBuildInput {
        turns: Vec<TurnRecord>,
        child_vecs: Vec<Vec<TurnRecord>>,
        project_path: Option<String>,
    }

    let session_inputs: Vec<SessionBuildInput> = corpus
        .primary_turns
        .into_iter()
        .map(|((agent, sid), turns)| {
            let child_vecs: Vec<Vec<TurnRecord>> = corpus
                .subagents_by_parent
                .remove(&(agent, sid.clone()))
                .map(|m| m.into_values().map(dedupe_and_renumber_turns).collect())
                .unwrap_or_default();
            let project_path = if capture_raw_paths {
                corpus.raw_cwds.remove(&(agent, sid))
            } else {
                None
            };
            SessionBuildInput {
                turns,
                child_vecs,
                project_path,
            }
        })
        .collect();

    let mut reports: Vec<SessionReport> = session_inputs
        .into_par_iter()
        .filter_map(|input| {
            let child_slices: Vec<&[TurnRecord]> =
                input.child_vecs.iter().map(Vec::as_slice).collect();
            let attribution = attribute_session_with_subagents(&input.turns, &child_slices);
            let flags = evaluate_flags_v1(
                &input.turns,
                &attribution,
                &flags_cfg,
                &models,
                user_median_p1,
            );
            SessionReport::build_with_pricing(
                input.turns,
                &child_slices,
                attribution,
                flags,
                input.project_path,
                pricing,
            )
        })
        .collect();

    sort_session_reports(&mut reports);
    Ok(reports)
}

#[derive(Default)]
struct ObservedModelAcc {
    sessions: BTreeSet<SessionKey>,
    turns: u32,
    prompt_tokens: u64,
    output_tokens: u64,
}

/// Scans discovered read-only session logs (uncached) and summarizes pricing coverage
/// for every observed model (`pricing check`, `PLAN.md` §6.2, §6.4).
pub fn collect_observed_models(
    catalog: &PathCatalog,
    pricing: &PricingCatalog,
    agent_filter: Option<AgentId>,
    cutoff: Option<Timestamp>,
) -> io::Result<Vec<ObservedModelPriceStatus>> {
    collect_observed_models_with_options(
        catalog,
        pricing,
        agent_filter,
        cutoff,
        IndexMode::Uncached,
        None,
    )
}

/// Scans discovered read-only session logs (via [`IndexMode`]) and summarizes pricing
/// coverage for every observed model (`pricing check`, `PLAN.md` §6.2, §6.4).
pub fn collect_observed_models_with_options(
    catalog: &PathCatalog,
    pricing: &PricingCatalog,
    agent_filter: Option<AgentId>,
    cutoff: Option<Timestamp>,
    mode: IndexMode<'_>,
    counter: Option<&ReadCounter>,
) -> io::Result<Vec<ObservedModelPriceStatus>> {
    let mut corpus = discover_raw_sessions(catalog, agent_filter, false, mode, counter)?;
    let mut acc_by_model: BTreeMap<String, ObservedModelAcc> = BTreeMap::new();

    for ((agent, sid), turns) in corpus.primary_turns {
        let child_vecs: Vec<Vec<TurnRecord>> = corpus
            .subagents_by_parent
            .remove(&(agent, sid.clone()))
            .map(|m| m.into_values().map(dedupe_and_renumber_turns).collect())
            .unwrap_or_default();

        if let Some(cut) = cutoff {
            let max_ts = turns
                .iter()
                .chain(child_vecs.iter().flatten())
                .map(|tr| tr.ts)
                .max();
            if max_ts.is_none_or(|ended| ended < cut) {
                continue;
            }
        }

        for tr in turns.iter().chain(child_vecs.iter().flatten()) {
            if tr.usage.confidence == Confidence::NotRecorded {
                continue;
            }
            let model_name = tr
                .model
                .as_deref()
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .unwrap_or("unknown")
                .to_string();
            let entry = acc_by_model.entry(model_name).or_default();
            entry.sessions.insert((agent, sid.clone()));
            entry.turns = entry.turns.saturating_add(1);
            entry.prompt_tokens = entry.prompt_tokens.saturating_add(tr.usage.prompt_tokens());
            entry.output_tokens = entry.output_tokens.saturating_add(tr.usage.output);
        }
    }

    let mut observed: Vec<ObservedModelPriceStatus> = acc_by_model
        .into_iter()
        .map(|(model, acc)| {
            let resolved = if model == "unknown" {
                None
            } else {
                pricing.resolve_priced(Some(&model))
            };
            ObservedModelPriceStatus {
                model,
                is_priced: resolved.is_some(),
                matched_key: resolved.map(|(k, _)| k.to_string()),
                currency: resolved.map(|(_, e)| e.currency.clone()),
                sessions: acc.sessions.len(),
                turns: acc.turns,
                prompt_tokens: acc.prompt_tokens,
                output_tokens: acc.output_tokens,
            }
        })
        .collect();

    observed.sort_by(|a, b| {
        a.is_priced
            .cmp(&b.is_priced)
            .then_with(|| {
                let b_tot = b.prompt_tokens.saturating_add(b.output_tokens);
                let a_tot = a.prompt_tokens.saturating_add(a.output_tokens);
                b_tot.cmp(&a_tot)
            })
            .then_with(|| a.model.cmp(&b.model))
    });

    Ok(observed)
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

fn resolve_opencode_root_parent(
    session_id: &str,
    parent_map: &BTreeMap<String, Option<String>>,
) -> Option<String> {
    let mut current = parent_map.get(session_id)?.as_deref()?;
    if current == session_id || !parent_map.contains_key(current) {
        return None;
    }
    let mut visited = BTreeSet::new();
    visited.insert(session_id);
    for _ in 0..16 {
        if !visited.insert(current) {
            return None;
        }
        match parent_map.get(current).and_then(Option::as_deref) {
            Some(next) if next != current && parent_map.contains_key(next) => {
                current = next;
            }
            _ => return Some(current.to_string()),
        }
    }
    Some(current.to_string())
}

fn count_json_files_recursive(dir: &Path) -> io::Result<usize> {
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut count = 0_usize;
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            count += count_json_files_recursive(&path)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
            count += 1;
        }
    }
    Ok(count)
}

#[derive(Default)]
struct DoctorAcc {
    log_files: usize,
    subagent_files: usize,
    sessions: BTreeSet<String>,
    turns: usize,
    cli_versions: BTreeSet<String>,
    ignored_records: u64,
    unknown_record_types: BTreeMap<String, u64>,
    parse_issues: usize,
}

/// Runs read-only discovery and diagnostic inspection (uncached) for `kakeibo doctor` (`PLAN.md` §6.4).
pub fn run_doctor(catalog: &PathCatalog) -> io::Result<DoctorReport> {
    run_doctor_with_options(catalog, IndexMode::Uncached, None)
}

/// Runs read-only discovery and diagnostic inspection (via [`IndexMode`]) for `kakeibo doctor` (`PLAN.md` §6.4).
pub fn run_doctor_with_options(
    catalog: &PathCatalog,
    mode: IndexMode<'_>,
    counter: Option<&ReadCounter>,
) -> io::Result<DoctorReport> {
    let SyncedSources { resolved, .. } = sync_agent_sources(catalog, None, mode, counter)?;

    let mut claude = DoctorAcc::default();
    let mut codex = DoctorAcc::default();
    let mut gemini = DoctorAcc::default();
    let mut opencode_db_batches: Vec<CachedSessionTurns> = Vec::new();
    let mut opencode_legacy_batches: Vec<CachedSessionTurns> = Vec::new();
    let mut opencode_diag = DoctorAcc::default();
    let mut aider = DoctorAcc::default();
    let mut cursor = DoctorAcc::default();

    for src in resolved {
        match src.task {
            SourceTaskKind::ClaudeTopLevel { .. } => {
                claude.log_files += 1;
                for batch in &src.sessions {
                    claude.turns += batch.records.len();
                    if !batch.records.is_empty() {
                        claude.sessions.insert(batch.session_id.clone());
                    }
                }
                merge_cached_diagnostics(
                    &mut claude.cli_versions,
                    &mut claude.ignored_records,
                    &mut claude.unknown_record_types,
                    &mut claude.parse_issues,
                    &src.diagnostics,
                );
            }
            SourceTaskKind::ClaudeSubagent { .. } => {
                claude.subagent_files += 1;
                for batch in &src.sessions {
                    claude.turns += batch.records.len();
                }
                merge_cached_diagnostics(
                    &mut claude.cli_versions,
                    &mut claude.ignored_records,
                    &mut claude.unknown_record_types,
                    &mut claude.parse_issues,
                    &src.diagnostics,
                );
            }
            SourceTaskKind::CodexRollout { .. } => {
                codex.log_files += 1;
                for batch in &src.sessions {
                    codex.turns += batch.records.len();
                    if !batch.records.is_empty() {
                        codex.sessions.insert(batch.session_id.clone());
                    }
                }
                merge_cached_diagnostics(
                    &mut codex.cli_versions,
                    &mut codex.ignored_records,
                    &mut codex.unknown_record_types,
                    &mut codex.parse_issues,
                    &src.diagnostics,
                );
            }
            SourceTaskKind::GeminiTopLevel { .. } => {
                gemini.log_files += 1;
                for batch in &src.sessions {
                    gemini.turns += batch.records.len();
                    if !batch.records.is_empty() {
                        gemini.sessions.insert(batch.session_id.clone());
                    }
                }
                merge_cached_diagnostics(
                    &mut gemini.cli_versions,
                    &mut gemini.ignored_records,
                    &mut gemini.unknown_record_types,
                    &mut gemini.parse_issues,
                    &src.diagnostics,
                );
            }
            SourceTaskKind::GeminiSubagent { .. } => {
                gemini.subagent_files += 1;
                for batch in &src.sessions {
                    gemini.turns += batch.records.len();
                }
                merge_cached_diagnostics(
                    &mut gemini.cli_versions,
                    &mut gemini.ignored_records,
                    &mut gemini.unknown_record_types,
                    &mut gemini.parse_issues,
                    &src.diagnostics,
                );
            }
            SourceTaskKind::OpencodeSqlite { .. } => {
                opencode_db_batches.extend(src.sessions);
                merge_cached_diagnostics(
                    &mut opencode_diag.cli_versions,
                    &mut opencode_diag.ignored_records,
                    &mut opencode_diag.unknown_record_types,
                    &mut opencode_diag.parse_issues,
                    &src.diagnostics,
                );
            }
            SourceTaskKind::OpencodeLegacy { .. } => {
                opencode_legacy_batches.extend(src.sessions);
                merge_cached_diagnostics(
                    &mut opencode_diag.cli_versions,
                    &mut opencode_diag.ignored_records,
                    &mut opencode_diag.unknown_record_types,
                    &mut opencode_diag.parse_issues,
                    &src.diagnostics,
                );
            }
            SourceTaskKind::AiderHistory { .. } | SourceTaskKind::AiderAnalytics { .. } => {
                aider.log_files += 1;
                for batch in &src.sessions {
                    aider.turns += batch.records.len();
                    if !batch.records.is_empty() {
                        aider.sessions.insert(batch.session_id.clone());
                    }
                }
                merge_cached_diagnostics(
                    &mut aider.cli_versions,
                    &mut aider.ignored_records,
                    &mut aider.unknown_record_types,
                    &mut aider.parse_issues,
                    &src.diagnostics,
                );
            }
            SourceTaskKind::CursorVscdb { .. } => {
                cursor.log_files += 1;
                for batch in &src.sessions {
                    cursor.turns += batch.records.len();
                    if !batch.records.is_empty() {
                        cursor.sessions.insert(batch.session_id.clone());
                    }
                }
                merge_cached_diagnostics(
                    &mut cursor.cli_versions,
                    &mut cursor.ignored_records,
                    &mut cursor.unknown_record_types,
                    &mut cursor.parse_issues,
                    &src.diagnostics,
                );
            }
        }
    }

    let opencode_root = catalog.opencode_root();
    let opencode_root_exists = opencode_root.as_ref().is_some_and(|p| p.is_dir());
    let mut opencode_log_files = 0_usize;
    let mut opencode_subagent_files = 0_usize;
    let mut opencode_sessions = 0_usize;
    let mut opencode_turns = 0_usize;

    if let Some(root) = opencode_root.filter(|p| p.is_dir()) {
        opencode_log_files = usize::from(root.join("opencode.db").is_file())
            + count_json_files_recursive(&root.join("storage").join("session"))?;
        let combined = combine_opencode_batches(opencode_db_batches, opencode_legacy_batches);
        let parent_map: BTreeMap<String, Option<String>> = combined
            .iter()
            .filter(|b| !b.records.is_empty())
            .map(|b| (b.session_id.clone(), b.parent_session_id.clone()))
            .collect();
        for batch in &combined {
            opencode_turns += batch.records.len();
            if batch.records.is_empty() {
                continue;
            }
            if resolve_opencode_root_parent(&batch.session_id, &parent_map).is_some() {
                opencode_subagent_files += 1;
            } else {
                opencode_sessions += 1;
            }
        }
    }

    let aider_root_exists = catalog.aider_root().is_some_and(|p| p.is_dir()) || aider.log_files > 0;

    let sources = vec![
        DoctorSourceSummary {
            agent: AgentId::ClaudeCode,
            root_exists: catalog.claude_root().is_some_and(|p| p.is_dir()),
            log_files: claude.log_files,
            subagent_files: claude.subagent_files,
            sessions: claude.sessions.len(),
            turns: claude.turns,
            cli_versions: claude.cli_versions.into_iter().collect(),
            ignored_records: claude.ignored_records,
            unknown_record_types: claude.unknown_record_types,
            parse_issues: claude.parse_issues,
        },
        DoctorSourceSummary {
            agent: AgentId::Codex,
            root_exists: catalog.codex_root().is_some_and(|p| p.is_dir()),
            log_files: codex.log_files,
            subagent_files: 0,
            sessions: codex.sessions.len(),
            turns: codex.turns,
            cli_versions: codex.cli_versions.into_iter().collect(),
            ignored_records: codex.ignored_records,
            unknown_record_types: codex.unknown_record_types,
            parse_issues: codex.parse_issues,
        },
        DoctorSourceSummary {
            agent: AgentId::GeminiCli,
            root_exists: catalog.gemini_root().is_some_and(|p| p.is_dir()),
            log_files: gemini.log_files,
            subagent_files: gemini.subagent_files,
            sessions: gemini.sessions.len(),
            turns: gemini.turns,
            cli_versions: gemini.cli_versions.into_iter().collect(),
            ignored_records: gemini.ignored_records,
            unknown_record_types: gemini.unknown_record_types,
            parse_issues: gemini.parse_issues,
        },
        DoctorSourceSummary {
            agent: AgentId::Opencode,
            root_exists: opencode_root_exists,
            log_files: opencode_log_files,
            subagent_files: opencode_subagent_files,
            sessions: opencode_sessions,
            turns: opencode_turns,
            cli_versions: opencode_diag.cli_versions.into_iter().collect(),
            ignored_records: opencode_diag.ignored_records,
            unknown_record_types: opencode_diag.unknown_record_types,
            parse_issues: opencode_diag.parse_issues,
        },
        DoctorSourceSummary {
            agent: AgentId::Aider,
            root_exists: aider_root_exists,
            log_files: aider.log_files,
            subagent_files: 0,
            sessions: aider.sessions.len(),
            turns: aider.turns,
            cli_versions: aider.cli_versions.into_iter().collect(),
            ignored_records: aider.ignored_records,
            unknown_record_types: aider.unknown_record_types,
            parse_issues: aider.parse_issues,
        },
        DoctorSourceSummary {
            agent: AgentId::Cursor,
            root_exists: catalog.cursor_root_exists(),
            log_files: cursor.log_files,
            subagent_files: 0,
            sessions: cursor.sessions.len(),
            turns: cursor.turns,
            cli_versions: cursor.cli_versions.into_iter().collect(),
            ignored_records: cursor.ignored_records,
            unknown_record_types: cursor.unknown_record_types,
            parse_issues: cursor.parse_issues,
        },
    ];

    Ok(DoctorReport { sources })
}
