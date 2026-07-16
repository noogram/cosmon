// SPDX-License-Identifier: AGPL-3.0-only

//! `cs resurrect` — revive a wrecked molecule with a fresh worker.
//!
//! Resurrection composes the molecule's on-disk artifacts (prompt, briefing,
//! git log, worker log, synthesis) into a bootstrap prompt for a new
//! worker, reuses `cs tackle`'s launch path (worktree + tmux + claude),
//! and flips the molecule back to `Running`. The molecule never died —
//! only the observer was lost.
//!
//! Pre-conditions (all must hold, else [`ResurrectError`]):
//!
//! - `status == Frozen` (the output of `cs recover` for a wreck)
//! - tmux session is not alive (no competing worker)
//! - `prompt.md` + `briefing.md` present in the molecule directory
//! - Resurrection flock can be acquired (no concurrent second call)
//!
//! Success emits:
//!
//! - `EventV2::Resurrected { composed_prompt_bytes, prior_count, ... }`
//! - A breadcrumb at `.cosmon/state/fleets/<f>/molecules/<id>/wrecks/<ts>.json`
//! - Status flip `Frozen → Running`
//! - Tmux session spawned (same `session_name` as the original when known).

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::Utc;
use cosmon_core::event_v2::EventV2;
use cosmon_core::id::WorkerId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::spawn_seam::validate_adapter_name;
use cosmon_core::transport::TransportBackend;
use cosmon_filestore::FileStore;
use cosmon_state::{MoleculeData, StateStore};
use cosmon_transport::TmuxBackend;
use fs2::FileExt;
use sha2::{Digest, Sha256};

use super::Context;
use crate::resurrect::{compose_resurrection_prompt, ComposeContext, ResurrectError};

/// Arguments for the `resurrect` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Molecule ID of the wreck to revive.
    pub molecule: String,

    /// Skip tmux spawn — print the composed prompt to stdout. No state
    /// mutation, no event emission, no breadcrumb.
    #[arg(long)]
    pub dry_run: bool,
}

/// Execute the `resurrect` command.
#[allow(clippy::too_many_lines)]
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    super::require_project_identity(ctx)?;

    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = FileStore::new(&state_dir);

    let mol = resolve_molecule(&store, &args.molecule)?;
    let mol_id = mol.id.clone();

    // Pre-condition: state == Frozen (the wreck state produced by `cs recover`).
    if mol.status != MoleculeStatus::Frozen {
        return Err(anyhow::Error::from(ResurrectError::NotAWreck {
            molecule_id: mol_id.as_str().to_owned(),
            status: mol.status.to_string(),
        }));
    }

    let mol_dir = store.molecule_dir(&mol_id);

    // Pre-condition: prompt.md and briefing.md exist.
    for required in ["prompt.md", "briefing.md"] {
        if !mol_dir.join(required).exists() {
            return Err(anyhow::Error::from(ResurrectError::ArtifactsMissing {
                mol_dir: mol_dir.clone(),
                missing: required.to_owned(),
            }));
        }
    }

    // Resolve session + branch context.
    let repo_root = crate::cmd::tackle::find_repo_root()?;
    let branch = mol
        .originating_branch
        .clone()
        .unwrap_or_else(|| format!("feat/{}", mol_id.as_str()));
    let session_name = mol
        .session_name
        .clone()
        .unwrap_or_else(|| mol_id.as_str().to_owned());
    let socket = super::tmux_socket_name(ctx);
    let backend = TmuxBackend::new(&socket);
    let wid = WorkerId::new(&session_name)?;

    // Pre-condition: tmux session must NOT be alive.
    if backend.is_alive(&wid).unwrap_or(false) {
        return Err(anyhow::Error::from(ResurrectError::DoubleResurrect {
            molecule_id: mol_id.as_str().to_owned(),
            session: session_name.clone(),
        }));
    }

    // Acquire resurrection flock — rejects concurrent second invocations.
    fs::create_dir_all(&mol_dir)?;
    let lock_path = mol_dir.join("resurrect.lock");
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)?;
    lock_file
        .try_lock_exclusive()
        .map_err(|_| ResurrectError::FlockContended {
            molecule_id: mol_id.as_str().to_owned(),
        })?;

    // Count prior resurrections by scanning the breadcrumb directory.
    // Cheap and authoritative — one file per resurrection, filesystem is truth.
    let wrecks_dir = mol_dir.join("wrecks");
    let prior_count = count_prior_wrecks(&wrecks_dir);

    // Compose the bootstrap prompt (pure function).
    let compose_ctx = ComposeContext {
        molecule_id: mol_id.as_str(),
        branch: &branch,
        repo_root: &repo_root,
        prior_count,
        next_step_display: Some(format!("Step {}/{}", mol.current_step + 1, mol.total_steps)),
        synthesis_is_draft: false,
    };
    let prompt =
        compose_resurrection_prompt(&mol_dir, &compose_ctx).map_err(anyhow::Error::from)?;
    let composed_prompt_bytes = prompt.len() as u64;

    if args.dry_run {
        if ctx.json {
            let out = serde_json::json!({
                "command": "resurrect",
                "molecule_id": mol_id.as_str(),
                "dry_run": true,
                "composed_prompt_bytes": composed_prompt_bytes,
                "prior_count": prior_count,
                "prompt": prompt,
            });
            println!("{out}");
        } else {
            println!("{prompt}");
        }
        return Ok(());
    }

    // Ensure worktree exists (recreate if branch survived but directory was
    // removed — aligns with Hawking's tip.sha concern, minimal version).
    let worktree_path = repo_root.join(".worktrees").join(mol_id.as_str());
    if !worktree_path.exists() {
        crate::cmd::tackle::create_worktree(&repo_root, &worktree_path, &branch, None)?;
    }

    // Resolve tip sha for the breadcrumb (best effort — empty branch OK).
    let tip_sha = resolve_tip_sha(&repo_root, &branch);

    // Spawn the tmux session and inject the composed prompt.
    // `cs resurrect` rebuilds a Claude session by construction (see
    // the matching `register_tackle_worker` call below). Pin the
    // adapter so spawn_and_prompt routes through the Claude branch
    // explicitly — pre-C8 the routing was implicit; ADR-097 C8 made
    // it a required argument so the cat-test sees a faithful
    // `adapter_name`. ADR-099 / TS-0 promotes that argument to a
    // `ValidatedAdapterName`: the name is checked against the
    // built-in registry before reaching the spawn seam, so the
    // resurrection path obeys the same dispatch-site stability
    // contract as `cs tackle`.
    let (adapter, _supervision, loop_ownership) =
        validate_adapter_name("claude", &["claude".to_owned(), "aider".to_owned()])
            .expect("'claude' is a built-in adapter");
    // `cs resurrect` has no `--model` flag and no formula context, so its
    // model resolution is the env tier alone (delib-20260704-b476 C1):
    // `$COSMON_DEFAULT_MODEL` else the legacy `$ANTHROPIC_MODEL`. This
    // preserves the exact pre-C1 behaviour — before C1 the model was read
    // inline from `$ANTHROPIC_MODEL` inside `resolve_worker_model`, which
    // now takes the pin as a parameter.
    let resurrect_model = crate::cmd::tackle::env_default_model().map(|(v, _)| v);
    crate::cmd::tackle::spawn_and_prompt(
        &backend,
        &wid,
        &session_name,
        &worktree_path,
        &prompt,
        None,
        &mol,
        &mol_dir,
        &state_dir,
        &adapter,
        None,
        resurrect_model.as_deref(),
        // No adapters config is threaded here, so no operator strong set —
        // cosmon's intrinsic `DEFAULT_STRONG_MODELS` still keeps a cheap
        // pin's fallback tail off the strong model (task-20260705-ba98).
        &[],
    )?;

    // Re-arm the worker-exit → `cs done` bridge. A resurrected worker
    // has the same terminal-closure need as a freshly tackled one.
    //
    // ADR-052 child #4: the hook is mandatory. If install fails we log
    // but do NOT tear down — the resurrected molecule's state is
    // partially committed by this point (the new tmux session already
    // exists). A patrol-driven witness + the backstop `cs patrol
    // --harvest` sweep still covers the gap. Tackle-time install
    // remains the only path that refuses to proceed on install failure.
    if let Err(e) =
        crate::cmd::tackle::install_harvest_hook(&backend, &session_name, &mol_id, &repo_root)
    {
        eprintln!(
            "cs resurrect: warning: failed to install pane-died hook on \
             {session_name}: {e}. Patrol sweeps will backstop liveness."
        );
    }

    // Flip molecule status Frozen → Running under the fleet lock so concurrent
    // patrol/done invocations don't clobber the transition.
    let prior_session = mol.session_name.clone();
    {
        // ADR-131 Decision 2: RAII guard replaces the lock-bounding closure.
        let _g = store.lock_fleet()?;
        let s = &store;
        let mut updated = s.load_molecule(&mol_id)?;
        updated.status = MoleculeStatus::Running;
        updated.assigned_worker = Some(wid.clone());
        updated.session_name = Some(session_name.clone());
        updated.updated_at = Utc::now();
        // `stuck_at` is the marker for stuck-flavored Frozen
        // (`task-20260509-177e`); clear it on the way back to Running so a
        // future `cs collapse` reports `previous_status: "running"` rather
        // than carrying a stale stuck context across resurrection.
        updated.stuck_at = None;
        s.save_molecule(&mol_id, &updated)?;
        // `cs resurrect` rebuilds a Claude session by construction
        // (the resurrection codepath only emits `claude --resume …`);
        // pin the adapter_name so the WorkerSpawned event matches
        // the binary actually invoked (ADR-097 / C8). ADR-099 / TS-0
        // — `&adapter` is the same `ValidatedAdapterName` constructed
        // above, so the registered worker carries the byte-identical
        // value that the spawn seam consumed.
        crate::cmd::tackle::register_tackle_worker(
            s,
            &wid,
            &worktree_path,
            &repo_root,
            &updated,
            &adapter,
            loop_ownership,
        )?;
    }

    // Breadcrumb — small metadata file, NOT an artifact duplicate.
    let prompt_hash = {
        let mut h = Sha256::new();
        h.update(prompt.as_bytes());
        format!("{:x}", h.finalize())
    };
    if let Err(e) = write_breadcrumb(
        &wrecks_dir,
        &tip_sha,
        prior_count,
        &prompt_hash,
        composed_prompt_bytes,
    ) {
        eprintln!("warn: failed to write resurrect breadcrumb: {e}");
    }

    // Emit Resurrected event + status change.
    let events_path = state_dir.join("events.jsonl");
    let _ = cosmon_state::event_log::emit_one(
        &events_path,
        EventV2::Resurrected {
            molecule_id: mol_id.clone(),
            from_session: prior_session,
            composed_prompt_bytes,
            t_orig_tokens: None,
            prior_count,
        },
        None,
    );
    let _ = cosmon_state::event_log::emit_one(
        &events_path,
        EventV2::MoleculeStatusChanged {
            molecule_id: mol_id.clone(),
            from: "frozen".to_owned(),
            to: "running".to_owned(),
        },
        None,
    );

    if ctx.json {
        let out = serde_json::json!({
            "command": "resurrect",
            "molecule_id": mol_id.as_str(),
            "status": "running",
            "tmux_session": session_name,
            "worktree": worktree_path.to_string_lossy(),
            "branch": branch,
            "prior_count": prior_count,
            "composed_prompt_bytes": composed_prompt_bytes,
            "attach": format!("tmux -L {socket} attach -t {session_name}"),
        });
        println!("{out}");
    } else {
        println!("⚛ Resurrected {mol_id}");
        println!("  session:  {session_name}");
        println!("  branch:   {branch}");
        println!("  worktree: {}", worktree_path.display());
        println!("  prior resurrections: {prior_count}");
        println!("  prompt bytes: {composed_prompt_bytes}");
        println!("  attach: tmux -L {socket} attach -t {session_name}");
    }

    drop(lock_file);
    Ok(())
}

fn resolve_molecule(store: &FileStore, query: &str) -> anyhow::Result<MoleculeData> {
    let mid = cosmon_core::id::MoleculeId::new(query)
        .map_err(|e| anyhow::anyhow!("invalid molecule id `{query}`: {e}"))?;
    store
        .load_molecule(&mid)
        .map_err(|e| anyhow::anyhow!("molecule {query} not found: {e}"))
}

fn count_prior_wrecks(wrecks_dir: &Path) -> u32 {
    let Ok(rd) = fs::read_dir(wrecks_dir) else {
        return 0;
    };
    let n = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .count();
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn resolve_tip_sha(repo_root: &Path, branch: &str) -> String {
    let out = std::process::Command::new("git")
        .args(["rev-parse", branch])
        .current_dir(repo_root)
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_owned(),
        _ => String::new(),
    }
}

fn write_breadcrumb(
    wrecks_dir: &Path,
    tip_sha: &str,
    prior_count: u32,
    prompt_hash: &str,
    prompt_bytes: u64,
) -> std::io::Result<PathBuf> {
    fs::create_dir_all(wrecks_dir)?;
    let ts = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let path = wrecks_dir.join(format!("{ts}.json"));
    let payload = serde_json::json!({
        "timestamp": Utc::now().to_rfc3339(),
        "tip_sha": tip_sha,
        "prior_count": prior_count,
        "composed_prompt_hash": prompt_hash,
        "composed_prompt_bytes": prompt_bytes,
        "t_orig_tokens": serde_json::Value::Null,
    });
    let mut f = fs::File::create(&path)?;
    f.write_all(serde_json::to_string_pretty(&payload)?.as_bytes())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_prior_wrecks_handles_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        assert_eq!(count_prior_wrecks(&missing), 0);
    }

    #[test]
    fn count_prior_wrecks_counts_only_json_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("wrecks");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("one.json"), "{}").unwrap();
        fs::write(dir.join("two.json"), "{}").unwrap();
        fs::write(dir.join("note.txt"), "ignore").unwrap();
        assert_eq!(count_prior_wrecks(&dir), 2);
    }

    #[test]
    fn write_breadcrumb_produces_valid_json() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("wrecks");
        let p = write_breadcrumb(&dir, "abc123", 1, "deadbeef", 2048).unwrap();
        let body = fs::read_to_string(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["tip_sha"], "abc123");
        assert_eq!(v["prior_count"], 1);
        assert_eq!(v["composed_prompt_hash"], "deadbeef");
        assert_eq!(v["composed_prompt_bytes"], 2048);
    }
}
