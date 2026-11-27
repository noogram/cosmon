// SPDX-License-Identifier: AGPL-3.0-only

//! Locate final Cargo artifacts from workspace integration tests.

use std::env;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

/// A binary that the caller or workspace test run must build in advance.
pub(crate) struct Prebuilt {
    /// Environment variable for an explicitly selected binary.
    pub(crate) override_env: &'static str,
    /// Binary name for diagnostics.
    pub(crate) name: &'static str,
    /// Binary path relative to Cargo's final profile directory.
    pub(crate) path: &'static str,
    /// Command to suggest when the binary is missing.
    pub(crate) build_command: &'static str,
}

impl Prebuilt {
    /// Resolve from Cargo's test environment without spawning a build.
    pub(crate) fn resolve(&self, manifest_dir: &Path) -> Result<PathBuf, String> {
        let library_path_var = if cfg!(windows) {
            "PATH"
        } else if cfg!(target_os = "macos") {
            "DYLD_FALLBACK_LIBRARY_PATH"
        } else if cfg!(target_os = "aix") {
            "LIBPATH"
        } else {
            "LD_LIBRARY_PATH"
        };
        self.resolve_from(
            manifest_dir,
            env::current_exe().ok().as_deref(),
            env::var_os("CARGO_TARGET_DIR").as_deref().map(Path::new),
            env::var_os(self.override_env).map(PathBuf::from),
            env::var_os(library_path_var).as_deref(),
        )
    }

    /// Resolve from explicit inputs so tests can supply layouts and overrides
    /// without changing the process environment.
    pub(crate) fn resolve_from(
        &self,
        manifest_dir: &Path,
        test_exe: Option<&Path>,
        target_dir: Option<&Path>,
        override_path: Option<PathBuf>,
        cargo_library_path: Option<&OsStr>,
    ) -> Result<PathBuf, String> {
        let Self {
            override_env,
            name,
            path,
            build_command,
        } = self;
        if let Some(explicit) = override_path {
            if explicit.is_file() {
                return Ok(explicit);
            }
            return Err(format!(
                "{override_env} is set to `{}`, but no file exists there. \
                 Point it at a prebuilt `{name}` binary, or unset it to search Cargo's profile directories.",
                explicit.display(),
            ));
        }

        let legacy_dir = test_exe.and_then(profile_dir_of);
        // This workspace keeps its member manifests under crates/<member>.
        let workspace_dir = manifest_dir.ancestors().nth(2);
        let final_dir =
            legacy_dir
                .and_then(Path::file_name)
                .zip(workspace_dir)
                .map(|(profile, workspace)| {
                    target_dir
                        .filter(|dir| dir.is_relative())
                        .zip(cargo_library_path)
                        .and_then(|(dir, paths)| relative_target_profile(dir, profile, paths))
                        .unwrap_or_else(|| {
                            workspace
                                .join(target_dir.unwrap_or(Path::new("target")))
                                .join(profile)
                        })
                });

        let mut tried = Vec::new();
        let dirs = final_dir
            .as_deref()
            .into_iter()
            .chain(legacy_dir.filter(|dir| Some(*dir) != final_dir.as_deref()));
        for dir in dirs {
            let candidate = dir.join(path);
            if candidate.is_file() {
                return Ok(candidate);
            }
            tried.push(format!("`{}`", candidate.display()));
        }

        let locations = if tried.is_empty() {
            "no Cargo profile directory could be inferred from the test executable".to_owned()
        } else {
            format!("tried {}", tried.join(", "))
        };
        Err(format!(
            "prebuilt `{name}` binary not found ({locations}). \
             Integration tests never build it implicitly. Build it first with \
             `{build_command}`, run `cargo test --workspace`, or set \
             {override_env} to an existing binary.",
        ))
    }
}

/// Cargo resolves a relative CARGO_TARGET_DIR from its invocation directory,
/// then runs the test from the package directory. Its library search path
/// includes the absolute final profile directory. Match the part of the path
/// that is known without the invocation directory.
fn relative_target_profile(target_dir: &Path, profile: &OsStr, paths: &OsStr) -> Option<PathBuf> {
    let mut suffix = PathBuf::new();
    for component in target_dir.components() {
        match component {
            Component::Normal(segment) => suffix.push(segment),
            Component::ParentDir => {
                suffix.pop();
            }
            _ => {}
        }
    }
    suffix.push(profile);
    env::split_paths(paths).find(|path| path.is_absolute() && path.ends_with(&suffix))
}

/// Extract the profile from either Cargo's `<profile>/deps/<test>` layout or
/// its `<profile>/build/<package>/<hash>/out/<test>` layout. This locates the
/// build directory, which can differ from the final artifact directory.
fn profile_dir_of(exe: &Path) -> Option<&Path> {
    exe.parent()?
        .ancestors()
        .find(|dir| dir.file_name().is_some_and(|n| n == "deps" || n == "build"))
        .and_then(Path::parent)
}
