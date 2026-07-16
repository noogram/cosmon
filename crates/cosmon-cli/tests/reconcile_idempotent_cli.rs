// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end CLI proof that `cs reconcile` is idempotent at the *command*
//! level — not merely at the renderer level (task-20260622-7072 D3,
//! delib-20260622-187a F-LINUS-6).
//!
//! Idempotency was already byte-proven inside `cosmon-surface`:
//! `tests/derived_view_overwrite.rs::reprojection_is_idempotent_and_never_stacks`
//! and the 5× byte-equal loop in `render.rs`. But CLAUDE.md claims
//! reconcile idempotency is "enforced by tests", and torvalds' review found
//! the one gap: *no test ran `cs reconcile` twice end-to-end and diffed the
//! surface set*. This test closes that gap.
//!
//! It drives the real `cs` binary against a **multi-surface** fixture
//! (`STATUS.md` + `ISSUES.md`), runs `cs reconcile` twice, and asserts every
//! declared surface file is **byte-identical** on the second pass — the
//! property a stray non-deterministic projection (unsorted reads, embedded
//! wall-clock, stacking appends) would break. The internal
//! `surfaces.snapshot.json` is intentionally *not* diffed: it embeds a
//! `projected_at` wall-clock by design, and is bookkeeping rather than a
//! user-facing surface.

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

/// Throwaway `.cosmon/` layout with the `task-work` formula and a
/// **two-surface** `surfaces.toml` (STATUS.md + ISSUES.md) so reconcile
/// projects more than one file — the "surface set" the idempotency claim
/// is about.
fn setup_multi_surface_project(tmp: &std::path::Path) -> std::path::PathBuf {
    let cosmon_dir = tmp.join(".cosmon");
    let state_dir = cosmon_dir.join("state");
    let formulas_dir = cosmon_dir.join("formulas");
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir_all(&formulas_dir).unwrap();
    fs::write(
        cosmon_dir.join("config.toml"),
        "[project]\nproject_id = \"test-reconcile-idem\"\n",
    )
    .unwrap();
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.cosmon/formulas/task-work.formula.toml");
    fs::copy(&src, formulas_dir.join("task-work.formula.toml"))
        .unwrap_or_else(|e| panic!("copy task-work.formula.toml: {e}"));
    fs::write(
        cosmon_dir.join("surfaces.toml"),
        "[[surface]]\n\
         referent = \"project.status\"\n\
         kind = \"markdown\"\n\
         path = \"STATUS.md\"\n\
         \n\
         [[surface]]\n\
         referent = \"project.issues\"\n\
         kind = \"markdown\"\n\
         path = \"ISSUES.md\"\n",
    )
    .unwrap();
    fs::write(
        state_dir.join("fleet.json"),
        "{\"workers\":{},\"repos\":{}}\n",
    )
    .unwrap();
    state_dir
}

fn nucleate_task(state_dir: &std::path::Path, topic: &str) {
    let out = cosmon_bin_isolated(state_dir)
        .arg("--json")
        .arg("nucleate")
        .arg("task-work")
        .arg("--var")
        .arg(format!("topic={topic}"))
        .output()
        .expect("spawn cs nucleate");
    assert!(
        out.status.success(),
        "cs nucleate task-work failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_reconcile(state_dir: &std::path::Path) {
    let out = cosmon_bin_isolated(state_dir)
        .arg("reconcile")
        .output()
        .expect("spawn cs reconcile");
    assert!(
        out.status.success(),
        "cs reconcile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The surface files declared in the fixture's `surfaces.toml`, relative to
/// the project root (parent of `.cosmon/`).
const SURFACE_FILES: [&str; 2] = ["STATUS.md", "ISSUES.md"];

#[test]
fn reconcile_twice_is_byte_identical_at_cli_level() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = setup_multi_surface_project(tmp.path());
    let project_root = state_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("project root is parent of .cosmon/")
        .to_path_buf();

    // Populate both surfaces with real content: pending molecules render
    // into STATUS.md *and* ISSUES.md.
    nucleate_task(&state_dir, "first idempotency probe");
    nucleate_task(&state_dir, "second idempotency probe");

    // First projection.
    run_reconcile(&state_dir);

    // Every declared surface must now exist; snapshot its bytes.
    let mut first_pass: Vec<(String, Vec<u8>)> = Vec::new();
    for rel in SURFACE_FILES {
        let path = project_root.join(rel);
        let bytes = fs::read(&path)
            .unwrap_or_else(|e| panic!("surface {rel} must exist after first reconcile: {e}"));
        assert!(
            !bytes.is_empty(),
            "surface {rel} must have content after first reconcile"
        );
        first_pass.push((rel.to_owned(), bytes));
    }

    // Second projection — a pure idempotent re-run, no state changes between.
    run_reconcile(&state_dir);

    // Byte-for-byte identical on the second pass: no stacking, no reordering,
    // no embedded wall-clock leaking into the surface body.
    for (rel, before) in &first_pass {
        let after = fs::read(project_root.join(rel)).unwrap_or_else(|e| {
            panic!("surface {rel} must still exist after second reconcile: {e}")
        });
        assert_eq!(
            before, &after,
            "surface {rel} changed between two reconcile passes — projection is not idempotent at the CLI level"
        );
    }
}
