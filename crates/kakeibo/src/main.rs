//! CLI binary entrypoint (`PLAN.md` §3.1, §3.2, §6.4, §10 T08).

mod cli;
mod loader;
mod since;

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::process::ExitCode;

use clap::Parser;
use kakeibo_core::product;
use kakeibo_parse::PathCatalog;
use kakeibo_report::ndjson::write_ledger;
use kakeibo_report::{
    render_autopsy_report, render_cross_session_flags, render_doctor_report,
    render_markdown_export, render_sessions_table, render_top_table, write_json_export_v1,
    SessionReport,
};

use crate::cli::{Cli, Commands, ExportArgs, ExportFormat};
use crate::loader::{load_session_reports, matches_project_filter, run_doctor, select_session};
use crate::since::{parse_since_cutoff, resolve_now};

const DEFAULT_RECENT_LIMIT: usize = 20;

fn filter_by_since(
    sessions: Vec<SessionReport>,
    since_spec: Option<&str>,
) -> Result<Vec<SessionReport>, String> {
    let Some(spec) = since_spec else {
        return Ok(sessions);
    };
    let now = resolve_now()?;
    let cutoff = parse_since_cutoff(spec, now)?;
    Ok(sessions
        .into_iter()
        .filter(|s| s.ended_at >= cutoff)
        .collect())
}

fn open_export_writer(out_path: Option<&Path>) -> Result<Box<dyn Write>, String> {
    match out_path {
        Some(path) => {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("failed to create parent directory: {e}"))?;
            }
            let file = File::create(path)
                .map_err(|e| format!("failed to create export file `{}`: {e}", path.display()))?;
            Ok(Box::new(BufWriter::new(file)))
        }
        None => Ok(Box::new(BufWriter::new(io::stdout().lock()))),
    }
}

fn execute_export(catalog: &PathCatalog, args: &ExportArgs) -> Result<(), String> {
    if args.format == ExportFormat::Ndjson && args.show_paths {
        return Err(format!(
            "`--show-paths` cannot be combined with `--format ndjson` ({} is strictly pathless)",
            product::LEDGER_SCHEMA_V1
        ));
    }

    match (&args.session, &args.since) {
        (None, None) => {
            return Err(
                "`export` requires either a `<SESSION>` (`<id|latest>`) or `--since <WINDOW>`"
                    .to_string(),
            );
        }
        (Some(_), Some(_)) => {
            return Err("`export` accepts either `<SESSION>` or `--since`, not both".to_string());
        }
        _ => {}
    }

    let capture_raw_paths = args.show_paths && args.format != ExportFormat::Ndjson;
    let all_sessions =
        load_session_reports(catalog, None, capture_raw_paths).map_err(|e| e.to_string())?;

    let selected: Vec<SessionReport> = if let Some(ref selector) = args.session {
        vec![select_session(&all_sessions, selector)?.clone()]
    } else {
        filter_by_since(all_sessions, args.since.as_deref())?
    };

    let mut writer = open_export_writer(args.out.as_deref())?;
    match args.format {
        ExportFormat::Json => {
            write_json_export_v1(&mut writer, &selected, args.show_paths)
                .map_err(|e| format!("failed to write JSON export: {e}"))?;
        }
        ExportFormat::Md => {
            let md = render_markdown_export(&selected, args.show_paths);
            writer
                .write_all(md.as_bytes())
                .map_err(|e| format!("failed to write Markdown export: {e}"))?;
        }
        ExportFormat::Ndjson => {
            for report in &selected {
                write_ledger(&mut writer, &report.turns)
                    .map_err(|e| format!("failed to write NDJSON ledger: {e}"))?;
            }
        }
    }
    writer
        .flush()
        .map_err(|e| format!("failed to flush export output: {e}"))?;
    Ok(())
}

fn run(cli: Cli) -> Result<(), String> {
    // `--no-cache` (`PLAN.md` §3.2) forces direct raw-log decoding; before T12's
    // incremental index lands, both code paths execute the identical read-only pipeline.
    let _no_cache = cli.no_cache;
    let catalog = PathCatalog::from_env();

    match cli.command {
        None => {
            let mut sessions =
                load_session_reports(&catalog, None, false).map_err(|e| e.to_string())?;
            sessions.truncate(DEFAULT_RECENT_LIMIT);
            print!("{}", render_sessions_table(&sessions));
        }
        Some(Commands::Sessions(args)) => {
            let sessions =
                load_session_reports(&catalog, args.agent, false).map_err(|e| e.to_string())?;
            let mut filtered = filter_by_since(sessions, args.since.as_deref())?;
            if let Some(ref proj) = args.project {
                filtered.retain(|s| matches_project_filter(s, proj));
            }
            print!("{}", render_sessions_table(&filtered));
        }
        Some(Commands::Autopsy(args)) => {
            let sessions =
                load_session_reports(&catalog, None, false).map_err(|e| e.to_string())?;
            let target = select_session(&sessions, &args.session)?;
            print!(
                "{}",
                render_autopsy_report(target, args.turns, args.category)
            );
        }
        Some(Commands::Flags(args)) => {
            let sessions =
                load_session_reports(&catalog, None, false).map_err(|e| e.to_string())?;
            let filtered = filter_by_since(sessions, args.since.as_deref())?;
            print!("{}", render_cross_session_flags(&filtered));
        }
        Some(Commands::Top(args)) => {
            let sessions =
                load_session_reports(&catalog, None, false).map_err(|e| e.to_string())?;
            let filtered = filter_by_since(sessions, args.since.as_deref())?;
            print!("{}", render_top_table(&filtered, args.by));
        }
        Some(Commands::Export(ref args)) => {
            execute_export(&catalog, args)?;
        }
        Some(Commands::Doctor) => {
            let report = run_doctor(&catalog).map_err(|e| e.to_string())?;
            print!("{}", render_doctor_report(&report));
        }
    }

    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Err(err) = run(cli) {
        eprintln!("{}: error: {err}", product::BINARY_NAME);
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
