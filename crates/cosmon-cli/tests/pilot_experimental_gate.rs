// SPDX-License-Identifier: AGPL-3.0-only

//! `cs pilot` experimental-gate tests.
//!
//! `cs pilot` launches an interactive REPL against a local model; it is
//! gated behind `--experimental` exactly like `cs ask` (ADR-071). The
//! contract these tests lock is the *gate*, not the loop: without
//! `--experimental` the verb must be a **no-op** — print a safety notice,
//! touch no model, create no file, and exit 0.
//!
//! We deliberately do not exercise the `--experimental` happy path here: it
//! would block on stdin and reach for a local Ollama endpoint that is not
//! present in CI. The REPL itself is covered by `cosmon-pilot`'s own
//! end-to-end test against a scripted provider.

use std::process::Command;

fn cs_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cs")
}

/// Run `cs <args...>` in `dir` and return (stdout, stderr, exit-code).
fn run_cs_in(dir: &std::path::Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(cs_bin())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn cs");
    let stdout = String::from_utf8(out.stdout).expect("stdout is utf8");
    let stderr = String::from_utf8(out.stderr).expect("stderr is utf8");
    let code = out.status.code().unwrap_or(-1);
    (stdout, stderr, code)
}

/// Without `--experimental`, `cs pilot` prints the safety notice to stderr,
/// launches nothing, and exits 0.
#[test]
fn pilot_without_experimental_is_a_noop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (stdout, stderr, code) = run_cs_in(dir.path(), &["pilot"]);

    assert_eq!(code, 0, "cs pilot (no flag) must exit 0; stderr:\n{stderr}");
    assert!(
        stdout.is_empty(),
        "no stdout on the gated path; got:\n{stdout}"
    );
    assert!(
        stderr.contains("experimental"),
        "safety notice must mention 'experimental'; got:\n{stderr}"
    );
    // The no-op must not create the transcript that the live REPL would.
    assert!(
        !dir.path().join("pilot-transcript.md").exists(),
        "gated cs pilot must not create a transcript"
    );
}

/// `--json` on the gated path emits one machine-readable line on stdout
/// signalling the verb did not launch, and still exits 0.
#[test]
fn pilot_without_experimental_json_emits_structured_notice() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (stdout, _stderr, code) = run_cs_in(dir.path(), &["--json", "pilot"]);

    assert_eq!(code, 0, "cs --json pilot (no flag) must exit 0");
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("--json output is a single JSON object");
    assert_eq!(parsed["command"], "pilot");
    assert_eq!(parsed["experimental"], false);
    assert_eq!(parsed["launched"], false);
    assert!(
        !dir.path().join("pilot-transcript.md").exists(),
        "gated cs --json pilot must not create a transcript"
    );
}
