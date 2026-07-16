// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end CLI coverage for the **blocking `[hooks] pre_done` gate**
//! (cosmon-ward from showroom delib-20260701-bfdf, torvalds D1).
//!
//! THE HOLE IT CLOSES: `[hooks] post_merge` runs *after* the merge lands and
//! can only warn — nothing in the molecule cycle could *refuse* a DONE. A
//! falsifiable Definition-of-Done (DROVE ∧ OBSERVED ∧ BADGE ∧ FALSIFIER)
//! could therefore only be enforced out-of-band in GitHub branch-protection,
//! outside cosmon's teardown.
//!
//! THE CONTRACT exercised here, against the **real `cs` binary** so the exit
//! code is the actual process exit:
//!
//!   * a configured `pre_done` that exits non-zero ⇒ **non-zero exit**, a
//!     loud `pre_done_refused` outcome, the branch preserved, nothing landed
//!     on main, and the script's stderr surfaced as the reason;
//!   * a configured `pre_done` that exits zero ⇒ the merge proceeds and
//!     teardown runs (branch deleted);
//!   * `--skip-pre-done-hook` bypasses a *failing* gate and lets teardown
//!     proceed — the operator kill-switch.
//!
//! The hermetic unit tests in `cmd::done` cover the hook-runner layer
//! (`run_pre_done_hook`, `pre_done_hook_skipped`); this file proves the
//! command-level wiring: that a refused gate actually aborts the *whole*
//! teardown before anything irreversible happens.

use std::fs;
use std::path::Path;
use std::process::Command;

fn cs() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        // Ensure a stray env kill-switch in the outer shell never leaks in.
        .env_remove("COSMON_SKIP_PRE_DONE_HOOK")
        // This fixture exercises the pre_done gate itself in a throwaway repo;
        // bypass the B5 repo-supplied-shell trust gate so the pre_done hook
        // runs. The trust gate is covered by `src/trust.rs` +
        // `tests/trust_gate_cli.rs`.
        .env("COSMON_ASSUME_TRUSTED", "1");
    cmd
}

fn cs_isolated(repo: &Path) -> Command {
    let state_dir = repo.join(".cosmon/state");
    let config_path = repo.join(".cosmon/config.toml");
    let mut cmd = cs();
    cmd.env("COSMON_STATE_DIR", &state_dir)
        .env("COSMON_CONFIG", &config_path)
        .current_dir(&state_dir);
    cmd
}

fn git(repo: &Path, args: &[&str]) -> std::process::Output {
    let mut full: Vec<&str> = vec!["-C", repo.to_str().unwrap()];
    full.extend_from_slice(args);
    Command::new("git")
        .args(&full)
        .output()
        .expect("git spawn failed")
}

fn git_ok(repo: &Path, args: &[&str]) {
    let out = git(repo, args);
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Init a git repo with a `.cosmon` project. `pre_done_hook` (when `Some`)
/// is written verbatim into `[hooks] pre_done`.
fn setup_repo(tmp: &Path, pre_done_hook: Option<&str>) {
    git_ok(tmp, &["init", "-q", "-b", "main"]);
    git_ok(tmp, &["config", "user.email", "test@example.com"]);
    git_ok(tmp, &["config", "user.name", "Test"]);
    git_ok(tmp, &["config", "commit.gpgsign", "false"]);

    let cosmon = tmp.join(".cosmon");
    fs::create_dir_all(cosmon.join("state")).unwrap();
    fs::create_dir_all(cosmon.join("formulas")).unwrap();
    let mut config = String::from("[project]\nproject_id = \"test-pre-done-gate\"\n");
    if let Some(hook) = pre_done_hook {
        // TOML single-quoted literal string — the hook bodies here contain no
        // single quotes, so this needs no escaping.
        config.push_str(&format!("\n[hooks]\npre_done = '{hook}'\n"));
    }
    fs::write(cosmon.join("config.toml"), config).unwrap();
    fs::write(cosmon.join("state/fleet.json"), "{}\n").unwrap();
    fs::create_dir_all(tmp.join("app/src")).unwrap();
    fs::write(
        tmp.join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\"]\nresolver = \"2\"\n",
    )
    .unwrap();
    fs::write(
        tmp.join("app/Cargo.toml"),
        "[package]\nname = \"done-gate-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(tmp.join("app/src/lib.rs"), "pub fn healthy() {}\n").unwrap();
    let formula_src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cosmon/formulas/task-work.formula.toml");
    fs::copy(&formula_src, cosmon.join("formulas/task-work.formula.toml")).unwrap();

    fs::write(tmp.join(".gitignore"), ".cosmon/\n.worktrees/\n").unwrap();
}

/// Nucleate a `task-work` molecule and drive it to a terminal state
/// (collapsed) so `cs done` will attempt the merge without `--force`.
fn nucleate_terminal(repo: &Path) -> String {
    let nuc = cs_isolated(repo)
        .args([
            "--json",
            "nucleate",
            "task-work",
            "--var",
            "topic=pre-done gate integration test",
        ])
        .output()
        .expect("cs nucleate");
    assert!(
        nuc.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&nuc.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&nuc.stdout).unwrap();
    let mol_id = v["id"].as_str().expect("nucleate id").to_owned();

    let col = cs_isolated(repo)
        .args([
            "--json",
            "collapse",
            &mol_id,
            "--reason",
            "integration test",
        ])
        .output()
        .expect("cs collapse");
    assert!(
        col.status.success(),
        "collapse failed: {}",
        String::from_utf8_lossy(&col.stderr)
    );
    mol_id
}

/// Build a base commit + a clean feat branch that WOULD merge without
/// conflict, returning the branch name and main's HEAD before the merge.
fn stage_clean_merge(repo: &Path, mol_id: &str) -> (String, String) {
    let branch = format!("feat/{mol_id}");

    fs::write(repo.join("base.txt"), "base\n").unwrap();
    git_ok(
        repo,
        &["add", ".gitignore", "base.txt", "Cargo.toml", "app"],
    );
    git_ok(repo, &["commit", "-q", "-m", "base"]);

    git_ok(repo, &["checkout", "-q", "-b", &branch]);
    fs::write(repo.join("worker.txt"), "worker\n").unwrap();
    git_ok(repo, &["add", "worker.txt"]);
    git_ok(repo, &["commit", "-qm", "worker file"]);

    git_ok(repo, &["checkout", "-q", "main"]);

    let main_before = String::from_utf8_lossy(&git(repo, &["rev-parse", "main"]).stdout)
        .trim()
        .to_owned();
    (branch, main_before)
}

#[test]
fn post_merge_compile_gate_rejects_test_target_failure_and_resets_main() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo, None);

    let mol_id = nucleate_terminal(repo);
    let (branch, _main_before) = stage_clean_merge(repo, &mol_id);

    // `cargo check --workspace` would not compile this integration test;
    // `--all-targets` must catch its arity error before the merge is stamped.
    git_ok(repo, &["checkout", "-q", &branch]);
    fs::create_dir_all(repo.join("app/tests")).unwrap();
    fs::write(
        repo.join("app/tests/broken_arity.rs"),
        "#[test]\nfn broken_arity() { done_gate_fixture::healthy(1); }\n",
    )
    .unwrap();
    git_ok(repo, &["add", "app/tests/broken_arity.rs"]);
    git_ok(repo, &["commit", "-qm", "add broken integration test"]);
    git_ok(repo, &["checkout", "-q", "main"]);

    let done = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");
    let stdout = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);

    assert!(
        !done.status.success(),
        "a failing all-target compile gate must refuse done.\\nstdout={stdout}\\nstderr={stderr}"
    );
    assert!(
        stdout.contains("post_merge_compile_gate_refused")
            || stderr.contains("POST-MERGE COMPILE GATE REFUSED"),
        "the refusal must be loud and typed.\\nstdout={stdout}\\nstderr={stderr}"
    );

    assert_eq!(
        git(repo, &["merge-base", "--is-ancestor", &branch, "main"])
            .status
            .code(),
        Some(1),
        "the gate reset must leave the broken branch unmerged.\\nstdout={stdout}\\nstderr={stderr}"
    );
    assert!(
        !git(repo, &["cat-file", "-e", "main:app/tests/broken_arity.rs"])
            .status
            .success(),
        "the broken test target must not remain on main after rollback"
    );
    assert!(
        git(repo, &["rev-parse", "--verify", &branch])
            .status
            .success(),
        "the worker branch must remain available for repair and retry"
    );
}

/// PR-B honesty floor, D1-updated (task-20260715-ff5b): when the post-merge
/// gate cannot positively verify the merged tree AND **nothing was declared**
/// to verify it (`Unverified { expected: false }`), the merge still **lands** —
/// an unexpected gate is a loud advisory, not a rollback — but the gap MUST be
/// durable in `events.jsonl` as the single post-gate `merge_completed` whose
/// result is the `ok:unverified` witness, never a bare `ok`.
///
/// The `expected: false` case is the ONE Unverified flavour that stays fail-open
/// by default after the ratified D1 discriminator (`fail_closed = expected ||
/// flag`): the tree is a non-Cargo repo with no `integrity_command` and no
/// `build_command`, so cosmon genuinely has no declared way to verify it. (An
/// `expected: true` Unverified — a gate WAS expected but a code diff went
/// unchecked — now fails CLOSED by default; see
/// `default_config_expected_true_unverified_fails_closed`.)
///
/// FALSIFIER: reverting the `GateOutcome::Unverified` arm of
/// `post_gate_merge_result` to `MergeResult::Ok` reddens this test — the merge
/// would land as a clean `ok` and a downstream honesty auditor reading the log
/// would wrongly conclude the tree was verified.
#[test]
fn post_merge_gate_unverified_expected_false_lands_with_ok_unverified_witness() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    // Non-Cargo repo, no [gates] — nothing declared to verify the tree.
    setup_repo_polyglot(repo, "");

    let mol_id = nucleate_terminal(repo);
    // A loose Rust source in a repo with NO Cargo workspace: cargo cannot
    // resolve a manifest, so the cascade declines through every rung to the
    // terminal `Unverified { expected: false }` (nobody declared a verifier).
    let (branch, _main_before) =
        stage_polyglot_merge(repo, &mol_id, "scripts/tool.rs", "fn main() {}\n");

    let done = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");
    let stdout = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);

    // 1. The merge LANDED — Unverified proceeds. Exit is success, teardown ran
    //    (feat branch deleted), and the worker's content is on main.
    assert!(
        done.status.success(),
        "an Unverified gate is a loud advisory, not a rollback — done must succeed.\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        git(repo, &["cat-file", "-e", "main:scripts/tool.rs"])
            .status
            .success(),
        "the worker's change must be on main — the Unverified gate does not roll back"
    );
    assert!(
        !git(repo, &["rev-parse", "--verify", &branch])
            .status
            .success(),
        "teardown must proceed on an Unverified (non-refusing) gate"
    );

    // 2. The operator saw the loud advisory in the action stream.
    assert!(
        stdout.contains("UNVERIFIED") || stderr.contains("UNVERIFIED"),
        "the gate must surface a loud UNVERIFIED advisory.\nstdout={stdout}\nstderr={stderr}"
    );

    // 3. THE HONESTY FLOOR: the gap is durable in events.jsonl as the single
    //    post-gate `merge_completed` carrying the `ok:unverified` witness — not
    //    a silent clean `ok`. This is the byte a downstream auditor reads.
    let events = fs::read_to_string(repo.join(".cosmon/state/events.jsonl"))
        .expect("events.jsonl must exist after done");
    let merge_lines: Vec<&str> = events
        .lines()
        .filter(|l| l.contains("merge_completed") && l.contains(&mol_id))
        .collect();
    assert!(
        merge_lines.iter().any(|l| l.contains("ok:unverified")),
        "the Unverified gate outcome must be durable as a merge_completed \
         ok:unverified witness in events.jsonl.\nmerge_lines={merge_lines:#?}\nevents=\n{events}"
    );
    // And it must NOT have also written a bare clean `ok` for the same merge —
    // the pre-gate double-witness the single-emission redesign removed. A
    // `merge_completed` line whose result is exactly `"ok"` would be that lie.
    assert!(
        !merge_lines.iter().any(|l| l.contains("\"result\":\"ok\"")),
        "no bare clean `ok` merge witness may accompany the ok:unverified one — \
         the post-gate emission is singular.\nmerge_lines={merge_lines:#?}"
    );
}

/// Round-2 I/O failure path (task-20260715-e0a6): when the append of the
/// durable merge witness to `events.jsonl` FAILS, the merge has already
/// landed, so `cs done` must NOT abort teardown — but the lost witness must be
/// LOUD, never swallowed by the historical bare `let _ = emit_one(...)`.
///
/// The failure is injected by pre-creating `events.jsonl` as a *directory*, so
/// every append to it errors. The git-based merge is independent of the event
/// log, so it still lands; the final witness emit then errors and must surface
/// a `CRITICAL` advisory on stderr.
///
/// FALSIFIER: restoring the bare `let _ = emit_one(...)` at the post-gate
/// witness site (dropping the loud-log) reddens this test — the persistence
/// failure would again be silent and stderr would carry no `CRITICAL` line.
#[test]
fn merge_witness_persistence_failure_is_loud_not_swallowed() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo, None);

    let mol_id = nucleate_terminal(repo);
    let (_branch, _main_before) = stage_clean_merge(repo, &mol_id);

    // Inject the I/O failure: a directory where the append expects a file.
    // `nucleate_terminal` already wrote real events here, so remove the file
    // first, then replace it with a directory.
    let events = repo.join(".cosmon/state/events.jsonl");
    if events.exists() {
        fs::remove_file(&events).unwrap();
    }
    fs::create_dir(&events).unwrap();

    let done = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");
    let stdout = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);

    // 1. The merge still LANDED — a witness-log write failure never rolls back
    //    an already-landed merge (that would be strictly worse). Teardown ran.
    assert!(
        done.status.success(),
        "a witness-persistence failure must not abort an already-landed merge.\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        git(repo, &["cat-file", "-e", "main:worker.txt"])
            .status
            .success(),
        "worker.txt must be on main — the merge landed despite the log-write failure"
    );

    // 2. THE LOUD-LOG: the lost durable witness is conspicuous on stderr, not
    //    swallowed. A downstream operator must never infer merge health from a
    //    silently missing merge_completed line.
    assert!(
        stderr.contains("CRITICAL: failed to persist durable merge witness"),
        "a witness-persistence failure must be LOUD on stderr, never swallowed.\nstdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn pre_done_nonzero_exit_aborts_teardown() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    // A gate that always refuses, writing a falsifiable reason to stderr.
    // (No single quotes: the hook is stored as a TOML literal string.)
    setup_repo(repo, Some("echo DoD-not-proven-no-FALSIFIER >&2; exit 7"));

    let mol_id = nucleate_terminal(repo);
    let (branch, main_before) = stage_clean_merge(repo, &mol_id);

    let done = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");

    let stdout = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);

    // 1. NON-ZERO EXIT — a refused DONE is a hard failure.
    assert!(
        !done.status.success(),
        "cs done MUST fail when pre_done refuses.\nstdout={stdout}\nstderr={stderr}"
    );

    // 2. Loud, typed outcome carrying the script's stderr as the reason.
    assert!(
        stdout.contains("pre_done_refused") || stderr.contains("PRE-DONE GATE REFUSED"),
        "expected a loud pre_done_refused signal.\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("DoD-not-proven-no-FALSIFIER")
            || stderr.contains("DoD-not-proven-no-FALSIFIER"),
        "the gate's stderr must be surfaced as the reason.\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("\"ok\":true"),
        "JSON must not report ok:true on a refused gate.\nstdout={stdout}"
    );

    // 3. Branch preserved — nothing was torn down.
    assert!(
        git(repo, &["rev-parse", "--verify", &branch])
            .status
            .success(),
        "the worker's branch must survive a refused pre_done gate"
    );

    // 4. Nothing landed on main — the gate ran BEFORE the merge.
    let main_after = String::from_utf8_lossy(&git(repo, &["rev-parse", "main"]).stdout)
        .trim()
        .to_owned();
    assert_eq!(
        main_before, main_after,
        "main HEAD must NOT move when the pre_done gate refuses"
    );
    assert!(
        !git(repo, &["cat-file", "-e", "main:worker.txt"])
            .status
            .success(),
        "the worker's content must NOT have landed on main"
    );
}

#[test]
fn pre_done_zero_exit_allows_teardown() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    // A gate that proves the DoD (exits 0) and asserts it received the mol id.
    setup_repo(
        repo,
        Some(r#"test -n "$1" || exit 9; echo "verified $1"; exit 0"#),
    );

    let mol_id = nucleate_terminal(repo);
    let (branch, _main_before) = stage_clean_merge(repo, &mol_id);

    let done = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");

    let stdout = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);

    // 1. ZERO EXIT — a passing gate must not block the happy path.
    assert!(
        done.status.success(),
        "cs done must succeed when pre_done passes.\nstdout={stdout}\nstderr={stderr}"
    );

    // 2. Merge landed and teardown proceeded (feat branch deleted).
    assert!(
        git(repo, &["cat-file", "-e", "main:worker.txt"])
            .status
            .success(),
        "worker.txt must be on main after a passing gate + clean merge"
    );
    assert!(
        !git(repo, &["rev-parse", "--verify", &branch])
            .status
            .success(),
        "a passing gate + clean merge must delete the feat branch"
    );
}

#[test]
fn pre_done_kill_switch_bypasses_failing_gate() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    // A gate that would always refuse — the kill-switch must override it.
    setup_repo(repo, Some("echo always-refuse >&2; exit 1"));

    let mol_id = nucleate_terminal(repo);
    let (branch, _main_before) = stage_clean_merge(repo, &mol_id);

    let done = cs_isolated(repo)
        .args([
            "--json",
            "done",
            &mol_id,
            "--no-auto-propel",
            "--skip-pre-done-hook",
        ])
        .output()
        .expect("cs done");

    let stdout = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);

    // 1. ZERO EXIT — the operator kill-switch waived the gate.
    assert!(
        done.status.success(),
        "--skip-pre-done-hook must bypass a failing gate.\nstdout={stdout}\nstderr={stderr}"
    );

    // 2. Teardown proceeded despite the gate that would have refused.
    assert!(
        git(repo, &["cat-file", "-e", "main:worker.txt"])
            .status
            .success(),
        "worker.txt must be on main when the gate is skipped"
    );
    assert!(
        !git(repo, &["rev-parse", "--verify", &branch])
            .status
            .success(),
        "the feat branch must be deleted when the gate is skipped"
    );
}

/// B5 (RCE-by-clone): the `pre_done` hook is a repo-supplied shell string.
/// On an **untrusted** repository, `cs done` must refuse to run it, abort the
/// teardown (nothing merged, branch preserved), and tell the operator how to
/// grant trust — proving the trust gate is wired in front of the real
/// `sh -c`, not just unit-tested in isolation.
#[test]
fn pre_done_hook_refused_on_untrusted_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    // An isolated, EMPTY trust store: the repo has no grant on record.
    let trust_store = tempfile::tempdir().unwrap();
    // A gate that would PASS if it ran — so a merge here would prove the gate
    // was bypassed. The trust refusal must stop it before it runs.
    setup_repo(repo, Some("exit 0"));

    let mol_id = nucleate_terminal(repo);
    let (branch, main_before) = stage_clean_merge(repo, &mol_id);

    // A command builder WITHOUT the trust bypass, pinned to the empty store.
    let state_dir = repo.join(".cosmon/state");
    let config_path = repo.join(".cosmon/config.toml");
    let done = Command::new(env!("CARGO_BIN_EXE_cs"))
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_SKIP_PRE_DONE_HOOK")
        .env_remove("COSMON_ASSUME_TRUSTED")
        .env("COSMON_STATE_DIR", &state_dir)
        .env("COSMON_CONFIG", &config_path)
        .env("COSMON_TRUST_DIR", trust_store.path())
        .current_dir(&state_dir)
        .args(["done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&done.stdout),
        String::from_utf8_lossy(&done.stderr)
    );

    // 1. NON-ZERO EXIT — the untrusted repo's hook was refused.
    assert!(
        !done.status.success(),
        "cs done must refuse an untrusted repo's pre_done hook.\n{combined}"
    );
    // 2. The refusal names the remedy.
    assert!(
        combined.contains("cs trust") || combined.to_lowercase().contains("not trusted"),
        "refusal must tell the operator to `cs trust`.\n{combined}"
    );
    // 3. Nothing merged — main is untouched and the branch survives.
    let main_after = String::from_utf8_lossy(&git(repo, &["rev-parse", "main"]).stdout)
        .trim()
        .to_owned();
    assert_eq!(
        main_before, main_after,
        "no merge may land when the pre_done hook is refused"
    );
    assert!(
        git(repo, &["rev-parse", "--verify", &branch])
            .status
            .success(),
        "the feat branch must be preserved when done is refused"
    );

    // 4. After an explicit grant, the same `cs done` proceeds.
    let grant = Command::new(env!("CARGO_BIN_EXE_cs"))
        .env("COSMON_TRUST_DIR", trust_store.path())
        .current_dir(repo)
        .args(["trust"])
        .output()
        .expect("cs trust");
    assert!(
        grant.status.success(),
        "cs trust must grant: {}",
        String::from_utf8_lossy(&grant.stderr)
    );

    let done2 = Command::new(env!("CARGO_BIN_EXE_cs"))
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_SKIP_PRE_DONE_HOOK")
        .env_remove("COSMON_ASSUME_TRUSTED")
        .env("COSMON_STATE_DIR", &state_dir)
        .env("COSMON_CONFIG", &config_path)
        .env("COSMON_TRUST_DIR", trust_store.path())
        .current_dir(&state_dir)
        .args(["done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done (trusted)");
    assert!(
        done2.status.success(),
        "cs done must proceed once the repo is trusted.\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&done2.stdout),
        String::from_utf8_lossy(&done2.stderr)
    );
    assert!(
        git(repo, &["cat-file", "-e", "main:worker.txt"])
            .status
            .success(),
        "worker.txt must be on main once the trusted hook passes"
    );
}

// ===================================================================
// ADR-158 inc-2 — polyglot delegation cascade + fail-closed policy (D1).
//
// `cs done`'s post-merge integrity gate now DELEGATES the WHAT to the
// per-galaxy `[gates]` commands (cosmon owns only the WHEN). These e2e
// tests drive the real `cs` binary so the exit code, the durable
// `events.jsonl` witness, and main's post-gate revision are the actual
// process outputs. The `cs()` harness sets COSMON_ASSUME_TRUSTED=1, so the
// repo-supplied `integrity_command` may exec (the untrusted refusal has its
// own coverage in `trust_gate_cli.rs`).
// ===================================================================

/// Init a repo exactly like [`setup_repo`] but with an extra raw config block
/// appended (e.g. a `[gates]` section). Kept separate so the cascade tests can
/// declare `integrity_command` / `fail_closed_on_unverified` without perturbing
/// the pre-done-gate fixtures.
fn setup_repo_with_config(tmp: &Path, extra_config: &str) {
    git_ok(tmp, &["init", "-q", "-b", "main"]);
    git_ok(tmp, &["config", "user.email", "test@example.com"]);
    git_ok(tmp, &["config", "user.name", "Test"]);
    git_ok(tmp, &["config", "commit.gpgsign", "false"]);

    let cosmon = tmp.join(".cosmon");
    fs::create_dir_all(cosmon.join("state")).unwrap();
    fs::create_dir_all(cosmon.join("formulas")).unwrap();
    let mut config = String::from("[project]\nproject_id = \"test-integrity-cascade\"\n");
    config.push_str(extra_config);
    fs::write(cosmon.join("config.toml"), config).unwrap();
    fs::write(cosmon.join("state/fleet.json"), "{}\n").unwrap();
    fs::create_dir_all(tmp.join("app/src")).unwrap();
    fs::write(
        tmp.join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\"]\nresolver = \"2\"\n",
    )
    .unwrap();
    fs::write(
        tmp.join("app/Cargo.toml"),
        "[package]\nname = \"done-gate-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(tmp.join("app/src/lib.rs"), "pub fn healthy() {}\n").unwrap();
    let formula_src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cosmon/formulas/task-work.formula.toml");
    fs::copy(&formula_src, cosmon.join("formulas/task-work.formula.toml")).unwrap();
    fs::write(tmp.join(".gitignore"), ".cosmon/\n.worktrees/\n").unwrap();
}

/// Init a **non-Cargo (polyglot)** repo with an optional extra config block.
/// There is deliberately NO `Cargo.toml`, so `cargo metadata` cannot resolve a
/// workspace from the root — the cascade exercises the polyglot path
/// (`build_command` fallback / `Unverified { expected: false }`) instead of the
/// cargo auto-detect rung.
fn setup_repo_polyglot(tmp: &Path, extra_config: &str) {
    git_ok(tmp, &["init", "-q", "-b", "main"]);
    git_ok(tmp, &["config", "user.email", "test@example.com"]);
    git_ok(tmp, &["config", "user.name", "Test"]);
    git_ok(tmp, &["config", "commit.gpgsign", "false"]);

    let cosmon = tmp.join(".cosmon");
    fs::create_dir_all(cosmon.join("state")).unwrap();
    fs::create_dir_all(cosmon.join("formulas")).unwrap();
    let mut config = String::from("[project]\nproject_id = \"test-polyglot-cascade\"\n");
    config.push_str(extra_config);
    fs::write(cosmon.join("config.toml"), config).unwrap();
    fs::write(cosmon.join("state/fleet.json"), "{}\n").unwrap();
    // A Python source stands in for the polyglot toolchain — no Cargo.toml.
    fs::create_dir_all(tmp.join("src")).unwrap();
    fs::write(tmp.join("src/main.py"), "print('hello')\n").unwrap();
    let formula_src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cosmon/formulas/task-work.formula.toml");
    fs::copy(&formula_src, cosmon.join("formulas/task-work.formula.toml")).unwrap();
    fs::write(tmp.join(".gitignore"), ".cosmon/\n.worktrees/\n").unwrap();
}

/// Base commit + a clean feat branch for a polyglot repo, where the branch's
/// single change is `branch_file` (created with `contents`). Returns the branch
/// name and main's HEAD before the merge, mirroring [`stage_clean_merge`].
fn stage_polyglot_merge(
    repo: &Path,
    mol_id: &str,
    branch_file: &str,
    contents: &str,
) -> (String, String) {
    let branch = format!("feat/{mol_id}");

    fs::write(repo.join("base.txt"), "base\n").unwrap();
    git_ok(repo, &["add", ".gitignore", "base.txt", "src"]);
    git_ok(repo, &["commit", "-q", "-m", "base"]);

    git_ok(repo, &["checkout", "-q", "-b", &branch]);
    if let Some(parent) = Path::new(branch_file).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(repo.join(parent)).unwrap();
        }
    }
    fs::write(repo.join(branch_file), contents).unwrap();
    git_ok(repo, &["add", branch_file]);
    git_ok(repo, &["commit", "-qm", "worker change"]);

    git_ok(repo, &["checkout", "-q", "main"]);

    let main_before = String::from_utf8_lossy(&git(repo, &["rev-parse", "main"]).stdout)
        .trim()
        .to_owned();
    (branch, main_before)
}

/// A `PATH` value with every directory containing a `cargo` executable removed,
/// so a child process spawned with it cannot resolve `cargo` — the Defect-4
/// "cargo absent from PATH" condition. Directories holding `git`/`sh` remain,
/// so the rest of `cs done` still works.
fn cargo_free_path() -> String {
    let orig = std::env::var("PATH").unwrap_or_default();
    orig.split(':')
        .filter(|dir| !dir.is_empty() && !Path::new(dir).join("cargo").exists())
        .collect::<Vec<_>>()
        .join(":")
}

/// Rung 1: a declared `[gates].integrity_command` that exits 0 is the verdict —
/// it runs verbatim (its stdout is visible) and the merge lands as a clean `ok`,
/// NOT `ok:unverified`. This proves cosmon delegates the WHAT rather than
/// hardcoding cargo.
#[test]
fn integrity_command_green_lands_clean_ok() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo_with_config(
        repo,
        "\n[gates]\nintegrity_command = 'echo GATE_RAN_INTEGRITY_CMD'\n",
    );

    let mol_id = nucleate_terminal(repo);
    let (_branch, _main_before) = stage_clean_merge(repo, &mol_id);

    let done = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");
    let stdout = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);

    assert!(
        done.status.success(),
        "a green integrity_command must let done proceed.\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("GATE_RAN_INTEGRITY_CMD"),
        "the declared integrity_command must actually run.\nstdout={stdout}\nstderr={stderr}"
    );
    let events = fs::read_to_string(repo.join(".cosmon/state/events.jsonl")).unwrap();
    let merge_lines: Vec<&str> = events
        .lines()
        .filter(|l| l.contains("merge_completed") && l.contains(&mol_id))
        .collect();
    assert!(
        merge_lines.iter().any(|l| l.contains("\"result\":\"ok\"")),
        "a verified integrity_command lands as clean ok.\nmerge_lines={merge_lines:#?}"
    );
    assert!(
        !merge_lines.iter().any(|l| l.contains("unverified")),
        "a verified merge must NOT be witnessed as unverified.\nmerge_lines={merge_lines:#?}"
    );
}

/// Rung 1: a declared `integrity_command` that exits non-zero is a gate ERROR —
/// exactly as load-bearing as `cargo check` failing. main is rolled back to its
/// pre-merge revision, done exits non-zero, and the worker branch is preserved
/// for repair. This is the polyglot analogue of
/// `post_merge_compile_gate_rejects_test_target_failure_and_resets_main`.
#[test]
fn integrity_command_red_rolls_back_and_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo_with_config(repo, "\n[gates]\nintegrity_command = 'exit 7'\n");

    let mol_id = nucleate_terminal(repo);
    let (branch, main_before) = stage_clean_merge(repo, &mol_id);

    let done = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");
    let stdout = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);

    assert!(
        !done.status.success(),
        "a red integrity_command must refuse done.\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("post_merge_compile_gate_refused")
            || stderr.contains("POST-MERGE COMPILE GATE REFUSED"),
        "the refusal must be loud and typed.\nstdout={stdout}\nstderr={stderr}"
    );
    let main_after = String::from_utf8_lossy(&git(repo, &["rev-parse", "main"]).stdout)
        .trim()
        .to_owned();
    assert_eq!(
        main_after, main_before,
        "main must be reset to its pre-merge revision on a red integrity_command"
    );
    assert!(
        !git(repo, &["cat-file", "-e", "main:worker.txt"])
            .status
            .success(),
        "the worker content must not remain on main after rollback"
    );
    assert!(
        git(repo, &["rev-parse", "--verify", &branch])
            .status
            .success(),
        "the worker branch must be preserved for repair"
    );
}

/// D1 fail-closed policy — the FLAG isolated (task-20260715-ff5b). An
/// `expected: false` Unverified (a non-Cargo repo with NOTHING declared to
/// verify it) lands fail-open by default, but the operator opt-in
/// `[gates].fail_closed_on_unverified = true` promotes it to a ROLLBACK. done
/// fails, main is reset, and the durable witness is an `error:` — never a landed
/// `ok:unverified`.
///
/// Isolating `expected: false` here is deliberate: it makes the FLAG the sole
/// cause of the rollback. An `expected: true` Unverified would fail closed on
/// its own (the ratified discriminator), so it could not falsify the flag.
///
/// FALSIFIER: dropping the `fail_closed_on_unverified` disjunct from
/// `fail_closed = expected || fail_closed_on_unverified` reddens this — with
/// `expected: false` and no flag the merge would land as `ok:unverified`.
#[test]
fn fail_closed_flag_promotes_expected_false_unverified_to_rollback() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    // Non-Cargo repo + the flag: nothing is declared to verify the tree
    // (expected:false), and the operator promotes that to fail-closed.
    setup_repo_polyglot(repo, "\n[gates]\nfail_closed_on_unverified = true\n");

    let mol_id = nucleate_terminal(repo);
    // Loose Rust source in a repo with no Cargo workspace → the cascade declines
    // to the terminal `Unverified { expected: false }`.
    let (_branch, main_before) =
        stage_polyglot_merge(repo, &mol_id, "scripts/tool.rs", "fn main() {}\n");

    let done = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");
    let stdout = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);

    assert!(
        !done.status.success(),
        "fail_closed_on_unverified must refuse an expected:false Unverified merge.\nstdout={stdout}\nstderr={stderr}"
    );
    let main_after = String::from_utf8_lossy(&git(repo, &["rev-parse", "main"]).stdout)
        .trim()
        .to_owned();
    assert_eq!(
        main_after, main_before,
        "main must be reset when fail_closed_on_unverified rolls back an Unverified merge"
    );
    let events = fs::read_to_string(repo.join(".cosmon/state/events.jsonl")).unwrap();
    let merge_lines: Vec<&str> = events
        .lines()
        .filter(|l| l.contains("merge_completed") && l.contains(&mol_id))
        .collect();
    assert!(
        merge_lines.iter().any(|l| l.contains("\"result\":\"error:")),
        "the rollback must be witnessed as an error, not a landed ok:unverified.\nmerge_lines={merge_lines:#?}"
    );
    assert!(
        !merge_lines.iter().any(|l| l.contains("ok:unverified")),
        "a fail-closed rollback must NOT also write an ok:unverified witness.\nmerge_lines={merge_lines:#?}"
    );
}

/// D1 ratified discriminator (Defect 5, task-20260715-ff5b): with **default
/// config** (no `fail_closed_on_unverified`), an `Unverified { expected: true }`
/// on a code diff — a gate WAS expected (cargo resolved the workspace) but a
/// loose `.rs` mapped to no member, so nothing verified it — must fail **CLOSED
/// by default**: main is reset, done exits non-zero, and the witness is an
/// `error:`. This protects cosmon-on-cosmon's own net without any opt-in.
///
/// FALSIFIER: reverting `fail_closed = expected || fail_closed_on_unverified`
/// back to `fail_closed = fail_closed_on_unverified` (dropping the `expected`
/// disjunct) reddens this — with default config the merge would land as
/// `ok:unverified` instead of rolling back.
#[test]
fn default_config_expected_true_unverified_fails_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    // A confirmed Cargo workspace, DEFAULT gates (no fail_closed flag).
    setup_repo_with_config(repo, "");

    let mol_id = nucleate_terminal(repo);
    let (branch, main_before) = stage_clean_merge(repo, &mol_id);

    // Loose Rust source that maps to no workspace member — cargo resolves the
    // root workspace (member `app`) but cannot bound this file's blast radius,
    // so the gate reports `Unverified { expected: true }` on a code diff.
    git_ok(repo, &["checkout", "-q", &branch]);
    fs::create_dir_all(repo.join("scripts")).unwrap();
    fs::write(repo.join("scripts/tool.rs"), "fn main() {}\n").unwrap();
    git_ok(repo, &["add", "scripts/tool.rs"]);
    git_ok(repo, &["commit", "-qm", "add loose rust script"]);
    git_ok(repo, &["checkout", "-q", "main"]);

    let done = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");
    let stdout = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);

    assert!(
        !done.status.success(),
        "expected:true Unverified must fail CLOSED by default (D1).\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("post_merge_compile_gate_refused")
            || stderr.contains("POST-MERGE COMPILE GATE REFUSED"),
        "the D1 fail-closed refusal must be loud and typed.\nstdout={stdout}\nstderr={stderr}"
    );
    let main_after = String::from_utf8_lossy(&git(repo, &["rev-parse", "main"]).stdout)
        .trim()
        .to_owned();
    assert_eq!(
        main_after, main_before,
        "main must be reset when expected:true fails closed by default"
    );
    let events = fs::read_to_string(repo.join(".cosmon/state/events.jsonl")).unwrap();
    let merge_lines: Vec<&str> = events
        .lines()
        .filter(|l| l.contains("merge_completed") && l.contains(&mol_id))
        .collect();
    assert!(
        merge_lines
            .iter()
            .any(|l| l.contains("\"result\":\"error:")),
        "the D1 default rollback must be witnessed as an error.\nmerge_lines={merge_lines:#?}"
    );
    assert!(
        !merge_lines.iter().any(|l| l.contains("ok:unverified")),
        "an expected:true fail-closed rollback must NOT write an ok:unverified witness.\nmerge_lines={merge_lines:#?}"
    );
}

/// Defect 3 (task-20260715-ff5b): a polyglot (non-Cargo) repo that declares a
/// `[gates].build_command` runs that command UNCONDITIONALLY for a non-Rust
/// code change. The `.py` change is "not build-relevant" to cargo cognition,
/// but the declared command must still run (rung 3) rather than short-circuit to
/// a clean `NothingToVerify`. Proof: the command's marker appears in stdout and
/// the merge lands as a clean `ok` (Verified), not `ok:unverified`.
///
/// FALSIFIER: reverting the docs-only short-circuit to ignore `has_fallback`
/// (concluding `NothingToVerify` for the `.py` diff) reddens this — the command
/// never runs, so the marker is absent.
#[test]
fn build_command_runs_for_non_rust_change_in_polyglot_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo_polyglot(
        repo,
        "\n[gates]\nbuild_command = 'echo BUILD_CMD_RAN_VIA_FALLBACK'\n",
    );

    let mol_id = nucleate_terminal(repo);
    // A pure Python (non-Rust) code change: cargo cognition sees nothing
    // build-relevant, so a buggy short-circuit would skip the declared command.
    let (_branch, _main_before) = stage_polyglot_merge(
        repo,
        &mol_id,
        "src/feature.py",
        "def feature():\n    return 1\n",
    );

    let done = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");
    let stdout = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);

    assert!(
        done.status.success(),
        "a green build_command must let done proceed.\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("BUILD_CMD_RAN_VIA_FALLBACK"),
        "the declared build_command MUST run for a non-Rust change (Defect 3).\nstdout={stdout}\nstderr={stderr}"
    );
    let events = fs::read_to_string(repo.join(".cosmon/state/events.jsonl")).unwrap();
    let merge_lines: Vec<&str> = events
        .lines()
        .filter(|l| l.contains("merge_completed") && l.contains(&mol_id))
        .collect();
    assert!(
        merge_lines.iter().any(|l| l.contains("\"result\":\"ok\"")),
        "a verified build_command lands as clean ok.\nmerge_lines={merge_lines:#?}"
    );
    assert!(
        !merge_lines.iter().any(|l| l.contains("unverified")),
        "a build_command-verified merge must NOT be witnessed as unverified.\nmerge_lines={merge_lines:#?}"
    );
}

/// Defect 4 (task-20260715-ff5b): a Cargo workspace with a `build_command`
/// fallback and a Rust (cargo-classified) change, but `cargo` ABSENT from
/// `PATH`, must fall through to `build_command` — never hard-error into a
/// rollback for a mere absence of cargo. "cargo cannot be spawned" is the
/// auto-detect declining, not an integrity failure.
///
/// FALSIFIER: reverting `cargo_metadata_bounded` to propagate the spawn error
/// (`run_bounded_capture(...)?`) instead of mapping it to `Ok(None)` reddens
/// this — the cargo-absent spawn error would roll the merge back.
#[test]
fn cargo_absent_falls_through_to_build_command() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo_with_config(
        repo,
        "\n[gates]\nbuild_command = 'echo BUILD_CMD_RAN_CARGO_ABSENT'\n",
    );

    let mol_id = nucleate_terminal(repo);
    let (branch, _main_before) = stage_clean_merge(repo, &mol_id);

    // A Rust change (cargo-classified) so the cascade enters the cargo rung —
    // where the spawn fails because cargo is absent from PATH.
    git_ok(repo, &["checkout", "-q", &branch]);
    fs::write(
        repo.join("app/src/lib.rs"),
        "pub fn healthy() {}\npub fn added() {}\n",
    )
    .unwrap();
    git_ok(repo, &["add", "app/src/lib.rs"]);
    git_ok(repo, &["commit", "-qm", "edit crate source"]);
    git_ok(repo, &["checkout", "-q", "main"]);

    let done = cs_isolated(repo)
        .env("PATH", cargo_free_path())
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");
    let stdout = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);

    assert!(
        done.status.success(),
        "cargo absent must fall through to build_command, not roll back.\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("BUILD_CMD_RAN_CARGO_ABSENT"),
        "the build_command fallback MUST run when cargo is absent (Defect 4).\nstdout={stdout}\nstderr={stderr}"
    );
    let events = fs::read_to_string(repo.join(".cosmon/state/events.jsonl")).unwrap();
    let merge_lines: Vec<&str> = events
        .lines()
        .filter(|l| l.contains("merge_completed") && l.contains(&mol_id))
        .collect();
    assert!(
        merge_lines.iter().any(|l| l.contains("\"result\":\"ok\"")),
        "the fallback-verified merge lands as clean ok.\nmerge_lines={merge_lines:#?}"
    );
}

/// Defect 1 wiring, guarded (task-20260715-ff5b): a delegated `integrity_command`
/// under an EXPOSED (`COSMON_API_REQUEST=1`) `deny-external` dispatch is routed
/// through the egress jail. On a host that cannot kernel-enforce the jail
/// (`netns_available() == false`, e.g. macOS), the gate REFUSES fail-closed —
/// the repo-supplied shell never runs and the merge rolls back. On a
/// netns-capable host the command runs jailed instead, so this end-to-end wiring
/// assertion is scoped to the non-enforceable branch (the pure decision is
/// covered host-independently in `cmd::egress_delegate` unit tests).
#[test]
fn delegated_command_egress_refused_on_unenforceable_exposed_host() {
    if cosmon_agent_harness::egress_probe::netns_available() {
        eprintln!(
            "SKIP delegated_command_egress_refused_on_unenforceable_exposed_host: \
             host can kernel-enforce the netns jail; the delegated command runs jailed \
             rather than refused. The pure refusal decision is covered in \
             cmd::egress_delegate unit tests."
        );
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    // The command's ONLY observable is a side-effect file: if it runs
    // unconfined, `ran-proof.txt` appears in the repo. The marker never leaks
    // into the refusal message (which names the command text), so its absence
    // is an honest "the shell never executed" witness.
    setup_repo_with_config(
        repo,
        "\n[gates]\nintegrity_command = 'touch ran-proof.txt'\n",
    );

    let mol_id = nucleate_terminal(repo);
    let (_branch, main_before) = stage_clean_merge(repo, &mol_id);

    let done = cs_isolated(repo)
        // Exposed multi-tenant + strict-local egress that this host cannot
        // enforce ⇒ the delegated command must be refused fail-closed.
        .env("COSMON_EGRESS_POLICY", "deny-external")
        .env("COSMON_API_REQUEST", "1")
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");
    let stdout = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);

    assert!(
        !done.status.success(),
        "an unenforceable exposed deny-external delegated command must fail closed.\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !repo.join("ran-proof.txt").exists(),
        "the repo-supplied shell must NEVER run unconfined when the jail is refused.\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("egress fail-closed") || stderr.contains("egress fail-closed"),
        "the refusal must name the egress fail-closed cause.\nstdout={stdout}\nstderr={stderr}"
    );
    let main_after = String::from_utf8_lossy(&git(repo, &["rev-parse", "main"]).stdout)
        .trim()
        .to_owned();
    assert_eq!(
        main_after, main_before,
        "main must be reset when the delegated command is refused fail-closed"
    );
}
