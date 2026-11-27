// SPDX-License-Identifier: AGPL-3.0-only

//! Explicit resolution of a *prebuilt* sibling binary for integration tests.
//!
//! # Why this module exists
//!
//! `cs` belongs to `cosmon-cli`, a sibling workspace member, so cargo hands
//! this crate no `CARGO_BIN_EXE_cs`. The previous resolver papered over that
//! by shelling out to `cargo build -p cosmon-cli --bin cs` whenever the
//! binary happened to be missing.
//!
//! That fallback is unsound under a parallel runner, and the failure is not
//! hypothetical: `cargo test --workspace` runs every test *target* in
//! parallel, so several test binaries can miss the artifact at the same
//! instant and each spawn its own nested `cargo`. Those nested invocations
//! then serialize on cargo's build lock — which the outer `cargo test` is
//! *already holding* in some layouts — so what a reader sees is not a slow
//! test but a test that never returns. Even when it does return, the
//! artifact it produced is being written while a sibling test is executing
//! it, and the timing of that race is set by the runner's scheduling rather
//! than by anything in the test.
//!
//! The resolver honours `COSMON_TEST_CS_BIN` first. Otherwise it looks
//! for the final artifact under Cargo's target directory, with the test
//! executable's profile directory as a fallback.

#[path = "../../../../tests/support/prebuilt.rs"]
mod shared;

use std::path::{Path, PathBuf};

const CS: shared::Prebuilt = shared::Prebuilt {
    override_env: "COSMON_TEST_CS_BIN",
    name: "cs",
    path: "cs",
    build_command: "cargo build -p cosmon-cli --bin cs",
};

/// Locate the prebuilt `cs` binary. A missing prerequisite fails the test
/// with the paths searched and the command to build the binary.
pub fn cs_bin() -> PathBuf {
    match CS.resolve(Path::new(env!("CARGO_MANIFEST_DIR"))) {
        Ok(path) => path,
        Err(msg) => panic!("{msg}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn file(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"fixture").unwrap();
    }

    #[test]
    fn prebuilt_is_found_with_a_separate_build_dir() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let manifest = workspace.join("crates/cosmon-api");
        let exe = temp
            .path()
            .join("cache/debug/build/cosmon-api/hash/out/smoke-hash");
        let binary = workspace.join("target/debug/cs");
        file(&exe);
        file(&binary);

        assert_eq!(
            CS.resolve_from(&manifest, Some(&exe), None, None, None),
            Ok(binary)
        );
    }

    #[test]
    fn configured_target_dir_wins_over_default_target_and_legacy_paths() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let manifest = workspace.join("crates/cosmon-api");
        let cache_profile = temp.path().join("build/project/cache/release");
        let exe = cache_profile.join("build/cosmon-api/hash/out/smoke-hash");
        let custom_target = workspace.join("custom-target");
        let binary = custom_target.join("release/cs");
        for path in [
            workspace.join("target/release/cs"),
            cache_profile.join("cs"),
            binary.clone(),
        ] {
            file(&path);
        }

        for custom in [&custom_target, Path::new("custom-target")] {
            assert_eq!(
                CS.resolve_from(&manifest, Some(&exe), Some(custom), None, None),
                Ok(binary.clone()),
            );
        }
        assert_eq!(
            CS.resolve_from(&manifest, Some(&exe), None, None, None),
            Ok(workspace.join("target/release/cs")),
        );
    }

    #[test]
    fn relative_target_dir_uses_the_absolute_profile_cargo_passed_to_the_test() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let manifest = workspace.join("crates/cosmon-api");
        let exe = temp.path().join("cache/debug/deps/smoke-hash");
        let relative = Path::new("../temporary/../member-target");
        let intended_profile = workspace.join("crates/member-target/debug");
        let binary = intended_profile.join("cs");
        let decoy = workspace.join("other-target/debug");
        file(&binary);
        file(&decoy.join("cs"));
        let cargo_library_path = std::env::join_paths([
            Path::new("member-target/debug"),
            decoy.as_path(),
            intended_profile.as_path(),
        ])
        .unwrap();

        assert_eq!(
            CS.resolve_from(
                &manifest,
                Some(&exe),
                Some(relative),
                None,
                Some(&cargo_library_path),
            ),
            Ok(binary),
        );
    }

    #[test]
    fn legacy_directory_is_checked_when_the_final_artifact_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = temp.path().join("workspace/crates/cosmon-api");
        let profile = temp.path().join("custom/debug");
        let exe = profile.join("deps/smoke-hash");
        let binary = profile.join("cs");
        file(&binary);

        assert_eq!(
            CS.resolve_from(&manifest, Some(&exe), None, None, None),
            Ok(binary)
        );
    }

    #[test]
    fn missing_cs_names_both_paths_and_the_build_prerequisite() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let manifest = workspace.join("crates/cosmon-api");
        let profile = temp.path().join("cache/debug");
        let exe = profile.join("deps/smoke-hash");

        let err = CS
            .resolve_from(&manifest, Some(&exe), None, None, None)
            .unwrap_err();
        for path in [workspace.join("target/debug/cs"), profile.join("cs")] {
            assert!(err.contains(&format!("`{}`", path.display())), "{err}");
        }
        assert!(err.contains("cargo build -p cosmon-cli --bin cs"), "{err}");
        assert!(err.contains("COSMON_TEST_CS_BIN"), "{err}");
    }

    #[test]
    fn explicit_override_wins_and_an_invalid_override_fails() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let manifest = workspace.join("crates/cosmon-api");
        let exe = workspace.join("target/debug/deps/smoke-hash");
        let fallback = workspace.join("target/debug/cs");
        let explicit = temp.path().join("explicit-cs");
        file(&fallback);
        file(&explicit);

        assert_eq!(
            CS.resolve_from(&manifest, Some(&exe), None, Some(explicit.clone()), None),
            Ok(explicit.clone()),
        );
        assert_eq!(
            CS.resolve_from(&manifest, None, None, Some(explicit.clone()), None),
            Ok(explicit),
        );
        let bad_override = temp.path().join("missing-cs");
        for invalid in [bad_override.as_path(), temp.path()] {
            let err = CS
                .resolve_from(
                    &manifest,
                    Some(&exe),
                    None,
                    Some(invalid.to_path_buf()),
                    None,
                )
                .unwrap_err();
            assert!(err.contains("COSMON_TEST_CS_BIN"), "{err}");
            assert!(err.contains(&format!("`{}`", invalid.display())), "{err}");
        }
    }

    #[test]
    fn missing_test_executable_path_still_gives_a_prerequisite_error() {
        let manifest = Path::new("/workspace/crates/cosmon-api");
        let err = CS
            .resolve_from(manifest, None, None, None, None)
            .unwrap_err();
        assert!(
            err.contains("no Cargo profile directory could be inferred"),
            "{err}"
        );
        assert!(err.contains("COSMON_TEST_CS_BIN"), "{err}");
    }
}
