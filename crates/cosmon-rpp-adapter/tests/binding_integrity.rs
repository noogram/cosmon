// SPDX-License-Identifier: AGPL-3.0-only

//! Binding integrity at boot and on operator reload.

use std::fs;
use std::path::Path;

use cosmon_rpp_adapter::image_init::ImageInit;
use cosmon_rpp_adapter::nucleon_map::{HabilitationMap, SharedHabilitationMap};
use cosmon_rpp_adapter::reload;

const ISSUER: &str = "https://issuer.example";
const AUDIENCE: &str = "tenant-audience";

fn binding(root: &Path, dir: &str, file: &str, sub: &str, noyau: &str) {
    let path = root.join("nucleons").join(dir);
    fs::create_dir_all(&path).unwrap();
    fs::write(
        path.join(file),
        format!("nucleon_id = {dir:?}\nphase = \"Biological\"\nnoyau = {noyau:?}\n[oidc]\nissuer = {ISSUER:?}\nsub = {sub:?}\naudience = {AUDIENCE:?}\n"),
    )
    .unwrap();
}

#[test]
fn duplicate_binding_is_refused_at_boot_across_directories_and_files() {
    for second_file in [false, true] {
        let td = tempfile::tempdir().unwrap();
        binding(
            td.path(),
            "binding-1",
            "oidc-identity.toml",
            "alice",
            "tenant-1",
        );
        let dir = if second_file {
            "binding-1"
        } else {
            "binding-2"
        };
        binding(
            td.path(),
            dir,
            "oidc-identity-extra.toml",
            "alice",
            "tenant-2",
        );
        let error = HabilitationMap::load(td.path()).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("duplicate binding"));
    }
}

#[test]
fn duplicate_binding_on_reload_keeps_the_live_map() {
    let td = tempfile::tempdir().unwrap();
    binding(
        td.path(),
        "binding-1",
        "oidc-identity.toml",
        "alice",
        "tenant-1",
    );
    let shared = SharedHabilitationMap::new(HabilitationMap::load(td.path()).unwrap());
    binding(
        td.path(),
        "binding-2",
        "oidc-identity.toml",
        "alice",
        "tenant-2",
    );
    let image_init = ImageInit {
        inbox_root: td.path().join("inbox"),
        galaxies_root: td.path().join("galaxies"),
        claude_home: td.path().join("home"),
        formulas_seed_dir: None,
    };
    let outcome = reload::reload(&shared, td.path(), &image_init);
    assert!(outcome
        .error
        .as_deref()
        .is_some_and(|e| e.contains("duplicate binding")));
    assert_eq!(outcome.bindings_before, 1);
    assert_eq!(outcome.bindings_after, 1);
    assert_eq!(
        shared
            .load()
            .resolve_for_audience(ISSUER, "alice", AUDIENCE)
            .unwrap()
            .noyau
            .as_str(),
        "tenant-1"
    );
}

#[test]
fn operator_docs_do_not_claim_an_unrecorded_binding_seal() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let adr =
        fs::read_to_string(repo.join("docs/adr/080-remote-pilot-port-https-oidc.md")).unwrap();
    let guide =
        fs::read_to_string(repo.join("docs/book/src/how-to/deploy-remote-service.md")).unwrap();
    assert!(!adr.contains("a BLAKE3 hash of its content is stored in `state.json`"));
    assert!(guide.contains("Binding files are operator-controlled configuration"));
}
