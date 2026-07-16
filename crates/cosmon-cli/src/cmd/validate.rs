// SPDX-License-Identifier: AGPL-3.0-only

//! `cs validate` — the deliberate, heavyweight project-milestone gate (tier 2).
//!
//! This is intentionally separate from `cs done`: ordinary merges only need the
//! bounded combined-compile gate (`cs done`'s post-merge cascade), while
//! validation runs the slow assurance suite when an operator chooses to validate
//! a project stage.
//!
//! Like the `cs done` gate (ADR-158), this tier used to hardcode cosmon's
//! Rust/Cargo layout into the transport layer — a Principle-1 (Transport ≠
//! Cognition) leak: the *content* of "is this project sound?" was baked into the
//! binary. It now **delegates the WHAT** to the per-galaxy `[gates]` commands
//! (`test_command`, `lint_command`, `format_command`, `typecheck_command`,
//! `setup_command`), falling back to the cargo invocations only when a slot is
//! unset. cosmon's own config declares those slots as cargo commands, so
//! cosmon-on-cosmon behaviour is unchanged; a Python or Go galaxy that declares
//! its own commands is now validated with *its* toolchain, not cargo's.

use std::path::{Path, PathBuf};
use std::process::Command;

use cosmon_core::config::GatesConfig;

use super::Context;

/// Arguments for `cs validate`.
#[derive(clap::Args)]
pub struct Args {}

/// One tier-2 validation stage: a human-readable name, the shell command to run,
/// and whether that command was *declared* by the repo's config (so it must pass
/// the B5 trust gate before it may exec) or is one of cosmon's own hardcoded
/// cargo fallbacks (which are not repo-supplied and need no trust).
struct Stage {
    name: String,
    command: String,
    repo_supplied: bool,
}

impl Stage {
    fn declared(name: &str, command: String) -> Self {
        Self {
            name: name.to_owned(),
            command,
            repo_supplied: true,
        }
    }

    fn fallback(name: &str, command: &str) -> Self {
        Self {
            name: name.to_owned(),
            command: command.to_owned(),
            repo_supplied: false,
        }
    }
}

/// Run the heavyweight validation suite from the repository root.
pub fn run(ctx: &Context, _args: &Args) -> anyhow::Result<()> {
    let repo_root = repository_root()?;
    let config_path = super::resolve_config_from_context(ctx);
    let gates = cosmon_filestore::load_project_config(&config_path)
        .map(|c| c.gates)
        .unwrap_or_default();

    let stages = plan_stages(&gates, &repo_root);

    // B5 (RCE-by-clone): any stage whose command came from the repo's own
    // `.cosmon/config.toml` is repo-supplied shell. Refuse to exec it in an
    // untrusted clone — `cs trust` is the gate. cosmon's hardcoded cargo
    // fallbacks are not repo-supplied and run regardless. The check is hoisted so
    // a long suite never runs a dozen safe stages only to abort on a declared
    // one halfway through.
    if stages.iter().any(|s| s.repo_supplied) {
        cosmon_cli::trust::ensure_trusted(&repo_root)?;
    }

    for stage in &stages {
        println!("==> {}", stage.name);
        // Defect 1 fold-in (codex-sol, task-20260715-ff5b): tier-2 stages run
        // repo-supplied shell against the working tree — the SAME
        // trust-hashes-the-config-not-the-script hole `cs done` had. Route every
        // stage through the identical egress/sandbox discipline (`exec_command`
        // mirror). On a host that cannot kernel-enforce a required `deny-external`
        // policy for an exposed multi-tenant dispatch, refuse fail-closed rather
        // than run unconfined. With `COSMON_EGRESS_POLICY` unset (the trusted
        // single-operator default) the command is byte-identical to the pre-fix
        // `sh -c <command>`, so cosmon-on-cosmon `cs validate` is unchanged.
        let (program, args) = match super::egress_delegate::jail_delegated_sh(&stage.command) {
            super::egress_delegate::JailDecision::Ready {
                program,
                args,
                advisory_reason,
                ..
            } => {
                if let Some(reason) = advisory_reason {
                    eprintln!(
                        "⚠ egress advisory (validate stage `{}`): {reason}",
                        stage.name
                    );
                }
                (program, args)
            }
            super::egress_delegate::JailDecision::Refused { message } => {
                anyhow::bail!(
                    "validation stage `{}` (`{}`) refused (egress fail-closed) — {}",
                    stage.name,
                    stage.command,
                    message
                );
            }
        };
        let status = Command::new(&program)
            .args(&args)
            .current_dir(&repo_root)
            .status()?;
        if !status.success() {
            anyhow::bail!(
                "validation stage `{}` (`{}`) exited {}",
                stage.name,
                stage.command,
                status.code().unwrap_or(-1)
            );
        }
    }

    println!("validation complete");
    Ok(())
}

/// Build the ordered stage list, delegating each dimension to its `[gates]`
/// command when declared and falling back to the cargo default otherwise
/// (ADR-158, tier-2 symmetry). The mutation falsifier is a cosmon-specific extra
/// gated on the wrapper script's presence, so a non-cosmon galaxy skips it
/// rather than failing on a missing script.
fn plan_stages(gates: &GatesConfig, repo_root: &Path) -> Vec<Stage> {
    let mut stages = Vec::new();

    if let Some(cmd) = gates.setup_command.as_deref() {
        stages.push(Stage::declared("setup", cmd.to_owned()));
    }
    if let Some(cmd) = gates.typecheck_command.as_deref() {
        stages.push(Stage::declared("typecheck", cmd.to_owned()));
    }
    // A declared test command owns testing entirely — cosmon does not bolt a
    // cargo doctest stage onto a Python/Go suite. cosmon's own
    // `cargo test --workspace` already runs doctests, so no coverage is lost.
    if let Some(cmd) = gates.test_command.as_deref() {
        stages.push(Stage::declared("tests", cmd.to_owned()));
    } else {
        stages.push(Stage::fallback("doctests", "cargo test --workspace --doc"));
        stages.push(Stage::fallback("workspace tests", "cargo test --workspace"));
    }
    match gates.lint_command.as_deref() {
        Some(cmd) => stages.push(Stage::declared("lint", cmd.to_owned())),
        None => stages.push(Stage::fallback(
            "strict clippy",
            "cargo clippy --workspace -- -D warnings",
        )),
    }
    match gates.format_command.as_deref() {
        Some(cmd) => stages.push(Stage::declared("format", cmd.to_owned())),
        None => stages.push(Stage::fallback("format", "cargo fmt --all -- --check")),
    }
    // cosmon-specific heavyweight extra: run the mutation falsifier only when its
    // wrapper script is present (cosmon has it; other galaxies do not).
    if repo_root.join("scripts/mutation-falsifier.sh").is_file() {
        stages.push(Stage::fallback(
            "mutation falsifier",
            "./scripts/mutation-falsifier.sh",
        ));
    }

    stages
}

fn repository_root() -> anyhow::Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "could not find repository root: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let root = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    if !Path::new(&root).is_dir() {
        anyhow::bail!(
            "git returned a non-directory repository root: {}",
            root.display()
        );
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_root_is_a_directory() {
        assert!(repository_root().unwrap().is_dir());
    }

    /// With no `[gates]` declared, every stage falls back to a cargo default and
    /// none is repo-supplied — so `cs validate` needs no trust grant, exactly as
    /// before the delegation. The mutation falsifier is absent (no script in a
    /// bare temp root).
    #[test]
    fn empty_gates_fall_back_to_cargo_and_need_no_trust() {
        let tmp = tempfile::tempdir().unwrap();
        let stages = plan_stages(&GatesConfig::default(), tmp.path());
        assert!(
            stages.iter().all(|s| !s.repo_supplied),
            "cargo fallbacks are cosmon's own strings, not repo-supplied"
        );
        // doctests, workspace tests, strict clippy, format.
        assert_eq!(stages.len(), 4);
        assert!(stages.iter().any(|s| s.command == "cargo test --workspace"));
        assert!(stages
            .iter()
            .any(|s| s.command == "cargo fmt --all -- --check"));
    }

    /// A polyglot galaxy that declares its own commands is validated with *its*
    /// toolchain — the Principle-1 leak removed. Declared stages are repo-supplied
    /// (trust-gated), and no cargo string leaks in.
    #[test]
    fn declared_gates_delegate_and_are_trust_gated() {
        let tmp = tempfile::tempdir().unwrap();
        let gates = GatesConfig {
            test_command: Some("pytest -q".to_owned()),
            lint_command: Some("ruff check .".to_owned()),
            format_command: Some("ruff format --check .".to_owned()),
            ..Default::default()
        };
        let stages = plan_stages(&gates, tmp.path());
        assert!(
            stages.iter().all(|s| s.repo_supplied),
            "every declared stage is repo-supplied and must be trust-gated"
        );
        assert!(
            stages.iter().all(|s| !s.command.contains("cargo")),
            "no cargo default may leak into a fully-declared polyglot suite"
        );
        assert!(stages.iter().any(|s| s.command == "pytest -q"));
        // A declared test_command owns testing — no separate doctest stage.
        assert!(!stages.iter().any(|s| s.name == "doctests"));
    }
}
