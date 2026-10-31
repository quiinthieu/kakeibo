//! Streaming NDJSON I/O tests and T02 ground-truth fixture round-trip tests
//! (`PLAN.md` §3.1, §4, §10 T03).

use std::fs;
use std::io::{self, BufReader, Cursor, Read};
use std::path::{Path, PathBuf};

use kakeibo_core::ledger::{decode_ndjson, LedgerError, LEDGER_SCHEMA};
use kakeibo_core::{
    AgentId, BlockKind, Confidence, ContentBlock, Timestamp, TurnHints, TurnRecord, Usage,
};
use kakeibo_report::ndjson::{read_ledger, write_ledger, write_record, LedgerReader, NdjsonError};
use proptest::prelude::*;
use serde::Deserialize;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root must resolve")
}

#[derive(Debug, Deserialize)]
struct ExpectedFixtureFile {
    agent: AgentId,
    scenario: String,
    sessions: Vec<ExpectedSession>,
}

#[derive(Debug, Deserialize)]
struct ExpectedSession {
    session_id: String,
    turns: Vec<ExpectedTurn>,
}

#[derive(Debug, Deserialize)]
struct ExpectedTurn {
    turn: u32,
    ts: Timestamp,
    model: Option<String>,
    prompt_tokens: u64,
    rate_limit_used_percent: Option<f64>,
    usage: Usage,
    added_blocks: Vec<ExpectedBlock>,
    hints: ExpectedHints,
}

#[derive(Debug, Deserialize)]
struct ExpectedBlock {
    kind: BlockKind,
    bytes: u64,
    tool: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExpectedHints {
    resume_start: bool,
    compaction: bool,
    sidechain: bool,
    api_error: bool,
}

fn arb_turn_record() -> impl Strategy<Value = TurnRecord> {
    (
        prop_oneof![
            Just(AgentId::ClaudeCode),
            Just(AgentId::Codex),
            Just(AgentId::GeminiCli),
            Just(AgentId::Opencode),
            Just(AgentId::Aider),
            Just(AgentId::Cursor),
        ],
        "[a-z0-9-]{4,24}",
        1_u32..500,
        946_684_800_i64..2_500_000_000_i64,
        0_u64..500_000,
        0_u64..500_000,
        0_u64..500_000,
        0_u64..100_000,
        proptest::option::of(0_u64..50_000),
        0_u64..20_000,
    )
        .prop_map(
            |(
                agent,
                session_id,
                turn,
                secs,
                fresh_in,
                cache_write,
                cache_read,
                output,
                reasoning,
                block_bytes,
            )| TurnRecord {
                agent,
                session_id,
                parent_session_id: None,
                turn,
                ts: Timestamp::from_unix_timestamp(secs).expect("valid unix seconds"),
                model: Some("synth-model-1".to_string()),
                cwd_hash: Some("0123456789abcdef".to_string()),
                usage: Usage {
                    fresh_in,
                    cache_write,
                    cache_read,
                    output,
                    reasoning,
                    confidence: Confidence::Measured,
                    estimation_method: None,
                    cache_write_split: None,
                    agent_reported_cost_usd: None,
                },
                added: vec![ContentBlock {
                    kind: BlockKind::UserPrompt,
                    est_tokens: block_bytes / 4,
                    bytes: block_bytes,
                    tool: None,
                    key_hash: None,
                    is_error: false,
                }],
                hints: TurnHints::default(),
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: None,
        .. ProptestConfig::default()
    })]

    #[test]
    fn streaming_ndjson_round_trip_matches_records(
        records in proptest::collection::vec(arb_turn_record(), 0..16),
    ) {
        let mut buf = Vec::new();
        let count = write_ledger(&mut buf, &records).expect("write_ledger must succeed");
        prop_assert_eq!(count, records.len());

        // Use a deliberately tiny BufReader capacity (37 bytes) so lines split across buffer refills.
        let small_reader = BufReader::with_capacity(37, Cursor::new(&buf));
        let streamed = read_ledger(small_reader).expect("streaming read_ledger must succeed");
        prop_assert_eq!(&streamed, &records);

        let as_str = std::str::from_utf8(&buf).expect("NDJSON output must be valid UTF-8");
        let pure_decoded = decode_ndjson(as_str).expect("pure decode_ndjson must match");
        prop_assert_eq!(pure_decoded, streamed);
    }
}

#[test]
fn streaming_reader_handles_crlf_blank_lines_and_unterminated_final_line() {
    let ts = Timestamp::parse_rfc3339("2026-10-31T09:00:04Z").unwrap();
    let rec1 = TurnRecord {
        agent: AgentId::ClaudeCode,
        session_id: "sess-1".to_string(),
        parent_session_id: None,
        turn: 1,
        ts,
        model: Some("claude-sonnet-4-5".to_string()),
        cwd_hash: None,
        usage: Usage {
            fresh_in: 100,
            cache_write: 400,
            cache_read: 0,
            output: 25,
            reasoning: None,
            confidence: Confidence::Measured,
            estimation_method: None,
            cache_write_split: None,
            agent_reported_cost_usd: None,
        },
        added: Vec::new(),
        hints: TurnHints::default(),
    };
    let mut rec2 = rec1.clone();
    rec2.turn = 2;

    let mut line1 = Vec::new();
    write_record(&mut line1, &rec1).unwrap();
    let mut line2 = Vec::new();
    write_record(&mut line2, &rec2).unwrap();
    // Strip trailing `\n` from line 2 and interleave CRLF + whitespace lines.
    let line1_trimmed = std::str::from_utf8(&line1).unwrap().trim_end();
    let line2_trimmed = std::str::from_utf8(&line2).unwrap().trim_end();
    let mixed = format!("  \r\n{line1_trimmed}\r\n\n\t\r\n{line2_trimmed}");

    let decoded = read_ledger(Cursor::new(mixed.as_bytes())).unwrap();
    assert_eq!(decoded, vec![rec1, rec2]);
}

#[test]
fn streaming_reader_reports_exact_line_numbers_and_io_errors() {
    let bad_stream = concat!(
        "\n",
        r#"{"schema":"kakeibo.ledger/1","agent":"codex","session_id":"s1","turn":1,"ts":"2026-10-31T10:00:03Z","usage":{"fresh_in":10,"cache_write":0,"cache_read":0,"output":5,"confidence":"measured"},"added":[],"hints":{}}"#,
        "\n",
        "\r\n",
        r#"{"schema":"kakeibo.ledger/99","agent":"codex"}"#,
        "\n"
    );

    let mut reader = LedgerReader::new(Cursor::new(bad_stream.as_bytes()));
    let first = reader
        .next()
        .expect("first item")
        .expect("line 2 valid record");
    assert_eq!(first.turn, 1);
    assert_eq!(reader.line_number(), 2);

    let second = reader.next().expect("second item").unwrap_err();
    assert_eq!(second.line(), Some(4));
    assert!(matches!(
        second,
        NdjsonError::Ledger(LedgerError::UnsupportedSchema {
            ref found,
            expected: LEDGER_SCHEMA,
            line: Some(4),
        }) if found == "kakeibo.ledger/99"
    ));

    struct FailingReader;
    impl Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "synthetic pipe failure",
            ))
        }
    }

    let err = read_ledger(BufReader::new(FailingReader)).unwrap_err();
    assert_eq!(err.line(), Some(1));
    assert!(matches!(err, NdjsonError::Io { line: Some(1), .. }));
}

#[test]
fn committed_t02_ground_truth_fixtures_round_trip_through_ndjson_ledger() {
    let root = workspace_root();
    let scenarios = [
        "tests/fixtures/claude-code/basic/expected.json",
        "tests/fixtures/claude-code/streaming-dupes/expected.json",
        "tests/fixtures/codex/basic/expected.json",
        "tests/fixtures/codex/codex-dual-usage/expected.json",
    ];

    let mut all_records = Vec::new();
    for rel in scenarios {
        let raw = fs::read_to_string(root.join(rel))
            .unwrap_or_else(|e| panic!("failed to read {rel}: {e}"));
        let expected: ExpectedFixtureFile = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("failed to deserialise {rel}: {e}"));

        for session in expected.sessions {
            for turn in session.turns {
                assert_eq!(
                    turn.usage.prompt_tokens(),
                    turn.prompt_tokens,
                    "Usage::prompt_tokens() must match ground truth in {}",
                    expected.scenario
                );
                all_records.push(TurnRecord {
                    agent: expected.agent,
                    session_id: session.session_id.clone(),
                    parent_session_id: None,
                    turn: turn.turn,
                    ts: turn.ts,
                    model: turn.model,
                    cwd_hash: None,
                    usage: turn.usage,
                    added: turn
                        .added_blocks
                        .into_iter()
                        .map(|b| ContentBlock {
                            kind: b.kind,
                            est_tokens: b.bytes.div_ceil(4),
                            bytes: b.bytes,
                            tool: b.tool,
                            key_hash: None,
                            is_error: false,
                        })
                        .collect(),
                    hints: TurnHints {
                        resume_start: turn.hints.resume_start,
                        compaction: turn.hints.compaction,
                        sidechain: turn.hints.sidechain,
                        api_error: turn.hints.api_error,
                        rate_limit_used_percent: turn.rate_limit_used_percent,
                        context_window_override: None,
                    },
                });
            }
        }
    }

    assert_eq!(all_records.len(), 9);

    let mut ndjson_bytes = Vec::new();
    let written = write_ledger(&mut ndjson_bytes, &all_records).unwrap();
    assert_eq!(written, 9);

    let decoded = read_ledger(Cursor::new(&ndjson_bytes)).unwrap();
    assert_eq!(decoded, all_records);

    let ndjson_text = String::from_utf8(ndjson_bytes).unwrap();
    insta::assert_snapshot!("t03_ground_truth_ledger_ndjson", ndjson_text);
}
