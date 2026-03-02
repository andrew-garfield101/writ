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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ignore::IgnoreRules;
    use crate::index::Index;
    use std::fs;
    use tempfile::TempDir;

    /// Helper: create a temp dir with files, an index, and default ignore rules.
    fn setup_workspace(files: &[(&str, &str)]) -> (TempDir, std::path::PathBuf) {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        for (path, content) in files {
            let full = root.join(path);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&full, content).unwrap();
        }
        (tmp, root)
    }

    /// Helper: build an index with the given files pre-hashed from content.
    fn index_from_files(root: &Path, paths: &[&str]) -> Index {
        let mut idx = Index::default();
        for path in paths {
            let content = fs::read(root.join(path)).unwrap();
            let hash = crate::hash::hash_bytes(&content);
            idx.upsert(path, hash, content.len() as u64);
        }
        idx
    }

    // ── WorkingState struct tests ────────────────────────────

    #[test]
    fn is_clean_returns_true_when_no_changes() {
        let state = WorkingState {
            changes: vec![],
            tracked_count: 3,
        };
        assert!(state.is_clean());
    }

    #[test]
    fn is_clean_returns_false_when_changes_exist() {
        let state = WorkingState {
            changes: vec![FileState {
                path: "foo.py".to_string(),
                status: FileStatus::New,
                hash: Some("abc".to_string()),
            }],
            tracked_count: 0,
        };
        assert!(!state.is_clean());
    }

    #[test]
    fn brief_returns_clean_for_empty_changes() {
        let state = WorkingState {
            changes: vec![],
            tracked_count: 5,
        };
        assert_eq!(state.brief(), "clean");
    }

    #[test]
    fn brief_shows_new_count() {
        let state = WorkingState {
            changes: vec![
                FileState {
                    path: "a.py".to_string(),
                    status: FileStatus::New,
                    hash: Some("h1".to_string()),
                },
                FileState {
                    path: "b.py".to_string(),
                    status: FileStatus::New,
                    hash: Some("h2".to_string()),
                },
            ],
            tracked_count: 0,
        };
        assert_eq!(state.brief(), "tracked:0 changes:2-new");
    }

    #[test]
    fn brief_shows_mixed_counts() {
        let state = WorkingState {
            changes: vec![
                FileState {
                    path: "a.py".to_string(),
                    status: FileStatus::New,
                    hash: Some("h1".to_string()),
                },
                FileState {
                    path: "b.py".to_string(),
                    status: FileStatus::Modified,
                    hash: Some("h2".to_string()),
                },
                FileState {
                    path: "c.py".to_string(),
                    status: FileStatus::Deleted,
                    hash: None,
                },
            ],
            tracked_count: 2,
        };
        assert_eq!(
            state.brief(),
            "tracked:2 changes:1-new,1-modified,1-deleted"
        );
    }

    #[test]
    fn brief_omits_zero_categories() {
        let state = WorkingState {
            changes: vec![FileState {
                path: "x.rs".to_string(),
                status: FileStatus::Deleted,
                hash: None,
            }],
            tracked_count: 1,
        };
        assert_eq!(state.brief(), "tracked:1 changes:1-deleted");
    }

    // ── compute_state: clean state ───────────────────────────

    #[test]
    fn compute_state_clean_when_files_match_index() {
        let (_tmp, root) = setup_workspace(&[("main.py", "print('hello')")]);
        let idx = index_from_files(&root, &["main.py"]);
        let rules = IgnoreRules::defaults();

        let state = compute_state(&root, &idx, &rules);
        assert!(state.is_clean());
        assert_eq!(state.tracked_count, 1);
    }

    #[test]
    fn compute_state_clean_empty_repo() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let idx = Index::default();
        let rules = IgnoreRules::defaults();

        let state = compute_state(root, &idx, &rules);
        assert!(state.is_clean());
        assert_eq!(state.tracked_count, 0);
    }

    // ── compute_state: new files ─────────────────────────────

    #[test]
    fn compute_state_detects_new_file() {
        let (_tmp, root) = setup_workspace(&[("new_file.py", "content")]);
        let idx = Index::default();
        let rules = IgnoreRules::defaults();

        let state = compute_state(&root, &idx, &rules);
        assert_eq!(state.changes.len(), 1);
        assert_eq!(state.changes[0].path, "new_file.py");
        assert_eq!(state.changes[0].status, FileStatus::New);
        assert!(state.changes[0].hash.is_some());
    }

    #[test]
    fn compute_state_detects_multiple_new_files() {
        let (_tmp, root) = setup_workspace(&[("a.py", "aaa"), ("b.py", "bbb"), ("c.py", "ccc")]);
        let idx = Index::default();
        let rules = IgnoreRules::defaults();

        let state = compute_state(&root, &idx, &rules);
        assert_eq!(state.changes.len(), 3);
        // Should be sorted alphabetically
        assert_eq!(state.changes[0].path, "a.py");
        assert_eq!(state.changes[1].path, "b.py");
        assert_eq!(state.changes[2].path, "c.py");
    }

    // ── compute_state: modified files ────────────────────────

    #[test]
    fn compute_state_detects_modified_file() {
        let (_tmp, root) = setup_workspace(&[("app.py", "original")]);
        let idx = index_from_files(&root, &["app.py"]);

        // Modify the file
        fs::write(root.join("app.py"), "modified").unwrap();
        let rules = IgnoreRules::defaults();

        let state = compute_state(&root, &idx, &rules);
        assert_eq!(state.changes.len(), 1);
        assert_eq!(state.changes[0].path, "app.py");
        assert_eq!(state.changes[0].status, FileStatus::Modified);
        assert!(state.changes[0].hash.is_some());
    }

    #[test]
    fn compute_state_unchanged_file_not_in_changes() {
        let (_tmp, root) = setup_workspace(&[("unchanged.py", "same"), ("changed.py", "before")]);
        let idx = index_from_files(&root, &["unchanged.py", "changed.py"]);

        // Only modify one file
        fs::write(root.join("changed.py"), "after").unwrap();
        let rules = IgnoreRules::defaults();

        let state = compute_state(&root, &idx, &rules);
        assert_eq!(state.changes.len(), 1);
        assert_eq!(state.changes[0].path, "changed.py");
    }

    // ── compute_state: deleted files ─────────────────────────

    #[test]
    fn compute_state_detects_deleted_file() {
        let (_tmp, root) = setup_workspace(&[("doomed.py", "bye")]);
        let idx = index_from_files(&root, &["doomed.py"]);

        // Delete the file
        fs::remove_file(root.join("doomed.py")).unwrap();
        let rules = IgnoreRules::defaults();

        let state = compute_state(&root, &idx, &rules);
        assert_eq!(state.changes.len(), 1);
        assert_eq!(state.changes[0].path, "doomed.py");
        assert_eq!(state.changes[0].status, FileStatus::Deleted);
        assert!(state.changes[0].hash.is_none());
    }

    // ── compute_state: mixed changes ─────────────────────────

    #[test]
    fn compute_state_mixed_new_modified_deleted() {
        let (_tmp, root) =
            setup_workspace(&[("existing.py", "original"), ("to_delete.py", "gone soon")]);
        let idx = index_from_files(&root, &["existing.py", "to_delete.py"]);

        // Add new file
        fs::write(root.join("brand_new.py"), "fresh").unwrap();
        // Modify existing
        fs::write(root.join("existing.py"), "changed").unwrap();
        // Delete tracked
        fs::remove_file(root.join("to_delete.py")).unwrap();

        let rules = IgnoreRules::defaults();
        let state = compute_state(&root, &idx, &rules);

        assert_eq!(state.changes.len(), 3);
        assert_eq!(state.tracked_count, 2);

        // Sorted alphabetically
        assert_eq!(state.changes[0].path, "brand_new.py");
        assert_eq!(state.changes[0].status, FileStatus::New);
        assert_eq!(state.changes[1].path, "existing.py");
        assert_eq!(state.changes[1].status, FileStatus::Modified);
        assert_eq!(state.changes[2].path, "to_delete.py");
        assert_eq!(state.changes[2].status, FileStatus::Deleted);
    }

    // ── compute_state: ignore rules ──────────────────────────

    #[test]
    fn compute_state_ignores_writ_directory() {
        let (_tmp, root) =
            setup_workspace(&[("app.py", "code"), (".writ/seals/abc.json", "seal data")]);
        let idx = Index::default();
        let rules = IgnoreRules::defaults();

        let state = compute_state(&root, &idx, &rules);
        // Should only see app.py, not the .writ file
        assert_eq!(state.changes.len(), 1);
        assert_eq!(state.changes[0].path, "app.py");
    }

    #[test]
    fn compute_state_ignores_git_directory() {
        let (_tmp, root) = setup_workspace(&[
            ("src/main.rs", "fn main() {}"),
            (".git/config", "git config"),
        ]);
        let idx = Index::default();
        let rules = IgnoreRules::defaults();

        let state = compute_state(&root, &idx, &rules);
        assert_eq!(state.changes.len(), 1);
        assert_eq!(state.changes[0].path, "src/main.rs");
    }

    #[test]
    fn compute_state_respects_custom_file_ignore_patterns() {
        let (_tmp, root) = setup_workspace(&[
            ("app.py", "code"),
            ("data.log", "log data"),
            ("cache.tmp", "temp"),
        ]);
        let idx = Index::default();
        let rules = IgnoreRules::parse("*.log\n*.tmp\n");

        let state = compute_state(&root, &idx, &rules);
        assert_eq!(state.changes.len(), 1);
        assert_eq!(state.changes[0].path, "app.py");
    }

    #[test]
    fn compute_state_respects_custom_dir_ignore() {
        let (_tmp, root) =
            setup_workspace(&[("src/main.py", "code"), ("build/output.bin", "binary")]);
        let idx = Index::default();
        let rules = IgnoreRules::parse("build\n");

        let state = compute_state(&root, &idx, &rules);
        assert_eq!(state.changes.len(), 1);
        assert_eq!(state.changes[0].path, "src/main.py");
    }

    // ── compute_state: nested directories ────────────────────

    #[test]
    fn compute_state_handles_nested_directories() {
        let (_tmp, root) = setup_workspace(&[
            ("src/main.py", "main"),
            ("src/lib/utils.py", "utils"),
            ("tests/test_main.py", "test"),
        ]);
        let idx = Index::default();
        let rules = IgnoreRules::defaults();

        let state = compute_state(&root, &idx, &rules);
        assert_eq!(state.changes.len(), 3);
        // Paths should use forward slashes and be sorted
        assert_eq!(state.changes[0].path, "src/lib/utils.py");
        assert_eq!(state.changes[1].path, "src/main.py");
        assert_eq!(state.changes[2].path, "tests/test_main.py");
    }

    // ── compute_state: deterministic output ──────────────────

    #[test]
    fn compute_state_output_is_sorted() {
        let (_tmp, root) = setup_workspace(&[
            ("z_last.py", "z"),
            ("a_first.py", "a"),
            ("m_middle.py", "m"),
        ]);
        let idx = Index::default();
        let rules = IgnoreRules::defaults();

        let state = compute_state(&root, &idx, &rules);
        let paths: Vec<&str> = state.changes.iter().map(|c| c.path.as_str()).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted);
    }

    // ── compute_state: tracked_count ─────────────────────────

    #[test]
    fn tracked_count_reflects_index_size() {
        let (_tmp, root) = setup_workspace(&[("a.py", "a"), ("b.py", "b")]);
        let idx = index_from_files(&root, &["a.py", "b.py"]);
        let rules = IgnoreRules::defaults();

        let state = compute_state(&root, &idx, &rules);
        assert_eq!(state.tracked_count, 2);
    }

    #[test]
    fn tracked_count_includes_deleted_files() {
        let (_tmp, root) = setup_workspace(&[("alive.py", "here")]);
        let mut idx = index_from_files(&root, &["alive.py"]);
        // Add a file to the index that doesn't exist on disk
        idx.upsert("ghost.py", "fakehash".to_string(), 10);

        let rules = IgnoreRules::defaults();
        let state = compute_state(&root, &idx, &rules);
        assert_eq!(state.tracked_count, 2); // Both entries in index
    }

    // ── compute_state: hash correctness ──────────────────────

    #[test]
    fn new_file_hash_matches_content() {
        let content = "deterministic content";
        let (_tmp, root) = setup_workspace(&[("file.txt", content)]);
        let idx = Index::default();
        let rules = IgnoreRules::defaults();

        let state = compute_state(&root, &idx, &rules);
        let expected_hash = crate::hash::hash_bytes(content.as_bytes());
        assert_eq!(
            state.changes[0].hash.as_deref(),
            Some(expected_hash.as_str())
        );
    }

    #[test]
    fn modified_file_hash_reflects_new_content() {
        let (_tmp, root) = setup_workspace(&[("file.txt", "original")]);
        let idx = index_from_files(&root, &["file.txt"]);

        let new_content = "modified";
        fs::write(root.join("file.txt"), new_content).unwrap();

        let rules = IgnoreRules::defaults();
        let state = compute_state(&root, &idx, &rules);
        let expected_hash = crate::hash::hash_bytes(new_content.as_bytes());
        assert_eq!(
            state.changes[0].hash.as_deref(),
            Some(expected_hash.as_str())
        );
    }

    // ── FileStatus serialization ─────────────────────────────

    #[test]
    fn file_status_serializes_to_lowercase() {
        let new_json = serde_json::to_string(&FileStatus::New).unwrap();
        let mod_json = serde_json::to_string(&FileStatus::Modified).unwrap();
        let del_json = serde_json::to_string(&FileStatus::Deleted).unwrap();
        assert_eq!(new_json, "\"new\"");
        assert_eq!(mod_json, "\"modified\"");
        assert_eq!(del_json, "\"deleted\"");
    }
}
