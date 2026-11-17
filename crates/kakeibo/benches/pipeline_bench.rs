//! Criterion micro- and pipeline-level benchmarks for T12 (`PLAN.md` §7, §9, §10 T12).

use std::fs::{self, OpenOptions};
use std::io::Write;

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use kakeibo::loader::{load_session_reports_with_options, IndexMode};
use kakeibo_core::pricing::PricingCatalog;
use kakeibo_index::AppPaths;
use kakeibo_parse::prefilter::{
    classify_claude_line, classify_codex_line, ClaudeLineClass, CodexLineClass,
};
use kakeibo_parse::PathCatalog;
use tempfile::TempDir;

struct BenchFixture {
    _tmp: TempDir,
    catalog: PathCatalog,
    app_paths: AppPaths,
    pricing: PricingCatalog,
    active_claude_log: std::path::PathBuf,
    total_bytes: u64,
}

fn build_bench_fixture(file_count: usize, turns_per_file: usize) -> BenchFixture {
    let tmp = tempfile::tempdir().expect("bench tempdir");
    let home = tmp.path().join("home");
    let config_dir = tmp.path().join("config");
    let cache_dir = tmp.path().join("cache");
    let claude_proj = home.join(".claude/projects/synth-bench");
    let codex_day = home.join(".codex/sessions/2026/10/31");
    for d in [&claude_proj, &codex_day, &config_dir, &cache_dir] {
        fs::create_dir_all(d).expect("create bench dir");
    }

    let blob = "synthetic_tool_output_line_alpha_beta_gamma_delta_0123456789 ".repeat(64);
    let mut total_bytes: u64 = 0;
    let mut active_claude_log = std::path::PathBuf::new();

    for f_idx in 0..file_count {
        if f_idx % 2 == 0 {
            let sid = format!("bench-claude-{f_idx:03}");
            let path = claude_proj.join(format!("{sid}.jsonl"));
            let mut buf = String::with_capacity(turns_per_file * 6_000);
            for t in 1..=turns_per_file {
                let ts = format!("2026-10-31T12:{:02}:{:02}Z", (t / 60) % 60, t % 60);
                let cache_read = (t as u64) * 1_200;
                buf.push_str(&format!(
                    r#"{{"type":"file-history-snapshot","messageId":"snap-{f_idx}-{t}","snapshot":{{"trackedFileBackups":{{}}}}}}"#
                ));
                buf.push('\n');
                buf.push_str(&format!(
                    r#"{{"type":"user","uuid":"u-{f_idx}-{t}","sessionId":"{sid}","cwd":"/workspace/synth-bench","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"tu-{f_idx}-{prev}","content":"{blob}"}}]}}}}"#,
                    prev = t.saturating_sub(1),
                ));
                buf.push('\n');
                buf.push_str(&format!(
                    r#"{{"type":"assistant","uuid":"a-{f_idx}-{t}","sessionId":"{sid}","requestId":"req-{f_idx}-{t}","timestamp":"{ts}","cwd":"/workspace/synth-bench","message":{{"id":"msg-{f_idx}-{t}","model":"claude-sonnet-4-5","content":[{{"type":"text","text":"synthetic assistant response turn {t}"}},{{"type":"tool_use","id":"tu-{f_idx}-{t}","name":"Read","input":{{"file_path":"src/mod_{t}.rs"}}}}],"usage":{{"input_tokens":450,"cache_creation_input_tokens":150,"cache_read_input_tokens":{cache_read},"output_tokens":140}}}}}}"#
                ));
                buf.push('\n');
            }
            total_bytes += buf.len() as u64;
            fs::write(&path, buf.as_bytes()).expect("write claude bench file");
            if active_claude_log.as_os_str().is_empty() {
                active_claude_log = path;
            }
        } else {
            let sid = format!("bench-codex-{f_idx:03}");
            let path = codex_day.join(format!("rollout-2026-10-31T12-00-00-{sid}.jsonl"));
            let mut buf = String::with_capacity(turns_per_file * 6_000);
            buf.push_str(&format!(
                r#"{{"timestamp":"2026-10-31T12:00:00Z","type":"session_meta","payload":{{"id":"{sid}","cwd":"/workspace/synth-bench","cli_version":"0.45.0"}}}}"#
            ));
            buf.push('\n');
            buf.push_str(
                r#"{"timestamp":"2026-10-31T12:00:00Z","type":"turn_context","payload":{"cwd":"/workspace/synth-bench","model":"gpt-5-codex"}}"#,
            );
            buf.push('\n');
            let mut cum_in = 0_u64;
            let mut cum_cached = 0_u64;
            let mut cum_out = 0_u64;
            for t in 1..=turns_per_file {
                let ts = format!("2026-10-31T12:{:02}:{:02}Z", (t / 60) % 60, t % 60);
                let cached = (t as u64) * 1_000;
                let inp = cached + 500;
                let out = 120_u64;
                cum_in += inp;
                cum_cached += cached;
                cum_out += out;
                buf.push_str(&format!(
                    r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"agent_message_delta","delta":"{blob}"}}}}"#
                ));
                buf.push('\n');
                buf.push_str(&format!(
                    r#"{{"timestamp":"{ts}","type":"response_item","payload":{{"type":"function_call_output","call_id":"call-{t}","output":"{blob}"}}}}"#
                ));
                buf.push('\n');
                buf.push_str(&format!(
                    r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":{inp},"cached_input_tokens":{cached},"output_tokens":{out},"reasoning_output_tokens":20,"total_tokens":{tot}}},"total_token_usage":{{"input_tokens":{cum_in},"cached_input_tokens":{cum_cached},"output_tokens":{cum_out},"reasoning_output_tokens":20,"total_tokens":{gtot}}}}}}}}}"#,
                    tot = inp + out,
                    gtot = cum_in + cum_out,
                ));
                buf.push('\n');
            }
            total_bytes += buf.len() as u64;
            fs::write(&path, buf.as_bytes()).expect("write codex bench file");
        }
    }

    let catalog = PathCatalog::new(Some(home), None, None);
    let app_paths = AppPaths::new(config_dir, cache_dir);
    let pricing = PricingCatalog::bundled();

    BenchFixture {
        _tmp: tmp,
        catalog,
        app_paths,
        pricing,
        active_claude_log,
        total_bytes,
    }
}

fn bench_prefilter(c: &mut Criterion) {
    let claude_snapshot = format!(
        r#"{{"type":"file-history-snapshot","messageId":"snap-1","snapshot":{{"blob":"{}"}}}}"#,
        "x".repeat(4096)
    );
    let codex_delta = format!(
        r#"{{"timestamp":"2026-10-31T12:00:00Z","type":"event_msg","payload":{{"type":"agent_message_delta","delta":"{}"}}}}"#,
        "y".repeat(4096)
    );

    let mut group = c.benchmark_group("prefilter");
    group.throughput(Throughput::Bytes(claude_snapshot.len() as u64));
    group.bench_function("classify_claude_non_turn_4kb", |b| {
        b.iter(|| {
            let cls = classify_claude_line(black_box(claude_snapshot.as_str()));
            debug_assert_eq!(cls, ClaudeLineClass::NonTurnFastPath);
            cls
        });
    });

    group.throughput(Throughput::Bytes(codex_delta.len() as u64));
    group.bench_function("classify_codex_delta_4kb", |b| {
        b.iter(|| {
            let cls = classify_codex_line(black_box(codex_delta.as_str()));
            debug_assert_eq!(cls, CodexLineClass::NonTurnFastPath);
            cls
        });
    });
    group.finish();
}

fn bench_pipeline(c: &mut Criterion) {
    let fx = build_bench_fixture(16, 25);

    // Prime the SQLite incremental cache once before warm/append benchmarks.
    let baseline = load_session_reports_with_options(
        &fx.catalog,
        &fx.pricing,
        None,
        false,
        IndexMode::Cached(&fx.app_paths),
        None,
    )
    .expect("prime cache");
    assert_eq!(baseline.len(), 16);

    let mut group = c.benchmark_group("pipeline");
    group.throughput(Throughput::Bytes(fx.total_bytes));

    group.bench_function("cold_uncached_16_sessions", |b| {
        b.iter(|| {
            let reports = load_session_reports_with_options(
                black_box(&fx.catalog),
                black_box(&fx.pricing),
                None,
                false,
                IndexMode::Uncached,
                None,
            )
            .expect("uncached load");
            black_box(reports.len())
        });
    });

    group.bench_function("warm_cached_16_sessions", |b| {
        b.iter(|| {
            let reports = load_session_reports_with_options(
                black_box(&fx.catalog),
                black_box(&fx.pricing),
                None,
                false,
                IndexMode::Cached(&fx.app_paths),
                None,
            )
            .expect("warm cached load");
            black_box(reports.len())
        });
    });

    let mut append_seq = 10_000_u64;
    group.bench_function("incremental_append_1_turn", |b| {
        b.iter(|| {
            append_seq += 1;
            let line = format!(
                "{{\"type\":\"assistant\",\"uuid\":\"a-app-{append_seq}\",\"sessionId\":\"bench-claude-000\",\"requestId\":\"req-app-{append_seq}\",\"timestamp\":\"2026-10-31T13:00:00Z\",\"cwd\":\"/workspace/synth-bench\",\"message\":{{\"id\":\"msg-app-{append_seq}\",\"model\":\"claude-sonnet-4-5\",\"content\":[{{\"type\":\"text\",\"text\":\"incremental turn\"}}],\"usage\":{{\"input_tokens\":300,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":32000,\"output_tokens\":80}}}}}}\n"
            );
            let mut f = OpenOptions::new()
                .append(true)
                .open(&fx.active_claude_log)
                .expect("open for append");
            f.write_all(line.as_bytes()).expect("append turn");
            drop(f);

            let reports = load_session_reports_with_options(
                black_box(&fx.catalog),
                black_box(&fx.pricing),
                None,
                false,
                IndexMode::Cached(&fx.app_paths),
                None,
            )
            .expect("incremental append load");
            black_box(reports.len())
        });
    });

    group.finish();
}

criterion_group!(benches, bench_prefilter, bench_pipeline);
criterion_main!(benches);
