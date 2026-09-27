// SPDX-License-Identifier: AGPL-3.0-only

//! Regression coverage for the server image's zero-edit trust bootstrap.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use cosmon_rpp_adapter::config::RppConfig;
use tempfile::TempDir;

const SHIPPED_CONFIG: &str = include_str!("../deploy/rpp.toml");
const IMAGE_RECIPE: &str = include_str!("../Dockerfile");

fn run_trust_converge(config: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cosmon-rpp-adapter"))
        .args([
            "--config",
            &config.display().to_string(),
            "trust",
            "converge",
        ])
        .env_remove("TRUSTED_ISS")
        .env_remove("TRUSTED_JWKS_URI")
        .env_remove("TRUSTED_AUDIENCES")
        .env_remove("TRUSTED_FORCE")
        .output()
        .expect("the adapter binary must start")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "trust convergence failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn shipped_image_config_bootstraps_a_handoff_binding() {
    let shipped: RppConfig = toml::from_str(SHIPPED_CONFIG).expect("shipped rpp.toml must parse");
    assert_eq!(
        shipped.trust_bootstrap.handoff_dir,
        Some(PathBuf::from("/cosmon/handoff")),
        "the image config must opt into the handoff directory it creates",
    );
    assert!(
        IMAGE_RECIPE
            .contains("crates/cosmon-rpp-adapter/deploy/rpp.toml /cosmon/.config/cosmon/rpp.toml"),
        "the runtime image must contain the shipped rpp.toml",
    );

    // Relocate only the container filesystem roots into a disposable root;
    // every other byte remains the config shipped by the image.
    let root = TempDir::new().expect("temporary image root");
    let state_dir = root.path().join("state");
    let handoff_dir = root.path().join("handoff");
    std::fs::create_dir_all(&handoff_dir).expect("create handoff mount");
    std::fs::write(
        handoff_dir.join("issuer.toml"),
        "schema = \"cosmon-issuer-handoff/v1\"\n\
         [issuer]\n\
         iss = \"https://identity.example.test\"\n\
         jwks_uri = \"http://identity:3000/keys\"\n\
         audiences = [\"rpp-client\"]\n\
         [binding]\n\
         noyau = \"sandbox\"\n\
         nucleon_id = \"operator-demo\"\n\
         sub = \"operator-subject\"\n\
         audience = \"rpp-client\"\n",
    )
    .expect("write handoff");

    let relocated = SHIPPED_CONFIG
        .replace(
            "state_dir = \"/cosmon/.cosmon/state\"",
            &format!("state_dir = {:?}", state_dir.display().to_string()),
        )
        .replace(
            "handoff_dir = \"/cosmon/handoff\"",
            &format!("handoff_dir = {:?}", handoff_dir.display().to_string()),
        );
    let config = root.path().join("rpp.toml");
    std::fs::write(&config, relocated).expect("write relocated image config");

    assert_success(&run_trust_converge(&config));

    let binding =
        std::fs::read_to_string(state_dir.join("nucleons/operator-demo/oidc-identity.toml"))
            .expect("boot must apply the handoff binding");
    assert!(binding.contains("sub = \"operator-subject\""));
    assert!(binding.contains("audience = \"rpp-client\""));

    // Deployments without a producer remain valid: both an empty mounted
    // directory and an absent mount are immediate, successful no-ops.
    std::fs::remove_file(handoff_dir.join("issuer.toml")).expect("empty handoff directory");
    assert_success(&run_trust_converge(&config));
    std::fs::remove_dir(&handoff_dir).expect("remove handoff directory");
    assert_success(&run_trust_converge(&config));
}
