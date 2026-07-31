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

    // Record the resurrection BEFORE spawning it, for the same reason
    // `cs tackle` does (task-20260727-198f): a tmux worker outlives the
    // process that started it, so any ledger write ordered after the spawn
    // is a window in which a live worker exists that nothing on disk knows
    // about. `cs resurrect` had the identical shape — spawn, install the
    // hook, then flip Frozen → Running — and the same exposure.
    //
    // The bare `?` **on this call** is safe on the ledger axis: `commit_dispatch`
    // undoes the writes it landed before returning an error, so a refused
    // resurrection leaves the molecule exactly as `prior` holds it. That
    // guarantee belongs to `commit_dispatch` alone and does not extend one line
    // further: every fallible step *after* it must roll back explicitly, which
    // is what the promotion block and the spawn arm below do. `prior` exists for
    // both of them.
    let prior = mol.clone();
    let (mol, recorded) = crate::cmd::dispatch_ledger::commit_dispatch(
        &store,
        &mol,
        &crate::cmd::dispatch_ledger::DispatchRecord {
            worker: &wid,
            session_name: &session_name,
            adapter: &adapter,
            loop_ownership,
            model: resurrect_model.as_deref(),
            // A resurrection is a human gesture (`cs resurrect` has no
            // runtime caller), so the claim is sticky — the resident runtime
            // must not preempt a molecule an operator just revived.
            tackled_by: cosmon_core::tackle::TackledBy::Human,
            worktree_path: &worktree_path,
            repo_root: &repo_root,
        },
    )?;
    // `commit_dispatch` only promotes Pending/Queued; a resurrection starts
    // from Frozen, so state the transition the command exists to perform.
    // `stuck_at` is the marker for stuck-flavored Frozen
    // (`task-20260509-177e`); clear it on the way back to Running so a
    // future `cs collapse` reports `previous_status: "running"` rather than
    // carrying a stale stuck context across resurrection.
    //
    // Neither `?` in here may be bare. Once `commit_dispatch` has returned Ok
    // the ledger says a worker is Active — a live entry in `fleet.json`, a bound
    // `MoleculeProcess`, a `WorkerSpawned` on the wire — and no process has been
    // started yet. An early return from this block would leave exactly that: a
    // worker nothing can find, because it never existed. That is the same
    // phantom window the commit-before-spawn reordering opened on the tackle
    // path, which rolls it back at both of its exits; this door was missed.
    //
    // The rollback is deliberately outside the guard's scope. `lock_fleet` is a
    // non-reentrant advisory `flock`, so calling `rollback_dispatch` (which
    // takes it) while still holding it would block on itself forever — the mute
    // hang this codebase treats as worse than the error being handled.
    let promoted = match store.lock_fleet() {
        Ok(_g) => {
            let mut updated = mol;
            updated.status = MoleculeStatus::Running;
            updated.stuck_at = None;
            updated.updated_at = Utc::now();
            store.save_molecule(&mol_id, &updated).map(|()| updated)
        }
        Err(e) => Err(e),
    };
    let mol = match promoted {
        Ok(updated) => updated,
        Err(e) => {
            crate::cmd::dispatch_ledger::rollback_dispatch(&store, &prior, &wid);
            return Err(e.into());
        }
    };

    if let Err(e) = crate::cmd::tackle::spawn_and_prompt(
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
        &recorded,
        // `cs resurrect` starts the profile here, at the spawn. Everything this
        // command did first — worktree repair, model resolution, the ledger
        // commit, the state promotion — is therefore NOT in the profile, so its
        // `spawn.enter=0` is an origin and not a claim that nothing preceded it.
        // Named rather than fixed: `cs tackle` is the dispatch path the #26
        // latency question is about, and threading a second origin here would
        // add a number nobody has asked a question about yet.
        std::time::Instant::now(),
    ) {
        // The spawn we recorded did not happen: undo the ledger entry so the
        // molecule returns to the state the operator can retry from, rather
        // than reading Running against a session that never came up.
        crate::cmd::dispatch_ledger::rollback_dispatch(&store, &prior, &wid);
        return Err(e);
    }

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

    // The Frozen → Running flip, the worker binding and the fleet
    // registration all happened in the pre-spawn commit above; nothing is
    // left to do here but carry the session the molecule held *before* this
    // resurrection into the `Resurrected` event.
    let prior_session = prior.session_name.clone();

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
            // Carries a UTF-8 locale when the spawn env declares none, so a
            // copied attach line renders the worker's TUI instead of a field
            // of underscores (invariant §8x).
            "attach": cosmon_transport::locale::attach_command_from_env(&socket, &session_name),
        });
        println!("{out}");
    } else {
        println!("⚛ Resurrected {mol_id}");
        println!("  session:  {session_name}");
        println!("  branch:   {branch}");
        println!("  worktree: {}", worktree_path.display());
        println!("  prior resurrections: {prior_count}");
        println!("  prompt bytes: {composed_prompt_bytes}");
        println!(
            "  attach: {}",
            cosmon_transport::locale::attach_command_from_env(&socket, &session_name)
        );
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
