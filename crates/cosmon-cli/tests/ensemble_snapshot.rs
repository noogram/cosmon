// SPDX-License-Identifier: AGPL-3.0-only

//! Snapshot tests for the plaintext `cs ensemble` row renderer.

use cosmon_cli::visual::{classify, temp_token, RowInputs};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_observability::HeartbeatTier;

fn strip_ansi(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            i += 2;
            while i < bytes.len() && !bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

fn render_ensemble_row(mol_id: &str, status_label: &str, inputs: &RowInputs<'_>) -> String {
    let kind = classify(inputs);
    let glyph = kind.colorize(kind.glyph()).to_string();
    let label = kind.colorize(status_label).to_string();
    let (temp_g, _temp_s) = temp_token(inputs.tags);
    let mol = kind.colorize(mol_id).to_string();
    let joined = format!("{glyph} {mol} [{temp_g}] {label}");
    format!(
        "row_kind={kind:?}\nraw       = {joined}\nstripped  = {stripped}\n",
        stripped = strip_ansi(&joined),
    )
}

fn inputs_for(
    status: MoleculeStatus,
    hb: Option<HeartbeatTier>,
    has_blockers: bool,
    tags: &[String],
) -> RowInputs<'_> {
    RowInputs {
        status,
        heartbeat: hb,
        tags,
        has_blockers,
        ghost: false,
        drift: false,
    }
}

#[test]
fn ensemble_row_healthy_running() {
    let i = inputs_for(
        MoleculeStatus::Running,
        Some(HeartbeatTier::Active),
        false,
        &[],
    );
    insta::assert_snapshot!(render_ensemble_row("task-abcd", "running", &i));
}

#[test]
fn ensemble_row_healthy_stalled_stays_green() {
    let i = inputs_for(
        MoleculeStatus::Running,
        Some(HeartbeatTier::Stalled),
        false,
        &[],
    );
    insta::assert_snapshot!(render_ensemble_row("task-stll", "running", &i));
}

#[test]
fn ensemble_row_ghost_orphaned() {
    let i = inputs_for(
        MoleculeStatus::Running,
        Some(HeartbeatTier::Orphaned),
        false,
        &[],
    );
    insta::assert_snapshot!(render_ensemble_row("task-ghst", "running", &i));
}

#[test]
fn ensemble_row_idle_pending_untagged() {
    let i = inputs_for(MoleculeStatus::Pending, None, false, &[]);
    insta::assert_snapshot!(render_ensemble_row("task-idle", "pending", &i));
}

#[test]
fn ensemble_row_idle_pending_with_temp_hot() {
    let tags = ["temp:hot".to_string()];
    let i = inputs_for(MoleculeStatus::Pending, None, false, &tags);
    insta::assert_snapshot!(render_ensemble_row("task-hot1", "pending", &i));
}

#[test]
fn ensemble_row_idle_pending_with_temp_warm() {
    let tags = ["temp:warm".to_string()];
    let i = inputs_for(MoleculeStatus::Pending, None, false, &tags);
    insta::assert_snapshot!(render_ensemble_row("task-warm", "pending", &i));
}

#[test]
fn ensemble_row_idle_pending_with_temp_cold() {
    let tags = ["temp:cold".to_string()];
    let i = inputs_for(MoleculeStatus::Pending, None, false, &tags);
    insta::assert_snapshot!(render_ensemble_row("task-cold", "pending", &i));
}

#[test]
fn ensemble_row_idle_pending_with_temp_frozen_tag() {
    let tags = ["temp:frozen".to_string()];
    let i = inputs_for(MoleculeStatus::Pending, None, false, &tags);
    insta::assert_snapshot!(render_ensemble_row("task-fztg", "pending", &i));
}

#[test]
fn ensemble_row_blocked_pending() {
    let i = inputs_for(MoleculeStatus::Pending, None, true, &[]);
    insta::assert_snapshot!(render_ensemble_row("task-blk1", "pending", &i));
}

#[test]
fn ensemble_row_blocked_pending_with_temp_hot() {
    let tags = ["temp:hot".to_string()];
    let i = inputs_for(MoleculeStatus::Pending, None, true, &tags);
    insta::assert_snapshot!(render_ensemble_row("task-blkh", "pending", &i));
}

#[test]
fn ensemble_row_frozen_no_worker() {
    let i = inputs_for(MoleculeStatus::Frozen, None, false, &[]);
    insta::assert_snapshot!(render_ensemble_row("task-fr0w", "frozen", &i));
}

#[test]
fn ensemble_row_frozen_with_live_worker() {
    let i = inputs_for(
        MoleculeStatus::Frozen,
        Some(HeartbeatTier::Active),
        false,
        &[],
    );
    insta::assert_snapshot!(render_ensemble_row("task-frlw", "frozen", &i));
}

#[test]
fn ensemble_row_completed() {
    let i = inputs_for(MoleculeStatus::Completed, None, false, &[]);
    insta::assert_snapshot!(render_ensemble_row("task-done", "completed", &i));
}

#[test]
fn ensemble_row_collapsed() {
    let i = inputs_for(MoleculeStatus::Collapsed, None, false, &[]);
    insta::assert_snapshot!(render_ensemble_row("task-coll", "collapsed", &i));
}

#[test]
fn ensemble_row_drift_alarm() {
    let mut i = inputs_for(
        MoleculeStatus::Frozen,
        Some(HeartbeatTier::Active),
        false,
        &[],
    );
    i.drift = true;
    insta::assert_snapshot!(render_ensemble_row("task-drft", "frozen", &i));
}
