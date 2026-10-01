// SPDX-License-Identifier: AGPL-3.0-only

//! A stranded administrative write cannot become usable authority.

use cosmon_core::config::RemoteHarvestPolicy;
use cosmon_core::harvest_authorization::GrantEpoch;
use cosmon_filestore::harvest_authority::{
    authority_state, configure_harvest_authority, read_epoch, HarvestAuthorityUpdate,
    HARVEST_AUTHORITY_PENDING_REL, HARVEST_ROOT_RESETS_REL,
};
use cosmon_minisign_testkit::Operator;

#[test]
fn interrupted_authority_update_refuses_every_epoch_read() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".cosmon/state")).unwrap();
    std::fs::write(
        root.path().join(".cosmon/config.toml"),
        "[project]\nproject_id = \"authority-test\"\n",
    )
    .unwrap();
    let current = authority_state(root.path()).unwrap();
    let update = HarvestAuthorityUpdate {
        expected_policy: current.policy,
        expected_key_digest: current.key_digest,
        expected_epoch: current.epoch,
        policy: RemoteHarvestPolicy::Scoped,
        public_key: None,
        epoch: Some(GrantEpoch::from_u64(2)),
    };
    configure_harvest_authority(root.path(), &update).unwrap();
    assert_eq!(read_epoch(root.path()).unwrap().as_u64(), 2);

    // A process that dies between an epoch/root replacement and the final
    // policy write leaves this durable marker. The next effect cannot read
    // an apparently coherent epoch from the partially updated files.
    std::fs::write(
        root.path().join(HARVEST_AUTHORITY_PENDING_REL),
        "harvest-authority-update-v1\n",
    )
    .unwrap();
    let error = read_epoch(root.path()).unwrap_err().to_string();
    assert!(error.contains("harvest_facts_unavailable"));
    assert!(configure_harvest_authority(root.path(), &update).is_err());
}

#[test]
fn local_root_recovery_records_intent_before_replacement() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".cosmon/state")).unwrap();
    std::fs::write(
        root.path().join(".cosmon/config.toml"),
        "[project]\nproject_id = \"authority-test\"\n",
    )
    .unwrap();
    let first = authority_state(root.path()).unwrap();
    let first_key = Operator::from_seed(51).public_key_file();
    let install = HarvestAuthorityUpdate {
        expected_policy: first.policy,
        expected_key_digest: first.key_digest,
        expected_epoch: first.epoch,
        policy: RemoteHarvestPolicy::Sealed,
        public_key: Some(first_key),
        epoch: None,
    };
    let installed = configure_harvest_authority(root.path(), &install).unwrap();
    let replacement = Operator::from_seed(52).public_key_file();
    let reset = HarvestAuthorityUpdate {
        expected_policy: installed.policy,
        expected_key_digest: installed.key_digest.clone(),
        expected_epoch: installed.epoch,
        policy: RemoteHarvestPolicy::Sealed,
        public_key: Some(replacement.clone()),
        epoch: Some(GrantEpoch::from_u64(2)),
    };
    configure_harvest_authority(root.path(), &reset).unwrap();
    let log = std::fs::read_to_string(root.path().join(HARVEST_ROOT_RESETS_REL)).unwrap();
    assert!(log.contains("harvest-root-reset-v1"));
    assert!(log.contains("epoch=2 intent=local"));
    assert_eq!(
        std::fs::read_to_string(root.path().join(".cosmon/harvest.pub")).unwrap(),
        replacement
    );
}
