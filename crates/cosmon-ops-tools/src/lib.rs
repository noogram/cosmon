// SPDX-License-Identifier: AGPL-3.0-only

//! `cosmon-ops-tools` — read-only cosmon-**domain** operation tools that
//! plug into the [`cosmon_agent_harness::Tool`] registry by calling
//! `cosmon-core` / `cosmon-state` **directly**, never by shelling out to
//! the `cs` binary.
//!
//! ## Role in the architecture
//!
//! This crate is the *internal-API tool backend* for the cs-pilot
//! cognitive loop (delib `2026-05-31-cs-pilot-external-cognitive-pilot`,
//! §4/§5). The cs-pilot REPL (a separate crate) owns the model loop; this
//! crate supplies the cosmon-aware tools the model may call during a turn.
//!
//! The whole reason the crate exists is **efficiency via the internal
//! API** (delib §4): a tool here loads a [`cosmon_filestore::FileStore`]
//! and calls `cosmon_state::ops::observe` / `ensemble` in-process, instead
//! of spawning `cs observe …` as a subprocess and parsing its stdout.
//! Shelling out would re-introduce the exact mechanical-CRUD limitation
//! that the existing remote crates already pay for (delib §3, "What we
//! must NOT copy").
//!
//! ## Two backends, one Tool surface
//!
//! ### Local backend (v0) — calls `cosmon-state` in-process
//!
//! Three read-only tools, used when the pilot runs *inside* a cosmon
//! instance (model in-process):
//!
//! - [`observe::ObserveTool`] — single-molecule state projection.
//! - [`peek::PeekTool`] — fleet + molecule overview.
//! - [`ensemble::EnsembleTool`] — filtered backlog snapshot.
//!
//! ### Remote backend (increment 2) — calls `cosmon-rpp-adapter` over HTTP
//!
//! The [`remote`] module re-implements the *same* `Tool` surface against the
//! ADR-080 §8p wire via [`cosmon_remote::Client`] (JWT auth), for a thin CLI
//! installed *outside* an avatar (e.g. tenant-demo). The cs-pilot loop is
//! unchanged — only the backend swaps (ADR-115 §6). The §8p strict subset
//! exposed is `observe` + `ensemble` (read) and `nucleate` + `tackle`
//! (write). `peek` has no RPP route, so it is **absent remotely**;
//! `done` / `evolve` / `complete` are absent **by construction** (operator-
//! only / worker-internal — ADR-080 §5). See [`remote`] for the full map.
//!
//! `done` is operator-only **forever** (teardown is a human gesture — see
//! CLAUDE.md "Command perimeters") on either backend. A pilot that can
//! *see* the fleet is the honest walking skeleton (delib §8, Q3); write
//! tools are opt-in.
//!
//! ## JSON-in / JSON-out contract (ADR-096 §2.6)
//!
//! Each tool follows the claw-code JSON-in / JSON-out tool contract
//! borrowed as **bibliography** under [ADR-096]: a serde `Deserialize`
//! input struct + a [`cosmon_agent_harness::Tool::execute`] that returns a
//! JSON string. We use cosmon glossary names throughout — these are
//! *operation tools* in a *read-only registry*, **not** claw's
//! `Plugin` / `Channel` (ADR-096 §3 forbids that vocabulary inside
//! cosmon). The borrowed pattern is the contract shape only; the names,
//! the direct-internal-API backend, and the worker/human perimeter are
//! native cosmon design.
//!
//! [ADR-096]: ../../docs/adr/096-openclaw-as-bibliography.md

#![forbid(unsafe_code)]

use std::path::Path;

use cosmon_filestore::{resolve_state_dir_from, FileStore};

pub mod ensemble;
pub mod observe;
pub mod peek;
pub mod remote;

pub use ensemble::EnsembleTool;
pub use observe::ObserveTool;
pub use peek::PeekTool;
pub use remote::{
    remote_read_only_registry, remote_registry, RemoteBackend, RemoteEnsembleTool,
    RemoteNucleateTool, RemoteObserveTool, RemoteTackleTool,
};

use cosmon_agent_harness::{ToolError, ToolRegistry};

/// Build a [`ToolRegistry`] holding the v0 read-only cosmon-ops tools.
///
/// Reuses [`cosmon_agent_harness::ToolRegistry`] (delib §5: the tools are
/// "independently testable and reusable by the remote adapter") rather
/// than inventing a parallel registry type. The cs-pilot REPL takes this
/// registry, merges it with the harness's filesystem tools as needed, and
/// hands the union to the model loop.
///
/// The three tools registered — `observe`, `peek`, `ensemble` — are all
/// read-only: none mutates state, so there is no double-writer hazard and
/// no worker/human-perimeter concern (those bind only to the write tools
/// deferred to increment 2). Registration order is irrelevant because
/// [`ToolRegistry`] is `BTreeMap`-backed (stable iteration by key).
#[must_use]
pub fn read_only_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ObserveTool));
    registry.register(Box::new(PeekTool));
    registry.register(Box::new(EnsembleTool));
    registry
}

/// Resolve a [`FileStore`] and its state directory from the tool call's
/// `work_dir`.
///
/// Tools receive the worker's `work_dir` (the [`Tool::execute`] contract);
/// from it we walk up to the project's `.cosmon/state/` exactly like the
/// `cs` CLI and the MCP server do, via
/// [`cosmon_filestore::resolve_state_dir_from`]. The resolved path is both
/// the [`FileStore`] root and the `state_dir` argument the read-only verb
/// library ([`cosmon_state::ops`]) expects for its energy/instrumentation
/// side-reads — they are the same directory (`.cosmon/state/`).
///
/// Returning the path alongside the store spares each tool a second
/// resolution; the two are always consumed together.
fn resolve_store(work_dir: &Path) -> (FileStore, std::path::PathBuf) {
    let state_dir = resolve_state_dir_from(work_dir);
    let store = FileStore::new(state_dir.clone());
    (store, state_dir)
}

/// Map a domain-side failure message into a [`ToolError::Io`].
///
/// The harness's [`ToolError`] is `#[non_exhaustive]` and lives in
/// `cosmon-agent-harness`; this crate cannot add a domain-specific
/// variant. The read-only verbs return their own rich error enums
/// (`ObserveError`, `EnsembleError`) and a `FileStore` can surface a
/// `CosmonError`; all of those collapse to the harness's catch-all
/// [`ToolError::Io`] carrying the original `Display` message so the model
/// still sees *why* (e.g. `"molecule not found: task-…"`). Invalid tool
/// arguments use [`ToolError::InvalidArguments`] instead — that distinction
/// is preserved at each call site.
fn io_err(message: impl std::fmt::Display) -> ToolError {
    ToolError::Io(message.to_string())
}

/// Deserialize a tool's argument object, mapping a parse failure onto
/// [`ToolError::InvalidArguments`] tagged with the tool name.
///
/// Shared by all three tools so the invalid-arguments wire shape stays
/// identical to the harness's own filesystem tools (`read_file`,
/// `list_dir`, …), which tag the same way.
fn parse_args<T: serde::de::DeserializeOwned>(
    tool: &'static str,
    arguments_json: &str,
) -> Result<T, ToolError> {
    serde_json::from_str(arguments_json).map_err(|e| ToolError::InvalidArguments {
        tool: tool.to_owned(),
        message: e.to_string(),
    })
}

/// Shared test fixture — seeds a temp `.cosmon/` project on disk so the
/// tools' walk-up `state_dir` resolution and `FileStore` reads exercise
/// the real filesystem path (the brief's "temp .cosmon state fixture").
///
/// `pub(crate)` so each tool module's unit tests reuse it; gated on
/// `cfg(test)` so it never ships in the library.
#[cfg(test)]
pub(crate) mod test_fixture {
    use std::collections::{BTreeSet, HashMap};
    use std::path::Path;

    use chrono::Utc;
    use cosmon_core::id::{FleetId, FormulaId, MoleculeId, StepId, WorkerId};
    use cosmon_core::tag::Tag;
    use cosmon_filestore::FileStore;
    use cosmon_state::{MoleculeData, StateStore};

    /// Create the `.cosmon/config.toml` marker so walk-up recognises
    /// `root` as a cosmon project (ADR-069 — a config-less `.cosmon/` is
    /// skipped). No molecules; an empty-fleet overview.
    pub(crate) fn seed_project(root: &Path) {
        let cosmon = root.join(".cosmon");
        std::fs::create_dir_all(&cosmon).unwrap();
        std::fs::write(
            cosmon.join("config.toml"),
            "# cosmon-ops-tools test fixture\n",
        )
        .unwrap();
    }

    /// Seed one untagged molecule with the given lifecycle status.
    pub(crate) fn seed_molecule(root: &Path, id: &str, status: &str) {
        seed_molecule_tagged(root, id, status, &[]);
    }

    /// Seed one molecule with the given status and tag set into the temp
    /// project's `FileStore`.
    pub(crate) fn seed_molecule_tagged(root: &Path, id: &str, status: &str, tags: &[&str]) {
        seed_project(root);
        let store = FileStore::new(root.join(".cosmon").join("state"));
        let data = make_molecule(id, status, tags);
        store.save_molecule(&data.id.clone(), &data).unwrap();
    }

    fn make_molecule(id: &str, status: &str, tags: &[&str]) -> MoleculeData {
        let now = Utc::now();
        let tag_set: BTreeSet<Tag> = tags.iter().map(|t| Tag::new(*t).unwrap()).collect();
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status: status.parse().unwrap(),
            variables: HashMap::new(),
            assigned_worker: Some(WorkerId::new("ruby").unwrap()),
            created_at: now,
            updated_at: now,
            total_steps: 2,
            current_step: 1,
            completed_steps: vec![StepId::new("implement").unwrap()],
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
            tags: tag_set,
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_registry_holds_exactly_the_three_read_tools() {
        let registry = read_only_registry();
        let names: Vec<&str> = registry.declarations().iter().map(|d| d.name).collect();
        // BTreeMap key order — alphabetical.
        assert_eq!(names, vec!["ensemble", "observe", "peek"]);
    }
}
