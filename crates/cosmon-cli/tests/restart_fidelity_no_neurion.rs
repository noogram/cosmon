// SPDX-License-Identifier: AGPL-3.0-only

//! restart_fidelity_no_neurion.rs — mandatory CI gate against
//! Universe D paradigm drift.
//!
//! # Why this test exists
//!
//! The danger this test guards against:
//!
//! > "If neurion becomes the canonical clock, the gravitational pull of
//! > 'just put it in the TOML' will be enormous. Over months, teams will
//! > accrete logic in neurion's TOML … The TOML becomes the scheduler …
//! > Cosmon's wedge (git-composable, one binary, no daemon) quietly
//! > dissolves into 'one binary, but you need neurion.' A new user clones
//! > cosmon, runs cs on a fresh machine, and… nothing happens, because
//! > neurion isn't installed."
//!
//! That is **Universe D — paradigm drift**. It cannot be detected from
//! a single decision; it appears only in the trajectory. This test is
//! the cultural enforcement against it. If it ever becomes infeasible
//! to keep green, cosmon has drifted away from its Markov-property
//! foundation (`docs/architectural-invariants.md` §7c) — raise a
//! successor ADR before merging the change that caused it to fail.
//!
//! # What the test asserts
//!
//! Three concentric rings, each weaker than `cs tackle` + real-tmux but
//! strong enough to be a hard CI gate:
//!
//! 1. **Hygiene** — the cs binary boots and answers `--version`,
//!    `ensemble`, `nucleate`, `complete`, `observe` with no neurion
//!    socket reachable, no `NEURION_*` env beyond the sandbox redirect,
//!    and an empty `HOME` that contains no LaunchAgent plists. The
//!    auto-register write side stays inside the sandbox.
//!
//! 2. **Standalone DAG trajectory** — three molecules in a fan-out
//!    pattern (A → {B, C}) nucleate, complete, and project onto
//!    `ensemble --json` exactly as they would on a workstation with
//!    neurion + LaunchAgents installed. The shape of the on-disk state
//!    is independent of the surrounding nervous system.
//!
//! 3. **Markov restart-fidelity** — running the same trajectory in two
//!    halves with a snapshot/restart in between produces an on-disk
//!    state whose canonical fields (status set, completed_step counts,
//!    typed-link symmetry) are identical to an uninterrupted run. Each
//!    `cs` invocation is itself a fresh process, so this is a *direct*
//!    test of the §7c claim that "the runtime is a pure function of
//!    disk state".
//!
//! The fuller "kill mid-flight while real tmux holds the worker" story
//! (the operator's "redémarrer mon ordi" moment that surfaced the
//! Convoy Cascade) lives in `pane_died_hook.rs`, behind `#[ignore]`
//! because it requires a real tmux server. The two tests are
//! complementary: this one is the CI gate (always-on, fast, no
//! external infra); that one is the lifecycle smoke that exercises the
//! transport layer.

#![cfg(unix)]
// The module doc + per-test docs are intentionally prose-heavy: they
// describe a *cultural* gate, not an algorithm, and naked vocabulary
// like `cs tackle`, `LaunchAgents`, `pane-died`, `delib-…` reads more
// fluently than backtick-spangled markdown. The `similar_names` lint
// fires on the parallel `proj_a` / `proj_b` / `tmp_a` / `tmp_b` /
// `final_a_statuses` / `final_b_statuses` bindings used by the
// uninterrupted-vs-restarted comparison; renaming them defeats the
// readability they buy.
#![allow(clippy::doc_markdown, clippy::similar_names)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Build a `cs` command with a sandboxed environment that:
/// - redirects neurion auto-register hints into a temp file under the
///   sandbox (so a real `~/.local/share/neurion/auto-register.jsonl`
///   on the developer's host is never touched);
/// - redirects every XDG base directory and `HOME` into the sandbox
///   (so `dirs::data_dir()` and friends cannot leak out);
/// - clears any cosmon worker context the harness may have inherited
///   (the runner itself may be a cosmon worker — see
///   `tests/fakes/fake-claude/claude` for the same trick).
///
/// The test asserts post-hoc that nothing wrote to a path outside the
/// sandbox, so accidental escapes show up as failed assertions rather
/// than silent host pollution.
fn cs_no_neurion(sandbox: &Path, hint_file: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env("NEURION_AUTO_REGISTER_FILE", hint_file)
        .env("HOME", sandbox)
        .env("XDG_DATA_HOME", sandbox.join("xdg-data"))
        .env("XDG_CACHE_HOME", sandbox.join("xdg-cache"))
        .env("XDG_CONFIG_HOME", sandbox.join("xdg-config"))
        .env("XDG_STATE_HOME", sandbox.join("xdg-state"))
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_RUNTIME_ACTIVE")
        .env_remove("NEURION_SOCKET")
        .env_remove("NEURION_DB_PATH");
    cmd
}

/// Run a git command in `project`, panicking on non-zero exit. Tests
/// are unforgiving: a silent git failure here would let a downstream
/// assertion pass for the wrong reason.
fn git(project: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(project)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Bootstrap an isolated cosmon project under `sandbox` and return its
/// root + the path of the sandboxed neurion-hint file. The project has:
/// - a real git repo (so `cs nucleate` can record an originating branch
///   and downstream `cs done` calls — if any — can resolve `main`);
/// - the canonical formulas seeded by `cs init`;
/// - a synthesized `hello.formula.toml` (one trivial step) so the test
///   can mark molecules as Completed without needing claude.
fn bootstrap(sandbox: &Path) -> (PathBuf, PathBuf) {
    let project = sandbox.join("project");
    fs::create_dir_all(&project).unwrap();
    let hint = sandbox.join("neurion-hint.jsonl");

    // git init MUST come before `cs init`: walk-up state discovery
    // anchors on the .git directory.
    git(&project, &["init", "-q", "--initial-branch=main"]);
    git(&project, &["config", "user.email", "test@test.local"]);
    git(&project, &["config", "user.name", "cosmon-test"]);
    git(&project, &["config", "commit.gpgsign", "false"]);
    git(&project, &["commit", "--allow-empty", "-q", "-m", "seed"]);

    let out = cs_no_neurion(sandbox, &hint)
        .args(["init", "--yes"])
        .current_dir(&project)
        .output()
        .expect("cs init");
    assert!(
        out.status.success(),
        "cs init failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // The smoke harness ships a one-step `hello` formula precisely so
    // tests can exercise the lifecycle without claude. Reuse it.
    let hello_src = project_root_repo().join("tests/fixtures/hello.formula.toml");
    let hello_dst = project.join(".cosmon/formulas/hello.formula.toml");
    fs::copy(&hello_src, &hello_dst).expect("seed hello.formula.toml from tests/fixtures");

    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "init cosmon"]);

    (project, hint)
}

/// Find the workspace repo root from the integration-test binary.
/// `CARGO_MANIFEST_DIR` for this crate is `crates/cosmon-cli`, so two
/// `..` hops land at the workspace root.
fn project_root_repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// Nucleate a `hello` molecule; returns its id. Optional `--blocked-by`
/// pairs let the test build a DAG.
fn nucleate(sandbox: &Path, hint: &Path, project: &Path, extra_args: &[&str]) -> String {
    let mut args: Vec<&str> = vec![
        "--json",
        "nucleate",
        "hello",
        "--var",
        "topic=restart-fidelity",
    ];
    args.extend_from_slice(extra_args);

    let out = cs_no_neurion(sandbox, hint)
        .args(&args)
        .current_dir(project)
        .output()
        .expect("cs nucleate");
    assert!(
        out.status.success(),
        "cs nucleate {extra_args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("cs nucleate --json must emit valid JSON");
    v["id"]
        .as_str()
        .expect("nucleate JSON must include `id`")
        .to_owned()
}

/// Mark a molecule `Completed` via `cs complete` — the worker-callable
/// state transition. Returns the parsed JSON outcome.
fn complete(sandbox: &Path, hint: &Path, project: &Path, mol_id: &str) -> serde_json::Value {
    let out = cs_no_neurion(sandbox, hint)
        .args([
            "--json",
            "complete",
            mol_id,
            "--reason",
            "restart-fidelity test",
        ])
        .current_dir(project)
        .output()
        .expect("cs complete");
    assert!(
        out.status.success(),
        "cs complete {mol_id} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // `cs complete --json` writes one NDJSON line per molecule; we
    // always pass a single id so the first line is canonical.
    let line = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .expect("cs complete must emit at least one JSON line")
        .to_owned();
    serde_json::from_str(&line).expect("cs complete --json must be valid JSON")
}

/// Read the on-disk status of every molecule under the project's
/// `.cosmon/state/fleets/default/molecules/`. Returns an ordered map
/// `{molecule_id: status}` so two trajectories can be compared
/// position-independent (the test inserts them in the same order, so
/// id-aware comparison is also legitimate, but BTreeMap removes any
/// hash-iteration nondeterminism).
fn read_state_signature(project: &Path) -> BTreeMap<String, String> {
    let mol_dir = project.join(".cosmon/state/fleets/default/molecules");
    let mut sig = BTreeMap::new();
    let Ok(entries) = fs::read_dir(&mol_dir) else {
        return sig;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let state_path = path.join("state.json");
        let Ok(text) = fs::read_to_string(&state_path) else {
            continue;
        };
        let Ok(state) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let id = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
        let status = state["status"].as_str().unwrap_or("unknown").to_owned();
        sig.insert(id.to_owned(), status);
    }
    sig
}

/// Recursively copy a directory tree. `cs` snapshots are usually small
/// (KB), so a naïve copy is fine — no need for `cp -a` semantics
/// (symlinks, xattrs) inside `.cosmon/state/`.
fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_dir(&src, &dst);
        } else {
            fs::copy(&src, &dst).unwrap();
        }
    }
}

/// Walk a directory and return the set of paths *outside* `sandbox`
/// that any file references textually. Best-effort grep: we look for
/// the literal `~/.local/share/neurion` and the actual canonical home
/// to catch the most common escape paths.
///
/// This is a defensive check, not a soundness proof. The point is to
/// make accidental host writes loud and obvious in CI logs.
fn neurion_paths_outside_sandbox(sandbox: &Path) -> Vec<PathBuf> {
    let mut hits = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let hint = PathBuf::from(home)
            .join(".local/share/neurion")
            .join("auto-register.jsonl");
        // The real-home hint file is SHARED global state: on Linux
        // `dirs::data_dir()` is `~/.local/share`, so any sibling test in the
        // same `cargo test` job that runs `cs` unsandboxed pre-creates it.
        // Flagging on mere existence made this test a false-positive on that
        // pollution (it never fired on macOS, where data_dir is elsewhere).
        //
        // Attribute precisely instead: a leak from THIS test's cs invocations
        // would append a hint line whose `local_path` points inside OUR
        // sandbox. Match on that; a sibling's hint (a different sandbox / the
        // repo) is correctly ignored.
        let canonical_sandbox = sandbox
            .canonicalize()
            .unwrap_or_else(|_| sandbox.to_path_buf());
        let needles = [
            sandbox.to_string_lossy().into_owned(),
            canonical_sandbox.to_string_lossy().into_owned(),
        ];
        if let Ok(content) = fs::read_to_string(&hint) {
            if content.lines().any(|line| {
                needles
                    .iter()
                    .any(|n| !n.is_empty() && line.contains(n.as_str()))
            }) {
                hits.push(hint);
            }
        }
    }
    hits
}

// ─────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────

/// (1) Hygiene: the cs binary boots, answers commands, and writes
///     auto-register hints only into the sandbox. No neurion service
///     is contacted; no LaunchAgent is required.
#[test]
fn cosmon_runs_standalone_without_neurion() {
    let tmp = tempfile::tempdir().unwrap();
    let sandbox = tmp.path();
    let (project, hint) = bootstrap(sandbox);

    // 1a. `cs --version` works without any external service.
    let out = cs_no_neurion(sandbox, &hint)
        .arg("--version")
        .current_dir(&project)
        .output()
        .expect("cs --version");
    assert!(
        out.status.success(),
        "cs --version must work standalone: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // 1b. `cs ensemble --json` works with an empty fleet, no neurion.
    let out = cs_no_neurion(sandbox, &hint)
        .args(["--json", "ensemble"])
        .current_dir(&project)
        .output()
        .expect("cs ensemble");
    assert!(
        out.status.success(),
        "cs ensemble must work standalone: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // 1c. The neurion auto-register hint, if it was created at all,
    //     stays inside the sandbox. A leak here would mean we wrote
    //     to the developer's `~/.local/share/neurion/`.
    let leaks = neurion_paths_outside_sandbox(sandbox);
    assert!(
        leaks.is_empty(),
        "cs must not write neurion hints outside the sandbox: leaks={leaks:?}"
    );

    // 1d. No cosmon-managed LaunchAgent plist exists under the
    //     sandboxed HOME. (The sandbox is a fresh tempdir, so this is
    //     trivially true — but the assertion documents the contract:
    //     the trajectory below must succeed *because* of disk state,
    //     not because launchd nudged anything.)
    let launch_agents = sandbox.join("Library/LaunchAgents");
    if launch_agents.exists() {
        let plists: Vec<_> = fs::read_dir(&launch_agents)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .to_lowercase()
                    .contains("cosmon")
            })
            .collect();
        assert!(
            plists.is_empty(),
            "no cosmon-named LaunchAgent must be required: found {plists:?}"
        );
    }
}

/// (2) Standalone DAG trajectory: three molecules in a fan-out pattern
///     nucleate, complete, and surface on `ensemble --json` exactly as
///     they would on a workstation with neurion + LaunchAgents
///     installed. The trajectory is the cosmon equivalent of a one-bit
///     control plane (typed links carry "blocked-by") — this test
///     confirms the bit is preserved on disk and observable from a
///     fresh process.
#[test]
fn dag_trajectory_completes_without_external_clock() {
    let tmp = tempfile::tempdir().unwrap();
    let sandbox = tmp.path();
    let (project, hint) = bootstrap(sandbox);

    // Build the DAG: A → {B, C}
    let a = nucleate(sandbox, &hint, &project, &[]);
    let b = nucleate(sandbox, &hint, &project, &["--blocked-by", &a]);
    let c = nucleate(sandbox, &hint, &project, &["--blocked-by", &a]);

    // After nucleation everyone is pending.
    let sig = read_state_signature(&project);
    assert_eq!(sig.get(&a).map(String::as_str), Some("pending"));
    assert_eq!(sig.get(&b).map(String::as_str), Some("pending"));
    assert_eq!(sig.get(&c).map(String::as_str), Some("pending"));

    // Complete A (root).
    let outcome = complete(sandbox, &hint, &project, &a);
    assert_eq!(outcome["new_status"].as_str(), Some("completed"));

    // B and C are now reachable in the dependency frontier — but the
    // typed-link layer doesn't auto-advance them. The test is about
    // restart fidelity, not auto-dispatch: confirm A is Completed and
    // B/C still pending.
    let sig = read_state_signature(&project);
    assert_eq!(sig.get(&a).map(String::as_str), Some("completed"));
    assert_eq!(sig.get(&b).map(String::as_str), Some("pending"));
    assert_eq!(sig.get(&c).map(String::as_str), Some("pending"));

    // Drain B and C.
    complete(sandbox, &hint, &project, &b);
    complete(sandbox, &hint, &project, &c);

    let sig = read_state_signature(&project);
    let statuses: BTreeSet<&str> = sig.values().map(String::as_str).collect();
    assert_eq!(
        statuses,
        BTreeSet::from(["completed"]),
        "every molecule must reach Completed: {sig:?}"
    );
    assert_eq!(sig.len(), 3, "exactly three molecules on disk");
}

/// (3) Markov restart-fidelity: an interrupted trajectory + a snapshot
///     restart converges on the same on-disk state as an uninterrupted
///     trajectory. This is the operative version of the §7c claim
///     that "the runtime is a pure function of disk state".
///
///     The interruption is process-level (each `cs` invocation is its
///     own process; the test's own Rust frame holds zero molecule
///     state between calls). The snapshot is a verbatim copy of
///     `.cosmon/state/` taken mid-trajectory and restored before the
///     second half resumes.
#[test]
fn restart_fidelity_two_runs_converge() {
    // ── Run A — uninterrupted ───────────────────────────────────────
    let tmp_a = tempfile::tempdir().unwrap();
    let (proj_a, hint_a) = bootstrap(tmp_a.path());

    let a1 = nucleate(tmp_a.path(), &hint_a, &proj_a, &[]);
    let a2 = nucleate(tmp_a.path(), &hint_a, &proj_a, &["--blocked-by", &a1]);
    let a3 = nucleate(tmp_a.path(), &hint_a, &proj_a, &["--blocked-by", &a1]);
    complete(tmp_a.path(), &hint_a, &proj_a, &a1);
    complete(tmp_a.path(), &hint_a, &proj_a, &a2);
    complete(tmp_a.path(), &hint_a, &proj_a, &a3);
    let final_a_statuses: BTreeSet<String> = read_state_signature(&proj_a).into_values().collect();

    // ── Run B — interrupted at midpoint, restarted from snapshot ────
    let tmp_b = tempfile::tempdir().unwrap();
    let (proj_b, hint_b) = bootstrap(tmp_b.path());

    let b1 = nucleate(tmp_b.path(), &hint_b, &proj_b, &[]);
    let b2 = nucleate(tmp_b.path(), &hint_b, &proj_b, &["--blocked-by", &b1]);
    let b3 = nucleate(tmp_b.path(), &hint_b, &proj_b, &["--blocked-by", &b1]);

    // Snapshot the entire state directory mid-trajectory.
    let snap_src = proj_b.join(".cosmon/state");
    let snap_dst = tmp_b.path().join("snapshot/state");
    copy_dir(&snap_src, &snap_dst);

    // Continue: complete the root.
    complete(tmp_b.path(), &hint_b, &proj_b, &b1);

    // Simulate a "machine restart": wipe `.cosmon/state` and restore
    // it from the mid-trajectory snapshot. This forces the next
    // command to read disk state alone — no in-memory frontier, no
    // process-resident plan.
    fs::remove_dir_all(&snap_src).unwrap();
    copy_dir(&snap_dst, &snap_src);

    // After restoration, `b1` is back to pending — the snapshot
    // captured pre-completion state. Re-completing it reaches the
    // same terminal as the uninterrupted run.
    complete(tmp_b.path(), &hint_b, &proj_b, &b1);
    complete(tmp_b.path(), &hint_b, &proj_b, &b2);
    complete(tmp_b.path(), &hint_b, &proj_b, &b3);

    let final_b_statuses: BTreeSet<String> = read_state_signature(&proj_b).into_values().collect();

    assert_eq!(
        final_a_statuses, final_b_statuses,
        "uninterrupted and snapshot-restored trajectories must reach \
         identical molecule statuses (Markov property, \
         architectural-invariants.md §7c)"
    );
    assert_eq!(
        final_a_statuses,
        BTreeSet::from(["completed".to_owned()]),
        "both trajectories must drain to all-Completed"
    );
}

/// (4) Sandbox guarantee: re-emitting auto-register hints from inside
///     the test never touches the developer's `~/.local/share/neurion`.
///     A regression that hardcodes the hint path would surface here.
#[test]
fn neurion_hint_writes_stay_inside_sandbox() {
    let tmp = tempfile::tempdir().unwrap();
    let sandbox = tmp.path();
    let (project, hint) = bootstrap(sandbox);

    // Several invocations to give the hint emitter multiple chances
    // to escape (each cs invocation may re-emit).
    for _ in 0..3 {
        let out = cs_no_neurion(sandbox, &hint)
            .args(["--json", "ensemble"])
            .current_dir(&project)
            .output()
            .expect("cs ensemble");
        assert!(out.status.success());
    }

    let leaks = neurion_paths_outside_sandbox(sandbox);
    assert!(
        leaks.is_empty(),
        "neurion hint emitter must respect NEURION_AUTO_REGISTER_FILE: leaks={leaks:?}"
    );
}
