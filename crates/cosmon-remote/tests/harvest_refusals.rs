// SPDX-License-Identifier: AGPL-3.0-only

//! Typed harvest diagnostics through the real HTTP client. The server double
//! supplies wire bytes; the client owns decoding and the local action map.

use cosmon_remote::client::{Client, DoneRequest, HarvestAuthorizationWire};
use cosmon_remote::config::Profile;
use cosmon_remote::error::Error;
use cosmon_remote::hints;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn profile(server: &MockServer) -> Profile {
    Profile {
        host: server.uri(),
        sub: "operator".into(),
        aud: "tenant".into(),
        oidc_url: server.uri(),
        issuer: None,
        client_id: None,
        noyau: Some("default".into()),
        scopes: vec!["cosmon:molecule:harvest".into()],
        artifacts_dir: None,
        timeout_secs: 5,
        phone_home: false,
    }
}

async fn refused_body(reason: &str, action: &str) -> serde_json::Value {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/molecules/task-1/done"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": "not_authorized",
            "request_id": "req-harvest",
            "harvest_authorization": {"gate": "grant", "reason": reason, "action": action}
        })))
        .mount(&server)
        .await;
    let client = Client::new(&profile(&server), Some("test-token".into())).expect("client");
    let err = client
        .done("task-1", &DoneRequest::new("test harvest"))
        .await
        .expect_err("refusal");
    let Error::Api { status, body, .. } = err else {
        panic!("expected API error")
    };
    assert_eq!(status, 403);
    body
}

#[tokio::test]
async fn expired_and_mismatched_grants_render_their_actual_gesture() {
    for reason in ["harvest_grant_expired", "harvest_grant_mismatch"] {
        let body = refused_body(reason, "mint_grant").await;
        let typed = HarvestAuthorizationWire::from_error_body(&body).expect("typed diagnostic");
        assert_eq!(typed.reason, reason);
        assert_eq!(body["request_id"], "req-harvest");
        let (seen, gesture) = hints::for_harvest_authorization(&body).expect("local gesture");
        assert_eq!(seen, reason);
        assert!(gesture.contains("matching grant"));
    }
}

#[tokio::test]
async fn older_and_unknown_diagnostics_keep_the_coarse_fallback() {
    let old = json!({"error": "not_authorized", "request_id": "req-old"});
    assert!(HarvestAuthorizationWire::from_error_body(&old).is_none());
    assert!(hints::for_harvest_authorization(&old).is_none());

    let unknown = refused_body("future_reason", "run_arbitrary_command").await;
    assert_eq!(unknown["error"], "not_authorized");
    assert!(hints::for_harvest_authorization(&unknown).is_none());
}

fn config_root_under(home: &std::path::Path) -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Application Support/cosmon-remote")
    }
    #[cfg(not(target_os = "macos"))]
    {
        home.join(".config/cosmon-remote")
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_json_preserves_the_body_and_human_mode_names_one_gesture() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/molecules/task-1/done"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": "not_authorized",
            "request_id": "req-cli",
            "harvest_authorization": {
                "gate": "grant", "reason": "harvest_grant_expired", "action": "mint_grant"
            }
        })))
        .mount(&server)
        .await;
    let home = tempfile::tempdir().expect("home");
    let root = config_root_under(home.path());
    std::fs::create_dir_all(root.join("profiles")).expect("config directory");
    std::fs::write(root.join("config.toml"), "default_profile = \"test\"\n")
        .expect("profile selection");
    std::fs::write(
        root.join("profiles/test.toml"),
        format!(
            "host = {:?}\nsub = \"operator\"\naud = \"tenant\"\noidc_url = {:?}\n",
            server.uri(),
            server.uri()
        ),
    )
    .expect("profile");
    for json_mode in [true, false] {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_cosmon-remote"));
        command
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path().join(".config"))
            .env("COSMON_REMOTE_TOKEN", "fake-jwt");
        if json_mode {
            command.arg("--json");
        }
        let output = command
            .args(["molecule", "done", "task-1", "--reason", "test harvest"])
            .output()
            .expect("client process");
        assert!(!output.status.success());
        if json_mode {
            let body: serde_json::Value =
                serde_json::from_slice(&output.stdout).expect("JSON error");
            assert_eq!(
                body["harvest_authorization"]["reason"],
                "harvest_grant_expired"
            );
            assert_eq!(body["request_id"], "req-cli");
        } else {
            let stderr = String::from_utf8(output.stderr).expect("stderr");
            assert!(
                stderr.contains("harvest_grant_expired: inspect current harvest status"),
                "{stderr}"
            );
            assert!(stderr.contains("request_id: req-cli"), "{stderr}");
        }
    }
}
