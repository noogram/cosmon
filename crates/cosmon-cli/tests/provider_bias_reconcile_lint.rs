// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end CLI proof of the ADR-147 tier-(a) provider-committee diversity
//! lint (`check_no_profile_requirement_downgrade`, task-20260711-e542 / C3).
//!
//! The unit tests in `cosmon_core::provider_diversity` prove the *policy*
//! (endpoint resolution + collision/floor detection). This file proves the
//! *wiring*: that `cs reconcile --check` actually loads the `[provider_bias]`
//! section, runs the lint, and **fails closed (`exit 1`)** when the committee's
//! resolved endpoints collapse below its floor — while a plain `cs reconcile`
//! (projection) only reports and never aborts (§8b: a config lint must never
//! wedge a surface sync). It mirrors the sibling Ghost-A lint's contract.

use std::fs;
use std::process::Command;

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

fn cosmon_bin_isolated(state_dir: &std::path::Path) -> Command {
    let config_path = state_dir
        .parent()
        .expect("state_dir must live under .cosmon/")
        .join("config.toml");
    let mut cmd = cosmon_bin();
    cmd.env("COSMON_STATE_DIR", state_dir)
        .env("COSMON_CONFIG", config_path)
        .current_dir(state_dir);
    cmd
}

/// Write a minimal `.cosmon/` with the given `config.toml` body and return the
/// state dir. No `surfaces.toml` — the lint runs *before* the surfaces gate, so
/// it fires even in a galaxy that declares no surfaces (which is exactly the
/// path this test exercises).
fn setup_project(tmp: &std::path::Path, config_body: &str) -> std::path::PathBuf {
    let cosmon_dir = tmp.join(".cosmon");
    let state_dir = cosmon_dir.join("state");
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(cosmon_dir.join("config.toml"), config_body).unwrap();
    fs::write(
        state_dir.join("fleet.json"),
        "{\"workers\":{},\"repos\":{}}\n",
    )
    .unwrap();
    state_dir
}

/// Two vendor-default seats that both resolve to the `OpenAI` family — the
/// committee *names* two readers but delivers one endpoint. Floor of 2 is not
/// met; `--check` must fail closed.
const COLLIDING_CONFIG: &str = "\
[project]
project_id = \"test-provbias-collide\"

[adapters.reader_a]
default_model = \"gpt-4o\"

[adapters.reader_b]
default_model = \"gpt-4o-mini\"

[provider_bias]
additional_readers = [\"reader_a\", \"reader_b\"]
min_distinct_provider_endpoints = 2
";

/// Two seats resolving to genuinely distinct families (`Anthropic` + `OpenAI`) —
/// the committee is diverse, the lint stays green.
const DISTINCT_CONFIG: &str = "\
[project]
project_id = \"test-provbias-distinct\"

[adapters.reader_a]
default_model = \"claude-opus-4-8\"

[adapters.reader_b]
default_model = \"gpt-4o\"

[provider_bias]
additional_readers = [\"reader_a\"]
additional_falsifiers = [\"reader_b\"]
min_distinct_provider_endpoints = 2
";

#[test]
fn reconcile_check_fails_closed_on_endpoint_collapse() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = setup_project(tmp.path(), COLLIDING_CONFIG);

    let out = cosmon_bin_isolated(&state_dir)
        .args(["reconcile", "--check"])
        .output()
        .expect("spawn cs reconcile --check");

    assert!(
        !out.status.success(),
        "cs reconcile --check must fail on a committee whose seats collapse to one endpoint"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("diversity violation") || stderr.contains("SAME endpoint"),
        "stderr should name the diversity violation, got: {stderr}"
    );
}

#[test]
fn plain_reconcile_reports_but_does_not_abort() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = setup_project(tmp.path(), COLLIDING_CONFIG);

    // Without `--check`, the lint reports on stderr but the command still
    // succeeds — a config lint must never wedge a projection (§8b ceiling).
    let out = cosmon_bin_isolated(&state_dir)
        .arg("reconcile")
        .output()
        .expect("spawn cs reconcile");

    assert!(
        out.status.success(),
        "plain cs reconcile must not abort on a provider-bias violation: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn reconcile_check_passes_on_distinct_providers() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = setup_project(tmp.path(), DISTINCT_CONFIG);

    let out = cosmon_bin_isolated(&state_dir)
        .args(["reconcile", "--check"])
        .output()
        .expect("spawn cs reconcile --check");

    // A genuinely diverse committee resolves to distinct endpoints — the
    // provider-bias lint does not trip. (The command may still exit non-zero
    // for OTHER reasons such as a missing surfaces.toml surface-drift, so we
    // assert specifically that the diversity violation message is ABSENT.)
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("diversity violation"),
        "a distinct-provider committee must not trip the lint, got: {stderr}"
    );
}

#[test]
fn reconcile_check_json_emits_violation_status() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = setup_project(tmp.path(), COLLIDING_CONFIG);

    let out = cosmon_bin_isolated(&state_dir)
        .args(["--json", "reconcile", "--check"])
        .output()
        .expect("spawn cs --json reconcile --check");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("provider_requirement_downgrade"),
        "JSON output should carry the provider_requirement_downgrade status, got: {stdout}"
    );
}
