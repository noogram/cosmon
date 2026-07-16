// SPDX-License-Identifier: AGPL-3.0-only

//! `cs security` — operator-only binary security posture toggle.
//!
//! Implements the **prepared ↔ active** posture model. Three couches
//! indépendantes — réseau (`Tailscale`), identité (`YubiKey` + `WebAuthn`),
//! supply-chain (cargo-deny / vet / audit) — with a single binary flag
//! that flips the supply-chain layer between *préparé* (warn) and
//! *active* (deny). No partial state. Either the bretelle béton is
//! posée, ou elle ne l'est pas.
//!
//! # Why binary, not graduated
//!
//! "Partiellement active" is the state where a bretelle carton sits
//! next to a bretelle béton and nobody knows which one is load-bearing.
//! The posture toggle is the structural refusal to land in that state:
//! every CI gate flips together, in one commit, atomically.
//!
//! # Operator-only
//!
//! Refuses to run inside a worker context (`COSMON_MOL_DIR` set). The
//! activation gesture is a kill-switch peer — non-overridable, manual,
//! first-class. A worker that mutates the project's posture by mistake
//! would silently weaken the whole system; the simplest enforcement is
//! to make the command refuse the worker process altogether.

use std::path::{Path, PathBuf};

use chrono::Utc;

use super::Context;

/// Top-level args for `cs security`.
#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub command: SecurityCommand,
}

/// `cs security` subcommands.
#[derive(clap::Subcommand)]
pub enum SecurityCommand {
    /// Flip the supply-chain posture (prepared → active). Use `--rollback`
    /// to reverse (active → prepared).
    Activate(ActivateArgs),
    /// Show the current posture and any drift between security.toml and
    /// the on-disk gates (deny.toml + CI workflow).
    Status(StatusArgs),
}

/// Arguments for `cs security activate`.
#[derive(clap::Args)]
#[allow(clippy::struct_excessive_bools)]
pub struct ActivateArgs {
    /// Reverse the flip — go from `active` back to `prepared` (deny → warn).
    /// Same operator-only guard, same atomic single-commit transition.
    #[arg(long)]
    pub rollback: bool,

    /// Repo root containing `deny.toml` and `.github/workflows/deny.yml`.
    /// Defaults to walking up from CWD until a `deny.toml` is found.
    #[arg(long, value_name = "PATH")]
    pub root: Option<PathBuf>,

    /// Skip the auto-generated git commit. The file edits land on disk,
    /// but the operator commits manually. Useful for review-before-push
    /// flows.
    #[arg(long)]
    pub no_commit: bool,

    /// Skip the auto-push after commit. The commit lands locally, the
    /// operator pushes manually. Implied by `--no-commit`.
    #[arg(long)]
    pub no_push: bool,

    /// Show what would change without writing anything.
    #[arg(long)]
    pub dry_run: bool,
}

/// Arguments for `cs security status`.
#[derive(clap::Args)]
pub struct StatusArgs {
    /// Repo root containing `deny.toml` and `.github/workflows/deny.yml`.
    /// Defaults to walking up from CWD.
    #[arg(long, value_name = "PATH")]
    pub root: Option<PathBuf>,
}

/// The two postures. Binary by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Posture {
    /// Supply-chain gates wired but `severity = "warn"`. `WebAuthn` câblé,
    /// non-required. Default shipping state.
    Prepared,
    /// Supply-chain gates `severity = "deny"`. `WebAuthn` required.
    /// Reached only via `cs security activate`, never silently.
    Active,
}

impl Posture {
    fn as_str(self) -> &'static str {
        match self {
            Posture::Prepared => "prepared",
            Posture::Active => "active",
        }
    }

    fn parse(s: &str) -> anyhow::Result<Self> {
        match s {
            "prepared" => Ok(Posture::Prepared),
            "active" => Ok(Posture::Active),
            other => {
                anyhow::bail!("unknown posture mode `{other}`; expected `prepared` or `active`")
            }
        }
    }
}

/// Dispatch `cs security <sub>`.
///
/// # Errors
/// Propagates errors from subcommand handlers.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.command {
        SecurityCommand::Activate(a) => run_activate(ctx, a),
        SecurityCommand::Status(a) => run_status(ctx, a),
    }
}

fn run_activate(ctx: &Context, args: &ActivateArgs) -> anyhow::Result<()> {
    refuse_worker_context()?;

    let root = resolve_root(args.root.as_deref())?;
    let deny_path = root.join("deny.toml");
    let workflow_path = root.join(".github/workflows/deny.yml");

    let detected = detect_posture(&deny_path, &workflow_path)?;

    let (from, to) = if args.rollback {
        (Posture::Active, Posture::Prepared)
    } else {
        (Posture::Prepared, Posture::Active)
    };

    if detected != from {
        anyhow::bail!(
            "current posture is `{}`, refusing to {} (would no-op or corrupt state). \
             Run `cs security status` to inspect, or pass `--rollback` to reverse.",
            detected.as_str(),
            if args.rollback {
                "rollback"
            } else {
                "activate"
            }
        );
    }

    let deny_before = std::fs::read_to_string(&deny_path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", deny_path.display()))?;
    let workflow_before = std::fs::read_to_string(&workflow_path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", workflow_path.display()))?;

    let deny_after = transition_deny_toml(&deny_before, to)?;
    let workflow_after = transition_workflow(&workflow_before, to)?;

    if args.dry_run {
        report_dry_run(
            ctx,
            from,
            to,
            &deny_path,
            &deny_before,
            &deny_after,
            &workflow_path,
            &workflow_before,
            &workflow_after,
        );
        return Ok(());
    }

    std::fs::write(&deny_path, &deny_after)
        .map_err(|e| anyhow::anyhow!("write {}: {e}", deny_path.display()))?;
    std::fs::write(&workflow_path, &workflow_after)
        .map_err(|e| anyhow::anyhow!("write {}: {e}", workflow_path.display()))?;

    write_security_toml(to)?;

    let commit_sha = if args.no_commit {
        None
    } else {
        Some(commit_transition(&root, from, to)?)
    };

    if !args.no_commit && !args.no_push {
        let _ = push_to_remote(&root);
    }

    if ctx.json {
        let out = serde_json::json!({
            "from": from.as_str(),
            "to": to.as_str(),
            "deny_toml": deny_path.display().to_string(),
            "workflow": workflow_path.display().to_string(),
            "security_toml": security_toml_path().display().to_string(),
            "commit": commit_sha,
            "rollback": args.rollback,
        });
        println!("{out}");
    } else {
        println!("security posture: {} → {}", from.as_str(), to.as_str());
        println!("  deny.toml      : {}", deny_path.display());
        println!("  workflow       : {}", workflow_path.display());
        println!("  security.toml  : {}", security_toml_path().display());
        if let Some(sha) = &commit_sha {
            println!("  commit         : {sha}");
        } else {
            println!("  commit         : (skipped, --no-commit)");
        }
        if to == Posture::Active {
            println!();
            println!("supply-chain gates strict. webauthn.required = true.");
            println!("rollback in 30s: cs security activate --rollback");
        } else {
            println!();
            println!("supply-chain gates in warn-mode. webauthn.required = false.");
            println!("re-activate: cs security activate");
        }
    }
    Ok(())
}

fn run_status(ctx: &Context, args: &StatusArgs) -> anyhow::Result<()> {
    let root = resolve_root(args.root.as_deref())?;
    let deny_path = root.join("deny.toml");
    let workflow_path = root.join(".github/workflows/deny.yml");

    let on_disk = detect_posture(&deny_path, &workflow_path)?;
    let recorded = load_security_toml().ok().flatten();
    let drift = recorded.is_some_and(|r| r != on_disk);

    if ctx.json {
        let out = serde_json::json!({
            "on_disk": on_disk.as_str(),
            "recorded": recorded.map(Posture::as_str),
            "drift": drift,
            "deny_toml": deny_path.display().to_string(),
            "workflow": workflow_path.display().to_string(),
            "security_toml": security_toml_path().display().to_string(),
        });
        println!("{out}");
    } else {
        println!("posture (on-disk gates)  : {}", on_disk.as_str());
        match recorded {
            Some(r) => println!("posture (security.toml)  : {}", r.as_str()),
            None => println!("posture (security.toml)  : (not initialized)"),
        }
        if drift {
            println!();
            println!(
                "DRIFT: security.toml does not match on-disk gates. \
                 Re-run `cs security activate` (or `--rollback`) from a clean state."
            );
        }
        println!();
        println!("  deny.toml     : {}", deny_path.display());
        println!("  workflow      : {}", workflow_path.display());
        println!("  security.toml : {}", security_toml_path().display());
    }
    Ok(())
}

/// Refuse to run when `COSMON_MOL_DIR` is set — the security toggle is a
/// kill-switch peer, operator-only, manual, non-overridable. A worker
/// that mutates posture by mistake silently weakens the whole system.
fn refuse_worker_context() -> anyhow::Result<()> {
    if std::env::var("COSMON_MOL_DIR").is_ok() {
        anyhow::bail!(
            "cs security: refused — running inside a worker context (COSMON_MOL_DIR set). \
             Posture toggles are operator-only by design. \
             Run from your own shell, not from a worker."
        );
    }
    Ok(())
}

/// Walk up from CWD until we find a directory with `deny.toml`.
/// Override with explicit `--root` when scripted.
fn resolve_root(explicit: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(p) = explicit {
        if !p.join("deny.toml").exists() {
            anyhow::bail!("explicit root {} does not contain deny.toml", p.display());
        }
        return Ok(p.to_path_buf());
    }
    let cwd = std::env::current_dir().map_err(|e| anyhow::anyhow!("cannot read CWD: {e}"))?;
    let mut cur: &Path = &cwd;
    loop {
        if cur.join("deny.toml").exists() {
            return Ok(cur.to_path_buf());
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => anyhow::bail!(
                "no deny.toml found walking up from {}; pass --root <path>",
                cwd.display()
            ),
        }
    }
}

/// Detect posture from on-disk gates.
///
/// `prepared` ↔ `yanked = "warn"` AND `cargo vet ... || true` step.
/// `active`   ↔ `yanked = "deny"` AND no `|| true` on vet step.
///
/// Mismatched files (one strict, one warn) raise an error — the binary
/// invariant has been broken outside the toggle.
fn detect_posture(deny_path: &Path, workflow_path: &Path) -> anyhow::Result<Posture> {
    let deny = std::fs::read_to_string(deny_path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", deny_path.display()))?;
    let workflow = std::fs::read_to_string(workflow_path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", workflow_path.display()))?;

    let deny_active = deny_yanked_value(&deny)? == "deny";
    let workflow_active = !workflow_vet_has_passthrough(&workflow);

    match (deny_active, workflow_active) {
        (true, true) => Ok(Posture::Active),
        (false, false) => Ok(Posture::Prepared),
        _ => anyhow::bail!(
            "posture inconsistent: deny.toml is {} but CI vet step is {}. \
             Restore both manually before toggling.",
            if deny_active { "active" } else { "prepared" },
            if workflow_active {
                "active"
            } else {
                "prepared"
            }
        ),
    }
}

/// Extract the `yanked = "..."` value from `[advisories]`.
fn deny_yanked_value(deny: &str) -> anyhow::Result<String> {
    let value: toml::Value =
        toml::from_str(deny).map_err(|e| anyhow::anyhow!("parse deny.toml: {e}"))?;
    let advisories = value
        .get("advisories")
        .and_then(|v| v.as_table())
        .ok_or_else(|| anyhow::anyhow!("deny.toml: missing [advisories] table"))?;
    let yanked = advisories
        .get("yanked")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("deny.toml: [advisories].yanked missing or not a string"))?;
    Ok(yanked.to_owned())
}

/// True iff the cargo-vet step in the workflow ends with `|| true`.
fn workflow_vet_has_passthrough(workflow: &str) -> bool {
    workflow.lines().any(|l| {
        let t = l.trim();
        (t.starts_with("run: cargo vet") || t.starts_with("run:  cargo vet"))
            && t.ends_with("|| true")
    })
}

/// Apply the deny.toml transition to the target posture. Substring edits
/// chosen over `toml::to_string` to preserve comments and ordering — the
/// deny.toml carries operator-readable rationale that must survive flips.
fn transition_deny_toml(input: &str, to: Posture) -> anyhow::Result<String> {
    match to {
        Posture::Active => {
            // yanked = "warn" → "deny"
            let after = if input.contains("yanked = \"warn\"") {
                input.replacen("yanked = \"warn\"", "yanked = \"deny\"", 1)
            } else if input.contains("yanked = \"deny\"") {
                input.to_owned()
            } else {
                anyhow::bail!("deny.toml: cannot find `yanked = \"warn\"` line to flip");
            };
            // ensure a `vulnerability = "deny"` line exists in [advisories].
            // Insert just after the `yanked = "deny"` line.
            if after.contains("vulnerability = \"deny\"") {
                Ok(after)
            } else {
                Ok(insert_after_first(
                    &after,
                    "yanked = \"deny\"",
                    "\nvulnerability = \"deny\"",
                ))
            }
        }
        Posture::Prepared => {
            // yanked = "deny" → "warn"
            let after = if input.contains("yanked = \"deny\"") {
                input.replacen("yanked = \"deny\"", "yanked = \"warn\"", 1)
            } else if input.contains("yanked = \"warn\"") {
                input.to_owned()
            } else {
                anyhow::bail!("deny.toml: cannot find `yanked` line to flip");
            };
            // strip vulnerability = "deny" if present (with surrounding newline)
            let stripped = strip_vulnerability_deny(&after);
            Ok(stripped)
        }
    }
}

fn insert_after_first(s: &str, needle: &str, insert: &str) -> String {
    if let Some(idx) = s.find(needle) {
        let end = idx + needle.len();
        let mut out = String::with_capacity(s.len() + insert.len());
        out.push_str(&s[..end]);
        out.push_str(insert);
        out.push_str(&s[end..]);
        out
    } else {
        s.to_owned()
    }
}

fn strip_vulnerability_deny(s: &str) -> String {
    s.lines()
        .filter(|l| l.trim() != "vulnerability = \"deny\"")
        .collect::<Vec<_>>()
        .join("\n")
        + if s.ends_with('\n') { "\n" } else { "" }
}

/// Apply the workflow transition. The cargo-vet step gains `|| true` to
/// step down to `prepared`, loses it to step up to `active`. All other
/// vet flags (`--locked`) survive untouched.
fn transition_workflow(input: &str, to: Posture) -> anyhow::Result<String> {
    let mut out = String::with_capacity(input.len() + 16);
    let mut found = false;
    for (i, line) in input.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("run: cargo vet") || trimmed.starts_with("run:  cargo vet") {
            found = true;
            let new_line = match to {
                Posture::Prepared => {
                    if line.trim_end().ends_with("|| true") {
                        line.to_owned()
                    } else {
                        format!("{} || true", line.trim_end())
                    }
                }
                Posture::Active => {
                    if line.trim_end().ends_with("|| true") {
                        line.trim_end()
                            .trim_end_matches("|| true")
                            .trim_end()
                            .to_owned()
                    } else {
                        line.to_owned()
                    }
                }
            };
            out.push_str(&new_line);
        } else {
            out.push_str(line);
        }
        if i + 1 < input.lines().count() || input.ends_with('\n') {
            out.push('\n');
        }
    }
    if !found {
        anyhow::bail!("workflow: no `run: cargo vet ...` step found; cannot flip posture");
    }
    Ok(out)
}

/// Resolve `~/.config/cosmon/security.toml`.
///
/// Matches the existing `~/.config/cosmon/` convention for
/// `daemons.toml`, `patrols.toml`, and `operator.key`. Honours
/// `COSMON_CONFIG_HOME` for test isolation, falling back to
/// `$HOME/.config` (NOT `dirs::config_dir()`, which would land in
/// `~/Library/Application Support/` on macOS and split the operator's
/// config across two trees).
fn security_toml_path() -> PathBuf {
    if let Ok(p) = std::env::var("COSMON_CONFIG_HOME") {
        return PathBuf::from(p).join("cosmon").join("security.toml");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    PathBuf::from(home)
        .join(".config")
        .join("cosmon")
        .join("security.toml")
}

fn write_security_toml(posture: Posture) -> anyhow::Result<()> {
    let path = security_toml_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            anyhow::anyhow!("create security.toml parent {}: {e}", parent.display())
        })?;
    }
    let webauthn_required = matches!(posture, Posture::Active);
    let now = Utc::now().to_rfc3339();
    let body = format!(
        "# ~/.config/cosmon/security.toml — operator posture record.
# Written by `cs security activate`. Do not edit by hand: the file is a
# cache of the current on-disk gate state, not a source of truth.
# Source of truth = deny.toml + .github/workflows/deny.yml.

[posture]
# Binary toggle: prepared | active. See ADR-076.
mode = \"{mode}\"
updated_at = \"{now}\"

[webauthn]
# Mirrors the supply-chain posture. WebAuthn enforcement landing in
# tenant-demo / cosmon-cockpit-http reads this flag.
required = {webauthn}
",
        mode = posture.as_str(),
        webauthn = webauthn_required,
    );
    std::fs::write(&path, body).map_err(|e| anyhow::anyhow!("write {}: {e}", path.display()))?;
    Ok(())
}

fn load_security_toml() -> anyhow::Result<Option<Posture>> {
    let path = security_toml_path();
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
    let value: toml::Value =
        toml::from_str(&raw).map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))?;
    let mode = value
        .get("posture")
        .and_then(|v| v.get("mode"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("{}: missing [posture].mode", path.display()))?;
    Posture::parse(mode).map(Some)
}

/// Stage `deny.toml` + workflow + security.toml-shaped commit. Returns the
/// new HEAD sha. The commit message is auto-generated from the transition
/// direction so the audit trail in `git log` is stable.
fn commit_transition(root: &Path, from: Posture, to: Posture) -> anyhow::Result<String> {
    let stage = std::process::Command::new("git")
        .args(["add", "deny.toml", ".github/workflows/deny.yml"])
        .current_dir(root)
        .status()
        .map_err(|e| anyhow::anyhow!("git add: {e}"))?;
    if !stage.success() {
        anyhow::bail!("git add failed");
    }
    let msg = format!(
        "security: posture {} → {}\n\n\
         Auto-generated by `cs security activate{}`. \
         Flips deny.toml + CI cargo-vet step to keep the supply-chain layer \
         binary (delib-20260425-39c1, ADR-076).\n",
        from.as_str(),
        to.as_str(),
        if matches!(to, Posture::Prepared) {
            " --rollback"
        } else {
            ""
        },
    );
    let commit = std::process::Command::new("git")
        .args(["commit", "-m", &msg])
        .current_dir(root)
        .status()
        .map_err(|e| anyhow::anyhow!("git commit: {e}"))?;
    if !commit.success() {
        anyhow::bail!("git commit failed");
    }
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .map_err(|e| anyhow::anyhow!("git rev-parse HEAD: {e}"))?;
    if !sha.status.success() {
        anyhow::bail!("git rev-parse HEAD failed");
    }
    Ok(String::from_utf8_lossy(&sha.stdout).trim().to_owned())
}

fn push_to_remote(root: &Path) -> anyhow::Result<()> {
    let st = std::process::Command::new("git")
        .args(["push"])
        .current_dir(root)
        .status()
        .map_err(|e| anyhow::anyhow!("git push: {e}"))?;
    if !st.success() {
        anyhow::bail!("git push failed");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn report_dry_run(
    ctx: &Context,
    from: Posture,
    to: Posture,
    deny_path: &Path,
    deny_before: &str,
    deny_after: &str,
    workflow_path: &Path,
    workflow_before: &str,
    workflow_after: &str,
) {
    if ctx.json {
        let out = serde_json::json!({
            "dry_run": true,
            "from": from.as_str(),
            "to": to.as_str(),
            "deny_toml_changed": deny_before != deny_after,
            "workflow_changed": workflow_before != workflow_after,
        });
        println!("{out}");
    } else {
        println!(
            "dry-run: would flip posture {} → {}",
            from.as_str(),
            to.as_str()
        );
        let deny_changed = if deny_before == deny_after {
            "no change"
        } else {
            "would change"
        };
        let workflow_changed = if workflow_before == workflow_after {
            "no change"
        } else {
            "would change"
        };
        println!("  {} : {deny_changed}", deny_path.display());
        println!("  {} : {workflow_changed}", workflow_path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DENY_PREPARED: &str = r#"[advisories]
version = 2
yanked = "warn"
"#;

    const DENY_ACTIVE: &str = r#"[advisories]
version = 2
yanked = "deny"
vulnerability = "deny"
"#;

    const WORKFLOW_PREPARED: &str = r"jobs:
  cargo-vet:
    steps:
      - name: Run cargo vet
        run: cargo vet --locked || true
";

    const WORKFLOW_ACTIVE: &str = r"jobs:
  cargo-vet:
    steps:
      - name: Run cargo vet
        run: cargo vet --locked
";

    #[test]
    fn detect_prepared_state() {
        let dir = tempfile::tempdir().unwrap();
        let deny = dir.path().join("deny.toml");
        let wf = dir.path().join("workflow.yml");
        std::fs::write(&deny, DENY_PREPARED).unwrap();
        std::fs::write(&wf, WORKFLOW_PREPARED).unwrap();
        assert_eq!(detect_posture(&deny, &wf).unwrap(), Posture::Prepared);
    }

    #[test]
    fn detect_active_state() {
        let dir = tempfile::tempdir().unwrap();
        let deny = dir.path().join("deny.toml");
        let wf = dir.path().join("workflow.yml");
        std::fs::write(&deny, DENY_ACTIVE).unwrap();
        std::fs::write(&wf, WORKFLOW_ACTIVE).unwrap();
        assert_eq!(detect_posture(&deny, &wf).unwrap(), Posture::Active);
    }

    #[test]
    fn detect_inconsistent_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let deny = dir.path().join("deny.toml");
        let wf = dir.path().join("workflow.yml");
        std::fs::write(&deny, DENY_ACTIVE).unwrap();
        std::fs::write(&wf, WORKFLOW_PREPARED).unwrap();
        let err = detect_posture(&deny, &wf).unwrap_err();
        assert!(format!("{err:#}").contains("inconsistent"));
    }

    #[test]
    fn transition_prepared_to_active_is_idempotent() {
        let once = transition_deny_toml(DENY_PREPARED, Posture::Active).unwrap();
        let twice = transition_deny_toml(&once, Posture::Active).unwrap();
        assert_eq!(once, twice, "active transition must be idempotent");
        assert!(once.contains("yanked = \"deny\""));
        assert!(once.contains("vulnerability = \"deny\""));
    }

    #[test]
    fn transition_active_to_prepared_is_idempotent() {
        let once = transition_deny_toml(DENY_ACTIVE, Posture::Prepared).unwrap();
        let twice = transition_deny_toml(&once, Posture::Prepared).unwrap();
        assert_eq!(once, twice, "prepared transition must be idempotent");
        assert!(once.contains("yanked = \"warn\""));
        assert!(!once.contains("vulnerability = \"deny\""));
    }

    #[test]
    fn transition_round_trip_preserves_deny() {
        // active → prepared → active must restore the active form.
        let down = transition_deny_toml(DENY_ACTIVE, Posture::Prepared).unwrap();
        let back = transition_deny_toml(&down, Posture::Active).unwrap();
        // Both must end with the same set of strict directives (formatting may vary slightly).
        assert!(back.contains("yanked = \"deny\""));
        assert!(back.contains("vulnerability = \"deny\""));
    }

    #[test]
    fn workflow_transition_adds_passthrough() {
        let out = transition_workflow(WORKFLOW_ACTIVE, Posture::Prepared).unwrap();
        assert!(out.contains("cargo vet --locked || true"));
    }

    #[test]
    fn workflow_transition_removes_passthrough() {
        let out = transition_workflow(WORKFLOW_PREPARED, Posture::Active).unwrap();
        assert!(out.contains("cargo vet --locked"));
        assert!(!out.contains("|| true"));
    }

    #[test]
    fn workflow_transition_idempotent_active() {
        let once = transition_workflow(WORKFLOW_PREPARED, Posture::Active).unwrap();
        let twice = transition_workflow(&once, Posture::Active).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn workflow_transition_idempotent_prepared() {
        let once = transition_workflow(WORKFLOW_ACTIVE, Posture::Prepared).unwrap();
        let twice = transition_workflow(&once, Posture::Prepared).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn workflow_transition_no_vet_step_errors() {
        let bad = "jobs:\n  other:\n    steps:\n      - run: echo hi\n";
        let err = transition_workflow(bad, Posture::Active).unwrap_err();
        assert!(format!("{err:#}").contains("cargo vet"));
    }

    #[test]
    fn worker_context_refused() {
        // SAFETY: tests are single-threaded for env mutation; we restore.
        let prev = std::env::var("COSMON_MOL_DIR").ok();
        unsafe {
            std::env::set_var("COSMON_MOL_DIR", "/tmp/x");
        }
        let err = refuse_worker_context().unwrap_err();
        match prev {
            Some(p) => unsafe {
                std::env::set_var("COSMON_MOL_DIR", p);
            },
            None => unsafe {
                std::env::remove_var("COSMON_MOL_DIR");
            },
        }
        assert!(format!("{err:#}").contains("worker context"));
    }

    #[test]
    fn posture_parse_round_trip() {
        assert_eq!(Posture::parse("prepared").unwrap(), Posture::Prepared);
        assert_eq!(Posture::parse("active").unwrap(), Posture::Active);
        assert!(Posture::parse("garbage").is_err());
    }

    #[test]
    fn yanked_value_extracted() {
        assert_eq!(deny_yanked_value(DENY_PREPARED).unwrap(), "warn");
        assert_eq!(deny_yanked_value(DENY_ACTIVE).unwrap(), "deny");
    }
}
