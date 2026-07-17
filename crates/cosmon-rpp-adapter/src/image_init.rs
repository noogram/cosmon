// SPDX-License-Identifier: AGPL-3.0-only

//! `cs-server-image-init-discipline` — boot-time state-tree materialization.
//!
//! This module absorbs the former `cosmon-server-init.sh` ENTRYPOINT
//! (T24) into the audited
//! Rust binary. The shell script ran *before* the adapter, with the most
//! privileged boot context, and could only ever dress **one** table: it
//! materialised a single `$COSMON_GALAXY`. This module materialises
//! **one galaxy per `noyau`** so an instance can host more than one
//! nucléon.
//!
//! ## Discipline (already-decided constraints — do not drift)
//!
//! - **Shell-out, not re-implementation.** `cs init --upgrade` and `git
//!   init` stay the authority; this module only *orchestrates* them,
//!   per-noyau, at boot. No `rusqlite` / `neurion_core` are pulled into
//!   the adapter — exactly the dependency creep the shell-out avoids.
//! - **multi-nucléon = multi-`noyau` materialization.** The binding
//!   layer is already plural ([`crate::HabilitationMap`]); what was missing is
//!   the per-`noyau` materialization. The loop iterates the noyaux of the
//!   map, not a single galaxy.
//! - **node leaves the *setup* path.** The two JSON merges the script did
//!   via inline `node -e` (Claude Code first-run gates) are now
//!   `serde_json` in-process (sens (i)). node stays in the image only
//!   because `claude` itself is an npm package — that is a different,
//!   out-of-scope concern (sens (ii)).
//! - **Write before spawn.** The Claude Code config gates are written at
//!   *boot*, before any worker is spawned — the same ordering the script
//!   relied on. Claude Code rewrites `.claude.json` at startup, but the
//!   gate is *read before* the rewrite, so a boot-time write wins. A
//!   Dockerfile-baked write would lose to the rewrite; this does not.
//! - **Idempotent + best-effort.** Safe to re-run on every container
//!   restart (B2 eager). `mkdir` is `create_dir_all`, `cs init --upgrade`
//!   is a no-op once `config.toml` exists, `git init` is guarded on
//!   `.git`, the JSON merges are guarded on the gated field. A per-step
//!   failure is logged and reported, never fatal: the adapter must still
//!   boot and serve even if one noyau's init fails (non-regression with
//!   the script's `log_warn`-and-continue behaviour).

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::nucleon_map::Noyau;

/// Default docker-secret path (`secrets:` mount in `docker-compose`).
pub const ANTHROPIC_SECRET_DOCKER: &str = "/run/secrets/anthropic-api-key";
/// Default operator-managed secret file.
pub const ANTHROPIC_SECRET_OPERATOR: &str = "/var/lib/cosmon/secrets/anthropic-api-key";
/// Default image-baked formula seed directory (Dockerfile snapshot).
pub const FORMULAS_SEED_DIR: &str = "/opt/cosmon-formulas";

/// Default image-baked nucleon-binding seed directory (Dockerfile
/// snapshot). The binding-layer twin of [`FORMULAS_SEED_DIR`]: the
/// `nucleons/` tree lives *inside* the state volume, so anything the
/// Dockerfile bakes into `<state_dir>/nucleons/` is shadowed by the named
/// volume on a fresh instance. The seed lives outside the mount (`/opt`)
/// and is copied in at boot when no binding exists yet — bootstrapping
/// the very map that [`ImageInit::run`] then iterates over. See
/// [`seed_nucleons`].
pub const NUCLEONS_SEED_DIR: &str = "/opt/cosmon-nucleons";

/// Claude Code onboarding version recorded on pre-seed. Mirrors the
/// value the V1 script wrote; it only needs to be a non-empty string so
/// the first-run wizard treats onboarding as completed.
const CLAUDE_ONBOARDING_VERSION: &str = "2.1.140";

/// Outcome of a single materialization step, for boot logging and for
/// idempotence assertions in tests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StepOutcome {
    /// The artifact was created / written this run.
    Done,
    /// Already present — no action taken (the idempotent re-run path).
    AlreadyPresent,
    /// Deliberately not attempted (e.g. no formula seed dir configured).
    Skipped,
    /// Attempted and failed; carries a short reason for the log. Never
    /// fatal — the boot continues.
    Failed(String),
}

impl StepOutcome {
    /// `true` when the step did not fail (created, idempotent, or
    /// skipped). Used by tests to assert a clean re-run.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        !matches!(self, StepOutcome::Failed(_))
    }
}

/// Per-`noyau` slice of the materialization report.
#[derive(Clone, Debug)]
pub struct NoyauReport {
    /// The `noyau` (tenant axis) this slice describes.
    pub noyau: String,
    /// Step 2 — `.cosmon/state/{events,molecules,fleets/default}`.
    pub state_dirs: StepOutcome,
    /// Step 2a — `cs init --upgrade` (shell-out).
    pub cs_init: StepOutcome,
    /// Step 2b — `git init` + initial commit (shell-out).
    pub git_init: StepOutcome,
    /// Step 3 — formula backfill from the image seed (belt-and-braces).
    pub formulas: StepOutcome,
}

/// Aggregate report of one [`ImageInit::run`] pass.
#[derive(Clone, Debug)]
pub struct ImageInitReport {
    /// Step 1 — `whispers/inbox` (instance-level).
    pub inbox: StepOutcome,
    /// One slice per materialised `noyau`.
    pub noyaux: Vec<NoyauReport>,
    /// Step 3a — `.claude.json` `hasCompletedOnboarding` gate.
    pub claude_onboarding: StepOutcome,
    /// Step 3b — `settings.json` `skipDangerousModePermissionPrompt` gate.
    pub claude_skip_dangerous: StepOutcome,
}

impl ImageInitReport {
    /// `true` when every step across every noyau succeeded (created,
    /// idempotent, or skipped — never `Failed`).
    #[must_use]
    pub fn all_ok(&self) -> bool {
        self.inbox.is_ok()
            && self.claude_onboarding.is_ok()
            && self.claude_skip_dangerous.is_ok()
            && self.noyaux.iter().all(|n| {
                n.state_dirs.is_ok()
                    && n.cs_init.is_ok()
                    && n.git_init.is_ok()
                    && n.formulas.is_ok()
            })
    }

    /// Emit a `tracing` line per step — the structured equivalent of the
    /// script's `[init] INFO ...` log surface.
    pub fn log(&self) {
        log_step("init.whispers_inbox", &self.inbox);
        for n in &self.noyaux {
            log_step(&format!("init.{}.state", n.noyau), &n.state_dirs);
            log_step(&format!("init.{}.cs_init", n.noyau), &n.cs_init);
            log_step(&format!("init.{}.git_init", n.noyau), &n.git_init);
            log_step(&format!("init.{}.formulas", n.noyau), &n.formulas);
        }
        log_step("init.claude_onboarding", &self.claude_onboarding);
        log_step("init.claude_skip_dangerous", &self.claude_skip_dangerous);
    }
}

fn log_step(event: &str, outcome: &StepOutcome) {
    match outcome {
        StepOutcome::Failed(reason) => {
            tracing::warn!(event = "image_init.step", step = event, outcome = "failed", reason = %reason);
        }
        other => {
            let label = match other {
                StepOutcome::Done => "done",
                StepOutcome::AlreadyPresent => "already_present",
                StepOutcome::Skipped => "skipped",
                StepOutcome::Failed(_) => unreachable!(),
            };
            tracing::info!(event = "image_init.step", step = event, outcome = label);
        }
    }
}

/// Boot-time state-tree materializer. Holds the resolved roots and the
/// `cs` binary path; [`Self::run`] does the work for a list of noyaux.
#[derive(Clone, Debug)]
pub struct ImageInit {
    /// Whispers ingestion dropbox (instance-level, step 1).
    pub inbox_root: PathBuf,
    /// Tenant galaxy root; the per-noyau dir is `galaxies_root/<noyau>`,
    /// the same path [`crate::subprocess::SystemInvoker::cwd_for_spark`]
    /// pins as the subprocess `cwd` (ADR-080 §3.5).
    pub galaxies_root: PathBuf,
    /// Path to the `cs` binary shelled out for `cs init --upgrade`.
    pub cs_path: PathBuf,
    /// `$HOME` whose `.claude.json` / `.claude/settings.json` the
    /// spawned worker reads (Famille B, steps 3a/3b).
    pub claude_home: PathBuf,
    /// Optional formula seed dir; formulas missing after `cs init` are
    /// backfilled from here. `None` skips the belt-and-braces copy
    /// (`cs init --upgrade` already seeds the builtin formulas).
    pub formulas_seed_dir: Option<PathBuf>,
}

impl ImageInit {
    /// Materialise the instance-level artifacts plus one galaxy tree per
    /// `noyau`. Best-effort: a per-step failure lands in the report and
    /// is logged, but never aborts the boot.
    #[must_use]
    pub fn run(&self, noyaux: &[Noyau]) -> ImageInitReport {
        // Step 1 — whispers/inbox (instance-level, shared across noyaux
        // per Q-F default).
        let inbox = ensure_dir(&self.inbox_root);

        // Steps 2/2a/2b/3 — per noyau.
        let noyaux_reports = noyaux.iter().map(|n| self.materialize_noyau(n)).collect();

        // Steps 3a/3b — Claude Code worker env (instance-level: the
        // spawned worker reads the adapter's `$HOME`).
        let claude_onboarding = ensure_claude_onboarding(&self.claude_home);
        let claude_skip_dangerous = ensure_skip_dangerous(&self.claude_home);

        ImageInitReport {
            inbox,
            noyaux: noyaux_reports,
            claude_onboarding,
            claude_skip_dangerous,
        }
    }

    fn materialize_noyau(&self, noyau: &Noyau) -> NoyauReport {
        let root = self.galaxies_root.join(noyau.as_str());
        // Step 2 — state subtree. Creates `.cosmon/` as a side effect,
        // which `cs init --upgrade` requires to exist.
        let state_dirs = ensure_state_subtree(&root);
        // Step 2a — `cs init --upgrade` (only when config.toml absent).
        let cs_init = ensure_cs_init(&self.cs_path, &root);
        // Step 2b — `git init` + commit (only when `.git` absent). Run
        // after cs init so the initial commit captures the seeded tree.
        let git_init = ensure_git_init(&root, noyau);
        // Step 3 — formula backfill (belt-and-braces; cs init already
        // seeds the builtins).
        let formulas = backfill_formulas(&root, self.formulas_seed_dir.as_deref());
        NoyauReport {
            noyau: noyau.as_str().to_owned(),
            state_dirs,
            cs_init,
            git_init,
            formulas,
        }
    }
}

/// Create `dir` (and parents) if absent. `AlreadyPresent` when it
/// already existed, `Done` when created, `Failed` on an `io::Error`.
fn ensure_dir(dir: &Path) -> StepOutcome {
    if dir.is_dir() {
        return StepOutcome::AlreadyPresent;
    }
    match std::fs::create_dir_all(dir) {
        Ok(()) => StepOutcome::Done,
        Err(e) => StepOutcome::Failed(format!("create_dir_all {}: {e}", dir.display())),
    }
}

/// Step 2 — `.cosmon/state/{events,molecules,fleets/default}`. Mirrors
/// the script's three sub-trees verbatim.
fn ensure_state_subtree(root: &Path) -> StepOutcome {
    let state = root.join(".cosmon").join("state");
    let mut any_created = false;
    for sub in ["events", "molecules", "fleets/default"] {
        let target = state.join(sub);
        if target.is_dir() {
            continue;
        }
        if let Err(e) = std::fs::create_dir_all(&target) {
            return StepOutcome::Failed(format!("create_dir_all {}: {e}", target.display()));
        }
        any_created = true;
    }
    if any_created {
        StepOutcome::Done
    } else {
        StepOutcome::AlreadyPresent
    }
}

/// Step 2a — shell-out `cs init --upgrade` in `root` when
/// `config.toml` is absent. `cs init` stays the single authority for
/// `project_id` / formulas / registry; this only invokes it.
fn ensure_cs_init(cs_path: &Path, root: &Path) -> StepOutcome {
    let config = root.join(".cosmon").join("config.toml");
    if config.is_file() {
        return StepOutcome::AlreadyPresent;
    }
    let output = Command::new(cs_path)
        .arg("init")
        .arg("--upgrade")
        .current_dir(root)
        .output();
    match output {
        Ok(out) if out.status.success() => StepOutcome::Done,
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let excerpt: String = stderr.chars().take(256).collect();
            StepOutcome::Failed(format!(
                "cs init --upgrade exit {:?}: {excerpt}",
                out.status.code()
            ))
        }
        Err(e) => StepOutcome::Failed(format!("spawn cs init: {e}")),
    }
}

/// Step 2b — `git init` + initial commit in `root` when `.git` is
/// absent. The identity is set locally (never global, never pushed —
/// the repo is local-only). `cs tackle` walks up for `.git`; without
/// it, it refuses to start.
fn ensure_git_init(root: &Path, noyau: &Noyau) -> StepOutcome {
    if root.join(".git").is_dir() {
        return StepOutcome::AlreadyPresent;
    }
    let email = format!("cosmon@{}.local", noyau.as_str());
    let steps: [&[&str]; 5] = [
        &["init", "-q"],
        &["config", "user.email", &email],
        &["config", "user.name", "cosmon-runtime"],
        &["add", "-A"],
        &["commit", "-q", "-m", "init cosmon-state galaxy"],
    ];
    for step in steps {
        match Command::new("git").args(step).current_dir(root).output() {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let excerpt: String = stderr.chars().take(256).collect();
                return StepOutcome::Failed(format!(
                    "git {:?} exit {:?}: {excerpt}",
                    step,
                    out.status.code()
                ));
            }
            Err(e) => return StepOutcome::Failed(format!("spawn git {step:?}: {e}")),
        }
    }
    StepOutcome::Done
}

/// Step 3 — copy any `*.formula.toml` from `seed_dir` into
/// `<root>/.cosmon/formulas/` that is not already present. This guards
/// the volume-shadow case (a named volume masking the image-baked
/// formulas); `cs init --upgrade` already backfills the builtin set, so
/// this only adds formulas the seed carries beyond the builtins.
fn backfill_formulas(root: &Path, seed_dir: Option<&Path>) -> StepOutcome {
    let Some(seed_dir) = seed_dir else {
        return StepOutcome::Skipped;
    };
    if !seed_dir.is_dir() {
        return StepOutcome::Skipped;
    }
    let dest = root.join(".cosmon").join("formulas");
    if let Err(e) = std::fs::create_dir_all(&dest) {
        return StepOutcome::Failed(format!("create_dir_all {}: {e}", dest.display()));
    }
    let entries = match std::fs::read_dir(seed_dir) {
        Ok(e) => e,
        Err(e) => return StepOutcome::Failed(format!("read_dir {}: {e}", seed_dir.display())),
    };
    let mut copied = 0_usize;
    for entry in entries.flatten() {
        let src = entry.path();
        let Some(name) = src.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".formula.toml") {
            continue;
        }
        let target = dest.join(name);
        if target.exists() {
            continue;
        }
        if let Err(e) = std::fs::copy(&src, &target) {
            return StepOutcome::Failed(format!("copy {name}: {e}"));
        }
        copied += 1;
    }
    if copied > 0 {
        StepOutcome::Done
    } else {
        StepOutcome::AlreadyPresent
    }
}

/// Boot bootstrap — copy the image-baked default nucleon binding(s) from
/// `seed_dir` into `<state_dir>/nucleons/` **only when no binding exists
/// yet**. Symmetric to `backfill_formulas`, but the no-clobber rule is
/// stricter: a single existing `*/oidc-identity*.toml` anywhere under
/// `nucleons/` means a real binding is already provisioned (operator gesture
/// or a prior boot), so the seed is skipped wholesale. The seed only ever
/// *bootstraps* an empty instance — it never reconciles, merges, or clobbers
/// a live binding (belt-and-braces).
///
/// This runs BEFORE [`crate::nucleon_map::HabilitationMap::load`] at boot: the
/// binding *is* the bootstrap. An instance whose `nucleons/` is empty
/// (root-owned volume, no SSH/API/SSM write path — the autonomie-pool
/// pathology) resolves zero noyaux, so
/// [`ImageInit::run`] has nothing to materialise and the instance cannot
/// admit its operator. The seed closes that gap so a freshly-cut pool
/// instance self-provisions its default binding at first boot — no root
/// gesture per instance.
///
/// Best-effort: a copy failure lands in the returned [`StepOutcome`] and is
/// logged, never fatal — the adapter still boots (it simply resolves zero
/// noyaux, exactly as before this seed existed).
#[must_use]
pub fn seed_nucleons(state_dir: &Path, seed_dir: Option<&Path>) -> StepOutcome {
    let Some(seed_dir) = seed_dir else {
        return StepOutcome::Skipped;
    };
    if !seed_dir.is_dir() {
        return StepOutcome::Skipped;
    }
    let dest_root = state_dir.join("nucleons");
    // No-clobber: any existing binding short-circuits the entire seed.
    if nucleons_has_binding(&dest_root) {
        return StepOutcome::AlreadyPresent;
    }
    // Copy each `<seed>/<nucleon_id>/oidc-identity*.toml` into
    // `<state_dir>/nucleons/<nucleon_id>/`, mirroring the on-disk layout the
    // map loader reads.
    let entries = match std::fs::read_dir(seed_dir) {
        Ok(e) => e,
        Err(e) => return StepOutcome::Failed(format!("read_dir {}: {e}", seed_dir.display())),
    };
    let mut copied = 0_usize;
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let src_dir = entry.path();
        let Some(id) = src_dir.file_name() else {
            continue;
        };
        let dest_dir = dest_root.join(id);
        let idents = match std::fs::read_dir(&src_dir) {
            Ok(e) => e,
            Err(e) => return StepOutcome::Failed(format!("read_dir {}: {e}", src_dir.display())),
        };
        for ident in idents.flatten() {
            let src = ident.path();
            let Some(name) = src.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !is_oidc_identity(name) {
                continue;
            }
            if let Err(e) = std::fs::create_dir_all(&dest_dir) {
                return StepOutcome::Failed(format!("create_dir_all {}: {e}", dest_dir.display()));
            }
            if let Err(e) = std::fs::copy(&src, dest_dir.join(name)) {
                return StepOutcome::Failed(format!("copy {name}: {e}"));
            }
            copied += 1;
        }
    }
    if copied > 0 {
        StepOutcome::Done
    } else {
        // Seed dir present but carried no binding file — nothing to do.
        StepOutcome::Skipped
    }
}

/// `true` when any `<root>/<id>/oidc-identity*.toml` exists. Mirrors the
/// exact file-selection predicate [`crate::nucleon_map::HabilitationMap::load`]
/// uses, so "has a binding" here means precisely "load would find one".
fn nucleons_has_binding(root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(idents) = std::fs::read_dir(entry.path()) else {
            continue;
        };
        for ident in idents.flatten() {
            let path = ident.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if is_oidc_identity(name) {
                    return true;
                }
            }
        }
    }
    false
}

/// Match the binding-file predicate of the map loader
/// ([`crate::nucleon_map::HabilitationMap::load`]): a `.toml` file (any case)
/// whose name starts with `oidc-identity` (one per Orbitale).
fn is_oidc_identity(name: &str) -> bool {
    name.starts_with("oidc-identity")
        && Path::new(name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
}

/// Step 3a — `<claude_home>/.claude.json` with
/// `hasCompletedOnboarding = true`, merged into any existing file.
/// `serde_json` — no `node`. Returns `AlreadyPresent` when the gate is
/// already set (no write), so re-runs do not churn the file.
fn ensure_claude_onboarding(claude_home: &Path) -> StepOutcome {
    let path = claude_home.join(".claude.json");
    merge_json_gate(&path, "hasCompletedOnboarding", true, |obj| {
        // On first write, also record the onboarding version (matches
        // the script's heredoc). Only set when absent so we never
        // clobber a value Claude later wrote.
        obj.entry("lastOnboardingVersion".to_owned())
            .or_insert_with(|| serde_json::Value::String(CLAUDE_ONBOARDING_VERSION.to_owned()));
    })
}

/// Step 3b — `<claude_home>/.claude/settings.json` with
/// `skipDangerousModePermissionPrompt = true`, merged into any existing
/// file. `serde_json` — no `node`.
fn ensure_skip_dangerous(claude_home: &Path) -> StepOutcome {
    let dir = claude_home.join(".claude");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return StepOutcome::Failed(format!("create_dir_all {}: {e}", dir.display()));
    }
    let path = dir.join("settings.json");
    merge_json_gate(&path, "skipDangerousModePermissionPrompt", true, |_| {})
}

/// Merge a single boolean gate field into a JSON object file, creating
/// the file if absent. `extra` runs only when a write happens, to add
/// companion fields (e.g. `lastOnboardingVersion`).
///
/// - File absent → write `{ <field>: true, <extra...> }`.
/// - File present, gate already `true` → `AlreadyPresent` (no write).
/// - File present, gate absent/false → set it, run `extra`, write the
///   merged object (every other key preserved).
/// - Unparseable file → treated as `{}` (matches the script's `catch`),
///   then written with the gate set.
fn merge_json_gate(
    path: &Path,
    field: &str,
    value: bool,
    extra: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> StepOutcome {
    use serde_json::Value;

    let mut obj: serde_json::Map<String, Value> = if path.exists() {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default(),
            Err(e) => return StepOutcome::Failed(format!("read {}: {e}", path.display())),
        }
    } else {
        serde_json::Map::new()
    };

    if obj.get(field).and_then(Value::as_bool) == Some(value) {
        return StepOutcome::AlreadyPresent;
    }

    obj.insert(field.to_owned(), Value::Bool(value));
    extra(&mut obj);

    let serialized = match serde_json::to_string_pretty(&Value::Object(obj)) {
        Ok(s) => format!("{s}\n"),
        Err(e) => return StepOutcome::Failed(format!("serialize {}: {e}", path.display())),
    };
    // The parent ($HOME, or $HOME/.claude) exists in production but may
    // not on a fresh volume; create it so the write never races a
    // missing directory.
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return StepOutcome::Failed(format!("create_dir_all {}: {e}", parent.display()));
        }
    }
    match std::fs::write(path, serialized) {
        Ok(()) => StepOutcome::Done,
        Err(e) => StepOutcome::Failed(format!("write {}: {e}", path.display())),
    }
}

/// Anthropic key resolution backends, in priority order. Mirrors the
/// shell ladder so the binary resolves the same key the V1 script would
/// (docker-secret → operator-file → inherited env).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnthropicKeyBackend {
    /// `/run/secrets/anthropic-api-key` (`docker-compose` secret mount).
    DockerSecret,
    /// `/var/lib/cosmon/secrets/anthropic-api-key` (operator-managed).
    OperatorFile,
    /// Inherited `ANTHROPIC_API_KEY` env var (dev only — `ps`-visible).
    Env,
}

impl AnthropicKeyBackend {
    /// Stable log label for the backend.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            AnthropicKeyBackend::DockerSecret => "docker-secret",
            AnthropicKeyBackend::OperatorFile => "operator-file",
            AnthropicKeyBackend::Env => "env",
        }
    }
}

/// Step 3c — resolve the Anthropic API key from the ladder, returning
/// the trimmed key and the backend that supplied it. Pure over its
/// three inputs so it is unit-testable without touching real paths or
/// env.
///
/// Whitespace-only values are rejected at every step (ADR-0008 §2
/// anti-pattern "Blanker `ANTHROPIC_API_KEY=`"), so a stale blank env
/// var cannot wrongly outrank a populated file.
#[must_use]
pub fn resolve_anthropic_key_from(
    docker_secret: &Path,
    operator_file: &Path,
    env_value: Option<&str>,
) -> Option<(String, AnthropicKeyBackend)> {
    if let Some(key) = read_secret_file(docker_secret) {
        return Some((key, AnthropicKeyBackend::DockerSecret));
    }
    if let Some(key) = read_secret_file(operator_file) {
        return Some((key, AnthropicKeyBackend::OperatorFile));
    }
    if let Some(raw) = env_value {
        let trimmed = strip_ws(raw);
        if !trimmed.is_empty() {
            return Some((trimmed, AnthropicKeyBackend::Env));
        }
    }
    None
}

/// Step 3c convenience wrapper: the default container paths plus the
/// process `ANTHROPIC_API_KEY`.
#[must_use]
pub fn resolve_anthropic_key() -> Option<(String, AnthropicKeyBackend)> {
    let env_value = std::env::var("ANTHROPIC_API_KEY").ok();
    resolve_anthropic_key_from(
        Path::new(ANTHROPIC_SECRET_DOCKER),
        Path::new(ANTHROPIC_SECRET_OPERATOR),
        env_value.as_deref(),
    )
}

/// SHA-256 first-12-hex fingerprint of a key, for D4 traceability in
/// logs without leaking the secret (matches the script's `sha256sum |
/// cut -c1-12` over the trimmed key).
#[must_use]
pub fn key_fingerprint(key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    let hex = format!("{digest:x}");
    hex.chars().take(12).collect()
}

/// Read a secret file, returning its whitespace-stripped contents when
/// the file exists and is non-empty after stripping.
fn read_secret_file(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = strip_ws(&raw);
    (!trimmed.is_empty()).then_some(trimmed)
}

/// Remove *all* ASCII whitespace (matching the script's `tr -d
/// '[:space:]'`), not just leading/trailing — a secret file may carry
/// stray internal newlines from a mis-paste.
fn strip_ws(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Anthropic ladder (step 3c) ──────────────────────────────────

    #[test]
    fn anthropic_docker_secret_wins() {
        let td = tempfile::tempdir().unwrap();
        let docker = td.path().join("docker-secret");
        let operator = td.path().join("operator-file");
        std::fs::write(&docker, "sk-docker\n").unwrap();
        std::fs::write(&operator, "sk-operator\n").unwrap();
        let (key, backend) =
            resolve_anthropic_key_from(&docker, &operator, Some("sk-env")).unwrap();
        assert_eq!(key, "sk-docker");
        assert_eq!(backend, AnthropicKeyBackend::DockerSecret);
    }

    #[test]
    fn anthropic_operator_file_beats_env() {
        let td = tempfile::tempdir().unwrap();
        let docker = td.path().join("absent-docker");
        let operator = td.path().join("operator-file");
        std::fs::write(&operator, "  sk-operator  ").unwrap();
        let (key, backend) =
            resolve_anthropic_key_from(&docker, &operator, Some("sk-env")).unwrap();
        assert_eq!(key, "sk-operator");
        assert_eq!(backend, AnthropicKeyBackend::OperatorFile);
    }

    #[test]
    fn anthropic_env_is_last_resort() {
        let td = tempfile::tempdir().unwrap();
        let docker = td.path().join("absent-docker");
        let operator = td.path().join("absent-operator");
        let (key, backend) =
            resolve_anthropic_key_from(&docker, &operator, Some("sk-env")).unwrap();
        assert_eq!(key, "sk-env");
        assert_eq!(backend, AnthropicKeyBackend::Env);
    }

    #[test]
    fn anthropic_blank_env_does_not_win() {
        let td = tempfile::tempdir().unwrap();
        let docker = td.path().join("absent-docker");
        let operator = td.path().join("absent-operator");
        // A stale, whitespace-only env var must resolve to None, not to
        // a spuriously-populated blank key.
        assert!(resolve_anthropic_key_from(&docker, &operator, Some("   \n\t")).is_none());
    }

    #[test]
    fn anthropic_blank_docker_falls_through_to_operator() {
        let td = tempfile::tempdir().unwrap();
        let docker = td.path().join("docker-secret");
        let operator = td.path().join("operator-file");
        std::fs::write(&docker, "   \n").unwrap(); // blank → rejected
        std::fs::write(&operator, "sk-operator").unwrap();
        let (key, backend) = resolve_anthropic_key_from(&docker, &operator, None).unwrap();
        assert_eq!(key, "sk-operator");
        assert_eq!(backend, AnthropicKeyBackend::OperatorFile);
    }

    #[test]
    fn anthropic_all_empty_is_none() {
        let td = tempfile::tempdir().unwrap();
        assert!(
            resolve_anthropic_key_from(&td.path().join("a"), &td.path().join("b"), None,).is_none()
        );
    }

    #[test]
    fn fingerprint_is_twelve_hex_and_stable() {
        let fp = key_fingerprint("sk-some-key");
        assert_eq!(fp.len(), 12);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(fp, key_fingerprint("sk-some-key"));
        assert_ne!(fp, key_fingerprint("sk-other-key"));
    }

    // ── Claude Code gates (steps 3a/3b) ─────────────────────────────

    #[test]
    fn onboarding_written_when_absent() {
        let td = tempfile::tempdir().unwrap();
        let outcome = ensure_claude_onboarding(td.path());
        assert_eq!(outcome, StepOutcome::Done);
        let text = std::fs::read_to_string(td.path().join(".claude.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["hasCompletedOnboarding"], serde_json::Value::Bool(true));
        assert_eq!(v["lastOnboardingVersion"], CLAUDE_ONBOARDING_VERSION);
    }

    #[test]
    fn onboarding_merges_preserving_other_keys() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join(".claude.json");
        // Simulate a file Claude already wrote: other keys + the gate
        // not yet set.
        std::fs::write(
            &path,
            r#"{"firstStartTime":"2026-05-21","hasCompletedOnboarding":false,"projects":{"x":1}}"#,
        )
        .unwrap();
        let outcome = ensure_claude_onboarding(td.path());
        assert_eq!(outcome, StepOutcome::Done);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["hasCompletedOnboarding"], serde_json::Value::Bool(true));
        // Untouched keys survive the merge.
        assert_eq!(v["firstStartTime"], "2026-05-21");
        assert_eq!(v["projects"]["x"], 1);
    }

    #[test]
    fn onboarding_is_idempotent_no_rewrite() {
        let td = tempfile::tempdir().unwrap();
        let first = ensure_claude_onboarding(td.path());
        assert_eq!(first, StepOutcome::Done);
        let second = ensure_claude_onboarding(td.path());
        assert_eq!(second, StepOutcome::AlreadyPresent);
    }

    #[test]
    fn skip_dangerous_written_and_idempotent() {
        let td = tempfile::tempdir().unwrap();
        let first = ensure_skip_dangerous(td.path());
        assert_eq!(first, StepOutcome::Done);
        let path = td.path().join(".claude").join("settings.json");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            v["skipDangerousModePermissionPrompt"],
            serde_json::Value::Bool(true)
        );
        let second = ensure_skip_dangerous(td.path());
        assert_eq!(second, StepOutcome::AlreadyPresent);
    }

    #[test]
    fn unparseable_claude_json_treated_as_empty() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join(".claude.json");
        std::fs::write(&path, "}{ not json").unwrap();
        let outcome = ensure_claude_onboarding(td.path());
        assert_eq!(outcome, StepOutcome::Done);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["hasCompletedOnboarding"], serde_json::Value::Bool(true));
    }

    // ── Filesystem steps (1, 2, 3) ──────────────────────────────────

    #[test]
    fn state_subtree_creates_three_dirs_then_idempotent() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().join("galaxies").join("tenant-demo-sandbox");
        assert_eq!(ensure_state_subtree(&root), StepOutcome::Done);
        for sub in ["events", "molecules", "fleets/default"] {
            assert!(
                root.join(".cosmon/state").join(sub).is_dir(),
                "missing {sub}"
            );
        }
        assert_eq!(ensure_state_subtree(&root), StepOutcome::AlreadyPresent);
    }

    #[test]
    fn formulas_skipped_when_no_seed_dir() {
        let td = tempfile::tempdir().unwrap();
        assert_eq!(backfill_formulas(td.path(), None), StepOutcome::Skipped);
    }

    #[test]
    fn formulas_copied_then_idempotent() {
        let td = tempfile::tempdir().unwrap();
        let seed = td.path().join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        std::fs::write(seed.join("custom.formula.toml"), "name='custom'\n").unwrap();
        std::fs::write(seed.join("not-a-formula.txt"), "ignored").unwrap();
        let root = td.path().join("galaxy");
        assert_eq!(backfill_formulas(&root, Some(&seed)), StepOutcome::Done);
        assert!(root.join(".cosmon/formulas/custom.formula.toml").is_file());
        assert!(!root.join(".cosmon/formulas/not-a-formula.txt").exists());
        assert_eq!(
            backfill_formulas(&root, Some(&seed)),
            StepOutcome::AlreadyPresent
        );
    }

    // ── Nucleon-binding seed (smithy autonomie-pool) ──────────────

    /// Write a minimal-but-valid default binding under `<seed>/<id>/`,
    /// shaped like the smithy-baked `/opt/cosmon-nucleons/tenant-demo/`.
    fn write_seed_binding(seed: &Path, id: &str) {
        let dir = seed.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("oidc-identity.toml"),
            "nucleon_id = \"tenant-demo\"\nnoyau = \"tenant-demo-sandbox\"\n",
        )
        .unwrap();
    }

    #[test]
    fn nucleons_seed_skipped_when_no_seed_dir() {
        let td = tempfile::tempdir().unwrap();
        assert_eq!(seed_nucleons(td.path(), None), StepOutcome::Skipped);
    }

    #[test]
    fn nucleons_seed_skipped_when_seed_dir_absent() {
        let td = tempfile::tempdir().unwrap();
        let missing = td.path().join("no-such-seed");
        assert_eq!(
            seed_nucleons(td.path(), Some(&missing)),
            StepOutcome::Skipped
        );
    }

    /// Empty `nucleons/` + seed present → binding copied in, then a
    /// re-run is a no-clobber `AlreadyPresent`.
    #[test]
    fn nucleons_seed_copies_into_empty_then_idempotent() {
        let td = tempfile::tempdir().unwrap();
        let seed = td.path().join("opt-cosmon-nucleons");
        write_seed_binding(&seed, "tenant-demo");
        let state = td.path().join("state");

        assert_eq!(seed_nucleons(&state, Some(&seed)), StepOutcome::Done);
        let dest = state.join("nucleons/tenant-demo/oidc-identity.toml");
        assert!(dest.is_file(), "binding must be seeded into nucleons/");
        assert!(
            std::fs::read_to_string(&dest)
                .unwrap()
                .contains("tenant-demo-sandbox"),
            "seeded binding must carry the baked content"
        );

        // Second pass: a binding now exists → seed is a no-op.
        assert_eq!(
            seed_nucleons(&state, Some(&seed)),
            StepOutcome::AlreadyPresent
        );
    }

    /// A pre-existing real binding must NEVER be clobbered, even if the
    /// seed carries a different `<id>` — any binding short-circuits.
    #[test]
    fn nucleons_seed_never_clobbers_existing_binding() {
        let td = tempfile::tempdir().unwrap();
        let seed = td.path().join("opt-cosmon-nucleons");
        write_seed_binding(&seed, "tenant-demo");
        let state = td.path().join("state");

        // Operator-provisioned binding already present under a different id.
        let real = state.join("nucleons/operator-real");
        std::fs::create_dir_all(&real).unwrap();
        let real_file = real.join("oidc-identity.toml");
        std::fs::write(&real_file, "nucleon_id = \"real\"\nnoyau = \"prod\"\n").unwrap();

        assert_eq!(
            seed_nucleons(&state, Some(&seed)),
            StepOutcome::AlreadyPresent,
            "an existing binding must short-circuit the seed"
        );
        // The seed's `tenant-demo` binding must NOT have been written.
        assert!(
            !state.join("nucleons/tenant-demo").exists(),
            "seed must not add bindings when one already exists"
        );
        // The real binding must be untouched.
        assert_eq!(
            std::fs::read_to_string(&real_file).unwrap(),
            "nucleon_id = \"real\"\nnoyau = \"prod\"\n"
        );
    }

    /// Seed dir present but carrying no `oidc-identity*.toml` → `Skipped`,
    /// and `nucleons/` is left untouched.
    #[test]
    fn nucleons_seed_skips_when_seed_carries_no_binding() {
        let td = tempfile::tempdir().unwrap();
        let seed = td.path().join("opt-cosmon-nucleons");
        std::fs::create_dir_all(seed.join("tenant-demo")).unwrap();
        std::fs::write(seed.join("tenant-demo/README.txt"), "not a binding").unwrap();
        let state = td.path().join("state");

        assert_eq!(seed_nucleons(&state, Some(&seed)), StepOutcome::Skipped);
        assert!(!state.join("nucleons/tenant-demo").exists());
    }
}
