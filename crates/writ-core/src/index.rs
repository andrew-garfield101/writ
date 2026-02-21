//! File tracking index.
//!
//! The index tracks which files are under writ's control and their
//! current content hashes. Stored as `.writ/index.json`.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::WritResult;
use crate::fsutil::atomic_write;

/// A tracked file entry in the index.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexEntry {
    /// SHA-256 hash of the file's content.
    pub hash: String,
    /// File size in bytes.
    pub size: u64,
}

/// The full index mapping relative paths to their tracked state.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    /// Map of relative file path -> index entry.
    pub entries: BTreeMap<String, IndexEntry>,
}

impl Index {
    /// Load the index from a JSON file, or return an empty index.
    pub fn load(path: &Path) -> WritResult<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = fs::read_to_string(path)?;
        let index: Index = serde_json::from_str(&data)?;
        Ok(index)
    }

    /// Save the index to a JSON file (atomic: temp + fsync + rename).
    pub fn save(&self, path: &Path) -> WritResult<()> {
        let json = serde_json::to_string_pretty(self)?;
        atomic_write(path, json.as_bytes())?;
        Ok(())
    }

    /// Update or insert an entry for a file.
    pub fn upsert(&mut self, rel_path: &str, hash: String, size: u64) {
        self.entries
            .insert(rel_path.to_string(), IndexEntry { hash, size });
    }

    /// Remove a file from the index.
    pub fn remove(&mut self, rel_path: &str) -> bool {
        self.entries.remove(rel_path).is_some()
    }

    /// Check if a file is tracked.
    pub fn is_tracked(&self, rel_path: &str) -> bool {
        self.entries.contains_key(rel_path)
    }

    /// Get the hash of a tracked file.
    pub fn get_hash(&self, rel_path: &str) -> Option<&str> {
        self.entries.get(rel_path).map(|e| e.hash.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_empty_index() {
        let idx = Index::default();
        assert!(idx.entries.is_empty());
        assert!(!idx.is_tracked("foo.rs"));
    }

    #[test]
    fn test_upsert_and_lookup() {
        let mut idx = Index::default();
        idx.upsert("src/main.rs", "abc123".to_string(), 42);

        assert!(idx.is_tracked("src/main.rs"));
        assert_eq!(idx.get_hash("src/main.rs"), Some("abc123"));
    }

    #[test]
    fn test_save_and_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("index.json");

        let mut idx = Index::default();
        idx.upsert("file.txt", "hash123".to_string(), 100);
        idx.save(&path).unwrap();

        let loaded = Index::load(&path).unwrap();
        assert_eq!(loaded.get_hash("file.txt"), Some("hash123"));
    }

    #[test]
    fn test_remove() {
        let mut idx = Index::default();
        idx.upsert("file.txt", "hash".to_string(), 10);
        assert!(idx.remove("file.txt"));
        assert!(!idx.is_tracked("file.txt"));
        assert!(!idx.remove("nonexistent"));
    }
}
