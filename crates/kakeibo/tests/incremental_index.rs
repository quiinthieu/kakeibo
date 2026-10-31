//! Property, byte-counter, WAL-invalidation, self-healing, CLI parity, and privacy
//! tests for T12 incremental index (`PLAN.md` §3.1–§3.3, §7, §9, §10 T12).

mod common;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use common::TestEnv;
use kakeibo::loader::{
    collect_observed_models_with_options, load_session_reports_with_options,
    run_doctor_with_options, IndexMode,
};
use kakeibo_core::pricing::PricingCatalog;
use kakeibo_index::AppPaths;
use kakeibo_parse::{PathCatalog, ReadCounter};
use proptest::prelude::*;
use rusqlite::Connection;

fn test_app_paths(env: &TestEnv) -> AppPaths {
    AppPaths::new(
        env.config_dir().join("kakeibo"),
        env.cache_dir().join("kakeibo"),
    )
}

fn test_catalog(env: &TestEnv) -> PathCatalog {
    PathCatalog::new(Some(env.home_dir().to_path_buf()), None, None)
        .with_xdg_config_home(Some(env.config_dir().to_path_buf()))
        .with_xdg_data_home(Some(env.data_dir().to_path_buf()))
        .with_current_dir(Some(env.root_dir().to_path_buf()))
}

fn claude_user_line(uid: &str, sid: &str, prompt: &str) -> String {
    format!(
        r#"{{"type":"user","uuid":"{uid}","sessionId":"{sid}","version":"1.0.42","cwd":"/workspace/proj","message":{{"role":"user","content":"{prompt}"}}}}"#
    )
}

#[allow(clippy::too_many_arguments)]
fn claude_assistant_line(
    uid: &str,
    sid: &str,
    req_id: &str,
    ts: &str,
    text: &str,
    inp: u64,
    cw: u64,
    cr: u64,
    out: u64,
    tool_use_id: Option<&str>,
) -> String {
    let content = match tool_use_id {
        Some(tid) => format!(
            r#"[{{"type":"text","text":"{text}"}},{{"type":"tool_use","id":"{tid}","name":"Read","input":{{"file_path":"src/lib.rs"}}}}]"#
        ),
        None => format!(r#"[{{"type":"text","text":"{text}"}}]"#),
    };
    format!(
        r#"{{"type":"assistant","uuid":"{uid}","sessionId":"{sid}","requestId":"{req_id}","timestamp":"{ts}","version":"1.0.42","cwd":"/workspace/proj","message":{{"id":"msg-{req_id}","model":"claude-sonnet-4-5","content":{content},"usage":{{"input_tokens":{inp},"cache_creation_input_tokens":{cw},"cache_read_input_tokens":{cr},"output_tokens":{out}}}}}}}"#
    )
}

fn claude_tool_result_line(uid: &str, sid: &str, tool_use_id: &str, body: &str) -> String {
    format!(
        r#"{{"type":"user","uuid":"{uid}","sessionId":"{sid}","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{tool_use_id}","content":"{body}"}}]}}}}"#
    )
}

fn codex_meta_and_ctx(sid: &str, ts: &str) -> String {
    format!(
        concat!(
            r#"{{"timestamp":"{ts}","type":"session_meta","payload":{{"id":"{sid}","cwd":"/workspace/codex-proj","cli_version":"0.45.0"}}}}"#,
            "\n",
            r#"{{"timestamp":"{ts}","type":"turn_context","payload":{{"cwd":"/workspace/codex-proj","model":"gpt-5-codex"}}}}"#,
            "\n"
        ),
        ts = ts,
        sid = sid
    )
}

fn codex_user_and_reply(ts: &str, prompt: &str, reply: &str) -> String {
    format!(
        concat!(
            r#"{{"timestamp":"{ts}","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"{prompt}"}}]}}}}"#,
            "\n",
            r#"{{"timestamp":"{ts}","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"{reply}"}}]}}}}"#,
            "\n"
        ),
        ts = ts,
        prompt = prompt,
        reply = reply
    )
}

fn codex_tc_line(
    ts: &str,
    inp: u64,
    cached: u64,
    out: u64,
    total_in: u64,
    total_cached: u64,
    total_out: u64,
) -> String {
    format!(
        r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":{inp},"cached_input_tokens":{cached},"output_tokens":{out},"reasoning_output_tokens":0,"total_tokens":{tot}}},"total_token_usage":{{"input_tokens":{total_in},"cached_input_tokens":{total_cached},"output_tokens":{total_out},"reasoning_output_tokens":0,"total_tokens":{gtot}}}}},"rate_limits":{{"primary":{{"used_percent":12.5}}}}}}}}"#,
        ts = ts,
        inp = inp,
        cached = cached,
        out = out,
        tot = inp + out,
        total_in = total_in,
        total_cached = total_cached,
        total_out = total_out,
        gtot = total_in + total_out,
    )
}

fn codex_tur_line(ts: &str, turn_id: &str, inp: u64, cached: u64, out: u64) -> String {
    format!(
        r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"turn_completed","turn_id":"{turn_id}","model":"gpt-5-codex","usage":{{"input_tokens":{inp},"cached_input_tokens":{cached},"output_tokens":{out},"reasoning_output_tokens":0}}}}}}"#
    )
}

#[derive(Debug, Clone)]
enum MutationOp {
    AppendClaudeCompleteTurn {
        fresh: u16,
        cache_read: u16,
        out: u16,
    },
    AppendClaudeStreamingAndToolSplit {
        out_first: u16,
        out_final: u16,
    },
    AppendClaudePartialMidLine {
        fresh: u16,
        out: u16,
    },
    AppendCodexSplitTcThenTur {
        inp: u16,
        cached: u16,
        out: u16,
    },
    TruncateClaudeToFirstTurn,
    RotateCodexNewInode {
        turns: u8,
        inp: u16,
    },
    AppendGeminiWithRewindOrPatch {
        do_rewind: bool,
        inp: u16,
        out: u16,
    },
    DeleteAndRecreateSubagent {
        recreate: bool,
    },
}

fn arb_mutation_op() -> impl Strategy<Value = MutationOp> {
    prop_oneof![
        (100_u16..4000, 0_u16..8000, 10_u16..800).prop_map(|(fresh, cache_read, out)| {
            MutationOp::AppendClaudeCompleteTurn {
                fresh,
                cache_read,
                out,
            }
        }),
        (5_u16..80, 81_u16..500).prop_map(|(out_first, out_final)| {
            MutationOp::AppendClaudeStreamingAndToolSplit {
                out_first,
                out_final,
            }
        }),
        (100_u16..3000, 10_u16..400)
            .prop_map(|(fresh, out)| MutationOp::AppendClaudePartialMidLine { fresh, out }),
        (500_u16..4000, 0_u16..400, 20_u16..400).prop_map(|(inp, cached, out)| {
            MutationOp::AppendCodexSplitTcThenTur { inp, cached, out }
        }),
        Just(MutationOp::TruncateClaudeToFirstTurn),
        (1_u8..=4, 600_u16..3000)
            .prop_map(|(turns, inp)| MutationOp::RotateCodexNewInode { turns, inp }),
        (any::<bool>(), 200_u16..2000, 20_u16..300).prop_map(|(do_rewind, inp, out)| {
            MutationOp::AppendGeminiWithRewindOrPatch {
                do_rewind,
                inp,
                out,
            }
        }),
        any::<bool>().prop_map(|recreate| MutationOp::DeleteAndRecreateSubagent { recreate }),
    ]
}

fn append_bytes(path: &Path, bytes: &[u8]) {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open log for append");
    f.write_all(bytes).expect("append bytes");
}

fn assert_cached_equals_uncached(
    catalog: &PathCatalog,
    pricing: &PricingCatalog,
    app_paths: &AppPaths,
) {
    let cached = load_session_reports_with_options(
        catalog,
        pricing,
        None,
        false,
        IndexMode::Cached(app_paths),
        None,
    )
    .expect("cached load_session_reports");
    let uncached =
        load_session_reports_with_options(catalog, pricing, None, false, IndexMode::Uncached, None)
            .expect("uncached load_session_reports");
    assert_eq!(cached, uncached, "cached reports diverged from uncached");

    let cached_doc = run_doctor_with_options(catalog, IndexMode::Cached(app_paths), None)
        .expect("cached doctor");
    let uncached_doc =
        run_doctor_with_options(catalog, IndexMode::Uncached, None).expect("uncached doctor");
    assert_eq!(
        cached_doc, uncached_doc,
        "cached doctor diverged from uncached"
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn proptest_cached_equals_uncached_across_appends_splits_truncations_rotations_and_deletions(
        ops in prop::collection::vec(arb_mutation_op(), 3..8)
    ) {
        let env = TestEnv::new();
        let app_paths = test_app_paths(&env);
        let catalog = test_catalog(&env);
        let pricing = PricingCatalog::bundled();

        let claude_dir = env.home_dir().join(".claude/projects/synth-proj");
        let claude_sub_dir = claude_dir.join("claude-prop-1/subagents");
        let codex_dir = env.home_dir().join(".codex/sessions/2026/10/31");
        let gemini_dir = env.home_dir().join(".gemini/tmp/synth-gem/chats");
        for dir in [&claude_sub_dir, &codex_dir, &gemini_dir] {
            fs::create_dir_all(dir).unwrap();
        }

        let claude_log = claude_dir.join("claude-prop-1.jsonl");
        let claude_sub_log = claude_sub_dir.join("agent-sub1.jsonl");
        let codex_log = codex_dir.join("rollout-2026-10-31T10-00-00-codex-prop-1.jsonl");
        let gemini_log = gemini_dir.join("session-2026-10-31T10-00-gemprop1.jsonl");

        // Initial seed state across Claude (+ subagent), Codex, and Gemini.
        let claude_turn1 = format!(
            "{}\n{}\n",
            claude_user_line("u-0", "claude-prop-1", "initial user prompt"),
            claude_assistant_line(
                "a-0",
                "claude-prop-1",
                "req-0",
                "2026-10-31T10:00:01Z",
                "initial reply",
                800,
                400,
                0,
                60,
                None,
            )
        );
        let claude_first_turn_len = claude_turn1.len() as u64;
        fs::write(&claude_log, claude_turn1.as_bytes()).unwrap();

        let sub_init = format!(
            "{}\n{}\n",
            claude_user_line("su-0", "sub-agent-1", "subagent prompt"),
            claude_assistant_line(
                "sa-0",
                "sub-agent-1",
                "sreq-0",
                "2026-10-31T10:00:02Z",
                "subagent reply",
                300,
                0,
                0,
                40,
                None,
            )
        );
        fs::write(&claude_sub_log, sub_init.as_bytes()).unwrap();

        let mut codex_seq: u32 = 1;
        let mut codex_cum_in: u64 = 1000;
        let mut codex_cum_cached: u64 = 200;
        let mut codex_cum_out: u64 = 80;
        let codex_init = format!(
            "{}{}{}\n{}\n",
            codex_meta_and_ctx("codex-prop-1", "2026-10-31T10:00:00Z"),
            codex_user_and_reply("2026-10-31T10:00:01Z", "codex q1", "codex a1"),
            codex_tc_line("2026-10-31T10:00:02Z", 1000, 200, 80, codex_cum_in, codex_cum_cached, codex_cum_out),
            codex_tur_line("2026-10-31T10:00:02Z", "cturn-1", 1000, 200, 80),
        );
        fs::write(&codex_log, codex_init.as_bytes()).unwrap();

        let mut gem_msg_idx: u32 = 1;
        let gem_init = concat!(
            r#"{"sessionId":"gem-prop-1","projectHash":"abcdef0123456789","startTime":"2026-10-31T10:00:00Z","lastUpdated":"2026-10-31T10:00:01Z"}"#, "\n",
            r#"{"id":"gu-1","timestamp":"2026-10-31T10:00:01Z","type":"user","content":"gemini q1"}"#, "\n",
            r#"{"id":"ga-1","timestamp":"2026-10-31T10:00:02Z","type":"gemini","model":"gemini-2.5-pro","content":"gemini a1","tokens":{"input":900,"output":70,"cached":100,"thoughts":10,"tool":0,"total":980}}"#, "\n",
        );
        fs::write(&gemini_log, gem_init.as_bytes()).unwrap();

        // Verify baseline cold + warm parity.
        assert_cached_equals_uncached(&catalog, &pricing, &app_paths);
        assert_cached_equals_uncached(&catalog, &pricing, &app_paths);

        let mut step_no: u32 = 1;
        for op in ops {
            step_no += 1;
            let ts = format!("2026-10-31T10:{:02}:{:02}Z", (step_no / 60) % 60, step_no % 60);

            match op {
                MutationOp::AppendClaudeCompleteTurn { fresh, cache_read, out } => {
                    let line = format!(
                        "{}\n{}\n",
                        claude_user_line(&format!("u-{step_no}"), "claude-prop-1", "next prompt"),
                        claude_assistant_line(
                            &format!("a-{step_no}"),
                            "claude-prop-1",
                            &format!("req-{step_no}"),
                            &ts,
                            "next reply",
                            u64::from(fresh),
                            0,
                            u64::from(cache_read),
                            u64::from(out),
                            None,
                        )
                    );
                    append_bytes(&claude_log, line.as_bytes());
                    assert_cached_equals_uncached(&catalog, &pricing, &app_paths);
                }
                MutationOp::AppendClaudeStreamingAndToolSplit { out_first, out_final } => {
                    let req = format!("req-split-{step_no}");
                    let tid = format!("toolu-{step_no}");
                    // Sub-step A: user prompt + first streaming chunk only
                    let chunk_a = format!(
                        "{}\n{}\n",
                        claude_user_line(&format!("u-sa-{step_no}"), "claude-prop-1", "run tool"),
                        claude_assistant_line(
                            &format!("a-sa1-{step_no}"),
                            "claude-prop-1",
                            &req,
                            &ts,
                            "partial stream",
                            1100,
                            0,
                            400,
                            u64::from(out_first),
                            None,
                        )
                    );
                    append_bytes(&claude_log, chunk_a.as_bytes());
                    assert_cached_equals_uncached(&catalog, &pricing, &app_paths);

                    // Sub-step B: final streaming chunk with higher output + tool_use (no tool_result yet!)
                    let chunk_b = format!(
                        "{}\n",
                        claude_assistant_line(
                            &format!("a-sa2-{step_no}"),
                            "claude-prop-1",
                            &req,
                            &ts,
                            "completed stream",
                            1100,
                            0,
                            400,
                            u64::from(out_final),
                            Some(&tid),
                        )
                    );
                    append_bytes(&claude_log, chunk_b.as_bytes());
                    assert_cached_equals_uncached(&catalog, &pricing, &app_paths);

                    // Sub-step C: tool_result + follow-up assistant turn across append boundary
                    step_no += 1;
                    let ts_next = format!("2026-10-31T10:{:02}:{:02}Z", (step_no / 60) % 60, step_no % 60);
                    let chunk_c = format!(
                        "{}\n{}\n",
                        claude_tool_result_line(&format!("u-tr-{step_no}"), "claude-prop-1", &tid, "tool output payload"),
                        claude_assistant_line(
                            &format!("a-after-tr-{step_no}"),
                            "claude-prop-1",
                            &format!("req-after-tr-{step_no}"),
                            &ts_next,
                            "processed tool output",
                            1400,
                            0,
                            900,
                            75,
                            None,
                        )
                    );
                    append_bytes(&claude_log, chunk_c.as_bytes());
                    assert_cached_equals_uncached(&catalog, &pricing, &app_paths);
                }
                MutationOp::AppendClaudePartialMidLine { fresh, out } => {
                    let full_line = format!(
                        "{}\n",
                        claude_assistant_line(
                            &format!("a-mid-{step_no}"),
                            "claude-prop-1",
                            &format!("req-mid-{step_no}"),
                            &ts,
                            "line written in two halves",
                            u64::from(fresh),
                            0,
                            200,
                            u64::from(out),
                            None,
                        )
                    );
                    let split_at = full_line.len() / 2;
                    // Half 1: non-`\n`-terminated trailing fragment!
                    append_bytes(&claude_log, &full_line.as_bytes()[..split_at]);
                    assert_cached_equals_uncached(&catalog, &pricing, &app_paths);

                    // Half 2: completes the JSON line + `\n`!
                    append_bytes(&claude_log, &full_line.as_bytes()[split_at..]);
                    assert_cached_equals_uncached(&catalog, &pricing, &app_paths);
                }
                MutationOp::AppendCodexSplitTcThenTur { inp, cached, out } => {
                    codex_seq += 1;
                    let inp_u = u64::from(inp);
                    let cached_u = u64::from(cached).min(inp_u);
                    let out_u = u64::from(out);
                    codex_cum_in += inp_u;
                    codex_cum_cached += cached_u;
                    codex_cum_out += out_u;

                    // Sub-step A: `tc` flushed at EOF BEFORE `tur` arrives
                    let part_a = format!(
                        "{}{}\n",
                        codex_user_and_reply(&ts, "split q", "split a"),
                        codex_tc_line(&ts, inp_u, cached_u, out_u, codex_cum_in, codex_cum_cached, codex_cum_out)
                    );
                    append_bytes(&codex_log, part_a.as_bytes());
                    assert_cached_equals_uncached(&catalog, &pricing, &app_paths);

                    // Sub-step B: `tur` arrives for that exact turn (must NOT double count!)
                    let part_b = format!(
                        "{}\n",
                        codex_tur_line(&ts, &format!("cturn-{codex_seq}"), inp_u, cached_u, out_u)
                    );
                    append_bytes(&codex_log, part_b.as_bytes());
                    assert_cached_equals_uncached(&catalog, &pricing, &app_paths);
                }
                MutationOp::TruncateClaudeToFirstTurn => {
                    let f = OpenOptions::new().write(true).open(&claude_log).unwrap();
                    f.set_len(claude_first_turn_len).unwrap();
                    assert_cached_equals_uncached(&catalog, &pricing, &app_paths);
                }
                MutationOp::RotateCodexNewInode { turns, inp } => {
                    codex_seq = 0;
                    codex_cum_in = 0;
                    codex_cum_cached = 0;
                    codex_cum_out = 0;
                    let mut buf = codex_meta_and_ctx("codex-prop-1", "2026-10-31T09:59:00Z");
                    for idx in 1..=turns {
                        codex_seq += 1;
                        let turn_in = u64::from(inp) + u64::from(idx) * 100;
                        let turn_cached = 100;
                        let turn_out = 55;
                        codex_cum_in += turn_in;
                        codex_cum_cached += turn_cached;
                        codex_cum_out += turn_out;
                        let t_ts = format!("2026-10-31T09:59:{:02}Z", idx);
                        buf.push_str(&codex_user_and_reply(&t_ts, "rotated q", "rotated a"));
                        buf.push_str(&codex_tc_line(
                            &t_ts,
                            turn_in,
                            turn_cached,
                            turn_out,
                            codex_cum_in,
                            codex_cum_cached,
                            codex_cum_out,
                        ));
                        buf.push('\n');
                        buf.push_str(&codex_tur_line(
                            &t_ts,
                            &format!("rot-turn-{idx}"),
                            turn_in,
                            turn_cached,
                            turn_out,
                        ));
                        buf.push('\n');
                    }
                    // Write to sibling temp file and rename atomically (new inode, possibly larger size!)
                    let tmp_rot = codex_dir.join(".tmp-rotate-codex");
                    fs::write(&tmp_rot, buf.as_bytes()).unwrap();
                    fs::rename(&tmp_rot, &codex_log).unwrap();
                    assert_cached_equals_uncached(&catalog, &pricing, &app_paths);
                }
                MutationOp::AppendGeminiWithRewindOrPatch { do_rewind, inp, out } => {
                    gem_msg_idx += 1;
                    let cached_u = (u64::from(inp) / 4).min(u64::from(inp));
                    if do_rewind {
                        let line = format!(
                            concat!(
                                r#"{{"$rewindTo":"ga-1"}}"#, "\n",
                                r#"{{"id":"gu-{idx}","timestamp":"{ts}","type":"user","content":"rewound q"}}"#, "\n",
                                r#"{{"id":"ga-{idx}","timestamp":"{ts}","type":"gemini","model":"gemini-2.5-pro","content":"rewound a","tokens":{{"input":{inp},"output":{out},"cached":{cached},"thoughts":5,"tool":0,"total":{tot}}}}}"#, "\n"
                            ),
                            idx = gem_msg_idx,
                            ts = ts,
                            inp = inp,
                            out = out,
                            cached = cached_u,
                            tot = u64::from(inp) + u64::from(out) + 5,
                        );
                        append_bytes(&gemini_log, line.as_bytes());
                    } else {
                        let prev_id = "ga-1";
                        let line = format!(
                            concat!(
                                r#"{{"$patch":{{"id":"{prev_id}","tokens":{{"input":{inp},"output":{out},"cached":{cached},"thoughts":12,"tool":0,"total":{tot}}}}}}}"#, "\n",
                                r#"{{"id":"gu-{idx}","timestamp":"{ts}","type":"user","content":"patched q"}}"#, "\n",
                                r#"{{"id":"ga-{idx}","timestamp":"{ts}","type":"gemini","model":"gemini-2.5-pro","content":"patched a","tokens":{{"input":{inp},"output":{out},"cached":{cached},"thoughts":0,"tool":0,"total":{tot2}}}}}"#, "\n"
                            ),
                            prev_id = prev_id,
                            idx = gem_msg_idx,
                            ts = ts,
                            inp = inp,
                            out = out,
                            cached = cached_u,
                            tot = u64::from(inp) + u64::from(out) + 12,
                            tot2 = u64::from(inp) + u64::from(out),
                        );
                        append_bytes(&gemini_log, line.as_bytes());
                    }
                    assert_cached_equals_uncached(&catalog, &pricing, &app_paths);
                }
                MutationOp::DeleteAndRecreateSubagent { recreate } => {
                    let _ = fs::remove_file(&claude_sub_log);
                    assert_cached_equals_uncached(&catalog, &pricing, &app_paths);
                    if recreate {
                        fs::write(&claude_sub_log, sub_init.as_bytes()).unwrap();
                        assert_cached_equals_uncached(&catalog, &pricing, &app_paths);
                    }
                }
            }
        }
    }
}

#[test]
fn append_rereads_only_new_bytes_and_warm_cache_reads_zero_bytes() {
    let env = TestEnv::new();
    let app_paths = test_app_paths(&env);
    let catalog = test_catalog(&env);
    let pricing = PricingCatalog::bundled();
    let counter = ReadCounter::new();

    let claude_dir = env.home_dir().join(".claude/projects/counter-proj");
    let codex_dir = env.home_dir().join(".codex/sessions/2026/10/31");
    let gemini_dir = env.home_dir().join(".gemini/tmp/counter-gem/chats");
    let aider_dir = env.home_dir().join(".aider");
    for d in [&claude_dir, &codex_dir, &gemini_dir, &aider_dir] {
        fs::create_dir_all(d).unwrap();
    }

    let claude_path = claude_dir.join("claude-cnt.jsonl");
    let codex_path = codex_dir.join("rollout-2026-10-31T12-00-00-codex-cnt.jsonl");
    let gemini_path = gemini_dir.join("session-2026-10-31T12-00-gemcnt.jsonl");
    let aider_md_path = aider_dir.join(".aider.chat.history.md");
    let aider_ana_path = aider_dir.join("analytics.jsonl");

    let claude_init = format!(
        "{}\n{}\n",
        claude_user_line("u-1", "claude-cnt", "hello claude"),
        claude_assistant_line(
            "a-1",
            "claude-cnt",
            "req-1",
            "2026-10-31T12:00:01Z",
            "reply 1",
            900,
            300,
            0,
            50,
            None,
        )
    );
    let codex_init = format!(
        "{}{}{}\n{}\n",
        codex_meta_and_ctx("codex-cnt", "2026-10-31T12:00:00Z"),
        codex_user_and_reply("2026-10-31T12:00:01Z", "hello codex", "codex reply 1"),
        codex_tc_line("2026-10-31T12:00:02Z", 1200, 300, 90, 1200, 300, 90),
        codex_tur_line("2026-10-31T12:00:02Z", "ct-1", 1200, 300, 90),
    );
    let gemini_init = concat!(
        r#"{"sessionId":"gem-cnt","projectHash":"1122334455667788","startTime":"2026-10-31T12:00:00Z","lastUpdated":"2026-10-31T12:00:01Z"}"#,
        "\n",
        r#"{"id":"gu-1","timestamp":"2026-10-31T12:00:01Z","type":"user","content":"hello gemini"}"#,
        "\n",
        r#"{"id":"ga-1","timestamp":"2026-10-31T12:00:02Z","type":"gemini","model":"gemini-2.5-pro","content":"gemini reply 1","tokens":{"input":800,"output":60,"cached":100,"thoughts":0,"tool":0,"total":860}}"#,
        "\n"
    );
    let aider_md_init = "\
# aider chat started at 2026-10-31 12:00:00
> Aider v0.83.0
> Main model: claude-sonnet-4-5 with diff edit format
#### Initial aider question
Initial aider answer.
> Tokens: 2.0k sent, 1.0k cache hit, 120 received.
";
    let aider_ana_init = concat!(
        r#"{"event":"launched","properties":{"version":"0.83.0","main_model":"gpt-4o"},"time":1793448000,"user_id":"uid-cnt"}"#,
        "\n",
        r#"{"event":"message_send","properties":{"main_model":"gpt-4o","prompt_tokens":1500,"completion_tokens":110},"time":1793448005,"user_id":"uid-cnt"}"#,
        "\n"
    );

    fs::write(&claude_path, claude_init.as_bytes()).unwrap();
    fs::write(&codex_path, codex_init.as_bytes()).unwrap();
    fs::write(&gemini_path, gemini_init.as_bytes()).unwrap();
    fs::write(&aider_md_path, aider_md_init.as_bytes()).unwrap();
    fs::write(&aider_ana_path, aider_ana_init.as_bytes()).unwrap();

    let total_init_bytes = (claude_init.len()
        + codex_init.len()
        + gemini_init.len()
        + aider_md_init.len()
        + aider_ana_init.len()) as u64;

    // 1. Cold cache run reads all 5 files and exact initial byte count.
    let cold_reports = load_session_reports_with_options(
        &catalog,
        &pricing,
        None,
        false,
        IndexMode::Cached(&app_paths),
        Some(&counter),
    )
    .unwrap();
    assert_eq!(counter.files_opened(), 5);
    assert_eq!(counter.bytes_read(), total_init_bytes);
    assert_eq!(cold_reports.len(), 5);

    // 2. Warm unchanged cache run: 0 files opened, 0 bytes read across reports, models, and doctor!
    counter.reset();
    let warm_reports = load_session_reports_with_options(
        &catalog,
        &pricing,
        None,
        false,
        IndexMode::Cached(&app_paths),
        Some(&counter),
    )
    .unwrap();
    assert_eq!(counter.files_opened(), 0);
    assert_eq!(counter.bytes_read(), 0);
    assert_eq!(warm_reports, cold_reports);

    let warm_models = collect_observed_models_with_options(
        &catalog,
        &pricing,
        None,
        None,
        IndexMode::Cached(&app_paths),
        Some(&counter),
    )
    .unwrap();
    let uncached_models = collect_observed_models_with_options(
        &catalog,
        &pricing,
        None,
        None,
        IndexMode::Uncached,
        None,
    )
    .unwrap();
    assert_eq!(counter.files_opened(), 0);
    assert_eq!(counter.bytes_read(), 0);
    assert_eq!(warm_models, uncached_models);

    let warm_doctor =
        run_doctor_with_options(&catalog, IndexMode::Cached(&app_paths), Some(&counter)).unwrap();
    let uncached_doctor = run_doctor_with_options(&catalog, IndexMode::Uncached, None).unwrap();
    assert_eq!(counter.files_opened(), 0);
    assert_eq!(counter.bytes_read(), 0);
    assert_eq!(warm_doctor, uncached_doctor);

    // 3. Append new turns to all 5 files -> re-reads ONLY the newly appended bytes!
    let claude_delta = format!(
        "{}\n{}\n",
        claude_user_line("u-2", "claude-cnt", "second claude prompt"),
        claude_assistant_line(
            "a-2",
            "claude-cnt",
            "req-2",
            "2026-10-31T12:01:00Z",
            "reply 2",
            400,
            0,
            900,
            70,
            None,
        )
    );
    let codex_delta = format!(
        "{}{}\n{}\n",
        codex_user_and_reply("2026-10-31T12:01:00Z", "second codex", "codex reply 2"),
        codex_tc_line("2026-10-31T12:01:01Z", 1600, 1100, 95, 2800, 1400, 185),
        codex_tur_line("2026-10-31T12:01:01Z", "ct-2", 1600, 1100, 95),
    );
    let gemini_delta = concat!(
        r#"{"id":"gu-2","timestamp":"2026-10-31T12:01:00Z","type":"user","content":"second gemini"}"#,
        "\n",
        r#"{"id":"ga-2","timestamp":"2026-10-31T12:01:01Z","type":"gemini","model":"gemini-2.5-pro","content":"gemini reply 2","tokens":{"input":1100,"output":80,"cached":700,"thoughts":0,"tool":0,"total":1180}}"#,
        "\n"
    );
    let aider_md_delta = "\
#### Second aider question
Second aider answer.
> Tokens: 2.5k sent, 1.8k cache hit, 140 received.
";
    let aider_ana_delta = concat!(
        r#"{"event":"message_send","properties":{"main_model":"gpt-4o","prompt_tokens":1900,"completion_tokens":130},"time":1793448060,"user_id":"uid-cnt"}"#,
        "\n"
    );

    append_bytes(&claude_path, claude_delta.as_bytes());
    append_bytes(&codex_path, codex_delta.as_bytes());
    append_bytes(&gemini_path, gemini_delta.as_bytes());
    append_bytes(&aider_md_path, aider_md_delta.as_bytes());
    append_bytes(&aider_ana_path, aider_ana_delta.as_bytes());

    let expected_appended_bytes = (claude_delta.len()
        + codex_delta.len()
        + gemini_delta.len()
        + aider_md_delta.len()
        + aider_ana_delta.len()) as u64;

    counter.reset();
    let appended_reports = load_session_reports_with_options(
        &catalog,
        &pricing,
        None,
        false,
        IndexMode::Cached(&app_paths),
        Some(&counter),
    )
    .unwrap();
    assert_eq!(counter.files_opened(), 5);
    assert_eq!(
        counter.bytes_read(),
        expected_appended_bytes,
        "append must re-read ONLY the newly appended bytes"
    );

    let uncached_after_append = load_session_reports_with_options(
        &catalog,
        &pricing,
        None,
        false,
        IndexMode::Uncached,
        None,
    )
    .unwrap();
    assert_eq!(appended_reports, uncached_after_append);

    // 4. Subsequent warm run reads 0 bytes again.
    counter.reset();
    let warm_again = load_session_reports_with_options(
        &catalog,
        &pricing,
        None,
        false,
        IndexMode::Cached(&app_paths),
        Some(&counter),
    )
    .unwrap();
    assert_eq!(counter.files_opened(), 0);
    assert_eq!(counter.bytes_read(), 0);
    assert_eq!(warm_again, uncached_after_append);
}

#[test]
fn sqlite_wal_commits_invalidate_cache_even_before_checkpoint() {
    let env = TestEnv::new();
    let app_paths = test_app_paths(&env);
    let catalog = test_catalog(&env);
    let pricing = PricingCatalog::bundled();

    // Create a WAL-mode OpenCode SQLite DB with autocheckpoint disabled so subsequent
    // writes stay exclusively in `opencode.db-wal` without growing `opencode.db`.
    let opencode_dir = env.data_dir().join("opencode");
    fs::create_dir_all(&opencode_dir).unwrap();
    let db_path = opencode_dir.join("opencode.db");

    let writer = Connection::open(&db_path).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA wal_autocheckpoint = 0;
             CREATE TABLE session (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 directory TEXT,
                 time_created INTEGER NOT NULL
             );
             CREATE TABLE message (
                 id TEXT PRIMARY KEY,
                 session_id TEXT NOT NULL,
                 time_created INTEGER NOT NULL,
                 data TEXT NOT NULL
             );
             CREATE TABLE part (
                 id TEXT PRIMARY KEY,
                 message_id TEXT NOT NULL,
                 session_id TEXT NOT NULL,
                 time_created INTEGER NOT NULL,
                 data TEXT NOT NULL
             );
             INSERT INTO session (id, parent_id, directory, time_created)
             VALUES ('ses_wal_1', NULL, '/workspace/opencode-wal', 1793448000000);
             INSERT INTO message (id, session_id, time_created, data)
             VALUES (
                 'msg_1',
                 'ses_wal_1',
                 1793448001000,
                 '{\"role\":\"assistant\",\"modelID\":\"claude-sonnet-4-5\",\"tokens\":{\"input\":500,\"output\":40,\"reasoning\":0,\"cache\":{\"read\":100,\"write\":0}}}'
             );",
        )
        .unwrap();
    writer
        .query_row("PRAGMA wal_checkpoint(TRUNCATE);", [], |_| Ok(()))
        .unwrap();

    let main_db_size_before = fs::metadata(&db_path).unwrap().len();

    // Populate cache on initial state (1 turn).
    let counter = ReadCounter::new();
    let initial_cached = load_session_reports_with_options(
        &catalog,
        &pricing,
        None,
        false,
        IndexMode::Cached(&app_paths),
        Some(&counter),
    )
    .unwrap();
    assert_eq!(initial_cached.len(), 1);
    assert_eq!(initial_cached[0].turns.len(), 1);
    assert_eq!(counter.files_opened(), 1);

    // Warm run: 0 opens.
    counter.reset();
    let warm = load_session_reports_with_options(
        &catalog,
        &pricing,
        None,
        false,
        IndexMode::Cached(&app_paths),
        Some(&counter),
    )
    .unwrap();
    assert_eq!(counter.files_opened(), 0);
    assert_eq!(warm, initial_cached);

    // Commit a second message strictly into `opencode.db-wal` (no checkpoint!).
    writer
        .execute(
            "INSERT INTO message (id, session_id, time_created, data)
             VALUES (
                 'msg_2',
                 'ses_wal_1',
                 1793448010000,
                 '{\"role\":\"assistant\",\"modelID\":\"claude-sonnet-4-5\",\"tokens\":{\"input\":700,\"output\":65,\"reasoning\":0,\"cache\":{\"read\":450,\"write\":0}}}'
             );",
            [],
        )
        .unwrap();

    let main_db_size_after = fs::metadata(&db_path).unwrap().len();
    assert_eq!(
        main_db_size_before, main_db_size_after,
        "main opencode.db file size must still be unchanged before checkpoint"
    );
    assert!(
        opencode_dir.join("opencode.db-wal").is_file(),
        "opencode.db-wal must exist with uncheckpointed frames"
    );

    counter.reset();
    let updated_cached = load_session_reports_with_options(
        &catalog,
        &pricing,
        None,
        false,
        IndexMode::Cached(&app_paths),
        Some(&counter),
    )
    .unwrap();
    let updated_uncached = load_session_reports_with_options(
        &catalog,
        &pricing,
        None,
        false,
        IndexMode::Uncached,
        None,
    )
    .unwrap();
    assert_eq!(
        counter.files_opened(),
        1,
        "WAL growth must invalidate cached DB entry"
    );
    assert_eq!(updated_cached.len(), 1);
    assert_eq!(updated_cached[0].turns.len(), 2);
    assert_eq!(updated_cached, updated_uncached);
}

#[test]
fn corrupt_index_sqlite_self_heals_and_no_cache_never_creates_index() {
    let env = TestEnv::new();
    env.stage_fixture("claude-code", "basic");
    env.stage_fixture("codex", "basic");

    let index_db = env.cache_dir().join("kakeibo/index.sqlite");
    assert!(!index_db.exists());

    // 1. `--no-cache` must never create or touch `<cache_dir>/index.sqlite`.
    let no_cache_out = env
        .kakeibo_cmd()
        .args(["--no-cache", "sessions"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(
        !index_db.exists(),
        "`--no-cache` must not create `<cache_dir>/index.sqlite`"
    );

    // 2. Default cached invocation creates `<cache_dir>/index.sqlite` and matches `--no-cache`.
    let cold_out = env
        .kakeibo_cmd()
        .args(["sessions"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(index_db.is_file(), "cached run must create index.sqlite");
    assert_eq!(cold_out, no_cache_out);

    // 3. Corrupt `index.sqlite` with arbitrary non-SQLite bytes -> self-heals transparently!
    fs::write(&index_db, b"CORRUPT_NOT_SQLITE_HEADER_BYTES_12345").unwrap();
    let healed_out = env
        .kakeibo_cmd()
        .args(["sessions"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(healed_out, no_cache_out);

    // Verify rebuilt `index.sqlite` is a valid readable SQLite database.
    let conn = Connection::open(&index_db).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM indexed_sources;", [], |r| r.get(0))
        .unwrap();
    assert!(count >= 2);
}

#[test]
fn full_six_agent_cli_parity_and_privacy_zero_markers_in_index_sqlite() {
    let env = TestEnv::new();
    env.stage_fixture("claude-code", "basic");
    env.stage_fixture("codex", "basic");
    env.stage_fixture("gemini-cli", "rewind-patch");
    env.stage_fixture("opencode", "legacy-tree");
    env.stage_fixture("aider", "chat-history");
    env.stage_fixture("cursor", "estimated-tokens");

    // Also plant explicit canary transcript & path markers in an additional Claude + Aider log
    // to prove zero planted markers or raw paths ever enter `index.sqlite` (`PLAN.md` §9).
    let canary_prompt = "KAKEIBO_T12_SECRET_PROMPT_CANARY_9911";
    let canary_reply = "KAKEIBO_T12_SECRET_REPLY_CANARY_9922";
    let canary_tool = "KAKEIBO_T12_SECRET_TOOL_CANARY_9933";
    let canary_uid = "KAKEIBO_T12_RAW_AIDER_UID_9944";
    let canary_cwd = "/workspace/KAKEIBO_T12_RAW_PATH_CANARY_9955/repo";

    let extra_claude_dir = env.home_dir().join(".claude/projects/canary-proj");
    fs::create_dir_all(&extra_claude_dir).unwrap();
    let extra_claude = format!(
        concat!(
            r#"{{"type":"user","uuid":"cu-1","sessionId":"claude-canary-1","cwd":"{cwd}","message":{{"role":"user","content":"{prompt}"}}}}"#,
            "\n",
            r#"{{"type":"assistant","uuid":"ca-1","sessionId":"claude-canary-1","requestId":"creq-1","timestamp":"2026-10-31T18:00:01Z","cwd":"{cwd}","message":{{"id":"cmsg-1","model":"claude-sonnet-4-5","content":[{{"type":"text","text":"{reply}"}},{{"type":"tool_use","id":"ctu-1","name":"Bash","input":{{"command":"{tool}"}}}}],"usage":{{"input_tokens":1000,"cache_creation_input_tokens":200,"cache_read_input_tokens":0,"output_tokens":90}}}}}}"#,
            "\n",
            r#"{{"type":"user","uuid":"cu-2","sessionId":"claude-canary-1","cwd":"{cwd}","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"ctu-1","content":"{tool}"}}]}}}}"#,
            "\n",
            r#"{{"type":"assistant","uuid":"ca-2","sessionId":"claude-canary-1","requestId":"creq-2","timestamp":"2026-10-31T18:00:05Z","cwd":"{cwd}","message":{{"id":"cmsg-2","model":"claude-sonnet-4-5","content":[{{"type":"text","text":"{reply}"}}],"usage":{{"input_tokens":400,"cache_creation_input_tokens":0,"cache_read_input_tokens":1100,"output_tokens":60}}}}}}"#,
            "\n"
        ),
        cwd = canary_cwd,
        prompt = canary_prompt,
        reply = canary_reply,
        tool = canary_tool,
    );
    fs::write(extra_claude_dir.join("claude-canary-1.jsonl"), extra_claude).unwrap();

    let extra_aider_ana = env.home_dir().join(".aider/canary-analytics.jsonl");
    fs::create_dir_all(extra_aider_ana.parent().unwrap()).unwrap();
    let aider_ana_payload = format!(
        concat!(
            r#"{{"event":"launched","properties":{{"version":"0.83.0","main_model":"gpt-4o"}},"time":1793469600,"user_id":"{uid}"}}"#,
            "\n",
            r#"{{"event":"message_send","properties":{{"main_model":"gpt-4o","prompt_tokens":2100,"completion_tokens":125}},"time":1793469610,"user_id":"{uid}"}}"#,
            "\n"
        ),
        uid = canary_uid,
    );
    fs::write(&extra_aider_ana, aider_ana_payload).unwrap();

    // Freeze agent log directories strictly read-only while exercising both `--no-cache`
    // and cached (`index.sqlite`) CLI pipelines!
    let _ro_guard = env.make_logs_readonly();

    let subcommand_matrix: &[&[&str]] = &[
        &[],
        &["sessions"],
        &["sessions", "--agent", "codex"],
        &["sessions", "--agent", "cursor"],
        &["autopsy", "latest", "--turns"],
        &["flags"],
        &["top", "--by", "category"],
        &["top", "--by", "tool"],
        &["export", "--since", "36500d", "--format", "json"],
        &[
            "export",
            "--since",
            "36500d",
            "--format",
            "json",
            "--show-paths",
        ],
        &["export", "--since", "36500d", "--format", "md"],
        &["export", "--since", "36500d", "--format", "ndjson"],
        &["pricing", "check"],
        &["doctor"],
    ];

    for args in subcommand_matrix {
        let mut uncached_args = vec!["--no-cache"];
        uncached_args.extend_from_slice(args);

        let uncached_stdout = env
            .kakeibo_cmd()
            .env("AIDER_ANALYTICS_LOG", &extra_aider_ana)
            .args(&uncached_args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();

        let cached_stdout_1 = env
            .kakeibo_cmd()
            .env("AIDER_ANALYTICS_LOG", &extra_aider_ana)
            .args(*args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();

        let cached_stdout_2 = env
            .kakeibo_cmd()
            .env("AIDER_ANALYTICS_LOG", &extra_aider_ana)
            .args(*args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();

        assert_eq!(
            cached_stdout_1, uncached_stdout,
            "first cached run diverged from --no-cache for args {args:?}"
        );
        assert_eq!(
            cached_stdout_2, uncached_stdout,
            "warm cached run diverged from --no-cache for args {args:?}"
        );
    }

    // Privacy scan over `<cache_dir>/index.sqlite` (+ `-wal` / `-shm`):
    // zero planted transcript markers (`KAKEIBO_`), zero raw `user_id`, and zero raw paths!
    let cache_kakeibo_dir = env.cache_dir().join("kakeibo");
    let home_str = env.home_dir().to_str().unwrap();
    let forbidden_substrings = [
        "KAKEIBO_",
        canary_prompt,
        canary_reply,
        canary_tool,
        canary_uid,
        canary_cwd,
        "KAKEIBO_T12_RAW_PATH_CANARY_9955",
        home_str,
    ];

    let mut inspected_files = 0_usize;
    for suffix in ["index.sqlite", "index.sqlite-wal", "index.sqlite-shm"] {
        let candidate = cache_kakeibo_dir.join(suffix);
        if !candidate.is_file() {
            continue;
        }
        inspected_files += 1;
        let raw_bytes = fs::read(&candidate).unwrap();
        let lossy = String::from_utf8_lossy(&raw_bytes);
        for needle in forbidden_substrings {
            assert!(
                !lossy.contains(needle),
                "privacy violation: `{needle}` found inside `{}`",
                candidate.display()
            );
        }
    }
    assert!(
        inspected_files >= 1,
        "index.sqlite must exist for privacy scan"
    );
}
