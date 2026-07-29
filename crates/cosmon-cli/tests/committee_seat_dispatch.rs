// SPDX-License-Identifier: AGPL-3.0-only

//! Lifecycle contracts a cross-provider committee seat depends on, asserted
//! **through the `cs` binary** — `cs tackle` and `cs complete` — rather than
//! against a predicate.
//!
//! # Why through the command
//!
//! Every defect these tests pin was invisible to unit tests that passed.
//! `reinstate_committee_posture_reference` had its own green tests and one
//! production call site — in `cs evolve` — so it proved the adversarial
//! contract *survives regeneration* while nothing asked whether it existed
//! *before the first regeneration*, and nothing asked whether it survived the
//! verb every seat ENDS with. And
//! `classify_model_composition`'s siblings in `provider_diversity` all pass
//! their own tests while an illegal `(adapter, model)` pair was dispatched
//! anyway, because no production path consulted one. A check that is only
//! ever exercised in isolation measures the property next to the one that
//! matters, so these tests run the binary.
//!
//! Every dispatch case uses `cs tackle --dry-run`: no tmux session, no
//! worktree, no paid probe — and every gate under test runs before any of
//! that. The completion cases run `cs complete` on the same fixture, which
//! touches only the molecule directory.

use std::fs;
use std::path::Path;
use std::process::Command;

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    // Hermetic resolution chains: strip the operator's session hammers, or
    // the composition case measures whatever the developer exported rather
    // than what the test pinned. `$ANTHROPIC_MODEL` in particular is the
    // variable that produced the measured incident.
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_DEFAULT_ADAPTER")
        .env_remove("COSMON_DEFAULT_MODEL")
        .env_remove("ANTHROPIC_MODEL")
        // `cs tackle` inside a `cargo test` run inherits the worker
        // call-depth counter and every dispatch trips the recursion guard
        // before it reaches the gates under test.
        .env_remove("CB_DEPTH");
    cmd
}

fn cosmon_bin_in(cwd: &Path) -> Command {
    let mut cmd = cosmon_bin();
    cmd.current_dir(cwd);
    // Point the global-config tier at an empty dir so the test never reads
    // the operator's real `~/.config/cosmon/config.toml`.
    cmd.env("COSMON_CONFIG_HOME", cwd.join("isolated-config-home"));
    cmd
}

/// A tempdir project with a one-step formula and one nucleated molecule.
///
/// `adapters_block` is spliced into `.cosmon/config.toml` so a case can
/// declare `[adapters.default]` (or a `base_url`) without a second helper.
fn setup(adapters_block: &str) -> (tempfile::TempDir, std::path::PathBuf, String) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    fs::create_dir_all(&formulas_dir).expect("formulas dir");

    fs::write(
        formulas_dir.join("seat-test.formula.toml"),
        r#"
formula = "seat-test"
version = 1
description = "One-step formula standing in for a committee seat"
id_prefix = "seat"

[[steps]]
id = "step-1"
title = "Step 1"
description = "Solo step — the seat would read the artefacts here."
acceptance = "Done"
"#,
    )
    .expect("write formula");

    let state_str = state_dir.to_str().expect("utf-8 state dir");
    let out = cosmon_bin_in(tmp.path())
        .args([
            "--json",
            "nucleate",
            "seat-test",
            "--store-dir",
            state_str,
            "--formulas-dir",
            formulas_dir.to_str().expect("utf-8 formulas dir"),
        ])
        .output()
        .expect("nucleate spawned");
    assert!(
        out.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("nucleate json");
    let mol_id = json["id"].as_str().expect("nucleate id").to_owned();

    let cosmon_dir = tmp.path().join(".cosmon");
    fs::create_dir_all(&cosmon_dir).expect("cosmon dir");
    fs::write(
        cosmon_dir.join("config.toml"),
        format!("[project]\nproject_id = \"seat-test-a1a1\"\n\n{adapters_block}"),
    )
    .expect("write config");

    (tmp, state_dir, mol_id)
}

fn molecule_dir(state_dir: &Path, mol_id: &str) -> std::path::PathBuf {
    state_dir
        .join("fleets")
        .join("default")
        .join("molecules")
        .join(mol_id)
}

fn tackle_dry_run(
    tmp: &Path,
    state_dir: &Path,
    mol_id: &str,
    extra: &[&str],
) -> std::process::Output {
    let mut cmd = cosmon_bin_in(tmp);
    cmd.args([
        "tackle",
        mol_id,
        "--dry-run",
        "--no-worktree",
        "--config",
        state_dir.to_str().expect("utf-8 state dir"),
    ]);
    cmd.args(extra);
    cmd.output().expect("tackle spawned")
}

// ── F3 — the posture pointer must exist on STEP 1, not only after an evolve ──

/// The measured hole: immediately after `cs nucleate` plus writing the
/// durable `committee-posture.md`, the seat's `briefing.md` referenced it
/// **zero** times, because only `cs evolve` re-established the pointer. So
/// `AdversarialBriefing::from_durable_injection` returned `injected = false`
/// for every seat on its first step — the step on which the verdict contract
/// must be written, and the one a provider refusal is most likely to end on.
#[test]
fn tackle_delivers_the_committee_posture_pointer_on_the_first_step() {
    let (tmp, state_dir, mol_id) = setup("");
    let mol_dir = molecule_dir(&state_dir, &mol_id);
    let posture = cosmon_core::committee::render_committee_posture(
        cosmon_core::committee::ADVERSARIAL_BRIEFING_VERSION,
        "blake3:test",
        "Audit the artefacts. The generator's confidence is not evidence.",
    );
    fs::write(
        mol_dir.join(cosmon_core::committee::COMMITTEE_POSTURE_FILE),
        posture,
    )
    .expect("write posture");

    // Precondition, stated rather than assumed: nucleation alone leaves the
    // briefing with no pointer. Without this the test could pass on a
    // briefing that happened to carry one all along.
    let before = fs::read_to_string(mol_dir.join("briefing.md")).unwrap_or_default();
    assert!(
        !before.contains(cosmon_core::committee::COMMITTEE_POSTURE_FILE),
        "precondition: a freshly nucleated seat must NOT already carry the \
         pointer, or this test proves nothing about tackle"
    );

    let out = tackle_dry_run(tmp.path(), &state_dir, &mol_id, &[]);
    assert!(
        out.status.success(),
        "tackle --dry-run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let after = fs::read_to_string(mol_dir.join("briefing.md")).expect("briefing.md after tackle");
    assert!(
        after.contains(cosmon_core::committee::COMMITTEE_POSTURE_FILE),
        "the seat's briefing must point at its durable adversarial contract \
         on step 1; got:\n{after}"
    );

    // And the two-fact test the persona witness actually runs must now pass.
    let briefing = cosmon_core::committee::AdversarialBriefing::from_durable_injection(
        cosmon_core::committee::ADVERSARIAL_BRIEFING_VERSION,
        "blake3:test",
        mol_dir
            .join(cosmon_core::committee::COMMITTEE_POSTURE_FILE)
            .exists(),
        after.contains(cosmon_core::committee::committee_posture_reference().trim()),
    );
    assert!(
        briefing.is_valid(),
        "witness (2) must be satisfiable on step 1, not only after an evolve"
    );
}

/// **R1.** `cs tackle` re-establishes the POINTER; it does not author the file
/// the pointer points at.
///
/// The reconcile-scope doc comment said `cs tackle` / `cs evolve` "write"
/// `committee-posture.md`, which would make its presence attest a dispatch. It
/// attests a *convening*: the driver renders it into the seat's directory, and
/// both dispatch verbs return early when it is absent. This is the second half
/// of the probe — delete the file, run tackle, and nothing recreates it — and it
/// is what makes door 2 of `molecule_owes_a_seat_verdict` mean what it now says.
#[test]
fn tackle_does_not_author_the_posture_file_it_points_at() {
    let (tmp, state_dir, mol_id) = setup("");
    let mol_dir = molecule_dir(&state_dir, &mol_id);
    let posture_path = mol_dir.join(cosmon_core::committee::COMMITTEE_POSTURE_FILE);
    let posture = cosmon_core::committee::render_committee_posture(
        cosmon_core::committee::ADVERSARIAL_BRIEFING_VERSION,
        "blake3:test",
        "Audit the artefacts. The generator's confidence is not evidence.",
    );
    fs::write(&posture_path, posture).expect("write posture");

    // With the convener's file in place, tackle delivers the pointer...
    let out = tackle_dry_run(tmp.path(), &state_dir, &mol_id, &[]);
    assert!(out.status.success(), "tackle --dry-run failed");
    assert!(
        fs::read_to_string(mol_dir.join("briefing.md"))
            .unwrap_or_default()
            .contains(cosmon_core::committee::COMMITTEE_POSTURE_FILE),
        "precondition: tackle must deliver the pointer, or the removal below \
         proves nothing about which verb authors the file"
    );

    // ...and with the file removed, tackle does NOT bring it back.
    fs::remove_file(&posture_path).expect("remove posture");
    let out = tackle_dry_run(tmp.path(), &state_dir, &mol_id, &[]);
    assert!(out.status.success(), "tackle --dry-run failed");
    assert!(
        !posture_path.exists(),
        "`cs tackle` may not author `{}` — the convening driver does, and a \
         seat that could grow its own contract on dispatch would be attesting \
         its own convening",
        cosmon_core::committee::COMMITTEE_POSTURE_FILE
    );
}

/// The other half of "free for everything else": a molecule that is not a
/// committee seat must come out of `cs tackle` byte-identical. A gate that
/// stamped a posture stanza onto every briefing would be a different bug.
#[test]
fn tackle_leaves_a_non_seat_briefing_untouched() {
    let (tmp, state_dir, mol_id) = setup("");
    let mol_dir = molecule_dir(&state_dir, &mol_id);
    let before = fs::read_to_string(mol_dir.join("briefing.md")).unwrap_or_default();

    let out = tackle_dry_run(tmp.path(), &state_dir, &mol_id, &[]);
    assert!(
        out.status.success(),
        "tackle --dry-run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let after = fs::read_to_string(mol_dir.join("briefing.md")).unwrap_or_default();
    assert_eq!(
        before, after,
        "a molecule with no durable committee-posture.md is not a seat; its \
         briefing must be untouched"
    );
    assert!(
        !after.contains("Committee posture"),
        "no posture stanza may appear on a non-seat briefing"
    );
}

// ── U1 — the pointer must survive the verb every seat ENDS with ─────────────

/// Run `cs complete` on the fixture molecule.
fn complete(tmp: &Path, state_dir: &Path, mol_id: &str) -> std::process::Output {
    let mut cmd = cosmon_bin_in(tmp);
    cmd.args([
        "complete",
        mol_id,
        // The fixture is a bare tempdir with no git repository, so the visual
        // mindguard has nothing to read; this is the escape hatch it documents
        // for exactly that case, not a bypass of a refusal.
        "--ignore-mindguard",
        "--ops-dir",
        state_dir.to_str().expect("utf-8 state dir"),
    ]);
    cmd.output().expect("complete spawned")
}

/// **The falsifier.** `cs complete` rewrites `briefing.md` down to a terse
/// COMPLETED line. Before this fix that rewrite dropped the seat's pointer at
/// its durable `committee-posture.md`, so witness (2) `BriefingNotInjected` was
/// satisfiable **only while the seat was running** and false from the moment it
/// finished — which is the only moment an auditor, a release gate or
/// `cs reconcile --check` ever reads a seat's record.
///
/// Measured on committee-20260728-2d37's two seats before the fix: pointer
/// present after `cs tackle`, `grep -c committee-posture.md briefing.md == 0`
/// after `cs complete`, both seats.
#[test]
fn complete_preserves_the_committee_posture_pointer() {
    let (tmp, state_dir, mol_id) = setup("");
    let mol_dir = molecule_dir(&state_dir, &mol_id);
    let posture = cosmon_core::committee::render_committee_posture(
        cosmon_core::committee::ADVERSARIAL_BRIEFING_VERSION,
        "blake3:test",
        "Audit the artefacts. The generator's confidence is not evidence.",
    );
    fs::write(
        mol_dir.join(cosmon_core::committee::COMMITTEE_POSTURE_FILE),
        posture,
    )
    .expect("write posture");

    // Precondition: the seat is dispatched and carries the pointer. Without
    // this the assertion below could pass on a briefing that never had one.
    let out = tackle_dry_run(tmp.path(), &state_dir, &mol_id, &[]);
    assert!(
        out.status.success(),
        "tackle --dry-run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fs::read_to_string(mol_dir.join("briefing.md"))
            .unwrap_or_default()
            .contains(cosmon_core::committee::COMMITTEE_POSTURE_FILE),
        "precondition: tackle must deliver the pointer"
    );

    let out = complete(tmp.path(), &state_dir, &mol_id);
    assert!(
        out.status.success(),
        "complete failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let after =
        fs::read_to_string(mol_dir.join("briefing.md")).expect("briefing.md after complete");
    assert!(
        after.contains(cosmon_core::committee::COMMITTEE_POSTURE_FILE),
        "the pointer must survive completion — a witness that is true only \
         while the seat runs is false at every moment it is read; got:\n{after}"
    );

    // And the two-fact test the persona witness actually runs must still pass
    // on the TERMINAL record, which is the record that gets audited.
    let briefing = cosmon_core::committee::AdversarialBriefing::from_durable_injection(
        cosmon_core::committee::ADVERSARIAL_BRIEFING_VERSION,
        "blake3:test",
        mol_dir
            .join(cosmon_core::committee::COMMITTEE_POSTURE_FILE)
            .exists(),
        after.contains(cosmon_core::committee::committee_posture_reference().trim()),
    );
    assert!(
        briefing.is_valid(),
        "witness (2) must hold on a COMPLETED seat, not only on a running one"
    );
}

/// **The counterweight.** A molecule that is not a committee seat must come out
/// of `cs complete` carrying exactly the terse COMPLETED briefing and nothing
/// else. Without this, the fix above is satisfiable by stamping a posture stanza
/// onto every briefing in the fleet — the bug the tackle-side counterweight
/// already forbids, arriving through the completion door instead.
#[test]
fn complete_leaves_a_non_seat_briefing_untouched() {
    let (tmp, state_dir, mol_id) = setup("");
    let mol_dir = molecule_dir(&state_dir, &mol_id);

    let out = complete(tmp.path(), &state_dir, &mol_id);
    assert!(
        out.status.success(),
        "complete failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let after = fs::read_to_string(mol_dir.join("briefing.md")).expect("briefing.md");
    assert_eq!(
        after, "# Molecule Briefing\n\n**Status:** COMPLETED\n\nCompleted via `cs complete`.\n",
        "a molecule with no durable committee-posture.md is not a seat; its \
         completed briefing must be the terse text and nothing more"
    );
}

/// And `cs complete` may not author the file either — the same property the
/// tackle-side test pins, asserted at the third call site. A verb that could
/// grow a seat its own contract would let a molecule attest its own convening.
///
/// # Why the fixture must have POINTED at the file first
///
/// This test used to be `setup("")` → `cs complete` →
/// `assert!(!posture_path.exists())`, on a molecule that never had a posture
/// file and was never dispatched as a seat. Its NAME promised
/// `…the_posture_file_it_points_at`; its body established no pointer and had no
/// file to delete, so it could not distinguish "the verb does not author the
/// file" from "nothing here was ever a seat". Measured 2026-07-29 (mutation
/// M-1): a `cs complete` that re-authored the posture file for a molecule whose
/// pre-rewrite briefing already carried the pointer — precisely the
/// self-attestation hazard the reconcile-scope doc warns about — left the whole
/// suite green, 15 passed / 0 failed.
///
/// It now takes the tackle-side shape: write the file, dispatch, assert the
/// pointer landed, remove the file, complete, assert nothing brought it back.
/// The removal proves something about which verb authors the file only because
/// the pointer was there before it.
#[test]
fn complete_does_not_author_the_posture_file_it_points_at() {
    let (tmp, state_dir, mol_id) = setup("");
    let mol_dir = molecule_dir(&state_dir, &mol_id);
    let posture_path = mol_dir.join(cosmon_core::committee::COMMITTEE_POSTURE_FILE);
    fs::write(
        &posture_path,
        cosmon_core::committee::render_committee_posture(
            cosmon_core::committee::ADVERSARIAL_BRIEFING_VERSION,
            "blake3:test",
            "Audit the artefacts. The generator's confidence is not evidence.",
        ),
    )
    .expect("write posture");

    // With the convener's file in place, dispatch delivers the pointer...
    let out = tackle_dry_run(tmp.path(), &state_dir, &mol_id, &[]);
    assert!(
        out.status.success(),
        "tackle --dry-run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fs::read_to_string(mol_dir.join("briefing.md"))
            .unwrap_or_default()
            .contains(cosmon_core::committee::COMMITTEE_POSTURE_FILE),
        "precondition: the briefing must POINT at the file, or the removal \
         below proves nothing about which verb authors it"
    );

    // ...and with the file removed under a briefing that still points at it,
    // `cs complete` must not bring it back.
    fs::remove_file(&posture_path).expect("remove posture");
    let out = complete(tmp.path(), &state_dir, &mol_id);
    assert!(
        out.status.success(),
        "complete failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !posture_path.exists(),
        "`cs complete` may not author `{}` — the convening driver does, and a \
         seat that could grow its own contract at completion would be attesting \
         its own convening",
        cosmon_core::committee::COMMITTEE_POSTURE_FILE
    );
}

// ── F6 — an illegal (adapter, model) pair is a refusal, not a 400 at launch ──

/// The measured incident, reproduced through the command: `--adapter codex`
/// with a `claude-*` model resolved, dispatched, and was rejected by the
/// provider with an HTTP 400 — after which the seat sat mute at a prompt,
/// indistinguishable from a provider refusal.
#[test]
fn tackle_refuses_an_incoherent_adapter_model_pair() {
    let (tmp, state_dir, mol_id) = setup("");
    let out = tackle_dry_run(
        tmp.path(),
        &state_dir,
        &mol_id,
        &["--adapter", "codex", "--model", "claude-opus-5"],
    );
    assert!(
        !out.status.success(),
        "an incoherent pair must fail closed at dispatch, not at the \
         provider's HTTP 400; stdout:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("incoherent"),
        "the refusal must name what it refused; got:\n{err}"
    );
    assert!(
        err.contains("openai") && err.contains("anthropic"),
        "the refusal must name BOTH resolved families so the operator knows \
         which side to move; got:\n{err}"
    );
    assert!(
        err.contains("--model"),
        "the refusal must name the knob the pin came from; got:\n{err}"
    );
}

/// The same variable that produced the incident — an inherited
/// `$ANTHROPIC_MODEL` in the dispatching shell — must be refused too. This
/// is the case a `--model`-only guard would miss, and it is the one that
/// actually happened: nobody typed the model at all.
#[test]
fn tackle_refuses_an_incoherent_pair_inherited_from_the_environment() {
    let (tmp, state_dir, mol_id) = setup("");
    let mut cmd = cosmon_bin_in(tmp.path());
    cmd.args([
        "tackle",
        &mol_id,
        "--dry-run",
        "--no-worktree",
        "--adapter",
        "codex",
        "--config",
        state_dir.to_str().expect("utf-8 state dir"),
    ])
    .env("ANTHROPIC_MODEL", "claude-opus-5");
    let out = cmd.output().expect("tackle spawned");

    assert!(
        !out.status.success(),
        "an env-inherited incoherent pair must fail closed — this is the \
         measured incident, where nobody typed a model at all"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("ANTHROPIC_MODEL"),
        "the refusal must point at the environment variable, since that is \
         the knob to turn; got:\n{err}"
    );
}

/// A coherent pair must dispatch untouched. Without this the refusal above
/// could be satisfied by a gate that refuses everything.
#[test]
fn tackle_accepts_a_coherent_adapter_model_pair() {
    let (tmp, state_dir, mol_id) = setup("");
    let out = tackle_dry_run(
        tmp.path(),
        &state_dir,
        &mol_id,
        &["--adapter", "codex", "--model", "gpt-5.6"],
    );
    assert!(
        out.status.success(),
        "a coherent (codex, gpt-*) pair must dispatch; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The gate must NOT refuse what it cannot decide. A local endpoint serves
/// any lineage, so `(local, qwen3)` is unvalidated, not illegal — and a
/// guard that refused everything unknown would break every self-hosted
/// endpoint cosmon supports.
#[test]
fn tackle_does_not_refuse_a_pair_it_cannot_decide() {
    let (tmp, state_dir, mol_id) = setup("");
    let out = tackle_dry_run(
        tmp.path(),
        &state_dir,
        &mol_id,
        &["--adapter", "local", "--model", "qwen3:8b"],
    );
    assert!(
        out.status.success(),
        "an undecidable pair must pass — NotChecked is not a refusal; \
         stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// **R3-7.** Why there is no posture delivery inside `cs nucleate`.
///
/// A `reinstate_committee_posture_reference` call sat immediately after
/// `write_briefing` in `cs nucleate`, added so a convener could satisfy witness
/// (2) before handoff. It could never fire: that function returns immediately
/// unless `committee-posture.md` already exists in the molecule directory, and
/// nucleation MINTS the id and CREATES the directory in the same breath, so
/// nothing can pre-exist it. A permanent no-op in the recipe's own sequence —
/// a control that reads as delivered work and does none.
///
/// This test asserts the sequence property that makes it a no-op, rather than
/// the removal: after `cs nucleate` and nothing else, the seat's directory
/// carries no posture file, so there was never anything for the call to act on.
/// The delivery that does work is `cs tackle`'s, proven above.
///
/// It is the one finding in this batch with no fail-before falsifier, and
/// deliberately so: the jury's own note is "no functional impact", the removal
/// changes no observable behaviour, and inventing a red would mean asserting
/// something the change does not claim.
#[test]
fn nucleation_cannot_deliver_a_posture_file_it_creates_the_directory_for() {
    let (_tmp, state_dir, mol_id) = setup("");
    let mol_dir = molecule_dir(&state_dir, &mol_id);

    assert!(
        !mol_dir
            .join(cosmon_core::committee::COMMITTEE_POSTURE_FILE)
            .exists(),
        "a freshly nucleated molecule cannot carry a posture file — the \
         directory did not exist a moment ago, so no convener could have \
         written one into it"
    );
    let briefing = fs::read_to_string(mol_dir.join("briefing.md")).expect("briefing.md");
    assert!(
        !briefing.contains(cosmon_core::committee::COMMITTEE_POSTURE_FILE),
        "…and with no posture file there is no pointer to establish; got:\n{briefing}"
    );
}

// ── F4 — the reviewed tree was a declaration and the worktree was the fact ──

/// Turn the test project into a git repository with two commits, and return
/// `(tree_of_head, tree_of_first_commit)`.
///
/// The second commit is `main`'s tip and the checkout sits on it; the first
/// commit's tree stands in for "the artefact the contract pinned, which this
/// checkout is no longer on" — the exact shape of the measured incident.
fn git_repo_with_two_trees(root: &Path) -> (String, String) {
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(["-C", root.to_str().expect("utf-8 root")])
            .args(args)
            .env("GIT_AUTHOR_NAME", "seat-test")
            .env("GIT_AUTHOR_EMAIL", "seat-test@example.invalid")
            .env("GIT_COMMITTER_NAME", "seat-test")
            .env("GIT_COMMITTER_EMAIL", "seat-test@example.invalid")
            .output()
            .expect("git spawned");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    git(&["init", "--initial-branch=main", "."]);
    fs::write(root.join("reviewed.txt"), "first\n").expect("write file");
    git(&["add", "reviewed.txt"]);
    git(&["commit", "-m", "first"]);
    let old_tree = git(&["rev-parse", "HEAD^{tree}"]);
    fs::write(root.join("reviewed.txt"), "second\n").expect("write file");
    git(&["add", "reviewed.txt"]);
    git(&["commit", "-m", "second"]);
    let head_tree = git(&["rev-parse", "HEAD^{tree}"]);
    assert_ne!(head_tree, old_tree, "the two commits must differ");
    (head_tree, old_tree)
}

/// Nucleate a molecule carrying `reviewed_tree = <pin>`.
fn setup_pinned(pin: &str) -> (tempfile::TempDir, std::path::PathBuf, String) {
    let (tmp, state_dir, _) = setup("");
    let formulas_dir = tmp.path().join("formulas");
    let out = cosmon_bin_in(tmp.path())
        .args([
            "--json",
            "nucleate",
            "seat-test",
            "--var",
            &format!("reviewed_tree={pin}"),
            "--store-dir",
            state_dir.to_str().expect("utf-8 state dir"),
            "--formulas-dir",
            formulas_dir.to_str().expect("utf-8 formulas dir"),
        ])
        .output()
        .expect("nucleate spawned");
    assert!(
        out.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("nucleate json");
    let mol_id = json["id"].as_str().expect("nucleate id").to_owned();
    (tmp, state_dir, mol_id)
}

/// **The falsifier.** A seat pinned to a tree no available base carries must
/// not launch. Before this gate the pin was prose: `cs tackle` cut the seat's
/// worktree from the HEAD of whatever worktree the operator dispatched from,
/// and the pinned tree never entered the decision. Measured — same command,
/// same pin, two dispatch cwds, two different reviewed artefacts.
#[test]
fn tackle_refuses_a_seat_it_cannot_put_on_the_reviewed_tree() {
    // A pin nothing in the repository carries at all.
    let (tmp, state_dir, mol_id) = setup_pinned("dead0beefcafe1234567890abcdef1234567890a");
    git_repo_with_two_trees(tmp.path());

    let out = tackle_dry_run(tmp.path(), &state_dir, &mol_id, &[]);
    assert!(
        !out.status.success(),
        "a pin no base carries must fail closed at dispatch — dispatching \
         anyway produces a verdict about a tree nobody asked about while the \
         seat's own files declare the pinned one; stdout:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("reviewed tree"),
        "the refusal must name what it refused; got:\n{err}"
    );
    assert!(
        err.contains("HEAD"),
        "and it must name what WAS offered, or its reader cannot tell a stale \
         checkout from a wrong pin; got:\n{err}"
    );
}

/// **The counterweight.** The same molecule pinned to the tree the checkout
/// really carries dispatches untouched. Without it the gate above is satisfied
/// by refusing every seat there is, which is an outage.
#[test]
fn tackle_accepts_a_seat_whose_pin_the_checkout_carries() {
    // A tree id is a pure function of content, so the fixture's HEAD tree is
    // known before the repository exists: build it once to learn the id, then
    // nucleate a molecule pinned to it and build the same fixture again.
    let scratch = tempfile::tempdir().expect("tempdir");
    let (head_tree, old_tree) = git_repo_with_two_trees(scratch.path());

    let (tmp, state_dir, mol_id) = setup_pinned(&head_tree[..12]);
    let (rebuilt_head, rebuilt_old) = git_repo_with_two_trees(tmp.path());
    assert_eq!(
        (rebuilt_head, rebuilt_old),
        (head_tree, old_tree),
        "the fixture must be reproducible, or this test pins nothing"
    );

    let out = tackle_dry_run(tmp.path(), &state_dir, &mol_id, &[]);
    assert!(
        out.status.success(),
        "a pin the checkout satisfies must dispatch; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A molecule with NO pin is untouched, and — the part that matters — does not
/// acquire a dependency on being inside a git repository. Every ordinary
/// dispatch runs through this code path.
#[test]
fn an_unpinned_molecule_dispatches_with_no_repository_at_all() {
    let (tmp, state_dir, mol_id) = setup("");
    let out = tackle_dry_run(tmp.path(), &state_dir, &mol_id, &[]);
    assert!(
        out.status.success(),
        "an unpinned molecule must not be made to require a repository; \
         stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A pin that is not a tree id is refused rather than ignored: ignoring it is
/// how a typo becomes an unconstrained dispatch that still looks constrained.
#[test]
fn tackle_refuses_an_unreadable_reviewed_tree_pin() {
    let (tmp, state_dir, mol_id) = setup_pinned("not-a-tree");
    git_repo_with_two_trees(tmp.path());

    let out = tackle_dry_run(tmp.path(), &state_dir, &mol_id, &[]);
    assert!(
        !out.status.success(),
        "an uninterpretable pin enforces nothing while looking as though it \
         does; stdout:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not a git tree id"),
        "the refusal must say WHY it could not read the pin; got:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
