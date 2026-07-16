// SPDX-License-Identifier: AGPL-3.0-only

//! §8j HTTPS+JWT clause-by-clause admission tests.
//!
//! Each test exercises one of the five clauses end-to-end:
//!
//! 1. Operator-only verb refusal (clause a, §5).
//! 2. Unknown sub (clause a).
//! 3. JWT alg whitelist (clause a — RS256/ES256 only).
//! 4. Cross-tenant audience pivot (clause a).
//! 5. Rate limiter exhaustion (clause c).
//! 6. Deny-list `jti` revocation.
//! 7. Causal closure — inbox file exists *before* the subprocess runs.

use std::time::Duration;

use cosmon_rpp_adapter::admission::{
    http_request_to_spark, AdmissionRig, Verb, OPERATOR_ONLY_VERBS,
};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::error::RppRejectReason;
use cosmon_rpp_adapter::jwt::ValidatedJwt;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use tempfile::TempDir;

fn jwt(sub: &str, jti: &str, aud: &str) -> ValidatedJwt {
    ValidatedJwt {
        iss: "https://idp".into(),
        sub: sub.into(),
        aud: aud.into(),
        jti: jti.into(),
        lifetime_sec: 60,
        exp: 9_999_999_999,
        scopes: Vec::new(),
    }
}

fn rig_with(td: &TempDir) -> (HabilitationMap, IngressRateLimiter, DenyList) {
    let map = HabilitationMap::builder()
        .insert(
            "https://idp",
            "sub-1",
            HabilitationId::new("nuc-a"),
            Noyau::new("tenant-demo"),
            "cosmon-rpp-tenant",
        )
        .build();
    let lim = IngressRateLimiter::new(td.path().join("rl"), 5.0, 0.0);
    let dl = DenyList::new(td.path().to_path_buf()).with_ttl(Duration::ZERO);
    (map, lim, dl)
}

#[test]
fn operator_only_list_is_closed() {
    // ADR-080 §5.2 — closed list. If a verb is added (or removed)
    // here without a successor ADR, this test should remind the
    // contributor. `run` left the list 2026-06-11 via ADR-124
    // (bounded drain, task-20260610-56c4).
    assert_eq!(
        OPERATOR_ONLY_VERBS,
        &[
            "done",
            "evolve",
            "complete",
            "security",
            "kill",
            "purge",
            "reconcile",
            "verify",
            "whisper",
            "drop",
        ],
    );
}

#[test]
fn admits_observer_with_valid_token() {
    let td = TempDir::new().unwrap();
    let (map, lim, dl) = rig_with(&td);
    let inbox = td.path().join("whispers/inbox");
    let rig = AdmissionRig {
        nucleon_map: &map,
        rate_limiter: &lim,
        deny_list: &dl,
        inbox_root: &inbox,
        now_ms: 1_000,
    };
    let spark = http_request_to_spark(
        &rig,
        &jwt("sub-1", "tok-1", "cosmon-rpp-tenant"),
        Verb::ObserveMolecule,
        Some("mol-1"),
    )
    .unwrap();
    assert_eq!(spark.nucleon_id, "nuc-a");
    assert!(spark.inbox_path.exists());
    // The audit envelope is under <inbox>/api/<request_id>.json.
    assert!(spark.inbox_path.starts_with(inbox.join("api")));
}

#[test]
fn unknown_sub_rejects_as_unknownsub() {
    let td = TempDir::new().unwrap();
    let (map, lim, dl) = rig_with(&td);
    let rig = AdmissionRig {
        nucleon_map: &map,
        rate_limiter: &lim,
        deny_list: &dl,
        inbox_root: &td.path().join("inbox"),
        now_ms: 0,
    };
    let err = http_request_to_spark(
        &rig,
        &jwt("nope", "tok-x", "cosmon-rpp-tenant"),
        Verb::ObserveMolecule,
        Some("mol-1"),
    )
    .unwrap_err();
    assert!(matches!(err, RppRejectReason::UnknownSub));
}

#[test]
fn cross_tenant_audience_yields_pivot() {
    let td = TempDir::new().unwrap();
    let (map, lim, dl) = rig_with(&td);
    let rig = AdmissionRig {
        nucleon_map: &map,
        rate_limiter: &lim,
        deny_list: &dl,
        inbox_root: &td.path().join("inbox"),
        now_ms: 0,
    };
    let err = http_request_to_spark(
        &rig,
        &jwt("sub-1", "tok-x", "cosmon-rpp-other"),
        Verb::ObserveMolecule,
        Some("mol-1"),
    )
    .unwrap_err();
    assert!(matches!(err, RppRejectReason::CrossTenantPivot { .. }));
}

#[test]
fn rate_limit_admits_then_rejects() {
    let td = TempDir::new().unwrap();
    let map = HabilitationMap::builder()
        .insert(
            "https://idp",
            "sub-1",
            HabilitationId::new("nuc-a"),
            Noyau::new("tenant-demo"),
            "cosmon-rpp-tenant",
        )
        .build();
    let lim = IngressRateLimiter::new(td.path().join("rl"), 1.0, 0.0); // capacity 1
    let dl = DenyList::new(td.path().to_path_buf()).with_ttl(Duration::ZERO);
    let inbox = td.path().join("inbox");
    let rig = AdmissionRig {
        nucleon_map: &map,
        rate_limiter: &lim,
        deny_list: &dl,
        inbox_root: &inbox,
        now_ms: 0,
    };
    http_request_to_spark(
        &rig,
        &jwt("sub-1", "tok-1", "cosmon-rpp-tenant"),
        Verb::ObserveMolecule,
        Some("mol-1"),
    )
    .unwrap();
    let err = http_request_to_spark(
        &rig,
        &jwt("sub-1", "tok-2", "cosmon-rpp-tenant"),
        Verb::ObserveMolecule,
        Some("mol-1"),
    )
    .unwrap_err();
    assert!(matches!(err, RppRejectReason::RateLimited { .. }));
}

#[test]
fn deny_list_jti_blocks_admission() {
    let td = TempDir::new().unwrap();
    let (map, lim, dl) = rig_with(&td);
    std::fs::create_dir_all(td.path().join("security")).unwrap();
    std::fs::write(
        td.path().join("security/oidc-policy.toml"),
        r#"
[[deny.jti]]
jti = "tok-leak"
reason = "leaked"
since = "2026-04-27T00:00:00Z"
"#,
    )
    .unwrap();
    dl.invalidate();
    let rig = AdmissionRig {
        nucleon_map: &map,
        rate_limiter: &lim,
        deny_list: &dl,
        inbox_root: &td.path().join("inbox"),
        now_ms: 0,
    };
    let err = http_request_to_spark(
        &rig,
        &jwt("sub-1", "tok-leak", "cosmon-rpp-tenant"),
        Verb::ObserveMolecule,
        Some("mol-1"),
    )
    .unwrap_err();
    assert!(matches!(err, RppRejectReason::JtiKilled));
}

#[test]
fn causal_closure_inbox_file_exists_before_returning() {
    let td = TempDir::new().unwrap();
    let (map, lim, dl) = rig_with(&td);
    let inbox = td.path().join("inbox");
    let rig = AdmissionRig {
        nucleon_map: &map,
        rate_limiter: &lim,
        deny_list: &dl,
        inbox_root: &inbox,
        now_ms: 0,
    };
    let spark = http_request_to_spark(
        &rig,
        &jwt("sub-1", "tok-1", "cosmon-rpp-tenant"),
        Verb::ObserveMolecule,
        Some("mol-42"),
    )
    .unwrap();
    // File must exist on disk. Read its contents — raw `sub` MUST NOT
    // leak into the audit envelope.
    let text = std::fs::read_to_string(&spark.inbox_path).unwrap();
    assert!(!text.contains("sub-1"));
    assert!(text.contains("nuc-a"));
    assert!(text.contains("tenant-demo"));
    assert!(text.contains("observe"));
    assert!(text.contains("mol-42"));
}

#[test]
fn audit_envelope_carries_blake3_sub_hash() {
    let td = TempDir::new().unwrap();
    let (map, lim, dl) = rig_with(&td);
    let inbox = td.path().join("inbox");
    let rig = AdmissionRig {
        nucleon_map: &map,
        rate_limiter: &lim,
        deny_list: &dl,
        inbox_root: &inbox,
        now_ms: 0,
    };
    let spark = http_request_to_spark(
        &rig,
        &jwt("sub-1", "tok-1", "cosmon-rpp-tenant"),
        Verb::ObserveMolecule,
        Some("mol-x"),
    )
    .unwrap();
    let expected = cosmon_rpp_adapter::rate_limit::hash_sub("sub-1");
    let text = std::fs::read_to_string(&spark.inbox_path).unwrap();
    assert!(
        text.contains(&expected),
        "audit envelope must include BLAKE3(sub), got: {text}"
    );
}
