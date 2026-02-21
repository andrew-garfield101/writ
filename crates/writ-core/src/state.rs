//! Working directory state detection.
//!
//! Compares the current working tree against the index to determine
//! which files are new, modified, or deleted.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Serialize;
use walkdir::WalkDir;

use crate::hash::hash_bytes;
use crate::ignore::IgnoreRules;
use crate::index::Index;

/// The type of change detected for a file.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum FileStatus {
    /// New file not yet tracked.
    New,
    /// Tracked file with different content.
    Modified,
    /// Tracked file no longer on disk.
    Deleted,
}

/// A single file's status in the working directory.
#[derive(Debug, Clone, Serialize)]
pub struct FileState {
    pub path: String,
    pub status: FileStatus,
    /// Current content hash (None for deleted files).
    pub hash: Option<String>,
}

/// Full working directory state.
#[derive(Debug, Clone, Serialize)]
pub struct WorkingState {
    /// Files with changes.
    pub changes: Vec<FileState>,
    /// Total tracked files.
    pub tracked_count: usize,
}

impl WorkingState {
    /// True if there are no changes.
    pub fn is_clean(&self) -> bool {
        self.changes.is_empty()
    }

    /// Format a brief one-line summary for LLM context efficiency.
    pub fn brief(&self) -> String {
        if self.is_clean() {
            return "clean".to_string();
        }

        let new_count = self
            .changes
            .iter()
            .filter(|f| f.status == FileStatus::New)
            .count();
        let mod_count = self
            .changes
            .iter()
            .filter(|f| f.status == FileStatus::Modified)
            .count();
        let del_count = self
            .changes
            .iter()
            .filter(|f| f.status == FileStatus::Deleted)
            .count();

        let mut parts = Vec::new();
        if new_count > 0 {
            parts.push(format!("{new_count}-new"));
        }
        if mod_count > 0 {
            parts.push(format!("{mod_count}-modified"));
        }
        if del_count > 0 {
            parts.push(format!("{del_count}-deleted"));
        }
        format!("tracked:{} changes:{}", self.tracked_count, parts.join(","))
    }
}

/// Compute the working directory state by comparing files on disk to the index.
pub fn compute_state(repo_root: &Path, index: &Index, rules: &IgnoreRules) -> WorkingState {
    let mut changes = Vec::new();
    let mut seen: BTreeMap<String, bool> = BTreeMap::new();

    // Walk the working directory
    for entry in WalkDir::new(repo_root)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !rules.is_dir_ignored(&name)
        })
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }

        let full_path = entry.path();
        let rel_path = match full_path.strip_prefix(repo_root) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => continue,
        };

        // Check file-level ignore patterns
        if rules.is_file_ignored(&rel_path) {
            continue;
        }

        seen.insert(rel_path.clone(), true);

        // Read and hash the file
        let content = match fs::read(full_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let current_hash = hash_bytes(&content);
        let size = content.len() as u64;

        if let Some(indexed_hash) = index.get_hash(&rel_path) {
            // Tracked file — check if modified
            if indexed_hash != current_hash {
                changes.push(FileState {
                    path: rel_path,
                    status: FileStatus::Modified,
                    hash: Some(current_hash),
                });
            }
        } else {
            // New untracked file
            changes.push(FileState {
                path: rel_path,
                status: FileStatus::New,
                hash: Some(current_hash),
            });
        }

        let _ = size; // Will be used when we store to index
    }

    // Check for deleted files (in index but not on disk)
    for tracked_path in index.entries.keys() {
        if !seen.contains_key(tracked_path.as_str()) {
            changes.push(FileState {
                path: tracked_path.clone(),
                status: FileStatus::Deleted,
                hash: None,
            });
        }
    }

    // Sort for deterministic output
    changes.sort_by(|a, b| a.path.cmp(&b.path));

    WorkingState {
        changes,
        tracked_count: index.entries.len(),
    }
}
