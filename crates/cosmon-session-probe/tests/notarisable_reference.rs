// SPDX-License-Identifier: Apache-2.0

//! Experimental proof for content-addressed provider-log segment references.

use std::path::{Path, PathBuf};

use cosmon_session_probe::{SegmentReference, SegmentResolution};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn provider_fixtures(provider: &str) -> Vec<PathBuf> {
    let root = fixture_root().join(provider);
    let mut found = Vec::new();
    let mut pending = vec![root];
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            let mut children: Vec<_> = std::fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            children.sort();
            pending.extend(children);
        } else if path.is_file() {
            found.push(path);
        }
    }
    found.sort();
    found
}

fn copy_fixture(path: &Path) -> (tempfile::TempDir, PathBuf, Vec<u8>) {
    let temp = tempfile::tempdir().unwrap();
    let live = temp.path().join("provider.log");
    let bytes = std::fs::read(path).unwrap();
    std::fs::write(&live, &bytes).unwrap();
    (temp, live, bytes)
}

#[test]
fn claude_and_codex_fixture_bytes_have_location_independent_identity() {
    for provider in ["claude", "codex"] {
        let fixtures = provider_fixtures(provider);
        assert!(!fixtures.is_empty(), "{provider} fixtures must exist");
        for fixture in fixtures {
            let bytes = std::fs::read(&fixture).unwrap();
            let reference = SegmentReference::from_bytes(&bytes);
            assert_eq!(
                reference.verify(Some(&bytes)),
                SegmentResolution::Verified,
                "{}",
                fixture.display()
            );

            let temp = tempfile::tempdir().unwrap();
            let relocated = temp.path().join("renamed.log");
            std::fs::write(&relocated, &bytes).unwrap();
            let relocated_bytes = std::fs::read(relocated).unwrap();
            assert_eq!(
                reference.verify(Some(&relocated_bytes)),
                SegmentResolution::Verified,
                "a path change must not change content identity"
            );
        }
    }
}

#[test]
fn rotation_preserves_identity_only_while_the_old_generation_remains_resolvable() {
    for provider in ["claude", "codex"] {
        let fixture = provider_fixtures(provider).remove(0);
        let (_temp, live, original) = copy_fixture(&fixture);
        let reference = SegmentReference::from_bytes(&original);
        let rotated = live.with_extension("log.1");

        std::fs::rename(&live, &rotated).unwrap();
        std::fs::write(&live, vec![b'x'; original.len()]).unwrap();

        let replacement = std::fs::read(&live).unwrap();
        assert!(matches!(
            reference.verify(Some(&replacement)),
            SegmentResolution::Mismatch { .. }
        ));
        let old_generation = std::fs::read(&rotated).unwrap();
        assert_eq!(
            reference.verify(Some(&old_generation)),
            SegmentResolution::Verified
        );
    }
}

#[test]
fn truncation_is_detected_but_the_lost_suffix_is_not_recoverable() {
    for provider in ["claude", "codex"] {
        let fixture = provider_fixtures(provider).remove(0);
        let (_temp, live, original) = copy_fixture(&fixture);
        let reference = SegmentReference::from_bytes(&original);

        std::fs::write(&live, &original[..original.len() / 2]).unwrap();
        let truncated = std::fs::read(&live).unwrap();
        assert!(matches!(
            reference.verify(Some(&truncated)),
            SegmentResolution::Mismatch { .. }
        ));
    }
}

#[test]
fn deletion_leaves_a_stable_identifier_but_no_retrievable_referent() {
    for provider in ["claude", "codex"] {
        let fixture = provider_fixtures(provider).remove(0);
        let (_temp, live, original) = copy_fixture(&fixture);
        let reference = SegmentReference::from_bytes(&original);
        let persisted = serde_json::to_string(&reference).unwrap();

        std::fs::remove_file(&live).unwrap();
        let restored: SegmentReference = serde_json::from_str(&persisted).unwrap();

        assert_eq!(restored, reference, "the identifier survives restart");
        assert_eq!(
            restored.verify(None),
            SegmentResolution::Missing,
            "the identifier cannot reconstruct erased bytes"
        );
    }
}

#[test]
fn the_claimed_cursor_editor_fixture_is_not_present_in_the_m1_fixture_set() {
    assert!(
        !fixture_root().join("cursor").exists(),
        "remove this finding and extend the provider matrix when a Cursor editor fixture lands"
    );
}
