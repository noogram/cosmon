// SPDX-License-Identifier: AGPL-3.0-only

//! Filesystem publication for formula-declared worker response artifacts.
//!
//! The pure decision lives in `cosmon_core::worker_acceptance`; this module is
//! the effect boundary that keeps final text inside canonical molecule custody,
//! preserves replaced attempts, and mirrors the tenant's single-result contract.

use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::Write as IoWrite;
use std::path::{Path, PathBuf};

use anyhow::Context;
use sha2::{Digest, Sha256};

/// Durable receipt for a published final-text deliverable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedResponseArtifact {
    /// Canonical destination under the molecule directory.
    pub path: PathBuf,
    /// Lowercase SHA-256 digest of the exact published bytes.
    pub sha256: String,
}

/// Publish non-empty final text at a formula-declared relative destination.
///
/// Existing different bytes are copied into `.response-attempts/` before the
/// canonical destination is replaced. Existing ancestors and the destination
/// itself must not be symlinks. Publication uses a same-directory tempfile and
/// atomic persist, so a crash cannot expose a partial accepted result.
pub fn publish_response_artifact(
    molecule_dir: &Path,
    relative: &str,
    response: &str,
) -> anyhow::Result<PublishedResponseArtifact> {
    if response.trim().is_empty() {
        anyhow::bail!("declared response artifact is empty");
    }
    if !cosmon_core::worker_acceptance::validate_response_artifact_path(relative) {
        anyhow::bail!("invalid response_artifact path {relative:?}");
    }

    fs::create_dir_all(molecule_dir).with_context(|| {
        format!(
            "create canonical molecule directory {}",
            molecule_dir.display()
        )
    })?;
    let canonical_root = molecule_dir
        .canonicalize()
        .with_context(|| format!("canonicalize molecule directory {}", molecule_dir.display()))?;
    let relative_path = Path::new(relative);
    let parent_relative = relative_path.parent().unwrap_or_else(|| Path::new(""));
    let mut parent = canonical_root.clone();
    for component in parent_relative.components() {
        parent.push(component.as_os_str());
        match fs::symlink_metadata(&parent) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!(
                    "response_artifact ancestor is a symlink: {}",
                    parent.display()
                );
            }
            Ok(metadata) if !metadata.is_dir() => {
                anyhow::bail!(
                    "response_artifact ancestor is not a directory: {}",
                    parent.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&parent).with_context(|| {
                    format!("create response artifact parent {}", parent.display())
                })?;
            }
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", parent.display()))
            }
        }
    }

    let destination = canonical_root.join(relative_path);
    if let Ok(metadata) = fs::symlink_metadata(&destination) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            anyhow::bail!(
                "response_artifact destination is not a regular file: {}",
                destination.display()
            );
        }
        let previous = fs::read(&destination)
            .with_context(|| format!("read prior response artifact {}", destination.display()))?;
        if previous != response.as_bytes() {
            preserve_prior_attempt(&canonical_root, relative_path, &previous)?;
        }
    }

    atomic_write(&destination, response.as_bytes())?;
    if let Ok(dir) = std::env::var("COSMON_ARTIFACT_DIR") {
        if !dir.is_empty() {
            let result_dir = PathBuf::from(dir);
            fs::create_dir_all(&result_dir).with_context(|| {
                format!("create tenant artifact directory {}", result_dir.display())
            })?;
            atomic_write(&result_dir.join("result.md"), response.as_bytes())?;
        }
    }

    Ok(PublishedResponseArtifact {
        path: destination,
        sha256: hex_digest(response.as_bytes()),
    })
}

fn preserve_prior_attempt(root: &Path, relative: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let attempts = root.join(".response-attempts");
    match fs::symlink_metadata(&attempts) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            anyhow::bail!(
                "response-attempt custody is not a regular directory: {}",
                attempts.display()
            );
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&attempts).with_context(|| {
                format!("create response-attempt custody {}", attempts.display())
            })?;
        }
        Err(error) => {
            return Err(error).with_context(|| format!("inspect {}", attempts.display()));
        }
    }
    let name = relative
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("response");
    let path = attempts.join(format!("{}-{}", &hex_digest(bytes)[..16], name));
    if !path.exists() {
        atomic_write(&path, bytes)?;
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path has no parent: {}", path.display()))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create temporary file under {}", parent.display()))?;
    temp.write_all(bytes)
        .with_context(|| format!("write temporary response for {}", path.display()))?;
    temp.as_file()
        .sync_all()
        .with_context(|| format!("sync temporary response for {}", path.display()))?;
    temp.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("publish response artifact {}", path.display()))?;
    Ok(())
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishes_atomically_and_preserves_replaced_attempt() {
        let root = tempfile::tempdir().expect("molecule dir");
        publish_response_artifact(root.path(), "result.md", "first").expect("first publish");
        let receipt =
            publish_response_artifact(root.path(), "result.md", "second").expect("second publish");
        assert_eq!(fs::read_to_string(receipt.path).expect("result"), "second");
        assert_eq!(
            fs::read_dir(root.path().join(".response-attempts"))
                .expect("attempts")
                .count(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("molecule dir");
        let outside = tempfile::tempdir().expect("outside dir");
        symlink(outside.path(), root.path().join("escape")).expect("symlink");
        let error = publish_response_artifact(root.path(), "escape/result.md", "payload")
            .expect_err("symlink ancestor must fail");
        assert!(error.to_string().contains("symlink"), "{error:#}");
        assert!(!outside.path().join("result.md").exists());
    }
}
