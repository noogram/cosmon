// SPDX-License-Identifier: AGPL-3.0-only

//! Project materialization — the `.cosmon/` upgrade pass as a library.
//!
//! This module owns everything `cs init --upgrade` writes to disk:
//! canonical formula templates, the neurion registry, the `state/` tree,
//! `project_id` in `config.toml`, the gitignore migration, the `CLAUDE.md`
//! cosmon section, and the repo-root gitleaks baseline.
//!
//! # Why it lives here rather than in the CLI
//!
//! It used to be `cosmon_cli::cmd::init::run_upgrade`, reachable only by
//! spawning the `cs` binary. That made every non-CLI caller — the RPP
//! adapter's boot-time image init, first among them — depend on a `cs`
//! executable being on `PATH` inside its container, which the shipped
//! image does not carry. Materialization is filesystem work, and
//! filesystem work belongs in this crate (the domain core in
//! `cosmon-core` stays I/O-free). The CLI is now one caller of
//! [`upgrade_project`] among several, and it prints the report rather
//! than owning the decision.
//!
//! # Contract
//!
//! Every pass is **idempotent** and **customization-preserving**: an
//! existing file is never overwritten (the two gitignore bodies are the
//! single exception, and only on an exact byte-match against a known
//! legacy body). Running [`upgrade_project`] twice leaves the second run
//! reporting `nothing_changed`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cosmon_core::id::ProjectId;

/// Why a project upgrade could not complete.
///
/// Distinguished from a plain `io::Error` because the first variant is a
/// *precondition* failure the caller can act on (run `cs init` first),
/// not a transient filesystem fault.
#[derive(Debug, thiserror::Error)]
pub enum UpgradeError {
    /// The target has no `.cosmon/` directory — there is nothing to
    /// upgrade. The caller must run a fresh init first.
    #[error("no .cosmon/ directory found — run `cs init` first")]
    MissingCosmonDir,
    /// A filesystem write or read failed.
    #[error("filesystem error during upgrade: {0}")]
    Io(#[from] std::io::Error),
    /// The neurion registry (`registry.sqlite`) could not be created or
    /// seeded. Carries the `SQLite` message as text so this crate's public
    /// error surface does not leak `rusqlite` types.
    #[error("registry: {0}")]
    Registry(String),
}

/// Knobs for [`upgrade_project`].
///
/// Deliberately near-empty today: the upgrade pass has no options a
/// caller has ever needed to vary. It exists so a future knob (a dry
/// run, a tenant label) is an added field rather than a changed
/// signature.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct UpgradeOptions {}

/// What [`upgrade_project`] created, kept, or updated.
///
/// Every field is a *fact about this run*, not about the end state: a
/// `false` means "already there", never "absent". The CLI renders this
/// into the `cs init --upgrade` output; the RPP adapter asserts on it
/// instead of reading a subprocess's exit code.
#[derive(Debug, Clone)]
#[non_exhaustive]
// One flag per independent backfill pass. Collapsing them into a bitset
// or a `Vec<Pass>` would make the caller name a pass by string to ask
// about it — the flags ARE the vocabulary the CLI prints and the adapter
// asserts on.
#[allow(clippy::struct_excessive_bools)]
pub struct UpgradeReport {
    /// The project's identity after the run — pre-existing or freshly
    /// generated.
    pub project_id: ProjectId,
    /// Canonical formula templates written because they were missing.
    pub added_formulas: Vec<String>,
    /// `project_id` was absent from `config.toml` and has been backfilled.
    pub project_id_added: bool,
    /// `registry.sqlite` was absent and has been created + seeded.
    pub registry_added: bool,
    /// The `state/` tree was absent and has been materialized.
    pub state_added: bool,
    /// A legacy gitignore body was migrated to the current scheme.
    pub gitignore_upgraded: bool,
    /// `CLAUDE.md` was created, or its cosmon section rewritten.
    pub claude_md_updated: bool,
    /// The repo-root `.gitleaks.toml` baseline was absent and written.
    pub gitleaks_added: bool,
}

impl UpgradeReport {
    /// True when the run changed nothing on disk — the idempotence
    /// witness. `cs init --upgrade` reports `already_upgraded` on this.
    #[must_use]
    pub fn nothing_changed(&self) -> bool {
        self.added_formulas.is_empty()
            && !self.project_id_added
            && !self.registry_added
            && !self.state_added
            && !self.gitignore_upgraded
            && !self.claude_md_updated
            && !self.gitleaks_added
    }

    /// The machine-readable status string: `"upgraded"` or
    /// `"already_upgraded"`. Kept here rather than at each call site so
    /// the CLI's JSON and any other projection cannot drift apart.
    #[must_use]
    pub fn status(&self) -> &'static str {
        if self.nothing_changed() {
            "already_upgraded"
        } else {
            "upgraded"
        }
    }
}

/// Builtin formula templates compiled into the `cs` binary.
///
/// These are the canonical formulas every project gets on `cs init`, so
/// `cs nucleate deep-think` / `task-work` / `idea-to-plan` /
/// `editorial-work` works on the very first invocation — no "empty
/// `formulas/` trap", no extra setup. Galaxies that produce prose rather
/// than code (atlas, accord, chancery) reach for `editorial-work`
/// instead of `task-work`; making it builtin means an existing galaxy
/// backfills it with `cs init --soft` and a new one gets it on day one.
///
/// The paths are resolved at compile time from the workspace-level
/// `.cosmon/formulas/` directory (the canonical source) so that updates
/// to those files automatically flow into the next build.
pub const BUILTIN_FORMULAS: &[(&str, &str)] = &[
    (
        "deep-think.formula.toml",
        include_str!("../../../.cosmon/formulas/deep-think.formula.toml"),
    ),
    // The Tier-0 inline-panel variant of deep-think. Builtin because a
    // Tier-1 mission-controller cannot nucleate the Tier-1 `deep-think`
    // (the ordinal guard `ensure_tier_descends` demands strict descent),
    // and Tier-2 signing is unsupported until cosmon-sign lands. A mission
    // that needs a panel mid-flight reaches for this leaf — so it must
    // exist on every galaxy, not just where someone copied it in.
    // Origin: atlas-cours mission-20260611-fe9a (task-20260611-403a).
    (
        "deep-think-inline.formula.toml",
        include_str!("../../../.cosmon/formulas/deep-think-inline.formula.toml"),
    ),
    (
        "task-work.formula.toml",
        include_str!("../../../.cosmon/formulas/task-work.formula.toml"),
    ),
    (
        "idea-to-plan.formula.toml",
        include_str!("../../../.cosmon/formulas/idea-to-plan.formula.toml"),
    ),
    (
        "mission-plan.formula.toml",
        include_str!("../../../.cosmon/formulas/mission-plan.formula.toml"),
    ),
    (
        "temp-review.formula.toml",
        include_str!("../../../.cosmon/formulas/temp-review.formula.toml"),
    ),
    (
        "mission-controller.formula.toml",
        include_str!("../../../.cosmon/formulas/mission-controller.formula.toml"),
    ),
    (
        "editorial-work.formula.toml",
        include_str!("../../../.cosmon/formulas/editorial-work.formula.toml"),
    ),
    // The independent visual witness required by the `surface_visual`
    // mindguard. Builtin because the gate's remedy prescribes
    // `cs nucleate verify-surface` on EVERY galaxy: a fleet without
    // this formula cannot satisfy a refused `cs complete` at all (the
    // automata blocker of 2026-06-07 — gate shipped without its
    // remedy).
    (
        "verify-surface.formula.toml",
        include_str!("../../../.cosmon/formulas/verify-surface.formula.toml"),
    ),
];
/// Contents of `.cosmon/.gitignore` (paths are relative to `.cosmon/`).
///
/// Cosmon state is split like git itself: ephemeral runtime (registry,
/// lockfiles, PIDs, tmux/pty logs, volatile `state.json`) is ignored; durable
/// intellectual artifacts (deliberation syntheses, decision outcomes,
/// briefings, per-persona responses, append-only notes, the `events.jsonl`
/// audit trail, reports) are **tracked**. This is the chain of reasoning
/// that makes cosmon projects interesting archaeologically — it belongs in
/// git history, not in the runtime working tree.
/// Canonical, federation-shared gitleaks baseline scaffolded into each
/// galaxy's repo-root `.gitleaks.toml` by `cs init`. Embedded verbatim from the
/// single source of truth at `assets/gitleaks/cosmon-baseline.gitleaks.toml`
/// so the shipped file and the scaffolded copy can never drift.
///
/// WHY a galaxy needs it: cosmon writes `.cosmon/state/events.jsonl`, an
/// append-only journal whose `reason` field is free-text prose. gitleaks'
/// entropy-based `generic-api-key` rule structurally false-positives on benign
/// `word=word` fragments in that prose, blocking every `cs done` harvest that
/// runs through a pre-commit gitleaks hook. The baseline silences ONLY that
/// heuristic, ONLY on state-journal paths, while keeping every high-confidence
/// rule (plus a dedicated AWS rule) scanning those journals — so a real secret
/// is still caught. See `docs/guides/gitleaks-state-journals.md`.
pub const COSMON_GITLEAKS_BASELINE: &str =
    include_str!("../../../assets/gitleaks/cosmon-baseline.gitleaks.toml");
/// Opening marker of the cosmon-managed section of `.cosmon/.gitignore`.
///
/// Exists because the exact-match replacement discipline below has a blind
/// spot: once a user (or an agent acting for one) edits the file at all,
/// cosmon can never touch it again — including when the edit left the file
/// broken. A delimited block narrows cosmon's ownership from "the whole
/// file, if untouched" to "these lines, always", which is both safer and
/// repairable. Mirrors [`COSMON_SECTION_START`] for `CLAUDE.md`.
pub const COSMON_GITIGNORE_BLOCK_START: &str =
    "# cosmon:gitignore:start — managed by `cs init --upgrade`; edit outside this block";

/// Closing marker of the cosmon-managed section of `.cosmon/.gitignore`.
///
/// See [`COSMON_GITIGNORE_BLOCK_START`].
pub const COSMON_GITIGNORE_BLOCK_END: &str = "# cosmon:gitignore:end";

/// The cosmon-managed block of `.cosmon/.gitignore`, markers included.
///
/// Every rule here is relative to `.cosmon/`. Two forms are load-bearing
/// and were wrong until issue #60: see the comment body.
pub const COSMON_GITIGNORE_BLOCK: &str = "\
# cosmon:gitignore:start — managed by `cs init --upgrade`; edit outside this block
# Cosmon runtime — ephemeral state is ignored in bulk; the archive subtree
# (durable, human-readable proof-of-work snapshots) is re-included via
# negation, so a molecule's chain of reasoning reaches git history.
#
# Two forms are load-bearing and easy to get wrong (issue #60):
#   * `state/*`, never `state/`: git does not descend into an excluded
#     directory, so a blanket `state/` makes every negation below match
#     nothing — the rule reads as if the archive were tracked and it is not.
#   * `!state/` first: it keeps the directory itself un-excluded, so this
#     block still binds when a broader `state/` rule precedes it.
# See ADR: ARCHIVE M1 (task-20260413) and issue #60.
!state/
state/*
!state/archive/
!state/archive/**
registry.sqlite
registry.sqlite-journal
registry.sqlite-wal
*.lock
*.tmp
# cosmon:gitignore:end
";

/// Contents of `.cosmon/.gitignore` as written by a fresh `cs init`.
///
/// Cosmon state is split like git itself: ephemeral runtime (registry,
/// lockfiles, PIDs, tmux/pty logs, volatile `state.json`) is ignored;
/// durable intellectual artifacts (deliberation syntheses, decision
/// outcomes, briefings, per-persona responses, append-only notes, the
/// `events.jsonl` audit trail, reports) are **tracked** under
/// `state/archive/`. That chain of reasoning is what makes cosmon projects
/// interesting archaeologically — it belongs in git history, not only in
/// the runtime working tree that `cs done` tears down.
///
/// A fresh file is exactly the managed block; user lines, when there are
/// any, live outside it.
pub const COSMON_GITIGNORE_CONTENT: &str = COSMON_GITIGNORE_BLOCK;

/// Previous `.cosmon/.gitignore` body (ARCHIVE M1, 2026-04-12 → issue #60).
///
/// Announced the archive negation and did not deliver it: `state/` excludes
/// the directory, so git never descends and `!state/archive/` matches
/// nothing. Kept verbatim so `cs init --upgrade` can recognise and replace
/// it by exact match.
pub const LEGACY_BROKEN_NEGATION_GITIGNORE_CONTENT: &str = "\
# Cosmon runtime — ephemeral state is ignored in bulk; the archive subtree
# (durable, human-readable proof-of-work snapshots) is re-included via
# negation. See ADR: ARCHIVE M1 (task-20260413).
state/
!state/archive/
!state/archive/**
registry.sqlite
registry.sqlite-journal
registry.sqlite-wal
*.lock
*.tmp
";

/// Previous (selective) `.cosmon/.gitignore` body (2026-04-12 → ARCHIVE M1).
/// Tracked everything under `state/**` except a fixed blocklist. Replaced
/// by the blanket-ignore + `!state/archive/` negation scheme so the durable
/// chain of reasoning survives via the archive subsystem instead of being
/// scattered across ephemeral worker state. Detected exactly by
/// `cs init --upgrade` to migrate without clobbering user customizations.
pub const LEGACY_SELECTIVE_COSMON_GITIGNORE_CONTENT: &str = "\
# Cosmon runtime noise — durable markdown artifacts (synthesis.md,
# outcomes.md, briefing.md, notes/**, responses/**, scan.md,
# triage-report.md, analysis.md) and events.jsonl ARE tracked.
registry.sqlite
registry.sqlite-journal
registry.sqlite-wal
*.lock
*.tmp

# Runtime state — selective: ignore ephemeral/binary, track markdown.
state/fleet.json
state/**/state.json
state/**/runtime.lock
state/**/*.lock
state/**/*.pid
state/**/pty.log
state/**/tmux-capture.log
state/**/*.log
";

/// Legacy `.gitignore` block that previously carried `.worktrees/` at the git
/// root. `.worktrees/` is local by design (ephemeral per-molecule working
/// copies, never pushed), so its correct home is `.git/info/exclude` — a
/// per-clone notebook, not the shared bulletin board. `cs init --upgrade`
/// detects this block verbatim and relocates the rule to `.git/info/exclude`.
const GITIGNORE_ENTRIES: &str = "\
# Cosmon worktrees — ephemeral per-molecule working copies
.worktrees/
";

/// Line written to `.git/info/exclude` so that `.worktrees/` is treated as
/// a per-clone exclusion and never surfaces to the shared `.gitignore`.
///
/// ADR-055 §3.1 — solo = total local invisibility. `.worktrees/` is always
/// local (no submodules, no push), so even team/remote residences keep the
/// rule in `.git/info/exclude` rather than polluting the tracked
/// `.gitignore` with a rule nobody else needs.
const WORKTREES_EXCLUDE_COMMENT: &str =
    "# Cosmon worktrees — ephemeral per-molecule working copies";
const WORKTREES_EXCLUDE_LINE: &str = ".worktrees/";

/// Previous version of `GITIGNORE_ENTRIES` (pre-consolidation) that duplicated
/// `.cosmon/`-prefixed rules in the root `.gitignore`. Detected by
/// `cs init --upgrade` so the redundant entries can be removed.
const LEGACY_GITIGNORE_ENTRIES: &str = "\
# Cosmon runtime noise — durable artifacts under .cosmon/state/ are tracked
.cosmon/registry.sqlite
.cosmon/registry.sqlite-journal
.cosmon/registry.sqlite-wal
.cosmon/*.lock
.cosmon/*.tmp
.cosmon/state/fleet.json
.cosmon/state/**/state.json
.cosmon/state/**/runtime.lock
.cosmon/state/**/*.lock
.cosmon/state/**/*.pid
.cosmon/state/**/pty.log
.cosmon/state/**/tmux-capture.log
.cosmon/state/**/*.log
";

/// Legacy `.cosmon/.gitignore` body shipped by `cs init` before the
/// selective-rules refactor (2026-04-12). Detected by `cs init --upgrade`
/// to replace with the new content while leaving user-customized files
/// alone.
pub const LEGACY_COSMON_GITIGNORE_CONTENT: &str = "\
# Cosmon runtime state — not tracked in git.\n\
state/\n\
registry.sqlite\n\
registry.sqlite-journal\n\
registry.sqlite-wal\n\
*.lock\n\
*.tmp\n";

/// Legacy project `.gitignore` block (same-era sibling of
/// `LEGACY_COSMON_GITIGNORE_CONTENT`).
const LEGACY_PROJECT_GITIGNORE_BLOCK: &str = "\
# Cosmon runtime state (declarations and formulas ARE tracked)\n\
.cosmon/state/\n\
.cosmon/*.lock\n\
.cosmon/*.tmp\n";
/// Sentinel markers for the cosmon-managed section in `CLAUDE.md`.
///
/// `cs init` generates or appends this section; `cs init --upgrade` can
/// update it in-place without overwriting user content above/below.
pub const COSMON_SECTION_START: &str = "<!-- cosmon:start -->";
pub const COSMON_SECTION_END: &str = "<!-- cosmon:end -->";

/// Infer a human-readable project name from the directory name.
#[must_use]
pub fn infer_project_name(project_root: &Path) -> String {
    project_root.file_name().map_or_else(
        || "project".to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Generate the cosmon section content for `CLAUDE.md`.
///
/// This is the portable convention genome — minimal pointers to the
/// authoritative references (`cs help`, `cs help guide`, `man cs`).
/// No paraphrasing of commands, workflows, or gates — the agent reads
/// `cs help` at runtime. Maximum entropy per line, zero drift.
fn generate_cosmon_section(_project_root: &Path) -> String {
    format!(
        "{COSMON_SECTION_START}\n\
         ## Cosmon\n\
         \n\
         Run `cs help` for the full command reference.\n\
         Run `cs help guide` for the operator handbook.\n\
         Run `man cs` for the manual page.\n\
         \n\
         Source of truth: `.cosmon/state/` (JSON). Surfaces are projections — never edit directly.\n\
         {COSMON_SECTION_END}\n"
    )
}

/// Generate or update `CLAUDE.md` in the project root.
///
/// Three cases:
/// 1. No `CLAUDE.md` exists → create it with the cosmon section.
/// 2. `CLAUDE.md` exists but has no cosmon section → append it.
/// 3. `CLAUDE.md` exists with cosmon section markers → replace in-place.
///
/// Returns `true` if the file was created or modified.
///
/// # Errors
///
/// [`UpgradeError::Io`] when `CLAUDE.md` cannot be read or written.
pub fn generate_claude_md(project_root: &Path) -> Result<bool, UpgradeError> {
    let claude_md_path = project_root.join("CLAUDE.md");
    let section = generate_cosmon_section(project_root);
    let name = infer_project_name(project_root);

    if !claude_md_path.exists() {
        // Case 1: create fresh CLAUDE.md.
        let content = format!("# {name}\n\n{section}");
        fs::write(&claude_md_path, content)?;
        return Ok(true);
    }

    let existing = fs::read_to_string(&claude_md_path)?;

    if let (Some(start_idx), Some(end_idx)) = (
        existing.find(COSMON_SECTION_START),
        existing.find(COSMON_SECTION_END),
    ) {
        // Case 3: replace existing cosmon section.
        let end_of_marker = end_idx + COSMON_SECTION_END.len();
        // Consume trailing newline if present.
        let end_of_marker = if existing[end_of_marker..].starts_with('\n') {
            end_of_marker + 1
        } else {
            end_of_marker
        };
        let before = &existing[..start_idx];
        let after = &existing[end_of_marker..];
        let updated = format!("{before}{section}{after}");
        if updated == existing {
            return Ok(false);
        }
        fs::write(&claude_md_path, updated)?;
        return Ok(true);
    }

    // Case 2: append cosmon section.
    let separator = if existing.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    let updated = format!("{existing}{separator}{section}");
    fs::write(&claude_md_path, updated)?;
    Ok(true)
}
/// Generate `.cosmon/config.toml` with the project identity.
///
/// Writes the `[project]` section containing the generated `project_id`,
/// and optionally a `noyau = "<tenant>"` key (ADR-063 layer 3) when the
/// caller supplied one. Does not overwrite an existing `config.toml`.
///
/// Public because both entry points must write the same bytes: the
/// fresh-init path in `cs init` and the backfill pass of
/// [`upgrade_project`]. A second copy of this template is how the two
/// would drift.
///
/// # Errors
///
/// [`UpgradeError::Io`] when the file cannot be written.
#[allow(clippy::too_many_lines)]
pub fn generate_config_toml(
    cosmon_dir: &Path,
    project_id: &ProjectId,
    tenant: Option<&str>,
) -> Result<(), UpgradeError> {
    let config_path = cosmon_dir.join("config.toml");
    if config_path.exists() {
        return Ok(()); // Don't overwrite existing config.
    }

    let noyau_line = match tenant {
        Some(t) => format!("noyau = \"{t}\"\n"),
        None => String::new(),
    };

    let toml = format!(
        "# Cosmon project configuration.\n\
         # Generated by `cs init`. Do not remove the [project] section.\n\
         #\n\
         # Everything below [project] is commented out by default — uncomment\n\
         # the keys you want to set. See docs/project-config.md in the cosmon\n\
         # repo for the full schema.\n\
         \n\
         [project]\n\
         project_id = \"{project_id}\"\n\
         {noyau_line}\
         #\n\
         # The git repository this galaxy's work lands in. Left unset, it is\n\
         # whichever repository contains the directory `cs` was fired from —\n\
         # which is a coincidence, not a declaration, and branches the wrong\n\
         # repository in silence when the directory is wrong. A relative path\n\
         # resolves against this galaxy's root; \".\" says the galaxy is its\n\
         # own repository.\n\
         # target_repo = \".\"\n\
         \n\
         # ── Worker behavior ───────────────────────────────────────────────\n\
         # What a worker does after completing its molecule.\n\
         # Options: \"commit\" (default), \"commit+push\", \"commit+push+pr\"\n\
         # [worker]\n\
         # on_complete = \"commit\"\n\
         \n\
         # ── Lifecycle hooks ───────────────────────────────────────────────\n\
         # Shell commands run at specific lifecycle points, from the repo root.\n\
         # `pre_done` is BLOCKING: it runs before the merge as\n\
         # `sh -c '<pre_done>' -- <molecule-id>` and a non-zero exit ABORTS the\n\
         # whole teardown (nothing merged) — the galaxy-owned Definition-of-Done\n\
         # gate. Operator kill-switch: `cs done --skip-pre-done-hook` /\n\
         # COSMON_SKIP_PRE_DONE_HOOK. `post_merge` is advisory: it runs after the\n\
         # merge lands and a non-zero exit only warns.\n\
         # [hooks]\n\
         # pre_done   = \"tools/ci/verify-functional-evidence.sh\"  # before merge (blocking)\n\
         # post_merge = \"just install\"   # after `cs done` merges a worker branch\n\
         \n\
         # ── Project verification gates ────────────────────────────────────\n\
         # Language-agnostic shell commands used by `cs tackle` to tell the\n\
         # worker what \"green\" looks like. All fields are optional; set only\n\
         # the ones that apply to your stack. Language hints:\n\
         #\n\
         #   Rust:    build_command     = \"cargo check --workspace\"\n\
         #            test_command      = \"cargo test --workspace\"\n\
         #            lint_command      = \"cargo clippy --workspace -- -D warnings\"\n\
         #            format_command    = \"cargo fmt --all -- --check\"\n\
         #            doc_command       = \"RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps\"\n\
         #\n\
         #   Python:  setup_command     = \"uv sync\"\n\
         #            build_command     = \"uv sync\"\n\
         #            test_command      = \"pytest\"\n\
         #            lint_command      = \"ruff check .\"\n\
         #            format_command    = \"ruff format --check .\"\n\
         #            typecheck_command = \"mypy .\"\n\
         #\n\
         #   Node:    setup_command     = \"npm ci\"\n\
         #            build_command     = \"npm run build\"\n\
         #            test_command      = \"npm test\"\n\
         #            lint_command      = \"eslint .\"\n\
         #            format_command    = \"prettier --check .\"\n\
         #            typecheck_command = \"tsc --noEmit\"\n\
         #\n\
         #   Go:      build_command     = \"go build ./...\"\n\
         #            test_command      = \"go test ./...\"\n\
         #            lint_command      = \"golangci-lint run\"\n\
         #            format_command    = \"gofmt -l .\"\n\
         # [gates]\n\
         # build_command     = \"\"\n\
         # test_command      = \"\"\n\
         # lint_command      = \"\"\n\
         # format_command    = \"\"\n\
         # typecheck_command = \"\"\n\
         # setup_command     = \"\"\n\
         # doc_command       = \"\"\n\
         \n\
         # ── Operator notification channels ────────────────────────────────\n\
         # `cs notify <message>` and `cs patrol --silence-detect` push one-line\n\
         # alerts to every channel listed in `notify.channels`. Pick the\n\
         # subset that matches your environment; an empty/missing block makes\n\
         # `cs notify` a silent no-op (still safe for hooks to call).\n\
         #\n\
         # Channels:\n\
         #   macos      – osascript display notification (macOS only)\n\
         #   file-drop  – write a Markdown file the operator's watchers see\n\
         #   element    – POST a JSON payload to a Matrix/Element webhook\n\
         #   telegram   – POST to the Telegram Bot API sendMessage endpoint\n\
         #\n\
         # Set COSMON_NOTIFY_DRY_RUN=1 to skip every transport (CI default).\n\
         # [notify]\n\
         # channels = [\"macos\", \"file-drop\"]\n\
         #\n\
         # [notify.macos]\n\
         # sound = \"default\"\n\
         #\n\
         # [notify.file-drop]\n\
         # path = \"~/Drop/cosmon-notifications/\"\n\
         #\n\
         # [notify.element]\n\
         # webhook_url = \"https://your.element.host/_matrix/client/...\"\n\
         #\n\
         # [notify.telegram]\n\
         # bot_token = \"123456789:ABCdefGhIJKlmNoPQRsTUVwxyz\"  # from @BotFather\n\
         # chat_id = \"100000000\"                              # DM user id or group id\n"
    );

    fs::write(&config_path, toml)?;
    Ok(())
}
/// Walk upward from `start` looking for an ancestor `.git` entry. Returns
/// the first match or `None` if no repository contains the path.
fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut cursor = start.to_path_buf();
    if let Ok(canon) = cursor.canonicalize() {
        cursor = canon;
    }
    loop {
        if cursor.join(".git").exists() {
            return Some(cursor);
        }
        if !cursor.pop() {
            return None;
        }
    }
}

/// Ensure `.git/info/exclude` carries the `.worktrees/` rule.
///
/// `.worktrees/` is always a per-clone artefact (no submodules, no push),
/// so the rule belongs in `.git/info/exclude` rather than in the shared
/// `.gitignore`. Idempotent: returns `true` when the file was modified,
/// `false` if the rule was already present or if there is no git
/// repository. Best-effort — returns `Ok(false)` when no ancestor `.git`
/// is found so fresh init never fails on a non-git directory.
///
/// When the file does not yet exist, a minimal `.git/info/` layout is
/// materialised so the write succeeds. Existing content is preserved
/// byte-for-byte apart from the appended rule.
///
/// # Errors
///
/// [`UpgradeError::Io`] when `.git/info/exclude` cannot be written.
pub fn ensure_worktrees_in_exclude(project_root: &Path) -> Result<bool, UpgradeError> {
    let Some(git_root) = find_git_root(project_root) else {
        return Ok(false);
    };
    let exclude_path = git_root.join(".git/info/exclude");
    let body = fs::read_to_string(&exclude_path).unwrap_or_default();
    if body.lines().any(|l| l.trim_end() == WORKTREES_EXCLUDE_LINE) {
        return Ok(false);
    }
    let mut new_body = body.clone();
    if !new_body.is_empty() && !new_body.ends_with('\n') {
        new_body.push('\n');
    }
    // Include the comment alongside the rule so an operator reading
    // `.git/info/exclude` by hand knows who wrote the line.
    if !body.contains(WORKTREES_EXCLUDE_COMMENT) {
        new_body.push_str(WORKTREES_EXCLUDE_COMMENT);
        new_body.push('\n');
    }
    new_body.push_str(WORKTREES_EXCLUDE_LINE);
    new_body.push('\n');
    if let Some(parent) = exclude_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&exclude_path, new_body)?;
    Ok(true)
}

/// Strip any legacy Cosmon block from the project `.gitignore` body.
///
/// Removes:
///   * Exact legacy blocks (`LEGACY_GITIGNORE_ENTRIES`,
///     `LEGACY_PROJECT_GITIGNORE_BLOCK`, `GITIGNORE_ENTRIES`).
///   * Any single-line `.cosmon/` or `.worktrees/` entry left behind.
///   * Orphan `# Cosmon *` header comments that are followed by blank
///     lines or another comment (no content left to narrate).
///
/// The returned string may equal the input when no cleanup was needed.
#[must_use]
pub fn strip_cosmon_gitignore_block(body: &str) -> String {
    let mut out = body.to_owned();
    // Remove recognised exact blocks first — this catches the tidiest
    // variants and preserves surrounding whitespace better than a line
    // sweep would.
    for pat in [
        GITIGNORE_ENTRIES,
        LEGACY_GITIGNORE_ENTRIES,
        LEGACY_PROJECT_GITIGNORE_BLOCK,
    ] {
        if out.contains(pat) {
            out = out.replace(pat, "");
        }
    }

    // Line sweep: strip remaining Cosmon rules and any Cosmon header
    // comment. Header comments starting with `# Cosmon` are exclusively
    // used by `cs init` / `cs migrate` — a user writing a gitignore
    // comment about Cosmon would most likely quote it anyway, and a
    // false positive here leaves the rule below intact.
    let mut kept: Vec<String> = Vec::new();
    for line in out.lines() {
        let t = line.trim();
        if t == ".cosmon/" || t == ".worktrees/" {
            continue;
        }
        if t.starts_with("# Cosmon") {
            continue;
        }
        kept.push(line.to_owned());
    }
    let mut result = kept.join("\n");
    if body.ends_with('\n') && !result.is_empty() {
        result.push('\n');
    }

    // Collapse runs of 3+ blank lines introduced by the block removal.
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }
    // Trim leading blank lines — a stripped header at the very top
    // leaves a blank prologue that looks like editor litter.
    while result.starts_with('\n') {
        result.remove(0);
    }
    result
}
/// Rewrite the cosmon-managed part of a `.cosmon/.gitignore` body.
///
/// Returns the new body, or `None` when nothing should change.
///
/// Ownership is deliberately narrow, in four cases:
///
/// 1. The body already is the current canonical block — nothing to do.
/// 2. The body is one of the recognised legacy bodies, byte for byte —
///    replaced wholesale, the discipline that has always applied.
/// 3. The body carries [`COSMON_GITIGNORE_BLOCK_START`] and
///    [`COSMON_GITIGNORE_BLOCK_END`] — only the lines between the markers
///    are replaced; everything outside them is preserved byte for byte.
///    This is how cosmon keeps owning its rules in a file a user also edits.
/// 4. The body is customized and carries no marker — left alone, unless
///    `repair` is set, in which case the managed block is *appended*. An
///    appended block adds cosmon's own lines and removes none of the
///    user's; because git resolves an ignore file last-match-wins, and
///    because the block opens with `!state/`, it binds regardless of what
///    precedes it.
///
/// Case 4 is the answer to the mangled-file report in issue #60: an agent
/// had rewritten a galaxy's file into a chain of rules that ignored and
/// re-included each other, and the exact-match rule meant `cs init
/// --upgrade` would never look at it again. Appending under a marker keeps
/// the reason exact-match exists — a user's deliberate edits are not
/// cosmon's to overwrite — while giving cosmon a lane of its own.
///
/// # Example
///
/// ```
/// use cosmon_filestore::project_upgrade::{
///     rewrite_cosmon_gitignore, COSMON_GITIGNORE_CONTENT,
/// };
///
/// // A pristine current file needs nothing.
/// assert!(rewrite_cosmon_gitignore(COSMON_GITIGNORE_CONTENT, false).is_none());
///
/// // A customized file is left alone until it is known to be broken.
/// assert!(rewrite_cosmon_gitignore("my-own-rule\n", false).is_none());
/// let repaired = rewrite_cosmon_gitignore("my-own-rule\n", true).unwrap();
/// assert!(repaired.starts_with("my-own-rule\n"));
/// assert!(repaired.contains("!state/archive/**"));
/// ```
#[must_use]
pub fn rewrite_cosmon_gitignore(body: &str, repair: bool) -> Option<String> {
    if body == COSMON_GITIGNORE_CONTENT {
        return None;
    }
    if body == LEGACY_BROKEN_NEGATION_GITIGNORE_CONTENT
        || body == LEGACY_COSMON_GITIGNORE_CONTENT
        || body == LEGACY_SELECTIVE_COSMON_GITIGNORE_CONTENT
    {
        return Some(COSMON_GITIGNORE_CONTENT.to_owned());
    }
    if let Some(updated) = replace_managed_block(body) {
        return (updated != body).then_some(updated);
    }
    if !repair {
        return None;
    }
    let mut out = body.to_owned();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(COSMON_GITIGNORE_BLOCK);
    Some(out)
}

/// Replace the marked region of `body` with the current managed block.
///
/// `None` when the body carries no complete marker pair — the caller then
/// decides whether to append one.
fn replace_managed_block(body: &str) -> Option<String> {
    let start = body.find(COSMON_GITIGNORE_BLOCK_START)?;
    let end_marker = body[start..].find(COSMON_GITIGNORE_BLOCK_END)? + start;
    // Consume the end marker line up to and including its newline, so the
    // replacement block (which ends in one) does not double it.
    let after = body[end_marker..]
        .find('\n')
        .map_or(body.len(), |i| end_marker + i + 1);
    let mut out = String::with_capacity(body.len() + COSMON_GITIGNORE_BLOCK.len());
    out.push_str(&body[..start]);
    out.push_str(COSMON_GITIGNORE_BLOCK);
    out.push_str(&body[after..]);
    Some(out)
}

/// Ask real git whether the archive subtree of `cosmon_dir` is ignored.
///
/// Returns the `git check-ignore -v` verdict (`<file>:<line>:<pattern>`)
/// naming the rule that excludes a representative archive artifact, and
/// `None` when nothing excludes it — or when git is unavailable, which is
/// not a diagnosis and must not be reported as one.
///
/// WHY shell out rather than evaluate the rules here: an ignore file that
/// says one thing and does another is exactly the defect of issue #60, and
/// a re-implementation of git's precedence rules is a second place for the
/// same class of mistake to hide. git is the authority on what git ignores.
///
/// WHY two invocations: `check-ignore -v` exits 0 whenever *any* pattern
/// matches, negations included, so its exit status alone cannot answer the
/// question. The plain form exits 1 for a re-included path and is the
/// verdict; the verbose form is then run only to name the rule.
///
/// The probed path is synthetic and never created — `check-ignore` answers
/// for paths that do not exist.
#[must_use]
pub fn archive_subtree_ignored_rule(project_root: &Path, cosmon_dir: &Path) -> Option<String> {
    let git_root = find_git_root(project_root)?;
    let probe = cosmon_dir
        .join("state")
        .join("archive")
        .join("2026")
        .join("01")
        .join("probe")
        .join("result.md");
    let check = |verbose: bool| {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&git_root).arg("check-ignore");
        if verbose {
            cmd.arg("-v");
        }
        cmd.arg("--no-index").arg("--").arg(&probe).output().ok()
    };
    if !check(false)?.status.success() {
        return None;
    }
    let verbose = check(true)?;
    let named = String::from_utf8_lossy(&verbose.stdout).trim().to_owned();
    Some(if named.is_empty() {
        "ignored by an unnamed rule".to_owned()
    } else {
        named
    })
}

/// Upgrade legacy gitignore rules to the consolidated scheme.
///
/// Rewrites:
///   * `.cosmon/.gitignore`, per [`rewrite_cosmon_gitignore`]: a recognised
///     legacy body is replaced wholesale, a marked body has only its marked
///     region rewritten, and a customized marker-less body is left alone
///     unless real git reports the archive subtree ignored — in which case
///     the managed block is appended below the user's lines.
///   * The project `.gitignore` at the git root: removes every legacy
///     Cosmon block — `.cosmon/`, `.cosmon/state/`, `.worktrees/`, orphan
///     `# Cosmon …` comments — and writes `.worktrees/` to
///     `.git/info/exclude` instead. `.worktrees/` is per-clone by design
///     (ephemeral per-molecule working copies), so it does not belong on
///     the shared bulletin board.
///
/// Returns true if either file was updated.
fn upgrade_gitignore_rules(project_root: &Path, cosmon_dir: &Path) -> bool {
    let mut changed = false;

    // .cosmon/.gitignore — see `rewrite_cosmon_gitignore` for the ownership
    // rules. A customized, marker-less body is rewritten only when real git
    // says the archive subtree is currently ignored, which is the one state
    // the user cannot have intended while the archive is writing into it.
    let cosmon_ignore = cosmon_dir.join(".gitignore");
    if let Ok(body) = fs::read_to_string(&cosmon_ignore) {
        let broken = archive_subtree_ignored_rule(project_root, cosmon_dir).is_some();
        if let Some(updated) = rewrite_cosmon_gitignore(&body, broken) {
            if fs::write(&cosmon_ignore, &updated).is_ok() {
                changed = true;
            }
        }
    } else if !cosmon_ignore.exists() && fs::write(&cosmon_ignore, COSMON_GITIGNORE_CONTENT).is_ok()
    {
        changed = true;
    }

    // Project root .gitignore — remove every Cosmon block (including the
    // `.worktrees/` rule previously written here) and relocate the
    // `.worktrees/` exclusion to `.git/info/exclude`. `.worktrees/` is
    // local by design — per-clone notebook, not shared bulletin board.
    if let Some(git_root) = find_git_root(project_root) {
        let path = git_root.join(".gitignore");
        if let Ok(body) = fs::read_to_string(&path) {
            let updated = strip_cosmon_gitignore_block(&body);
            if updated != body {
                if updated.trim().is_empty() {
                    // Keep the file but empty — removing a tracked file
                    // is a separate git operation the operator can do.
                    if fs::write(&path, "").is_ok() {
                        changed = true;
                    }
                } else if fs::write(&path, &updated).is_ok() {
                    changed = true;
                }
            }
        }
        if ensure_worktrees_in_exclude(project_root).unwrap_or(false) {
            changed = true;
        }
    }

    changed
}
/// Upgrade an existing `.cosmon/` project by backfilling everything a
/// current cosmon expects to find there.
///
/// Runs seven independent backfill passes — canonical formulas, the
/// neurion registry, the `state/` tree, `project_id`, the gitignore
/// migration, `CLAUDE.md`, the gitleaks baseline — without overwriting
/// any existing file. Pre-formula-bundling projects (cosmon ≤ April
/// 2026) end up with an empty `.cosmon/formulas/` directory and cannot
/// nucleate anything until canonical templates are restored; this
/// function fixes that trap while preserving every user customization
/// on disk.
///
/// `root` is the galaxy root (the directory *containing* `.cosmon/`).
///
/// # Errors
///
/// [`UpgradeError::MissingCosmonDir`] when `root/.cosmon` does not
/// exist, [`UpgradeError::Registry`] when `registry.sqlite` cannot be
/// created or seeded, and [`UpgradeError::Io`] for any other
/// filesystem failure.
///
/// # Example
///
/// ```no_run
/// use cosmon_filestore::project_upgrade::{upgrade_project, UpgradeOptions};
///
/// let report = upgrade_project(std::path::Path::new("/srv/galaxies/demo"), &UpgradeOptions::default())?;
/// assert_eq!(report.status(), "upgraded");
/// # Ok::<(), cosmon_filestore::project_upgrade::UpgradeError>(())
/// ```
#[allow(clippy::too_many_lines)]
pub fn upgrade_project(root: &Path, _opts: &UpgradeOptions) -> Result<UpgradeReport, UpgradeError> {
    let cosmon_dir = root.join(".cosmon");
    if !cosmon_dir.exists() {
        return Err(UpgradeError::MissingCosmonDir);
    }

    // --- Pass 1: backfill missing canonical formulas ------------------------
    let formulas_dir = cosmon_dir.join("formulas");
    fs::create_dir_all(&formulas_dir)?;

    let mut added_formulas: Vec<String> = Vec::new();
    for (name, contents) in BUILTIN_FORMULAS {
        let path = formulas_dir.join(name);
        if path.exists() {
            continue; // Preserve user customizations — never overwrite.
        }
        fs::write(&path, contents)?;
        added_formulas.push((*name).to_string());
    }

    // --- Pass 2: backfill registry.sqlite -----------------------------------
    // A fresh clone has no registry.sqlite (it's gitignored), so any cs command
    // that touches the neurion registry fails with database-not-found. Re-seed
    // with the canonical schema + default referents, matching the fresh-init
    // path in `cs init`. Idempotent: if the file exists, assume the schema is
    // there.
    let registry_path = cosmon_dir.join("registry.sqlite");
    let registry_added = !registry_path.exists();
    if registry_added {
        seed_registry(&registry_path)?;
    }

    // --- Pass 3: backfill state/ directory tree -----------------------------
    // state/ is gitignored, so a fresh clone lacks fleet.json and the default
    // fleet's molecule directory. Rather than lazy-create on every write path,
    // materialize the layout here so the runtime code can assume it exists.
    let state_dir = cosmon_dir.join("state");
    let molecules_dir = state_dir.join("fleets/default/molecules");
    let state_added = !state_dir.exists();
    fs::create_dir_all(&molecules_dir)?;
    let fleet_json = state_dir.join("fleet.json");
    if !fleet_json.exists() {
        fs::write(&fleet_json, "{\"workers\":{},\"repos\":{}}\n")?;
    }

    // --- Pass 4: backfill project_id into config.toml -----------------------
    let config_path = cosmon_dir.join("config.toml");
    let existing_content = fs::read_to_string(&config_path).unwrap_or_default();

    let existing_project_id = if existing_content.is_empty() {
        None
    } else {
        cosmon_core::config::ProjectConfig::parse(&existing_content)
            .ok()
            .and_then(|c| c.project.project_id)
    };

    let (project_id, project_id_added) = if let Some(pid) = existing_project_id {
        (pid, false)
    } else {
        let project_id = ProjectId::generate(root);
        if existing_content.is_empty() || !config_path.exists() {
            // Upgrade path does not surface a tenant — the operator must
            // re-run `cs init --tenant <noyau>` from scratch on a fresh
            // galaxy if multi-tenant labelling is required.
            generate_config_toml(&cosmon_dir, &project_id, None)?;
        } else {
            let project_section = format!("[project]\nproject_id = \"{project_id}\"\n\n");
            let upgraded = if existing_content.contains("[project]") {
                existing_content.replacen(
                    "[project]",
                    &format!("[project]\nproject_id = \"{project_id}\""),
                    1,
                )
            } else {
                format!("{project_section}{existing_content}")
            };
            fs::write(&config_path, upgraded)?;
        }
        (project_id, true)
    };

    // --- Pass 5: upgrade legacy gitignore rules -----------------------------
    // Pre-2026-04-12 init shipped a blanket `state/` ignore rule, which
    // swallowed durable deliberation artifacts (synthesis.md, outcomes.md,
    // events.jsonl, ...). Detect the exact legacy body and replace with the
    // selective rules. User-customized gitignores are left alone.
    let gitignore_upgraded = upgrade_gitignore_rules(root, &cosmon_dir);

    // --- Pass 6: backfill CLAUDE.md ------------------------------------------
    let claude_md_updated = generate_claude_md(root)?;

    // --- Pass 7: backfill repo-root .gitleaks.toml --------------------------
    // Existing galaxies hit the `already_initialized` early-return on a bare
    // `cs init`, so the gitleaks baseline only reaches them through `--upgrade`.
    // Customization-preserving: a pre-existing `.gitleaks.toml` is left
    // untouched (the operator may have extended it). This closes the
    // `cs done`-blocked-by-gitleaks gap (task-20260623-e9f0) for galaxies
    // already in flight, not just freshly-born ones.
    let gitleaks_config = root.join(".gitleaks.toml");
    let gitleaks_added = !gitleaks_config.exists();
    if gitleaks_added {
        fs::write(&gitleaks_config, COSMON_GITLEAKS_BASELINE)?;
    }

    Ok(UpgradeReport {
        project_id,
        added_formulas,
        project_id_added,
        registry_added,
        state_added,
        gitignore_upgraded,
        claude_md_updated,
        gitleaks_added,
    })
}

/// Create `registry.sqlite` and apply the canonical neurion schema plus
/// the three default referents.
///
/// Split out of [`upgrade_project`] so the `rusqlite` surface is
/// confined to one function and the `SQLite` error text is converted to
/// [`UpgradeError::Registry`] at exactly one place. Shared with the
/// fresh-init path in `cs init`, which is why it is public: the two
/// paths must seed byte-identical databases or a galaxy born fresh and
/// a galaxy upgraded would disagree about what the registry contains.
///
/// # Errors
///
/// [`UpgradeError::Registry`] when the file cannot be opened as a
/// database or a schema statement fails.
pub fn seed_registry(registry_path: &Path) -> Result<(), UpgradeError> {
    let conn = rusqlite::Connection::open(registry_path)
        .map_err(|e| UpgradeError::Registry(format!("failed to create registry: {e}")))?;
    conn.execute_batch(neurion_core::schema::SCHEMA_SQL)
        .map_err(|e| {
            UpgradeError::Registry(format!("failed to initialize registry schema: {e}"))
        })?;
    conn.execute_batch(neurion_core::schema::HYPERGRAPH_SQL)
        .map_err(|e| {
            UpgradeError::Registry(format!("failed to initialize hypergraph schema: {e}"))
        })?;
    conn.execute_batch(
        "INSERT OR IGNORE INTO referents (name, description) VALUES
         ('project.status', 'Current state of fleets, workers, and molecules'),
         ('project.issues', 'Tracked issues, blockers, and work items'),
         ('project.decisions', 'Architecture Decision Records');",
    )
    .map_err(|e| UpgradeError::Registry(format!("failed to seed referents: {e}")))?;
    Ok(())
}
