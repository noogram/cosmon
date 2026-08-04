// SPDX-License-Identifier: AGPL-3.0-only

//! The I/O half of `cs spore install`: fetching a bundle and copying it.
//!
//! The decidable-without-a-disk half — the `<SOURCE>` grammar, the path
//! containment rules, the registry three-way decision — lives in
//! [`cosmon_core::spore::install`]. What is left here is the part that must
//! touch the world: a `git` subprocess, a temporary checkout, and a file copy.
//!
//! Two properties this module is responsible for, both about a bundle whose
//! bytes an operator did not write:
//!
//! - **Nothing outside the destination is written.** Every copied path is a
//!   relative path validated by
//!   [`check_bundle_path`](cosmon_core::spore::check_bundle_path) and joined
//!   onto the destination, and every entry is copied only if
//!   `symlink_metadata` says it is a regular file or a directory. A symlink in
//!   a fetched bundle is refused, not followed: following one would let
//!   `spore.toml -> /etc/passwd` place arbitrary bytes under a name the
//!   operator trusts, and `docs -> /` make the copy unbounded.
//! - **The temporary checkout outlives nothing.** The `TempDir` is owned by
//!   the returned `FetchedBundle`, so the clone is removed when the caller
//!   drops it, whether the install succeeded or refused.

use std::path::{Path, PathBuf};
use std::process::Command;

use cosmon_core::spore::{check_bundle_path, SporeSource};
use tempfile::TempDir;

/// A bundle sitting on local disk, ready to be read and copied.
pub struct FetchedBundle {
    /// The directory holding `spore.toml`.
    pub root: PathBuf,
    /// The commit the bundle was taken from, when it came from a git remote.
    /// Recorded in the provenance file so an installed bundle can say what it
    /// is a copy *of* — a branch name alone cannot.
    pub commit: Option<String>,
    /// Owns the temporary checkout, when there is one. Dropping the bundle
    /// removes it; a local source has `None` and is never touched.
    _checkout: Option<TempDir>,
}

/// Fetch a bundle from its source into a readable local directory.
///
/// `git_ref` and `subdir` override whatever the source string encoded, so a
/// generic git remote (which has no unambiguous way to spell either) is as
/// addressable as a GitHub URL.
///
/// # Errors
/// Bails when the local path does not exist, when `git` is missing or fails,
/// or when the requested subdirectory is absent from the checkout.
pub fn fetch(
    source: &SporeSource,
    git_ref: Option<&str>,
    subdir: Option<&str>,
) -> anyhow::Result<FetchedBundle> {
    match source {
        SporeSource::Local(path) => {
            let root = local_bundle_root(Path::new(path), subdir)?;
            Ok(FetchedBundle {
                root,
                commit: None,
                _checkout: None,
            })
        }
        SporeSource::Git {
            remote,
            git_ref: url_ref,
            subdir: url_subdir,
        } => {
            let wanted_ref = git_ref.or(url_ref.as_deref());
            let wanted_subdir = subdir.or(url_subdir.as_deref());
            clone_bundle(remote, wanted_ref, wanted_subdir)
        }
        // `SporeSource` is `#[non_exhaustive]`: a source kind added later must
        // fail loudly here rather than be silently mis-fetched as one of the
        // two this shell knows how to reach.
        other => anyhow::bail!("unsupported spore source {other:?}"),
    }
}

/// Resolve a local `<SOURCE>` (a directory or a `spore.toml`) plus an optional
/// subdirectory to the directory holding the manifest.
fn local_bundle_root(path: &Path, subdir: Option<&str>) -> anyhow::Result<PathBuf> {
    let base = if path.is_file() {
        path.parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    } else {
        path.to_path_buf()
    };
    let root = join_subdir(&base, subdir)?;
    if !root.is_dir() {
        anyhow::bail!("spore source {} is not a directory", root.display());
    }
    Ok(root)
}

/// Join a validated subdirectory onto a checkout root.
fn join_subdir(base: &Path, subdir: Option<&str>) -> anyhow::Result<PathBuf> {
    match subdir {
        None => Ok(base.to_path_buf()),
        Some(rel) => {
            check_bundle_path(rel)
                .map_err(|reason| anyhow::anyhow!("unsafe subdirectory `{rel}`: {reason}"))?;
            Ok(base.join(rel))
        }
    }
}

/// Shallow-fetch a git remote into a temporary checkout.
///
/// `init` + `fetch --depth 1` rather than `clone --branch`, because the same
/// three commands serve a branch, a tag, and a full commit sha (GitHub serves
/// a sha to `fetch` by object id; `clone --branch` cannot take one). `HEAD` is
/// the ref when the caller pinned nothing.
fn clone_bundle(
    remote: &str,
    git_ref: Option<&str>,
    subdir: Option<&str>,
) -> anyhow::Result<FetchedBundle> {
    let checkout = TempDir::new()
        .map_err(|e| anyhow::anyhow!("failed to create a temporary checkout: {e}"))?;
    let dir = checkout.path().to_path_buf();
    let reference = git_ref.unwrap_or("HEAD");

    git(&dir, &["init", "--quiet"])?;
    git(&dir, &["remote", "add", "origin", remote])?;
    git(
        &dir,
        &["fetch", "--depth", "1", "--quiet", "origin", reference],
    )
    .map_err(|e| anyhow::anyhow!("failed to fetch {reference} from {remote}: {e}"))?;
    git(&dir, &["checkout", "--quiet", "FETCH_HEAD"])?;

    let commit = git(&dir, &["rev-parse", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string());
    let root = join_subdir(&dir, subdir)?;
    if !root.is_dir() {
        anyhow::bail!(
            "{} holds no directory {} at {reference}",
            remote,
            subdir.unwrap_or(".")
        );
    }

    Ok(FetchedBundle {
        root,
        commit,
        _checkout: Some(checkout),
    })
}

/// Run one `git` command in `dir`, returning its stdout.
fn git(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run `git {}`: {e}", args.join(" ")))?;
    if !out.status.success() {
        anyhow::bail!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// What a directory holds, for the "destination is already occupied" check.
///
/// An empty directory is not an obstacle — `mkdir spores/x` then installing
/// into it is the ordinary thing — so occupancy is about *content*.
///
/// # Errors
/// Propagates the directory read.
pub fn is_occupied(dir: &Path) -> anyhow::Result<bool> {
    if !dir.exists() {
        return Ok(false);
    }
    if !dir.is_dir() {
        anyhow::bail!("{} exists and is not a directory", dir.display());
    }
    let mut entries = std::fs::read_dir(dir)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", dir.display()))?;
    Ok(entries.next().is_some())
}

/// Directories a bundle copy never carries into the destination: git's own
/// bookkeeping (huge, and meaningless once the bundle is a directory in
/// another repository).
const SKIPPED_DIRS: [&str; 1] = [".git"];

/// Copy a fetched bundle tree into `dest`, returning the relative paths copied
/// in sorted order.
///
/// Refuses on the first entry that is neither a regular file nor a directory.
/// That refusal is the load-bearing one: the tree comes from a remote, and a
/// symlink is the cheapest way to make a copy write somewhere it was not told
/// to. Refusing (rather than skipping) keeps the installed bundle a faithful
/// copy — a silently dropped symlink would produce a bundle that parses and
/// then fails at germination for a reason nothing recorded.
///
/// # Errors
/// Bails on a non-regular entry, an unsafe relative path, or any I/O failure.
pub fn copy_tree(src: &Path, dest: &Path) -> anyhow::Result<Vec<String>> {
    let mut copied = Vec::new();
    copy_into(src, dest, Path::new(""), &mut copied)?;
    copied.sort();
    Ok(copied)
}

/// Recursive worker for [`copy_tree`]; `prefix` is the path relative to the
/// bundle root, which is what gets validated and reported.
fn copy_into(
    src: &Path,
    dest: &Path,
    prefix: &Path,
    copied: &mut Vec<String>,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest)
        .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", dest.display()))?;

    let entries = std::fs::read_dir(src)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", src.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| anyhow::anyhow!("failed to read {}: {e}", src.display()))?;
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            anyhow::bail!(
                "bundle entry {} has a non-UTF-8 name; refusing to install",
                entry.path().display()
            );
        };
        if SKIPPED_DIRS.contains(&name_str) {
            continue;
        }

        let rel = prefix.join(name_str);
        let Some(rel_str) = rel.to_str() else {
            anyhow::bail!("bundle path {} is not UTF-8", rel.display());
        };
        check_bundle_path(rel_str)
            .map_err(|reason| anyhow::anyhow!("refusing bundle path `{rel_str}`: {reason}"))?;

        // `symlink_metadata` does NOT follow the link: this is the check, and
        // reading through `metadata()` here would defeat it entirely.
        let meta = entry
            .metadata()
            .map_err(|e| anyhow::anyhow!("failed to stat {}: {e}", entry.path().display()))?;
        if meta.is_symlink() {
            anyhow::bail!(
                "bundle entry `{rel_str}` is a symlink; refusing to install a bundle that \
                 points outside itself"
            );
        }
        if meta.is_dir() {
            copy_into(&entry.path(), &dest.join(name_str), &rel, copied)?;
        } else if meta.is_file() {
            std::fs::copy(entry.path(), dest.join(name_str)).map_err(|e| {
                anyhow::anyhow!("failed to copy {} into {}: {e}", rel_str, dest.display())
            })?;
            copied.push(rel_str.to_string());
        } else {
            anyhow::bail!(
                "bundle entry `{rel_str}` is neither a regular file nor a directory; \
                 refusing to install"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tree_copy_reports_every_file_it_wrote() {
        let src = TempDir::new().expect("temp src");
        let dest = TempDir::new().expect("temp dest");
        std::fs::write(src.path().join("spore.toml"), "x").expect("write manifest");
        std::fs::create_dir(src.path().join("formulas")).expect("mkdir");
        std::fs::write(src.path().join("formulas/a.formula.toml"), "y").expect("write recipe");

        let copied = copy_tree(src.path(), dest.path()).expect("copy");
        assert_eq!(copied, vec!["formulas/a.formula.toml", "spore.toml"]);
        assert!(dest.path().join("formulas/a.formula.toml").is_file());
    }

    #[test]
    fn git_bookkeeping_is_not_carried_into_the_destination() {
        let src = TempDir::new().expect("temp src");
        let dest = TempDir::new().expect("temp dest");
        std::fs::create_dir(src.path().join(".git")).expect("mkdir .git");
        std::fs::write(src.path().join(".git/HEAD"), "ref: x").expect("write");
        std::fs::write(src.path().join("spore.toml"), "x").expect("write manifest");

        let copied = copy_tree(src.path(), dest.path()).expect("copy");
        assert_eq!(copied, vec!["spore.toml"]);
        assert!(!dest.path().join(".git").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_in_a_fetched_bundle_is_refused_not_followed() {
        let src = TempDir::new().expect("temp src");
        let dest = TempDir::new().expect("temp dest");
        let outside = TempDir::new().expect("temp outside");
        std::fs::write(outside.path().join("secret"), "s3cret").expect("write outside");
        std::fs::write(src.path().join("spore.toml"), "x").expect("write manifest");
        std::os::unix::fs::symlink(outside.path().join("secret"), src.path().join("leak"))
            .expect("symlink");

        let err = copy_tree(src.path(), dest.path()).expect_err("a symlink is refused");
        assert!(err.to_string().contains("symlink"), "{err}");
        assert!(!dest.path().join("leak").exists());
    }

    #[test]
    fn occupancy_is_about_content_not_existence() {
        let dir = TempDir::new().expect("temp dir");
        assert!(!is_occupied(dir.path()).expect("empty dir"));
        std::fs::write(dir.path().join("f"), "x").expect("write");
        assert!(is_occupied(dir.path()).expect("occupied dir"));
        assert!(!is_occupied(&dir.path().join("absent")).expect("absent dir"));
    }

    #[test]
    fn a_local_source_may_be_the_manifest_itself() {
        let dir = TempDir::new().expect("temp dir");
        let manifest = dir.path().join("spore.toml");
        std::fs::write(&manifest, "x").expect("write manifest");
        let root = local_bundle_root(&manifest, None).expect("manifest resolves to its directory");
        assert_eq!(root, dir.path());
    }
}
