// SPDX-License-Identifier: AGPL-3.0-only

//! Strict, shared authority facts for grant tooling and the harvest effect.
//!
//! A caller loads a preliminary snapshot before its gates and another while
//! holding the trunk lock. A changed snapshot invalidates the earlier gates;
//! an unavailable fact is an error, never a default. Mission ancestry is
//! resolved separately because a molecule-scoped seal does not depend on it.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use chrono::{DateTime, Utc};
use cosmon_core::config::ProjectConfig;
use cosmon_core::error::CosmonError;
use cosmon_core::harvest_authorization::{
    policy_digest, reservations_crossed, GrantEpoch, HarvestAction, HarvestFacts,
};
use cosmon_core::id::{MoleculeId, ProjectId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::harvest_authority::{read_epoch, read_policy_bytes, MinisignHarvestVerifier};
use cosmon_state::{MoleculeData, StateStore, TrunkGuard};

use crate::base_branch::{resolve_with_source, ResolvedBase};

/// Paths and state port from which one authority snapshot is read.
pub struct AuthorizationSources<'a> {
    /// The state port holding the molecule and its lineage.
    pub store: &'a dyn StateStore,
    /// Required project configuration path.
    pub config_path: &'a Path,
    /// Galaxy directory holding the policy, epoch and public trust root.
    pub galaxy_root: &'a Path,
    /// Repository whose branch names are resolved.
    pub repo_root: &'a Path,
    /// Molecule about to be harvested.
    pub molecule: &'a MoleculeId,
}

/// One successful read of the authority inputs, before or under the lock.
///
/// The parsed config is used by the transaction; its raw bytes are retained
/// to detect an edit even if it preserves the same parsed values. The key is
/// the verifier built from exactly the bytes whose digest is compared.
#[derive(Debug, Clone)]
pub struct AuthorizationFacts {
    /// Current parsed project configuration.
    pub config: ProjectConfig,
    /// Required project identity, with no placeholder fallback.
    pub project_id: ProjectId,
    /// Current molecule status.
    pub status: MoleculeStatus,
    /// Current resolved integration base and its provenance.
    pub base: ResolvedBase,
    /// Current molecule tags in canonical order.
    pub tags: Vec<String>,
    /// Protected reference paths used by the preliminary gate.
    pub protected_paths: Vec<String>,
    /// Raw declared scope perimeter used by the preliminary gate.
    pub scope_allow: Option<String>,
    /// Current epoch; an absent epoch has the documented first value.
    pub epoch: GrantEpoch,
    /// Current policy bytes; `None` means no optional policy exists.
    pub policy_bytes: Option<Vec<u8>>,
    /// Current pinned verifier; `None` means no trust root is configured.
    pub verifier: Option<MinisignHarvestVerifier>,
    config_bytes: Vec<u8>,
    persisted_base: Option<String>,
    parents: Vec<MoleculeId>,
    molecule_project_id: Option<ProjectId>,
    merged_at: Option<DateTime<Utc>>,
    archived: bool,
}

impl AuthorizationFacts {
    /// Read authority facts while the caller holds the shared trunk guard.
    /// Effect, challenge and import paths use this entry so their current
    /// facts are serialized with cooperating harvest and authority writers.
    ///
    /// # Errors
    ///
    /// Propagates any required fact read or validation failure.
    pub fn load_under_trunk(
        _guard: &dyn TrunkGuard,
        sources: &AuthorizationSources<'_>,
    ) -> Result<Self, CosmonError> {
        Self::load(sources)
    }

    /// Read required state and optional policy/key presence from their ports.
    ///
    /// # Errors
    ///
    /// Refuses unreadable or malformed config, molecule, epoch, policy or key.
    /// A missing required config or project identity is also a fault.
    pub fn load(sources: &AuthorizationSources<'_>) -> Result<Self, CosmonError> {
        let config_bytes =
            std::fs::read(sources.config_path).map_err(|e| CosmonError::StateStore {
                reason: format!("harvest_facts_unavailable: project config: {e}"),
            })?;
        let config_text =
            std::str::from_utf8(&config_bytes).map_err(|e| CosmonError::StateStore {
                reason: format!("harvest_facts_unavailable: project config encoding: {e}"),
            })?;
        let config = ProjectConfig::parse(config_text).map_err(|e| CosmonError::StateStore {
            reason: format!("harvest_facts_unavailable: project config: {e}"),
        })?;
        let project_id = config
            .require_project_id()
            .map_err(|reason| CosmonError::StateStore {
                reason: format!("harvest_facts_unavailable: {reason}"),
            })?
            .clone();
        let mol = sources.store.load_molecule(sources.molecule)?;
        if mol
            .project_id
            .as_ref()
            .is_some_and(|stamped| stamped != &project_id)
        {
            return Err(CosmonError::StateStore {
                reason:
                    "harvest_facts_unavailable: molecule project identity differs from the galaxy"
                        .to_owned(),
            });
        }
        let base = resolve_with_source(
            sources.repo_root,
            mol.base_branch.as_deref(),
            config.project.trunk_branch.as_deref(),
        );
        let mut parents: Vec<MoleculeId> = mol.blocked_by().into_iter().cloned().collect();
        parents.sort();
        let tags = mol.tags.iter().map(ToString::to_string).collect();
        let scope_allow = mol.variables.get("scope_allow").cloned();
        Ok(Self {
            config,
            project_id,
            status: mol.status,
            base,
            tags,
            protected_paths: mol.protected_paths,
            scope_allow,
            epoch: read_epoch(sources.galaxy_root)?,
            policy_bytes: read_policy_bytes(sources.galaxy_root)?,
            verifier: MinisignHarvestVerifier::resolve(sources.galaxy_root)?,
            config_bytes,
            persisted_base: mol.base_branch,
            parents,
            molecule_project_id: mol.project_id,
            merged_at: mol.merged_at,
            archived: mol.archived,
        })
    }

    /// Refuse a changed pre-gate snapshot before any effect mutation.
    ///
    /// This is a pure comparison: filesystem and clock reads belong to
    /// [`Self::load`], so tests can enumerate individual changed facts.
    ///
    /// # Errors
    ///
    /// Returns `harvest_facts_changed` when a preliminary gate observed a
    /// different authority input.
    pub fn require_unchanged(&self, current: &Self) -> Result<(), CosmonError> {
        let old_key = self
            .verifier
            .as_ref()
            .map(|v| (v.source(), v.content_digest()));
        let new_key = current
            .verifier
            .as_ref()
            .map(|v| (v.source(), v.content_digest()));
        if self.config_bytes != current.config_bytes
            || self.project_id != current.project_id
            || self.status != current.status
            || self.base != current.base
            || self.tags != current.tags
            || self.protected_paths != current.protected_paths
            || self.scope_allow != current.scope_allow
            || self.persisted_base != current.persisted_base
            || self.parents != current.parents
            || self.molecule_project_id != current.molecule_project_id
            || self.merged_at != current.merged_at
            || self.archived != current.archived
            || self.epoch != current.epoch
            || self.policy_bytes != current.policy_bytes
            || old_key != new_key
        {
            return Err(CosmonError::StateStore {
                reason: "harvest_facts_changed: restart the gated transaction".to_owned(),
            });
        }
        Ok(())
    }

    /// Ensure early command guards and later gates used the molecule whose
    /// authority facts were captured. This closes the interval between the
    /// command's first state read and its preliminary gate snapshot.
    ///
    /// # Errors
    ///
    /// Returns `harvest_facts_changed` if a gate-relevant molecule field moved.
    pub fn require_same_molecule(&self, preliminary: &MoleculeData) -> Result<(), CosmonError> {
        let mut parents: Vec<MoleculeId> = preliminary.blocked_by().into_iter().cloned().collect();
        parents.sort();
        let tags: Vec<String> = preliminary.tags.iter().map(ToString::to_string).collect();
        if self.status != preliminary.status
            || self.persisted_base != preliminary.base_branch
            || self.parents != parents
            || self.tags != tags
            || self.protected_paths != preliminary.protected_paths
            || self.scope_allow != preliminary.variables.get("scope_allow").cloned()
            || self.molecule_project_id != preliminary.project_id
            || self.merged_at != preliminary.merged_at
            || self.archived != preliminary.archived
        {
            return Err(CosmonError::StateStore {
                reason: "harvest_facts_changed: molecule changed before the preliminary gates"
                    .to_owned(),
            });
        }
        Ok(())
    }

    /// Build I/O-free reducer input from this locked snapshot.
    #[must_use]
    pub fn for_effect(
        &self,
        molecule: &MoleculeId,
        mission: Option<MoleculeId>,
        now: DateTime<Utc>,
    ) -> HarvestFacts {
        HarvestFacts {
            galaxy: self.project_id.as_str().to_owned(),
            molecule: molecule.clone(),
            mission,
            policy_digest: Some(policy_digest(
                self.policy_bytes.as_deref().unwrap_or_default(),
            )),
            base: self.base.branch.clone(),
            action: HarvestAction::Done,
            reservations_crossed: reservations_crossed(&self.tags),
            epoch: self.epoch,
            now,
        }
    }
}

/// Full ancestor-edge witness for one uniquely rooted mission.
///
/// Comparing the whole graph before and after gates detects a parent edit
/// even when the root molecule ID happens to remain unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissionAncestry {
    /// Unique root reached by every ancestry path.
    pub root: MoleculeId,
    /// Every visited molecule and its sorted direct parents.
    pub edges: Vec<(MoleculeId, Vec<MoleculeId>)>,
}

/// Resolve a unique mission root without the display projection's recovery
/// defaults. A missing parent, cycle or multiple roots makes mission-scoped
/// authority unavailable. Molecule-scoped callers need not invoke this.
///
/// # Errors
///
/// Returns a state fault for unreadable ancestry, cycles or ambiguous roots.
pub fn strict_mission_root(
    store: &dyn StateStore,
    molecule: &MoleculeId,
) -> Result<MoleculeId, CosmonError> {
    strict_mission_ancestry(store, molecule).map(|ancestry| ancestry.root)
}

/// Read every mission-parent edge for a pre-gate or locked snapshot.
///
/// # Errors
///
/// Returns a state fault for unreadable ancestry, cycles or ambiguous roots.
pub fn strict_mission_ancestry(
    store: &dyn StateStore,
    molecule: &MoleculeId,
) -> Result<MissionAncestry, CosmonError> {
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    let mut roots = BTreeSet::new();
    let mut edges = Vec::new();
    let mut stack = vec![(molecule.clone(), false)];
    while let Some((id, exit)) = stack.pop() {
        if exit {
            visiting.remove(&id);
            visited.insert(id);
            continue;
        }
        if visited.contains(&id) {
            continue;
        }
        if !visiting.insert(id.clone()) {
            return Err(CosmonError::StateStore {
                reason: "harvest_facts_unavailable: mission ancestry has a cycle".to_owned(),
            });
        }
        let mol = store.load_molecule(&id)?;
        let parents = mol.blocked_by();
        if parents.is_empty() {
            roots.insert(id.clone());
        }
        let mut sorted_parents: Vec<MoleculeId> = parents.iter().map(|p| (*p).clone()).collect();
        sorted_parents.sort();
        edges.push((id.clone(), sorted_parents));
        stack.push((id, true));
        for parent in parents {
            if visiting.contains(parent) {
                return Err(CosmonError::StateStore {
                    reason: "harvest_facts_unavailable: mission ancestry has a cycle".to_owned(),
                });
            }
            stack.push((parent.clone(), false));
        }
    }
    if roots.len() != 1 {
        return Err(CosmonError::StateStore {
            reason: "harvest_facts_unavailable: mission ancestry has no unique root".to_owned(),
        });
    }
    let root = roots
        .into_iter()
        .next()
        .ok_or_else(|| CosmonError::StateStore {
            reason: "harvest_facts_unavailable: mission root absent".to_owned(),
        })?;
    edges.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(MissionAncestry { root, edges })
}
