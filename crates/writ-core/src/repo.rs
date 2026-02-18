//! Repository — the main entry point for writ operations.
//!
//! A Repository ties together the object store, index, seals, and specs
//! into a unified interface.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::context::{
    ContextOutput, ContextScope, DiffSummary, SealSummary, WorkingStateSummary,
};
use crate::diff::{self, DiffOutput, FileDiff};
use crate::error::{WritError, WritResult};
use crate::ignore::IgnoreRules;
use crate::index::{Index, IndexEntry};
use crate::object::ObjectStore;
use crate::seal::{AgentIdentity, ChangeType, FileChange, Seal, TaskStatus, Verification};
use crate::spec::{Spec, SpecUpdate};
use crate::state::{self, FileStatus, WorkingState};

/// The `.writ` directory name.
const WRIT_DIR: &str = ".writ";

/// A writ repository.
pub struct Repository {
    /// Root of the working directory (where `.writ/` lives).
    root: PathBuf,
    /// Path to the `.writ/` directory.
    writ_dir: PathBuf,
    /// Content-addressable object store.
    objects: ObjectStore,
}

impl Repository {
    /// Initialize a new writ repository in the given directory.
    ///
    /// Creates the `.writ/` directory structure.
    pub fn init(root: &Path) -> WritResult<Self> {
        let writ_dir = root.join(WRIT_DIR);

        if writ_dir.exists() {
            return Err(WritError::AlreadyExists);
        }

        // Create directory structure
        fs::create_dir_all(writ_dir.join("objects"))?;
        fs::create_dir_all(writ_dir.join("seals"))?;
        fs::create_dir_all(writ_dir.join("specs"))?;

        // Create empty HEAD
        fs::write(writ_dir.join("HEAD"), "")?;

        // Create empty index
        let index = Index::default();
        index.save(&writ_dir.join("index.json"))?;

        Self::open(root)
    }

    /// Open an existing writ repository.
    ///
    /// Searches for `.writ/` in the given directory.
    pub fn open(root: &Path) -> WritResult<Self> {
        let writ_dir = root.join(WRIT_DIR);

        if !writ_dir.exists() {
            return Err(WritError::NotARepo);
        }

        let objects = ObjectStore::new(&writ_dir.join("objects"));

        Ok(Self {
            root: root.to_path_buf(),
            writ_dir,
            objects,
        })
    }

    /// Get the working directory state.
    pub fn state(&self) -> WritResult<WorkingState> {
        let index = self.load_index()?;
        let rules = self.ignore_rules();
        Ok(state::compute_state(&self.root, &index, &rules))
    }

    /// Create a seal from all current changes.
    pub fn seal(
        &self,
        agent: AgentIdentity,
        summary: String,
        spec_id: Option<String>,
        status: TaskStatus,
        verification: Verification,
    ) -> WritResult<Seal> {
        let mut index = self.load_index()?;
        let rules = self.ignore_rules();
        let working_state = state::compute_state(&self.root, &index, &rules);

        if working_state.is_clean() {
            return Err(WritError::NothingToSeal);
        }

        // Build the change list and update the index
        let mut changes = Vec::new();

        for file_state in &working_state.changes {
            match file_state.status {
                FileStatus::New | FileStatus::Modified => {
                    let content = fs::read(self.root.join(&file_state.path))?;
                    let new_hash = self.objects.store(&content)?;
                    let old_hash = index.get_hash(&file_state.path).map(String::from);

                    let change_type = if file_state.status == FileStatus::New {
                        ChangeType::Added
                    } else {
                        ChangeType::Modified
                    };

                    changes.push(FileChange {
                        path: file_state.path.clone(),
                        change_type,
                        old_hash,
                        new_hash: Some(new_hash.clone()),
                    });

                    // Update index
                    let size = content.len() as u64;
                    index.upsert(&file_state.path, new_hash, size);
                }
                FileStatus::Deleted => {
                    let old_hash = index.get_hash(&file_state.path).map(String::from);
                    changes.push(FileChange {
                        path: file_state.path.clone(),
                        change_type: ChangeType::Deleted,
                        old_hash,
                        new_hash: None,
                    });
                    index.remove(&file_state.path);
                }
            }
        }

        // Build tree hash from the current index state
        let tree_json = serde_json::to_string(&index.entries)?;
        let tree_hash = self.objects.store(tree_json.as_bytes())?;

        // Read the current HEAD to get the parent seal
        let parent = self.read_head()?;

        // Create the seal
        let seal = Seal::new(
            parent,
            tree_hash,
            agent,
            spec_id.clone(),
            status,
            changes,
            verification,
            summary,
        );

        // Save the seal
        self.save_seal(&seal)?;

        // Update HEAD
        fs::write(self.writ_dir.join("HEAD"), &seal.id)?;

        // Save updated index
        index.save(&self.writ_dir.join("index.json"))?;

        // If linked to a spec, record the seal ID on the spec
        if let Some(ref sid) = spec_id {
            if let Ok(mut spec) = self.load_spec(sid) {
                spec.sealed_by.push(seal.id.clone());
                spec.updated_at = chrono::Utc::now();
                self.save_spec(&spec)?;
            }
        }

        Ok(seal)
    }

    /// Get the seal history (newest first).
    pub fn log(&self) -> WritResult<Vec<Seal>> {
        let mut seals = Vec::new();
        let mut current = self.read_head()?;

        while let Some(seal_id) = current {
            let seal = self.load_seal(&seal_id)?;
            current = seal.parent.clone();
            seals.push(seal);
        }

        Ok(seals)
    }

    /// Add a new spec to the repository.
    pub fn add_spec(&self, spec: &Spec) -> WritResult<()> {
        self.save_spec(spec)
    }

    /// List all specs.
    pub fn list_specs(&self) -> WritResult<Vec<Spec>> {
        let specs_dir = self.writ_dir.join("specs");
        let mut specs = Vec::new();

        if !specs_dir.exists() {
            return Ok(specs);
        }

        for entry in fs::read_dir(&specs_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                let data = fs::read_to_string(&path)?;
                let spec: Spec = serde_json::from_str(&data)?;
                specs.push(spec);
            }
        }

        specs.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(specs)
    }

    /// Load a spec by ID.
    pub fn load_spec(&self, id: &str) -> WritResult<Spec> {
        let path = self.writ_dir.join("specs").join(format!("{id}.json"));
        if !path.exists() {
            return Err(WritError::SpecNotFound(id.to_string()));
        }
        let data = fs::read_to_string(&path)?;
        let spec: Spec = serde_json::from_str(&data)?;
        Ok(spec)
    }

    /// Diff working tree against the last seal (HEAD).
    ///
    /// If no seals exist, the entire working tree appears as additions.
    pub fn diff(&self) -> WritResult<DiffOutput> {
        let index = self.load_index()?;
        let head = self.read_head()?;

        let sealed_index = if let Some(ref seal_id) = head {
            let seal = self.load_seal(seal_id)?;
            self.load_tree_index(&seal.tree)?
        } else {
            Index::default()
        };

        let rules = self.ignore_rules();
        let working_state = state::compute_state(&self.root, &index, &rules);
        let mut files = Vec::new();

        for file_state in &working_state.changes {
            let file_diff = match file_state.status {
                FileStatus::New => {
                    let content = fs::read(self.root.join(&file_state.path))?;
                    self.compute_file_diff(
                        &file_state.path,
                        ChangeType::Added,
                        &[],
                        &content,
                        3,
                    )
                }
                FileStatus::Modified => {
                    // Prefer sealed index for the "before" content. Falls back to current
                    // index for the edge case where no seals exist yet (shouldn't happen
                    // for Modified status, but defensive).
                    let old_hash = sealed_index
                        .get_hash(&file_state.path)
                        .or_else(|| index.get_hash(&file_state.path));
                    let old_content = if let Some(hash) = old_hash {
                        self.objects.retrieve(hash)?
                    } else {
                        Vec::new()
                    };
                    let new_content = fs::read(self.root.join(&file_state.path))?;
                    self.compute_file_diff(
                        &file_state.path,
                        ChangeType::Modified,
                        &old_content,
                        &new_content,
                        3,
                    )
                }
                FileStatus::Deleted => {
                    let old_hash = sealed_index.get_hash(&file_state.path);
                    let old_content = if let Some(hash) = old_hash {
                        self.objects.retrieve(hash)?
                    } else {
                        Vec::new()
                    };
                    self.compute_file_diff(
                        &file_state.path,
                        ChangeType::Deleted,
                        &old_content,
                        &[],
                        3,
                    )
                }
            };
            files.push(file_diff);
        }

        files.sort_by(|a, b| a.path.cmp(&b.path));
        let total_additions = files.iter().map(|f| f.additions).sum();
        let total_deletions = files.iter().map(|f| f.deletions).sum();
        let files_changed = files.len();

        Ok(DiffOutput {
            description: if head.is_some() {
                "working tree vs HEAD".to_string()
            } else {
                "working tree vs empty".to_string()
            },
            files,
            files_changed,
            total_additions,
            total_deletions,
        })
    }

    /// Diff between two seals by their IDs (supports short ID prefix).
    pub fn diff_seals(&self, old_id: &str, new_id: &str) -> WritResult<DiffOutput> {
        let old_full = self.resolve_seal_id(old_id)?;
        let new_full = self.resolve_seal_id(new_id)?;

        let old_seal = self.load_seal(&old_full)?;
        let new_seal = self.load_seal(&new_full)?;

        let old_index = self.load_tree_index(&old_seal.tree)?;
        let new_index = self.load_tree_index(&new_seal.tree)?;

        let files = self.diff_indices(&old_index, &new_index)?;
        let total_additions = files.iter().map(|f| f.additions).sum();
        let total_deletions = files.iter().map(|f| f.deletions).sum();
        let files_changed = files.len();

        Ok(DiffOutput {
            description: format!("seal {}..{}", &old_full[..12], &new_full[..12]),
            files,
            files_changed,
            total_additions,
            total_deletions,
        })
    }

    /// Generate a structured context dump optimized for LLM consumption.
    pub fn context(
        &self,
        scope: ContextScope,
        seal_limit: usize,
    ) -> WritResult<ContextOutput> {
        let working_state = self.state()?;
        let seals = self.log()?;
        let ws_summary = WorkingStateSummary::from_state(&working_state);

        let pending_changes = if !working_state.is_clean() {
            let diff_output = self.diff()?;
            Some(DiffSummary::from_diff(&diff_output))
        } else {
            None
        };

        match scope {
            ContextScope::Full => {
                let specs = self.list_specs()?;
                let recent: Vec<SealSummary> = seals
                    .iter()
                    .take(seal_limit)
                    .map(SealSummary::from_seal)
                    .collect();

                let index = self.load_index()?;
                let file_scope: Vec<String> = index.entries.keys().cloned().collect();
                let tracked_files = index.entries.len();

                Ok(ContextOutput {
                    writ_version: "0.1.0".to_string(),
                    active_spec: None,
                    all_specs: if specs.is_empty() { None } else { Some(specs) },
                    working_state: ws_summary,
                    recent_seals: recent,
                    pending_changes,
                    file_scope,
                    tracked_files,
                })
            }
            ContextScope::Spec(spec_id) => {
                let spec = self.load_spec(&spec_id)?;
                let spec_seals: Vec<SealSummary> = seals
                    .iter()
                    .filter(|s| s.spec_id.as_deref() == Some(spec_id.as_str()))
                    .take(seal_limit)
                    .map(SealSummary::from_seal)
                    .collect();

                let file_scope = if spec.file_scope.is_empty() {
                    let index = self.load_index()?;
                    index.entries.keys().cloned().collect()
                } else {
                    spec.file_scope.clone()
                };
                let tracked_files = file_scope.len();

                Ok(ContextOutput {
                    writ_version: "0.1.0".to_string(),
                    active_spec: Some(spec),
                    all_specs: None,
                    working_state: ws_summary,
                    recent_seals: spec_seals,
                    pending_changes,
                    file_scope,
                    tracked_files,
                })
            }
        }
    }

    /// Restore the working directory to match a specific seal's state.
    ///
    /// Updates files on disk, the index, and HEAD. Does not create a new seal.
    /// Untracked files are left alone.
    pub fn restore(&self, seal_id: &str) -> WritResult<RestoreResult> {
        let full_id = self.resolve_seal_id(seal_id)?;
        let seal = self.load_seal(&full_id)?;
        let target_index = self.load_tree_index(&seal.tree)?;
        let current_index = self.load_index()?;

        let mut created = Vec::new();
        let mut modified = Vec::new();
        let mut deleted = Vec::new();

        // Write/update all files from the target index
        for (rel_path, entry) in &target_index.entries {
            let full_path = self.root.join(rel_path);

            if let Some(parent) = full_path.parent() {
                fs::create_dir_all(parent)?;
            }

            let content = self.objects.retrieve(&entry.hash)?;
            let exists_on_disk = full_path.exists();

            let needs_write = if exists_on_disk {
                let disk_content = fs::read(&full_path)?;
                let disk_hash = crate::hash::hash_bytes(&disk_content);
                disk_hash != entry.hash
            } else {
                true
            };

            if needs_write {
                fs::write(&full_path, &content)?;
                if exists_on_disk {
                    modified.push(rel_path.clone());
                } else {
                    created.push(rel_path.clone());
                }
            }
        }

        // Delete tracked files not in the target index
        for tracked_path in current_index.entries.keys() {
            if !target_index.entries.contains_key(tracked_path) {
                let full_path = self.root.join(tracked_path);
                if full_path.exists() {
                    fs::remove_file(&full_path)?;
                    deleted.push(tracked_path.clone());
                }
                // Clean up empty parent directories (best-effort)
                if let Some(parent) = full_path.parent() {
                    let _ = Self::remove_empty_dirs(parent, &self.root);
                }
            }
        }

        // Update index to match target
        target_index.save(&self.writ_dir.join("index.json"))?;

        // Update HEAD
        fs::write(self.writ_dir.join("HEAD"), &seal.id)?;

        let total_files = target_index.entries.len();

        Ok(RestoreResult {
            seal_id: seal.id,
            created,
            modified,
            deleted,
            total_files,
        })
    }

    /// Update a spec's mutable fields. Bumps `updated_at`.
    pub fn update_spec(&self, id: &str, update: SpecUpdate) -> WritResult<Spec> {
        let mut spec = self.load_spec(id)?;

        if let Some(status) = update.status {
            spec.status = status;
        }
        if let Some(depends_on) = update.depends_on {
            spec.depends_on = depends_on;
        }
        if let Some(file_scope) = update.file_scope {
            spec.file_scope = file_scope;
        }

        spec.updated_at = chrono::Utc::now();
        self.save_spec(&spec)?;
        Ok(spec)
    }

    /// Load a seal by full or short ID.
    pub fn get_seal(&self, id: &str) -> WritResult<Seal> {
        let full_id = self.resolve_seal_id(id)?;
        self.load_seal(&full_id)
    }

    /// Compute the diff introduced by a specific seal (vs its parent, or vs empty).
    pub fn diff_seal(&self, seal_id: &str) -> WritResult<DiffOutput> {
        let full_id = self.resolve_seal_id(seal_id)?;
        let seal = self.load_seal(&full_id)?;

        let new_index = self.load_tree_index(&seal.tree)?;
        let old_index = if let Some(ref parent_id) = seal.parent {
            let parent = self.load_seal(parent_id)?;
            self.load_tree_index(&parent.tree)?
        } else {
            Index::default()
        };

        let files = self.diff_indices(&old_index, &new_index)?;
        let total_additions = files.iter().map(|f| f.additions).sum();
        let total_deletions = files.iter().map(|f| f.deletions).sum();
        let files_changed = files.len();

        let description = if seal.parent.is_some() {
            format!("seal {} vs parent", &full_id[..12])
        } else {
            format!("seal {} vs empty", &full_id[..12])
        };

        Ok(DiffOutput {
            description,
            files,
            files_changed,
            total_additions,
            total_deletions,
        })
    }

    /// Create a seal from changes matching the given paths only.
    ///
    /// Paths are matched exactly or as directory prefixes.
    /// Remaining changes stay pending.
    pub fn seal_paths(
        &self,
        agent: AgentIdentity,
        summary: String,
        spec_id: Option<String>,
        status: TaskStatus,
        verification: Verification,
        paths: &[String],
    ) -> WritResult<Seal> {
        let mut index = self.load_index()?;
        let rules = self.ignore_rules();
        let working_state = state::compute_state(&self.root, &index, &rules);

        let matching_changes: Vec<_> = working_state
            .changes
            .iter()
            .filter(|fs| {
                paths.iter().any(|p| {
                    fs.path == *p || fs.path.starts_with(&format!("{p}/"))
                })
            })
            .collect();

        if matching_changes.is_empty() {
            return Err(WritError::NothingToSeal);
        }

        let mut changes = Vec::new();

        for file_state in &matching_changes {
            match file_state.status {
                FileStatus::New | FileStatus::Modified => {
                    let content = fs::read(self.root.join(&file_state.path))?;
                    let new_hash = self.objects.store(&content)?;
                    let old_hash = index.get_hash(&file_state.path).map(String::from);

                    let change_type = if file_state.status == FileStatus::New {
                        ChangeType::Added
                    } else {
                        ChangeType::Modified
                    };

                    changes.push(FileChange {
                        path: file_state.path.clone(),
                        change_type,
                        old_hash,
                        new_hash: Some(new_hash.clone()),
                    });

                    let size = content.len() as u64;
                    index.upsert(&file_state.path, new_hash, size);
                }
                FileStatus::Deleted => {
                    let old_hash = index.get_hash(&file_state.path).map(String::from);
                    changes.push(FileChange {
                        path: file_state.path.clone(),
                        change_type: ChangeType::Deleted,
                        old_hash,
                        new_hash: None,
                    });
                    index.remove(&file_state.path);
                }
            }
        }

        let tree_json = serde_json::to_string(&index.entries)?;
        let tree_hash = self.objects.store(tree_json.as_bytes())?;
        let parent = self.read_head()?;

        let seal = Seal::new(
            parent,
            tree_hash,
            agent,
            spec_id.clone(),
            status,
            changes,
            verification,
            summary,
        );

        self.save_seal(&seal)?;
        fs::write(self.writ_dir.join("HEAD"), &seal.id)?;
        index.save(&self.writ_dir.join("index.json"))?;

        if let Some(ref sid) = spec_id {
            if let Ok(mut spec) = self.load_spec(sid) {
                spec.sealed_by.push(seal.id.clone());
                spec.updated_at = chrono::Utc::now();
                self.save_spec(&spec)?;
            }
        }

        Ok(seal)
    }

    // --- Internal helpers ---

    fn ignore_rules(&self) -> IgnoreRules {
        IgnoreRules::load(&self.root)
    }

    fn load_index(&self) -> WritResult<Index> {
        Index::load(&self.writ_dir.join("index.json"))
    }

    fn read_head(&self) -> WritResult<Option<String>> {
        let head_path = self.writ_dir.join("HEAD");
        let content = fs::read_to_string(&head_path)?;
        let trimmed = content.trim();
        if trimmed.is_empty() {
            Ok(None)
        } else {
            Ok(Some(trimmed.to_string()))
        }
    }

    /// Load a seal by its full ID (low-level).
    ///
    /// Prefer [`get_seal`](Self::get_seal) which also handles short ID prefixes.
    pub fn load_seal(&self, id: &str) -> WritResult<Seal> {
        let path = self.writ_dir.join("seals").join(format!("{id}.json"));
        if !path.exists() {
            return Err(WritError::ObjectNotFound(id.to_string()));
        }
        let data = fs::read_to_string(&path)?;
        let seal: Seal = serde_json::from_str(&data)?;
        Ok(seal)
    }

    fn save_seal(&self, seal: &Seal) -> WritResult<()> {
        let path = self.writ_dir.join("seals").join(format!("{}.json", seal.id));
        let json = serde_json::to_string_pretty(seal)?;
        fs::write(path, json)?;
        Ok(())
    }

    fn save_spec(&self, spec: &Spec) -> WritResult<()> {
        let path = self.writ_dir.join("specs").join(format!("{}.json", spec.id));
        let json = serde_json::to_string_pretty(spec)?;
        fs::write(path, json)?;
        Ok(())
    }

    /// Load the Index stored at a seal's tree hash.
    ///
    /// The tree hash points to a serialized `BTreeMap<String, IndexEntry>`,
    /// which we wrap into an Index struct.
    fn load_tree_index(&self, tree_hash: &str) -> WritResult<Index> {
        let data = self.objects.retrieve(tree_hash)?;
        let entries: BTreeMap<String, IndexEntry> = serde_json::from_slice(&data)?;
        Ok(Index { entries })
    }

    /// Resolve a potentially-short seal ID to a full seal ID.
    ///
    /// Scans the seals directory for a unique prefix match.
    pub fn resolve_seal_id(&self, short_id: &str) -> WritResult<String> {
        // If it looks like a full hash (64 chars), use directly
        if short_id.len() == 64 {
            let path = self.writ_dir.join("seals").join(format!("{short_id}.json"));
            if path.exists() {
                return Ok(short_id.to_string());
            }
            return Err(WritError::SealNotFound(short_id.to_string()));
        }

        let seals_dir = self.writ_dir.join("seals");
        let mut matches = Vec::new();

        for entry in fs::read_dir(&seals_dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(id) = name.strip_suffix(".json") {
                if id.starts_with(short_id) {
                    matches.push(id.to_string());
                }
            }
        }

        match matches.len() {
            0 => Err(WritError::SealNotFound(short_id.to_string())),
            1 => Ok(matches.into_iter().next().unwrap()),
            _ => Err(WritError::Other(format!(
                "ambiguous seal ID '{short_id}' matches {} seals",
                matches.len()
            ))),
        }
    }

    /// Compute file diffs between two index snapshots.
    fn diff_indices(&self, old_index: &Index, new_index: &Index) -> WritResult<Vec<FileDiff>> {
        let mut files = Vec::new();
        let mut all_paths: Vec<String> = old_index.entries.keys().cloned().collect();
        for key in new_index.entries.keys() {
            if !old_index.entries.contains_key(key) {
                all_paths.push(key.clone());
            }
        }
        all_paths.sort();

        for path in &all_paths {
            let old_entry = old_index.entries.get(path);
            let new_entry = new_index.entries.get(path);

            match (old_entry, new_entry) {
                (Some(old_e), Some(new_e)) if old_e.hash != new_e.hash => {
                    let old_content = self.objects.retrieve(&old_e.hash)?;
                    let new_content = self.objects.retrieve(&new_e.hash)?;
                    files.push(self.compute_file_diff(
                        path,
                        ChangeType::Modified,
                        &old_content,
                        &new_content,
                        3,
                    ));
                }
                (None, Some(new_e)) => {
                    let new_content = self.objects.retrieve(&new_e.hash)?;
                    files.push(self.compute_file_diff(
                        path,
                        ChangeType::Added,
                        &[],
                        &new_content,
                        3,
                    ));
                }
                (Some(old_e), None) => {
                    let old_content = self.objects.retrieve(&old_e.hash)?;
                    files.push(self.compute_file_diff(
                        path,
                        ChangeType::Deleted,
                        &old_content,
                        &[],
                        3,
                    ));
                }
                _ => {}
            }
        }

        Ok(files)
    }

    /// Compute a FileDiff for a single file given old and new content bytes.
    fn compute_file_diff(
        &self,
        path: &str,
        change_type: ChangeType,
        old_bytes: &[u8],
        new_bytes: &[u8],
        context_lines: usize,
    ) -> FileDiff {
        if diff::is_binary(old_bytes) || diff::is_binary(new_bytes) {
            return FileDiff {
                path: path.to_string(),
                change_type,
                hunks: Vec::new(),
                is_binary: true,
                additions: 0,
                deletions: 0,
            };
        }

        let old_str = String::from_utf8_lossy(old_bytes);
        let new_str = String::from_utf8_lossy(new_bytes);
        let hunks = diff::compute_line_diff(&old_str, &new_str, context_lines);

        let mut additions = 0;
        let mut deletions = 0;
        for hunk in &hunks {
            for line in &hunk.lines {
                match line.op {
                    diff::LineOp::Add => additions += 1,
                    diff::LineOp::Remove => deletions += 1,
                    diff::LineOp::Context => {}
                }
            }
        }

        FileDiff {
            path: path.to_string(),
            change_type,
            hunks,
            is_binary: false,
            additions,
            deletions,
        }
    }

    /// Remove empty directories walking up from `dir` to `stop_at` (exclusive).
    fn remove_empty_dirs(dir: &Path, stop_at: &Path) -> std::io::Result<()> {
        let mut current = dir.to_path_buf();
        while current != stop_at.to_path_buf() {
            if fs::read_dir(&current)?.next().is_none() {
                fs::remove_dir(&current)?;
            } else {
                break;
            }
            match current.parent() {
                Some(p) => current = p.to_path_buf(),
                None => break,
            }
        }
        Ok(())
    }
}

/// The result of a restore operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreResult {
    /// Seal ID that was restored to.
    pub seal_id: String,
    /// Files that were created.
    pub created: Vec<String>,
    /// Files that were modified.
    pub modified: Vec<String>,
    /// Files that were deleted.
    pub deleted: Vec<String>,
    /// Total files in the restored state.
    pub total_files: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seal::AgentType;
    use crate::spec::SpecStatus;
    use tempfile::tempdir;

    fn test_agent() -> AgentIdentity {
        AgentIdentity {
            id: "test-human".to_string(),
            agent_type: AgentType::Human,
        }
    }

    #[test]
    fn test_init_creates_structure() {
        let dir = tempdir().unwrap();
        Repository::init(dir.path()).unwrap();

        assert!(dir.path().join(".writ").exists());
        assert!(dir.path().join(".writ/objects").exists());
        assert!(dir.path().join(".writ/seals").exists());
        assert!(dir.path().join(".writ/specs").exists());
        assert!(dir.path().join(".writ/HEAD").exists());
        assert!(dir.path().join(".writ/index.json").exists());
    }

    #[test]
    fn test_init_twice_fails() {
        let dir = tempdir().unwrap();
        Repository::init(dir.path()).unwrap();
        let result = Repository::init(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_open_nonexistent_fails() {
        let dir = tempdir().unwrap();
        let result = Repository::open(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_state_clean_after_init() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let state = repo.state().unwrap();
        assert!(state.is_clean());
    }

    #[test]
    fn test_seal_with_new_file() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Create a file
        fs::write(dir.path().join("hello.txt"), "hello world").unwrap();

        let seal = repo
            .seal(
                test_agent(),
                "Added hello.txt".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
            )
            .unwrap();

        assert_eq!(seal.changes.len(), 1);
        assert_eq!(seal.changes[0].path, "hello.txt");
        assert_eq!(seal.summary, "Added hello.txt");
    }

    #[test]
    fn test_seal_nothing_fails() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let result = repo.seal(
            test_agent(),
            "empty".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_log_empty() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let log = repo.log().unwrap();
        assert!(log.is_empty());
    }

    #[test]
    fn test_log_after_seals() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("a.txt"), "aaa").unwrap();
        repo.seal(
            test_agent(),
            "first".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
        )
        .unwrap();

        fs::write(dir.path().join("b.txt"), "bbb").unwrap();
        repo.seal(
            test_agent(),
            "second".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
        )
        .unwrap();

        let log = repo.log().unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].summary, "second"); // newest first
        assert_eq!(log[1].summary, "first");
    }

    #[test]
    fn test_spec_lifecycle() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new(
            "auth-migration".to_string(),
            "Migrate to OAuth2".to_string(),
            "Replace password auth with token-based auth".to_string(),
        );

        repo.add_spec(&spec).unwrap();

        let loaded = repo.load_spec("auth-migration").unwrap();
        assert_eq!(loaded.title, "Migrate to OAuth2");
        assert_eq!(loaded.status, SpecStatus::Pending);

        let all = repo.list_specs().unwrap();
        assert_eq!(all.len(), 1);
    }

    // --- Diff tests ---

    #[test]
    fn test_diff_clean_working_tree() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("file.txt"), "hello").unwrap();
        repo.seal(
            test_agent(),
            "initial".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
        )
        .unwrap();

        let diff = repo.diff().unwrap();
        assert!(diff.files.is_empty());
        assert_eq!(diff.files_changed, 0);
    }

    #[test]
    fn test_diff_no_seals_yet() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("new.txt"), "content").unwrap();

        let diff = repo.diff().unwrap();
        assert_eq!(diff.files_changed, 1);
        assert_eq!(diff.description, "working tree vs empty");
        assert!(diff.total_additions > 0);
    }

    #[test]
    fn test_diff_after_modification() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("file.txt"), "line1\nline2\n").unwrap();
        repo.seal(
            test_agent(),
            "initial".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
        )
        .unwrap();

        fs::write(dir.path().join("file.txt"), "line1\nchanged\n").unwrap();

        let diff = repo.diff().unwrap();
        assert_eq!(diff.files_changed, 1);
        assert_eq!(diff.files[0].path, "file.txt");
        assert!(diff.files[0].additions > 0);
        assert!(diff.files[0].deletions > 0);
    }

    #[test]
    fn test_diff_after_deletion() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("file.txt"), "content\n").unwrap();
        repo.seal(
            test_agent(),
            "initial".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
        )
        .unwrap();

        fs::remove_file(dir.path().join("file.txt")).unwrap();

        let diff = repo.diff().unwrap();
        assert_eq!(diff.files_changed, 1);
        assert!(diff.total_deletions > 0);
        assert_eq!(diff.total_additions, 0);
    }

    #[test]
    fn test_diff_seals() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("a.txt"), "original\n").unwrap();
        let seal1 = repo
            .seal(
                test_agent(),
                "first".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
            )
            .unwrap();

        fs::write(dir.path().join("a.txt"), "modified\n").unwrap();
        fs::write(dir.path().join("b.txt"), "new file\n").unwrap();
        let seal2 = repo
            .seal(
                test_agent(),
                "second".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
            )
            .unwrap();

        let diff = repo.diff_seals(&seal1.id, &seal2.id).unwrap();
        assert_eq!(diff.files_changed, 2);
    }

    #[test]
    fn test_diff_seals_nonexistent() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let result = repo.diff_seals("nonexistent", "alsonotreal");
        assert!(result.is_err());
    }

    // --- Context tests ---

    #[test]
    fn test_context_full_empty_repo() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let ctx = repo.context(ContextScope::Full, 10).unwrap();
        assert!(ctx.working_state.clean);
        assert!(ctx.recent_seals.is_empty());
        assert!(ctx.pending_changes.is_none());
        assert_eq!(ctx.tracked_files, 0);
    }

    #[test]
    fn test_context_full_with_seals() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("file.txt"), "hello").unwrap();
        repo.seal(
            test_agent(),
            "initial".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
        )
        .unwrap();

        let ctx = repo.context(ContextScope::Full, 10).unwrap();
        assert!(ctx.working_state.clean);
        assert_eq!(ctx.recent_seals.len(), 1);
        assert_eq!(ctx.recent_seals[0].summary, "initial");
        assert_eq!(ctx.tracked_files, 1);
    }

    #[test]
    fn test_context_spec_scoped() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new(
            "feature-1".to_string(),
            "Feature One".to_string(),
            String::new(),
        );
        repo.add_spec(&spec).unwrap();

        fs::write(dir.path().join("a.txt"), "aaa").unwrap();
        repo.seal(
            test_agent(),
            "for feature".to_string(),
            Some("feature-1".to_string()),
            TaskStatus::InProgress,
            Verification::default(),
        )
        .unwrap();

        fs::write(dir.path().join("b.txt"), "bbb").unwrap();
        repo.seal(
            test_agent(),
            "unrelated".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Spec("feature-1".to_string()), 10)
            .unwrap();
        assert!(ctx.active_spec.is_some());
        assert_eq!(ctx.active_spec.unwrap().id, "feature-1");
        // Only the seal linked to this spec
        assert_eq!(ctx.recent_seals.len(), 1);
        assert_eq!(ctx.recent_seals[0].summary, "for feature");
        assert!(ctx.all_specs.is_none());
    }

    #[test]
    fn test_context_spec_not_found() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let result = repo.context(ContextScope::Spec("nope".to_string()), 10);
        assert!(result.is_err());
    }

    #[test]
    fn test_context_pending_changes() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("file.txt"), "hello").unwrap();
        repo.seal(
            test_agent(),
            "initial".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
        )
        .unwrap();

        fs::write(dir.path().join("file.txt"), "changed").unwrap();

        let ctx = repo.context(ContextScope::Full, 10).unwrap();
        assert!(!ctx.working_state.clean);
        assert!(ctx.pending_changes.is_some());
        let pc = ctx.pending_changes.unwrap();
        assert_eq!(pc.files_changed, 1);
    }

    #[test]
    fn test_context_seal_limit() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        for i in 0..5 {
            fs::write(dir.path().join(format!("f{i}.txt")), format!("content{i}")).unwrap();
            repo.seal(
                test_agent(),
                format!("seal {i}"),
                None,
                TaskStatus::Complete,
                Verification::default(),
            )
            .unwrap();
        }

        let ctx = repo.context(ContextScope::Full, 3).unwrap();
        assert_eq!(ctx.recent_seals.len(), 3);
    }

    // --- Restore tests ---

    #[test]
    fn test_restore_to_previous_seal() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("file.txt"), "original").unwrap();
        let seal1 = repo
            .seal(
                test_agent(),
                "first".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
            )
            .unwrap();

        fs::write(dir.path().join("file.txt"), "modified").unwrap();
        repo.seal(
            test_agent(),
            "second".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
        )
        .unwrap();

        let result = repo.restore(&seal1.id).unwrap();
        assert_eq!(result.modified.len(), 1);
        assert_eq!(result.modified[0], "file.txt");

        let content = fs::read_to_string(dir.path().join("file.txt")).unwrap();
        assert_eq!(content, "original");
    }

    #[test]
    fn test_restore_creates_files() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("a.txt"), "aaa").unwrap();
        fs::write(dir.path().join("b.txt"), "bbb").unwrap();
        let seal1 = repo
            .seal(
                test_agent(),
                "both files".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
            )
            .unwrap();

        // Delete one file and seal
        fs::remove_file(dir.path().join("b.txt")).unwrap();
        repo.seal(
            test_agent(),
            "removed b".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
        )
        .unwrap();

        // Restore to when both files existed
        let result = repo.restore(&seal1.id).unwrap();
        assert!(result.created.contains(&"b.txt".to_string()));
        assert!(dir.path().join("b.txt").exists());
    }

    #[test]
    fn test_restore_deletes_files() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("a.txt"), "aaa").unwrap();
        let seal1 = repo
            .seal(
                test_agent(),
                "just a".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
            )
            .unwrap();

        fs::write(dir.path().join("b.txt"), "bbb").unwrap();
        repo.seal(
            test_agent(),
            "added b".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
        )
        .unwrap();

        // Restore to when only a.txt existed
        let result = repo.restore(&seal1.id).unwrap();
        assert!(result.deleted.contains(&"b.txt".to_string()));
        assert!(!dir.path().join("b.txt").exists());
    }

    #[test]
    fn test_restore_updates_head_and_index() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("file.txt"), "v1").unwrap();
        let seal1 = repo
            .seal(
                test_agent(),
                "v1".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
            )
            .unwrap();

        fs::write(dir.path().join("file.txt"), "v2").unwrap();
        repo.seal(
            test_agent(),
            "v2".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
        )
        .unwrap();

        repo.restore(&seal1.id).unwrap();

        // HEAD should point to seal1
        let head = repo.read_head().unwrap();
        assert_eq!(head, Some(seal1.id));

        // State should be clean
        let state = repo.state().unwrap();
        assert!(state.is_clean());
    }

    #[test]
    fn test_restore_nonexistent_seal() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let result = repo.restore("nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_restore_to_current_head() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("file.txt"), "content").unwrap();
        let seal1 = repo
            .seal(
                test_agent(),
                "first".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
            )
            .unwrap();

        let result = repo.restore(&seal1.id).unwrap();
        assert!(result.created.is_empty());
        assert!(result.modified.is_empty());
        assert!(result.deleted.is_empty());
    }

    #[test]
    fn test_resolve_seal_id_short() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("file.txt"), "content").unwrap();
        let seal = repo
            .seal(
                test_agent(),
                "test".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
            )
            .unwrap();

        // Short prefix should resolve
        let resolved = repo.resolve_seal_id(&seal.id[..8]).unwrap();
        assert_eq!(resolved, seal.id);
    }

    // --- Spec update tests ---

    #[test]
    fn test_update_spec_status() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new("task-1".to_string(), "Task One".to_string(), String::new());
        repo.add_spec(&spec).unwrap();

        let updated = repo
            .update_spec(
                "task-1",
                SpecUpdate {
                    status: Some(SpecStatus::InProgress),
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(updated.status, SpecStatus::InProgress);
    }

    #[test]
    fn test_update_spec_depends_on() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new("task-2".to_string(), "Task Two".to_string(), String::new());
        repo.add_spec(&spec).unwrap();

        let updated = repo
            .update_spec(
                "task-2",
                SpecUpdate {
                    depends_on: Some(vec!["task-1".to_string()]),
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(updated.depends_on, vec!["task-1".to_string()]);
    }

    #[test]
    fn test_update_spec_file_scope() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new("task-3".to_string(), "Task Three".to_string(), String::new());
        repo.add_spec(&spec).unwrap();

        let updated = repo
            .update_spec(
                "task-3",
                SpecUpdate {
                    file_scope: Some(vec!["src/main.rs".to_string(), "lib.rs".to_string()]),
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(updated.file_scope.len(), 2);
    }

    #[test]
    fn test_update_spec_not_found() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let result = repo.update_spec("nonexistent", SpecUpdate::default());
        assert!(result.is_err());
    }

    #[test]
    fn test_update_spec_persists() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new("persist".to_string(), "Persist Test".to_string(), String::new());
        repo.add_spec(&spec).unwrap();

        repo.update_spec(
            "persist",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        // Re-open and verify
        let repo2 = Repository::open(dir.path()).unwrap();
        let loaded = repo2.load_spec("persist").unwrap();
        assert_eq!(loaded.status, SpecStatus::Complete);
    }

    // --- get_seal tests ---

    #[test]
    fn test_get_seal_by_short_id() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("file.txt"), "content").unwrap();
        let seal = repo
            .seal(
                test_agent(),
                "test seal".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
            )
            .unwrap();

        let loaded = repo.get_seal(&seal.id[..8]).unwrap();
        assert_eq!(loaded.id, seal.id);
        assert_eq!(loaded.summary, "test seal");
    }

    // --- diff_seal tests ---

    #[test]
    fn test_diff_seal_first_seal() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        let seal = repo
            .seal(
                test_agent(),
                "initial".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
            )
            .unwrap();

        let diff = repo.diff_seal(&seal.id).unwrap();
        assert_eq!(diff.files_changed, 1);
        assert!(diff.total_additions > 0);
        assert_eq!(diff.total_deletions, 0);
        assert!(diff.description.contains("vs empty"));
    }

    #[test]
    fn test_diff_seal_with_parent() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("a.txt"), "original\n").unwrap();
        repo.seal(
            test_agent(),
            "first".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
        )
        .unwrap();

        fs::write(dir.path().join("a.txt"), "modified\n").unwrap();
        fs::write(dir.path().join("b.txt"), "new\n").unwrap();
        let seal2 = repo
            .seal(
                test_agent(),
                "second".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
            )
            .unwrap();

        let diff = repo.diff_seal(&seal2.id).unwrap();
        assert_eq!(diff.files_changed, 2); // a.txt modified + b.txt added
        assert!(diff.description.contains("vs parent"));
    }

    // --- seal_paths tests ---

    #[test]
    fn test_seal_paths_selective() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("a.txt"), "aaa").unwrap();
        fs::write(dir.path().join("b.txt"), "bbb").unwrap();

        let seal = repo
            .seal_paths(
                test_agent(),
                "only a".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
                &["a.txt".to_string()],
            )
            .unwrap();

        assert_eq!(seal.changes.len(), 1);
        assert_eq!(seal.changes[0].path, "a.txt");

        // b.txt should still show as pending
        let state = repo.state().unwrap();
        assert_eq!(state.changes.len(), 1);
        assert_eq!(state.changes[0].path, "b.txt");
    }

    #[test]
    fn test_seal_paths_nothing_matching() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("a.txt"), "aaa").unwrap();

        let result = repo.seal_paths(
            test_agent(),
            "no match".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            &["nonexistent.txt".to_string()],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_seal_paths_directory_prefix() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(dir.path().join("readme.txt"), "hello").unwrap();

        let seal = repo
            .seal_paths(
                test_agent(),
                "only src".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
                &["src".to_string()],
            )
            .unwrap();

        assert_eq!(seal.changes.len(), 1);
        assert_eq!(seal.changes[0].path, "src/main.rs");

        // readme.txt should still be pending
        let state = repo.state().unwrap();
        assert_eq!(state.changes.len(), 1);
        assert_eq!(state.changes[0].path, "readme.txt");
    }

    #[test]
    fn test_seal_paths_multiple_files() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("a.txt"), "aaa").unwrap();
        fs::write(dir.path().join("b.txt"), "bbb").unwrap();
        fs::write(dir.path().join("c.txt"), "ccc").unwrap();

        let seal = repo
            .seal_paths(
                test_agent(),
                "a and b".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
                &["a.txt".to_string(), "b.txt".to_string()],
            )
            .unwrap();

        assert_eq!(seal.changes.len(), 2);

        let state = repo.state().unwrap();
        assert_eq!(state.changes.len(), 1);
        assert_eq!(state.changes[0].path, "c.txt");
    }

    #[test]
    fn test_seal_paths_with_spec() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new("feat-1".to_string(), "Feature One".to_string(), String::new());
        repo.add_spec(&spec).unwrap();

        fs::write(dir.path().join("a.txt"), "aaa").unwrap();

        repo.seal_paths(
            test_agent(),
            "selective with spec".to_string(),
            Some("feat-1".to_string()),
            TaskStatus::InProgress,
            Verification::default(),
            &["a.txt".to_string()],
        )
        .unwrap();

        let loaded = repo.load_spec("feat-1").unwrap();
        assert_eq!(loaded.sealed_by.len(), 1);
    }

    #[test]
    fn test_seal_paths_interleaved_with_seal_all() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Full seal
        fs::write(dir.path().join("a.txt"), "aaa").unwrap();
        repo.seal(
            test_agent(),
            "all".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
        )
        .unwrap();

        // Selective seal
        fs::write(dir.path().join("b.txt"), "bbb").unwrap();
        fs::write(dir.path().join("c.txt"), "ccc").unwrap();
        repo.seal_paths(
            test_agent(),
            "only b".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            &["b.txt".to_string()],
        )
        .unwrap();

        // Full seal picks up remaining
        let seal = repo
            .seal(
                test_agent(),
                "rest".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
            )
            .unwrap();

        assert_eq!(seal.changes.len(), 1);
        assert_eq!(seal.changes[0].path, "c.txt");

        // Now clean
        let state = repo.state().unwrap();
        assert!(state.is_clean());
    }

    // --- .writignore integration tests ---

    #[test]
    fn test_writignore_hides_files() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join(".writignore"), "*.log\n").unwrap();
        fs::write(dir.path().join("app.log"), "log data").unwrap();
        fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

        let state = repo.state().unwrap();
        // .writignore itself + main.rs visible, but app.log hidden
        let paths: Vec<&str> = state.changes.iter().map(|c| c.path.as_str()).collect();
        assert!(paths.contains(&"main.rs"));
        assert!(!paths.contains(&"app.log"));
    }

    #[test]
    fn test_no_writignore_uses_defaults() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Create a file inside a default-ignored dir
        fs::create_dir_all(dir.path().join("node_modules")).unwrap();
        fs::write(dir.path().join("node_modules/pkg.js"), "module").unwrap();
        fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

        let state = repo.state().unwrap();
        let paths: Vec<&str> = state.changes.iter().map(|c| c.path.as_str()).collect();
        assert!(paths.contains(&"main.rs"));
        assert!(!paths.contains(&"node_modules/pkg.js"));
    }
}
