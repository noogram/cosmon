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
//! Writes are atomic: each writer stages data in a unique sibling file,
//! syncs it, then renames it into place. A crash mid-write leaves the
//! previous blob intact. Matching blobs are deduplicated and corrupt
//! contents are replaced.

#![forbid(unsafe_code)]

use std::fs::{self, File};
use std::io::{self, Read, Write};
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

    /// Read a complete blob within the caller's byte limit.
    ///
    /// Reads at most one byte past the limit to distinguish EOF from an
    /// oversized blob. Useful for callers with a payload budget.
    ///
    /// # Errors
    ///
    /// Returns an I/O error of kind [`io::ErrorKind::FileTooLarge`] when the
    /// blob exceeds `max_bytes`, or the filesystem error if it cannot be read.
    pub fn get_bounded(&self, hash: &ContentHash, max_bytes: u64) -> Result<Vec<u8>, CosmonError> {
        let mut blob = Vec::new();
        File::open(self.blob_path(hash))?
            .take(max_bytes.saturating_add(1))
            .read_to_end(&mut blob)?;
        if blob.len() as u64 > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                format!("blob {hash} exceeds {max_bytes} bytes"),
            )
            .into());
        }
        Ok(blob)
    }
}

/// Rehash an existing blob, reading only enough to reject an oversized one.
fn stored_blob_matches(path: &Path, size: u64, digest: &[u8]) -> io::Result<bool> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let mut hasher = Sha256::new();
    let bytes_read = io::copy(&mut file.take(size + 1), &mut hasher)?;
    Ok(bytes_read == size && hasher.finalize().as_slice() == digest)
}

impl CasStore for FileCas {
    fn put(&self, data: &[u8]) -> Result<ContentHash, CosmonError> {
        let digest = Sha256::digest(data);
        let hash = ContentHash::new(format!("{digest:x}"))?;
        let target = self.blob_path(&hash);

        if stored_blob_matches(&target, data.len() as u64, &digest)? {
            return Ok(hash);
        }

        atomic_write_with_fsync(&target, data)?;
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

/// Publish fully written bytes by replacing the destination atomically.
fn atomic_write_with_fsync(target: &Path, data: &[u8]) -> io::Result<()> {
    let shard = target
        .parent()
        .ok_or_else(|| io::Error::other("CAS blob path has no parent"))?;
    fs::create_dir_all(shard)?;

    let mut builder = tempfile::Builder::new();
    builder.prefix(".cas-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        // Preserve normal file creation permissions, subject to umask.
        builder.permissions(fs::Permissions::from_mode(0o666));
    }
    let mut temporary = builder.tempfile_in(shard)?;
    temporary.write_all(data)?;
    temporary.as_file().sync_all()?;
    // Concurrent writers for this address publish the same bytes.
    temporary.persist(target).map_err(|error| error.error)?;
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
        let path = store.blob_path(&h1);
        let stamp = std::time::UNIX_EPOCH + std::time::Duration::from_secs(12_345);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(stamp)
            .unwrap();
        let h2 = store.put(data).unwrap();
        assert_eq!(h1, h2);
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), stamp);
    }

    #[test]
    fn put_repairs_corrupt_existing_bytes_and_returns_the_same_address() {
        let (_tmp, store) = make_store();
        let expected = b"hello world";
        let hash = store.put(expected).unwrap();
        let path = store.blob_path(&hash);

        for corrupt in [
            b"hello WORLD".as_slice(),
            b"hello",
            b"hello world with extra bytes",
        ] {
            fs::write(&path, corrupt).unwrap();
            assert_eq!(store.put(expected).unwrap(), hash);
            assert_eq!(fs::read(&path).unwrap(), expected, "after {corrupt:?}");
            assert_eq!(store.get(&hash).unwrap(), expected);
        }
    }

    #[cfg(unix)]
    #[test]
    fn put_propagates_open_errors_other_than_not_found() {
        use std::os::unix::fs::symlink;

        let (_tmp, store) = make_store();
        let data = b"unreadable blob path";
        let hash = store.put(data).unwrap();
        let path = store.blob_path(&hash);
        fs::remove_file(&path).unwrap();
        symlink(path.file_name().unwrap(), &path).unwrap();

        // Opening this path fails on the loop, but replacing it would succeed.
        assert!(matches!(store.put(data), Err(CosmonError::Io(_))));
        assert_eq!(fs::read_link(&path).unwrap(), Path::new(hash.as_str()));
    }

    #[test]
    fn concurrent_puts_publish_only_complete_bytes_and_all_succeed() {
        use std::sync::Barrier;
        use std::thread;

        const WRITERS: usize = 4;
        let (_tmp, store) = make_store();
        let data = vec![0xA5; 8 * 1024 * 1024];
        // SHA-256 computed independently with shasum over the fixture bytes.
        let hash =
            ContentHash::new("e474a45740e5efac02e9f7e0626b3f86111b293f8363e477786dfad64e98df3b")
                .unwrap();
        let start = Barrier::new(WRITERS + 1);
        thread::scope(|scope| {
            let writers: Vec<_> = (0..WRITERS)
                .map(|_| {
                    scope.spawn(|| {
                        start.wait();
                        store.put(&data)
                    })
                })
                .collect();
            start.wait();
            while writers.iter().any(|writer| !writer.is_finished()) {
                if store.exists(&hash).unwrap() {
                    let visible = store.get(&hash).unwrap();
                    assert!(visible == data, "reader saw {} bytes", visible.len());
                }
                thread::yield_now();
            }
            for writer in writers {
                assert_eq!(writer.join().unwrap().unwrap(), hash);
            }
        });
        assert!(store.get(&hash).unwrap() == data);
    }

    #[test]
    fn bounded_get_returns_complete_bytes_or_a_size_error() {
        let (_tmp, store) = make_store();
        let hash = store.put(b"abcde").unwrap();
        for limit in [5, 6, u64::MAX] {
            assert_eq!(store.get_bounded(&hash, limit).unwrap(), b"abcde");
        }
        for limit in [0, 4] {
            assert!(matches!(
                store.get_bounded(&hash, limit),
                Err(CosmonError::Io(error)) if error.kind() == std::io::ErrorKind::FileTooLarge
            ));
        }
        let empty = store.put(b"").unwrap();
        assert!(store.get_bounded(&empty, 0).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn bounded_get_stops_when_the_size_limit_is_exceeded_before_eof() {
        use std::sync::mpsc;
        use std::thread;
        use std::time::Duration;

        let (_tmp, store) = make_store();
        let hash = store.put(b"seed").unwrap();
        let path = store.blob_path(&hash);
        fs::remove_file(&path).unwrap();
        assert!(std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success());
        let mut fifo = fs::File::options()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        fifo.write_all(b"abcde").unwrap();

        let (send, receive) = mpsc::channel();
        let reader = thread::spawn(move || send.send(store.get_bounded(&hash, 4)));
        let result = receive.recv_timeout(Duration::from_secs(3));
        drop(fifo);
        reader.join().unwrap().unwrap();
        assert!(matches!(
            result.expect("bounded read must return while the writer is still open"),
            Err(CosmonError::Io(error)) if error.kind() == std::io::ErrorKind::FileTooLarge
        ));
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
        let (_tmp, store) = make_store();
        let empty = store.put(b"").unwrap();
        assert_eq!(
            empty.as_str(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        let abc = store.put(b"abc").unwrap();
        assert_eq!(
            abc.as_str(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
