// SPDX-License-Identifier: AGPL-3.0-only

//! `cs test` — spec-suite entry point.
//!
//! Today this command only exposes `--binding-report`, which emits the
//! Constitution-clause ↔ scenario binding table in JSON (for tooling)
//! and rewrites `docs/spec-bindings.md` (for humans). The execution
//! engine that actually runs scenarios under `tests/spec/scenarios/`
//! lands in a follow-up molecule; this skeleton establishes the CLI
//! surface and the triple bijection (prose / scenario / Lean).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::Context;

/// Arguments for `cs test`.
#[derive(clap::Args)]
pub struct Args {
    /// Emit the Constitution ↔ scenario binding report (JSON to stdout
    /// and Markdown to `docs/spec-bindings.md`).
    #[arg(long)]
    pub binding_report: bool,

    /// When writing the Markdown report, use this path instead of
    /// `docs/spec-bindings.md` (repo-root-relative).
    #[arg(long)]
    pub out: Option<PathBuf>,

    /// Glob of scenario TOML files to execute. Defaults to
    /// `tests/scenarios/*.toml` (repo-root-relative).
    pub glob: Option<String>,
}

/// One row in the binding table. `foundry_proposition` is intentionally
/// `Option<String>` — Track C (polymer-proof correspondence) will fill
/// these in later; until then the value is `None` and the TODO list at
/// the bottom of the Markdown report names the target lemma.
#[derive(Debug, Clone, Serialize)]
pub struct Binding {
    /// Constitution clause (short, human-readable).
    pub clause: &'static str,
    /// Scenario TOML path, repo-root-relative.
    pub scenario: &'static str,
    /// Future Lean proposition name. `None` until Track C lands.
    pub foundry_proposition: Option<&'static str>,
    /// Current execution status — `"skeleton"` until the harness lands.
    pub status: &'static str,
}

/// The canonical binding table. Five clauses, one row each.
///
/// Keep this list in sync with `docs/spec-bindings.md` and the files
/// under `tests/spec/scenarios/` — `cs test --binding-report` is the
/// mechanism that enforces that synchronisation.
pub const BINDINGS: &[Binding] = &[
    Binding {
        clause: "merge-before-dispatch",
        scenario: "tests/spec/scenarios/merge-before-dispatch.toml",
        foundry_proposition: None,
        status: "skeleton",
    },
    Binding {
        clause: "reconcile idempotence",
        scenario: "tests/spec/scenarios/reconcile-idempotent.toml",
        foundry_proposition: None,
        status: "skeleton",
    },
    Binding {
        clause: "collapse is terminal",
        scenario: "tests/spec/scenarios/collapse-terminal.toml",
        foundry_proposition: None,
        status: "skeleton",
    },
    Binding {
        clause: "native step drain",
        scenario: "tests/spec/scenarios/native-step-drain.toml",
        foundry_proposition: None,
        status: "skeleton",
    },
    Binding {
        clause: "ready_frontier monotone",
        scenario: "tests/spec/scenarios/ready-frontier-monotone.toml",
        foundry_proposition: None,
        status: "skeleton",
    },
];

/// Target Lean propositions — the Track C TODO list rendered into the
/// Markdown report. One per binding, in the same order.
pub const LEAN_TODO: &[(&str, &str)] = &[
    (
        "merge-before-dispatch",
        "MergeBeforeDispatch_monotone: ∀ m, dispatched m → merged (predecessors m)",
    ),
    (
        "reconcile idempotence",
        "Reconcile_idempotent: reconcile ∘ reconcile = reconcile",
    ),
    (
        "collapse is terminal",
        "Collapse_absorbing: collapsed m → ∀ t, phase (step t m) = collapsed",
    ),
    (
        "native step drain",
        "NativeStepDrain_maximal: tackle m drains the maximal native prefix of m",
    ),
    (
        "ready_frontier monotone",
        "ReadyFrontier_monotone: ready_frontier t ⊆ ready_frontier (t+1) ∪ dispatched t",
    ),
];

/// Entrypoint.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    if !args.binding_report {
        return run_scenarios(ctx, args);
    }

    let repo_root = find_repo_root()?;
    let md_path = args
        .out
        .clone()
        .unwrap_or_else(|| repo_root.join("docs/spec-bindings.md"));

    std::fs::write(&md_path, render_markdown())?;

    let json = serde_json::json!({
        "bindings": BINDINGS,
        "lean_todo": LEAN_TODO
            .iter()
            .map(|(c, p)| serde_json::json!({ "clause": c, "proposition": p }))
            .collect::<Vec<_>>(),
        "markdown_path": md_path.to_string_lossy(),
    });

    if ctx.json {
        println!("{}", serde_json::to_string(&json)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&json)?);
    }
    Ok(())
}

/// Execute scenarios matching `args.glob` (default `tests/scenarios/*.toml`).
/// Prints one `PASS`/`FAIL` line per scenario and exits non-zero iff any fail.
fn run_scenarios(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let repo_root = find_repo_root()?;
    let pattern = args
        .glob
        .clone()
        .unwrap_or_else(|| "tests/scenarios/*.toml".to_string());
    let abs_pattern = if Path::new(&pattern).is_absolute() {
        pattern.clone()
    } else {
        repo_root.join(&pattern).to_string_lossy().into_owned()
    };

    let files = cosmon_scenario::discover(&abs_pattern)
        .map_err(|e| anyhow::anyhow!("discover {abs_pattern}: {e}"))?;
    if files.is_empty() {
        anyhow::bail!("no scenarios matched {abs_pattern}");
    }

    let mut failed = 0usize;
    let mut results = Vec::new();
    for f in &files {
        let r = cosmon_scenario::run_scenario(f);
        if r.passed {
            if !ctx.json {
                println!("PASS  {}  ({} ms)", r.name, r.duration_ms);
            }
        } else {
            failed += 1;
            if !ctx.json {
                println!("FAIL  {}  ({} ms)", r.name, r.duration_ms);
                for msg in &r.failures {
                    println!("      - {msg}");
                }
            }
        }
        results.push(r);
    }

    if ctx.json {
        let v: Vec<_> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name,
                    "passed": r.passed,
                    "duration_ms": r.duration_ms,
                    "failures": r.failures,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "scenarios": v,
                "failed": failed,
                "total": results.len(),
            }))?
        );
    } else {
        println!(
            "\n{} passed, {} failed out of {}",
            results.len() - failed,
            failed,
            results.len()
        );
    }

    if failed > 0 {
        anyhow::bail!("{failed} scenario(s) failed");
    }
    Ok(())
}

fn find_repo_root() -> anyhow::Result<PathBuf> {
    let mut cur = std::env::current_dir()?;
    loop {
        if cur.join("Cargo.toml").exists() && cur.join("docs").is_dir() {
            return Ok(cur);
        }
        if !cur.pop() {
            anyhow::bail!("could not find repo root");
        }
    }
}

fn render_markdown() -> String {
    let mut s = String::new();
    s.push_str("# Spec Bindings — Constitution ↔ Scenario ↔ Lean\n\n");
    s.push_str(
        "This file is generated by `cs test --binding-report`. Do not edit by hand; \
         update `crates/cosmon-cli/src/cmd/test.rs::BINDINGS` and regenerate.\n\n",
    );
    s.push_str("## The triple bijection\n\n");
    s.push_str(
        "Cosmon's specification lives in three mutually-consistent forms. A clause is \
         correctly specified iff all three agree.\n\n",
    );
    s.push_str("```\n");
    s.push_str("        prose (CLAUDE.md / CONSTITUTION.md)\n");
    s.push_str("                    │\n");
    s.push_str("                    ▼\n");
    s.push_str("   scenario TOML (tests/spec/scenarios/*.toml)\n");
    s.push_str("                    │\n");
    s.push_str("                    ▼\n");
    s.push_str("   Lean proposition (Track C, foundry_proposition)\n");
    s.push_str("```\n\n");
    s.push_str(
        "Prose tells a human *what* the invariant is. The scenario TOML declares an \
         *executable* witness — a fixture the harness can replay to detect regressions. \
         The Lean proposition is the *formal* statement the polymer-proof correspondence \
         (Track C) will discharge. `cs test --binding-report` is the fsck that checks \
         every clause has all three.\n\n",
    );
    s.push_str("## Bindings\n\n");
    s.push_str("| Scenario | Clause | Lean | Status |\n");
    s.push_str("| --- | --- | --- | --- |\n");
    for b in BINDINGS {
        let scenario = Path::new(b.scenario).file_stem().map_or_else(
            || b.scenario.to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
        let lean = b.foundry_proposition.unwrap_or("_TODO_");
        let _ = writeln!(
            s,
            "| `{scenario}` | {clause} | {lean} | {status} |",
            scenario = scenario,
            clause = b.clause,
            lean = lean,
            status = b.status,
        );
    }
    s.push_str("\n## Lean TODO — target propositions (Track C)\n\n");
    for (clause, prop) in LEAN_TODO {
        let _ = writeln!(s, "- **{clause}** — `{prop}`");
    }
    s.push_str(
        "\nWhen a Lean proposition is discharged, set `foundry_proposition` in the \
         matching `BINDINGS` entry and regenerate this file.\n",
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_table_has_five_clauses() {
        assert_eq!(BINDINGS.len(), 5);
        assert_eq!(LEAN_TODO.len(), BINDINGS.len());
        for (b, (clause, _)) in BINDINGS.iter().zip(LEAN_TODO) {
            assert_eq!(&b.clause, clause);
        }
    }

    #[test]
    fn markdown_contains_every_clause() {
        let md = render_markdown();
        for b in BINDINGS {
            assert!(md.contains(b.clause), "missing clause {}", b.clause);
        }
        assert!(md.contains("| Scenario | Clause | Lean | Status |"));
    }

    #[test]
    fn scenario_files_exist() {
        // Best-effort: only run when repo root is discoverable from the
        // test binary's CWD (it is, under `cargo test`).
        let Ok(root) = find_repo_root() else { return };
        for b in BINDINGS {
            let p = root.join(b.scenario);
            assert!(p.exists(), "missing scenario file {}", p.display());
        }
    }
}
