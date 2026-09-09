// SPDX-License-Identifier: AGPL-3.0-only

//! The pilot-lease authority guard on a lifecycle gesture (ADR-168 §D6).
//!
//! Five verbs reach this guard — `cs evolve`, `cs complete`, `cs collapse`,
//! `cs tackle` and `cs done` — and the harvest is the one of the five that a
//! second caller (the RPP route's library harvest effect) also performs. That
//! is why the guard lives here rather than in the CLI: a gate the route
//! skipped would be a second door, which is the drift ADR-176 §11 refuses.
//!
//! The refusal type is this module's own [`UnleasedPilotGesture`];
//! `cosmon_cli::cmd::guard::GuardError` wraps it transparently so the CLI's
//! exit-code table keeps naming it, and its message is written once, here.

use cosmon_core::id::MoleculeId;
use cosmon_filestore::{MinisignOperatorVerifier, PilotLeaseStore};

/// The lease ledger over an explicit state root, trust root attached.
///
/// It exists because the guard once built `PilotLeaseStore::new` directly,
/// and a store with no pinned key honours no grant — so every leased mission
/// read back as *unleased* and the guard returned `Ok(())` for callers the
/// ledger refused. The M8 relève exercise caught it by collapsing a leased
/// mission from an unleased co-pilot while `cs sessions takeover check`,
/// reading the same ledger through `cs presence`'s own accessor, refused the
/// very same session.
///
/// Both readers come through here, which is the point: this is one function
/// so that "resolve the trust root" is not a step a call site can perform
/// differently, or forget.
///
/// # Errors
///
/// When the state root declares a trust root that cannot be read or parsed.
pub fn leases_at(state_root: &std::path::Path) -> anyhow::Result<PilotLeaseStore> {
    let store = PilotLeaseStore::new(state_root);
    Ok(
        match MinisignOperatorVerifier::resolve_for_state_root(state_root)? {
            Some(v) => store.trusting(std::sync::Arc::new(v)),
            None => store,
        },
    )
}

/// A lifecycle gesture refused because the mission is under co-pilotage and
/// the caller does not hold its lease.
///
/// A struct rather than a CLI enum variant so that the message — the only
/// place an operator learns what to do next — is written once and reaches
/// both callers of the harvest unchanged.
#[derive(Debug, thiserror::Error)]
#[error(
    "{gesture} {mol_id}: refusing the gesture — {why}.\n\n\
     This mission is under co-pilotage (ADR-168 §D6): a lease exists, so \
     only the session holding it may issue a lifecycle gesture on it. \
     Read the seat with `cs sessions takeover show --mission {mol_id}`.\n\n\
     A co-pilot is read-only by mechanism, not by discipline. It may still \
     observe, message, publish checkpoints and report findings. To fly \
     this mission, an operator — never the beneficiary — must grant the \
     lease: `cs sessions takeover request --mission {mol_id}` then \
     `cs sessions takeover grant --mission {mol_id} --request <ID>`."
)]
pub struct UnleasedPilotGesture {
    /// The lifecycle verb that was refused, as the operator typed it
    /// (`cs evolve`, `cs done`, …), so the message names the gesture and not
    /// the guard.
    pub gesture: &'static str,
    /// The mission the gesture targeted. A `String` rather than a
    /// `MoleculeId` so that wrapping it in `cosmon_cli`'s `GuardError` does
    /// not push that enum past clippy's `result_large_err` threshold on the
    /// dispatch path.
    pub mol_id: String,
    /// The refusal, in [`cosmon_core::pilot_lease::RefusalReason::explain`]'s
    /// words — one of the five the guard distinguishes, because the next move
    /// differs.
    pub why: String,
}

/// ADR-168 §D6 guard — refuse a lifecycle gesture on a co-piloted mission from
/// a session that does not hold its lease.
///
/// # Why this exists
///
/// The M7 dogfood (`task-20260731-bd92` §8, friction F9) ran a real Codex
/// co-pilot beside a Claude primary and recorded zero mutations by the
/// co-pilot. The zero was true and it was *verified afterwards* — the co-pilot
/// abstained because its brief told it to. The lease guard existed and refused
/// five falsifiers, but every one of those went through
/// `cs sessions takeover check`, and not one of the co-pilot's nine gestures
/// did. Nothing mechanical stood between a co-pilot and `cs evolve`. That is
/// the difference between an invariant and good conduct, and it is what made
/// F9 blocking for the supervised-handover exercise.
///
/// This function is the mechanism. It is the same
/// [`cosmon_core::pilot_lease::authorize`] the five falsifiers already reach,
/// moved onto the path a pilot actually types.
///
/// # When it fires, and when it says nothing
///
/// **Only on a mission that has a lease.** `current(mission) == None` returns
/// `Ok(())` unconditionally, which is every molecule on every fleet today: no
/// co-pilotage, no guard, no behaviour change. The guard switches on exactly
/// when an operator has granted the mission's PRIMARY seat — the one gesture
/// that declares "this mission is being flown by two".
///
/// That scoping is deliberate and is not a weakening of FAIL-CLOSED-AUTHORITY.
/// D6's *"unknown lease ⇒ read-only"* governs who may pilot a **co-piloted**
/// mission; reading it as "no lease ⇒ nobody may run `cs evolve` anywhere"
/// would make the lease ledger a global kill-switch for the fleet, which no
/// ADR asks for and which the M4 note explicitly declines ("the model's rights
/// are unchanged and `cs done` is no more autonomous than it was").
///
/// Inside the perimeter the rule is fail-closed without exception:
///
/// - No resolvable session id ⇒ refused. An anonymous caller on a leased
///   mission is the unknown session of D6's fourth bullet.
/// - A session whose snapshot is absent, corrupt, or claims another mission
///   ⇒ presents no epoch ⇒ refused as `EpochNotPresented`.
/// - Anyone but the holder ⇒ `NotHolder`. A stale epoch ⇒ `EpochMismatch`.
///   An expired lease ⇒ `Expired`, for the holder too.
///
/// # Where the epoch comes from
///
/// D6 says every mutation carries the epoch it believed it held, and a
/// lifecycle verb has no `--epoch` flag to carry one. It does not need one:
/// the pilot's presence snapshot **is** its standing claim, and M4 already
/// checks that claim against the ledger before writing it. So the gesture
/// presents what the seat presents, filtered to this mission by
/// [`cosmon_core::pilot_lease::epoch_presented_for`]. A pilot whose lease was
/// transferred away is refused by its own recorded belief — which is falsifier
/// 3 of ADR-168, made unreachable rather than merely unobserved.
///
/// # Errors
///
/// [`UnleasedPilotGesture`] when the gesture is refused. Store
/// errors propagate as an anyhow error so the CLI's generic path handles a
/// transient I/O hiccup as an error and not as a refusal — a filesystem
/// stumble must not read as "you are a co-pilot".
pub fn refuse_unleased_pilot_gesture<F>(
    state_root: &std::path::Path,
    mission: &MoleculeId,
    gesture: &'static str,
    env_lookup: &F,
) -> anyhow::Result<()>
where
    F: Fn(&str) -> Option<String>,
{
    use cosmon_core::pilot_lease::{authorize, epoch_presented_for, RefusalReason};
    use cosmon_filestore::PresenceStore;

    // Through `leases_at`, never `PilotLeaseStore::new`: a store with
    // no trust root pinned honours no grant, so building one here made every
    // leased mission read back as unleased and turned this whole guard into a
    // no-op. See that function for the exercise that caught it.
    let Some(lease) = leases_at(state_root)?.current(mission)? else {
        return Ok(());
    };

    let refuse = |why: String| -> anyhow::Result<()> {
        Err(UnleasedPilotGesture {
            gesture,
            mol_id: mission.as_str().to_owned(),
            why,
        }
        .into())
    };

    let Some(session) = resolve_pilot_session(env_lookup) else {
        return refuse(format!(
            "this session names no identity, and an unnamed caller holds \
             nothing — the lease is held by {holder}. Export COSMON_SESSION_ID \
             to say who you are",
            holder = lease.holder_session_id.as_str(),
        ));
    };

    let seat = PresenceStore::new(state_root).load(&session)?;
    let presented = epoch_presented_for(
        mission,
        seat.as_ref()
            .and_then(cosmon_core::presence::Presence::claimed_authority),
    );

    match authorize(Some(&lease), chrono::Utc::now(), &session, presented).refusal() {
        None => Ok(()),
        Some(reason) => refuse(RefusalReason::explain(reason)),
    }
}

/// Resolve the session id this `cs` invocation speaks for, or `None`.
///
/// Mirrors the two variables `cs presence` reads, in the same order, so a
/// pilot is the same pilot to the guard as it is to the cockpit. A blank or
/// malformed value is `None` and not an error: the guard's caller turns that
/// into a refusal, which is stricter than an error would be.
fn resolve_pilot_session<F>(env_lookup: &F) -> Option<cosmon_core::id::SessionId>
where
    F: Fn(&str) -> Option<String>,
{
    ["COSMON_SESSION_ID", "CLAUDE_SESSION_ID"]
        .into_iter()
        .filter_map(env_lookup)
        .find(|raw| !raw.trim().is_empty())
        .and_then(|raw| cosmon_core::id::SessionId::new(raw).ok())
}

/// The process environment, as [`refuse_unleased_pilot_gesture`] wants it.
///
/// A named function rather than a closure at each of the five call-sites, so
/// the five cannot drift into reading different variables.
pub fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::id::MoleculeId;

    // -----------------------------------------------------------------
    // ADR-168 §D6 — the authority guard on the lifecycle path.
    //
    // The end-to-end walk lives in
    // `tests/pilot_lease_guards_lifecycle.rs`; these pin the two decisions
    // that are easiest to get backwards and cheapest to check here: when the
    // guard stays silent, and what an anonymous caller is.
    // -----------------------------------------------------------------

    fn mission_id() -> MoleculeId {
        MoleculeId::new("task-20260804-6158").unwrap()
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn a_mission_with_no_lease_is_not_the_guard_s_business() {
        // Every molecule on every fleet, until an operator grants a seat.
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            refuse_unleased_pilot_gesture(tmp.path(), &mission_id(), "cs done", &no_env,).is_ok()
        );
    }

    /// A galaxy with a lease the guard will actually honour: the ledger at
    /// `<root>/.cosmon/state`, the trust root at `<root>/.cosmon/takeover.pub`,
    /// and one grant carrying the operator's signature over its challenge.
    ///
    /// Seeding an *unsigned* grant instead — which this test used to do — makes
    /// the mission read back as unleased, so the guard stays silent and the
    /// refusal being asserted never has to happen. That is the same vacuity
    /// the M8 exercise found in the shipped path.
    fn leased_galaxy(root: &std::path::Path, holder: &str) -> std::path::PathBuf {
        use cosmon_core::id::SessionId;
        use cosmon_core::operator_attestation::GrantChallenge;
        use cosmon_core::pilot_lease::{LeaseEpoch, PilotLease};
        use cosmon_minisign_testkit::Operator;

        let cosmon = root.join(".cosmon");
        let state = cosmon.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let operator = Operator::from_seed(23);
        std::fs::write(cosmon.join("takeover.pub"), operator.public_key_file()).unwrap();

        let holder = SessionId::new(holder.to_owned()).unwrap();
        let challenge = GrantChallenge::new(
            mission_id(),
            holder.clone(),
            LeaseEpoch::first(),
            "operator",
            None,
        )
        .unwrap();
        let signed = cosmon_notary::minisign::MinisignSignature::parse(
            &operator.sign(&challenge.canonical_bytes()),
        )
        .unwrap();
        let attestation = cosmon_core::operator_attestation::OperatorAttestation {
            key_id: cosmon_core::operator_attestation::OperatorKeyId::from_bytes(signed.key_id),
            signature: signed.signature_line(),
            global_signature: signed.global_signature_line(),
            trusted_comment: signed.trusted_comment.clone(),
            untrusted_comment: signed.untrusted_comment.clone(),
        };

        leases_at(&state)
            .unwrap()
            .grant(
                &PilotLease::new(
                    mission_id(),
                    holder,
                    LeaseEpoch::first(),
                    "operator",
                    chrono::Utc::now(),
                    None,
                )
                .attested_by(attestation),
            )
            .unwrap();
        state
    }

    #[test]
    fn an_anonymous_caller_on_a_leased_mission_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let state = leased_galaxy(tmp.path(), "claude-primary");

        // No COSMON_SESSION_ID, no CLAUDE_SESSION_ID: D6's unknown session,
        // which is read-only and not "probably the holder".
        let err = refuse_unleased_pilot_gesture(&state, &mission_id(), "cs done", &no_env)
            .expect_err("an unnamed caller holds nothing");
        let refusal = err
            .downcast_ref::<UnleasedPilotGesture>()
            .expect("the refusal must be typed, so a script can branch on it");
        assert_eq!(refusal.gesture, "cs done");
        assert!(refusal.to_string().contains("claude-primary"));
    }

    #[test]
    fn a_blank_session_variable_is_no_session_at_all() {
        // A worker that exports COSMON_SESSION_ID="" has named nobody. Reading
        // that as a session id would mint a ghost pilot rather than refuse.
        let blank = |k: &str| (k == "COSMON_SESSION_ID").then(|| "   ".to_owned());
        assert!(resolve_pilot_session(&blank).is_none());
    }
}
