// SPDX-License-Identifier: Apache-2.0

//! `.gitignore`-aware directory walking for Rust source files.
//!
//! Uses the [`ignore`] crate (from the ripgrep ecosystem) to walk a directory
//! tree while respecting `.gitignore` rules. Returns `(path, source)` pairs
//! ready for symbol extraction.

use std::path::{Path, PathBuf};

use crate::error::CfsError;

/// Walk a directory tree and collect all Rust source files.
///
/// Respects `.gitignore` rules. Returns pairs of `(relative_path, source_content)`.
/// Paths are relative to `root` for cleaner structural map output.
///
/// # Errors
///
/// Returns [`CfsError::ReadFile`] if a discovered file cannot be read.
pub fn walk_rust_files(root: &Path) -> Result<Vec<(PathBuf, String)>, CfsError> {
    let root = root.canonicalize().map_err(|e| CfsError::ReadFile {
        path: root.to_path_buf(),
        source: e,
    })?;

    let mut files = Vec::new();

    for entry in ignore::WalkBuilder::new(&root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build()
        .flatten()
    {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "rs") && path.is_file() {
            let source = std::fs::read_to_string(path).map_err(|e| CfsError::ReadFile {
                path: path.to_path_buf(),
                source: e,
            })?;
            let rel = path.strip_prefix(&root).unwrap_or(path).to_path_buf();
            files.push((rel, source));
        }
    }

    files.sort_by(|(a, _), (b, _)| a.cmp(b));
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_walk_finds_rs_files() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("main.rs"), "fn main() {}").unwrap();
        fs::write(src.join("lib.rs"), "pub fn hello() {}").unwrap();
        fs::write(src.join("readme.md"), "# Hello").unwrap();

        let files = walk_rust_files(dir.path()).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|(p, _)| p.extension().unwrap() == "rs"));
    }

    #[test]
    fn test_walk_respects_gitignore() {
        let dir = TempDir::new().unwrap();
        // `ignore` crate needs a git repo to respect .gitignore.
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        let target = dir.path().join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("generated.rs"), "fn gen() {}").unwrap();
        fs::write(dir.path().join("lib.rs"), "fn lib() {}").unwrap();

        let files = walk_rust_files(dir.path()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, PathBuf::from("lib.rs"));
    }

    #[test]
    fn test_walk_sorted_output() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("z.rs"), "fn z() {}").unwrap();
        fs::write(dir.path().join("a.rs"), "fn a() {}").unwrap();

        let files = walk_rust_files(dir.path()).unwrap();
        assert_eq!(files[0].0, PathBuf::from("a.rs"));
        assert_eq!(files[1].0, PathBuf::from("z.rs"));
    }
}
