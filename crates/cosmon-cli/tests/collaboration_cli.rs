// SPDX-License-Identifier: AGPL-3.0-only

//! `cs collaboration` provisions, inspects and revokes the bindings a network
//! boundary admits against; the proof leaves the host exactly once.

use std::path::Path;
use std::process::{Command, Output};

use cosmon_core::collaboration::{
    AdmissionRequest, AdmittedIdentity, AttachmentId, AttachmentProof, CollaborationRefusal,
    CollaborationScope, CollaborationTarget,
};
use cosmon_core::id::MoleculeId;
use cosmon_state::collaboration::{CollaborationBindingReader, CollaborationStoreError};
use serde_json::Value;

const OWNER: &str = "task-20261002-aaaa";

fn cs(state: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cs"))
        .arg("--config")
        .arg(state)
        .arg("collaboration")
        .args(args)
        .output()
        .unwrap()
}

fn bind_args<'a>(subject: &'a str, seat: &'a str, proof: &'a str) -> Vec<&'a str> {
    vec![
        "bind",
        "--issuer",
        "https://idp",
        "--subject",
        subject,
        "--audience",
        "cosmon-rpp-demo",
        "--tenant",
        "demo",
        "--work-owner",
        OWNER,
        "--seat",
        seat,
        "--scope",
        "cosmon:work:write",
        "--proof-out",
        proof,
    ]
}

fn json(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn admit(
    state: &Path,
    subject: &str,
    attachment: &str,
    proof: &AttachmentProof,
) -> Result<String, CollaborationStoreError> {
    let identity =
        AdmittedIdentity::new("https://idp", subject, "cosmon-rpp-demo", "demo").unwrap();
    CollaborationBindingReader::new(state)
        .admit(&AdmissionRequest {
            identity: &identity,
            attachment: &AttachmentId::new(attachment).unwrap(),
            proof,
            target: &CollaborationTarget::Work {
                owner: MoleculeId::new(OWNER).unwrap(),
            },
            required: CollaborationScope::WorkWrite,
        })
        .map(|admitted| admitted.work_caller().unwrap().seat().to_string())
}

#[test]
fn bind_admit_and_revoke_through_the_host_command() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join(".cosmon/state");
    std::fs::create_dir_all(&state).unwrap();
    let proof_path = root.path().join("review.proof");
    let proof_arg = proof_path.to_str().unwrap();

    let bound = cs(&state, &bind_args("pilot-b", "review", proof_arg));
    let stdout = String::from_utf8_lossy(&bound.stdout).to_string();
    let binding = json(&bound);
    assert_eq!(binding["capability"]["seat"], "review");
    assert_eq!(binding["active"], true);
    assert!(binding.get("verifier").is_none());

    let proof_text = std::fs::read_to_string(&proof_path).unwrap();
    let proof = AttachmentProof::parse(&proof_text).unwrap();
    assert!(!stdout.contains(proof.expose_secret()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&proof_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    let attachment = binding["attachment"].as_str().unwrap();
    assert_eq!(
        admit(&state, "pilot-b", attachment, &proof).unwrap(),
        "review"
    );
    assert!(matches!(
        admit(&state, "pilot-a", attachment, &proof),
        Err(CollaborationStoreError::Refused(
            CollaborationRefusal::BindingMissing
        ))
    ));

    let id = binding["id"].as_str().unwrap();
    let revoked = json(&cs(&state, &["revoke", id]));
    assert_eq!(revoked["active"], false);
    assert_eq!(revoked["revision"], 2);
    assert!(matches!(
        admit(&state, "pilot-b", attachment, &proof),
        Err(CollaborationStoreError::Refused(
            CollaborationRefusal::BindingRevoked
        ))
    ));
    assert_eq!(json(&cs(&state, &["show", id]))["active"], false);
    let listed = cs(&state, &["list"]);
    assert!(listed.status.success());
    assert_eq!(String::from_utf8_lossy(&listed.stdout).lines().count(), 1);
}

#[test]
fn bind_never_overwrites_a_proof_file_and_refuses_mismatched_scopes() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join(".cosmon/state");
    std::fs::create_dir_all(&state).unwrap();
    let proof_path = root.path().join("existing.proof");
    std::fs::write(&proof_path, "keep me").unwrap();

    let refused = cs(
        &state,
        &bind_args("pilot-b", "review", proof_path.to_str().unwrap()),
    );
    assert!(!refused.status.success());
    assert_eq!(std::fs::read_to_string(&proof_path).unwrap(), "keep me");
    assert!(!state.join("collaboration/bindings").exists());

    let fresh = root.path().join("mission.proof");
    let wrong_family = cs(
        &state,
        &[
            "bind",
            "--issuer",
            "https://idp",
            "--subject",
            "pilot-b",
            "--audience",
            "cosmon-rpp-demo",
            "--tenant",
            "demo",
            "--mission",
            OWNER,
            "--scope",
            "cosmon:work:write",
            "--proof-out",
            fresh.to_str().unwrap(),
        ],
    );
    assert!(!wrong_family.status.success());
    assert!(
        !fresh.exists(),
        "a refused bind must not leave a proof file"
    );
    assert!(CollaborationBindingReader::new(&state)
        .load()
        .unwrap()
        .is_empty());
}
