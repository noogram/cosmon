// SPDX-License-Identifier: AGPL-3.0-only

//! Operator-side harvest surface, exercised through the shipped binary.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use cosmon_core::harvest_authorization::{
    DoneAuthorization, GrantEpoch, HarvestAction, HarvestGrant, HarvestScope,
};
use cosmon_core::id::MoleculeId;
use cosmon_notary::minisign::{self, MinisignPublicKey, MinisignSignature};
use serde_json::{json, Value};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn invoke(home: &Path, args: &[&str], password: &str) -> Output {
    invoke_with_path(home, args, password, None)
}

fn invoke_with_path(home: &Path, args: &[&str], password: &str, path: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cosmon-remote"));
    command
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_ARTIFACT_DIR")
        .env_remove("COSMON_API_REQUEST")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path) = path {
        command.env("PATH", path);
    }
    let mut child = command.spawn().expect("run shipped client");
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(password.as_bytes())
            .expect("send signer approval");
    }
    child.wait_with_output().expect("wait for shipped client")
}

fn assert_ok(output: &Output) {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn prepare_profile(home: &Path, host: &str) {
    for args in [
        vec!["config", "init", "operator", host],
        vec!["config", "set", "sub", "operator"],
        vec!["config", "set", "aud", "tenant"],
        vec!["config", "set", "oidc-url", host],
        vec!["config", "set", "noyau", "demo"],
    ] {
        assert_ok(&invoke(home, &args, ""));
    }
}

#[test]
fn operator_harvest_commands_and_offline_modes_are_available() {
    let bin = env!("CARGO_BIN_EXE_cosmon-remote");
    for sub in ["configure", "init", "grant", "status"] {
        let output = Command::new(bin)
            .args(["harvest", sub, "--help"])
            .output()
            .expect("run shipped client");
        assert!(
            output.status.success(),
            "harvest {sub} absent: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let output = Command::new(bin)
        .args(["harvest", "grant", "--help"])
        .output()
        .expect("run grant help");
    let help = String::from_utf8_lossy(&output.stdout);
    for mode in ["--export", "--sign", "--import"] {
        assert!(help.contains(mode), "grant is missing {mode}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configure_uses_admin_seal_and_current_compare_and_set_facts() {
    let home = tempfile::tempdir().expect("operator home");
    let server = MockServer::start().await;
    prepare_profile(home.path(), &server.uri());
    Mock::given(method("GET"))
        .and(path("/v1/harvest/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "policy": "sealed", "provenance": "legacy", "epoch": 7,
            "key_fingerprint": "old-public-digest", "required_scope": "cosmon:molecule:write",
            "executor_supported": true
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/v1/admin/noyaux/demo/harvest-authority"))
        .and(header("X-Cosmon-Admin-Token", "admin-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "policy": "scoped", "key_fingerprint": "old-public-digest", "epoch": 7
        })))
        .mount(&server)
        .await;
    let status_output = invoke(
        home.path(),
        &["--token", "tenant-test", "--json", "harvest", "status"],
        "",
    );
    assert_ok(&status_output);
    let observed: Value = serde_json::from_slice(&status_output.stdout).expect("status JSON");
    assert_eq!(observed["policy"], "sealed");
    assert_eq!(observed["provenance"], "legacy");
    let token_file = home.path().join("admin-token");
    std::fs::write(&token_file, "admin-test\n").expect("token file");
    let output = invoke(
        home.path(),
        &[
            "--token",
            "tenant-test",
            "--json",
            "harvest",
            "configure",
            "--policy",
            "scoped",
            "--admin-token-file",
            token_file.to_str().expect("utf8"),
        ],
        "",
    );
    assert_ok(&output);
    let response: Value = serde_json::from_slice(&output.stdout).expect("JSON response");
    assert_eq!(response["policy"], "scoped");
    let requests = server.received_requests().await.expect("requests");
    let update = requests
        .iter()
        .find(|request| request.method.as_str() == "PUT")
        .expect("admin update");
    let body: Value = serde_json::from_slice(&update.body).expect("CAS JSON");
    assert_eq!(body["expected_policy"], Value::Null);
    assert_eq!(body["expected_key_digest"], "old-public-digest");
    assert_eq!(body["expected_epoch"], 7);
    assert_eq!(body["policy"], "scoped");
    assert_eq!(body["public_key"], Value::Null);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_external_signer_installs_molecule_grant_without_exporting_its_key() {
    if Command::new("minisign").arg("-v").output().is_err() {
        eprintln!("minisign unavailable: operator signer integration not exercised");
        return;
    }
    let home = tempfile::tempdir().expect("operator home");
    let server = MockServer::start().await;
    let host = server.uri();
    prepare_profile(home.path(), &host);
    Mock::given(method("GET"))
        .and(path("/v1/harvest/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "policy": "disabled", "provenance": "legacy", "epoch": 1,
            "key_fingerprint": null, "required_scope": "cosmon:molecule:write",
            "executor_supported": true, "request_id": "req-status"
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/v1/admin/noyaux/demo/harvest-authority"))
        .and(header("X-Cosmon-Admin-Token", "admin-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "policy": "sealed", "key_fingerprint": "installed", "epoch": 1
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/v1/admin/noyaux/demo/harvest-authority"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({"error": "retry"})))
        .with_priority(1)
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let mol = MoleculeId::new("task-20260930-1234").expect("molecule id");
    let grant = HarvestGrant::new(
        "demo",
        HarvestScope::Molecule { molecule: mol },
        "main",
        HarvestAction::Done,
        Vec::<String>::new(),
        GrantEpoch::from_u64(1),
        None,
    )
    .expect("grant");
    Mock::given(method("POST"))
        .and(path("/v1/harvest/challenge"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "grant": grant, "canonical": String::from_utf8(grant.canonical_bytes()).expect("utf8"),
            "fingerprint": grant.fingerprint().as_str(), "request_id": "req-challenge"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/harvest/grants"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "fingerprint": grant.fingerprint().as_str(), "installed": true,
            "consumed": false, "request_id": "req-import"
        })))
        .mount(&server)
        .await;
    let token_file = home.path().join("admin-token");
    std::fs::write(&token_file, "admin-test\n").expect("admin token");
    let token_path = token_file.to_str().expect("utf8 path");
    let init_args = [
        "--token",
        "tenant-test",
        "harvest",
        "init",
        "--admin-token-file",
        token_path,
    ];
    let missing_signer = invoke_with_path(home.path(), &init_args, "", Some("/no/signer"));
    assert!(!missing_signer.status.success());
    assert!(String::from_utf8_lossy(&missing_signer.stderr).contains("minisign is required"));
    let first = invoke(
        home.path(),
        &init_args,
        "correct horse battery\ncorrect horse battery\n",
    );
    assert!(
        !first.status.success(),
        "first admin call must expose the network fault"
    );
    let key_dir = home.path().join(".config/cosmon/harvest/keys/operator");
    let pending = key_dir.join("pending.key");
    let pending_bytes = std::fs::read(&pending).expect("pending encrypted key survives retry");
    let init = invoke(home.path(), &init_args, "");
    assert_ok(&init);
    assert!(
        !pending.exists(),
        "successful retry finalizes the pending key"
    );
    let key = std::fs::read_dir(&key_dir)
        .expect("key directory")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| path.extension().is_some_and(|ext| ext == "key"))
        .expect("encrypted key");
    let key_text = std::fs::read_to_string(&key).expect("key contents");
    assert_eq!(std::fs::read(&key).expect("final key"), pending_bytes);
    assert!(key_text.starts_with("untrusted comment: minisign encrypted secret key"));
    let second_init = invoke(home.path(), &init_args, "");
    assert_ok(&second_init);
    assert_eq!(
        std::fs::read(&key).expect("key after repeat init"),
        pending_bytes
    );
    let denied = invoke(
        home.path(),
        &[
            "--token",
            "tenant-test",
            "harvest",
            "grant",
            "--molecule",
            "task-20260930-1234",
            "--no-expiry",
        ],
        "",
    );
    assert!(
        !denied.status.success(),
        "signer cancellation must refuse the grant"
    );
    let before_success = server.received_requests().await.expect("recorded requests");
    assert!(!before_success
        .iter()
        .any(|r| r.url.path() == "/v1/harvest/grants"));
    let grant_out = invoke(
        home.path(),
        &[
            "--token",
            "tenant-test",
            "--json",
            "harvest",
            "grant",
            "--molecule",
            "task-20260930-1234",
            "--no-expiry",
        ],
        "correct horse battery\n",
    );
    assert_ok(&grant_out);
    let result: Value = serde_json::from_slice(&grant_out.stdout).expect("import receipt JSON");
    assert_eq!(result["installed"], true);
    assert_eq!(result["consumed"], false);
    let requests = server.received_requests().await.expect("recorded requests");
    let admin = requests
        .iter()
        .find(|r| r.url.path().contains("admin/noyaux"))
        .expect("admin call");
    let admin_body: Value = serde_json::from_slice(&admin.body).expect("admin JSON");
    assert_eq!(admin_body["policy"], "sealed");
    assert!(admin_body["public_key"].as_str().is_some());
    let imported = requests
        .iter()
        .find(|r| r.url.path() == "/v1/harvest/grants")
        .expect("import call");
    let imported_body: Value = serde_json::from_slice(&imported.body).expect("import JSON");
    let authorization: DoneAuthorization =
        serde_json::from_value(imported_body["authorization"].clone()).expect("authorization");
    assert_eq!(authorization.grant(), &grant);
    let public = std::fs::read_to_string(key.with_extension("pub")).expect("public key");
    let public = MinisignPublicKey::parse(&public).expect("parse public key");
    let signature = MinisignSignature::parse(&authorization.attestation().to_minisig_file())
        .expect("parse signature");
    minisign::verify(&public, &grant.canonical_bytes(), &signature).expect("independent verifier");
    let all_requests = format!("{admin_body}{imported_body}");
    assert!(!all_requests.contains(&key_text));
    assert!(!all_requests.contains("encrypted secret key"));

    Mock::given(method("POST"))
        .and(path("/v1/harvest/challenge"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "grant": grant, "canonical": "opaque server bytes",
            "fingerprint": grant.fingerprint().as_str()
        })))
        .with_priority(1)
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let export = home.path().join("opaque.challenge.json");
    let refused = invoke(
        home.path(),
        &[
            "--token",
            "tenant-test",
            "harvest",
            "grant",
            "--molecule",
            "task-20260930-1234",
            "--export",
            export.to_str().expect("utf8 path"),
        ],
        "",
    );
    assert!(!refused.status.success());
    assert!(
        !export.exists(),
        "opaque challenge is never exported for signing"
    );

    let mission_grant = HarvestGrant::new(
        "demo",
        HarvestScope::Mission {
            mission: MoleculeId::new("task-20260930-1234").expect("mission id"),
            policy_digest: "policy-digest".to_owned(),
        },
        "main",
        HarvestAction::Done,
        Vec::<String>::new(),
        GrantEpoch::from_u64(1),
        None,
    )
    .expect("mission grant");
    Mock::given(method("POST"))
        .and(path("/v1/harvest/challenge"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "grant": mission_grant,
            "canonical": String::from_utf8(mission_grant.canonical_bytes()).expect("utf8"),
            "fingerprint": mission_grant.fingerprint().as_str()
        })))
        .with_priority(1)
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let challenge_file = home.path().join("mission.challenge.json");
    assert_ok(&invoke(
        home.path(),
        &[
            "--token",
            "tenant-test",
            "harvest",
            "grant",
            "--mission",
            "task-20260930-1234",
            "--no-expiry",
            "--export",
            challenge_file.to_str().expect("utf8"),
        ],
        "",
    ));
    let signed_file = home.path().join("mission.signed.json");
    assert_ok(&invoke(
        home.path(),
        &[
            "harvest",
            "grant",
            "--sign",
            challenge_file.to_str().expect("utf8"),
            "--output",
            signed_file.to_str().expect("utf8"),
        ],
        "correct horse battery\n",
    ));
    let signed: Value = serde_json::from_slice(&std::fs::read(&signed_file).expect("signed file"))
        .expect("signed JSON");
    assert_eq!(signed["authorization"]["authority"], "delegated");
    let offline_import = invoke(
        home.path(),
        &[
            "--token",
            "tenant-test",
            "--json",
            "harvest",
            "grant",
            "--import",
            signed_file.to_str().expect("utf8"),
        ],
        "",
    );
    assert_ok(&offline_import);
    let receipt: Value = serde_json::from_slice(&offline_import.stdout).expect("receipt");
    assert_eq!(receipt["installed"], true);
    assert!(
        receipt.get("merged").is_none(),
        "import does not claim a merge"
    );
}
