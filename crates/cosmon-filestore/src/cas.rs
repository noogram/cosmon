// SPDX-License-Identifier: AGPL-3.0-only

//! Filesystem backend for content-addressed binary storage.
//!
//! Stores blobs in a `hash[:2]/hash` directory layout under a configurable root:
//!
//! ```text
//! <root>/
//!   e3/e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
//!   0a/0a1b2c3d...
//! ```
//!
//! Writes are atomic: data lands in a `.tmp` sibling, is fsynced, then renamed
//! into place. A crash mid-write never corrupts the store. Identical content
//! is deduplicated automatically — if the target path already exists, the write
//! is skipped.

#![forbid(unsafe_code)]

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use cosmon_core::cas::{CasStore, ContentHash};
use cosmon_core::error::CosmonError;

/// Filesystem-backed content-addressed store.
///
/// Binary blobs are stored under `root/hash[:2]/hash` and deduplicated by
/// SHA-256 content hash.
///
/// # Examples
///
/// ```no_run
/// use cosmon_filestore::cas::FileCas;
/// use cosmon_core::cas::CasStore;
///
/// let store = FileCas::new("/tmp/cosmon-cas");
/// let hash = store.put(b"hello world").unwrap();
/// assert!(store.exists(&hash).unwrap());
/// let data = store.get(&hash).unwrap();
/// assert_eq!(data, b"hello world");
/// ```
#[derive(Debug, Clone)]
pub struct FileCas {
    root: PathBuf,
}

impl FileCas {
    /// Create a new `FileCas` rooted at the given directory.
    ///
    /// The directory (and shard subdirectories) will be created on the first write.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The storage path for a given hash: `root/hash[:2]/hash`.
    fn blob_path(&self, hash: &ContentHash) -> PathBuf {
        self.root.join(hash.prefix()).join(hash.as_str())
    }
}

/// Compute the SHA-256 hash of the given data, returning a [`ContentHash`].
fn sha256(data: &[u8]) -> ContentHash {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    let hex = hex_encode(&digest);
    // Safety: SHA-256 always produces 64 lowercase hex chars.
    ContentHash::new(hex).expect("SHA-256 always produces valid hex")
}

/// Encode bytes as lowercase hex.
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX_CHARS[(b >> 4) as usize]);
        s.push(HEX_CHARS[(b & 0x0f) as usize]);
    }
    s
}

const HEX_CHARS: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
];

impl CasStore for FileCas {
    fn put(&self, data: &[u8]) -> Result<ContentHash, CosmonError> {
        let hash = sha256(data);
        let target = self.blob_path(&hash);

        // Dedup: if the blob already exists, skip the write.
        if target.exists() {
            return Ok(hash);
        }

        // Ensure the shard directory exists.
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }

        // Atomic write: write to .tmp, fsync, rename.
        let tmp = target.with_extension("tmp");
        atomic_write_with_fsync(&tmp, data)?;
        fs::rename(&tmp, &target)?;

        Ok(hash)
    }

    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>, CosmonError> {
        let path = self.blob_path(hash);
        if !path.exists() {
            return Err(CosmonError::Runtime {
                reason: format!("blob not found: {hash}"),
            });
        }
        Ok(fs::read(&path)?)
    }

    fn exists(&self, hash: &ContentHash) -> Result<bool, CosmonError> {
        Ok(self.blob_path(hash).exists())
    }
}

/// Write `data` to `path` with an fsync before returning.
fn atomic_write_with_fsync(path: &Path, data: &[u8]) -> Result<(), CosmonError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store() -> (TempDir, FileCas) {
        let tmp = TempDir::new().unwrap();
        let store = FileCas::new(tmp.path().join("cas"));
        (tmp, store)
    }

    #[test]
    fn test_put_and_get_roundtrip() {
        let (_tmp, store) = make_store();
        let data = b"hello world";
        let hash = store.put(data).unwrap();
        let retrieved = store.get(&hash).unwrap();
        assert_eq!(retrieved, data);
    }

    #[test]
    fn test_put_returns_correct_sha256() {
        let (_tmp, store) = make_store();
        // SHA-256("hello world") = b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9
        let hash = store.put(b"hello world").unwrap();
        assert_eq!(
            hash.as_str(),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn test_put_empty_content() {
        let (_tmp, store) = make_store();
        let hash = store.put(b"").unwrap();
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        assert_eq!(
            hash.as_str(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_dedup_idempotent() {
        let (_tmp, store) = make_store();
        let data = b"dedup test";
        let h1 = store.put(data).unwrap();
        let h2 = store.put(data).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_exists_true_after_put() {
        let (_tmp, store) = make_store();
        let hash = store.put(b"exists test").unwrap();
        assert!(store.exists(&hash).unwrap());
    }

    #[test]
    fn test_exists_false_for_unknown() {
        let (_tmp, store) = make_store();
        let hash =
            ContentHash::new("0000000000000000000000000000000000000000000000000000000000000000")
                .unwrap();
        assert!(!store.exists(&hash).unwrap());
    }

    #[test]
    fn test_get_not_found() {
        let (_tmp, store) = make_store();
        let hash =
            ContentHash::new("0000000000000000000000000000000000000000000000000000000000000000")
                .unwrap();
        let err = store.get(&hash).unwrap_err();
        assert!(err.to_string().contains("blob not found"));
    }

    #[test]
    fn test_directory_layout() {
        let (tmp, store) = make_store();
        let hash = store.put(b"layout test").unwrap();

        // Verify hash[:2]/hash layout
        let expected_path = tmp
            .path()
            .join("cas")
            .join(hash.prefix())
            .join(hash.as_str());
        assert!(expected_path.exists());
    }

    #[test]
    fn test_multiple_blobs_different_shards() {
        let (tmp, store) = make_store();
        let h1 = store.put(b"alpha").unwrap();
        let h2 = store.put(b"beta").unwrap();
        let h3 = store.put(b"gamma").unwrap();

        // All three retrievable
        assert_eq!(store.get(&h1).unwrap(), b"alpha");
        assert_eq!(store.get(&h2).unwrap(), b"beta");
        assert_eq!(store.get(&h3).unwrap(), b"gamma");

        // All stored under cas root
        let cas_root = tmp.path().join("cas");
        assert!(cas_root.exists());
    }

    #[test]
    fn test_large_blob() {
        let (_tmp, store) = make_store();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let data: Vec<u8> = (0..100_000).map(|i| (i % 256) as u8).collect();
        let hash = store.put(&data).unwrap();
        let retrieved = store.get(&hash).unwrap();
        assert_eq!(retrieved, data);
    }

    #[test]
    fn test_binary_content() {
        let (_tmp, store) = make_store();
        // PDF magic bytes + some binary content
        let data: Vec<u8> = vec![0x25, 0x50, 0x44, 0x46, 0x00, 0xFF, 0xFE, 0xFD];
        let hash = store.put(&data).unwrap();
        let retrieved = store.get(&hash).unwrap();
        assert_eq!(retrieved, data);
    }

    #[test]
    fn test_sha256_known_vectors() {
        // Verify our SHA-256 implementation against known test vectors.
        let empty = sha256(b"");
        assert_eq!(
            empty.as_str(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        let abc = sha256(b"abc");
        assert_eq!(
            abc.as_str(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
