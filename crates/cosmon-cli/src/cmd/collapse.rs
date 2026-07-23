// SPDX-License-Identifier: AGPL-3.0-only

//! `cs collapse` — terminate a molecule with final state recording.
//!
//! Transitions a molecule to `Collapsed` (terminal). Records the reason
//! and the step at which the collapse occurred.

use std::fs;
use std::str::FromStr;

use chrono::Utc;
use cosmon_core::event::{Envelope, Event};
use cosmon_core::event_v2::{CollapseReason, EventV2};
use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::{CollapseCause, MoleculeStatus};
use cosmon_state::event_log;

use super::Context;

/// Arguments for the `collapse` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Molecule ID to collapse.
    molecule: String,

    /// Reason for the collapse.
    #[arg(long)]
    reason: String,

    /// Structured cause attribution (ADR-062): `rate_limit`,
    /// `inference_stall`, `manual`, `process_death`, `unknown`. With
    /// `rate_limit`, pair `--account` and `--kind` for the K3 fixture
    /// shape.
    #[arg(long, value_name = "CAUSE")]
    cause: Option<String>,

    /// Account alias for `--cause rate_limit` (e.g. `default`).
    #[arg(long, value_name = "ALIAS")]
    account: Option<String>,

    /// Quota currency name for `--cause rate_limit` (e.g.
    /// `max_rolling_5h`, `max_weekly`, `api_key_org_monthly`,
    /// `financial_usd`, `custody_scoped`). Free-form to remain
    /// extensible across providers.
    #[arg(long, value_name = "KIND")]
    kind: Option<String>,

    /// Operator-facing collapse classification for `cs errors`
    /// aggregation: one of `worker_crashed`, `gate_failed`,
    /// `blocker_stuck`, `manual_abort`, `resource_exhausted`. Any other
    /// value lands in [`CollapseReason::Other`] verbatim.
    #[arg(long, value_name = "REASON_KIND")]
    reason_kind: Option<String>,

    /// Path to the state store root (overrides walk-up discovery).
    #[arg(long)]
    ops_dir: Option<std::path::PathBuf>,
}

/// Parse the `--cause` / `--account` / `--kind` triple into a
/// [`CollapseCause`]. Returns `None` if `--cause` was not supplied —
/// preserving the legacy free-form-`reason`-only behaviour.
fn parse_cause(args: &Args) -> anyhow::Result<Option<CollapseCause>> {
    let Some(raw) = args.cause.as_deref() else {
        return Ok(None);
    };
    let mut cause = CollapseCause::from_str(raw).map_err(|_| {
        anyhow::anyhow!(
            "unknown --cause `{raw}` (expected one of: rate_limit, \
             inference_stall, manual, process_death, unknown)"
        )
    })?;
    if let CollapseCause::RateLimit {
        account,
        kind_quota,
    } = &mut cause
    {
        account.clone_from(&args.account);
        kind_quota.clone_from(&args.kind);
    } else if args.account.is_some() || args.kind.is_some() {
        anyhow::bail!(
            "--account / --kind are only meaningful with --cause rate_limit \
             (received --cause {raw})"
        );
    }
    Ok(Some(cause))
}

/// Execute the `collapse` command.
#[allow(clippy::too_many_lines)]
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let mol_id = MoleculeId::new(&args.molecule)?;
    let cause = parse_cause(args)?;
    let reason_kind = args
        .reason_kind
        .as_deref()
        .map(|s| CollapseReason::from(s.to_owned()));
    let ops_dir = cosmon_filestore::resolve_state_dir(args.ops_dir.as_deref());
    let store = ctx.store_at(&ops_dir);

    let mol_data = store.load_molecule(&mol_id)?;
    let prev_status = mol_data.status;
    // Stuck-flavored Frozen: when the prior transition to Frozen was
    // via `cs stuck`, the wire-level `previous_status` renders as
    // `"stuck"` rather than `"frozen"` so the audit trail preserves the
    // operator's gesture (`task-20260509-177e`).
    let previous_was_stuck = prev_status == MoleculeStatus::Frozen && mol_data.stuck_at.is_some();
    let prev_status_label = if previous_was_stuck {
        "stuck".to_owned()
    } else {
        prev_status.to_string()
    };

    if prev_status == MoleculeStatus::Collapsed {
        if ctx.json {
            let out = serde_json::json!({
                "molecule": mol_id.as_str(),
                "status": "collapsed",
                "already_collapsed": true,
            });
            println!("{}", serde_json::to_string(&out)?);
        } else {
            println!(
                "{} {} already collapsed (no-op)",
                MoleculeStatus::Collapsed.emoji(),
                mol_id
            );
        }
        return Ok(());
    }

    if prev_status == MoleculeStatus::Completed {
        anyhow::bail!("molecule {mol_id} is completed — cannot collapse a completed molecule");
    }

    let mut updated = mol_data;
    updated.status = MoleculeStatus::Collapsed;
    updated.collapse_reason = Some(args.reason.clone());
    updated.collapse_cause.clone_from(&cause);
    updated.collapse_reason_kind.clone_from(&reason_kind);
    updated.collapsed_step = Some(updated.current_step);
    // Terminal transition: clear the inline live-process record so a
    // collapsed molecule never carries a phantom worker pointer
    // (delib-20260426-1bcd #1 fold-in).
    if updated.process.is_some() {
        updated.release_process();
    }
    updated.updated_at = Utc::now();
    store.save_molecule(&updated.id.clone(), &updated)?;

    // Append to log.md.
    let mol_dir = store.molecule_dir(&mol_id);
    let log_path = mol_dir.join("log.md");
    let timestamp = Utc::now().format("%Y-%m-%d %H:%M UTC");
    let log_entry = format!(
        "\n## {timestamp} — Collapsed\n\n{}\n\nCollapsed at step {}/{}.\n",
        args.reason, updated.current_step, updated.total_steps
    );
    let existing_log = fs::read_to_string(&log_path).unwrap_or_default();
    let new_log = if existing_log.is_empty() {
        format!("# Evolution Log\n{log_entry}")
    } else {
        format!("{existing_log}{log_entry}")
    };
    let _ = fs::write(&log_path, new_log);

    // Emit legacy events.
    let events_path = ops_dir.join("events.jsonl");
    let _ = cosmon_filestore::event::append(
        &events_path,
        &Envelope::now(Event::MoleculeTransitioned {
            molecule_id: mol_id.clone(),
            from: prev_status,
            to: MoleculeStatus::Collapsed,
        }),
    );
    let _ = cosmon_filestore::event::append(
        &events_path,
        &Envelope::now(Event::MoleculeCollapsed {
            molecule_id: mol_id.clone(),
            reason: args.reason.clone(),
        }),
    );

    // Emit EventV2 records.
    let status_seq = event_log::emit_one(
        &events_path,
        EventV2::MoleculeStatusChanged {
            molecule_id: mol_id.clone(),
            from: prev_status_label.clone(),
            to: "collapsed".to_owned(),
        },
        None,
    )
    .ok();
    let _ = event_log::emit_one(
        &events_path,
        EventV2::MoleculeCollapsed {
            molecule_id: mol_id.clone(),
            reason: args.reason.clone(),
            kind: reason_kind.clone(),
        },
        status_seq,
    );

    // ADR-030 M3 — archive the collapsed molecule when the subsystem is
    // enabled. Non-fatal: any failure is logged and the collapse still
    // succeeds (the caller owns the terminal transition). The
    // `updated.archived` idempotence gate makes re-running `cs collapse`
    // a no-op on the archive — a prior successful write is never
    // clobbered, and collapsing an already-archived molecule never
    // double-counts.
    let config_path = super::resolve_config_from_context(ctx);
    let project_config = cosmon_filestore::load_project_config(&config_path).unwrap_or_default();
    if project_config.archive.enabled && !updated.archived {
        let archive_mol_dir = cosmon_state::archive::resolve_molecule_dir(&ops_dir, &mol_id)
            .unwrap_or_else(|| mol_dir.clone());
        if cosmon_state::archive::write_non_fatal(
            &ops_dir,
            &archive_mol_dir,
            &updated,
            cosmon_state::archive::Trigger::Collapse,
            Utc::now(),
        )
        .is_some()
        {
            updated.archived = true;
            let _ = store.save_molecule(&updated.id.clone(), &updated);
        }
    }

    if ctx.json {
        let out = serde_json::json!({
            "molecule": mol_id.as_str(),
            "previous_status": prev_status_label,
            "status": "collapsed",
            "reason": args.reason,
            "cause": cause.as_ref(),
            "reason_kind": reason_kind.as_ref().map(CollapseReason::as_str),
            "archived": updated.archived,
            "nudge_count": updated.nudge_count,
        });
        println!("{}", serde_json::to_string(&out)?);
    } else {
        let cause_label = cause
            .as_ref()
            .map(|c| format!(" [{c}]"))
            .unwrap_or_default();
        println!(
            "{} {} collapsed (was {}){}: {}",
            MoleculeStatus::Collapsed.emoji(),
            mol_id,
            prev_status_label,
            cause_label,
            args.reason
        );
        // Post-mortem nudge accounting (delib-20260420-1b02 P2):
        // surface how many times `cs patrol --nudge` had to poke this
        // molecule before it collapsed. > 2 nudges suggests an
        // ambiguous briefing rather than a runtime stall — a data
        // point a future audit formula will read.
        if updated.nudge_count > 0 {
            let suffix = if updated.nudge_count == 1 { "" } else { "s" };
            println!(
                "  • nudge_count: {} patrol nudge{suffix}",
                updated.nudge_count
            );
            if updated.nudge_count > 2 {
                println!(
                    "  ⚠ nudge_count > 2 — likely a briefing-clarity issue, not a runtime bug"
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_filestore::FileStore;
    use cosmon_state::{MoleculeData, StateStore};

    use super::Context;
    use super::{run, Args};

    /// Write a project `config.toml` with `[archive] enabled = true`
    /// at the parent of the state dir — i.e. in the `.cosmon/` layout
    /// the CLI walks up to find.
    fn enable_archive_config(state_dir: &std::path::Path) {
        let cosmon_dir = state_dir.parent().unwrap();
        std::fs::write(
            cosmon_dir.join("config.toml"),
            "[project]\nproject_id = \"test-col\"\n\n[archive]\nenabled = true\n",
        )
        .unwrap();
    }

    fn mol(id: &str) -> MoleculeData {
        let now = chrono::Utc::now();
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status: MoleculeStatus::Running,
            variables: HashMap::new(),
            assigned_worker: None,
            created_at: now,
            updated_at: now,
            total_steps: 2,
            current_step: 1,
            completed_steps: vec![],
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: vec![],
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: vec![],
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: BTreeSet::new(),
            escalations: vec![],
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
    fn collapse_archives_and_sets_archived_flag() {
        // Fresh state dir, archive enabled, molecule running. A first
        // `cs collapse` must write the archive entry, flip `archived` to
        // true in state.json, and leave the archive root on disk.
        let tmp = tempfile::tempdir().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        let state_dir = cosmon_dir.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        enable_archive_config(&state_dir);

        let store = FileStore::new(&state_dir);
        let m = mol("task-20260415-col1");
        store.save_molecule(&m.id, &m).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            molecule: "task-20260415-col1".to_owned(),
            reason: "test blocker".to_owned(),
            cause: None,
            account: None,
            kind: None,
            reason_kind: None,
            ops_dir: Some(state_dir.clone()),
        };
        run(&ctx, &args).unwrap();

        let reloaded = store.load_molecule(&m.id).unwrap();
        assert!(
            reloaded.archived,
            "archived flag should be true after first collapse"
        );
        assert_eq!(reloaded.status, MoleculeStatus::Collapsed);

        // Archive dir laid out YYYY/MM/<id>/molecule.json.
        let archive_root = state_dir.join("archive");
        assert!(
            archive_root.is_dir(),
            "archive root should exist: {archive_root:?}"
        );
    }

    #[test]
    fn collapse_is_idempotent_when_archived_flag_true() {
        // When the archived flag is already true on disk (from a prior
        // collapse), re-invoking collapse on the same molecule — after
        // resetting status for the test — must NOT re-write the archive.
        // We check this by pre-populating `archived = true` and a
        // sentinel file inside the archive entry; the sentinel must
        // survive a second `cs collapse`.
        let tmp = tempfile::tempdir().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        let state_dir = cosmon_dir.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        enable_archive_config(&state_dir);

        let store = FileStore::new(&state_dir);
        let mut m = mol("task-20260415-col2");
        m.archived = true;
        store.save_molecule(&m.id, &m).unwrap();

        // Ensure the archive dir is absent — the write is skipped by the
        // idempotence gate, so after `cs collapse` runs the archive must
        // still be absent (cs collapse never creates state_dir/archive/
        // when archived is already true).
        let archive_root = state_dir.join("archive");
        assert!(!archive_root.exists());

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            molecule: "task-20260415-col2".to_owned(),
            reason: "replay".to_owned(),
            cause: None,
            account: None,
            kind: None,
            reason_kind: None,
            ops_dir: Some(state_dir.clone()),
        };
        run(&ctx, &args).unwrap();

        assert!(
            !archive_root.exists(),
            "archive root must not be created when archived flag is already true"
        );
    }

    #[test]
    fn collapse_skips_archive_when_disabled() {
        // [archive] enabled = false (default). collapse must not touch
        // the archive directory even for a brand-new molecule.
        let tmp = tempfile::tempdir().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        let state_dir = cosmon_dir.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(
            cosmon_dir.join("config.toml"),
            "[project]\nproject_id = \"test-col\"\n",
        )
        .unwrap();

        let store = FileStore::new(&state_dir);
        let m = mol("task-20260415-col3");
        store.save_molecule(&m.id, &m).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            molecule: "task-20260415-col3".to_owned(),
            reason: "noop".to_owned(),
            cause: None,
            account: None,
            kind: None,
            reason_kind: None,
            ops_dir: Some(state_dir.clone()),
        };
        run(&ctx, &args).unwrap();

        let archive_root = state_dir.join("archive");
        assert!(
            !archive_root.exists(),
            "archive disabled — archive root must not exist"
        );
        let reloaded = store.load_molecule(&m.id).unwrap();
        assert!(!reloaded.archived);
    }

    #[test]
    fn collapse_records_rate_limit_cause_with_account_and_kind() {
        // ADR-062 K3 fixture replay: cs collapse --cause rate_limit
        // --account you --kind max_rolling_5h must persist the
        // structured CollapseCause so cs peek can surface
        // GhostKind::QuotaExhausted.
        use cosmon_core::molecule::CollapseCause;

        let tmp = tempfile::tempdir().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        let state_dir = cosmon_dir.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(
            cosmon_dir.join("config.toml"),
            "[project]\nproject_id = \"test-col\"\n",
        )
        .unwrap();

        let store = FileStore::new(&state_dir);
        let m = mol("task-20260421-k3xx");
        store.save_molecule(&m.id, &m).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            molecule: "task-20260421-k3xx".to_owned(),
            reason: "Claude usage limit reached".to_owned(),
            cause: Some("rate_limit".to_owned()),
            account: Some("you".to_owned()),
            kind: Some("max_rolling_5h".to_owned()),
            reason_kind: None,
            ops_dir: Some(state_dir.clone()),
        };
        run(&ctx, &args).unwrap();

        let reloaded = store.load_molecule(&m.id).unwrap();
        assert_eq!(reloaded.status, MoleculeStatus::Collapsed);
        match reloaded.collapse_cause {
            Some(CollapseCause::RateLimit {
                account,
                kind_quota,
            }) => {
                assert_eq!(account.as_deref(), Some("you"));
                assert_eq!(kind_quota.as_deref(), Some("max_rolling_5h"));
            }
            other => panic!("expected RateLimit cause, got {other:?}"),
        }
    }

    #[test]
    fn collapse_preserves_nudge_count_for_post_mortem() {
        // delib-20260420-1b02 P2: a molecule that was nudged before
        // collapsing must carry that count through to `cs collapse`'s
        // output so a future audit formula can read it. The state-side
        // expectation is simply that `nudge_count` round-trips
        // unchanged across the collapse transition (the human/JSON
        // formatting is verified by the snapshot harness).
        let tmp = tempfile::tempdir().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        let state_dir = cosmon_dir.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(
            cosmon_dir.join("config.toml"),
            "[project]\nproject_id = \"test-col\"\n",
        )
        .unwrap();

        let store = FileStore::new(&state_dir);
        let mut m = mol("task-20260420-nudg");
        m.nudge_count = 3;
        m.last_nudged_at = Some(chrono::Utc::now());
        store.save_molecule(&m.id, &m).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            molecule: "task-20260420-nudg".to_owned(),
            reason: "ambiguous briefing".to_owned(),
            cause: None,
            account: None,
            kind: None,
            reason_kind: None,
            ops_dir: Some(state_dir.clone()),
        };
        run(&ctx, &args).unwrap();

        let reloaded = store.load_molecule(&m.id).unwrap();
        assert_eq!(reloaded.status, MoleculeStatus::Collapsed);
        assert_eq!(
            reloaded.nudge_count, 3,
            "collapse must preserve nudge_count for post-mortem audit"
        );
    }

    #[test]
    fn collapse_rejects_account_without_rate_limit_cause() {
        // --account / --kind only make sense with --cause rate_limit.
        // Anything else is a user error caught at parse time.
        let tmp = tempfile::tempdir().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        let state_dir = cosmon_dir.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(
            cosmon_dir.join("config.toml"),
            "[project]\nproject_id = \"test-col\"\n",
        )
        .unwrap();

        let store = FileStore::new(&state_dir);
        let m = mol("task-20260421-k3yy");
        store.save_molecule(&m.id, &m).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            molecule: "task-20260421-k3yy".to_owned(),
            reason: "stalled".to_owned(),
            cause: Some("manual".to_owned()),
            account: Some("you".to_owned()),
            kind: None,
            reason_kind: None,
            ops_dir: Some(state_dir.clone()),
        };
        let err = run(&ctx, &args).unwrap_err();
        assert!(
            err.to_string().contains("--account / --kind"),
            "expected mismatch error, got: {err}"
        );
    }
}
