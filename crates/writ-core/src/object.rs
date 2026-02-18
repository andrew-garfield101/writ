//! Content-addressable object store.
//!
//! Objects are stored in `.writ/objects/` using a 2-character prefix
//! directory scheme (like git). Each object is identified by its SHA-256
//! hash and stored as raw bytes.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{WritError, WritResult};
use crate::hash::hash_bytes;

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

        // Create the 2-char prefix directory
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(&path, data)?;
        Ok(hash)
    }

    /// Retrieve an object by its hash.
    pub fn retrieve(&self, hash: &str) -> WritResult<Vec<u8>> {
        let path = self.object_path(hash);
        if !path.exists() {
            return Err(WritError::ObjectNotFound(hash.to_string()));
        }
        Ok(fs::read(&path)?)
    }

    /// Check if an object exists.
    pub fn exists(&self, hash: &str) -> bool {
        self.object_path(hash).exists()
    }

    /// Get the filesystem path for an object hash.
    ///
    /// Uses 2-char prefix directories: hash `abcdef...` -> `ab/cdef...`
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
}
