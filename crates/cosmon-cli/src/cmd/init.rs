// SPDX-License-Identifier: AGPL-3.0-only

//! `cs init` — initialize a project-local `.cosmon/` directory.
//!
//! Creates the directory structure that [`cosmon_filestore::resolve_state_dir`]
//! will discover via walk-up. The minimal primitive: create the target
//! directory if it does not exist, then populate `.cosmon/{config.toml,
//! state/, formulas/, …}`. Nothing else — `git init` is git's job, and
//! `CLAUDE.md` is a formula's job (e.g. the `galaxy-onboarding` formula).
//! Symmetric undo: `rm -rf <path>/.cosmon/` removes every artifact this
//! command creates. The only refusal is nested galaxies: if any ancestor
//! of the target already carries a `.cosmon/`, `cs init` errors out — no
//! `--force` escape, because nested galaxies silently break walk-up
//! discovery.

use std::fs;
use std::path::{Path, PathBuf};

use cosmon_core::id::ProjectId;

use super::Context;

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
const BUILTIN_FORMULAS: &[(&str, &str)] = &[
    (
        "deep-think.formula.toml",
        include_str!("../../../../.cosmon/formulas/deep-think.formula.toml"),
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
        include_str!("../../../../.cosmon/formulas/deep-think-inline.formula.toml"),
    ),
    (
        "task-work.formula.toml",
        include_str!("../../../../.cosmon/formulas/task-work.formula.toml"),
    ),
    (
        "idea-to-plan.formula.toml",
        include_str!("../../../../.cosmon/formulas/idea-to-plan.formula.toml"),
    ),
    (
        "mission-plan.formula.toml",
        include_str!("../../../../.cosmon/formulas/mission-plan.formula.toml"),
    ),
    (
        "temp-review.formula.toml",
        include_str!("../../../../.cosmon/formulas/temp-review.formula.toml"),
    ),
    (
        "mission-controller.formula.toml",
        include_str!("../../../../.cosmon/formulas/mission-controller.formula.toml"),
    ),
    (
        "editorial-work.formula.toml",
        include_str!("../../../../.cosmon/formulas/editorial-work.formula.toml"),
    ),
    // The independent visual witness required by the `surface_visual`
    // mindguard. Builtin because the gate's remedy prescribes
    // `cs nucleate verify-surface` on EVERY galaxy: a fleet without
    // this formula cannot satisfy a refused `cs complete` at all (the
    // automata blocker of 2026-06-07 — gate shipped without its
    // remedy).
    (
        "verify-surface.formula.toml",
        include_str!("../../../../.cosmon/formulas/verify-surface.formula.toml"),
    ),
];

/// Project-type templates for `cs init --soft`.
///
/// Each variant maps to a different CLAUDE.md template tailored to the
/// project's technology stack. The template content is embedded at compile
/// time — no external files required.
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
pub enum ProjectTemplate {
    /// Generic project — language-agnostic conventions.
    #[default]
    Generic,
    /// Rust project — cargo-based conventions.
    Rust,
    /// Data/research project — notebook + pipeline conventions.
    Data,
}

/// Arguments for the `init` subcommand.
#[derive(clap::Args)]
#[allow(clippy::struct_excessive_bools)]
#[command(
    about = "Create a project-local .cosmon/ directory",
    long_about = "Create a project-local .cosmon/ directory.\n\
                  \n\
                  Accepts a target path (default: current directory). If the \
                  path does not exist, it is created (`mkdir -p`). \
                  Idempotent: running `cs init` twice on the same path is a \
                  no-op (exit 0, no changes).\n\
                  \n\
                  Refuses to clobber an existing `.cosmon/`: if any ANCESTOR \
                  of the target carries one, `cs init` exits non-zero — \
                  nested galaxies are forbidden and there is no `--force` \
                  escape. Walk-up detection preserves the invariant that \
                  every `.cosmon/` is its own galaxy root.\n\
                  \n\
                  Does NOT run `git init` (that is git's job) and does NOT \
                  write `CLAUDE.md` by default — pass `--soft` to generate \
                  an agent-instruction template. Symmetric undo: \
                  `rm -rf <path>/.cosmon/`."
)]
pub struct Args {
    /// Directory to initialize (default: current directory).
    ///
    /// Need not exist. If it does not, `cs init` creates it with
    /// `mkdir -p`. If it exists and already contains `.cosmon/`, the
    /// command is a no-op (strict idempotency).
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Upgrade an existing `.cosmon/` project by backfilling missing
    /// canonical formulas AND `project_id` without overwriting existing files.
    #[arg(long)]
    upgrade: bool,

    /// Generate only a minimal CLAUDE.md (≤50 lines) without creating `.cosmon/`.
    ///
    /// Constitutional projection: conventions propagated via a single file
    /// that any agent can read independently. No orchestration infrastructure,
    /// no runtime state, no external dependency.
    #[arg(long)]
    soft: bool,

    /// Project-type template for `--soft` (default: generic).
    #[arg(long, value_enum, default_value_t = ProjectTemplate::Generic)]
    template: ProjectTemplate,

    /// Deprecated no-op. `cs init` never runs `git init` — git's lifecycle
    /// is managed by the user. Kept for backward-compatible CLI parsing.
    #[arg(long, hide = true)]
    no_git: bool,

    /// Assume "yes" to any confirmation prompt (non-interactive mode).
    ///
    /// `cs init` is already fully non-interactive today, so this flag is
    /// accepted and ignored. It is reserved so that README quickstarts
    /// (`cs init --yes`) stay paste-testable if prompts are ever added —
    /// per the knuth paste-testability invariant and tolnay's semver
    /// rule that reserving a flag now makes adding prompts later a
    /// non-breaking elaboration.
    #[arg(long, short = 'y')]
    yes: bool,

    /// Tenant (noyau) this galaxy belongs to. Records the ADR-063 layer-3
    /// label in `config.toml` and provisions the
    /// `.cosmon/state/nucleons/` directory where ADR-080 OIDC identity
    /// mappings (`oidc-identity.toml`) and future `YubiKey` keyring
    /// entries land.
    ///
    /// Convention: one tenant per galaxy below the configured cluster root.
    /// This flag records the label but does not enforce a path. Downstream
    /// tools read the `noyau` to verify the galaxy belongs to its tenant.
    #[arg(long, value_name = "NOYAU")]
    tenant: Option<String>,
}

/// Execute the `init` command.
///
/// Creates `.cosmon/` with the standard layout:
/// ```text
/// .cosmon/
///   .gitignore      # ignores state/, *.lock, *.tmp
///   config.toml     # project identity + gates + hooks
///   surfaces.toml   # surface projections
///   formulas/       # formula templates (git-tracked)
///   molecules/      # molecule declarations (git-tracked)
///   state/          # runtime state (git-ignored)
///     fleet.json
///     fleets/default/molecules/
/// ```
///
/// The target path need not exist — it is created with `mkdir -p` when
/// absent. Running `cs init` twice on the same path is a strict no-op
/// (idempotent). If any ancestor of the target already carries a
/// `.cosmon/`, the command refuses with a non-zero exit — there is no
/// `--force` escape, because nested galaxies break walk-up discovery.
///
/// This command does **not** run `git init` and does **not** write
/// `CLAUDE.md`. Those belong to git and to a dedicated formula
/// respectively (see the `galaxy-onboarding` formula). The symmetric
/// undo is therefore `rm -rf <path>/.cosmon/`.
///
/// # Errors
///
/// Returns an error if the directory cannot be created, or if the target
/// is inside an existing cosmon galaxy (nested-galaxy refusal).
#[allow(clippy::too_many_lines)]
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    // Resolve the requested path WITHOUT requiring existence. We
    // canonicalize only after the directory has been created — otherwise
    // `canonicalize` fails on non-existent paths.
    let root_requested: PathBuf = if args.path.as_os_str() == "." {
        std::env::current_dir()?
    } else if args.path.is_absolute() {
        args.path.clone()
    } else {
        std::env::current_dir()?.join(&args.path)
    };

    if args.soft {
        let root = root_requested
            .canonicalize()
            .unwrap_or_else(|_| root_requested.clone());
        return run_soft(ctx, &root, args.template);
    }

    if args.upgrade {
        if !root_requested.exists() {
            return Err(anyhow::anyhow!(
                "cannot --upgrade a non-existent path: {}",
                root_requested.display()
            ));
        }
        let root = root_requested.canonicalize()?;
        let cosmon_dir = root.join(".cosmon");
        return run_upgrade(ctx, &root, &cosmon_dir);
    }

    let cosmon_dir = root_requested.join(".cosmon");

    // Strict idempotency: if the target already has its own `.cosmon/`,
    // emit the "already initialized" message and exit 0 without touching
    // anything on disk.
    if cosmon_dir.is_dir() {
        if ctx.json {
            let output = serde_json::json!({
                "status": "already_initialized",
                "path": cosmon_dir.display().to_string(),
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            println!("Already initialized: {}", cosmon_dir.display());
        }
        return Ok(());
    }

    // Nested-galaxy refusal — walk up from the target's parent looking for
    // an ancestor `.cosmon/`. We evaluate the LOGICAL path (no canonicalize)
    // so the check is correct even when the target does not yet exist.
    if let Some(ancestor) = find_nearest_ancestor_cosmon(&root_requested) {
        return Err(anyhow::anyhow!(
            "refusing to nest cosmon projects: ancestor `.cosmon/` exists at {}\n\
             target: {}\n\
             Pick a path outside that galaxy, or `rm -rf {}` first.\n\
             No `--force` escape — nested galaxies silently break walk-up discovery.",
            ancestor.display(),
            root_requested.display(),
            ancestor.display(),
        ));
    }

    // Create the target if it doesn't yet exist. `create_dir_all` is
    // itself idempotent (returns Ok for an existing directory).
    let created_path = !root_requested.exists();
    if created_path {
        fs::create_dir_all(&root_requested)?;
    }

    // Now that the path exists, canonicalize for a stable root reference.
    let root = root_requested
        .canonicalize()
        .unwrap_or_else(|_| root_requested.clone());
    let cosmon_dir = root.join(".cosmon");

    // --- Populate `.cosmon/` idempotently --------------------------------
    //
    // Every write below is guarded by an `exists()` check: re-running
    // after a partial write (e.g. Ctrl-C between steps) converges on the
    // same end state without clobbering anything on disk.
    let formulas_dir = cosmon_dir.join("formulas");
    fs::create_dir_all(&formulas_dir)?;
    fs::create_dir_all(cosmon_dir.join("molecules"))?;
    fs::create_dir_all(cosmon_dir.join("state/fleets/default/molecules"))?;
    // ADR-080 §8.1 multi-tenant: provision the nucleons directory so
    // `oidc-identity.toml` mappings (and future YubiKey keyring entries)
    // have a home. Created unconditionally — empty until provisioning
    // writes a nucleon.
    fs::create_dir_all(cosmon_dir.join("state/nucleons"))?;

    // Seed `.cosmon/formulas/` with the canonical templates. Preserve any
    // pre-existing file — users may have customized templates already.
    for (name, contents) in BUILTIN_FORMULAS {
        let path = formulas_dir.join(name);
        if !path.exists() {
            fs::write(&path, contents)?;
        }
    }

    // `.cosmon/.gitignore` (preserves user customization if already present).
    let cosmon_gitignore = cosmon_dir.join(".gitignore");
    if !cosmon_gitignore.exists() {
        fs::write(&cosmon_gitignore, COSMON_GITIGNORE_CONTENT)?;
    }

    // `.cosmon/state/fleet.json`.
    let fleet_json = cosmon_dir.join("state/fleet.json");
    if !fleet_json.exists() {
        fs::write(&fleet_json, "{\"workers\":{},\"repos\":{}}\n")?;
    }

    // Repo-root `.gitleaks.toml` — the federation-shared scan baseline that
    // keeps `cs done` harvests from being blocked by gitleaks' entropy-based
    // `generic-api-key` false positives on the free-text `reason` prose in
    // `.cosmon/state/events.jsonl`. Born-correct beats paste-and-diverge: a
    // galaxy that runs gitleaks in a pre-commit hook (most external-facing
    // ones eventually do) inherits the canonical allowlist instead of
    // reinventing a divergent one. Idempotent + customization-preserving: if a
    // `.gitleaks.toml` already exists we never touch it (the operator may have
    // extended it). Harmless for galaxies that don't use gitleaks — the file
    // just sits unread. See docs/guides/gitleaks-state-journals.md.
    let gitleaks_config = root.join(".gitleaks.toml");
    if !gitleaks_config.exists() {
        fs::write(&gitleaks_config, COSMON_GITLEAKS_BASELINE)?;
    }

    // Project-local nervous system (neurion registry).
    let registry_path = cosmon_dir.join("registry.sqlite");
    if !registry_path.exists() {
        let conn = rusqlite::Connection::open(&registry_path)
            .map_err(|e| anyhow::anyhow!("failed to create registry: {e}"))?;
        conn.execute_batch(neurion_core::schema::SCHEMA_SQL)
            .map_err(|e| anyhow::anyhow!("failed to initialize registry schema: {e}"))?;
        conn.execute_batch(neurion_core::schema::HYPERGRAPH_SQL)
            .map_err(|e| anyhow::anyhow!("failed to initialize hypergraph schema: {e}"))?;
        conn.execute_batch(
            "INSERT OR IGNORE INTO referents (name, description) VALUES
             ('project.status', 'Current state of fleets, workers, and molecules'),
             ('project.issues', 'Tracked issues, blockers, and work items'),
             ('project.decisions', 'Architecture Decision Records');",
        )
        .map_err(|e| anyhow::anyhow!("failed to seed referents: {e}"))?;
    }

    // `.cosmon/config.toml` with project identity.
    let project_id = ProjectId::generate(&root);
    generate_config_toml(&cosmon_dir, &project_id, args.tenant.as_deref())?;
    // Re-read to surface the authoritative id (may differ if a stale
    // config.toml was already present — generate_config_toml preserves it).
    let authoritative_project_id = fs::read_to_string(cosmon_dir.join("config.toml"))
        .ok()
        .and_then(|body| cosmon_core::config::ProjectConfig::parse(&body).ok())
        .and_then(|cfg| cfg.project.project_id)
        .unwrap_or(project_id);

    // `.cosmon/surfaces.toml` (with optional github-issues remote).
    let github_repo = detect_github_remote(&root);
    generate_surfaces_toml(&cosmon_dir, github_repo.as_deref())?;

    // `.worktrees/` lives in `.git/info/exclude` (per-clone notebook), never
    // in `.gitignore` (shared bulletin board). A kitchen is not a dish —
    // ephemeral per-molecule working copies are a property of this clone,
    // not of the project. Best-effort: silent when no git repo is found.
    let _worktrees_excluded = ensure_worktrees_in_exclude(&root).unwrap_or(false);

    let builtin_formula_names: Vec<&str> = BUILTIN_FORMULAS.iter().map(|(n, _)| *n).collect();

    if ctx.json {
        let mut output = serde_json::json!({
            "status": "initialized",
            "path": cosmon_dir.display().to_string(),
            "project_id": authoritative_project_id.as_str(),
            "created_path": created_path,
            "layout": {
                "formulas": ".cosmon/formulas/",
                "molecules": ".cosmon/molecules/",
                "state": ".cosmon/state/ (git-ignored)",
            },
            "builtin_formulas": builtin_formula_names,
        });
        if let Some(ref repo) = github_repo {
            output["github_issues"] = serde_json::json!(repo);
        }
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        if created_path {
            println!("Created directory: {}", root.display());
        }
        println!("Initialized cosmon project: {}", cosmon_dir.display());
        println!("  project_id: {authoritative_project_id}");
        println!();
        println!("  .cosmon/");
        println!("    .gitignore          # ignores runtime state");
        println!("    config.toml         # project identity + gates + hooks");
        println!("    surfaces.toml       # surface projections");
        println!("    formulas/           # formula templates (git-tracked)");
        for name in &builtin_formula_names {
            println!("      {name}");
        }
        println!("    molecules/          # molecule declarations (git-tracked)");
        println!("    state/              # runtime state (git-ignored)");
        println!("  .gitleaks.toml        # federation scan baseline (unblocks cs done harvests)");
        if let Some(ref repo) = github_repo {
            println!();
            println!("  Detected GitHub remote: {repo}");
            println!("     GitHub Issues surface auto-configured.");
        }
        println!();
        println!("Next steps:");
        println!("  1. Initialize git if needed:  `git init && git commit --allow-empty -m init`");
        println!(
            "     (cs needs at least one commit on the base branch; `cs demo`/`cs tackle` seed"
        );
        println!("      one for you if you forget, but never over existing history).");
        println!("  2. Try the full cycle in one command:  `cs demo`.");
        println!(
            "  3. Nucleate work:  `cs nucleate task-work --var topic=\"...\"`  then  `cs tackle <id>`."
        );
        println!("  4. Orient yourself:  `cs help` (commands) · `cs help guide` (handbook).");
        println!("  5. (optional) Drop a CLAUDE.md for your agent:  `cs init --soft`.");
        println!();
        println!("Symmetric undo: rm -rf {}", cosmon_dir.display());
    }

    Ok(())
}

/// Walk upward from `path.parent()` looking for an ancestor cosmon
/// **project root** — a directory containing `.cosmon/config.toml` (as
/// a regular file). Returns the path to the first one found, or `None`
/// if nothing is found up to the filesystem root.
///
/// Operates on the logical path — `path` itself need not exist. The
/// caller is responsible for checking `<path>/.cosmon` separately; this
/// helper is the "nested galaxy" detector that refuses to create a new
/// galaxy inside an existing one.
///
/// A `.cosmon/` directory **without** `config.toml` is a user-level
/// state host (scheduler state, patrol supervisor, recovery logs) and
/// does not participate in project discovery. The walk continues past
/// it. See [ADR-069](../../../docs/adr/069-cosmon-project-vs-user-root.md).
fn find_nearest_ancestor_cosmon(path: &Path) -> Option<PathBuf> {
    let mut cursor: &Path = path.parent()?;
    loop {
        let candidate = cursor.join(".cosmon");
        if candidate.is_dir() && candidate.join("config.toml").is_file() {
            return Some(candidate);
        }
        {
            let p = cursor.parent()?;
            cursor = p;
        }
    }
}

/// Upgrade an existing `.cosmon/` project by backfilling missing canonical
/// formulas and `project_id`.
///
/// Runs two independent backfill passes — formula backfill, then `project_id`
/// backfill — without overwriting any existing file. Pre-formula-bundling
/// projects (cosmon ≤ April 2026) end up with an empty `.cosmon/formulas/`
/// directory and cannot nucleate anything until canonical templates are
/// restored; this function fixes that trap while preserving every user
/// customization on disk.
#[allow(clippy::too_many_lines)]
fn run_upgrade(
    ctx: &Context,
    root: &std::path::Path,
    cosmon_dir: &std::path::Path,
) -> anyhow::Result<()> {
    if !cosmon_dir.exists() {
        return Err(anyhow::anyhow!(
            "no .cosmon/ directory found — run `cs init` first"
        ));
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
    // path above. Idempotent: if the file exists, assume the schema is there.
    let registry_path = cosmon_dir.join("registry.sqlite");
    let registry_added = !registry_path.exists();
    if registry_added {
        let conn = rusqlite::Connection::open(&registry_path)
            .map_err(|e| anyhow::anyhow!("failed to create registry: {e}"))?;
        conn.execute_batch(neurion_core::schema::SCHEMA_SQL)
            .map_err(|e| anyhow::anyhow!("failed to initialize registry schema: {e}"))?;
        conn.execute_batch(neurion_core::schema::HYPERGRAPH_SQL)
            .map_err(|e| anyhow::anyhow!("failed to initialize hypergraph schema: {e}"))?;
        conn.execute_batch(
            "INSERT OR IGNORE INTO referents (name, description) VALUES
             ('project.status', 'Current state of fleets, workers, and molecules'),
             ('project.issues', 'Tracked issues, blockers, and work items'),
             ('project.decisions', 'Architecture Decision Records');",
        )
        .map_err(|e| anyhow::anyhow!("failed to seed referents: {e}"))?;
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
            generate_config_toml(cosmon_dir, &project_id, None)?;
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
    let gitignore_upgraded = upgrade_gitignore_rules(root, cosmon_dir);

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

    // --- Report -------------------------------------------------------------
    let nothing_changed = added_formulas.is_empty()
        && !project_id_added
        && !registry_added
        && !state_added
        && !gitignore_upgraded
        && !claude_md_updated
        && !gitleaks_added;
    let status = if nothing_changed {
        "already_upgraded"
    } else {
        "upgraded"
    };

    if ctx.json {
        let output = serde_json::json!({
            "status": status,
            "project_id": project_id.as_str(),
            "added_formulas": added_formulas,
            "project_id_added": project_id_added,
            "registry_added": registry_added,
            "state_added": state_added,
            "gitignore_upgraded": gitignore_upgraded,
            "claude_md_updated": claude_md_updated,
            "gitleaks_added": gitleaks_added,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if status == "already_upgraded" {
        println!("Already upgraded: project_id = {project_id}");
    } else {
        println!("Upgraded: project_id = {project_id}");
        for name in &added_formulas {
            println!("  + formulas/{name} (canonical)");
        }
        if project_id_added {
            println!("  + project_id backfilled");
        }
        if registry_added {
            println!(
                "  + registry.sqlite (derived index — gitignored cache, not the source of truth)"
            );
        }
        if state_added {
            println!("  + state/fleets/default/molecules/ + fleet.json");
        }
        if gitignore_upgraded {
            println!("  + .gitignore migrated to selective rules (durable artifacts now tracked)");
        }
        if claude_md_updated {
            println!("  + CLAUDE.md cosmon section generated/updated");
        }
        if gitleaks_added {
            println!("  + .gitleaks.toml (federation scan baseline — unblocks cs done harvests)");
        }
    }

    Ok(())
}

/// Execute the `--soft` variant: generate only CLAUDE.md, no `.cosmon/`.
///
/// Constitutional projection — a minimal convention surface (≤50 lines)
/// that any agent can read independently. No orchestration infrastructure,
/// no runtime state, no external dependency. The generated file contains
/// zero references to cosmon or its vocabulary.
///
/// Idempotent: if CLAUDE.md already exists, warns and does not overwrite.
fn run_soft(ctx: &Context, root: &Path, template: ProjectTemplate) -> anyhow::Result<()> {
    let claude_md_path = root.join("CLAUDE.md");

    if claude_md_path.exists() {
        if ctx.json {
            let output = serde_json::json!({
                "status": "already_exists",
                "path": claude_md_path.display().to_string(),
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            println!("CLAUDE.md already exists: {}", claude_md_path.display());
            println!("  Remove it first to regenerate.");
        }
        return Ok(());
    }

    let name = infer_project_name(root);
    let content = render_soft_template(&name, template);

    // Sanity check: the Leeloo invariant — ≤50 lines.
    debug_assert!(
        content.lines().count() <= SOFT_TEMPLATE_MAX_LINES,
        "soft template exceeds {SOFT_TEMPLATE_MAX_LINES}-line limit: {} lines",
        content.lines().count(),
    );

    fs::write(&claude_md_path, &content)?;

    if ctx.json {
        let output = serde_json::json!({
            "status": "created",
            "path": claude_md_path.display().to_string(),
            "template": format!("{template:?}").to_lowercase(),
            "lines": content.lines().count(),
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Created: {}", claude_md_path.display());
        println!(
            "  template: {}  lines: {}",
            format!("{template:?}").to_lowercase(),
            content.lines().count(),
        );
    }

    Ok(())
}

/// Maximum line count for soft-generated CLAUDE.md (Leeloo invariant).
const SOFT_TEMPLATE_MAX_LINES: usize = 50;

/// Render a soft CLAUDE.md from the given project name and template type.
///
/// The output must satisfy:
/// - ≤50 lines (Leeloo: max entropy per line)
/// - Zero cosmon vocabulary (no `cs`, `molecule`, `nucleate`, `evolve`, etc.)
/// - Sections: project identity, truth pointers, output conventions,
///   invariants, machine consumption notice
fn render_soft_template(name: &str, template: ProjectTemplate) -> String {
    match template {
        ProjectTemplate::Generic => render_generic_template(name),
        ProjectTemplate::Rust => render_rust_template(name),
        ProjectTemplate::Data => render_data_template(name),
    }
}

/// Generic project template — language-agnostic.
fn render_generic_template(name: &str) -> String {
    format!(
        "\
# {name}

## Truth Pointers

| Concern | Source of truth |
|---------|----------------|
| Architecture decisions | `docs/adr/` |
| API contracts | `docs/api/` |
| Dependencies | lock file at repo root |

## Output Conventions

- **Commits**: conventional commits (`feat:`, `fix:`, `refactor:`, `docs:`, `test:`, `chore:`)
- **Branches**: `feat/<slug>`, `fix/<slug>`
- **PRs**: title < 70 chars; body has `## Summary` + `## Test plan`
- **Issues**: one problem per issue, reproducible steps when applicable

## Invariants

- Do not commit secrets, credentials, or API keys
- Do not bypass CI checks or skip pre-commit hooks
- Keep PRs under 400 lines — split larger changes
- Every public interface change needs a test

## Machine Notice

Artifacts produced in this repository are consumed by automated systems.
Exact format compliance matters — test output structure, not just logic.
"
    )
}

/// Rust project template.
fn render_rust_template(name: &str) -> String {
    format!(
        "\
# {name}

## Truth Pointers

| Concern | Source of truth |
|---------|----------------|
| Architecture decisions | `docs/adr/` |
| Public API | `cargo doc` output |
| Dependencies | `Cargo.lock` |
| CI gates | `cargo check`, `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check` |

## Output Conventions

- **Commits**: conventional commits (`feat:`, `fix:`, `refactor:`, `docs:`, `test:`, `chore:`)
- **Branches**: `feat/<slug>`, `fix/<slug>`
- **PRs**: title < 70 chars; body has `## Summary` + `## Test plan`
- No `unwrap()` in library code — return `Result`
- `///` doc comments on every `pub` item (explain *why*, not *what*)
- `#![forbid(unsafe_code)]` unless explicitly opted out per crate

## Invariants

- Do not commit secrets, credentials, or API keys
- Do not bypass CI checks or skip pre-commit hooks
- Keep PRs under 400 lines — split larger changes
- All four gates must pass: `check`, `test`, `clippy`, `fmt`

## Machine Notice

Artifacts produced in this repository are consumed by automated systems.
Exact format compliance matters — test output structure, not just logic.
"
    )
}

/// Data/research project template.
fn render_data_template(name: &str) -> String {
    format!(
        "\
# {name}

## Truth Pointers

| Concern | Source of truth |
|---------|----------------|
| Pipeline DAG | `Makefile` or `Snakefile` at repo root |
| Environment | `requirements.txt` or `pyproject.toml` |
| Data schemas | `schemas/` directory |
| Experiment logs | `experiments/` directory |

## Output Conventions

- **Commits**: conventional commits (`feat:`, `fix:`, `refactor:`, `docs:`, `test:`, `chore:`)
- **Branches**: `feat/<slug>`, `fix/<slug>`
- **PRs**: title < 70 chars; body has `## Summary` + `## Test plan`
- Notebooks: clear all outputs before committing; use `scripts/` for reproducible runs
- Data files > 10 MB belong in storage, not git — track with DVC or git-lfs

## Invariants

- Do not commit secrets, credentials, or API keys
- Do not commit large data files or model weights to git
- Do not bypass CI checks or skip pre-commit hooks
- Every pipeline step must be reproducible from a clean checkout

## Machine Notice

Artifacts produced in this repository are consumed by automated systems.
Exact format compliance matters — test output structure, not just logic.
"
    )
}

/// Generate `.cosmon/config.toml` with the project identity.
///
/// Writes the `[project]` section containing the generated `project_id`,
/// and optionally a `noyau = "<tenant>"` key (ADR-063 layer 3) when the
/// caller supplied one. Does not overwrite an existing `config.toml`.
#[allow(clippy::too_many_lines)]
fn generate_config_toml(
    cosmon_dir: &std::path::Path,
    project_id: &ProjectId,
    tenant: Option<&str>,
) -> anyhow::Result<()> {
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
         \n\
         # ── Worker behavior ───────────────────────────────────────────────\n\
         # What a worker does after completing its molecule.\n\
         # Options: \"commit\" (default), \"commit+push\", \"commit+push+pr\"\n\
         # [worker]\n\
         # on_complete = \"commit\"\n\
         \n\
         # ── Surface auto-reconcile ────────────────────────────────────────\n\
         # Run `cs reconcile` automatically after state-mutating operations.\n\
         # [surfaces]\n\
         # auto_reconcile = false\n\
         \n\
         # ── Documentation ─────────────────────────────────────────────────\n\
         # [documentation]\n\
         # enabled = true\n\
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

/// Sentinel markers for the cosmon-managed section in `CLAUDE.md`.
///
/// `cs init` generates or appends this section; `cs init --upgrade` can
/// update it in-place without overwriting user content above/below.
const COSMON_SECTION_START: &str = "<!-- cosmon:start -->";
const COSMON_SECTION_END: &str = "<!-- cosmon:end -->";

/// Infer a human-readable project name from the directory name.
fn infer_project_name(project_root: &Path) -> String {
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
fn generate_claude_md(project_root: &Path) -> anyhow::Result<bool> {
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

/// Detect a GitHub remote from git config.
///
/// Parses `git remote -v` looking for `github.com` URLs and extracts
/// `owner/repo`. Checks `origin` first, then any remote.
fn detect_github_remote(project_root: &std::path::Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["remote", "-v"])
        .current_dir(project_root)
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse lines like: origin	git@github.com:owner/repo.git (fetch)
    // or: origin	https://github.com/owner/repo.git (fetch)
    for line in stdout.lines() {
        if !line.contains("github.com") {
            continue;
        }

        // Extract owner/repo from SSH URL: git@github.com:owner/repo.git
        if let Some(rest) = line.split("github.com:").nth(1) {
            let repo = rest.split_whitespace().next()?.trim_end_matches(".git");
            return Some(repo.to_string());
        }

        // Extract owner/repo from HTTPS URL: https://github.com/owner/repo.git
        if let Some(rest) = line.split("github.com/").nth(1) {
            let repo = rest.split_whitespace().next()?.trim_end_matches(".git");
            return Some(repo.to_string());
        }
    }

    None
}

/// Generate `.cosmon/surfaces.toml` with default surfaces.
///
/// If a GitHub remote is detected, automatically includes a github-issues
/// surface — the "it just works" experience.
fn generate_surfaces_toml(
    cosmon_dir: &std::path::Path,
    github_repo: Option<&str>,
) -> anyhow::Result<()> {
    let surfaces_path = cosmon_dir.join("surfaces.toml");
    if surfaces_path.exists() {
        return Ok(()); // Don't overwrite existing config.
    }

    let mut toml = String::from(
        "# Surface projections — cosmon internal state → standard files.\n\
         # See THESIS Part XVI (Surface Observability).\n\
         # Run `cs reconcile` to project surfaces.\n\
         \n\
         [[surface]]\n\
         referent = \"project.status\"\n\
         kind = \"markdown\"\n\
         path = \"STATUS.md\"\n\
         \n\
         [[surface]]\n\
         referent = \"project.issues\"\n\
         kind = \"markdown\"\n\
         path = \"ISSUES.md\"\n\
         \n\
         [[surface]]\n\
         referent = \"project.decisions\"\n\
         kind = \"directory\"\n\
         path = \"docs/adr/\"\n",
    );

    if let Some(repo) = github_repo {
        use std::fmt::Write;
        let _ = write!(
            toml,
            "\n\
             # Auto-detected GitHub remote: {repo}\n\
             # Set `public = true` if this repo is public: cosmon then suppresses\n\
             # the internal molecule-id marker from issue bodies and requires an\n\
             # explicit COSMON_SURFACE_PUBLISH=1 gesture before publishing.\n\
             [[surface]]\n\
             referent = \"project.issues\"\n\
             kind = \"github-issues\"\n\
             repo = \"{repo}\"\n\
             public = false\n"
        );
    }

    fs::write(&surfaces_path, toml)?;
    Ok(())
}

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
/// galaxy's repo-root `.gitleaks.toml` by [`run`]. Embedded verbatim from the
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
const COSMON_GITLEAKS_BASELINE: &str =
    include_str!("../../../../assets/gitleaks/cosmon-baseline.gitleaks.toml");

const COSMON_GITIGNORE_CONTENT: &str = "\
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
const LEGACY_SELECTIVE_COSMON_GITIGNORE_CONTENT: &str = "\
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
const LEGACY_COSMON_GITIGNORE_CONTENT: &str = "\
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

/// Walk upward from `start` looking for an ancestor `.git` entry. Returns
/// the first match or `None` if no repository contains the path.
fn find_git_root(start: &std::path::Path) -> Option<PathBuf> {
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
fn ensure_worktrees_in_exclude(project_root: &std::path::Path) -> anyhow::Result<bool> {
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
pub(super) fn strip_cosmon_gitignore_block(body: &str) -> String {
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

/// Upgrade legacy gitignore rules to the consolidated scheme.
///
/// Rewrites:
///   * `.cosmon/.gitignore` if its body matches `LEGACY_COSMON_GITIGNORE_CONTENT`
///     exactly (untouched user-customized files are preserved).
///   * The project `.gitignore` at the git root: removes every legacy
///     Cosmon block — `.cosmon/`, `.cosmon/state/`, `.worktrees/`, orphan
///     `# Cosmon …` comments — and writes `.worktrees/` to
///     `.git/info/exclude` instead. `.worktrees/` is per-clone by design
///     (ephemeral per-molecule working copies), so it does not belong on
///     the shared bulletin board.
///
/// Returns true if either file was updated.
fn upgrade_gitignore_rules(project_root: &std::path::Path, cosmon_dir: &std::path::Path) -> bool {
    let mut changed = false;

    // .cosmon/.gitignore — exact-match replacement only. Two legacy bodies
    // are recognized: the blanket `state/` era and the selective-rules era.
    // User-customized files are left untouched.
    let cosmon_ignore = cosmon_dir.join(".gitignore");
    if let Ok(body) = fs::read_to_string(&cosmon_ignore) {
        let is_legacy = body == LEGACY_COSMON_GITIGNORE_CONTENT
            || body == LEGACY_SELECTIVE_COSMON_GITIGNORE_CONTENT;
        if is_legacy
            && body != COSMON_GITIGNORE_CONTENT
            && fs::write(&cosmon_ignore, COSMON_GITIGNORE_CONTENT).is_ok()
        {
            changed = true;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every builtin formula is non-empty and exposes a `formula = "..."` line
    /// whose value matches the file stem. Guards against accidental path typos
    /// in `include_str!` and against shipping broken or mismatched templates.
    #[test]
    fn builtin_formulas_embedded_and_well_formed() {
        assert_eq!(
            BUILTIN_FORMULAS.len(),
            9,
            "expected 9 canonical formulas: deep-think, deep-think-inline, task-work, idea-to-plan, mission-plan, temp-review, mission-controller, editorial-work, verify-surface",
        );

        for (name, contents) in BUILTIN_FORMULAS {
            assert!(!contents.is_empty(), "{name} is empty");

            let stem = name
                .strip_suffix(".formula.toml")
                .expect("builtin formula filename must end with .formula.toml");
            let expected = format!("formula = \"{stem}\"");
            assert!(
                contents.contains(&expected),
                "{name} does not declare `{expected}` — did the template drift?",
            );

            // Every formula must have at least one step.
            assert!(
                contents.contains("[[steps]]"),
                "{name} has no [[steps]] — unusable as a formula",
            );
        }
    }

    /// `deep-think-inline` is the sanctioned Tier-0 panel a Tier-1 mission
    /// can commission. The whole point of the formula is that it parses
    /// clean as a *leaf* — `validate_tier` must accept it (no nucleation
    /// verb in any step body) and its declared tier must be `Tier::Zero`,
    /// so the ordinal guard `ensure_tier_descends` permits a Tier-1 parent
    /// (mission-controller) to nucleate it. If a future edit reintroduces a
    /// `cs nucleate` into a step `command`, this test fails loudly — that
    /// edit would re-break the very guard the formula exists to satisfy.
    #[test]
    fn deep_think_inline_is_a_valid_tier0_leaf() {
        let (_, contents) = BUILTIN_FORMULAS
            .iter()
            .find(|(name, _)| *name == "deep-think-inline.formula.toml")
            .expect("deep-think-inline must be a builtin formula");

        let formula =
            cosmon_core::formula::Formula::parse(contents).expect("deep-think-inline must parse");

        assert_eq!(
            formula.tier,
            cosmon_core::formula::Tier::Zero,
            "deep-think-inline MUST be Tier 0 — a Tier-1 mission can only \
             nucleate a leaf, never a same-tier panel",
        );

        cosmon_core::formula::validate_tier(&formula)
            .expect("deep-think-inline must satisfy the Tier-0 contract (no nucleation)");
    }

    /// `cs init` must generate a `project_id` and write it to `config.toml`.
    /// The embedded gitleaks baseline must be valid TOML and carry the three
    /// load-bearing pieces of the fix: (a) `useDefault` so the full
    /// high-confidence ruleset stays active, (b) a `generic-api-key`-scoped
    /// allowlist matching the cosmon state-journal path so the entropy false
    /// positive is silenced ONLY there, and (c) the dedicated AWS rule that
    /// replaces the AWS coverage `generic-api-key` would otherwise provide on
    /// journals. If a future edit drops any of these, this test fails loudly —
    /// the edit would silently re-open either the `cs done` block or the
    /// real-secret gap. (task-20260623-e9f0)
    #[test]
    fn gitleaks_baseline_embedded_and_well_formed() {
        assert!(
            !COSMON_GITLEAKS_BASELINE.is_empty(),
            "embedded gitleaks baseline must not be empty"
        );

        // Parses as TOML — a malformed config would make every galaxy's
        // pre-commit gitleaks invocation error out, blocking ALL commits.
        let parsed: toml::Value =
            toml::from_str(COSMON_GITLEAKS_BASELINE).expect("baseline must be valid TOML");

        assert_eq!(
            parsed
                .get("extend")
                .and_then(|e| e.get("useDefault"))
                .and_then(toml::Value::as_bool),
            Some(true),
            "baseline must extend the default gitleaks ruleset",
        );

        // The allowlist that silences the entropy heuristic on journals.
        let allowlists = parsed
            .get("allowlists")
            .and_then(|v| v.as_array())
            .expect("baseline must define [[allowlists]]");
        let journal_allow = allowlists
            .iter()
            .find(|a| {
                a.get("targetRules")
                    .and_then(|t| t.as_array())
                    .is_some_and(|t| t.iter().any(|r| r.as_str() == Some("generic-api-key")))
            })
            .expect("an allowlist must target the generic-api-key rule");
        let paths = journal_allow
            .get("paths")
            .and_then(|p| p.as_array())
            .expect("the generic-api-key allowlist must scope to paths");
        assert!(
            paths
                .iter()
                .any(|p| p.as_str().is_some_and(|s| s.contains("events"))),
            "the allowlist must scope to events.jsonl state journals",
        );

        // The dedicated AWS rule — gitleaks default has no keyword-free AKIA
        // rule, so silencing generic-api-key on journals would drop AWS
        // coverage without this.
        let rules = parsed
            .get("rules")
            .and_then(|v| v.as_array())
            .expect("baseline must define a [[rules]] AWS rule");
        assert!(
            rules
                .iter()
                .any(|r| r.get("id").and_then(|v| v.as_str()) == Some("cosmon-aws-access-key-id")),
            "baseline must carry the dedicated cosmon-aws-access-key-id rule",
        );
    }

    /// A fresh `cs init` scaffolds the federation gitleaks baseline at the repo
    /// root so the galaxy is born immune to the `cs done`-blocking false
    /// positive. (task-20260623-e9f0)
    #[test]
    fn init_scaffolds_gitleaks_baseline() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: false,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("cs init must succeed");

        let gitleaks = tmp.path().join(".gitleaks.toml");
        assert!(
            gitleaks.exists(),
            "fresh init must scaffold repo-root .gitleaks.toml"
        );
        let body = fs::read_to_string(&gitleaks).unwrap();
        assert_eq!(
            body, COSMON_GITLEAKS_BASELINE,
            "scaffolded .gitleaks.toml must be byte-identical to the embedded baseline"
        );
    }

    /// `cs init` must never clobber a galaxy's own `.gitleaks.toml` — the
    /// operator may have extended it. Customization-preserving, like every
    /// other init write. (task-20260623-e9f0)
    #[test]
    fn init_preserves_existing_gitleaks_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let custom = "# my custom config\n[extend]\nuseDefault = true\n";
        fs::write(tmp.path().join(".gitleaks.toml"), custom).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: false,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("cs init must succeed");

        let body = fs::read_to_string(tmp.path().join(".gitleaks.toml")).unwrap();
        assert_eq!(body, custom, "existing .gitleaks.toml must be preserved");
    }

    #[test]
    fn init_writes_config_toml_with_project_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: false,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("cs init must succeed");

        let config_path = tmp.path().join(".cosmon/config.toml");
        assert!(config_path.exists(), "config.toml must be created");

        let content = fs::read_to_string(&config_path).unwrap();
        assert!(
            content.contains("[project]"),
            "config.toml must have [project] section"
        );
        assert!(
            content.contains("project_id"),
            "config.toml must have project_id"
        );

        // Verify the project_id is parseable.
        let config = cosmon_core::config::ProjectConfig::parse(&content).unwrap();
        let pid = config.require_project_id().unwrap();
        assert!(!pid.as_str().is_empty());

        // The template must self-document every configurable section so users
        // discover them without reading source. [hooks] and [gates] in
        // particular were previously invisible in fresh projects.
        assert!(
            content.contains("[hooks]"),
            "config.toml must surface [hooks] section (even commented)"
        );
        assert!(
            content.contains("[gates]"),
            "config.toml must surface [gates] section (even commented)"
        );
        assert!(
            content.contains("post_merge"),
            "config.toml must mention post_merge hook"
        );
        assert!(
            content.contains("build_command"),
            "config.toml must mention build_command gate"
        );
        assert!(
            content.contains("test_command"),
            "config.toml must mention test_command gate"
        );
        // Language hints must be visible so users can adapt to their stack.
        assert!(
            content.contains("Rust:"),
            "config.toml must show Rust hints"
        );
        assert!(
            content.contains("Python:"),
            "config.toml must show Python hints"
        );
        assert!(
            content.contains("Node:"),
            "config.toml must show Node hints"
        );
    }

    /// `cs init --tenant <noyau>` must record the tenant label in
    /// `config.toml` (ADR-063 layer-3 / ADR-080 §8.1) and provision the
    /// `state/nucleons/` directory where ADR-080 OIDC identity mappings
    /// land.
    ///
    /// Single-operator galaxies (no `--tenant`) must NOT emit the
    /// `noyau` field — its absence is the signal that the galaxy is
    /// not multi-tenant.
    #[test]
    fn init_records_tenant_when_flag_supplied() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: false,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: Some("tenant-demo".to_string()),
        };

        run(&ctx, &args).expect("cs init --tenant must succeed");

        // config.toml must carry noyau = "tenant-demo".
        let content = fs::read_to_string(tmp.path().join(".cosmon/config.toml"))
            .expect("config.toml must exist");
        assert!(
            content.contains("noyau = \"tenant-demo\""),
            "config.toml must record noyau = \"tenant-demo\" when --tenant is supplied; got:\n{content}",
        );
        let config =
            cosmon_core::config::ProjectConfig::parse(&content).expect("config.toml must parse");
        assert_eq!(
            config.project.noyau.as_deref(),
            Some("tenant-demo"),
            "ProjectSection.noyau must round-trip the --tenant value",
        );

        // The nucleons directory (ADR-080 §8.1 multi-tenant identity
        // store) must be provisioned so future `oidc-identity.toml`
        // mappings have a home.
        let nucleons = tmp.path().join(".cosmon/state/nucleons");
        assert!(
            nucleons.is_dir(),
            "cs init must provision .cosmon/state/nucleons/ for ADR-080 mappings",
        );
    }

    /// Without `--tenant`, `cs init` must NOT emit a `noyau` field —
    /// the absence is the signal that the galaxy is single-tenant.
    /// This test guards against accidentally emitting a default value.
    #[test]
    fn init_omits_noyau_when_no_tenant_flag() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: false,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("cs init must succeed");

        let content = fs::read_to_string(tmp.path().join(".cosmon/config.toml"))
            .expect("config.toml must exist");
        assert!(
            !content.contains("noyau"),
            "config.toml must NOT mention noyau when --tenant is absent; got:\n{content}",
        );

        // The nucleons directory is provisioned regardless: a
        // single-tenant galaxy may still be promoted to multi-tenant
        // later, and the empty directory is harmless.
        let nucleons = tmp.path().join(".cosmon/state/nucleons");
        assert!(
            nucleons.is_dir(),
            "cs init must provision .cosmon/state/nucleons/ unconditionally",
        );
    }

    /// `cs init` must seed `.cosmon/formulas/` so the first `cs nucleate` on a
    /// fresh project succeeds without the user copying templates by hand.
    #[test]
    fn init_writes_builtin_formulas_into_project() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: false,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("cs init must succeed");

        let formulas_dir = tmp.path().join(".cosmon/formulas");
        for (name, contents) in BUILTIN_FORMULAS {
            let on_disk = fs::read_to_string(formulas_dir.join(name))
                .unwrap_or_else(|e| panic!("{name} not written by cs init: {e}"));
            assert_eq!(on_disk, *contents, "{name} on-disk differs from embedded");
        }
    }

    /// `cs init --upgrade` on a project with no `.cosmon/` should error.
    #[test]
    fn upgrade_fails_without_cosmon_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = Context {
            verbose: false,
            json: false,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: true,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        let result = run(&ctx, &args);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("no .cosmon/"),
            "should mention missing .cosmon/"
        );
    }

    /// `cs init --upgrade` backfills `project_id` into an existing project
    /// that has `.cosmon/` but no config.toml.
    #[test]
    fn upgrade_creates_config_toml_when_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cosmon_dir = tmp.path().join(".cosmon");
        fs::create_dir_all(&cosmon_dir).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: true,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("upgrade must succeed");

        let config_path = cosmon_dir.join("config.toml");
        assert!(config_path.exists(), "config.toml must be created");

        let content = fs::read_to_string(&config_path).unwrap();
        let config = cosmon_core::config::ProjectConfig::parse(&content).unwrap();
        assert!(config.project.project_id.is_some());
    }

    /// `cs init --upgrade` backfills `project_id` into an existing config.toml
    /// that has other sections but no `[project]`.
    #[test]
    fn upgrade_preserves_existing_config_sections() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cosmon_dir = tmp.path().join(".cosmon");
        fs::create_dir_all(&cosmon_dir).unwrap();

        let existing = "[worker]\non_complete = \"commit+push\"\n";
        fs::write(cosmon_dir.join("config.toml"), existing).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: true,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("upgrade must succeed");

        let content = fs::read_to_string(cosmon_dir.join("config.toml")).unwrap();
        let config = cosmon_core::config::ProjectConfig::parse(&content).unwrap();
        assert!(config.project.project_id.is_some());
        assert_eq!(
            config.worker.on_complete,
            cosmon_core::config::OnComplete::CommitPush,
            "existing worker config must be preserved"
        );
    }

    /// `cs init --upgrade` is idempotent — re-running on an already-upgraded
    /// project is a no-op.
    #[test]
    fn upgrade_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cosmon_dir = tmp.path().join(".cosmon");
        fs::create_dir_all(&cosmon_dir).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: true,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("first upgrade must succeed");
        let content1 = fs::read_to_string(cosmon_dir.join("config.toml")).unwrap();

        run(&ctx, &args).expect("second upgrade must succeed");
        let content2 = fs::read_to_string(cosmon_dir.join("config.toml")).unwrap();

        assert_eq!(content1, content2, "idempotent: file must not change");
    }

    /// `cs init --upgrade` handles a config.toml with an empty `[project]`
    /// section (no `project_id`).
    #[test]
    fn upgrade_fills_empty_project_section() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cosmon_dir = tmp.path().join(".cosmon");
        fs::create_dir_all(&cosmon_dir).unwrap();

        let existing = "[project]\n\n[worker]\non_complete = \"commit\"\n";
        fs::write(cosmon_dir.join("config.toml"), existing).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: true,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("upgrade must succeed");

        let content = fs::read_to_string(cosmon_dir.join("config.toml")).unwrap();
        let config = cosmon_core::config::ProjectConfig::parse(&content).unwrap();
        assert!(
            config.project.project_id.is_some(),
            "project_id must be backfilled"
        );
    }

    /// `cs init --upgrade` against a pre-formula-bundling project (config.toml
    /// with `project_id` already set, but empty `formulas/`) must backfill every
    /// canonical formula AND preserve any existing user-custom formula.
    #[test]
    fn upgrade_backfills_missing_canonical_formulas() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cosmon_dir = tmp.path().join(".cosmon");
        let formulas_dir = cosmon_dir.join("formulas");
        fs::create_dir_all(&formulas_dir).unwrap();

        // Pre-existing project_id — the old upgrade path would have
        // short-circuited here and left formulas/ empty.
        fs::write(
            cosmon_dir.join("config.toml"),
            "[project]\nproject_id = \"legacy-beef\"\n",
        )
        .unwrap();

        // User-custom formula that must not be touched.
        let custom_path = formulas_dir.join("my-custom.formula.toml");
        let custom_body = "formula = \"my-custom\"\n[[steps]]\nname = \"hack\"\n";
        fs::write(&custom_path, custom_body).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: true,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("upgrade must succeed");

        // All 4 canonical formulas present on disk.
        for (name, contents) in BUILTIN_FORMULAS {
            let on_disk = fs::read_to_string(formulas_dir.join(name))
                .unwrap_or_else(|e| panic!("{name} not backfilled by upgrade: {e}"));
            assert_eq!(on_disk, *contents, "{name} on-disk differs from embedded");
        }

        // User-custom formula preserved verbatim.
        let custom_after = fs::read_to_string(&custom_path).unwrap();
        assert_eq!(
            custom_after, custom_body,
            "custom formula must not be touched"
        );

        // Legacy project_id preserved.
        let cfg = cosmon_core::config::ProjectConfig::parse(
            &fs::read_to_string(cosmon_dir.join("config.toml")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            cfg.require_project_id().unwrap().as_str(),
            "legacy-beef",
            "existing project_id must be preserved"
        );
    }

    /// `cs init --upgrade` against a fresh clone (config.toml + surfaces.toml
    /// only, no registry, no state/) must backfill registry.sqlite with the
    /// neurion schema AND materialize state/fleets/default/molecules/ +
    /// fleet.json so subsequent `cs nucleate` / `cs reconcile` succeeds
    /// without manual intervention. Covers BUG 8 + BUG 9.
    #[test]
    fn upgrade_backfills_registry_and_state_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cosmon_dir = tmp.path().join(".cosmon");
        fs::create_dir_all(&cosmon_dir).unwrap();

        // Minimal fresh-clone layout: config.toml + surfaces.toml, no state,
        // no registry, empty formulas/.
        fs::write(
            cosmon_dir.join("config.toml"),
            "[project]\nproject_id = \"fresh-clone\"\n",
        )
        .unwrap();
        fs::write(
            cosmon_dir.join("surfaces.toml"),
            "[[surface]]\nreferent = \"project.status\"\nkind = \"markdown\"\npath = \"STATUS.md\"\n",
        )
        .unwrap();

        assert!(!cosmon_dir.join("registry.sqlite").exists());
        assert!(!cosmon_dir.join("state").exists());

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: true,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("upgrade must succeed");

        // BUG 8: registry.sqlite exists with the expected schema.
        let registry_path = cosmon_dir.join("registry.sqlite");
        assert!(registry_path.exists(), "registry.sqlite must be created");
        let conn = rusqlite::Connection::open(&registry_path).unwrap();
        let referent_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM referents", [], |row| row.get(0))
            .expect("referents table must exist and be queryable");
        assert!(
            referent_count >= 3,
            "expected default referents to be seeded, got {referent_count}"
        );

        // BUG 9: state dirs + fleet.json exist.
        assert!(
            cosmon_dir.join("state/fleets/default/molecules").is_dir(),
            "state/fleets/default/molecules/ must be created"
        );
        let fleet_json = cosmon_dir.join("state/fleet.json");
        assert!(fleet_json.exists(), "fleet.json must be created");
        let body = fs::read_to_string(&fleet_json).unwrap();
        assert!(
            body.contains("\"workers\""),
            "fleet.json must have empty workers payload"
        );

        // Idempotent second run: no change, status = already_upgraded.
        let mtime_before = fs::metadata(&registry_path).unwrap().modified().unwrap();
        run(&ctx, &args).expect("second upgrade must succeed");
        let mtime_after = fs::metadata(&registry_path).unwrap().modified().unwrap();
        assert_eq!(
            mtime_before, mtime_after,
            "registry.sqlite must not be touched on idempotent re-run"
        );
    }

    /// Fresh `cs init` must write the archive-aware `.cosmon/.gitignore`:
    /// `state/` is ignored in bulk, with `!state/archive/` negated so the
    /// durable archive subtree (synthesis.md, outcomes.md, manifests) lands
    /// in git history once the archive subsystem starts writing.
    #[test]
    fn init_writes_archive_aware_cosmon_gitignore() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: false,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("cs init must succeed");

        let body = fs::read_to_string(tmp.path().join(".cosmon/.gitignore")).unwrap();

        // Runtime state is ignored in bulk.
        assert!(
            body.lines().any(|l| l.trim() == "state/"),
            "gitignore must blanket-ignore state/"
        );

        // The archive subtree is re-included via negation.
        assert!(
            body.lines().any(|l| l.trim() == "!state/archive/"),
            "gitignore must negate state/archive/ so it is tracked"
        );
        assert!(
            body.lines().any(|l| l.trim() == "!state/archive/**"),
            "gitignore must negate state/archive/** so archive contents are tracked"
        );

        // Runtime binary noise remains ignored.
        assert!(body.contains("registry.sqlite"));
        assert!(body.contains("*.lock"));
    }

    /// `cs init --upgrade` must migrate a legacy `.cosmon/.gitignore`
    /// (pre-2026-04-12) to the selective rules, so existing projects stop
    /// swallowing their deliberation artifacts.
    #[test]
    fn upgrade_migrates_legacy_cosmon_gitignore() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cosmon_dir = tmp.path().join(".cosmon");
        fs::create_dir_all(&cosmon_dir).unwrap();
        fs::write(
            cosmon_dir.join(".gitignore"),
            LEGACY_COSMON_GITIGNORE_CONTENT,
        )
        .unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: true,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("upgrade must succeed");

        let body = fs::read_to_string(cosmon_dir.join(".gitignore")).unwrap();
        assert_eq!(
            body, COSMON_GITIGNORE_CONTENT,
            "legacy gitignore must be replaced verbatim"
        );
    }

    /// `cs init --upgrade` must also migrate the selective-rules legacy
    /// body (the pre-ARCHIVE M1 scheme) to the new archive-aware content.
    #[test]
    fn upgrade_migrates_selective_legacy_cosmon_gitignore() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cosmon_dir = tmp.path().join(".cosmon");
        fs::create_dir_all(&cosmon_dir).unwrap();
        fs::write(
            cosmon_dir.join(".gitignore"),
            LEGACY_SELECTIVE_COSMON_GITIGNORE_CONTENT,
        )
        .unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: true,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("upgrade must succeed");

        let body = fs::read_to_string(cosmon_dir.join(".gitignore")).unwrap();
        assert_eq!(
            body, COSMON_GITIGNORE_CONTENT,
            "selective legacy gitignore must be replaced with archive-aware content"
        );
    }

    /// `cs init --upgrade` must NOT touch a user-customized
    /// `.cosmon/.gitignore` — only exact legacy matches get migrated.
    #[test]
    fn upgrade_preserves_user_customized_gitignore() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cosmon_dir = tmp.path().join(".cosmon");
        fs::create_dir_all(&cosmon_dir).unwrap();
        let custom = "# my own rules\nstate/\nfoo.bar\n";
        fs::write(cosmon_dir.join(".gitignore"), custom).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: true,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("upgrade must succeed");

        let body = fs::read_to_string(cosmon_dir.join(".gitignore")).unwrap();
        assert_eq!(body, custom, "custom gitignore must not be touched");
    }

    /// `cs init --upgrade` against a project missing `.cosmon/.gitignore`
    /// altogether must create the selective one.
    #[test]
    fn upgrade_creates_missing_cosmon_gitignore() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cosmon_dir = tmp.path().join(".cosmon");
        fs::create_dir_all(&cosmon_dir).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: true,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("upgrade must succeed");

        let body = fs::read_to_string(cosmon_dir.join(".gitignore")).unwrap();
        assert_eq!(body, COSMON_GITIGNORE_CONTENT);
    }

    /// `cs init --upgrade` against a project where every canonical formula
    /// already exists AND `project_id` is set must be a no-op — files unchanged,
    /// status = `already_upgraded`.
    #[test]
    fn upgrade_is_noop_when_everything_present() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cosmon_dir = tmp.path().join(".cosmon");
        let formulas_dir = cosmon_dir.join("formulas");
        fs::create_dir_all(&formulas_dir).unwrap();

        fs::write(
            cosmon_dir.join("config.toml"),
            "[project]\nproject_id = \"stable-f00d\"\n",
        )
        .unwrap();

        // Pre-populate formulas/ with all canonical templates, but with a
        // modified body to prove the upgrade does not overwrite them.
        for (name, _) in BUILTIN_FORMULAS {
            fs::write(
                formulas_dir.join(name),
                "# user-modified canonical formula\n",
            )
            .unwrap();
        }

        let mtimes_before: Vec<_> = BUILTIN_FORMULAS
            .iter()
            .map(|(name, _)| {
                fs::metadata(formulas_dir.join(name))
                    .unwrap()
                    .modified()
                    .unwrap()
            })
            .collect();

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: true,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("upgrade must succeed");

        // User-modified canonical templates preserved byte-for-byte.
        for (i, (name, _)) in BUILTIN_FORMULAS.iter().enumerate() {
            let body = fs::read_to_string(formulas_dir.join(name)).unwrap();
            assert_eq!(
                body, "# user-modified canonical formula\n",
                "{name} was overwritten"
            );
            let mtime_after = fs::metadata(formulas_dir.join(name))
                .unwrap()
                .modified()
                .unwrap();
            assert_eq!(mtime_after, mtimes_before[i], "{name} mtime changed");
        }
    }

    /// `cs init` never runs `git init` — git's lifecycle is the user's. On
    /// a fresh empty directory (no git repo), `cs init` creates `.cosmon/`
    /// but leaves `.git/` alone. Users bootstrap git with `git init`
    /// themselves; the `--no-git` flag is a retained no-op.
    #[test]
    fn init_never_runs_git_init() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(
            !tmp.path().join(".git").exists(),
            "precondition: no .git/ yet"
        );

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: false,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: false,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("cs init must succeed");

        assert!(
            !tmp.path().join(".git").exists(),
            "cs init must NOT create .git/ — that is git's job"
        );
        assert!(
            tmp.path().join(".cosmon").exists(),
            "cs init must create .cosmon/"
        );
    }

    /// `cs init` never writes `CLAUDE.md` on a non-`--soft`, non-`--upgrade`
    /// run — that is the `galaxy-onboarding` formula's job. The whole
    /// `cs init` mission is the minimal bootstrap primitive: create the
    /// directory, populate `.cosmon/`, stop.
    #[test]
    fn init_never_writes_claude_md() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: false,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("cs init must succeed");

        assert!(
            !tmp.path().join("CLAUDE.md").exists(),
            "cs init must NOT create CLAUDE.md — that is a formula's job"
        );
        assert!(tmp.path().join(".cosmon").exists());
    }

    /// `cs init <non-existent-path>` creates the directory (and its
    /// parents) and then populates `.cosmon/` inside it. This is the
    /// bootstrap primitive the panel converged on: a formula cannot
    /// `mkdir -p` its own galaxy root, so `cs init` must.
    #[test]
    fn init_creates_non_existent_target_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("brand-new").join("galaxy");
        assert!(!target.exists(), "precondition: target does not exist");

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: target.clone(),
            upgrade: false,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("cs init on non-existent path must succeed");

        assert!(target.is_dir(), "target directory must be created");
        assert!(target.join(".cosmon").is_dir(), ".cosmon/ must exist");
        assert!(
            target.join(".cosmon/config.toml").is_file(),
            "config.toml must be written"
        );
        assert!(
            target.join(".cosmon/formulas").is_dir(),
            "formulas/ must exist"
        );
        assert!(target.join(".cosmon/state").is_dir(), "state/ must exist");
    }

    /// Running `cs init` twice on the same path is strictly idempotent:
    /// the second invocation exits 0 without error, and the on-disk
    /// content is unchanged (no clobber, no mtime churn on
    /// already-present files).
    #[test]
    fn init_is_idempotent_on_existing_galaxy() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("galaxy");

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let make_args = || Args {
            path: target.clone(),
            upgrade: false,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        // First init — creates everything from scratch.
        run(&ctx, &make_args()).expect("first cs init must succeed");
        let config_before = fs::read_to_string(target.join(".cosmon/config.toml"))
            .expect("config.toml must exist after first init");
        let mtime_before = fs::metadata(target.join(".cosmon/config.toml"))
            .unwrap()
            .modified()
            .unwrap();

        // Second init on same path — no-op, no error, no clobber.
        run(&ctx, &make_args()).expect("second cs init must succeed (idempotent)");

        let config_after = fs::read_to_string(target.join(".cosmon/config.toml"))
            .expect("config.toml still present");
        let mtime_after = fs::metadata(target.join(".cosmon/config.toml"))
            .unwrap()
            .modified()
            .unwrap();

        assert_eq!(
            config_before, config_after,
            "config.toml content must not change on idempotent re-init"
        );
        assert_eq!(
            mtime_before, mtime_after,
            "config.toml mtime must not change — no rewrite happened"
        );
    }

    /// `cs init` still refuses to create a galaxy inside an existing
    /// **real** cosmon project — one whose `.cosmon/` carries a
    /// `config.toml`. Walk-up detection finds that ancestor and exits
    /// non-zero with a clear error message. There is no `--force`
    /// escape — nested galaxies silently break walk-up discovery.
    ///
    /// See [ADR-069](../../../docs/adr/069-cosmon-project-vs-user-root.md):
    /// the predicate that defines a "cosmon project root" is
    /// `.cosmon/` ∧ `.cosmon/config.toml`. This test preserves the
    /// original intent of the pre-ADR `init_refuses_nested_galaxy`
    /// test (which only seeded `.cosmon/`) by seeding the full
    /// project-root fixture.
    #[test]
    fn init_still_refuses_nested_real_project() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let outer = tmp.path().join("outer");
        let outer_cosmon = outer.join(".cosmon");
        fs::create_dir_all(&outer_cosmon).expect("seed outer galaxy");
        // Seed the `config.toml` marker that identifies a real cosmon
        // project root per ADR-069.
        fs::write(outer_cosmon.join("config.toml"), "# seeded by test\n")
            .expect("seed outer config.toml");

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };

        // Target is a non-existent path INSIDE the outer galaxy — the
        // walk-up check must fire on the logical path even when the
        // target directory does not yet exist.
        let nested = outer.join("sub").join("nested");
        let args = Args {
            path: nested.clone(),
            upgrade: false,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        let err = run(&ctx, &args).expect_err("must refuse nested galaxy");
        let msg = err.to_string();
        assert!(msg.contains("nest"), "error must mention nesting: {msg}");
        assert!(
            msg.contains(".cosmon"),
            "error must reference the ancestor .cosmon/: {msg}"
        );
        assert!(
            !nested.exists(),
            "nested target must NOT be created on refusal"
        );
    }

    /// `cs init` accepts a target under a config-less ancestor `.cosmon/`.
    ///
    /// Regression fixture for ADR-069: a user-level `.cosmon/` (scheduler
    /// state, patrol supervisor, recovery logs) that was never created by
    /// `cs init` — and therefore carries no `config.toml` — must not be
    /// treated as a cosmon project root. Walk-up continues past it, and
    /// `cs init` succeeds on any subdirectory.
    ///
    /// Before the ADR landed, this exact shape — a legitimate
    /// `~/.cosmon/` cohabiting with `~/galaxies/<new>/` — silently
    /// locked the operator out of the galaxy-birthing primitive.
    #[test]
    fn init_allows_child_of_configless_ancestor_cosmon() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Simulate a user-level host: `.cosmon/` directory without
        // `config.toml`. This stands in for `~/.cosmon/` on a machine
        // running the daemon supervisor or a scheduler.
        let user_host = tmp.path().join(".cosmon");
        fs::create_dir_all(&user_host).expect("seed user-level .cosmon/");
        // Intentionally NO config.toml — this is the crux of the test.
        assert!(
            !user_host.join("config.toml").exists(),
            "precondition: user-level host must lack config.toml"
        );

        // Target a fresh galaxy under the same parent.
        let target = tmp.path().join("galaxies").join("addl");
        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: target.clone(),
            upgrade: false,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("cs init must succeed under a config-less ancestor");

        assert!(
            target.join(".cosmon").is_dir(),
            "target must carry its own .cosmon/"
        );
        assert!(
            target.join(".cosmon/config.toml").is_file(),
            "target must carry its own .cosmon/config.toml"
        );
    }

    /// `cs init --upgrade` updates the cosmon section in-place when markers
    /// exist, preserving content before and after.
    #[test]
    fn upgrade_updates_cosmon_section_in_place() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cosmon_dir = tmp.path().join(".cosmon");
        fs::create_dir_all(&cosmon_dir).unwrap();

        let before = "# My Project\n\nBefore.\n\n";
        let old_section = format!(
            "{COSMON_SECTION_START}\n## Cosmon — Agent Orchestration\n\nOLD CONTENT\n{COSMON_SECTION_END}\n"
        );
        let after = "\n## Other Section\n\nAfter.\n";
        fs::write(
            tmp.path().join("CLAUDE.md"),
            format!("{before}{old_section}{after}"),
        )
        .unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: true,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("upgrade must succeed");

        let body = fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();
        // User content preserved.
        assert!(body.starts_with("# My Project"));
        assert!(body.contains("Before."));
        assert!(body.contains("## Other Section"));
        assert!(body.contains("After."));
        // Old content replaced.
        assert!(!body.contains("OLD CONTENT"));
        // New cosmon section present.
        assert!(body.contains("cs help"));
    }

    /// `cs init --upgrade` on a project without CLAUDE.md creates one.
    #[test]
    fn upgrade_backfills_claude_md_when_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cosmon_dir = tmp.path().join(".cosmon");
        fs::create_dir_all(&cosmon_dir).unwrap();

        assert!(!tmp.path().join("CLAUDE.md").exists());

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: true,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("upgrade must succeed");

        let claude_md = tmp.path().join("CLAUDE.md");
        assert!(claude_md.exists(), "CLAUDE.md must be created by upgrade");

        let body = fs::read_to_string(&claude_md).unwrap();
        assert!(body.contains(COSMON_SECTION_START));
        assert!(body.contains("## Cosmon"));
    }

    /// CLAUDE.md generation is idempotent — running twice produces the same file.
    #[test]
    fn claude_md_generation_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let changed1 = generate_claude_md(tmp.path()).expect("first call");
        assert!(changed1, "first call must create the file");
        let body1 = fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();

        let changed2 = generate_claude_md(tmp.path()).expect("second call");
        assert!(!changed2, "second call must be a no-op");
        let body2 = fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();

        assert_eq!(body1, body2);
    }

    // ── Soft init tests ──────────────────────────────────────────────

    /// `cs init --soft` generates CLAUDE.md without creating `.cosmon/`.
    #[test]
    fn soft_init_creates_claude_md_without_cosmon_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: false,
            soft: true,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("cs init --soft must succeed");

        assert!(
            tmp.path().join("CLAUDE.md").exists(),
            "CLAUDE.md must be created"
        );
        assert!(
            !tmp.path().join(".cosmon").exists(),
            ".cosmon/ must NOT be created by --soft"
        );
    }

    /// Soft-generated CLAUDE.md must be ≤50 lines (Leeloo invariant).
    #[test]
    fn soft_template_respects_line_limit() {
        for template in [
            ProjectTemplate::Generic,
            ProjectTemplate::Rust,
            ProjectTemplate::Data,
        ] {
            let content = render_soft_template("test-project", template);
            let line_count = content.lines().count();
            assert!(
                line_count <= SOFT_TEMPLATE_MAX_LINES,
                "{template:?} template has {line_count} lines, max is {SOFT_TEMPLATE_MAX_LINES}",
            );
        }
    }

    /// Soft-generated CLAUDE.md must contain zero cosmon vocabulary.
    #[test]
    fn soft_template_has_no_cosmon_vocabulary() {
        let forbidden = [
            "cosmon", "molecule", "nucleate", "evolve", "collapse", "entangle", "ensemble", "thaw",
            "freeze", " cs ",
        ];

        for template in [
            ProjectTemplate::Generic,
            ProjectTemplate::Rust,
            ProjectTemplate::Data,
        ] {
            let content = render_soft_template("test-project", template);
            let lower = content.to_lowercase();
            for word in &forbidden {
                assert!(
                    !lower.contains(word),
                    "{template:?} template contains forbidden word: {word:?}",
                );
            }
        }
    }

    /// Soft CLAUDE.md must have the required sections.
    #[test]
    fn soft_template_has_required_sections() {
        for template in [
            ProjectTemplate::Generic,
            ProjectTemplate::Rust,
            ProjectTemplate::Data,
        ] {
            let content = render_soft_template("my-project", template);
            assert!(
                content.contains("# my-project"),
                "{template:?} must have project identity header",
            );
            assert!(
                content.contains("## Truth Pointers"),
                "{template:?} must have truth pointers section",
            );
            assert!(
                content.contains("## Output Conventions"),
                "{template:?} must have output conventions section",
            );
            assert!(
                content.contains("## Invariants"),
                "{template:?} must have invariants section",
            );
            assert!(
                content.contains("## Machine Notice"),
                "{template:?} must have machine notice section",
            );
        }
    }

    /// `cs init --soft` is idempotent — does not overwrite existing CLAUDE.md.
    #[test]
    fn soft_init_does_not_overwrite_existing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let existing = "# My existing CLAUDE.md\n";
        fs::write(tmp.path().join("CLAUDE.md"), existing).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: false,
            soft: true,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };

        run(&ctx, &args).expect("cs init --soft must succeed (no-op)");

        let body = fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();
        assert_eq!(body, existing, "existing CLAUDE.md must not be overwritten");
    }

    /// Rust template has Rust-specific content.
    #[test]
    fn rust_template_has_cargo_conventions() {
        let content = render_soft_template("my-crate", ProjectTemplate::Rust);
        assert!(content.contains("cargo"));
        assert!(content.contains("Cargo.lock"));
        assert!(content.contains("clippy"));
    }

    /// Data template has data/research-specific content.
    #[test]
    fn data_template_has_pipeline_conventions() {
        let content = render_soft_template("my-pipeline", ProjectTemplate::Data);
        assert!(content.contains("Notebook"));
        assert!(content.contains("pipeline"));
    }

    /// Minimal helper: seed a git repo with `README.md` committed. Returns
    /// the canonicalised repo root.
    fn seed_git_repo(root: &Path) -> PathBuf {
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .current_dir(root)
                .args(args)
                .status()
                .expect("git invocation failed in test");
            assert!(status.success(), "git {args:?} failed");
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "test@cosmon.local"]);
        run(&["config", "user.name", "Cosmon Test"]);
        run(&["config", "commit.gpgsign", "false"]);
        fs::write(root.join("README.md"), "# galaxy\n").unwrap();
        run(&["add", "README.md"]);
        run(&["commit", "-q", "-m", "initial"]);
        root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
    }

    /// Fresh `cs init` must write `.worktrees/` to `.git/info/exclude`
    /// — the per-clone notebook — and never to the shared `.gitignore`.
    /// `.worktrees/` is always local by design (ephemeral working copies),
    /// so its place is the personal cahier, not the bulletin board.
    #[test]
    fn fresh_init_writes_worktrees_to_exclude_not_gitignore() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = seed_git_repo(tmp.path());

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: repo_root.clone(),
            upgrade: false,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };
        run(&ctx, &args).expect("cs init must succeed");

        let exclude = fs::read_to_string(repo_root.join(".git/info/exclude"))
            .expect(".git/info/exclude must be readable");
        assert!(
            exclude.lines().any(|l| l.trim() == ".worktrees/"),
            ".git/info/exclude must contain .worktrees/, got: {exclude:?}"
        );

        // The shared `.gitignore` must stay clean — `.worktrees/` is not a
        // shared concern.
        let gitignore_path = repo_root.join(".gitignore");
        if gitignore_path.exists() {
            let body = fs::read_to_string(&gitignore_path).unwrap();
            assert!(
                !body.contains(".worktrees/"),
                ".gitignore must NOT contain .worktrees/, got: {body:?}"
            );
        }
    }

    /// Fresh `cs init` with no surrounding git repo must not fail and
    /// must not try to create `.git/info/exclude` — the rule is only
    /// material when a git repository exists.
    #[test]
    fn fresh_init_no_git_leaves_exclude_alone() {
        let tmp = tempfile::tempdir().unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: tmp.path().to_path_buf(),
            upgrade: false,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };
        run(&ctx, &args).expect("cs init must succeed without git");

        assert!(
            !tmp.path().join(".git").exists(),
            "cs init must NOT create a .git/ directory"
        );
    }

    /// Running `cs init` a second time must not duplicate the `.worktrees/`
    /// exclude rule — idempotent by construction.
    #[test]
    fn fresh_init_exclude_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = seed_git_repo(tmp.path());

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: repo_root.clone(),
            upgrade: false,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };
        run(&ctx, &args).unwrap();
        // Second run is a no-op on `.cosmon/` (already initialised); we
        // call `ensure_worktrees_in_exclude` directly to prove the
        // worktree rule itself is idempotent.
        let changed = ensure_worktrees_in_exclude(&repo_root).unwrap();
        assert!(!changed, "second call must be a no-op");

        let exclude = fs::read_to_string(repo_root.join(".git/info/exclude")).unwrap();
        let count = exclude
            .lines()
            .filter(|l| l.trim() == ".worktrees/")
            .count();
        assert_eq!(count, 1, "rule must not be duplicated, got: {exclude:?}");
    }

    /// `cs init --upgrade` must move an existing `.worktrees/` entry from
    /// the shared `.gitignore` to `.git/info/exclude`, preserving any
    /// user rules above/below. This inverts the pre-migration logic that
    /// wrote `.worktrees/` into the tracked `.gitignore`.
    #[test]
    fn upgrade_moves_worktrees_from_gitignore_to_exclude() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = seed_git_repo(tmp.path());
        fs::create_dir_all(repo_root.join(".cosmon")).unwrap();
        // Seed a `.gitignore` that mirrors the Atlas observation — the
        // legacy block is present alongside a user rule.
        let legacy = "target/\n\n# Cosmon worktrees — ephemeral per-molecule working copies\n.worktrees/\nnode_modules/\n";
        fs::write(repo_root.join(".gitignore"), legacy).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = Args {
            path: repo_root.clone(),
            upgrade: true,
            soft: false,
            template: ProjectTemplate::Generic,
            no_git: true,
            yes: false,
            tenant: None,
        };
        run(&ctx, &args).expect("cs init --upgrade must succeed");

        let body = fs::read_to_string(repo_root.join(".gitignore")).unwrap();
        assert!(
            !body.contains(".worktrees/"),
            ".gitignore must no longer contain .worktrees/: {body:?}"
        );
        assert!(
            !body.contains("# Cosmon worktrees"),
            "orphan Cosmon header comment must be removed: {body:?}"
        );
        assert!(body.contains("target/"), "user rule must survive");
        assert!(body.contains("node_modules/"), "user rule must survive");

        let exclude = fs::read_to_string(repo_root.join(".git/info/exclude")).unwrap();
        assert!(
            exclude.lines().any(|l| l.trim() == ".worktrees/"),
            ".git/info/exclude must gain .worktrees/, got: {exclude:?}"
        );
    }

    /// Regression for the `Atlas` observation: a `.gitignore` carrying the
    /// legacy "agent-orchestration state" block plus `.cosmon/` /
    /// `.worktrees/` must be entirely emptied of Cosmon rules by the
    /// cleanup sweep.
    #[test]
    fn strip_cosmon_gitignore_block_removes_legacy_atlas_layout() {
        let body = "\
target/

# Cosmon agent-orchestration state (local-only, not pushed to shared repo)
.cosmon/
.worktrees/

node_modules/
";
        let out = strip_cosmon_gitignore_block(body);
        assert!(
            !out.contains(".cosmon/"),
            ".cosmon/ must be removed: {out:?}"
        );
        assert!(
            !out.contains(".worktrees/"),
            ".worktrees/ must be removed: {out:?}"
        );
        assert!(
            !out.contains("# Cosmon"),
            "orphan Cosmon comment must be removed: {out:?}"
        );
        assert!(out.contains("target/"), "user rules must survive");
        assert!(out.contains("node_modules/"), "user rules must survive");
    }

    /// `strip_cosmon_gitignore_block` on a body with zero Cosmon content
    /// returns the input verbatim — no spurious edits on user
    /// `.gitignore`s that never touched Cosmon.
    #[test]
    fn strip_cosmon_gitignore_block_is_noop_without_cosmon_rules() {
        let body = "target/\nnode_modules/\n# My own comment\n*.log\n";
        let out = strip_cosmon_gitignore_block(body);
        assert_eq!(out, body);
    }
}
