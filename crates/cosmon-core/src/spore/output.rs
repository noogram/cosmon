// SPDX-License-Identifier: AGPL-3.0-only

//! The **run-scoped output home** a germination hands to every node (ADR-161).
//!
//! # The gap this closes
//!
//! A spore is a reusable *moule* (template); germinating it produces *instances*.
//! Before this module, germination told a worker *what* to produce but never
//! *where* to durably put it: the worktree is destroyed at `cs done`, the
//! per-molecule state dir is not shared so cross-node references (`from
//! reproduce/`) cannot resolve, and `$COSMON_ARTIFACT_DIR` is set only under the
//! RPP path. With no defined home handed to it, a germinated worker *invented*
//! one — and the paths it invented landed inside the spore **definition** tree
//! (`spores/<name>/intakes/…`) or at the **repo root** (`reproduction.md`). Both
//! are the core anti-pattern: writing an instance back into the moule pollutes
//! the public repo and collides across runs (cosmon-dev dogfooding finding F9,
//! 2026-07-23).
//!
//! # The convention (ADR-161)
//!
//! Every germination gets a **run-scoped, gitignored, germination-id-namespaced**
//! output root under the state store:
//!
//! ```text
//! <state_root>/spore-runs/<germination-id>/        ← ${run_dir}   (shared)
//! <state_root>/spore-runs/<germination-id>/<alias>/ ← ${output_dir} (per node)
//! ```
//!
//! It sits under `.cosmon/state/`, so the existing `.gitignore` rule keeps it out
//! of the tracked tree by construction — no per-spore band-aid `.gitignore` is
//! needed. It is **shared** across the polymer's nodes, so cross-node references
//! (`${run_dir}/reproduce/`) resolve; it is **namespaced** by germination id, so
//! two runs of the same params never alias (the seal's `NoResourceCollision`
//! made concrete). It is neither the spore definition tree nor the repo root —
//! the two homes a worker must never write into.
//!
//! # How it reaches the worker
//!
//! [`expand`](super::expand::expand) stays pure: it cannot mint a run id (no
//! clock, no randomness). The germination **shell** mints the id, composes the
//! run dir, and calls [`inject_run_outputs`] on the expanded call list. That
//! function substitutes the two reserved runtime tokens ([`RUN_DIR_TOKEN`] /
//! [`OUTPUT_DIR_TOKEN`]) in every node var and adds the resolved paths as the
//! [`RUN_DIR_VAR`] / [`OUTPUT_DIR_VAR`] molecule variables — so a worker writes
//! where it is **told**, never where it guesses. The tokens are left verbatim by
//! `expand` (they are not `${params.*}`), exactly like a `${nodes.x.findings}`
//! runtime reference, so this later pass is where they resolve.

use std::path::{Component, Path, PathBuf};

use super::expand::NucleateCall;

/// The directory under the state store that holds every spore germination's
/// per-run gate records. Gitignored transitively by the `.cosmon/state/` rule.
pub const SPORE_RUNS_DIR: &str = "spore-runs";

/// The molecule variable carrying the shared, run-scoped output root
/// (`${run_dir}`), the same for every node of one germination.
pub const RUN_DIR_VAR: &str = "run_dir";

/// The molecule variable carrying this node's own output directory
/// (`${output_dir}` = `${run_dir}/<alias>/`), distinct per node.
pub const OUTPUT_DIR_VAR: &str = "output_dir";

/// The reserved token a spore topic/formula writes to reference the shared run
/// root. `expand` leaves it verbatim; [`inject_run_outputs`] resolves it.
pub const RUN_DIR_TOKEN: &str = "${run_dir}";

/// The reserved token a spore topic/formula writes to reference this node's own
/// output directory. `expand` leaves it verbatim; [`inject_run_outputs`] resolves
/// it.
pub const OUTPUT_DIR_TOKEN: &str = "${output_dir}";

/// Compose the shared run root for one germination:
/// `<state_root>/spore-runs/<germination_id>/`.
///
/// Pure path arithmetic — it touches no filesystem. The germination id is a
/// runtime value the shell mints (it embeds a wall-clock date and entropy, which
/// is why it cannot originate in the zero-I/O core).
#[must_use]
pub fn run_dir(state_root: &Path, germination_id: &str) -> PathBuf {
    state_root.join(SPORE_RUNS_DIR).join(germination_id)
}

/// This node's own output directory under the shared run root:
/// `<run_dir>/<alias>/`.
///
/// The alias is the node's unique germination handle (node id, or
/// `<node-id>__<index>` for a fan-out instance), so each node — and each
/// round-indexed emergent instance — writes a distinct path. This is the seal's
/// `NoResourceCollision` property made concrete.
///
/// **Containment is checked, not assumed.** `Path::join` with an absolute alias
/// *replaces* the base, and a `..` component traverses out of it — so a hostile
/// node id could otherwise hand a worker a path outside the run home. The alias
/// grammar ([`validate_node_id`](super::validate_node_id)) already refuses such
/// ids at parse time; this is the second, independent line of defence, on the
/// composition itself.
///
/// # Errors
/// Returns [`ForbiddenOutput::EscapesRunHome`] when the composed path is not
/// strictly under `run_dir`.
pub fn node_output_dir(run_dir: &Path, alias: &str) -> Result<PathBuf, ForbiddenOutput> {
    // The alias must name exactly ONE ordinary directory component. This alone
    // rules out `/tmp/x` (a root component), `../x` (a parent component), `a/b`
    // (two components), and `.` (no component at all).
    let mut components = Path::new(alias).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(one)), None) if one == alias => {}
        _ => return Err(ForbiddenOutput::EscapesRunHome),
    }

    let composed = run_dir.join(alias);

    // Lexical containment: resolve `.` and `..` against the base, then require
    // the result to be strictly *under* the run home. Purely lexical, so the
    // zero-I/O core keeps its property (no `canonicalize`, no symlink probe);
    // conservative, because it never widens the accepted set.
    let base = resolve_traversal(run_dir);
    let resolved = resolve_traversal(&composed);
    if resolved == base || !resolved.starts_with(&base) {
        return Err(ForbiddenOutput::EscapesRunHome);
    }
    Ok(composed)
}

/// Hand every call its run-scoped output home: substitute the reserved tokens in
/// each var value and record the resolved paths as the [`RUN_DIR_VAR`] /
/// [`OUTPUT_DIR_VAR`] molecule variables.
///
/// Pure and deterministic in `(calls, run_dir)`: the same inputs always yield the
/// same mutation, so a replay is byte-stable. It never removes or overwrites a
/// var the spore author set except the two reserved names, which it owns.
///
/// After this pass, a worker reads `output_dir` from its molecule variables (or
/// sees `${output_dir}` already resolved inside its `topic`) and writes its gate
/// records there — inside the state store, never the spore definition tree or the
/// repo root.
///
/// # Errors
/// Returns [`EscapedOutputHome`] when an alias would compose a path outside the
/// run home. Germination is refused as a whole: no call is handed a home if any
/// one of them escapes, so a hostile node cannot be silently dropped while its
/// siblings run.
pub fn inject_run_outputs(
    calls: &mut [NucleateCall],
    run_dir: &Path,
) -> Result<(), EscapedOutputHome> {
    // Check every alias BEFORE mutating anything, so a refusal leaves the call
    // list untouched: all-or-nothing, never a half-injected polymer.
    for call in calls.iter() {
        node_output_dir(run_dir, &call.alias).map_err(|_| EscapedOutputHome {
            alias: call.alias.clone(),
        })?;
    }

    let run_dir_str = run_dir.to_string_lossy().into_owned();
    for call in calls.iter_mut() {
        let output_dir = node_output_dir(run_dir, &call.alias).map_err(|_| EscapedOutputHome {
            alias: call.alias.clone(),
        })?;
        let output_dir_str = output_dir.to_string_lossy().into_owned();

        // Resolve the reserved tokens anywhere they appear in a var value
        // (typically the `topic`). Resolve `${output_dir}` first so a value
        // never accidentally reinterprets the substituted run-dir text.
        for value in call.vars.values_mut() {
            if value.contains(OUTPUT_DIR_TOKEN) {
                *value = value.replace(OUTPUT_DIR_TOKEN, &output_dir_str);
            }
            if value.contains(RUN_DIR_TOKEN) {
                *value = value.replace(RUN_DIR_TOKEN, &run_dir_str);
            }
        }

        // Also hand the paths as first-class molecule variables, so a worker
        // that reads its vars (rather than parsing the topic) still finds the
        // home it is told to write to.
        call.vars.insert(OUTPUT_DIR_VAR.to_string(), output_dir_str);
        call.vars
            .insert(RUN_DIR_VAR.to_string(), run_dir_str.clone());
    }
    Ok(())
}

/// A germination alias would compose an output directory outside the run home.
///
/// Returned by [`inject_run_outputs`] so the germination shell **refuses** the
/// run rather than handing a worker a path it must not write to. Carrying the
/// alias makes the refusal actionable: it names the node to fix.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("node alias \"{alias}\" would compose an output directory outside the run-scoped output home (ADR-161); refusing to germinate")]
pub struct EscapedOutputHome {
    /// The offending germination alias (node id, or `<node-id>__<index>`).
    pub alias: String,
}

/// Why a candidate gate-output path is refused (the two documented anti-patterns).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ForbiddenOutput {
    /// The path lies inside the spore **definition** tree (`spores/<name>/…`).
    /// Writing an instance back into the reusable moule pollutes the public repo
    /// and collides on the next germination.
    InsideSporeDefinition,
    /// The path is dumped **directly at the repo root** (a top-level file such as
    /// `reproduction.md`), the other place a home-less worker improvises into.
    RepoRoot,
    /// The path leaves the run-scoped output home (`<state>/spore-runs/<id>/`)
    /// — an absolute alias that replaced the base, or a `..` that traversed out
    /// of it. Run isolation is the point of the home; a path outside it is
    /// refused rather than silently written.
    EscapesRunHome,
}

/// Detect the germinated-worker anti-pattern: a gate-record path that lands in
/// the spore definition tree or at the repo root.
///
/// Returns `Some` naming which home was violated, or `None` when the path is an
/// acceptable destination (in particular, anything under the run-scoped output
/// home — which is inside `<repo>/.cosmon/state/spore-runs/…`, neither the spore
/// tree nor a top-level file). This is the pure kernel a guard wires: it decides,
/// it performs no I/O.
///
/// The comparison is **lexical** (component-wise, after collapsing `.`), so it is
/// deterministic and testable without a filesystem. Callers that receive
/// relative or `..`-laden paths should normalize against a known root first; the
/// documented anti-patterns are absolute writes the worker chose, which compare
/// cleanly.
#[must_use]
pub fn forbidden_gate_output(
    path: &Path,
    spore_definition_dir: &Path,
    repo_root: &Path,
) -> Option<ForbiddenOutput> {
    let path = lexically_normalize(path);
    let spore_dir = lexically_normalize(spore_definition_dir);
    let repo_root = lexically_normalize(repo_root);

    // Anything at or under the spore definition tree is the primary violation.
    if path.starts_with(&spore_dir) {
        return Some(ForbiddenOutput::InsideSporeDefinition);
    }

    // A file dumped directly at the repo root (its parent IS the repo root).
    if path.parent() == Some(repo_root.as_path()) {
        return Some(ForbiddenOutput::RepoRoot);
    }

    None
}

/// Collapse `.` components so lexical prefix/parent comparisons are stable. Does
/// not resolve symlinks or `..` (kept pure — no filesystem access); `..` is
/// preserved verbatim, which only ever makes the guard *more* conservative.
/// Collapse `.` **and** `..` components lexically, so a containment test cannot
/// be fooled by `<run>/../../elsewhere`.
///
/// Unlike [`lexically_normalize`] this *does* resolve `..` (popping the previous
/// normal component), because the caller is deciding containment rather than
/// comparing author-written prefixes. It still performs no I/O: symlinks are not
/// resolved. A leading `..` with nothing to pop is kept, which can only make the
/// result fail a `starts_with` check — i.e. refuse.
fn resolve_traversal(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn lexically_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::spore::{expand, Spore};

    /// A minimal two-node spore whose topics reference both reserved tokens, so a
    /// test can prove the germination shell resolves them.
    const SPORE: &str = r#"
[spore]
name = "cosmon-dev"

[spore.formulas.work]
path = "formulas/work.formula.toml"

[[spore.node]]
id = "intake"
kind = "fixed"
formula = "work"
[spore.node.vars]
topic = "Emit ${output_dir}/verdict.json"

[[spore.node]]
id = "green"
kind = "fixed"
formula = "work"
[spore.node.vars]
topic = "Re-run the frozen red from ${run_dir}/reproduce/ and emit ${output_dir}/green.md"

[[spore.edge]]
from = "intake"
to = "green"
type = "feeds"
"#;

    fn expanded() -> Vec<NucleateCall> {
        let spore = Spore::parse(SPORE).unwrap();
        expand(&spore, &BTreeMap::new()).unwrap()
    }

    #[test]
    fn run_dir_is_namespaced_under_the_state_store() {
        let dir = run_dir(Path::new("/repo/.cosmon/state"), "germ-20260723-abcd");
        assert_eq!(
            dir,
            Path::new("/repo/.cosmon/state/spore-runs/germ-20260723-abcd")
        );
    }

    #[test]
    fn node_output_dir_is_distinct_per_alias() {
        let run = run_dir(Path::new("/s"), "germ-1");
        assert_ne!(
            node_output_dir(&run, "intake"),
            node_output_dir(&run, "green"),
            "each node must get its own output dir (NoResourceCollision)"
        );
    }

    /// DELIVERABLE (a): germination HANDS each node its output path. Every call
    /// carries an `output_dir` var, distinct per node and rooted under the run
    /// dir — so a worker never has to invent a home.
    #[test]
    fn inject_hands_every_node_an_output_dir_under_the_run_root() {
        let mut calls = expanded();
        let run = run_dir(Path::new("/repo/.cosmon/state"), "germ-xyz");
        inject_run_outputs(&mut calls, &run).unwrap();

        let run_str = run.to_string_lossy().into_owned();
        for call in &calls {
            let out = call
                .vars
                .get(OUTPUT_DIR_VAR)
                .expect("every germinated node must be handed an output_dir");
            assert!(
                out.starts_with(&run_str),
                "output_dir {out} must be under the run root {run_str}"
            );
            assert!(
                out.ends_with(&call.alias),
                "output_dir must be namespaced by the node alias"
            );
            assert_eq!(
                call.vars.get(RUN_DIR_VAR).map(String::as_str),
                Some(run_str.as_str()),
                "every node shares the same run_dir"
            );
        }
    }

    /// The reserved tokens inside a topic are resolved to concrete paths, so the
    /// worker reads an absolute destination, not a literal `${output_dir}`.
    #[test]
    fn inject_resolves_the_reserved_tokens_in_topics() {
        let mut calls = expanded();
        let run = run_dir(Path::new("/repo/.cosmon/state"), "germ-xyz");
        inject_run_outputs(&mut calls, &run).unwrap();

        let green = calls.iter().find(|c| c.alias == "green").unwrap();
        let topic = green.vars.get("topic").unwrap();
        assert!(
            !topic.contains(OUTPUT_DIR_TOKEN) && !topic.contains(RUN_DIR_TOKEN),
            "no reserved token may survive substitution: {topic}"
        );
        assert!(
            topic.contains("/spore-runs/germ-xyz/reproduce/"),
            "the shared cross-node reference must resolve under the run root: {topic}"
        );
        assert!(
            topic.contains("/spore-runs/germ-xyz/green/green.md"),
            "the node's own output must resolve under its output_dir: {topic}"
        );
    }

    /// DELIVERABLE (b): the anti-pattern is DETECTABLE. A gate record written
    /// into the spore definition tree is refused.
    #[test]
    fn output_inside_the_spore_definition_tree_is_forbidden() {
        let repo = Path::new("/repo");
        let spore_dir = Path::new("/repo/spores/cosmon-dev");
        let scattered = Path::new("/repo/spores/cosmon-dev/intakes/issue-21-g0/verdict.json");
        assert_eq!(
            forbidden_gate_output(scattered, spore_dir, repo),
            Some(ForbiddenOutput::InsideSporeDefinition),
        );
    }

    /// The other documented anti-pattern: a file dumped at the repo root.
    #[test]
    fn output_dumped_at_the_repo_root_is_forbidden() {
        let repo = Path::new("/repo");
        let spore_dir = Path::new("/repo/spores/cosmon-dev");
        let dumped = Path::new("/repo/reproduction.md");
        assert_eq!(
            forbidden_gate_output(dumped, spore_dir, repo),
            Some(ForbiddenOutput::RepoRoot),
        );
    }

    /// The run-scoped home is the ACCEPTED destination — the whole point of the
    /// convention. It is under the repo but neither the spore tree nor a
    /// top-level file, so the guard must pass it.
    #[test]
    fn output_under_the_run_home_is_allowed() {
        let repo = Path::new("/repo");
        let spore_dir = Path::new("/repo/spores/cosmon-dev");
        let run = run_dir(Path::new("/repo/.cosmon/state"), "germ-1");
        let good = node_output_dir(&run, "intake").unwrap().join("verdict.json");
        assert_eq!(forbidden_gate_output(&good, spore_dir, repo), None);
    }

    /// Review finding F6, frozen as a red-first regression.
    ///
    /// `run_dir.join(alias)` is not containment: an **absolute** alias replaces
    /// the base outright, and a `..` traverses out of it lexically. A spore that
    /// parses cleanly could therefore hand a worker an `output_dir` outside
    /// `<state>/spore-runs/<germination-id>/` — pointing it at the tracked tree.
    /// Composition must refuse, independently of the id grammar upstream.
    #[test]
    fn hostile_aliases_never_compose_a_path_outside_the_run_home() {
        let run = run_dir(Path::new("/repo/.cosmon/state"), "germ-1");
        let hostile = [
            "../../tracked-output", // traverses out of the run home
            "..",                   // the run home's parent itself
            "/tmp/cosmon-output",   // absolute: `join` replaces the base
            "a/b",                  // more than one component
            "./x",                  // a relative prefix
            ".",                    // no component at all
            "",                     // empty
        ];
        for alias in hostile {
            let composed = node_output_dir(&run, alias);
            assert_eq!(
                composed,
                Err(ForbiddenOutput::EscapesRunHome),
                "alias {alias:?} must be refused, got {composed:?}"
            );
            // The property that matters, stated directly: whatever a caller
            // ends up with, it is never outside the run home.
            if let Ok(path) = composed {
                assert!(
                    path.starts_with(&run) && path != run,
                    "alias {alias:?} escaped the run home: {}",
                    path.display()
                );
            }
        }

        // The benign shapes the convention actually produces still work.
        for alias in ["intake", "green", "analyse__0", "ci-gate", "a_b-1"] {
            let path = node_output_dir(&run, alias).expect("benign alias");
            assert_eq!(path, run.join(alias));
        }
    }

    /// The refusal must reach the germination shell, not merely the pure
    /// composer: `inject_run_outputs` refuses the whole call list and leaves it
    /// untouched, so no worker is ever handed an escaping home.
    #[test]
    fn injection_refuses_a_call_list_carrying_an_escaping_alias() {
        let mut calls = expanded();
        calls[0].alias = "../../tracked-output".to_string();
        let before = calls.clone();
        let run = run_dir(Path::new("/repo/.cosmon/state"), "germ-1");

        let err = inject_run_outputs(&mut calls, &run).expect_err("must refuse");
        assert_eq!(err.alias, "../../tracked-output");
        assert_eq!(calls, before, "a refused injection mutates nothing");
    }
}
