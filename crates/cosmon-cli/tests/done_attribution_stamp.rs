// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end CLI coverage for native attribution stamping on `cs done`
//! (delib-20260717-194b; hardened after the pré-mortem task-20260717-ffe1).
//!
//! THE CONTRACT exercised here, against the **real `cs` binary** (same
//! isolation discipline as `done_merge_conflict.rs`):
//!
//!   * default `--strategy merge` + configured `[attribution]` + a durable
//!     adapter witness ⇒ the merge commit `cs done` creates carries
//!     `Co-Authored-By: <maker> (<adapter>) <coauthor_email>` as a trailer git
//!     itself parses, every published commit is operator-authored AND
//!     operator-committed, and the worker commit is never rewritten;
//!   * no adapter witness in the event log ⇒ the maker trailer still rides
//!     (no parenthetical), and the omission is warned about, never silent (C5);
//!   * `--strategy ff-only` + configured attribution ⇒ refused before any git
//!     mutation — a fast-forward has no commit to carry the trailers (C1);
//!   * missing operator git identity ⇒ fail-closed refusal, not a silently
//!     skipped author assertion (C2);
//!   * `[attribution]` without `coauthor_email` ⇒ the merge proceeds unstamped
//!     but the fail-open corner is surfaced as a warning (C4).
//!
//! The hermetic unit tests in `cmd::done` cover each layer in isolation
//! (`ensure_attribution_carrier`, `collect_non_operator_authored`,
//! `try_merge_branch`, `warn_unstamped_attribution`); this file proves the
//! command-level wiring produces the right git bytes.

use std::fs;
use std::path::Path;
use std::process::Command;

use cosmon_core::event_v2::ModelSelectionSource;
use cosmon_core::id::MoleculeId;
use cosmon_state::events::worker_spawn::emit_model_selected;

/// The operator identity every test repo configures — the only identity
/// allowed in author/committer slots.
const OPERATOR_NAME: &str = "Test";
const OPERATOR_EMAIL: &str = "test@example.com";

fn cs() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

/// `cs` invocation pinned to an isolated state dir and run from inside the
/// project repo (so `find_repo_root()` resolves to the temp git repo).
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

fn git_stdout(repo: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&git(repo, args).stdout)
        .trim()
        .to_owned()
}

/// Init a git repo with a `.cosmon` project whose state is **gitignored**
/// and whose config carries the given `[attribution]` block (verbatim TOML,
/// appended after `[project]`).
fn setup_repo(tmp: &Path, attribution_toml: &str) {
    git_ok(tmp, &["init", "-q", "-b", "main"]);
    git_ok(tmp, &["config", "user.email", OPERATOR_EMAIL]);
    git_ok(tmp, &["config", "user.name", OPERATOR_NAME]);
    git_ok(tmp, &["config", "commit.gpgsign", "false"]);

    let cosmon = tmp.join(".cosmon");
    fs::create_dir_all(cosmon.join("state")).unwrap();
    fs::create_dir_all(cosmon.join("formulas")).unwrap();
    fs::write(
        cosmon.join("config.toml"),
        format!("[project]\nproject_id = \"test-done-attribution\"\n\n{attribution_toml}"),
    )
    .unwrap();
    fs::write(cosmon.join("state/fleet.json"), "{}\n").unwrap();
    let formula_src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cosmon/formulas/task-work.formula.toml");
    fs::copy(&formula_src, cosmon.join("formulas/task-work.formula.toml")).unwrap();

    // Keep all cosmon state + worktrees out of git history.
    fs::write(tmp.join(".gitignore"), ".cosmon/\n.worktrees/\n").unwrap();
}

/// The canonical fully-configured attribution block used by the stamping
/// tests — same shape as the production `.cosmon/config.toml`.
const FULL_ATTRIBUTION: &str = "[attribution]\n\
     public_name = \"Noogram\"\n\
     public_url = \"noogram.org\"\n\
     coauthor_email = \"noreply@noogram.org\"\n";

/// Nucleate a `task-work` molecule and drive it to a terminal state
/// (collapsed) so `cs done` will attempt the merge without `--force`.
fn nucleate_terminal(repo: &Path) -> String {
    let nuc = cs_isolated(repo)
        .args([
            "--json",
            "nucleate",
            "task-work",
            "--var",
            "topic=attribution stamp integration test",
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

/// Build the standard branch topology: a base commit on main, one
/// operator-authored worker commit on `feat/<mol>`, main checked out.
/// Returns the worker commit SHA.
fn seed_worker_branch(repo: &Path, branch: &str) -> String {
    fs::write(repo.join("base.txt"), "base\n").unwrap();
    git_ok(repo, &["add", ".gitignore", "base.txt"]);
    git_ok(repo, &["commit", "-q", "-m", "base"]);

    git_ok(repo, &["checkout", "-q", "-b", branch]);
    fs::write(repo.join("worker.txt"), "worker\n").unwrap();
    git_ok(repo, &["add", "worker.txt"]);
    git_ok(repo, &["commit", "-qm", "feat: worker deliverable"]);
    let sha = git_stdout(repo, &["rev-parse", "HEAD"]);
    git_ok(repo, &["checkout", "-q", "main"]);
    sha
}

/// Plant the durable adapter witness `cs done` folds the trailer from —
/// the same `ModelSelected` event `cs tackle` emits before a real spawn.
fn plant_adapter_witness(repo: &Path, mol_id: &str, adapter: &str) {
    let state_dir = repo.join(".cosmon/state");
    let mol = MoleculeId::new(mol_id).expect("valid molecule id");
    emit_model_selected(
        &state_dir,
        &mol,
        adapter,
        None,
        ModelSelectionSource::Flag {
            flag: "test-model".to_owned(),
        },
    );
}

/// The nominal contract: merge commit stamped with maker (adapter) trailer,
/// every published commit operator-authored, worker commit unrewritten.
#[test]
fn cs_done_stamps_merge_commit_with_trailer_and_operator_author() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo, FULL_ATTRIBUTION);

    let mol_id = nucleate_terminal(repo);
    let branch = format!("feat/{mol_id}");
    let worker_sha = seed_worker_branch(repo, &branch);
    plant_adapter_witness(repo, &mol_id, "claude");

    let done = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");
    let stdout = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);
    assert!(
        done.status.success(),
        "cs done must succeed.\nstdout={stdout}\nstderr={stderr}"
    );

    // HEAD on main is the merge commit cs done created: two parents, the
    // second being the untouched worker commit (no rewrite — same SHA).
    let parents = git_stdout(repo, &["log", "-1", "--format=%P", "main"]);
    let parent_shas: Vec<&str> = parents.split_whitespace().collect();
    assert_eq!(parent_shas.len(), 2, "expected a merge commit: {parents}");
    assert_eq!(
        parent_shas[1], worker_sha,
        "the worker commit must be integrated by SHA, never rewritten"
    );

    // git itself parses the trailer — the adapter witness rides in the
    // display name, the address is the stable maker address (no synthetic
    // per-model email).
    let trailers = git_stdout(
        repo,
        &["log", "-1", "--format=%(trailers:key=Co-Authored-By)", "main"],
    );
    assert!(
        trailers.contains("Noogram (claude) <noreply@noogram.org>"),
        "merge commit must carry the maker(adapter) trailer: {trailers:?}"
    );
    assert!(
        !trailers.contains("claude@"),
        "no synthetic adapter email may be minted: {trailers:?}"
    );

    // Every commit now reachable on main (worker + merge) is authored AND
    // committed by the operator — maker/adapter never occupy identity slots.
    let idents = git_stdout(repo, &["log", "--format=%an|%ae|%cn|%ce", "main"]);
    for line in idents.lines() {
        assert_eq!(
            line,
            format!("{OPERATOR_NAME}|{OPERATOR_EMAIL}|{OPERATOR_NAME}|{OPERATOR_EMAIL}"),
            "non-operator identity leaked into an author/committer slot"
        );
    }
}

/// C5 visibility: no `ModelSelected` in the event log ⇒ maker-only trailer
/// (no invented adapter) and an explicit warning — never silence.
#[test]
fn cs_done_without_adapter_witness_stamps_maker_only_and_warns() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo, FULL_ATTRIBUTION);

    let mol_id = nucleate_terminal(repo);
    let branch = format!("feat/{mol_id}");
    seed_worker_branch(repo, &branch);
    // Deliberately NO plant_adapter_witness.

    let done = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");
    let stdout = String::from_utf8_lossy(&done.stdout);
    assert!(
        done.status.success(),
        "cs done must still integrate: {stdout}"
    );

    let trailers = git_stdout(
        repo,
        &["log", "-1", "--format=%(trailers:key=Co-Authored-By)", "main"],
    );
    assert!(
        trailers.contains("Noogram <noreply@noogram.org>"),
        "maker trailer must ride without an adapter witness: {trailers:?}"
    );
    assert!(
        !trailers.contains('('),
        "no adapter may be invented when the log is silent: {trailers:?}"
    );
    assert!(
        stdout.contains("no adapter witness"),
        "the missing witness must be warned about, not silent: {stdout}"
    );
}

/// C1: `--strategy ff-only` under configured attribution is refused before
/// any git mutation — a fast-forward creates no commit to carry the trailers.
#[test]
fn cs_done_refuses_ff_only_under_configured_attribution() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo, FULL_ATTRIBUTION);

    let mol_id = nucleate_terminal(repo);
    let branch = format!("feat/{mol_id}");
    seed_worker_branch(repo, &branch);
    plant_adapter_witness(repo, &mol_id, "claude");
    let main_before = git_stdout(repo, &["rev-parse", "main"]);

    let done = cs_isolated(repo)
        .args([
            "--json",
            "done",
            &mol_id,
            "--no-auto-propel",
            "--strategy",
            "ff-only",
        ])
        .output()
        .expect("cs done");
    let stdout = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);

    assert!(
        !done.status.success(),
        "ff-only + attribution must be refused.\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stderr.contains("ff-only") && stderr.contains("no merge commit"),
        "the refusal must name the contradiction: {stderr}"
    );
    // Nothing moved, nothing lost: main untouched, worker branch preserved.
    assert_eq!(git_stdout(repo, &["rev-parse", "main"]), main_before);
    assert!(
        git(repo, &["rev-parse", "--verify", &branch]).status.success(),
        "the worker branch must survive the refusal"
    );
}

/// C2: attribution-enabled integration with an incomplete operator git
/// identity fails closed — it must NOT silently skip the author assertion.
#[test]
fn cs_done_fails_closed_without_operator_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo, FULL_ATTRIBUTION);

    let mol_id = nucleate_terminal(repo);
    let branch = format!("feat/{mol_id}");
    seed_worker_branch(repo, &branch);
    plant_adapter_witness(repo, &mol_id, "claude");
    let main_before = git_stdout(repo, &["rev-parse", "main"]);

    // Blank the local identity AFTER the commits exist — the local empty
    // values shadow any developer/CI global config, reproducing the bare
    // checkout the pré-mortem flagged.
    git_ok(repo, &["config", "user.name", ""]);
    git_ok(repo, &["config", "user.email", ""]);

    let done = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");
    let stdout = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);

    assert!(
        !done.status.success(),
        "missing operator identity must abort integration.\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stderr.contains("refuses attribution-enabled integration"),
        "the refusal must be the fail-closed identity error: {stderr}"
    );
    assert_eq!(
        git_stdout(repo, &["rev-parse", "main"]),
        main_before,
        "main must not move when the identity gate refuses"
    );
    assert!(
        git(repo, &["rev-parse", "--verify", &branch]).status.success(),
        "the worker branch must survive the refusal"
    );
}

/// C4 visibility: `[attribution]` configured but `coauthor_email` empty ⇒ the
/// merge proceeds unstamped (the facet is opt-in), and the fail-open corner
/// is surfaced as a warning instead of passing in silence.
#[test]
fn cs_done_warns_when_attribution_has_no_coauthor_email() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(
        repo,
        "[attribution]\npublic_name = \"Noogram\"\npublic_url = \"noogram.org\"\n",
    );

    let mol_id = nucleate_terminal(repo);
    let branch = format!("feat/{mol_id}");
    seed_worker_branch(repo, &branch);
    plant_adapter_witness(repo, &mol_id, "claude");

    let done = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");
    let stdout = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);
    assert!(
        done.status.success(),
        "an unstamped-but-legal config must still integrate.\nstdout={stdout}\nstderr={stderr}"
    );

    // The merge commit is byte-honest: no trailer was stamped.
    let trailers = git_stdout(
        repo,
        &["log", "-1", "--format=%(trailers:key=Co-Authored-By)", "main"],
    );
    assert!(
        trailers.is_empty(),
        "no trailer may be stamped without coauthor_email: {trailers:?}"
    );
    // …and the omission is visible at integration time (C4).
    assert!(
        stdout.contains("coauthor_email"),
        "the unstamped integration must be warned about: {stdout}"
    );
}
