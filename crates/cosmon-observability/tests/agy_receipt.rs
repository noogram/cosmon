// SPDX-License-Identifier: AGPL-3.0-only

//! Contract tests for folding a provider receipt without files or a pane.

use cosmon_observability::agy::{parse_agy_receipt, AgyReceipt};

#[test]
fn structured_quota_refusals_require_an_error_result() {
    let result = r#"{"event":"result","result":{"status":"ERROR","error":"Resets in 19m45s"}}"#;
    for refusal in [
        r#"{"event":"error","error":{"code":429,"message":"Resets in 19m45s."}}"#,
        r#"{"event":"error","error":{"status":"RESOURCE_EXHAUSTED","message":"Resets in 19m45s."}}"#,
    ] {
        let receipt = parse_agy_receipt([refusal, result]);
        let stop = receipt.quota_stop.unwrap();
        assert_eq!(stop.reset_delay.as_deref(), Some("19m45s"));
        assert_eq!(stop.to_string(), "quota-exhausted (resets in 19m45s)");
        assert!(parse_agy_receipt([refusal]).quota_stop.is_none());
        assert!(parse_agy_receipt([
            refusal,
            r#"{"event":"result","result":{"status":"SUCCESS"}}"#
        ])
        .quota_stop
        .is_none());
    }
    let unexpected = r#"{"event":"error","error":{"code":500,"message":"Resets in 19m45s."}}"#;
    assert_eq!(
        parse_agy_receipt([unexpected, result]),
        AgyReceipt::default()
    );
    assert_eq!(parse_agy_receipt([result]), AgyReceipt::default());
}

#[test]
fn reset_delay_remains_unknown_if_the_provider_does_not_give_one() {
    let result = r#"{"event":"result","result":{"status":"ERROR"}}"#;
    for refusal in [
        r#"{"event":"error","error":{"code":429}}"#,
        r#"{"event":"error","error":{"code":429,"message":"Resets in soon."}}"#,
        r#"{"event":"error","error":{"code":429,"message":"Resets in ."}} "#,
        r#"{"event":"error","error":{"code":429,"message":"Resets in 123."}}"#,
        r#"{"event":"error","error":{"code":429,"message":"Resets in 1hm."}}"#,
    ] {
        let stop = parse_agy_receipt([refusal, result]).quota_stop.unwrap();
        assert_eq!(stop.reset_delay, None);
        assert_eq!(stop.to_string(), "quota-exhausted");
    }
}

#[test]
fn a_new_init_clears_a_previous_receipt_even_if_the_next_result_is_truncated() {
    let result = r#"{"event":"result","result":{"status":"ERROR","usage":{"input_tokens":5}}}"#;
    let error = r#"{"event":"error","error":{"code":429}}"#;
    let init = r#"{"event":"init"}"#;
    let torn = r#"{"event":"result","result":{"status":"#;
    let restarted = parse_agy_receipt([init, error, result, init, torn]);
    assert_eq!(restarted.init_count, 2);
    assert!(restarted.usage.is_none());
    assert!(restarted.quota_stop.is_none());
    assert!(parse_agy_receipt([error, init, result])
        .quota_stop
        .is_none());
    assert_eq!(
        parse_agy_receipt([error, result, torn]),
        parse_agy_receipt([error, result])
    );
}

#[test]
fn malformed_fields_do_not_erase_other_counters() {
    let receipt = parse_agy_receipt([
        "stderr from the launch",
        r#"{"event":"result","result":{"status":"SUCCESS","usage":{"input_tokens":0,"cache_read_tokens":"2","output_tokens":9,"thinking_tokens":-1}}}"#,
        r#"{"event":"result","result":null}"#,
    ]);
    assert_eq!(receipt.usage.unwrap().columns(|n| n.to_string()), "0/-/9/-");
    assert_eq!(
        parse_agy_receipt([r#"{"event":"result","result":{"usage":{"total_tokens":17}}}"#]),
        AgyReceipt::default()
    );
}

#[test]
fn the_documented_result_envelope_can_report_the_provider_quota_refusal() {
    let result = r#"{"event":"result","result":{"status":"ERROR","error":"Individual quota reached. Resets in 1h2m3s."}}"#;
    let stop = parse_agy_receipt([result]).quota_stop.unwrap();
    assert_eq!(stop.reset_delay.as_deref(), Some("1h2m3s"));

    let error = r#"{"event":"error","error":{"code":429}}"#;
    assert_eq!(parse_agy_receipt([error, result]).quota_stop, Some(stop));

    let success = r#"{"event":"result","result":{"status":"SUCCESS","error":"Individual quota reached. Resets in 1h2m3s."}}"#;
    assert!(parse_agy_receipt([success]).quota_stop.is_none());
}

#[test]
fn canonical_snapshot_displays_receipt_counters_and_quota_detail() {
    use cosmon_observability::fixture::canonical_snapshot;
    use cosmon_observability::render::{render_canonical, SnapshotConfig};

    let mut fleet = canonical_snapshot();
    let mut worker = fleet.workers().next().unwrap().clone();
    worker.energy.agy_receipt = Some(parse_agy_receipt([
        r#"{"event":"error","error":{"code":429,"message":"Resets in 19m45s."}}"#,
        r#"{"event":"result","result":{"status":"ERROR","usage":{"input_tokens":17,"cache_read_tokens":42,"output_tokens":8}}}"#,
    ]));
    fleet.insert_worker(worker);
    let snapshot = render_canonical(&fleet, &SnapshotConfig::default());
    assert!(snapshot.contains("17/42/8/-"), "{snapshot}");
    assert!(
        snapshot.contains("quota-exhausted (resets in 19m45s)"),
        "{snapshot}"
    );
}
