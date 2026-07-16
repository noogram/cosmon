// SPDX-License-Identifier: AGPL-3.0-only

//! Layer-2 witness-quorum seal — pure logic for [ADR-085](../../../docs/adr/085-stress-test-seal-mechanism.md) §3 (M3).
//!
//! A *witness* is a cosmon agent that observes a stress-test prior's
//! BLAKE3 hash and emits a structurally-independent attestation. The
//! witness reads the prior's bytes only to compute a hash; it never
//! interprets the content. The dispatch precondition (Layer 1) checks
//! that a matching `EventV2::SealAttested` was emitted by a witness
//! distinct from the worker's tackler — closing the
//! closed-loop-without-oracle pathology that motivated this ADR.
//!
//! # What lives in this module
//!
//! - [`canonical_attestation_record`] — the byte-canonical surface a
//!   witness signs. BLAKE3 of its return value is the
//!   [`EventV2::SealAttested::attestation_b3`](cosmon_core::event_v2::EventV2::SealAttested)
//!   field.
//! - [`compute_attestation_b3`] — convenience: hash a record into the
//!   64-char lowercase hex form the event variant expects.
//! - [`resolve_witness_id`] — derive an agent identity from the
//!   environment (`$TMUX` first, then `<hostname>-<pid>` fallback) so
//!   the cheap same-session heuristic in [`refuse_if_same_session`] has
//!   something to compare against.
//! - [`refuse_if_same_session`] — the structural-independence guard.
//!   Refuses an attestation whose `witness_id` matches the molecule's
//!   tackler `session_name` (ADR-085 §3, *cheap heuristic*).
//!
//! # What lives in the CLI (`cs witness attest`)
//!
//! - I/O against the molecule directory (read `prior.md` /
//!   `prior.b3`, parse `state.json`).
//! - Resolving the molecule via prefix lookup against the
//!   [`StateStore`](cosmon_state::StateStore).
//! - Appending the [`EventV2::SealAttested`](cosmon_core::event_v2::EventV2::SealAttested)
//!   envelope to `events.jsonl`.
//!
//! Splitting along these lines keeps the runtime crate I/O-free per
//! the §*Crate Structure* discipline in `CLAUDE.md` (zero I/O in core
//! and runtime; I/O only in CLI/filestore/transport).

use chrono::{DateTime, Utc};
use cosmon_core::id::MoleculeId;
use cosmon_hash::Hash;

/// Schema tag for the canonical attestation record.
///
/// Bumped if the byte layout below ever changes — readers compare it
/// before parsing fields, the same discipline cosmon-notary uses for
/// `canonical_version`. Today only `v1` exists.
pub const ATTESTATION_RECORD_SCHEMA: &str = "cosmon-witness-attestation/v1";

/// Build the byte-canonical attestation record a witness signs.
///
/// Field order is fixed; one field per line in `key=value` form,
/// terminated by a single `\n`. Two witnesses given the same arguments
/// produce identical bytes — the predicate `attestation_b3ₐ ==
/// attestation_b3_b` is therefore an *attestation-equality* check the
/// Layer-1 guard can run without re-reading the prior.
///
/// The record deliberately commits the operator's **`prior_b3`** (the
/// hash of `prior.md`), not the prior bytes themselves. This preserves
/// the structural-independence guarantee inherited from ADR-052
/// (one-writer / one-witness): the witness signs *what was sealed*,
/// not *what was said*.
#[must_use]
pub fn canonical_attestation_record(
    molecule_id: &MoleculeId,
    prior_b3: &str,
    sealed_at: DateTime<Utc>,
    witness_id: &str,
    attested_at: DateTime<Utc>,
) -> Vec<u8> {
    let body = format!(
        "{ATTESTATION_RECORD_SCHEMA}\nmolecule_id={mol}\nprior_b3={prior}\n\
         sealed_at={sealed}\nwitness_id={witness}\nattested_at={attested}\n",
        mol = molecule_id.as_str(),
        prior = prior_b3,
        sealed = sealed_at.to_rfc3339(),
        witness = witness_id,
        attested = attested_at.to_rfc3339(),
    );
    body.into_bytes()
}

/// Hash the canonical attestation record to its 64-char lowercase hex
/// form — the value [`EventV2::SealAttested::attestation_b3`](cosmon_core::event_v2::EventV2::SealAttested)
/// expects.
#[must_use]
pub fn compute_attestation_b3(
    molecule_id: &MoleculeId,
    prior_b3: &str,
    sealed_at: DateTime<Utc>,
    witness_id: &str,
    attested_at: DateTime<Utc>,
) -> String {
    let bytes =
        canonical_attestation_record(molecule_id, prior_b3, sealed_at, witness_id, attested_at);
    Hash::of_bytes(&bytes).to_hex()
}

/// Refusal returned by [`refuse_if_same_session`].
///
/// The CLI layer maps this to a typed `GuardError` so scripts can
/// branch on the specific rule that fired (single-actor refusal).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "witness refusal: witness_id ({witness_id}) matches the molecule's tackler session ({tackler}); \
     ADR-085 §3 requires a witness spawned in a separate session"
)]
pub struct SameSessionRefusal {
    /// The proposed witness identity (e.g. `$TMUX` value).
    pub witness_id: String,
    /// The molecule's recorded tackler session name.
    pub tackler: String,
}

/// Refuse the attestation when the proposed witness identity matches
/// the molecule's recorded `session_name`.
///
/// `tackler_session` is the molecule's
/// [`MoleculeData::session_name`](cosmon_state::MoleculeData::session_name);
/// `None` (no recorded tackler) cannot collide and short-circuits to
/// `Ok(())` — the operator may legitimately attest before any worker
/// has been spawned.
///
/// # Errors
///
/// Returns [`SameSessionRefusal`] when `witness_id == tackler_session`.
pub fn refuse_if_same_session(
    witness_id: &str,
    tackler_session: Option<&str>,
) -> Result<(), SameSessionRefusal> {
    match tackler_session {
        Some(t) if t == witness_id => Err(SameSessionRefusal {
            witness_id: witness_id.to_owned(),
            tackler: t.to_owned(),
        }),
        _ => Ok(()),
    }
}

/// Resolve a witness identity from the environment.
///
/// Precedence:
/// 1. `$TMUX` (the canonical "I am inside this session" signal — its
///    raw value is `<socket-path>,<pid>,<window>`, sufficiently unique
///    across concurrent witnesses on a single host).
/// 2. `<hostname>-<pid>` fallback for non-tmux invocations (`LaunchAgent`,
///    cron, CI).
///
/// Callers may always override via `--witness-id` on the CLI; this
/// function only supplies the default.
#[must_use]
pub fn resolve_witness_id() -> String {
    resolve_witness_id_from(|key| std::env::var(key).ok(), std::process::id())
}

/// Pure form of [`resolve_witness_id`] taking an env-var lookup
/// callback and a PID. Exists so tests can exercise every branch
/// without mutating process-global state — `forbid(unsafe_code)` on
/// this crate rules out `std::env::set_var`.
#[must_use]
pub fn resolve_witness_id_from(env: impl Fn(&str) -> Option<String>, pid: u32) -> String {
    if let Some(tmux) = env("TMUX") {
        if !tmux.is_empty() {
            return format!("tmux:{tmux}");
        }
    }
    let host = env("HOSTNAME")
        .or_else(|| env("HOST"))
        .unwrap_or_else(|| "unknown-host".to_owned());
    format!("{host}-{pid}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mid(s: &str) -> MoleculeId {
        MoleculeId::new(s).unwrap()
    }

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn canonical_record_is_deterministic() {
        let a = canonical_attestation_record(
            &mid("delib-20260503-5a74"),
            &"f".repeat(64),
            ts("2026-05-03T10:00:00Z"),
            "witness-a",
            ts("2026-05-03T10:05:00Z"),
        );
        let b = canonical_attestation_record(
            &mid("delib-20260503-5a74"),
            &"f".repeat(64),
            ts("2026-05-03T10:00:00Z"),
            "witness-a",
            ts("2026-05-03T10:05:00Z"),
        );
        assert_eq!(a, b);
    }

    #[test]
    fn canonical_record_carries_schema_tag() {
        let bytes = canonical_attestation_record(
            &mid("delib-20260503-5a74"),
            &"a".repeat(64),
            ts("2026-05-03T10:00:00Z"),
            "witness-a",
            ts("2026-05-03T10:05:00Z"),
        );
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(
            s.starts_with(ATTESTATION_RECORD_SCHEMA),
            "schema tag must lead so verifiers can refuse before parsing"
        );
    }

    #[test]
    fn record_changes_when_any_field_changes() {
        let base = compute_attestation_b3(
            &mid("delib-20260503-5a74"),
            &"a".repeat(64),
            ts("2026-05-03T10:00:00Z"),
            "witness-a",
            ts("2026-05-03T10:05:00Z"),
        );
        let other_prior = compute_attestation_b3(
            &mid("delib-20260503-5a74"),
            &"b".repeat(64),
            ts("2026-05-03T10:00:00Z"),
            "witness-a",
            ts("2026-05-03T10:05:00Z"),
        );
        let other_witness = compute_attestation_b3(
            &mid("delib-20260503-5a74"),
            &"a".repeat(64),
            ts("2026-05-03T10:00:00Z"),
            "witness-b",
            ts("2026-05-03T10:05:00Z"),
        );
        assert_ne!(base, other_prior);
        assert_ne!(base, other_witness);
    }

    #[test]
    fn compute_b3_is_64_char_hex() {
        let h = compute_attestation_b3(
            &mid("delib-20260503-5a74"),
            &"a".repeat(64),
            ts("2026-05-03T10:00:00Z"),
            "witness-a",
            ts("2026-05-03T10:05:00Z"),
        );
        assert_eq!(h.len(), 64);
        assert!(h
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    #[test]
    fn same_session_witness_is_refused() {
        let err =
            refuse_if_same_session("delib-5a74-stress", Some("delib-5a74-stress")).unwrap_err();
        assert_eq!(err.witness_id, "delib-5a74-stress");
        assert_eq!(err.tackler, "delib-5a74-stress");
    }

    #[test]
    fn different_session_witness_passes() {
        refuse_if_same_session("witness-host-42", Some("delib-5a74-stress"))
            .expect("distinct sessions must pass the structural-independence check");
    }

    #[test]
    fn untackled_molecule_passes_session_check() {
        // No recorded tackler → no collision possible. The dispatch
        // gate (Layer 1) will still fail until a worker is bound, but
        // that is its job, not the witness's.
        refuse_if_same_session("witness-host-42", None).expect("absent tackler short-circuits OK");
    }

    #[test]
    fn resolve_witness_id_uses_tmux_when_set() {
        let env = |k: &str| match k {
            "TMUX" => Some("/tmp/cosmon-witness-test,123,0".to_owned()),
            _ => None,
        };
        let id = resolve_witness_id_from(env, 999);
        assert!(id.starts_with("tmux:/tmp/cosmon-witness-test"));
    }

    #[test]
    fn resolve_witness_id_falls_back_to_hostname_pid() {
        let env = |k: &str| match k {
            "HOSTNAME" => Some("ci-runner-3".to_owned()),
            _ => None,
        };
        assert_eq!(resolve_witness_id_from(env, 42), "ci-runner-3-42");
    }

    #[test]
    fn resolve_witness_id_handles_no_env_at_all() {
        let id = resolve_witness_id_from(|_| None, 1);
        assert_eq!(id, "unknown-host-1");
    }

    #[test]
    fn empty_tmux_falls_back_like_unset() {
        let env = |k: &str| match k {
            "TMUX" => Some(String::new()),
            "HOSTNAME" => Some("h".to_owned()),
            _ => None,
        };
        assert_eq!(resolve_witness_id_from(env, 7), "h-7");
    }
}
