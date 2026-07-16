// SPDX-License-Identifier: AGPL-3.0-only

//! `cs tackle --adapter` integration tests (ADR-097 / C6).
//!
//! Validates the four-way resolution of the Worker-Spawn Port Adapter
//! at `cs tackle` time — CLI flag, `[adapters.default]` config,
//! built-in fallback, unknown-name failure — and the `AdapterSelected`
//! event emitted on every Adapter-bound invocation.
//!
//! Every test uses `cs tackle --dry-run` so no tmux session is
//! spawned. The dry-run path still walks the adapter-resolution
//! block, validates the name, and emits the `AdapterSelected` event
//! — that is the C6 surface under test.

use std::fs;
use std::path::Path;
use std::process::Command;

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        // Hermetic adapter-resolution chain: strip the operator's
        // session hammer ($COSMON_DEFAULT_ADAPTER, rank 3) so the test
        // exercises the documented default-resolution order rather than
        // whatever the developer happened to export in their shell.
        .env_remove("COSMON_DEFAULT_ADAPTER");
    cmd
}

fn cosmon_bin_in(cwd: &Path) -> Command {
    let mut cmd = cosmon_bin();
    cmd.current_dir(cwd);
    // Hermetic global-config tier: point `$COSMON_CONFIG_HOME` (rank 5,
    // the machine-wide `~/.config/cosmon/config.toml::[adapters.default]`)
    // at an empty dir under the per-test tmp. Without this the test reads
    // the operator's real global config — which legitimately pins
    // `default = "claude"` while on critical work — and the `local`-floor
    // assertion (`tackle_without_flag_emits_default_source`) fails with
    // left=claude. `global_adapter_config_path()` honours this var
    // precisely for this isolation. The dir need not exist: a missing
    // `cosmon/config.toml` under it falls through to the built-in floor.
    cmd.env("COSMON_CONFIG_HOME", cwd.join("isolated-config-home"));
    cmd
}

/// Set up a tempdir with a one-step formula and a nucleated molecule;
/// return `(tmp, state_dir, molecule_id)`.
///
/// The molecule is in `Pending` — `cs tackle --dry-run` runs the full
/// adapter-resolution path against a real molecule without spawning a
/// tmux session.
fn setup_project_with_molecule() -> (tempfile::TempDir, std::path::PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();

    let formula_toml = r#"
formula = "tackle-adapter-test"
version = 1
description = "One-step formula for the cs tackle --adapter integration tests"
id_prefix = "ta"

[[steps]]
id = "step-1"
title = "Step 1"
description = "Solo step — the worker would do the work."
acceptance = "Done"
"#;
    fs::write(
        formulas_dir.join("tackle-adapter-test.formula.toml"),
        formula_toml,
    )
    .unwrap();

    let state_str = state_dir.to_str().unwrap();
    let output = cosmon_bin_in(tmp.path())
        .args([
            "--json",
            "nucleate",
            "tackle-adapter-test",
            "--store-dir",
            state_str,
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate failed to spawn");
    assert!(
        output.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let nucleate_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    let molecule_id = nucleate_json["id"].as_str().unwrap().to_owned();

    // `cs nucleate` does not stamp project identity (the seed used by
    // the in-tree `cs init`). Write a minimal `.cosmon/config.toml`
    // at the cosmon discovery root (tmp dir) so `require_project_identity`
    // accepts the `cs tackle` invocation.
    write_minimal_config(tmp.path(), &state_dir, /* adapters */ "");

    (tmp, state_dir, molecule_id)
}

/// Like [`setup_project_with_molecule`] but the formula's single step pins
/// `adapter = "<step_adapter>"` (per-workflow override).
/// Returns `(tmp, state_dir, molecule_id)`.
fn setup_project_with_step_adapter(
    step_adapter: &str,
) -> (tempfile::TempDir, std::path::PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();

    let formula_toml = format!(
        r#"
formula = "tackle-step-adapter-test"
version = 1
description = "One-step formula whose step pins a specific adapter"
id_prefix = "tsa"

[[steps]]
id = "step-1"
title = "Step 1"
description = "Solo step pinning a frontier adapter."
acceptance = "Done"
adapter = "{step_adapter}"
"#
    );
    fs::write(
        formulas_dir.join("tackle-step-adapter-test.formula.toml"),
        &formula_toml,
    )
    .unwrap();

    let state_str = state_dir.to_str().unwrap();
    let output = cosmon_bin_in(tmp.path())
        .args([
            "--json",
            "nucleate",
            "tackle-step-adapter-test",
            "--store-dir",
            state_str,
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate failed to spawn");
    assert!(
        output.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let nucleate_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    let molecule_id = nucleate_json["id"].as_str().unwrap().to_owned();
    write_minimal_config(tmp.path(), &state_dir, "");

    // `cs tackle` loads the formula via walk-up discovery of `.cosmon/formulas/`
    // (resolve_formulas_dir, case 3) — not the `--formulas-dir` passed to
    // nucleate. Mirror the real on-disk layout so the step's `adapter` pin is
    // visible at tackle time.
    let cosmon_formulas = tmp.path().join(".cosmon").join("formulas");
    fs::create_dir_all(&cosmon_formulas).unwrap();
    fs::write(
        cosmon_formulas.join("tackle-step-adapter-test.formula.toml"),
        &formula_toml,
    )
    .unwrap();

    (tmp, state_dir, molecule_id)
}

/// Drop a minimal `.cosmon/config.toml` (`[project]` + a stable
/// `project_id` + optional `[adapters]` block) at the project root.
fn write_minimal_config(project_root: &Path, _state_dir: &Path, adapters_block: &str) {
    let cosmon_dir = project_root.join(".cosmon");
    fs::create_dir_all(&cosmon_dir).unwrap();
    let body = format!("[project]\nproject_id = \"tackle-adapter-test-c6c6\"\n\n{adapters_block}",);
    fs::write(cosmon_dir.join("config.toml"), body).unwrap();
}

/// Read every `AdapterSelected` envelope from
/// `<state_dir>/events.jsonl` (the canonical write target of every
/// Worker-Spawn Port emit helper — see
/// [`cosmon_state::event_log::resolve_events_log_path`]).
fn read_adapter_selected_events(state_dir: &Path, _mol_id: &str) -> Vec<serde_json::Value> {
    let path = state_dir.join("events.jsonl");
    if !path.exists() {
        return Vec::new();
    }
    read_adapter_selected_from(&path)
}

fn read_adapter_selected_from(path: &Path) -> Vec<serde_json::Value> {
    let raw = fs::read_to_string(path).unwrap_or_default();
    raw.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        // `EventV2` is `#[serde(tag = "type")]` so the discriminator
        // lives at the top level of the envelope alongside the
        // variant's own fields — there is no nested `.event`.
        .filter(|v| v.get("type").and_then(serde_json::Value::as_str) == Some("adapter_selected"))
        .collect()
}

/// Locate the single `adapter_selected` envelope under the `state_dir`
/// — fails the test if zero or more-than-one exist. Returns the
/// inner `event` payload.
fn single_adapter_selected_event(state_dir: &Path, mol_id: &str) -> serde_json::Value {
    let events = read_adapter_selected_events(state_dir, mol_id);
    if events.is_empty() {
        // Surface what was actually written so the failure points at
        // the root cause instead of a generic "0 events" message.
        let raw = fs::read_to_string(state_dir.join("events.jsonl")).unwrap_or_default();
        panic!(
            "no adapter_selected event found. events.jsonl contents:\n{raw}\nlooked under: {}",
            state_dir.display(),
        );
    }
    assert_eq!(
        events.len(),
        1,
        "expected exactly one adapter_selected event, found {} (events: {events:#?})",
        events.len()
    );
    events.into_iter().next().unwrap()
}

/// `cs tackle <id> --dry-run` (no flag, no config) emits an
/// `AdapterSelected` with `selection_source = default` and
/// `adapter_name = "local"`.
///
/// **Local-first contract pin.** This is the mechanically-checkable
/// witness that a bare `cs tackle` routes to the in-process
/// Ollama-backed `local` adapter — NOT Claude Code. If a future change
/// re-defaults to `"claude"`, this assertion fails loudly. The
/// walking-skeleton deliverable's condition (i): "a bare `cs tackle`
/// (no `--adapter` flag) routes to a LOCAL model … no `claude` process
/// spawned" lives or dies here.
#[test]
fn tackle_without_flag_emits_default_source() {
    let (tmp, state_dir, mol_id) = setup_project_with_molecule();
    let output = cosmon_bin_in(tmp.path())
        .args([
            "tackle",
            &mol_id,
            "--dry-run",
            "--no-worktree",
            "--config",
            state_dir.to_str().unwrap(),
        ])
        .output()
        .expect("tackle failed");
    assert!(
        output.status.success(),
        "tackle should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let event = single_adapter_selected_event(&state_dir, &mol_id);
    assert_eq!(event["type"], "adapter_selected");
    assert_eq!(
        event["adapter_name"], "local",
        "bare `cs tackle` must default to the local (Ollama-backed) \
         adapter, NOT claude — local-first walking-skeleton contract"
    );
    let source = event.get("selection_source").expect("selection_source");
    assert_eq!(source["source"], "default", "source: {source}");
    assert!(
        event.get("role_hint").is_none()
            || event.get("role_hint") == Some(&serde_json::Value::Null),
        "no --role-hint → field absent or null, got: {event}"
    );
}

/// `cs tackle <id> --adapter aider --role-hint researcher --dry-run`
/// emits an `AdapterSelected` with `selection_source = cli` (carrying
/// the flag value) and `role_hint = "researcher"`.
#[test]
fn tackle_with_cli_flag_emits_cli_source_and_role_hint() {
    let (tmp, state_dir, mol_id) = setup_project_with_molecule();
    let output = cosmon_bin_in(tmp.path())
        .args([
            "tackle",
            &mol_id,
            "--adapter",
            "aider",
            "--role-hint",
            "researcher",
            "--dry-run",
            "--no-worktree",
            "--config",
            state_dir.to_str().unwrap(),
        ])
        .output()
        .expect("tackle failed");
    assert!(
        output.status.success(),
        "tackle should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let event = single_adapter_selected_event(&state_dir, &mol_id);
    assert_eq!(event["adapter_name"], "aider");
    assert_eq!(event["role_hint"], "researcher");
    let source = event.get("selection_source").expect("selection_source");
    assert_eq!(source["source"], "cli");
    assert_eq!(source["flag"], "aider");
}

/// `cs tackle <id> --adapter nope --dry-run` exits non-zero with
/// `AdapterNotFound` (message names the bad adapter and lists the
/// available ones). No `adapter_selected` event lands.
#[test]
fn tackle_with_unknown_adapter_fails_with_typed_diagnostic() {
    let (tmp, state_dir, mol_id) = setup_project_with_molecule();
    let output = cosmon_bin_in(tmp.path())
        .args([
            "tackle",
            &mol_id,
            "--adapter",
            "nope",
            "--dry-run",
            "--no-worktree",
            "--config",
            state_dir.to_str().unwrap(),
        ])
        .output()
        .expect("tackle failed to spawn");
    assert!(
        !output.status.success(),
        "tackle with unknown adapter must fail: stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nope"),
        "stderr must name the bad adapter: {stderr}"
    );
    assert!(
        stderr.contains("claude") && stderr.contains("aider"),
        "stderr must list available adapters: {stderr}"
    );
    // No event landed — the validation fires before the emit.
    let events = read_adapter_selected_events(&state_dir, &mol_id);
    assert!(
        events.is_empty(),
        "unknown-adapter fast-fail must not emit adapter_selected: {events:#?}"
    );
}

/// `[adapters.default] = "aider"` in `.cosmon/config.toml` is honoured
/// when no `--adapter` flag is passed; the emitted event carries
/// `selection_source = config`.
#[test]
fn tackle_honours_adapters_default_from_config() {
    let (tmp, state_dir, mol_id) = setup_project_with_molecule();
    // Overwrite the minimal config to include the [adapters] block.
    write_minimal_config(
        tmp.path(),
        &state_dir,
        "[adapters]\ndefault = \"aider\"\n\n[adapters.aider]\npane_signatures = [\"aider\"]\n",
    );

    let output = cosmon_bin_in(tmp.path())
        .args([
            "tackle",
            &mol_id,
            "--dry-run",
            "--no-worktree",
            "--config",
            state_dir.to_str().unwrap(),
        ])
        .output()
        .expect("tackle failed");
    assert!(
        output.status.success(),
        "tackle should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let event = single_adapter_selected_event(&state_dir, &mol_id);
    assert_eq!(event["adapter_name"], "aider");
    let source = event.get("selection_source").expect("selection_source");
    assert_eq!(source["source"], "config");
    assert_eq!(source["key"], "adapters.default");
}

/// `--adapter` (CLI) wins over `[adapters.default]` (config) — the
/// ADR-097 / C8 — `cs tackle --adapter aider` actually spawns
/// `aider`, not `claude`. Pre-C8 the `--adapter` flag was a
/// half-wired signal: it emitted `AdapterSelected("aider")` but
/// then routed through the Claude tmux path regardless. This test
/// gates that contract.
///
/// Gated `#[ignore]` per the PR-3 pattern: it requires real `aider`
/// and `tmux` on PATH. Local invocation:
///
/// ```bash
/// which aider tmux
/// cargo test -p cosmon-cli --test tackle_adapter_flag \
///   tackle_aider_flag_spawns_aider_binary -- --ignored
/// ```
///
/// The test inspects the captured tmux pane content for the string
/// `aider` (the binary name appears in Aider's startup banner and is
/// the cheapest possible structural witness — model-output is
/// non-deterministic by design).
#[test]
#[ignore = "requires real aider + tmux on PATH"]
fn tackle_aider_flag_spawns_aider_binary() {
    if Command::new("aider")
        .arg("--version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("aider not on PATH; skipping (covered by #[ignore])");
        return;
    }

    let (tmp, state_dir, mol_id) = setup_project_with_molecule();
    let socket = format!("cosmon-c8-{}", &mol_id[mol_id.len().saturating_sub(8)..]);

    let _ = Command::new("tmux")
        .args(["-L", &socket, "kill-server"])
        .output();

    let output = cosmon_bin_in(tmp.path())
        .env("COSMON_TMUX_SOCKET", &socket)
        .args([
            "tackle",
            &mol_id,
            "--adapter",
            "aider",
            "--no-worktree",
            "--config",
            state_dir.to_str().unwrap(),
        ])
        .output()
        .expect("tackle failed to spawn");

    let pane = Command::new("tmux")
        .args(["-L", &socket, "capture-pane", "-pS", "-200"])
        .output();
    let _ = Command::new("tmux")
        .args(["-L", &socket, "kill-server"])
        .output();

    let pane_text = pane
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();

    assert!(
        output.status.success(),
        "tackle should succeed: stderr={}, pane={pane_text}",
        String::from_utf8_lossy(&output.stderr),
    );

    // Structural witness: aider's banner / prompt mentions `aider`.
    // Pre-C8 the pane would have shown `claude` (the Claude Code TUI).
    assert!(
        pane_text.to_lowercase().contains("aider"),
        "pane must show evidence of aider running, got:\n{pane_text}",
    );
}

/// A formula step that pins `adapter =
/// "claude"` is honoured when no `--adapter` flag is passed, even though
/// the galaxy default would otherwise be the `local` floor. The emitted
/// event carries `selection_source = formula_step` with the formula name
/// and step id — the per-workflow override seam.
#[test]
fn tackle_honours_formula_step_adapter_pin() {
    let (tmp, state_dir, mol_id) = setup_project_with_step_adapter("claude");
    let output = cosmon_bin_in(tmp.path())
        .args([
            "tackle",
            &mol_id,
            "--dry-run",
            "--no-worktree",
            "--config",
            state_dir.to_str().unwrap(),
        ])
        .output()
        .expect("tackle failed");
    assert!(
        output.status.success(),
        "tackle should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let event = single_adapter_selected_event(&state_dir, &mol_id);
    assert_eq!(
        event["adapter_name"], "claude",
        "step pin must override the local floor"
    );
    let source = event.get("selection_source").expect("selection_source");
    assert_eq!(source["source"], "formula_step", "source: {source}");
    assert_eq!(source["formula"], "tackle-step-adapter-test");
    assert_eq!(source["step_id"], "step-1");
}

/// Q5a: the `--adapter` flag (rank 1) still wins over a formula-step pin
/// (rank 2). `--adapter local` on a step that pins `claude` resolves to
/// `local` with `selection_source = cli`.
#[test]
fn tackle_cli_flag_wins_over_formula_step_adapter() {
    let (tmp, state_dir, mol_id) = setup_project_with_step_adapter("claude");
    let output = cosmon_bin_in(tmp.path())
        .args([
            "tackle",
            &mol_id,
            "--adapter",
            "local",
            "--dry-run",
            "--no-worktree",
            "--config",
            state_dir.to_str().unwrap(),
        ])
        .output()
        .expect("tackle failed");
    assert!(
        output.status.success(),
        "tackle should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let event = single_adapter_selected_event(&state_dir, &mol_id);
    assert_eq!(event["adapter_name"], "local");
    let source = event.get("selection_source").expect("selection_source");
    assert_eq!(source["source"], "cli");
    assert_eq!(source["flag"], "local");
}

/// `--fallback-from-local` onto a *local*
/// adapter is a contradiction and fails fast — before any worktree or
/// tmux side effect. You cannot "fall back to a remote oracle" while
/// still pointing at the local default.
#[test]
fn tackle_fallback_from_local_onto_local_adapter_is_rejected() {
    let (tmp, state_dir, mol_id) = setup_project_with_molecule();
    let output = cosmon_bin_in(tmp.path())
        .args([
            "tackle",
            &mol_id,
            "--adapter",
            "local",
            "--fallback-from-local",
            "timeout",
            "--dry-run",
            "--no-worktree",
            "--config",
            state_dir.to_str().unwrap(),
        ])
        .output()
        .expect("tackle failed to spawn");
    assert!(
        !output.status.success(),
        "fallback onto a local adapter must fail: stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("REMOTE") && stderr.contains("local"),
        "stderr must explain the contradiction: {stderr}"
    );
}

/// Q5b: a blank `--fallback-from-local` cause is refused — the loud line
/// must always name a decidable cause.
#[test]
fn tackle_fallback_from_local_blank_cause_is_rejected() {
    let (tmp, state_dir, mol_id) = setup_project_with_molecule();
    let output = cosmon_bin_in(tmp.path())
        .args([
            "tackle",
            &mol_id,
            "--adapter",
            "claude",
            "--fallback-from-local",
            "   ",
            "--dry-run",
            "--no-worktree",
            "--config",
            state_dir.to_str().unwrap(),
        ])
        .output()
        .expect("tackle failed to spawn");
    assert!(
        !output.status.success(),
        "blank fallback cause must fail: stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("non-empty cause"),
        "stderr must demand a non-empty cause: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// briefing-named resolution order is honoured: CLI → config →
/// built-in.
#[test]
fn tackle_cli_flag_wins_over_adapters_default_in_config() {
    let (tmp, state_dir, mol_id) = setup_project_with_molecule();
    write_minimal_config(tmp.path(), &state_dir, "[adapters]\ndefault = \"aider\"\n");

    let output = cosmon_bin_in(tmp.path())
        .args([
            "tackle",
            &mol_id,
            "--adapter",
            "claude",
            "--dry-run",
            "--no-worktree",
            "--config",
            state_dir.to_str().unwrap(),
        ])
        .output()
        .expect("tackle failed");
    assert!(
        output.status.success(),
        "tackle should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let event = single_adapter_selected_event(&state_dir, &mol_id);
    assert_eq!(event["adapter_name"], "claude");
    let source = event.get("selection_source").expect("selection_source");
    assert_eq!(source["source"], "cli");
    assert_eq!(source["flag"], "claude");
}
