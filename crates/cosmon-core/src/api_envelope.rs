// SPDX-License-Identifier: AGPL-3.0-only

//! The RPP subprocess envelope (ADR-080 §3.5) as seen from *inside* `cs`.
//!
//! # Why this module exists
//!
//! ADR-080 gives the operator-only verb list (§5.1) **two** locks:
//!
//! 1. the adapter refuses to *route* to one
//!    ([`crate::api_envelope::OPERATOR_ONLY_VERBS`] is what
//!    `cosmon_rpp_adapter::admission` matches on), and
//! 2. `cs` itself refuses to *run* one when it can see the envelope
//!    marker `COSMON_API_REQUEST=1` — "at parse time, before any state
//!    mutation" (§3.5, last paragraph).
//!
//! Lock 2 did not exist. Every occurrence of the marker in `crates/` read
//! it to *suppress a `cb` probe* or to *project the exposed egress
//! posture*; none of them refused anything. The announced defence in
//! depth was a defence in surface: one mis-wired route reached `cs done`
//! — worktree removed, branch deleted, `main` moved — with nothing behind
//! the router to say no. This module is the second lock, and it lives in
//! the core so that the two locks read the *same list* rather than two
//! copies of it.
//!
//! # Why the marker is consumed at a local hand-off
//!
//! `COSMON_API_REQUEST=1` means **"this process *is* the network
//! request"**. It does not mean "somewhere upstream there was one". The
//! distinction is load-bearing, because two exposed §8p verbs spawn `cs`
//! again as a *local* gesture of the tenant's own machinery:
//!
//! - `POST /v1/molecules/{id}/run` (ADR-124 bounded drain) runs a
//!   resident loop that calls `cs done` on each completed molecule to
//!   merge and tear down (`cosmon_runtime::…::on_complete`,
//!   `cmd::run`'s auto-teardown);
//! - `POST /v1/molecules/{id}/tackle` spawns a tmux worker whose whole
//!   job is to call `cs evolve` and `cs complete`.
//!
//! Inherited wholesale, the marker would make lock 2 refuse all three —
//! the drain loop could never tear down, and an RPP-tackled worker would
//! die on its first `cs evolve`. So the envelope is **consumed** at those
//! hand-offs ([`hand_off_to_local_child`]): the child is no longer the
//! request, and says so. What is deliberately *not* consumed is any
//! security posture: `COSMON_EGRESS_POLICY` and
//! [`crate::egress::EXPOSED_MULTITENANT_ENV`] are untouched here, and the
//! fail-closed exposed-host refusal that `cs tackle` performs happens
//! *before* the spawn this function configures.
//!
//! Everything here is a pure decision over an injected `lookup`; nothing
//! reads the ambient environment or the filesystem.

use std::fmt;

/// The envelope marker ADR-080 §3.5 requires on every RPP subprocess.
///
/// Named here rather than at the four call sites that used to spell it as
/// a literal, so the producer (`cosmon_rpp_adapter::subprocess`), the
/// refusal, and the hand-off cannot drift apart.
pub const REQUEST_ENV: &str = "COSMON_API_REQUEST";

/// Correlation id of the originating HTTP request (ADR-080 §3.5).
pub const REQUEST_ID_ENV: &str = "COSMON_API_REQUEST_ID";

/// Operator-only verbs the RPP must never reach — ADR-080 §5.1.
///
/// The list is **closed**: extending it requires a successor ADR with a
/// `delegate_for` claim model (§5.2). It is verb-level, one notch wider
/// than §5.1's prose in one place — §5.1 names `cs security activate`
/// and `cs whisper --to-session`, and this list refuses the whole verb.
/// That is deliberate: a subcommand-sensitive list would have to be
/// re-derived identically in the adapter (which sees a route, not an
/// argv) and in `cs` (which sees an argv), and the first divergence
/// between them is a hole. Refusing the verb is the intersection both
/// locks can state exactly.
///
/// `run` left the list on 2026-06-11 via the §5.2 successor path
/// (ADR-124): `POST /v1/molecules/{id}/run` admits a *request* for a
/// bounded drain of the caller's own DAG, not the operator orchestrator.
///
/// `done` left the list on 2026-09-07 by the same §5.2 successor path
/// (issue #51, the ADR-080 §5.1 amendment). The classification was the
/// upstream error the issue's reporters named: closing a molecule is part
/// of its **normal lifecycle**, not an administration surface — whoever may
/// nucleate a molecule, build its worker and run it may legitimately close
/// it, and that the operation has an effect on workers does not make it
/// administration. Restricting *which* molecules a requester may close
/// stays legitimate and is the multi-tenant question, deliberately not
/// answered here; ADR-176 D5 still refuses an `owner` field. What continues
/// to gate the *effect* is the operator's `[harvest_authority]` arming
/// (ADR-176 D1), which this change does not touch.
pub const OPERATOR_ONLY_VERBS: &[&str] = &[
    "evolve",
    "complete",
    "security",
    "kill",
    "purge",
    "reconcile",
    "verify",
    "whisper",
    "drop",
];

/// Exit code of the §3.5 parse-time refusal.
///
/// Distinct from the generic exit-1 so an adapter, a script, or a future
/// route audit can tell "this verb is not exposable" apart from "the verb
/// ran and failed". Registered alongside the CLI's other typed refusals in
/// `cosmon_cli`'s `cmd::guard::exit_code`.
pub const EXIT_OPERATOR_ONLY_VERB_IN_API: i32 = 17;

/// The §3.5 refusal: an operator-only verb was invoked under the RPP
/// subprocess envelope.
///
/// Carries the verb so the message names what was refused; the ADR
/// citation is in the `Display` because the person reading this line is
/// usually a tenant operator with no copy of the ADR open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorOnlyVerbInApi {
    /// The refused verb, as clap names it.
    pub verb: &'static str,
}

impl fmt::Display for OperatorOnlyVerbInApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cs {}: refusing an operator-only verb under the RPP request \
             envelope ({REQUEST_ENV}=1). ADR-080 §5.1 keeps this verb off \
             the network-exposed surface, and §3.5 requires `cs` to refuse \
             it at parse time as the second lock behind the adapter's \
             admission. A remote pilot cannot perform this gesture; an \
             operator on the host can, without the envelope.",
            self.verb
        )
    }
}

impl std::error::Error for OperatorOnlyVerbInApi {}

/// Whether this process *is* an RPP request — `COSMON_API_REQUEST=1`.
///
/// Exactly the predicate the three existing readers already use
/// (`tackle_env::cb_probe_suppressed`, `tackle::egress_launch_is_exposed`,
/// `egress_probe`), so the second lock fires on the same bit the rest of
/// the envelope is keyed on and never on a broader one: an empty or
/// otherwise-valued marker is not an envelope.
pub fn envelope_active<F>(lookup: F) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    lookup(REQUEST_ENV).as_deref() == Some("1")
}

/// Lock 2 of ADR-080 §3.5 — the parse-time refusal.
///
/// `verb` is the subcommand name as clap resolved it, which is why this
/// takes a `&str` rather than an enum: the CLI reads it straight off the
/// parsed `ArgMatches`, so a renamed or newly-added verb cannot silently
/// fall out of the gate the way a hand-written `match` arm would.
///
/// Returns `Ok(())` for every verb that is not on the closed list, and
/// for every invocation with no envelope — the local operator path is
/// byte-identical.
///
/// # Errors
///
/// [`OperatorOnlyVerbInApi`] when the envelope is active and `verb` is on
/// [`OPERATOR_ONLY_VERBS`].
pub fn refuse_operator_only_verb<F>(verb: &str, lookup: F) -> Result<(), OperatorOnlyVerbInApi>
where
    F: Fn(&str) -> Option<String>,
{
    if !envelope_active(lookup) {
        return Ok(());
    }
    OPERATOR_ONLY_VERBS
        .iter()
        .find(|v| **v == verb)
        .map_or(Ok(()), |verb| Err(OperatorOnlyVerbInApi { verb }))
}

/// Consume the request envelope on a `cs`-spawns-`cs` hand-off.
///
/// Call this on every child that cosmon spawns as a **local gesture of
/// the tenant's own machinery** rather than as the network request
/// itself — the resident drain loop's `cs done`, and any future
/// equivalent. The child then passes [`refuse_operator_only_verb`],
/// because it is telling the truth: it is not the request.
///
/// Only the two correlation markers are removed. No security posture is
/// touched — `COSMON_EGRESS_POLICY` and
/// [`crate::egress::EXPOSED_MULTITENANT_ENV`] survive the hand-off, so a
/// child of an exposed dispatch stays as confined as its parent.
pub fn hand_off_to_local_child(cmd: &mut std::process::Command) {
    cmd.env_remove(REQUEST_ENV);
    cmd.env_remove(REQUEST_ID_ENV);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn a_local_operator_run_is_untouched() {
        // No envelope: every verb passes, including the whole closed list.
        for verb in OPERATOR_ONLY_VERBS {
            assert!(refuse_operator_only_verb(verb, env(&[])).is_ok());
        }
    }

    #[test]
    fn every_closed_list_verb_is_refused_under_the_envelope() {
        let lookup = env(&[(REQUEST_ENV, "1")]);
        for verb in OPERATOR_ONLY_VERBS {
            let refusal = refuse_operator_only_verb(verb, &lookup)
                .expect_err("an operator-only verb must not run under the envelope");
            assert_eq!(refusal.verb, *verb);
        }
    }

    #[test]
    fn the_exposed_surface_still_runs_under_the_envelope() {
        // §8p's exposed verbs are the whole point of the envelope: the
        // second lock must be invisible to them. `run` is the load-bearing
        // one — it left the closed list through §5.2 (ADR-124) — and `done`
        // is the second, by the same path (issue #51, the ADR-080 §5.1
        // amendment): closing a molecule is lifecycle, not administration.
        let lookup = env(&[(REQUEST_ENV, "1")]);
        for verb in [
            "observe", "nucleate", "tag", "ensemble", "collapse", "freeze", "thaw", "stuck",
            "tackle", "run", "note", "done",
        ] {
            assert!(
                refuse_operator_only_verb(verb, &lookup).is_ok(),
                "exposed verb `{verb}` must survive the second lock"
            );
        }
    }

    #[test]
    fn only_the_exact_marker_is_an_envelope() {
        // Same bit the rest of the envelope reads, no broader: a stray
        // `COSMON_API_REQUEST=0` in a shell must not brick `cs purge`.
        for value in ["0", "true", "", "  "] {
            assert!(
                refuse_operator_only_verb("purge", env(&[(REQUEST_ENV, value)])).is_ok(),
                "`{value}` must not read as an active envelope"
            );
        }
        assert!(refuse_operator_only_verb("purge", env(&[(REQUEST_ENV, "1")])).is_err());
    }

    /// The §5.1 amendment of issue #51, asserted where the list lives.
    ///
    /// `done` off the closed list is what makes the harvest reachable at
    /// all: while it was on, `cs` refused it under the request envelope and
    /// the door's effect half could not run in any shape. A revert of the
    /// amendment fails here first, and names why.
    #[test]
    fn done_is_no_longer_an_operator_only_verb() {
        assert!(
            !OPERATOR_ONLY_VERBS.contains(&"done"),
            "closing a molecule is lifecycle, not administration (ADR-080 §5.1 as amended \
             by issue #51); the multi-tenant restriction is on WHICH molecules a requester \
             may close, not on the verb"
        );
        assert!(
            refuse_operator_only_verb("done", env(&[(REQUEST_ENV, "1")])).is_ok(),
            "the second lock must not refuse a lifecycle verb"
        );
    }

    #[test]
    fn hand_off_removes_the_markers_and_nothing_else() {
        let mut cmd = std::process::Command::new("true");
        cmd.env(REQUEST_ENV, "1")
            .env(REQUEST_ID_ENV, "req-1")
            .env(crate::egress::EXPOSED_MULTITENANT_ENV, "1")
            .env(crate::egress::EgressPolicy::ENV_VAR, "deny-external");
        hand_off_to_local_child(&mut cmd);

        let removed: Vec<_> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert_eq!(removed, vec![REQUEST_ENV, REQUEST_ID_ENV]);

        let kept: Vec<_> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_some())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert!(
            kept.contains(&crate::egress::EXPOSED_MULTITENANT_ENV.to_owned())
                && kept.contains(&crate::egress::EgressPolicy::ENV_VAR.to_owned()),
            "the hand-off must never relax a security posture: {kept:?}"
        );
    }
}
