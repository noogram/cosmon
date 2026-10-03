// SPDX-License-Identifier: AGPL-3.0-only

//! Direct-arm usage reaches the event ledger as canonical, cumulative,
//! deduplicated `UsageObserved` records, and an absent counter stays unknown.
//!
//! The first test drives a real `cs tackle` against a loopback provider and
//! reads the ledger back, so the whole chain is under test: wire response,
//! sample, cumulative history, durable record. The rest pin the accounting
//! rules at the recorder: replay safety, gaps, malformed values, model
//! separation and pricing coverage.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use cosmon_agent_harness::spine::{ReportedCount, ReportedUsage, UsageSample};
use cosmon_cli::harness_usage::{HarnessUsageRecorder, RecordOutcome};
use cosmon_core::event_v2::EventV2;
use cosmon_core::id::WorkerId;
use cosmon_core::usage::{
    ApiEquivalent, Availability, PricingCoverage, TokenCount, UnavailableReason, UsageHistory,
    UsageRecord,
};
use cosmon_observability::usage_projection::{deduplicate_histories, summarize_api_equivalent};

fn measured(n: u64) -> ReportedCount {
    ReportedCount::Measured(n)
}

fn sample(id: Option<&str>, served: Option<&str>, usage: ReportedUsage) -> UsageSample {
    UsageSample {
        response_id: id.map(str::to_owned),
        provider: "openai",
        requested_model: "requested-model".to_owned(),
        served_model: served.map(str::to_owned),
        usage,
    }
}

fn full_usage(input: u64, cached: u64, output: u64) -> ReportedUsage {
    ReportedUsage {
        input_tokens: measured(input),
        cached_input_tokens: measured(cached),
        cache_write_tokens: measured(0),
        output_tokens: measured(output),
        reasoning_output_tokens: measured(0),
    }
}

fn recorder(dir: &Path) -> HarnessUsageRecorder {
    HarnessUsageRecorder::new(
        dir,
        WorkerId::new("worker-1").expect("worker id"),
        "harness/mol/worker-1/attempt-1",
        None,
    )
}

fn ledger(dir: &Path) -> Vec<UsageRecord> {
    let path = cosmon_state::event_log::resolve_events_log_path(dir);
    cosmon_state::event_log::read_all(path)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|env| match env.event {
            EventV2::UsageObserved { usage } => Some(*usage),
            _ => None,
        })
        .collect()
}

fn tokens(record: &UsageRecord) -> (TokenCount, TokenCount, TokenCount) {
    (
        record.tokens.input_tokens,
        record.tokens.cached_input_tokens,
        record.tokens.output_tokens,
    )
}

#[test]
fn known_samples_become_a_cumulative_measured_history() {
    let dir = tempfile::tempdir().expect("tempdir");
    let usage = recorder(dir.path());
    assert_eq!(
        usage.record_sample(&sample(
            Some("r1"),
            Some("served-a"),
            full_usage(100, 40, 10)
        )),
        RecordOutcome::Persisted
    );
    assert_eq!(
        usage.record_sample(&sample(Some("r2"), Some("served-a"), full_usage(50, 0, 5))),
        RecordOutcome::Persisted
    );

    let records = ledger(dir.path());
    assert_eq!(records.len(), 2, "one durable record per answered request");
    assert_eq!(
        tokens(&records[0]),
        (
            TokenCount::Measured { tokens: 100 },
            TokenCount::Measured { tokens: 40 },
            TokenCount::Measured { tokens: 10 }
        )
    );
    assert_eq!(
        tokens(&records[1]),
        (
            TokenCount::Measured { tokens: 150 },
            TokenCount::Measured { tokens: 40 },
            TokenCount::Measured { tokens: 15 }
        ),
        "the second record is the running total, not the second request's delta"
    );
    for record in &records {
        record.validate().expect("record conforms to the schema");
        assert_eq!(
            record.subject.history,
            UsageHistory::Known {
                id: "harness/mol/worker-1/attempt-1".into()
            }
        );
    }
}

#[test]
fn replaying_a_receipt_does_not_increase_the_totals() {
    let dir = tempfile::tempdir().expect("tempdir");
    let usage = recorder(dir.path());
    let once = sample(Some("r1"), Some("served-a"), full_usage(100, 0, 10));
    assert_eq!(usage.record_sample(&once), RecordOutcome::Persisted);
    assert_eq!(usage.record_sample(&once), RecordOutcome::Duplicate);
    assert_eq!(usage.record_sample(&once), RecordOutcome::Duplicate);

    let records = ledger(dir.path());
    assert_eq!(records.len(), 1, "a replay writes nothing");
    assert_eq!(
        usage.latest().expect("latest").tokens.input_tokens,
        TokenCount::Measured { tokens: 100 }
    );
    // The projection over the durable records agrees: one history, counted once.
    assert_eq!(deduplicate_histories(&records).len(), 1);
}

#[test]
fn a_response_without_usage_makes_the_category_unknown_not_zero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let usage = recorder(dir.path());
    usage.record_sample(&sample(
        Some("r1"),
        Some("served-a"),
        full_usage(100, 0, 10),
    ));
    usage.record_sample(&sample(
        Some("r2"),
        Some("served-a"),
        ReportedUsage::default(),
    ));
    usage.record_sample(&sample(Some("r3"), Some("served-a"), full_usage(7, 0, 3)));

    let last = usage.latest().expect("latest");
    assert_eq!(
        last.tokens.input_tokens,
        TokenCount::Unavailable {
            reason: UnavailableReason::NotObserved
        },
        "a total that missed one request is not a total"
    );
    assert_eq!(
        last.tokens.output_tokens,
        TokenCount::Unavailable {
            reason: UnavailableReason::NotObserved
        }
    );

    // The projection prefers the latest record of the history even though an
    // earlier one measured more, so a partial sum is not shown as complete.
    let records = ledger(dir.path());
    let kept = deduplicate_histories(&records);
    assert_eq!(kept.len(), 1);
    assert_eq!(
        kept[0].tokens.input_tokens,
        TokenCount::Unavailable {
            reason: UnavailableReason::NotObserved
        }
    );
}

#[test]
fn malformed_values_are_marked_malformed_and_still_validate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let usage = recorder(dir.path());
    let bad = ReportedUsage {
        input_tokens: ReportedCount::Malformed,
        // A cache count larger than the input it is a subset of.
        cached_input_tokens: measured(500),
        output_tokens: measured(4),
        ..full_usage(10, 0, 4)
    };
    assert_eq!(
        usage.record_sample(&sample(Some("r1"), Some("served-a"), bad)),
        RecordOutcome::Persisted
    );
    let record = usage.latest().expect("latest");
    record.validate().expect("record stays schema-valid");
    assert_eq!(
        record.tokens.input_tokens,
        TokenCount::Unavailable {
            reason: UnavailableReason::MalformedSource
        }
    );
    assert_eq!(
        record.tokens.output_tokens,
        TokenCount::Measured { tokens: 4 }
    );

    let contradicting = ReportedUsage {
        input_tokens: measured(10),
        cached_input_tokens: measured(500),
        ..full_usage(10, 0, 4)
    };
    let other = recorder(dir.path());
    other.record_sample(&sample(Some("x"), Some("served-a"), contradicting));
    assert_eq!(
        other.latest().expect("latest").tokens.cached_input_tokens,
        TokenCount::Unavailable {
            reason: UnavailableReason::MalformedSource
        },
        "a subset larger than its total is rejected, not published"
    );
}

#[test]
fn served_model_is_recorded_and_the_requested_one_is_never_substituted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let usage = recorder(dir.path());
    usage.record_sample(&sample(Some("r1"), Some("served-a"), full_usage(10, 0, 1)));
    let paired = usage.latest().expect("latest");
    let Availability::Available { value: segments } = &paired.tokens.model_segments else {
        panic!("every sample named its served model: {paired:?}");
    };
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].model, "served-a");
    assert!(
        !serde_json::to_string(&paired)
            .expect("serialize")
            .contains("requested-model"),
        "the requested name is a different fact and is not stored as the served one"
    );

    // A response that does not name its model leaves the model axis unknown.
    let unpaired_dir = tempfile::tempdir().expect("tempdir");
    let unpaired = recorder(unpaired_dir.path());
    unpaired.record_sample(&sample(Some("r1"), None, full_usage(10, 0, 1)));
    let record = unpaired.latest().expect("latest");
    assert!(
        matches!(
            record.tokens.model_segments,
            Availability::Unavailable { .. }
        ),
        "no served model, no segment: {record:?}"
    );
    assert!(
        !matches!(record.api_equivalent, ApiEquivalent::Estimated { .. }),
        "without a model there is nothing to price against"
    );
}

#[test]
fn a_gap_never_yields_a_complete_priced_total() {
    let dir = tempfile::tempdir().expect("tempdir");
    let usage = recorder(dir.path());
    usage.record_sample(&sample(
        Some("r1"),
        Some("unlisted-model"),
        full_usage(10, 0, 1),
    ));
    usage.record_sample(&sample(
        Some("r2"),
        Some("unlisted-model"),
        ReportedUsage::default(),
    ));
    let record = usage.latest().expect("latest");
    let summary = summarize_api_equivalent(std::slice::from_ref(&record));
    assert!(
        !summary.complete,
        "unknown pricing plus a counter gap cannot be a complete total: {summary:?}"
    );
    if let ApiEquivalent::Estimated { coverage, .. } = &record.api_equivalent {
        assert!(!matches!(coverage, PricingCoverage::Complete));
    }
}

fn cs(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(root)
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .args(args)
        .output()
        .expect("cs")
}

fn cs_ok(root: &Path, args: &[&str]) -> String {
    let out = cs(root, args);
    assert!(
        out.status.success(),
        "cs {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8")
}

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}");
}

fn read_request(stream: &mut std::net::TcpStream) {
    let mut request = Vec::new();
    let mut buf = [0; 4096];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        let count = stream.read(&mut buf).expect("request header");
        assert!(count > 0, "request closed before headers");
        request.extend_from_slice(&buf[..count]);
    }
    let header_end = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
    let length = headers
        .lines()
        .find_map(|l| {
            l.strip_prefix("content-length: ")?
                .trim()
                .parse::<usize>()
                .ok()
        })
        .unwrap_or(0);
    while request.len() - header_end < length {
        let count = stream.read(&mut buf).expect("request body");
        assert!(count > 0, "request closed before body");
        request.extend_from_slice(&buf[..count]);
    }
}

fn reply(mut stream: std::net::TcpStream, body: &serde_json::Value) {
    let body = body.to_string();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .expect("response");
}

/// Every `UsageObserved` record under the galaxy's state tree.
fn ledger_under(root: &Path) -> Vec<UsageRecord> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.file_name().is_some_and(|n| n == "events.jsonl") {
                out.push(path);
            }
        }
    }
    let mut logs = Vec::new();
    walk(&root.join(".cosmon"), &mut logs);
    let mut records = Vec::new();
    for log in logs {
        for env in cosmon_state::event_log::read_all(log).unwrap_or_default() {
            if let EventV2::UsageObserved { usage } = env.event {
                records.push(*usage);
            }
        }
    }
    records
}

#[test]
fn a_real_tackle_leaves_cumulative_usage_in_the_ledger() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    cs_ok(root, &["init", "--yes"]);
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "test@cosmon.test"]);
    git(root, &["config", "user.name", "cosmon-test"]);

    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
    let address = listener.local_addr().expect("address");
    let config = root.join(".cosmon/config.toml");
    let mut body = fs::read_to_string(&config).expect("config");
    body.push_str(&format!(
        "\n[adapters.anthropic]\nbase_url = \"http://{address}\"\ndefault_model = \"probe-model\"\n"
    ));
    fs::write(&config, body).expect("configured adapter");
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "init"]);

    let nucleated = cs_ok(
        root,
        &["--json", "nucleate", "task-work", "--var", "topic=probe"],
    );
    let parsed: serde_json::Value = serde_json::from_str(&nucleated).expect("nucleate json");
    let id = parsed["id"].as_str().expect("id").to_owned();

    let server = std::thread::spawn(move || {
        let (mut first, _) = listener.accept().expect("first request");
        read_request(&mut first);
        reply(
            first,
            &serde_json::json!({
                "id":"msg-1","model":"probe-model","type":"message","role":"assistant",
                "content":[{"type":"tool_use","id":"call_1","name":"exec_command",
                    "input":{"command":"true"}}],
                "stop_reason":"tool_use",
                "usage":{"input_tokens":10,"cache_read_input_tokens":4,
                         "cache_creation_input_tokens":0,"output_tokens":5}
            }),
        );
        let (mut second, _) = listener.accept().expect("second request");
        read_request(&mut second);
        reply(
            second,
            &serde_json::json!({
                "id":"msg-2","model":"probe-model","type":"message","role":"assistant",
                "content":[{"type":"text","text":"done"}],
                "stop_reason":"end_turn",
                "usage":{"input_tokens":20,"cache_read_input_tokens":0,
                         "cache_creation_input_tokens":0,"output_tokens":6}
            }),
        );
    });

    let tackle = Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(root)
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env("ANTHROPIC_API_KEY", "probe-key")
        .args(["tackle", &id, "--adapter", "anthropic"])
        .output()
        .expect("tackle");
    server.join().expect("provider server");
    let stderr = String::from_utf8_lossy(&tackle.stderr);

    let records = ledger_under(root);
    assert_eq!(
        records.len(),
        2,
        "one durable usage record per answered request; stderr: {stderr}"
    );
    assert_eq!(
        tokens(&records[0]),
        (
            TokenCount::Measured { tokens: 14 },
            TokenCount::Measured { tokens: 4 },
            TokenCount::Measured { tokens: 5 }
        ),
        "input includes the cache-read subset"
    );
    assert_eq!(
        tokens(&records[1]),
        (
            TokenCount::Measured { tokens: 34 },
            TokenCount::Measured { tokens: 4 },
            TokenCount::Measured { tokens: 11 }
        ),
        "cumulative across both requests"
    );
    assert_eq!(records[0].subject.history, records[1].subject.history);
    for record in &records {
        record.validate().expect("canonical record");
        let Availability::Available { value } = &record.tokens.model_segments else {
            panic!("served model was reported: {record:?}");
        };
        assert_eq!(value[0].model, "probe-model");
    }
}
