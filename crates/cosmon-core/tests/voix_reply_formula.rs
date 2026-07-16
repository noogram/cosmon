// SPDX-License-Identifier: AGPL-3.0-only

//! Parser-and-shape test for the `voix-reply` formula.
//!
//! The `voix-reply` formula is the VOIX organ of cosmon-incarné v0:
//! draft-only, email-only, permit-gated outbound. The formula carries
//! load-bearing discipline that must not be silently refactored away:
//!
//! * **Three steps** in order — `draft` → `permit` → `send`. Skipping
//!   `permit` would let the worker send without a ledger row; merging
//!   `send` into `permit` would let the worker auto-send without the
//!   operator's review gesture (karpathy `ADR-NEXT-embargo-structural`).
//! * **Worker never auto-sends** — the inviolable invariant the panel
//!   converged on unanimously. Step 3 must require an
//!   `operator-approved.flag` whose mtime is newer than the draft's.
//! * **Permission gate** — `.cosmon/state/voix/permissions.ndjson`
//!   schema must be documented in the formula description (the file is
//!   operator-appended only; the formula must declare that it never
//!   writes to it). The four failure modes — no-row, expired,
//!   scope-mismatch, outside-kitchen-window — must each be named
//!   explicitly so the contract is reviewable from the formula alone.
//! * **Embargo-before-send** — confidentiality folded into the draft
//!   body BEFORE the operator reads it, never as a postscript. Drawn
//!   from chronicle 2026-04-27 (the embargo pivot) and promoted from
//!   prose discipline to formula invariant.
//! * **Kitchen-window guard** — `~/.config/cosmon/voix.toml` declares
//!   `send_window`, fail-open when absent (godin's invariant).
//! * **Channel restriction (v0)** — only `email` works end-to-end;
//!   iMessage / `WhatsApp` / Signal must `cs stuck
//!   "voix-channel-not-implemented"` until mailroom ships the
//!   wrappers.
//! * **Sent-ledger initiator** — `Operator`, never the formula. This
//!   honors wheeler's §-causal-attribution invariant.
//!
//! These are spec-as-test assertions: the test fails loudly if the
//! formula drifts away from the discipline articulated above.

use cosmon_core::formula::Formula;

const VOIX_TOML: &str = include_str!("../../../.cosmon/formulas/voix-reply.formula.toml");

/// Build a lowercased, whitespace-collapsed haystack from the full
/// formula text (top-level description + every step description).
/// Multi-line TOML strings preserve newlines, which would otherwise
/// break naive `contains()` checks for multi-word phrases that wrap.
fn formula_haystack(formula: &Formula) -> String {
    let mut buf = String::new();
    buf.push_str(&formula.description);
    buf.push('\n');
    for step in &formula.steps {
        buf.push_str(&step.description);
        buf.push('\n');
        buf.push_str(step.exit_criteria.as_deref().unwrap_or(""));
        buf.push('\n');
    }
    // Collapse all whitespace runs to a single space so phrases that
    // wrap across lines in the formula source still match.
    let collapsed: String = buf.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.to_lowercase()
}

#[test]
fn formula_parses_with_three_step_shape() {
    let formula = Formula::parse(VOIX_TOML).expect("voix-reply formula must parse");
    assert_eq!(formula.name.as_str(), "voix-reply");
    assert_eq!(formula.id_prefix, "voix");
    assert!(
        !formula.freeze_on_last_step,
        "voix-reply completes after a successful send — must not freeze \
         (a frozen molecule cannot be `cs done`d cleanly)"
    );

    let step_ids: Vec<_> = formula.steps.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        step_ids,
        vec!["draft", "permit", "send"],
        "step shape is load-bearing — draft writes voix-draft.md, \
         permit gates on permissions.ndjson + kitchen window, send \
         only fires on operator approval"
    );
}

#[test]
fn formula_is_tier_zero_no_self_nucleation() {
    let formula = Formula::parse(VOIX_TOML).expect("parse");
    // Tier 0: voix-reply is always downstream of an operator gesture
    // (the verdict-door of peau-morning-digest). It does not nucleate
    // children. A Tier 1 voix-reply would risk an auto-reply loop the
    // permission ledger could not fully prevent.
    assert_eq!(
        formula.tier.level(),
        0,
        "voix-reply must be Tier 0 — downstream of an operator gesture, \
         never upstream (parent synthesis §5.2 step 3)"
    );
}

#[test]
fn step_dependencies_chain_linearly() {
    let formula = Formula::parse(VOIX_TOML).expect("parse");
    let draft = formula
        .steps
        .iter()
        .find(|s| s.id == "draft")
        .expect("draft");
    let permit = formula
        .steps
        .iter()
        .find(|s| s.id == "permit")
        .expect("permit");
    let send = formula.steps.iter().find(|s| s.id == "send").expect("send");

    assert!(draft.depends_on.is_empty(), "draft has no predecessors");
    assert_eq!(
        permit
            .depends_on
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["draft"]
    );
    assert_eq!(
        send.depends_on
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["permit"]
    );
}

#[test]
fn formula_declares_worker_never_auto_sends() {
    let formula = Formula::parse(VOIX_TOML).expect("parse");
    let lowered = formula_haystack(&formula);

    // The load-bearing invariant — the entire reason VOIX ships as
    // draft-only in v0. Must be named explicitly so any future PR that
    // tries to soften it fails this assertion.
    assert!(
        lowered.contains("worker never auto-sends") || lowered.contains("never auto-sends"),
        "formula must declare the WORKER NEVER AUTO-SENDS invariant \
         (parent synthesis §5.4, karpathy ADR-NEXT-embargo-structural)"
    );

    // The mechanism: operator-approved flag with mtime newer than the
    // draft. Without this check, a stale flag from a prior draft
    // revision would silently approve a fresh draft.
    let send_step = formula
        .steps
        .iter()
        .find(|s| s.id == "send")
        .expect("send step exists");
    let send_lower = send_step.description.to_lowercase();
    assert!(
        send_lower.contains("operator-approved.flag"),
        "step 3 (send) must require operator-approved.flag — the load-\
         bearing gate against worker auto-send"
    );
    assert!(
        send_lower.contains("mtime"),
        "step 3 must check the flag's mtime against the draft's \
         (stale flag from a prior draft must not silently approve a \
         fresh draft)"
    );
}

#[test]
fn formula_documents_permission_ledger_schema() {
    let formula = Formula::parse(VOIX_TOML).expect("parse");
    let lowered = formula_haystack(&formula);

    // The permission ledger is the data plane of the gate. Its schema
    // MUST be in the formula description so the contract is reviewable
    // without leaving the file.
    for field in [
        "recipient",
        "channel",
        "scope",
        "granted_until",
        "granted_by",
    ] {
        assert!(
            lowered.contains(field),
            "permission row field `{field}` missing from formula \
             description — the operator cannot append a valid row \
             without knowing the schema"
        );
    }

    // The four failure modes must each be named, so the audit
    // ledger's `result` enum is reviewable from the formula.
    let permit_step = formula
        .steps
        .iter()
        .find(|s| s.id == "permit")
        .expect("permit step exists");
    let permit_lower = permit_step.description.to_lowercase();
    for failure_mode in [
        "no-row",         // (a) no matching row
        "expired",        // (b) granted_until < now
        "scope-mismatch", // (c) intent not in scope
        "outside-window", // kitchen-window guard rejection
        "permit-ok",      // success result
    ] {
        assert!(
            permit_lower.contains(failure_mode),
            "permit step must name audit result `{failure_mode}` — \
             the operator reads audit.ndjson at breakfast and must \
             recognize the result enum from the formula"
        );
    }
}

#[test]
fn formula_documents_embargo_before_send_discipline() {
    let formula = Formula::parse(VOIX_TOML).expect("parse");
    let lowered = formula_haystack(&formula);

    // Embargo-before-send was promoted from a CLAUDE.md prose
    // discipline (chronicle 2026-04-27, embargo pivot) to a VOIX
    // invariant (parent synthesis §5.4, kahneman). The structural
    // form: the draft file carries the embargo at creation, never
    // as a follow-up message.
    assert!(
        lowered.contains("embargo-before-send") || lowered.contains("embargo before send"),
        "formula must declare the EMBARGO-BEFORE-SEND invariant \
         (chronicle 2026-04-27, parent synthesis §5.4)"
    );

    let draft_step = formula
        .steps
        .iter()
        .find(|s| s.id == "draft")
        .expect("draft step exists");
    let draft_lower = draft_step.description.to_lowercase();
    assert!(
        draft_lower.contains("embargo"),
        "step 1 (draft) must handle the embargo field — the draft \
         file MUST carry the confidentiality constraint at creation, \
         folded into the body"
    );
    assert!(
        draft_lower.contains("before") && draft_lower.contains("read"),
        "step 1 must explicitly fold embargo into the draft BEFORE \
         the operator reads — never as a postscript"
    );
}

#[test]
fn formula_documents_kitchen_window_guard() {
    let formula = Formula::parse(VOIX_TOML).expect("parse");
    let lowered = formula_haystack(&formula);

    // godin's invariant — cosmon does not speak outside the operator's
    // kitchen window. Config path is operator-owned, fail-open when
    // absent.
    assert!(
        lowered.contains("kitchen-window") || lowered.contains("kitchen window"),
        "formula must declare the KITCHEN-WINDOW GUARD invariant \
         (godin, parent synthesis §5.4)"
    );
    assert!(
        lowered.contains("~/.config/cosmon/voix.toml"),
        "formula must reference the operator-owned config path \
         `~/.config/cosmon/voix.toml` so the operator can find it \
         to opt in"
    );
    assert!(
        lowered.contains("send_window"),
        "formula must declare the `send_window` config key — without \
         it the operator cannot enable the guard"
    );
}

#[test]
fn formula_documents_channel_restriction_v0() {
    let formula = Formula::parse(VOIX_TOML).expect("parse");
    let lowered = formula_haystack(&formula);

    // v0 implements `email` only. Non-email channels must refuse at
    // send time, NOT at draft/permit time (so permissions can be
    // pre-staged for the eventual mailroom wrappers).
    assert!(
        lowered.contains("voix-channel-not-implemented"),
        "formula must declare the `voix-channel-not-implemented` \
         refusal — non-email channels are deferred to Rung 3 / \
         mailroom MCP wrappers"
    );
    assert!(
        lowered.contains("sec_send_email"),
        "formula must reference `sec_send_email` — the only channel \
         wired in v0"
    );

    let send_step = formula
        .steps
        .iter()
        .find(|s| s.id == "send")
        .expect("send step exists");
    let send_lower = send_step.description.to_lowercase();
    for channel in ["imessage", "whatsapp", "signal"] {
        assert!(
            send_lower.contains(channel),
            "step 3 must explicitly name the deferred channel \
             `{channel}` so the operator knows what is and isn't \
             implemented"
        );
    }
}

#[test]
fn formula_documents_sent_ledger_initiator_operator() {
    let formula = Formula::parse(VOIX_TOML).expect("parse");
    let lowered = formula_haystack(&formula);

    // wheeler's §-causal-attribution invariant — the sent ledger's
    // `initiator` field is hard-coded `Operator` because the
    // operator's `cs evolve` + `operator-approved.flag` is what
    // triggered the send. The formula is NEVER the initiator for
    // VOIX. A future PR that sets `initiator: "Worker"` would break
    // the causal trace.
    assert!(
        lowered.contains("initiator: \"operator\"") || lowered.contains("initiator: operator"),
        "sent.ndjson rows MUST carry `initiator: Operator` — wheeler's \
         §-causal-attribution invariant (parent synthesis §5.4)"
    );
    assert!(
        lowered.contains("sent.ndjson"),
        "formula must declare the sent-ledger path — the operator \
         reads sent.ndjson to audit outbound traffic"
    );
}

#[test]
fn formula_documents_kill_switch_first_silence() {
    let formula = Formula::parse(VOIX_TOML).expect("parse");
    let lowered = formula_haystack(&formula);

    assert!(
        lowered.contains("autopilot.off"),
        "formula must check `~/.cosmon/autopilot.off` at each step \
         start — kill-switch generality"
    );

    // godin's generosity-preserving order: VOIX is silenced FIRST
    // (then CŒUR, then PEAU). The formula must declare its position
    // in the silencing order so the operator's mental model is
    // accurate.
    assert!(
        lowered.contains("first organ silenced") || lowered.contains("first silenced"),
        "formula must declare VOIX as the FIRST organ silenced by \
         the kill-switch (godin's generosity-preserving order, \
         parent synthesis §5.4)"
    );
}

#[test]
fn formula_documents_typed_provenance_sparked_by() {
    let formula = Formula::parse(VOIX_TOML).expect("parse");
    let lowered = formula_haystack(&formula);

    // godel's §8m — every durable file read into a worker prompt
    // (here: the parent tile + pre-draft) must carry an explicit
    // SparkedBy / InformedBy link in the receiving molecule. This
    // catches the PEAU+HIPPOCAMPE+VISAGE joint-invariant violation
    // before VISAGE worker-read ships.
    assert!(
        lowered.contains("sparkedby") || lowered.contains("§8m"),
        "formula must declare the SparkedBy link to the parent tile \
         (godel §8m typed-provenance, parent synthesis §5.4)"
    );
    assert!(
        lowered.contains("provenance.ndjson"),
        "formula must write provenance.ndjson with the SparkedBy row \
         so the causal trace from incoming signal → tile → draft → \
         send is queryable"
    );
}

#[test]
fn formula_required_vars_present() {
    let formula = Formula::parse(VOIX_TOML).expect("parse");

    // Required vars to nucleate a voix-reply: which tile, which day,
    // which recipient, which channel.
    for required in ["tile_id", "morning_date", "recipient", "channel"] {
        let var = formula.variables.get(required).unwrap_or_else(|| {
            panic!(
                "formula must declare variable `{required}` — the \
                 parent verdict-door of peau-morning-digest sets all \
                 four when nucleating the child voix-reply"
            )
        });
        assert!(
            var.required,
            "variable `{required}` must be required (no default) — \
             the parent flow always supplies it"
        );
    }

    // Optional vars — declared so the contract is explicit, with
    // sensible defaults.
    for optional in ["intent", "subject"] {
        assert!(
            formula.variables.contains_key(optional),
            "formula must declare optional variable `{optional}` so \
             the contract is reviewable from the formula alone"
        );
    }
}
