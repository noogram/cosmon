// SPDX-License-Identifier: AGPL-3.0-only

//! `cs mission graph <root>` — read-only DAG view (delib-20260720-cff4, Phase 1).
//!
//! Renders the mission rooted at `<root>` by **joining** two sources that the
//! Phase-1 trailers finally make joinable:
//!
//! - the *ledger* — the scheduler's `blocked_by` / `blocks` edges, walked
//!   downstream from the root to enumerate every molecule in the mission;
//! - *git history* — the completion merge commits, matched to their molecule
//!   by the `Mol-Id` trailer that [`cs done`](super::done) stamps.
//!
//! The result is the "history whose DAG structure is legible" the operator
//! asked for: for a mission root, the tree of molecules, the merge commit each
//! one landed on (or `—` if it has not merged yet), and the dependency edges.
//!
//! Strictly read-only: it loads molecules and reads `git log`, never mutating
//! state and never spawning a worker. It adds no verb to the frozen `cs done`
//! surface (§8p) — it is a separate observation command, the same class as
//! [`cs deps`](super::deps).

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::Path;
use std::process::Command;

use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_state::{MoleculeFilter, StateStore};

use super::lineage;
use super::Context;

/// Arguments for `cs mission`.
#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub command: Sub,
}

/// `cs mission` subcommands.
#[derive(clap::Subcommand)]
pub enum Sub {
    /// Render the mission DAG rooted at a molecule, joining ledger edges to
    /// their completion merge commits.
    Graph(GraphArgs),
}

/// Arguments for `cs mission graph`.
#[derive(clap::Args)]
pub struct GraphArgs {
    /// Mission root molecule ID (exact or unambiguous prefix).
    pub root: String,
}

/// Execute `cs mission`.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.command {
        Sub::Graph(g) => run_graph(ctx, g),
    }
}

/// One molecule's row in the rendered mission.
struct Node {
    id: MoleculeId,
    status: MoleculeStatus,
    /// Direct `blocked_by` parents (the DAG edges into this node).
    depends_on: Vec<MoleculeId>,
    /// Short SHA of the completion merge whose `Mol-Id` trailer names this
    /// molecule, if one exists in the joined git history.
    merge_commit: Option<String>,
}

fn run_graph(ctx: &Context, args: &GraphArgs) -> anyhow::Result<()> {
    let state_dir = ctx.state_dir();
    let store = ctx.store_at(&state_dir);

    let root_id = resolve_molecule_id(store.as_ref(), &args.root)?;

    // Enumerate the mission: the root plus everything it transitively blocks.
    let members = collect_mission(store.as_ref(), &root_id);

    // Join to git history: Mol-Id trailer → completion merge commit.
    let repo_root = find_repo_root();
    let commit_by_mol = repo_root
        .as_deref()
        .map(harvest_completion_commits)
        .unwrap_or_default();

    let mut nodes: Vec<Node> = members
        .iter()
        .map(|id| {
            let (status, depends_on) = match store.load_molecule(id) {
                Ok(m) => (m.status, m.blocked_by().into_iter().cloned().collect()),
                Err(_) => (MoleculeStatus::Pending, Vec::new()),
            };
            Node {
                id: id.clone(),
                status,
                depends_on,
                merge_commit: commit_by_mol.get(id.as_str()).cloned(),
            }
        })
        .collect();
    // Stable order: id-sorted so repeated runs render identically.
    nodes.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));

    if ctx.json {
        emit_json(&root_id, &nodes);
    } else {
        emit_human(&root_id, &nodes);
    }
    Ok(())
}

/// Resolve a molecule ID from an exact match or an unambiguous prefix.
fn resolve_molecule_id(store: &dyn StateStore, query: &str) -> anyhow::Result<MoleculeId> {
    if let Ok(exact) = MoleculeId::new(query) {
        if store.load_molecule(&exact).is_ok() {
            return Ok(exact);
        }
    }
    let all = store.list_molecules(&MoleculeFilter::default())?;
    let matches: Vec<_> = all
        .iter()
        .filter(|m| m.id.as_str().starts_with(query))
        .collect();
    match matches.len() {
        0 => Err(anyhow::anyhow!("no molecule matching `{query}`")),
        1 => Ok(matches[0].id.clone()),
        n => Err(anyhow::anyhow!(
            "ambiguous prefix `{query}` matches {n} molecules"
        )),
    }
}

/// Collect the mission members: `root` plus its transitive `blocks` closure.
///
/// BFS over `blocks()` (downstream) edges. Cycle-safe via a visited set. The
/// root is included as the first member.
fn collect_mission(store: &dyn StateStore, root: &MoleculeId) -> Vec<MoleculeId> {
    let mut seen: HashSet<MoleculeId> = HashSet::new();
    let mut order: Vec<MoleculeId> = Vec::new();
    let mut queue: VecDeque<MoleculeId> = VecDeque::new();

    seen.insert(root.clone());
    order.push(root.clone());
    queue.push_back(root.clone());

    while let Some(id) = queue.pop_front() {
        if let Ok(mol) = store.load_molecule(&id) {
            for child in mol.blocks() {
                if seen.insert(child.clone()) {
                    order.push(child.clone());
                    queue.push_back(child.clone());
                }
            }
        }
    }
    order
}

/// Walk `git log` on HEAD and build a map from `Mol-Id` trailer → short SHA of
/// the merge commit that carries it. The most recent commit wins on the
/// (pathological) chance of a duplicate id.
///
/// Best-effort: any git failure yields an empty map so the view degrades to
/// "no merge commit found" rather than erroring — the ledger half still
/// renders.
fn harvest_completion_commits(repo_root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    // NUL-separate records, RS-separate fields: <short-sha>\x1f<full-body>.
    let output = Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "log",
            "--format=%h\x1f%B%x00",
            "HEAD",
        ])
        .output();
    let Ok(output) = output else {
        return out;
    };
    if !output.status.success() {
        return out;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for record in text.split('\0') {
        let record = record.trim_start_matches('\n');
        let Some((sha, body)) = record.split_once('\x1f') else {
            continue;
        };
        if let Some(mol) = lineage::trailer_value(body, lineage::MOL_ID_KEY) {
            out.entry(mol).or_insert_with(|| sha.trim().to_owned());
        }
    }
    out
}

/// Locate the repository root by walking up from the current directory.
/// Returns `None` when not inside a git repo (the view then omits commits).
fn find_repo_root() -> Option<std::path::PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if path.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(path))
    }
}

fn status_label(status: MoleculeStatus) -> &'static str {
    match status {
        MoleculeStatus::Completed => "completed",
        MoleculeStatus::Collapsed => "collapsed",
        MoleculeStatus::Running => "running",
        MoleculeStatus::Pending => "pending",
        MoleculeStatus::Queued => "queued",
        MoleculeStatus::Frozen => "frozen",
        MoleculeStatus::Starved => "starved",
        _ => "unknown",
    }
}

fn emit_human(root: &MoleculeId, nodes: &[Node]) {
    println!("Mission {root} — {} molecule(s)", nodes.len());
    println!();
    for node in nodes {
        let merge = node
            .merge_commit
            .as_deref()
            .map_or_else(|| "—".to_owned(), |s| format!("merge {s}"));
        println!("  {} [{}]  {merge}", node.id, status_label(node.status));
        if node.id == *root {
            println!("      (mission root)");
        }
        for parent in &node.depends_on {
            println!("      ⤴ depends-on {parent}");
        }
    }
}

fn emit_json(root: &MoleculeId, nodes: &[Node]) {
    let molecules: Vec<serde_json::Value> = nodes
        .iter()
        .map(|n| {
            serde_json::json!({
                "id": n.id.as_str(),
                "status": status_label(n.status),
                "depends_on": n.depends_on.iter().map(MoleculeId::as_str).collect::<Vec<_>>(),
                "merge_commit": n.merge_commit,
            })
        })
        .collect();
    let out = serde_json::json!({
        "mission": root.as_str(),
        "molecules": molecules,
    });
    println!("{out}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::id::{FleetId, FormulaId};
    use cosmon_core::interaction::MoleculeLink;
    use cosmon_filestore::FileStore;
    use cosmon_state::MoleculeData;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn sample(id: &str) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status: MoleculeStatus::Completed,
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
            tags: std::collections::BTreeSet::new(),
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
    fn collect_mission_reconstructs_known_small_dag() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        // root -blocks-> b -blocks-> c ; root -blocks-> d
        let root = MoleculeId::new("task-20260720-root").unwrap();
        let b = MoleculeId::new("task-20260720-000b").unwrap();
        let c = MoleculeId::new("task-20260720-000c").unwrap();
        let d = MoleculeId::new("task-20260720-000d").unwrap();

        let mut mr = sample(root.as_str());
        mr.typed_links
            .push(MoleculeLink::Blocks { target: b.clone() });
        mr.typed_links
            .push(MoleculeLink::Blocks { target: d.clone() });
        let mut mb = sample(b.as_str());
        mb.typed_links.push(MoleculeLink::BlockedBy {
            source: root.clone(),
        });
        mb.typed_links
            .push(MoleculeLink::Blocks { target: c.clone() });
        let mut mc = sample(c.as_str());
        mc.typed_links
            .push(MoleculeLink::BlockedBy { source: b.clone() });
        let mut md = sample(d.as_str());
        md.typed_links.push(MoleculeLink::BlockedBy {
            source: root.clone(),
        });

        store.save_molecule(&root, &mr).unwrap();
        store.save_molecule(&b, &mb).unwrap();
        store.save_molecule(&c, &mc).unwrap();
        store.save_molecule(&d, &md).unwrap();

        let members = collect_mission(&store, &root);
        let ids: HashSet<&str> = members.iter().map(MoleculeId::as_str).collect();
        assert_eq!(ids.len(), 4);
        assert!(ids.contains("task-20260720-root"));
        assert!(ids.contains("task-20260720-000b"));
        assert!(ids.contains("task-20260720-000c"));
        assert!(ids.contains("task-20260720-000d"));
    }

    #[test]
    fn collect_mission_is_cycle_safe() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let a = MoleculeId::new("task-20260720-cyca").unwrap();
        let b = MoleculeId::new("task-20260720-cycb").unwrap();
        let mut ma = sample(a.as_str());
        ma.typed_links
            .push(MoleculeLink::Blocks { target: b.clone() });
        let mut mb = sample(b.as_str());
        mb.typed_links
            .push(MoleculeLink::Blocks { target: a.clone() });
        store.save_molecule(&a, &ma).unwrap();
        store.save_molecule(&b, &mb).unwrap();

        let members = collect_mission(&store, &a);
        assert_eq!(members.len(), 2);
    }

    #[test]
    fn resolve_prefix_unambiguous() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let m = sample("task-20260720-uniq");
        store.save_molecule(&m.id, &m).unwrap();
        let resolved = resolve_molecule_id(&store, "task-20260720-uni").unwrap();
        assert_eq!(resolved.as_str(), "task-20260720-uniq");
    }
}
