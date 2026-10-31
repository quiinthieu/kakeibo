//! TDD integration tests for `BoundedLineReader` byte offsets, `ReadCounter`,
//! `memchr` pre-filtering, and exact offset-resumed incremental parsing (`PLAN.md` §7, §9, §10 T12).

use std::fs::{self, OpenOptions};
use std::io::{BufReader, Cursor, Seek, SeekFrom, Write};

use kakeibo_parse::line_reader::{BoundedLineReader, CountingReader, LineReadOutcome, ReadCounter};
use kakeibo_parse::prefilter::{
    classify_aider_analytics_line, classify_claude_line, classify_codex_line, AiderAnalyticsClass,
    ClaudeLineClass, CodexLineClass,
};
use kakeibo_parse::{
    parse_aider_analytics_file, parse_aider_analytics_file_incremental, parse_aider_history_file,
    parse_aider_history_file_incremental, parse_claude_file, parse_claude_file_incremental,
    parse_codex_file, parse_codex_file_incremental, parse_gemini_file_incremental,
    parse_gemini_file_with_options, ClaudeParseOptions, GeminiParseOptions, ParseDiagnosticKind,
};
use tempfile::TempDir;

#[test]
fn bounded_line_reader_tracks_exact_offsets_and_newline_termination() {
    let payload = b"first\nsecond\r\nthird_without_nl";
    let counter = ReadCounter::new();
    let counting = CountingReader::new(Cursor::new(&payload[..]), Some(&counter));
    let mut reader =
        BoundedLineReader::with_offset_and_line(BufReader::new(counting), 1024, 100, 5);

    assert_eq!(reader.bytes_consumed(), 100);
    assert_eq!(reader.line_number(), 5);
    assert!(reader.last_line_ended_with_newline());

    let l1 = reader.next_line().unwrap().unwrap();
    assert_eq!(
        l1,
        LineReadOutcome::Line {
            line_number: 6,
            text: "first"
        }
    );
    assert_eq!(reader.offset_before_last_line(), 100);
    assert_eq!(reader.bytes_consumed(), 106);
    assert!(reader.last_line_ended_with_newline());

    let l2 = reader.next_line().unwrap().unwrap();
    assert_eq!(
        l2,
        LineReadOutcome::Line {
            line_number: 7,
            text: "second"
        }
    );
    assert_eq!(reader.offset_before_last_line(), 106);
    assert_eq!(reader.bytes_consumed(), 114);
    assert!(reader.last_line_ended_with_newline());

    let l3 = reader.next_line().unwrap().unwrap();
    assert_eq!(
        l3,
        LineReadOutcome::Line {
            line_number: 8,
            text: "third_without_nl"
        }
    );
    assert_eq!(reader.offset_before_last_line(), 114);
    assert_eq!(reader.bytes_consumed(), 130);
    assert!(!reader.last_line_ended_with_newline());

    assert!(reader.next_line().unwrap().is_none());
    assert_eq!(counter.bytes_read(), payload.len() as u64);
}

#[test]
fn prefilter_classifies_turn_vs_metadata_lines_accurately() {
    assert_eq!(
        classify_claude_line(r#"{"type":"file-history-snapshot","snapshot":{"files":[]}}"#),
        ClaudeLineClass::NonTurnFastPath
    );
    assert_eq!(
        classify_claude_line(r#"{"type": "assistant", "message": {"usage": {}}}"#),
        ClaudeLineClass::CandidateTurnOrSystem
    );
    assert_eq!(
        classify_claude_line(r#"{"type":"user","message":{"content":"hi"}}"#),
        ClaudeLineClass::CandidateTurnOrSystem
    );
    assert_eq!(
        classify_claude_line(r#"{"type":"system","subtype":"compact_boundary"}"#),
        ClaudeLineClass::CandidateTurnOrSystem
    );

    assert_eq!(
        classify_codex_line(
            r#"{"type":"event_msg","payload":{"type":"agent_message_delta","delta":"hello"}}"#
        ),
        CodexLineClass::NonTurnFastPath
    );
    assert_eq!(
        classify_codex_line(r#"{"type":"world_state","payload":{"blob":"huge"}}"#),
        CodexLineClass::NonTurnFastPath
    );
    assert_eq!(
        classify_codex_line(
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"output_tokens":2}}}}"#
        ),
        CodexLineClass::CandidateTurnOrState
    );
    assert_eq!(
        classify_codex_line(r#"{"type":"token_usage_record","payload":{}}"#),
        CodexLineClass::CandidateTurnOrState
    );

    assert_eq!(
        classify_aider_analytics_line(
            r#"{"event":"command_exit","properties":{"version":"0.83.0"}}"#
        ),
        AiderAnalyticsClass::NonTurnFastPath
    );
    assert_eq!(
        classify_aider_analytics_line(
            r#"{"event":"message_send","properties":{"prompt_tokens":100}}"#
        ),
        AiderAnalyticsClass::CandidateSessionOrTurn
    );
}

#[test]
fn claude_incremental_resume_matches_full_parse_and_reads_only_appended_bytes() {
    let tmp = TempDir::new().unwrap();
    let log_path = tmp.path().join("claude-session.jsonl");
    let opts = ClaudeParseOptions::default();

    let canary_prompt = "KAKEIBO_T12_CANARY_CLAUDE_PROMPT_7711";
    let canary_tool_arg = "KAKEIBO_T12_CANARY_CLAUDE_ARG_8822";
    let canary_tool_out = "KAKEIBO_T12_CANARY_CLAUDE_OUT_9933";
    let raw_cwd = "/workspace/secret-t12-claude-repo";

    let chunk1 = format!(
        concat!(
            r#"{{"type":"file-history-snapshot","version":"2.1.314","snapshot":{{"tracked":12}}}}"#,
            "\n",
            r#"{{"type":"user","uuid":"u-1","sessionId":"sess-c1","cwd":"{cwd}","message":{{"role":"user","content":"{prompt}"}}}}"#,
            "\n",
            r#"{{"type":"assistant","uuid":"a-1a","sessionId":"sess-c1","requestId":"req-1","timestamp":"2026-10-31T10:00:01Z","cwd":"{cwd}","message":{{"id":"msg-1","model":"claude-sonnet-4-5","content":[{{"type":"text","text":"thinking..."}}],"usage":{{"input_tokens":500,"cache_creation_input_tokens":2000,"cache_read_input_tokens":0,"output_tokens":15}}}}}}"#,
            "\n"
        ),
        cwd = raw_cwd,
        prompt = canary_prompt,
    );

    fs::write(&log_path, chunk1.as_bytes()).unwrap();

    let counter = ReadCounter::new();
    let step1 = parse_claude_file_incremental(&log_path, &opts, 0, None, Some(&counter)).unwrap();
    assert_eq!(counter.files_opened(), 1);
    assert_eq!(counter.bytes_read(), chunk1.len() as u64);
    assert_eq!(step1.committed_offset, chunk1.len() as u64);
    assert_eq!(step1.output, parse_claude_file(&log_path).unwrap());

    let cp1 = step1
        .checkpoint_json
        .as_deref()
        .expect("checkpoint must exist");
    assert!(!cp1.contains(canary_prompt));
    assert!(!cp1.contains(raw_cwd));

    // Append a SECOND streaming chunk for the SAME `(msg-1, req-1)` with higher `output_tokens` + `tool_use`,
    // plus a partial non-`\n`-terminated trailing line!
    let chunk2_complete = format!(
        r#"{{"type":"assistant","uuid":"a-1b","sessionId":"sess-c1","requestId":"req-1","timestamp":"2026-10-31T10:00:02Z","cwd":"{cwd}","message":{{"id":"msg-1","model":"claude-sonnet-4-5","content":[{{"type":"text","text":"thinking..."}},{{"type":"tool_use","id":"toolu_01","name":"Read","input":{{"file_path":"{arg}"}}}}],"usage":{{"input_tokens":500,"cache_creation_input_tokens":2000,"cache_read_input_tokens":0,"output_tokens":140}}}}}}"#,
        cwd = raw_cwd,
        arg = canary_tool_arg,
    ) + "\n";
    let partial_user_prefix = format!(
        r#"{{"type":"user","uuid":"u-2","sessionId":"sess-c1","cwd":"{cwd}","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_01","content":"{out}"}}"#,
        cwd = raw_cwd,
        out = canary_tool_out,
    ); // intentionally missing `]}}` and `\n`!

    {
        let mut f = OpenOptions::new().append(true).open(&log_path).unwrap();
        f.write_all(chunk2_complete.as_bytes()).unwrap();
        f.write_all(partial_user_prefix.as_bytes()).unwrap();
    }

    counter.reset();
    let step2 = parse_claude_file_incremental(
        &log_path,
        &opts,
        step1.committed_offset,
        step1.checkpoint_json.as_deref(),
        Some(&counter),
    )
    .unwrap();
    assert_eq!(counter.files_opened(), 1);
    assert_eq!(
        counter.bytes_read(),
        (chunk2_complete.len() + partial_user_prefix.len()) as u64,
        "append must read ONLY newly appended bytes"
    );
    // Committed offset must stop right after `chunk2_complete`'s `\n`, before `partial_user_prefix`!
    assert_eq!(
        step2.committed_offset,
        (chunk1.len() + chunk2_complete.len()) as u64
    );
    assert_eq!(step2.output, parse_claude_file(&log_path).unwrap());

    // Now finish the partial user `tool_result` line and append Turn 2 assistant response!
    let partial_user_suffix = "]}}\n";
    let chunk3_turn2 = format!(
        r#"{{"type":"assistant","uuid":"a-2","sessionId":"sess-c1","requestId":"req-2","timestamp":"2026-10-31T10:00:05Z","cwd":"{cwd}","message":{{"id":"msg-2","model":"claude-sonnet-4-5","content":[{{"type":"text","text":"done"}}],"usage":{{"input_tokens":120,"cache_creation_input_tokens":300,"cache_read_input_tokens":2500,"output_tokens":45}}}}}}"#,
        cwd = raw_cwd,
    ) + "\n";

    {
        let mut f = OpenOptions::new().append(true).open(&log_path).unwrap();
        f.write_all(partial_user_suffix.as_bytes()).unwrap();
        f.write_all(chunk3_turn2.as_bytes()).unwrap();
    }

    counter.reset();
    let step3 = parse_claude_file_incremental(
        &log_path,
        &opts,
        step2.committed_offset,
        step2.checkpoint_json.as_deref(),
        Some(&counter),
    )
    .unwrap();
    assert_eq!(
        counter.bytes_read(),
        (partial_user_prefix.len() + partial_user_suffix.len() + chunk3_turn2.len()) as u64
    );
    assert_eq!(step3.output, parse_claude_file(&log_path).unwrap());
    assert_eq!(step3.output.records.len(), 2);
    assert_eq!(step3.output.records[0].usage.output, 140);
    // Verify `tool_result` in Turn 2 resolved `"Read"` from `tool_registry` stored in the checkpoint!
    assert_eq!(
        step3.output.records[1].added[0].tool.as_deref(),
        Some("Read")
    );

    let cp3 = step3.checkpoint_json.as_deref().unwrap();
    for secret in [canary_prompt, canary_tool_arg, canary_tool_out, raw_cwd] {
        assert!(
            !cp3.contains(secret),
            "checkpoint leaked sensitive string `{secret}`"
        );
    }

    // Verify skipped non-turn malformed JSON still emits MalformedJson diagnostic.
    let bad_meta = "{\"type\":\"file-history-snapshot\",\"broken\":\n";
    {
        let mut f = OpenOptions::new().append(true).open(&log_path).unwrap();
        f.write_all(bad_meta.as_bytes()).unwrap();
    }
    let step4 = parse_claude_file_incremental(
        &log_path,
        &opts,
        step3.committed_offset,
        step3.checkpoint_json.as_deref(),
        None,
    )
    .unwrap();
    assert_eq!(step4.output, parse_claude_file(&log_path).unwrap());
    assert!(step4
        .output
        .diagnostics
        .issues
        .iter()
        .any(|d| d.kind == ParseDiagnosticKind::MalformedJson));
}

#[test]
fn codex_incremental_resume_handles_split_tc_tur_pairs_and_reads_only_new_bytes() {
    let tmp = TempDir::new().unwrap();
    let log_path = tmp.path().join("rollout-codex.jsonl");

    let canary_prompt = "KAKEIBO_T12_CANARY_CODEX_PROMPT_1122";
    let canary_cmd = "KAKEIBO_T12_CANARY_CODEX_CMD_3344";
    let raw_cwd = "/workspace/secret-t12-codex-repo";

    // Chunk 1 ends right after Turn 1 `tur` (before Turn 1's trailing `tc` with `rate_limits`!).
    let chunk1 = format!(
        concat!(
            r#"{{"timestamp":"2026-10-31T11:00:00Z","type":"session_meta","payload":{{"id":"codex-s1","cwd":"{cwd}","cli_version":"0.165.0"}}}}"#,
            "\n",
            r#"{{"timestamp":"2026-10-31T11:00:00Z","type":"turn_context","payload":{{"model":"gpt-5-codex","cwd":"{cwd}"}}}}"#,
            "\n",
            r#"{{"timestamp":"2026-10-31T11:00:01Z","type":"response_item","payload":{{"id":"item-1","type":"message","role":"user","content":"{prompt}"}}}}"#,
            "\n",
            r#"{{"timestamp":"2026-10-31T11:00:01Z","type":"event_msg","payload":{{"type":"agent_message_delta","delta":"{prompt}"}}}}"#,
            "\n",
            r#"{{"timestamp":"2026-10-31T11:00:02Z","type":"response_item","payload":{{"id":"item-2","type":"function_call","call_id":"call-1","name":"shell","arguments":"{{\"cmd\":\"{cmd}\"}}"}}}}"#,
            "\n",
            r#"{{"timestamp":"2026-10-31T11:00:02Z","type":"token_usage_record","payload":{{"response_id":"resp-1","session_id":"codex-s1","usage":{{"input_tokens":4000,"cached_input_tokens":1000,"output_tokens":120,"reasoning_output_tokens":40}}}}}}"#,
            "\n"
        ),
        cwd = raw_cwd,
        prompt = canary_prompt,
        cmd = canary_cmd,
    );
    fs::write(&log_path, chunk1.as_bytes()).unwrap();

    let counter = ReadCounter::new();
    let step1 = parse_codex_file_incremental(&log_path, 0, None, Some(&counter)).unwrap();
    assert_eq!(counter.bytes_read(), chunk1.len() as u64);
    assert_eq!(step1.output, parse_codex_file(&log_path).unwrap());

    // Chunk 2 appends Turn 1's trailing `tc` (`rate_limits`), Turn 2 `function_call_output`,
    // AND Turn 2's leading `tc` (BEFORE Turn 2's `tur` arrives — testing `pending_tc_fallback`!).
    let chunk2 = concat!(
        r#"{"timestamp":"2026-10-31T11:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":272000,"last_token_usage":{"input_tokens":4000,"cached_input_tokens":1000,"output_tokens":120,"reasoning_output_tokens":40}},"rate_limits":{"primary":{"used_percent":12.5}}}}"#,
        "\n",
        r#"{"timestamp":"2026-10-31T11:00:03Z","type":"response_item","payload":{"id":"item-3","type":"function_call_output","call_id":"call-1","output":"ok"}}"#,
        "\n",
        r#"{"timestamp":"2026-10-31T11:00:04Z","type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":272000,"last_token_usage":{"input_tokens":4500,"cached_input_tokens":3900,"output_tokens":80,"reasoning_output_tokens":20}},"rate_limits":{"primary":{"used_percent":14.0}}}}"#,
        "\n"
    );
    {
        let mut f = OpenOptions::new().append(true).open(&log_path).unwrap();
        f.write_all(chunk2.as_bytes()).unwrap();
    }

    counter.reset();
    let step2 = parse_codex_file_incremental(
        &log_path,
        step1.committed_offset,
        step1.checkpoint_json.as_deref(),
        Some(&counter),
    )
    .unwrap();
    assert_eq!(counter.bytes_read(), chunk2.len() as u64);
    assert_eq!(
        step2.output,
        parse_codex_file(&log_path).unwrap(),
        "intermediate EOF with pending_tc_fallback must match full single-pass parse"
    );
    assert_eq!(step2.output.records.len(), 2);
    assert_eq!(
        step2.output.records[0].hints.rate_limit_used_percent,
        Some(12.5)
    );

    // Chunk 3 appends Turn 2's paired `token_usage_record` — must NOT double-count Turn 2!
    let chunk3 = concat!(
        r#"{"timestamp":"2026-10-31T11:00:04Z","type":"token_usage_record","payload":{"response_id":"resp-2","session_id":"codex-s1","usage":{"input_tokens":4500,"cached_input_tokens":3900,"output_tokens":85,"reasoning_output_tokens":20}}}"#,
        "\n"
    );
    {
        let mut f = OpenOptions::new().append(true).open(&log_path).unwrap();
        f.write_all(chunk3.as_bytes()).unwrap();
    }

    counter.reset();
    let step3 = parse_codex_file_incremental(
        &log_path,
        step2.committed_offset,
        step2.checkpoint_json.as_deref(),
        Some(&counter),
    )
    .unwrap();
    assert_eq!(counter.bytes_read(), chunk3.len() as u64);
    assert_eq!(step3.output, parse_codex_file(&log_path).unwrap());
    assert_eq!(step3.output.records.len(), 2);
    assert_eq!(step3.output.records[1].usage.output, 85);
    assert_eq!(
        step3.output.records[1].hints.rate_limit_used_percent,
        Some(14.0)
    );

    let cp3 = step3.checkpoint_json.as_deref().unwrap();
    for secret in [canary_prompt, canary_cmd, raw_cwd] {
        assert!(!cp3.contains(secret), "Codex checkpoint leaked `{secret}`");
    }
}

#[test]
fn gemini_incremental_resume_handles_patch_and_rewind_and_reads_only_new_bytes() {
    let tmp = TempDir::new().unwrap();
    let log_path = tmp.path().join("session-gemini.jsonl");
    let opts = GeminiParseOptions::default();

    let canary_prompt = "KAKEIBO_T12_CANARY_GEMINI_PROMPT_5566";
    let raw_dir = "/workspace/secret-t12-gemini-repo";

    let chunk1 = format!(
        concat!(
            r#"{{"sessionId":"gem-s1","startTime":"2026-10-31T12:00:00Z","directories":["{dir}"]}}"#,
            "\n",
            r#"{{"id":"m1","type":"user","timestamp":"2026-10-31T12:00:01Z","content":"{prompt}"}}"#,
            "\n",
            r#"{{"id":"m2","type":"gemini","timestamp":"2026-10-31T12:00:02Z","model":"gemini-2.5-pro","content":"reply 1","tokens":{{"input":1200,"output":80,"cached":200,"thoughts":20,"tool":0}}}}"#,
            "\n",
            r#"{{"id":"m3","type":"user","timestamp":"2026-10-31T12:00:03Z","content":"abandoned branch"}}"#,
            "\n",
            r#"{{"id":"m4","type":"gemini","timestamp":"2026-10-31T12:00:04Z","model":"gemini-2.5-pro","content":"abandoned reply","tokens":{{"input":1500,"output":90,"cached":1100,"thoughts":0,"tool":0}}}}"#,
            "\n"
        ),
        dir = raw_dir,
        prompt = canary_prompt,
    );
    fs::write(&log_path, chunk1.as_bytes()).unwrap();

    let counter = ReadCounter::new();
    let step1 = parse_gemini_file_incremental(&log_path, &opts, 0, None, Some(&counter)).unwrap();
    assert_eq!(counter.bytes_read(), chunk1.len() as u64);
    assert_eq!(
        step1.output,
        parse_gemini_file_with_options(&log_path, &opts).unwrap()
    );
    assert_eq!(step1.output.records.len(), 2);

    // Append `$rewindTo: "m3"` (dropping m3 and m4!), `$patch` updating m2's output tokens, and a new m5/m6 turn!
    let chunk2 = concat!(
        r#"{"$rewindTo":"m3"}"#,
        "\n",
        r#"{"id":"m2","$patch":[{"op":"replace","path":"/tokens/output","value":95}]}"#,
        "\n",
        r#"{"id":"m5","type":"user","timestamp":"2026-10-31T12:00:10Z","content":"new branch"}"#,
        "\n",
        r#"{"id":"m6","type":"gemini","timestamp":"2026-10-31T12:00:12Z","model":"gemini-2.5-pro","content":"new reply","tokens":{"input":1400,"output":110,"cached":1150,"thoughts":10,"tool":0}}"#,
        "\n"
    );
    {
        let mut f = OpenOptions::new().append(true).open(&log_path).unwrap();
        f.write_all(chunk2.as_bytes()).unwrap();
    }

    counter.reset();
    let step2 = parse_gemini_file_incremental(
        &log_path,
        &opts,
        step1.committed_offset,
        step1.checkpoint_json.as_deref(),
        Some(&counter),
    )
    .unwrap();
    assert_eq!(counter.bytes_read(), chunk2.len() as u64);
    assert_eq!(
        step2.output,
        parse_gemini_file_with_options(&log_path, &opts).unwrap()
    );
    assert_eq!(step2.output.records.len(), 2);
    assert_eq!(step2.output.records[0].usage.output, 115); // 95 + 20 thoughts

    let cp2 = step2.checkpoint_json.as_deref().unwrap();
    assert!(!cp2.contains(canary_prompt));
    assert!(!cp2.contains(raw_dir));
}

#[test]
fn aider_history_and_analytics_incremental_resume_matches_full_parse_and_protects_privacy() {
    let tmp = TempDir::new().unwrap();

    // 1. Aider `.aider.chat.history.md`
    let hist_path = tmp.path().join(".aider.chat.history.md");
    let canary_md = "KAKEIBO_T12_CANARY_AIDER_MD_4455";
    let h_chunk1 = format!(
        "# aider chat started at 2026-10-31 13:00:00\n\
         > Aider v0.83.0\n\
         > Main model: claude-sonnet-4-5 with diff edit format\n\
         #### {canary_md}\n\
         Sure, updated the module.\n\
         > Tokens: 12k sent, 2.0k cache write, 8.0k cache hit, 450 received. Cost: $0.02 message, $0.02 session.\n"
    );
    fs::write(&hist_path, h_chunk1.as_bytes()).unwrap();

    let counter = ReadCounter::new();
    let h_step1 =
        parse_aider_history_file_incremental(&hist_path, 0, None, Some(&counter)).unwrap();
    assert_eq!(counter.bytes_read(), h_chunk1.len() as u64);
    assert_eq!(
        h_step1.output,
        parse_aider_history_file(&hist_path).unwrap()
    );

    let h_chunk2 = "#### follow-up question\n\
                    All tests pass.\n\
                    > Tokens: 14k sent, 10k cache hit, 320 received.\n";
    {
        let mut f = OpenOptions::new().append(true).open(&hist_path).unwrap();
        f.write_all(h_chunk2.as_bytes()).unwrap();
    }
    counter.reset();
    let h_step2 = parse_aider_history_file_incremental(
        &hist_path,
        h_step1.committed_offset,
        h_step1.checkpoint_json.as_deref(),
        Some(&counter),
    )
    .unwrap();
    assert_eq!(counter.bytes_read(), h_chunk2.len() as u64);
    assert_eq!(
        h_step2.output,
        parse_aider_history_file(&hist_path).unwrap()
    );
    assert!(!h_step2
        .checkpoint_json
        .as_deref()
        .unwrap()
        .contains(canary_md));

    // 2. Aider `--analytics-log` JSONL (including `"launched"` alone at EOF first!)
    let ana_path = tmp.path().join("aider-analytics.jsonl");
    let raw_uid = "user-secret-uuid-9999-aaaa";
    let a_chunk0 = format!(
        r#"{{"event":"launched","properties":{{"version":"0.83.0"}},"time":1793458800,"user_id":"{raw_uid}"}}"#
    ) + "\n";
    fs::write(&ana_path, a_chunk0.as_bytes()).unwrap();

    let a_step0 = parse_aider_analytics_file_incremental(&ana_path, 0, None, None).unwrap();
    assert_eq!(
        a_step0.output,
        parse_aider_analytics_file(&ana_path).unwrap()
    );
    assert!(
        !a_step0
            .checkpoint_json
            .as_deref()
            .unwrap_or("")
            .contains(raw_uid),
        "analytics checkpoint must never persist raw user_id"
    );

    let a_chunk1 = format!(
        r#"{{"event":"message_send","properties":{{"main_model":"gpt-4o","prompt_tokens":4500,"completion_tokens":210}},"time":"2026-10-31T15:00:05Z","user_id":"{raw_uid}"}}"#
    ) + "\n";
    {
        let mut f = OpenOptions::new().append(true).open(&ana_path).unwrap();
        f.write_all(a_chunk1.as_bytes()).unwrap();
    }
    let a_step1 = parse_aider_analytics_file_incremental(
        &ana_path,
        a_step0.committed_offset,
        a_step0.checkpoint_json.as_deref(),
        None,
    )
    .unwrap();
    assert_eq!(
        a_step1.output,
        parse_aider_analytics_file(&ana_path).unwrap()
    );
    assert!(!a_step1
        .checkpoint_json
        .as_deref()
        .unwrap()
        .contains(raw_uid));

    let a_chunk2 = format!(
        r#"{{"event":"message_send","properties":{{"main_model":"gpt-4o","prompt_tokens":5200,"completion_tokens":180}},"time":"2026-10-31T15:00:15Z","user_id":"{raw_uid}"}}"#
    ) + "\n";
    {
        let mut f = OpenOptions::new().append(true).open(&ana_path).unwrap();
        f.write_all(a_chunk2.as_bytes()).unwrap();
    }
    counter.reset();
    let a_step2 = parse_aider_analytics_file_incremental(
        &ana_path,
        a_step1.committed_offset,
        a_step1.checkpoint_json.as_deref(),
        Some(&counter),
    )
    .unwrap();
    assert_eq!(counter.bytes_read(), a_chunk2.len() as u64);
    assert_eq!(
        a_step2.output,
        parse_aider_analytics_file(&ana_path).unwrap()
    );
    assert!(!a_step2
        .checkpoint_json
        .as_deref()
        .unwrap()
        .contains(raw_uid));
}

#[test]
fn sparse_zero_hole_after_truncate_falls_back_to_full_reparse() {
    let tmp = TempDir::new().unwrap();
    let log_path = tmp.path().join("sparse-claude.jsonl");
    let opts = ClaudeParseOptions::default();

    let initial = concat!(
        r#"{"type":"user","uuid":"u-1","sessionId":"s-1","message":{"role":"user","content":"hello"}}"#,
        "\n",
        r#"{"type":"assistant","uuid":"a-1","sessionId":"s-1","requestId":"r-1","timestamp":"2026-10-31T10:00:01Z","message":{"id":"m-1","model":"claude-sonnet-4-5","content":[{"type":"text","text":"world"}],"usage":{"input_tokens":100,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":20}}}"#,
        "\n"
    );
    fs::write(&log_path, initial.as_bytes()).unwrap();
    let step1 = parse_claude_file_incremental(&log_path, &opts, 0, None, None).unwrap();

    // Simulate POSIX sparse-hole write at offset > initial.len() after ftruncate(0).
    let replacement_line = concat!(
        r#"{"type":"assistant","uuid":"a-new","sessionId":"s-2","requestId":"r-new","timestamp":"2026-10-31T11:00:00Z","message":{"id":"m-new","model":"claude-sonnet-4-5","content":[{"type":"text","text":"after sparse truncate"}],"usage":{"input_tokens":333,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":44}}}"#,
        "\n"
    );
    {
        let f = OpenOptions::new().write(true).open(&log_path).unwrap();
        f.set_len(0).unwrap();
        let mut f = f;
        f.seek(SeekFrom::Start(step1.committed_offset + 16))
            .unwrap();
        f.write_all(replacement_line.as_bytes()).unwrap();
    }

    let resumed = parse_claude_file_incremental(
        &log_path,
        &opts,
        step1.committed_offset,
        step1.checkpoint_json.as_deref(),
        None,
    )
    .unwrap();
    let full = parse_claude_file(&log_path).unwrap();
    assert_eq!(resumed.output, full);
}
