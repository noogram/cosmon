// SPDX-License-Identifier: AGPL-3.0-only

//! Visual snapshot tests — lock the exact rendering of every fleet-row
//! state so a silent regression in the pastille cannot hide behind a
//! passing unit test.
//!
//! # Why snapshots
//!
//! Three visual regressions shipped on 2026-04-19:
//!
//! 1. `Stalled` heartbeat used to repaint the
//!    pastille red, lying about a live tmux with no recent output.
//! 2. `temp:hot` leaked into the ♥ column as 🔥,
//!    duplicating the T-column glyph.
//! 3. the orthogonality fix wiped the 💤 glyph
//!    from `Idle`, leaving every pending row invisible.
//!
//! Each regression got a hand-written assertion test. Those tests catch
//! the known case but miss a 4th bug that shifts some other cell of the
//! matrix (a new glyph added without updating `ratatui_style`, a colour
//! drifts from `LightRed` to `Red`, the plaintext label grows a trailing
//! space, …). Snapshot tests pin the **full matrix** so every drift
//! surfaces as a diff on `cargo insta review`.

use cosmon_cli::visual::{classify, temp_token, whisper_token, RowInputs, RowKind};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_observability::HeartbeatTier;
use ratatui::style::Style;

fn fmt_style(style: Style) -> String {
    let fg = style
        .fg
        .map_or_else(|| "-".to_owned(), |c| format!("{c:?}"));
    let m = style.add_modifier;
    let bits = if m.is_empty() {
        "NONE".to_owned()
    } else {
        format!("{m:?}")
    };
    format!("fg={fg} mod={bits}")
}

fn fmt_heartbeat(hb: Option<HeartbeatTier>) -> &'static str {
    match hb {
        None => "none",
        Some(HeartbeatTier::Active) => "Active",
        Some(HeartbeatTier::Idle) => "Idle",
        Some(HeartbeatTier::Quiet) => "Quiet",
        Some(HeartbeatTier::Stalled) => "Stalled",
        Some(HeartbeatTier::Orphaned) => "Orphaned",
    }
}

fn fmt_status(s: MoleculeStatus) -> &'static str {
    match s {
        MoleculeStatus::Pending => "pending",
        MoleculeStatus::Queued => "queued",
        MoleculeStatus::Running => "running",
        MoleculeStatus::Frozen => "frozen",
        MoleculeStatus::Starved => "starved",
        MoleculeStatus::Completed => "completed",
        MoleculeStatus::Collapsed => "collapsed",
        _ => "unknown",
    }
}

fn render_row(inputs: &RowInputs<'_>, whisper_fresh: bool) -> String {
    let kind = classify(inputs);
    let (temp_g, temp_s) = temp_token(inputs.tags);
    let (whi_g, whi_s) = whisper_token(whisper_fresh);
    let tag_list = if inputs.tags.is_empty() {
        "(none)".to_owned()
    } else {
        inputs.tags.join(",")
    };
    format!(
        "\
INPUTS
  status:        {status}
  heartbeat:     {hb}
  has_blockers:  {blk}
  ghost:         {ghost}
  drift:         {drift}
  tags:          {tags}
  whisper_fresh: {fresh}

RENDER
  row_kind:       {rk:?}
  label:          {label}
  lifecycle_glyph {lg}   style={ls}
  temp_glyph      {tg}   style={ts}
  whisper_glyph   {wg}   style={ws}

STRIP (what the operator sees)
  [{lg}] [{tg}] [{wg}]
",
        status = fmt_status(inputs.status),
        hb = fmt_heartbeat(inputs.heartbeat),
        blk = inputs.has_blockers,
        ghost = inputs.ghost,
        drift = inputs.drift,
        tags = tag_list,
        fresh = whisper_fresh,
        rk = kind,
        label = kind.label(),
        lg = kind.glyph(),
        ls = fmt_style(kind.ratatui_style()),
        tg = temp_g,
        ts = fmt_style(temp_s),
        wg = whi_g,
        ws = fmt_style(whi_s),
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

// ── Lifecycle × heartbeat snapshots ────────────────────────────────────

#[test]
fn peek_row_healthy_active_no_tag_stale() {
    let i = inputs_for(
        MoleculeStatus::Running,
        Some(HeartbeatTier::Active),
        false,
        &[],
    );
    insta::assert_snapshot!(render_row(&i, false));
}

#[test]
fn peek_row_healthy_idle_hb_no_tag_stale() {
    let i = inputs_for(
        MoleculeStatus::Running,
        Some(HeartbeatTier::Idle),
        false,
        &[],
    );
    insta::assert_snapshot!(render_row(&i, false));
}

#[test]
fn peek_row_healthy_quiet_hb_no_tag_fresh() {
    let i = inputs_for(
        MoleculeStatus::Running,
        Some(HeartbeatTier::Quiet),
        false,
        &[],
    );
    insta::assert_snapshot!(render_row(&i, true));
}

#[test]
fn peek_row_healthy_stalled_hb_stays_green() {
    // Regression for task-20260419-ad9c — Stalled must render green ♥.
    let i = inputs_for(
        MoleculeStatus::Running,
        Some(HeartbeatTier::Stalled),
        false,
        &[],
    );
    insta::assert_snapshot!(render_row(&i, false));
}

#[test]
fn peek_row_healthy_orphaned_becomes_ghost() {
    let i = inputs_for(
        MoleculeStatus::Running,
        Some(HeartbeatTier::Orphaned),
        false,
        &[],
    );
    insta::assert_snapshot!(render_row(&i, false));
}

// ── Pending matrix (Idle, Blocked) — the orthogonality charter ─────────

#[test]
fn peek_row_idle_pending_no_tag_stale() {
    let i = inputs_for(MoleculeStatus::Pending, None, false, &[]);
    insta::assert_snapshot!(render_row(&i, false));
}

#[test]
fn peek_row_idle_pending_no_tag_fresh() {
    let i = inputs_for(MoleculeStatus::Pending, None, false, &[]);
    insta::assert_snapshot!(render_row(&i, true));
}

#[test]
fn peek_row_idle_temp_hot_no_blocker() {
    // Regression for task-20260419-39d2 — "La flamme qui doublait".
    let tags = ["temp:hot".to_string()];
    let i = inputs_for(MoleculeStatus::Pending, None, false, &tags);
    insta::assert_snapshot!(render_row(&i, false));
}

#[test]
fn peek_row_idle_temp_warm() {
    let tags = ["temp:warm".to_string()];
    let i = inputs_for(MoleculeStatus::Pending, None, false, &tags);
    insta::assert_snapshot!(render_row(&i, false));
}

#[test]
fn peek_row_idle_temp_cold() {
    let tags = ["temp:cold".to_string()];
    let i = inputs_for(MoleculeStatus::Pending, None, false, &tags);
    insta::assert_snapshot!(render_row(&i, false));
}

#[test]
fn peek_row_idle_temp_frozen_tag() {
    let tags = ["temp:frozen".to_string()];
    let i = inputs_for(MoleculeStatus::Pending, None, false, &tags);
    insta::assert_snapshot!(render_row(&i, false));
}

#[test]
fn peek_row_blocked_pending_no_tag() {
    let i = inputs_for(MoleculeStatus::Pending, None, true, &[]);
    insta::assert_snapshot!(render_row(&i, false));
}

#[test]
fn peek_row_blocked_pending_temp_hot() {
    let tags = ["temp:hot".to_string()];
    let i = inputs_for(MoleculeStatus::Pending, None, true, &tags);
    insta::assert_snapshot!(render_row(&i, false));
}

#[test]
fn peek_row_blocked_pending_temp_warm() {
    let tags = ["temp:warm".to_string()];
    let i = inputs_for(MoleculeStatus::Pending, None, true, &tags);
    insta::assert_snapshot!(render_row(&i, false));
}

// ── Queued ────────────────────────────────────────────────────────────

#[test]
fn peek_row_queued_unblocked() {
    let i = inputs_for(MoleculeStatus::Queued, None, false, &[]);
    insta::assert_snapshot!(render_row(&i, false));
}

#[test]
fn peek_row_queued_blocked() {
    let i = inputs_for(MoleculeStatus::Queued, None, true, &[]);
    insta::assert_snapshot!(render_row(&i, false));
}

// ── Frozen ────────────────────────────────────────────────────────────

#[test]
fn peek_row_frozen_no_worker() {
    let i = inputs_for(MoleculeStatus::Frozen, None, false, &[]);
    insta::assert_snapshot!(render_row(&i, false));
}

#[test]
fn peek_row_frozen_with_live_worker_stays_frozen() {
    let i = inputs_for(
        MoleculeStatus::Frozen,
        Some(HeartbeatTier::Active),
        false,
        &[],
    );
    insta::assert_snapshot!(render_row(&i, false));
}

#[test]
fn peek_row_frozen_with_temp_hot() {
    let tags = ["temp:hot".to_string()];
    let i = inputs_for(MoleculeStatus::Frozen, None, false, &tags);
    insta::assert_snapshot!(render_row(&i, false));
}

// ── Ghost / Drift ─────────────────────────────────────────────────────

#[test]
fn peek_row_ghost_flag_even_with_active_heartbeat() {
    let mut i = inputs_for(
        MoleculeStatus::Running,
        Some(HeartbeatTier::Active),
        false,
        &[],
    );
    i.ghost = true;
    insta::assert_snapshot!(render_row(&i, false));
}

#[test]
fn peek_row_drift_wins_over_every_other_axis() {
    let tags = ["temp:hot".to_string()];
    let mut i = inputs_for(
        MoleculeStatus::Frozen,
        Some(HeartbeatTier::Active),
        true,
        &tags,
    );
    i.ghost = true;
    i.drift = true;
    insta::assert_snapshot!(render_row(&i, true));
}

// ── Terminal states ───────────────────────────────────────────────────

#[test]
fn peek_row_completed() {
    let i = inputs_for(MoleculeStatus::Completed, None, false, &[]);
    insta::assert_snapshot!(render_row(&i, false));
}

#[test]
fn peek_row_collapsed() {
    let i = inputs_for(MoleculeStatus::Collapsed, None, false, &[]);
    insta::assert_snapshot!(render_row(&i, false));
}

#[test]
fn peek_row_completed_with_temp_hot_is_terminal() {
    let tags = ["temp:hot".to_string()];
    let i = inputs_for(MoleculeStatus::Completed, None, false, &tags);
    insta::assert_snapshot!(render_row(&i, false));
}

// ── Whisper column ────────────────────────────────────────────────────

#[test]
fn peek_row_healthy_whisper_fresh_bubble() {
    let i = inputs_for(
        MoleculeStatus::Running,
        Some(HeartbeatTier::Active),
        false,
        &[],
    );
    insta::assert_snapshot!(render_row(&i, true));
}

#[test]
fn peek_row_blocked_whisper_fresh_bubble() {
    let i = inputs_for(MoleculeStatus::Pending, None, true, &[]);
    insta::assert_snapshot!(render_row(&i, true));
}

#[test]
fn peek_row_frozen_whisper_fresh_bubble() {
    let i = inputs_for(MoleculeStatus::Frozen, None, false, &[]);
    insta::assert_snapshot!(render_row(&i, true));
}

// ── Per-RowKind glyph/style pins ──────────────────────────────────────

fn render_kind_card(kind: RowKind) -> String {
    format!(
        "variant={kind:?}\nlabel={label}\nglyph={glyph}\nratatui_style={style}\nneeds_attention={atn}\n",
        label = kind.label(),
        glyph = kind.glyph(),
        style = fmt_style(kind.ratatui_style()),
        atn = kind.needs_attention(),
    )
}

#[test]
fn peek_kind_card_healthy() {
    insta::assert_snapshot!(render_kind_card(RowKind::Healthy));
}

#[test]
fn peek_kind_card_blocked() {
    insta::assert_snapshot!(render_kind_card(RowKind::Blocked));
}

#[test]
fn peek_kind_card_frozen() {
    insta::assert_snapshot!(render_kind_card(RowKind::Frozen));
}

#[test]
fn peek_kind_card_ghost() {
    insta::assert_snapshot!(render_kind_card(RowKind::Ghost));
}

#[test]
fn peek_kind_card_drift() {
    insta::assert_snapshot!(render_kind_card(RowKind::Drift));
}

#[test]
fn peek_kind_card_terminal() {
    insta::assert_snapshot!(render_kind_card(RowKind::Terminal));
}

#[test]
fn peek_kind_card_idle_is_zzz() {
    // Regression for task-20260419-3c30 — Idle must render 💤.
    insta::assert_snapshot!(render_kind_card(RowKind::Idle));
}

#[test]
fn peek_kind_card_parked_deprecated() {
    insta::assert_snapshot!(render_kind_card(RowKind::Parked));
}

#[test]
fn peek_kind_card_hot_deprecated() {
    insta::assert_snapshot!(render_kind_card(RowKind::Hot));
}
