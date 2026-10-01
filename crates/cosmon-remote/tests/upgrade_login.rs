// SPDX-License-Identifier: AGPL-3.0-only

//! An older saved file credential authenticates through the upgraded client.

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use cosmon_remote::config::{Profile, ProfileStore, TopConfig};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test(flavor = "multi_thread")]
async fn an_existing_credential_authenticates_after_profile_reinitialization() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules"))
        .and(header("authorization", "Bearer saved-access"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "request_id": "test-request",
            "ensemble": {"molecules": []}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let home = tempfile::tempdir().unwrap();
    let base = if cfg!(target_os = "macos") {
        home.path().join("Library/Application Support")
    } else {
        home.path().join(".config")
    };
    let root = base.join("cosmon-remote");
    let profiles = ProfileStore::at(&root);
    let mut profile = Profile::from_host(server.uri());
    profile.sub = "subject-1".into();
    profile.aud = "audience-1".into();
    profile.oidc_url = server.uri();
    profile.issuer = Some("https://issuer.invalid".into());
    profile.client_id = Some("client-1".into());
    profile.timeout_secs = 300;
    profiles.write_profile("work-profile", &profile).unwrap();
    profiles
        .write_top(&TopConfig {
            default_profile: Some("work-profile".into()),
            credit_guard_acknowledged: None,
        })
        .unwrap();

    // This is the serialization used by the earlier client. It is independent
    // of the current CredentialKey implementation so a changed derivation
    // would make the authenticated request fail.
    let old_key = format!(
        "cosmon-remote\u{1f}v1\u{1f}{}\u{1f}{}\u{1f}{}",
        profile.issuer.as_deref().unwrap(),
        profile.sub,
        profile.client_id.as_deref().unwrap()
    );
    let digest = blake3::hash(old_key.as_bytes()).to_hex().to_string();
    let credentials = root.join("credentials");
    std::fs::create_dir_all(&credentials).unwrap();
    let saved = credentials.join(format!("{digest}.cred"));
    std::fs::write(
        &saved,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "access_token": "saved-access",
            "refresh_token": "saved-refresh",
            "expires_at": (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339()
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::set_permissions(&saved, std::fs::Permissions::from_mode(0o600)).unwrap();

    let bin = env!("CARGO_BIN_EXE_cosmon-remote");
    let run = |args: &[&str]| {
        Command::new(bin)
            .args(args)
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path().join(".config"))
            .env("COSMON_REMOTE_CRED_BACKEND", "file")
            .env_remove("COSMON_REMOTE_TOKEN")
            .output()
            .unwrap()
    };
    let init = run(&[
        "config",
        "init",
        "work-profile",
        &server.uri(),
        "--report-created",
    ]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    assert_eq!(init.stdout, b"preserved\n");
    let list = run(&["molecule", "list"]);
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    assert_eq!(
        profiles.read_profile("work-profile").unwrap().timeout_secs,
        300
    );
    assert!(saved.exists());
}

#[test]
fn missing_login_error_names_profile_and_expected_credential_file() {
    let home = tempfile::tempdir().unwrap();
    let base = if cfg!(target_os = "macos") {
        home.path().join("Library/Application Support")
    } else {
        home.path().join(".config")
    };
    let root = base.join("cosmon-remote");
    let profiles = ProfileStore::at(&root);
    let mut profile = Profile::from_host("https://example.invalid");
    profile.sub = "subject-1".into();
    profile.aud = "audience-1".into();
    profile.oidc_url = "https://example.invalid/oidc".into();
    profile.issuer = Some("https://issuer.invalid".into());
    profile.client_id = Some("client-1".into());
    let digest = profile.credential_key().unwrap().storage_id();
    profiles.write_profile("work-profile", &profile).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cosmon-remote"))
        .args(["--profile", "work-profile", "molecule", "list"])
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("COSMON_REMOTE_CRED_BACKEND", "file")
        .env_remove("COSMON_REMOTE_TOKEN")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let message = String::from_utf8(output.stderr).unwrap();
    assert!(message.contains("profile \"work-profile\""), "{message}");
    assert!(
        message.contains(&format!("credentials/{digest}.cred")),
        "{message}"
    );
    assert!(!message.contains("profile \"subject-1\""), "{message}");
}

#[test]
fn an_already_reset_profile_explains_why_relogin_is_needed() {
    let home = tempfile::tempdir().unwrap();
    let base = if cfg!(target_os = "macos") {
        home.path().join("Library/Application Support")
    } else {
        home.path().join(".config")
    };
    let root = base.join("cosmon-remote");
    let profiles = ProfileStore::at(&root);
    let mut profile = Profile::from_host("https://example.invalid");
    profile.sub = "subject-1".into();
    profile.aud = "audience-1".into();
    profile.oidc_url = "https://example.invalid/oidc".into();
    profiles.write_profile("work-profile", &profile).unwrap();
    let old_file = root.join("credentials/old-file.cred");
    std::fs::create_dir_all(old_file.parent().unwrap()).unwrap();
    std::fs::write(&old_file, b"saved credential remains").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cosmon-remote"))
        .args(["--profile", "work-profile", "molecule", "list"])
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("COSMON_REMOTE_CRED_BACKEND", "file")
        .env_remove("COSMON_REMOTE_TOKEN")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let message = String::from_utf8(output.stderr).unwrap();
    assert!(message.contains("profile \"work-profile\""), "{message}");
    assert!(message.contains("path cannot be derived"), "{message}");
    assert!(message.contains("cosmon-remote login"), "{message}");
    assert!(old_file.exists());
}
