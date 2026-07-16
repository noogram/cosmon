// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end fixture for the `cs spore` verb family (ADR-140 N7).
//!
//! This is the *citation-only* fixture: it wires the public workshop
//! `grace-business-analysis` spore bundle as the end-to-end test subject
//! **without copying it**. The bundle lives at
//! `/srv/cosmon/workshop/spores/grace-business-analysis/`; it is 100% public
//! (only the public Thierry Grace business-model frame) and self-contained
//! (no absolute paths, no galaxy dependency). The test references it where
//! it lives.
//!
//! Because the bundle is an external citation, a machine may simply not have
//! the workshop galaxy checked out (CI, a fresh clone). That is the same
//! honesty discipline as N4's TLC availability: when the fixture is absent we
//! **skip with a note**, never silently pass a hollow assertion and never
//! fail a green build for a missing citation.
//!
//! When the bundle *is* present the test drives the real `cs` binary through
//! the whole verb family and asserts the three N7 properties:
//!
//! 1. **Germination produces the expected node set**: `cs spore validate`
//!    expands to the five named pipeline stages (frame, analyse-axis,
//!    verify-finding, synthesize, graded-verdict), and `cs spore run`
//!    germinates them as real molecules on disk with the `blocked_by` wiring.
//! 2. **The seal gate fires**: the bundle carries a `[spore.seal]`, so
//!    `cs spore run` *without* `--allow-unchecked-seal` fails closed
//!    (ADR-140 D4), and *with* the flag germinates under an honest
//!    `seal: present, NOT verified` line, never "verified".
//! 3. **An ASTRA `ro-crate-metadata.json` is emitted**: `cs spore export`
//!    writes the RO-Crate descriptive layer (ADR-140 D6), marking the seal
//!    present-but-unverified.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Locate the citation-only workshop bundle relative to `$HOME`. Returns
/// `None` when the workshop galaxy is not checked out on this machine (the
/// fixture is a citation, not a vendored copy).
fn workshop_bundle() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let dir = PathBuf::from(home).join("galaxies/workshop/spores/grace-business-analysis");
    dir.join("spore.toml").is_file().then_some(dir)
}

/// A `cs` invocation with the worker-context env scrubbed so the e2e run
/// cannot reach into an enclosing molecule's state.
fn cs() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

/// The five named pipeline stages the bundle's DAG must always germinate,
/// independent of how the fan-out node expands (one fixed node in the
/// pre-D1 prototype form, one node per axis in the D1 fan-out form).
const STAGES: [&str; 5] = [
    "frame",
    "analyse-axis",
    "verify-finding",
    "synthesize",
    "graded-verdict",
];

/// `true` if `alias` belongs to `stage`: either the bare stage id (fixed or
/// emergent-controller) or a `stage__<index>` fan-out instance.
fn alias_is_stage(alias: &str, stage: &str) -> bool {
    alias == stage || alias.starts_with(&format!("{stage}__"))
}

#[test]
fn spore_e2e_validate_run_export_on_workshop_bundle() {
    let Some(bundle) = workshop_bundle() else {
        eprintln!(
            "skip: citation-only fixture absent \
             (/srv/cosmon/workshop/spores/grace-business-analysis not checked out)"
        );
        return;
    };

    // A neutral cwd so `cs` walk-up config discovery cannot reach the cosmon
    // checkout's own `.cosmon`.
    let cwd = tempfile::tempdir().unwrap();

    validate_expands_expected_node_set(&bundle, cwd.path());
    seal_gate_fails_closed_without_flag(&bundle, cwd.path());
    run_germinates_real_polymer_under_flag(&bundle, cwd.path());
    export_emits_ro_crate(&bundle, cwd.path());
}

/// Property 1 (dry run): `cs spore validate --json` expands to a call list
/// covering all five pipeline stages.
fn validate_expands_expected_node_set(bundle: &Path, cwd: &Path) {
    let out = cs()
        .current_dir(cwd)
        .args(["--json", "spore", "validate"])
        .arg(bundle)
        .args(["--var", "subject=Tenant-Demo Corp"])
        .output()
        .expect("run cs spore validate");
    assert!(
        out.status.success(),
        "validate failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let aliases = parse_aliases(&out.stdout);
    for stage in STAGES {
        assert!(
            aliases.iter().any(|a| alias_is_stage(a, stage)),
            "validate node set missing stage `{stage}`; got {aliases:?}"
        );
    }
}

/// Property 2 (closed): a sealed spore refuses to germinate without
/// `--allow-unchecked-seal` (ADR-140 D4 fail-closed).
fn seal_gate_fails_closed_without_flag(bundle: &Path, cwd: &Path) {
    let store = tempfile::tempdir().unwrap();
    let out = cs()
        .current_dir(cwd)
        .args(["spore", "run"])
        .arg(bundle)
        .args(["--var", "subject=Tenant-Demo Corp"])
        .arg("--store-dir")
        .arg(store.path())
        .output()
        .expect("run cs spore run (no flag)");

    assert!(
        !out.status.success(),
        "sealed spore must fail closed without --allow-unchecked-seal"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("fail-closed"),
        "expected a fail-closed seal refusal, got: {stderr}"
    );
    // Nothing must have germinated into the store.
    assert_eq!(
        germinated_count(store.path()),
        0,
        "no molecule may be written when the seal gate fails closed"
    );
}

/// Properties 1 (germination) + 2 (honest unverified) together: under the
/// flag the polymer germinates as real molecules with `blocked_by` wiring,
/// and the status line never claims the seal is verified.
fn run_germinates_real_polymer_under_flag(bundle: &Path, cwd: &Path) {
    let store = tempfile::tempdir().unwrap();
    let out = cs()
        .current_dir(cwd)
        .args(["--json", "spore", "run"])
        .arg(bundle)
        .args(["--var", "subject=Tenant-Demo Corp"])
        .arg("--allow-unchecked-seal")
        .arg("--store-dir")
        .arg(store.path())
        .output()
        .expect("run cs spore run --allow-unchecked-seal");
    assert!(
        out.status.success(),
        "germination failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Honest seal line: present, never "verified".
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("seal: present, NOT verified"),
        "expected an honest unverified seal line, got: {stderr}"
    );

    // Every stage germinated, both in the NDJSON report and on disk.
    let aliases = parse_aliases(&out.stdout);
    for stage in STAGES {
        assert!(
            aliases.iter().any(|a| alias_is_stage(a, stage)),
            "run did not germinate stage `{stage}`; got {aliases:?}"
        );
    }
    let on_disk = germinated_count(store.path());
    assert!(
        on_disk >= STAGES.len(),
        "expected at least {} molecules on disk, found {on_disk}",
        STAGES.len()
    );

    // `blocked_by` wiring landed: at least one germinated molecule carries a
    // BlockedBy link (the DAG is a chain, so every non-root node has one).
    assert!(
        any_molecule_has_blocked_by(store.path()),
        "no germinated molecule carries a BlockedBy edge: DAG wiring lost"
    );
}

/// Property 3: `cs spore export` writes the ASTRA `ro-crate-metadata.json`,
/// marking the seal present-but-unverified.
fn export_emits_ro_crate(bundle: &Path, cwd: &Path) {
    let out_dir = tempfile::tempdir().unwrap();
    let out = cs()
        .current_dir(cwd)
        .args(["--json", "spore", "export"])
        .arg(bundle)
        .arg("--out")
        .arg(out_dir.path())
        .output()
        .expect("run cs spore export");
    assert!(
        out.status.success(),
        "export failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let crate_path = out_dir.path().join("ro-crate-metadata.json");
    assert!(
        crate_path.is_file(),
        "export must emit {}",
        crate_path.display()
    );

    let crate_json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&crate_path).unwrap()).unwrap();
    assert_eq!(
        crate_json["@context"],
        serde_json::json!("https://w3id.org/ro/crate/1.1/context"),
        "emitted file must be a valid RO-Crate"
    );
    let dataset = &crate_json["@graph"][1];
    assert_eq!(
        dataset["name"],
        serde_json::json!("grace-business-analysis")
    );
    assert_eq!(
        dataset["spore:seal"]["present"],
        serde_json::json!(true),
        "the workshop bundle carries a seal"
    );
    assert_eq!(
        dataset["spore:seal"]["verified"],
        serde_json::json!(false),
        "ASTRA must never claim an unverified seal as verified"
    );
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Parse the `alias` field out of each NDJSON line on stdout.
fn parse_aliases(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v["alias"].as_str().map(str::to_string))
        .collect()
}

/// Count molecule directories under a state store's default fleet.
fn germinated_count(store: &Path) -> usize {
    let mol_dir = store.join("fleets/default/molecules");
    std::fs::read_dir(&mol_dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter(|e| e.path().is_dir())
                .count()
        })
        .unwrap_or(0)
}

/// `true` if any germinated molecule's `state.json` carries a `BlockedBy`
/// typed link, proving the alias-to-id DAG wiring landed on disk.
fn any_molecule_has_blocked_by(store: &Path) -> bool {
    let mol_dir = store.join("fleets/default/molecules");
    let Ok(entries) = std::fs::read_dir(&mol_dir) else {
        return false;
    };
    for entry in entries.filter_map(Result::ok) {
        let state = entry.path().join("state.json");
        let Ok(bytes) = std::fs::read(&state) else {
            continue;
        };
        let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        // Typed links are internally tagged on `rel` (snake_case): a
        // BlockedBy link reads `{"rel": "blocked_by", "source": "..."}`.
        if json
            .get("typed_links")
            .and_then(|v| v.as_array())
            .is_some_and(|arr| {
                arr.iter()
                    .any(|l| l.get("rel").and_then(|r| r.as_str()) == Some("blocked_by"))
            })
        {
            return true;
        }
    }
    false
}
