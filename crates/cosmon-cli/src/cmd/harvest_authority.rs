// SPDX-License-Identifier: AGPL-3.0-only

//! Local operator counterpart of the remote harvest authority routes.
//! These commands use the same locked fact loader, verifier and atomic store;
//! none can sign a grant or derive a private key from service state.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context as _};
use chrono::{DateTime, Duration, Utc};
use cosmon_core::config::RemoteHarvestPolicy;
use cosmon_core::harvest_authorization::{
    authorize, policy_digest, DoneAuthorization, GrantEpoch, HarvestAction, HarvestGrant,
    HarvestScope,
};
use cosmon_core::id::MoleculeId;
use cosmon_core::remote_harvest::{resolve_remote_policy, EffectiveRemotePolicy};
use cosmon_filestore::harvest_authority::{
    authority_state, configure_harvest_authority, load_authorizations_with_diagnostics,
    store_verified_authorization, HarvestAuthorityUpdate,
};
use cosmon_harvest::authorization_facts::{
    strict_mission_root, AuthorizationFacts, AuthorizationSources,
};
use cosmon_state::TrunkGuard;

use super::Context;

/// Local administrative and grant-verification actions.
#[derive(clap::Args)]
pub struct Args {
    /// Authority action to perform.
    #[command(subcommand)]
    pub action: Action,
}

/// Actions corresponding to the four remote authority routes.
#[derive(clap::Subcommand)]
pub enum Action {
    /// Select a policy or install a root; local replacement needs --local-reset.
    Configure {
        /// Disabled, scoped or sealed.
        #[arg(long)]
        policy: String,
        /// File containing a minisign public key, never its private half.
        #[arg(long)]
        public_key_file: Option<PathBuf>,
        /// New monotone revocation epoch; required for rotation.
        #[arg(long)]
        epoch: Option<u64>,
        /// Recover a lost current signer by resetting its root on this host.
        #[arg(long)]
        local_reset: bool,
    },
    /// Inspect effective policy, provenance, epoch and public root.
    Status {
        /// Check an installed grant against this molecule's current facts.
        #[arg(long)]
        molecule: Option<String>,
    },
    /// Print a canonical v1 grant and its exact bytes for an external signer.
    Challenge {
        /// Grant for exactly this molecule.
        #[arg(long, conflicts_with = "mission")]
        molecule: Option<String>,
        /// Grant for a mission rooted here.
        #[arg(long, conflicts_with = "molecule")]
        mission: Option<String>,
        /// Requested UTC expiry; default is one hour from now.
        #[arg(long, conflicts_with = "no_expiry")]
        expires_at: Option<DateTime<Utc>>,
        /// Explicitly request a grant without expiry.
        #[arg(long)]
        no_expiry: bool,
    },
    /// Verify and atomically install an independently signed JSON grant.
    Import {
        /// Molecule whose current facts the grant must cover.
        #[arg(long)]
        molecule: String,
        /// JSON file holding a `DoneAuthorization`.
        #[arg(long)]
        file: PathBuf,
    },
}

fn galaxy_root(state: &Path) -> anyhow::Result<&Path> {
    let cosmon = state
        .parent()
        .ok_or_else(|| anyhow!("state root has no parent"))?;
    let root = cosmon
        .parent()
        .ok_or_else(|| anyhow!("state root has no galaxy"))?;
    if cosmon.file_name().is_none_or(|name| name != ".cosmon") {
        bail!("state root must be <galaxy>/.cosmon/state");
    }
    Ok(root)
}

fn facts_for(
    ctx: &Context,
    root: &Path,
    molecule: &MoleculeId,
    guard: &dyn TrunkGuard,
) -> anyhow::Result<AuthorizationFacts> {
    let store = ctx.store();
    AuthorizationFacts::load_under_trunk(
        guard,
        &AuthorizationSources {
            store: store.as_ref(),
            config_path: &root.join(".cosmon/config.toml"),
            galaxy_root: root,
            repo_root: root,
            molecule,
        },
    )
    .map_err(Into::into)
}

/// Dispatch one local authority action without signing or merging.
#[allow(clippy::too_many_lines)]
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let state = ctx.state_dir();
    let root = galaxy_root(&state)?;
    match &args.action {
        Action::Configure {
            policy,
            public_key_file,
            epoch,
            local_reset,
        } => {
            let policy = match policy.as_str() {
                "disabled" => RemoteHarvestPolicy::Disabled,
                "scoped" => RemoteHarvestPolicy::Scoped,
                "sealed" => RemoteHarvestPolicy::Sealed,
                _ => bail!("policy must be disabled, scoped or sealed"),
            };
            let current = authority_state(root)?;
            let public_key = public_key_file
                .as_ref()
                .map(std::fs::read_to_string)
                .transpose()
                .context("read public key")?;
            let replacing_root = public_key.as_deref().is_some_and(|key| {
                current
                    .key_digest
                    .as_deref()
                    .is_some_and(|prior| prior != policy_digest(key.as_bytes()))
            });
            if replacing_root && !local_reset {
                bail!("replacing a local harvest root requires --local-reset");
            }
            if *local_reset && !replacing_root {
                bail!("--local-reset requires a different installed public root");
            }
            let update = HarvestAuthorityUpdate {
                expected_policy: current.policy,
                expected_key_digest: current.key_digest,
                expected_epoch: current.epoch,
                policy,
                public_key,
                epoch: epoch.map(GrantEpoch::from_u64),
            };
            let changed = configure_harvest_authority(root, &update)?;
            println!(
                "{}",
                serde_json::json!({
                    "policy": changed.policy,
                    "key_fingerprint": changed.key_digest,
                    "epoch": changed.epoch.as_u64(),
                })
            );
        }
        Action::Status { molecule } => {
            let store = ctx.store();
            let guard = store.lock_trunk("harvest authority status")?;
            let config = cosmon_filestore::load_project_config(&root.join(".cosmon/config.toml"))?;
            let policy = resolve_remote_policy(&config.harvest_authority)
                .map_err(|_| anyhow!("harvest_policy_conflict"))?;
            let current = authority_state(root)?;
            let grant = if let Some(raw) = molecule {
                let id = MoleculeId::new(raw)?;
                let facts = facts_for(ctx, root, &id, guard.as_ref())?;
                let candidates = load_authorizations_with_diagnostics(&state)?;
                let mut valid = false;
                if let Some(verifier) = &facts.verifier {
                    for candidate in &candidates.authorizations {
                        let mission =
                            if matches!(candidate.grant().scope, HarvestScope::Mission { .. }) {
                                Some(strict_mission_root(store.as_ref(), &id)?)
                            } else {
                                None
                            };
                        let current = facts.for_effect(&id, mission, Utc::now());
                        if authorize(candidate, &current, None, verifier).is_ok() {
                            valid = true;
                            break;
                        }
                    }
                }
                Some(
                    serde_json::json!({"valid": valid, "malformed_candidate": candidates.malformed}),
                )
            } else {
                None
            };
            println!(
                "{}",
                serde_json::json!({
                    "policy": format!("{:?}", policy.policy).to_lowercase(),
                    "provenance": format!("{:?}", policy.provenance).to_lowercase(),
                    "key_fingerprint": current.key_digest,
                    "epoch": current.epoch.as_u64(),
                    "grant": grant,
                })
            );
        }
        Action::Challenge {
            molecule,
            mission,
            expires_at,
            no_expiry,
        } => {
            let raw = molecule
                .as_ref()
                .or(mission.as_ref())
                .ok_or_else(|| anyhow!("--molecule or --mission is required"))?;
            let id = MoleculeId::new(raw)?;
            let store = ctx.store();
            let guard = store.lock_trunk("harvest authority challenge")?;
            let facts = facts_for(ctx, root, &id, guard.as_ref())?;
            let policy = resolve_remote_policy(&facts.config.harvest_authority)
                .map_err(|_| anyhow!("harvest_policy_conflict"))?;
            if policy.policy == EffectiveRemotePolicy::Disabled {
                bail!("harvest_disabled");
            }
            let reservations =
                cosmon_core::harvest_authorization::reservations_crossed(&facts.tags);
            if mission.is_some() && !reservations.is_empty() {
                bail!("harvest_override_requires_ratification");
            }
            let scope = if mission.is_some() {
                HarvestScope::Mission {
                    mission: strict_mission_root(store.as_ref(), &id)?,
                    policy_digest: policy_digest(facts.policy_bytes.as_deref().unwrap_or_default()),
                }
            } else {
                HarvestScope::Molecule { molecule: id }
            };
            let expires = if *no_expiry {
                None
            } else {
                Some(expires_at.unwrap_or_else(|| Utc::now() + Duration::hours(1)))
            };
            if expires.is_some_and(|date| date <= Utc::now()) {
                bail!("harvest_expiry_invalid");
            }
            let grant = HarvestGrant::new(
                facts.project_id.as_str(),
                scope,
                facts.base.branch,
                HarvestAction::Done,
                reservations,
                facts.epoch,
                expires,
            )?;
            println!(
                "{}",
                serde_json::json!({
                    "grant": grant,
                    "canonical": String::from_utf8(grant.canonical_bytes())?,
                    "fingerprint": grant.fingerprint().as_str(),
                })
            );
        }
        Action::Import { molecule, file } => {
            let id = MoleculeId::new(molecule)?;
            let bytes = std::fs::read(file).context("read signed grant")?;
            if bytes.len() > 64 * 1024 {
                bail!("harvest_grant_too_large");
            }
            let authorization: DoneAuthorization =
                serde_json::from_slice(&bytes).context("decode signed grant")?;
            let store = ctx.store();
            let guard = store.lock_trunk("harvest authority import")?;
            let facts = AuthorizationFacts::load_under_trunk(
                guard.as_ref(),
                &AuthorizationSources {
                    store: store.as_ref(),
                    config_path: &root.join(".cosmon/config.toml"),
                    galaxy_root: root,
                    repo_root: root,
                    molecule: &id,
                },
            )?;
            let verifier = facts
                .verifier
                .as_ref()
                .ok_or_else(|| anyhow!("harvest_key_missing"))?;
            let mission = if matches!(authorization.grant().scope, HarvestScope::Mission { .. }) {
                Some(strict_mission_root(store.as_ref(), &id)?)
            } else {
                None
            };
            let current = facts.for_effect(&id, mission, Utc::now());
            if !current.reservations_crossed.is_empty() {
                bail!("reserved remote harvest is closed");
            }
            authorize(&authorization, &current, None, verifier)
                .map_err(|error| anyhow!("{}", error.authorization_cause().reason()))?;
            let fingerprint = store_verified_authorization(&state, &authorization)?;
            println!(
                "{}",
                serde_json::json!({
                    "fingerprint": fingerprint, "installed": true, "consumed": false,
                })
            );
        }
    }
    Ok(())
}
