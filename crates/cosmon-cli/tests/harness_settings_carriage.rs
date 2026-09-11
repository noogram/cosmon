// SPDX-License-Identifier: AGPL-3.0-only

//! Falsifiers 1, 2, 3 and 6 of ADR-177 / issue #65: the harness-settings map
//! travels from a formula step (or a `--harness` flag) to the bytes on an
//! adapter's command line, and leaves a receipt on `events.jsonl` that names
//! everything a reader needs to interpret it.
//!
//! These are end-to-end over the *carriage*, not over a spawned process. That
//! boundary is the point of ADR-177 Decision 4: no test in this repository can
//! show that a harness **ran** at a setting, because cosmon has no access to
//! that fact. What it can show — and what these tests show — is that cosmon
//! *dispatched* the setting through a named channel, and that the argv it
//! recorded is the argv it sent.

use std::path::{Path, PathBuf};

use cosmon_core::event_v2::{EventV2, HarnessLaunchStatus};
use cosmon_core::formula::Formula;
use cosmon_core::harness_settings::{
    parse_harness_flags, render_harness_args, resolve_harness_settings, HarnessMap,
};
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_state::events::worker_spawn::emit_harness_settings_selected;
use cosmon_transport::codex::{build_codex_command, CodexMode, CodexSessionConfig};

/// A formula whose single worker-spawn step pins two harness keys on codex.
/// Two, not one, because the per-key merge is only observable with a sibling.
const FORMULA: &str = r#"
formula = "harness-demo"
version = 1
description = "a step pinning harness settings on codex"

[[steps]]
id = "implement"
title = "Implement"
description = "Needs a deliberate model."
adapter = "codex"

[steps.harness]
model_reasoning_effort = "high"
model_max_output_tokens = "4096"
"#;

/// The harness map the executing step of [`FORMULA`] pins, as
/// `resolve_selection` reads it.
fn step_pin() -> (Formula, &'static str, &'static str) {
    let formula = Formula::parse(FORMULA).expect("the fixture formula parses");
    (formula, "harness-demo", "implement")
}

fn codex_config(harness_args: Vec<String>) -> CodexSessionConfig {
    CodexSessionConfig {
        socket: "cosmon".to_owned(),
        session_name: "harness-demo-codex".to_owned(),
        work_dir: "/state/wt".to_owned(),
        binary: PathBuf::from("codex"),
        prompt: None,
        mode: CodexMode::Interactive,
        model: None,
        extra_args: vec![],
        telemetry: None,
        pre_existing_worker: None,
        git_identity: None,
        writable_roots: vec![],
        harness_args,
    }
}

fn argv(args: &[cosmon_core::harness_settings::HarnessArg]) -> Vec<String> {
    args.iter().flat_map(|a| a.argv.iter().cloned()).collect()
}

/// **Falsifier 1.** A step pin `model_reasoning_effort = "high"` on codex
/// produces `-c model_reasoning_effort=high` in the realized argv.
#[test]
fn a_step_pin_reaches_the_codex_command_line_as_a_dash_c_override() {
    let (formula, name, step_id) = step_pin();
    let step = &formula.steps[0];
    let resolved =
        resolve_harness_settings(&HarnessMap::new(), Some((&step.harness, name, step_id)));

    let args = render_harness_args("codex", &resolved).expect("codex carries the map");
    let cmd = build_codex_command(&codex_config(argv(&args)));

    assert!(
        cmd.contains("-c model_reasoning_effort=high"),
        "the pin must reach the command line verbatim: {cmd}"
    );
    assert!(
        cmd.contains("-c model_max_output_tokens=4096"),
        "both pinned keys travel, not just the first: {cmd}"
    );
    // The recorded fragment IS the argv, not a reconstruction of it — the
    // property that makes it diffable against codex's own echo.
    for arg in &args {
        assert!(
            cmd.contains(&arg.argv_fragment()),
            "the recorded fragment `{}` must appear in the command: {cmd}",
            arg.argv_fragment()
        );
    }
}

/// **Falsifier 2.** `--harness model_reasoning_effort=low` overrides the pin,
/// and the *second* key on the pin survives the override.
///
/// The second half is the whole of ADR-177 Decision 2's "merged per key". Under
/// wholesale replacement the flag would win and `model_max_output_tokens` would
/// vanish from the command line without anyone being told.
#[test]
fn a_flag_overrides_one_key_and_leaves_its_sibling_on_the_command_line() {
    let (formula, name, step_id) = step_pin();
    let step = &formula.steps[0];
    let flag = parse_harness_flags(&["model_reasoning_effort=low"]).expect("well-formed pair");
    let resolved = resolve_harness_settings(&flag, Some((&step.harness, name, step_id)));

    let args = render_harness_args("codex", &resolved).expect("codex carries the map");
    let cmd = build_codex_command(&codex_config(argv(&args)));

    assert!(
        cmd.contains("-c model_reasoning_effort=low"),
        "the flag wins for the key it names: {cmd}"
    );
    assert!(
        !cmd.contains("model_reasoning_effort=high"),
        "the overridden value must not also be sent: {cmd}"
    );
    assert!(
        cmd.contains("-c model_max_output_tokens=4096"),
        "the key the flag did not name must survive the override: {cmd}"
    );
}

/// **Falsifier 3.** `events.jsonl` carries each resolved key with its
/// `selection_source`, argv fragment, channel, harness version and launch
/// status.
#[test]
fn each_resolved_key_lands_on_events_jsonl_with_its_full_receipt() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path();

    let (formula, name, step_id) = step_pin();
    let step = &formula.steps[0];
    let flag = parse_harness_flags(&["model_reasoning_effort=low"]).expect("well-formed pair");
    let resolved = resolve_harness_settings(&flag, Some((&step.harness, name, step_id)));
    let args = render_harness_args("codex", &resolved).expect("codex carries the map");

    let mol_id = MoleculeId::new("task-20260911-345c").expect("valid id");
    let worker = WorkerId::new("harness-demo-codex").expect("valid worker id");
    emit_harness_settings_selected(
        state_dir,
        &mol_id,
        &worker,
        "codex",
        &args,
        Some("codex-cli 0.153.0"),
        HarnessLaunchStatus::Launched,
    );

    let receipts = harness_receipts(state_dir);
    assert_eq!(receipts.len(), 2, "one line per resolved key, not per map");

    let effort = receipts
        .iter()
        .find(|(key, ..)| key == "model_reasoning_effort")
        .expect("the flag-sourced key is on the wire");
    assert_eq!(effort.1, "low");
    assert_eq!(effort.2.tag(), "flag", "the flag rung is recorded as such");
    assert_eq!(effort.3, "-c model_reasoning_effort=low");
    assert_eq!(effort.4, "codex:-c");
    assert_eq!(effort.5.as_deref(), Some("codex-cli 0.153.0"));
    assert_eq!(effort.6, HarnessLaunchStatus::Launched);

    let tokens = receipts
        .iter()
        .find(|(key, ..)| key == "model_max_output_tokens")
        .expect("the step-sourced key is on the wire");
    assert_eq!(
        tokens.2.tag(),
        "formula",
        "a key that came from the step pin must say so"
    );
    assert_eq!(tokens.3, "-c model_max_output_tokens=4096");
}

/// A launch that never came up records the keys it was carrying and says so.
///
/// The asymmetry this guards: a harness that rejected a flag at launch produces
/// no ex-post echo, and so does a harness that launched fine and never reports
/// the axis. Without `launch_status` the two are the same silence, and the
/// comfortable reading of that silence is the wrong one.
#[test]
fn a_failed_launch_is_recorded_as_such_rather_than_as_silence() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path();
    let flag = parse_harness_flags(&["effort=xhigh"]).expect("well-formed pair");
    let resolved = resolve_harness_settings(&flag, None);
    let args = render_harness_args("claude", &resolved).expect("claude carries the map");

    emit_harness_settings_selected(
        state_dir,
        &MoleculeId::new("task-20260911-345c").expect("valid id"),
        &WorkerId::new("harness-demo-claude").expect("valid worker id"),
        "claude",
        &args,
        None,
        HarnessLaunchStatus::LaunchFailed,
    );

    let receipts = harness_receipts(state_dir);
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].3, "--effort xhigh");
    assert_eq!(receipts[0].4, "claude:flag");
    assert_eq!(
        receipts[0].5, None,
        "an unprobeable version is an absence, never a claim"
    );
    assert_eq!(receipts[0].6, HarnessLaunchStatus::LaunchFailed);
}

/// **Falsifier 6.** A non-empty map reaching an adapter with no carrier fails,
/// naming the adapter — it is never silently dropped.
///
/// opencode is the case that motivates the wording: it is explicitly out of
/// #65's scope (it drops even the shipped `--model` pin, filed separately), so
/// a harness map reaching it would be dropped twice over if this refused
/// quietly.
#[test]
fn an_adapter_with_no_channel_refuses_and_names_itself() {
    let flag = parse_harness_flags(&["model_reasoning_effort=high"]).expect("well-formed pair");
    let resolved = resolve_harness_settings(&flag, None);

    for adapter in ["opencode", "aider", "local", "openai", "anthropic"] {
        let err = render_harness_args(adapter, &resolved)
            .unwrap_err_named(adapter);
        assert!(
            err.contains(adapter),
            "the refusal must name the adapter that cannot carry the map: {err}"
        );
        assert!(
            err.contains("model_reasoning_effort"),
            "and the key the operator must remove: {err}"
        );
    }
}

/// A dispatch that pins nothing leaves every adapter's command byte-identical
/// to the pre-#65 shape — including the adapters that have no channel at all.
///
/// This is the property that makes the feature free: the refusal above fires on
/// a pin, never on an adapter.
#[test]
fn pinning_nothing_changes_no_command_on_any_adapter() {
    let empty = resolve_harness_settings(&HarnessMap::new(), None);
    for adapter in ["opencode", "aider", "local", "codex", "claude"] {
        assert!(
            render_harness_args(adapter, &empty)
                .expect("an empty map is carried everywhere")
                .is_empty(),
            "{adapter} must render no tokens for an empty map"
        );
    }
    // codex's command already carries one structural `-c`
    // (`check_for_update_on_startup=false`, NO_STARTUP_UPDATE_OVERRIDE), which
    // is cosmon's own override and not a harness setting. So the property is
    // *no additional* `-c`, counted — not the absence of the token.
    let bare = build_codex_command(&codex_config(vec![]));
    assert_eq!(
        bare.matches(" -c ").count(),
        1,
        "no harness pin → only cosmon's own structural `-c` survives: {bare}"
    );
}

/// Harness settings share codex's `-c` channel with cosmon's own structural
/// override, and must be **additive** to it rather than replacing it.
///
/// The failure this pins is quiet: a harness pin that displaced
/// `check_for_update_on_startup=false` would leave every codex worker able to
/// stall on a startup update prompt, and nothing about the pin would look
/// wrong.
#[test]
fn a_harness_pin_is_additive_to_cosmons_own_structural_override() {
    let flag = parse_harness_flags(&["model_reasoning_effort=high"]).expect("well-formed pair");
    let resolved = resolve_harness_settings(&flag, None);
    let args = render_harness_args("codex", &resolved).expect("codex carries the map");
    let cmd = build_codex_command(&codex_config(argv(&args)));

    assert!(
        cmd.contains("-c check_for_update_on_startup=false"),
        "cosmon's structural override must survive a harness pin: {cmd}"
    );
    assert!(
        cmd.contains("-c model_reasoning_effort=high"),
        "and the pin must be there beside it: {cmd}"
    );
    assert_eq!(
        cmd.matches(" -c ").count(),
        2,
        "exactly one structural override plus one pinned key: {cmd}"
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

type Receipt = (
    String,
    String,
    cosmon_core::harness_settings::HarnessSelectionSource,
    String,
    String,
    Option<String>,
    HarnessLaunchStatus,
);

/// Read back every `harness_setting_selected` line from a state dir's event
/// log — the same fold an acceptance run performs with `jq`.
fn harness_receipts(state_dir: &Path) -> Vec<Receipt> {
    let log = cosmon_state::event_log::resolve_events_log_path(state_dir);
    cosmon_state::event_log::read_all(&log)
        .expect("the event log is readable")
        .into_iter()
        .filter_map(|env| match env.event {
            EventV2::HarnessSettingSelected {
                key,
                value,
                selection_source,
                argv_fragment,
                channel,
                harness_version,
                launch_status,
                ..
            } => Some((
                key,
                value,
                selection_source,
                argv_fragment,
                channel,
                harness_version,
                launch_status,
            )),
            _ => None,
        })
        .collect()
}

/// `expect_err` with a message that names which adapter was expected to refuse,
/// so a regression reads as "codex started refusing" rather than as a bare
/// unwrap panic.
trait UnwrapErrNamed {
    fn unwrap_err_named(self, adapter: &str) -> String;
}

impl<T: std::fmt::Debug> UnwrapErrNamed
    for Result<T, cosmon_core::harness_settings::UnsupportedHarnessCarrier>
{
    fn unwrap_err_named(self, adapter: &str) -> String {
        match self {
            Ok(v) => panic!("adapter '{adapter}' was expected to refuse a non-empty harness map, but it rendered {v:?}"),
            Err(e) => e.to_string(),
        }
    }
}
