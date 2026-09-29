// SPDX-License-Identifier: AGPL-3.0-only

//! The operator's deny files must give the same admission decision at boot
//! and after a cache refresh.

use std::fs;
use std::path::Path;
use std::time::Duration;

use cosmon_rpp_adapter::admission::{http_request_to_spark, AdmissionRig, Verb};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::error::RppRejectReason;
use cosmon_rpp_adapter::jwt::ValidatedJwt;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::{hash_sub, IngressRateLimiter};

fn security(root: &Path) -> std::path::PathBuf {
    let dir = root.join("security");
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn admission_result(root: &Path, deny_list: &DenyList) -> Result<(), RppRejectReason> {
    let map = HabilitationMap::builder()
        .insert(
            "https://issuer.example",
            "alice",
            HabilitationId::new("binding-1"),
            Noyau::new("tenant-1"),
            "tenant-audience",
        )
        .build();
    let limiter = IngressRateLimiter::new(root.join("rate-limit"), 10.0, 0.0);
    let inbox = root.join("inbox");
    let rig = AdmissionRig {
        nucleon_map: &map,
        rate_limiter: &limiter,
        deny_list,
        inbox_root: &inbox,
        now_ms: 0,
    };
    let jwt = ValidatedJwt {
        iss: "https://issuer.example".into(),
        sub: "alice".into(),
        aud: "tenant-audience".into(),
        jti: "token-1".into(),
        lifetime_sec: 60,
        exp: 9_999_999_999,
        scopes: vec![],
    };
    http_request_to_spark(&rig, &jwt, Verb::ObserveMolecule, None).map(|_| ())
}

#[test]
fn invalid_operator_files_refuse_admission_on_boot() {
    for name in ["oidc-kill.toml", "oidc-policy.toml"] {
        let td = tempfile::tempdir().unwrap();
        fs::write(security(td.path()).join(name), "[[deny.sub]\n").unwrap();
        let deny_list = DenyList::new(td.path().to_path_buf());
        assert!(
            matches!(
                admission_result(td.path(), &deny_list),
                Err(RppRejectReason::GlobalKill)
            ),
            "{name} must close admission at boot"
        );
    }
}

#[test]
fn malformed_policy_refresh_never_clears_an_existing_revocation() {
    let td = tempfile::tempdir().unwrap();
    let policy = security(td.path()).join("oidc-policy.toml");
    let sub_hash = hash_sub("alice");
    fs::write(&policy, format!("[[deny.sub]]\nsub_hash = {sub_hash:?}\n")).unwrap();
    let deny_list = DenyList::new(td.path().to_path_buf()).with_ttl(Duration::ZERO);
    assert!(matches!(
        admission_result(td.path(), &deny_list),
        Err(RppRejectReason::SubKilled)
    ));

    fs::write(
        &policy,
        format!("[[deny.sub]]\nsub_hash = {sub_hash:?}\n[[deny.sub]\n"),
    )
    .unwrap();
    assert!(
        matches!(
            admission_result(td.path(), &deny_list),
            Err(RppRejectReason::GlobalKill)
        ),
        "a failed refresh must refuse all admission"
    );

    fs::write(&policy, "").unwrap();
    assert!(
        admission_result(td.path(), &deny_list).is_ok(),
        "a repaired policy reopens admission"
    );
}

#[test]
fn unreadable_operator_files_close_admission_on_refresh() {
    for name in ["oidc-kill.toml", "oidc-policy.toml"] {
        let td = tempfile::tempdir().unwrap();
        let path = security(td.path()).join(name);
        fs::write(&path, "").unwrap();
        let deny_list = DenyList::new(td.path().to_path_buf()).with_ttl(Duration::ZERO);
        assert!(admission_result(td.path(), &deny_list).is_ok());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(
            matches!(
                admission_result(td.path(), &deny_list),
                Err(RppRejectReason::GlobalKill)
            ),
            "{name} must close admission after a read error"
        );
    }
}
