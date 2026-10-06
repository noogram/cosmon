// SPDX-License-Identifier: AGPL-3.0-only

//! Public authority facts and independently sealed grant transport.
//! Tenant routes never administer policy, epoch or trust roots. The one
//! administrative route checks the host seal before inspecting its body.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use chrono::{DateTime, Duration, Utc};
use cosmon_core::config::RemoteHarvestPolicy;
use cosmon_core::harvest_authorization::{
    authorize, policy_digest, DoneAuthorization, GrantEpoch, HarvestAction,
    HarvestAuthorizationCause, HarvestGrant, HarvestScope,
};
use cosmon_core::id::MoleculeId;
use cosmon_core::remote_harvest::{
    resolve_remote_policy, EffectiveRemotePolicy, PolicyProvenance, ResolvedRemotePolicy,
};
use cosmon_filestore::harvest_authority::{
    authority_state, configure_remote_harvest_authority, load_authorizations_with_diagnostics,
    store_verified_authorization, HarvestAuthorityUpdate,
};
use cosmon_filestore::FileStore;
use cosmon_harvest::authorization_facts::{
    strict_mission_root, AuthorizationFacts, AuthorizationSources,
};
use cosmon_state::StateStore;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::admission::{Spark, Verb};
use crate::audit::new_request_id;
use crate::auth::scopes::{MOLECULE_HARVEST, MOLECULE_READ, MOLECULE_WRITE};
use crate::error::{ApiError, HarvestApiError};
use crate::jwt::JwtVerifier;
use crate::routes::molecules::{
    authorise_scope_public, build_spark_public, extract_bearer, status_public,
};
use crate::AppState;

fn api(status: StatusCode, label: &'static str, id: Option<String>) -> ApiError {
    ApiError {
        status,
        label,
        request_id: id,
        retry_after_seconds: None,
    }
}

fn cause(id: &str, kind: HarvestAuthorizationCause) -> HarvestApiError {
    HarvestApiError::with_cause(
        api(StatusCode::FORBIDDEN, "not_authorized", Some(id.to_owned())),
        kind,
    )
}

fn policy_word(policy: EffectiveRemotePolicy) -> &'static str {
    match policy {
        EffectiveRemotePolicy::Disabled => "disabled",
        EffectiveRemotePolicy::Scoped => "scoped",
        EffectiveRemotePolicy::Sealed => "sealed",
    }
}

fn provenance_word(provenance: PolicyProvenance) -> &'static str {
    match provenance {
        PolicyProvenance::Legacy => "legacy",
        PolicyProvenance::Explicit => "explicit",
    }
}

fn tenant(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    verb: Verb,
    target: Option<&str>,
    scopes: &[&str],
) -> Result<(Spark, PathBuf), HarvestApiError> {
    let token = extract_bearer(headers).map_err(|e| state.reject(e))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| state.reject(e))?;
    let spark = build_spark_public(state, &jwt, verb, target)?;
    authorise_scope_public(state, &jwt, verb.as_str(), scopes, MOLECULE_HARVEST)
        .map_err(|_| cause(&spark.request_id, HarvestAuthorizationCause::ScopeMissing))?;
    let root = state.galaxies_root.join(spark.noyau.as_str());
    if !root.is_dir() {
        return Err(api(StatusCode::NOT_FOUND, "not_found", Some(spark.request_id)).into());
    }
    Ok((spark, root))
}

fn resolved_policy(root: &Path, id: &str) -> Result<ResolvedRemotePolicy, HarvestApiError> {
    let config = cosmon_filestore::load_project_config(&root.join(".cosmon/config.toml"))
        .map_err(|_| cause(id, HarvestAuthorizationCause::FactsUnavailable))?;
    resolve_remote_policy(&config.harvest_authority)
        .map_err(|_| cause(id, HarvestAuthorizationCause::PolicyConflict))
}

fn grant_scopes(policy: ResolvedRemotePolicy) -> &'static [&'static str] {
    if policy.provenance == PolicyProvenance::Legacy {
        &[MOLECULE_HARVEST, MOLECULE_WRITE]
    } else {
        &[MOLECULE_HARVEST]
    }
}

fn locked_facts(
    root: &Path,
    store: &FileStore,
    molecule: &MoleculeId,
    guard: &dyn cosmon_state::TrunkGuard,
    request_id: &str,
) -> Result<AuthorizationFacts, HarvestApiError> {
    let config = root.join(".cosmon/config.toml");
    AuthorizationFacts::load_under_trunk(
        guard,
        &AuthorizationSources {
            store,
            config_path: &config,
            galaxy_root: root,
            repo_root: root,
            molecule,
        },
    )
    .map_err(|_| cause(request_id, HarvestAuthorizationCause::FactsUnavailable))
}

/// Compare-and-set request. The expected triple is deliberately required.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigureBody {
    /// Explicit policy observed by the operator, or null for legacy.
    pub expected_policy: Option<RemoteHarvestPolicy>,
    /// Digest of the observed effective public root, or null.
    pub expected_key_digest: Option<String>,
    /// Observed revocation epoch.
    pub expected_epoch: u64,
    /// New explicit remote policy.
    pub policy: RemoteHarvestPolicy,
    /// Public verifier text only. Signing keys are never accepted.
    pub public_key: Option<String>,
    /// New monotone epoch, required for a root rotation.
    pub epoch: Option<u64>,
    /// Signature by the current root over the tenant-bound rotation statement.
    pub rotation_signature: Option<String>,
}

/// `PUT /v1/admin/noyaux/{noyau}/harvest-authority` — host-sealed CAS.
pub async fn configure(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(noyau): AxumPath<String>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    state.admin_seal.require(&headers)?;
    if noyau.is_empty()
        || !noyau
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(api(StatusCode::BAD_REQUEST, "malformed_noyau", None));
    }
    if body.len() > 32 * 1024 {
        return Err(api(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large", None));
    }
    let value: Value = serde_json::from_slice(&body)
        .map_err(|_| api(StatusCode::BAD_REQUEST, "malformed_harvest_authority", None))?;
    if !["expected_policy", "expected_key_digest", "expected_epoch"]
        .iter()
        .all(|key| value.get(key).is_some())
    {
        return Err(api(
            StatusCode::BAD_REQUEST,
            "malformed_harvest_authority",
            None,
        ));
    }
    let request: ConfigureBody = serde_json::from_value(value)
        .map_err(|_| api(StatusCode::BAD_REQUEST, "malformed_harvest_authority", None))?;
    if request.expected_epoch == 0 || request.epoch == Some(0) {
        return Err(api(
            StatusCode::BAD_REQUEST,
            "invalid_harvest_authority",
            None,
        ));
    }
    let root = state.galaxies_root.join(&noyau);
    if !root.is_dir() {
        return Err(api(StatusCode::NOT_FOUND, "not_found", None));
    }
    let update = HarvestAuthorityUpdate {
        expected_policy: request.expected_policy,
        expected_key_digest: request.expected_key_digest,
        expected_epoch: GrantEpoch::from_u64(request.expected_epoch),
        policy: request.policy,
        public_key: request.public_key,
        epoch: request.epoch.map(GrantEpoch::from_u64),
    };
    let result = configure_remote_harvest_authority(
        &root,
        &noyau,
        &update,
        request.rotation_signature.as_deref(),
    )
    .map_err(|e| {
        let message = e.to_string();
        let (status, label) = if message.contains("harvest_authority_changed") {
            (StatusCode::CONFLICT, "harvest_authority_changed")
        } else if message.contains("harvest_rotation_signature_required") {
            (StatusCode::FORBIDDEN, "rotation_signature_required")
        } else if message.contains("harvest_rotation_signature_invalid") {
            (StatusCode::FORBIDDEN, "rotation_signature_invalid")
        } else if message.contains("harvest_policy_conflict") {
            (StatusCode::CONFLICT, "harvest_policy_conflict")
        } else if message.contains("harvest_public_key_invalid")
            || message.contains("harvest_epoch_rollback")
            || message.contains("harvest_rotation_requires_epoch_bump")
            || message.contains("harvest_key_source_conflict")
        {
            (StatusCode::BAD_REQUEST, "invalid_harvest_authority")
        } else {
            (StatusCode::SERVICE_UNAVAILABLE, "harvest_facts_unavailable")
        };
        api(status, label, None)
    })?;
    Ok(Json(json!({
        "request_id": new_request_id(),
        "policy": result.policy,
        "key_fingerprint": result.key_digest,
        "epoch": result.epoch.as_u64(),
    })))
}

/// Optional molecule for a status answer with grant validity.
#[derive(Debug, Deserialize)]
pub struct StatusQuery {
    /// Molecule whose installed grant should be checked.
    pub molecule: Option<String>,
}

/// `GET /v1/harvest/status` — effective profile and safe public facts.
pub async fn status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<StatusQuery>,
) -> Result<Json<Value>, HarvestApiError> {
    let (spark, root) = tenant(
        &state,
        &headers,
        Verb::HarvestStatus,
        query.molecule.as_deref(),
        &[MOLECULE_READ, MOLECULE_WRITE, MOLECULE_HARVEST],
    )?;
    if let Some(raw) = &query.molecule {
        let molecule = MoleculeId::new(raw).map_err(|_| {
            api(
                StatusCode::NOT_FOUND,
                "not_found",
                Some(spark.request_id.clone()),
            )
        })?;
        status_public(&state, &spark, &molecule)?;
    }
    let store = FileStore::new(root.join(".cosmon/state"));
    let guard = store.lock_trunk("harvest status").map_err(|_| {
        cause(
            &spark.request_id,
            HarvestAuthorizationCause::FactsUnavailable,
        )
    })?;
    let policy = resolved_policy(&root, &spark.request_id)?;
    let public = authority_state(&root).map_err(|_| {
        cause(
            &spark.request_id,
            HarvestAuthorizationCause::FactsUnavailable,
        )
    })?;
    let grant = if let Some(raw) = &query.molecule {
        let molecule = MoleculeId::new(raw).map_err(|_| {
            api(
                StatusCode::NOT_FOUND,
                "not_found",
                Some(spark.request_id.clone()),
            )
        })?;
        let facts = locked_facts(&root, &store, &molecule, guard.as_ref(), &spark.request_id)?;
        let candidates =
            load_authorizations_with_diagnostics(root.join(".cosmon/state")).map_err(|_| {
                cause(
                    &spark.request_id,
                    HarvestAuthorizationCause::FactsUnavailable,
                )
            })?;
        let mut valid = false;
        if let Some(verifier) = &facts.verifier {
            for candidate in &candidates.authorizations {
                let mission = if matches!(candidate.grant().scope, HarvestScope::Mission { .. }) {
                    Some(strict_mission_root(&store, &molecule).map_err(|_| {
                        cause(
                            &spark.request_id,
                            HarvestAuthorizationCause::FactsUnavailable,
                        )
                    })?)
                } else {
                    None
                };
                let current = facts.for_effect(&molecule, mission, Utc::now());
                if authorize(candidate, &current, None, verifier).is_ok() {
                    valid = true;
                    break;
                }
            }
        }
        Some(json!({"valid": valid, "malformed_candidate": candidates.malformed}))
    } else {
        None
    };
    let required_scope = if policy.provenance == PolicyProvenance::Legacy {
        "cosmon:molecule:write or cosmon:molecule:harvest"
    } else {
        MOLECULE_HARVEST
    };
    Ok(Json(json!({
        "request_id": spark.request_id,
        "policy": policy_word(policy.policy),
        "provenance": provenance_word(policy.provenance),
        "required_scope": required_scope,
        "executor_supported": policy.provenance == PolicyProvenance::Legacy || state.harvest_effect.supports_explicit_remote(),
        "key_fingerprint": public.key_digest,
        "epoch": public.epoch.as_u64(),
        "grant": grant,
    })))
}

/// Requested scope and expiry for a canonical challenge.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChallengeBody {
    /// One molecule, exclusive with `mission`.
    pub molecule: Option<String>,
    /// Mission root, exclusive with `molecule`.
    pub mission: Option<String>,
    /// Requested expiry; default is one hour from the current clock.
    pub expires_at: Option<DateTime<Utc>>,
    /// Explicitly request a grant without an expiry.
    #[serde(default)]
    pub no_expiry: bool,
}

/// `POST /v1/harvest/challenge` — current facts and exact signed bytes.
#[allow(clippy::too_many_lines)]
pub async fn challenge(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, HarvestApiError> {
    if body.len() > 16 * 1024 {
        return Err(api(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large", None).into());
    }
    let request: ChallengeBody = serde_json::from_slice(&body)
        .map_err(|_| api(StatusCode::BAD_REQUEST, "malformed_harvest_challenge", None))?;
    let ((Some(raw), None) | (None, Some(raw))) = (&request.molecule, &request.mission) else {
        return Err(api(StatusCode::BAD_REQUEST, "malformed_harvest_challenge", None).into());
    };
    if request.no_expiry && request.expires_at.is_some() {
        return Err(api(StatusCode::BAD_REQUEST, "malformed_harvest_challenge", None).into());
    }
    let molecule =
        MoleculeId::new(raw).map_err(|_| api(StatusCode::NOT_FOUND, "not_found", None))?;
    let (spark, root) = tenant(
        &state,
        &headers,
        Verb::HarvestChallenge,
        Some(raw),
        &[MOLECULE_HARVEST, MOLECULE_WRITE],
    )?;
    status_public(&state, &spark, &molecule)?;
    let policy = resolved_policy(&root, &spark.request_id)?;
    if policy.policy == EffectiveRemotePolicy::Disabled {
        return Err(cause(
            &spark.request_id,
            HarvestAuthorizationCause::Disabled,
        ));
    }
    if policy.provenance == PolicyProvenance::Explicit {
        // `write` alone is not sufficient on an explicit profile.
        let token = extract_bearer(&headers).map_err(|e| state.reject(e))?;
        let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
            .map_err(|e| state.reject(e))?;
        authorise_scope_public(
            &state,
            &jwt,
            "harvest_challenge",
            grant_scopes(policy),
            MOLECULE_HARVEST,
        )
        .map_err(|_| cause(&spark.request_id, HarvestAuthorizationCause::ScopeMissing))?;
    }
    let store = FileStore::new(root.join(".cosmon/state"));
    let guard = store.lock_trunk("harvest challenge").map_err(|_| {
        cause(
            &spark.request_id,
            HarvestAuthorizationCause::FactsUnavailable,
        )
    })?;
    let facts = locked_facts(&root, &store, &molecule, guard.as_ref(), &spark.request_id)?;
    let current = resolve_remote_policy(&facts.config.harvest_authority)
        .map_err(|_| cause(&spark.request_id, HarvestAuthorizationCause::PolicyConflict))?;
    if current != policy {
        return Err(cause(
            &spark.request_id,
            HarvestAuthorizationCause::FactsChanged,
        ));
    }
    let scope = if request.mission.is_some() {
        let root_id = strict_mission_root(&store, &molecule).map_err(|_| {
            cause(
                &spark.request_id,
                HarvestAuthorizationCause::FactsUnavailable,
            )
        })?;
        HarvestScope::Mission {
            mission: root_id,
            policy_digest: policy_digest(facts.policy_bytes.as_deref().unwrap_or_default()),
        }
    } else {
        HarvestScope::Molecule {
            molecule: molecule.clone(),
        }
    };
    let now = Utc::now();
    let expiry = if request.no_expiry {
        None
    } else {
        Some(request.expires_at.unwrap_or(now + Duration::hours(1)))
    };
    if expiry.is_some_and(|at| at <= now) {
        return Err(api(
            StatusCode::BAD_REQUEST,
            "harvest_expiry_invalid",
            Some(spark.request_id),
        )
        .into());
    }
    if request.mission.is_some()
        && !cosmon_core::harvest_authorization::reservations_crossed(&facts.tags).is_empty()
    {
        return Err(cause(
            &spark.request_id,
            HarvestAuthorizationCause::OverrideRequiresRatification,
        ));
    }
    let grant = HarvestGrant::new(
        facts.project_id.as_str(),
        scope,
        facts.base.branch,
        HarvestAction::Done,
        cosmon_core::harvest_authorization::reservations_crossed(&facts.tags),
        facts.epoch,
        expiry,
    )
    .map_err(|_| {
        api(
            StatusCode::BAD_REQUEST,
            "harvest_grant_invalid",
            Some(spark.request_id.clone()),
        )
    })?;
    let canonical = String::from_utf8(grant.canonical_bytes()).map_err(|_| {
        cause(
            &spark.request_id,
            HarvestAuthorizationCause::FactsUnavailable,
        )
    })?;
    Ok(Json(json!({
        "request_id": spark.request_id,
        "grant": grant,
        "canonical": canonical,
        "fingerprint": grant.fingerprint().as_str(),
    })))
}

/// A signed grant and the target used to check its current facts.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportBody {
    /// Molecule whose current facts the grant must cover.
    pub molecule: String,
    /// Operator-signed authority, including its v1 attestation.
    pub authorization: DoneAuthorization,
}

/// `POST /v1/harvest/grants` — verify, then atomically install; never spend.
pub async fn import_grant(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), HarvestApiError> {
    if body.len() > 64 * 1024 {
        return Err(api(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large", None).into());
    }
    let request: ImportBody = serde_json::from_slice(&body)
        .map_err(|_| api(StatusCode::BAD_REQUEST, "malformed_harvest_grant", None))?;
    let molecule = MoleculeId::new(&request.molecule)
        .map_err(|_| api(StatusCode::NOT_FOUND, "not_found", None))?;
    let (spark, root) = tenant(
        &state,
        &headers,
        Verb::HarvestImport,
        Some(&request.molecule),
        &[MOLECULE_HARVEST, MOLECULE_WRITE],
    )?;
    status_public(&state, &spark, &molecule)?;
    let policy = resolved_policy(&root, &spark.request_id)?;
    if policy.policy == EffectiveRemotePolicy::Disabled {
        return Err(cause(
            &spark.request_id,
            HarvestAuthorizationCause::Disabled,
        ));
    }
    if policy.provenance == PolicyProvenance::Explicit {
        let token = extract_bearer(&headers).map_err(|e| state.reject(e))?;
        let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
            .map_err(|e| state.reject(e))?;
        authorise_scope_public(
            &state,
            &jwt,
            "harvest_import",
            grant_scopes(policy),
            MOLECULE_HARVEST,
        )
        .map_err(|_| cause(&spark.request_id, HarvestAuthorizationCause::ScopeMissing))?;
    }
    let store = FileStore::new(root.join(".cosmon/state"));
    let guard = store.lock_trunk("harvest grant import").map_err(|_| {
        cause(
            &spark.request_id,
            HarvestAuthorizationCause::FactsUnavailable,
        )
    })?;
    let facts = locked_facts(&root, &store, &molecule, guard.as_ref(), &spark.request_id)?;
    let current = resolve_remote_policy(&facts.config.harvest_authority)
        .map_err(|_| cause(&spark.request_id, HarvestAuthorizationCause::PolicyConflict))?;
    if current != policy {
        return Err(cause(
            &spark.request_id,
            HarvestAuthorizationCause::FactsChanged,
        ));
    }
    let verifier = facts
        .verifier
        .as_ref()
        .ok_or_else(|| cause(&spark.request_id, HarvestAuthorizationCause::KeyMissing))?;
    let mission = if matches!(
        request.authorization.grant().scope,
        HarvestScope::Mission { .. }
    ) {
        Some(strict_mission_root(&store, &molecule).map_err(|_| {
            cause(
                &spark.request_id,
                HarvestAuthorizationCause::FactsUnavailable,
            )
        })?)
    } else {
        None
    };
    let effect_facts = facts.for_effect(&molecule, mission, Utc::now());
    authorize(&request.authorization, &effect_facts, None, verifier)
        .map_err(|refusal| cause(&spark.request_id, refusal.authorization_cause()))?;
    if !effect_facts.reservations_crossed.is_empty() {
        return Err(cause(
            &spark.request_id,
            HarvestAuthorizationCause::OverrideRequiresRatification,
        ));
    }
    let fingerprint =
        store_verified_authorization(&root.join(".cosmon/state"), &request.authorization).map_err(
            |_| {
                cause(
                    &spark.request_id,
                    HarvestAuthorizationCause::FactsUnavailable,
                )
            },
        )?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "request_id": spark.request_id,
            "fingerprint": fingerprint,
            "installed": true,
            "consumed": false,
        })),
    ))
}
