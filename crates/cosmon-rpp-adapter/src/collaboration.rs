// SPDX-License-Identifier: AGPL-3.0-only

//! Collaboration admission for the cross-machine collaboration contract
//! (`docs/specs/cross-machine-collaboration.md`, §2, §3 and §6).
//!
//! The five-clause boundary of [`crate::admission`] establishes the tenant
//! identity. That is necessary and insufficient for collaboration: every
//! identity of a tenant passes it, so it cannot tell one work seat from
//! another. This module adds the second step a collaboration route runs after
//! [`crate::admission::http_request_to_spark`]:
//!
//! 1. the token (its own scopes plus the identity binding's granted scopes)
//!    must carry the route's collaboration scope — refused as
//!    `scope_missing` before any binding is read;
//! 2. the request must present an attachment ID and its private proof in the
//!    [`ATTACHMENT_HEADER`] and [`ATTACHMENT_PROOF_HEADER`] headers;
//! 3. an operator-provisioned collaboration binding must match the exact
//!    identity, attachment, proof and target, be unrevoked and grant the
//!    scope.
//!
//! The seat comes from the binding; no request field can choose it. The
//! module holds only a [`CollaborationBindingReader`], which has no write
//! method, so no request can edit its own grant. No route is mounted here:
//! the work routes arrive with W4 and the session routes with W7.

use cosmon_core::collaboration::{
    AdmissionRequest, AdmittedCollaborator, AdmittedIdentity, AttachmentId, AttachmentProof,
    CollaborationRefusal, CollaborationScope, CollaborationTarget,
};
use cosmon_state::collaboration::{
    CollaborationBindingReader, CollaborationStoreError, CommitError,
};
use http::HeaderMap;

use crate::jwt::ValidatedJwt;
use crate::nucleon_map::{HabilitationMap, Noyau};

/// Request header carrying the host-issued attachment ID.
pub const ATTACHMENT_HEADER: &str = "cosmon-attachment";

/// Request header carrying the private attachment proof. Never logged and
/// never written to the request audit.
pub const ATTACHMENT_PROOF_HEADER: &str = "cosmon-attachment-proof";

/// Why a collaboration admission did not produce a caller.
#[derive(Debug, thiserror::Error)]
pub enum CollaborationAdmissionError {
    /// The caller is refused; the code is one of the contract's 403 codes.
    #[error(transparent)]
    Refused(#[from] CollaborationRefusal),
    /// Stored bindings fail validation; nothing is repaired silently.
    #[error("collaboration bindings failed validation")]
    Corrupt,
    /// The binding store could not be read or locked; the outcome is unknown.
    #[error("collaboration bindings are unavailable")]
    Unavailable,
}

impl CollaborationAdmissionError {
    /// Stable wire code from the contract's error table (§6).
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Refused(refusal) => refusal.code(),
            Self::Corrupt => "corrupt_evidence",
            Self::Unavailable => "custody_unavailable",
        }
    }

    /// HTTP status from the contract's error table (§6).
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Refused(_) => 403,
            Self::Corrupt => 500,
            Self::Unavailable => 503,
        }
    }
}

impl From<CollaborationStoreError> for CollaborationAdmissionError {
    fn from(error: CollaborationStoreError) -> Self {
        match error {
            CollaborationStoreError::Refused(refusal) => Self::Refused(refusal),
            CollaborationStoreError::Io(_) => Self::Unavailable,
            CollaborationStoreError::Json(_)
            | CollaborationStoreError::Spec(_)
            | CollaborationStoreError::Set(_)
            | CollaborationStoreError::Corrupt(_)
            | CollaborationStoreError::Full
            | CollaborationStoreError::UnknownBinding(_) => Self::Corrupt,
        }
    }
}

/// Inputs of one collaboration admission, all taken from the already
/// admitted request.
#[derive(Debug)]
pub struct CollaborationAdmission<'a> {
    /// Read-only bindings of the tenant's galaxy.
    pub bindings: &'a CollaborationBindingReader,
    /// Identity bindings, for scopes granted to the token's identity.
    pub nucleon_map: &'a HabilitationMap,
    /// Validated token.
    pub jwt: &'a ValidatedJwt,
    /// Tenant selected by [`crate::admission::http_request_to_spark`].
    pub tenant: &'a Noyau,
    /// Request headers holding the attachment and its proof.
    pub headers: &'a HeaderMap,
    /// Work or mission named by the route path.
    pub target: &'a CollaborationTarget,
    /// Scope the route requires.
    pub required: CollaborationScope,
}

/// Admit a collaboration caller, or refuse it before any collaboration record
/// is read or written.
///
/// # Errors
/// Returns `scope_missing` when the token lacks the scope, `binding_missing`
/// for an absent or malformed attachment, proof, identity or target match,
/// `binding_revoked` for a revoked binding, and `corrupt_evidence` or
/// `custody_unavailable` when the bindings cannot be trusted or read.
pub fn admit_collaborator(
    admission: &CollaborationAdmission<'_>,
) -> Result<AdmittedCollaborator, CollaborationAdmissionError> {
    let jwt = admission.jwt;
    let granted = admission
        .nucleon_map
        .allowed_scopes_for_audience(&jwt.iss, &jwt.sub, &jwt.aud);
    let carries = |scope: CollaborationScope| {
        jwt.has_scope(scope.as_str()) || granted.iter().any(|s| s == scope.as_str())
    };
    if !CollaborationScope::ALL
        .into_iter()
        .any(|held| held.satisfies(admission.required) && carries(held))
    {
        return Err(CollaborationRefusal::ScopeMissing.into());
    }

    let attachment = header(admission.headers, ATTACHMENT_HEADER)
        .and_then(|value| AttachmentId::new(value).ok())
        .ok_or(CollaborationRefusal::BindingMissing)?;
    let proof = header(admission.headers, ATTACHMENT_PROOF_HEADER)
        .and_then(|value| AttachmentProof::parse(value).ok())
        .ok_or(CollaborationRefusal::BindingMissing)?;
    let identity = AdmittedIdentity::new(
        jwt.iss.as_str(),
        jwt.sub.as_str(),
        jwt.aud.as_str(),
        admission.tenant.as_str(),
    )
    .map_err(|_| CollaborationRefusal::BindingMissing)?;

    Ok(admission.bindings.admit(&AdmissionRequest {
        identity: &identity,
        attachment: &attachment,
        proof: &proof,
        target: admission.target,
        required: admission.required,
    })?)
}

/// Run `effect` only if `admitted` still holds at commit, under the shared
/// binding lock (see [`CollaborationBindingReader::commit`]).
///
/// # Errors
/// Returns the binding refusal without running `effect`, or the effect's own
/// error.
pub fn commit_collaboration<T, E>(
    bindings: &CollaborationBindingReader,
    admitted: &AdmittedCollaborator,
    effect: impl FnOnce() -> Result<T, E>,
) -> Result<T, CommitError<E>> {
    bindings.commit(admitted, effect)
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    // Two values for one header are ambiguous; refuse rather than pick one.
    if values.next().is_some() {
        return None;
    }
    value.to_str().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use cosmon_core::advisory_attempt::AdvisorySeatId;
    use cosmon_core::collaboration::{BindingSpec, CollaborationCapability};
    use cosmon_core::id::MoleculeId;
    use cosmon_state::collaboration::CollaborationBindingStore;
    use http::HeaderValue;

    use crate::nucleon_map::HabilitationId;

    const AUD: &str = "cosmon-rpp-demo";

    fn jwt(sub: &str, scopes: &[&str]) -> ValidatedJwt {
        ValidatedJwt {
            iss: "https://idp".into(),
            sub: sub.into(),
            aud: AUD.into(),
            jti: format!("tok-{sub}"),
            lifetime_sec: 60,
            exp: 9_999_999_999,
            scopes: scopes.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn owner() -> MoleculeId {
        MoleculeId::new("task-20261002-aaaa").unwrap()
    }

    fn map() -> HabilitationMap {
        HabilitationMap::builder()
            .insert(
                "https://idp",
                "pilot-a",
                HabilitationId::new("nuc-a"),
                Noyau::new("demo"),
                AUD,
            )
            .insert(
                "https://idp",
                "pilot-b",
                HabilitationId::new("nuc-b"),
                Noyau::new("demo"),
                AUD,
            )
            .build()
    }

    fn provision(
        store: &CollaborationBindingStore,
        sub: &str,
        seat: &str,
    ) -> (AttachmentId, AttachmentProof) {
        let (binding, proof) = store
            .provision(
                BindingSpec {
                    identity: AdmittedIdentity::new("https://idp", sub, AUD, "demo").unwrap(),
                    capability: CollaborationCapability::WorkSeat {
                        owner: owner(),
                        seat: AdvisorySeatId::new(seat).unwrap(),
                    },
                    scopes: [CollaborationScope::WorkWrite].into_iter().collect(),
                    label: None,
                },
                Utc.timestamp_opt(1_800_000_000, 0).single().unwrap(),
            )
            .unwrap();
        (binding.attachment, proof)
    }

    fn headers(attachment: &AttachmentId, proof: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            ATTACHMENT_HEADER,
            HeaderValue::from_str(attachment.as_str()).unwrap(),
        );
        headers.insert(
            ATTACHMENT_PROOF_HEADER,
            HeaderValue::from_str(proof).unwrap(),
        );
        headers
    }

    fn admit_as(
        reader: &CollaborationBindingReader,
        token: &ValidatedJwt,
        headers: &HeaderMap,
        required: CollaborationScope,
    ) -> Result<AdmittedCollaborator, CollaborationAdmissionError> {
        admit_collaborator(&CollaborationAdmission {
            bindings: reader,
            nucleon_map: &map(),
            jwt: token,
            tenant: &Noyau::new("demo"),
            headers,
            target: &CollaborationTarget::Work { owner: owner() },
            required,
        })
    }

    #[test]
    fn a_tenant_identity_cannot_send_as_another_seat() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollaborationBindingStore::new(dir.path());
        let (a_attachment, a_proof) = provision(&store, "pilot-a", "implementer");
        let (b_attachment, b_proof) = provision(&store, "pilot-b", "reviewer");
        let reader = store.reader();
        let token_a = jwt("pilot-a", &["cosmon:work:write"]);

        // The tenant boundary admits pilot A for this tenant whatever seat it
        // means to act for: tenant identity alone cannot bind a seat.
        let limiter = crate::rate_limit::IngressRateLimiter::new(dir.path().join("rl"), 5.0, 0.0);
        let deny = crate::deny_list::DenyList::new(dir.path().to_path_buf())
            .with_ttl(std::time::Duration::ZERO);
        let identities = map();
        let spark = crate::admission::http_request_to_spark(
            &crate::admission::AdmissionRig {
                nucleon_map: &identities,
                rate_limiter: &limiter,
                deny_list: &deny,
                inbox_root: &dir.path().join("inbox"),
                now_ms: 0,
            },
            &token_a,
            crate::admission::Verb::ObserveMolecule,
            Some(owner().as_str()),
        )
        .unwrap();
        assert_eq!(spark.noyau, Noyau::new("demo"));

        let own = admit_as(
            &reader,
            &token_a,
            &headers(&a_attachment, a_proof.expose_secret()),
            CollaborationScope::WorkWrite,
        )
        .unwrap();
        assert_eq!(own.work_caller().unwrap().seat().as_str(), "implementer");

        // Pilot A, admitted to the tenant, presents pilot B's attachment and
        // even pilot B's leaked proof.
        let refused = admit_as(
            &reader,
            &token_a,
            &headers(&b_attachment, b_proof.expose_secret()),
            CollaborationScope::WorkWrite,
        )
        .unwrap_err();
        assert_eq!(refused.code(), "binding_missing");
        assert_eq!(refused.http_status(), 403);
    }

    #[test]
    fn scope_is_checked_before_the_binding_and_headers_are_required() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollaborationBindingStore::new(dir.path());
        let (attachment, proof) = provision(&store, "pilot-a", "implementer");
        let reader = store.reader();

        let no_scope = admit_as(
            &reader,
            &jwt("pilot-a", &["cosmon:molecule:write"]),
            &headers(&attachment, proof.expose_secret()),
            CollaborationScope::WorkRead,
        )
        .unwrap_err();
        assert_eq!(no_scope.code(), "scope_missing");

        let read_implied = admit_as(
            &reader,
            &jwt("pilot-a", &["cosmon:work:write"]),
            &headers(&attachment, proof.expose_secret()),
            CollaborationScope::WorkRead,
        );
        assert!(read_implied.is_ok());

        let headerless = admit_as(
            &reader,
            &jwt("pilot-a", &["cosmon:work:write"]),
            &HeaderMap::new(),
            CollaborationScope::WorkWrite,
        )
        .unwrap_err();
        assert_eq!(headerless.code(), "binding_missing");

        let mut doubled = headers(&attachment, proof.expose_secret());
        doubled.append(
            ATTACHMENT_HEADER,
            HeaderValue::from_static("att-ffffffffffffffff"),
        );
        let ambiguous = admit_as(
            &reader,
            &jwt("pilot-a", &["cosmon:work:write"]),
            &doubled,
            CollaborationScope::WorkWrite,
        )
        .unwrap_err();
        assert_eq!(ambiguous.code(), "binding_missing");
    }

    #[test]
    fn a_legacy_identity_binding_with_collaboration_scopes_grants_nothing() {
        // The identity binding grants every collaboration scope literal, and
        // the token carries them too, but no collaboration binding exists.
        let dir = tempfile::tempdir().unwrap();
        let reader = CollaborationBindingReader::new(dir.path());
        let legacy = HabilitationMap::builder()
            .insert_with_scopes(
                "https://idp",
                "pilot-a",
                HabilitationId::new("nuc-a"),
                Noyau::new("demo"),
                AUD,
                CollaborationScope::ALL
                    .map(|s| s.as_str().to_owned())
                    .to_vec(),
            )
            .build();
        let token = jwt("pilot-a", &["cosmon:work:write"]);
        let headers = headers(
            &AttachmentId::from_random([1; 8]),
            AttachmentProof::from_secret_bytes([1; 32]).expose_secret(),
        );
        let refused = admit_collaborator(&CollaborationAdmission {
            bindings: &reader,
            nucleon_map: &legacy,
            jwt: &token,
            tenant: &Noyau::new("demo"),
            headers: &headers,
            target: &CollaborationTarget::Work { owner: owner() },
            required: CollaborationScope::WorkWrite,
        })
        .unwrap_err();
        assert_eq!(refused.code(), "binding_missing");
        assert!(!dir.path().join("collaboration").exists());
    }

    #[test]
    fn revocation_between_admission_and_commit_refuses_the_effect() {
        let dir = tempfile::tempdir().unwrap();
        let store = CollaborationBindingStore::new(dir.path());
        let (attachment, proof) = provision(&store, "pilot-a", "implementer");
        let reader = store.reader();
        let admitted = admit_as(
            &reader,
            &jwt("pilot-a", &["cosmon:work:write"]),
            &headers(&attachment, proof.expose_secret()),
            CollaborationScope::WorkWrite,
        )
        .unwrap();
        let binding = reader.load().unwrap().iter().next().unwrap().id.clone();
        store
            .revoke(
                &binding,
                Utc.timestamp_opt(1_800_000_100, 0).single().unwrap(),
            )
            .unwrap();

        let mut ran = false;
        let refused = commit_collaboration(&reader, &admitted, || {
            ran = true;
            Ok::<_, ()>(())
        });
        let Err(CommitError::Binding(error)) = refused else {
            panic!("a revoked binding must refuse at commit");
        };
        assert_eq!(
            CollaborationAdmissionError::from(error).code(),
            "binding_revoked"
        );
        assert!(!ran);
    }
}
