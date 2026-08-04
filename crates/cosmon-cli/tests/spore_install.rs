// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end behaviour of `cs spore install`, the package-manager verb.
//!
//! Every test here drives the real `cs` binary against a bundle this file
//! writes, so nothing reaches the network and nothing depends on a checkout
//! being present. The git path is exercised through a `file://` remote — the
//! same three `git` commands the verb runs against GitHub, minus the internet.
//!
//! The properties asserted are the ones the verb exists for:
//!
//! 1. installing places the bundle **and** registers its recipes under the
//!    name they *declare*, which is the name `cs tackle` resolves at dispatch
//!    (the whole point — task-20260725-eb3b);
//! 2. re-installing the same bundle is a no-op, and a registry recipe that
//!    differs is a refusal, not a silent overwrite;
//! 3. a refused install writes nothing;
//! 4. what was installed can be germinated: `cs spore validate` succeeds on
//!    the destination.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// A `cs` invocation with the worker-context env scrubbed, so a run inside a
/// molecule cannot reach into the enclosing molecule's state.
fn cs() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_STATE_DIR")
        .env_remove("COSMON_FORMULAS_DIR");
    cmd
}

/// The recipe the fixture bundle ships. Its file name (`recipe.formula.toml`)
/// deliberately differs from the name it declares (`bundled-work`), because
/// the registry key is the declared name and installing under the file name
/// would leave the pins exactly as unreachable as never installing at all.
const RECIPE: &str = r#"
formula = "bundled-work"
version = 1
id_prefix = "task"

[[steps]]
id = "do"
title = "Do the work"
description = "the body"
acceptance = "any evidence"
adapter = "claude"
model = "opus"
"#;

/// A minimal, valid, unsealed bundle.
const MANIFEST: &str = r#"
[spore]
name = "fixture"
version = 1
description = "install fixture"

[spore.formulas.work]
path = "recipe.formula.toml"

[[spore.node]]
id = "frame"
kind = "fixed"
formula = "work"
"#;

/// Write the fixture bundle into `dir` and return it.
fn write_bundle(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).expect("create bundle dir");
    std::fs::write(dir.join("spore.toml"), MANIFEST).expect("write manifest");
    std::fs::write(dir.join("recipe.formula.toml"), RECIPE).expect("write recipe");
    dir.to_path_buf()
}

/// A project skeleton: `<root>/.cosmon/formulas/` is the registry the verb
/// writes into, and `<root>/spores/` is where a bundle lands by default.
fn write_project(root: &Path) -> PathBuf {
    let formulas = root.join(".cosmon").join("formulas");
    std::fs::create_dir_all(&formulas).expect("create registry");
    formulas
}

/// Run `cs spore install` in `root` with the registry pinned explicitly, so
/// the test never depends on walk-up discovery finding the *cosmon* checkout
/// the test binary happens to live in.
fn install(root: &Path, registry: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = cs();
    cmd.current_dir(root)
        .arg("spore")
        .arg("install")
        .args(args)
        .arg("--formulas-dir")
        .arg(registry);
    cmd.output().expect("run cs spore install")
}

#[test]
fn installing_places_the_bundle_and_registers_its_recipe_by_declared_name() {
    let src = TempDir::new().expect("temp src");
    let project = TempDir::new().expect("temp project");
    let bundle = write_bundle(&src.path().join("bundle"));
    let registry = write_project(project.path());

    let out = install(
        project.path(),
        &registry,
        &[&bundle.display().to_string(), "--dest", "spores/fixture"],
    );
    assert!(
        out.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let dest = project.path().join("spores/fixture");
    assert!(dest.join("spore.toml").is_file(), "manifest was placed");
    assert!(
        dest.join("recipe.formula.toml").is_file(),
        "recipe was placed"
    );
    assert!(
        dest.join(".spore-install.toml").is_file(),
        "provenance was recorded"
    );

    // The load-bearing assertion: the registry key is the DECLARED formula
    // name, not the bundle's file name. Installing under the file name would
    // put bytes in the registry and leave every per-step pin unreachable at
    // dispatch, which is the failure this verb exists to end.
    assert!(
        registry.join("bundled-work.formula.toml").is_file(),
        "recipe registered under its declared name; registry holds: {:?}",
        std::fs::read_dir(&registry)
            .expect("read registry")
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .collect::<Vec<_>>()
    );
    assert!(
        !registry.join("recipe.formula.toml").exists(),
        "not registered under the bundle's file name"
    );
}

#[test]
fn what_was_installed_validates() {
    let src = TempDir::new().expect("temp src");
    let project = TempDir::new().expect("temp project");
    let bundle = write_bundle(&src.path().join("bundle"));
    let registry = write_project(project.path());

    let out = install(project.path(), &registry, &[&bundle.display().to_string()]);
    assert!(out.status.success(), "install failed");

    // No --dest: the bundle lands under <project>/spores/<spore-name>/.
    let dest = project.path().join("spores").join("fixture");
    assert!(
        dest.join("spore.toml").is_file(),
        "default destination used"
    );

    let validated = cs()
        .current_dir(project.path())
        .args(["spore", "validate"])
        .arg(&dest)
        .output()
        .expect("run cs spore validate");
    assert!(
        validated.status.success(),
        "the installed bundle does not validate: {}",
        String::from_utf8_lossy(&validated.stderr)
    );
}

#[test]
fn reinstalling_the_same_bundle_is_idempotent_and_a_different_recipe_refuses() {
    let src = TempDir::new().expect("temp src");
    let project = TempDir::new().expect("temp project");
    let bundle = write_bundle(&src.path().join("bundle"));
    let registry = write_project(project.path());
    let dest = "spores/fixture";

    let first = install(
        project.path(),
        &registry,
        &[&bundle.display().to_string(), "--dest", dest],
    );
    assert!(first.status.success(), "first install failed");

    // Same bytes: the destination is occupied, so --force is the honest way to
    // re-install, and the registry write is a no-op rather than a conflict.
    let again = install(
        project.path(),
        &registry,
        &[&bundle.display().to_string(), "--dest", dest, "--force"],
    );
    assert!(again.status.success(), "re-install failed");
    let stdout = String::from_utf8_lossy(&again.stdout);
    assert!(
        stdout.contains("already identical"),
        "re-installing an unchanged recipe is a no-op, got: {stdout}"
    );

    // A registry recipe that differs must NOT be silently overwritten:
    // dispatch resolves already-germinated molecules through it.
    std::fs::write(
        registry.join("bundled-work.formula.toml"),
        RECIPE.replace("opus", "haiku"),
    )
    .expect("diverge the registry copy");
    let conflict = install(
        project.path(),
        &registry,
        &[&bundle.display().to_string(), "--dest", dest, "--force"],
    );
    // --force covers the occupied destination but the registry conflict is a
    // separate consent; this call passes --force so it proceeds, and the point
    // of the assertion below is the *without* --force case.
    assert!(conflict.status.success(), "--force replaces the recipe");

    std::fs::write(
        registry.join("bundled-work.formula.toml"),
        RECIPE.replace("opus", "haiku"),
    )
    .expect("diverge again");
    let refused = install(
        project.path(),
        &registry,
        &[
            &bundle.display().to_string(),
            "--dest",
            "spores/other",
            // no --force
        ],
    );
    assert!(
        !refused.status.success(),
        "a divergent registry recipe must refuse without --force"
    );
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("DIFFERENT recipe"),
        "the refusal names the cause, got: {stderr}"
    );
    assert!(
        !project.path().join("spores/other").exists(),
        "a refused install writes nothing"
    );
}

#[test]
fn a_hash_mismatch_refuses_before_anything_is_written() {
    let src = TempDir::new().expect("temp src");
    let project = TempDir::new().expect("temp project");
    let bundle = write_bundle(&src.path().join("bundle"));
    let registry = write_project(project.path());

    let out = install(
        project.path(),
        &registry,
        &[
            &bundle.display().to_string(),
            "--dest",
            "spores/fixture",
            "--expect-hash",
            "blake3:0000000000000000000000000000000000000000000000000000000000000000",
        ],
    );
    assert!(!out.status.success(), "a hash mismatch must refuse");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("bundle hash mismatch"),
        "the refusal names the cause"
    );
    assert!(
        !project.path().join("spores/fixture").exists(),
        "nothing was placed"
    );
    assert!(
        !registry.join("bundled-work.formula.toml").exists(),
        "nothing was registered"
    );
}

#[test]
fn a_dry_run_reports_the_plan_and_writes_nothing() {
    let src = TempDir::new().expect("temp src");
    let project = TempDir::new().expect("temp project");
    let bundle = write_bundle(&src.path().join("bundle"));
    let registry = write_project(project.path());

    let out = install(
        project.path(),
        &registry,
        &[
            &bundle.display().to_string(),
            "--dest",
            "spores/fixture",
            "--dry-run",
        ],
    );
    assert!(out.status.success(), "dry run failed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("would install"), "got: {stdout}");
    assert!(
        !project.path().join("spores/fixture").exists(),
        "a dry run places nothing"
    );
    assert!(
        !registry.join("bundled-work.formula.toml").exists(),
        "a dry run registers nothing"
    );
}

#[test]
fn no_formulas_places_the_bundle_and_leaves_the_registry_alone() {
    let src = TempDir::new().expect("temp src");
    let project = TempDir::new().expect("temp project");
    let bundle = write_bundle(&src.path().join("bundle"));
    let registry = write_project(project.path());

    let out = install(
        project.path(),
        &registry,
        &[
            &bundle.display().to_string(),
            "--dest",
            "spores/fixture",
            "--no-formulas",
        ],
    );
    assert!(out.status.success(), "install failed");
    assert!(project.path().join("spores/fixture/spore.toml").is_file());
    assert!(
        !registry.join("bundled-work.formula.toml").exists(),
        "--no-formulas leaves the registry untouched"
    );
}

#[test]
fn a_git_remote_is_fetched_and_installed() {
    let Some(git) = git_available() else {
        eprintln!("SKIP: `git` is not on PATH");
        return;
    };
    let src = TempDir::new().expect("temp src");
    let project = TempDir::new().expect("temp project");
    let registry = write_project(project.path());

    // A real repository, served over `file://` — the same fetch path as a
    // GitHub remote, with no network.
    let repo = src.path().join("repo");
    write_bundle(&repo.join("spores").join("fixture"));
    for args in [
        vec!["init", "--quiet", "-b", "main"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["config", "user.name", "fixture"],
        vec!["add", "-A"],
        vec!["commit", "--quiet", "-m", "fixture bundle"],
    ] {
        let done = Command::new(&git)
            .arg("-C")
            .arg(&repo)
            .args(&args)
            .output()
            .expect("run git");
        assert!(
            done.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&done.stderr)
        );
    }

    let remote = format!("file://{}", repo.display());
    let out = install(
        project.path(),
        &registry,
        &[
            &remote,
            "--subdir",
            "spores/fixture",
            "--dest",
            "spores/fixture",
        ],
    );
    assert!(
        out.status.success(),
        "git install failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(project.path().join("spores/fixture/spore.toml").is_file());
    assert!(registry.join("bundled-work.formula.toml").is_file());
    assert!(
        !project.path().join("spores/fixture/.git").exists(),
        "git bookkeeping is not carried into the project"
    );

    // The provenance file pins the commit, because a branch name does not
    // answer "a copy of what?" a week later.
    let provenance =
        std::fs::read_to_string(project.path().join("spores/fixture/.spore-install.toml"))
            .expect("read provenance");
    assert!(provenance.contains("commit = "), "got: {provenance}");
    assert!(provenance.contains("bundle_hash = "), "got: {provenance}");
}

/// `git`, if this machine has one.
fn git_available() -> Option<String> {
    let probe = Command::new("git").arg("--version").output().ok()?;
    probe.status.success().then(|| "git".to_string())
}
