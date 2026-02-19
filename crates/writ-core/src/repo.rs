//! Repository — the main entry point for writ operations.
//!
//! A Repository ties together the object store, index, seals, and specs
//! into a unified interface.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::convergence::{
    self, ConvergenceReport, FileConflict, FileResolution, FileMergeResult, MergedFile,
};
use crate::lock::RepoLock;
use crate::context::{
    ContextFilter, ContextOutput, ContextScope, DepStatus, DiffSummary, SealNudge, SealSummary,
    SpecProgress, WorkingStateSummary,
};
use crate::diff::{self, DiffOutput, FileDiff};
use crate::error::{WritError, WritResult};
use crate::fsutil::atomic_write;
use crate::ignore::IgnoreRules;
use crate::index::{Index, IndexEntry};
use crate::object::ObjectStore;
use crate::seal::{AgentIdentity, ChangeType, FileChange, Seal, TaskStatus, Verification};
use crate::spec::{Spec, SpecStatus, SpecUpdate};
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

        fs::create_dir_all(writ_dir.join("objects"))?;
        fs::create_dir_all(writ_dir.join("seals"))?;
        fs::create_dir_all(writ_dir.join("specs"))?;
        fs::write(writ_dir.join("HEAD"), "")?;

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

    /// One-command setup: init writ, detect git, and import baseline.
    ///
    /// If `.writ/` already exists, opens the existing repo. If a `.git/`
    /// is found, imports the current git HEAD as the writ baseline.
    #[cfg(feature = "bridge")]
    pub fn install(root: &Path) -> WritResult<InstallResult> {
        let writ_dir = root.join(WRIT_DIR);
        let initialized = !writ_dir.exists();

        let repo = if initialized {
            Self::init(root)?
        } else {
            Self::open(root)?
        };

        let git_detected = root.join(".git").exists()
            || git2::Repository::discover(root).is_ok();

        if git_detected {
            let agent = AgentIdentity {
                id: "writ-bridge".to_string(),
                agent_type: crate::seal::AgentType::Agent,
            };
            match repo.bridge_import(None, agent) {
                Ok(result) => {
                    return Ok(InstallResult {
                        initialized,
                        git_detected: true,
                        git_imported: true,
                        imported_seal_id: Some(result.seal_id),
                        imported_files: Some(result.files_imported),
                    });
                }
                Err(_) => {
                    return Ok(InstallResult {
                        initialized,
                        git_detected: true,
                        git_imported: false,
                        imported_seal_id: None,
                        imported_files: None,
                    });
                }
            }
        }

        Ok(InstallResult {
            initialized,
            git_detected: false,
            git_imported: false,
            imported_seal_id: None,
            imported_files: None,
        })
    }

    /// Default lock timeout for mutable operations.
    const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

    /// Acquire an exclusive lock on the repository.
    fn lock(&self) -> WritResult<RepoLock> {
        RepoLock::acquire(&self.writ_dir, Self::LOCK_TIMEOUT)
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
        allow_empty: bool,
    ) -> WritResult<Seal> {
        Self::validate_agent_id(&agent.id)?;
        let _lock = self.lock()?;
        let mut index = self.load_index()?;
        let rules = self.ignore_rules();
        let working_state = state::compute_state(&self.root, &index, &rules);

        if working_state.is_clean() && !allow_empty {
            return Err(WritError::NothingToSeal);
        }

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
        atomic_write(&self.writ_dir.join("HEAD"), seal.id.as_bytes())?;
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
    ///
    /// `filter` narrows the seal history by status and/or agent. The filter
    /// is applied *before* `seal_limit` truncation.
    pub fn context(
        &self,
        scope: ContextScope,
        seal_limit: usize,
        filter: &ContextFilter,
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

        let seal_nudge = if !working_state.is_clean() {
            let count = working_state.changes.len();
            let msg = format!(
                "{count} file(s) changed since last seal — consider checkpointing with seal()"
            );
            Some(SealNudge {
                unsealed_file_count: count,
                message: msg,
            })
        } else {
            None
        };

        let available_operations = vec![
            "state()".to_string(),
            "seal(summary, agent_id?, spec_id?, status?, allow_empty?)".to_string(),
            "log(limit?)".to_string(),
            "diff()".to_string(),
            "diff_seals(from_id, to_id)".to_string(),
            "diff_seal(seal_id)".to_string(),
            "get_seal(seal_id)".to_string(),
            "restore(seal_id)".to_string(),
            "context(spec?, seal_limit?, status?, agent?)".to_string(),
            "add_spec(id, title, description?)".to_string(),
            "update_spec(id, status?, depends_on?, file_scope?)".to_string(),
            "list_specs()".to_string(),
            "converge(left_spec, right_spec)".to_string(),
            "apply_convergence(report, resolutions?)".to_string(),
            "push(remote?)".to_string(),
            "pull(remote?)".to_string(),
            "remote_init(path)".to_string(),
            "remote_add(name, path)".to_string(),
            "remote_status(remote?)".to_string(),
            "bridge_import(git_ref?)".to_string(),
            "bridge_export(branch?)".to_string(),
            "bridge_status()".to_string(),
        ];

        let apply_filter = |seal: &&Seal| -> bool {
            if let Some(ref status) = filter.status {
                let status_str = match status {
                    TaskStatus::InProgress => "in-progress",
                    TaskStatus::Complete => "complete",
                    TaskStatus::Blocked => "blocked",
                };
                let seal_str = match seal.status {
                    TaskStatus::InProgress => "in-progress",
                    TaskStatus::Complete => "complete",
                    TaskStatus::Blocked => "blocked",
                };
                if status_str != seal_str {
                    return false;
                }
            }
            if let Some(ref agent) = filter.agent {
                if seal.agent.id != *agent {
                    return false;
                }
            }
            true
        };

        match scope {
            ContextScope::Full => {
                let specs = self.list_specs()?;
                let recent: Vec<SealSummary> = seals
                    .iter()
                    .filter(apply_filter)
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
                    seal_nudge,
                    file_scope,
                    tracked_files,
                    dependency_status: None,
                    spec_progress: None,
                    available_operations,
                })
            }
            ContextScope::Spec(spec_id) => {
                let spec = self.load_spec(&spec_id)?;
                let spec_seals: Vec<SealSummary> = seals
                    .iter()
                    .filter(|s| s.spec_id.as_deref() == Some(spec_id.as_str()))
                    .filter(apply_filter)
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

                // Compute dependency status
                let dependency_status = if !spec.depends_on.is_empty() {
                    let deps: Vec<DepStatus> = spec
                        .depends_on
                        .iter()
                        .map(|dep_id| match self.load_spec(dep_id) {
                            Ok(dep_spec) => DepStatus::from_spec(dep_id, &dep_spec.status),
                            Err(_) => DepStatus::not_found(dep_id),
                        })
                        .collect();
                    Some(deps)
                } else {
                    None
                };

                // Compute spec progress
                let spec_progress = if !spec.sealed_by.is_empty() {
                    let mut agents = Vec::new();
                    let mut latest_at: Option<String> = None;
                    for seal_id in &spec.sealed_by {
                        if let Ok(seal) = self.load_seal(seal_id) {
                            if !agents.contains(&seal.agent.id) {
                                agents.push(seal.agent.id.clone());
                            }
                            let ts = seal.timestamp.to_rfc3339();
                            if latest_at.as_ref().map_or(true, |prev| ts > *prev) {
                                latest_at = Some(ts);
                            }
                        }
                    }
                    let status_str = match spec.status {
                        SpecStatus::Pending => "pending",
                        SpecStatus::InProgress => "in-progress",
                        SpecStatus::Complete => "complete",
                        SpecStatus::Blocked => "blocked",
                    };
                    Some(SpecProgress {
                        total_seals: spec.sealed_by.len(),
                        current_status: status_str.to_string(),
                        agents_involved: agents,
                        latest_seal_at: latest_at,
                    })
                } else {
                    None
                };

                Ok(ContextOutput {
                    writ_version: "0.1.0".to_string(),
                    active_spec: Some(spec),
                    all_specs: None,
                    working_state: ws_summary,
                    recent_seals: spec_seals,
                    pending_changes,
                    seal_nudge,
                    file_scope,
                    tracked_files,
                    dependency_status,
                    spec_progress,
                    available_operations,
                })
            }
        }
    }

    /// Restore the working directory to match a specific seal's state.
    ///
    /// Updates files on disk, the index, and HEAD. Does not create a new seal.
    /// Untracked files are left alone.
    pub fn restore(&self, seal_id: &str) -> WritResult<RestoreResult> {
        let _lock = self.lock()?;
        let full_id = self.resolve_seal_id(seal_id)?;
        let seal = self.load_seal(&full_id)?;
        let target_index = self.load_tree_index(&seal.tree)?;
        let current_index = self.load_index()?;

        let mut created = Vec::new();
        let mut modified = Vec::new();
        let mut deleted = Vec::new();

        // Write/update all files from the target index
        for (rel_path, entry) in &target_index.entries {
            let full_path = self.validate_path(rel_path)?;

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

        for tracked_path in current_index.entries.keys() {
            if !target_index.entries.contains_key(tracked_path) {
                let full_path = self.validate_path(tracked_path)?;
                if full_path.exists() {
                    fs::remove_file(&full_path)?;
                    deleted.push(tracked_path.clone());
                }
                if let Some(parent) = full_path.parent() {
                    let _ = Self::remove_empty_dirs(parent, &self.root);
                }
            }
        }

        target_index.save(&self.writ_dir.join("index.json"))?;
        atomic_write(&self.writ_dir.join("HEAD"), seal.id.as_bytes())?;

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
        if let Some(acceptance_criteria) = update.acceptance_criteria {
            spec.acceptance_criteria = acceptance_criteria;
        }
        if let Some(design_notes) = update.design_notes {
            spec.design_notes = design_notes;
        }
        if let Some(tech_stack) = update.tech_stack {
            spec.tech_stack = tech_stack;
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

    // -------------------------------------------------------------------
    // Convergence
    // -------------------------------------------------------------------

    /// Analyze convergence between two specs.
    ///
    /// Performs a three-way merge for each file modified by both specs,
    /// using the state before either spec started as the common base.
    /// Returns a structured report — no side effects.
    pub fn converge(
        &self,
        left_spec: &str,
        right_spec: &str,
    ) -> WritResult<ConvergenceReport> {
        let left_spec_data = self.load_spec(left_spec)?;
        let right_spec_data = self.load_spec(right_spec)?;

        if left_spec_data.sealed_by.is_empty() {
            return Err(WritError::SpecHasNoSeals(left_spec.to_string()));
        }
        if right_spec_data.sealed_by.is_empty() {
            return Err(WritError::SpecHasNoSeals(right_spec.to_string()));
        }

        let left_files = self.spec_modified_files(&left_spec_data)?;
        let right_files = self.spec_modified_files(&right_spec_data)?;

        let left_seal_id = left_spec_data.sealed_by.last().unwrap().clone();
        let right_seal_id = right_spec_data.sealed_by.last().unwrap().clone();
        let left_seal = self.load_seal(&left_seal_id)?;
        let right_seal = self.load_seal(&right_seal_id)?;

        // Find the base: walk the seal chain and find the earliest seal
        // belonging to either spec, then use its parent as base.
        let all_spec_seals: HashSet<&str> = left_spec_data
            .sealed_by
            .iter()
            .chain(right_spec_data.sealed_by.iter())
            .map(|s| s.as_str())
            .collect();

        let chain = self.log()?;
        let mut base_seal_id: Option<String> = None;
        // chain is newest-first; we want the earliest spec seal.
        for seal in chain.iter().rev() {
            if all_spec_seals.contains(seal.id.as_str()) {
                base_seal_id = seal.parent.clone();
                break;
            }
        }

        let base_index = match &base_seal_id {
            Some(id) => {
                let base_seal = self.load_seal(id)?;
                self.load_tree_index(&base_seal.tree)?
            }
            None => Index::default(),
        };
        let left_index = self.load_tree_index(&left_seal.tree)?;
        let right_index = self.load_tree_index(&right_seal.tree)?;

        let both_files: HashSet<&String> = left_files.intersection(&right_files).collect();
        let left_only: Vec<String> = left_files
            .iter()
            .filter(|f| !both_files.contains(f))
            .cloned()
            .collect();
        let right_only: Vec<String> = right_files
            .iter()
            .filter(|f| !both_files.contains(f))
            .cloned()
            .collect();

        let mut auto_merged = Vec::new();
        let mut conflicts = Vec::new();

        for path in &both_files {
            let base_content = self.file_content_at_tree(&base_index, path)?;
            let left_content = self.file_content_at_tree(&left_index, path)?;
            let right_content = self.file_content_at_tree(&right_index, path)?;

            let base_str = base_content.as_deref().unwrap_or("");
            let left_str = left_content.as_deref().unwrap_or("");
            let right_str = right_content.as_deref().unwrap_or("");

            match convergence::three_way_merge(base_str, left_str, right_str) {
                FileMergeResult::Clean(content) => {
                    auto_merged.push(MergedFile {
                        path: path.to_string(),
                        content,
                    });
                }
                FileMergeResult::Conflict(regions) => {
                    conflicts.push(FileConflict {
                        path: path.to_string(),
                        base_content: base_content.clone(),
                        left_content: left_str.to_string(),
                        right_content: right_str.to_string(),
                        regions,
                    });
                }
            }
        }

        let is_clean = conflicts.is_empty();

        Ok(ConvergenceReport {
            left_spec: left_spec.to_string(),
            right_spec: right_spec.to_string(),
            base_seal_id,
            left_seal_id,
            right_seal_id,
            auto_merged,
            conflicts,
            left_only,
            right_only,
            is_clean,
        })
    }

    /// Apply a convergence result to the working directory.
    ///
    /// Writes merged files and resolved conflicts to disk. Does NOT
    /// create a seal — call `seal()` after to capture the result.
    pub fn apply_convergence(
        &self,
        report: &ConvergenceReport,
        resolutions: &[FileResolution],
    ) -> WritResult<()> {
        let unresolved = report
            .conflicts
            .iter()
            .filter(|c| !resolutions.iter().any(|r| r.path == c.path))
            .count();
        if unresolved > 0 {
            return Err(WritError::UnresolvedConflicts(unresolved));
        }

        let _lock = self.lock()?;

        let left_seal = self.load_seal(&report.left_seal_id)?;
        let right_seal = self.load_seal(&report.right_seal_id)?;
        let left_index = self.load_tree_index(&left_seal.tree)?;
        let right_index = self.load_tree_index(&right_seal.tree)?;

        for merged in &report.auto_merged {
            let file_path = self.validate_path(&merged.path)?;
            if let Some(parent) = file_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&file_path, &merged.content)?;
        }

        for path in &report.left_only {
            if let Some(content) = self.file_content_at_tree(&left_index, path)? {
                let file_path = self.validate_path(path)?;
                if let Some(parent) = file_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&file_path, content)?;
            }
        }

        for path in &report.right_only {
            if let Some(content) = self.file_content_at_tree(&right_index, path)? {
                let file_path = self.validate_path(path)?;
                if let Some(parent) = file_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&file_path, content)?;
            }
        }

        for resolution in resolutions {
            let file_path = self.validate_path(&resolution.path)?;
            if let Some(parent) = file_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&file_path, &resolution.content)?;
        }

        Ok(())
    }

    // -------------------------------------------------------------------
    // Convergence helpers
    // -------------------------------------------------------------------

    /// Get file content from a tree index, returned as a UTF-8 string.
    fn file_content_at_tree(&self, index: &Index, path: &str) -> WritResult<Option<String>> {
        if let Some(entry) = index.entries.get(path) {
            let bytes = self.objects.retrieve(&entry.hash)?;
            Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
        } else {
            Ok(None)
        }
    }

    /// Collect all file paths modified by a spec (union of all its seals' changes).
    fn spec_modified_files(&self, spec: &Spec) -> WritResult<HashSet<String>> {
        let mut files = HashSet::new();
        for seal_id in &spec.sealed_by {
            let seal = self.load_seal(seal_id)?;
            for change in &seal.changes {
                files.insert(change.path.clone());
            }
        }
        Ok(files)
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
        allow_empty: bool,
    ) -> WritResult<Seal> {
        Self::validate_agent_id(&agent.id)?;
        let _lock = self.lock()?;
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

        if matching_changes.is_empty() && !allow_empty {
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
        atomic_write(&self.writ_dir.join("HEAD"), seal.id.as_bytes())?;
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

    /// Validate a relative path and return its absolute form within the repo root.
    ///
    /// Rejects absolute paths, `..` components, and any path that would
    /// resolve outside the repository root.
    fn validate_path(&self, rel_path: &str) -> WritResult<PathBuf> {
        if rel_path.starts_with('/') || rel_path.starts_with('\\') {
            return Err(WritError::PathTraversal(rel_path.to_string()));
        }
        for component in Path::new(rel_path).components() {
            if matches!(component, std::path::Component::ParentDir) {
                return Err(WritError::PathTraversal(rel_path.to_string()));
            }
        }
        Ok(self.root.join(rel_path))
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
        atomic_write(&path, json.as_bytes())?;
        Ok(())
    }

    fn save_spec(&self, spec: &Spec) -> WritResult<()> {
        let path = self.writ_dir.join("specs").join(format!("{}.json", spec.id));
        let json = serde_json::to_string_pretty(spec)?;
        atomic_write(&path, json.as_bytes())?;
        Ok(())
    }

    /// Load the Index stored at a seal's tree hash.
    ///
    /// The tree hash points to a serialized `BTreeMap<String, IndexEntry>`,
    /// which we wrap into an Index struct.
    pub(crate) fn load_tree_index(&self, tree_hash: &str) -> WritResult<Index> {
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

    // --- Input validation helpers ---

    /// Validate a git branch name against basic safety rules.
    #[cfg(feature = "bridge")]
    fn validate_branch_name(name: &str) -> WritResult<()> {
        if name.is_empty() || name.len() > 256 {
            return Err(WritError::InvalidInput(format!(
                "branch name must be 1-256 chars, got {}",
                name.len()
            )));
        }
        if name.contains("..") || name.contains("\\") || name.ends_with(".lock") {
            return Err(WritError::InvalidInput(format!(
                "branch name contains forbidden pattern: {name}"
            )));
        }
        if name.bytes().any(|b| b < 0x20 || b == 0x7f || b == b' ' || b == b'~' || b == b'^' || b == b':') {
            return Err(WritError::InvalidInput(format!(
                "branch name contains control or forbidden characters: {name}"
            )));
        }
        Ok(())
    }

    /// Validate a git ref string for basic safety.
    #[cfg(feature = "bridge")]
    fn validate_git_ref(refstr: &str) -> WritResult<()> {
        if refstr.is_empty() || refstr.len() > 512 {
            return Err(WritError::InvalidInput(format!(
                "git ref must be 1-512 chars, got {}",
                refstr.len()
            )));
        }
        Ok(())
    }

    /// Validate an agent ID (alphanumeric, hyphens, underscores, dots).
    fn validate_agent_id(id: &str) -> WritResult<()> {
        if id.is_empty() || id.len() > 128 {
            return Err(WritError::InvalidInput(format!(
                "agent ID must be 1-128 chars, got {}",
                id.len()
            )));
        }
        if !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.') {
            return Err(WritError::InvalidInput(format!(
                "agent ID contains invalid characters: {id}"
            )));
        }
        Ok(())
    }

    // --- Bridge: git <> writ round-trip ---

    #[cfg(feature = "bridge")]
    fn load_bridge_state(&self) -> WritResult<crate::bridge::BridgeState> {
        let path = self.writ_dir.join("bridge.json");
        if !path.exists() {
            return Ok(crate::bridge::BridgeState::default());
        }
        let data = fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&data)?)
    }

    #[cfg(feature = "bridge")]
    fn save_bridge_state(&self, state: &crate::bridge::BridgeState) -> WritResult<()> {
        let path = self.writ_dir.join("bridge.json");
        let json = serde_json::to_string_pretty(state)?;
        atomic_write(&path, json.as_bytes())?;
        Ok(())
    }

    /// Import git state as a baseline writ seal.
    ///
    /// Reads the tree at `git_ref` (default "HEAD"), stores all file contents
    /// in writ's object store, and creates a seal representing that snapshot.
    #[cfg(feature = "bridge")]
    pub fn bridge_import(
        &self,
        git_ref: Option<&str>,
        agent: AgentIdentity,
    ) -> WritResult<crate::bridge::ImportResult> {
        use crate::bridge::{BridgeState, ImportResult};

        let git_ref_str = git_ref.unwrap_or("HEAD");
        Self::validate_git_ref(git_ref_str)?;
        Self::validate_agent_id(&agent.id)?;

        // Open the git repository (discover walks up to find .git/)
        let git_repo = git2::Repository::discover(&self.root)
            .map_err(|_| WritError::NoGitRepo)?;

        // Resolve ref to a commit
        let obj = git_repo
            .revparse_single(git_ref_str)
            .map_err(|e| WritError::GitError(format!("cannot resolve '{}': {}", git_ref_str, e)))?;
        let commit = obj
            .peel_to_commit()
            .map_err(|e| WritError::GitError(format!("not a commit: {}", e)))?;
        let git_commit_hash = commit.id().to_string();
        let tree = commit.tree()?;

        // Walk the git tree, store every blob in writ's object store
        let _lock = self.lock()?;
        let mut index = Index::default();
        let mut changes = Vec::new();

        self.walk_git_tree(&git_repo, &tree, "", &mut index, &mut changes)?;

        let tree_json = serde_json::to_string(&index.entries)?;
        let tree_hash = self.objects.store(tree_json.as_bytes())?;
        let parent = self.read_head()?;

        let short_hash = &git_commit_hash[..12.min(git_commit_hash.len())];
        let seal = Seal::new(
            parent,
            tree_hash,
            agent,
            None,
            TaskStatus::Complete,
            changes.clone(),
            Verification::default(),
            format!("bridge import from git {short_hash}"),
        );

        self.save_seal(&seal)?;
        atomic_write(&self.writ_dir.join("HEAD"), seal.id.as_bytes())?;
        index.save(&self.writ_dir.join("index.json"))?;

        let bridge_state = BridgeState {
            last_imported_git_commit: Some(git_commit_hash.clone()),
            last_imported_seal_id: Some(seal.id.clone()),
            imported_from_ref: Some(git_ref_str.to_string()),
            last_sync_at: Some(chrono::Utc::now()),
            ..Default::default()
        };
        self.save_bridge_state(&bridge_state)?;

        Ok(ImportResult {
            git_commit: git_commit_hash,
            git_ref: git_ref_str.to_string(),
            seal_id: seal.id,
            files_imported: changes.len(),
        })
    }

    /// Recursively walk a git tree and store all blobs in writ's object store.
    #[cfg(feature = "bridge")]
    fn walk_git_tree(
        &self,
        git_repo: &git2::Repository,
        tree: &git2::Tree,
        prefix: &str,
        index: &mut Index,
        changes: &mut Vec<FileChange>,
    ) -> WritResult<()> {
        for entry in tree.iter() {
            let name = entry.name().unwrap_or("");
            let path = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };

            // Skip .writ/ and .git/ directories
            if path == ".writ" || path == ".git" || path.starts_with(".writ/") || path.starts_with(".git/") {
                continue;
            }

            match entry.kind() {
                Some(git2::ObjectType::Blob) => {
                    let obj = entry.to_object(git_repo)?;
                    let blob = obj.as_blob().ok_or_else(|| {
                        WritError::GitError(format!("expected blob at {path}"))
                    })?;
                    let content = blob.content();
                    let hash = self.objects.store(content)?;
                    let size = content.len() as u64;
                    index.upsert(&path, hash.clone(), size);
                    changes.push(FileChange {
                        path,
                        change_type: ChangeType::Added,
                        old_hash: None,
                        new_hash: Some(hash),
                    });
                }
                Some(git2::ObjectType::Tree) => {
                    let obj = entry.to_object(git_repo)?;
                    let subtree = obj.as_tree().ok_or_else(|| {
                        WritError::GitError(format!("expected tree at {path}"))
                    })?;
                    self.walk_git_tree(git_repo, subtree, &path, index, changes)?;
                }
                _ => {} // skip submodules, etc.
            }
        }
        Ok(())
    }

    /// Export writ seals as git commits on a branch.
    ///
    /// Creates one git commit per seal since the last export (or since
    /// the import baseline).
    #[cfg(feature = "bridge")]
    pub fn bridge_export(
        &self,
        branch: Option<&str>,
    ) -> WritResult<crate::bridge::ExportResult> {
        use crate::bridge::{ExportResult, ExportedSeal};

        let branch_name = branch.unwrap_or("writ/export");
        Self::validate_branch_name(branch_name)?;
        let mut bridge_state = self.load_bridge_state()?;

        if bridge_state.last_imported_git_commit.is_none() {
            return Err(WritError::BridgeError(
                "import required before export — run bridge_import first".to_string(),
            ));
        }

        let git_repo = git2::Repository::discover(&self.root)
            .map_err(|_| WritError::NoGitRepo)?;

        // Determine the boundary seal (last export or last import)
        let boundary_seal_id = bridge_state
            .last_exported_seal_id
            .as_deref()
            .or(bridge_state.last_imported_seal_id.as_deref())
            .unwrap()
            .to_string();

        let all_seals = self.log()?;
        let mut to_export = Vec::new();
        for seal in &all_seals {
            if seal.id == boundary_seal_id {
                break;
            }
            to_export.push(seal);
        }
        to_export.reverse(); // oldest first for commit ordering

        if to_export.is_empty() {
            return Ok(ExportResult {
                branch: branch_name.to_string(),
                exported: Vec::new(),
                seals_exported: 0,
            });
        }

        let parent_git_hash = bridge_state
            .last_exported_git_commit
            .as_deref()
            .or(bridge_state.last_imported_git_commit.as_deref())
            .unwrap();
        let parent_oid = git2::Oid::from_str(parent_git_hash)?;
        let mut parent_commit = git_repo.find_commit(parent_oid)?;

        let mut exported = Vec::new();

        for seal in &to_export {
            let writ_index = self.load_tree_index(&seal.tree)?;
            let git_tree_oid = self.build_git_tree(&git_repo, &writ_index)?;
            let git_tree = git_repo.find_tree(git_tree_oid)?;

            // Build commit message with trailers
            let mut message = seal.summary.clone();
            message.push_str("\n\n");
            message.push_str(&format!("Writ-Seal-Id: {}\n", seal.id));
            if let Some(ref spec) = seal.spec_id {
                message.push_str(&format!("Writ-Spec: {spec}\n"));
            }
            let status_str = match seal.status {
                TaskStatus::InProgress => "in-progress",
                TaskStatus::Complete => "complete",
                TaskStatus::Blocked => "blocked",
            };
            message.push_str(&format!("Writ-Status: {status_str}\n"));
            if let Some(p) = seal.verification.tests_passed {
                message.push_str(&format!("Writ-Tests-Passed: {p}\n"));
            }
            if let Some(f) = seal.verification.tests_failed {
                message.push_str(&format!("Writ-Tests-Failed: {f}\n"));
            }
            if seal.verification.linted {
                message.push_str("Writ-Linted: true\n");
            }

            // Create author signature from seal agent + timestamp
            let timestamp = seal.timestamp.timestamp();
            let sig = git2::Signature::new(
                &seal.agent.id,
                &format!("{}@writ", seal.agent.id),
                &git2::Time::new(timestamp, 0),
            )?;

            let new_commit_oid = git_repo.commit(
                None, // don't update any ref yet
                &sig,
                &sig,
                &message,
                &git_tree,
                &[&parent_commit],
            )?;

            exported.push(ExportedSeal {
                seal_id: seal.id.clone(),
                git_commit: new_commit_oid.to_string(),
                summary: seal.summary.clone(),
                agent_id: Some(seal.agent.id.clone()),
            });

            parent_commit = git_repo.find_commit(new_commit_oid)?;
        }

        // Point the branch at the final commit
        let final_oid = parent_commit.id();
        let refname = format!("refs/heads/{branch_name}");
        git_repo.reference(&refname, final_oid, true, "writ bridge export")?;

        bridge_state.last_exported_seal_id = Some(to_export.last().unwrap().id.clone());
        bridge_state.exported_to_branch = Some(branch_name.to_string());
        bridge_state.last_exported_git_commit = Some(final_oid.to_string());
        bridge_state.last_sync_at = Some(chrono::Utc::now());
        self.save_bridge_state(&bridge_state)?;

        let seals_exported = exported.len();
        Ok(ExportResult {
            branch: branch_name.to_string(),
            exported,
            seals_exported,
        })
    }

    /// Build a nested git tree from a flat writ index.
    #[cfg(feature = "bridge")]
    fn build_git_tree(
        &self,
        git_repo: &git2::Repository,
        writ_index: &Index,
    ) -> WritResult<git2::Oid> {
        let mut tree_builder = git_repo.treebuilder(None)?;

        // Partition entries: files at this level vs subdirectories
        let mut subdirs: BTreeMap<String, Index> = BTreeMap::new();

        for (path, entry) in &writ_index.entries {
            if let Some(slash_pos) = path.find('/') {
                let dir = &path[..slash_pos];
                let rest = &path[slash_pos + 1..];
                subdirs
                    .entry(dir.to_string())
                    .or_default()
                    .entries
                    .insert(rest.to_string(), entry.clone());
            } else {
                // File at this level — create blob
                let content = self.objects.retrieve(&entry.hash)?;
                let blob_oid = git_repo.blob(&content)?;
                tree_builder.insert(path, blob_oid, 0o100644)?;
            }
        }

        // Recurse into subdirectories
        for (dir_name, sub_index) in &subdirs {
            let sub_tree_oid = self.build_git_tree(git_repo, &sub_index)?;
            tree_builder.insert(dir_name, sub_tree_oid, 0o040000)?;
        }

        let tree_oid = tree_builder.write()?;
        Ok(tree_oid)
    }

    /// Get current bridge sync status.
    #[cfg(feature = "bridge")]
    pub fn bridge_status(&self) -> WritResult<crate::bridge::BridgeStatus> {
        use crate::bridge::{BridgeStatus, ExportSummary, ImportSummary};

        let state = self.load_bridge_state()?;

        if state.last_imported_git_commit.is_none() {
            return Ok(BridgeStatus {
                initialized: false,
                last_import: None,
                last_export: None,
                pending_export_count: 0,
            });
        }

        let last_import = Some(ImportSummary {
            git_commit: state.last_imported_git_commit.clone().unwrap(),
            git_ref: state.imported_from_ref.clone().unwrap_or_default(),
            seal_id: state.last_imported_seal_id.clone().unwrap(),
        });

        let last_export = match (&state.last_exported_seal_id, &state.last_exported_git_commit, &state.exported_to_branch) {
            (Some(seal_id), Some(git_commit), Some(branch)) => Some(ExportSummary {
                seal_id: seal_id.clone(),
                git_commit: git_commit.clone(),
                branch: branch.clone(),
            }),
            _ => None,
        };

        // Count pending seals
        let boundary = state.last_exported_seal_id.as_deref()
            .or(state.last_imported_seal_id.as_deref())
            .unwrap();
        let all_seals = self.log()?;
        let mut pending = 0;
        for seal in &all_seals {
            if seal.id == boundary {
                break;
            }
            pending += 1;
        }

        Ok(BridgeStatus {
            initialized: true,
            last_import,
            last_export,
            pending_export_count: pending,
        })
    }
}

// ---------------------------------------------------------------------------
// Remote / push / pull
// ---------------------------------------------------------------------------

impl Repository {
    /// Initialize a bare remote directory for push/pull.
    pub fn remote_init(path: &Path) -> WritResult<()> {
        if path.join("objects").exists() && path.join("seals").exists() {
            return Err(WritError::AlreadyExists);
        }
        fs::create_dir_all(path.join("objects"))?;
        fs::create_dir_all(path.join("seals"))?;
        fs::create_dir_all(path.join("specs"))?;
        fs::write(path.join("HEAD"), "")?;
        Ok(())
    }

    /// Add a named remote to this repository's config.
    pub fn remote_add(&self, name: &str, path: &str) -> WritResult<()> {
        let mut config = self.load_config()?;
        if config.remotes.contains_key(name) {
            return Err(WritError::RemoteAlreadyExists(name.to_string()));
        }
        config.remotes.insert(
            name.to_string(),
            crate::remote::RemoteEntry {
                path: path.to_string(),
            },
        );
        self.save_config(&config)
    }

    /// Remove a named remote from this repository's config.
    pub fn remote_remove(&self, name: &str) -> WritResult<()> {
        let mut config = self.load_config()?;
        if config.remotes.remove(name).is_none() {
            return Err(WritError::RemoteNotFound(name.to_string()));
        }
        self.save_config(&config)
    }

    /// List all configured remotes.
    pub fn remote_list(&self) -> WritResult<BTreeMap<String, crate::remote::RemoteEntry>> {
        let config = self.load_config()?;
        Ok(config.remotes)
    }

    /// Push local state to a named remote.
    pub fn push(&self, remote_name: &str) -> WritResult<crate::remote::PushResult> {
        let config = self.load_config()?;
        let entry = config
            .remotes
            .get(remote_name)
            .ok_or_else(|| WritError::RemoteNotFound(remote_name.to_string()))?;
        let remote_path = PathBuf::from(&entry.path);
        self.validate_remote(&remote_path)?;

        let _remote_lock =
            RepoLock::acquire_named(&remote_path, "remote.lock", Duration::from_secs(10))
                .map_err(|_| WritError::RemoteLockTimeout)?;

        let objects_pushed =
            Self::sync_objects(&self.writ_dir.join("objects"), &remote_path.join("objects"))?;
        let seals_pushed =
            Self::sync_seals(&self.writ_dir.join("seals"), &remote_path.join("seals"))?;
        let (specs_pushed, _conflicts) =
            Self::merge_specs(&self.writ_dir.join("specs"), &remote_path.join("specs"))?;

        let local_head = self.read_head()?;
        let remote_head_str = fs::read_to_string(remote_path.join("HEAD"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let remote_head = if remote_head_str.is_empty() {
            None
        } else {
            Some(remote_head_str)
        };

        let head_updated = if let Some(ref local_h) = local_head {
            match &remote_head {
                None => {
                    atomic_write(&remote_path.join("HEAD"), local_h.as_bytes())?;
                    true
                }
                Some(remote_h) if remote_h == local_h => false,
                Some(remote_h) => {
                    if self.is_descendant(local_h, remote_h)? {
                        atomic_write(&remote_path.join("HEAD"), local_h.as_bytes())?;
                        true
                    } else {
                        return Err(WritError::PushDiverged);
                    }
                }
            }
        } else {
            false
        };

        let mut sync_state = self.load_sync_state()?;
        sync_state.last_push_at = Some(chrono::Utc::now());
        sync_state.last_push_seal_id = local_head.clone();
        sync_state.remote_head = local_head;
        self.save_sync_state(&sync_state)?;

        Ok(crate::remote::PushResult {
            remote: remote_name.to_string(),
            objects_pushed,
            seals_pushed,
            specs_pushed,
            head_updated,
        })
    }

    /// Pull remote state into local.
    pub fn pull(&self, remote_name: &str) -> WritResult<crate::remote::PullResult> {
        let config = self.load_config()?;
        let entry = config
            .remotes
            .get(remote_name)
            .ok_or_else(|| WritError::RemoteNotFound(remote_name.to_string()))?;
        let remote_path = PathBuf::from(&entry.path);
        self.validate_remote(&remote_path)?;

        let _remote_lock =
            RepoLock::acquire_named(&remote_path, "remote.lock", Duration::from_secs(10))
                .map_err(|_| WritError::RemoteLockTimeout)?;

        let objects_pulled =
            Self::sync_objects(&remote_path.join("objects"), &self.writ_dir.join("objects"))?;
        let seals_pulled =
            Self::sync_seals(&remote_path.join("seals"), &self.writ_dir.join("seals"))?;
        let (specs_pulled, spec_conflicts) =
            Self::merge_specs(&remote_path.join("specs"), &self.writ_dir.join("specs"))?;

        let local_head = self.read_head()?;
        let remote_head_str = fs::read_to_string(remote_path.join("HEAD"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let remote_head = if remote_head_str.is_empty() {
            None
        } else {
            Some(remote_head_str)
        };

        let head_updated = match (&local_head, &remote_head) {
            (_, None) => false,
            (None, Some(remote_h)) => {
                atomic_write(&self.writ_dir.join("HEAD"), remote_h.as_bytes())?;
                true
            }
            (Some(local_h), Some(remote_h)) if local_h == remote_h => false,
            (Some(local_h), Some(remote_h)) => {
                if self.is_descendant(remote_h, local_h)? {
                    atomic_write(&self.writ_dir.join("HEAD"), remote_h.as_bytes())?;
                    true
                } else if self.is_descendant(local_h, remote_h)? {
                    // Local is ahead — no-op
                    false
                } else {
                    return Err(WritError::PullDiverged);
                }
            }
        };

        let mut sync_state = self.load_sync_state()?;
        sync_state.last_pull_at = Some(chrono::Utc::now());
        sync_state.last_pull_seal_id = remote_head.clone();
        sync_state.remote_head = remote_head;
        self.save_sync_state(&sync_state)?;

        Ok(crate::remote::PullResult {
            remote: remote_name.to_string(),
            objects_pulled,
            seals_pulled,
            specs_pulled,
            head_updated,
            spec_conflicts,
        })
    }

    /// Get sync status with a remote.
    pub fn remote_status(
        &self,
        remote_name: &str,
    ) -> WritResult<crate::remote::RemoteStatus> {
        let config = self.load_config()?;
        let entry = config
            .remotes
            .get(remote_name)
            .ok_or_else(|| WritError::RemoteNotFound(remote_name.to_string()))?;
        let remote_path = PathBuf::from(&entry.path);
        self.validate_remote(&remote_path)?;

        let local_head = self.read_head()?;
        let remote_head_str = fs::read_to_string(remote_path.join("HEAD"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let remote_head = if remote_head_str.is_empty() {
            None
        } else {
            Some(remote_head_str)
        };

        // Count ahead/behind by walking seal chains
        let ahead = match (&local_head, &remote_head) {
            (Some(local_h), Some(remote_h)) if local_h != remote_h => {
                self.count_seals_between(local_h, remote_h).unwrap_or(0)
            }
            (Some(_), None) => {
                // All local seals are ahead
                self.log().map(|s| s.len()).unwrap_or(0)
            }
            _ => 0,
        };
        let behind = match (&local_head, &remote_head) {
            (Some(local_h), Some(remote_h)) if local_h != remote_h => {
                self.count_remote_seals_between(&remote_path, remote_h, local_h)
                    .unwrap_or(0)
            }
            (None, Some(_)) => {
                // Count all remote seals
                Self::count_seals_in_dir(&remote_path.join("seals")).unwrap_or(0)
            }
            _ => 0,
        };

        Ok(crate::remote::RemoteStatus {
            name: remote_name.to_string(),
            path: entry.path.clone(),
            local_head,
            remote_head,
            ahead,
            behind,
        })
    }

    // --- Private helpers for remote ---

    fn validate_remote(&self, path: &Path) -> WritResult<()> {
        if !path.join("objects").is_dir() || !path.join("seals").is_dir() {
            return Err(WritError::InvalidRemote(
                path.display().to_string(),
            ));
        }
        Ok(())
    }

    fn load_config(&self) -> WritResult<crate::remote::RemoteConfig> {
        let path = self.writ_dir.join("config.json");
        if path.exists() {
            let data = fs::read_to_string(&path)?;
            Ok(serde_json::from_str(&data)?)
        } else {
            Ok(crate::remote::RemoteConfig::default())
        }
    }

    fn save_config(&self, config: &crate::remote::RemoteConfig) -> WritResult<()> {
        let path = self.writ_dir.join("config.json");
        let data = serde_json::to_string_pretty(config)?;
        fs::write(path, data)?;
        Ok(())
    }

    fn load_sync_state(&self) -> WritResult<crate::remote::SyncState> {
        let path = self.writ_dir.join("sync.json");
        if path.exists() {
            let data = fs::read_to_string(&path)?;
            Ok(serde_json::from_str(&data)?)
        } else {
            Ok(crate::remote::SyncState::default())
        }
    }

    fn save_sync_state(&self, state: &crate::remote::SyncState) -> WritResult<()> {
        let path = self.writ_dir.join("sync.json");
        let data = serde_json::to_string_pretty(state)?;
        fs::write(path, data)?;
        Ok(())
    }

    /// Copy objects that exist in src but not dst (by 2-char prefix dirs).
    fn sync_objects(src: &Path, dst: &Path) -> WritResult<usize> {
        let mut count = 0;
        if !src.is_dir() {
            return Ok(0);
        }
        for prefix_entry in fs::read_dir(src)? {
            let prefix_entry = prefix_entry?;
            if !prefix_entry.file_type()?.is_dir() {
                continue;
            }
            let prefix_name = prefix_entry.file_name();
            let dst_prefix = dst.join(&prefix_name);
            for obj_entry in fs::read_dir(prefix_entry.path())? {
                let obj_entry = obj_entry?;
                let obj_name = obj_entry.file_name();
                let dst_obj = dst_prefix.join(&obj_name);
                if !dst_obj.exists() {
                    fs::create_dir_all(&dst_prefix)?;
                    fs::copy(obj_entry.path(), &dst_obj)?;
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    /// Copy seals that exist in src but not dst.
    fn sync_seals(src: &Path, dst: &Path) -> WritResult<usize> {
        let mut count = 0;
        if !src.is_dir() {
            return Ok(0);
        }
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let name = entry.file_name();
            let dst_path = dst.join(&name);
            if !dst_path.exists() {
                fs::copy(entry.path(), &dst_path)?;
                count += 1;
            }
        }
        Ok(count)
    }

    /// Merge specs from src into dst. Returns (count, conflicts).
    fn merge_specs(
        src: &Path,
        dst: &Path,
    ) -> WritResult<(usize, Vec<crate::remote::SpecMergeConflict>)> {
        let mut count = 0;
        let conflicts = Vec::new();
        if !src.is_dir() {
            return Ok((0, conflicts));
        }
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let name = entry.file_name();
            let dst_path = dst.join(&name);
            if !dst_path.exists() {
                // New spec — just copy
                fs::copy(entry.path(), &dst_path)?;
                count += 1;
            } else {
                // Both sides have this spec — field-level merge
                let src_data = fs::read_to_string(entry.path())?;
                let dst_data = fs::read_to_string(&dst_path)?;
                let src_spec: crate::spec::Spec = serde_json::from_str(&src_data)?;
                let dst_spec: crate::spec::Spec = serde_json::from_str(&dst_data)?;

                let merged = Self::merge_spec_fields(&src_spec, &dst_spec);
                let merged_json = serde_json::to_string_pretty(&merged)?;
                fs::write(&dst_path, merged_json)?;
                count += 1;
            }
        }
        Ok((count, conflicts))
    }

    /// Merge two versions of the same spec field-by-field.
    fn merge_spec_fields(incoming: &crate::spec::Spec, existing: &crate::spec::Spec) -> crate::spec::Spec {
        use crate::spec::SpecStatus;

        // Title/description: take the one with newer updated_at
        let (title, description) = if incoming.updated_at > existing.updated_at {
            (incoming.title.clone(), incoming.description.clone())
        } else {
            (existing.title.clone(), existing.description.clone())
        };

        // Status: take the most progressed (Blocked always wins)
        let status = if incoming.status == SpecStatus::Blocked
            || existing.status == SpecStatus::Blocked
        {
            SpecStatus::Blocked
        } else {
            let rank = |s: &SpecStatus| match s {
                SpecStatus::Pending => 0,
                SpecStatus::InProgress => 1,
                SpecStatus::Complete => 2,
                SpecStatus::Blocked => 3,
            };
            if rank(&incoming.status) >= rank(&existing.status) {
                incoming.status.clone()
            } else {
                existing.status.clone()
            }
        };

        // List fields: union + dedup
        let mut depends_on: Vec<String> = existing.depends_on.clone();
        for d in &incoming.depends_on {
            if !depends_on.contains(d) {
                depends_on.push(d.clone());
            }
        }
        let mut file_scope: Vec<String> = existing.file_scope.clone();
        for f in &incoming.file_scope {
            if !file_scope.contains(f) {
                file_scope.push(f.clone());
            }
        }
        let mut sealed_by: Vec<String> = existing.sealed_by.clone();
        for s in &incoming.sealed_by {
            if !sealed_by.contains(s) {
                sealed_by.push(s.clone());
            }
        }
        let mut acceptance_criteria: Vec<String> = existing.acceptance_criteria.clone();
        for a in &incoming.acceptance_criteria {
            if !acceptance_criteria.contains(a) {
                acceptance_criteria.push(a.clone());
            }
        }
        let mut design_notes: Vec<String> = existing.design_notes.clone();
        for n in &incoming.design_notes {
            if !design_notes.contains(n) {
                design_notes.push(n.clone());
            }
        }
        let mut tech_stack: Vec<String> = existing.tech_stack.clone();
        for t in &incoming.tech_stack {
            if !tech_stack.contains(t) {
                tech_stack.push(t.clone());
            }
        }

        // Timestamps: earlier created_at, later updated_at
        let created_at = std::cmp::min(incoming.created_at, existing.created_at);
        let updated_at = std::cmp::max(incoming.updated_at, existing.updated_at);

        crate::spec::Spec {
            id: existing.id.clone(),
            title,
            description,
            status,
            depends_on,
            file_scope,
            created_at,
            updated_at,
            sealed_by,
            acceptance_criteria,
            design_notes,
            tech_stack,
        }
    }

    /// Check if `child` seal is a descendant of `ancestor` by walking the chain.
    fn is_descendant(&self, child: &str, ancestor: &str) -> WritResult<bool> {
        if child == ancestor {
            return Ok(true);
        }
        let mut current = child.to_string();
        loop {
            let seal = match self.load_seal(&current) {
                Ok(s) => s,
                Err(_) => return Ok(false),
            };
            match seal.parent {
                Some(ref parent) if parent == ancestor => return Ok(true),
                Some(ref parent) => current = parent.clone(),
                None => return Ok(false),
            }
        }
    }

    /// Count seals between child and ancestor (exclusive on both ends).
    fn count_seals_between(&self, child: &str, ancestor: &str) -> WritResult<usize> {
        if child == ancestor {
            return Ok(0);
        }
        let mut count = 0;
        let mut current = child.to_string();
        loop {
            if current == ancestor {
                return Ok(count);
            }
            let seal = match self.load_seal(&current) {
                Ok(s) => s,
                Err(_) => return Ok(count),
            };
            count += 1;
            match seal.parent {
                Some(ref parent) => current = parent.clone(),
                None => return Ok(count),
            }
        }
    }

    /// Count remote seals between child and ancestor by reading remote seal files.
    fn count_remote_seals_between(
        &self,
        remote_path: &Path,
        child: &str,
        ancestor: &str,
    ) -> WritResult<usize> {
        if child == ancestor {
            return Ok(0);
        }
        let mut count = 0;
        let mut current = child.to_string();
        let seals_dir = remote_path.join("seals");
        loop {
            if current == ancestor {
                return Ok(count);
            }
            let seal_path = seals_dir.join(format!("{current}.json"));
            let data = match fs::read_to_string(&seal_path) {
                Ok(d) => d,
                Err(_) => return Ok(count),
            };
            let seal: Seal = match serde_json::from_str(&data) {
                Ok(s) => s,
                Err(_) => return Ok(count),
            };
            count += 1;
            match seal.parent {
                Some(ref parent) => current = parent.clone(),
                None => return Ok(count),
            }
        }
    }

    /// Count total seals in a directory (simple file count).
    fn count_seals_in_dir(dir: &Path) -> WritResult<usize> {
        if !dir.is_dir() {
            return Ok(0);
        }
        let count = fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .map_or(false, |n| n.ends_with(".json"))
            })
            .count();
        Ok(count)
    }
}

/// Result of `writ install`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallResult {
    pub initialized: bool,
    pub git_detected: bool,
    pub git_imported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imported_seal_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imported_files: Option<usize>,
}

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
                false,
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
            false,
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
            false,
        )
        .unwrap();

        fs::write(dir.path().join("b.txt"), "bbb").unwrap();
        repo.seal(
            test_agent(),
            "second".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
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
            false,
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
            false,
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
            false,
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
                false,
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
                false,
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

        let ctx = repo.context(ContextScope::Full, 10, &ContextFilter::default()).unwrap();
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
            false,
        )
        .unwrap();

        let ctx = repo.context(ContextScope::Full, 10, &ContextFilter::default()).unwrap();
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
            false,
        )
        .unwrap();

        fs::write(dir.path().join("b.txt"), "bbb").unwrap();
        repo.seal(
            test_agent(),
            "unrelated".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Spec("feature-1".to_string()), 10, &ContextFilter::default())
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

        let result = repo.context(ContextScope::Spec("nope".to_string()), 10, &ContextFilter::default());
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
            false,
        )
        .unwrap();

        fs::write(dir.path().join("file.txt"), "changed").unwrap();

        let ctx = repo.context(ContextScope::Full, 10, &ContextFilter::default()).unwrap();
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
                false,
            )
            .unwrap();
        }

        let ctx = repo.context(ContextScope::Full, 3, &ContextFilter::default()).unwrap();
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
                false,
            )
            .unwrap();

        fs::write(dir.path().join("file.txt"), "modified").unwrap();
        repo.seal(
            test_agent(),
            "second".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
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
                false,
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
            false,
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
                false,
            )
            .unwrap();

        fs::write(dir.path().join("b.txt"), "bbb").unwrap();
        repo.seal(
            test_agent(),
            "added b".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
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
                false,
            )
            .unwrap();

        fs::write(dir.path().join("file.txt"), "v2").unwrap();
        repo.seal(
            test_agent(),
            "v2".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
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
                false,
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
                false,
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
                false,
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
                false,
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
            false,
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
                false,
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
                false,
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
            false,
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
                false,
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
                false,
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
            false,
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
            false,
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
            false,
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
                false,
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

    // -------------------------------------------------------------------
    // Verification tests
    // -------------------------------------------------------------------

    #[test]
    fn test_seal_with_verification() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("app.py"), "print('hello')").unwrap();

        let verification = Verification {
            tests_passed: Some(42),
            tests_failed: Some(0),
            linted: true,
        };

        let seal = repo
            .seal(
                test_agent(),
                "tested change".to_string(),
                None,
                TaskStatus::Complete,
                verification,
                false,
            )
            .unwrap();

        assert_eq!(seal.verification.tests_passed, Some(42));
        assert_eq!(seal.verification.tests_failed, Some(0));
        assert!(seal.verification.linted);
    }

    #[test]
    fn test_seal_verification_in_log() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("app.py"), "print('hello')").unwrap();

        let verification = Verification {
            tests_passed: Some(10),
            tests_failed: Some(2),
            linted: false,
        };

        repo.seal(
            test_agent(),
            "with verification".to_string(),
            None,
            TaskStatus::Complete,
            verification,
            false,
        )
        .unwrap();

        let log = repo.log().unwrap();
        assert_eq!(log[0].verification.tests_passed, Some(10));
        assert_eq!(log[0].verification.tests_failed, Some(2));
        assert!(!log[0].verification.linted);
    }

    #[test]
    fn test_seal_verification_default() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("app.py"), "print('hello')").unwrap();

        let seal = repo
            .seal(
                test_agent(),
                "default verification".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
                false,
            )
            .unwrap();

        assert_eq!(seal.verification.tests_passed, None);
        assert_eq!(seal.verification.tests_failed, None);
        assert!(!seal.verification.linted);
    }

    // -------------------------------------------------------------------
    // Lock integration tests
    // -------------------------------------------------------------------

    #[test]
    fn test_seal_holds_lock() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("file.txt"), "content").unwrap();

        // Lock file should be created during seal and released after.
        repo.seal(
            test_agent(),
            "lock test".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Lock should be released — acquiring again should succeed.
        let lock = crate::lock::RepoLock::acquire(
            &dir.path().join(".writ"),
            Duration::from_millis(100),
        );
        assert!(lock.is_ok());
    }

    #[test]
    fn test_concurrent_seal_safety() {
        use std::sync::{Arc, Barrier};

        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Create two files
        fs::write(dir.path().join("a.txt"), "aaa").unwrap();
        fs::write(dir.path().join("b.txt"), "bbb").unwrap();

        // Seal 'a' first so 'b' remains pending for the second seal.
        repo.seal_paths(
            test_agent(),
            "seal a".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            &["a.txt".to_string()],
            false,
        )
        .unwrap();

        // Now create a second file for the concurrent test.
        fs::write(dir.path().join("c.txt"), "ccc").unwrap();

        // Both threads will try to seal simultaneously — locking ensures
        // they succeed sequentially without corruption.
        let root = dir.path().to_path_buf();
        let barrier = Arc::new(Barrier::new(2));

        let b1 = barrier.clone();
        let r1 = root.clone();
        let t1 = std::thread::spawn(move || {
            let repo = Repository::open(&r1).unwrap();
            b1.wait();
            repo.seal_paths(
                test_agent(),
                "thread 1".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
                &["b.txt".to_string()],
                false,
            )
        });

        let b2 = barrier.clone();
        let r2 = root.clone();
        let t2 = std::thread::spawn(move || {
            let repo = Repository::open(&r2).unwrap();
            b2.wait();
            repo.seal_paths(
                test_agent(),
                "thread 2".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
                &["c.txt".to_string()],
                false,
            )
        });

        let res1 = t1.join().unwrap();
        let res2 = t2.join().unwrap();

        // Both should succeed (sequential via locking).
        assert!(res1.is_ok());
        assert!(res2.is_ok());

        // Verify repository integrity — should have 3 seals total.
        let log = repo.log().unwrap();
        assert_eq!(log.len(), 3);
    }

    // -------------------------------------------------------------------
    // Convergence integration tests
    // -------------------------------------------------------------------

    /// Helper: set up a repo with a base seal and two specs ready for convergence testing.
    fn setup_convergence_repo(
        dir: &tempfile::TempDir,
    ) -> (Repository, String, String) {
        let repo = Repository::init(dir.path()).unwrap();

        // Create a base file and seal it.
        fs::write(
            dir.path().join("shared.py"),
            "line1\nline2\nline3\nline4\nline5\n",
        )
        .unwrap();
        repo.seal(
            test_agent(),
            "base state".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Create two specs.
        let spec_a = Spec::new("feat-a".to_string(), "Feature A".to_string(), String::new());
        let spec_b = Spec::new("feat-b".to_string(), "Feature B".to_string(), String::new());
        repo.add_spec(&spec_a).unwrap();
        repo.add_spec(&spec_b).unwrap();

        (repo, "feat-a".to_string(), "feat-b".to_string())
    }

    #[test]
    fn test_converge_disjoint_files() {
        let dir = tempdir().unwrap();
        let (repo, spec_a, spec_b) = setup_convergence_repo(&dir);

        // Spec A modifies a separate file.
        fs::write(dir.path().join("module_a.py"), "feature A code\n").unwrap();
        repo.seal(
            test_agent(),
            "add module a".to_string(),
            Some(spec_a.clone()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Spec B modifies a different separate file.
        fs::write(dir.path().join("module_b.py"), "feature B code\n").unwrap();
        repo.seal(
            test_agent(),
            "add module b".to_string(),
            Some(spec_b.clone()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let report = repo.converge(&spec_a, &spec_b).unwrap();

        assert!(report.is_clean);
        assert!(report.auto_merged.is_empty());
        assert!(report.conflicts.is_empty());
        assert!(report.left_only.contains(&"module_a.py".to_string()));
        assert!(report.right_only.contains(&"module_b.py".to_string()));
    }

    #[test]
    fn test_converge_overlapping_clean() {
        let dir = tempdir().unwrap();
        let (repo, spec_a, spec_b) = setup_convergence_repo(&dir);

        // Spec A changes line 1 of shared.py.
        fs::write(
            dir.path().join("shared.py"),
            "CHANGED_A\nline2\nline3\nline4\nline5\n",
        )
        .unwrap();
        repo.seal(
            test_agent(),
            "change top of shared".to_string(),
            Some(spec_a.clone()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Spec B changes line 5 of shared.py (non-overlapping).
        fs::write(
            dir.path().join("shared.py"),
            "line1\nline2\nline3\nline4\nCHANGED_B\n",
        )
        .unwrap();
        repo.seal(
            test_agent(),
            "change bottom of shared".to_string(),
            Some(spec_b.clone()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let report = repo.converge(&spec_a, &spec_b).unwrap();

        assert!(report.is_clean);
        assert_eq!(report.auto_merged.len(), 1);
        assert_eq!(report.auto_merged[0].path, "shared.py");
        assert!(report.conflicts.is_empty());
        // Both changes should be present in merged content.
        assert!(report.auto_merged[0].content.contains("CHANGED_A"));
        assert!(report.auto_merged[0].content.contains("CHANGED_B"));
    }

    #[test]
    fn test_converge_overlapping_conflict() {
        let dir = tempdir().unwrap();
        let (repo, spec_a, spec_b) = setup_convergence_repo(&dir);

        // Spec A changes line 2 to something.
        fs::write(
            dir.path().join("shared.py"),
            "line1\nFEATURE_A\nline3\nline4\nline5\n",
        )
        .unwrap();
        repo.seal(
            test_agent(),
            "feature a in shared".to_string(),
            Some(spec_a.clone()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Spec B changes the same line differently.
        fs::write(
            dir.path().join("shared.py"),
            "line1\nFEATURE_B\nline3\nline4\nline5\n",
        )
        .unwrap();
        repo.seal(
            test_agent(),
            "feature b in shared".to_string(),
            Some(spec_b.clone()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let report = repo.converge(&spec_a, &spec_b).unwrap();

        assert!(!report.is_clean);
        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(report.conflicts[0].path, "shared.py");
        assert!(!report.conflicts[0].regions.is_empty());
        // Verify conflict has structured data.
        let region = &report.conflicts[0].regions[0];
        assert_eq!(region.left_lines, vec!["FEATURE_A"]);
        assert_eq!(region.right_lines, vec!["FEATURE_B"]);
    }

    #[test]
    fn test_converge_spec_not_found() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let result = repo.converge("nonexistent", "also-missing");
        assert!(result.is_err());
    }

    #[test]
    fn test_converge_no_seals() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Create spec but don't seal anything against it.
        let spec = Spec::new("empty-spec".to_string(), "No Seals".to_string(), String::new());
        repo.add_spec(&spec).unwrap();

        let spec2 = Spec::new("other-spec".to_string(), "Other".to_string(), String::new());
        repo.add_spec(&spec2).unwrap();

        let result = repo.converge("empty-spec", "other-spec");
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("no seals"));
    }

    #[test]
    fn test_apply_convergence_clean() {
        let dir = tempdir().unwrap();
        let (repo, spec_a, spec_b) = setup_convergence_repo(&dir);

        // Spec A adds module_a.py and modifies top of shared.py.
        fs::write(dir.path().join("module_a.py"), "# module A\n").unwrap();
        fs::write(
            dir.path().join("shared.py"),
            "CHANGED_A\nline2\nline3\nline4\nline5\n",
        )
        .unwrap();
        repo.seal(
            test_agent(),
            "spec a work".to_string(),
            Some(spec_a.clone()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Spec B adds module_b.py and modifies bottom of shared.py.
        fs::write(dir.path().join("module_b.py"), "# module B\n").unwrap();
        fs::write(
            dir.path().join("shared.py"),
            "line1\nline2\nline3\nline4\nCHANGED_B\n",
        )
        .unwrap();
        repo.seal(
            test_agent(),
            "spec b work".to_string(),
            Some(spec_b.clone()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let report = repo.converge(&spec_a, &spec_b).unwrap();
        assert!(report.is_clean);

        // Apply convergence (no resolutions needed for clean merge).
        repo.apply_convergence(&report, &[]).unwrap();

        // Verify files on disk.
        let shared = fs::read_to_string(dir.path().join("shared.py")).unwrap();
        assert!(shared.contains("CHANGED_A"));
        assert!(shared.contains("CHANGED_B"));

        let module_a = fs::read_to_string(dir.path().join("module_a.py")).unwrap();
        assert_eq!(module_a, "# module A\n");

        let module_b = fs::read_to_string(dir.path().join("module_b.py")).unwrap();
        assert_eq!(module_b, "# module B\n");
    }

    #[test]
    fn test_apply_convergence_with_resolutions() {
        let dir = tempdir().unwrap();
        let (repo, spec_a, spec_b) = setup_convergence_repo(&dir);

        // Both specs change the same line differently → conflict.
        fs::write(
            dir.path().join("shared.py"),
            "line1\nFEATURE_A\nline3\nline4\nline5\n",
        )
        .unwrap();
        repo.seal(
            test_agent(),
            "a changes shared".to_string(),
            Some(spec_a.clone()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        fs::write(
            dir.path().join("shared.py"),
            "line1\nFEATURE_B\nline3\nline4\nline5\n",
        )
        .unwrap();
        repo.seal(
            test_agent(),
            "b changes shared".to_string(),
            Some(spec_b.clone()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let report = repo.converge(&spec_a, &spec_b).unwrap();
        assert!(!report.is_clean);

        // Provide resolution for the conflict.
        let resolutions = vec![crate::convergence::FileResolution {
            path: "shared.py".to_string(),
            content: "line1\nMERGED_RESULT\nline3\nline4\nline5\n".to_string(),
        }];

        repo.apply_convergence(&report, &resolutions).unwrap();

        // Verify resolved content is written.
        let shared = fs::read_to_string(dir.path().join("shared.py")).unwrap();
        assert!(shared.contains("MERGED_RESULT"));
    }

    #[test]
    fn test_apply_unresolved_conflicts() {
        let dir = tempdir().unwrap();
        let (repo, spec_a, spec_b) = setup_convergence_repo(&dir);

        // Create a conflict.
        fs::write(
            dir.path().join("shared.py"),
            "line1\nFEATURE_A\nline3\nline4\nline5\n",
        )
        .unwrap();
        repo.seal(
            test_agent(),
            "a work".to_string(),
            Some(spec_a.clone()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        fs::write(
            dir.path().join("shared.py"),
            "line1\nFEATURE_B\nline3\nline4\nline5\n",
        )
        .unwrap();
        repo.seal(
            test_agent(),
            "b work".to_string(),
            Some(spec_b.clone()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let report = repo.converge(&spec_a, &spec_b).unwrap();
        assert!(!report.is_clean);

        // Try to apply without providing resolutions → should error.
        let result = repo.apply_convergence(&report, &[]);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("unresolved conflict"));
    }

    // -------------------------------------------------------------------
    // Empty seal tests
    // -------------------------------------------------------------------

    #[test]
    fn test_seal_allow_empty() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Create a file and seal it so we have a non-empty repo.
        fs::write(dir.path().join("file.txt"), "content").unwrap();
        repo.seal(
            test_agent(),
            "initial".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Repo is now clean — seal with allow_empty=true should succeed.
        let seal = repo
            .seal(
                test_agent(),
                "metadata-only update".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
                true,
            )
            .unwrap();

        assert_eq!(seal.changes.len(), 0);
        assert_eq!(seal.summary, "metadata-only update");
        assert!(seal.parent.is_some());
    }

    #[test]
    fn test_seal_allow_empty_with_spec() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new("task-1".to_string(), "Task One".to_string(), String::new());
        repo.add_spec(&spec).unwrap();

        fs::write(dir.path().join("file.txt"), "content").unwrap();
        repo.seal(
            test_agent(),
            "initial work".to_string(),
            Some("task-1".to_string()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Update spec to complete (no file changes).
        repo.update_spec(
            "task-1",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        // Seal the spec completion — the AAIS_1 workflow.
        let seal = repo
            .seal(
                test_agent(),
                "mark task-1 complete".to_string(),
                Some("task-1".to_string()),
                TaskStatus::Complete,
                Verification::default(),
                true,
            )
            .unwrap();

        assert_eq!(seal.changes.len(), 0);
        let loaded_spec = repo.load_spec("task-1").unwrap();
        assert_eq!(loaded_spec.sealed_by.len(), 2);
    }

    #[test]
    fn test_seal_allow_empty_false_still_fails() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let result = repo.seal(
            test_agent(),
            "should fail".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_seal_paths_allow_empty() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("file.txt"), "content").unwrap();
        repo.seal(
            test_agent(),
            "initial".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // seal_paths with no matching paths but allow_empty=true.
        let seal = repo
            .seal_paths(
                test_agent(),
                "empty paths seal".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
                &["nonexistent.txt".to_string()],
                true,
            )
            .unwrap();

        assert_eq!(seal.changes.len(), 0);
    }

    // -------------------------------------------------------------------
    // Enriched context tests
    // -------------------------------------------------------------------

    #[test]
    fn test_context_seal_summary_has_status_and_verification() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("file.txt"), "content").unwrap();
        repo.seal(
            test_agent(),
            "verified work".to_string(),
            None,
            TaskStatus::InProgress,
            Verification {
                tests_passed: Some(10),
                tests_failed: Some(0),
                linted: true,
            },
            false,
        )
        .unwrap();

        let ctx = repo.context(ContextScope::Full, 10, &ContextFilter::default()).unwrap();
        assert_eq!(ctx.recent_seals.len(), 1);
        assert_eq!(ctx.recent_seals[0].status, "in-progress");
        let v = ctx.recent_seals[0].verification.as_ref().unwrap();
        assert_eq!(v.tests_passed, Some(10));
        assert_eq!(v.tests_failed, Some(0));
        assert!(v.linted);
    }

    #[test]
    fn test_context_seal_summary_omits_empty_verification() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("file.txt"), "content").unwrap();
        repo.seal(
            test_agent(),
            "no verification".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo.context(ContextScope::Full, 10, &ContextFilter::default()).unwrap();
        assert!(ctx.recent_seals[0].verification.is_none());
        assert_eq!(ctx.recent_seals[0].status, "complete");
    }

    #[test]
    fn test_context_has_available_operations() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let ctx = repo.context(ContextScope::Full, 10, &ContextFilter::default()).unwrap();
        assert!(!ctx.available_operations.is_empty());
        assert!(ctx.available_operations.iter().any(|op| op.contains("seal")));
        assert!(ctx.available_operations.iter().any(|op| op.contains("restore")));
        assert!(ctx.available_operations.iter().any(|op| op.contains("converge")));
        assert!(ctx.available_operations.iter().any(|op| op.contains("diff_seals")));
    }

    // --- Context filtering tests ---

    #[test]
    fn test_context_filter_by_status() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("a.txt"), "alpha").unwrap();
        repo.seal(
            test_agent(),
            "in progress work".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("b.txt"), "beta").unwrap();
        repo.seal(
            test_agent(),
            "done".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let filter = ContextFilter {
            status: Some(TaskStatus::Complete),
            ..Default::default()
        };
        let ctx = repo.context(ContextScope::Full, 10, &filter).unwrap();
        assert_eq!(ctx.recent_seals.len(), 1);
        assert_eq!(ctx.recent_seals[0].status, "complete");

        let filter = ContextFilter {
            status: Some(TaskStatus::InProgress),
            ..Default::default()
        };
        let ctx = repo.context(ContextScope::Full, 10, &filter).unwrap();
        assert_eq!(ctx.recent_seals.len(), 1);
        assert_eq!(ctx.recent_seals[0].status, "in-progress");
    }

    #[test]
    fn test_context_filter_by_agent() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let agent_a = AgentIdentity {
            id: "agent-alpha".to_string(),
            agent_type: AgentType::Agent,
        };
        let agent_b = AgentIdentity {
            id: "agent-beta".to_string(),
            agent_type: AgentType::Agent,
        };

        fs::write(dir.path().join("a.txt"), "alpha work").unwrap();
        repo.seal(
            agent_a,
            "alpha did this".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("b.txt"), "beta work").unwrap();
        repo.seal(
            agent_b,
            "beta did this".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let filter = ContextFilter {
            agent: Some("agent-alpha".to_string()),
            ..Default::default()
        };
        let ctx = repo.context(ContextScope::Full, 10, &filter).unwrap();
        assert_eq!(ctx.recent_seals.len(), 1);
        assert_eq!(ctx.recent_seals[0].agent, "agent-alpha");
    }

    #[test]
    fn test_context_filter_combined_status_and_agent() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let agent_a = AgentIdentity {
            id: "worker-1".to_string(),
            agent_type: AgentType::Agent,
        };
        let agent_b = AgentIdentity {
            id: "worker-2".to_string(),
            agent_type: AgentType::Agent,
        };

        fs::write(dir.path().join("a.txt"), "w1 progress").unwrap();
        repo.seal(
            agent_a.clone(),
            "w1 wip".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("b.txt"), "w2 done").unwrap();
        repo.seal(
            agent_b,
            "w2 complete".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("c.txt"), "w1 done").unwrap();
        repo.seal(
            agent_a,
            "w1 complete".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let filter = ContextFilter {
            status: Some(TaskStatus::Complete),
            agent: Some("worker-1".to_string()),
        };
        let ctx = repo.context(ContextScope::Full, 10, &filter).unwrap();
        assert_eq!(ctx.recent_seals.len(), 1);
        assert_eq!(ctx.recent_seals[0].summary, "w1 complete");
    }

    #[test]
    fn test_context_filter_empty_returns_all() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("a.txt"), "a").unwrap();
        repo.seal(
            test_agent(),
            "seal-1".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();
        fs::write(dir.path().join("b.txt"), "b").unwrap();
        repo.seal(
            test_agent(),
            "seal-2".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo.context(ContextScope::Full, 10, &ContextFilter::default()).unwrap();
        assert_eq!(ctx.recent_seals.len(), 2);
    }

    // --- Seal nudge tests ---

    #[test]
    fn test_seal_nudge_present_when_dirty() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("tracked.txt"), "initial").unwrap();
        repo.seal(
            test_agent(),
            "initial".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("tracked.txt"), "modified").unwrap();
        fs::write(dir.path().join("new.txt"), "brand new").unwrap();

        let ctx = repo.context(ContextScope::Full, 10, &ContextFilter::default()).unwrap();
        let nudge = ctx.seal_nudge.as_ref().expect("nudge should be present");
        assert_eq!(nudge.unsealed_file_count, 2);
        assert!(nudge.message.contains("2 file(s) changed"));
    }

    #[test]
    fn test_seal_nudge_absent_when_clean() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("file.txt"), "clean").unwrap();
        repo.seal(
            test_agent(),
            "sealed".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo.context(ContextScope::Full, 10, &ContextFilter::default()).unwrap();
        assert!(ctx.seal_nudge.is_none());
    }

    // --- File relevance / changed_paths tests ---

    #[test]
    fn test_seal_summary_includes_changed_paths() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("auth.py"), "pass").unwrap();
        fs::write(dir.path().join("main.py"), "run").unwrap();
        repo.seal(
            test_agent(),
            "initial commit".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo.context(ContextScope::Full, 10, &ContextFilter::default()).unwrap();
        let paths = &ctx.recent_seals[0].changed_paths;
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&"auth.py".to_string()));
        assert!(paths.contains(&"main.py".to_string()));
    }

    #[test]
    fn test_context_filter_with_spec_scope() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new("feat-x".to_string(), "Feature X".to_string(), String::new());
        repo.add_spec(&spec).unwrap();

        let agent_a = AgentIdentity {
            id: "architect".to_string(),
            agent_type: AgentType::Agent,
        };
        let agent_b = AgentIdentity {
            id: "implementer".to_string(),
            agent_type: AgentType::Agent,
        };

        fs::write(dir.path().join("design.md"), "arch").unwrap();
        repo.seal(
            agent_a,
            "architecture".to_string(),
            Some("feat-x".to_string()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("impl.py"), "code").unwrap();
        repo.seal(
            agent_b,
            "implementation".to_string(),
            Some("feat-x".to_string()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let filter = ContextFilter {
            agent: Some("implementer".to_string()),
            ..Default::default()
        };
        let ctx = repo
            .context(ContextScope::Spec("feat-x".to_string()), 10, &filter)
            .unwrap();
        assert_eq!(ctx.recent_seals.len(), 1);
        assert_eq!(ctx.recent_seals[0].agent, "implementer");
    }

    // --- Rich context: spec enrichment tests ---

    #[test]
    fn test_spec_new_has_empty_enrichment_fields() {
        let spec = Spec::new("test".to_string(), "Test".to_string(), String::new());
        assert!(spec.acceptance_criteria.is_empty());
        assert!(spec.design_notes.is_empty());
        assert!(spec.tech_stack.is_empty());
    }

    #[test]
    fn test_spec_backwards_compat_deserialize() {
        let json = r#"{
            "id": "old-spec",
            "title": "Old Spec",
            "description": "",
            "status": "pending",
            "depends_on": [],
            "file_scope": [],
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "sealed_by": []
        }"#;
        let spec: Spec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.id, "old-spec");
        assert!(spec.acceptance_criteria.is_empty());
        assert!(spec.design_notes.is_empty());
        assert!(spec.tech_stack.is_empty());
    }

    #[test]
    fn test_spec_serializes_without_empty_fields() {
        let spec = Spec::new("test".to_string(), "Test".to_string(), String::new());
        let json = serde_json::to_string(&spec).unwrap();
        assert!(!json.contains("acceptance_criteria"));
        assert!(!json.contains("design_notes"));
        assert!(!json.contains("tech_stack"));
    }

    #[test]
    fn test_spec_serializes_with_enrichment() {
        let mut spec = Spec::new("test".to_string(), "Test".to_string(), String::new());
        spec.acceptance_criteria = vec!["All tests pass".to_string()];
        spec.design_notes = vec!["Use async where possible".to_string()];
        spec.tech_stack = vec!["rust".to_string(), "pyo3".to_string()];
        let json = serde_json::to_string(&spec).unwrap();
        assert!(json.contains("acceptance_criteria"));
        assert!(json.contains("design_notes"));
        assert!(json.contains("tech_stack"));
        assert!(json.contains("All tests pass"));
    }

    #[test]
    fn test_update_spec_acceptance_criteria() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let spec = Spec::new("feat".to_string(), "Feature".to_string(), String::new());
        repo.add_spec(&spec).unwrap();

        let updated = repo
            .update_spec(
                "feat",
                SpecUpdate {
                    acceptance_criteria: Some(vec![
                        "Auth flow works".to_string(),
                        "Tests pass".to_string(),
                    ]),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.acceptance_criteria.len(), 2);
        assert_eq!(updated.acceptance_criteria[0], "Auth flow works");
    }

    #[test]
    fn test_update_spec_design_notes() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let spec = Spec::new("feat".to_string(), "Feature".to_string(), String::new());
        repo.add_spec(&spec).unwrap();

        let updated = repo
            .update_spec(
                "feat",
                SpecUpdate {
                    design_notes: Some(vec!["Use JWT for auth".to_string()]),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.design_notes.len(), 1);
        assert_eq!(updated.design_notes[0], "Use JWT for auth");
    }

    #[test]
    fn test_update_spec_tech_stack() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let spec = Spec::new("feat".to_string(), "Feature".to_string(), String::new());
        repo.add_spec(&spec).unwrap();

        let updated = repo
            .update_spec(
                "feat",
                SpecUpdate {
                    tech_stack: Some(vec!["rust".to_string(), "python".to_string()]),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.tech_stack, vec!["rust", "python"]);
    }

    #[test]
    fn test_context_dependency_status() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let dep = Spec::new("dep-1".to_string(), "Dependency".to_string(), String::new());
        repo.add_spec(&dep).unwrap();

        let mut main_spec = Spec::new("main".to_string(), "Main".to_string(), String::new());
        main_spec.depends_on = vec!["dep-1".to_string()];
        repo.add_spec(&main_spec).unwrap();

        fs::write(dir.path().join("file.txt"), "content").unwrap();
        repo.seal(
            test_agent(),
            "initial".to_string(),
            Some("main".to_string()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(
                ContextScope::Spec("main".to_string()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();
        let deps = ctx
            .dependency_status
            .expect("should have dependency_status");
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].spec_id, "dep-1");
        assert_eq!(deps[0].status, "pending");
        assert!(!deps[0].resolved);
    }

    #[test]
    fn test_context_dependency_resolved() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let dep = Spec::new("dep-1".to_string(), "Dependency".to_string(), String::new());
        repo.add_spec(&dep).unwrap();
        repo.update_spec(
            "dep-1",
            SpecUpdate {
                status: Some(crate::spec::SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        let mut main_spec = Spec::new("main".to_string(), "Main".to_string(), String::new());
        main_spec.depends_on = vec!["dep-1".to_string()];
        repo.add_spec(&main_spec).unwrap();

        fs::write(dir.path().join("file.txt"), "content").unwrap();
        repo.seal(
            test_agent(),
            "work".to_string(),
            Some("main".to_string()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(
                ContextScope::Spec("main".to_string()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();
        let deps = ctx.dependency_status.unwrap();
        assert_eq!(deps[0].status, "complete");
        assert!(deps[0].resolved);
    }

    #[test]
    fn test_context_dependency_missing() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let mut spec = Spec::new("main".to_string(), "Main".to_string(), String::new());
        spec.depends_on = vec!["nonexistent".to_string()];
        repo.add_spec(&spec).unwrap();

        fs::write(dir.path().join("file.txt"), "content").unwrap();
        repo.seal(
            test_agent(),
            "work".to_string(),
            Some("main".to_string()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(
                ContextScope::Spec("main".to_string()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();
        let deps = ctx.dependency_status.unwrap();
        assert_eq!(deps[0].spec_id, "nonexistent");
        assert_eq!(deps[0].status, "not-found");
        assert!(!deps[0].resolved);
    }

    #[test]
    fn test_context_spec_progress() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new("feat".to_string(), "Feature".to_string(), String::new());
        repo.add_spec(&spec).unwrap();

        let agent_a = AgentIdentity {
            id: "designer".to_string(),
            agent_type: AgentType::Agent,
        };
        let agent_b = AgentIdentity {
            id: "coder".to_string(),
            agent_type: AgentType::Agent,
        };

        fs::write(dir.path().join("design.md"), "design").unwrap();
        repo.seal(
            agent_a,
            "design done".to_string(),
            Some("feat".to_string()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("impl.py"), "code").unwrap();
        repo.seal(
            agent_b,
            "impl done".to_string(),
            Some("feat".to_string()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        repo.update_spec(
            "feat",
            SpecUpdate {
                status: Some(crate::spec::SpecStatus::InProgress),
                ..Default::default()
            },
        )
        .unwrap();

        let ctx = repo
            .context(
                ContextScope::Spec("feat".to_string()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();
        let progress = ctx.spec_progress.expect("should have spec_progress");
        assert_eq!(progress.total_seals, 2);
        assert_eq!(progress.current_status, "in-progress");
        assert_eq!(progress.agents_involved.len(), 2);
        assert!(progress.agents_involved.contains(&"designer".to_string()));
        assert!(progress.agents_involved.contains(&"coder".to_string()));
        assert!(progress.latest_seal_at.is_some());
    }

    #[test]
    fn test_context_full_no_enrichment() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        assert!(ctx.dependency_status.is_none());
        assert!(ctx.spec_progress.is_none());
    }

    // ─── Security: path traversal ─────────────────────────────
    #[test]
    fn test_validate_path_rejects_parent_traversal() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let err = repo.validate_path("../etc/passwd");
        assert!(err.is_err());
        let msg = format!("{}", err.unwrap_err());
        assert!(msg.contains("traversal"));
    }

    #[test]
    fn test_validate_path_rejects_absolute() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let err = repo.validate_path("/etc/passwd");
        assert!(err.is_err());
        let msg = format!("{}", err.unwrap_err());
        assert!(msg.contains("traversal"));
    }

    #[test]
    fn test_validate_path_accepts_normal() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let result = repo.validate_path("src/main.rs");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), dir.path().join("src/main.rs"));
    }

    #[test]
    fn test_validate_path_rejects_nested_traversal() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        assert!(repo.validate_path("src/../../secret").is_err());
    }

    // ─── Security: agent ID validation ────────────────────────
    #[test]
    fn test_seal_rejects_invalid_agent_id() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("test.txt"), "data").unwrap();
        let bad_agent = AgentIdentity {
            id: "evil agent; rm -rf /".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        let result = repo.seal(
            bad_agent,
            "test".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        );
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("invalid"));
    }

    #[test]
    fn test_seal_accepts_valid_agent_id() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("test.txt"), "data").unwrap();
        let good_agent = AgentIdentity {
            id: "my-agent_v2.0".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        let result = repo.seal(
            good_agent,
            "test".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        );
        assert!(result.is_ok());
    }
}

#[cfg(test)]
mod remote_tests {
    use super::*;
    use crate::seal::{AgentType, TaskStatus, Verification};
    use tempfile::tempdir;

    fn test_agent() -> AgentIdentity {
        AgentIdentity {
            id: "test-agent".to_string(),
            agent_type: AgentType::Agent,
        }
    }

    /// Helper: init a repo, create a file, and seal it.
    fn setup_repo_with_seal(dir: &Path) -> Repository {
        let repo = Repository::init(dir).unwrap();
        fs::write(dir.join("hello.txt"), "hello world").unwrap();
        repo.seal(
            test_agent(),
            "Initial seal".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();
        repo
    }

    #[test]
    fn test_remote_init_creates_structure() {
        let dir = tempdir().unwrap();
        let remote_dir = dir.path().join("remote");
        fs::create_dir(&remote_dir).unwrap();

        Repository::remote_init(&remote_dir).unwrap();

        assert!(remote_dir.join("objects").is_dir());
        assert!(remote_dir.join("seals").is_dir());
        assert!(remote_dir.join("specs").is_dir());
        assert!(remote_dir.join("HEAD").exists());
    }

    #[test]
    fn test_remote_init_twice_fails() {
        let dir = tempdir().unwrap();
        let remote_dir = dir.path().join("remote");
        fs::create_dir(&remote_dir).unwrap();

        Repository::remote_init(&remote_dir).unwrap();
        let result = Repository::remote_init(&remote_dir);
        assert!(matches!(result, Err(WritError::AlreadyExists)));
    }

    #[test]
    fn test_remote_add_and_list() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.remote_add("origin", "/tmp/fake-remote").unwrap();
        let remotes = repo.remote_list().unwrap();

        assert_eq!(remotes.len(), 1);
        assert_eq!(remotes["origin"].path, "/tmp/fake-remote");
    }

    #[test]
    fn test_remote_add_duplicate_fails() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.remote_add("origin", "/tmp/remote1").unwrap();
        let result = repo.remote_add("origin", "/tmp/remote2");
        assert!(matches!(result, Err(WritError::RemoteAlreadyExists(_))));
    }

    #[test]
    fn test_remote_remove() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.remote_add("origin", "/tmp/remote").unwrap();
        repo.remote_remove("origin").unwrap();
        let remotes = repo.remote_list().unwrap();
        assert!(remotes.is_empty());
    }

    #[test]
    fn test_remote_remove_nonexistent() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let result = repo.remote_remove("origin");
        assert!(matches!(result, Err(WritError::RemoteNotFound(_))));
    }

    #[test]
    fn test_push_objects_and_seals() {
        let work = tempdir().unwrap();
        let remote = tempdir().unwrap();
        let remote_dir = remote.path().join("bare");
        fs::create_dir(&remote_dir).unwrap();
        Repository::remote_init(&remote_dir).unwrap();

        let repo = setup_repo_with_seal(work.path());
        repo.remote_add("origin", remote_dir.to_str().unwrap())
            .unwrap();

        let result = repo.push("origin").unwrap();
        assert_eq!(result.remote, "origin");
        assert!(result.objects_pushed > 0);
        assert!(result.seals_pushed > 0);
        assert!(result.head_updated);
    }

    #[test]
    fn test_push_fast_forward() {
        let work = tempdir().unwrap();
        let remote = tempdir().unwrap();
        let remote_dir = remote.path().join("bare");
        fs::create_dir(&remote_dir).unwrap();
        Repository::remote_init(&remote_dir).unwrap();

        let repo = setup_repo_with_seal(work.path());
        repo.remote_add("origin", remote_dir.to_str().unwrap())
            .unwrap();

        // First push
        repo.push("origin").unwrap();

        // Add another seal
        fs::write(work.path().join("second.txt"), "more data").unwrap();
        repo.seal(
            test_agent(),
            "Second seal".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Second push — should fast-forward
        let result = repo.push("origin").unwrap();
        assert!(result.head_updated);
    }

    #[test]
    fn test_push_diverged_fails() {
        let work1 = tempdir().unwrap();
        let work2 = tempdir().unwrap();
        let remote = tempdir().unwrap();
        let remote_dir = remote.path().join("bare");
        fs::create_dir(&remote_dir).unwrap();
        Repository::remote_init(&remote_dir).unwrap();

        // Repo 1: create and push
        let repo1 = setup_repo_with_seal(work1.path());
        repo1
            .remote_add("origin", remote_dir.to_str().unwrap())
            .unwrap();
        repo1.push("origin").unwrap();

        // Repo 2: create independently (different seal chain)
        let repo2 = setup_repo_with_seal(work2.path());
        repo2
            .remote_add("origin", remote_dir.to_str().unwrap())
            .unwrap();

        // Repo 2 push should fail — divergent history
        let result = repo2.push("origin");
        assert!(matches!(result, Err(WritError::PushDiverged)));
    }

    #[test]
    fn test_pull_objects_and_seals() {
        let work1 = tempdir().unwrap();
        let work2 = tempdir().unwrap();
        let remote = tempdir().unwrap();
        let remote_dir = remote.path().join("bare");
        fs::create_dir(&remote_dir).unwrap();
        Repository::remote_init(&remote_dir).unwrap();

        // Repo 1: create and push
        let repo1 = setup_repo_with_seal(work1.path());
        repo1
            .remote_add("origin", remote_dir.to_str().unwrap())
            .unwrap();
        repo1.push("origin").unwrap();

        // Repo 2: empty, pull from remote
        let repo2 = Repository::init(work2.path()).unwrap();
        repo2
            .remote_add("origin", remote_dir.to_str().unwrap())
            .unwrap();

        let result = repo2.pull("origin").unwrap();
        assert!(result.objects_pulled > 0);
        assert!(result.seals_pulled > 0);
        assert!(result.head_updated);
    }

    #[test]
    fn test_push_pull_roundtrip() {
        let work1 = tempdir().unwrap();
        let work2 = tempdir().unwrap();
        let remote = tempdir().unwrap();
        let remote_dir = remote.path().join("bare");
        fs::create_dir(&remote_dir).unwrap();
        Repository::remote_init(&remote_dir).unwrap();

        // Repo 1: create content and push
        let repo1 = setup_repo_with_seal(work1.path());
        repo1
            .remote_add("origin", remote_dir.to_str().unwrap())
            .unwrap();
        repo1.push("origin").unwrap();

        // Repo 2: pull
        let repo2 = Repository::init(work2.path()).unwrap();
        repo2
            .remote_add("origin", remote_dir.to_str().unwrap())
            .unwrap();
        repo2.pull("origin").unwrap();

        // Verify HEAD matches
        let log1 = repo1.log().unwrap();
        let log2 = repo2.log().unwrap();
        assert_eq!(log1.len(), log2.len());
        assert_eq!(log1[0].id, log2[0].id);

        // Verify the object can be retrieved
        let seal = repo2.load_seal(&log2[0].id).unwrap();
        assert_eq!(seal.summary, "Initial seal");
    }

    #[test]
    fn test_spec_merge_union_sealed_by() {
        let work1 = tempdir().unwrap();
        let work2 = tempdir().unwrap();
        let remote = tempdir().unwrap();
        let remote_dir = remote.path().join("bare");
        fs::create_dir(&remote_dir).unwrap();
        Repository::remote_init(&remote_dir).unwrap();

        // Repo 1: create spec and push
        let repo1 = setup_repo_with_seal(work1.path());
        repo1
            .remote_add("origin", remote_dir.to_str().unwrap())
            .unwrap();
        let spec = crate::spec::Spec::new(
            "test-spec".to_string(),
            "Test spec".to_string(),
            "A spec for testing".to_string(),
        );
        repo1.add_spec(&spec).unwrap();
        repo1.push("origin").unwrap();

        // Repo 2: pull, then modify the spec
        let repo2 = Repository::init(work2.path()).unwrap();
        repo2
            .remote_add("origin", remote_dir.to_str().unwrap())
            .unwrap();
        repo2.pull("origin").unwrap();

        // Both repos update the spec's sealed_by independently
        let specs1 = repo1.list_specs().unwrap();
        let spec_id = &specs1[0].id;

        // Repo1: update file_scope (a list field)
        repo1
            .update_spec(
                spec_id,
                SpecUpdate {
                    file_scope: Some(vec!["src/a.rs".to_string()]),
                    ..Default::default()
                },
            )
            .unwrap();

        // Repo2: update file_scope with different value
        repo2
            .update_spec(
                spec_id,
                SpecUpdate {
                    file_scope: Some(vec!["src/b.rs".to_string()]),
                    ..Default::default()
                },
            )
            .unwrap();

        // Push from repo1, then push from repo2 (specs merge)
        repo1.push("origin").unwrap();
        repo2.push("origin").unwrap();

        // Pull into repo1 to get merged result
        repo1.pull("origin").unwrap();
        let updated_specs = repo1.list_specs().unwrap();
        let spec = &updated_specs[0];

        // file_scope should be union of both
        assert!(spec.file_scope.contains(&"src/a.rs".to_string()));
        assert!(spec.file_scope.contains(&"src/b.rs".to_string()));
    }

    #[test]
    fn test_spec_merge_status_progression() {
        // Verify that merge_spec_fields picks the most progressed status
        let now = chrono::Utc::now();
        let spec_pending = crate::spec::Spec {
            id: "spec-1".to_string(),
            title: "Test".to_string(),
            description: "Test spec".to_string(),
            status: SpecStatus::Pending,
            depends_on: vec![],
            file_scope: vec![],
            created_at: now,
            updated_at: now,
            sealed_by: vec![],
            acceptance_criteria: vec![],
            design_notes: vec![],
            tech_stack: vec![],
        };

        let spec_in_progress = crate::spec::Spec {
            status: SpecStatus::InProgress,
            ..spec_pending.clone()
        };

        // InProgress should win over Pending
        let merged = Repository::merge_spec_fields(&spec_in_progress, &spec_pending);
        assert_eq!(merged.status, SpecStatus::InProgress);

        // Blocked should always win
        let spec_blocked = crate::spec::Spec {
            status: SpecStatus::Blocked,
            ..spec_pending.clone()
        };
        let merged2 = Repository::merge_spec_fields(&spec_pending, &spec_blocked);
        assert_eq!(merged2.status, SpecStatus::Blocked);
    }

    #[test]
    fn test_push_idempotent() {
        let work = tempdir().unwrap();
        let remote = tempdir().unwrap();
        let remote_dir = remote.path().join("bare");
        fs::create_dir(&remote_dir).unwrap();
        Repository::remote_init(&remote_dir).unwrap();

        let repo = setup_repo_with_seal(work.path());
        repo.remote_add("origin", remote_dir.to_str().unwrap())
            .unwrap();

        // Push twice — second should succeed with no new data
        let first = repo.push("origin").unwrap();
        let second = repo.push("origin").unwrap();

        assert!(first.objects_pushed > 0);
        assert_eq!(second.objects_pushed, 0);
        assert_eq!(second.seals_pushed, 0);
        assert!(!second.head_updated); // HEAD already matches
    }

    #[test]
    fn test_pull_fast_forward() {
        let work1 = tempdir().unwrap();
        let work2 = tempdir().unwrap();
        let remote = tempdir().unwrap();
        let remote_dir = remote.path().join("bare");
        fs::create_dir(&remote_dir).unwrap();
        Repository::remote_init(&remote_dir).unwrap();

        // Repo 1: create and push initial seal
        let repo1 = setup_repo_with_seal(work1.path());
        repo1
            .remote_add("origin", remote_dir.to_str().unwrap())
            .unwrap();
        repo1.push("origin").unwrap();

        // Repo 2: pull to sync
        let repo2 = Repository::init(work2.path()).unwrap();
        repo2
            .remote_add("origin", remote_dir.to_str().unwrap())
            .unwrap();
        repo2.pull("origin").unwrap();

        // Repo 1: add more work and push
        fs::write(work1.path().join("extra.txt"), "extra").unwrap();
        repo1
            .seal(
                test_agent(),
                "More work".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
                false,
            )
            .unwrap();
        repo1.push("origin").unwrap();

        // Repo 2: pull again — fast-forward
        let result = repo2.pull("origin").unwrap();
        assert!(result.head_updated);
        assert!(result.seals_pulled > 0);
    }

    #[test]
    fn test_remote_status_ahead_behind() {
        let work1 = tempdir().unwrap();
        let work2 = tempdir().unwrap();
        let remote = tempdir().unwrap();
        let remote_dir = remote.path().join("bare");
        fs::create_dir(&remote_dir).unwrap();
        Repository::remote_init(&remote_dir).unwrap();

        // Repo 1: create and push
        let repo1 = setup_repo_with_seal(work1.path());
        repo1
            .remote_add("origin", remote_dir.to_str().unwrap())
            .unwrap();
        repo1.push("origin").unwrap();

        // Repo 2: pull, then add local work (don't push)
        let repo2 = Repository::init(work2.path()).unwrap();
        repo2
            .remote_add("origin", remote_dir.to_str().unwrap())
            .unwrap();
        repo2.pull("origin").unwrap();

        // Repo 1: add more work and push
        fs::write(work1.path().join("extra.txt"), "extra").unwrap();
        repo1
            .seal(
                test_agent(),
                "Repo1 extra".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
                false,
            )
            .unwrap();
        repo1.push("origin").unwrap();

        // Repo 2: status should show behind > 0
        let status = repo2.remote_status("origin").unwrap();
        assert_eq!(status.name, "origin");
        assert!(status.behind > 0, "Expected behind > 0, got {}", status.behind);
    }
}

#[cfg(all(test, feature = "bridge"))]
mod bridge_tests {
    use super::*;
    use tempfile::TempDir;

    /// Set up a git repo with files, then init writ inside it.
    fn setup_git_and_writ() -> (TempDir, Repository) {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Initialize git repo
        let git_repo = git2::Repository::init(root).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();

        // Create some files
        fs::write(root.join("README.md"), "# Hello\n").unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.py"), "print('hello')\n").unwrap();
        fs::write(root.join("src/utils.py"), "def add(a, b):\n    return a + b\n").unwrap();

        // Add all and commit
        let mut index = git_repo.index().unwrap();
        index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = git_repo.find_tree(tree_id).unwrap();
        git_repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[]).unwrap();

        // Initialize writ
        let repo = Repository::init(root).unwrap();
        (tmp, repo)
    }

    #[test]
    fn test_bridge_import_creates_seal() {
        let (_tmp, repo) = setup_git_and_writ();
        let agent = AgentIdentity {
            id: "bridge".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };

        let result = repo.bridge_import(None, agent).unwrap();
        assert!(!result.seal_id.is_empty());
        assert!(!result.git_commit.is_empty());
        assert_eq!(result.git_ref, "HEAD");
        assert_eq!(result.files_imported, 3); // README.md, src/main.py, src/utils.py

        // Verify HEAD was updated
        let head = repo.read_head().unwrap();
        assert_eq!(head, Some(result.seal_id));
    }

    #[test]
    fn test_bridge_import_stores_all_files() {
        let (_tmp, repo) = setup_git_and_writ();
        let agent = AgentIdentity {
            id: "bridge".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };

        let result = repo.bridge_import(None, agent).unwrap();

        // Load the seal and verify tree contains all files
        let seal = repo.load_seal(&result.seal_id).unwrap();
        let index = repo.load_tree_index(&seal.tree).unwrap();
        assert!(index.entries.contains_key("README.md"));
        assert!(index.entries.contains_key("src/main.py"));
        assert!(index.entries.contains_key("src/utils.py"));

        // Verify content round-trips
        let readme_hash = &index.entries["README.md"].hash;
        let content = repo.objects.retrieve(readme_hash).unwrap();
        assert_eq!(String::from_utf8_lossy(&content), "# Hello\n");
    }

    #[test]
    fn test_bridge_import_no_git_repo() {
        let tmp = TempDir::new().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();
        let agent = AgentIdentity {
            id: "bridge".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };

        let err = repo.bridge_import(None, agent).unwrap_err();
        assert!(matches!(err, WritError::NoGitRepo));
    }

    #[test]
    fn test_bridge_import_with_ref() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let git_repo = git2::Repository::init(root).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();

        // First commit (1 file)
        fs::write(root.join("file1.txt"), "v1").unwrap();
        let mut index = git_repo.index().unwrap();
        index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = git_repo.find_tree(tree_id).unwrap();
        let commit1_oid = git_repo.commit(Some("HEAD"), &sig, &sig, "first", &tree, &[]).unwrap();

        // Second commit (2 files)
        fs::write(root.join("file2.txt"), "v2").unwrap();
        let mut index = git_repo.index().unwrap();
        index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = git_repo.find_tree(tree_id).unwrap();
        let commit1_obj = git_repo.find_commit(commit1_oid).unwrap();
        git_repo.commit(Some("HEAD"), &sig, &sig, "second", &tree, &[&commit1_obj]).unwrap();

        // Import from the first commit by OID (only 1 file)
        let repo = Repository::init(root).unwrap();
        let agent = AgentIdentity {
            id: "bridge".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        let result = repo.bridge_import(Some(&commit1_oid.to_string()), agent).unwrap();
        assert_eq!(result.files_imported, 1);
        assert_eq!(result.git_commit, commit1_oid.to_string());
    }

    #[test]
    fn test_bridge_export_creates_commits() {
        let (tmp, repo) = setup_git_and_writ();
        let agent = AgentIdentity {
            id: "bridge".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };

        // Import baseline
        repo.bridge_import(None, agent).unwrap();

        // Create a new file and seal
        fs::write(tmp.path().join("new_file.txt"), "new content").unwrap();
        let agent2 = AgentIdentity {
            id: "implementer".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            agent2,
            "added new file".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        ).unwrap();

        // Export
        let result = repo.bridge_export(Some("writ/export")).unwrap();
        assert_eq!(result.seals_exported, 1);
        assert_eq!(result.branch, "writ/export");
        assert_eq!(result.exported[0].summary, "added new file");

        // Verify the git branch exists
        let git_repo = git2::Repository::discover(tmp.path()).unwrap();
        let branch = git_repo.find_branch("writ/export", git2::BranchType::Local).unwrap();
        assert!(branch.is_head() == false);
    }

    #[test]
    fn test_bridge_export_maps_metadata() {
        let (tmp, repo) = setup_git_and_writ();
        let agent = AgentIdentity {
            id: "bridge".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.bridge_import(None, agent).unwrap();

        // Seal with verification and spec
        fs::write(tmp.path().join("tested.py"), "# tested").unwrap();
        repo.add_spec(&Spec::new("auth".to_string(), "Auth feature".to_string(), String::new())).unwrap();
        let agent2 = AgentIdentity {
            id: "tester".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            agent2,
            "auth tests passing".to_string(),
            Some("auth".to_string()),
            TaskStatus::Complete,
            Verification { tests_passed: Some(42), tests_failed: Some(0), linted: true },
            false,
        ).unwrap();

        let result = repo.bridge_export(Some("writ/export")).unwrap();
        assert_eq!(result.seals_exported, 1);

        // Verify commit message has trailers
        let git_repo = git2::Repository::discover(tmp.path()).unwrap();
        let oid = git2::Oid::from_str(&result.exported[0].git_commit).unwrap();
        let commit = git_repo.find_commit(oid).unwrap();
        let msg = commit.message().unwrap();
        assert!(msg.contains("Writ-Seal-Id:"));
        assert!(msg.contains("Writ-Spec: auth"));
        assert!(msg.contains("Writ-Status: complete"));
        assert!(msg.contains("Writ-Tests-Passed: 42"));
        assert!(msg.contains("Writ-Linted: true"));
    }

    #[test]
    fn test_bridge_export_no_import() {
        let (_tmp, repo) = setup_git_and_writ();

        let err = repo.bridge_export(None).unwrap_err();
        assert!(matches!(err, WritError::BridgeError(_)));
    }

    #[test]
    fn test_bridge_export_nothing_pending() {
        let (_tmp, repo) = setup_git_and_writ();
        let agent = AgentIdentity {
            id: "bridge".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.bridge_import(None, agent).unwrap();

        // Export immediately — no new seals
        let result = repo.bridge_export(None).unwrap();
        assert_eq!(result.seals_exported, 0);
    }

    #[test]
    fn test_bridge_export_incremental() {
        let (tmp, repo) = setup_git_and_writ();
        let agent = AgentIdentity {
            id: "bridge".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.bridge_import(None, agent).unwrap();

        // First seal + export
        fs::write(tmp.path().join("a.txt"), "a").unwrap();
        let a1 = AgentIdentity { id: "agent-1".to_string(), agent_type: crate::seal::AgentType::Agent };
        repo.seal(a1, "first change".to_string(), None, TaskStatus::InProgress, Verification::default(), false).unwrap();
        let result1 = repo.bridge_export(None).unwrap();
        assert_eq!(result1.seals_exported, 1);

        // Second seal + export (should only export the new one)
        fs::write(tmp.path().join("b.txt"), "b").unwrap();
        let a2 = AgentIdentity { id: "agent-1".to_string(), agent_type: crate::seal::AgentType::Agent };
        repo.seal(a2, "second change".to_string(), None, TaskStatus::Complete, Verification::default(), false).unwrap();
        let result2 = repo.bridge_export(None).unwrap();
        assert_eq!(result2.seals_exported, 1);
        assert_eq!(result2.exported[0].summary, "second change");
    }

    #[test]
    fn test_bridge_status_no_state() {
        let (_tmp, repo) = setup_git_and_writ();

        let status = repo.bridge_status().unwrap();
        assert!(!status.initialized);
        assert_eq!(status.pending_export_count, 0);
    }

    #[test]
    fn test_bridge_status_after_import() {
        let (_tmp, repo) = setup_git_and_writ();
        let agent = AgentIdentity {
            id: "bridge".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.bridge_import(None, agent).unwrap();

        let status = repo.bridge_status().unwrap();
        assert!(status.initialized);
        assert!(status.last_import.is_some());
        assert!(status.last_export.is_none());
        assert_eq!(status.pending_export_count, 0);
    }

    #[test]
    fn test_bridge_status_pending_count() {
        let (tmp, repo) = setup_git_and_writ();
        let agent = AgentIdentity {
            id: "bridge".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.bridge_import(None, agent).unwrap();

        // Create 3 seals
        for i in 1..=3 {
            fs::write(tmp.path().join(format!("file{i}.txt")), format!("content {i}")).unwrap();
            let a = AgentIdentity { id: "worker".to_string(), agent_type: crate::seal::AgentType::Agent };
            repo.seal(a, format!("change {i}"), None, TaskStatus::InProgress, Verification::default(), false).unwrap();
        }

        let status = repo.bridge_status().unwrap();
        assert_eq!(status.pending_export_count, 3);
    }

    #[test]
    fn test_bridge_roundtrip() {
        let (tmp, repo) = setup_git_and_writ();
        let root = tmp.path();
        let agent = AgentIdentity {
            id: "bridge".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };

        // Import git baseline
        let import_result = repo.bridge_import(None, agent).unwrap();
        assert_eq!(import_result.files_imported, 3);

        // Agent does work in writ
        fs::write(root.join("src/new_module.py"), "class Auth:\n    pass\n").unwrap();
        let a1 = AgentIdentity { id: "implementer".to_string(), agent_type: crate::seal::AgentType::Agent };
        repo.seal(a1, "added auth module".to_string(), None, TaskStatus::InProgress, Verification::default(), false).unwrap();

        // Modify existing file
        fs::write(root.join("src/main.py"), "from auth import Auth\nprint('hello')\n").unwrap();
        let a2 = AgentIdentity { id: "implementer".to_string(), agent_type: crate::seal::AgentType::Agent };
        repo.seal(a2, "integrated auth".to_string(), None, TaskStatus::Complete, Verification { tests_passed: Some(5), tests_failed: Some(0), linted: false }, false).unwrap();

        // Export back to git
        let export_result = repo.bridge_export(Some("writ/output")).unwrap();
        assert_eq!(export_result.seals_exported, 2);
        assert_eq!(export_result.branch, "writ/output");

        // Verify git branch has the correct file tree
        let git_repo = git2::Repository::discover(root).unwrap();
        let branch = git_repo.find_branch("writ/output", git2::BranchType::Local).unwrap();
        let commit = branch.get().peel_to_commit().unwrap();
        let tree = commit.tree().unwrap();

        // Check new file exists in git tree
        assert!(tree.get_path(std::path::Path::new("src/new_module.py")).is_ok());
        // Check modified file content
        let entry = tree.get_path(std::path::Path::new("src/main.py")).unwrap();
        let blob = entry.to_object(&git_repo).unwrap();
        let content = blob.as_blob().unwrap().content();
        assert_eq!(String::from_utf8_lossy(content), "from auth import Auth\nprint('hello')\n");
    }

    // ─── Security: bridge input validation ────────────────────
    #[test]
    fn test_bridge_rejects_invalid_branch_name() {
        assert!(Repository::validate_branch_name("").is_err());
        assert!(Repository::validate_branch_name("a..b").is_err());
        assert!(Repository::validate_branch_name("main.lock").is_err());
        assert!(Repository::validate_branch_name("has space").is_err());
        assert!(Repository::validate_branch_name("ctrl\x01char").is_err());
        assert!(Repository::validate_branch_name(&"x".repeat(300)).is_err());
    }

    #[test]
    fn test_bridge_accepts_valid_branch_names() {
        assert!(Repository::validate_branch_name("main").is_ok());
        assert!(Repository::validate_branch_name("writ/export").is_ok());
        assert!(Repository::validate_branch_name("feature/my-thing").is_ok());
        assert!(Repository::validate_branch_name("v2.0-beta").is_ok());
    }

    #[test]
    fn test_bridge_rejects_invalid_git_ref() {
        assert!(Repository::validate_git_ref("").is_err());
        assert!(Repository::validate_git_ref(&"x".repeat(600)).is_err());
    }
}
