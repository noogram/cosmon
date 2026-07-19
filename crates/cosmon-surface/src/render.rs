// SPDX-License-Identifier: AGPL-3.0-only

//! Surface rendering — project state onto files.
//!
//! Pure functions that take fleet/molecule state and produce file content.
//! The I/O (writing files) is done by the caller.

use std::collections::HashMap;
use std::fmt::Write;
use std::path::Path;

use cosmon_core::declaration::MoleculeDeclaration;
use cosmon_core::expiry::format_expiry_badge_static;
use cosmon_core::formula::Formula;
use cosmon_core::id::{FormulaId, MoleculeId};
use cosmon_core::interaction::MoleculeLink;
use cosmon_core::kind::MoleculeKind;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_state::{Fleet, MoleculeData};

use crate::config::{Branding, SurfaceConfig, SurfaceKind};

/// Map from [`FormulaId`] to its parsed [`Formula`] declaration, loaded from
/// `.cosmon/formulas/*.formula.toml`.
///
/// Threaded through the projection pipeline so surface renderers can resolve
/// formula titles, step metadata, and descriptions when rendering molecules.
/// The renderers fall back gracefully when a molecule's formula is absent
/// from the map (legacy molecules or deleted formulas).
pub type FormulaMap = HashMap<FormulaId, Formula>;

/// Map from a molecule's `id_prefix` (the declaration's alphanumeric prefix
/// field) to its parsed [`MoleculeDeclaration`], loaded from
/// `.cosmon/molecules/*.toml`.
///
/// Declarations carry a *declaration-specific* `description` that is distinct
/// from the formula's description: the formula answers *what kind of work is
/// this* (e.g. "Data quality issue — re-download or locate a missing file"),
/// while the declaration answers *which instance of that work* (e.g.
/// "Re-download GDELT 20221110.export.CSV.zip (missing from source disk)").
/// When projecting a molecule onto a surface, the declaration's description
/// is the most specific human label and takes precedence over the formula's.
///
/// Keyed by `id_prefix` because declaration files do not record the generated
/// molecule ID (the suffix is a random 4-hex token chosen at nucleate time),
/// while [`MoleculeId::prefix`] is always recoverable. This assumes a 1:1
/// correspondence between `id_prefix` and declaration file in the project's
/// layout — the common pattern (one alphanumeric prefix per tracked issue)
/// in Atlas-style deployments. Collisions (two declarations with the same
/// prefix) resolve last-wins, which is acceptable because the renderers
/// already fall back cleanly when the lookup misses.
pub type DeclarationMap = HashMap<String, MoleculeDeclaration>;

/// Project all surfaces from a config, writing files to `project_root`.
///
/// # Errors
///
/// Returns an error if any file cannot be written.
pub fn project_surfaces(
    config: &SurfaceConfig,
    project_root: &Path,
    fleet: &Fleet,
    molecules: &[MoleculeData],
    formulas: &FormulaMap,
    declarations: &DeclarationMap,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    // Derive state_dir from project_root for GitHub mirror storage.
    let state_dir = project_root.join(".cosmon/state");
    let state_ref = if state_dir.is_dir() {
        Some(state_dir.as_path())
    } else {
        None
    };

    let mut written = Vec::new();

    for surface in &config.surface {
        // Apply molecule_kinds filter for this surface.
        let filtered: Vec<&MoleculeData> = if surface.molecule_kinds.is_empty() {
            molecules.iter().collect()
        } else {
            molecules
                .iter()
                .filter(|m| {
                    let kind = m.kind.unwrap_or(MoleculeKind::Task);
                    surface.accepts_kind(kind)
                })
                .collect()
        };
        let filtered_data: Vec<MoleculeData> = filtered.into_iter().cloned().collect();

        // GitHub Issues — separate path, uses gh CLI.
        if surface.kind == SurfaceKind::GithubIssues {
            let synced = crate::github::project_github_issues(
                surface,
                &filtered_data,
                state_ref,
                formulas,
                declarations,
            )?;
            for s in &synced {
                written.push(s.clone());
            }
            continue;
        }

        let content = match surface.referent.as_str() {
            "project.status" => render_status(fleet, &filtered_data, formulas, surface.branding),
            "project.issues" => render_issues(&filtered_data, formulas, surface.branding),
            "project.ideas" => render_ideas(&filtered_data, formulas, surface.branding),
            "project.deliberations" => {
                render_deliberations(&filtered_data, formulas, surface.branding)
            }
            "project.decisions" if surface.kind == SurfaceKind::Directory => {
                render_adr_index(project_root, &surface.path, surface.branding)
            }
            _ => continue, // Unknown referent — skip silently.
        };

        // For directory surfaces, write an INDEX.md inside the directory.
        let target = if surface.kind == SurfaceKind::Directory {
            let dir = project_root.join(&surface.path);
            std::fs::create_dir_all(&dir)?;
            dir.join("INDEX.md")
        } else {
            let target = project_root.join(&surface.path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            target
        };
        atomic_write_surface(&target, &content)?;
        written.push(surface.path.clone());
    }

    Ok(written)
}

/// Atomically overwrite a surface file: write to a `.tmp` sibling, then
/// rename over the target.
///
/// Surfaces are pure derived views (CLAUDE.md: *"Source of truth:
/// `.cosmon/state/`. Surfaces are derived views"*). Regeneration is always a
/// full truncate + rewrite — never a merge. The tempfile + rename makes the
/// replacement atomic, so a reader (another `cs` invocation, an editor, a CI
/// gate) never observes a half-written surface, and an interrupted reconcile
/// leaves the previous complete file in place rather than a truncated stub.
///
/// This is the data-plane counterpart of the 2026-05-09 reconcile fix: by
/// regenerating surfaces atomically and never merging them, `cs reconcile`
/// cannot accumulate git-style conflict markers in STATUS.md / ISSUES.md.
fn atomic_write_surface(target: &Path, content: &str) -> std::io::Result<()> {
    let tmp = target.with_extension("md.tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, target)
}

/// Render STATUS.md content (public for dry-run diff).
#[must_use]
pub fn render_status_content(
    fleet: &Fleet,
    molecules: &[MoleculeData],
    formulas: &FormulaMap,
    branding: Branding,
) -> String {
    render_status(fleet, molecules, formulas, branding)
}

/// Render ISSUES.md content (public for dry-run diff).
#[must_use]
pub fn render_issues_content(
    molecules: &[MoleculeData],
    formulas: &FormulaMap,
    branding: Branding,
) -> String {
    render_issues(molecules, formulas, branding)
}

/// Render IDEAS.md content (public for dry-run diff).
#[must_use]
pub fn render_ideas_content(
    molecules: &[MoleculeData],
    formulas: &FormulaMap,
    branding: Branding,
) -> String {
    render_ideas(molecules, formulas, branding)
}

/// Render DELIBERATIONS.md content (public for dry-run diff).
#[must_use]
pub fn render_deliberations_content(
    molecules: &[MoleculeData],
    formulas: &FormulaMap,
    branding: Branding,
) -> String {
    render_deliberations(molecules, formulas, branding)
}

/// Filter molecules by the kinds listed in a `Surface` config.
///
/// Returns all molecules if the surface has no `molecule_kinds` filter.
#[must_use]
pub fn filter_by_surface_kinds<'a>(
    surface: &crate::config::Surface,
    molecules: &'a [MoleculeData],
) -> Vec<&'a MoleculeData> {
    if surface.molecule_kinds.is_empty() {
        molecules.iter().collect()
    } else {
        molecules
            .iter()
            .filter(|m| {
                let kind = m.kind.unwrap_or(MoleculeKind::Task);
                surface.accepts_kind(kind)
            })
            .collect()
    }
}

/// Emit the surface header comment based on branding. The header is an
/// HTML comment that tools can use to detect auto-generated files, but
/// only the `Attributed` mode mentions cosmon — host-native declares the
/// file is generated and points at the source directory without
/// announcing the tool.
fn push_surface_header(out: &mut String, branding: Branding) {
    match branding {
        Branding::Attributed => {
            out.push_str("<!-- Generated by cosmon. Source of truth: .cosmon/ -->\n");
        }
        Branding::HostNative => {
            out.push_str(
                "<!-- auto-generated from .cosmon/ — edit the source, not this file -->\n",
            );
        }
        Branding::None => {}
    }
}

/// Render STATUS.md content.
fn render_status(
    fleet: &Fleet,
    molecules: &[MoleculeData],
    _formulas: &FormulaMap,
    branding: Branding,
) -> String {
    let mut out = String::new();
    push_surface_header(&mut out, branding);
    out.push_str("# Project Status\n\n");

    // Fleet summary.
    let _ = writeln!(out, "## Fleet ({} workers)\n", fleet.workers.len());
    if fleet.workers.is_empty() {
        out.push_str("No workers deployed.\n\n");
    } else {
        out.push_str("| Worker | Role | Status |\n");
        out.push_str("|--------|------|--------|\n");
        let mut workers: Vec<_> = fleet.workers.iter().collect();
        workers.sort_by_key(|(id, _)| id.as_str().to_owned());
        for (id, w) in &workers {
            let _ = writeln!(out, "| {} | {:?} | {} |", id, w.role, w.status);
        }
        out.push('\n');
    }

    // Molecule summary by fleet.
    let _ = writeln!(out, "## Molecules ({} total)\n", molecules.len());
    if molecules.is_empty() {
        out.push_str("No molecules.\n");
    } else {
        // Group by fleet_id.
        let mut by_fleet: std::collections::BTreeMap<String, Vec<&MoleculeData>> =
            std::collections::BTreeMap::new();
        for mol in molecules {
            by_fleet
                .entry(mol.fleet_id.as_str().to_owned())
                .or_default()
                .push(mol);
        }

        for (fleet_name, mols) in &by_fleet {
            let _ = writeln!(out, "### Fleet: {fleet_name}\n");
            out.push_str("| ID | Formula | Status | Step | Worker | Tags | Links |\n");
            out.push_str("|----|---------|--------|------|--------|------|-------|\n");
            for mol in mols {
                let worker = mol.assigned_worker.as_ref().map_or("-", |w| w.as_str());
                let links = summarize_typed_links(&mol.typed_links);
                let tags = summarize_tags(&mol.tags);
                let mut status_str = if mol.status == MoleculeStatus::Collapsed {
                    let reason = mol.collapse_reason.as_deref().unwrap_or("unknown");
                    format!("{} ({})", mol.status, reason)
                } else {
                    mol.status.to_string()
                };
                if let Some(badge) = format_expiry_badge_static(mol.expires_at) {
                    status_str.push_str(" · ");
                    status_str.push_str(&badge);
                }
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} | {} | {} | {} |",
                    mol.id,
                    mol.formula_id,
                    status_str,
                    format_step_progress(mol),
                    worker,
                    tags,
                    links,
                );
            }
            out.push('\n');
        }
    }

    out
}

/// Render ISSUES.md content — pending and running molecules as issues.
fn render_issues(molecules: &[MoleculeData], _formulas: &FormulaMap, branding: Branding) -> String {
    let mut out = String::new();
    push_surface_header(&mut out, branding);
    out.push_str("# Issues\n\n");

    let alive: Vec<_> = molecules
        .iter()
        .filter(|m| {
            m.status.is_alive()
                && !matches!(m.status, cosmon_core::molecule::MoleculeStatus::Frozen)
        })
        .collect();

    if alive.is_empty() {
        out.push_str("No active issues.\n");
    } else {
        for mol in &alive {
            let _ = writeln!(out, "## {} ({})\n", mol.id, mol.status);
            let _ = writeln!(out, "- **Formula**: {}", mol.formula_id);
            let _ = writeln!(out, "- **Progress**: step {}", format_step_progress(mol));
            if let Some(ref worker) = mol.assigned_worker {
                let _ = writeln!(out, "- **Worker**: {worker}");
            } else {
                out.push_str("- **Worker**: *unassigned*\n");
            }
            render_expiry_line(&mut out, mol);
            // Show variables as context.
            render_context_variables(&mut out, &mol.variables);
            // Show typed links (decay/merge/transform relationships).
            render_typed_links(&mut out, &mol.typed_links);
            out.push('\n');
        }
    }

    out
}

/// Format a molecule's step progress as `"N/M"` for display.
///
/// For terminal molecules (Completed / Collapsed), always shows `M/M` —
/// the molecule is past all steps. For active molecules, shows
/// `(current_step+1)/total_steps` (1-indexed for human readability).
///
/// This handles two edge cases correctly:
/// - `cs complete` called on a Pending molecule: `current_step=0` but
///   the molecule is done, so show `M/M` not `1/M`.
/// - `cs evolve` auto-completing after the last step with `current_step=M`:
///   show `M/M` not `M+1/M` (which would exceed the total).
fn format_step_progress(mol: &MoleculeData) -> String {
    if mol.status.is_terminal() {
        format!("{}/{}", mol.total_steps, mol.total_steps)
    } else {
        format!("{}/{}", mol.current_step + 1, mol.total_steps)
    }
}

/// Render an `Expires` bullet when the molecule carries a TTL, using the
/// clock-invariant [`format_expiry_badge_static`] helper so STATUS / ISSUES /
/// GitHub stay lexically in sync *and* the rendered bytes stay a pure function
/// of the store (no `Utc::now()` countdown — see F-C7-1). No-op when the
/// molecule has no `expires_at`.
fn render_expiry_line(out: &mut String, mol: &MoleculeData) {
    if let Some(badge) = format_expiry_badge_static(mol.expires_at) {
        let _ = writeln!(out, "- **Expires**: {badge}");
    }
}

/// Variable keys that identify the molecule and are already shown elsewhere
/// on the surface (e.g., headline, explicit `**Topic**` line). Filtered out
/// of the context-variables table to avoid redundant rendering.
const IDENTITY_KEYS: &[&str] = &["topic", "title", "description"];

/// Render a molecule's variables as a 2-column markdown table, sorted by
/// key for deterministic output and filtered to skip identity keys that
/// appear elsewhere in the surface. Without sorting, `HashMap` iteration
/// order would produce a fresh diff on every `cs reconcile` call, breaking
/// idempotency and causing spurious "unchanged" commits.
fn render_context_variables(
    out: &mut String,
    variables: &std::collections::HashMap<String, String>,
) {
    let mut entries: Vec<(&String, &String)> = variables
        .iter()
        .filter(|(k, _)| !IDENTITY_KEYS.contains(&k.as_str()))
        .collect();
    if entries.is_empty() {
        return;
    }
    entries.sort_by(|a, b| a.0.cmp(b.0));
    out.push_str("\n| Variable | Value |\n| --- | --- |\n");
    for (k, v) in entries {
        let _ = writeln!(out, "| `{k}` | {v} |");
    }
}

/// Render IDEAS.md content — idea molecules as a dedicated surface.
///
/// Ideas are unstructured insights that need cognitive work before becoming
/// actionable tasks or decisions. This surface gives them visibility distinct
/// from the issue tracker.
fn render_ideas(molecules: &[MoleculeData], _formulas: &FormulaMap, branding: Branding) -> String {
    let mut out = String::new();
    push_surface_header(&mut out, branding);
    out.push_str("# Ideas\n\n");

    let alive: Vec<_> = molecules
        .iter()
        .filter(|m| {
            m.status.is_alive()
                && !matches!(m.status, cosmon_core::molecule::MoleculeStatus::Frozen)
        })
        .collect();

    if alive.is_empty() {
        out.push_str("No active ideas.\n");
    } else {
        for mol in &alive {
            let _ = writeln!(
                out,
                "## {} {} ({})\n",
                MoleculeKind::Idea.emoji(),
                mol.id,
                mol.status
            );
            let _ = writeln!(out, "- **Formula**: {}", mol.formula_id);
            let _ = writeln!(out, "- **Progress**: step {}", format_step_progress(mol));
            if let Some(ref worker) = mol.assigned_worker {
                let _ = writeln!(out, "- **Worker**: {worker}");
            } else {
                out.push_str("- **Worker**: *unassigned*\n");
            }
            render_expiry_line(&mut out, mol);
            // Show variables as context.
            render_context_variables(&mut out, &mol.variables);
            // Show typed links (decay/merge/transform relationships).
            render_typed_links(&mut out, &mol.typed_links);
            out.push('\n');
        }
    }

    out
}

/// Render DELIBERATIONS.md content — structured multi-perspective panels.
///
/// Deliberations mobilize a panel of expert personas in parallel and
/// synthesize convergences/divergences. This surface lists alive
/// deliberations with their topic, current step, panel members, and any
/// decay-product links — symmetric to [`render_ideas`].
fn render_deliberations(
    molecules: &[MoleculeData],
    _formulas: &FormulaMap,
    branding: Branding,
) -> String {
    let mut out = String::new();
    push_surface_header(&mut out, branding);
    out.push_str("# Deliberations\n\n");

    let alive: Vec<_> = molecules
        .iter()
        .filter(|m| {
            m.status.is_alive()
                && !matches!(m.status, cosmon_core::molecule::MoleculeStatus::Frozen)
        })
        .collect();

    if alive.is_empty() {
        out.push_str("No active deliberations.\n");
    } else {
        for mol in &alive {
            let _ = writeln!(
                out,
                "## {} {} ({})\n",
                MoleculeKind::Deliberation.emoji(),
                mol.id,
                mol.status
            );
            let _ = writeln!(out, "- **Formula**: {}", mol.formula_id);
            let _ = writeln!(out, "- **Progress**: step {}", format_step_progress(mol));
            if let Some(ref worker) = mol.assigned_worker {
                let _ = writeln!(out, "- **Worker**: {worker}");
            } else {
                out.push_str("- **Worker**: *unassigned*\n");
            }
            render_expiry_line(&mut out, mol);
            // Topic and panel are variables the deep-think formula relies on.
            if let Some(topic) = mol.variables.get("topic") {
                let _ = writeln!(out, "- **Topic**: {topic}");
            }
            if let Some(panel) = mol.variables.get("panel") {
                let _ = writeln!(out, "- **Panel**: {panel}");
            }
            // Remaining variables as context (sorted for determinism).
            render_context_variables(&mut out, &mol.variables);
            // Typed links — decay products (child tasks/ideas) and merges.
            render_typed_links(&mut out, &mol.typed_links);
            out.push('\n');
        }
    }

    out
}

/// Render an ADR index from the docs/adr/ directory.
fn render_adr_index(project_root: &Path, adr_dir: &str, branding: Branding) -> String {
    let mut out = String::new();
    match branding {
        Branding::Attributed => {
            out.push_str("<!-- Generated by cosmon. Source of truth: docs/adr/*.md -->\n");
        }
        Branding::HostNative => {
            out.push_str(
                "<!-- auto-generated from docs/adr/ — edit the source, not this file -->\n",
            );
        }
        Branding::None => {}
    }
    out.push_str("# Architecture Decision Records\n\n");

    let adr_path = project_root.join(adr_dir);
    if !adr_path.is_dir() {
        out.push_str("No ADRs yet.\n");
        return out;
    }

    let mut files: Vec<_> = std::fs::read_dir(&adr_path)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| name != "INDEX.md") // Don't list ourselves.
        .collect();
    files.sort();

    if files.is_empty() {
        out.push_str("No ADRs yet.\n");
    } else {
        out.push_str("| ADR | File |\n");
        out.push_str("|-----|------|\n");
        for file in &files {
            let name = file.trim_end_matches(".md");
            let _ = writeln!(out, "| {name} | [{file}]({adr_dir}{file}) |");
        }
    }
    out.push('\n');
    out
}

/// Render typed links as markdown list items.
///
/// Appends relationship lines to the output string.
fn render_typed_links(out: &mut String, links: &[MoleculeLink]) {
    if links.is_empty() {
        return;
    }
    for link in links {
        match link {
            MoleculeLink::DecayedFrom { id } => {
                let _ = writeln!(out, "- \u{1f4a5} Decayed from `{id}`");
            }
            MoleculeLink::DecayProduct { id } => {
                let _ = writeln!(out, "- \u{1f331} Decay product: `{id}`");
            }
            MoleculeLink::MergedFrom { ids } => {
                let refs: Vec<_> = ids.iter().map(|id| format!("`{id}`")).collect();
                let _ = writeln!(out, "- \u{1f500} Merged from: {}", refs.join(", "));
            }
            MoleculeLink::MergedInto { id } => {
                let _ = writeln!(out, "- \u{1f500} Merged into `{id}`");
            }
            MoleculeLink::TransformedFrom { kind } => {
                let _ = writeln!(out, "- \u{1f504} Transformed from {kind}");
            }
            MoleculeLink::Blocks { target } => {
                let _ = writeln!(out, "- \u{26d4} Blocks `{target}`");
            }
            MoleculeLink::BlockedBy { source } => {
                let _ = writeln!(out, "- \u{23f3} Blocked by `{source}`");
            }
            MoleculeLink::Entangled { target } => {
                let _ = writeln!(out, "- \u{1f517} {target}");
            }
            MoleculeLink::CrossGalaxyBlocks { target } => {
                let _ = writeln!(out, "- \u{26d4}\u{1f30c} Blocks `{target}` (cross-galaxy)");
            }
            MoleculeLink::CrossGalaxyBlockedBy { source } => {
                let _ = writeln!(
                    out,
                    "- \u{23f3}\u{1f30c} Blocked by `{source}` (cross-galaxy)"
                );
            }
            _ => {}
        }
    }
}

/// Summarize a molecule's tags as a compact, deterministic cell value.
fn summarize_tags(tags: &std::collections::BTreeSet<cosmon_core::tag::Tag>) -> String {
    if tags.is_empty() {
        return "-".to_owned();
    }
    tags.iter()
        .map(|t| format!("`{t}`"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Summarize typed links as a compact string for table cells.
///
/// Returns a short representation like `←idea-001` or `→task-001, →task-002`.
fn summarize_typed_links(links: &[MoleculeLink]) -> String {
    if links.is_empty() {
        return "-".to_string();
    }
    let parts: Vec<String> = links
        .iter()
        .map(|link| match link {
            MoleculeLink::DecayedFrom { id } => format!("\u{2190}{id}"),
            MoleculeLink::DecayProduct { id } | MoleculeLink::MergedInto { id } => {
                format!("\u{2192}{id}")
            }
            MoleculeLink::MergedFrom { ids } => {
                let refs: Vec<_> = ids.iter().map(MoleculeId::as_str).collect();
                format!("\u{2190}[{}]", refs.join(","))
            }
            MoleculeLink::TransformedFrom { kind: k } => format!("\u{1f504}{k}"),
            MoleculeLink::Blocks { target } => format!("\u{26d4}{target}"),
            MoleculeLink::BlockedBy { source } => format!("\u{23f3}{source}"),
            MoleculeLink::Entangled { target } => format!("\u{1f517}{target}"),
            MoleculeLink::CrossGalaxyBlocks { target } => format!("\u{26d4}\u{1f30c}{target}"),
            MoleculeLink::CrossGalaxyBlockedBy { source } => {
                format!("\u{23f3}\u{1f30c}{source}")
            }
            _ => String::new(),
        })
        .collect();
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
    use cosmon_core::kind::MoleculeKind;
    use cosmon_state::Fleet;

    /// Empty [`FormulaMap`] for tests that don't exercise the formula lookup
    /// path. Test callers use `&fm()` at every render invocation to keep the
    /// signatures honest without cluttering each test with `HashMap::new()`.
    fn fm() -> FormulaMap {
        FormulaMap::new()
    }

    /// Empty [`DeclarationMap`] — symmetric helper to [`fm`] for tests that
    /// do not exercise the declaration lookup path.
    fn dm() -> DeclarationMap {
        DeclarationMap::new()
    }

    fn test_molecule(id: &str, status: MoleculeStatus) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("mol-task-work").unwrap(),
            status,
            variables: std::collections::HashMap::new(),
            assigned_worker: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            total_steps: 2,
            current_step: 0,
            completed_steps: vec![],
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: vec![],
            kind: Some(MoleculeKind::Task),
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: vec![],
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
        }
    }

    #[test]
    fn test_render_status_empty() {
        let content = render_status(&Fleet::default(), &[], &fm(), Branding::HostNative);
        assert!(content.contains("# Project Status"));
        assert!(content.contains("No workers deployed"));
        assert!(content.contains("No molecules"));
    }

    /// Regression: `HashMap` iteration order caused non-deterministic
    /// rendering of the `- **Context**:` line, producing spurious diffs
    /// on every `cs reconcile` call. The fix sorts variables by key.
    #[test]
    fn test_render_context_variables_is_deterministic() {
        let mut mol = test_molecule("task-20260409-rndm", MoleculeStatus::Running);
        mol.variables.insert("z_last".to_owned(), "1".to_owned());
        mol.variables.insert("a_first".to_owned(), "2".to_owned());
        mol.variables.insert("m_middle".to_owned(), "3".to_owned());
        mol.variables
            .insert("topic".to_owned(), "deterministic".to_owned());

        // Render ten times and confirm identical output each time.
        let baseline = render_issues(&[mol.clone()], &fm(), Branding::HostNative);
        for _ in 0..10 {
            let rendered = render_issues(&[mol.clone()], &fm(), Branding::HostNative);
            assert_eq!(
                rendered, baseline,
                "render_issues must be deterministic across HashMap iterations"
            );
        }

        // Verify the table header and sorted rows appear, with the
        // identity key `topic` filtered out (it's shown elsewhere).
        assert!(baseline.contains("| Variable | Value |"));
        assert!(baseline.contains("| `a_first` | 2 |"));
        assert!(baseline.contains("| `m_middle` | 3 |"));
        assert!(baseline.contains("| `z_last` | 1 |"));
        assert!(
            !baseline.contains("| `topic` |"),
            "identity key `topic` must be filtered from the variables table"
        );
        // Rows are in alphabetical order.
        let a = baseline.find("| `a_first`").unwrap();
        let m = baseline.find("| `m_middle`").unwrap();
        let z = baseline.find("| `z_last`").unwrap();
        assert!(a < m && m < z, "variables must be sorted by key");
    }

    #[test]
    fn test_format_step_progress_running() {
        let mut mol = test_molecule("task-20260409-run", MoleculeStatus::Running);
        mol.current_step = 0;
        mol.total_steps = 3;
        assert_eq!(format_step_progress(&mol), "1/3");

        mol.current_step = 2;
        assert_eq!(format_step_progress(&mol), "3/3");
    }

    #[test]
    fn test_format_step_progress_completed_caps_at_total() {
        // Regression: cs complete/cs evolve set current_step = total_steps on
        // completion. Without capping, display would show "M+1/M".
        let mut mol = test_molecule("task-20260409-done", MoleculeStatus::Completed);
        mol.current_step = 2;
        mol.total_steps = 2;
        assert_eq!(format_step_progress(&mol), "2/2");
    }

    #[test]
    fn test_format_step_progress_completed_from_pending() {
        // Regression: `cs complete` on a Pending molecule leaves current_step
        // at whatever the domain model left it (could be 0). Render must
        // still show M/M, not 1/M.
        let mut mol = test_molecule("task-20260409-skip", MoleculeStatus::Completed);
        mol.current_step = 0;
        mol.total_steps = 3;
        assert_eq!(format_step_progress(&mol), "3/3");
    }

    #[test]
    fn test_format_step_progress_collapsed() {
        let mut mol = test_molecule("task-20260409-ded", MoleculeStatus::Collapsed);
        mol.current_step = 1;
        mol.total_steps = 2;
        assert_eq!(format_step_progress(&mol), "2/2");
    }

    /// Same regression guard for `render_ideas`.
    #[test]
    fn test_render_ideas_context_is_deterministic() {
        let mut mol = test_molecule("idea-20260409-rndm", MoleculeStatus::Pending);
        mol.kind = Some(MoleculeKind::Idea);
        mol.variables.insert("zz".to_owned(), "1".to_owned());
        mol.variables.insert("aa".to_owned(), "2".to_owned());

        let baseline = render_ideas(&[mol.clone()], &fm(), Branding::HostNative);
        for _ in 0..10 {
            assert_eq!(
                render_ideas(&[mol.clone()], &fm(), Branding::HostNative),
                baseline
            );
        }
    }

    /// A fixed UTC instant for clock-free expiry tests. Using an absolute
    /// deadline (not `Utc::now() ± n`) is deliberate: the persisted-surface
    /// badge must be a pure function of `expires_at`, so the fixture pins the
    /// exact date the badge is expected to carry.
    fn ts(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn test_render_status_surfaces_expiry_date_badge() {
        let mut mol = test_molecule("task-20260412-fut", MoleculeStatus::Pending);
        mol.expires_at = Some(ts("2026-07-02T23:59:59Z"));
        let content = render_status(&Fleet::default(), &[mol], &fm(), Branding::HostNative);
        assert!(
            content.contains("\u{1f4c5} 2026-07-02"),
            "status table should render the 📅 absolute-date expiry badge, got:\n{content}"
        );
        // F-C7-1 (delib-20260711-9928): the persisted surface must NOT embed a
        // wall-clock-relative countdown, or `cs reconcile --check` flips
        // PASS→FAIL on an unchanged store. Reverting the fix (badge with
        // `Utc::now()`) reddens this.
        assert!(
            !content.contains("left") && !content.contains("ago"),
            "persisted surface must not embed a wall-clock countdown, got:\n{content}"
        );
    }

    #[test]
    fn test_render_issues_surfaces_expiry_date_badge() {
        let mut mol = test_molecule("task-20260412-exp", MoleculeStatus::Pending);
        mol.expires_at = Some(ts("2026-04-09T23:59:59Z"));
        let content = render_issues(&[mol], &fm(), Branding::HostNative);
        assert!(
            content.contains("- **Expires**: \u{1f4c5} 2026-04-09"),
            "issues entry should render the 📅 absolute-date expiry badge, got:\n{content}"
        );
        // F-C7-1: no `expired Nd ago` countdown in the hashable surface bytes.
        assert!(
            !content.contains("expired") && !content.contains("ago"),
            "persisted surface must not embed a wall-clock countdown, got:\n{content}"
        );
    }

    /// F-C7-1 regression (delib-20260711-9928): `render_*_content` for a
    /// TTL'd molecule must be a pure function of the store — rendering the
    /// *same* molecules at two wall-clock instants ≥1 day apart must produce
    /// byte-identical output, so `detect_divergence` reports `UpToDate` and
    /// `cs reconcile --check` stays green on an unchanged store.
    ///
    /// The badge is now clock-invariant, so the two renders below are
    /// literally the same call; the assertion encodes the *contract*. The
    /// companion `!contains("left"/"ago")` guards above are what actually
    /// redden when the fix is reverted to a `Utc::now()` countdown.
    #[test]
    fn test_render_ttl_surface_is_clock_invariant() {
        use crate::snapshot::{
            detect_divergence, record_projection, ProjectionSnapshot, SurfaceDivergence,
        };

        let mut mol = test_molecule("task-20260412-ttl", MoleculeStatus::Pending);
        mol.expires_at = Some(ts("2026-09-01T23:59:59Z"));

        // "Instant T1" projection → snapshot baseline.
        let render_t1 = render_status(
            &Fleet::default(),
            &[mol.clone()],
            &fm(),
            Branding::HostNative,
        );
        let mut snap = ProjectionSnapshot::default();
        record_projection(&mut snap, "STATUS.md", &render_t1);
        let snapshot_hash = snap.surfaces["STATUS.md"].content_hash.clone();

        // "Instant T2" (≥1 day later) render of the *unchanged* store.
        let render_t2 = render_status(&Fleet::default(), &[mol], &fm(), Branding::HostNative);

        assert_eq!(
            render_t1, render_t2,
            "TTL'd surface render must not depend on the wall clock"
        );
        assert_eq!(
            detect_divergence(Some(&snapshot_hash), &render_t1, &render_t2),
            SurfaceDivergence::UpToDate,
            "unchanged store must stay UpToDate across a clock advance (--check must not flip)"
        );
    }

    #[test]
    fn test_render_issues_empty() {
        let content = render_issues(&[], &fm(), Branding::HostNative);
        assert!(content.contains("# Issues"));
        assert!(content.contains("No active issues"));
    }

    #[test]
    fn test_render_status_has_generated_header() {
        // Default branding is HostNative: header declares the file is
        // auto-generated and points at the source directory, without
        // mentioning cosmon by name.
        let content = render_status(&Fleet::default(), &[], &fm(), Branding::HostNative);
        assert!(
            content.starts_with("<!-- auto-generated from .cosmon/"),
            "host-native header should lead with an auto-generated notice"
        );
        assert!(!content.contains("Generated by cosmon"));
    }

    #[test]
    fn test_render_status_attributed_header_keeps_cosmon_vocabulary() {
        // Explicit opt-in to the legacy attributed header.
        let content = render_status(&Fleet::default(), &[], &fm(), Branding::Attributed);
        assert!(content.starts_with("<!-- Generated by cosmon"));
    }

    #[test]
    fn test_render_status_none_branding_has_no_header() {
        let content = render_status(&Fleet::default(), &[], &fm(), Branding::None);
        assert!(content.starts_with("# Project Status"));
    }

    #[test]
    fn test_render_status_shows_collapse_reason() {
        let mut mol = test_molecule("task-20260407-x001", MoleculeStatus::Collapsed);
        mol.collapse_reason = Some("lint failed".to_string());
        let content = render_status(&Fleet::default(), &[mol], &fm(), Branding::HostNative);
        assert!(content.contains("collapsed (lint failed)"));
    }

    #[test]
    fn test_render_status_shows_typed_links() {
        let mut mol = test_molecule("task-20260407-x002", MoleculeStatus::Running);
        mol.typed_links = vec![MoleculeLink::DecayedFrom {
            id: MoleculeId::new("idea-20260407-0001").unwrap(),
        }];
        let content = render_status(&Fleet::default(), &[mol], &fm(), Branding::HostNative);
        assert!(content.contains("\u{2190}idea-20260407-0001"));
    }

    #[test]
    fn test_render_issues_shows_typed_links() {
        let mut mol = test_molecule("task-20260407-x003", MoleculeStatus::Running);
        mol.typed_links = vec![MoleculeLink::DecayedFrom {
            id: MoleculeId::new("idea-20260407-0001").unwrap(),
        }];
        let content = render_issues(&[mol], &fm(), Branding::HostNative);
        assert!(content.contains("Decayed from `idea-20260407-0001`"));
    }

    #[test]
    fn test_summarize_typed_links_empty() {
        assert_eq!(summarize_typed_links(&[]), "-");
    }

    #[test]
    fn test_summarize_typed_links_decay_products() {
        let links = vec![
            MoleculeLink::DecayProduct {
                id: MoleculeId::new("task-20260407-0001").unwrap(),
            },
            MoleculeLink::DecayProduct {
                id: MoleculeId::new("task-20260407-0002").unwrap(),
            },
        ];
        let summary = summarize_typed_links(&links);
        assert!(summary.contains("\u{2192}task-20260407-0001"));
        assert!(summary.contains("\u{2192}task-20260407-0002"));
    }

    #[test]
    fn test_summarize_typed_links_merged_from() {
        let links = vec![MoleculeLink::MergedFrom {
            ids: vec![
                MoleculeId::new("task-20260407-0001").unwrap(),
                MoleculeId::new("task-20260407-0002").unwrap(),
            ],
        }];
        let summary = summarize_typed_links(&links);
        assert!(summary.contains("\u{2190}[task-20260407-0001,task-20260407-0002]"));
    }

    fn test_molecule_with_kind(
        id: &str,
        status: MoleculeStatus,
        kind: MoleculeKind,
    ) -> MoleculeData {
        let mut mol = test_molecule(id, status);
        mol.kind = Some(kind);
        mol
    }

    #[test]
    fn test_filter_by_surface_kinds_empty_accepts_all() {
        let surface = crate::config::Surface {
            referent: "project.issues".to_string(),
            kind: crate::config::SurfaceKind::Markdown,
            path: "ISSUES.md".to_string(),
            template: None,
            repo: None,
            labels: vec![],
            molecule_kinds: vec![],
            branding: Branding::HostNative,
        };
        let molecules = vec![
            test_molecule_with_kind(
                "task-20260407-0001",
                MoleculeStatus::Running,
                MoleculeKind::Task,
            ),
            test_molecule_with_kind(
                "idea-20260407-0001",
                MoleculeStatus::Running,
                MoleculeKind::Idea,
            ),
            test_molecule_with_kind(
                "signal-20260407-0001",
                MoleculeStatus::Running,
                MoleculeKind::Signal,
            ),
        ];
        let filtered = filter_by_surface_kinds(&surface, &molecules);
        assert_eq!(filtered.len(), 3);
    }

    #[test]
    fn test_filter_by_surface_kinds_task_and_issue_only() {
        let surface = crate::config::Surface {
            referent: "project.issues".to_string(),
            kind: crate::config::SurfaceKind::Markdown,
            path: "ISSUES.md".to_string(),
            template: None,
            repo: None,
            labels: vec![],
            molecule_kinds: vec![MoleculeKind::Task, MoleculeKind::Issue],
            branding: Branding::HostNative,
        };
        let molecules = vec![
            test_molecule_with_kind(
                "task-20260407-0001",
                MoleculeStatus::Running,
                MoleculeKind::Task,
            ),
            test_molecule_with_kind(
                "idea-20260407-0001",
                MoleculeStatus::Running,
                MoleculeKind::Idea,
            ),
            test_molecule_with_kind(
                "issue-20260407-0001",
                MoleculeStatus::Running,
                MoleculeKind::Issue,
            ),
            test_molecule_with_kind(
                "signal-20260407-0001",
                MoleculeStatus::Running,
                MoleculeKind::Signal,
            ),
        ];
        let filtered = filter_by_surface_kinds(&surface, &molecules);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].id.as_str(), "task-20260407-0001");
        assert_eq!(filtered[1].id.as_str(), "issue-20260407-0001");
    }

    #[test]
    fn test_filter_by_surface_kinds_legacy_molecule_defaults_to_task() {
        let surface = crate::config::Surface {
            referent: "project.issues".to_string(),
            kind: crate::config::SurfaceKind::Markdown,
            path: "ISSUES.md".to_string(),
            template: None,
            repo: None,
            labels: vec![],
            molecule_kinds: vec![MoleculeKind::Task],
            branding: Branding::HostNative,
        };
        let mut mol = test_molecule("legacy-20260407-0001", MoleculeStatus::Running);
        mol.kind = None; // Legacy molecule with no kind field
        let molecules = vec![mol];
        let filtered = filter_by_surface_kinds(&surface, &molecules);
        assert_eq!(
            filtered.len(),
            1,
            "legacy molecule (kind=None) should default to Task"
        );
    }

    #[test]
    fn test_render_ideas_empty() {
        let content = render_ideas(&[], &fm(), Branding::HostNative);
        assert!(content.contains("# Ideas"));
        assert!(content.contains("No active ideas"));
    }

    #[test]
    fn test_render_ideas_has_generated_header() {
        let content = render_ideas(&[], &fm(), Branding::HostNative);
        assert!(content.starts_with("<!-- auto-generated from .cosmon/"));
        assert!(!content.contains("Generated by cosmon"));
    }

    #[test]
    fn test_render_ideas_attributed_header() {
        let content = render_ideas(&[], &fm(), Branding::Attributed);
        assert!(content.starts_with("<!-- Generated by cosmon"));
    }

    #[test]
    fn test_render_ideas_shows_running_idea() {
        let mol = test_molecule_with_kind(
            "idea-20260407-0001",
            MoleculeStatus::Running,
            MoleculeKind::Idea,
        );
        let content = render_ideas(&[mol], &fm(), Branding::HostNative);
        assert!(content.contains("idea-20260407-0001"));
        assert!(content.contains("💡"));
        assert!(content.contains("running"));
    }

    #[test]
    fn test_render_ideas_excludes_completed() {
        let mol = test_molecule_with_kind(
            "idea-20260407-0001",
            MoleculeStatus::Completed,
            MoleculeKind::Idea,
        );
        let content = render_ideas(&[mol], &fm(), Branding::HostNative);
        assert!(content.contains("No active ideas"));
    }

    #[test]
    fn test_render_ideas_excludes_frozen() {
        let mol = test_molecule_with_kind(
            "idea-20260407-0001",
            MoleculeStatus::Frozen,
            MoleculeKind::Idea,
        );
        let content = render_ideas(&[mol], &fm(), Branding::HostNative);
        assert!(content.contains("No active ideas"));
    }

    // --- Deliberations surface -----------------------------------------

    #[test]
    fn test_render_deliberations_empty() {
        let content = render_deliberations(&[], &fm(), Branding::HostNative);
        assert!(content.contains("# Deliberations"));
        assert!(content.contains("No active deliberations"));
    }

    #[test]
    fn test_render_deliberations_has_generated_header() {
        let content = render_deliberations(&[], &fm(), Branding::HostNative);
        assert!(content.starts_with("<!-- auto-generated from .cosmon/"));
        assert!(!content.contains("Generated by cosmon"));
    }

    #[test]
    fn test_render_deliberations_attributed_header() {
        let content = render_deliberations(&[], &fm(), Branding::Attributed);
        assert!(content.starts_with("<!-- Generated by cosmon"));
    }

    #[test]
    fn test_render_deliberations_shows_running_deliberation() {
        let mut mol = test_molecule_with_kind(
            "delib-20260409-0001",
            MoleculeStatus::Running,
            MoleculeKind::Deliberation,
        );
        mol.variables
            .insert("topic".to_owned(), "cs watch review".to_owned());
        mol.variables
            .insert("panel".to_owned(), "feynman,jobs,wheeler".to_owned());
        let content = render_deliberations(&[mol], &fm(), Branding::HostNative);
        assert!(content.contains("delib-20260409-0001"));
        assert!(content.contains("🧠"));
        assert!(content.contains("running"));
        assert!(content.contains("**Topic**: cs watch review"));
        assert!(content.contains("**Panel**: feynman,jobs,wheeler"));
    }

    #[test]
    fn test_render_deliberations_excludes_completed() {
        let mol = test_molecule_with_kind(
            "delib-20260409-0002",
            MoleculeStatus::Completed,
            MoleculeKind::Deliberation,
        );
        let content = render_deliberations(&[mol], &fm(), Branding::HostNative);
        assert!(content.contains("No active deliberations"));
    }

    #[test]
    fn test_render_deliberations_excludes_frozen() {
        let mol = test_molecule_with_kind(
            "delib-20260409-0003",
            MoleculeStatus::Frozen,
            MoleculeKind::Deliberation,
        );
        let content = render_deliberations(&[mol], &fm(), Branding::HostNative);
        assert!(content.contains("No active deliberations"));
    }

    #[test]
    fn test_render_deliberations_shows_decay_products() {
        let mut mol = test_molecule_with_kind(
            "delib-20260409-0004",
            MoleculeStatus::Running,
            MoleculeKind::Deliberation,
        );
        mol.typed_links = vec![MoleculeLink::DecayProduct {
            id: MoleculeId::new("task-20260409-0001").unwrap(),
        }];
        let content = render_deliberations(&[mol], &fm(), Branding::HostNative);
        assert!(content.contains("Decay product: `task-20260409-0001`"));
    }

    /// End-to-end: `project_surfaces` writes DELIBERATIONS.md and is
    /// idempotent for molecules of kind Deliberation. Guards `HashMap`
    /// ordering regressions and proves the surface is wired into the
    /// config referent table.
    #[test]
    fn test_project_surfaces_deliberations_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let config = SurfaceConfig::parse(
            r#"
            [[surface]]
            referent = "project.deliberations"
            kind = "markdown"
            path = "DELIBERATIONS.md"
            molecule_kinds = ["deliberation"]
            "#,
        )
        .unwrap();

        let mut mol = test_molecule_with_kind(
            "delib-20260409-idem",
            MoleculeStatus::Running,
            MoleculeKind::Deliberation,
        );
        mol.variables
            .insert("topic".to_owned(), "reconcile discipline".to_owned());
        mol.variables
            .insert("panel".to_owned(), "feynman,jobs,wheeler".to_owned());
        mol.variables.insert("zz".to_owned(), "1".to_owned());
        mol.variables.insert("aa".to_owned(), "2".to_owned());

        let fleet = Fleet::default();

        project_surfaces(&config, root, &fleet, &[mol.clone()], &fm(), &dm()).unwrap();
        let first = std::fs::read_to_string(root.join("DELIBERATIONS.md")).unwrap();
        for _ in 0..5 {
            project_surfaces(&config, root, &fleet, &[mol.clone()], &fm(), &dm()).unwrap();
            let again = std::fs::read_to_string(root.join("DELIBERATIONS.md")).unwrap();
            assert_eq!(first, again, "DELIBERATIONS.md must be idempotent");
        }
        assert!(first.contains("delib-20260409-idem"));
        assert!(first.contains("🧠"));
        assert!(first.contains("**Topic**: reconcile discipline"));
        assert!(first.contains("**Panel**: feynman,jobs,wheeler"));
    }

    #[test]
    fn test_render_deliberations_context_is_deterministic() {
        let mut mol = test_molecule_with_kind(
            "delib-20260409-rndm",
            MoleculeStatus::Running,
            MoleculeKind::Deliberation,
        );
        mol.variables.insert("zz".to_owned(), "1".to_owned());
        mol.variables.insert("aa".to_owned(), "2".to_owned());
        let baseline = render_deliberations(&[mol.clone()], &fm(), Branding::HostNative);
        for _ in 0..10 {
            assert_eq!(
                render_deliberations(&[mol.clone()], &fm(), Branding::HostNative),
                baseline
            );
        }
    }

    #[test]
    fn test_render_ideas_shows_typed_links() {
        let mut mol = test_molecule_with_kind(
            "idea-20260407-0001",
            MoleculeStatus::Running,
            MoleculeKind::Idea,
        );
        mol.typed_links = vec![MoleculeLink::DecayProduct {
            id: MoleculeId::new("task-20260407-0001").unwrap(),
        }];
        let content = render_ideas(&[mol], &fm(), Branding::HostNative);
        assert!(content.contains("Decay product: `task-20260407-0001`"));
    }

    #[test]
    fn test_render_typed_links_all_variants() {
        let links = vec![
            MoleculeLink::DecayedFrom {
                id: MoleculeId::new("idea-20260407-0001").unwrap(),
            },
            MoleculeLink::DecayProduct {
                id: MoleculeId::new("task-20260407-0001").unwrap(),
            },
            MoleculeLink::MergedInto {
                id: MoleculeId::new("decision-20260407-0001").unwrap(),
            },
            MoleculeLink::TransformedFrom {
                kind: MoleculeKind::Idea,
            },
            MoleculeLink::Blocks {
                target: MoleculeId::new("task-20260409-dstr").unwrap(),
            },
            MoleculeLink::BlockedBy {
                source: MoleculeId::new("task-20260409-ustr").unwrap(),
            },
            MoleculeLink::Entangled {
                target: "https://example.com".to_string(),
            },
        ];
        let mut out = String::new();
        render_typed_links(&mut out, &links);
        assert!(out.contains("Decayed from `idea-20260407-0001`"));
        assert!(out.contains("Decay product: `task-20260407-0001`"));
        assert!(out.contains("Merged into `decision-20260407-0001`"));
        assert!(out.contains("Transformed from idea"));
        assert!(out.contains("Blocks `task-20260409-dstr`"));
        assert!(out.contains("Blocked by `task-20260409-ustr`"));
        assert!(out.contains("https://example.com"));
    }

    // --- Blocks / BlockedBy rendering (ADR-016 Phase 1, task #23) -------

    #[test]
    fn test_render_typed_links_blocks_emoji() {
        let links = vec![MoleculeLink::Blocks {
            target: MoleculeId::new("task-20260409-down").unwrap(),
        }];
        let mut out = String::new();
        render_typed_links(&mut out, &links);
        // ⛔ is the U+26D4 no-entry sign — the same we use in summarize.
        assert!(out.contains("\u{26d4}"));
        assert!(out.contains("Blocks `task-20260409-down`"));
    }

    #[test]
    fn test_render_typed_links_blocked_by_emoji() {
        let links = vec![MoleculeLink::BlockedBy {
            source: MoleculeId::new("task-20260409-up").unwrap(),
        }];
        let mut out = String::new();
        render_typed_links(&mut out, &links);
        // ⏳ hourglass — the molecule is waiting.
        assert!(out.contains("\u{23f3}"));
        assert!(out.contains("Blocked by `task-20260409-up`"));
    }

    #[test]
    fn test_summarize_typed_links_blocks() {
        let links = vec![MoleculeLink::Blocks {
            target: MoleculeId::new("task-20260409-down").unwrap(),
        }];
        let summary = summarize_typed_links(&links);
        assert!(summary.contains("\u{26d4}task-20260409-down"));
    }

    #[test]
    fn test_summarize_typed_links_blocked_by() {
        let links = vec![MoleculeLink::BlockedBy {
            source: MoleculeId::new("task-20260409-up").unwrap(),
        }];
        let summary = summarize_typed_links(&links);
        assert!(summary.contains("\u{23f3}task-20260409-up"));
    }

    #[test]
    fn test_summarize_typed_links_blocks_pair() {
        // Both directions on the same molecule: one side is upstream, one
        // side is downstream. Summary string must list both without ambiguity.
        let links = vec![
            MoleculeLink::BlockedBy {
                source: MoleculeId::new("task-20260409-uu").unwrap(),
            },
            MoleculeLink::Blocks {
                target: MoleculeId::new("task-20260409-dd").unwrap(),
            },
        ];
        let summary = summarize_typed_links(&links);
        assert!(summary.contains("\u{23f3}task-20260409-uu"));
        assert!(summary.contains("\u{26d4}task-20260409-dd"));
    }

    #[test]
    fn test_render_status_shows_blocks_link() {
        let mut mol = test_molecule("task-20260409-blok", MoleculeStatus::Running);
        mol.typed_links = vec![MoleculeLink::Blocks {
            target: MoleculeId::new("task-20260409-targ").unwrap(),
        }];
        let content = render_status(&Fleet::default(), &[mol], &fm(), Branding::HostNative);
        // Table cell uses the compact summary form.
        assert!(content.contains("\u{26d4}task-20260409-targ"));
    }

    #[test]
    fn test_render_issues_shows_blocked_by_link() {
        let mut mol = test_molecule("task-20260409-wait", MoleculeStatus::Running);
        mol.typed_links = vec![MoleculeLink::BlockedBy {
            source: MoleculeId::new("task-20260409-srce").unwrap(),
        }];
        let content = render_issues(&[mol], &fm(), Branding::HostNative);
        // ISSUES.md uses the full line form.
        assert!(content.contains("Blocked by `task-20260409-srce`"));
    }

    #[test]
    fn test_render_ideas_shows_blocking_link() {
        let mut mol = test_molecule_with_kind(
            "idea-20260409-bkng",
            MoleculeStatus::Pending,
            MoleculeKind::Idea,
        );
        mol.typed_links = vec![MoleculeLink::Blocks {
            target: MoleculeId::new("task-20260409-imp").unwrap(),
        }];
        let content = render_ideas(&[mol], &fm(), Branding::HostNative);
        assert!(content.contains("Blocks `task-20260409-imp`"));
    }

    /// Regression: the `typed_links` vector is iterated in-order by the
    /// render path. Without a stable source order, render would flicker.
    /// (The `Vec` ordering is already stable — this test guards against a
    /// future refactor to `HashMap`.)
    #[test]
    fn test_render_typed_links_order_preserved() {
        let mut mol = test_molecule("task-20260409-ord1", MoleculeStatus::Running);
        mol.typed_links = vec![
            MoleculeLink::BlockedBy {
                source: MoleculeId::new("task-20260409-001a").unwrap(),
            },
            MoleculeLink::BlockedBy {
                source: MoleculeId::new("task-20260409-002b").unwrap(),
            },
            MoleculeLink::Blocks {
                target: MoleculeId::new("task-20260409-003c").unwrap(),
            },
        ];
        let first = render_issues(std::slice::from_ref(&mol), &fm(), Branding::HostNative);
        for _ in 0..10 {
            assert_eq!(
                render_issues(std::slice::from_ref(&mol), &fm(), Branding::HostNative),
                first
            );
        }
    }
}
