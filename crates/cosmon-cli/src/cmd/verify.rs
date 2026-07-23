// SPDX-License-Identifier: AGPL-3.0-only

//! `cs verify` — re-check the proof-of-work chain of a molecule.
//!
//! Three layers of verification:
//!
//! 1. **Briefing seals (soft contract).** `cs nucleate` and `cs evolve`
//!    stamp BLAKE3 hashes of `prompt.md` (once) and `briefing.md` (once
//!    per step advance) onto `MoleculeData::prompt_seal` and
//!    `MoleculeData::briefing_seals`. These seals catch *retrospective*
//!    edits to cognitive contracts that should be immutable once their
//!    lifecycle moment has passed. A mismatch flags a shadow contract;
//!    a missing seal is inconclusive (legacy molecule), not a failure.
//!    Use `--step N` to verify a specific step seal.
//! 2. **Artifact chain.** `cs complete` seals `verify.json` with BLAKE3
//!    hashes of every markdown artifact (prompt, briefing, synthesis, …).
//!    `cs verify` recomputes those hashes and reports per-artifact
//!    PASS/FAIL. Editing `synthesis.md` after completion produces a
//!    hash mismatch.
//! 3. **Gate replay.** For each step in the molecule's formula:
//!      - `native = "..."` steps are re-invoked via the built-in registry;
//!      - `command = "..."` steps are re-executed via `sh -c`;
//!      - Claude-worker steps are not replayable and skipped.
//!
//!    Exit code per step is recorded.
//!
//! Finally the command writes `verify-report.md` to the molecule directory
//! and exits 0 iff every check passed, 1 otherwise.
//!
//! The legacy event-log hash chain (from plumbing v2) is still walked and
//! reported in the same run — a superset of the original behaviour.

use std::path::Path;

use cosmon_core::event_v2::EventV2;
use cosmon_core::formula::Formula;
use cosmon_state::{BriefingSeal, MoleculeData, StateStore};

use super::Context;
use crate::pow;

/// Arguments for `cs verify`.
#[derive(clap::Args)]
pub struct Args {
    /// Molecule ID (or prefix) whose proof-of-work chain should be verified.
    ///
    /// Optional when `--federation` is set — the federation provenance
    /// scan is a fleet-wide audit and does not target a single molecule.
    pub molecule_id: Option<String>,

    /// Skip gate replay (shell/native step re-execution). Artifact
    /// hash check and event chain check still run.
    #[arg(long)]
    pub no_replay: bool,

    /// Verify the briefing seal for a specific zero-based step index.
    ///
    /// When omitted, the most recent briefing seal (if any) is checked
    /// against the current `briefing.md`. When specified, the
    /// corresponding entry from `MoleculeData::briefing_seals` is used.
    /// If no seal exists for the requested step, the check is reported
    /// as SKIP (inconclusive), never as FAIL.
    #[arg(long, value_name = "N")]
    pub step: Option<u32>,

    /// Scan the fleet-wide event log for cross-galaxy events missing
    /// federation provenance (ADR-105, I9' machinery).
    ///
    /// When set, walks `<state_dir>/events.jsonl` and reports every
    /// cross-galaxy event whose `federation_provenance` is `None`:
    ///
    /// - `MergeDispatched` / `MergeCompleted` whose `molecule_id` or
    ///   `branch` carries a foreign galaxy alias (Oracle B subject-mark
    ///   per ADR-105 §D3).
    /// - `ChronicleAdded` whose `cites_galaxies` mentions a non-cosmon
    ///   peer (Oracle B'' delegation-dispatched per ADR-105 §D3).
    /// - `AdrInscribed` whose `cites_galaxies` mentions a non-cosmon
    ///   peer (same Oracle B'' channel for ADR-grade citations).
    ///
    /// Missing provenance is a hard FAIL — the federation discipline is
    /// detect-on-write, `cs verify --federation` is the audit oracle.
    ///
    /// The flag stacks with `molecule_id`: when both are given, the
    /// scan is restricted to the molecule's local `events.jsonl`.
    /// When `molecule_id` is omitted, the fleet-wide log is scanned.
    #[arg(long)]
    pub federation: bool,

    /// Tolerate cross-galaxy events emitted before a specific date
    /// that lack federation provenance.
    ///
    /// Format: ISO8601 date (e.g. `2026-05-19`). Events whose envelope
    /// timestamp is strictly before the date are downgraded from FAIL
    /// to SKIP with a `tracing::warn!`-equivalent detail line. Only
    /// meaningful in combination with `--federation`.
    ///
    /// Default: not set — every cross-galaxy event without provenance
    /// is a hard FAIL. ADR-105 §"Backfill discipline" recommends
    /// option (a) (backfill the field by reading the existing citation
    /// format); this flag is option (b), the legacy-tolerate escape
    /// hatch for the migration window.
    #[arg(long, value_name = "DATE")]
    pub legacy_tolerate_before: Option<String>,

    /// Check structural state-machine invariants over molecule rows.
    ///
    /// Currently a single invariant is enforced:
    /// **`archived ⇒ status.is_terminal()`** — an archived molecule
    /// must carry a terminal status (`Completed` or `Collapsed`). A
    /// row with `{archived: true, status: running}` is a *ghost*: it
    /// was torn down out-of-band (e.g. `cs done --force` on a
    /// never-completed molecule) without terminalizing its status, so
    /// it keeps rendering as live work.
    ///
    /// Detection only — `cs verify --invariants` never mutates state;
    /// it exits non-zero when any violation is found. To *heal* the
    /// on-disk rows (rewrite `status → Collapsed`), run
    /// `cs reconcile --heal-invariants`.
    ///
    /// Like `--federation`, the flag stacks with `molecule_id`: with a
    /// molecule given, only that row is checked; without one, every
    /// molecule in the fleet is swept (the galaxy-wide audit).
    #[arg(long)]
    pub invariants: bool,
}

/// One row in the verify report.
#[derive(Debug, Clone)]
struct Check {
    category: &'static str,
    name: String,
    status: CheckStatus,
    detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckStatus {
    Pass,
    Fail,
    Skip,
}

impl CheckStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Skip => "SKIP",
        }
    }
}

/// Execute the `verify` command.
///
/// # Errors
///
/// Returns an error if the molecule cannot be resolved. Successful
/// verification may still exit the process with status 1 — see the
/// command docs above.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);

    let store = ctx.store_at(&state_dir);

    // Fleet-wide audit mode: no molecule_id required. Runs whichever
    // fleet-scoped checks were requested (`--federation` and/or
    // `--invariants`) over the whole galaxy and exits. The federation
    // scan is the ADR-105 / I9' machinery hook; the invariant scan is
    // the `archived ⇒ status.is_terminal()` ghost detector
    // (idea-20260618-1b10).
    if args.molecule_id.is_none() && (args.federation || args.invariants) {
        return run_fleet_audit(ctx, store.as_ref(), &state_dir, args);
    }

    // Per-molecule mode: molecule_id is required.
    let needle = args
        .molecule_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("molecule_id is required unless --federation is set"))?;

    // Resolve prefix to full ID.
    let all = store.list_molecules(&cosmon_state::MoleculeFilter::default())?;
    let matches: Vec<_> = all
        .iter()
        .filter(|m| m.id.as_str().starts_with(needle) || m.id.as_str() == needle)
        .collect();
    let data = match matches.as_slice() {
        [one] => (*one).clone(),
        [] => anyhow::bail!("no molecule matches '{needle}'"),
        many => anyhow::bail!("ambiguous prefix '{needle}' ({} matches)", many.len()),
    };

    let mol_dir = store.molecule_dir(&data.id);
    let mut checks: Vec<Check> = Vec::new();

    // Layer 0 — soft-contract briefing seals (non-blocking probe).
    verify_briefing_seals(&mol_dir, &data, args.step, &mut checks);

    // Layer 1 — artifact hash chain.
    verify_artifacts(&mol_dir, &mut checks);

    // Layer 2 — gate replay.
    if !args.no_replay {
        replay_gates(&state_dir, &mol_dir, data.formula_id.as_str(), &mut checks);
    }

    // Layer 3 — event-log chain (existing plumbing v2 behaviour).
    verify_event_chain(&mol_dir, &mut checks);

    // Layer 4 — federation provenance (ADR-105 / I9') — opt-in via
    // `--federation`. Scoped to this molecule's local events.jsonl.
    if args.federation {
        verify_federation_provenance(
            &mol_dir.join("events.jsonl"),
            args.legacy_tolerate_before.as_deref(),
            &mut checks,
        );
    }

    // Layer 5 — structural invariant `archived ⇒ status.is_terminal()`
    // — opt-in via `--invariants`. Scoped to this molecule's row.
    if args.invariants {
        verify_archived_terminal(&data, &mut checks);
    }

    let any_fail = checks.iter().any(|c| c.status == CheckStatus::Fail);
    let no_seal = data.prompt_seal.is_none()
        && data.briefing_seals.is_empty()
        && data.bootstrap_seals.is_empty();

    // Write the markdown report.
    let report = render_report(data.id.as_str(), &checks);
    let _ = std::fs::write(mol_dir.join("verify-report.md"), &report);

    if ctx.json {
        let rows: Vec<serde_json::Value> = checks
            .iter()
            .map(|c| {
                serde_json::json!({
                    "category": c.category,
                    "name": c.name,
                    "status": c.status.label(),
                    "detail": c.detail,
                })
            })
            .collect();
        let out = serde_json::json!({
            "molecule_id": data.id.as_str(),
            "status": if any_fail { "fail" } else { "pass" },
            "checks": rows,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        for c in &checks {
            println!("[{}] {}: {}", c.status.label(), c.category, c.name);
            if !c.detail.is_empty() {
                println!("      {}", c.detail);
            }
        }
        println!();
        if any_fail {
            println!("verify: FAIL ({})", data.id.as_str());
        } else {
            println!("verify: PASS ({})", data.id.as_str());
        }
    }

    if any_fail {
        std::process::exit(1);
    }
    // Exit code 2: inconclusive — no seals present on this molecule
    // (legacy molecule, predates the feature). Distinguishes "verified
    // to match" from "we have no way to tell". Artifact-chain verify
    // still ran; if the molecule had a sealed `verify.json`, layer 1
    // would have produced PASS/FAIL rows.
    if no_seal && pow::read(&mol_dir).is_none() {
        std::process::exit(2);
    }
    Ok(())
}

/// Verify soft-contract briefing/prompt seals (non-blocking probe).
///
/// Never produces a FAIL when a seal is *absent* — missing seals are
/// SKIP (inconclusive), matching the "propose don't impose" principle.
/// A FAIL is produced only when a recorded seal's hash does not match
/// the current contents of the corresponding file — that is the
/// shadow-contract smoke alarm.
#[allow(clippy::too_many_lines)]
fn verify_briefing_seals(
    mol_dir: &Path,
    mol: &MoleculeData,
    step_filter: Option<u32>,
    checks: &mut Vec<Check>,
) {
    // Prompt seal is independent of step_filter — always checked when
    // present. Missing prompt seal is SKIP (legacy molecule).
    match &mol.prompt_seal {
        Some(seal) => match std::fs::read(mol_dir.join("prompt.md")) {
            Ok(bytes) if seal.matches(&bytes) => checks.push(Check {
                category: "seal",
                name: "prompt.md".to_owned(),
                status: CheckStatus::Pass,
                detail: format!("blake3 {}", short_hash(&seal.hash)),
            }),
            Ok(_) => checks.push(Check {
                category: "seal",
                name: "prompt.md".to_owned(),
                status: CheckStatus::Fail,
                detail: format!(
                    "shadow contract: prompt.md modified since nucleation (sealed {})",
                    seal.sealed_at.to_rfc3339()
                ),
            }),
            Err(e) => checks.push(Check {
                category: "seal",
                name: "prompt.md".to_owned(),
                status: CheckStatus::Skip,
                detail: format!("prompt.md unreadable: {e}"),
            }),
        },
        None => checks.push(Check {
            category: "seal",
            name: "prompt.md".to_owned(),
            status: CheckStatus::Skip,
            detail: "no prompt seal (legacy molecule or seal emission failed)".to_owned(),
        }),
    }

    // Briefing seal: either the specific step requested via --step, or
    // the most recent one on record.
    let selected_seal: Option<&BriefingSeal> = match step_filter {
        Some(n) => mol.briefing_seals.iter().find(|s| s.step == n),
        None => mol.briefing_seals.last(),
    };

    if let Some(seal) = selected_seal {
        // Verify against the immutable per-step snapshot when the seal
        // carries one. cosmon regenerates `briefing.md` at every advance
        // and `cs complete` rewrites it once more, so comparing a
        // historical seal against the *current* file flags cosmon's own
        // honest rewrite as tampering — the flagship false positive. The
        // snapshot is the briefing as it was at this step; a rewrite of
        // the recorded snapshot (hash no longer matches) is still caught.
        // Legacy seals (no snapshot) fall back to the live-file
        // comparison for backward compatibility.
        match &seal.snapshot {
            Some(snapshot) if seal.matches(snapshot.as_bytes()) => checks.push(Check {
                category: "seal",
                name: format!("briefing.md@step{}", seal.step),
                status: CheckStatus::Pass,
                detail: format!("blake3 {} (step snapshot)", short_hash(&seal.hash)),
            }),
            Some(_) => checks.push(Check {
                category: "seal",
                name: format!("briefing.md@step{}", seal.step),
                status: CheckStatus::Fail,
                detail: format!(
                    "shadow contract: sealed briefing snapshot for step {} was altered since seal at {}",
                    seal.step,
                    seal.sealed_at.to_rfc3339()
                ),
            }),
            None => match std::fs::read(mol_dir.join("briefing.md")) {
                Ok(bytes) if seal.matches(&bytes) => checks.push(Check {
                    category: "seal",
                    name: format!("briefing.md@step{}", seal.step),
                    status: CheckStatus::Pass,
                    detail: format!("blake3 {}", short_hash(&seal.hash)),
                }),
                Ok(_) => checks.push(Check {
                    category: "seal",
                    name: format!("briefing.md@step{}", seal.step),
                    status: CheckStatus::Skip,
                    detail: format!(
                        "legacy seal without snapshot; briefing.md differs (cosmon rewrites it per step) — inconclusive, sealed {}",
                        seal.sealed_at.to_rfc3339()
                    ),
                }),
                Err(e) => checks.push(Check {
                    category: "seal",
                    name: format!("briefing.md@step{}", seal.step),
                    status: CheckStatus::Skip,
                    detail: format!("briefing.md unreadable: {e}"),
                }),
            },
        }
    } else {
        let detail = step_filter.map_or_else(
            || "no briefing seals (legacy molecule or pre-evolve)".to_owned(),
            |n| format!("no seal recorded for step {n}"),
        );
        checks.push(Check {
            category: "seal",
            name: "briefing.md".to_owned(),
            status: CheckStatus::Skip,
            detail,
        });
    }

    // Bootstrap seal (W2 of delib-20260519-e6db). Re-run the
    // agent-harness walk over the verifier's cwd and compare against
    // the recorded seal. SKIP when no seal exists (legacy molecule).
    // FAIL when the walk's output hashes to something other than the
    // recorded value — that is the shadow-contract smoke alarm for
    // cross-worktree poisoning or a post-advance edit to
    // AGENTS.md/CLAUDE.md.
    let selected_bootstrap: Option<&BriefingSeal> = match step_filter {
        Some(n) => mol.bootstrap_seals.iter().find(|s| s.step == n),
        None => mol.bootstrap_seals.last(),
    };

    if let Some(seal) = selected_bootstrap {
        // Verify against the immutable snapshot of the walk-as-it-was-at-
        // this-step when the seal carries one. The bootstrap walk covers
        // the operator's ambient `AGENTS.md` / `CLAUDE.md`, which live
        // OUTSIDE the molecule and drift legitimately (the operator edits
        // their own instructions; the worktree is torn down and `cs
        // verify` runs from a different cwd). Re-walking live therefore
        // fires on ambient drift, not tampering — the flagship false
        // positive. A rewrite of the recorded snapshot is still caught.
        // Legacy seals (no snapshot) fall back to the live re-walk.
        match &seal.snapshot {
            Some(snapshot) if seal.matches(snapshot.as_bytes()) => checks.push(Check {
                category: "seal",
                name: format!("bootstrap@step{}", seal.step),
                status: CheckStatus::Pass,
                detail: format!("blake3 {} (step snapshot)", short_hash(&seal.hash)),
            }),
            Some(_) => checks.push(Check {
                category: "seal",
                name: format!("bootstrap@step{}", seal.step),
                status: CheckStatus::Fail,
                detail: format!(
                    "shadow contract: sealed bootstrap snapshot for step {} was altered since seal at {}",
                    seal.step,
                    seal.sealed_at.to_rfc3339()
                ),
            }),
            None => {
                let live_dir =
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                let live_bootstrap =
                    cosmon_agent_harness::bootstrap::collect_bootstrap_context(&live_dir);
                if seal.matches(live_bootstrap.as_bytes()) {
                    checks.push(Check {
                        category: "seal",
                        name: format!("bootstrap@step{}", seal.step),
                        status: CheckStatus::Pass,
                        detail: format!("blake3 {}", short_hash(&seal.hash)),
                    });
                } else {
                    // Legacy seal, no snapshot: the live walk covers ambient
                    // operator files that drift outside the molecule's
                    // control, so a mismatch is inconclusive, not tampering.
                    checks.push(Check {
                        category: "seal",
                        name: format!("bootstrap@step{}", seal.step),
                        status: CheckStatus::Skip,
                        detail: format!(
                            "legacy seal without snapshot; AGENTS.md/CLAUDE.md walk differs (ambient operator files drift) — inconclusive, sealed {}",
                            seal.sealed_at.to_rfc3339()
                        ),
                    });
                }
            }
        }
    } else {
        let detail = step_filter.map_or_else(
            || "no bootstrap seals (legacy molecule or pre-evolve)".to_owned(),
            |n| format!("no bootstrap seal recorded for step {n}"),
        );
        checks.push(Check {
            category: "seal",
            name: "bootstrap".to_owned(),
            status: CheckStatus::Skip,
            detail,
        });
    }
}

/// Re-hash artifacts and compare against the sealed manifest.
fn verify_artifacts(mol_dir: &Path, checks: &mut Vec<Check>) {
    let Some(manifest) = pow::read(mol_dir) else {
        checks.push(Check {
            category: "artifact",
            name: "verify.json".to_owned(),
            status: CheckStatus::Skip,
            detail: "no sealed manifest — molecule was not completed via `cs complete`".to_owned(),
        });
        return;
    };

    let current = pow::compute_artifact_hashes(mol_dir);

    // Detect sealed artifacts that have changed or disappeared.
    for (name, sealed_hex) in &manifest.artifacts {
        match current.get(name) {
            Some(cur_hex) if cur_hex == sealed_hex => checks.push(Check {
                category: "artifact",
                name: name.clone(),
                status: CheckStatus::Pass,
                detail: format!("blake3 {}", short_hash(sealed_hex)),
            }),
            Some(cur_hex) => checks.push(Check {
                category: "artifact",
                name: name.clone(),
                status: CheckStatus::Fail,
                detail: format!(
                    "hash mismatch: sealed {}, current {}",
                    short_hash(sealed_hex),
                    short_hash(cur_hex)
                ),
            }),
            None => checks.push(Check {
                category: "artifact",
                name: name.clone(),
                status: CheckStatus::Fail,
                detail: "artifact was present at seal time but is missing now".to_owned(),
            }),
        }
    }

    // Detect artifacts added after sealing (informational, not a failure).
    for name in current.keys() {
        if !manifest.artifacts.contains_key(name) {
            checks.push(Check {
                category: "artifact",
                name: name.clone(),
                status: CheckStatus::Skip,
                detail: "new artifact added after sealing (not part of manifest)".to_owned(),
            });
        }
    }
}

fn short_hash(hex: &str) -> String {
    hex.chars().take(12).collect::<String>() + "…"
}

/// Replay shell and native gates declared in the molecule's formula.
fn replay_gates(state_dir: &Path, mol_dir: &Path, formula_id: &str, checks: &mut Vec<Check>) {
    let formulas_dir = cosmon_filestore::resolve_formulas_dir_from(state_dir);
    let formula_path = formulas_dir.join(format!("{formula_id}.formula.toml"));
    let Ok(text) = std::fs::read_to_string(&formula_path) else {
        checks.push(Check {
            category: "gate",
            name: formula_id.to_owned(),
            status: CheckStatus::Skip,
            detail: format!("formula file not found at {}", formula_path.display()),
        });
        return;
    };
    let formula = match Formula::parse(&text) {
        Ok(f) => f,
        Err(e) => {
            checks.push(Check {
                category: "gate",
                name: formula_id.to_owned(),
                status: CheckStatus::Fail,
                detail: format!("failed to parse formula: {e}"),
            });
            return;
        }
    };

    let work_dir = state_dir.parent().and_then(Path::parent).map_or_else(
        || std::env::current_dir().unwrap_or_default(),
        Path::to_path_buf,
    );

    for step in &formula.steps {
        if let Some(native_key) = &step.native {
            replay_native(native_key, &step.id, mol_dir, &work_dir, checks);
        } else if let Some(command) = &step.command {
            replay_shell(command, &step.id, &work_dir, checks);
        }
        // Claude-worker steps (no command, no native) are not replayable.
    }
}

fn replay_native(
    key: &str,
    step_id: &str,
    mol_dir: &Path,
    work_dir: &Path,
    checks: &mut Vec<Check>,
) {
    let Some(func) = crate::native::lookup(key) else {
        checks.push(Check {
            category: "gate",
            name: format!("{step_id} ({key})"),
            status: CheckStatus::Fail,
            detail: "native key not found in registry".to_owned(),
        });
        return;
    };
    let ctx = crate::native::NativeCtx {
        mol_dir: mol_dir.to_path_buf(),
        step_id: step_id.to_owned(),
        work_dir: work_dir.to_path_buf(),
    };
    match func(&ctx) {
        Ok(_) => checks.push(Check {
            category: "gate",
            name: format!("{step_id} ({key})"),
            status: CheckStatus::Pass,
            detail: "native replay ok".to_owned(),
        }),
        Err(e) => checks.push(Check {
            category: "gate",
            name: format!("{step_id} ({key})"),
            status: CheckStatus::Fail,
            detail: format!("native replay failed: {e}"),
        }),
    }
}

fn replay_shell(command: &str, step_id: &str, work_dir: &Path, checks: &mut Vec<Check>) {
    // Trust gate (B5, RCE-by-clone): replaying a gate means running a
    // repo-supplied shell string. Refuse for an untrusted repository rather
    // than executing it during an audit.
    if let Err(e) = cosmon_cli::trust::ensure_trusted(work_dir) {
        checks.push(Check {
            category: "gate",
            name: format!("{step_id} (shell)"),
            status: CheckStatus::Fail,
            detail: format!("refused: {e}"),
        });
        return;
    }
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(work_dir)
        .output();
    match out {
        Ok(o) if o.status.success() => checks.push(Check {
            category: "gate",
            name: format!("{step_id} (shell)"),
            status: CheckStatus::Pass,
            detail: format!("exit 0: {}", truncate(command, 80)),
        }),
        Ok(o) => checks.push(Check {
            category: "gate",
            name: format!("{step_id} (shell)"),
            status: CheckStatus::Fail,
            detail: format!(
                "exit {}: {}",
                o.status.code().unwrap_or(-1),
                truncate(command, 80)
            ),
        }),
        Err(e) => checks.push(Check {
            category: "gate",
            name: format!("{step_id} (shell)"),
            status: CheckStatus::Fail,
            detail: format!("failed to spawn shell: {e}"),
        }),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_owned()
    } else {
        format!("{}…", &s[..n])
    }
}

/// Walk the event-log hash chain (plumbing v2 behaviour).
fn verify_event_chain(mol_dir: &Path, checks: &mut Vec<Check>) {
    let events_path = mol_dir.join("events.jsonl");
    if !events_path.exists() {
        checks.push(Check {
            category: "event-chain",
            name: "events.jsonl".to_owned(),
            status: CheckStatus::Skip,
            detail: "no molecule-local event log".to_owned(),
        });
        return;
    }
    match cosmon_filestore::event::verify_chain(&events_path) {
        Ok(cosmon_filestore::event::VerifyOutcome::Verified { count }) => checks.push(Check {
            category: "event-chain",
            name: "events.jsonl".to_owned(),
            status: CheckStatus::Pass,
            detail: format!("{count} events chained"),
        }),
        Ok(cosmon_filestore::event::VerifyOutcome::UnsignedLegacy { count }) => {
            checks.push(Check {
                category: "event-chain",
                name: "events.jsonl".to_owned(),
                status: CheckStatus::Skip,
                detail: format!("{count} legacy unsigned events (pre-v2)"),
            });
        }
        Ok(cosmon_filestore::event::VerifyOutcome::Diverged(d)) => checks.push(Check {
            category: "event-chain",
            name: "events.jsonl".to_owned(),
            status: CheckStatus::Fail,
            detail: format!("diverged at index {}: {:?}", d.index, d.reason),
        }),
        Err(e) => checks.push(Check {
            category: "event-chain",
            name: "events.jsonl".to_owned(),
            status: CheckStatus::Fail,
            detail: format!("chain walk error: {e}"),
        }),
    }
}

/// Fleet-wide federation provenance audit (ADR-105 / I9' machinery).
///
/// Walks the fleet-wide `events.jsonl`, classifies each cross-galaxy
/// event, and reports those whose `federation_provenance` is `None`.
/// Exits the process with status 1 on any FAIL — `cs verify
/// --federation` is intended to gate the chronicle-lint formula and
/// CI sweeps, so a non-zero exit on missing provenance is the
/// load-bearing semantic.
///
/// "Cross-galaxy" is detected from the `molecule_id` carrying a `/`
/// separator (the ADR-105 §D3 Oracle B'' convention: when cosmon
/// imports a sister-galaxy molecule, the local `cs delegate` writes
/// `<galaxy>/<sister_mol_id>` as the local `mol_id`). The branch name
/// is also scanned for the Oracle B subject-mark, where the prefix
/// signals a federation delegation.
fn run_fleet_audit(
    ctx: &Context,
    store: &dyn StateStore,
    state_dir: &Path,
    args: &Args,
) -> anyhow::Result<()> {
    let events_path = cosmon_state::event_log::resolve_events_log_path(state_dir);
    let mut checks: Vec<Check> = Vec::new();

    if args.federation {
        verify_federation_provenance(
            &events_path,
            args.legacy_tolerate_before.as_deref(),
            &mut checks,
        );
    }

    // `archived ⇒ status.is_terminal()` over every molecule in the fleet.
    if args.invariants {
        let molecules = store.list_molecules(&cosmon_state::MoleculeFilter::default())?;
        verify_archived_terminal_fleet(&molecules, &mut checks);
    }

    let any_fail = checks.iter().any(|c| c.status == CheckStatus::Fail);

    // A scope label that reflects which audits actually ran.
    let scope = match (args.federation, args.invariants) {
        (true, true) => "federation+invariants",
        (true, false) => "federation",
        (false, true) => "invariants",
        (false, false) => "fleet",
    };

    if ctx.json {
        let rows: Vec<serde_json::Value> = checks
            .iter()
            .map(|c| {
                serde_json::json!({
                    "category": c.category,
                    "name": c.name,
                    "status": c.status.label(),
                    "detail": c.detail,
                })
            })
            .collect();
        let out = serde_json::json!({
            "scope": scope,
            "events_log": events_path.display().to_string(),
            "status": if any_fail { "fail" } else { "pass" },
            "checks": rows,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        for c in &checks {
            println!("[{}] {}: {}", c.status.label(), c.category, c.name);
            if !c.detail.is_empty() {
                println!("      {}", c.detail);
            }
        }
        println!();
        if any_fail {
            println!("verify --{scope}: FAIL");
        } else {
            println!("verify --{scope}: PASS");
        }
    }

    if any_fail {
        std::process::exit(1);
    }
    Ok(())
}

/// Check `archived ⇒ status.is_terminal()` for a single molecule.
///
/// A molecule that is archived but carries a non-terminal status
/// (`Pending`/`Queued`/`Running`/`Frozen`/`Starved`) is a *ghost* — it
/// was archived/merged out-of-band without terminalizing its status and
/// therefore keeps surfacing as live work. The check is detect-only and
/// emits a FAIL row; no mutation happens here (`cs reconcile
/// --heal-invariants` is the healing path).
fn verify_archived_terminal(mol: &MoleculeData, checks: &mut Vec<Check>) {
    if mol.archived && !mol.status.is_terminal() {
        checks.push(Check {
            category: "invariant",
            name: format!("archived-terminal — {}", mol.id.as_str()),
            status: CheckStatus::Fail,
            detail: format!(
                "archived molecule has non-terminal status `{}` \
                 (expected completed/collapsed) — heal with \
                 `cs reconcile --heal-invariants`",
                mol.status
            ),
        });
    } else {
        checks.push(Check {
            category: "invariant",
            name: format!("archived-terminal — {}", mol.id.as_str()),
            status: CheckStatus::Pass,
            detail: if mol.archived {
                format!("archived ∧ terminal (`{}`)", mol.status)
            } else {
                "not archived (invariant vacuously holds)".to_owned()
            },
        });
    }
}

/// Sweep every molecule for the `archived ⇒ status.is_terminal()`
/// invariant and append one FAIL row per violation plus a summary row.
///
/// Only violations are reported individually — emitting a PASS row for
/// every healthy molecule in a large galaxy would bury the signal. The
/// summary row carries the falsification signal even when zero
/// violations exist (so a clean galaxy still shows `scanned: N`).
fn verify_archived_terminal_fleet(molecules: &[MoleculeData], checks: &mut Vec<Check>) {
    let mut violations = 0_usize;
    for mol in molecules {
        if mol.archived && !mol.status.is_terminal() {
            violations += 1;
            checks.push(Check {
                category: "invariant",
                name: format!("archived-terminal — {}", mol.id.as_str()),
                status: CheckStatus::Fail,
                detail: format!(
                    "archived molecule has non-terminal status `{}` \
                     (expected completed/collapsed)",
                    mol.status
                ),
            });
        }
    }
    checks.push(Check {
        category: "invariant",
        name: "summary".to_owned(),
        status: if violations == 0 {
            CheckStatus::Pass
        } else {
            CheckStatus::Fail
        },
        detail: format!(
            "archived⇒terminal: {} molecule(s) scanned, {violations} violation(s)",
            molecules.len()
        ),
    });
}

/// Walk an `events.jsonl` file and emit one check row per cross-galaxy
/// event. Cross-galaxy events without a `federation_provenance` are
/// FAIL (or SKIP under `--legacy-tolerate-before`).
///
/// Detection rule — an event is "cross-galaxy" when the `molecule_id`
/// contains a `/` (ADR-105 §D3 Oracle B'' convention:
/// `<galaxy>/<sister_mol_id>`), OR the branch name has a `<galaxy>/`
/// prefix outside the standard `feat/` / `fix/` / `refactor/` set
/// (Oracle B subject-mark). The rule is intentionally loose — false
/// positives are easier to triage than silent passes.
#[allow(clippy::too_many_lines)]
fn verify_federation_provenance(
    events_path: &Path,
    legacy_tolerate_before: Option<&str>,
    checks: &mut Vec<Check>,
) {
    if !events_path.exists() {
        checks.push(Check {
            category: "federation",
            name: "events.jsonl".to_owned(),
            status: CheckStatus::Skip,
            detail: format!(
                "event log not found at {} (no cross-galaxy events to audit)",
                events_path.display()
            ),
        });
        return;
    }

    let cutoff: Option<chrono::DateTime<chrono::Utc>> = legacy_tolerate_before.and_then(|s| {
        let parsed = if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
            d.and_hms_opt(0, 0, 0).map(|nd| {
                chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(nd, chrono::Utc)
            })
        } else {
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|d| d.with_timezone(&chrono::Utc))
        };
        if parsed.is_none() {
            checks.push(Check {
                category: "federation",
                name: "args".to_owned(),
                status: CheckStatus::Fail,
                detail: format!("could not parse --legacy-tolerate-before {s:?}"),
            });
        }
        parsed
    });

    let contents = match std::fs::read_to_string(events_path) {
        Ok(s) => s,
        Err(e) => {
            checks.push(Check {
                category: "federation",
                name: "events.jsonl".to_owned(),
                status: CheckStatus::Fail,
                detail: format!("could not read {}: {e}", events_path.display()),
            });
            return;
        }
    };

    let mut cross_galaxy_seen = 0_usize;
    let mut fail_count = 0_usize;
    let mut tolerated_count = 0_usize;

    for (line_no, raw) in contents.lines().enumerate() {
        if raw.trim().is_empty() {
            continue;
        }
        let Ok(env) = cosmon_core::event_v2::Envelope::from_line(raw) else {
            // legacy / malformed lines: out of scope here
            continue;
        };

        // Tuple shape: (display_id, secondary_context, provenance, is_cross).
        // `display_id` is the molecule id / ADR number / chronicle path for
        // the audit row; `secondary_context` is the branch / cites string
        // used for the cross-galaxy classifier. The classifier rule differs
        // per variant; the cross-galaxy bit is computed here and passed
        // through so the inner branch stays uniform.
        let (display_id, secondary_context, provenance, is_cross) = match &env.event {
            EventV2::MergeDispatched {
                molecule,
                branch,
                federation_provenance,
            }
            | EventV2::MergeCompleted {
                molecule,
                branch,
                federation_provenance,
                ..
            } => {
                let mol_id_str = molecule.as_str().to_owned();
                let cross = is_cross_galaxy(&mol_id_str, branch);
                (
                    mol_id_str,
                    branch.clone(),
                    federation_provenance.as_ref(),
                    cross,
                )
            }
            EventV2::ChronicleAdded {
                molecule_id,
                chronicle_path,
                cites_galaxies,
                federation_provenance,
                ..
            } => {
                let display = molecule_id
                    .as_ref()
                    .map_or_else(|| chronicle_path.clone(), |m| m.as_str().to_owned());
                let cross = is_cross_galaxy_doc(cites_galaxies);
                (
                    display,
                    chronicle_path.clone(),
                    federation_provenance.as_ref(),
                    cross,
                )
            }
            EventV2::AdrInscribed {
                adr_number,
                adr_path,
                cites_galaxies,
                federation_provenance,
                ..
            } => {
                let cross = is_cross_galaxy_doc(cites_galaxies);
                (
                    format!("ADR-{adr_number:03}"),
                    adr_path.clone(),
                    federation_provenance.as_ref(),
                    cross,
                )
            }
            _ => continue,
        };

        if !is_cross {
            continue;
        }

        let mol_id_str = display_id;
        let branch_str = secondary_context;

        cross_galaxy_seen += 1;
        let name = format!(
            "line {} — {} ({})",
            line_no + 1,
            mol_id_str,
            short_event_type(&env.event)
        );

        if provenance.is_some() {
            checks.push(Check {
                category: "federation",
                name,
                status: CheckStatus::Pass,
                detail: "federation_provenance present".to_owned(),
            });
            continue;
        }

        // Provenance missing — apply legacy-tolerate cutoff if any.
        if let Some(c) = cutoff {
            if env.timestamp < c {
                tolerated_count += 1;
                checks.push(Check {
                    category: "federation",
                    name,
                    status: CheckStatus::Skip,
                    detail: format!(
                        "missing federation_provenance — legacy-tolerated (event at {} < {})",
                        env.timestamp.to_rfc3339(),
                        c.to_rfc3339()
                    ),
                });
                continue;
            }
        }

        fail_count += 1;
        checks.push(Check {
            category: "federation",
            name,
            status: CheckStatus::Fail,
            detail: format!(
                "missing federation_provenance on cross-galaxy event \
                 (molecule={mol_id_str}, branch={branch_str}, ts={})",
                env.timestamp.to_rfc3339(),
            ),
        });
    }

    // Summary row so the audit has a falsification signal even when
    // there are zero cross-galaxy events.
    let summary_detail = format!(
        "cross-galaxy events scanned: {cross_galaxy_seen}; \
         missing provenance: {fail_count}; \
         legacy-tolerated: {tolerated_count}"
    );
    checks.push(Check {
        category: "federation",
        name: "summary".to_owned(),
        status: if fail_count == 0 {
            CheckStatus::Pass
        } else {
            CheckStatus::Fail
        },
        detail: summary_detail,
    });
}

/// Heuristic test for the cross-galaxy classifier.
///
/// `Mol-id` contains a `/` separator → ADR-105 §D3 Oracle B'' convention
/// (cosmon's local `mol_id` for an imported sister molecule). Branch
/// also carries a `<galaxy>/...` prefix outside the standard
/// `feat/` / `fix/` / `refactor/` / `chore/` / `docs/` / `test/` set
/// → Oracle B subject-mark.
fn is_cross_galaxy(mol_id: &str, branch: &str) -> bool {
    if mol_id.contains('/') {
        return true;
    }
    if let Some((prefix, _rest)) = branch.split_once('/') {
        let standard = matches!(
            prefix,
            "feat" | "fix" | "refactor" | "chore" | "docs" | "test" | "ci" | "build" | "perf"
        );
        if !standard && !prefix.is_empty() {
            return true;
        }
    }
    false
}

/// Compact discriminator string for the audit row (avoids the full
/// `EventV2::MergeDispatched { ... }` debug shape).
fn short_event_type(event: &EventV2) -> &'static str {
    match event {
        EventV2::MergeDispatched { .. } => "merge_dispatched",
        EventV2::MergeCompleted { .. } => "merge_completed",
        EventV2::ChronicleAdded { .. } => "chronicle_added",
        EventV2::AdrInscribed { .. } => "adr_inscribed",
        _ => "other",
    }
}

/// Cross-galaxy classifier for documentation events
/// ([`EventV2::ChronicleAdded`] and [`EventV2::AdrInscribed`]).
///
/// An entry is cross-galaxy iff its `cites_galaxies` list mentions at
/// least one alias that is not `"cosmon"`. Empty list → purely local;
/// `vec!["cosmon"]` → purely local (the self-reference is benign).
/// `vec!["smithy"]` or `vec!["cosmon", "mailroom"]` → cross-galaxy.
///
/// ADR-105 §D5 — the lint must be heuristic-driven on the
/// `cites_galaxies` field rather than re-parse the chronicle text. The
/// emitter is the right place to attribute the citation; the verifier
/// is the right place to demand the lineage.
fn is_cross_galaxy_doc(cites_galaxies: &[String]) -> bool {
    cites_galaxies.iter().any(|g| g != "cosmon")
}

/// Render the markdown verify report.
fn render_report(molecule_id: &str, checks: &[Check]) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "# Verify Report — {molecule_id}\n");
    let _ = writeln!(s, "Generated: {}\n", chrono::Utc::now().to_rfc3339());

    let fail = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Fail)
        .count();
    let pass = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Pass)
        .count();
    let skip = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Skip)
        .count();
    let _ = writeln!(s, "**Summary:** {pass} PASS, {fail} FAIL, {skip} SKIP\n");
    s.push_str(if fail == 0 {
        "**Status:** ALL CHECKS PASSED\n\n"
    } else {
        "**Status:** VERIFICATION FAILED\n\n"
    });

    s.push_str("| Status | Category | Name | Detail |\n");
    s.push_str("|--------|----------|------|--------|\n");
    for c in checks {
        let _ = writeln!(
            s,
            "| {} | {} | `{}` | {} |",
            c.status.label(),
            c.category,
            c.name,
            c.detail.replace('|', "\\|"),
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn artifact_check_passes_when_manifest_matches() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("synthesis.md"), "result").unwrap();
        pow::seal(tmp.path(), "m1", "task-work").unwrap();
        let mut checks = Vec::new();
        verify_artifacts(tmp.path(), &mut checks);
        assert!(checks.iter().all(|c| c.status != CheckStatus::Fail));
        assert!(checks
            .iter()
            .any(|c| c.name == "synthesis.md" && c.status == CheckStatus::Pass));
    }

    #[test]
    fn artifact_check_fails_after_tamper() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("synthesis.md"), "original").unwrap();
        pow::seal(tmp.path(), "m1", "task-work").unwrap();
        std::fs::write(tmp.path().join("synthesis.md"), "tampered").unwrap();
        let mut checks = Vec::new();
        verify_artifacts(tmp.path(), &mut checks);
        assert!(checks
            .iter()
            .any(|c| c.status == CheckStatus::Fail && c.name == "synthesis.md"));
    }

    #[test]
    fn missing_manifest_is_skip_not_fail() {
        let tmp = TempDir::new().unwrap();
        let mut checks = Vec::new();
        verify_artifacts(tmp.path(), &mut checks);
        assert!(checks.iter().all(|c| c.status != CheckStatus::Fail));
    }

    #[test]
    fn report_renders_table() {
        let checks = vec![Check {
            category: "artifact",
            name: "synthesis.md".to_owned(),
            status: CheckStatus::Pass,
            detail: "ok".to_owned(),
        }];
        let s = render_report("task-1", &checks);
        assert!(s.contains("task-1"));
        assert!(s.contains("PASS"));
    }

    // ─── Soft-contract briefing seals ────────────────────────────────────

    fn mol_fixture() -> MoleculeData {
        use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
        use std::collections::{BTreeSet, HashMap};
        MoleculeData {
            id: MoleculeId::new("task-20260417-seal").unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status: cosmon_core::molecule::MoleculeStatus::Pending,
            variables: HashMap::new(),
            assigned_worker: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            total_steps: 2,
            current_step: 0,
            completed_steps: Vec::new(),
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: Vec::new(),
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: BTreeSet::new(),
            escalations: Vec::new(),
            freeze_on_last_step: false,
            expires_at: None,
            expiry_policy: None,
            originating_branch: None,
            pending_step: None,
            merged_at: None,
            prompt_seal: None,
            briefing_seals: Vec::new(),
            bootstrap_seals: Vec::new(),
            archived: false,
            last_progress_at: None,
            last_output_at: None,
            nudge_count: 0,
            last_nudged_at: None,
            propel_count: 0,
            last_propelled_at: None,
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
            adapter: None,
        }
    }

    #[test]
    fn seal_check_passes_when_prompt_matches() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("prompt.md"), "operator intent").unwrap();
        let mut mol = mol_fixture();
        mol.prompt_seal = Some(BriefingSeal::of_bytes(0, b"operator intent"));
        let mut checks = Vec::new();
        verify_briefing_seals(tmp.path(), &mol, None, &mut checks);
        assert!(checks
            .iter()
            .any(|c| c.name == "prompt.md" && c.status == CheckStatus::Pass));
    }

    #[test]
    fn seal_check_detects_prompt_tamper() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("prompt.md"), "tampered").unwrap();
        let mut mol = mol_fixture();
        mol.prompt_seal = Some(BriefingSeal::of_bytes(0, b"original"));
        let mut checks = Vec::new();
        verify_briefing_seals(tmp.path(), &mol, None, &mut checks);
        assert!(checks
            .iter()
            .any(|c| c.name == "prompt.md" && c.status == CheckStatus::Fail));
    }

    #[test]
    fn seal_check_skips_when_no_seal_recorded() {
        let tmp = TempDir::new().unwrap();
        // Legacy molecule: no prompt_seal, no briefing_seals.
        let mol = mol_fixture();
        let mut checks = Vec::new();
        verify_briefing_seals(tmp.path(), &mol, None, &mut checks);
        // All seal checks should be SKIP, never FAIL — legacy
        // molecules are inconclusive, not failures.
        assert!(checks
            .iter()
            .filter(|c| c.category == "seal")
            .all(|c| c.status == CheckStatus::Skip));
    }

    #[test]
    fn seal_check_uses_latest_briefing_seal_by_default() {
        let tmp = TempDir::new().unwrap();
        // The live briefing.md is what `cs complete` last wrote — it
        // deliberately DIFFERS from every sealed step's snapshot, exactly
        // as it does in production. The snapshot-based verify must not
        // read that honest rewrite as tampering.
        std::fs::write(
            tmp.path().join("briefing.md"),
            b"briefing rewritten by cs complete",
        )
        .unwrap();
        let mut mol = mol_fixture();
        mol.briefing_seals.push(
            BriefingSeal::of_text(0, "briefing at step 0").with_snapshot("briefing at step 0"),
        );
        mol.briefing_seals.push(
            BriefingSeal::of_text(1, "briefing at step 1").with_snapshot("briefing at step 1"),
        );
        mol.briefing_seals.push(
            BriefingSeal::of_text(2, "briefing at step 2").with_snapshot("briefing at step 2"),
        );
        let mut checks = Vec::new();
        verify_briefing_seals(tmp.path(), &mol, None, &mut checks);
        let pass_row = checks
            .iter()
            .find(|c| c.category == "seal" && c.name.contains("briefing.md@step2"))
            .expect("latest briefing seal check present");
        assert_eq!(
            pass_row.status,
            CheckStatus::Pass,
            "the step snapshot must verify even though the live file was honestly rewritten: {}",
            pass_row.detail
        );
    }

    #[test]
    fn seal_check_honours_step_filter() {
        let tmp = TempDir::new().unwrap();
        // The live file is irrelevant now — verification is against the
        // per-step snapshot. The --step filter must select step 1's seal.
        std::fs::write(tmp.path().join("briefing.md"), b"latest").unwrap();
        let mut mol = mol_fixture();
        mol.briefing_seals
            .push(BriefingSeal::of_text(0, "step 0").with_snapshot("step 0"));
        mol.briefing_seals
            .push(BriefingSeal::of_text(1, "step 1").with_snapshot("step 1"));
        let mut checks = Vec::new();
        verify_briefing_seals(tmp.path(), &mol, Some(1), &mut checks);
        assert!(
            checks
                .iter()
                .any(|c| c.name.contains("briefing.md@step1") && c.status == CheckStatus::Pass),
            "the --step filter selects step 1's seal, which passes against its own snapshot"
        );
    }

    #[test]
    fn briefing_snapshot_passes_when_live_file_differs() {
        // The flagship fix: a multi-step molecule whose live briefing.md
        // has been legitimately rewritten (per-step re-briefing +
        // `cs complete`) verifies CLEAN against the sealed snapshot.
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("briefing.md"), b"# COMPLETED\n").unwrap();
        let mut mol = mol_fixture();
        mol.briefing_seals.push(
            BriefingSeal::of_text(1, "# Step 2 briefing\n\nDo the thing.\n")
                .with_snapshot("# Step 2 briefing\n\nDo the thing.\n"),
        );
        let mut checks = Vec::new();
        verify_briefing_seals(tmp.path(), &mol, None, &mut checks);
        assert!(
            checks
                .iter()
                .any(|c| c.name.contains("briefing.md@step1") && c.status == CheckStatus::Pass),
            "snapshot verify passes despite the live file differing"
        );
    }

    #[test]
    fn briefing_snapshot_tamper_is_detected() {
        // Genuine tamper-evidence is preserved: mutating the recorded
        // snapshot (without recomputing the hash) is caught as a FAIL.
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("briefing.md"), b"anything\n").unwrap();
        let mut mol = mol_fixture();
        // hash is over the honest content; snapshot has been swapped.
        mol.briefing_seals.push(
            BriefingSeal::of_text(1, "honest contract\n").with_snapshot("tampered contract\n"),
        );
        let mut checks = Vec::new();
        verify_briefing_seals(tmp.path(), &mol, None, &mut checks);
        assert!(
            checks
                .iter()
                .any(|c| c.name.contains("briefing.md@step1") && c.status == CheckStatus::Fail),
            "a mutated snapshot must FAIL"
        );
    }

    #[test]
    fn legacy_briefing_seal_without_snapshot_is_skip_not_fail() {
        // A pre-fix molecule has no snapshot and cosmon rewrites
        // briefing.md per step, so the live file always differs. That is
        // inconclusive (SKIP), never a false FAIL.
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("briefing.md"), b"current briefing\n").unwrap();
        let mut mol = mol_fixture();
        mol.briefing_seals
            .push(BriefingSeal::of_text(1, "old step-1 briefing\n")); // no snapshot
        let mut checks = Vec::new();
        verify_briefing_seals(tmp.path(), &mol, None, &mut checks);
        let row = checks
            .iter()
            .find(|c| c.name.contains("briefing.md@step1"))
            .expect("briefing seal check present");
        assert_eq!(
            row.status,
            CheckStatus::Skip,
            "legacy snapshot-less seal is inconclusive, not a failure: {}",
            row.detail
        );
    }

    #[test]
    fn bootstrap_snapshot_passes_and_tamper_is_detected() {
        // Ambient CLAUDE.md drift is invisible (snapshot verifies clean),
        // but a mutated snapshot is still caught.
        let tmp = TempDir::new().unwrap();
        let mut mol = mol_fixture();
        mol.bootstrap_seals.push(
            BriefingSeal::of_text(1, "<bootstrap_context>ambient</bootstrap_context>")
                .with_snapshot("<bootstrap_context>ambient</bootstrap_context>"),
        );
        let mut checks = Vec::new();
        verify_briefing_seals(tmp.path(), &mol, None, &mut checks);
        assert!(
            checks
                .iter()
                .any(|c| c.name.contains("bootstrap@step1") && c.status == CheckStatus::Pass),
            "bootstrap snapshot verifies clean regardless of the live cwd walk"
        );

        let mut tampered = mol_fixture();
        tampered.bootstrap_seals.push(
            BriefingSeal::of_text(1, "<bootstrap_context>ambient</bootstrap_context>")
                .with_snapshot("<bootstrap_context>SWAPPED</bootstrap_context>"),
        );
        let mut checks2 = Vec::new();
        verify_briefing_seals(tmp.path(), &tampered, None, &mut checks2);
        assert!(
            checks2
                .iter()
                .any(|c| c.name.contains("bootstrap@step1") && c.status == CheckStatus::Fail),
            "a mutated bootstrap snapshot must FAIL"
        );
    }

    #[test]
    fn seal_check_step_filter_with_no_matching_seal_is_skip() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("briefing.md"), b"whatever").unwrap();
        let mut mol = mol_fixture();
        mol.briefing_seals
            .push(BriefingSeal::of_bytes(0, b"step 0"));
        let mut checks = Vec::new();
        // Ask for step 7 — no seal recorded, not a failure.
        verify_briefing_seals(tmp.path(), &mol, Some(7), &mut checks);
        let briefing_check = checks
            .iter()
            .find(|c| c.category == "seal" && c.name.contains("briefing"))
            .expect("briefing seal check present");
        assert_eq!(briefing_check.status, CheckStatus::Skip);
        assert!(briefing_check.detail.contains("step 7"));
    }

    // ─── archived ⇒ status.is_terminal() invariant ──────────────────────

    #[test]
    fn invariant_archived_running_is_fail() {
        let mut mol = mol_fixture();
        mol.archived = true;
        mol.status = cosmon_core::molecule::MoleculeStatus::Running;
        let mut checks = Vec::new();
        verify_archived_terminal(&mol, &mut checks);
        let row = checks
            .iter()
            .find(|c| c.category == "invariant")
            .expect("invariant row present");
        assert_eq!(row.status, CheckStatus::Fail);
        assert!(row.detail.contains("non-terminal"));
        assert!(row.detail.contains("heal"));
    }

    #[test]
    fn invariant_archived_collapsed_is_pass() {
        let mut mol = mol_fixture();
        mol.archived = true;
        mol.status = cosmon_core::molecule::MoleculeStatus::Collapsed;
        let mut checks = Vec::new();
        verify_archived_terminal(&mol, &mut checks);
        let row = checks
            .iter()
            .find(|c| c.category == "invariant")
            .expect("invariant row present");
        assert_eq!(row.status, CheckStatus::Pass);
    }

    #[test]
    fn invariant_not_archived_is_pass_vacuously() {
        let mut mol = mol_fixture();
        mol.archived = false;
        mol.status = cosmon_core::molecule::MoleculeStatus::Running;
        let mut checks = Vec::new();
        verify_archived_terminal(&mol, &mut checks);
        let row = checks
            .iter()
            .find(|c| c.category == "invariant")
            .expect("invariant row present");
        assert_eq!(row.status, CheckStatus::Pass);
        assert!(row.detail.contains("not archived"));
    }

    #[test]
    fn invariant_fleet_reports_violations_and_summary() {
        use cosmon_core::id::MoleculeId;
        use cosmon_core::molecule::MoleculeStatus;
        let mut healthy = mol_fixture();
        healthy.id = MoleculeId::new("task-20260618-aaaa").unwrap();
        healthy.archived = true;
        healthy.status = MoleculeStatus::Completed;

        let mut ghost = mol_fixture();
        ghost.id = MoleculeId::new("task-20260618-bbbb").unwrap();
        ghost.archived = true;
        ghost.status = MoleculeStatus::Running;

        let mut alive = mol_fixture();
        alive.id = MoleculeId::new("task-20260618-cccc").unwrap();
        alive.archived = false;
        alive.status = MoleculeStatus::Running;

        let mols = vec![healthy, ghost, alive];
        let mut checks = Vec::new();
        verify_archived_terminal_fleet(&mols, &mut checks);

        // Exactly one FAIL row for the ghost.
        let fails: Vec<_> = checks
            .iter()
            .filter(|c| c.status == CheckStatus::Fail && c.name.contains("task-20260618-bbbb"))
            .collect();
        assert_eq!(fails.len(), 1);
        // No FAIL row for the healthy archived or the live non-archived rows.
        assert!(!checks
            .iter()
            .any(|c| c.status == CheckStatus::Fail && c.name.contains("task-20260618-aaaa")));
        assert!(!checks
            .iter()
            .any(|c| c.status == CheckStatus::Fail && c.name.contains("task-20260618-cccc")));
        // Summary is FAIL and counts the 3 scanned + 1 violation.
        let summary = checks
            .iter()
            .find(|c| c.category == "invariant" && c.name == "summary")
            .expect("summary present");
        assert_eq!(summary.status, CheckStatus::Fail);
        assert!(summary.detail.contains("3 molecule(s) scanned"));
        assert!(summary.detail.contains("1 violation"));
    }

    #[test]
    fn invariant_fleet_clean_galaxy_summary_pass() {
        use cosmon_core::id::MoleculeId;
        use cosmon_core::molecule::MoleculeStatus;
        let mut a = mol_fixture();
        a.id = MoleculeId::new("task-20260618-aaaa").unwrap();
        a.archived = true;
        a.status = MoleculeStatus::Collapsed;
        let mols = vec![a];
        let mut checks = Vec::new();
        verify_archived_terminal_fleet(&mols, &mut checks);
        let summary = checks
            .iter()
            .find(|c| c.category == "invariant" && c.name == "summary")
            .expect("summary present");
        assert_eq!(summary.status, CheckStatus::Pass);
        assert!(summary.detail.contains("0 violation"));
        assert!(!checks.iter().any(|c| c.status == CheckStatus::Fail));
    }

    // ─── Federation provenance (ADR-105 / I9' machinery) ────────────────

    use cosmon_core::event_v2::{Envelope, MergeResult, Seq};
    use cosmon_core::federation::FederationLineage;
    use cosmon_core::id::MoleculeId;

    fn write_event(path: &Path, env: &Envelope) {
        use std::fs::OpenOptions;
        use std::io::Write;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        let line = serde_json::to_string(env).unwrap();
        writeln!(f, "{line}").unwrap();
    }

    fn lineage_fixture() -> FederationLineage {
        FederationLineage {
            source_galaxy: "smithy".to_owned(),
            source_commit: "195ff5aa".to_owned(),
            source_path: std::path::PathBuf::from("docs/adr/0042.md"),
            crossed_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn cross_galaxy_detection_mol_id_with_slash() {
        // The federation convention (ADR-105 §D3 Oracle B'') stores
        // the foreign alias in the `source_galaxy` field, not in the
        // MoleculeId itself (MoleculeId validates against a strict
        // `PREFIX-YYYYMMDD-XXXX` shape). The classifier still accepts
        // a `/`-bearing mol_id string as a cross-galaxy mark, because
        // some emit paths may serialise the foreign id verbatim when
        // they bypass the typed newtype.
        assert!(is_cross_galaxy(
            "smithy/task-20260513-3a9e",
            "feat/anything"
        ));
        assert!(is_cross_galaxy("mailroom/idea-20260518-aaaa", "anything"));
    }

    #[test]
    fn cross_galaxy_detection_branch_with_galaxy_prefix() {
        assert!(is_cross_galaxy("task-20260513-3a9e", "smithy/something"));
        // Standard prefixes are NOT cross-galaxy:
        assert!(!is_cross_galaxy("task-x", "feat/task-x"));
        assert!(!is_cross_galaxy("task-x", "fix/bug"));
        assert!(!is_cross_galaxy("task-x", "refactor/foo"));
        assert!(!is_cross_galaxy("task-x", "chore/bump"));
    }

    #[test]
    fn cross_galaxy_detection_purely_local_is_false() {
        assert!(!is_cross_galaxy("task-20260519-013e", "feat/task-x"));
        assert!(!is_cross_galaxy("idea-aaaa", "test/foo"));
    }

    #[test]
    fn federation_check_passes_when_no_events_log() {
        let tmp = TempDir::new().unwrap();
        let mut checks = Vec::new();
        // Missing file: SKIP not FAIL.
        verify_federation_provenance(&tmp.path().join("missing.jsonl"), None, &mut checks);
        assert!(checks
            .iter()
            .any(|c| c.category == "federation" && c.status == CheckStatus::Skip));
        assert!(!checks
            .iter()
            .any(|c| c.category == "federation" && c.status == CheckStatus::Fail));
    }

    #[test]
    fn federation_check_passes_when_no_cross_galaxy_events() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("events.jsonl");
        let env = Envelope::new(
            Seq(0),
            None,
            EventV2::MergeCompleted {
                molecule: MoleculeId::new("task-20260519-aaaa").unwrap(),
                branch: "feat/task-20260519-aaaa".to_owned(),
                result: MergeResult::Ok,
                federation_provenance: None,
            },
        );
        write_event(&path, &env);

        let mut checks = Vec::new();
        verify_federation_provenance(&path, None, &mut checks);
        // No cross-galaxy events seen → PASS summary, no FAIL rows.
        assert!(!checks
            .iter()
            .any(|c| c.category == "federation" && c.status == CheckStatus::Fail));
        let summary = checks
            .iter()
            .find(|c| c.category == "federation" && c.name == "summary")
            .expect("summary row present");
        assert_eq!(summary.status, CheckStatus::Pass);
        assert!(summary.detail.contains("scanned: 0"));
    }

    #[test]
    fn federation_check_fails_on_cross_galaxy_event_missing_provenance() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("events.jsonl");
        // Cross-galaxy detection here is via the non-standard branch
        // prefix `smithy/...` — Oracle B subject-mark per ADR-105
        // §D3. The local MoleculeId stays canonical.
        let env = Envelope::new(
            Seq(0),
            None,
            EventV2::MergeDispatched {
                molecule: MoleculeId::new("task-20260513-3a9e").unwrap(),
                branch: "smithy/rpp-binding".to_owned(),
                federation_provenance: None,
            },
        );
        write_event(&path, &env);

        let mut checks = Vec::new();
        verify_federation_provenance(&path, None, &mut checks);
        let fail = checks
            .iter()
            .find(|c| c.status == CheckStatus::Fail && c.category == "federation")
            .expect("cross-galaxy missing provenance must FAIL");
        // The FAIL row name carries the molecule id so the operator can
        // act on it without grepping logs.
        assert!(fail.name.contains("task-20260513-3a9e"));
        assert!(fail.detail.contains("missing federation_provenance"));
    }

    #[test]
    fn federation_check_passes_with_valid_provenance() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("events.jsonl");
        let env = Envelope::new(
            Seq(0),
            None,
            EventV2::MergeDispatched {
                molecule: MoleculeId::new("task-20260513-3a9e").unwrap(),
                branch: "smithy/rpp-binding".to_owned(),
                federation_provenance: Some(lineage_fixture()),
            },
        );
        write_event(&path, &env);

        let mut checks = Vec::new();
        verify_federation_provenance(&path, None, &mut checks);
        let pass = checks
            .iter()
            .find(|c| {
                c.status == CheckStatus::Pass
                    && c.category == "federation"
                    && c.name.contains("task-20260513-3a9e")
            })
            .expect("cross-galaxy with provenance must PASS");
        assert!(pass.detail.contains("present"));
        assert!(!checks
            .iter()
            .any(|c| c.status == CheckStatus::Fail && c.category == "federation"));
    }

    #[test]
    fn federation_check_legacy_tolerate_downgrades_to_skip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("events.jsonl");
        // Craft an envelope with a backdated timestamp via direct
        // serialization (Envelope::new stamps Utc::now()).
        let mol = MoleculeId::new("task-20260513-3a9e").unwrap();
        let env = Envelope {
            seq: Seq(0),
            mol_seq: None,
            timestamp: chrono::DateTime::<chrono::Utc>::from_timestamp(
                chrono::NaiveDate::from_ymd_opt(2026, 5, 1)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap()
                    .and_utc()
                    .timestamp(),
                0,
            )
            .unwrap(),
            causal_parent: None,
            quality_band: None,
            emitter_kind: cosmon_core::event_v2::EmitterKind::Unknown,
            emitter_id: String::new(),
            meta_level: 0,
            event: EventV2::MergeDispatched {
                molecule: mol,
                branch: "smithy/x".to_owned(),
                federation_provenance: None,
            },
        };
        write_event(&path, &env);

        let mut checks = Vec::new();
        verify_federation_provenance(&path, Some("2026-05-19"), &mut checks);
        // Legacy-tolerated → SKIP, not FAIL.
        assert!(checks.iter().any(|c| c.status == CheckStatus::Skip
            && c.category == "federation"
            && c.detail.contains("legacy-tolerated")));
        // Summary should still report 0 failures.
        let summary = checks
            .iter()
            .find(|c| c.name == "summary" && c.category == "federation")
            .unwrap();
        assert_eq!(summary.status, CheckStatus::Pass);
        assert!(summary.detail.contains("legacy-tolerated: 1"));
    }

    // ─── ChronicleAdded / AdrInscribed (W7 — ADR-105 machinery) ──────────

    #[test]
    fn doc_cross_galaxy_classifier_empty_is_local() {
        assert!(!is_cross_galaxy_doc(&[]));
    }

    #[test]
    fn doc_cross_galaxy_classifier_cosmon_only_is_local() {
        assert!(!is_cross_galaxy_doc(&["cosmon".to_owned()]));
    }

    #[test]
    fn doc_cross_galaxy_classifier_smithy_is_cross() {
        assert!(is_cross_galaxy_doc(&["smithy".to_owned()]));
    }

    #[test]
    fn doc_cross_galaxy_classifier_mixed_is_cross() {
        assert!(is_cross_galaxy_doc(&[
            "cosmon".to_owned(),
            "mailroom".to_owned(),
        ]));
    }

    #[test]
    fn federation_check_fails_on_chronicle_added_cross_galaxy_missing_provenance() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("events.jsonl");
        let env = Envelope::new(
            Seq(0),
            None,
            EventV2::ChronicleAdded {
                molecule_id: Some(MoleculeId::new("task-20260519-aaaa").unwrap()),
                chronicle_path: "docs/lore/CHRONICLES.md".to_owned(),
                entry_anchor: Some("merger".to_owned()),
                cites_galaxies: vec!["smithy".to_owned()],
                federation_provenance: None,
            },
        );
        write_event(&path, &env);

        let mut checks = Vec::new();
        verify_federation_provenance(&path, None, &mut checks);
        let fail = checks
            .iter()
            .find(|c| c.status == CheckStatus::Fail && c.category == "federation")
            .expect("cross-galaxy chronicle without provenance must FAIL");
        assert!(fail.name.contains("chronicle_added"));
        assert!(fail.detail.contains("missing federation_provenance"));
    }

    #[test]
    fn federation_check_passes_on_chronicle_added_with_provenance() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("events.jsonl");
        let env = Envelope::new(
            Seq(0),
            None,
            EventV2::ChronicleAdded {
                molecule_id: Some(MoleculeId::new("task-20260519-aaaa").unwrap()),
                chronicle_path: "docs/lore/CHRONICLES.md".to_owned(),
                entry_anchor: Some("merger".to_owned()),
                cites_galaxies: vec!["smithy".to_owned()],
                federation_provenance: Some(lineage_fixture()),
            },
        );
        write_event(&path, &env);

        let mut checks = Vec::new();
        verify_federation_provenance(&path, None, &mut checks);
        assert!(!checks
            .iter()
            .any(|c| c.status == CheckStatus::Fail && c.category == "federation"));
        let pass = checks
            .iter()
            .find(|c| c.status == CheckStatus::Pass && c.name.contains("chronicle_added"))
            .expect("chronicle_added with provenance must PASS");
        assert!(pass.detail.contains("present"));
    }

    #[test]
    fn federation_check_ignores_local_chronicle_without_provenance() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("events.jsonl");
        // Local chronicle (no cites_galaxies): missing provenance is
        // not a federation concern.
        let env = Envelope::new(
            Seq(0),
            None,
            EventV2::ChronicleAdded {
                molecule_id: Some(MoleculeId::new("task-20260519-aaaa").unwrap()),
                chronicle_path: "docs/lore/CHRONICLES.md".to_owned(),
                entry_anchor: None,
                cites_galaxies: Vec::new(),
                federation_provenance: None,
            },
        );
        write_event(&path, &env);

        let mut checks = Vec::new();
        verify_federation_provenance(&path, None, &mut checks);
        // No cross-galaxy events: summary should be PASS, no FAIL rows.
        assert!(!checks
            .iter()
            .any(|c| c.status == CheckStatus::Fail && c.category == "federation"));
        let summary = checks
            .iter()
            .find(|c| c.name == "summary" && c.category == "federation")
            .unwrap();
        assert_eq!(summary.status, CheckStatus::Pass);
        assert!(summary.detail.contains("scanned: 0"));
    }

    #[test]
    fn federation_check_fails_on_adr_inscribed_cross_galaxy_missing_provenance() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("events.jsonl");
        let env = Envelope::new(
            Seq(0),
            None,
            EventV2::AdrInscribed {
                adr_number: 105,
                title: "I9' Federation Provenance".to_owned(),
                adr_path: "docs/adr/105-i9-prime-federation-provenance.md".to_owned(),
                cites_galaxies: vec!["smithy".to_owned(), "mailroom".to_owned()],
                federation_provenance: None,
            },
        );
        write_event(&path, &env);

        let mut checks = Vec::new();
        verify_federation_provenance(&path, None, &mut checks);
        let fail = checks
            .iter()
            .find(|c| c.status == CheckStatus::Fail && c.category == "federation")
            .expect("cross-galaxy ADR without provenance must FAIL");
        assert!(fail.name.contains("adr_inscribed"));
        assert!(fail.name.contains("ADR-105"));
        assert!(fail.detail.contains("missing federation_provenance"));
    }

    #[test]
    fn federation_check_passes_on_adr_inscribed_with_provenance() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("events.jsonl");
        let env = Envelope::new(
            Seq(0),
            None,
            EventV2::AdrInscribed {
                adr_number: 105,
                title: "I9' Federation Provenance".to_owned(),
                adr_path: "docs/adr/105-i9-prime-federation-provenance.md".to_owned(),
                cites_galaxies: vec!["smithy".to_owned()],
                federation_provenance: Some(lineage_fixture()),
            },
        );
        write_event(&path, &env);

        let mut checks = Vec::new();
        verify_federation_provenance(&path, None, &mut checks);
        assert!(!checks
            .iter()
            .any(|c| c.status == CheckStatus::Fail && c.category == "federation"));
        let pass = checks
            .iter()
            .find(|c| c.status == CheckStatus::Pass && c.name.contains("adr_inscribed"))
            .expect("adr_inscribed with provenance must PASS");
        assert!(pass.name.contains("ADR-105"));
    }

    #[test]
    fn federation_check_summary_includes_failure_count() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("events.jsonl");
        let env = Envelope::new(
            Seq(0),
            None,
            EventV2::MergeDispatched {
                molecule: MoleculeId::new("task-20260513-aaaa").unwrap(),
                branch: "smithy/x".to_owned(),
                federation_provenance: None,
            },
        );
        write_event(&path, &env);

        let mut checks = Vec::new();
        verify_federation_provenance(&path, None, &mut checks);
        let summary = checks
            .iter()
            .find(|c| c.name == "summary" && c.category == "federation")
            .unwrap();
        assert_eq!(summary.status, CheckStatus::Fail);
        assert!(summary.detail.contains("missing provenance: 1"));
    }
}
