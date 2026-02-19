//! Content-addressable object store.
//!
//! Objects are stored in `.writ/objects/` using a 2-character prefix
//! directory scheme (like git). Each object is identified by its SHA-256
//! hash and stored as raw bytes.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{WritError, WritResult};
use crate::fsutil::atomic_write;
use crate::hash::hash_bytes;

/// Validate that a hash string is well-formed (64 hex chars).
fn validate_hash(hash: &str) -> WritResult<()> {
    if hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(WritError::InvalidHash(hash.to_string()))
    }
}

/// The object store manages content-addressable storage on disk.
pub struct ObjectStore {
    /// Root path: `.writ/objects/`
    root: PathBuf,
}

impl ObjectStore {
    /// Create a new ObjectStore rooted at the given path.
    pub fn new(objects_dir: &Path) -> Self {
        Self {
            root: objects_dir.to_path_buf(),
        }
    }

    /// Store bytes and return their content hash.
    ///
    /// If the object already exists (same content), this is a no-op
    /// and simply returns the existing hash.
    pub fn store(&self, data: &[u8]) -> WritResult<String> {
        let hash = hash_bytes(data);
        let path = self.object_path(&hash);

        if path.exists() {
            return Ok(hash);
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        atomic_write(&path, data)?;
        Ok(hash)
    }

    /// Retrieve an object by its hash, verifying integrity on read.
    pub fn retrieve(&self, hash: &str) -> WritResult<Vec<u8>> {
        validate_hash(hash)?;
        let path = self.object_path(hash);
        if !path.exists() {
            return Err(WritError::ObjectNotFound(hash.to_string()));
        }
        let data = fs::read(&path)?;
        let actual = hash_bytes(&data);
        if actual != hash {
            return Err(WritError::Other(format!(
                "object corrupted: expected {}, got {actual}",
                &hash[..12]
            )));
        }
        Ok(data)
    }

    /// Check if an object exists.
    pub fn exists(&self, hash: &str) -> bool {
        validate_hash(hash).is_ok() && self.object_path(hash).exists()
    }

    /// Get the filesystem path for an object hash.
    ///
    /// Uses 2-char prefix directories: hash `abcdef...` -> `ab/cdef...`
    /// Callers must validate the hash before calling this.
    fn object_path(&self, hash: &str) -> PathBuf {
        let (prefix, rest) = hash.split_at(2);
        self.root.join(prefix).join(rest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_store_and_retrieve() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());

        let data = b"hello world";
        let hash = store.store(data).unwrap();

        let retrieved = store.retrieve(&hash).unwrap();
        assert_eq!(retrieved, data);
    }

    #[test]
    fn test_store_is_idempotent() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());

        let data = b"same content";
        let h1 = store.store(data).unwrap();
        let h2 = store.store(data).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_retrieve_nonexistent() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());

        let result = store.retrieve("deadbeef00");
        assert!(result.is_err());
    }

    #[test]
    fn test_exists() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());

        let hash = store.store(b"test").unwrap();
        assert!(store.exists(&hash));
        assert!(!store.exists("nonexistent"));
    }

    #[test]
    fn test_retrieve_rejects_invalid_hash() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        let result = store.retrieve("not-a-valid-hash");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("invalid object hash"));
    }

    #[test]
    fn test_retrieve_rejects_too_short_hash() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        let result = store.retrieve("abcd");
        assert!(result.is_err());
    }

    #[test]
    fn test_retrieve_rejects_traversal_in_hash() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        let result = store.retrieve("../../../etc/passwd/../../../etc/shadow/../xx");
        assert!(result.is_err());
    }

    #[test]
    fn test_exists_returns_false_for_invalid_hash() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        assert!(!store.exists("not-valid"));
        assert!(!store.exists("../../../etc/passwd"));
    }
}
