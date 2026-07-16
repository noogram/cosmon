// SPDX-License-Identifier: AGPL-3.0-only

//! `cs spec-audit` — ledger audit against a TLA+ spec.
//!
//! ## Default mode (`--spec cosmon-run`, the historical behaviour)
//!
//! Reads `events.jsonl`, projects each [`EventV2`] envelope to an
//! [`Action`] via [`cosmon_core::audit::Action::from_event`], replays
//! the sequence through the pure [`cosmon_core::spec::SpecState`],
//! and reports any drift between what the ledger claims happened and
//! what the TLA+ `CosmonRun` spec sanctioned. Catches the c1cb
//! Gödel-class bug (`branch_merged = TRUE` while `status = Pending`)
//! and the convoy-cascade stale-pending class.
//!
//! The historical mode is a **one-shot batch audit**, not a running daemon.
//!
//! ## Multi-spec mode
//!
//! `--spec <name>` selects an alternate auditor. Names supported:
//!
//! * `cosmon-run` (default) — historical behaviour above.
//! * `mycelial-gate` — witness diversity gate on
//!   [`AttestorEventV1::Absorption`].
//! * `attestor-graph` — temporal sanity on the attestor lifecycle.
//! * `witness-freshness` — `ClusterMetadata` snapshot freshness window.
//!
//! `--spec` also accepts a path to a `.tla` file whose basename
//! (without extension, snake-case normalised) matches one of the
//! names above. This is what the briefing's invocation shapes look
//! like, e.g. `--spec noogram/specs/MycelialGate.tla`.
//!
//! All noogram auditors consume the typed NDJSON ledger
//! [`cosmon_core::attestor_event_v1`] (`attestor-events.jsonl`). The
//! `--events` path is required to point at such a file in those
//! modes.
//!
//! ## Out-of-band probe (`CosmonRun` only)
//!
//! The ledger alone cannot witness an external `git merge` bypassing
//! `cs done`. A lightweight git-topology probe
//! (`git merge-base --is-ancestor feat/<mol> origin/main`) is run on
//! every molecule the ledger mentions; the audit cross-checks the
//! probe's answer against the spec state. The probe is only meaningful
//! for `cosmon-run` and is silently inert for the noogram modes.
//!
//! Non-goals (verbatim from the briefing):
//! * no daemon
//! * no auto-remediation — report only
//! * no duplication of the proptest harness in Chantier 1

use std::path::{Path, PathBuf};
use std::process::Command;

use cosmon_core::audit::{audit_trace, AuditReport, BranchMergedProbe};
use cosmon_core::id::MoleculeId;
use cosmon_state::event_log;

use super::Context;

// ---------------------------------------------------------------------------
// SpecKind — the registry of auditors
// ---------------------------------------------------------------------------

/// Which spec auditor to dispatch.
///
/// The registry is small and exhaustive — adding a noogram spec means
/// adding a variant here and a match arm in [`run`]. We deliberately
/// do not use a trait-object registry: the dispatch is one-line, the
/// variants are few, and the closed-set semantics surface
/// unsupported names at compile-time inside the binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpecKind {
    CosmonRun,
    MycelialGate,
    AttestorGraph,
    WitnessFreshness,
}

impl SpecKind {
    fn name(self) -> &'static str {
        match self {
            Self::CosmonRun => "cosmon-run",
            Self::MycelialGate => "mycelial-gate",
            Self::AttestorGraph => "attestor-graph",
            Self::WitnessFreshness => "witness-freshness",
        }
    }

    fn from_arg(raw: &str) -> Result<Self, String> {
        // Accept either a bare name or a path to a .tla file.
        let candidate: String = if raw.to_ascii_lowercase().ends_with(".tla") || raw.contains('/') {
            Path::new(raw)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(raw)
                .to_owned()
        } else {
            raw.to_owned()
        };
        // Normalise: CamelCase / snake_case / kebab-case all collapse
        // to kebab-case lowercase.
        let normalised = normalise_spec_name(&candidate);
        match normalised.as_str() {
            "cosmon-run" => Ok(Self::CosmonRun),
            "mycelial-gate" => Ok(Self::MycelialGate),
            "attestor-graph" => Ok(Self::AttestorGraph),
            "witness-freshness" => Ok(Self::WitnessFreshness),
            other => Err(format!(
                "unknown --spec value {raw:?} (normalised to {other:?}). \
                 Known: cosmon-run, mycelial-gate, attestor-graph, witness-freshness."
            )),
        }
    }

    fn default_events_path(self, ctx: &Context) -> PathBuf {
        let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
        match self {
            Self::CosmonRun => state_dir.join("events.jsonl"),
            Self::MycelialGate | Self::AttestorGraph | Self::WitnessFreshness => {
                state_dir.join("attestor-events.jsonl")
            }
        }
    }
}

/// Normalise a spec name to canonical kebab-case lowercase.
///
/// `MycelialGate` → `mycelial-gate`; `Mycelial_Gate` → `mycelial-gate`;
/// `mycelial-gate` → `mycelial-gate`. Any non-alphanumeric character
/// is treated as a separator.
fn normalise_spec_name(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    let chars: Vec<char> = s.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() {
            // Insert a separator before an uppercase letter that
            // follows a lowercase one (so `MycelialGate` splits, but
            // `URL` stays compact).
            if i > 0 {
                let prev = chars[i - 1];
                if prev.is_ascii_lowercase() || prev.is_ascii_digit() {
                    out.push('-');
                }
            }
            out.push(c.to_ascii_lowercase());
        } else if c.is_ascii_alphanumeric() {
            out.push(*c);
        } else {
            // Underscores, dashes, dots, slashes → all collapse to `-`.
            if !out.ends_with('-') {
                out.push('-');
            }
        }
    }
    out.trim_matches('-').to_owned()
}

// ---------------------------------------------------------------------------
// CLI args
// ---------------------------------------------------------------------------

/// Arguments for `cs spec-audit`.
#[derive(clap::Args)]
pub struct Args {
    /// Fleet id whose ledger should be audited. Defaults to the fleet
    /// resolved from the walk-up config (`default` for single-fleet
    /// projects). The fleet id is **advisory** today — the canonical
    /// `events.jsonl` lives at `.cosmon/state/events.jsonl` regardless
    /// of fleet — but the flag is accepted so future multi-ledger
    /// layouts stay backward-compatible.
    #[arg(long, value_name = "FLEET")]
    pub fleet: Option<String>,

    /// Which spec to audit against. Defaults to `cosmon-run`
    /// (historical behaviour). Other accepted values: `mycelial-gate`,
    /// `attestor-graph`, `witness-freshness`. A path to a `.tla` file
    /// is also accepted; the basename (snake/Camel-case normalised) is
    /// looked up in the registry.
    #[arg(long, value_name = "SPEC", default_value = "cosmon-run")]
    pub spec: String,

    /// Explicit path to the events file to audit. Overrides
    /// `--fleet` and the walk-up state-dir discovery. The expected
    /// format depends on `--spec`:
    ///
    /// * `cosmon-run` → `.cosmon/state/events.jsonl` (`EventV2` envelopes).
    /// * Noogram specs → `.cosmon/state/attestor-events.jsonl`
    ///   (`AttestorEventV1` envelopes, see
    ///   `cosmon/docs/specs/attestor-events.schema.json`).
    #[arg(long, value_name = "PATH")]
    pub events: Option<PathBuf>,

    /// Repository whose branch topology should be probed for the
    /// c1cb out-of-band check. Defaults to the current working
    /// directory. Pass `--no-git-probe` to disable the probe entirely
    /// (useful when `git` is not available, e.g. inside CI sandboxes).
    /// Only meaningful when `--spec cosmon-run`.
    #[arg(long, value_name = "PATH")]
    pub repo: Option<PathBuf>,

    /// Disable the git-topology probe. When set, the audit still flags
    /// disabled-action-fired drifts but does not emit `bypass_merge`
    /// findings. The switch exists so the audit stays useful in
    /// environments without `git` (containers, strict sandboxes).
    /// Only meaningful when `--spec cosmon-run`.
    #[arg(long)]
    pub no_git_probe: bool,

    /// Target branch for the merge-topology probe. Defaults to
    /// `origin/main`; pass `main` to check the local branch when no
    /// remote tracking is configured. Only meaningful when
    /// `--spec cosmon-run`.
    #[arg(long, value_name = "REF", default_value = "origin/main")]
    pub target_ref: String,
}

/// Resolve the path to the events log the audit should read.
fn resolve_events_path(ctx: &Context, args: &Args, kind: SpecKind) -> PathBuf {
    if let Some(p) = &args.events {
        return p.clone();
    }
    kind.default_events_path(ctx)
}

/// Run the audit and print the report.
///
/// Exit code is 0 on a clean report, 1 on any drift. The JSON surface
/// serialises [`AuditReport`] verbatim so scripts can pipe it through
/// `jq`.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let kind = SpecKind::from_arg(&args.spec).map_err(anyhow::Error::msg)?;
    let events_path = resolve_events_path(ctx, args, kind);

    let report = match kind {
        SpecKind::CosmonRun => audit_cosmon_run(ctx, args, &events_path)?,
        SpecKind::MycelialGate => audit_noogram(&events_path, |evts| {
            cosmon_core::attestor_audit::audit_mycelial_gate(evts)
        })?,
        SpecKind::AttestorGraph => audit_noogram(&events_path, |evts| {
            cosmon_core::attestor_audit::audit_attestor_graph(evts)
        })?,
        SpecKind::WitnessFreshness => audit_noogram(&events_path, |evts| {
            cosmon_core::attestor_audit::audit_witness_freshness(evts)
        })?,
    };

    if ctx.json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        print_human(&report, &events_path, kind);
    }

    if report.is_clean() {
        Ok(())
    } else {
        anyhow::bail!(
            "spec-audit ({spec}) found {drifts} drift(s) in {events_replayed} events",
            spec = kind.name(),
            drifts = report.drifts.len(),
            events_replayed = report.events_replayed,
        )
    }
}

fn audit_cosmon_run(ctx: &Context, args: &Args, events_path: &Path) -> anyhow::Result<AuditReport> {
    let _ = ctx; // currently unused; kept for symmetry with audit_noogram
    let envelopes = if events_path.exists() {
        event_log::read_all(events_path)?
    } else {
        // Empty log is a clean audit — keep the command ergonomic for
        // fresh projects that have not yet run anything.
        Vec::new()
    };

    let report = if args.no_git_probe {
        audit_trace(&envelopes, &cosmon_core::audit::NullProbe)
    } else {
        let repo = args
            .repo
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let probe = GitTopologyProbe::new(repo, args.target_ref.clone());
        audit_trace(&envelopes, &probe)
    };
    Ok(report)
}

fn audit_noogram<F>(events_path: &Path, audit_fn: F) -> anyhow::Result<AuditReport>
where
    F: FnOnce(&[cosmon_core::attestor_event_v1::AttestorEnvelope]) -> AuditReport,
{
    let envelopes = if events_path.exists() {
        cosmon_state::attestor_log::read_all(events_path)?
    } else {
        Vec::new()
    };
    Ok(audit_fn(&envelopes))
}

fn print_human(report: &AuditReport, path: &Path, kind: SpecKind) {
    if report.is_clean() {
        println!(
            "\u{2705} spec-audit [{spec}] clean: {events} events, {mols} subjects ({path})",
            spec = kind.name(),
            events = report.events_replayed,
            mols = report.molecules_seen,
            path = path.display(),
        );
        return;
    }

    println!(
        "\u{274C} spec-audit [{spec}]: {drifts} drift(s) in {events} events ({path})",
        spec = kind.name(),
        drifts = report.drifts.len(),
        events = report.events_replayed,
        path = path.display(),
    );
    for drift in &report.drifts {
        match drift {
            cosmon_core::audit::Drift::BypassMerge { seq, molecule_id } => {
                println!("   [seq {seq}] bypass_merge: {molecule_id} branch merged while status=Pending (c1cb)");
            }
            cosmon_core::audit::Drift::DisabledActionFired {
                seq,
                molecule_id,
                action,
                note,
            } => {
                let mol = molecule_id.as_ref().map_or("<unknown>", MoleculeId::as_str);
                println!("   [seq {seq}] disabled_action: {mol} {action:?} — {note}");
            }
            cosmon_core::audit::Drift::UnmappedEvent {
                seq,
                molecule_id,
                variant,
            } => {
                let mol = molecule_id.as_ref().map_or("<unknown>", MoleculeId::as_str);
                println!("   [seq {seq}] unmapped: {mol} {variant}");
            }
            cosmon_core::audit::Drift::SpecInvariantViolation {
                seq,
                spec,
                invariant,
                subject,
                note,
            } => {
                let subj = subject.as_deref().unwrap_or("<n/a>");
                println!("   [seq {seq}] {spec}/{invariant}: {subj} — {note}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Git-topology probe
// ---------------------------------------------------------------------------

/// A [`BranchMergedProbe`] backed by
/// `git merge-base --is-ancestor <candidate> <target>`.
///
/// The candidate branches searched are (in order):
///
/// 1. `feat/<molecule_id>` — the convention `cs tackle` uses for new
///    worktrees (see `crates/cosmon-cli/src/cmd/tackle.rs`).
/// 2. `<molecule_id>` — a bare candidate, used by a handful of early
///    molecules that predate the convention.
///
/// A `None` return means the probe could not decide (no branch found,
/// git exited non-zero, git not available). The audit treats `None` as
/// "no out-of-band witness" — it does not manufacture a drift from
/// absence of evidence.
struct GitTopologyProbe {
    repo: PathBuf,
    target_ref: String,
}

impl GitTopologyProbe {
    fn new(repo: PathBuf, target_ref: String) -> Self {
        Self { repo, target_ref }
    }

    fn is_ancestor(&self, candidate: &str) -> Option<bool> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .arg("merge-base")
            .arg("--is-ancestor")
            .arg(candidate)
            .arg(&self.target_ref)
            .output()
            .ok()?;
        if output.status.code() == Some(0) {
            Some(true)
        } else if output.status.code() == Some(1) {
            Some(false)
        } else {
            // Unknown candidate (e.g. branch does not exist) — surface as
            // "cannot decide" rather than a false negative.
            None
        }
    }
}

impl BranchMergedProbe for GitTopologyProbe {
    fn is_branch_merged(&self, molecule: &MoleculeId) -> Option<bool> {
        let candidates = [
            format!("feat/{}", molecule.as_str()),
            molecule.as_str().to_owned(),
        ];
        for c in &candidates {
            if let Some(answer) = self.is_ancestor(c) {
                return Some(answer);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::audit::NullProbe;
    use cosmon_core::event_v2::{Envelope, EventV2, Seq};
    use tempfile::tempdir;

    fn mid(s: &str) -> MoleculeId {
        MoleculeId::new(s).unwrap()
    }

    fn write_events(path: &Path, events: &[EventV2]) {
        use std::io::Write;
        let mut f = std::fs::File::create(path).unwrap();
        for (i, e) in events.iter().enumerate() {
            let env = Envelope::new(Seq(i as u64), None, e.clone());
            writeln!(f, "{}", serde_json::to_string(&env).unwrap()).unwrap();
        }
    }

    #[test]
    fn empty_ledger_is_clean() {
        let dir = tempdir().unwrap();
        let report = audit_trace(&[], &NullProbe);
        assert!(report.is_clean());
        let _ = dir; // keep it alive
    }

    #[test]
    fn resolve_events_path_uses_explicit_override() {
        let ctx = Context {
            verbose: false,
            json: false,
            config: None,
        };
        let args = Args {
            fleet: None,
            spec: "cosmon-run".into(),
            events: Some(PathBuf::from("/tmp/custom.jsonl")),
            repo: None,
            no_git_probe: true,
            target_ref: "origin/main".into(),
        };
        assert_eq!(
            resolve_events_path(&ctx, &args, SpecKind::CosmonRun),
            PathBuf::from("/tmp/custom.jsonl")
        );
    }

    #[test]
    fn reads_ledger_and_flags_disabled_done() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let m = mid("cs-20260419-cccc");
        write_events(
            &path,
            &[
                EventV2::MoleculeNucleated {
                    molecule_id: m.clone(),
                    formula_id: "task-work".into(),
                    parent_id: None,
                    blocks: Vec::new(),
                },
                EventV2::MergeCompleted {
                    molecule: m,
                    branch: "feat/task-y".into(),
                    result: cosmon_core::event_v2::MergeResult::Ok,
                    federation_provenance: None,
                },
            ],
        );
        let envelopes = event_log::read_all(&path).unwrap();
        let report = audit_trace(&envelopes, &NullProbe);
        assert_eq!(report.drifts.len(), 1);
    }

    #[test]
    fn spec_kind_resolves_bare_names() {
        assert_eq!(SpecKind::from_arg("cosmon-run"), Ok(SpecKind::CosmonRun));
        assert_eq!(
            SpecKind::from_arg("mycelial-gate"),
            Ok(SpecKind::MycelialGate)
        );
        assert_eq!(
            SpecKind::from_arg("attestor-graph"),
            Ok(SpecKind::AttestorGraph)
        );
        assert_eq!(
            SpecKind::from_arg("witness-freshness"),
            Ok(SpecKind::WitnessFreshness)
        );
    }

    #[test]
    fn spec_kind_resolves_tla_path() {
        assert_eq!(
            SpecKind::from_arg("noogram/specs/MycelialGate.tla"),
            Ok(SpecKind::MycelialGate)
        );
        assert_eq!(
            SpecKind::from_arg("AttestorGraph.tla"),
            Ok(SpecKind::AttestorGraph)
        );
        assert_eq!(
            SpecKind::from_arg("specs/WitnessFreshness.tla"),
            Ok(SpecKind::WitnessFreshness)
        );
    }

    #[test]
    fn spec_kind_rejects_unknown() {
        assert!(SpecKind::from_arg("UnknownSpec").is_err());
    }

    #[test]
    fn normalise_camel_case() {
        assert_eq!(normalise_spec_name("MycelialGate"), "mycelial-gate");
        assert_eq!(normalise_spec_name("CosmonRun"), "cosmon-run");
        assert_eq!(normalise_spec_name("Mycelial_Gate"), "mycelial-gate");
        assert_eq!(normalise_spec_name("mycelial-gate"), "mycelial-gate");
    }
}
