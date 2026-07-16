// SPDX-License-Identifier: AGPL-3.0-only

//! Frozen-client `config.toml` round-trip gate
//! — « we do not break userspace ».
//!
//! A real tenant has profile files on disk written by the pre-fusion
//! binary. The fixtures under `tests/fixtures/profile-store/` are the
//! bytes that binary wrote (blessed pre-fusion via `UPDATE_GOLDENS=1`);
//! the post-fusion binary must (a) still parse them and (b) re-emit
//! byte-identical files when it writes them back. Any drift in the
//! profile store serialisation is a breaking change for installed
//! tenants and must fail here.

use cosmon_remote::config::{Profile, ProfileStore, TopConfig};
use std::path::PathBuf;

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("profile-store")
}

/// The representative tenant profile: every field a served `install.sh`
/// or an operator `config set` can populate is exercised.
fn representative_profile() -> Profile {
    let mut p = Profile::from_host("https://cosmon.example.ts.net");
    p.set("sub", "tenant-example".into()).unwrap();
    p.set("aud", "cosmon-rpp".into()).unwrap();
    p.set("oidc-url", "https://cosmon.example.ts.net/oidc".into())
        .unwrap();
    p.set("noyau", "tenant-example-prod".into()).unwrap();
    p.set("timeout", "45".into()).unwrap();
    p.set("artifacts-dir", "/home/tenant/cosmon-artifacts".into())
        .unwrap();
    p
}

const PROFILE_NAME: &str = "example-ts-net";

/// Pin the serialised shape: writing the representative profile through
/// the store must reproduce the pre-fusion fixture bytes exactly.
#[test]
fn profile_store_writes_frozen_byte_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ProfileStore::at(tmp.path());
    store
        .write_profile(PROFILE_NAME, &representative_profile())
        .unwrap();
    store
        .write_top(&TopConfig {
            default_profile: Some(PROFILE_NAME.to_owned()),
            credit_guard_acknowledged: None,
        })
        .unwrap();

    let fixtures = fixtures_root();
    let bless = std::env::var_os("UPDATE_GOLDENS").is_some();
    for (live, fixture) in [
        (
            store.profile_path(PROFILE_NAME),
            fixtures
                .join("profiles")
                .join(format!("{PROFILE_NAME}.toml")),
        ),
        (store.top_path(), fixtures.join("config.toml")),
    ] {
        let bytes = std::fs::read(&live).unwrap();
        if bless {
            std::fs::create_dir_all(fixture.parent().unwrap()).unwrap();
            std::fs::write(&fixture, &bytes).unwrap();
            continue;
        }
        let expected = std::fs::read(&fixture).unwrap_or_else(|_| {
            panic!(
                "missing fixture {} — bless once with UPDATE_GOLDENS=1",
                fixture.display()
            )
        });
        assert_eq!(
            bytes,
            expected,
            "profile store serialisation drifted from the frozen tenant fixture {}",
            fixture.display()
        );
    }
}

/// Round-trip: read the frozen fixtures (as a tenant upgrade would),
/// write them back unchanged — bytes must be identical.
#[test]
fn frozen_config_round_trips_byte_identical() {
    let fixtures = fixtures_root();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("profiles")).unwrap();
    for rel in [
        format!("profiles/{PROFILE_NAME}.toml"),
        "config.toml".to_owned(),
    ] {
        std::fs::copy(fixtures.join(&rel), root.join(&rel)).unwrap();
    }

    let store = ProfileStore::at(root);
    let profile = store.read_profile(PROFILE_NAME).unwrap();
    let top_cfg = store.read_top().unwrap();
    store.write_profile(PROFILE_NAME, &profile).unwrap();
    store.write_top(&top_cfg).unwrap();

    for rel in [
        format!("profiles/{PROFILE_NAME}.toml"),
        "config.toml".to_owned(),
    ] {
        assert_eq!(
            std::fs::read(root.join(&rel)).unwrap(),
            std::fs::read(fixtures.join(&rel)).unwrap(),
            "round-trip of {rel} is not byte-identical — profile store changed",
        );
    }
}
