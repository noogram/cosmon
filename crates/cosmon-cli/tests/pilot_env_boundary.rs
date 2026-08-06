// SPDX-License-Identifier: AGPL-3.0-only

//! The hermetic boundary between a worker's pilotage environment and its
//! verification gates — falsified from both sides.
//!
//! # What is being pinned
//!
//! `cs tackle` steers a worker through environment variables. The worker then
//! runs `just gates`, and `cargo test` inherits them, so test processes read
//! instructions addressed to their parent. Three times that produced a false
//! verdict — `COSMON_EGRESS_POLICY`, `CB_DEPTH`, `ANTHROPIC_MODEL` — and on
//! 2026-08-06 it collapsed a healthy molecule (`task-20260804-2bbb`, work
//! intact at `226b9b0d`).
//!
//! `scripts/no-pilot-env.sh` is the boundary and
//! [`cosmon_core::pilot_env`] is its single source of truth. These tests fail
//! if the boundary is removed, if the two lists drift apart, or if the
//! boundary ever spreads onto a runtime path.
//!
//! # Why the falsifier runs the real `cs` binary
//!
//! Asserting that the script unsets some names proves the `env -u` call, not
//! that the poisoning stops — and the poisoning is the whole defect. So
//! [`depth_guard_fires_without_the_boundary_and_not_through_it`] reproduces
//! the exact mechanism that reddened eleven suites: `CB_DEPTH` in the
//! environment makes a real `cs tackle` refuse with exit 14, and the same
//! command through the boundary dispatches. Delete the `env -u` list and that
//! test goes red immediately.
//!
//! # The inverse direction, which matters just as much
//!
//! `deny-external` confining a local adapter is a real security guarantee, and
//! the fix for a test poisoned by it must never be to weaken it. Two tests
//! hold that line: [`egress_policy_still_fails_closed_when_absent`] (the
//! adapter's own resolution is unchanged — absence still means denial) and
//! [`boundary_is_never_applied_to_a_runtime_path`] (the script is reachable
//! from the gate recipes and from no shipped code).

use cosmon_core::pilot_env::{self, PilotVar};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Repo root, from this crate's manifest dir (`crates/cosmon-cli`).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/cosmon-cli always has two ancestors")
        .to_path_buf()
}

fn boundary_script() -> PathBuf {
    repo_root().join("scripts/no-pilot-env.sh")
}

/// The three variables that actually produced a false verdict, with values in
/// the shape that did it.
const KNOWN_POISONS: &[(&str, &str)] = &[
    ("COSMON_EGRESS_POLICY", "deny-external"),
    ("CB_DEPTH", "5"),
    ("ANTHROPIC_MODEL", "claude-opus-5"),
];

/// The shell projection is exactly the Rust manifest, in order.
///
/// This is what stops the list rotting. `cs tackle` cannot inject a variable
/// that is not a [`PilotVar`] variant (it emits through `PilotVar::name`), and
/// this test refuses any variant the boundary would not strip — so a new pilot
/// variable is enrolled in the boundary at the moment it is invented, not the
/// next time someone remembers the shell script exists.
#[test]
fn script_projection_matches_the_rust_manifest() {
    let out = Command::new(boundary_script())
        .arg("--list")
        .output()
        .expect("boundary script must be executable");
    assert!(out.status.success(), "`--list` must exit 0");
    let script_list: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    let manifest: Vec<String> = pilot_env::names().into_iter().map(str::to_owned).collect();
    assert_eq!(
        script_list, manifest,
        "scripts/no-pilot-env.sh has drifted from cosmon_core::pilot_env::PilotVar — \
         a pilot variable the gates still inherit is a false verdict waiting to happen"
    );
}

/// Every historically poisonous variable is in the manifest, by name.
///
/// Named individually rather than counted, because these three are the reason
/// the boundary exists and a regression that dropped one of them should say
/// which one.
#[test]
fn the_three_known_poisons_are_declared() {
    let names = pilot_env::names();
    for (var, _) in KNOWN_POISONS {
        assert!(
            names.contains(var),
            "{var} poisoned a real gate run and must stay in the manifest"
        );
    }
}

/// The boundary removes the poisons from the child's environment — and the
/// same command without the boundary keeps them.
///
/// The negative control is the load-bearing half: without it this test would
/// pass just as happily in an environment that never had the variables, which
/// is the "check that measures the property next door" this repo keeps finding.
#[test]
fn boundary_strips_the_poisons_and_the_control_proves_it_measures_that() {
    let mut through = Command::new(boundary_script());
    through.arg("/usr/bin/env");
    let mut without = Command::new("/usr/bin/env");
    for (var, value) in KNOWN_POISONS {
        through.env(var, value);
        without.env(var, value);
    }

    let stripped = String::from_utf8_lossy(
        &through
            .output()
            .expect("boundary script must run /usr/bin/env")
            .stdout,
    )
    .into_owned();
    let inherited =
        String::from_utf8_lossy(&without.output().expect("/usr/bin/env must run").stdout)
            .into_owned();

    for (var, value) in KNOWN_POISONS {
        let assignment = format!("{var}={value}");
        assert!(
            inherited.lines().any(|l| l == assignment),
            "negative control failed: {var} did not reach the unguarded child, \
             so this test is not measuring the boundary"
        );
        assert!(
            !stripped.lines().any(|l| l.starts_with(&format!("{var}="))),
            "{var} survived the boundary"
        );
    }
}

/// The boundary is transparent to the verdict it guards.
///
/// A boundary that swallowed a gate's exit status would be the very defect it
/// exists to prevent — a green run that means nothing. The script `exec`s, so
/// there is no shell left in between.
#[test]
fn boundary_passes_the_exit_status_through() {
    let status = Command::new(boundary_script())
        .args(["sh", "-c", "exit 7"])
        .status()
        .expect("boundary script must run sh");
    assert_eq!(status.code(), Some(7));
}

/// **The falsifier.** `CB_DEPTH` in the environment makes a real `cs tackle`
/// refuse; the identical command through the boundary dispatches.
///
/// This is the mechanism that reddened four tackle suites plus seven others
/// (`1588 passed / 0 failed` once removed), reproduced on the shipped binary
/// rather than argued about. Exit 14 is `GuardError::DepthLimitExceeded`.
///
/// Empty the `env -u` list in `scripts/no-pilot-env.sh` and this test fails on
/// its second assertion.
#[test]
fn depth_guard_fires_without_the_boundary_and_not_through_it() {
    let galaxy = tempfile::tempdir().expect("tempdir");
    let root = galaxy.path();

    // A galaxy of its own, so `cs`'s walk-up discovery stops here rather than
    // finding the checkout this test runs inside.
    run_ok(
        Command::new("git")
            .args(["init", "-q", "."])
            .current_dir(root),
    );
    run_ok(clean_cs().arg("init").current_dir(root));
    let nucleated = clean_cs()
        .args([
            "nucleate",
            "task-work",
            "--var",
            "topic=pilot-env boundary probe",
            "--no-parent",
        ])
        .current_dir(root)
        .output()
        .expect("cs nucleate must run");
    assert!(nucleated.status.success(), "cs nucleate: {nucleated:?}");
    let mol_id = String::from_utf8_lossy(&nucleated.stdout)
        .split_whitespace()
        .find(|tok| tok.starts_with("task-"))
        .expect("nucleate announces the molecule id")
        .to_owned();

    // Poisoned: the worker's own CB_DEPTH is read as this dispatch's depth.
    let poisoned = clean_cs()
        .args(["tackle", &mol_id, "--dry-run", "--no-worktree"])
        .env("CB_DEPTH", "5")
        .current_dir(root)
        .output()
        .expect("cs tackle must run");
    assert_eq!(
        poisoned.status.code(),
        Some(14),
        "expected the Gödel depth refusal — if this changed, the falsifier \
         below is no longer measuring anything: {}",
        String::from_utf8_lossy(&poisoned.stderr)
    );

    // Through the boundary: the same environment, the same command, dispatched.
    let guarded = Command::new(boundary_script())
        .arg(cs_bin())
        .args(["tackle", &mol_id, "--dry-run", "--no-worktree"])
        .env("CB_DEPTH", "5")
        .current_dir(root)
        .output()
        .expect("cs tackle must run through the boundary");
    assert_eq!(
        guarded.status.code(),
        Some(0),
        "the boundary did not strip CB_DEPTH: {}",
        String::from_utf8_lossy(&guarded.stderr)
    );
}

/// The egress control is untouched by all of this: absence still denies.
///
/// The temptation the boundary creates is to treat `COSMON_EGRESS_POLICY` as
/// noise because it once reddened a test. It is not noise — it is the fail-
/// closed switch a local adapter's confinement rests on, and stripping it at
/// the gate boundary must not have relaxed it anywhere else.
#[test]
fn egress_policy_still_fails_closed_when_absent() {
    use cosmon_core::egress::EgressPolicy;
    assert_eq!(
        EgressPolicy::from_env_value(None),
        EgressPolicy::DenyExternal,
        "an absent policy must still deny — the boundary is a gate concern, \
         never a relaxation of confinement"
    );
    assert_eq!(
        PilotVar::EgressPolicy.name(),
        EgressPolicy::ENV_VAR,
        "the manifest must track the security module's own name"
    );
}

/// The boundary is a *gate* mechanism and must never reach a runtime path.
///
/// Scans the tracked tree for references to the script: the gate recipes, CI,
/// docs and this test may name it; nothing under `crates/*/src`, `apps/`,
/// `docker/` or `templates/` may *invoke* it. A worker or adapter that ran
/// itself through the boundary would strip its own confinement — the one
/// outcome that would make this change a security regression rather than a fix.
///
/// Naming the script on those paths is allowed, deliberately: the emitter in
/// `tackle_env.rs` explains where its variables are stripped, and the canary's
/// remedy message has to name the way out or it is a diagnosis without a cure.
/// A rule that forbade saying the name would push the explanation away from
/// the code it explains. What is forbidden is a line that *runs* it, which on
/// these paths means a line that also carries a process-spawn construct.
///
/// That is a shape heuristic, and it is stated as one: it catches the way a
/// Rust or shell runtime path would actually invoke the script, not every
/// conceivable indirection. It is the second lock, behind the first — there is
/// exactly one script and it is reviewed as a whole.
#[test]
fn boundary_is_never_applied_to_a_runtime_path() {
    let out = Command::new("git")
        .args(["grep", "-n", "no-pilot-env.sh"])
        .current_dir(repo_root())
        .output()
        .expect("git grep must run");
    // `<path>:<line-no>:<line>`. An empty result cannot happen — the justfile
    // references the script — and is asserted against below rather than
    // allowed to pass silently, since it would make this test vacuous.
    let hits: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    assert!(
        hits.iter().any(|h| h.starts_with("justfile:")),
        "the justfile must reference the boundary; got {hits:?}"
    );
    // Positive control. A test whose detector never fires is a test that would
    // stay green through the very change it exists to catch, so prove the
    // classifier still recognises an invocation before trusting its silence.
    assert!(
        applies_the_boundary(
            r#"crates/cosmon-cli/src/cmd/tackle.rs:9: Command::new("scripts/no-pilot-env.sh")"#
        ),
        "the invocation detector stopped detecting invocations"
    );
    assert!(
        !applies_the_boundary("justfile:210:    ./scripts/no-pilot-env.sh cargo test"),
        "the gate recipe is where the boundary belongs"
    );

    let forbidden: Vec<&String> = hits
        .iter()
        .filter(|hit| applies_the_boundary(hit))
        .collect();
    assert!(
        forbidden.is_empty(),
        "the pilot-env boundary reached a runtime path: {forbidden:?} — \
         stripping COSMON_EGRESS_POLICY outside a gate would weaken a real jail"
    );
}

/// The canary: a test process that finds pilot variables in its own
/// environment can say so, by name.
///
/// The residual hole the manifest cannot close is a pilot variable set by
/// something other than `cs tackle` — an operator's shell, a frozen tmux
/// server env. This is the second line of defence for that case: one named
/// red instead of N unattributable ones.
#[test]
fn canary_names_the_breach_and_the_remedy() {
    let found = pilot_env::detect_in(|k| {
        KNOWN_POISONS
            .iter()
            .find(|(var, _)| *var == k)
            .map(|(_, value)| (*value).to_owned())
    });
    assert_eq!(found.len(), KNOWN_POISONS.len());
    let msg = pilot_env::canary_message(&found);
    for (var, _) in KNOWN_POISONS {
        assert!(msg.contains(var), "the canary must name {var}");
    }
    assert!(
        msg.contains("no-pilot-env.sh"),
        "a diagnosis without a remedy is the manual diagnosis this molecule \
         exists to stop repeating"
    );
}

/// **The live canary.** This very test process must not be inside a worker's
/// pilotage environment.
///
/// Every other test here proves the boundary *works*; this one proves it was
/// *used*. It is the whole difference between a mechanism and a convention:
/// without it, a worker who runs `cargo test --workspace` by hand still gets
/// the eleven unattributable reds this molecule exists to stop, and the
/// boundary in the `justfile` never fires.
///
/// When it fails, it fails with the names and the remedy — one red that says
/// what happened, instead of N that each need a manual diagnosis. The remedy
/// is `just gates` / `just quick`, or `./scripts/no-pilot-env.sh cargo test …`
/// for a scoped run.
///
/// CI is already hermetic (a runner has no `cs tackle` above it), so this is a
/// no-op there. It fires exactly in the situation that has cost three manual
/// diagnoses and one killed molecule.
#[test]
fn this_test_process_is_outside_the_pilotage_environment() {
    let found = pilot_env::detect_in(|k| std::env::var(k).ok());
    assert!(found.is_empty(), "{}", pilot_env::canary_message(&found));
}

/// Classify one `git grep -n` hit: does this line *apply* the boundary from a
/// runtime path?
///
/// Two conditions, both required. The path must be shipped code rather than a
/// gate recipe, CI job, doc or test; and the line must carry a process-spawn
/// construct, because naming the script (in a comment, or in the canary's
/// remedy message) documents the boundary while spawning it applies it.
fn applies_the_boundary(hit: &str) -> bool {
    /// How a runtime path would actually reach the script.
    const SPAWN_SHAPES: &[&str] = &[
        "Command::new",
        "process::Command",
        "exec ",
        "sh -c",
        "system(",
        "subprocess",
        "spawn(",
    ];
    let (path, rest) = hit.split_once(':').unwrap_or((hit, ""));
    let on_runtime_path = path.starts_with("apps/")
        || path.starts_with("docker/")
        || path.starts_with("templates/")
        || (path.starts_with("crates/") && path.contains("/src/"));
    let line = rest.split_once(':').map_or("", |(_, l)| l).trim_start();
    let is_comment = line.starts_with("//") || line.starts_with('#') || line.starts_with('*');
    on_runtime_path && !is_comment && SPAWN_SHAPES.iter().any(|shape| line.contains(shape))
}

fn cs_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cs")
}

/// A `cs` invocation with this test process's own pilotage environment
/// removed, so the fixture's behaviour does not depend on how the test run was
/// launched. The falsifier then puts back exactly the one variable it studies.
fn clean_cs() -> Command {
    let mut cmd = Command::new(cs_bin());
    for name in pilot_env::names() {
        cmd.env_remove(name);
    }
    cmd
}

fn run_ok(cmd: &mut Command) {
    let out = cmd.output().expect("fixture command must run");
    assert!(out.status.success(), "fixture command failed: {out:?}");
}
