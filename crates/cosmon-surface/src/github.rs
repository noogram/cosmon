// SPDX-License-Identifier: AGPL-3.0-only

//! GitHub Issues surface adapter.
//!
//! Projects molecules onto GitHub Issues using the `gh` CLI.
//! Only molecules with projectable kinds (Issue, Task, Idea) are synced.
//! Decisions and Signals are skipped.

use std::fmt::Write;
use std::process::Output;

use cosmon_core::expiry::format_expiry_badge_static;
use cosmon_core::formula::Formula;
use cosmon_core::interaction::MoleculeLink;
use cosmon_core::kind::MoleculeKind;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_state::MoleculeData;

use crate::config::{Branding, Surface};
use crate::render::{DeclarationMap, FormulaMap};

/// Interpret the result of a probe command (e.g. `gh --version`,
/// `gh auth status`) and translate it into a user-actionable error.
///
/// Split from the real probes so unit tests can exercise every branch
/// without spawning a real `gh` binary.
fn interpret_probe(
    result: std::io::Result<Output>,
    success_is_ok: bool,
    error_msg: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    match result {
        Ok(out) if out.status.success() == success_is_ok => Ok(()),
        _ => Err(error_msg.into()),
    }
}

/// Verify that the `gh` CLI binary is installed and invokable.
///
/// Previously callers only checked the process launched (I/O error). This is
/// not enough: a broken `gh` install can launch but return non-zero on
/// `--version`. We now also require a successful exit.
fn check_gh_available() -> Result<(), Box<dyn std::error::Error>> {
    interpret_probe(
        std::process::Command::new("gh").arg("--version").output(),
        true,
        "gh CLI not found. Install GitHub CLI: https://cli.github.com/",
    )
}

/// Verify that the local `gh` CLI is authenticated.
///
/// `gh --version` succeeds even when the user has never run `gh auth login`,
/// so the sync would previously fail deep inside `gh issue create` with a
/// cryptic "HTTP 401" or similar. We eagerly check `gh auth status` and
/// return an actionable error telling the user exactly how to fix it.
fn check_gh_authenticated() -> Result<(), Box<dyn std::error::Error>> {
    interpret_probe(
        std::process::Command::new("gh")
            .args(["auth", "status"])
            .output(),
        true,
        "gh CLI is installed but not authenticated. Run: gh auth login",
    )
}

/// Check that a `gh` CLI invocation exited cleanly.
///
/// `gh` is chatty on stderr and occasionally returns non-zero exit codes with
/// empty or garbage stdout (rate limits, expired auth, network blips). Those
/// failures were previously swallowed — `cmd.output()?` only propagates I/O
/// errors launching the process, not a non-zero exit. We must inspect
/// `status.success()` explicitly so the caller can refuse to trust the mirror
/// or retry the sync on the next reconcile.
fn check_gh_output(output: &Output, operation: &str) -> Result<(), Box<dyn std::error::Error>> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let code = output
        .status
        .code()
        .map_or_else(|| "signal".to_string(), |c| c.to_string());
    Err(format!("gh {operation} failed (exit {code}): {}", stderr.trim()).into())
}

/// Parse the JSON returned by `gh issue list --json number` into the first
/// existing issue number, if any.
///
/// Returns:
/// - `Err(..)` if `gh` exited non-zero, stdout is not valid JSON, or the
///   `number` field is present but not a `u64` (malformed response).
/// - `Ok(None)` if the result array is empty — no existing issue for this
///   molecule, caller should create one.
/// - `Ok(Some(n))` with the issue number if exactly one match is returned.
///
/// The previous code used `serde_json::from_slice(..).unwrap_or_default()`
/// followed by `number.as_u64().unwrap_or(0)`. On any gh error (rate limit,
/// network blip, auth expiry), stdout is empty or garbage, the parsed value
/// was treated as "no existing issue", and a duplicate was created on the
/// next reconcile. Returning an error instead aborts the sync for this
/// molecule so the next reconcile can retry cleanly.
fn parse_existing_issue_number(output: &Output) -> Result<Option<u64>, Box<dyn std::error::Error>> {
    check_gh_output(output, "issue list")?;
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("gh issue list returned invalid JSON: {e}"))?;
    let Some(arr) = parsed.as_array() else {
        return Err("gh issue list: expected a JSON array".into());
    };
    let Some(first) = arr.first() else {
        return Ok(None);
    };
    let number = first
        .get("number")
        .and_then(serde_json::Value::as_u64)
        .ok_or("gh issue list: missing or malformed 'number' field")?;
    Ok(Some(number))
}

/// Kinds that can be projected onto GitHub Issues.
const PROJECTABLE_KINDS: &[MoleculeKind] =
    &[MoleculeKind::Issue, MoleculeKind::Task, MoleculeKind::Idea];

/// Variable keys that identify the molecule and already appear in the issue
/// title or other dedicated surface fields. Skipped from the variables table
/// to avoid redundant rendering.
const IDENTITY_KEYS: &[&str] = &["topic", "title", "description"];

/// Hard cap on the rendered issue title length. GitHub's API rejects titles
/// longer than 256 characters; we leave 16 chars of headroom so the emoji
/// prefix and any future decorations can't push us over the limit.
const TITLE_MAX_LEN: usize = 240;

/// Compute the issue title for a molecule, applying the fallback chain:
/// `declaration.description` → [`MoleculeData::display_topic`] (which
/// walks `variables["topic"] → variables["title"] → variables["description"]`)
/// → `formula.description` → the molecule id. The chosen value is prefixed
/// with the kind emoji and kind label, then truncated to [`TITLE_MAX_LEN`]
/// characters on a char boundary so the output is always valid UTF-8.
///
/// The declaration's description sits at the top of the chain because it is
/// the *most specific* human label: a declaration answers "which instance of
/// this work?" whereas the formula only answers "what kind of work?". For
/// example, the formula for a data-quality task may say "Data quality issue
/// — re-download or locate a missing/corrupted file", while the declaration
/// says "Re-download GDELT 20221110.export.CSV.zip (missing from source
/// disk)" — the latter is what operators expect to see in their issue tracker.
///
/// The declaration lookup is keyed by [`MoleculeId::prefix`] against a
/// [`DeclarationMap`] populated from `.cosmon/molecules/*.toml`. When the
/// declaration is absent (molecule created via `cs nucleate <formula>`
/// without `--from`), has an empty `description`, or the operator has not
/// adopted the declarations pattern, the chain falls through cleanly to the
/// formula and variable sources.
fn compute_issue_title(
    mol: &MoleculeData,
    kind: MoleculeKind,
    kind_emoji: &str,
    formulas: &FormulaMap,
    declarations: &DeclarationMap,
) -> String {
    let declaration_desc = declarations
        .get(mol.id.prefix())
        .map(|d| d.description.as_str())
        .filter(|s| !s.is_empty());

    let text = declaration_desc
        .or_else(|| mol.display_topic())
        .or_else(|| {
            formulas
                .get(&mol.formula_id)
                .map(|f| f.description.as_str())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| mol.id.as_str());

    let expired_prefix = if mol
        .expires_at
        .is_some_and(|deadline| deadline <= chrono::Utc::now())
    {
        "\u{26a0}\u{fe0f} "
    } else {
        ""
    };
    let title = format!("{expired_prefix}{kind_emoji} [{kind}] {text}");
    truncate_chars(&title, TITLE_MAX_LEN)
}

/// Truncate `s` to at most `max_chars` Unicode scalar values, returning an
/// owned [`String`]. Byte-slicing would risk splitting a multi-byte UTF-8
/// sequence and producing invalid output; char-counting is the only safe
/// cap for user-provided text that may contain emoji or accented letters.
fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    s.chars().take(max_chars).collect()
}

/// Render the formula's step sequence as a GitHub-flavored markdown todo
/// list, reflecting the molecule's current progress.
///
/// Each step becomes a checklist item. Items at index `< current_step` are
/// checked (the worker has already evolved past them); the current step and
/// any later steps are unchecked. When a step declares exit criteria
/// (the formula's `acceptance` field, mapped onto [`Step::exit_criteria`]),
/// that text renders as a sub-bullet so reviewers see what "done" means for
/// each step without opening the formula file.
///
/// This is the primary progress signal on the host-native GitHub surface:
/// non-cosmon participants should be able to tell at a glance how far the
/// molecule has advanced without learning the `evolve`/`step` vocabulary.
///
/// Returns an empty string if the formula has no steps, so the caller can
/// unconditionally append the result without guarding against empty output.
fn render_steps_as_todo(formula: &Formula, current_step: usize) -> String {
    if formula.steps.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str("\n### Steps\n\n");
    for (i, step) in formula.steps.iter().enumerate() {
        let mark = if i < current_step { "x" } else { " " };
        let _ = writeln!(out, "- [{mark}] {}", step.title);
        if let Some(criteria) = step.exit_criteria.as_deref().filter(|s| !s.is_empty()) {
            let _ = writeln!(out, "  - {criteria}");
        }
    }
    out
}

/// Resolve which GitHub issue to update for this molecule, preferring a
/// local mirror entry over a live `gh issue list` search.
///
/// Returns `Some(issue_number)` if a mirror exists for this molecule; the
/// caller should then edit that issue and, if necessary, close/reopen it.
/// Returns `None` when no mirror exists — the caller must fall back to a
/// `gh issue list --search --state all` lookup.
///
/// The mirror is the canonical handle for issues that cosmon has previously
/// projected: it survives issue closure, remote title edits, and body
/// rewrites. Without this short-circuit, closed issues would be invisible
/// to the default-OPEN `gh issue list` search, and a Completed molecule
/// whose body changes on a subsequent reconcile would silently resurrect
/// as a duplicate. This was the root cause of the `Atlas` #6 resurrection
/// regression (`2026-04-09-cosmon-surface-feedback.md`).
fn resolve_existing_issue_number(
    mol: &MoleculeData,
    mirrors: &std::collections::HashMap<String, crate::github_mirror::IssueMirror>,
) -> Option<u64> {
    mirrors.get(mol.id.as_str()).map(|m| m.issue_number)
}

/// Check if a molecule should be projected to GitHub Issues.
fn is_projectable(mol: &MoleculeData) -> bool {
    let kind = mol.kind.unwrap_or(MoleculeKind::Task);
    PROJECTABLE_KINDS.contains(&kind)
}

/// Map a `MoleculeKind` to a GitHub label.
fn kind_to_label(kind: MoleculeKind) -> &'static str {
    match kind {
        MoleculeKind::Idea => "enhancement",
        MoleculeKind::Task => "task",
        MoleculeKind::Issue => "bug",
        MoleculeKind::Decision => "adr",
        MoleculeKind::Signal => "signal",
        MoleculeKind::Deliberation => "deliberation",
        MoleculeKind::Constellation => "constellation",
        // MoleculeKind is #[non_exhaustive]: a future kind gets a generic
        // label until this surface learns a specific one.
        _ => "molecule",
    }
}

/// Map a molecule status to GitHub open/closed.
fn is_open(mol: &MoleculeData) -> bool {
    mol.status.is_alive()
}

/// Render the GitHub Issue body for a molecule.
///
/// The visible shape depends on `branding`:
///
/// - [`Branding::Attributed`] keeps the legacy metadata block
///   (Molecule/Kind/Formula/Status/Progress/Fleet) and the
///   *Projected by cosmon surface* footer — the tool announces itself.
/// - [`Branding::HostNative`] (default) drops the metadata block entirely
///   and uses a neutral footer that declares the file is auto-generated
///   without mentioning cosmon. The host project owns the surface.
/// - [`Branding::None`] drops the metadata block and omits the footer.
///
/// In all modes, the HTML comment marker (`<!-- cosmon:molecule:ID -->`)
/// remains as invisible plumbing — `project_github_issues` uses it to find
/// existing issues via `gh issue list --search ... in:body` and keep the
/// sync idempotent. This comment is not rendered by GitHub's HTML pipeline,
/// so no "cosmon" string appears in the visible issue body under
/// host-native branding.
///
/// The `formulas` map supplies the formula declaration used by
/// [`render_steps_as_todo`] to emit a progress checklist for the molecule;
/// molecules whose formula is absent from the map simply skip the step
/// section.
fn render_issue_body(
    mol: &MoleculeData,
    kind: MoleculeKind,
    kind_emoji: &str,
    formulas: &FormulaMap,
    branding: Branding,
) -> String {
    let mut body = String::new();

    // Invisible molecule marker — required by the existing-issue search in
    // `project_github_issues`. This is plumbing, not a visible cosmon
    // reference: GitHub never renders HTML comments in the issue body.
    let _ = writeln!(body, "<!-- cosmon:molecule:{} -->", mol.id);

    // Attributed mode keeps the full metadata block. Host-native and none
    // drop it entirely — the operator does not want jargon like "Formula"
    // or "Fleet" leaking into their issue tracker.
    if branding == Branding::Attributed {
        let _ = write!(
            body,
            "\n**Molecule**: `{}`\n\
             **Kind**: {} {}\n\
             **Formula**: `{}`\n\
             **Status**: {} {}\n\
             **Progress**: step {}/{}\n\
             **Fleet**: `{}`\n",
            mol.id,
            kind_emoji,
            kind,
            mol.formula_id,
            mol.status.emoji(),
            mol.status,
            mol.current_step + 1,
            mol.total_steps,
            mol.fleet_id,
        );
    }

    // Expiry badge — visible for any molecule carrying a TTL, regardless of
    // branding. Operators rely on this line in the GitHub UI to decide
    // whether to touch/extend or let the molecule expire. The badge is the
    // clock-invariant absolute date (`format_expiry_badge_static`): the issue
    // body is a hashed, mirror-tracked surface, so a wall-clock countdown here
    // would make the render impure and churn the mirror on every sync (F-C7-1).
    if let Some(badge) = format_expiry_badge_static(mol.expires_at) {
        let _ = writeln!(body, "\n**Expires**: {badge}");
    }

    // Collapse details — show reason and step when molecule has collapsed.
    if mol.status == MoleculeStatus::Collapsed {
        if let Some(ref reason) = mol.collapse_reason {
            let _ = writeln!(body, "**Collapse reason**: {reason}");
        }
        if let Some(step) = mol.collapsed_step {
            let _ = writeln!(
                body,
                "**Collapsed at step**: {}/{}",
                step + 1,
                mol.total_steps
            );
        }
    }

    // Variables table — sorted by key, identity keys filtered. Replaces the
    // previous inline `Context: k=v k=v` blob with a structured 2-column
    // markdown table so long values (URLs, paths) render readably.
    let mut entries: Vec<(&String, &String)> = mol
        .variables
        .iter()
        .filter(|(k, _)| !IDENTITY_KEYS.contains(&k.as_str()))
        .collect();
    if !entries.is_empty() {
        entries.sort_by(|a, b| a.0.cmp(b.0));
        body.push_str("\n### Variables\n\n| Variable | Value |\n| --- | --- |\n");
        for (k, v) in entries {
            let _ = writeln!(body, "| `{k}` | {v} |");
        }
    }

    // Typed links — decay, merge, transform relationships.
    if !mol.typed_links.is_empty() {
        body.push_str("\n### Relationships\n\n");
        for link in &mol.typed_links {
            match link {
                MoleculeLink::DecayedFrom { id } => {
                    let _ = writeln!(body, "- \u{1f4a5} Decayed from `{id}`");
                }
                MoleculeLink::DecayProduct { id } => {
                    let _ = writeln!(body, "- \u{1f331} Decay product: `{id}`");
                }
                MoleculeLink::MergedFrom { ids } => {
                    let refs: Vec<_> = ids.iter().map(|id| format!("`{id}`")).collect();
                    let _ = writeln!(body, "- \u{1f500} Merged from: {}", refs.join(", "));
                }
                MoleculeLink::MergedInto { id } => {
                    let _ = writeln!(body, "- \u{1f500} Merged into `{id}`");
                }
                MoleculeLink::TransformedFrom { kind: from_kind } => {
                    let _ = writeln!(body, "- \u{1f504} Transformed from {from_kind}");
                }
                MoleculeLink::Blocks { target } => {
                    let _ = writeln!(body, "- \u{26d4} Blocks `{target}`");
                }
                MoleculeLink::BlockedBy { source } => {
                    let _ = writeln!(body, "- \u{23f3} Blocked by `{source}`");
                }
                MoleculeLink::Entangled { target } => {
                    let _ = writeln!(body, "- \u{1f517} {target}");
                }
                _ => {}
            }
        }
    }

    // Step progress — rendered right before the footer so it is the last
    // thing a reader sees before the auto-generated disclaimer. Skipped
    // silently if the formula is not in the map (legacy molecules or a
    // formula file that has since been deleted): the surface must still
    // render, just without the progress checklist.
    if let Some(formula) = formulas.get(&mol.formula_id) {
        body.push_str(&render_steps_as_todo(formula, mol.current_step));
    }

    match branding {
        Branding::Attributed => {
            body.push_str("\n---\n*Projected by cosmon surface. Source of truth: `.cosmon/`*");
        }
        Branding::HostNative => {
            body.push_str(
                "\n<!-- auto-generated from .cosmon/ — edit the source, not this file -->",
            );
        }
        Branding::None => {}
    }

    body
}

/// Project molecules onto GitHub Issues.
///
/// Uses the `gh` CLI. Returns the number of issues created/updated.
///
/// # Errors
///
/// Returns an error if `gh` is not available or API calls fail.
#[allow(clippy::too_many_lines)]
pub fn project_github_issues(
    surface: &Surface,
    molecules: &[MoleculeData],
    state_dir: Option<&std::path::Path>,
    formulas: &FormulaMap,
    declarations: &DeclarationMap,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let repo = surface
        .repo
        .as_deref()
        .ok_or("github-issues surface requires 'repo' field (owner/repo)")?;

    // Check gh is available and authenticated.
    check_gh_available()?;
    check_gh_authenticated()?;

    // Load existing mirrors for skip-if-unchanged.
    let mirrors = state_dir
        .map(|sd| crate::github_mirror::load_all_mirrors(sd, repo))
        .unwrap_or_default();

    let mut synced = Vec::new();

    for mol in molecules {
        if !is_projectable(mol) {
            continue;
        }

        let kind = mol.kind.unwrap_or(MoleculeKind::Task);
        let kind_label = kind_to_label(kind);
        let kind_emoji = kind.emoji();

        // Build the issue title and body. Title applies the declaration-aware
        // fallback chain so molecules without a `topic` variable still get a
        // human-legible heading instead of their raw id.
        let title = compute_issue_title(mol, kind, kind_emoji, formulas, declarations);

        let body = render_issue_body(mol, kind, kind_emoji, formulas, surface.branding);

        // Skip if unchanged (compare body hash against mirror).
        let body_hash = crate::github_mirror::hash_content(&body);
        if let Some(mirror) = mirrors.get(mol.id.as_str()) {
            if mirror.body_hash == body_hash
                && mirror.state == (if is_open(mol) { "open" } else { "closed" })
            {
                synced.push(format!(
                    "unchanged #{} ({kind_emoji} {})",
                    mirror.issue_number, mol.id
                ));
                continue;
            }
        }

        // Resolve which issue to update. Prefer the local mirror's
        // `issue_number` over a live `gh issue list --search` call: the
        // mirror is the canonical handle and survives issue closure, whereas
        // `gh issue list` defaults to OPEN issues only. The previous code
        // always searched gh, which silently resurrected closed issues as
        // duplicates whenever a Completed molecule's body changed (the skip
        // check fails on the hash diff, the search returns empty because
        // the issue is closed, and the create branch runs). See the
        // regression test `decide_reconcile_action_*` in this module.
        let existing_number = if let Some(num) = resolve_existing_issue_number(mol, &mirrors) {
            Some(num)
        } else {
            let search_output = std::process::Command::new("gh")
                .args([
                    "issue",
                    "list",
                    "--repo",
                    repo,
                    "--search",
                    &format!("cosmon:molecule:{} in:body", mol.id),
                    "--state",
                    "all",
                    "--json",
                    "number",
                    "--limit",
                    "1",
                ])
                .output()?;
            parse_existing_issue_number(&search_output)?
        };

        if let Some(number) = existing_number {
            // Update existing issue. `gh issue edit` / `gh issue close` must
            // have their exit status checked: on rate limit or expired auth
            // they exit non-zero, and if we silently continue we save a mirror
            // claiming "updated" when nothing was written — then hash-skip
            // trusts the lying mirror forever.
            let edit_output = std::process::Command::new("gh")
                .args([
                    "issue",
                    "edit",
                    &number.to_string(),
                    "--repo",
                    repo,
                    "--title",
                    &title,
                    "--body",
                    &body,
                ])
                .output()?;
            check_gh_output(&edit_output, "issue edit")?;

            // Close/reopen as needed.
            if !is_open(mol) {
                let close_output = std::process::Command::new("gh")
                    .args(["issue", "close", &number.to_string(), "--repo", repo])
                    .output()?;
                check_gh_output(&close_output, "issue close")?;
            }

            // Save mirror.
            if let Some(sd) = state_dir {
                let mirror = crate::github_mirror::IssueMirror {
                    molecule_id: mol.id.as_str().to_string(),
                    issue_number: number,
                    repo: repo.to_string(),
                    title: title.clone(),
                    body_hash: body_hash.clone(),
                    state: if is_open(mol) { "open" } else { "closed" }.to_string(),
                    kind: kind.to_string(),
                    status: mol.status.to_string(),
                    projected_at: chrono::Utc::now().to_rfc3339(),
                };
                let _ = crate::github_mirror::save_mirror(sd, &mirror);
            }
            synced.push(format!("updated #{number} ({kind_emoji} {})", mol.id));
        } else {
            // Create new issue (without labels first, then try to add them).
            let create_output = std::process::Command::new("gh")
                .args([
                    "issue", "create", "--repo", repo, "--title", &title, "--body", &body,
                ])
                .output()?;
            check_gh_output(&create_output, "issue create")?;

            let url = String::from_utf8_lossy(&create_output.stdout)
                .trim()
                .to_string();

            // Best-effort: add labels (may fail if they don't exist on the repo).
            if !url.is_empty() {
                let mut labels = surface.labels.clone();
                labels.push(kind_label.to_string());
                labels.push("cosmon".to_string());
                let labels_str = labels.join(",");

                // Extract issue number from URL.
                if let Some(num) = url.rsplit('/').next() {
                    let _ = std::process::Command::new("gh")
                        .args([
                            "issue",
                            "edit",
                            num,
                            "--repo",
                            repo,
                            "--add-label",
                            &labels_str,
                        ])
                        .output();
                }
            }

            // Save mirror with issue number from URL.
            if let (Some(sd), Some(num_str)) = (state_dir, url.rsplit('/').next()) {
                if let Ok(num) = num_str.parse::<u64>() {
                    let mirror = crate::github_mirror::IssueMirror {
                        molecule_id: mol.id.as_str().to_string(),
                        issue_number: num,
                        repo: repo.to_string(),
                        title: title.clone(),
                        body_hash: body_hash.clone(),
                        state: "open".to_string(),
                        kind: kind.to_string(),
                        status: mol.status.to_string(),
                        projected_at: chrono::Utc::now().to_rfc3339(),
                    };
                    let _ = crate::github_mirror::save_mirror(sd, &mirror);
                }
            }

            synced.push(format!("created {url} ({kind_emoji} {})", mol.id));
        }
    }

    Ok(synced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::id::{FleetId, FormulaId, MoleculeId};

    /// Empty [`FormulaMap`] for `render_issue_body` tests that don't exercise
    /// formula lookup yet.
    fn fm() -> FormulaMap {
        FormulaMap::new()
    }

    /// Empty [`DeclarationMap`] — symmetric helper for tests that do not
    /// exercise the declaration lookup path of the title fallback chain.
    fn dm() -> DeclarationMap {
        DeclarationMap::new()
    }

    fn test_molecule() -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new("task-20260407-abcd").unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("mol-task-work").unwrap(),
            status: MoleculeStatus::Running,
            variables: {
                let mut m = std::collections::HashMap::new();
                m.insert("topic".to_string(), "Fix the build".to_string());
                m
            },
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
    fn test_idea_is_projectable() {
        assert!(PROJECTABLE_KINDS.contains(&MoleculeKind::Idea));
        assert!(PROJECTABLE_KINDS.contains(&MoleculeKind::Task));
        assert!(PROJECTABLE_KINDS.contains(&MoleculeKind::Issue));
    }

    #[test]
    fn test_decision_not_projectable() {
        assert!(!PROJECTABLE_KINDS.contains(&MoleculeKind::Decision));
        assert!(!PROJECTABLE_KINDS.contains(&MoleculeKind::Signal));
    }

    #[test]
    fn test_kind_labels() {
        assert_eq!(kind_to_label(MoleculeKind::Idea), "enhancement");
        assert_eq!(kind_to_label(MoleculeKind::Issue), "bug");
        assert_eq!(kind_to_label(MoleculeKind::Task), "task");
    }

    #[test]
    fn test_expired_title_prefixed_with_warn_emoji() {
        let mut mol = test_molecule();
        mol.expires_at = Some(chrono::Utc::now() - chrono::Duration::days(1));
        let title = compute_issue_title(
            &mol,
            MoleculeKind::Task,
            MoleculeKind::Task.emoji(),
            &fm(),
            &dm(),
        );
        assert!(title.starts_with("\u{26a0}\u{fe0f} "), "got {title}");
    }

    #[test]
    fn test_future_expiry_title_not_decorated() {
        let mut mol = test_molecule();
        mol.expires_at = Some(chrono::Utc::now() + chrono::Duration::days(7));
        let title = compute_issue_title(
            &mol,
            MoleculeKind::Task,
            MoleculeKind::Task.emoji(),
            &fm(),
            &dm(),
        );
        assert!(!title.starts_with("\u{26a0}"), "got {title}");
    }

    #[test]
    fn test_body_includes_expiry_badge() {
        let mut mol = test_molecule();
        mol.expires_at = Some(chrono::Utc::now() + chrono::Duration::days(5));
        let body = render_issue_body(
            &mol,
            MoleculeKind::Task,
            MoleculeKind::Task.emoji(),
            &fm(),
            Branding::HostNative,
        );
        assert!(
            body.contains("**Expires**: \u{1f4c5}"),
            "missing expiry badge in body:\n{body}"
        );
    }

    #[test]
    fn test_body_includes_collapse_reason() {
        let mut mol = test_molecule();
        mol.status = MoleculeStatus::Collapsed;
        mol.collapse_reason = Some("build broke".to_string());
        mol.collapsed_step = Some(0);

        let body = render_issue_body(
            &mol,
            MoleculeKind::Task,
            "\u{1f527}",
            &fm(),
            Branding::HostNative,
        );
        assert!(body.contains("**Collapse reason**: build broke"));
        assert!(body.contains("**Collapsed at step**: 1/2"));
    }

    #[test]
    fn test_body_no_collapse_section_when_running() {
        let mol = test_molecule();
        let body = render_issue_body(
            &mol,
            MoleculeKind::Task,
            "\u{1f527}",
            &fm(),
            Branding::HostNative,
        );
        assert!(!body.contains("Collapse reason"));
    }

    #[test]
    fn test_body_includes_decay_links() {
        let mut mol = test_molecule();
        mol.typed_links = vec![
            MoleculeLink::DecayProduct {
                id: MoleculeId::new("task-20260407-0001").unwrap(),
            },
            MoleculeLink::DecayProduct {
                id: MoleculeId::new("task-20260407-0002").unwrap(),
            },
        ];

        let body = render_issue_body(
            &mol,
            MoleculeKind::Task,
            "\u{1f527}",
            &fm(),
            Branding::HostNative,
        );
        assert!(body.contains("### Relationships"));
        assert!(body.contains("Decay product: `task-20260407-0001`"));
        assert!(body.contains("Decay product: `task-20260407-0002`"));
    }

    #[test]
    fn test_body_includes_merged_from() {
        let mut mol = test_molecule();
        mol.typed_links = vec![MoleculeLink::MergedFrom {
            ids: vec![
                MoleculeId::new("task-20260407-0001").unwrap(),
                MoleculeId::new("task-20260407-0002").unwrap(),
            ],
        }];

        let body = render_issue_body(
            &mol,
            MoleculeKind::Task,
            "\u{1f527}",
            &fm(),
            Branding::HostNative,
        );
        assert!(body.contains("Merged from: `task-20260407-0001`, `task-20260407-0002`"));
    }

    #[test]
    fn test_body_includes_transform_link() {
        let mut mol = test_molecule();
        mol.typed_links = vec![MoleculeLink::TransformedFrom {
            kind: MoleculeKind::Idea,
        }];

        let body = render_issue_body(
            &mol,
            MoleculeKind::Task,
            "\u{1f527}",
            &fm(),
            Branding::HostNative,
        );
        assert!(body.contains("Transformed from idea"));
    }

    #[test]
    fn test_body_no_relationships_when_no_links() {
        let mol = test_molecule();
        let body = render_issue_body(
            &mol,
            MoleculeKind::Task,
            "\u{1f527}",
            &fm(),
            Branding::HostNative,
        );
        assert!(!body.contains("### Relationships"));
    }

    // --- title fallback chain ---------------------------------------------
    //
    // The title fallback chain is:
    //   variables["topic"] → variables["title"]
    //   → variables["description"] → formula.description → molecule id.
    // These tests pin the chain so the cryptic `[formula] molecule_id`
    // legacy title cannot regress for molecules whose formulas either
    // declare no `topic` variable or were created before the fallback
    // was added.

    #[test]
    fn test_compute_issue_title_prefers_topic_variable() {
        let mol = test_molecule(); // has variables["topic"] = "Fix the build"
        let title = compute_issue_title(&mol, MoleculeKind::Task, "\u{1f527}", &fm(), &dm());
        assert_eq!(title, "\u{1f527} [task] Fix the build");
    }

    #[test]
    fn test_compute_issue_title_falls_back_to_formula_description() {
        // Molecule with no topic/title/description variables — fallback must
        // pick up the formula declaration's description rather than the
        // molecule id.
        let mut mol = test_molecule();
        mol.variables.clear();

        let formula = cosmon_core::formula::Formula::parse(
            r#"
formula = "mol-task-work"
version = 1
description = "Execute a scoped task end-to-end"
id_prefix = "task"

[[steps]]
id = "step-1"
title = "Step one"
description = "Do the thing."
"#,
        )
        .expect("formula parses");

        let mut formulas = FormulaMap::new();
        formulas.insert(mol.formula_id.clone(), formula);

        let title = compute_issue_title(&mol, MoleculeKind::Task, "\u{1f527}", &formulas, &dm());
        assert_eq!(title, "\u{1f527} [task] Execute a scoped task end-to-end");
    }

    /// Regression for Atlas-#1 surface feedback (2026-04-09): the declaration's
    /// `description` must win the title chain even when the formula declares
    /// its own description. The declaration is the *most specific* label
    /// (answers "which instance"), the formula is the *generic* label
    /// (answers "what kind of work"). Before this fix, the fallback chain
    /// walked `variables → formula.description` and never reached the
    /// declaration, so operators saw the generic formula string where they
    /// expected the per-molecule declaration string.
    #[test]
    fn test_compute_issue_title_prefers_declaration_description() {
        let mut mol = test_molecule();
        mol.variables.clear(); // No topic/title/description variables.

        // Formula has its own description — this is what the chain used to
        // pick up, and what must now be shadowed by the declaration.
        let formula = cosmon_core::formula::Formula::parse(
            r#"
formula = "data-quality"
version = 1
description = "Data quality issue — re-download or locate a missing/corrupted file"
id_prefix = "task"

[[steps]]
id = "step-1"
title = "Fix it"
description = "Make the data whole again."
"#,
        )
        .expect("formula parses");
        let mut formulas = FormulaMap::new();
        formulas.insert(mol.formula_id.clone(), formula);

        // Declaration carries the per-instance description. The molecule id's
        // prefix is `task` (from `test_molecule`), so the DeclarationMap key
        // must match that prefix for the fallback lookup to succeed.
        let declaration = cosmon_core::declaration::MoleculeDeclaration::parse(
            r#"
id_prefix = "task"
formula = "data-quality"
description = "Re-download GDELT 20221110.export.CSV.zip (missing from source disk)"
"#,
        )
        .expect("declaration parses");
        let mut declarations = DeclarationMap::new();
        declarations.insert(declaration.id_prefix.clone(), declaration);

        let title = compute_issue_title(
            &mol,
            MoleculeKind::Task,
            "\u{1f527}",
            &formulas,
            &declarations,
        );
        assert_eq!(
            title,
            "\u{1f527} [task] Re-download GDELT 20221110.export.CSV.zip (missing from source disk)"
        );
    }

    /// When the declaration matches the molecule but carries an empty
    /// `description` (optional field), the fallback must continue cleanly
    /// down the chain to the formula. The declaration's *presence* must
    /// not short-circuit the chain with an empty string.
    #[test]
    fn test_compute_issue_title_declaration_empty_description_falls_through() {
        let mut mol = test_molecule();
        mol.variables.clear();

        let formula = cosmon_core::formula::Formula::parse(
            r#"
formula = "data-quality"
version = 1
description = "Fallback to formula"
id_prefix = "task"

[[steps]]
id = "step-1"
title = "Do"
description = "Something."
"#,
        )
        .expect("formula parses");
        let mut formulas = FormulaMap::new();
        formulas.insert(mol.formula_id.clone(), formula);

        // Declaration with empty description (legitimate: `description` is
        // `#[serde(default)]` on `MoleculeDeclaration`).
        let declaration = cosmon_core::declaration::MoleculeDeclaration::parse(
            r#"
id_prefix = "task"
formula = "data-quality"
"#,
        )
        .expect("declaration parses");
        let mut declarations = DeclarationMap::new();
        declarations.insert(declaration.id_prefix.clone(), declaration);

        let title = compute_issue_title(
            &mol,
            MoleculeKind::Task,
            "\u{1f527}",
            &formulas,
            &declarations,
        );
        assert_eq!(title, "\u{1f527} [task] Fallback to formula");
    }

    /// When the `topic` variable is present the fallback chain still prefers
    /// the declaration's description. Topic is a formula-author convention;
    /// the declaration is the operator's authoritative label for this
    /// specific instance and must win.
    #[test]
    fn test_compute_issue_title_declaration_beats_topic_variable() {
        let mol = test_molecule(); // has variables["topic"] = "Fix the build"

        let declaration = cosmon_core::declaration::MoleculeDeclaration::parse(
            r#"
id_prefix = "task"
formula = "mol-task-work"
description = "Declarative label wins"
"#,
        )
        .expect("declaration parses");
        let mut declarations = DeclarationMap::new();
        declarations.insert(declaration.id_prefix.clone(), declaration);

        let title =
            compute_issue_title(&mol, MoleculeKind::Task, "\u{1f527}", &fm(), &declarations);
        assert_eq!(title, "\u{1f527} [task] Declarative label wins");
    }

    #[test]
    fn test_compute_issue_title_hard_caps_length() {
        // A pathologically long topic must be truncated to TITLE_MAX_LEN
        // chars so GitHub's 256-char API limit is never breached.
        let mut mol = test_molecule();
        mol.variables.insert("topic".to_string(), "a".repeat(500));

        let title = compute_issue_title(&mol, MoleculeKind::Task, "\u{1f527}", &fm(), &dm());
        assert_eq!(title.chars().count(), TITLE_MAX_LEN);
    }

    // --- Atlas #6 resurrection regression ----------------------------------
    //
    // `resolve_existing_issue_number` is the narrow helper that fixes the
    // resurrection bug: when a mirror exists for a molecule, we must use
    // its `issue_number` instead of searching `gh issue list`, whose default
    // OPEN filter hides closed issues and causes the create-new branch to
    // run for any Completed molecule whose body has since changed. These
    // tests pin the mirror-wins policy at the pure-function level so the
    // fix cannot regress without a visible test failure.

    fn make_mirror(
        issue_number: u64,
        state: &str,
        body_hash: &str,
    ) -> crate::github_mirror::IssueMirror {
        crate::github_mirror::IssueMirror {
            molecule_id: "task-20260407-abcd".to_string(),
            issue_number,
            repo: "owner/repo".to_string(),
            title: "dummy".to_string(),
            body_hash: body_hash.to_string(),
            state: state.to_string(),
            kind: "task".to_string(),
            status: "running".to_string(),
            projected_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    #[test]
    fn resolve_existing_issue_number_returns_mirror_number_when_present() {
        let mol = test_molecule();
        let mut mirrors = std::collections::HashMap::new();
        mirrors.insert(
            mol.id.as_str().to_string(),
            make_mirror(42, "open", "hash-a"),
        );
        assert_eq!(resolve_existing_issue_number(&mol, &mirrors), Some(42));
    }

    #[test]
    fn resolve_existing_issue_number_returns_none_when_no_mirror() {
        let mol = test_molecule();
        let mirrors = std::collections::HashMap::new();
        assert_eq!(resolve_existing_issue_number(&mol, &mirrors), None);
    }

    /// The heart of the #6 regression: a Completed molecule whose GH issue
    /// has already been closed on a previous reconcile must still be
    /// reachable via its mirror — *without any `gh issue list` call* — so
    /// that subsequent reconciles edit the closed issue in place instead of
    /// creating a new duplicate. We cannot exercise the full reconcile path
    /// in a unit test (it shells out to `gh`), so we pin the decision at
    /// the helper level: even when the molecule is Completed and the mirror
    /// records `state = "closed"`, the helper still returns the mirror's
    /// issue number, which is the branch that prevents resurrection.
    #[test]
    fn resolve_existing_issue_number_handles_closed_completed_molecule() {
        let mut mol = test_molecule();
        mol.status = MoleculeStatus::Completed;

        let mut mirrors = std::collections::HashMap::new();
        mirrors.insert(
            mol.id.as_str().to_string(),
            make_mirror(7, "closed", "stale-hash"),
        );

        // Mirror hit → use its number. The previous code ignored the mirror
        // and queried `gh issue list`, whose default OPEN filter returned
        // an empty array, which was interpreted as "no issue exists" and
        // triggered the create-new branch — resurrecting the closed issue.
        assert_eq!(resolve_existing_issue_number(&mol, &mirrors), Some(7));
    }

    /// Multiple reconcile passes against the same Completed molecule must
    /// converge: the first reconcile closes the issue and records the
    /// mirror, subsequent reconciles see the mirror and short-circuit,
    /// never reaching the search path that resurrects the issue. This
    /// test walks the helper's decision three times against the same
    /// mirror state to pin the idempotency explicitly.
    #[test]
    fn resolve_existing_issue_number_is_idempotent_across_reconciles() {
        let mut mol = test_molecule();
        mol.status = MoleculeStatus::Completed;

        let mut mirrors = std::collections::HashMap::new();
        mirrors.insert(
            mol.id.as_str().to_string(),
            make_mirror(99, "closed", "closed-hash"),
        );

        for pass in 0..3 {
            assert_eq!(
                resolve_existing_issue_number(&mol, &mirrors),
                Some(99),
                "pass {pass} must keep returning the mirrored issue number"
            );
        }
    }

    // --- gh exit-status guards ---------------------------------------------
    //
    // These tests pin the behavior of `check_gh_output` and
    // `parse_existing_issue_number` so the original bug cannot regress: on a
    // non-zero exit from `gh` with garbage stdout, we must NOT treat the
    // response as "no existing issue found" (which would cause a duplicate
    // issue to be created on the next reconcile).

    /// Build a real `Output` by running a shell command. We use a subprocess
    /// so the `ExitStatus` is genuine rather than constructed via
    /// platform-specific APIs — this is cross-platform safe on all unix CI.
    #[cfg(unix)]
    fn sh_output(script: &str) -> Output {
        std::process::Command::new("sh")
            .args(["-c", script])
            .output()
            .expect("failed to run sh")
    }

    #[cfg(unix)]
    #[test]
    fn interpret_probe_success_is_ok() {
        let out = sh_output("exit 0");
        assert!(interpret_probe(Ok(out), true, "boom").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn interpret_probe_nonzero_exit_returns_actionable_error() {
        // Simulates `gh auth status` on an unauthenticated machine: the
        // binary runs but exits non-zero. Users must get the actionable
        // hint rather than a cryptic downstream failure.
        let out = sh_output("echo 'not logged in' >&2; exit 1");
        let err = interpret_probe(
            Ok(out),
            true,
            "gh CLI is installed but not authenticated. Run: gh auth login",
        )
        .unwrap_err();
        assert!(err.to_string().contains("gh auth login"));
    }

    #[test]
    fn interpret_probe_spawn_failure_returns_error() {
        // Simulates `gh` being absent entirely (Command::output() returns
        // an I/O error). Must funnel to the same actionable branch.
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file or directory");
        let err = interpret_probe(Err(io_err), true, "gh CLI not found. Install it").unwrap_err();
        assert!(err.to_string().contains("gh CLI not found"));
    }

    #[cfg(unix)]
    #[test]
    fn parse_existing_issue_number_rejects_nonzero_exit_with_garbage_stdout() {
        // Simulate gh erroring out (rate limit, auth expired, network blip)
        // and dumping unrelated text on stdout. The old code called
        // `serde_json::from_slice(..).unwrap_or_default()` on this output and
        // silently returned `None`, causing a duplicate issue to be created.
        let out = sh_output("printf 'API rate limit exceeded' >&1; exit 1");
        let result = parse_existing_issue_number(&out);
        assert!(
            result.is_err(),
            "non-zero gh exit must propagate as an error, got {result:?}"
        );
        // Crucially: the error branch is what prevents the caller from
        // reaching the "create new issue" path and producing a duplicate.
    }

    #[cfg(unix)]
    #[test]
    fn parse_existing_issue_number_rejects_invalid_json_on_clean_exit() {
        let out = sh_output("printf 'not json at all'");
        let result = parse_existing_issue_number(&out);
        assert!(result.is_err(), "garbage JSON must propagate as an error");
    }

    #[cfg(unix)]
    #[test]
    fn parse_existing_issue_number_returns_none_for_empty_array() {
        let out = sh_output("printf '[]'");
        let result = parse_existing_issue_number(&out).expect("empty array is valid");
        assert_eq!(result, None);
    }

    #[cfg(unix)]
    #[test]
    fn parse_existing_issue_number_extracts_number_from_first_match() {
        let out = sh_output("printf '[{\"number\": 42}]'");
        let result = parse_existing_issue_number(&out).expect("valid JSON with number");
        assert_eq!(result, Some(42));
    }

    #[cfg(unix)]
    #[test]
    fn parse_existing_issue_number_rejects_malformed_number_field() {
        // Previously `.as_u64().unwrap_or(0)` silently treated this as "no
        // valid match" and skipped the update; now it errors out loudly.
        let out = sh_output("printf '[{\"number\": \"not-a-number\"}]'");
        let result = parse_existing_issue_number(&out);
        assert!(
            result.is_err(),
            "malformed 'number' field must propagate as an error"
        );
    }

    #[cfg(unix)]
    #[test]
    fn check_gh_output_accepts_success() {
        let out = sh_output("true");
        assert!(check_gh_output(&out, "issue edit").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn check_gh_output_rejects_nonzero_exit_with_stderr_message() {
        let out = sh_output("printf 'gh: HTTP 401: Bad credentials' >&2; exit 1");
        let err =
            check_gh_output(&out, "issue edit").expect_err("non-zero exit must become an Err");
        let msg = err.to_string();
        assert!(
            msg.contains("issue edit"),
            "error must name the operation: {msg}"
        );
        assert!(
            msg.contains("Bad credentials"),
            "error must include stderr context: {msg}"
        );
    }

    /// Molecule variables render as a sorted 2-column markdown table, and
    /// identity keys (topic) are skipped since they already appear in the
    /// issue title.
    #[test]
    fn test_body_includes_variables_table_sorted_and_skips_identity_keys() {
        let mut mol = test_molecule();
        mol.variables
            .insert("bucket".to_string(), "prod".to_string());
        mol.variables
            .insert("url".to_string(), "https://example.com/x".to_string());
        mol.variables
            .insert("filename".to_string(), "abc.txt".to_string());
        // `topic` is already injected by test_molecule and must NOT appear
        // in the table (it already headlines the issue title).

        let body = render_issue_body(
            &mol,
            MoleculeKind::Task,
            "\u{1f527}",
            &fm(),
            Branding::HostNative,
        );

        assert!(body.contains("### Variables"));
        assert!(body.contains("| Variable | Value |"));
        assert!(body.contains("| `bucket` | prod |"));
        assert!(body.contains("| `filename` | abc.txt |"));
        assert!(body.contains("| `url` | https://example.com/x |"));
        assert!(
            !body.contains("| `topic` |"),
            "identity key `topic` must be filtered from the variables table"
        );

        // Rows appear in alphabetical order (bucket < filename < url).
        let b = body.find("| `bucket`").unwrap();
        let f = body.find("| `filename`").unwrap();
        let u = body.find("| `url`").unwrap();
        assert!(b < f && f < u, "variables must be sorted by key");
    }

    #[test]
    fn test_body_no_variables_section_when_only_identity_keys() {
        // Only `topic` present — section must be omitted entirely.
        let mol = test_molecule();
        let body = render_issue_body(
            &mol,
            MoleculeKind::Task,
            "\u{1f527}",
            &fm(),
            Branding::HostNative,
        );
        assert!(!body.contains("### Variables"));
    }

    // --- branding modes ----------------------------------------------------
    //
    // These tests pin the visible body of each branding variant so a
    // future refactor cannot quietly reintroduce "cosmon" vocabulary under
    // the default (`HostNative`). The HTML comment marker
    // `<!-- cosmon:molecule:ID -->` is kept in all modes as invisible
    // plumbing — GitHub never renders HTML comments, and the search in
    // `project_github_issues` relies on it to find existing issues.

    #[test]
    fn test_body_host_native_drops_metadata_block() {
        let mol = test_molecule();
        let body = render_issue_body(
            &mol,
            MoleculeKind::Task,
            "\u{1f527}",
            &fm(),
            Branding::HostNative,
        );
        // Jargon metadata labels are gone from the body.
        assert!(!body.contains("**Molecule**"));
        assert!(!body.contains("**Kind**"));
        assert!(!body.contains("**Formula**"));
        assert!(!body.contains("**Status**"));
        assert!(!body.contains("**Progress**"));
        assert!(!body.contains("**Fleet**"));
    }

    #[test]
    fn test_body_host_native_footer_is_neutral() {
        let mol = test_molecule();
        let body = render_issue_body(
            &mol,
            MoleculeKind::Task,
            "\u{1f527}",
            &fm(),
            Branding::HostNative,
        );
        assert!(
            body.contains("<!-- auto-generated from .cosmon/ — edit the source, not this file -->")
        );
        // No visible cosmon vocabulary in the footer.
        assert!(!body.contains("Projected by cosmon"));
    }

    #[test]
    fn test_body_host_native_marker_is_invisible_plumbing() {
        // The HTML comment marker must remain for idempotent issue lookup
        // by `gh issue list --search '... in:body'`. It is never rendered
        // by GitHub, so it does not count as visible cosmon vocabulary.
        let mol = test_molecule();
        let body = render_issue_body(
            &mol,
            MoleculeKind::Task,
            "\u{1f527}",
            &fm(),
            Branding::HostNative,
        );
        assert!(body.contains(&format!("<!-- cosmon:molecule:{} -->", mol.id)));
    }

    #[test]
    fn test_body_attributed_keeps_metadata_block_and_footer() {
        let mol = test_molecule();
        let body = render_issue_body(
            &mol,
            MoleculeKind::Task,
            "\u{1f527}",
            &fm(),
            Branding::Attributed,
        );
        // Metadata block intact.
        assert!(body.contains("**Molecule**"));
        assert!(body.contains("**Kind**"));
        assert!(body.contains("**Formula**"));
        assert!(body.contains("**Status**"));
        assert!(body.contains("**Progress**"));
        assert!(body.contains("**Fleet**"));
        // Attributed footer.
        assert!(body.contains("*Projected by cosmon surface."));
        // No host-native footer.
        assert!(!body.contains("auto-generated from .cosmon/"));
    }

    #[test]
    fn test_body_none_drops_metadata_and_footer() {
        let mol = test_molecule();
        let body = render_issue_body(&mol, MoleculeKind::Task, "\u{1f527}", &fm(), Branding::None);
        // No metadata block.
        assert!(!body.contains("**Molecule**"));
        // No footer whatsoever (neither attributed nor host-native).
        assert!(!body.contains("Projected by cosmon"));
        assert!(!body.contains("auto-generated from"));
        // Marker still present (invisible plumbing).
        assert!(body.contains("cosmon:molecule:"));
    }

    // --- step todo list ----------------------------------------------------
    //
    // `render_steps_as_todo` is the primary progress signal on host-native
    // GitHub issues: non-cosmon reviewers should see at a glance how far the
    // molecule has advanced without learning the `evolve` vocabulary.

    /// Build a two-step formula so tests can pin the checkbox behaviour at
    /// `current_step = 1` (first step done, second step current) without
    /// repeating the TOML literal in every test.
    fn two_step_formula() -> Formula {
        Formula::parse(
            r#"
formula = "mol-task-work"
version = 1
description = "Execute a scoped task end-to-end"
id_prefix = "task"

[[steps]]
id = "step-1"
title = "Implement the solution"
description = "Do the work."
acceptance = "Implementation complete, compiles clean"

[[steps]]
id = "step-2"
title = "Verify and validate"
description = "Run the gates."
"#,
        )
        .expect("formula parses")
    }

    #[test]
    fn render_steps_as_todo_checks_completed_and_leaves_current_unchecked() {
        // current_step = 1 means step 0 is done, step 1 is in progress.
        // Checkboxes must be `[x]` for index < current_step and `[ ]`
        // otherwise — this is what flips when `cs evolve` advances the
        // molecule and is what non-cosmon reviewers rely on.
        let formula = two_step_formula();
        let out = render_steps_as_todo(&formula, 1);

        assert!(out.contains("### Steps"));
        assert!(
            out.contains("- [x] Implement the solution"),
            "completed step must be checked: {out}"
        );
        assert!(
            out.contains("- [ ] Verify and validate"),
            "current step must be unchecked: {out}"
        );
        // And at current_step = 0, nothing is checked yet.
        let fresh = render_steps_as_todo(&formula, 0);
        assert!(fresh.contains("- [ ] Implement the solution"));
        assert!(fresh.contains("- [ ] Verify and validate"));
    }

    #[test]
    fn render_steps_as_todo_emits_acceptance_sub_bullet_when_present() {
        // Only step 1 has an `acceptance` clause in the TOML; step 2 omits
        // it. The renderer must emit the sub-bullet for step 1 and NOT
        // invent one for step 2.
        let formula = two_step_formula();
        let out = render_steps_as_todo(&formula, 0);

        // Step 1 has acceptance → sub-bullet rendered with exact text.
        assert!(
            out.contains("  - Implementation complete, compiles clean"),
            "step 1 acceptance must render as sub-bullet: {out}"
        );
        // Step 2 has no acceptance → no sub-bullet under it. We check by
        // asserting that after "Verify and validate" the next non-empty
        // content is not a two-space-indented bullet.
        let tail = out
            .split("- [ ] Verify and validate")
            .nth(1)
            .expect("step 2 line must be present");
        assert!(
            !tail.contains("  - "),
            "step 2 has no acceptance, must not emit a sub-bullet: {tail}"
        );
    }

    #[test]
    fn test_body_includes_step_checklist_when_formula_is_known() {
        // End-to-end: render_issue_body must splice the todo list into the
        // body when the molecule's formula is present in the FormulaMap.
        // This pins the wiring so a future refactor can't accidentally drop
        // the step section from the host-native surface.
        let mut mol = test_molecule();
        mol.current_step = 1;

        let mut formulas = FormulaMap::new();
        formulas.insert(mol.formula_id.clone(), two_step_formula());

        let body = render_issue_body(
            &mol,
            MoleculeKind::Task,
            "\u{1f527}",
            &formulas,
            Branding::HostNative,
        );

        assert!(body.contains("### Steps"));
        assert!(body.contains("- [x] Implement the solution"));
        assert!(body.contains("- [ ] Verify and validate"));
        // And the checklist must appear before the auto-generated footer.
        let steps_idx = body.find("### Steps").unwrap();
        let footer_idx = body.find("<!-- auto-generated").unwrap();
        assert!(
            steps_idx < footer_idx,
            "step checklist must precede the footer"
        );
    }

    #[test]
    fn test_body_omits_step_checklist_when_formula_unknown() {
        // An empty FormulaMap (legacy molecules or deleted formulas) must
        // still produce a valid body — no panic, no phantom "### Steps"
        // section.
        let mol = test_molecule();
        let body = render_issue_body(
            &mol,
            MoleculeKind::Task,
            "\u{1f527}",
            &fm(),
            Branding::HostNative,
        );
        assert!(!body.contains("### Steps"));
    }

    #[test]
    fn test_body_collapse_and_variables_render_in_all_modes() {
        // Regardless of branding, the functional sections (collapse info,
        // variables table, typed links) must keep rendering — branding
        // controls only the metadata jargon block and the footer.
        for branding in [Branding::Attributed, Branding::HostNative, Branding::None] {
            let mut mol = test_molecule();
            mol.status = MoleculeStatus::Collapsed;
            mol.collapse_reason = Some("rate limited".to_string());
            mol.collapsed_step = Some(1);
            mol.variables
                .insert("bucket".to_string(), "staging".to_string());

            let body = render_issue_body(&mol, MoleculeKind::Task, "\u{1f527}", &fm(), branding);
            assert!(
                body.contains("**Collapse reason**: rate limited"),
                "branding {branding:?} must keep collapse info"
            );
            assert!(
                body.contains("| `bucket` | staging |"),
                "branding {branding:?} must keep variables table"
            );
        }
    }
}
