// SPDX-License-Identifier: AGPL-3.0-only

//! Pure policy resolution for the remote harvest boundary.

use crate::config::{HarvestAuthorityConfig, RemoteHarvestPolicy};
use crate::harvest_door::HarvestOptions;
use crate::id::MoleculeId;

/// Provenance of the effective decision, needed to preserve legacy behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyProvenance {
    /// No remote policy was written by an administrator.
    Legacy,
    /// An administrator selected the remote policy.
    Explicit,
}

/// Effective authorization mode of a remote `done` request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveRemotePolicy {
    /// No remote harvest is enabled.
    Disabled,
    /// The dedicated scope authorizes ordinary effects.
    Scoped,
    /// A valid sealed grant is additionally required.
    Sealed,
}

/// A resolved remote policy with its configuration provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedRemotePolicy {
    /// The effect authorization mode.
    pub policy: EffectiveRemotePolicy,
    /// Whether compatibility rules apply.
    pub provenance: PolicyProvenance,
}

/// The explicit scoped mode cannot weaken an existing local seal requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HarvestPolicyConflict;

/// Resolve the full compatibility table without reading ambient state.
///
/// # Errors
///
/// Refuses `remote = "scoped"` together with `required = true`.
pub fn resolve_remote_policy(
    config: &HarvestAuthorityConfig,
) -> Result<ResolvedRemotePolicy, HarvestPolicyConflict> {
    let (policy, provenance) = match config.remote {
        None if config.required => (EffectiveRemotePolicy::Sealed, PolicyProvenance::Legacy),
        None => (EffectiveRemotePolicy::Disabled, PolicyProvenance::Legacy),
        Some(RemoteHarvestPolicy::Disabled) => {
            (EffectiveRemotePolicy::Disabled, PolicyProvenance::Explicit)
        }
        Some(RemoteHarvestPolicy::Scoped) if config.required => {
            return Err(HarvestPolicyConflict);
        }
        Some(RemoteHarvestPolicy::Scoped) => {
            (EffectiveRemotePolicy::Scoped, PolicyProvenance::Explicit)
        }
        Some(RemoteHarvestPolicy::Sealed) => {
            (EffectiveRemotePolicy::Sealed, PolicyProvenance::Explicit)
        }
    };
    Ok(ResolvedRemotePolicy { policy, provenance })
}

/// Server-created remote authority carried to the effect without a wire or
/// environment representation. The validator rechecks its source under lock.
#[derive(Debug, Clone)]
pub struct RemoteHarvestAdmission {
    /// Exact issuer selected by token validation.
    pub issuer: String,
    /// Exact subject selected by token validation.
    pub subject: String,
    /// Exact audience selected by token validation.
    pub audience: String,
    /// Tenant selected by the audience-pinned binding.
    pub tenant: String,
    /// Molecule selected by the route.
    pub molecule: MoleculeId,
    /// Options the route admitted.
    pub options: HarvestOptions,
    /// Validated token expiration as Unix seconds.
    pub expires_at: u64,
    /// Validated token identifier, rechecked against current revocations.
    pub token_id: String,
    /// Source that actually admitted the scope at the route.
    pub authority_source: RemoteAuthoritySource,
    /// Resolved route policy, checked again under the effect lock.
    pub policy: ResolvedRemotePolicy,
}

/// The exact scope source selected at route admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteAuthoritySource {
    /// The exact binding grants the dedicated harvest scope.
    BindingHarvest,
    /// The legacy sealed profile admitted a write scope in the token.
    LegacyTokenWrite,
    /// The legacy sealed profile admitted a write scope in the binding.
    LegacyBindingWrite,
}

/// Port for revalidating an admission against current binding and revocation
/// state at the effect boundary.
pub trait RemoteAdmissionValidator: Send + Sync {
    /// Refuse when identity, tenant, scope, expiration or revocation changed.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic for a denied or unavailable admission source.
    fn validate(&self, admission: &RemoteHarvestAdmission) -> Result<(), String>;
}

/// Refuse legacy override flags on an explicit profile until a signed
/// override format exists.
#[must_use]
pub fn explicit_override_requested(options: &HarvestOptions) -> bool {
    options.force || options.skip_pre_done_hook || options.deploy_off_trunk
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_remote_policy_rows_are_explicit() {
        let mut config = HarvestAuthorityConfig::default();
        assert_eq!(
            resolve_remote_policy(&config).unwrap().policy,
            EffectiveRemotePolicy::Disabled
        );
        config.required = true;
        assert_eq!(
            resolve_remote_policy(&config).unwrap().provenance,
            PolicyProvenance::Legacy
        );
        config.remote = Some(RemoteHarvestPolicy::Disabled);
        assert_eq!(
            resolve_remote_policy(&config).unwrap().policy,
            EffectiveRemotePolicy::Disabled
        );
        config.remote = Some(RemoteHarvestPolicy::Scoped);
        assert_eq!(resolve_remote_policy(&config), Err(HarvestPolicyConflict));
        config.required = false;
        assert_eq!(
            resolve_remote_policy(&config).unwrap().policy,
            EffectiveRemotePolicy::Scoped
        );
        config.remote = Some(RemoteHarvestPolicy::Sealed);
        assert_eq!(
            resolve_remote_policy(&config).unwrap().policy,
            EffectiveRemotePolicy::Sealed
        );
        config.required = true;
        assert_eq!(
            resolve_remote_policy(&config).unwrap().policy,
            EffectiveRemotePolicy::Sealed
        );
        assert!(crate::config::ProjectConfig::parse(
            "[harvest_authority]\nremote = \"unrecognized\"\n"
        )
        .is_err());
    }
}
