// SPDX-License-Identifier: AGPL-3.0-only

//! The §8j HTTPS+JWT admission boundary — five clauses.
//!
//! [`http_request_to_spark`] is the totality of trust between an
//! authenticated HTTPS pilot and the cosmon DAG. Every admitted
//! request leaves an inbox file on disk before any `cs` invocation
//! (clause b — causal closure). A pure function over disk-projection
//! state plus the validated JWT.
//!
//! Clause-by-clause mapping (ADR-080 §3):
//!
//! - **(a)** `claim.sub → nucleon_id` via `[HabilitationMap]` (sealed).
//! - **(b)** materialise on disk under
//!   `<inbox_root>/api/<request_id>.json` (audit envelope, never the
//!   raw token).
//! - **(c)** per-`sub` leaky bucket pre-admission rate limit.
//! - **(d)** one-way topology — V0 forbids POST routes outright; the
//!   `bidirectional` flag is reserved for V2+.
//! - **(e)** subprocess envelope — checked here as a list of
//!   *forbidden verbs* (operator-only, ADR-080 §5); the actual
//!   spawn happens in [`crate::subprocess`].

use std::path::Path;

use crate::audit::{self, new_request_id};
use crate::deny_list::DenyList;
use crate::error::RppRejectReason;
use crate::jwt::ValidatedJwt;
use crate::nucleon_map::{HabilitationMap, Noyau};
use crate::rate_limit::{hash_sub, IngressRateLimiter, RateOutcome};

/// Operator-only verbs the RPP MUST refuse to expose (ADR-080 §5.1).
/// The list is **closed**: extending it requires a successor ADR with
/// a `delegate_for` claim model.
pub const OPERATOR_ONLY_VERBS: &[&str] = &[
    "done",
    "evolve",
    "complete",
    "security",
    // `run` left the list 2026-06-11 (ADR-124, task-20260610-56c4):
    // `POST /v1/molecules/{id}/run` admits a REQUEST for a bounded
    // drain of the caller's own DAG — bounds binding-sealed (B1/B2/B3),
    // loop resident in the tenant container. Not the operator verb.
    "kill",
    "purge",
    "reconcile",
    "verify",
    "whisper",
    "drop",
];

/// Authenticated and admitted "Spark" — the unit of perturbation
/// downstream of admission. Carries everything the subprocess
/// envelope needs.
#[derive(Clone, Debug)]
pub struct Spark {
    /// Generated `request_id` (also the inbox file stem).
    pub request_id: String,
    /// Resolved nucleon (clause a output).
    pub nucleon_id: String,
    /// Tenant scope — drives the subprocess `cwd`.
    pub noyau: Noyau,
    /// Verb resolved from the route.
    pub verb: String,
    /// Optional molecule id when the route is per-molecule.
    pub molecule_id: Option<String>,
    /// Path of the materialised audit envelope.
    pub inbox_path: std::path::PathBuf,
}

/// Routes the V0 RPP knows about — the §8p frozen surface.
///
/// All variants are `…Molecule`-suffixed because the §8p surface is
/// (today) entirely molecule-scoped. Future verbs (registry, audit,
/// security) will widen the enum and the postfix collapses naturally;
/// the `enum_variant_names` lint is silenced here to avoid renaming
/// the variants every time we add a parallel `Molecule` verb.
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Verb {
    /// `GET /v1/molecules/{id}` → `cs observe :id --json`.
    ObserveMolecule,
    /// `POST /v1/molecules` → `cs nucleate <formula> ...`.
    /// V1 mutation cut (ADR-080 §10.2 / T-V1-MUTATIONS-NUCLEATE).
    NucleateMolecule,
    /// `POST /v1/molecules/{id}/tags` → `cs tag :id [--add ...] [--remove ...]`.
    /// V1 mutation cut for tagging (T-CST-V0). Reuses the
    /// `cosmon:molecule:write`
    /// scope as nucleate — tags are a write on the molecule's state,
    /// so the same gate applies until a finer-grained scope grid lands.
    TagMolecule,
    /// `GET /v1/molecules` → `cs ensemble --json` filtered listing
    /// (T-CST-EXPAND).
    EnsembleMolecule,
    /// `POST /v1/molecules/{id}/collapse` (T-CST-EXPAND).
    CollapseMolecule,
    /// `POST /v1/molecules/{id}/freeze` (T-CST-EXPAND).
    FreezeMolecule,
    /// `POST /v1/molecules/{id}/thaw` (T-CST-EXPAND).
    ThawMolecule,
    /// `POST /v1/molecules/{id}/stuck` (T-CST-EXPAND).
    StuckMolecule,
    /// `POST /v1/molecules/{id}/tackle` (T9 remote-tackle V2).
    /// Spawns a worker session through the
    /// subprocess envelope (§3.5) — `cs tackle :id` inside the
    /// per-tenant container, which in turn launches Claude Code
    /// via tmux. Unlike the other §8p verbs, tackle is **not**
    /// library-direct: spawning an external agent is fundamentally
    /// out-of-process, so the original §3.5 envelope is reinstated
    /// for this single verb.
    TackleMolecule,
    /// `POST /v1/molecules/{id}/run` (B2 bounded drain, ADR-124).
    /// Starts the resident drain loop
    /// (`cs run <root>`) inside the per-tenant container on the DAG
    /// rooted at `{id}`. The client only REQUESTS the drain; the loop
    /// decides what to tackle, when, under the binding's B1/B2/B3
    /// bounds (the bounds live in a system stronger than
    /// the client). Like tackle, fundamentally out-of-process: the
    /// §3.5 envelope spawns the loop, detached, co-located with the
    /// tenant `StateStore` and `trunk.lock` (the validity condition
    /// of I1 — an advisory flock only binds holders on the same
    /// filesystem).
    RunMolecule,
    /// `GET /v1/molecules/{id}/artifacts` — list artifacts produced
    /// by the worker for a molecule (e653 spec).
    /// Artifacts live on disk under
    /// `/tmp/cosmon/<noyau>/<molecule_id>/`; the handler scans the
    /// directory and returns a typed manifest envelope. Filesystem-
    /// mediated, not state-mediated — no `cs` CLI counterpart exists.
    ListArtifacts,
    /// `GET /v1/molecules/{id}/artifacts/{token}` — stream the binary
    /// bytes of one artifact (e653 spec).
    FetchArtifact,
    /// `PUT /v1/molecules/{id}/artifacts/{name}` — push a
    /// "back-utterance" artifact into the molecule's artifact dir
    /// (e653 spec).
    PushArtifact,
    /// `GET /v1/events` — subscribe to the per-tenant SSE stream of
    /// molecule lifecycle events. Adapter-only;
    /// no matching `cs` CLI verb. Admission still enforces tenant
    /// pinning so a noyau-A JWT only sees noyau-A events.
    SubscribeEvents,
    /// `GET /v1/molecules/{id}/logs` — subscribe to the per-molecule
    /// SSE stream of worker tmux output lines.
    /// Adapter-only; no matching `cs` CLI verb. Admission still
    /// enforces tenant pinning — a noyau-A JWT can only subscribe to
    /// molecules under noyau-A (the `<noyau>/<molecule_id>` join is
    /// what makes this safe under filesystem mediation).
    SubscribeLogs,
    /// `GET /v1/workers` — list active workers in the per-tenant noyau
    /// Adapter-only — no matching `cs` CLI verb (operator-side
    /// observability lives under `cs status` / `cs ensemble`, which the
    /// operator already runs against the on-disk fleet). The verb is
    /// admitted under the standard five-clause boundary so noyau-A JWTs
    /// only see noyau-A workers.
    ListWorkers,
    /// `POST /v1/avatar/converse` — canal (b) pilote↔avatar-tiers
    /// (ADR-0020 §5). A pilote sends a message
    /// to an avatar-tiers that has consented via explicit binding
    /// (on-by-binding). Adapter-only — no `cs` CLI verb.
    ConverseAvatar,
    /// `POST /v1/avatar/perceive` — canal (d) monde↔avatar
    /// (ADR-0020 §5). An external source pushes
    /// perception data into an avatar's perception log. OFF by
    /// default (feature flag per-source). Adapter-only.
    PerceiveAvatar,
    /// Any tool call arriving on the nested `/mcp` Streamable-HTTP surface
    /// (M2, delib-20260709-943e). Admission at this verb resolves the
    /// tenant `noyau` from the audience-pinned binding and enforces the
    /// `CrossTenantPivot` guard **before** the MCP dispatch runs — the
    /// same five-clause boundary every REST route crosses. It is
    /// deliberately a *single* verb: per-tool scope partition
    /// (`evolve`/`complete`/`done` deny-remote, `tackle` behind
    /// `WORKER_SPAWN`) is turing's Q5, a distinct follow-up seam. This
    /// verb's sole job is to yield `spark.noyau` so the host can pin the
    /// MCP state directory to the tenant and render the tool `cwd`
    /// parameter inert.
    McpToolCall,
}

impl Verb {
    /// Stable name carried into the audit log.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ObserveMolecule => "observe",
            Self::NucleateMolecule => "nucleate",
            Self::TagMolecule => "tag",
            Self::EnsembleMolecule => "ensemble",
            Self::CollapseMolecule => "collapse",
            Self::FreezeMolecule => "freeze",
            Self::ThawMolecule => "thaw",
            Self::StuckMolecule => "stuck",
            Self::TackleMolecule => "tackle",
            Self::RunMolecule => "run",
            Self::ListArtifacts => "list_artifacts",
            Self::FetchArtifact => "fetch_artifact",
            Self::PushArtifact => "push_artifact",
            Self::SubscribeEvents => "events_subscribe",
            Self::SubscribeLogs => "logs_subscribe",
            Self::ListWorkers => "list_workers",
            Self::ConverseAvatar => "converse_avatar",
            Self::PerceiveAvatar => "perceive_avatar",
            Self::McpToolCall => "mcp_tool_call",
        }
    }
}

/// Admission inputs — the totality of disk + JWT state needed to
/// decide whether to admit a request. The boundary is a pure
/// function over this struct (plus the [`IngressRateLimiter`] which
/// holds its own internal mutex).
#[derive(Debug)]
pub struct AdmissionRig<'a> {
    /// Sealed `(iss, sub) → Resolved` map (clause a).
    pub nucleon_map: &'a HabilitationMap,
    /// Per-`sub` bucket (clause c).
    pub rate_limiter: &'a IngressRateLimiter,
    /// Disk-backed kill-switch (clauses a + c).
    pub deny_list: &'a DenyList,
    /// Where the audit file lands (clause b).
    pub inbox_root: &'a Path,
    /// Wall-clock for the rate-limiter; tests inject a fixed clock.
    pub now_ms: i64,
}

/// Execute the five-clause admission boundary on a validated JWT
/// + verb pair, returning a [`Spark`] iff the request is admitted.
///
/// # Errors
///
/// Every rejection path returns a typed [`RppRejectReason`]. The
/// caller (HTTP route) translates it into an [`crate::ApiError`]
/// for the wire response and emits an audit-log entry.
pub fn http_request_to_spark(
    rig: &AdmissionRig<'_>,
    jwt: &ValidatedJwt,
    verb: Verb,
    molecule_id: Option<&str>,
) -> Result<Spark, RppRejectReason> {
    // Clause (a) part 1 — operator-only verbs.
    let verb_str = verb.as_str();
    if OPERATOR_ONLY_VERBS.contains(&verb_str) {
        return Err(RppRejectReason::OperatorOnlyVerb(verb_str));
    }

    // Clause (a) — identity mapping. The audience pins the galaxy
    // (ADR-0023 D4: one badge, one galaxy). A principal federated on N
    // galaxies holds N per-audience bindings; the presented `aud` selects
    // exactly one, so a token can never reach a galaxy it does not carry
    // the audience for. The two reject reasons are kept distinct:
    //   - no binding for this exact (iss, sub, aud) BUT the principal has
    //     *some* grant ⇒ CrossTenantPivot (audience scoped to a different
    //     galaxy than the one pinned for this identity);
    //   - the principal has no grant at all ⇒ UnknownSub (deny-by-default).
    let Some(resolved) = rig
        .nucleon_map
        .resolve_for_audience(&jwt.iss, &jwt.sub, &jwt.aud)
    else {
        return Err(match rig.nucleon_map.resolve(&jwt.iss, &jwt.sub) {
            Some(found) => RppRejectReason::CrossTenantPivot {
                expected_noyau: Noyau::new(infer_expected_noyau_from_aud(&jwt.aud)),
                found_noyau: found.noyau.clone(),
            },
            None => RppRejectReason::UnknownSub,
        });
    };

    // Seal verification — the briefing-seal model (ADR-058), per-galaxy
    // (the audience-pinned binding the request resolved to).
    if !rig
        .nucleon_map
        .seal_intact_for_audience(&jwt.iss, &jwt.sub, &jwt.aud)
    {
        return Err(RppRejectReason::SealBroken);
    }

    let snapshot = rig.deny_list.snapshot();
    if snapshot.global_kill {
        return Err(RppRejectReason::GlobalKill);
    }
    if snapshot.denied_jtis.iter().any(|j| j == &jwt.jti) {
        return Err(RppRejectReason::JtiKilled);
    }
    let sub_hash = hash_sub(&jwt.sub);
    if snapshot.denied_sub_hashes.contains(&sub_hash) {
        return Err(RppRejectReason::SubKilled);
    }
    if snapshot
        .denied_noyaus
        .iter()
        .any(|n| n == resolved.noyau.as_str())
    {
        return Err(RppRejectReason::NoyauKilled(resolved.noyau.clone()));
    }

    // Clause (c) — per-`sub` rate limit.
    let outcome = rig
        .rate_limiter
        .check_and_consume(&sub_hash, rig.now_ms)
        .map_err(|e| RppRejectReason::InboxMaterializationFailed(e.to_string()))?;
    match outcome {
        RateOutcome::Admitted => {}
        RateOutcome::Rejected { retry_ms } => {
            return Err(RppRejectReason::RateLimited {
                retry_after: std::time::Duration::from_millis(retry_ms.max(0) as u64),
            });
        }
    }

    // Clause (b) — materialise BEFORE returning Ok. If this fails the
    // rate-limit consumption is *not* rolled back: that is intentional
    // — an attacker hammering us into IO failure pays in token budget.
    let request_id = new_request_id();
    let inbox_path = audit::materialize(
        rig.inbox_root,
        &request_id,
        jwt,
        resolved,
        verb_str,
        molecule_id,
    )?;

    Ok(Spark {
        request_id,
        nucleon_id: resolved.nucleon_id.0.clone(),
        noyau: resolved.noyau.clone(),
        verb: verb_str.to_owned(),
        molecule_id: molecule_id.map(str::to_owned),
        inbox_path,
    })
}

/// Infer the expected `noyau` from the audience claim, used only to
/// produce a structured [`RppRejectReason::CrossTenantPivot`] error
/// (the rejection itself is opaque on the wire — turing G9).
fn infer_expected_noyau_from_aud(aud: &str) -> String {
    aud.strip_prefix("cosmon-rpp-").unwrap_or(aud).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deny_list::DenyList;
    use crate::nucleon_map::{HabilitationId, HabilitationMap};
    use crate::rate_limit::IngressRateLimiter;
    use std::time::Duration;
    use tempfile::TempDir;

    fn rig(td: &TempDir) -> (HabilitationMap, IngressRateLimiter, DenyList) {
        let map = HabilitationMap::builder()
            .insert(
                "https://idp",
                "sub-1",
                HabilitationId::new("nuc-a"),
                Noyau::new("tenant-demo"),
                "cosmon-rpp-tenant-demo",
            )
            .build();
        let lim = IngressRateLimiter::new(td.path().join("rl"), 5.0, 0.0);
        let dl = DenyList::new(td.path().to_path_buf()).with_ttl(Duration::ZERO);
        (map, lim, dl)
    }

    fn jwt(sub: &str, jti: &str, aud: &str) -> ValidatedJwt {
        ValidatedJwt {
            iss: "https://idp".into(),
            sub: sub.into(),
            aud: aud.into(),
            jti: jti.into(),
            lifetime_sec: 60,
            exp: 9_999_999_999,
            scopes: Vec::new(),
        }
    }

    #[test]
    fn admits_observer_with_valid_jwt() {
        let td = TempDir::new().unwrap();
        let (map, lim, dl) = rig(&td);
        let inbox_root = td.path().join("whispers/inbox");
        let rig = AdmissionRig {
            nucleon_map: &map,
            rate_limiter: &lim,
            deny_list: &dl,
            inbox_root: &inbox_root,
            now_ms: 1_000,
        };
        let spark = http_request_to_spark(
            &rig,
            &jwt("sub-1", "tok-1", "cosmon-rpp-tenant-demo"),
            Verb::ObserveMolecule,
            Some("mol-1"),
        )
        .unwrap();
        assert_eq!(spark.nucleon_id, "nuc-a");
        assert_eq!(spark.noyau.as_str(), "tenant-demo");
        assert!(spark.inbox_path.exists());
    }

    #[test]
    fn rejects_unknown_sub() {
        let td = TempDir::new().unwrap();
        let (map, lim, dl) = rig(&td);
        let rig = AdmissionRig {
            nucleon_map: &map,
            rate_limiter: &lim,
            deny_list: &dl,
            inbox_root: &td.path().join("inbox"),
            now_ms: 0,
        };
        let err = http_request_to_spark(
            &rig,
            &jwt("nope", "tok", "cosmon-rpp-tenant-demo"),
            Verb::ObserveMolecule,
            Some("mol-1"),
        )
        .unwrap_err();
        assert!(matches!(err, RppRejectReason::UnknownSub));
    }

    #[test]
    fn rejects_cross_tenant_audience() {
        let td = TempDir::new().unwrap();
        let (map, lim, dl) = rig(&td);
        let rig = AdmissionRig {
            nucleon_map: &map,
            rate_limiter: &lim,
            deny_list: &dl,
            inbox_root: &td.path().join("inbox"),
            now_ms: 0,
        };
        let err = http_request_to_spark(
            &rig,
            &jwt("sub-1", "tok", "cosmon-rpp-other"),
            Verb::ObserveMolecule,
            Some("mol-1"),
        )
        .unwrap_err();
        assert!(matches!(err, RppRejectReason::CrossTenantPivot { .. }));
    }

    #[test]
    fn rejects_when_rate_limit_exceeded() {
        let td = TempDir::new().unwrap();
        let map = HabilitationMap::builder()
            .insert(
                "https://idp",
                "sub-1",
                HabilitationId::new("nuc-a"),
                Noyau::new("tenant-demo"),
                "cosmon-rpp-tenant-demo",
            )
            .build();
        let lim = IngressRateLimiter::new(td.path().join("rl"), 1.0, 0.0); // capacity 1
        let dl = DenyList::new(td.path().to_path_buf()).with_ttl(Duration::ZERO);
        let inbox_root = td.path().join("inbox");
        let rig = AdmissionRig {
            nucleon_map: &map,
            rate_limiter: &lim,
            deny_list: &dl,
            inbox_root: &inbox_root,
            now_ms: 0,
        };
        // First admit: ok.
        http_request_to_spark(
            &rig,
            &jwt("sub-1", "tok-1", "cosmon-rpp-tenant-demo"),
            Verb::ObserveMolecule,
            Some("mol-1"),
        )
        .unwrap();
        // Second admit: rejected.
        let err = http_request_to_spark(
            &rig,
            &jwt("sub-1", "tok-2", "cosmon-rpp-tenant-demo"),
            Verb::ObserveMolecule,
            Some("mol-1"),
        )
        .unwrap_err();
        assert!(matches!(err, RppRejectReason::RateLimited { .. }));
    }

    #[test]
    fn rejects_revoked_jti() {
        let td = TempDir::new().unwrap();
        let (map, lim, dl) = rig(&td);
        // Drop a deny-list entry for tok-bad.
        std::fs::create_dir_all(td.path().join("security")).unwrap();
        std::fs::write(
            td.path().join("security/oidc-policy.toml"),
            r#"
[[deny.jti]]
jti = "tok-bad"
reason = "leak"
since = "2026-04-27T00:00:00Z"
"#,
        )
        .unwrap();
        dl.invalidate();
        let rig = AdmissionRig {
            nucleon_map: &map,
            rate_limiter: &lim,
            deny_list: &dl,
            inbox_root: &td.path().join("inbox"),
            now_ms: 0,
        };
        let err = http_request_to_spark(
            &rig,
            &jwt("sub-1", "tok-bad", "cosmon-rpp-tenant-demo"),
            Verb::ObserveMolecule,
            Some("mol-1"),
        )
        .unwrap_err();
        assert!(matches!(err, RppRejectReason::JtiKilled));
    }

    #[test]
    fn rejects_global_kill() {
        let td = TempDir::new().unwrap();
        let (map, lim, dl) = rig(&td);
        std::fs::create_dir_all(td.path().join("security")).unwrap();
        std::fs::write(
            td.path().join("security/oidc-kill.toml"),
            "[global]\nenabled = true\n",
        )
        .unwrap();
        dl.invalidate();
        let rig = AdmissionRig {
            nucleon_map: &map,
            rate_limiter: &lim,
            deny_list: &dl,
            inbox_root: &td.path().join("inbox"),
            now_ms: 0,
        };
        let err = http_request_to_spark(
            &rig,
            &jwt("sub-1", "tok-1", "cosmon-rpp-tenant-demo"),
            Verb::ObserveMolecule,
            Some("mol-1"),
        )
        .unwrap_err();
        assert!(matches!(err, RppRejectReason::GlobalKill));
    }

    #[test]
    fn causal_closure_writes_inbox_file_before_returning() {
        let td = TempDir::new().unwrap();
        let (map, lim, dl) = rig(&td);
        let inbox_root = td.path().join("inbox");
        let rig = AdmissionRig {
            nucleon_map: &map,
            rate_limiter: &lim,
            deny_list: &dl,
            inbox_root: &inbox_root,
            now_ms: 0,
        };
        let spark = http_request_to_spark(
            &rig,
            &jwt("sub-1", "tok-1", "cosmon-rpp-tenant-demo"),
            Verb::ObserveMolecule,
            Some("mol-42"),
        )
        .unwrap();
        // file under <inbox>/api/req-...json
        assert!(spark.inbox_path.exists());
        assert!(spark.inbox_path.starts_with(inbox_root.join("api")));
        let text = std::fs::read_to_string(&spark.inbox_path).unwrap();
        // Raw `sub` does not leak into the inbox file.
        assert!(!text.contains("sub-1"));
    }
}
