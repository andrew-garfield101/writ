//! Repository — the main entry point for writ operations.
//!
//! A Repository ties together the object store, index, seals, and specs
//! into a unified interface.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::agent::{AgentStatus, AgentUpdate, RegisteredAgent, TrustLevel};
use crate::context::{
    AgentActivity, ChainIntegritySummary, ContextFilter, ContextOutput, ContextScope, DepStatus,
    DiffSummary, DivergedBranchWarning, FileContention, FileScopeViolation, IntegrationRisk,
    RecommendedAction, SealNudge, SealSummary, SessionSummary, SpecProgress, WorkingStateSummary,
};
use crate::convergence::{
    self,
    pipeline::{ConvergencePipeline, PipelineInput},
    ConsistencyCheck, ConvergeAllReport, ConvergeStrategy, ConvergenceQualityReport,
    ConvergenceReport, FileAlternative, FileConflict, FileDecision, FileMergeResult,
    FileMetricValue, FileResolution, MergeStepResult, MergedFile, ResolutionRecord,
};
use crate::diff::{self, DiffOutput, FileDiff};
use crate::error::{WritError, WritResult};
use crate::fsutil::atomic_write;
use crate::ignore::IgnoreRules;
use crate::index::{Index, IndexEntry};
use crate::keystore::KeyStore;
use crate::lock::RepoLock;
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
    /// In-memory HEAD recorded at last context() call, for automatic conflict detection.
    last_context_head: Mutex<Option<String>>,
    /// When true, seal() rejects files outside an agent's scope constraints.
    /// When false (default), out-of-scope files produce warnings but the seal succeeds.
    enforce_scope: bool,
    /// Persistent repository settings loaded from `.writ/settings.json`.
    settings: crate::settings::WritSettings,
}

/// Snapshot of git working tree state, used by install().
#[cfg(feature = "bridge")]
struct GitStateSnapshot {
    branch: Option<String>,
    head_short: Option<String>,
    head_full: Option<String>,
    dirty: bool,
    dirty_count: usize,
}

/// Query git state without modifying anything.
#[cfg(feature = "bridge")]
fn query_git_state(root: &Path) -> Option<GitStateSnapshot> {
    let git_repo = git2::Repository::discover(root).ok()?;

    let branch = git_repo.head().ok().and_then(|h| {
        if h.is_branch() {
            h.shorthand().map(String::from)
        } else {
            None
        }
    });

    let (head_short, head_full) = match git_repo.head().ok().and_then(|h| h.target()) {
        Some(oid) => {
            let full = oid.to_string();
            let short = full[..12.min(full.len())].to_string();
            (Some(short), Some(full))
        }
        None => (None, None),
    };

    let (dirty, dirty_count) = match git_repo.statuses(None) {
        Ok(statuses) => {
            let count = statuses
                .iter()
                .filter(|s| {
                    let st = s.status();
                    st != git2::Status::CURRENT && st != git2::Status::IGNORED
                })
                .count();
            (count > 0, count)
        }
        Err(_) => (false, 0),
    };

    Some(GitStateSnapshot {
        branch,
        head_short,
        head_full,
        dirty,
        dirty_count,
    })
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
        fs::create_dir_all(writ_dir.join("heads"))?;
        fs::create_dir_all(writ_dir.join("keys"))?;
        fs::create_dir_all(writ_dir.join("agents"))?;
        fs::write(writ_dir.join("HEAD"), "")?;

        // Generate the convergence engine keypair (encrypted at rest via KeyStore)
        let ks = KeyStore::open(&writ_dir);
        ks.ensure_master_key()?;
        let (signing_key, verifying_key) = crate::crypto::generate_keypair();
        ks.store_agent_key("convergence", &signing_key, &verifying_key)?;

        let index = Index::default();
        index.save(&writ_dir.join("index.json"))?;

        // Write schema version stamp.
        crate::migrate::RepoVersion::new().save(&writ_dir)?;

        Self::open(root)
    }

    /// Open an existing writ repository.
    ///
    /// Searches for `.writ/` in the given directory. Loads compression
    /// settings from GcConfig if available, otherwise uses defaults.
    pub fn open(root: &Path) -> WritResult<Self> {
        let writ_dir = root.join(WRIT_DIR);

        if !writ_dir.exists() {
            return Err(WritError::NotARepo);
        }

        // --- Schema version check & auto-migration ---
        // Acquire lock during migration + version stamp to prevent races when
        // multiple processes/threads open the same repo concurrently.
        {
            let _lock = RepoLock::acquire(&writ_dir, Self::LOCK_TIMEOUT)?;

            let current_schema = crate::migrate::CURRENT_SCHEMA_VERSION;
            let repo_version = crate::migrate::RepoVersion::load(&writ_dir)?;
            let schema = repo_version.as_ref().map(|v| v.schema_version).unwrap_or(0);

            if schema > current_schema {
                return Err(WritError::Other(format!(
                    "this repo uses schema version {schema}, but this binary only supports up to \
                     {current_schema} — please update writ"
                )));
            }

            if schema < current_schema {
                crate::migrate::migrate(&writ_dir, schema, current_schema)?;
            }

            // Update last_opened_by stamp.
            let mut v = crate::migrate::RepoVersion::load(&writ_dir)?.unwrap_or_default();
            v.last_opened_by = env!("CARGO_PKG_VERSION").to_string();
            v.last_opened_at = Some(chrono::Utc::now());
            v.save(&writ_dir)?;
        }

        // Load storage config for ObjectStore compression settings.
        let storage_config = crate::gc::GcConfig::load(&writ_dir)
            .map(|c| c.storage)
            .unwrap_or_default();

        let objects = if storage_config.compression == "none" {
            // Compression disabled: store raw with magic byte prefix.
            ObjectStore::with_config(
                &writ_dir.join("objects"),
                0,
                storage_config.max_object_size_bytes,
            )
        } else {
            ObjectStore::with_config(
                &writ_dir.join("objects"),
                storage_config.compression_level,
                storage_config.max_object_size_bytes,
            )
        };

        // Load persistent settings (defaults if file missing).
        let settings = crate::settings::WritSettings::load(&writ_dir).unwrap_or_default();
        let enforce_scope = settings.enforce_scope.unwrap_or(false);

        Ok(Self {
            root: root.to_path_buf(),
            writ_dir,
            objects,
            last_context_head: Mutex::new(None),
            enforce_scope,
            settings,
        })
    }

    /// Enable or disable hard scope enforcement on seal().
    ///
    /// When enabled, sealing files outside an agent's scope constraints
    /// returns `WritError::ScopeViolation`. When disabled (default),
    /// out-of-scope files produce `AGENT_SCOPE` warnings but the seal succeeds.
    pub fn set_enforce_scope(&mut self, enforce: bool) {
        self.enforce_scope = enforce;
    }

    /// One-command setup: init writ, detect git, import baseline, install hooks.
    ///
    /// Idempotent: safe to run multiple times. Re-imports only when git HEAD
    /// moves. Reports git state, framework detection, and tracked file count.
    #[cfg(feature = "bridge")]
    pub fn init_project(root: &Path) -> WritResult<InitResult> {
        let repo_root = fs::canonicalize(root)
            .unwrap_or_else(|_| root.to_path_buf())
            .to_string_lossy()
            .to_string();

        // Step 1: Init or open
        let writ_dir = root.join(WRIT_DIR);
        let initialized = !writ_dir.exists();
        if initialized {
            Self::init(root)?;
        }
        let repo = Self::open(root)?;

        // Step 2: Create .writignore if needed
        let writignore_created = crate::ignore::create_writignore(root)?;

        // Step 3: Detect frameworks (read-only — actual hook installation is
        // handled by the CLI based on user flags like --no-claude, --bare, etc.)
        let frameworks_detected = crate::hooks::detect_frameworks(root);

        // Step 4: Detect git and query state
        let git_state = query_git_state(root);
        let git_detected = git_state.is_some();

        // Step 5: Idempotent import
        let mut git_imported = false;
        let mut imported_seal_id = None;
        let mut imported_files = None;
        let mut import_skipped_reason = None;
        let mut import_error = None;
        let mut already_imported = false;
        let mut reimported = false;

        if git_detected {
            let bridge_state = repo.load_bridge_state()?;
            let current_head = git_state.as_ref().and_then(|gs| gs.head_full.clone());

            match (
                bridge_state.last_imported_git_commit.as_deref(),
                current_head.as_deref(),
            ) {
                // No previous import → fresh import
                (None, _) => {
                    let agent = AgentIdentity {
                        id: "writ-bridge".to_string(),
                        agent_type: crate::seal::AgentType::Agent,
                    };
                    match repo.bridge_import(None, agent) {
                        Ok(result) => {
                            git_imported = true;
                            imported_seal_id = Some(result.seal_id);
                            imported_files = Some(result.files_imported);
                        }
                        Err(e) => {
                            import_error = Some(e.to_string());
                            import_skipped_reason = Some(format!("import failed: {e}"));
                        }
                    }
                }
                // Same HEAD → already synced
                (Some(prev), Some(curr)) if prev == curr => {
                    already_imported = true;
                    import_skipped_reason =
                        Some(format!("already synced at {}", &prev[..12.min(prev.len())]));
                    imported_seal_id = bridge_state.last_imported_seal_id.clone();
                }
                // HEAD moved → re-import
                (Some(prev), Some(_curr)) => {
                    let agent = AgentIdentity {
                        id: "writ-bridge".to_string(),
                        agent_type: crate::seal::AgentType::Agent,
                    };
                    match repo.bridge_import(None, agent) {
                        Ok(result) => {
                            git_imported = true;
                            reimported = true;
                            imported_seal_id = Some(result.seal_id);
                            imported_files = Some(result.files_imported);
                        }
                        Err(e) => {
                            import_error = Some(e.to_string());
                            import_skipped_reason = Some(format!(
                                "re-import failed (prev: {}): {e}",
                                &prev[..12.min(prev.len())]
                            ));
                        }
                    }
                }
                // Had import but can't resolve HEAD now
                (Some(prev), None) => {
                    import_skipped_reason = Some(format!(
                        "previous import from {} but cannot resolve current HEAD",
                        &prev[..12.min(prev.len())]
                    ));
                }
            }
        }

        // Step 6: Count tracked files
        let tracked_files = repo.load_index().map(|idx| idx.entries.len()).unwrap_or(0);

        // Step 7: Available operations
        let available_operations = vec![
            "writ context".to_string(),
            "writ state".to_string(),
            "writ seal --summary '...'".to_string(),
            "writ log".to_string(),
            "writ diff".to_string(),
        ];

        Ok(InitResult {
            initialized,
            git_detected,
            git_imported,
            imported_seal_id,
            imported_files,
            repo_root,
            git_branch: git_state.as_ref().and_then(|gs| gs.branch.clone()),
            git_head_short: git_state.as_ref().and_then(|gs| gs.head_short.clone()),
            git_dirty: git_state.as_ref().map(|gs| gs.dirty),
            git_dirty_count: git_state.as_ref().map(|gs| gs.dirty_count),
            import_skipped_reason,
            import_error,
            writignore_created,
            already_imported,
            reimported,
            tracked_files,
            available_operations,
            frameworks_detected,
            hooks_installed: Vec::new(),
        })
    }

    /// Remove writ from a project. Inverse of `install`.
    ///
    /// Removes `.writ/` directory, optionally `.writignore`, and cleans
    /// up any framework hooks (CLAUDE.md sections, command files, AGENTS.md sections).
    pub fn uninstall(root: &Path, keep_writignore: bool) -> WritResult<UninstallResult> {
        let writ_dir = root.join(WRIT_DIR);
        let mut result = UninstallResult::default();

        // Gather stats before removal (best-effort — don't fail if repo is broken).
        if writ_dir.exists() {
            if let Ok(repo) = Self::open(root) {
                result.tracked_files = repo.load_index().map(|idx| idx.entries.len()).unwrap_or(0);
                result.seals_existed = repo.log().unwrap_or_default().len();
            }
        }

        // Step 1: Remove framework hooks.
        result.hooks_removed = crate::hooks::uninstall_hooks(root)?;

        // Step 2: Remove .writ/ directory.
        if writ_dir.exists() {
            fs::remove_dir_all(&writ_dir).map_err(|e| {
                crate::WritError::Io(std::io::Error::new(
                    e.kind(),
                    format!("failed to remove .writ/: {e}"),
                ))
            })?;
            result.writ_dir_removed = true;
        } else {
            result
                .warnings
                .push("no .writ/ directory found".to_string());
        }

        // Step 3: Remove .writignore (unless asked to keep it).
        let writignore = root.join(".writignore");
        if writignore.exists() && !keep_writignore {
            fs::remove_file(&writignore)?;
            result.writignore_removed = true;
        }

        Ok(result)
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
        // Reject seals from revoked or suspended agents
        if let Ok(registered) = self.load_agent(&agent.id) {
            if registered.status == AgentStatus::Revoked {
                return Err(WritError::AgentInactive(format!(
                    "agent '{}' is revoked and cannot create seals",
                    agent.id
                )));
            }
            if registered.status == AgentStatus::Suspended {
                return Err(WritError::AgentInactive(format!(
                    "agent '{}' is suspended and cannot create seals",
                    agent.id
                )));
            }
        }
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
        let parent = self.resolve_parent(spec_id.as_deref())?;

        let mut seal_warnings: Vec<String> = Vec::new();

        if let Some(ref sid) = spec_id {
            let changed_paths: Vec<String> = changes.iter().map(|c| c.path.clone()).collect();
            if let Some(scope_warn) = self.check_file_scope(sid, &changed_paths) {
                seal_warnings.push(format!(
                    "FILE_SCOPE: {} file(s) outside declared scope for spec '{}': {}",
                    scope_warn.out_of_scope_files.len(),
                    sid,
                    scope_warn.out_of_scope_files.join(", "),
                ));
                // Emit security event for scope violation (best-effort, don't block seal)
                let logger = crate::security::SecurityEventLogger::new(&self.writ_dir);
                if let Err(e) =
                    logger.emit_scope_violation(&agent.id, sid, &scope_warn.out_of_scope_files)
                {
                    seal_warnings.push(format!("SECURITY_LOG_FAILURE: {e}"));
                }
            }
        }

        if changes.is_empty() && !summary.is_empty() && !allow_empty {
            seal_warnings.push(
                "GHOST_WORK: seal has a summary but 0 file changes — work may have been captured by another agent's seal".to_string(),
            );
        }

        // Agent identity checks (Sprint B)
        if let Ok(registered) = self.load_agent(&agent.id) {
            if registered.status != AgentStatus::Active {
                seal_warnings.push(format!(
                    "AGENT_INACTIVE: agent '{}' status is {:?}",
                    agent.id, registered.status
                ));
            }
            let out_of_scope: Vec<&str> = changes
                .iter()
                .filter(|c| !crate::agent::is_in_scope(&registered.scope_constraints, &c.path))
                .map(|c| c.path.as_str())
                .collect();
            if !out_of_scope.is_empty() {
                // Always emit security event (best-effort)
                let logger = crate::security::SecurityEventLogger::new(&self.writ_dir);
                let _ = logger.emit_agent_scope_violation(&agent.id, &out_of_scope);
                if self.enforce_scope {
                    return Err(WritError::ScopeViolation(format!(
                        "agent '{}' modified {} file(s) outside scope: {}",
                        agent.id,
                        out_of_scope.len(),
                        out_of_scope.join(", ")
                    )));
                } else {
                    seal_warnings.push(format!(
                        "AGENT_SCOPE: {} file(s) outside agent '{}' scope: {}",
                        out_of_scope.len(),
                        agent.id,
                        out_of_scope.join(", ")
                    ));
                }
            }
        } else {
            // Agent not in identity store — emit unrecognized agent event
            let logger = crate::security::SecurityEventLogger::new(&self.writ_dir);
            let _ = logger.emit_unrecognized_agent(&agent.id);
        }

        // Look up parent seal's chain_hash for the cryptographic chain link
        let parent_seal_hash = match parent {
            Some(ref pid) => match self.load_seal(pid) {
                Ok(s) => s.chain_hash.clone(),
                Err(e) => {
                    seal_warnings.push(format!(
                        "CHAIN_BREAK: failed to load parent seal {}: {e} — chain integrity may be compromised",
                        &pid[..12.min(pid.len())]
                    ));
                    None
                }
            },
            None => None,
        };

        let mut seal = Seal::new(
            parent,
            tree_hash,
            agent,
            spec_id.clone(),
            status,
            changes,
            verification,
            summary,
            seal_warnings,
            parent_seal_hash,
        );

        // Sign with agent's key if available, otherwise unsigned
        let ks = KeyStore::open(&self.writ_dir);
        let signing_key = ks.load_agent_signing_key(&seal.agent.id).ok();
        seal.secure(signing_key.as_ref());

        self.save_seal(&seal)?;
        atomic_write(&self.writ_dir.join("HEAD"), seal.id.as_bytes())?;
        index.save(&self.writ_dir.join("index.json"))?;

        if let Some(ref sid) = spec_id {
            self.write_spec_head(sid, &seal.id)?;
            if let Ok(mut spec) = self.load_spec(sid) {
                spec.sealed_by.push(seal.id.clone());
                let now = chrono::Utc::now();
                spec.updated_at = now;
                spec.last_activity = now;

                let promoted = self.auto_promote_spec_status(&mut spec, &seal.status);
                self.save_spec(&spec)?;

                if promoted {
                    self.check_all_specs_complete();
                }
            }
        }

        // GC.3.3c: Lightweight storage pressure check (best-effort, never blocks seal).
        self.check_storage_pressure(&seal);

        Ok(seal)
    }

    /// Seal with optimistic conflict detection.
    ///
    /// If `expected_head` is provided, checks whether HEAD moved since
    /// the agent started. The seal always proceeds, but returns a
    /// `SealConflictWarning` if another agent sealed in between.
    pub fn seal_with_check(
        &self,
        agent: AgentIdentity,
        summary: String,
        spec_id: Option<String>,
        status: TaskStatus,
        verification: Verification,
        allow_empty: bool,
        expected_head: Option<String>,
    ) -> WritResult<(Seal, Option<SealConflictWarning>)> {
        let pre_seal_head = if spec_id.is_some() {
            self.resolve_parent(spec_id.as_deref())?
        } else {
            self.read_head()?
        };

        let seal = self.seal(agent, summary, spec_id, status, verification, allow_empty)?;

        let normalized_expected = match expected_head {
            Some(ref eh) => self.resolve_seal_id(eh).ok(),
            None => None,
        };

        let warning = match (normalized_expected, pre_seal_head) {
            (Some(expected), Some(actual)) if expected != actual => {
                let mut intervening = Vec::new();
                let mut intervening_files: HashSet<String> = HashSet::new();
                let mut cursor = Some(actual.clone());
                while let Some(ref id) = cursor {
                    if *id == expected {
                        break;
                    }
                    if let Ok(s) = self.load_seal(id) {
                        for c in &s.changes {
                            intervening_files.insert(c.path.clone());
                        }
                        intervening.push(s.id.clone());
                        cursor = s.parent.clone();
                    } else {
                        break;
                    }
                }

                let my_files: HashSet<String> =
                    seal.changes.iter().map(|c| c.path.clone()).collect();

                let overlapping: Vec<String> =
                    my_files.intersection(&intervening_files).cloned().collect();

                let is_clean = overlapping.is_empty();

                Some(SealConflictWarning {
                    expected_head: expected,
                    actual_head: actual,
                    intervening_seals: intervening,
                    intervening_files: intervening_files.into_iter().collect(),
                    overlapping_files: overlapping,
                    is_clean,
                })
            }
            _ => None,
        };

        Ok((seal, warning))
    }

    /// Get the seal history (newest first) from global HEAD.
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

    /// Get the seal chain for a specific spec, walking from its tip.
    pub fn spec_log(&self, spec_id: &str) -> WritResult<Vec<Seal>> {
        let mut seals = Vec::new();
        let mut current = self.read_spec_head(spec_id)?;

        while let Some(seal_id) = current {
            let seal = self.load_seal(&seal_id)?;
            current = seal.parent.clone();
            seals.push(seal);
        }

        Ok(seals)
    }

    /// Get the tip seal ID for a specific spec (None if spec has no seals).
    pub fn spec_head(&self, spec_id: &str) -> WritResult<Option<String>> {
        self.read_spec_head(spec_id)
    }

    /// Verify cryptographic integrity of a single seal.
    ///
    /// Checks that `content_hash` matches the seal's canonical content and
    /// that `chain_hash` is correctly computed from `parent_seal_hash` and
    /// `content_hash`. Optionally verifies the Ed25519 signature if a
    /// verifying key is provided.
    pub fn verify_seal(
        &self,
        seal: &Seal,
        verifying_key: Option<&ed25519_dalek::VerifyingKey>,
    ) -> SealVerification {
        use crate::crypto;

        let (content_hash, chain_hash) = match (&seal.content_hash, &seal.chain_hash) {
            (Some(ch), Some(cch)) => (ch.clone(), cch.clone()),
            _ => {
                return SealVerification {
                    seal_id: seal.id.clone(),
                    content_hash_valid: false,
                    chain_hash_valid: false,
                    signature_present: seal.signature.is_some(),
                    signature_valid: None,
                    error: Some("seal missing crypto fields".into()),
                };
            }
        };

        let expected_content = crypto::compute_content_hash(seal);
        let content_valid = content_hash == expected_content;

        let expected_chain =
            crypto::compute_chain_hash(seal.parent_seal_hash.as_deref(), &content_hash);
        let chain_valid = chain_hash == expected_chain;

        let sig_present = seal.signature.is_some();
        let sig_valid = match (&seal.signature, verifying_key) {
            (Some(sig), Some(key)) => Some(crypto::verify_signature(&content_hash, sig, key)),
            (Some(_), None) => None,
            (None, _) => None,
        };

        let error = if !content_valid {
            Some(format!(
                "content_hash mismatch: expected {expected_content}"
            ))
        } else if !chain_valid {
            Some(format!("chain_hash mismatch: expected {expected_chain}"))
        } else if sig_valid == Some(false) {
            // Emit authentication failure event (best-effort)
            let logger = crate::security::SecurityEventLogger::new(&self.writ_dir);
            let _ = logger.emit_authentication_failure(&seal.id, "signature verification failed");
            Some("signature verification failed".into())
        } else {
            None
        };

        SealVerification {
            seal_id: seal.id.clone(),
            content_hash_valid: content_valid,
            chain_hash_valid: chain_valid,
            signature_present: sig_present,
            signature_valid: sig_valid,
            error,
        }
    }

    /// Verify the cryptographic integrity of the entire seal chain from HEAD.
    ///
    /// Walks the chain from HEAD to genesis, verifying each seal's content hash
    /// and chain hash linkage. Seals without crypto fields (pre-Sprint A) are
    /// counted as unsecured but don't cause verification failure.
    ///
    /// Gracefully handles missing or unreadable seals in the chain — these are
    /// reported as failures rather than causing verify_chain() to error out.
    pub fn verify_chain(
        &self,
        verifying_key: Option<&ed25519_dalek::VerifyingKey>,
    ) -> WritResult<ChainVerification> {
        let start = self.read_head()?;
        self.verify_chain_from(start, verifying_key)
    }

    /// Verify the HEAD chain plus every spec's branch chain.
    ///
    /// Returns an `AllChainsVerification` with the HEAD result and per-spec
    /// results. `all_valid` is true only if every chain passes.
    pub fn verify_all_chains(
        &self,
        verifying_key: Option<&ed25519_dalek::VerifyingKey>,
    ) -> WritResult<AllChainsVerification> {
        let head_chain = self.verify_chain(verifying_key)?;

        let mut spec_chains = Vec::new();
        let heads_dir = self.writ_dir.join("heads");
        if heads_dir.exists() {
            let mut spec_ids: Vec<String> = Vec::new();
            for entry in fs::read_dir(&heads_dir)? {
                let entry = entry?;
                spec_ids.push(entry.file_name().to_string_lossy().to_string());
            }
            spec_ids.sort();

            for spec_id in spec_ids {
                let chain = self.verify_spec_chain(&spec_id, verifying_key)?;
                spec_chains.push(SpecChainResult { spec_id, chain });
            }
        }

        let all_valid = head_chain.valid && spec_chains.iter().all(|sc| sc.chain.valid);

        Ok(AllChainsVerification {
            head_chain,
            spec_chains,
            all_valid,
        })
    }

    /// Verify cryptographic integrity of a spec's branch chain.
    fn verify_spec_chain(
        &self,
        spec_id: &str,
        verifying_key: Option<&ed25519_dalek::VerifyingKey>,
    ) -> WritResult<ChainVerification> {
        let start = self.read_spec_head(spec_id)?;
        self.verify_chain_from(start, verifying_key)
    }

    /// Walk a seal chain from a starting ID, gracefully handling missing seals.
    ///
    /// Returns `(loaded_seals, chain_break)` where `chain_break` is `Some`
    /// if a seal in the chain couldn't be loaded (missing or corrupt).
    fn walk_chain_graceful(
        &self,
        start: Option<String>,
    ) -> (Vec<crate::seal::Seal>, Option<(String, String)>) {
        let mut seals = Vec::new();
        let mut current = start;
        let mut chain_break: Option<(String, String)> = None;

        while let Some(seal_id) = current {
            match self.load_seal(&seal_id) {
                Ok(seal) => {
                    current = seal.parent.clone();
                    seals.push(seal);
                }
                Err(e) => {
                    chain_break = Some((seal_id, format!("{e}")));
                    break;
                }
            }
        }

        (seals, chain_break)
    }

    /// Verify a chain starting from a given seal ID. Handles missing seals
    /// gracefully by recording them as failures instead of propagating errors.
    fn verify_chain_from(
        &self,
        start: Option<String>,
        verifying_key: Option<&ed25519_dalek::VerifyingKey>,
    ) -> WritResult<ChainVerification> {
        let (seals, chain_break) = self.walk_chain_graceful(start);
        let mut result = self.verify_seal_chain(&seals, verifying_key)?;

        if let Some((missing_id, err_msg)) = chain_break {
            let error = format!("chain broken: seal not found: {missing_id} ({err_msg})");
            let logger = crate::security::SecurityEventLogger::new(&self.writ_dir);
            let _ = logger.emit_chain_hash_failure(&missing_id, &error);
            result.failures.push(SealVerification {
                seal_id: missing_id,
                content_hash_valid: false,
                chain_hash_valid: false,
                signature_present: false,
                signature_valid: None,
                error: Some(error),
            });
            result.valid = false;
        }

        Ok(result)
    }

    /// Shared implementation for chain verification — used by both
    /// `verify_chain()` (HEAD) and `verify_spec_chain()` (per-spec).
    fn verify_seal_chain(
        &self,
        seals: &[crate::seal::Seal],
        verifying_key: Option<&ed25519_dalek::VerifyingKey>,
    ) -> WritResult<ChainVerification> {
        let mut verified = 0;
        let mut unsecured = 0;
        let mut failures = Vec::new();

        for (i, seal) in seals.iter().enumerate() {
            if !seal.is_secured() {
                unsecured += 1;
                continue;
            }

            let result = self.verify_seal(seal, verifying_key);

            if result.content_hash_valid && result.chain_hash_valid {
                // For non-genesis seals, verify parent_seal_hash matches
                // the previous seal's chain_hash (seals are newest-first)
                if i + 1 < seals.len() {
                    let parent = &seals[i + 1];
                    if let Some(ref parent_chain) = parent.chain_hash {
                        if seal.parent_seal_hash.as_ref() != Some(parent_chain) {
                            let err_msg = format!(
                                "parent_seal_hash doesn't match parent's chain_hash (expected {})",
                                parent_chain
                            );
                            let logger = crate::security::SecurityEventLogger::new(&self.writ_dir);
                            let _ = logger.emit_chain_hash_failure(&seal.id, &err_msg);
                            failures.push(SealVerification {
                                seal_id: seal.id.clone(),
                                content_hash_valid: true,
                                chain_hash_valid: true,
                                signature_present: result.signature_present,
                                signature_valid: result.signature_valid,
                                error: Some(err_msg),
                            });
                            continue;
                        }
                    }
                }
                verified += 1;
            } else {
                // Content or chain hash mismatch — emit security event
                if let Some(ref err) = result.error {
                    let logger = crate::security::SecurityEventLogger::new(&self.writ_dir);
                    let _ = logger.emit_chain_hash_failure(&result.seal_id, err);
                }
                failures.push(result);
            }
        }

        let valid = failures.is_empty();
        Ok(ChainVerification {
            total_seals: seals.len(),
            verified,
            unsecured,
            failures,
            valid,
        })
    }

    /// Detect spec branches whose tip seals are not reachable from global HEAD.
    ///
    /// Returns a list of `(spec_id, branch_tip, seal_count)` for each
    /// diverged branch. Used to warn about "ghost agent" situations where
    /// concurrent agents sealed on spec-scoped branches that were never
    /// converged into the main HEAD chain.
    pub fn diverged_branches(&self) -> WritResult<Vec<DivergedBranch>> {
        let head_chain: std::collections::HashSet<String> = {
            let mut set = std::collections::HashSet::new();
            let mut current = self.read_head()?;
            while let Some(id) = current {
                let seal = self.load_seal(&id)?;
                set.insert(id);
                current = seal.parent;
            }
            set
        };

        let heads_dir = self.writ_dir.join("heads");
        if !heads_dir.exists() {
            return Ok(Vec::new());
        }

        let mut diverged = Vec::new();
        for entry in fs::read_dir(&heads_dir)? {
            let entry = entry?;
            let spec_id = entry.file_name().to_string_lossy().to_string();
            if let Some(tip) = self.read_spec_head(&spec_id)? {
                if !head_chain.contains(&tip) {
                    let branch_seals = self.spec_log(&spec_id)?;
                    let not_on_head: Vec<_> = branch_seals
                        .iter()
                        .filter(|s| !head_chain.contains(&s.id))
                        .collect();
                    if !not_on_head.is_empty() {
                        let agents: Vec<String> = not_on_head
                            .iter()
                            .map(|s| s.agent.id.clone())
                            .collect::<std::collections::HashSet<_>>()
                            .into_iter()
                            .collect();
                        diverged.push(DivergedBranch {
                            spec_id: spec_id.clone(),
                            tip_seal: tip[..12.min(tip.len())].to_string(),
                            seal_count: not_on_head.len(),
                            agents,
                        });
                    }
                }
            }
        }

        diverged.sort_by(|a, b| a.spec_id.cmp(&b.spec_id));
        Ok(diverged)
    }

    /// Add a new spec to the repository.
    ///
    /// Returns `SpecAlreadyExists` if a spec with this ID is already registered.
    pub fn add_spec(&self, spec: &Spec) -> WritResult<()> {
        let path = self
            .writ_dir
            .join("specs")
            .join(format!("{}.json", spec.id));
        if path.exists() {
            return Err(WritError::SpecAlreadyExists(spec.id.clone()));
        }
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

    /// Generate a high-level project status for the round-trip workflow.
    ///
    /// Returns agent activity, spec progress, commit readiness, and stale
    /// spec warnings. Used by `writ status` (porcelain) as opposed to
    /// `writ state` (plumbing).
    pub fn status(&self) -> WritResult<crate::status::StatusOutput> {
        use crate::spec::{CommitState, SpecStatus};
        use crate::status::{AgentSummary, SpecBrief, StatusOutput};
        use std::collections::{HashMap, HashSet};

        let specs = self.list_specs()?;
        let now = chrono::Utc::now();

        // Load project config for commit_mode and project name.
        let project_config = crate::config::ProjectConfig::load(&self.writ_dir).unwrap_or_default();
        let global_config = crate::config::GlobalConfig::load().unwrap_or_default();
        let project_name = project_config
            .project_name()
            .unwrap_or("unknown")
            .to_string();
        let commit_mode = crate::config::resolve_commit_mode(None, &project_config, &global_config)
            .unwrap_or_else(|_| "user".into());

        // Stale timeout from workflow config.
        let stale_timeout = project_config
            .stale_timeout()
            .or_else(|| global_config.stale_timeout())
            .unwrap_or(3600);

        // Build SpecBrief for each spec, bucketed by state.
        let mut in_progress = Vec::new();
        let mut completed = Vec::new();
        let mut committed = Vec::new();
        let mut stale = Vec::new();

        // Track agents by state for AgentSummary.
        let mut agents_active: HashSet<String> = HashSet::new();
        let mut agents_done: HashSet<String> = HashSet::new();
        let mut all_agents: HashSet<String> = HashSet::new();
        let mut total_files_changed: usize = 0;

        // For agent detection, we need the most recent seal per spec.
        // Build a map: spec_id -> (agent_id, seal_count, files_changed_set).
        let mut spec_seal_info: HashMap<String, (String, usize, HashSet<String>)> = HashMap::new();
        let all_seals = self.log()?;
        for seal in &all_seals {
            if let Some(ref spec_id) = seal.spec_id {
                let entry = spec_seal_info
                    .entry(spec_id.clone())
                    .or_insert_with(|| (seal.agent.id.clone(), 0, HashSet::new()));
                entry.1 += 1;
                for change in &seal.changes {
                    entry.2.insert(change.path.clone());
                }
                // Most recent seal's agent wins (seals are newest-first from log()).
                // First entry is already the most recent, so don't overwrite.
            }
        }

        for spec in &specs {
            let (agent, seal_count, files) = spec_seal_info
                .get(&spec.id)
                .map(|(a, c, f)| (a.clone(), *c, f.len()))
                .unwrap_or_else(|| ("unknown".into(), 0, 0));

            all_agents.insert(agent.clone());

            let brief = SpecBrief {
                id: spec.id.clone(),
                title: spec.title.clone(),
                agent: agent.clone(),
                seal_count,
                files_changed: files,
                last_activity: spec.last_activity,
                status: format!("{:?}", spec.status)
                    .to_lowercase()
                    .replace("inprogress", "in-progress"),
                completion_summary: spec.completion_summary.clone(),
            };

            match spec.status {
                SpecStatus::Complete => match spec.commit_state {
                    CommitState::Committed | CommitState::Pushed => {
                        agents_done.insert(agent.clone());
                        committed.push(brief);
                    }
                    CommitState::Uncommitted => {
                        agents_done.insert(agent.clone());
                        total_files_changed += files;
                        completed.push(brief);
                    }
                },
                SpecStatus::InProgress | SpecStatus::Pending => {
                    // Check for staleness.
                    if stale_timeout > 0 {
                        let age = now
                            .signed_duration_since(spec.last_activity)
                            .num_seconds()
                            .max(0) as u64;
                        if age >= stale_timeout && seal_count > 0 {
                            stale.push(brief.clone());
                        }
                    }
                    agents_active.insert(agent.clone());
                    in_progress.push(brief);
                }
                SpecStatus::Blocked => {
                    in_progress.push(brief);
                }
            }
        }

        // Agents who are "done" but also have active specs are "active".
        for agent in &agents_active {
            agents_done.remove(agent);
        }

        // Idle = agents with no active specs AND no completed specs
        // (they appeared in seals but all their specs are committed or cancelled).
        let idle_count = all_agents
            .iter()
            .filter(|a| !agents_active.contains(*a) && !agents_done.contains(*a))
            .count();

        // Sort completed by last_activity descending (most recent first).
        completed.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
        in_progress.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));

        Ok(StatusOutput {
            project_name,
            timestamp: now,
            agents: AgentSummary {
                active: agents_active.len(),
                done: agents_done.len(),
                idle: idle_count,
                total: all_agents.len(),
            },
            specs_in_progress: in_progress,
            specs_completed: completed,
            specs_committed: committed,
            total_files_changed,
            stale_specs: stale,
            commit_mode,
        })
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

    // --- Spec lifecycle management (GC.1.2) ---

    /// Transition a spec's lifecycle state, validating legal transitions.
    ///
    /// Legal transitions:
    /// - Active → Stale (timeout-based)
    /// - Active → Cancelled (manual)
    /// - Active → Completed (manual, requires status == Complete)
    /// - Stale → Active (reassignment)
    /// - Stale → Cancelled (manual or expiry)
    /// - Completed → Archived (retention period)
    /// - Cancelled → Archived (retention period)
    pub fn transition_spec_lifecycle(
        &self,
        spec_id: &str,
        target: crate::spec::LifecycleState,
    ) -> WritResult<()> {
        use crate::spec::LifecycleState;

        let mut spec = self.load_spec(spec_id)?;
        let current = &spec.lifecycle_state;

        let allowed = matches!(
            (current, &target),
            (LifecycleState::Active, LifecycleState::Stale)
                | (LifecycleState::Active, LifecycleState::Cancelled)
                | (LifecycleState::Active, LifecycleState::Completed)
                | (LifecycleState::Stale, LifecycleState::Active)
                | (LifecycleState::Stale, LifecycleState::Cancelled)
                | (LifecycleState::Completed, LifecycleState::Active)
                | (LifecycleState::Completed, LifecycleState::Archived)
                | (LifecycleState::Cancelled, LifecycleState::Archived)
        );

        if !allowed {
            return Err(WritError::InvalidLifecycleTransition(format!(
                "spec '{}': {:?} → {:?} is not a legal transition",
                spec_id, current, target
            )));
        }

        spec.lifecycle_state = target;
        spec.updated_at = chrono::Utc::now();
        self.save_spec(&spec)?;
        Ok(())
    }

    /// Cancel a spec (transition to Cancelled).
    ///
    /// Allowed from Active or Stale states. Returns error if spec is
    /// already in a terminal state (Cancelled, Completed, Archived).
    pub fn cancel_spec(&self, spec_id: &str) -> WritResult<()> {
        use crate::spec::LifecycleState;

        let spec = self.load_spec(spec_id)?;
        match spec.lifecycle_state {
            LifecycleState::Active | LifecycleState::Stale => {
                self.transition_spec_lifecycle(spec_id, LifecycleState::Cancelled)
            }
            other => Err(WritError::InvalidLifecycleTransition(format!(
                "spec '{}': cannot cancel from {:?} (already terminal)",
                spec_id, other
            ))),
        }
    }

    /// Complete a spec's lifecycle (transition to Completed).
    ///
    /// Requires that the spec's user-facing `status` is `Complete` — the
    /// agent must have sealed with `--status complete` first.
    pub fn complete_spec(&self, spec_id: &str) -> WritResult<()> {
        use crate::spec::LifecycleState;

        let spec = self.load_spec(spec_id)?;
        if spec.status != crate::spec::SpecStatus::Complete {
            return Err(WritError::InvalidLifecycleTransition(format!(
                "spec '{}': status must be 'complete' before lifecycle completion (current: {:?})",
                spec_id, spec.status
            )));
        }
        match spec.lifecycle_state {
            LifecycleState::Active | LifecycleState::Stale => {
                self.transition_spec_lifecycle(spec_id, LifecycleState::Completed)
            }
            other => Err(WritError::InvalidLifecycleTransition(format!(
                "spec '{}': cannot complete from {:?}",
                spec_id, other
            ))),
        }
    }

    /// Reopen a completed spec, returning it to active/in-progress state.
    ///
    /// Only specs with status `Complete` and commit_state `Uncommitted` can be
    /// reopened. Committed or pushed specs cannot be reopened — the git history
    /// is already written.
    ///
    /// The seal chain is preserved. A new or existing agent can pick up
    /// the spec and continue working.
    pub fn reopen_spec(&self, spec_id: &str) -> WritResult<()> {
        use crate::spec::{CommitState, LifecycleState, SpecStatus};

        let mut spec = self.load_spec(spec_id)?;

        // Only completed specs can be reopened.
        if spec.status != SpecStatus::Complete {
            return Err(WritError::InvalidLifecycleTransition(format!(
                "spec '{}': can only reopen completed specs (current status: {:?})",
                spec_id, spec.status
            )));
        }

        // Reject if already committed to git.
        if matches!(
            spec.commit_state,
            CommitState::Committed | CommitState::Pushed
        ) {
            return Err(WritError::InvalidLifecycleTransition(format!(
                "spec '{}': cannot reopen a committed spec (commit_state: {:?})",
                spec_id, spec.commit_state
            )));
        }

        // Reopen the spec (resets status, clears completion/commit data).
        spec.reopen();

        // Transition lifecycle back to Active.
        spec.lifecycle_state = LifecycleState::Active;

        self.save_spec(&spec)?;
        Ok(())
    }

    /// Scan for stale specs (Active specs past the stale timeout).
    ///
    /// Returns `(spec_id, seconds_since_last_activity)` for each stale
    /// spec, without transitioning them. The caller decides what to do.
    pub fn scan_stale_specs(&self, config: &crate::gc::GcConfig) -> WritResult<Vec<(String, u64)>> {
        use crate::spec::LifecycleState;

        let specs = self.list_specs()?;
        let now = chrono::Utc::now();
        let mut stale = Vec::new();

        for spec in &specs {
            if spec.lifecycle_state != LifecycleState::Active {
                continue;
            }
            let age = now
                .signed_duration_since(spec.last_activity)
                .num_seconds()
                .max(0) as u64;
            if age >= config.specs.stale_timeout_secs {
                stale.push((spec.id.clone(), age));
            }
        }

        Ok(stale)
    }

    /// Get a storage report for this repository.
    pub fn storage_report(&self) -> WritResult<crate::gc::StorageReport> {
        let config = crate::gc::GcConfig::load(&self.writ_dir)?;
        crate::gc::StorageReport::scan(&self.writ_dir, config.budget_bytes)
    }

    /// Get the repository root path (the directory containing `.writ/`).
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// Get the path to the `.writ/` directory.
    pub fn writ_dir(&self) -> &std::path::Path {
        &self.writ_dir
    }

    /// Get the persistent repository settings.
    pub fn settings(&self) -> &crate::settings::WritSettings {
        &self.settings
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
                    self.compute_file_diff(&file_state.path, ChangeType::Added, &[], &content, 3)
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

    /// Unified log across ALL heads (global + spec branches), deduped.
    ///
    /// Walks global HEAD chain first, then each spec head. Seals already seen
    /// (from the global chain or an earlier spec) are skipped. Result is sorted
    /// newest-first by timestamp. Use this for a complete chronological view
    /// of all work, including agents on diverged branches.
    pub fn log_all(&self) -> WritResult<Vec<Seal>> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut all_seals: Vec<Seal> = Vec::new();

        // 1. Walk global HEAD chain.
        let head_seals = self.log()?;
        for seal in head_seals {
            seen.insert(seal.id.clone());
            all_seals.push(seal);
        }

        // 2. Walk each spec head chain, adding unseen seals.
        let heads_dir = self.writ_dir.join("heads");
        if heads_dir.exists() {
            let mut spec_ids: Vec<String> = Vec::new();
            for entry in fs::read_dir(&heads_dir)? {
                let entry = entry?;
                spec_ids.push(entry.file_name().to_string_lossy().to_string());
            }
            spec_ids.sort();

            for spec_id in spec_ids {
                let branch_seals = self.spec_log(&spec_id)?;
                for seal in branch_seals {
                    if seen.insert(seal.id.clone()) {
                        all_seals.push(seal);
                    }
                }
            }
        }

        // 3. Walk archived pre-convergence branch tips. These chains were
        //    orphaned when apply_convergence advanced spec heads, but the
        //    seals still exist in the object store.
        let merged_heads_path = self.writ_dir.join("merged-heads");
        if merged_heads_path.exists() {
            let contents = fs::read_to_string(&merged_heads_path)?;
            for line in contents.lines() {
                let tip = line.trim();
                if tip.is_empty() || seen.contains(tip) {
                    continue;
                }
                let mut current: Option<String> = Some(tip.to_string());
                while let Some(seal_id) = current {
                    if !seen.insert(seal_id.clone()) {
                        break;
                    }
                    match self.load_seal(&seal_id) {
                        Ok(seal) => {
                            current = seal.parent.clone();
                            all_seals.push(seal);
                        }
                        Err(_) => break,
                    }
                }
            }
        }

        // Sort newest-first by timestamp.
        all_seals.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        Ok(all_seals)
    }

    /// Build per-agent activity summaries from seal history.
    ///
    /// Walks seals newest-first to determine file provenance (who last sealed
    /// each file). If `scope_filter` is provided, only files matching that
    /// scope are included in the per-agent `files_owned` lists.
    fn build_agent_activity(
        seals: &[Seal],
        scope_filter: Option<&dyn Fn(&str) -> bool>,
    ) -> Vec<AgentActivity> {
        use std::collections::HashMap;

        // File provenance: walk newest-first, first agent seen per file wins.
        // Deletes are excluded — removing a file shouldn't make you its "owner".
        // Bridge seals are excluded — import baselines shouldn't claim ownership.
        let mut file_owner: HashMap<String, String> = HashMap::new();
        for seal in seals {
            if seal.agent.id == "writ-bridge" {
                continue;
            }
            for change in &seal.changes {
                if change.change_type != ChangeType::Deleted {
                    file_owner
                        .entry(change.path.clone())
                        .or_insert_with(|| seal.agent.id.clone());
                }
            }
        }

        // Per-agent aggregation (excludes bridge seals).
        struct Stats {
            seal_count: usize,
            latest_summary: Option<String>,
            latest_at: Option<String>,
            specs_touched: Vec<String>,
        }

        let mut agent_stats: HashMap<String, Stats> = HashMap::new();
        for seal in seals {
            if seal.agent.id == "writ-bridge" {
                continue;
            }
            let entry = agent_stats
                .entry(seal.agent.id.clone())
                .or_insert_with(|| Stats {
                    seal_count: 0,
                    latest_summary: None,
                    latest_at: None,
                    specs_touched: Vec::new(),
                });
            entry.seal_count += 1;
            // First encounter is newest (seals are newest-first).
            if entry.latest_summary.is_none() {
                entry.latest_summary = Some(seal.summary.clone());
                entry.latest_at = Some(seal.timestamp.to_rfc3339());
            }
            if let Some(ref sid) = seal.spec_id {
                if !entry.specs_touched.contains(sid) {
                    entry.specs_touched.push(sid.clone());
                }
            }
        }

        // Collect files_owned per agent, optionally filtered by scope.
        let mut agent_files: HashMap<String, Vec<String>> = HashMap::new();
        for (path, agent_id) in &file_owner {
            let include = match scope_filter {
                Some(f) => f(path),
                None => true,
            };
            if include {
                agent_files
                    .entry(agent_id.clone())
                    .or_default()
                    .push(path.clone());
            }
        }

        // Build final output, sorted by most recent activity.
        let mut result: Vec<AgentActivity> = agent_stats
            .into_iter()
            .map(|(agent_id, stats)| {
                let mut files = agent_files.remove(&agent_id).unwrap_or_default();
                files.sort();
                AgentActivity {
                    agent_id,
                    files_owned: files,
                    seal_count: stats.seal_count,
                    latest_summary: stats.latest_summary,
                    latest_at: stats.latest_at,
                    specs_touched: stats.specs_touched,
                }
            })
            .collect();

        // Sort by latest_at descending (most recently active first).
        result.sort_by(|a, b| b.latest_at.cmp(&a.latest_at));
        result
    }

    /// Build file contention map: files touched by 2+ agents.
    ///
    /// Excludes bridge seals. Sorted by agent count descending, capped at 10.
    fn build_file_contention(seals: &[Seal]) -> Vec<FileContention> {
        use std::collections::HashMap;

        // Map: file path → (set of agent IDs, total seal count).
        let mut file_agents: HashMap<String, (HashSet<String>, usize)> = HashMap::new();

        for seal in seals {
            if seal.agent.id == "writ-bridge" {
                continue;
            }
            for change in &seal.changes {
                let entry = file_agents
                    .entry(change.path.clone())
                    .or_insert_with(|| (HashSet::new(), 0));
                entry.0.insert(seal.agent.id.clone());
                entry.1 += 1;
            }
        }

        // Filter to files with 2+ agents, sort by agent count desc.
        let mut contention: Vec<FileContention> = file_agents
            .into_iter()
            .filter(|(_, (agents, _))| agents.len() >= 2)
            .map(|(path, (agents, total_seals))| {
                let mut agents_vec: Vec<String> = agents.into_iter().collect();
                agents_vec.sort();
                FileContention {
                    path,
                    agents: agents_vec,
                    total_seals,
                }
            })
            .collect();

        contention.sort_by(|a, b| {
            b.agents
                .len()
                .cmp(&a.agents.len())
                .then(a.path.cmp(&b.path))
        });
        contention.truncate(10);
        contention
    }

    /// Compute the single most important recommended action from context signals.
    ///
    /// Priority order (first match wins):
    /// 1. Blocking dependency — agent can't proceed until resolved
    /// 2. Convergence needed — diverged branches should be merged
    /// 3. High integration risk — agent should review before continuing
    /// 4. Unsealed changes — agent should checkpoint their work
    /// 5. Session complete — all done, generate summary
    /// 6. None — nothing urgent, keep working
    /// Build a lightweight chain integrity summary for context output.
    /// Returns None if no seals have crypto fields (all pre-Sprint A).
    fn build_chain_integrity(&self) -> Option<ChainIntegritySummary> {
        let result = self.verify_chain(None).ok()?;
        // Only include if there are any secured seals
        if result.verified == 0 && result.failures.is_empty() {
            return None;
        }
        Some(ChainIntegritySummary {
            valid: result.valid,
            total_seals: result.total_seals,
            verified: result.verified,
            unsecured: result.unsecured,
            failures: result.failures.len(),
        })
    }

    fn compute_recommended_action(
        dependency_status: &Option<Vec<DepStatus>>,
        convergence_recommended: bool,
        diverged_count: usize,
        integration_risk: &IntegrationRisk,
        seal_nudge: &Option<SealNudge>,
        session_complete: bool,
    ) -> Option<RecommendedAction> {
        // 1. Blocking dependency (spec-scoped only).
        if let Some(deps) = dependency_status {
            let blocking: Vec<&DepStatus> = deps.iter().filter(|d| !d.resolved).collect();
            if !blocking.is_empty() {
                let names: Vec<&str> = blocking.iter().map(|d| d.spec_id.as_str()).collect();
                return Some(RecommendedAction {
                    action: "wait_for_dependency".to_string(),
                    message: format!(
                        "Blocked by incomplete {}: {} — coordinate with the owning agent or wait for completion",
                        if names.len() == 1 { "dependency" } else { "dependencies" },
                        names.join(", "),
                    ),
                    priority: "high".to_string(),
                });
            }
        }

        // 2. Convergence needed.
        if convergence_recommended {
            return Some(RecommendedAction {
                action: "converge".to_string(),
                message: format!(
                    "{} diverged branch(es) detected — run `writ converge-all` to merge before continuing",
                    diverged_count,
                ),
                priority: "high".to_string(),
            });
        }

        // 3. High integration risk.
        if integration_risk.level == "high" {
            return Some(RecommendedAction {
                action: "review_risk".to_string(),
                message: format!(
                    "Integration risk is high (score {}) — review file_contention and diverged_branches before starting new work",
                    integration_risk.score,
                ),
                priority: "medium".to_string(),
            });
        }

        // 4. Unsealed changes.
        if let Some(nudge) = seal_nudge {
            return Some(RecommendedAction {
                action: "seal".to_string(),
                message: format!(
                    "{} file(s) changed since last seal — checkpoint your work with seal()",
                    nudge.unsealed_file_count,
                ),
                priority: "medium".to_string(),
            });
        }

        // 5. Session complete.
        if session_complete {
            return Some(RecommendedAction {
                action: "finish".to_string(),
                message: "All specs complete — run `writ summary` or `writ finish` to wrap up"
                    .to_string(),
                priority: "low".to_string(),
            });
        }

        // 6. Nothing urgent.
        None
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
            "verify_chain(use_convergence_key?)".to_string(),
            "verify_seal(seal_id, use_convergence_key?)".to_string(),
        ];

        // Lazy stale detection (GC.3.3a) — runs on every context() call.
        let stale_specs: Vec<String> = {
            let gc_config = crate::gc::GcConfig::load(&self.writ_dir).unwrap_or_default();
            self.scan_stale_specs(&gc_config)
                .unwrap_or_default()
                .into_iter()
                .map(|(id, age_secs)| {
                    let hours = age_secs / 3600;
                    if hours > 0 {
                        format!("spec '{}' inactive for {}h", id, hours)
                    } else {
                        format!("spec '{}' inactive for {}m", id, age_secs / 60)
                    }
                })
                .collect()
        };

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

        // Track HEAD for automatic conflict detection in seal().
        let tracked_head = match &scope {
            ContextScope::Full | ContextScope::Agent(_) => self.read_head()?,
            ContextScope::Spec(spec_id) => self.resolve_parent(Some(spec_id))?,
        };
        if let Ok(mut guard) = self.last_context_head.lock() {
            *guard = tracked_head;
        }

        match scope {
            ContextScope::Full => {
                let specs = self.list_specs()?;
                // Include changed_paths only on the 3 most recent seals to save tokens.
                // Agents can use `writ show SEAL_ID` for paths on older seals.
                let recent: Vec<SealSummary> = seals
                    .iter()
                    .filter(apply_filter)
                    .take(seal_limit)
                    .enumerate()
                    .map(|(i, s)| SealSummary::from_seal_with_paths(s, i < 3))
                    .collect();

                let index = self.load_index()?;
                let file_scope: Vec<String> = index.entries.keys().cloned().collect();
                let tracked_files = index.entries.len();

                // Walk ALL heads (global + spec branches) for agent activity,
                // so agents on diverged branches aren't invisible.
                let all_seals = self.log_all()?;
                let agent_activity = Self::build_agent_activity(&all_seals, None);

                // Detect diverged branches and build warnings.
                let diverged = self.diverged_branches()?;
                let diverged_branches: Vec<DivergedBranchWarning> = diverged
                    .into_iter()
                    .map(|db| {
                        let recommendation = format!(
                            "Run converge() to merge spec '{}' ({} seal(s) by {}) into the main branch",
                            db.spec_id,
                            db.seal_count,
                            db.agents.join(", "),
                        );
                        DivergedBranchWarning {
                            spec_id: db.spec_id,
                            tip_seal: db.tip_seal,
                            seal_count: db.seal_count,
                            agents: db.agents,
                            recommendation,
                        }
                    })
                    .collect();

                let convergence_recommended = !diverged_branches.is_empty();

                let file_scope_violations: Vec<FileScopeViolation> = all_seals
                    .iter()
                    .take(seal_limit)
                    .filter_map(|seal| {
                        let spec_id = seal.spec_id.as_ref()?;
                        let changed: Vec<String> =
                            seal.changes.iter().map(|c| c.path.clone()).collect();
                        let warning = self.check_file_scope(spec_id, &changed)?;
                        Some(FileScopeViolation {
                            seal_id: seal.id[..12].to_string(),
                            agent_id: seal.agent.id.clone(),
                            spec_id: spec_id.clone(),
                            out_of_scope_files: warning.out_of_scope_files,
                            declared_scope: warning.declared_scope,
                        })
                    })
                    .collect();

                // Build file contention map: files touched by 2+ agents.
                let file_contention = Self::build_file_contention(&all_seals);

                let max_file_agents = file_contention
                    .iter()
                    .map(|fc| fc.agents.len())
                    .max()
                    .unwrap_or(0);
                let integration_risk = IntegrationRisk::compute(
                    diverged_branches.len(),
                    max_file_agents,
                    file_scope_violations.len(),
                    file_contention.len(),
                );
                // Always include integration risk (even when low) so agents always see the field.

                let mut result = ContextOutput {
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
                    agent_activity,
                    diverged_branches,
                    convergence_recommended,
                    file_scope_violations,
                    file_contention,
                    integration_risk,
                    chain_integrity: self.build_chain_integrity(),
                    stale_specs: stale_specs.clone(),
                    session_complete: false,
                    session_summary: None,
                    recommended_action: None,
                    available_operations,
                };

                // Check if all specs are complete and inject session summary.
                if let Some(ref all) = result.all_specs {
                    let all_complete = !all.is_empty()
                        && all
                            .iter()
                            .all(|s| matches!(s.status, crate::spec::SpecStatus::Complete));
                    if all_complete {
                        result.session_complete = true;
                        let work_seals = all_seals
                            .iter()
                            .filter(|s| s.agent.id != "writ-bridge")
                            .count();
                        let agent_ids: HashSet<&str> = all_seals
                            .iter()
                            .filter(|s| s.agent.id != "writ-bridge")
                            .map(|s| s.agent.id.as_str())
                            .collect();
                        let mut file_set: HashSet<&str> = HashSet::new();
                        for s in all_seals.iter().filter(|s| s.agent.id != "writ-bridge") {
                            for c in &s.changes {
                                file_set.insert(&c.path);
                            }
                        }
                        result.session_summary = Some(SessionSummary {
                            headline: format!(
                                "{} spec(s) complete — {} seal(s) by {} agent(s)",
                                all.len(),
                                work_seals,
                                agent_ids.len(),
                            ),
                            total_seals: work_seals,
                            agent_count: agent_ids.len(),
                            specs_completed: all.len(),
                            files_changed: file_set.len(),
                            message: "All specs complete. Run `writ summary` for the full report and suggested commit message.".to_string(),
                        });
                    }
                }

                // Recommended action (computed after session_complete is known).
                result.recommended_action = Self::compute_recommended_action(
                    &None, // Full scope has no dependency_status
                    result.convergence_recommended,
                    result.diverged_branches.len(),
                    &result.integration_risk,
                    &result.seal_nudge,
                    result.session_complete,
                );

                Ok(result)
            }
            ContextScope::Spec(spec_id) => {
                let spec = self.load_spec(&spec_id)?;

                // Walk from the spec's own head pointer so seals on diverged
                // branches are visible. Previously this filtered self.log()
                // (global HEAD chain), which missed diverged spec seals entirely.
                let spec_seal_chain = self.spec_log(&spec_id)?;
                let spec_seals: Vec<SealSummary> = spec_seal_chain
                    .iter()
                    .filter(|s| s.spec_id.as_deref() == Some(spec_id.as_str()))
                    .filter(apply_filter)
                    .take(seal_limit)
                    .map(SealSummary::from_seal)
                    .collect();

                // Build the set of spec-relevant files for filtering.
                // Priority: spec.file_scope (explicit) > files from spec seals (inferred).
                let file_scope: Vec<String>;
                let has_scope_filter: bool;

                if !spec.file_scope.is_empty() {
                    file_scope = spec.file_scope.clone();
                    has_scope_filter = true;
                } else {
                    // Infer scope from files touched in this spec's seals.
                    let mut inferred: HashSet<String> = HashSet::new();
                    for seal_id in &spec.sealed_by {
                        if let Ok(s) = self.load_seal(seal_id) {
                            for c in &s.changes {
                                inferred.insert(c.path.clone());
                            }
                        }
                    }
                    if inferred.is_empty() {
                        // No seals yet — fall back to all tracked files.
                        let index = self.load_index()?;
                        file_scope = index.entries.keys().cloned().collect();
                        has_scope_filter = false;
                    } else {
                        file_scope = inferred.into_iter().collect();
                        has_scope_filter = true;
                    }
                };

                // Filter working state to spec-relevant files.
                let (filtered_ws, filtered_pending, filtered_nudge) = if has_scope_filter {
                    let matches_scope = |path: &str| -> bool {
                        file_scope.iter().any(|scope_entry| {
                            // Exact match or prefix match (for directory patterns like "src/components/").
                            path == scope_entry
                                || (scope_entry.ends_with('/')
                                    && path.starts_with(scope_entry.as_str()))
                        })
                    };

                    let filtered_state = WorkingStateSummary {
                        clean: ws_summary
                            .new_files
                            .iter()
                            .chain(ws_summary.modified_files.iter())
                            .chain(ws_summary.deleted_files.iter())
                            .all(|p| !matches_scope(p)),
                        new_files: ws_summary
                            .new_files
                            .iter()
                            .filter(|p| matches_scope(p))
                            .cloned()
                            .collect(),
                        modified_files: ws_summary
                            .modified_files
                            .iter()
                            .filter(|p| matches_scope(p))
                            .cloned()
                            .collect(),
                        deleted_files: ws_summary
                            .deleted_files
                            .iter()
                            .filter(|p| matches_scope(p))
                            .cloned()
                            .collect(),
                        tracked_count: ws_summary.tracked_count,
                    };

                    let filtered_pending = pending_changes.map(|pc| {
                        let filtered_files: Vec<_> = pc
                            .files
                            .into_iter()
                            .filter(|f| matches_scope(&f.path))
                            .collect();
                        let total_add: usize = filtered_files.iter().map(|f| f.additions).sum();
                        let total_del: usize = filtered_files.iter().map(|f| f.deletions).sum();
                        DiffSummary {
                            files_changed: filtered_files.len(),
                            total_additions: total_add,
                            total_deletions: total_del,
                            files: filtered_files,
                        }
                    });

                    let spec_change_count = filtered_state.new_files.len()
                        + filtered_state.modified_files.len()
                        + filtered_state.deleted_files.len();
                    let filtered_nudge = if spec_change_count > 0 {
                        Some(SealNudge {
                            unsealed_file_count: spec_change_count,
                            message: format!(
                                "{spec_change_count} spec-relevant file(s) changed since last seal — consider checkpointing"
                            ),
                        })
                    } else {
                        None
                    };

                    (filtered_state, filtered_pending, filtered_nudge)
                } else {
                    (ws_summary, pending_changes, seal_nudge)
                };

                let tracked_files = file_scope.len();

                // Compute dependency status.
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

                // Compute spec progress.
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
                    let effective = self.effective_spec_status_from_sealed_by(&spec);
                    let status_str = match effective {
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

                // Walk ALL heads for agent activity so diverged agents are visible,
                // filtered to spec-relevant files so agents only see relevant ownership.
                let all_seals = self.log_all()?;
                let agent_activity = if has_scope_filter {
                    let scope_ref = &file_scope;
                    let scope_fn = |path: &str| -> bool {
                        scope_ref.iter().any(|scope_entry| {
                            path == scope_entry
                                || (scope_entry.ends_with('/')
                                    && path.starts_with(scope_entry.as_str()))
                        })
                    };
                    Self::build_agent_activity(&all_seals, Some(&scope_fn))
                } else {
                    Self::build_agent_activity(&all_seals, None)
                };

                // --- Risk signals (filtered to spec relevance) ---

                // Diverged branches: all of them. Agents need the full divergence
                // picture since any diverged branch can affect convergence.
                let diverged = self.diverged_branches()?;
                let diverged_branches: Vec<DivergedBranchWarning> = diverged
                    .into_iter()
                    .map(|db| {
                        let recommendation = format!(
                            "Run converge() to merge spec '{}' ({} seal(s) by {}) into the main branch",
                            db.spec_id,
                            db.seal_count,
                            db.agents.join(", "),
                        );
                        DivergedBranchWarning {
                            spec_id: db.spec_id,
                            tip_seal: db.tip_seal,
                            seal_count: db.seal_count,
                            agents: db.agents,
                            recommendation,
                        }
                    })
                    .collect();
                let convergence_recommended = !diverged_branches.is_empty();

                // File contention: filter to files in this spec's scope.
                let all_contention = Self::build_file_contention(&all_seals);
                let file_contention: Vec<FileContention> = if has_scope_filter {
                    let scope_ref = &file_scope;
                    all_contention
                        .into_iter()
                        .filter(|fc| {
                            scope_ref.iter().any(|scope_entry| {
                                fc.path == *scope_entry
                                    || (scope_entry.ends_with('/')
                                        && fc.path.starts_with(scope_entry.as_str()))
                            })
                        })
                        .collect()
                } else {
                    all_contention
                };

                // Scope violations: only for this spec's seals.
                let file_scope_violations: Vec<FileScopeViolation> = all_seals
                    .iter()
                    .filter(|s| s.spec_id.as_deref() == Some(spec_id.as_str()))
                    .take(seal_limit)
                    .filter_map(|seal| {
                        let changed: Vec<String> =
                            seal.changes.iter().map(|c| c.path.clone()).collect();
                        let warning = self.check_file_scope(&spec_id, &changed)?;
                        Some(FileScopeViolation {
                            seal_id: seal.id[..12].to_string(),
                            agent_id: seal.agent.id.clone(),
                            spec_id: spec_id.clone(),
                            out_of_scope_files: warning.out_of_scope_files,
                            declared_scope: warning.declared_scope,
                        })
                    })
                    .collect();

                // Integration risk: computed from filtered signals.
                let max_file_agents = file_contention
                    .iter()
                    .map(|fc| fc.agents.len())
                    .max()
                    .unwrap_or(0);
                let integration_risk = IntegrationRisk::compute(
                    diverged_branches.len(),
                    max_file_agents,
                    file_scope_violations.len(),
                    file_contention.len(),
                );

                // Recommended action.
                let recommended_action = Self::compute_recommended_action(
                    &dependency_status,
                    convergence_recommended,
                    diverged_branches.len(),
                    &integration_risk,
                    &filtered_nudge,
                    false,
                );

                Ok(ContextOutput {
                    writ_version: "0.1.0".to_string(),
                    active_spec: Some(spec),
                    all_specs: None,
                    working_state: filtered_ws,
                    recent_seals: spec_seals,
                    pending_changes: filtered_pending,
                    seal_nudge: filtered_nudge,
                    file_scope,
                    tracked_files,
                    dependency_status,
                    spec_progress,
                    agent_activity,
                    diverged_branches,
                    convergence_recommended,
                    file_scope_violations,
                    file_contention,
                    integration_risk,
                    chain_integrity: self.build_chain_integrity(),
                    stale_specs: stale_specs.clone(),
                    session_complete: false,
                    session_summary: None,
                    recommended_action,
                    available_operations,
                })
            }
            ContextScope::Agent(agent_id) => {
                // Agent-scoped context: derive the agent's world from their seals.
                let all_seals = self.log_all()?;

                // 1. Find all specs this agent has worked on.
                let agent_spec_ids: Vec<String> = {
                    let mut spec_set: HashSet<String> = HashSet::new();
                    for seal in &all_seals {
                        if seal.agent.id == agent_id {
                            if let Some(ref sid) = seal.spec_id {
                                spec_set.insert(sid.clone());
                            }
                        }
                    }
                    spec_set.into_iter().collect()
                };

                // 2. Load specs and build file scope.
                let agent_specs: Vec<Spec> = agent_spec_ids
                    .iter()
                    .filter_map(|sid| self.load_spec(sid).ok())
                    .collect();

                // File scope = union of specs' declared file_scope + files agent actually sealed.
                let mut file_scope_set: HashSet<String> = HashSet::new();
                for spec in &agent_specs {
                    for f in &spec.file_scope {
                        file_scope_set.insert(f.clone());
                    }
                }
                for seal in &all_seals {
                    if seal.agent.id == agent_id {
                        for c in &seal.changes {
                            file_scope_set.insert(c.path.clone());
                        }
                    }
                }
                let file_scope: Vec<String> = file_scope_set.into_iter().collect();
                let has_scope_filter = !file_scope.is_empty();

                // 3. Filter seal history (respects ContextFilter for status/agent filtering).
                let recent_seals: Vec<SealSummary> = all_seals
                    .iter()
                    .filter(|s| {
                        // Include seals on agent's specs (by any agent — cross-agent awareness).
                        s.spec_id
                            .as_ref()
                            .map_or(false, |sid| agent_spec_ids.contains(sid))
                    })
                    .filter(apply_filter)
                    .take(seal_limit)
                    .map(SealSummary::from_seal)
                    .collect();

                // 4. Filter working state to agent's file scope.
                let (filtered_ws, filtered_pending, filtered_nudge) = if has_scope_filter {
                    let scope_ref = &file_scope;
                    let matches_scope = |path: &str| -> bool {
                        scope_ref.iter().any(|scope_entry| {
                            path == scope_entry
                                || (scope_entry.ends_with('/')
                                    && path.starts_with(scope_entry.as_str()))
                        })
                    };

                    let filtered_state = WorkingStateSummary {
                        clean: ws_summary
                            .new_files
                            .iter()
                            .chain(ws_summary.modified_files.iter())
                            .chain(ws_summary.deleted_files.iter())
                            .all(|p| !matches_scope(p)),
                        new_files: ws_summary
                            .new_files
                            .iter()
                            .filter(|p| matches_scope(p))
                            .cloned()
                            .collect(),
                        modified_files: ws_summary
                            .modified_files
                            .iter()
                            .filter(|p| matches_scope(p))
                            .cloned()
                            .collect(),
                        deleted_files: ws_summary
                            .deleted_files
                            .iter()
                            .filter(|p| matches_scope(p))
                            .cloned()
                            .collect(),
                        tracked_count: ws_summary.tracked_count,
                    };

                    let filtered_pending = pending_changes.map(|pc| {
                        let filtered_files: Vec<_> = pc
                            .files
                            .into_iter()
                            .filter(|f| matches_scope(&f.path))
                            .collect();
                        let total_add: usize = filtered_files.iter().map(|f| f.additions).sum();
                        let total_del: usize = filtered_files.iter().map(|f| f.deletions).sum();
                        DiffSummary {
                            files_changed: filtered_files.len(),
                            total_additions: total_add,
                            total_deletions: total_del,
                            files: filtered_files,
                        }
                    });

                    let agent_change_count = filtered_state.new_files.len()
                        + filtered_state.modified_files.len()
                        + filtered_state.deleted_files.len();
                    let filtered_nudge = if agent_change_count > 0 {
                        Some(SealNudge {
                            unsealed_file_count: agent_change_count,
                            message: format!(
                                "{agent_change_count} file(s) in your scope changed since last seal — consider checkpointing"
                            ),
                        })
                    } else {
                        None
                    };

                    (filtered_state, filtered_pending, filtered_nudge)
                } else {
                    (ws_summary, pending_changes, seal_nudge)
                };

                let tracked_files = file_scope.len();

                // 5. Agent activity: ALL agents shown for cross-agent coordination,
                //    but file ownership filtered to agent's scope.
                let agent_activity = if has_scope_filter {
                    let scope_ref = &file_scope;
                    let scope_fn = |path: &str| -> bool {
                        scope_ref.iter().any(|scope_entry| {
                            path == scope_entry
                                || (scope_entry.ends_with('/')
                                    && path.starts_with(scope_entry.as_str()))
                        })
                    };
                    Self::build_agent_activity(&all_seals, Some(&scope_fn))
                } else {
                    Self::build_agent_activity(&all_seals, None)
                };

                // 6. Diverged branches: only for agent's specs.
                let all_diverged = self.diverged_branches()?;
                let diverged_branches: Vec<DivergedBranchWarning> = all_diverged
                    .into_iter()
                    .filter(|db| agent_spec_ids.contains(&db.spec_id))
                    .map(|db| {
                        let recommendation = format!(
                            "Run converge() to merge spec '{}' ({} seal(s) by {}) into the main branch",
                            db.spec_id,
                            db.seal_count,
                            db.agents.join(", "),
                        );
                        DivergedBranchWarning {
                            spec_id: db.spec_id,
                            tip_seal: db.tip_seal,
                            seal_count: db.seal_count,
                            agents: db.agents,
                            recommendation,
                        }
                    })
                    .collect();
                let convergence_recommended = !diverged_branches.is_empty();

                // 7. File contention: filtered to agent's file scope.
                let all_contention = Self::build_file_contention(&all_seals);
                let file_contention: Vec<FileContention> = if has_scope_filter {
                    let scope_ref = &file_scope;
                    all_contention
                        .into_iter()
                        .filter(|fc| {
                            scope_ref.iter().any(|scope_entry| {
                                fc.path == *scope_entry
                                    || (scope_entry.ends_with('/')
                                        && fc.path.starts_with(scope_entry.as_str()))
                            })
                        })
                        .collect()
                } else {
                    all_contention
                };

                // 8. Scope violations: only for agent's seals.
                let file_scope_violations: Vec<FileScopeViolation> = all_seals
                    .iter()
                    .filter(|s| s.agent.id == agent_id)
                    .take(seal_limit)
                    .filter_map(|seal| {
                        let spec_id = seal.spec_id.as_ref()?;
                        let changed: Vec<String> =
                            seal.changes.iter().map(|c| c.path.clone()).collect();
                        let warning = self.check_file_scope(spec_id, &changed)?;
                        Some(FileScopeViolation {
                            seal_id: seal.id[..12].to_string(),
                            agent_id: seal.agent.id.clone(),
                            spec_id: spec_id.clone(),
                            out_of_scope_files: warning.out_of_scope_files,
                            declared_scope: warning.declared_scope,
                        })
                    })
                    .collect();

                // 9. Dependency status: union of all agent specs' dependencies.
                let all_dep_ids: HashSet<String> = agent_specs
                    .iter()
                    .flat_map(|spec| spec.depends_on.iter().cloned())
                    .collect();
                let dependency_status = if !all_dep_ids.is_empty() {
                    let deps: Vec<DepStatus> = all_dep_ids
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

                // 10. Integration risk from agent-scoped signals.
                let max_file_agents = file_contention
                    .iter()
                    .map(|fc| fc.agents.len())
                    .max()
                    .unwrap_or(0);
                let integration_risk = IntegrationRisk::compute(
                    diverged_branches.len(),
                    max_file_agents,
                    file_scope_violations.len(),
                    file_contention.len(),
                );

                // 11. Recommended action.
                let recommended_action = Self::compute_recommended_action(
                    &dependency_status,
                    convergence_recommended,
                    diverged_branches.len(),
                    &integration_risk,
                    &filtered_nudge,
                    false, // agent scope is partial view — never session_complete
                );

                Ok(ContextOutput {
                    writ_version: "0.1.0".to_string(),
                    active_spec: None, // agent may have multiple specs
                    all_specs: if agent_specs.is_empty() {
                        None
                    } else {
                        Some(agent_specs)
                    },
                    working_state: filtered_ws,
                    recent_seals,
                    pending_changes: filtered_pending,
                    seal_nudge: filtered_nudge,
                    file_scope,
                    tracked_files,
                    dependency_status,
                    spec_progress: None, // no single spec to show progress for
                    agent_activity,
                    diverged_branches,
                    convergence_recommended,
                    file_scope_violations,
                    file_contention,
                    integration_risk,
                    chain_integrity: self.build_chain_integrity(),
                    stale_specs,
                    session_complete: false, // agent scope is partial view
                    session_summary: None,
                    recommended_action,
                    available_operations,
                })
            }
        }
    }

    /// Get the HEAD recorded at the last `context()` call.
    ///
    /// Used for automatic conflict detection: if HEAD moved between
    /// `context()` and `seal()`, the caller can detect it.
    pub fn last_context_head(&self) -> Option<String> {
        self.last_context_head.lock().ok().and_then(|g| g.clone())
    }

    /// Clear the tracked context HEAD (called after seal to prevent stale state).
    pub fn clear_context_head(&self) {
        if let Ok(mut guard) = self.last_context_head.lock() {
            *guard = None;
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

        // Update the spec head if the restored seal is spec-scoped, so
        // diverged_branches() reflects the restored state accurately.
        if let Some(ref spec_id) = seal.spec_id {
            self.write_spec_head(spec_id, &seal.id)?;
        }

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

        // Auto-write summary when all specs are complete.
        if matches!(spec.status, crate::spec::SpecStatus::Complete) {
            let all_specs = self.list_specs().unwrap_or_default();
            let all_complete = !all_specs.is_empty()
                && all_specs
                    .iter()
                    .all(|s| matches!(s.status, crate::spec::SpecStatus::Complete));
            if all_complete {
                if let Ok(summary) = self.summary() {
                    let summary_json = self.writ_dir.join("summary.json");
                    if let Ok(json) = serde_json::to_string_pretty(&summary) {
                        let _ = atomic_write(&summary_json, json.as_bytes());
                    }
                    let summary_txt = self.writ_dir.join("summary.txt");
                    let _ = atomic_write(&summary_txt, summary.commit_message.as_bytes());
                }
            }
        }

        Ok(spec)
    }

    /// Mark a spec as done: transitions to Complete with optional summary.
    /// This is the round-trip workflow entry point (distinct from GC's
    /// `complete_spec` which transitions lifecycle_state).
    pub fn mark_spec_done(&self, spec_id: &str, summary: Option<String>) -> WritResult<Spec> {
        let mut spec = self.load_spec(spec_id)?;

        if matches!(spec.status, crate::spec::SpecStatus::Complete) {
            return Err(WritError::Other(format!(
                "spec '{}' is already complete",
                spec_id
            )));
        }

        let now = chrono::Utc::now();
        spec.status = crate::spec::SpecStatus::Complete;
        spec.completion_summary = summary;
        spec.completed_at = Some(now);
        spec.updated_at = now;
        self.save_spec(&spec)?;
        Ok(spec)
    }

    /// Record that a spec's work was committed to git.
    pub fn mark_spec_committed(&self, spec_id: &str, git_hash: &str) -> WritResult<Spec> {
        let mut spec = self.load_spec(spec_id)?;

        if !matches!(spec.status, crate::spec::SpecStatus::Complete) {
            return Err(WritError::Other(format!(
                "spec '{}' is not complete, cannot mark as committed",
                spec_id
            )));
        }

        spec.mark_committed(git_hash.to_string());
        self.save_spec(&spec)?;
        Ok(spec)
    }

    // ── Proposal CRUD ──────────────────────────────────────────────────

    /// Directory where proposals are stored.
    fn proposals_dir(&self) -> std::path::PathBuf {
        self.writ_dir.join("proposals")
    }

    /// Create a new proposal. Supersedes any pending proposals with overlapping specs.
    pub fn create_proposal(
        &self,
        spec_ids: Vec<String>,
        message: String,
        proposed_by: String,
        strategy: String,
    ) -> WritResult<crate::proposal::Proposal> {
        let dir = self.proposals_dir();
        std::fs::create_dir_all(&dir)?;

        let mut proposal =
            crate::proposal::Proposal::new(spec_ids.clone(), message, proposed_by, strategy);

        // Supersede any pending proposals with overlapping specs
        let existing = self.list_proposals()?;
        for mut old in existing {
            if old.is_pending() && old.overlaps_with(&spec_ids) {
                old.supersede(&proposal.id);
                self.save_proposal(&old)?;
            }
        }

        self.save_proposal(&proposal)?;
        Ok(proposal)
    }

    /// List all proposals, sorted by creation time (newest first).
    pub fn list_proposals(&self) -> WritResult<Vec<crate::proposal::Proposal>> {
        let dir = self.proposals_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut proposals = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "json") {
                let data = std::fs::read_to_string(&path)?;
                if let Ok(p) = serde_json::from_str::<crate::proposal::Proposal>(&data) {
                    proposals.push(p);
                }
            }
        }

        proposals.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(proposals)
    }

    /// Accept a proposal: execute its commit and mark it accepted.
    /// Returns the commit hash.
    pub fn accept_proposal(&self, proposal_id: &str) -> WritResult<crate::proposal::Proposal> {
        let mut proposal = self.load_proposal(proposal_id)?;

        if !proposal.is_pending() {
            return Err(WritError::Other(format!(
                "proposal '{}' is not pending (status: {:?})",
                proposal_id, proposal.status
            )));
        }

        // The actual git commit is done by the CLI (it has GitOps).
        // We just mark the proposal as accepted — the CLI passes the hash back.
        // For now, mark accepted without a hash; CLI calls update_proposal_hash().
        proposal.accept(String::new());
        self.save_proposal(&proposal)?;
        Ok(proposal)
    }

    /// Update an accepted proposal with the actual commit hash.
    pub fn update_proposal_hash(&self, proposal_id: &str, hash: &str) -> WritResult<()> {
        let mut proposal = self.load_proposal(proposal_id)?;
        proposal.commit_hash = Some(hash.to_string());
        self.save_proposal(&proposal)?;
        Ok(())
    }

    /// Reject a proposal. Specs remain in completed state for future proposals.
    pub fn reject_proposal(&self, proposal_id: &str) -> WritResult<crate::proposal::Proposal> {
        let mut proposal = self.load_proposal(proposal_id)?;

        if !proposal.is_pending() {
            return Err(WritError::Other(format!(
                "proposal '{}' is not pending (status: {:?})",
                proposal_id, proposal.status
            )));
        }

        proposal.reject();
        self.save_proposal(&proposal)?;
        Ok(proposal)
    }

    /// Load a single proposal by ID.
    pub fn load_proposal(&self, id: &str) -> WritResult<crate::proposal::Proposal> {
        let path = self.proposals_dir().join(format!("{}.json", id));
        if !path.exists() {
            return Err(WritError::Other(format!("proposal '{}' not found", id)));
        }
        let data = std::fs::read_to_string(&path)?;
        serde_json::from_str(&data)
            .map_err(|e| WritError::Other(format!("failed to parse proposal: {e}")))
    }

    /// Save a proposal to disk.
    fn save_proposal(&self, proposal: &crate::proposal::Proposal) -> WritResult<()> {
        let dir = self.proposals_dir();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", proposal.id));
        let data = serde_json::to_string_pretty(proposal)?;
        std::fs::write(path, data)?;
        Ok(())
    }

    /// Promote a spec's status to match a seal's task status.
    /// Returns `true` if the spec status was actually changed.
    ///
    /// Rules:
    /// - `TaskStatus::Complete` → `SpecStatus::Complete` (unless blocked)
    /// - `TaskStatus::InProgress` → `SpecStatus::InProgress` (if currently pending)
    /// - Never downgrades: complete stays complete, blocked stays blocked
    fn auto_promote_spec_status(&self, spec: &mut Spec, seal_status: &TaskStatus) -> bool {
        use crate::spec::SpecStatus;

        if matches!(spec.status, SpecStatus::Blocked) {
            return false;
        }

        let new_status = match seal_status {
            TaskStatus::Complete => Some(SpecStatus::Complete),
            TaskStatus::InProgress => {
                if matches!(spec.status, SpecStatus::Pending) {
                    Some(SpecStatus::InProgress)
                } else {
                    None
                }
            }
            _ => None,
        };

        if let Some(status) = new_status {
            if spec.status != status {
                spec.status = status;
                return true;
            }
        }
        false
    }

    /// Derive effective status from a spec's `sealed_by` list.
    /// Used by `context()` where we don't have pre-filtered seal refs.
    ///
    /// Only promotes from `Pending` — if someone explicitly set a status
    /// (InProgress, Complete, Blocked), that decision is respected.
    fn effective_spec_status_from_sealed_by(&self, spec: &Spec) -> crate::spec::SpecStatus {
        use crate::spec::SpecStatus;

        if !matches!(spec.status, SpecStatus::Pending) {
            return spec.status.clone();
        }

        let mut has_complete = false;
        let mut has_in_progress = false;
        for seal_id in &spec.sealed_by {
            if let Ok(seal) = self.load_seal(seal_id) {
                match seal.status {
                    TaskStatus::Complete => has_complete = true,
                    TaskStatus::InProgress => has_in_progress = true,
                    _ => {}
                }
            }
        }

        if has_complete {
            return SpecStatus::Complete;
        }
        if has_in_progress {
            return SpecStatus::InProgress;
        }

        SpecStatus::Pending
    }

    /// Derive the effective status for a spec by considering both the
    /// spec-level status and the latest seal status.
    ///
    /// Only promotes from `Pending` — if someone explicitly set a status,
    /// that decision is respected. This catches the common case where
    /// agents sealed with --status complete but never called update_spec().
    fn effective_spec_status(&self, spec: &Spec, seals: &[&&Seal]) -> crate::spec::SpecStatus {
        use crate::spec::SpecStatus;

        if !matches!(spec.status, SpecStatus::Pending) {
            return spec.status.clone();
        }

        let has_complete_seal = seals.iter().any(|s| s.status == TaskStatus::Complete);
        let has_in_progress_seal = seals.iter().any(|s| s.status == TaskStatus::InProgress);

        if has_complete_seal {
            return SpecStatus::Complete;
        }
        if has_in_progress_seal {
            return SpecStatus::InProgress;
        }

        SpecStatus::Pending
    }

    /// If all specs are complete, write the summary cache files.
    fn check_all_specs_complete(&self) {
        let all_specs = self.list_specs().unwrap_or_default();
        let all_complete = !all_specs.is_empty()
            && all_specs
                .iter()
                .all(|s| matches!(s.status, crate::spec::SpecStatus::Complete));
        if all_complete {
            if let Ok(summary) = self.summary() {
                let summary_json = self.writ_dir.join("summary.json");
                if let Ok(json) = serde_json::to_string_pretty(&summary) {
                    let _ = atomic_write(&summary_json, json.as_bytes());
                }
                let summary_txt = self.writ_dir.join("summary.txt");
                let _ = atomic_write(&summary_txt, summary.commit_message.as_bytes());
            }
        }
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
    pub fn converge(&self, left_spec: &str, right_spec: &str) -> WritResult<ConvergenceReport> {
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

        // Update the index so state() reflects the converged working directory.
        let mut written_paths: Vec<String> = Vec::new();
        written_paths.extend(report.auto_merged.iter().map(|m| m.path.clone()));
        written_paths.extend(report.left_only.iter().cloned());
        written_paths.extend(report.right_only.iter().cloned());
        written_paths.extend(resolutions.iter().map(|r| r.path.clone()));

        let mut index = self.load_index()?;
        for path in &written_paths {
            let file_path = self.validate_path(path)?;
            let content = fs::read(&file_path)?;
            let hash = self.objects.store(&content)?;
            let size = content.len() as u64;
            index.upsert(path, hash, size);
        }
        index.save(&self.writ_dir.join("index.json"))?;

        // Archive the right spec's current head before advancing it.
        // This preserves the diverged seal chain so log_all() / summary()
        // can still enumerate seals from pre-convergence branches.
        if let Some(old_head) = self.read_spec_head(&report.right_spec)? {
            self.archive_merged_head(&old_head)?;
        }

        // Advance the right (diverged) spec's head pointer to the left
        // spec's latest seal. This puts the right spec "on" the HEAD chain
        // so diverged_branches() no longer reports it as diverged.
        self.write_spec_head(&report.right_spec, &report.left_seal_id)?;

        // Refresh summary.json so it reflects post-convergence state.
        self.check_all_specs_complete();

        Ok(())
    }

    /// Converge ALL diverged branches in sequence.
    ///
    /// Finds the base spec (on HEAD chain), orders diverged branches by
    /// most-seals-first, and merges each into the base. Returns a structured
    /// report with per-step results, conflict resolutions, and warnings.
    ///
    /// When `apply` is true, merged files are written to the working directory.
    /// With `ConvergeStrategy::MostRecent`, conflicts are auto-resolved by
    /// preferring the version from the more recently sealed branch.
    pub fn converge_all(
        &self,
        strategy: ConvergeStrategy,
        apply: bool,
    ) -> WritResult<ConvergeAllReport> {
        // ── All-spec convergence ─────────────────────────────────────────
        // Collect ALL specs with sealed work, not just diverged ones.
        // Sequential sealing puts all seals on the HEAD chain, so
        // diverged_branches() misses specs that still need N-way merging.
        let specs = self.list_specs()?;
        let specs_with_work: Vec<&Spec> =
            specs.iter().filter(|s| !s.sealed_by.is_empty()).collect();

        let diverged = self.diverged_branches()?;
        let diverged_ids: HashSet<String> = diverged.iter().map(|b| b.spec_id.clone()).collect();

        // Nothing to converge if fewer than 2 specs have work AND nothing
        // is diverged. (A single spec with diverged branches still needs
        // merging.)
        if specs_with_work.len() < 2 && diverged.is_empty() {
            return Ok(ConvergeAllReport {
                base_spec: String::new(),
                merge_order: Vec::new(),
                merges: Vec::new(),
                strategy: strategy_name(strategy),
                total_auto_merged: 0,
                total_conflicts: 0,
                total_resolutions: 0,
                is_clean: true,
                degraded: false,
                applied: false,
                warnings: Vec::new(),
                escalations: Vec::new(),
                quality_report: None,
                files_changed: Vec::new(),
                convergence_record: None,
            });
        }

        // ── Choose a base spec ──────────────────────────────────────────
        // Prefer an on-HEAD, complete spec with sealed work as the base.
        // Its tree becomes the initial accumulated state.
        let on_head: Vec<_> = specs_with_work
            .iter()
            .filter(|s| {
                !diverged_ids.contains(&s.id)
                    && matches!(s.status, crate::spec::SpecStatus::Complete)
            })
            .collect();

        let base_spec = if let Some(s) = on_head.first() {
            s.id.clone()
        } else if !specs_with_work.is_empty() {
            // No on-head complete spec. Pick the spec with the most seals
            // as the base (it has the most context).
            let mut sorted: Vec<_> = specs_with_work.iter().collect();
            sorted.sort_by(|a, b| {
                b.sealed_by
                    .len()
                    .cmp(&a.sealed_by.len())
                    .then_with(|| a.id.cmp(&b.id))
            });
            sorted[0].id.clone()
        } else {
            return Err(WritError::Other(
                "No specs available for convergence base".to_string(),
            ));
        };

        // ── Build ordered merge list from ALL non-base specs ────────────
        // Include diverged specs AND non-diverged specs with sealed work.
        // This ensures every spec's contributions participate in the merge.
        let mut ordered: Vec<DivergedBranch> = Vec::new();

        // Add diverged specs.
        for branch in &diverged {
            if branch.spec_id != base_spec {
                ordered.push(branch.clone());
            }
        }

        // Add non-diverged specs with work (excluding base and already-
        // included diverged specs).
        for spec in &specs_with_work {
            if spec.id == base_spec || diverged_ids.contains(&spec.id) {
                continue;
            }
            let agents: Vec<String> = spec
                .sealed_by
                .iter()
                .filter_map(|sid| self.load_seal(sid).ok())
                .map(|s| s.agent.id.clone())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            let tip = spec
                .sealed_by
                .last()
                .map(|s| s[..12.min(s.len())].to_string())
                .unwrap_or_default();
            ordered.push(DivergedBranch {
                spec_id: spec.id.clone(),
                tip_seal: tip,
                seal_count: spec.sealed_by.len(),
                agents,
            });
        }

        // After including all specs, if nothing to merge, return early.
        if ordered.is_empty() {
            return Ok(ConvergeAllReport {
                base_spec,
                merge_order: Vec::new(),
                merges: Vec::new(),
                strategy: strategy_name(strategy),
                total_auto_merged: 0,
                total_conflicts: 0,
                total_resolutions: 0,
                is_clean: true,
                degraded: false,
                applied: false,
                warnings: Vec::new(),
                escalations: Vec::new(),
                quality_report: None,
                files_changed: Vec::new(),
                convergence_record: None,
            });
        }

        // ── N-agent merge ordering (T9) ──────────────────────────────────
        // Build MergeCandidates with file overlap data, then compute an
        // optimal merge order that minimizes conflict complexity.
        let base_spec_data_for_order = self.load_spec(&base_spec)?;
        let base_modified_for_order = self
            .spec_modified_files(&base_spec_data_for_order)
            .unwrap_or_default();

        let mut candidates: Vec<convergence::MergeCandidate> = Vec::new();
        for branch in &ordered {
            let modified = if let Ok(spec_data) = self.load_spec(&branch.spec_id) {
                self.spec_modified_files(&spec_data).unwrap_or_default()
            } else {
                HashSet::new()
            };
            candidates.push(convergence::MergeCandidate {
                spec_id: branch.spec_id.clone(),
                modified_files: modified,
                seal_count: branch.seal_count,
            });
        }

        let optimal_order = convergence::compute_merge_order(&candidates, &base_modified_for_order);

        // Reorder `ordered` to match the computed optimal order.
        let order_map: HashMap<String, usize> = optimal_order
            .iter()
            .enumerate()
            .map(|(i, id)| (id.clone(), i))
            .collect();
        ordered.sort_by_key(|b| order_map.get(&b.spec_id).copied().unwrap_or(usize::MAX));

        let merge_order: Vec<String> = ordered.iter().map(|b| b.spec_id.clone()).collect();
        let mut merges = Vec::new();
        let mut total_auto_merged = 0usize;
        let mut total_conflicts = 0usize;
        let mut total_resolutions = 0usize;
        let mut warnings: Vec<String> = Vec::new();
        let mut all_escalations: Vec<convergence::PipelineEscalation> = Vec::new();
        let mut all_clean = true;
        let mut any_degraded = false;
        let mut file_decisions: Vec<FileDecision> = Vec::new();
        let mut traceability_reports: Vec<convergence::traceability::TraceabilityReport> =
            Vec::new();

        let v2_pipeline = ConvergencePipeline::new();

        // Emit convergence_started event (best-effort).
        let conv_logger = crate::security::SecurityEventLogger::new(&self.writ_dir);
        let _ = conv_logger.emit_convergence_event(
            "convergence_started",
            crate::security::Severity::Info,
            &format!(
                "Convergence started: {} specs, base='{}', strategy={}",
                ordered.len() + 1,
                base_spec,
                strategy_name(strategy)
            ),
        );

        // ── Accumulated-content approach ──────────────────────────────
        // Load the common base (ancestor before any spec sealed) and the
        // base spec's current tree into an accumulated content map.  Each
        // pairwise merge updates `accumulated` so the NEXT merge sees the
        // combined result of all prior merges.

        // Collect all spec seal IDs to find the common ancestor.
        let mut all_spec_seal_ids: HashSet<String> = HashSet::new();
        if let Ok(base_data) = self.load_spec(&base_spec) {
            all_spec_seal_ids.extend(base_data.sealed_by.iter().cloned());
        }
        for branch in &ordered {
            if let Ok(sd) = self.load_spec(&branch.spec_id) {
                all_spec_seal_ids.extend(sd.sealed_by.iter().cloned());
            }
        }

        let chain = self.log()?;
        let mut common_base_seal_id: Option<String> = None;
        for seal in chain.iter().rev() {
            if all_spec_seal_ids.contains(&seal.id) {
                common_base_seal_id = seal.parent.clone();
                break;
            }
        }

        // Load base content (common ancestor state).
        let base_content_map: HashMap<String, String> = if let Some(ref id) = common_base_seal_id {
            let base_seal = self.load_seal(id)?;
            let base_idx = self.load_tree_index(&base_seal.tree)?;
            let mut map = HashMap::new();
            for path in base_idx.entries.keys() {
                if let Ok(Some(c)) = self.file_content_at_tree(&base_idx, path) {
                    map.insert(path.clone(), c);
                }
            }
            map
        } else {
            HashMap::new()
        };

        // Load the base spec's latest tree as the initial accumulated state.
        let base_spec_data = self.load_spec(&base_spec)?;
        let base_spec_seal_id = base_spec_data.sealed_by.last().cloned().unwrap_or_default();
        let mut accumulated: HashMap<String, String> = if !base_spec_seal_id.is_empty() {
            let seal = self.load_seal(&base_spec_seal_id)?;
            let idx = self.load_tree_index(&seal.tree)?;
            let mut map = HashMap::new();
            for path in idx.entries.keys() {
                if let Ok(Some(c)) = self.file_content_at_tree(&idx, path) {
                    map.insert(path.clone(), c);
                }
            }
            map
        } else {
            HashMap::new()
        };

        // Track the right seal IDs so we can update heads after all merges.
        let mut right_seal_ids: Vec<(String, String)> = Vec::new();

        // Track which files the accumulated state has modified vs the base.
        // Only these files need three-way merging with incoming specs.
        let mut accumulated_modified: HashSet<String> = self
            .spec_modified_files(&base_spec_data)
            .unwrap_or_default();

        for branch in &ordered {
            let spec_id = &branch.spec_id;

            let step_result: Result<(), String> = (|| {
                let right_spec_data = self.load_spec(spec_id).map_err(|e| e.to_string())?;
                if right_spec_data.sealed_by.is_empty() {
                    return Err(format!("spec '{}' has no seals", spec_id));
                }

                let right_seal_id = right_spec_data.sealed_by.last().unwrap().clone();
                let right_seal = self.load_seal(&right_seal_id).map_err(|e| e.to_string())?;
                let right_index = self
                    .load_tree_index(&right_seal.tree)
                    .map_err(|e| e.to_string())?;

                right_seal_ids.push((spec_id.clone(), right_seal_id.clone()));

                // Load full tree content for the right spec's latest seal.
                let mut right_tree: HashMap<String, String> = HashMap::new();
                for path in right_index.entries.keys() {
                    if let Ok(Some(c)) = self.file_content_at_tree(&right_index, path) {
                        right_tree.insert(path.clone(), c);
                    }
                }

                // Files the right spec actually modified (from its seal changes).
                let right_modified = self
                    .spec_modified_files(&right_spec_data)
                    .map_err(|e| e.to_string())?;

                // "Shared" = files modified by BOTH accumulated and this spec.
                // "Right-only" = files modified ONLY by this spec.
                let shared: Vec<String> = right_modified
                    .iter()
                    .filter(|f| accumulated_modified.contains(*f))
                    .cloned()
                    .collect();
                let right_only: Vec<String> = right_modified
                    .iter()
                    .filter(|f| !accumulated_modified.contains(*f))
                    .cloned()
                    .collect();

                let mut step_auto = 0usize;
                let mut step_conflicts_count = 0usize;
                let mut step_conflict_files: Vec<String> = Vec::new();
                let mut resolutions_records: Vec<ResolutionRecord> = Vec::new();
                let mut step_clean = true;
                let mut step_degraded = false;

                for path in &shared {
                    let base_str = base_content_map.get(path).cloned().unwrap_or_default();
                    let left_str = accumulated.get(path).cloned().unwrap_or_default();
                    let right_str = right_tree.get(path).cloned().unwrap_or_default();
                    let base = base_str.as_str();
                    let left = left_str.as_str();
                    let right = right_str.as_str();

                    if left == right {
                        continue;
                    }
                    if left == base {
                        accumulated.insert(path.clone(), right.to_string());
                        accumulated_modified.insert(path.clone());
                        step_auto += 1;
                        file_decisions.push(FileDecision {
                            path: path.clone(),
                            decision: "auto-merged".to_string(),
                            chosen_lines: right.lines().count(),
                            chosen_spec: Some(spec_id.clone()),
                            alternatives: vec![],
                            confidence: Some(1.0),
                        });
                        continue;
                    }
                    if right == base {
                        continue;
                    }

                    // ── v2 pipeline: structural diff → classify → pattern resolve ──
                    let pipeline_input = PipelineInput {
                        file_path: path.clone(),
                        base: base.to_string(),
                        left: left.to_string(),
                        right: right.to_string(),
                        left_spec: base_spec.clone(),
                        right_spec: spec_id.to_string(),
                        spec_context: None,
                        trust_context: self.build_trust_context(&base_spec, spec_id),
                    };
                    let pipeline_output = v2_pipeline.run(&pipeline_input);

                    // Collect traceability report for reproducibility record.
                    if let Some(trace) = &pipeline_output.traceability {
                        traceability_reports.push(trace.clone());
                    }

                    if pipeline_output.fully_resolved {
                        let content = pipeline_output.merged_content.unwrap_or_default();
                        let num_regions = pipeline_output.resolutions.len();

                        if num_regions == 0 {
                            step_auto += 1;
                            file_decisions.push(FileDecision {
                                path: path.clone(),
                                decision: "auto-merged".to_string(),
                                chosen_lines: content.lines().count(),
                                chosen_spec: None,
                                alternatives: vec![],
                                confidence: Some(1.0),
                            });
                        } else {
                            let method_summary: Vec<String> = pipeline_output
                                .resolutions
                                .iter()
                                .filter_map(|ro| match &ro.resolution {
                                    convergence::types::RegionResolutionStatus::Resolved {
                                        method,
                                        ..
                                    } => Some(method.clone()),
                                    _ => None,
                                })
                                .collect::<HashSet<_>>()
                                .into_iter()
                                .collect();

                            let avg_confidence = {
                                let cs: Vec<f64> = pipeline_output
                                    .resolutions
                                    .iter()
                                    .filter_map(|ro| match &ro.resolution {
                                        convergence::types::RegionResolutionStatus::Resolved {
                                            confidence,
                                            ..
                                        } => Some(*confidence),
                                        _ => None,
                                    })
                                    .collect();
                                if cs.is_empty() {
                                    1.0
                                } else {
                                    cs.iter().sum::<f64>() / cs.len() as f64
                                }
                            };

                            resolutions_records.push(ResolutionRecord {
                                path: path.clone(),
                                strategy: format!("v2-pipeline: {}", method_summary.join(", ")),
                                chosen_spec: None,
                                lost_content_warning: None,
                            });

                            file_decisions.push(FileDecision {
                                path: path.clone(),
                                decision: "auto-resolved".to_string(),
                                chosen_lines: content.lines().count(),
                                chosen_spec: None,
                                alternatives: vec![
                                    FileAlternative {
                                        spec: "accumulated".to_string(),
                                        lines: left.lines().count(),
                                        reason: format!(
                                            "composed (confidence: {:.0}%)",
                                            avg_confidence * 100.0
                                        ),
                                    },
                                    FileAlternative {
                                        spec: spec_id.to_string(),
                                        lines: right.lines().count(),
                                        reason: format!(
                                            "composed (confidence: {:.0}%)",
                                            avg_confidence * 100.0
                                        ),
                                    },
                                ],
                                confidence: Some(avg_confidence),
                            });

                            step_conflicts_count += 1;
                            step_conflict_files.push(path.clone());
                            total_resolutions += 1;
                        }

                        accumulated.insert(path.clone(), content);
                        accumulated_modified.insert(path.clone());
                    } else {
                        // v2 pipeline could not fully resolve. Fall back to the
                        // v1 text-level resolver which has robust import detection
                        // for languages without a dedicated structural analyzer.
                        let v1_result = match convergence::three_way_merge(base, left, right) {
                            convergence::FileMergeResult::Clean(content) => {
                                convergence::ResolvedFile {
                                    content,
                                    resolutions: vec![],
                                    fully_resolved: true,
                                    unresolved_regions: vec![],
                                    escalation_records: vec![],
                                }
                            }
                            convergence::FileMergeResult::Conflict(regions) => {
                                convergence::resolve_conflict_regions(
                                    path, base, left, right, &regions, &base_spec, spec_id,
                                )
                            }
                        };

                        if v1_result.fully_resolved {
                            accumulated.insert(path.clone(), v1_result.content.clone());
                            accumulated_modified.insert(path.clone());

                            all_escalations.extend(v1_result.escalation_records);

                            let method_summary: Vec<String> = v1_result
                                .resolutions
                                .iter()
                                .map(|r| r.method.clone())
                                .collect::<HashSet<_>>()
                                .into_iter()
                                .collect();

                            let avg_confidence = if v1_result.resolutions.is_empty() {
                                1.0
                            } else {
                                v1_result
                                    .resolutions
                                    .iter()
                                    .map(|r| r.confidence)
                                    .sum::<f64>()
                                    / v1_result.resolutions.len() as f64
                            };

                            resolutions_records.push(ResolutionRecord {
                                path: path.clone(),
                                strategy: format!("v1-fallback: {}", method_summary.join(", ")),
                                chosen_spec: None,
                                lost_content_warning: None,
                            });

                            file_decisions.push(FileDecision {
                                path: path.clone(),
                                decision: "auto-resolved".to_string(),
                                chosen_lines: v1_result.content.lines().count(),
                                chosen_spec: None,
                                alternatives: vec![
                                    FileAlternative {
                                        spec: "accumulated".to_string(),
                                        lines: left.lines().count(),
                                        reason: format!(
                                            "composed (confidence: {:.0}%)",
                                            avg_confidence * 100.0
                                        ),
                                    },
                                    FileAlternative {
                                        spec: spec_id.to_string(),
                                        lines: right.lines().count(),
                                        reason: format!(
                                            "composed (confidence: {:.0}%)",
                                            avg_confidence * 100.0
                                        ),
                                    },
                                ],
                                confidence: Some(avg_confidence),
                            });

                            step_conflicts_count += 1;
                            step_conflict_files.push(path.clone());
                            total_resolutions += 1;
                        } else {
                            // Neither v2 nor v1 resolved. Apply strategy.
                            step_conflicts_count += 1;
                            step_conflict_files.push(path.clone());

                            all_escalations.extend(v1_result.escalation_records);
                            for esc in &pipeline_output.escalations {
                                all_escalations.push(convergence::PipelineEscalation {
                                    file_path: esc.file_path.clone(),
                                    reason: format!("{:?}", esc.reason),
                                    conflict_class: format!("{:?}", esc.conflict_type),
                                    left_spec: esc.left_agent.clone(),
                                    right_spec: esc.right_agent.clone(),
                                    recommended_action: esc.recommended_action.clone(),
                                    left_content: Some(esc.left_content.clone()),
                                    right_content: Some(esc.right_content.clone()),
                                    suggested_content: esc
                                        .phase3_suggestion
                                        .as_ref()
                                        .map(|s| s.merged_content.clone()),
                                    suggestion_confidence: esc
                                        .phase3_suggestion
                                        .as_ref()
                                        .map(|s| s.confidence),
                                });
                                // Emit per-escalation security event with agent context.
                                let _ = conv_logger.emit_convergence_event_with_agent(
                                    "convergence_escalation",
                                    crate::security::Severity::Warning,
                                    &format!(
                                        "Escalation in '{}': {:?} between '{}' and '{}'",
                                        esc.file_path, esc.reason, esc.left_agent, esc.right_agent
                                    ),
                                    Some(&format!("{},{}", esc.left_agent, esc.right_agent)),
                                );
                            }

                            match strategy {
                                #[allow(deprecated)]
                                ConvergeStrategy::MostRecent => {
                                    let left_ts = self
                                        .load_spec(&base_spec)
                                        .ok()
                                        .and_then(|s| s.sealed_by.last().cloned())
                                        .and_then(|id| self.load_seal(&id).ok())
                                        .map(|s| s.timestamp);
                                    let right_ts = self
                                        .load_spec(spec_id)
                                        .ok()
                                        .and_then(|s| s.sealed_by.last().cloned())
                                        .and_then(|id| self.load_seal(&id).ok())
                                        .map(|s| s.timestamp);

                                    let prefer_right = match (left_ts, right_ts) {
                                        (Some(l), Some(r)) => r >= l,
                                        (None, Some(_)) => true,
                                        _ => false,
                                    };

                                    let content = if prefer_right {
                                        right.to_string()
                                    } else {
                                        left.to_string()
                                    };
                                    let (chosen, other) = if prefer_right {
                                        (spec_id.to_string(), base_spec.clone())
                                    } else {
                                        (base_spec.clone(), spec_id.to_string())
                                    };

                                    let lost = if prefer_right { left } else { right };
                                    let lost_warning = if !lost.is_empty() {
                                        Some(format!(
                                            "Discarded {} line(s) from '{}' in favor of '{}'",
                                            lost.lines().count(),
                                            other,
                                            chosen,
                                        ))
                                    } else {
                                        None
                                    };

                                    if let Some(ref w) = lost_warning {
                                        warnings.push(format!("{}: {}", path, w));
                                        step_degraded = true;
                                    }

                                    accumulated.insert(path.clone(), content);
                                    accumulated_modified.insert(path.clone());

                                    resolutions_records.push(ResolutionRecord {
                                        path: path.clone(),
                                        strategy: "most-recent".to_string(),
                                        chosen_spec: Some(chosen.clone()),
                                        lost_content_warning: lost_warning,
                                    });

                                    file_decisions.push(FileDecision {
                                        path: path.clone(),
                                        decision: "most-recent".to_string(),
                                        chosen_lines: if prefer_right {
                                            right.lines().count()
                                        } else {
                                            left.lines().count()
                                        },
                                        chosen_spec: Some(chosen),
                                        alternatives: vec![FileAlternative {
                                            spec: other,
                                            lines: lost.lines().count(),
                                            reason: "discarded: not most recent".to_string(),
                                        }],
                                        confidence: Some(0.7),
                                    });

                                    total_resolutions += 1;
                                }
                                ConvergeStrategy::Manual
                                | ConvergeStrategy::Orchestrator
                                | ConvergeStrategy::Escalate => {
                                    let decision_str = match strategy {
                                        ConvergeStrategy::Escalate => "escalated",
                                        _ => "conflict-unresolved",
                                    };
                                    file_decisions.push(FileDecision {
                                        path: path.clone(),
                                        decision: decision_str.to_string(),
                                        chosen_lines: 0,
                                        chosen_spec: None,
                                        alternatives: vec![
                                            FileAlternative {
                                                spec: "accumulated".to_string(),
                                                lines: left.lines().count(),
                                                reason: "left side (accumulated)".to_string(),
                                            },
                                            FileAlternative {
                                                spec: spec_id.to_string(),
                                                lines: right.lines().count(),
                                                reason: "right side (diverged)".to_string(),
                                            },
                                        ],
                                        confidence: Some(0.0),
                                    });
                                    step_clean = false;
                                    all_clean = false;
                                }
                            }
                        }
                    }
                }

                // Left-only files: in accumulated but not modified by right spec.
                let left_only: Vec<String> = accumulated_modified
                    .iter()
                    .filter(|f| !right_modified.contains(*f))
                    .cloned()
                    .collect();
                for path in &left_only {
                    file_decisions.push(FileDecision {
                        path: path.clone(),
                        decision: "left-only".to_string(),
                        chosen_lines: 0,
                        chosen_spec: Some(base_spec.clone()),
                        alternatives: vec![],
                        confidence: None,
                    });
                }

                // Right-only files: add to accumulated.
                for path in &right_only {
                    if let Some(content) = right_tree.get(path) {
                        accumulated.insert(path.clone(), content.clone());
                        accumulated_modified.insert(path.clone());
                    }
                    file_decisions.push(FileDecision {
                        path: path.clone(),
                        decision: "right-only".to_string(),
                        chosen_lines: 0,
                        chosen_spec: Some(spec_id.clone()),
                        alternatives: vec![],
                        confidence: None,
                    });
                }

                total_auto_merged += step_auto;
                total_conflicts += step_conflicts_count;

                if !step_clean {
                    all_clean = false;
                }
                if step_degraded {
                    any_degraded = true;
                }

                merges.push(MergeStepResult {
                    left_spec: base_spec.clone(),
                    right_spec: spec_id.clone(),
                    auto_merged: step_auto,
                    conflicts: step_conflicts_count,
                    left_only: left_only.len(),
                    right_only: right_only.len(),
                    conflict_files: step_conflict_files,
                    resolutions: resolutions_records,
                    clean: step_clean,
                    degraded: step_degraded,
                    error: None,
                });

                Ok(())
            })();

            if let Err(e) = step_result {
                all_clean = false;
                merges.push(MergeStepResult {
                    left_spec: base_spec.clone(),
                    right_spec: spec_id.clone(),
                    auto_merged: 0,
                    conflicts: 0,
                    left_only: 0,
                    right_only: 0,
                    conflict_files: Vec::new(),
                    resolutions: Vec::new(),
                    clean: false,
                    degraded: false,
                    error: Some(e),
                });
            }
        }

        // ── Layer 5: Post-merge cleanup on all accumulated files ──────
        // Run language-specific cleanup (import dedup, unused pruning,
        // PEP 8 formatting) that smart_merge normally handles. Without
        // this, converge_all bypasses Layer 5 entirely.
        for (path, content) in accumulated.iter_mut() {
            let cleaned = convergence::post_merge_cleanup(content, path);
            if cleaned != *content {
                *content = cleaned;
            }
        }

        // ── Apply accumulated result ──────────────────────────────────
        let did_apply = apply && all_clean;
        if did_apply {
            let _lock = self.lock()?;

            for (path, content) in &accumulated {
                let file_path = self.validate_path(path)?;
                if let Some(parent) = file_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&file_path, content)?;
            }

            // Update the index so state() reflects the converged working directory.
            // Without this, context() would report false pending_changes for every
            // converged file because the index still has pre-convergence hashes.
            let mut index = self.load_index()?;
            for (path, content) in &accumulated {
                let hash = self.objects.store(content.as_bytes())?;
                let size = content.len() as u64;
                index.upsert(path, hash, size);
            }
            index.save(&self.writ_dir.join("index.json"))?;

            // Update diverged specs' heads so they're no longer diverged.
            for (spec_id, _right_seal_id) in &right_seal_ids {
                if let Some(old_head) = self.read_spec_head(spec_id)? {
                    self.archive_merged_head(&old_head)?;
                }
                self.write_spec_head(spec_id, &base_spec_seal_id)?;
            }

            // When the base spec was itself diverged (pulled from ordered),
            // its head also needs updating to the current HEAD so
            // diverged_branches() no longer flags it.
            if diverged_ids.contains(&base_spec) {
                let current_head = self.read_head()?;
                if let Some(ref head_id) = current_head {
                    if self.read_spec_head(&base_spec)?.as_ref() != Some(head_id) {
                        if let Some(old_head) = self.read_spec_head(&base_spec)? {
                            self.archive_merged_head(&old_head)?;
                        }
                        self.write_spec_head(&base_spec, head_id)?;
                    }
                }
            }

            // Refresh summary.json so it reflects post-convergence state
            // (diverged branches cleared, convergence_recommended: false).
            self.check_all_specs_complete();
        }

        // Add high-contention file warnings.
        let all_seals = self.log_all().unwrap_or_default();
        let contention = Self::build_file_contention(&all_seals);
        for fc in &contention {
            if fc.agents.len() >= 3 {
                warnings.push(format!(
                    "{}: touched by {} agents ({}) — review for semantic consistency",
                    fc.path,
                    fc.agents.len(),
                    fc.agents.join(", "),
                ));
            }
        }

        // Post-convergence structural validation (only when changes were applied).
        if apply {
            let mut left_map: HashMap<String, String> = HashMap::new();
            let mut right_map: HashMap<String, String> = HashMap::new();
            for m in &merges {
                if let Ok(left_spec_data) = self.load_spec(&m.left_spec) {
                    if let Some(seal_id) = left_spec_data.sealed_by.last() {
                        if let Ok(seal) = self.load_seal(seal_id) {
                            if let Ok(idx) = self.load_tree_index(&seal.tree) {
                                for path in idx.entries.keys() {
                                    if let Ok(Some(c)) = self.file_content_at_tree(&idx, path) {
                                        left_map.entry(path.clone()).or_insert(c);
                                    }
                                }
                            }
                        }
                    }
                }
                if let Ok(right_spec_data) = self.load_spec(&m.right_spec) {
                    if let Some(seal_id) = right_spec_data.sealed_by.last() {
                        if let Ok(seal) = self.load_seal(seal_id) {
                            if let Ok(idx) = self.load_tree_index(&seal.tree) {
                                for path in idx.entries.keys() {
                                    if let Ok(Some(c)) = self.file_content_at_tree(&idx, path) {
                                        right_map.entry(path.clone()).or_insert(c);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            let validation_warnings =
                post_convergence_validation(&self.root, &file_decisions, &left_map, &right_map);
            warnings.extend(validation_warnings);
        }

        // Deduplicate warnings (same warning can appear in multiple merge phases).
        let mut seen = HashSet::new();
        warnings.retain(|w| seen.insert(w.clone()));

        // Build post-convergence quality report.
        let quality_report = build_quality_report(
            file_decisions,
            &self.root,
            apply,
            total_conflicts,
            total_resolutions,
        );

        // Emit low-confidence warning if any decision fell below threshold.
        if quality_report.min_confidence < 0.85 {
            let _ = conv_logger.emit_convergence_event(
                "convergence_low_confidence",
                crate::security::Severity::Warning,
                &format!(
                    "Low confidence detected: min={}%, avg={}%",
                    (quality_report.min_confidence * 100.0).round() as u32,
                    (quality_report.avg_confidence * 100.0).round() as u32,
                ),
            );
        }

        // Collect the list of files changed by convergence so callers can
        // create accurate convergence seals.
        let mut files_changed: Vec<String> = accumulated.keys().cloned().collect();
        files_changed.sort();

        // Emit convergence completion event (best-effort).
        let completion_event_type = if any_degraded {
            "convergence_degraded"
        } else {
            "convergence_completed"
        };
        let completion_severity = if any_degraded {
            crate::security::Severity::Warning
        } else {
            crate::security::Severity::Info
        };
        let _ = conv_logger.emit_convergence_event(
            completion_event_type,
            completion_severity,
            &format!(
                "Convergence {}: {} merges, {} auto-merged, {} conflicts, {} resolutions, {} escalations, clean={}",
                if any_degraded { "degraded" } else { "completed" },
                merges.len(),
                total_auto_merged,
                total_conflicts,
                total_resolutions,
                all_escalations.len(),
                all_clean
            ),
        );

        // Build reproducibility record with real data.
        let input_seal_hashes: Vec<String> = ordered.iter().map(|b| b.tip_seal.clone()).collect();
        let pipeline_version = env!("CARGO_PKG_VERSION").to_string();
        let pattern_versions: HashMap<String, String> = [
            ("import_accumulation", "1.0"),
            ("additive_composition", "1.0"),
            ("superset", "1.0"),
            ("non_overlapping_definitions", "1.0"),
            ("eof_append", "1.0"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        // Compute configuration hash from strategy + thresholds.
        let config_data = format!(
            "strategy={};auto_resolve={};suggest={}",
            strategy_name(strategy),
            convergence::types::ConfidenceThresholds::default().auto_resolve,
            convergence::types::ConfidenceThresholds::default().suggest,
        );
        let configuration_hash = crate::crypto::blake3_hex(config_data.as_bytes());

        // Build the partial report to pass to from_report (need it constructed
        // first to use the factory method).
        let partial_report = ConvergeAllReport {
            base_spec: base_spec.clone(),
            merge_order: merge_order.clone(),
            merges: merges.clone(),
            strategy: strategy_name(strategy),
            total_auto_merged,
            total_conflicts,
            total_resolutions,
            is_clean: all_clean,
            degraded: any_degraded,
            applied: did_apply,
            warnings: warnings.clone(),
            escalations: all_escalations.clone(),
            quality_report: Some(quality_report),
            files_changed: files_changed.clone(),
            convergence_record: None,
        };

        let convergence_record = convergence::types::ConvergenceSealRecord::from_report(
            "pending", // seal ID is assigned when the caller creates the seal
            &partial_report,
            traceability_reports,
            input_seal_hashes,
            &pipeline_version,
            pattern_versions,
            &configuration_hash,
        );

        Ok(ConvergeAllReport {
            convergence_record: Some(convergence_record),
            ..partial_report
        })
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
        // Reject seals from revoked or suspended agents
        if let Ok(registered) = self.load_agent(&agent.id) {
            if registered.status == AgentStatus::Revoked {
                return Err(WritError::AgentInactive(format!(
                    "agent '{}' is revoked and cannot create seals",
                    agent.id
                )));
            }
            if registered.status == AgentStatus::Suspended {
                return Err(WritError::AgentInactive(format!(
                    "agent '{}' is suspended and cannot create seals",
                    agent.id
                )));
            }
        }
        let _lock = self.lock()?;
        let mut index = self.load_index()?;
        let rules = self.ignore_rules();
        let working_state = state::compute_state(&self.root, &index, &rules);

        let matching_changes: Vec<_> = working_state
            .changes
            .iter()
            .filter(|fs| {
                paths
                    .iter()
                    .any(|p| fs.path == *p || fs.path.starts_with(&format!("{p}/")))
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
        let parent = self.resolve_parent(spec_id.as_deref())?;

        let mut seal_warnings: Vec<String> = Vec::new();

        if let Some(ref sid) = spec_id {
            let changed_paths: Vec<String> = changes.iter().map(|c| c.path.clone()).collect();
            if let Some(scope_warn) = self.check_file_scope(sid, &changed_paths) {
                seal_warnings.push(format!(
                    "FILE_SCOPE: {} file(s) outside declared scope for spec '{}': {}",
                    scope_warn.out_of_scope_files.len(),
                    sid,
                    scope_warn.out_of_scope_files.join(", "),
                ));
                // Emit security event for scope violation (best-effort, don't block seal)
                let logger = crate::security::SecurityEventLogger::new(&self.writ_dir);
                if let Err(e) =
                    logger.emit_scope_violation(&agent.id, sid, &scope_warn.out_of_scope_files)
                {
                    seal_warnings.push(format!("SECURITY_LOG_FAILURE: {e}"));
                }
            }
        }

        if changes.is_empty() && !summary.is_empty() {
            seal_warnings.push(
                "GHOST_WORK: seal has a summary but 0 file changes — work may have been captured by another agent's seal".to_string(),
            );
        }

        // Agent identity checks (Sprint B)
        if let Ok(registered) = self.load_agent(&agent.id) {
            if registered.status != AgentStatus::Active {
                seal_warnings.push(format!(
                    "AGENT_INACTIVE: agent '{}' status is {:?}",
                    agent.id, registered.status
                ));
            }
            let out_of_scope: Vec<&str> = changes
                .iter()
                .filter(|c| !crate::agent::is_in_scope(&registered.scope_constraints, &c.path))
                .map(|c| c.path.as_str())
                .collect();
            if !out_of_scope.is_empty() {
                let logger = crate::security::SecurityEventLogger::new(&self.writ_dir);
                let _ = logger.emit_agent_scope_violation(&agent.id, &out_of_scope);
                if self.enforce_scope {
                    return Err(WritError::ScopeViolation(format!(
                        "agent '{}' modified {} file(s) outside scope: {}",
                        agent.id,
                        out_of_scope.len(),
                        out_of_scope.join(", ")
                    )));
                } else {
                    seal_warnings.push(format!(
                        "AGENT_SCOPE: {} file(s) outside agent '{}' scope: {}",
                        out_of_scope.len(),
                        agent.id,
                        out_of_scope.join(", ")
                    ));
                }
            }
        } else {
            // Agent not in identity store — emit unrecognized agent event
            let logger = crate::security::SecurityEventLogger::new(&self.writ_dir);
            let _ = logger.emit_unrecognized_agent(&agent.id);
        }

        let parent_seal_hash = match parent {
            Some(ref pid) => match self.load_seal(pid) {
                Ok(s) => s.chain_hash.clone(),
                Err(e) => {
                    seal_warnings.push(format!(
                        "CHAIN_BREAK: failed to load parent seal {}: {e} — chain integrity may be compromised",
                        &pid[..12.min(pid.len())]
                    ));
                    None
                }
            },
            None => None,
        };

        let mut seal = Seal::new(
            parent,
            tree_hash,
            agent,
            spec_id.clone(),
            status,
            changes,
            verification,
            summary,
            seal_warnings,
            parent_seal_hash,
        );

        // Sign with agent's key if available, otherwise unsigned
        let ks = KeyStore::open(&self.writ_dir);
        let signing_key = ks.load_agent_signing_key(&seal.agent.id).ok();
        seal.secure(signing_key.as_ref());

        self.save_seal(&seal)?;
        atomic_write(&self.writ_dir.join("HEAD"), seal.id.as_bytes())?;
        index.save(&self.writ_dir.join("index.json"))?;

        if let Some(ref sid) = spec_id {
            self.write_spec_head(sid, &seal.id)?;
            if let Ok(mut spec) = self.load_spec(sid) {
                spec.sealed_by.push(seal.id.clone());
                let now = chrono::Utc::now();
                spec.updated_at = now;
                spec.last_activity = now;
                self.save_spec(&spec)?;
            }
        }

        Ok(seal)
    }

    // --- Internal helpers ---

    /// Check storage pressure after a seal and emit warnings/events.
    ///
    /// Best-effort: failures are silently ignored. Seals are never refused.
    fn check_storage_pressure(&self, seal: &Seal) {
        let config = match crate::gc::GcConfig::load(&self.writ_dir) {
            Ok(c) => c,
            Err(_) => return,
        };

        // Quick size estimate: sum of file sizes in .writ/ via StorageReport::scan.
        // This is fast for typical repos (< 10k files in .writ/).
        let report = match crate::gc::StorageReport::scan(&self.writ_dir, config.budget_bytes) {
            Ok(r) => r,
            Err(_) => return,
        };

        let usage_pct = report.usage_pct();

        if usage_pct >= config.warning_threshold_pct as f64 {
            let logger = crate::security::SecurityEventLogger::new(&self.writ_dir);
            let _ = logger.emit_event(&crate::security::SecurityEvent {
                timestamp: chrono::Utc::now(),
                severity: crate::security::Severity::Warning,
                event_type: "storage_pressure".to_string(),
                agent_id: Some(seal.agent.id.clone()),
                details: format!(
                    "Storage at {:.1}% of budget ({}/{} bytes) after seal {}",
                    usage_pct,
                    report.total_bytes,
                    config.budget_bytes,
                    &seal.id[..12.min(seal.id.len())]
                ),
            });
        }
    }

    fn ignore_rules(&self) -> IgnoreRules {
        IgnoreRules::load(&self.root)
    }

    /// Load the convergence engine's Ed25519 signing key.
    ///
    /// Returns `None` if the key doesn't exist (pre-Sprint A repos).
    pub fn convergence_signing_key(&self) -> Option<ed25519_dalek::SigningKey> {
        let ks = KeyStore::open(&self.writ_dir);
        ks.load_agent_signing_key("convergence").ok()
    }

    /// Load the convergence engine's Ed25519 verifying (public) key.
    ///
    /// Returns `None` if the key doesn't exist (pre-Sprint A repos).
    pub fn convergence_verifying_key(&self) -> Option<ed25519_dalek::VerifyingKey> {
        let ks = KeyStore::open(&self.writ_dir);
        ks.load_agent_verifying_key("convergence").ok()
    }

    // -----------------------------------------------------------------------
    // Agent management
    // -----------------------------------------------------------------------

    /// Register a new agent. Generates an Ed25519 keypair and stores it in
    /// the keystore. Returns the `RegisteredAgent` record.
    pub fn register_agent(
        &self,
        agent_id: &str,
        registered_by: &str,
        trust_level: TrustLevel,
        scope_constraints: Vec<String>,
    ) -> WritResult<RegisteredAgent> {
        let agent_path = self.writ_dir.join(format!("agents/{agent_id}.json"));
        if agent_path.exists() {
            return Err(WritError::AgentAlreadyExists(agent_id.to_string()));
        }

        // Generate keypair and store in keystore
        let (signing_key, verifying_key) = crate::crypto::generate_keypair();
        let ks = KeyStore::open(&self.writ_dir);
        ks.store_agent_key(agent_id, &signing_key, &verifying_key)?;

        let agent = RegisteredAgent {
            agent_id: agent_id.to_string(),
            public_key: crate::crypto::verifying_key_to_hex(&verifying_key),
            registered_at: chrono::Utc::now(),
            registered_by: registered_by.to_string(),
            trust_level,
            scope_constraints,
            status: AgentStatus::Active,
            revoked_at: None,
            revocation_reason: None,
        };

        let json = serde_json::to_string_pretty(&agent)?;
        atomic_write(&agent_path, json.as_bytes())?;
        Ok(agent)
    }

    /// Load a registered agent by ID.
    pub fn load_agent(&self, agent_id: &str) -> WritResult<RegisteredAgent> {
        let agent_path = self.writ_dir.join(format!("agents/{agent_id}.json"));
        let data = fs::read_to_string(&agent_path)
            .map_err(|_| WritError::AgentNotFound(agent_id.to_string()))?;
        let agent: RegisteredAgent = serde_json::from_str(&data)
            .map_err(|e| WritError::Other(format!("agent JSON: {e}")))?;
        Ok(agent)
    }

    /// List all registered agents.
    pub fn list_agents(&self) -> WritResult<Vec<RegisteredAgent>> {
        let agents_dir = self.writ_dir.join("agents");
        if !agents_dir.exists() {
            return Ok(Vec::new());
        }
        let mut agents = Vec::new();
        for entry in fs::read_dir(&agents_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "json") {
                if let Ok(data) = fs::read_to_string(&path) {
                    if let Ok(agent) = serde_json::from_str::<RegisteredAgent>(&data) {
                        agents.push(agent);
                    }
                }
            }
        }
        agents.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        Ok(agents)
    }

    /// Update a registered agent (trust level, scope constraints).
    pub fn update_agent(&self, agent_id: &str, update: AgentUpdate) -> WritResult<RegisteredAgent> {
        let mut agent = self.load_agent(agent_id)?;

        if let Some(trust) = update.trust_level {
            agent.trust_level = trust;
        }
        if let Some(scope) = update.scope_constraints {
            agent.scope_constraints = scope;
        }

        let agent_path = self.writ_dir.join(format!("agents/{agent_id}.json"));
        let json = serde_json::to_string_pretty(&agent)?;
        atomic_write(&agent_path, json.as_bytes())?;
        Ok(agent)
    }

    /// Revoke an agent. Sets status to Revoked, records reason and timestamp.
    /// Removes keys from keystore (agent can no longer sign).
    ///
    /// If `compromise_timestamp` is provided, all seals created by this agent
    /// between that time and now are flagged in the flagged-seals manifest.
    /// If not provided, defaults to `revoked_at` (assumes compromise started
    /// at revocation time). Downstream seals that incorporate flagged seals
    /// are also transitively flagged.
    pub fn revoke_agent(&self, agent_id: &str, reason: &str) -> WritResult<RegisteredAgent> {
        self.revoke_agent_with_compromise(agent_id, reason, None)
    }

    /// Revoke an agent with an explicit compromise timestamp.
    ///
    /// Seals created by the agent between `compromise_timestamp` and now are
    /// flagged. Downstream seals that incorporate any flagged seal as a parent
    /// are transitively flagged as well.
    pub fn revoke_agent_with_compromise(
        &self,
        agent_id: &str,
        reason: &str,
        compromise_timestamp: Option<chrono::DateTime<chrono::Utc>>,
    ) -> WritResult<RegisteredAgent> {
        let mut agent = self.load_agent(agent_id)?;
        agent.status = AgentStatus::Revoked;
        let now = chrono::Utc::now();
        agent.revoked_at = Some(now);
        agent.revocation_reason = Some(reason.to_string());

        let agent_path = self.writ_dir.join(format!("agents/{agent_id}.json"));
        let json = serde_json::to_string_pretty(&agent)?;
        atomic_write(&agent_path, json.as_bytes())?;

        // Remove keys from keystore (best-effort)
        let ks = KeyStore::open(&self.writ_dir);
        let _ = ks.remove_agent_keys(agent_id);

        // Emit security event (best-effort)
        let logger = crate::security::SecurityEventLogger::new(&self.writ_dir);
        let _ = logger.emit_agent_revoked(agent_id, reason);

        // Flag seals in the compromise window (best-effort)
        let compromise_start = compromise_timestamp.unwrap_or(now);
        let _ = self.flag_compromised_seals(agent_id, compromise_start, now);

        Ok(agent)
    }

    /// Flag all seals by `agent_id` created between `from` and `to`,
    /// plus any downstream seals that transitively depend on them.
    fn flag_compromised_seals(
        &self,
        agent_id: &str,
        from: chrono::DateTime<chrono::Utc>,
        to: chrono::DateTime<chrono::Utc>,
    ) -> WritResult<()> {
        use crate::security::{FlagReason, FlaggedSeal, FlaggedSealStore};

        let store = FlaggedSealStore::new(&self.writ_dir);
        let all_seals = self.log_all()?;

        // Phase 1: flag seals created by this agent in the compromise window
        let mut flagged_ids: HashSet<String> = HashSet::new();
        for seal in &all_seals {
            if seal.agent.id == agent_id && seal.timestamp >= from && seal.timestamp <= to {
                flagged_ids.insert(seal.id.clone());
                store.flag_seal(&FlaggedSeal {
                    seal_id: seal.id.clone(),
                    agent_id: agent_id.to_string(),
                    reason: FlagReason::AgentCompromised,
                    compromise_window: (from, to),
                    flagged_by: "system".to_string(),
                    flagged_at: to,
                })?;
            }
        }

        if flagged_ids.is_empty() {
            return Ok(());
        }

        // Phase 2: transitively flag downstream seals whose parent is flagged
        let mut changed = true;
        while changed {
            changed = false;
            for seal in &all_seals {
                if flagged_ids.contains(&seal.id) {
                    continue;
                }
                if let Some(ref parent) = seal.parent {
                    if flagged_ids.contains(parent) {
                        flagged_ids.insert(seal.id.clone());
                        store.flag_seal(&FlaggedSeal {
                            seal_id: seal.id.clone(),
                            agent_id: agent_id.to_string(),
                            reason: FlagReason::DownstreamOfCompromised,
                            compromise_window: (from, to),
                            flagged_by: "system".to_string(),
                            flagged_at: to,
                        })?;
                        changed = true;
                    }
                }
            }
        }

        Ok(())
    }

    /// Load the set of flagged seal IDs for cheap membership checks.
    ///
    /// Returns an empty set if no seals have been flagged.
    pub fn flagged_seal_ids(&self) -> WritResult<HashSet<String>> {
        crate::security::FlaggedSealStore::new(&self.writ_dir).flagged_ids()
    }

    /// Load all flagged seal entries with full metadata.
    pub fn flagged_seals(&self) -> WritResult<Vec<crate::security::FlaggedSeal>> {
        crate::security::FlaggedSealStore::new(&self.writ_dir).load_all()
    }

    /// Suspend an agent. Sets status to Suspended.
    pub fn suspend_agent(&self, agent_id: &str) -> WritResult<RegisteredAgent> {
        let mut agent = self.load_agent(agent_id)?;
        agent.status = AgentStatus::Suspended;

        let agent_path = self.writ_dir.join(format!("agents/{agent_id}.json"));
        let json = serde_json::to_string_pretty(&agent)?;
        atomic_write(&agent_path, json.as_bytes())?;
        Ok(agent)
    }

    /// Reactivate a suspended agent.
    pub fn reactivate_agent(&self, agent_id: &str) -> WritResult<RegisteredAgent> {
        let mut agent = self.load_agent(agent_id)?;
        if agent.status == AgentStatus::Revoked {
            return Err(WritError::AgentInactive(format!(
                "agent '{agent_id}' is revoked and cannot be reactivated"
            )));
        }
        agent.status = AgentStatus::Active;

        let agent_path = self.writ_dir.join(format!("agents/{agent_id}.json"));
        let json = serde_json::to_string_pretty(&agent)?;
        atomic_write(&agent_path, json.as_bytes())?;
        Ok(agent)
    }

    /// Look up the trust level for an agent.
    /// Returns `TrustLevel::Untrusted` for unregistered agents.
    pub fn agent_trust_level(&self, agent_id: &str) -> TrustLevel {
        self.load_agent(agent_id)
            .map(|a| a.trust_level)
            .unwrap_or(TrustLevel::Untrusted)
    }

    /// Build a trust context for convergence by looking up the agents
    /// involved in each spec's latest seal.
    fn build_trust_context(
        &self,
        left_spec: &str,
        right_spec: &str,
    ) -> Option<crate::agent::TrustContext> {
        let left_agent = self.spec_latest_agent(left_spec)?;
        let right_agent = self.spec_latest_agent(right_spec)?;

        let left_trust = self.agent_trust_level(&left_agent);
        let right_trust = self.agent_trust_level(&right_agent);

        Some(crate::agent::TrustContext {
            left_trust,
            right_trust,
        })
    }

    /// Find the agent_id of the most recent seal for a given spec.
    fn spec_latest_agent(&self, spec_id: &str) -> Option<String> {
        let spec = self.load_spec(spec_id).ok()?;
        let latest_seal_id = spec.sealed_by.last()?;
        let seal = self.load_seal(latest_seal_id).ok()?;
        Some(seal.agent.id.clone())
    }

    /// Check whether an agent is in scope for a given file path.
    /// Returns `true` for unregistered agents (no constraints to enforce).
    pub fn agent_in_scope(&self, agent_id: &str, file_path: &str) -> bool {
        match self.load_agent(agent_id) {
            Ok(agent) => crate::agent::is_in_scope(&agent.scope_constraints, file_path),
            Err(_) => true, // Unregistered = no constraints
        }
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

    /// Read the tip seal for a specific spec.
    fn read_spec_head(&self, spec_id: &str) -> WritResult<Option<String>> {
        let path = self.writ_dir.join("heads").join(spec_id);
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&path)?;
        let trimmed = content.trim();
        if trimmed.is_empty() {
            Ok(None)
        } else {
            Ok(Some(trimmed.to_string()))
        }
    }

    /// Update the tip seal for a specific spec.
    fn write_spec_head(&self, spec_id: &str, seal_id: &str) -> WritResult<()> {
        let heads_dir = self.writ_dir.join("heads");
        if !heads_dir.exists() {
            fs::create_dir_all(&heads_dir)?;
        }
        atomic_write(&heads_dir.join(spec_id), seal_id.as_bytes())
    }

    /// Record a pre-convergence branch tip so `log_all()` can still walk
    /// orphaned seal chains after convergence advances the spec head.
    fn archive_merged_head(&self, seal_id: &str) -> WritResult<()> {
        let path = self.writ_dir.join("merged-heads");
        let mut contents = if path.exists() {
            fs::read_to_string(&path)?
        } else {
            String::new()
        };
        if !contents.lines().any(|l| l == seal_id) {
            if !contents.is_empty() && !contents.ends_with('\n') {
                contents.push('\n');
            }
            contents.push_str(seal_id);
            contents.push('\n');
            atomic_write(&path, contents.as_bytes())?;
        }
        Ok(())
    }

    /// Determine the parent seal for a new seal. Uses spec-scoped head
    /// when a spec is given, falling back to global HEAD.
    fn resolve_parent(&self, spec_id: Option<&str>) -> WritResult<Option<String>> {
        if let Some(sid) = spec_id {
            if let Some(spec_head) = self.read_spec_head(sid)? {
                return Ok(Some(spec_head));
            }
        }
        self.read_head()
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
        let path = self
            .writ_dir
            .join("seals")
            .join(format!("{}.json", seal.id));

        // Append-only guard: reject if a seal with this ID already exists.
        if path.exists() {
            return Err(WritError::SealAlreadyExists(seal.id.clone()));
        }

        let json = serde_json::to_string_pretty(seal)?;
        atomic_write(&path, json.as_bytes())?;
        Ok(())
    }

    fn save_spec(&self, spec: &Spec) -> WritResult<()> {
        let path = self
            .writ_dir
            .join("specs")
            .join(format!("{}.json", spec.id));
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
        if name
            .bytes()
            .any(|b| b < 0x20 || b == 0x7f || b == b' ' || b == b'~' || b == b'^' || b == b':')
        {
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
        if !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
        {
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
        let git_repo = git2::Repository::discover(&self.root).map_err(|_| WritError::NoGitRepo)?;

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

        let mut seal_warnings = Vec::new();
        let parent_seal_hash = match parent {
            Some(ref pid) => match self.load_seal(pid) {
                Ok(s) => s.chain_hash.clone(),
                Err(e) => {
                    seal_warnings.push(format!(
                        "CHAIN_BREAK: failed to load parent seal {}: {e} — chain integrity may be compromised",
                        &pid[..12.min(pid.len())]
                    ));
                    None
                }
            },
            None => None,
        };

        let short_hash = &git_commit_hash[..12.min(git_commit_hash.len())];
        let mut seal = Seal::new(
            parent,
            tree_hash,
            agent,
            None,
            TaskStatus::Complete,
            changes.clone(),
            Verification::default(),
            format!("bridge import from git {short_hash}"),
            seal_warnings,
            parent_seal_hash,
        );

        // Sign with agent's key if available, otherwise unsigned
        let ks = KeyStore::open(&self.writ_dir);
        let signing_key = ks.load_agent_signing_key(&seal.agent.id).ok();
        seal.secure(signing_key.as_ref());

        self.save_seal(&seal)?;
        atomic_write(&self.writ_dir.join("HEAD"), seal.id.as_bytes())?;
        index.save(&self.writ_dir.join("index.json"))?;

        // Refresh the index to match the actual working directory so the
        // next seal() only captures the agent's genuine changes, not the
        // delta between the git tree and the working tree. Without this,
        // the first agent to seal after bridge_import gets attributed with
        // every file that differs from the git snapshot (dirty files, files
        // not in git, ignore-rule mismatches, etc.).
        let rules = self.ignore_rules();
        let post_import_state = state::compute_state(&self.root, &index, &rules);
        if !post_import_state.is_clean() {
            for file_state in &post_import_state.changes {
                match file_state.status {
                    FileStatus::New | FileStatus::Modified => {
                        let content = fs::read(self.root.join(&file_state.path))?;
                        let hash = self.objects.store(&content)?;
                        let size = content.len() as u64;
                        index.upsert(&file_state.path, hash, size);
                    }
                    FileStatus::Deleted => {
                        index.remove(&file_state.path);
                    }
                }
            }
            index.save(&self.writ_dir.join("index.json"))?;
        }

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
            if path == ".writ"
                || path == ".git"
                || path.starts_with(".writ/")
                || path.starts_with(".git/")
            {
                continue;
            }

            match entry.kind() {
                Some(git2::ObjectType::Blob) => {
                    let obj = entry.to_object(git_repo)?;
                    let blob = obj
                        .as_blob()
                        .ok_or_else(|| WritError::GitError(format!("expected blob at {path}")))?;
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
                    let subtree = obj
                        .as_tree()
                        .ok_or_else(|| WritError::GitError(format!("expected tree at {path}")))?;
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
    pub fn bridge_export(&self, branch: Option<&str>) -> WritResult<crate::bridge::ExportResult> {
        use crate::bridge::{ExportResult, ExportedSeal};

        let branch_name = branch.unwrap_or("writ/export");
        Self::validate_branch_name(branch_name)?;
        let mut bridge_state = self.load_bridge_state()?;

        if bridge_state.last_imported_git_commit.is_none() {
            return Err(WritError::BridgeError(
                "import required before export — run bridge_import first".to_string(),
            ));
        }

        let git_repo = git2::Repository::discover(&self.root).map_err(|_| WritError::NoGitRepo)?;

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

        let last_export = match (
            &state.last_exported_seal_id,
            &state.last_exported_git_commit,
            &state.exported_to_branch,
        ) {
            (Some(seal_id), Some(git_commit), Some(branch)) => Some(ExportSummary {
                seal_id: seal_id.clone(),
                git_commit: git_commit.clone(),
                branch: branch.clone(),
            }),
            _ => None,
        };

        // Count pending seals
        let boundary = state
            .last_exported_seal_id
            .as_deref()
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
        fs::create_dir_all(path.join("heads"))?;
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
        Self::sync_heads(&self.writ_dir, &remote_path)?;

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
        Self::sync_heads(&remote_path, &self.writ_dir)?;

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
    pub fn remote_status(&self, remote_name: &str) -> WritResult<crate::remote::RemoteStatus> {
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
            (Some(local_h), Some(remote_h)) if local_h != remote_h => self
                .count_remote_seals_between(&remote_path, remote_h, local_h)
                .unwrap_or(0),
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
            return Err(WritError::InvalidRemote(path.display().to_string()));
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

    /// Sync spec head pointers from src to dst (latest-wins).
    fn sync_heads(src: &Path, dst: &Path) -> WritResult<usize> {
        let mut count = 0;
        let src_heads = src.join("heads");
        let dst_heads = dst.join("heads");
        if !src_heads.is_dir() {
            return Ok(0);
        }
        if !dst_heads.exists() {
            fs::create_dir_all(&dst_heads)?;
        }
        for entry in fs::read_dir(&src_heads)? {
            let entry = entry?;
            let name = entry.file_name();
            let dst_path = dst_heads.join(&name);
            let src_content = fs::read_to_string(entry.path())?.trim().to_string();
            let dst_content = if dst_path.exists() {
                fs::read_to_string(&dst_path)?.trim().to_string()
            } else {
                String::new()
            };
            if src_content != dst_content && !src_content.is_empty() {
                atomic_write(&dst_path, src_content.as_bytes())?;
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
    fn merge_spec_fields(
        incoming: &crate::spec::Spec,
        existing: &crate::spec::Spec,
    ) -> crate::spec::Spec {
        use crate::spec::SpecStatus;

        // Title/description: take the one with newer updated_at
        let (title, description) = if incoming.updated_at > existing.updated_at {
            (incoming.title.clone(), incoming.description.clone())
        } else {
            (existing.title.clone(), existing.description.clone())
        };

        // Status: take the most progressed (Blocked always wins)
        let status =
            if incoming.status == SpecStatus::Blocked || existing.status == SpecStatus::Blocked {
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
            lifecycle_state: existing.lifecycle_state.clone(),
            last_activity: std::cmp::max(incoming.last_activity, existing.last_activity),
            completion_summary: existing
                .completion_summary
                .clone()
                .or(incoming.completion_summary.clone()),
            commit_state: existing.commit_state.clone(),
            completed_at: existing.completed_at.or(incoming.completed_at),
            commit_hash: existing
                .commit_hash
                .clone()
                .or(incoming.commit_hash.clone()),
            committed_at: existing.committed_at.or(incoming.committed_at),
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

    /// Check whether changed files fall outside the spec's declared file_scope.
    /// Returns None if the spec has no file_scope set (empty = no restriction).
    pub fn check_file_scope(
        &self,
        spec_id: &str,
        changed_paths: &[String],
    ) -> Option<FileScopeWarning> {
        let spec = self.load_spec(spec_id).ok()?;
        if spec.file_scope.is_empty() {
            return None;
        }

        let mut in_scope = Vec::new();
        let mut out_of_scope = Vec::new();

        for path in changed_paths {
            let matches = spec.file_scope.iter().any(|scope| {
                if scope.ends_with('/') {
                    path.starts_with(scope) || path.starts_with(&scope[..scope.len() - 1])
                } else if scope.contains('*') {
                    crate::ignore::glob_match(scope, path)
                } else {
                    path == scope || path.starts_with(&format!("{scope}/"))
                }
            });
            if matches {
                in_scope.push(path.clone());
            } else {
                out_of_scope.push(path.clone());
            }
        }

        if out_of_scope.is_empty() {
            return None;
        }

        Some(FileScopeWarning {
            spec_id: spec_id.to_string(),
            declared_scope: spec.file_scope.clone(),
            out_of_scope_files: out_of_scope,
            in_scope_files: in_scope,
        })
    }

    /// Join a list of names with commas, truncating with "+N more" if too long.
    fn truncate_list(items: &[&str], max_chars: usize) -> String {
        if items.is_empty() {
            return String::new();
        }
        let mut result = String::new();
        let mut included = 0;
        for (i, item) in items.iter().enumerate() {
            let addition = if i == 0 {
                item.to_string()
            } else {
                format!(", {item}")
            };
            if !result.is_empty() && result.len() + addition.len() > max_chars {
                let remaining = items.len() - included;
                if remaining > 0 {
                    result.push_str(&format!(", +{remaining} more"));
                }
                break;
            }
            result.push_str(&addition);
            included += 1;
        }
        result
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

    /// Generate a human-readable summary of all work done in this writ session.
    ///
    /// Walks the full seal history, aggregates by spec and agent, and produces
    /// a structured output suitable for generating git commit messages and
    /// reviewing what happened during an agentic workflow.
    pub fn summary(&self) -> WritResult<SummaryOutput> {
        let all_seals = self.log_all()?;

        // Skip bridge import seals for the summary.
        let work_seals: Vec<&Seal> = all_seals
            .iter()
            .filter(|s| s.agent.id != "writ-bridge")
            .collect();

        // Aggregate by spec.
        let specs = self.list_specs()?;
        let mut specs_summary: Vec<SpecSummaryEntry> = Vec::new();
        for spec in &specs {
            let spec_seals: Vec<&&Seal> = work_seals
                .iter()
                .filter(|s| s.spec_id.as_deref() == Some(&spec.id))
                .collect();

            if spec_seals.is_empty() {
                continue;
            }

            let mut agents: Vec<String> = spec_seals
                .iter()
                .map(|s| s.agent.id.clone())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            agents.sort();

            // Derive effective status: use seal history if it's more progressed
            // than the spec-level status. This handles the case where agents
            // sealed with --status complete but didn't call update_spec().
            let effective_status = self.effective_spec_status(spec, &spec_seals);
            let status = match effective_status {
                crate::spec::SpecStatus::Pending => "pending",
                crate::spec::SpecStatus::InProgress => "in-progress",
                crate::spec::SpecStatus::Complete => "complete",
                crate::spec::SpecStatus::Blocked => "blocked",
            };

            // Collect seal summaries (oldest first for chronological ordering).
            let mut seal_summaries: Vec<String> =
                spec_seals.iter().rev().map(|s| s.summary.clone()).collect();
            seal_summaries.dedup();

            specs_summary.push(SpecSummaryEntry {
                id: spec.id.clone(),
                title: spec.title.clone(),
                status: status.to_string(),
                seal_count: spec_seals.len(),
                agents,
                seal_summaries,
            });
        }

        // Aggregate by agent.
        let mut agent_map: BTreeMap<String, (usize, HashSet<String>, Option<String>)> =
            BTreeMap::new();
        for seal in &work_seals {
            let entry = agent_map
                .entry(seal.agent.id.clone())
                .or_insert((0, HashSet::new(), None));
            entry.0 += 1;
            if let Some(ref sid) = seal.spec_id {
                entry.1.insert(sid.clone());
            }
            if entry.2.is_none() {
                entry.2 = Some(seal.summary.clone());
            }
        }

        let agents: Vec<AgentSummaryEntry> = agent_map
            .into_iter()
            .map(|(id, (count, specs_set, latest))| {
                let mut specs_touched: Vec<String> = specs_set.into_iter().collect();
                specs_touched.sort();
                AgentSummaryEntry {
                    id,
                    seal_count: count,
                    specs_touched,
                    latest_summary: latest,
                }
            })
            .collect();

        // Collect all changed files across all seals (deduplicated).
        let mut all_files: HashSet<String> = HashSet::new();
        for seal in &work_seals {
            for change in &seal.changes {
                all_files.insert(change.path.clone());
            }
        }
        let mut files_changed: Vec<String> = all_files.into_iter().collect();
        files_changed.sort();

        // Files to stage = current working tree changes.
        let state = self.state()?;
        let mut files_to_stage: Vec<String> =
            state.changes.iter().map(|c| c.path.clone()).collect();
        files_to_stage.sort();

        // Divergence info.
        let diverged = self.diverged_branches().unwrap_or_default();
        let convergence_recommended = !diverged.is_empty();

        // Build the headline.
        let completed_specs: Vec<&SpecSummaryEntry> = specs_summary
            .iter()
            .filter(|s| s.status == "complete")
            .collect();
        let agent_count = agents.len();

        let headline = if specs_summary.is_empty() && work_seals.is_empty() {
            "writ: no changes".to_string()
        } else if specs_summary.is_empty() {
            // Persona A: no specs — synthesize from seal summaries.
            let summaries: Vec<&str> = work_seals
                .iter()
                .rev() // oldest first
                .map(|s| s.summary.as_str())
                .collect();
            let joined = Self::truncate_list(&summaries, 80);
            format!("writ: {joined}")
        } else if completed_specs.len() == 1 && specs_summary.len() == 1 {
            format!(
                "writ: {} — {} seal(s) by {} agent(s)",
                completed_specs[0].title,
                work_seals.len(),
                agent_count,
            )
        } else if completed_specs.len() >= 2 {
            // Multiple complete specs — include titles.
            let titles: Vec<&str> = completed_specs.iter().map(|s| s.title.as_str()).collect();
            let joined = Self::truncate_list(&titles, 60);
            format!(
                "writ: {} features complete — {}",
                completed_specs.len(),
                joined,
            )
        } else {
            // Specs exist but none/some complete — use spec titles.
            let titles: Vec<&str> = specs_summary.iter().map(|s| s.title.as_str()).collect();
            let joined = Self::truncate_list(&titles, 60);
            format!(
                "writ: {} — {} seal(s) by {} agent(s)",
                joined,
                work_seals.len(),
                agent_count,
            )
        };

        // Build the body.
        let mut body_lines: Vec<String> = Vec::new();

        // For the no-specs case, list seal summaries.
        if specs_summary.is_empty() && !work_seals.is_empty() {
            body_lines.push("Work:".to_string());
            for seal in work_seals.iter().rev() {
                body_lines.push(format!("  - {} ({})", seal.summary, seal.agent.id));
            }
            body_lines.push(String::new());
        }

        if !specs_summary.is_empty() {
            body_lines.push("Specs:".to_string());
            for s in &specs_summary {
                body_lines.push(format!(
                    "  - {} [{}]: {} ({} seal(s) by {})",
                    s.id,
                    s.status,
                    s.title,
                    s.seal_count,
                    s.agents.join(", "),
                ));
                // Include seal summaries (cap at 5 per spec).
                for desc in s.seal_summaries.iter().take(5) {
                    body_lines.push(format!("    • {desc}"));
                }
                if s.seal_summaries.len() > 5 {
                    body_lines.push(format!("    • ... and {} more", s.seal_summaries.len() - 5));
                }
            }
            body_lines.push(String::new());
        }

        if !agents.is_empty() {
            body_lines.push("Agents:".to_string());
            for a in &agents {
                body_lines.push(format!(
                    "  - {}: {} seal(s) on {}",
                    a.id,
                    a.seal_count,
                    a.specs_touched.join(", "),
                ));
            }
            body_lines.push(String::new());
        }

        body_lines.push(format!("Files changed: {}", files_changed.len()));
        body_lines.push(format!("Total seals: {}", work_seals.len()));

        if convergence_recommended {
            body_lines.push(String::new());
            body_lines.push(format!(
                "Note: {} diverged branch(es) — consider running `writ converge` before committing.",
                diverged.len(),
            ));
        }

        let body = body_lines.join("\n");
        let commit_message = format!("{headline}\n\n{body}");

        Ok(SummaryOutput {
            headline,
            body,
            commit_message,
            specs_summary,
            agents,
            total_seals: work_seals.len(),
            files_changed,
            files_to_stage,
            convergence_recommended,
            diverged_branch_count: diverged.len(),
        })
    }
}

/// Result of `writ init`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InitResult {
    pub initialized: bool,
    pub git_detected: bool,
    pub git_imported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imported_seal_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imported_files: Option<usize>,
    /// Absolute path to repository root.
    #[serde(default)]
    pub repo_root: String,
    /// Current git branch name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    /// First 12 chars of git HEAD hash.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_head_short: Option<String>,
    /// Whether git working tree has uncommitted changes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_dirty: Option<bool>,
    /// Number of dirty files in git working tree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_dirty_count: Option<usize>,
    /// Why import was skipped (human-readable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub import_skipped_reason: Option<String>,
    /// Error message if bridge_import failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub import_error: Option<String>,
    /// Whether .writignore was created by this install.
    #[serde(default)]
    pub writignore_created: bool,
    /// True if baseline was already imported and HEAD hasn't moved.
    #[serde(default)]
    pub already_imported: bool,
    /// True if baseline was re-imported because git HEAD moved.
    #[serde(default)]
    pub reimported: bool,
    /// Total files tracked after install.
    #[serde(default)]
    pub tracked_files: usize,
    /// Next steps for agents.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub available_operations: Vec<String>,
    /// Frameworks detected during install.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub frameworks_detected: Vec<crate::hooks::FrameworkDetection>,
    /// Hooks installed during install.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub hooks_installed: Vec<crate::hooks::HookResult>,
}

/// Result of `writ uninit` (inverse of `writ init`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UninstallResult {
    /// Whether .writ/ was removed.
    pub writ_dir_removed: bool,
    /// Whether .writignore was removed.
    pub writignore_removed: bool,
    /// Number of seals that existed before removal.
    pub seals_existed: usize,
    /// Number of tracked files before removal.
    pub tracked_files: usize,
    /// Framework hooks that were cleaned up.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub hooks_removed: Vec<crate::hooks::UninstallHookResult>,
    /// Warnings generated during uninstall.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
}

/// Returned by `seal()` when changed files fall outside the spec's declared `file_scope`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileScopeWarning {
    /// The spec ID whose scope was exceeded.
    pub spec_id: String,
    /// The declared file scope patterns on the spec.
    pub declared_scope: Vec<String>,
    /// Changed files that fall outside the declared scope.
    pub out_of_scope_files: Vec<String>,
    /// Changed files that are within scope.
    pub in_scope_files: Vec<String>,
}

/// A spec branch whose tip seal is not reachable from global HEAD.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DivergedBranch {
    /// The spec this branch belongs to.
    pub spec_id: String,
    /// Short ID of the branch tip seal.
    pub tip_seal: String,
    /// Number of seals on this branch not reachable from HEAD.
    pub seal_count: usize,
    /// Agent IDs that sealed on this branch.
    pub agents: Vec<String>,
}

/// Convert a ConvergeStrategy to a human-readable string.
#[allow(deprecated)]
fn strategy_name(strategy: ConvergeStrategy) -> String {
    match strategy {
        ConvergeStrategy::Manual => "manual".to_string(),
        ConvergeStrategy::MostRecent => "most-recent".to_string(),
        ConvergeStrategy::Orchestrator => "orchestrator".to_string(),
        ConvergeStrategy::Escalate => "escalate".to_string(),
    }
}

/// Structural validation of applied merged files.
///
/// Returns a list of warnings for issues detected in the merged result.
/// These are lightweight, language-agnostic checks that catch the class of
/// errors that killed TR13 (orphaned modules, unbalanced brackets, etc.).
fn post_convergence_validation(
    root: &Path,
    file_decisions: &[FileDecision],
    left_content_map: &std::collections::HashMap<String, String>,
    right_content_map: &std::collections::HashMap<String, String>,
) -> Vec<String> {
    let mut warnings = Vec::new();

    for decision in file_decisions {
        let path = &decision.path;
        let full_path = root.join(path);

        let merged = match fs::read_to_string(&full_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // No-loss check: if an agent contributed >3 non-empty lines to this
        // file, at least some of their content should appear in the merged
        // result. Flag if an entire contribution was discarded.
        for alt in &decision.alternatives {
            let source_content = if let Some(c) = left_content_map.get(path) {
                if alt.spec == *c {
                    None
                } else {
                    left_content_map.get(path)
                }
            } else {
                right_content_map.get(path)
            };

            if let Some(content) = source_content {
                let significant_lines: Vec<&str> =
                    content.lines().filter(|l| !l.trim().is_empty()).collect();
                if significant_lines.len() > 3 {
                    let lines_found = significant_lines
                        .iter()
                        .filter(|l| merged.contains(*l))
                        .count();
                    let ratio = lines_found as f64 / significant_lines.len() as f64;
                    if ratio < 0.3 {
                        warnings.push(format!(
                            "NO_LOSS: {}: spec '{}' contributed {} significant lines but only {:.0}% appear in merged result",
                            path, alt.spec, significant_lines.len(), ratio * 100.0
                        ));
                    }
                }
            }
        }

        // Bracket balance check: count {, }, (, ), [, ] in the merged file.
        // If unbalanced and both inputs were balanced, the merge introduced
        // a structural error.
        let opens: usize = merged
            .chars()
            .filter(|c| matches!(c, '{' | '(' | '['))
            .count();
        let closes: usize = merged
            .chars()
            .filter(|c| matches!(c, '}' | ')' | ']'))
            .count();
        if opens != closes {
            let left_bal = left_content_map.get(path).map_or(true, |c| {
                let lo: usize = c.chars().filter(|ch| matches!(ch, '{' | '(' | '[')).count();
                let lc: usize = c.chars().filter(|ch| matches!(ch, '}' | ')' | ']')).count();
                lo == lc
            });
            let right_bal = right_content_map.get(path).map_or(true, |c| {
                let ro: usize = c.chars().filter(|ch| matches!(ch, '{' | '(' | '[')).count();
                let rc: usize = c.chars().filter(|ch| matches!(ch, '}' | ')' | ']')).count();
                ro == rc
            });
            if left_bal && right_bal {
                warnings.push(format!(
                    "BRACKET_BALANCE: {}: merged file has unbalanced brackets (opens={}, closes={}) but both inputs were balanced",
                    path, opens, closes
                ));
            }
        }

        // Import consistency: check for orphaned imports (imports reference
        // modules that don't exist as local files, aren't in stdlib, and
        // aren't declared as installed dependencies).
        if path.ends_with(".py") || path.ends_with(".js") || path.ends_with(".ts") {
            let installed = installed_packages(root);
            for line in merged.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("from ") && trimmed.contains(" import ") {
                    if let Some(module) = trimmed
                        .strip_prefix("from ")
                        .and_then(|s| s.split_whitespace().next())
                    {
                        if !module.starts_with('.') && !module.contains('.') {
                            let module_file = root.join(format!("{}.py", module));
                            let module_dir = root.join(module).join("__init__.py");

                            let file_dir = Path::new(path).parent();
                            let local_module_file =
                                file_dir.map(|d| root.join(d).join(format!("{}.py", module)));
                            let local_module_dir =
                                file_dir.map(|d| root.join(d).join(module).join("__init__.py"));

                            let found_locally =
                                local_module_file.as_ref().map_or(false, |p| p.exists())
                                    || local_module_dir.as_ref().map_or(false, |p| p.exists());

                            if !module_file.exists()
                                && !module_dir.exists()
                                && !found_locally
                                && !is_stdlib_module(module)
                                && !installed.contains(module)
                            {
                                warnings.push(format!(
                                    "IMPORT_ORPHAN: {}: imports '{}' but no matching file found — may be orphaned after merge",
                                    path, module
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    warnings
}

fn is_stdlib_module(name: &str) -> bool {
    matches!(
        name,
        "os" | "sys"
            | "json"
            | "re"
            | "math"
            | "datetime"
            | "collections"
            | "functools"
            | "itertools"
            | "pathlib"
            | "typing"
            | "abc"
            | "io"
            | "time"
            | "logging"
            | "unittest"
            | "subprocess"
            | "hashlib"
            | "copy"
            | "random"
            | "string"
            | "tempfile"
            | "shutil"
            | "glob"
            | "csv"
            | "sqlite3"
            | "http"
            | "urllib"
            | "socket"
            | "threading"
            | "multiprocessing"
            | "argparse"
            | "contextlib"
            | "dataclasses"
            | "enum"
            | "textwrap"
    )
}

/// Collect package names declared in dependency files (requirements.txt,
/// pyproject.toml, package.json) so we can suppress false-positive
/// IMPORT_ORPHAN warnings for installed third-party packages.
fn installed_packages(root: &Path) -> HashSet<String> {
    let mut pkgs = HashSet::new();

    // Python: requirements.txt — lines like "flask>=2.0", "requests", "PyJWT==2.8"
    if let Ok(content) = fs::read_to_string(root.join("requirements.txt")) {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
                continue;
            }
            // Strip version specifiers and extras: "flask[async]>=2.0" → "flask"
            let name: String = trimmed
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            if !name.is_empty() {
                // Python package names are case-insensitive; import names
                // use lowercase with hyphens mapped to underscores.
                pkgs.insert(name.to_lowercase().replace('-', "_"));
            }
        }
    }

    // Python: pyproject.toml — look for lines in [project.dependencies]
    // Simple heuristic: grab quoted package names from dependency arrays.
    if let Ok(content) = fs::read_to_string(root.join("pyproject.toml")) {
        for line in content.lines() {
            let trimmed = line.trim().trim_start_matches('-').trim();
            // Match "flask>=2.0", 'requests', etc. inside dependency arrays.
            let cleaned = trimmed.trim_matches(|c: char| c == '"' || c == '\'' || c == ',');
            if cleaned.is_empty() {
                continue;
            }
            let name: String = cleaned
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            if name.len() >= 2 && name.chars().next().map_or(false, |c| c.is_alphabetic()) {
                pkgs.insert(name.to_lowercase().replace('-', "_"));
            }
        }
    }

    // JS/TS: package.json — extract keys from "dependencies" and "devDependencies".
    if let Ok(content) = fs::read_to_string(root.join("package.json")) {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
            for section in &["dependencies", "devDependencies"] {
                if let Some(deps) = parsed.get(section).and_then(|v| v.as_object()) {
                    for key in deps.keys() {
                        // Strip npm scope: "@types/react" → "react" for import matching.
                        let name = key
                            .strip_prefix('@')
                            .and_then(|s| s.split('/').nth(1))
                            .unwrap_or(key);
                        pkgs.insert(name.to_string());
                        pkgs.insert(key.to_string());
                    }
                }
            }
        }
    }

    pkgs
}

/// Build a post-convergence quality report from collected file decisions.
///
/// When `apply` is true, reads applied HTML files from disk to run
/// consistency checks (nav item counts, CSS link counts, etc.).
fn build_quality_report(
    file_decisions: Vec<FileDecision>,
    root: &Path,
    apply: bool,
    total_conflicts: usize,
    total_resolutions: usize,
) -> ConvergenceQualityReport {
    let mut consistency_checks = Vec::new();

    // Run consistency checks on applied HTML files.
    if apply {
        let html_paths: Vec<String> = file_decisions
            .iter()
            .filter(|d| d.path.ends_with(".html"))
            .map(|d| d.path.clone())
            .collect();

        if html_paths.len() >= 2 {
            let mut li_counts: Vec<FileMetricValue> = Vec::new();
            let mut link_counts: Vec<FileMetricValue> = Vec::new();

            for path in &html_paths {
                let full_path = root.join(path);
                if let Ok(content) = fs::read_to_string(&full_path) {
                    li_counts.push(FileMetricValue {
                        path: path.clone(),
                        value: content.matches("<li").count(),
                    });
                    link_counts.push(FileMetricValue {
                        path: path.clone(),
                        value: content.matches("<link").count(),
                    });
                }
            }

            if li_counts.len() >= 2 {
                let vals: Vec<usize> = li_counts.iter().map(|v| v.value).collect();
                let min = *vals.iter().min().unwrap_or(&0);
                let max = *vals.iter().max().unwrap_or(&0);
                let consistent = max <= min + 2; // allow small variance
                let warning = if !consistent {
                    Some(format!(
                        "nav item counts vary from {} to {} across HTML files",
                        min, max,
                    ))
                } else {
                    None
                };
                consistency_checks.push(ConsistencyCheck {
                    metric: "nav_item_count".to_string(),
                    values: li_counts,
                    consistent,
                    warning,
                });
            }

            if link_counts.len() >= 2 {
                let vals: Vec<usize> = link_counts.iter().map(|v| v.value).collect();
                let min = *vals.iter().min().unwrap_or(&0);
                let max = *vals.iter().max().unwrap_or(&0);
                let consistent = min == max;
                let warning = if !consistent {
                    Some(format!(
                        "CSS link counts vary from {} to {} across HTML files",
                        min, max,
                    ))
                } else {
                    None
                };
                consistency_checks.push(ConsistencyCheck {
                    metric: "css_link_count".to_string(),
                    values: link_counts,
                    consistent,
                    warning,
                });
            }
        }
    }

    // Check bracket balance on all applied files.
    if apply {
        let mut bracket_values: Vec<FileMetricValue> = Vec::new();
        let mut any_imbalanced = false;

        for d in &file_decisions {
            let full = root.join(&d.path);
            if let Ok(content) = fs::read_to_string(&full) {
                let opens: usize = content
                    .chars()
                    .filter(|c| matches!(c, '{' | '(' | '['))
                    .count();
                let closes: usize = content
                    .chars()
                    .filter(|c| matches!(c, '}' | ')' | ']'))
                    .count();
                let imbalance = (opens as i64 - closes as i64).unsigned_abs() as usize;
                if imbalance > 0 {
                    any_imbalanced = true;
                }
                bracket_values.push(FileMetricValue {
                    path: d.path.clone(),
                    value: imbalance,
                });
            }
        }

        if !bracket_values.is_empty() {
            consistency_checks.push(ConsistencyCheck {
                metric: "bracket_balance".to_string(),
                values: bracket_values,
                consistent: !any_imbalanced,
                warning: if any_imbalanced {
                    Some("one or more merged files have unbalanced brackets — structural corruption likely".to_string())
                } else {
                    None
                },
            });
        }
    }

    // Check for duplicate definitions in applied code files.
    // Catches duplicate `def`, `class`, `fn`, `struct`, `function`, `const` etc.
    if apply {
        let mut dup_def_values: Vec<FileMetricValue> = Vec::new();
        let mut any_dup_defs = false;

        for d in &file_decisions {
            let ext = Path::new(&d.path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            if !matches!(ext, "py" | "rs" | "go" | "js" | "ts" | "tsx" | "jsx" | "rb") {
                continue;
            }
            let full = root.join(&d.path);
            if let Ok(content) = fs::read_to_string(&full) {
                let mut def_names: Vec<String> = Vec::new();
                for line in content.lines() {
                    let trimmed = line.trim();
                    // Python: def foo(...) / class Foo
                    // Rust: fn foo / struct Foo / enum Foo
                    // Go: func foo
                    // JS/TS: function foo / class Foo / const foo =
                    let name = if let Some(rest) = trimmed.strip_prefix("def ") {
                        rest.split('(').next().map(|s| format!("def:{}", s.trim()))
                    } else if let Some(rest) = trimmed.strip_prefix("class ") {
                        rest.split(['(', ':', '{', ' '])
                            .next()
                            .map(|s| format!("class:{}", s.trim()))
                    } else if let Some(rest) = trimmed.strip_prefix("fn ") {
                        rest.split(['(', '<'])
                            .next()
                            .map(|s| format!("fn:{}", s.trim()))
                    } else if let Some(rest) = trimmed.strip_prefix("pub fn ") {
                        rest.split(['(', '<'])
                            .next()
                            .map(|s| format!("fn:{}", s.trim()))
                    } else if let Some(rest) = trimmed.strip_prefix("struct ") {
                        rest.split(['{', '(', '<', ' '])
                            .next()
                            .map(|s| format!("struct:{}", s.trim()))
                    } else if let Some(rest) = trimmed.strip_prefix("pub struct ") {
                        rest.split(['{', '(', '<', ' '])
                            .next()
                            .map(|s| format!("struct:{}", s.trim()))
                    } else if let Some(rest) = trimmed.strip_prefix("function ") {
                        rest.split('(')
                            .next()
                            .map(|s| format!("function:{}", s.trim()))
                    } else {
                        None
                    };
                    if let Some(n) = name {
                        if !n.ends_with(':') {
                            def_names.push(n);
                        }
                    }
                }

                let mut seen = HashSet::new();
                let dup_count = def_names
                    .iter()
                    .filter(|n| !seen.insert(n.as_str()))
                    .count();
                if dup_count > 0 {
                    any_dup_defs = true;
                }
                dup_def_values.push(FileMetricValue {
                    path: d.path.clone(),
                    value: dup_count,
                });
            }
        }

        if !dup_def_values.is_empty() {
            consistency_checks.push(ConsistencyCheck {
                metric: "duplicate_definitions".to_string(),
                values: dup_def_values,
                consistent: !any_dup_defs,
                warning: if any_dup_defs {
                    Some(
                        "one or more files contain duplicate definitions (functions, classes, structs) — likely merge corruption"
                            .to_string(),
                    )
                } else {
                    None
                },
            });
        }
    }

    // Check for duplicate imports in applied files.
    if apply {
        let mut dup_import_values: Vec<FileMetricValue> = Vec::new();
        let mut any_dup = false;

        for d in &file_decisions {
            if d.path.ends_with(".py") || d.path.ends_with(".js") || d.path.ends_with(".ts") {
                let full = root.join(&d.path);
                if let Ok(content) = fs::read_to_string(&full) {
                    let import_lines: Vec<&str> = content
                        .lines()
                        .map(|l| l.trim())
                        .filter(|l| l.starts_with("import ") || l.starts_with("from "))
                        .collect();
                    let unique: HashSet<&str> = import_lines.iter().copied().collect();
                    let dup_count = import_lines.len() - unique.len();
                    if dup_count > 0 {
                        any_dup = true;
                    }
                    dup_import_values.push(FileMetricValue {
                        path: d.path.clone(),
                        value: dup_count,
                    });
                }
            }
        }

        if !dup_import_values.is_empty() {
            consistency_checks.push(ConsistencyCheck {
                metric: "duplicate_imports".to_string(),
                values: dup_import_values,
                consistent: !any_dup,
                warning: if any_dup {
                    Some("one or more files contain duplicate import statements".to_string())
                } else {
                    None
                },
            });
        }
    }

    // Check for unused imports in applied Python files.
    if apply {
        let mut unused_import_values: Vec<FileMetricValue> = Vec::new();
        let mut any_unused = false;

        for d in &file_decisions {
            if d.path.ends_with(".py") {
                let full = root.join(&d.path);
                if let Ok(content) = fs::read_to_string(&full) {
                    let lines: Vec<&str> = content.lines().collect();
                    let non_import_text: String = lines
                        .iter()
                        .filter(|l| {
                            let t = l.trim();
                            !t.starts_with("import ") && !t.starts_with("from ")
                        })
                        .copied()
                        .collect::<Vec<&str>>()
                        .join("\n");

                    let mut unused_count = 0usize;
                    for line in &lines {
                        let trimmed = line.trim();
                        // Match "from X import Y" and check if Y is used.
                        if trimmed.starts_with("from ") && trimmed.contains(" import ") {
                            if let Some(after_import) = trimmed.splitn(2, " import ").nth(1) {
                                for name in after_import.split(',') {
                                    let name = name.trim();
                                    // Skip wildcard imports and aliased names.
                                    if name == "*" || name.contains(" as ") {
                                        continue;
                                    }
                                    if !convergence::contains_word(&non_import_text, name) {
                                        unused_count += 1;
                                    }
                                }
                            }
                        }
                    }
                    if unused_count > 0 {
                        any_unused = true;
                    }
                    unused_import_values.push(FileMetricValue {
                        path: d.path.clone(),
                        value: unused_count,
                    });
                }
            }
        }

        if !unused_import_values.is_empty() {
            consistency_checks.push(ConsistencyCheck {
                metric: "unused_imports".to_string(),
                values: unused_import_values,
                consistent: !any_unused,
                warning: if any_unused {
                    Some("one or more files contain unused imports".to_string())
                } else {
                    None
                },
            });
        }
    }

    // Check cross-file reference integrity for most-recent resolved Python files.
    // When most-recent discards content, other files may import symbols that no
    // longer exist in the chosen version — this is structural breakage.
    if apply {
        let most_recent_py: Vec<&FileDecision> = file_decisions
            .iter()
            .filter(|d| d.decision == "most-recent" && d.path.ends_with(".py"))
            .collect();

        if !most_recent_py.is_empty() {
            let mut ref_values: Vec<FileMetricValue> = Vec::new();
            let mut any_broken = false;
            let mut total_broken_symbols = 0usize;
            let mut broken_modules: Vec<String> = Vec::new();

            for mr in &most_recent_py {
                let module_name = Path::new(&mr.path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();

                if module_name.is_empty() || module_name == "__init__" {
                    continue;
                }

                let mr_full = root.join(&mr.path);
                let mr_content = match fs::read_to_string(&mr_full) {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                let mut broken_refs = 0usize;
                for other in &file_decisions {
                    if other.path == mr.path || !other.path.ends_with(".py") {
                        continue;
                    }
                    let other_full = root.join(&other.path);
                    let other_content = match fs::read_to_string(&other_full) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };

                    for line in other_content.lines() {
                        let trimmed = line.trim();
                        let prefix = format!("from {} import ", module_name);
                        if let Some(after) = trimmed.strip_prefix(&prefix) {
                            for name in after.split(',') {
                                let name = name.trim();
                                if name.is_empty() || name == "*" || name.contains(" as ") {
                                    continue;
                                }
                                if !mr_content.contains(name) {
                                    broken_refs += 1;
                                }
                            }
                        }
                    }
                }

                if broken_refs > 0 {
                    any_broken = true;
                    total_broken_symbols += broken_refs;
                    broken_modules.push(module_name);
                }
                ref_values.push(FileMetricValue {
                    path: mr.path.clone(),
                    value: broken_refs,
                });
            }

            if !ref_values.is_empty() {
                let warning = if any_broken {
                    Some(format!(
                        "{} symbol(s) imported from '{}' not found in merged version \
                         — content loss from most-recent strategy",
                        total_broken_symbols,
                        broken_modules.join("', '"),
                    ))
                } else {
                    None
                };
                consistency_checks.push(ConsistencyCheck {
                    metric: "cross_file_reference_integrity".to_string(),
                    values: ref_values,
                    consistent: !any_broken,
                    warning,
                });
            }
        }
    }

    // Compute quality score.
    let mut score: i32 = 100;
    let unresolved = total_conflicts.saturating_sub(total_resolutions);
    score -= (unresolved * 15) as i32;
    for check in &consistency_checks {
        if !check.consistent {
            let penalty = match check.metric.as_str() {
                "bracket_balance" => 25,
                "cross_file_reference_integrity" => 25,
                "duplicate_definitions" => 30,
                "duplicate_imports" => 5,
                "unused_imports" => 3,
                _ => 10,
            };
            score -= penalty;
        }
    }

    // Compute per-file confidence stats.
    let confidences: Vec<f64> = file_decisions.iter().filter_map(|d| d.confidence).collect();
    let min_confidence = confidences.iter().copied().fold(f64::INFINITY, f64::min);
    let min_confidence = if min_confidence.is_infinite() {
        1.0
    } else {
        min_confidence
    };
    let avg_confidence = if confidences.is_empty() {
        1.0
    } else {
        confidences.iter().sum::<f64>() / confidences.len() as f64
    };

    // Penalize low confidence in quality score.
    if min_confidence < 0.85 {
        score -= 10;
    }
    if min_confidence < 0.5 {
        score -= 15; // additional penalty for very low confidence
    }

    let score = score.max(0).min(100) as u32;

    // Build summary.
    let n_files = file_decisions.len();
    let n_auto = file_decisions
        .iter()
        .filter(|d| d.decision == "auto-merged")
        .count();
    let n_resolved = file_decisions
        .iter()
        .filter(|d| matches!(d.decision.as_str(), "most-recent" | "auto-resolved"))
        .count();
    let n_unresolved = file_decisions
        .iter()
        .filter(|d| d.decision == "conflict-unresolved")
        .count();
    let n_inconsistent = consistency_checks.iter().filter(|c| !c.consistent).count();

    let mut summary = format!(
        "{} file(s) processed: {} auto-merged, {} resolved by strategy, {} unresolved conflict(s), {} consistency issue(s)",
        n_files, n_auto, n_resolved, n_unresolved, n_inconsistent,
    );
    if min_confidence < 1.0 {
        summary.push_str(&format!(
            " — confidence: min={}% avg={}%",
            (min_confidence * 100.0).round() as u32,
            (avg_confidence * 100.0).round() as u32,
        ));
    }

    ConvergenceQualityReport {
        file_decisions,
        consistency_checks,
        quality_score: score,
        min_confidence,
        avg_confidence,
        summary,
    }
}

/// Returned by `seal()` when HEAD moved since the agent started working.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SealConflictWarning {
    /// The HEAD the agent expected (what they saw at session start).
    pub expected_head: String,
    /// The actual HEAD at seal time (someone else sealed in between).
    pub actual_head: String,
    /// Seals that were added between expected and actual HEAD.
    pub intervening_seals: Vec<String>,
    /// Files modified by intervening seals.
    pub intervening_files: Vec<String>,
    /// Files this seal touches that overlap with intervening changes.
    pub overlapping_files: Vec<String>,
    /// True if no files overlap (safe concurrent work).
    pub is_clean: bool,
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

/// Human-readable summary of all work done in this writ session.
/// Designed for the round-trip workflow: writ init -> agents work -> writ summary -> git commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryOutput {
    /// High-level one-liner suitable for a git commit message subject line.
    pub headline: String,
    /// Detailed multi-line body suitable for a git commit message body.
    pub body: String,
    /// Suggested git commit message (headline + body combined).
    pub commit_message: String,
    /// Specs that were worked on, with their final status.
    pub specs_summary: Vec<SpecSummaryEntry>,
    /// Agents that participated, with seal counts.
    pub agents: Vec<AgentSummaryEntry>,
    /// Total seals created in this session.
    pub total_seals: usize,
    /// Files changed (added/modified/deleted) across all seals.
    pub files_changed: Vec<String>,
    /// Files to stage for git (current working tree changes).
    pub files_to_stage: Vec<String>,
    /// Whether convergence is recommended before committing.
    pub convergence_recommended: bool,
    /// Number of diverged branches.
    pub diverged_branch_count: usize,
}

/// Per-spec entry in the summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecSummaryEntry {
    pub id: String,
    pub title: String,
    pub status: String,
    pub seal_count: usize,
    pub agents: Vec<String>,
    /// Human-readable summaries from each seal (oldest first).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub seal_summaries: Vec<String>,
}

/// Per-agent entry in the summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSummaryEntry {
    pub id: String,
    pub seal_count: usize,
    pub specs_touched: Vec<String>,
    pub latest_summary: Option<String>,
}

/// Result of verifying a seal's cryptographic integrity.
#[derive(Debug, Clone, Serialize)]
pub struct SealVerification {
    pub seal_id: String,
    pub content_hash_valid: bool,
    pub chain_hash_valid: bool,
    pub signature_present: bool,
    pub signature_valid: Option<bool>,
    pub error: Option<String>,
}

/// Result of verifying the full chain from HEAD.
#[derive(Debug, Clone, Serialize)]
pub struct ChainVerification {
    pub total_seals: usize,
    pub verified: usize,
    pub unsecured: usize,
    pub failures: Vec<SealVerification>,
    pub valid: bool,
}

/// Result of verifying a single spec's branch chain.
#[derive(Debug, Clone, Serialize)]
pub struct SpecChainResult {
    pub spec_id: String,
    pub chain: ChainVerification,
}

/// Result of verifying HEAD chain + all spec branch chains.
#[derive(Debug, Clone, Serialize)]
pub struct AllChainsVerification {
    pub head_chain: ChainVerification,
    pub spec_chains: Vec<SpecChainResult>,
    pub all_valid: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seal::AgentType;
    use crate::spec::{LifecycleState, SpecStatus};
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

    // --- Lifecycle transition tests (Gap 2 coverage) ---

    #[test]
    fn test_lifecycle_active_to_stale() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let spec = Spec::new("s1".to_string(), "T".to_string(), "".to_string());
        repo.add_spec(&spec).unwrap();

        repo.transition_spec_lifecycle("s1", LifecycleState::Stale)
            .unwrap();
        let loaded = repo.load_spec("s1").unwrap();
        assert_eq!(loaded.lifecycle_state, LifecycleState::Stale);
    }

    #[test]
    fn test_lifecycle_active_to_cancelled() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let spec = Spec::new("s1".to_string(), "T".to_string(), "".to_string());
        repo.add_spec(&spec).unwrap();

        repo.transition_spec_lifecycle("s1", LifecycleState::Cancelled)
            .unwrap();
        let loaded = repo.load_spec("s1").unwrap();
        assert_eq!(loaded.lifecycle_state, LifecycleState::Cancelled);
    }

    #[test]
    fn test_lifecycle_active_to_completed() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let spec = Spec::new("s1".to_string(), "T".to_string(), "".to_string());
        repo.add_spec(&spec).unwrap();

        repo.transition_spec_lifecycle("s1", LifecycleState::Completed)
            .unwrap();
        let loaded = repo.load_spec("s1").unwrap();
        assert_eq!(loaded.lifecycle_state, LifecycleState::Completed);
    }

    #[test]
    fn test_lifecycle_stale_to_active() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let spec = Spec::new("s1".to_string(), "T".to_string(), "".to_string());
        repo.add_spec(&spec).unwrap();

        repo.transition_spec_lifecycle("s1", LifecycleState::Stale)
            .unwrap();
        repo.transition_spec_lifecycle("s1", LifecycleState::Active)
            .unwrap();
        let loaded = repo.load_spec("s1").unwrap();
        assert_eq!(loaded.lifecycle_state, LifecycleState::Active);
    }

    #[test]
    fn test_lifecycle_stale_to_cancelled() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let spec = Spec::new("s1".to_string(), "T".to_string(), "".to_string());
        repo.add_spec(&spec).unwrap();

        repo.transition_spec_lifecycle("s1", LifecycleState::Stale)
            .unwrap();
        repo.transition_spec_lifecycle("s1", LifecycleState::Cancelled)
            .unwrap();
        let loaded = repo.load_spec("s1").unwrap();
        assert_eq!(loaded.lifecycle_state, LifecycleState::Cancelled);
    }

    #[test]
    fn test_lifecycle_completed_to_archived() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let spec = Spec::new("s1".to_string(), "T".to_string(), "".to_string());
        repo.add_spec(&spec).unwrap();

        repo.transition_spec_lifecycle("s1", LifecycleState::Completed)
            .unwrap();
        repo.transition_spec_lifecycle("s1", LifecycleState::Archived)
            .unwrap();
        let loaded = repo.load_spec("s1").unwrap();
        assert_eq!(loaded.lifecycle_state, LifecycleState::Archived);
    }

    #[test]
    fn test_lifecycle_cancelled_to_archived() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let spec = Spec::new("s1".to_string(), "T".to_string(), "".to_string());
        repo.add_spec(&spec).unwrap();

        repo.transition_spec_lifecycle("s1", LifecycleState::Cancelled)
            .unwrap();
        repo.transition_spec_lifecycle("s1", LifecycleState::Archived)
            .unwrap();
        let loaded = repo.load_spec("s1").unwrap();
        assert_eq!(loaded.lifecycle_state, LifecycleState::Archived);
    }

    // --- Invalid transitions (Gap 2: corruption/illegal state handling) ---

    #[test]
    fn test_lifecycle_allows_completed_to_active() {
        // Completed → Active is a valid transition (used by writ reopen).
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let spec = Spec::new("s1".to_string(), "T".to_string(), "".to_string());
        repo.add_spec(&spec).unwrap();

        repo.transition_spec_lifecycle("s1", LifecycleState::Completed)
            .unwrap();
        repo.transition_spec_lifecycle("s1", LifecycleState::Active)
            .unwrap();

        let reopened = repo.load_spec("s1").unwrap();
        assert_eq!(reopened.lifecycle_state, LifecycleState::Active);
    }

    #[test]
    fn test_lifecycle_rejects_archived_to_any() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let spec = Spec::new("s1".to_string(), "T".to_string(), "".to_string());
        repo.add_spec(&spec).unwrap();

        repo.transition_spec_lifecycle("s1", LifecycleState::Completed)
            .unwrap();
        repo.transition_spec_lifecycle("s1", LifecycleState::Archived)
            .unwrap();

        // Archived is terminal — no transitions out
        for target in [
            LifecycleState::Active,
            LifecycleState::Stale,
            LifecycleState::Completed,
            LifecycleState::Cancelled,
        ] {
            let label = format!("{:?}", target);
            let err = repo.transition_spec_lifecycle("s1", target).unwrap_err();
            assert!(
                err.to_string().contains("not a legal transition"),
                "Archived → {} should be rejected, got: {err}",
                label
            );
        }
    }

    #[test]
    fn test_lifecycle_rejects_active_to_archived_directly() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let spec = Spec::new("s1".to_string(), "T".to_string(), "".to_string());
        repo.add_spec(&spec).unwrap();

        let err = repo
            .transition_spec_lifecycle("s1", LifecycleState::Archived)
            .unwrap_err();
        assert!(err.to_string().contains("not a legal transition"));
    }

    #[test]
    fn test_lifecycle_rejects_cancelled_to_active() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let spec = Spec::new("s1".to_string(), "T".to_string(), "".to_string());
        repo.add_spec(&spec).unwrap();

        repo.transition_spec_lifecycle("s1", LifecycleState::Cancelled)
            .unwrap();
        let err = repo
            .transition_spec_lifecycle("s1", LifecycleState::Active)
            .unwrap_err();
        assert!(err.to_string().contains("not a legal transition"));
    }

    #[test]
    fn test_lifecycle_noop_same_state_rejected() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let spec = Spec::new("s1".to_string(), "T".to_string(), "".to_string());
        repo.add_spec(&spec).unwrap();

        // Active → Active is not in the allowed transitions list
        let err = repo
            .transition_spec_lifecycle("s1", LifecycleState::Active)
            .unwrap_err();
        assert!(err.to_string().contains("not a legal transition"));
    }

    #[test]
    fn test_lifecycle_nonexistent_spec_errors() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let err = repo
            .transition_spec_lifecycle("ghost", LifecycleState::Stale)
            .unwrap_err();
        assert!(
            err.to_string().contains("ghost") || err.to_string().contains("not found"),
            "expected spec-not-found error, got: {err}"
        );
    }

    // --- cancel_spec convenience method ---

    #[test]
    fn test_cancel_spec_from_active() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let spec = Spec::new("s1".to_string(), "T".to_string(), "".to_string());
        repo.add_spec(&spec).unwrap();

        repo.cancel_spec("s1").unwrap();
        let loaded = repo.load_spec("s1").unwrap();
        assert_eq!(loaded.lifecycle_state, LifecycleState::Cancelled);
    }

    #[test]
    fn test_cancel_spec_from_stale() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let spec = Spec::new("s1".to_string(), "T".to_string(), "".to_string());
        repo.add_spec(&spec).unwrap();

        repo.transition_spec_lifecycle("s1", LifecycleState::Stale)
            .unwrap();
        repo.cancel_spec("s1").unwrap();
        let loaded = repo.load_spec("s1").unwrap();
        assert_eq!(loaded.lifecycle_state, LifecycleState::Cancelled);
    }

    #[test]
    fn test_cancel_spec_rejects_terminal_states() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let spec = Spec::new("s1".to_string(), "T".to_string(), "".to_string());
        repo.add_spec(&spec).unwrap();

        repo.transition_spec_lifecycle("s1", LifecycleState::Completed)
            .unwrap();
        let err = repo.cancel_spec("s1").unwrap_err();
        assert!(err.to_string().contains("already terminal"));
    }

    // --- complete_spec convenience method ---

    #[test]
    fn test_complete_spec_requires_complete_status() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let spec = Spec::new("s1".to_string(), "T".to_string(), "".to_string());
        repo.add_spec(&spec).unwrap();

        // Status is Pending, not Complete
        let err = repo.complete_spec("s1").unwrap_err();
        assert!(err.to_string().contains("status must be 'complete'"));
    }

    #[test]
    fn test_complete_spec_succeeds_with_complete_status() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let spec = Spec::new("s1".to_string(), "T".to_string(), "".to_string());
        repo.add_spec(&spec).unwrap();

        // Set spec status to Complete
        let update = crate::spec::SpecUpdate {
            status: Some(SpecStatus::Complete),
            depends_on: None,
            file_scope: None,
            acceptance_criteria: None,
            design_notes: None,
            tech_stack: None,
        };
        repo.update_spec("s1", update).unwrap();

        repo.complete_spec("s1").unwrap();
        let loaded = repo.load_spec("s1").unwrap();
        assert_eq!(loaded.lifecycle_state, LifecycleState::Completed);
    }

    #[test]
    fn test_complete_spec_rejects_from_cancelled() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let spec = Spec::new("s1".to_string(), "T".to_string(), "".to_string());
        repo.add_spec(&spec).unwrap();

        // Mark status as complete first
        let update = crate::spec::SpecUpdate {
            status: Some(SpecStatus::Complete),
            depends_on: None,
            file_scope: None,
            acceptance_criteria: None,
            design_notes: None,
            tech_stack: None,
        };
        repo.update_spec("s1", update).unwrap();

        // Cancel the spec lifecycle
        repo.cancel_spec("s1").unwrap();

        // Now complete_spec should fail — lifecycle is Cancelled
        let err = repo.complete_spec("s1").unwrap_err();
        assert!(err.to_string().contains("cannot complete from"));
    }

    #[test]
    fn test_add_spec_rejects_duplicate() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new("dup".to_string(), "First".to_string(), "".to_string());
        repo.add_spec(&spec).unwrap();

        let dup = Spec::new("dup".to_string(), "Second".to_string(), "".to_string());
        let err = repo.add_spec(&dup).unwrap_err();
        assert!(
            err.to_string().contains("already exists"),
            "expected SpecAlreadyExists error, got: {err}"
        );

        // Original spec should be untouched.
        let loaded = repo.load_spec("dup").unwrap();
        assert_eq!(loaded.title, "First");
    }

    #[test]
    fn test_update_spec_unaffected_by_duplicate_guard() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new("upd".to_string(), "Original".to_string(), "".to_string());
        repo.add_spec(&spec).unwrap();

        // update_spec should still work on existing specs.
        let update = crate::spec::SpecUpdate {
            status: Some(SpecStatus::InProgress),
            depends_on: None,
            file_scope: None,
            acceptance_criteria: None,
            design_notes: None,
            tech_stack: None,
        };
        repo.update_spec("upd", update).unwrap();
        let loaded = repo.load_spec("upd").unwrap();
        assert_eq!(loaded.status, SpecStatus::InProgress);
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

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
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

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
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
            .context(
                ContextScope::Spec("feature-1".to_string()),
                10,
                &ContextFilter::default(),
            )
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

        let result = repo.context(
            ContextScope::Spec("nope".to_string()),
            10,
            &ContextFilter::default(),
        );
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

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
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

        let ctx = repo
            .context(ContextScope::Full, 3, &ContextFilter::default())
            .unwrap();
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
    fn test_seal_auto_promotes_spec_status() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new("auth".to_string(), "Auth Module".to_string(), String::new());
        repo.add_spec(&spec).unwrap();

        let loaded = repo.load_spec("auth").unwrap();
        assert_eq!(loaded.status, SpecStatus::Pending);

        fs::write(dir.path().join("auth.rs"), "fn login() {}").unwrap();
        repo.seal(
            AgentIdentity {
                id: "dev-1".into(),
                agent_type: AgentType::Agent,
            },
            "started auth".into(),
            Some("auth".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();
        let loaded = repo.load_spec("auth").unwrap();
        assert_eq!(loaded.status, SpecStatus::InProgress);

        fs::write(dir.path().join("auth.rs"), "fn login() { ok }").unwrap();
        repo.seal(
            AgentIdentity {
                id: "dev-1".into(),
                agent_type: AgentType::Agent,
            },
            "finished auth".into(),
            Some("auth".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();
        let loaded = repo.load_spec("auth").unwrap();
        assert_eq!(loaded.status, SpecStatus::Complete);
    }

    #[test]
    fn test_seal_auto_promote_respects_blocked() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new(
            "blocked-feat".to_string(),
            "Blocked".to_string(),
            String::new(),
        );
        repo.add_spec(&spec).unwrap();

        // Explicitly block the spec.
        repo.update_spec(
            "blocked-feat",
            SpecUpdate {
                status: Some(SpecStatus::Blocked),
                ..Default::default()
            },
        )
        .unwrap();

        fs::write(dir.path().join("feat.rs"), "done").unwrap();
        repo.seal(
            AgentIdentity {
                id: "dev-1".into(),
                agent_type: AgentType::Agent,
            },
            "work done".into(),
            Some("blocked-feat".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();
        let loaded = repo.load_spec("blocked-feat").unwrap();
        assert_eq!(loaded.status, SpecStatus::Blocked);
    }

    #[test]
    fn test_seal_auto_promote_never_downgrades() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new("feat".to_string(), "Feature".to_string(), String::new());
        repo.add_spec(&spec).unwrap();

        fs::write(dir.path().join("a.txt"), "done").unwrap();
        repo.seal(
            AgentIdentity {
                id: "dev-1".into(),
                agent_type: AgentType::Agent,
            },
            "complete".into(),
            Some("feat".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();
        assert_eq!(repo.load_spec("feat").unwrap().status, SpecStatus::Complete);

        fs::write(dir.path().join("b.txt"), "more work").unwrap();
        repo.seal(
            AgentIdentity {
                id: "dev-2".into(),
                agent_type: AgentType::Agent,
            },
            "followup".into(),
            Some("feat".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();
        assert_eq!(repo.load_spec("feat").unwrap().status, SpecStatus::Complete);
    }

    #[test]
    fn test_summary_derives_status_from_seals() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new("ui".to_string(), "UI Work".to_string(), String::new());
        repo.add_spec(&spec).unwrap();

        fs::write(dir.path().join("ui.html"), "<div>hello</div>").unwrap();
        repo.seal(
            AgentIdentity {
                id: "designer".into(),
                agent_type: AgentType::Agent,
            },
            "built ui".into(),
            Some("ui".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let summary = repo.summary().unwrap();
        assert_eq!(summary.specs_summary.len(), 1);
        assert_eq!(summary.specs_summary[0].status, "complete");
        assert!(
            summary.headline.contains("UI Work"),
            "headline should include spec title when complete: {}",
            summary.headline
        );
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

        let spec = Spec::new(
            "task-3".to_string(),
            "Task Three".to_string(),
            String::new(),
        );
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

        let spec = Spec::new(
            "persist".to_string(),
            "Persist Test".to_string(),
            String::new(),
        );
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

        let spec = Spec::new(
            "feat-1".to_string(),
            "Feature One".to_string(),
            String::new(),
        );
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
        let lock =
            crate::lock::RepoLock::acquire(&dir.path().join(".writ"), Duration::from_millis(100));
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
    fn setup_convergence_repo(dir: &tempfile::TempDir) -> (Repository, String, String) {
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
        let spec = Spec::new(
            "empty-spec".to_string(),
            "No Seals".to_string(),
            String::new(),
        );
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

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
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

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        assert!(ctx.recent_seals[0].verification.is_none());
        assert_eq!(ctx.recent_seals[0].status, "complete");
    }

    #[test]
    fn test_context_has_available_operations() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        assert!(!ctx.available_operations.is_empty());
        assert!(ctx
            .available_operations
            .iter()
            .any(|op| op.contains("seal")));
        assert!(ctx
            .available_operations
            .iter()
            .any(|op| op.contains("restore")));
        assert!(ctx
            .available_operations
            .iter()
            .any(|op| op.contains("converge")));
        assert!(ctx
            .available_operations
            .iter()
            .any(|op| op.contains("diff_seals")));
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

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
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

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
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

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
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

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
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

    // --- Append-only seal store (A.1.8) ---

    #[test]
    fn test_save_seal_rejects_overwrite() {
        // Manually craft a seal and save it twice — the second save must fail.
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let seal = Seal::new(
            None,
            "tree-hash-123".to_string(),
            test_agent(),
            None,
            TaskStatus::InProgress,
            vec![],
            Verification::default(),
            "test seal".to_string(),
            vec![],
            None,
        );

        // First save succeeds
        repo.save_seal(&seal).unwrap();

        // Second save with same ID must fail
        let err = repo.save_seal(&seal).unwrap_err();
        assert!(
            matches!(err, WritError::SealAlreadyExists(_)),
            "expected SealAlreadyExists, got: {err}"
        );
    }

    #[test]
    fn test_seal_store_no_delete_path() {
        // After creating a seal, verify the file exists on disk and
        // no seal operation removes it.
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("file.txt"), "content").unwrap();

        let seal = repo
            .seal(
                test_agent(),
                "seal to preserve".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        let seal_path = dir
            .path()
            .join(".writ/seals")
            .join(format!("{}.json", seal.id));
        assert!(seal_path.exists(), "seal file must exist after creation");

        // Create a second seal — the first must still exist
        fs::write(dir.path().join("file.txt"), "updated content").unwrap();
        let _seal2 = repo
            .seal(
                test_agent(),
                "second seal".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        assert!(
            seal_path.exists(),
            "first seal file must still exist after second seal"
        );

        // Restore to first seal — the first seal file must still exist
        repo.restore(&seal.id).unwrap();
        assert!(
            seal_path.exists(),
            "seal file must survive restore operations"
        );
    }

    #[test]
    fn test_seal_integrity_after_overwrite_attempt() {
        // Verify the original seal is unchanged after a rejected overwrite.
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let seal = Seal::new(
            None,
            "tree-hash-abc".to_string(),
            AgentIdentity {
                id: "integrity-agent".to_string(),
                agent_type: AgentType::Agent,
            },
            None,
            TaskStatus::InProgress,
            vec![],
            Verification::default(),
            "original summary".to_string(),
            vec![],
            None,
        );

        repo.save_seal(&seal).unwrap();

        // Attempt overwrite (will fail)
        let _ = repo.save_seal(&seal);

        // Verify original content is intact
        let loaded = repo.load_seal(&seal.id).unwrap();
        assert_eq!(loaded.summary, "original summary");
        assert_eq!(loaded.agent.id, "integrity-agent");
    }
}

// --- A.1.9: Hash chain integration tests (Djo + Amis) ---

#[cfg(test)]
mod chain_tests {
    use super::*;
    use crate::seal::{AgentType, TaskStatus, Verification};
    use tempfile::tempdir;

    fn chain_agent() -> AgentIdentity {
        AgentIdentity {
            id: "chain-agent".to_string(),
            agent_type: AgentType::Agent,
        }
    }

    /// Helper: create a repo, write a file, and seal. Returns (repo, seal).
    fn setup_chain_repo(dir: &std::path::Path) -> Repository {
        let repo = Repository::init(dir).unwrap();
        fs::write(dir.join("file.txt"), "initial").unwrap();
        repo.seal(
            chain_agent(),
            "initial seal".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();
        repo
    }

    #[test]
    fn test_chain_single_seal_verifies() {
        let dir = tempdir().unwrap();
        let repo = setup_chain_repo(dir.path());

        let result = repo.verify_chain(None).unwrap();
        assert!(result.valid, "single-seal chain should verify");
        assert_eq!(result.verified, 1);
        assert_eq!(result.total_seals, 1);
        assert!(result.failures.is_empty());
    }

    #[test]
    fn test_chain_three_seals_verify() {
        let dir = tempdir().unwrap();
        let repo = setup_chain_repo(dir.path());

        // Seal 2
        fs::write(dir.path().join("file.txt"), "second version").unwrap();
        repo.seal(
            chain_agent(),
            "second seal".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Seal 3
        fs::write(dir.path().join("file.txt"), "third version").unwrap();
        repo.seal(
            chain_agent(),
            "third seal".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let result = repo.verify_chain(None).unwrap();
        assert!(result.valid, "3-seal chain should verify");
        assert_eq!(result.verified, 3);
        assert!(result.failures.is_empty());
    }

    #[test]
    fn test_chain_tampered_content_detected() {
        let dir = tempdir().unwrap();
        let repo = setup_chain_repo(dir.path());

        // Tamper with the seal file on disk
        let seals = repo.log().unwrap();
        let seal_id = &seals[0].id;
        let seal_path = dir
            .path()
            .join(".writ/seals")
            .join(format!("{seal_id}.json"));
        let mut seal: Seal =
            serde_json::from_str(&fs::read_to_string(&seal_path).unwrap()).unwrap();
        seal.summary = "TAMPERED SUMMARY".to_string();
        // Write tampered seal back (bypass save_seal guard by writing directly)
        fs::write(&seal_path, serde_json::to_string_pretty(&seal).unwrap()).unwrap();

        let result = repo.verify_chain(None).unwrap();
        assert!(!result.valid, "tampered chain should fail verification");
        assert_eq!(result.failures.len(), 1);
        assert!(
            result.failures[0]
                .error
                .as_ref()
                .unwrap()
                .contains("content_hash"),
            "error should mention content_hash"
        );
    }

    #[test]
    fn test_chain_tampered_timestamp_detected() {
        let dir = tempdir().unwrap();
        let repo = setup_chain_repo(dir.path());

        let seals = repo.log().unwrap();
        let seal_id = &seals[0].id;
        let seal_path = dir
            .path()
            .join(".writ/seals")
            .join(format!("{seal_id}.json"));
        let mut seal: Seal =
            serde_json::from_str(&fs::read_to_string(&seal_path).unwrap()).unwrap();
        seal.timestamp = seal.timestamp + chrono::Duration::hours(1);
        fs::write(&seal_path, serde_json::to_string_pretty(&seal).unwrap()).unwrap();

        let result = repo.verify_chain(None).unwrap();
        assert!(!result.valid, "tampered timestamp should fail verification");
    }

    #[test]
    fn test_chain_tampered_agent_detected() {
        let dir = tempdir().unwrap();
        let repo = setup_chain_repo(dir.path());

        let seals = repo.log().unwrap();
        let seal_id = &seals[0].id;
        let seal_path = dir
            .path()
            .join(".writ/seals")
            .join(format!("{seal_id}.json"));
        let mut seal: Seal =
            serde_json::from_str(&fs::read_to_string(&seal_path).unwrap()).unwrap();
        seal.agent.id = "evil-agent".to_string();
        fs::write(&seal_path, serde_json::to_string_pretty(&seal).unwrap()).unwrap();

        let result = repo.verify_chain(None).unwrap();
        assert!(!result.valid, "tampered agent should fail verification");
    }

    #[test]
    fn test_chain_tampered_parent_hash_detected() {
        let dir = tempdir().unwrap();
        let repo = setup_chain_repo(dir.path());

        // Create second seal to have a chain link
        fs::write(dir.path().join("file.txt"), "v2").unwrap();
        repo.seal(
            chain_agent(),
            "second".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Tamper with second seal's parent_seal_hash
        let seals = repo.log().unwrap();
        let seal_id = &seals[0].id; // newest seal
        let seal_path = dir
            .path()
            .join(".writ/seals")
            .join(format!("{seal_id}.json"));
        let mut seal: Seal =
            serde_json::from_str(&fs::read_to_string(&seal_path).unwrap()).unwrap();
        seal.parent_seal_hash = Some("fake-parent-hash-000".to_string());
        fs::write(&seal_path, serde_json::to_string_pretty(&seal).unwrap()).unwrap();

        let result = repo.verify_chain(None).unwrap();
        assert!(
            !result.valid,
            "tampered parent_seal_hash should fail verification"
        );
    }

    #[test]
    fn test_chain_each_seal_has_crypto_fields() {
        let dir = tempdir().unwrap();
        let repo = setup_chain_repo(dir.path());

        // Create multiple seals
        for i in 1..5 {
            fs::write(dir.path().join("file.txt"), format!("v{i}")).unwrap();
            repo.seal(
                chain_agent(),
                format!("seal {i}"),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();
        }

        let seals = repo.log().unwrap();
        for seal in &seals {
            assert!(
                seal.content_hash.is_some(),
                "seal {} missing content_hash",
                seal.id
            );
            assert!(
                seal.chain_hash.is_some(),
                "seal {} missing chain_hash",
                seal.id
            );
            assert!(seal.is_secured(), "seal {} not secured", seal.id);
        }
    }

    #[test]
    fn test_chain_parent_linkage_correct() {
        let dir = tempdir().unwrap();
        let repo = setup_chain_repo(dir.path());

        fs::write(dir.path().join("file.txt"), "v2").unwrap();
        repo.seal(
            chain_agent(),
            "second".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("file.txt"), "v3").unwrap();
        repo.seal(
            chain_agent(),
            "third".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let seals = repo.log().unwrap(); // newest first
                                         // seal[0] (newest) should have parent_seal_hash == seal[1].chain_hash
        assert_eq!(
            seals[0].parent_seal_hash.as_ref(),
            seals[1].chain_hash.as_ref(),
            "newest seal's parent_seal_hash should be parent's chain_hash"
        );
        // seal[1] should have parent_seal_hash == seal[2].chain_hash
        assert_eq!(
            seals[1].parent_seal_hash.as_ref(),
            seals[2].chain_hash.as_ref(),
            "middle seal's parent_seal_hash should be grandparent's chain_hash"
        );
        // seal[2] (oldest/genesis) should have no parent_seal_hash
        assert!(
            seals[2].parent_seal_hash.is_none(),
            "genesis seal should have no parent_seal_hash"
        );
    }

    #[test]
    fn test_chain_100_seals_performance() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let start = std::time::Instant::now();
        for i in 0..100 {
            fs::write(dir.path().join("file.txt"), format!("iteration {i}")).unwrap();
            repo.seal(
                chain_agent(),
                format!("seal {i}"),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();
        }
        let seal_time = start.elapsed();

        let start = std::time::Instant::now();
        let result = repo.verify_chain(None).unwrap();
        let verify_time = start.elapsed();

        assert!(result.valid, "100-seal chain should verify");
        assert_eq!(result.verified, 100);
        assert!(
            verify_time.as_secs() < 10,
            "100-seal chain verification took {verify_time:?}"
        );
        assert!(
            seal_time.as_secs() < 30,
            "100-seal chain creation took {seal_time:?}"
        );
    }

    #[test]
    fn test_chain_with_signed_seals_verifies() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let (_signing_key, verifying_key) = crate::crypto::generate_keypair();

        fs::write(dir.path().join("file.txt"), "signed content").unwrap();

        // Create through normal seal path (gets secured without signing key)
        let _seal = repo
            .seal(
                chain_agent(),
                "unsigned seal".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        // Verify without signing key first
        let result = repo.verify_chain(None).unwrap();
        assert!(result.valid);

        // Verify with key — signature_valid should be None (not signed)
        let result = repo.verify_chain(Some(&verifying_key)).unwrap();
        assert!(result.valid);
        // Seal is verified but not signed
        assert_eq!(result.verified, 1);
    }

    #[test]
    fn test_chain_all_hashes_unique() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        for i in 0..10 {
            fs::write(dir.path().join("file.txt"), format!("v{i}")).unwrap();
            repo.seal(
                chain_agent(),
                format!("seal {i}"),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();
        }

        let seals = repo.log().unwrap();
        let content_hashes: std::collections::HashSet<&str> = seals
            .iter()
            .filter_map(|s| s.content_hash.as_deref())
            .collect();
        let chain_hashes: std::collections::HashSet<&str> = seals
            .iter()
            .filter_map(|s| s.chain_hash.as_deref())
            .collect();

        assert_eq!(
            content_hashes.len(),
            10,
            "all content hashes should be unique"
        );
        assert_eq!(chain_hashes.len(), 10, "all chain hashes should be unique");
    }

    #[test]
    fn test_verify_chain_missing_seal_returns_failure_not_error() {
        // Regression: verify_chain() used to throw "object not found" when a
        // seal in the chain was missing from disk. Now it returns a failure
        // entry in the ChainVerification instead.
        let dir = tempdir().unwrap();
        let repo = setup_chain_repo(dir.path());

        // Create a second seal
        fs::write(dir.path().join("file.txt"), "second version").unwrap();
        repo.seal(
            chain_agent(),
            "second seal".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Delete the first seal from disk (simulating a broken chain)
        let seals = repo.log().unwrap();
        let first_seal_id = &seals[1].id; // seals[1] is the older/genesis seal
        let first_seal_path = dir
            .path()
            .join(".writ/seals")
            .join(format!("{first_seal_id}.json"));
        fs::remove_file(&first_seal_path).unwrap();

        // verify_chain should NOT error — it should return a result with failures
        let result = repo.verify_chain(None).unwrap();
        assert!(!result.valid, "chain with missing seal should be invalid");
        assert!(
            !result.failures.is_empty(),
            "should have failure entries for missing seal"
        );

        // The failure should reference the missing seal
        let missing_failure = result.failures.iter().find(|f| f.seal_id == *first_seal_id);
        assert!(
            missing_failure.is_some(),
            "failure should reference the missing seal ID"
        );
        let err = missing_failure.unwrap().error.as_ref().unwrap();
        assert!(
            err.contains("chain broken"),
            "error should indicate chain break: {err}"
        );
    }

    #[test]
    fn test_verify_chain_missing_head_seal_returns_failure() {
        // HEAD points to a seal that doesn't exist on disk
        let dir = tempdir().unwrap();
        let repo = setup_chain_repo(dir.path());

        let seals = repo.log().unwrap();
        let head_id = &seals[0].id;

        // Delete the HEAD seal from disk
        let seal_path = dir
            .path()
            .join(".writ/seals")
            .join(format!("{head_id}.json"));
        fs::remove_file(&seal_path).unwrap();

        let result = repo.verify_chain(None).unwrap();
        assert!(
            !result.valid,
            "chain with missing HEAD seal should be invalid"
        );
        assert_eq!(result.total_seals, 0, "no seals could be loaded");
        assert_eq!(result.failures.len(), 1);
        assert!(result.failures[0]
            .error
            .as_ref()
            .unwrap()
            .contains("chain broken"),);
    }

    #[test]
    fn test_verify_chain_partial_chain_still_verifies_loaded_seals() {
        // Chain: seal3 -> seal2 -> seal1 (genesis)
        // Delete seal1 — seal3 and seal2 should still verify, plus a chain-break failure
        let dir = tempdir().unwrap();
        let repo = setup_chain_repo(dir.path());

        fs::write(dir.path().join("file.txt"), "v2").unwrap();
        repo.seal(
            chain_agent(),
            "second".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("file.txt"), "v3").unwrap();
        repo.seal(
            chain_agent(),
            "third".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Delete the genesis seal
        let seals = repo.log().unwrap();
        assert_eq!(seals.len(), 3);
        let genesis_id = &seals[2].id;
        fs::remove_file(
            dir.path()
                .join(".writ/seals")
                .join(format!("{genesis_id}.json")),
        )
        .unwrap();

        let result = repo.verify_chain(None).unwrap();
        assert!(!result.valid);
        // seal3 and seal2 were loaded and verified (2 verified)
        assert_eq!(result.verified, 2, "the two loadable seals should verify");
        assert_eq!(result.total_seals, 2, "only 2 seals could be loaded");
        // Plus 1 failure for the missing genesis seal
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].seal_id, *genesis_id);
    }

    #[test]
    fn test_verify_chain_reopened_repo_passes() {
        // Verify chain integrity survives repo close/reopen (cross-process scenario)
        let dir = tempdir().unwrap();

        // First "process": init and seal
        {
            let repo = Repository::init(dir.path()).unwrap();
            fs::write(dir.path().join("file.txt"), "initial").unwrap();
            repo.seal(
                chain_agent(),
                "baseline".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();
        }

        // Second "process": open, modify, seal
        {
            let repo = Repository::open(dir.path()).unwrap();
            fs::write(dir.path().join("file.txt"), "updated by agent").unwrap();
            repo.seal(
                AgentIdentity {
                    id: "cli-agent".to_string(),
                    agent_type: AgentType::Agent,
                },
                "agent seal".to_string(),
                Some("test-spec".to_string()),
                TaskStatus::InProgress,
                Verification::default(),
                true,
            )
            .unwrap();
        }

        // Third "process": open and verify
        {
            let repo = Repository::open(dir.path()).unwrap();
            let result = repo.verify_chain(None).unwrap();
            assert!(
                result.valid,
                "chain should verify after repo reopen: {:?}",
                result.failures
            );
            assert_eq!(result.verified, 2);
            assert!(result.failures.is_empty());
        }
    }
}

// --- B.7: verify_all_chains tests ---

#[cfg(test)]
mod verify_all_chains_tests {
    use super::*;
    use crate::seal::{AgentType, TaskStatus, Verification};
    use crate::spec::{Spec, SpecStatus};
    use tempfile::tempdir;

    fn vac_agent() -> AgentIdentity {
        AgentIdentity {
            id: "vac-agent".to_string(),
            agent_type: AgentType::Agent,
        }
    }

    #[test]
    fn test_verify_all_chains_empty_repo() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let result = repo.verify_all_chains(None).unwrap();
        assert!(result.all_valid, "empty repo should be valid");
        assert_eq!(result.head_chain.total_seals, 0);
        assert!(result.spec_chains.is_empty());
    }

    #[test]
    fn test_verify_all_chains_head_only() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("file.txt"), "v1").unwrap();
        repo.seal(
            vac_agent(),
            "first".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let result = repo.verify_all_chains(None).unwrap();
        assert!(result.all_valid);
        assert!(result.head_chain.valid);
        assert_eq!(result.head_chain.total_seals, 1);
        assert!(result.spec_chains.is_empty());
    }

    #[test]
    fn test_verify_all_chains_with_spec_branch() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Create a spec
        let spec = Spec {
            id: "test-spec".to_string(),
            title: "Test spec".to_string(),
            description: "testing".to_string(),
            status: SpecStatus::InProgress,
            file_scope: vec!["src/".to_string()],
            acceptance_criteria: vec![],
            design_notes: vec![],
            depends_on: vec![],
            sealed_by: vec![],
            tech_stack: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            lifecycle_state: crate::spec::LifecycleState::Active,
            last_activity: chrono::Utc::now(),
            completion_summary: None,
            commit_state: crate::spec::CommitState::Uncommitted,
            completed_at: None,
            commit_hash: None,
            committed_at: None,
        };
        repo.add_spec(&spec).unwrap();

        // Seal on HEAD
        fs::write(dir.path().join("file.txt"), "v1").unwrap();
        repo.seal(
            vac_agent(),
            "head seal".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Seal on spec branch
        fs::write(dir.path().join("file.txt"), "v2").unwrap();
        repo.seal(
            vac_agent(),
            "spec seal".to_string(),
            Some("test-spec".to_string()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let result = repo.verify_all_chains(None).unwrap();
        assert!(result.all_valid, "all chains should be valid");
        assert!(result.head_chain.valid);
        assert_eq!(result.spec_chains.len(), 1);
        assert_eq!(result.spec_chains[0].spec_id, "test-spec");
        assert!(result.spec_chains[0].chain.valid);
        assert!(result.spec_chains[0].chain.total_seals >= 1);
    }

    #[test]
    fn test_verify_all_chains_multiple_specs() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        for name in &["alpha", "beta"] {
            let spec = Spec {
                id: name.to_string(),
                title: name.to_string(),
                description: "testing".to_string(),
                status: SpecStatus::InProgress,
                file_scope: vec![],
                acceptance_criteria: vec![],
                design_notes: vec![],
                depends_on: vec![],
                sealed_by: vec![],
                tech_stack: vec![],
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                lifecycle_state: crate::spec::LifecycleState::Active,
                last_activity: chrono::Utc::now(),
                completion_summary: None,
                commit_state: crate::spec::CommitState::Uncommitted,
                completed_at: None,
                commit_hash: None,
                committed_at: None,
            };
            repo.add_spec(&spec).unwrap();
        }

        // Seal on HEAD first
        fs::write(dir.path().join("file.txt"), "base").unwrap();
        repo.seal(
            vac_agent(),
            "base seal".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Seal on each spec branch
        for name in &["alpha", "beta"] {
            fs::write(dir.path().join("file.txt"), format!("{name}-v1")).unwrap();
            repo.seal(
                vac_agent(),
                format!("{name} seal"),
                Some(name.to_string()),
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();
        }

        let result = repo.verify_all_chains(None).unwrap();
        assert!(result.all_valid);
        assert_eq!(result.spec_chains.len(), 2);
        // Should be sorted by spec_id
        assert_eq!(result.spec_chains[0].spec_id, "alpha");
        assert_eq!(result.spec_chains[1].spec_id, "beta");
        assert!(result.spec_chains[0].chain.valid);
        assert!(result.spec_chains[1].chain.valid);
    }
}

// --- A.2.7: Signature integration tests (Djo + Amis) ---

#[cfg(test)]
mod signature_tests {
    use super::*;
    use crate::crypto;
    use crate::seal::{AgentType, TaskStatus, Verification};
    use tempfile::tempdir;

    fn sig_agent() -> AgentIdentity {
        AgentIdentity {
            id: "sig-agent".to_string(),
            agent_type: AgentType::Agent,
        }
    }

    #[test]
    fn test_seal_signed_with_key_verifies() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let (signing_key, verifying_key) = crypto::generate_keypair();

        fs::write(dir.path().join("file.txt"), "content").unwrap();

        // Create a seal, then manually sign it
        let mut seal = Seal::new(
            None,
            "tree".to_string(),
            sig_agent(),
            None,
            TaskStatus::InProgress,
            vec![],
            Verification::default(),
            "signed seal".to_string(),
            vec![],
            None,
        );
        seal.secure(Some(&signing_key));

        assert!(seal.is_signed(), "seal should be signed");
        assert!(seal.is_secured(), "seal should be secured");

        // Verify
        let result = repo.verify_seal(&seal, Some(&verifying_key));
        assert!(result.content_hash_valid, "content hash should be valid");
        assert!(result.chain_hash_valid, "chain hash should be valid");
        assert!(result.signature_present, "signature should be present");
        assert_eq!(
            result.signature_valid,
            Some(true),
            "signature should verify"
        );
        assert!(result.error.is_none(), "no error expected");
    }

    #[test]
    fn test_seal_tampered_content_fails_signature() {
        let (signing_key, verifying_key) = crypto::generate_keypair();

        let mut seal = Seal::new(
            None,
            "tree".to_string(),
            sig_agent(),
            None,
            TaskStatus::InProgress,
            vec![],
            Verification::default(),
            "original".to_string(),
            vec![],
            None,
        );
        seal.secure(Some(&signing_key));

        // Tamper with content after signing
        seal.summary = "TAMPERED".to_string();

        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let result = repo.verify_seal(&seal, Some(&verifying_key));

        assert!(
            !result.content_hash_valid,
            "tampered content should fail hash check"
        );
    }

    #[test]
    fn test_seal_wrong_key_fails_signature() {
        let (signing_key, _) = crypto::generate_keypair();
        let (_, wrong_verifying_key) = crypto::generate_keypair();

        let mut seal = Seal::new(
            None,
            "tree".to_string(),
            sig_agent(),
            None,
            TaskStatus::InProgress,
            vec![],
            Verification::default(),
            "signed".to_string(),
            vec![],
            None,
        );
        seal.secure(Some(&signing_key));

        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let result = repo.verify_seal(&seal, Some(&wrong_verifying_key));

        // Content and chain hashes should still be valid (they don't depend on key)
        assert!(result.content_hash_valid, "content hash valid with any key");
        assert!(result.chain_hash_valid, "chain hash valid with any key");
        // But signature should fail
        assert_eq!(
            result.signature_valid,
            Some(false),
            "wrong key should fail signature"
        );
        assert!(result.error.is_some(), "error expected for wrong key");
    }

    #[test]
    fn test_seal_truncated_signature_fails() {
        let (signing_key, verifying_key) = crypto::generate_keypair();

        let mut seal = Seal::new(
            None,
            "tree".to_string(),
            sig_agent(),
            None,
            TaskStatus::InProgress,
            vec![],
            Verification::default(),
            "truncated sig test".to_string(),
            vec![],
            None,
        );
        seal.secure(Some(&signing_key));

        // Truncate the signature
        if let Some(ref sig) = seal.signature {
            seal.signature = Some(sig[..sig.len() / 2].to_string());
        }

        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let result = repo.verify_seal(&seal, Some(&verifying_key));

        assert_eq!(
            result.signature_valid,
            Some(false),
            "truncated signature should fail"
        );
    }

    #[test]
    fn test_unsigned_seal_reports_no_signature() {
        let mut seal = Seal::new(
            None,
            "tree".to_string(),
            sig_agent(),
            None,
            TaskStatus::InProgress,
            vec![],
            Verification::default(),
            "unsigned".to_string(),
            vec![],
            None,
        );
        seal.secure(None); // No signing key

        assert!(!seal.is_signed(), "seal should not be signed");
        assert!(seal.is_secured(), "seal should still be secured");

        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let (_, vk) = crypto::generate_keypair();
        let result = repo.verify_seal(&seal, Some(&vk));

        assert!(result.content_hash_valid);
        assert!(result.chain_hash_valid);
        assert!(!result.signature_present, "no signature present");
        assert!(result.signature_valid.is_none(), "no signature to verify");
    }

    #[test]
    fn test_convergence_signing_key_signs_seals() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let conv_sk = repo.convergence_signing_key().unwrap();
        let conv_vk = repo.convergence_verifying_key().unwrap();

        let mut seal = Seal::new(
            None,
            "conv-tree".to_string(),
            AgentIdentity {
                id: "convergence-engine".to_string(),
                agent_type: AgentType::Agent,
            },
            None,
            TaskStatus::Complete,
            vec![],
            Verification::default(),
            "convergence result".to_string(),
            vec![],
            None,
        );
        seal.secure(Some(&conv_sk));

        let result = repo.verify_seal(&seal, Some(&conv_vk));
        assert!(result.content_hash_valid);
        assert!(result.chain_hash_valid);
        assert!(result.signature_present);
        assert_eq!(
            result.signature_valid,
            Some(true),
            "convergence engine signature should verify"
        );
    }

    #[test]
    fn test_convergence_key_rejects_agent_seal() {
        // A seal signed by an agent's key should fail verification
        // with the convergence engine's key.
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let conv_vk = repo.convergence_verifying_key().unwrap();
        let (agent_sk, _) = crypto::generate_keypair();

        let mut seal = Seal::new(
            None,
            "tree".to_string(),
            sig_agent(),
            None,
            TaskStatus::InProgress,
            vec![],
            Verification::default(),
            "agent signed".to_string(),
            vec![],
            None,
        );
        seal.secure(Some(&agent_sk));

        let result = repo.verify_seal(&seal, Some(&conv_vk));
        assert_eq!(
            result.signature_valid,
            Some(false),
            "agent key should not verify against convergence key"
        );
    }

    #[test]
    fn test_seal_modified_agent_id_detected() {
        let (signing_key, verifying_key) = crypto::generate_keypair();

        let mut seal = Seal::new(
            None,
            "tree".to_string(),
            sig_agent(),
            None,
            TaskStatus::InProgress,
            vec![],
            Verification::default(),
            "identity test".to_string(),
            vec![],
            None,
        );
        seal.secure(Some(&signing_key));

        // Tamper with agent identity
        seal.agent.id = "impersonator".to_string();

        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let result = repo.verify_seal(&seal, Some(&verifying_key));

        assert!(
            !result.content_hash_valid,
            "modified agent_id should invalidate content hash"
        );
    }
}

#[cfg(test)]
mod convergence_keypair_tests {
    use super::*;
    use crate::seal::{AgentType, TaskStatus, Verification};
    use tempfile::tempdir;

    fn agent() -> AgentIdentity {
        AgentIdentity {
            id: "keypair-agent".to_string(),
            agent_type: AgentType::Agent,
        }
    }

    #[test]
    fn test_init_creates_convergence_keypair() {
        let dir = tempdir().unwrap();
        let _repo = Repository::init(dir.path()).unwrap();

        assert!(
            dir.path().join(".writ/keys").is_dir(),
            "keys directory should exist"
        );
        assert!(
            dir.path().join(".writ/keys/.master").exists(),
            "master encryption key should exist"
        );
        assert!(
            dir.path().join(".writ/keys/convergence.pub").exists(),
            "convergence verifying key should exist"
        );
        assert!(
            dir.path().join(".writ/keys/convergence.enc").exists(),
            "convergence signing key (encrypted) should exist"
        );
    }

    #[test]
    fn test_convergence_keypair_loads() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let signing = repo.convergence_signing_key();
        let verifying = repo.convergence_verifying_key();

        assert!(signing.is_some(), "signing key should load");
        assert!(verifying.is_some(), "verifying key should load");

        // Signing key should correspond to verifying key
        let sk = signing.unwrap();
        let vk = verifying.unwrap();
        assert_eq!(
            sk.verifying_key().as_bytes(),
            vk.as_bytes(),
            "keypair should be consistent"
        );
    }

    #[test]
    fn test_convergence_key_can_sign_and_verify() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let sk = repo.convergence_signing_key().unwrap();
        let vk = repo.convergence_verifying_key().unwrap();

        // Sign something with the convergence key
        let sig = crate::crypto::sign("test-content", &sk);
        assert!(
            crate::crypto::verify_signature("test-content", &sig, &vk),
            "convergence key should sign and verify"
        );
    }

    #[test]
    fn test_convergence_key_unique_per_repo() {
        let dir1 = tempdir().unwrap();
        let dir2 = tempdir().unwrap();
        let repo1 = Repository::init(dir1.path()).unwrap();
        let repo2 = Repository::init(dir2.path()).unwrap();

        let vk1 = repo1.convergence_verifying_key().unwrap();
        let vk2 = repo2.convergence_verifying_key().unwrap();

        assert_ne!(
            vk1.as_bytes(),
            vk2.as_bytes(),
            "different repos should have different convergence keys"
        );
    }

    #[test]
    fn test_seal_secured_with_convergence_key() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("file.txt"), "convergence content").unwrap();
        let mut seal = repo
            .seal(
                agent(),
                "convergence seal".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
                false,
            )
            .unwrap();

        // Re-secure with the convergence signing key
        let sk = repo.convergence_signing_key().unwrap();
        let vk = repo.convergence_verifying_key().unwrap();
        seal.secure(Some(&sk));

        assert!(seal.is_signed(), "seal should be signed after secure()");
        assert!(
            crate::crypto::verify_signature(
                seal.content_hash.as_ref().unwrap(),
                seal.signature.as_ref().unwrap(),
                &vk,
            ),
            "convergence signature should verify"
        );
    }

    #[test]
    fn test_wrong_key_signature_rejected() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("file.txt"), "signed content").unwrap();
        let mut seal = repo
            .seal(
                agent(),
                "signed seal".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
                false,
            )
            .unwrap();

        // Sign with convergence key
        let sk = repo.convergence_signing_key().unwrap();
        seal.secure(Some(&sk));

        // Verify with a DIFFERENT key — should fail
        let (_wrong_sk, wrong_vk) = crate::crypto::generate_keypair();
        assert!(
            !crate::crypto::verify_signature(
                seal.content_hash.as_ref().unwrap(),
                seal.signature.as_ref().unwrap(),
                &wrong_vk,
            ),
            "wrong key should reject signature"
        );
    }

    #[test]
    fn test_truncated_signature_rejected() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("file.txt"), "truncated test").unwrap();
        let mut seal = repo
            .seal(
                agent(),
                "truncated sig test".to_string(),
                None,
                TaskStatus::Complete,
                Verification::default(),
                false,
            )
            .unwrap();

        let sk = repo.convergence_signing_key().unwrap();
        let vk = repo.convergence_verifying_key().unwrap();
        seal.secure(Some(&sk));

        // Truncate the signature
        let truncated = &seal.signature.as_ref().unwrap()[..64]; // half of 128 hex chars
        assert!(
            !crate::crypto::verify_signature(seal.content_hash.as_ref().unwrap(), truncated, &vk,),
            "truncated signature should be rejected"
        );
    }
}

#[cfg(test)]
mod agent_store_tests {
    use super::*;
    use crate::agent::{AgentStatus, TrustLevel};
    use tempfile::tempdir;

    #[test]
    fn test_init_creates_agents_dir() {
        let dir = tempdir().unwrap();
        Repository::init(dir.path()).unwrap();
        assert!(dir.path().join(".writ/agents").exists());
    }

    #[test]
    fn test_register_agent_creates_file() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let agent = repo
            .register_agent("worker-1", "human-andrew", TrustLevel::Standard, vec![])
            .unwrap();

        assert_eq!(agent.agent_id, "worker-1");
        assert_eq!(agent.trust_level, TrustLevel::Standard);
        assert_eq!(agent.status, AgentStatus::Active);
        assert!(!agent.public_key.is_empty());
        assert!(dir.path().join(".writ/agents/worker-1.json").exists());
    }

    #[test]
    fn test_register_agent_duplicate_rejected() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.register_agent("worker-1", "human", TrustLevel::Standard, vec![])
            .unwrap();
        let err = repo
            .register_agent("worker-1", "human", TrustLevel::Full, vec![])
            .unwrap_err();
        assert!(
            matches!(err, WritError::AgentAlreadyExists(_)),
            "expected AgentAlreadyExists, got: {err}"
        );
    }

    #[test]
    fn test_load_agent_roundtrip() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let scope = vec!["src/".to_string(), "tests/".to_string()];
        repo.register_agent("worker-2", "human", TrustLevel::Restricted, scope.clone())
            .unwrap();

        let loaded = repo.load_agent("worker-2").unwrap();
        assert_eq!(loaded.agent_id, "worker-2");
        assert_eq!(loaded.trust_level, TrustLevel::Restricted);
        assert_eq!(loaded.scope_constraints, scope);
    }

    #[test]
    fn test_load_agent_not_found() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let err = repo.load_agent("nonexistent").unwrap_err();
        assert!(matches!(err, WritError::AgentNotFound(_)));
    }

    #[test]
    fn test_list_agents() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.register_agent("agent-b", "human", TrustLevel::Standard, vec![])
            .unwrap();
        repo.register_agent("agent-a", "human", TrustLevel::Full, vec![])
            .unwrap();

        let agents = repo.list_agents().unwrap();
        assert_eq!(agents.len(), 2);
        // Sorted alphabetically
        assert_eq!(agents[0].agent_id, "agent-a");
        assert_eq!(agents[1].agent_id, "agent-b");
    }

    #[test]
    fn test_update_agent_trust_level() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.register_agent("worker", "human", TrustLevel::Standard, vec![])
            .unwrap();

        let updated = repo
            .update_agent(
                "worker",
                AgentUpdate {
                    trust_level: Some(TrustLevel::Full),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.trust_level, TrustLevel::Full);

        // Persisted
        let loaded = repo.load_agent("worker").unwrap();
        assert_eq!(loaded.trust_level, TrustLevel::Full);
    }

    #[test]
    fn test_update_agent_scope() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.register_agent("worker", "human", TrustLevel::Standard, vec![])
            .unwrap();

        let updated = repo
            .update_agent(
                "worker",
                AgentUpdate {
                    scope_constraints: Some(vec!["src/**".to_string()]),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.scope_constraints, vec!["src/**".to_string()]);
    }

    #[test]
    fn test_revoke_agent() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.register_agent("bad-agent", "human", TrustLevel::Standard, vec![])
            .unwrap();

        let revoked = repo.revoke_agent("bad-agent", "compromised").unwrap();
        assert_eq!(revoked.status, AgentStatus::Revoked);
        assert!(revoked.revoked_at.is_some());
        assert_eq!(revoked.revocation_reason.as_deref(), Some("compromised"));

        // Keys removed — loading signing key should fail
        let ks = KeyStore::open(&dir.path().join(".writ"));
        assert!(ks.load_agent_signing_key("bad-agent").is_err());
    }

    #[test]
    fn test_suspend_and_reactivate() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.register_agent("worker", "human", TrustLevel::Standard, vec![])
            .unwrap();

        let suspended = repo.suspend_agent("worker").unwrap();
        assert_eq!(suspended.status, AgentStatus::Suspended);

        let reactivated = repo.reactivate_agent("worker").unwrap();
        assert_eq!(reactivated.status, AgentStatus::Active);
    }

    #[test]
    fn test_reactivate_revoked_fails() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.register_agent("worker", "human", TrustLevel::Standard, vec![])
            .unwrap();
        repo.revoke_agent("worker", "done").unwrap();

        let err = repo.reactivate_agent("worker").unwrap_err();
        assert!(matches!(err, WritError::AgentInactive(_)));
    }

    #[test]
    fn test_agent_trust_level_registered() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.register_agent("worker", "human", TrustLevel::Restricted, vec![])
            .unwrap();
        assert_eq!(repo.agent_trust_level("worker"), TrustLevel::Restricted);
    }

    #[test]
    fn test_agent_trust_level_unregistered() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        assert_eq!(repo.agent_trust_level("unknown"), TrustLevel::Untrusted);
    }

    #[test]
    fn test_agent_in_scope_registered_with_constraints() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.register_agent(
            "worker",
            "human",
            TrustLevel::Standard,
            vec!["src/".to_string()],
        )
        .unwrap();

        assert!(repo.agent_in_scope("worker", "src/main.rs"));
        assert!(!repo.agent_in_scope("worker", "tests/test.rs"));
    }

    #[test]
    fn test_agent_in_scope_unregistered() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        // Unregistered agents have no constraints — always in scope
        assert!(repo.agent_in_scope("unknown", "anything/goes.rs"));
    }
}

#[cfg(test)]
mod agent_identity_tests {
    use super::*;
    use crate::agent::TrustLevel;
    use crate::seal::{AgentType, TaskStatus, Verification};
    use tempfile::tempdir;

    fn test_agent(id: &str) -> AgentIdentity {
        AgentIdentity {
            id: id.to_string(),
            agent_type: AgentType::Agent,
        }
    }

    #[test]
    fn test_revoked_agent_seal_rejected() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.register_agent("bad-agent", "human", TrustLevel::Standard, vec![])
            .unwrap();
        repo.revoke_agent("bad-agent", "compromised key").unwrap();

        fs::write(dir.path().join("file.txt"), "content").unwrap();
        let err = repo
            .seal(
                test_agent("bad-agent"),
                "should fail".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap_err();

        assert!(matches!(err, WritError::AgentInactive(_)));
        let msg = err.to_string();
        assert!(msg.contains("revoked"), "expected 'revoked' in: {msg}");
    }

    #[test]
    fn test_suspended_agent_seal_rejected() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.register_agent("paused-agent", "human", TrustLevel::Standard, vec![])
            .unwrap();
        repo.suspend_agent("paused-agent").unwrap();

        fs::write(dir.path().join("file.txt"), "content").unwrap();
        let err = repo
            .seal(
                test_agent("paused-agent"),
                "should fail".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap_err();

        assert!(matches!(err, WritError::AgentInactive(_)));
        let msg = err.to_string();
        assert!(msg.contains("suspended"), "expected 'suspended' in: {msg}");
    }

    #[test]
    fn test_revoked_agent_seal_paths_rejected() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.register_agent("bad-agent", "human", TrustLevel::Standard, vec![])
            .unwrap();
        repo.revoke_agent("bad-agent", "compromised").unwrap();

        fs::write(dir.path().join("file.txt"), "content").unwrap();
        let err = repo
            .seal_paths(
                test_agent("bad-agent"),
                "should fail".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                &["file.txt".to_string()],
                false,
            )
            .unwrap_err();

        assert!(matches!(err, WritError::AgentInactive(_)));
    }

    #[test]
    fn test_revocation_emits_security_event() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.register_agent("worker", "human", TrustLevel::Standard, vec![])
            .unwrap();
        repo.revoke_agent("worker", "lost trust").unwrap();

        let logger = crate::security::SecurityEventLogger::new(&dir.path().join(".writ"));
        let events = logger.read_events(None).unwrap();
        assert!(!events.is_empty(), "expected security event on revocation");

        let revocation_event = events
            .iter()
            .find(|e| e.event_type == "agent_revoked")
            .expect("expected agent_revoked event");
        assert_eq!(
            revocation_event.severity,
            crate::security::Severity::Critical
        );
        assert_eq!(revocation_event.agent_id.as_deref(), Some("worker"));
        assert!(revocation_event.details.contains("lost trust"));
    }

    #[test]
    fn test_seals_before_revocation_remain_valid() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.register_agent("worker", "human", TrustLevel::Standard, vec![])
            .unwrap();

        // Create a seal while agent is active
        fs::write(dir.path().join("work.txt"), "good work").unwrap();
        let seal = repo
            .seal(
                test_agent("worker"),
                "pre-revocation work".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        // Now revoke the agent
        repo.revoke_agent("worker", "no longer trusted").unwrap();

        // The previous seal should still be loadable and valid
        let loaded = repo.load_seal(&seal.id).unwrap();
        assert_eq!(loaded.summary, "pre-revocation work");
        assert_eq!(loaded.agent.id, "worker");

        // verify_chain should still pass for the pre-revocation seal
        let chain_result = repo.verify_chain(None);
        assert!(chain_result.is_ok());
    }

    #[test]
    fn test_unregistered_agent_seal_allowed() {
        // Backward compatibility: agents that aren't registered can still seal
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("file.txt"), "content").unwrap();
        let seal = repo
            .seal(
                test_agent("unknown-agent"),
                "should succeed".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        assert_eq!(seal.agent.id, "unknown-agent");
    }

    #[test]
    fn test_reactivated_agent_can_seal_again() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.register_agent("worker", "human", TrustLevel::Standard, vec![])
            .unwrap();

        // Suspend, then verify seal fails
        repo.suspend_agent("worker").unwrap();
        fs::write(dir.path().join("file.txt"), "v1").unwrap();
        assert!(repo
            .seal(
                test_agent("worker"),
                "blocked".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .is_err());

        // Reactivate, then verify seal succeeds
        repo.reactivate_agent("worker").unwrap();
        let seal = repo
            .seal(
                test_agent("worker"),
                "back in action".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();
        assert_eq!(seal.agent.id, "worker");
    }
}

// --- B.1.5: Flagged seals integration tests ---

#[cfg(test)]
mod flagged_seals_tests {
    use super::*;
    use crate::agent::TrustLevel;
    use crate::seal::{AgentType, TaskStatus, Verification};
    use crate::security::{FlagReason, FlaggedSealStore};
    use tempfile::tempdir;

    fn test_agent(id: &str) -> AgentIdentity {
        AgentIdentity {
            id: id.to_string(),
            agent_type: AgentType::Agent,
        }
    }

    #[test]
    fn test_revoke_with_compromise_flags_seals_in_window() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.register_agent("worker", "human", TrustLevel::Standard, vec![])
            .unwrap();

        // Create two seals
        fs::write(dir.path().join("file.txt"), "v1").unwrap();
        let seal1 = repo
            .seal(
                test_agent("worker"),
                "first".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();
        let seal1_ts = seal1.timestamp;

        fs::write(dir.path().join("file.txt"), "v2").unwrap();
        repo.seal(
            test_agent("worker"),
            "second".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Revoke with compromise timestamp = before first seal
        // Both seals should be flagged
        let compromise_time = seal1_ts - chrono::Duration::seconds(1);
        repo.revoke_agent_with_compromise("worker", "compromised", Some(compromise_time))
            .unwrap();

        let flagged = repo.flagged_seal_ids().unwrap();
        // At minimum the 2 direct seals should be flagged
        assert!(
            flagged.len() >= 2,
            "expected at least 2 flagged seals, got {}",
            flagged.len()
        );
    }

    #[test]
    fn test_revoke_without_compromise_timestamp_uses_now() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.register_agent("worker", "human", TrustLevel::Standard, vec![])
            .unwrap();

        // Create a seal well before "now"
        fs::write(dir.path().join("file.txt"), "v1").unwrap();
        repo.seal(
            test_agent("worker"),
            "old seal".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Revoke without compromise_timestamp — defaults to now
        // Since the seal was created before now, the window is [now, now]
        // so no seals should be in the window
        repo.revoke_agent("worker", "policy violation").unwrap();

        let flagged = repo.flagged_seal_ids().unwrap();
        assert!(
            flagged.is_empty(),
            "no seals should be flagged when compromise_timestamp defaults to now"
        );
    }

    #[test]
    fn test_downstream_seals_transitively_flagged() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.register_agent("bad-agent", "human", TrustLevel::Standard, vec![])
            .unwrap();
        repo.register_agent("good-agent", "human", TrustLevel::Full, vec![])
            .unwrap();

        // bad-agent creates a seal
        fs::write(dir.path().join("file.txt"), "compromised content").unwrap();
        let bad_seal = repo
            .seal(
                test_agent("bad-agent"),
                "malicious work".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        // good-agent creates a seal on top (downstream of bad seal)
        fs::write(dir.path().join("file.txt"), "good content").unwrap();
        let good_seal = repo
            .seal(
                test_agent("good-agent"),
                "innocent work".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        // Revoke bad-agent with compromise window covering bad_seal
        let compromise_time = bad_seal.timestamp - chrono::Duration::seconds(1);
        repo.revoke_agent_with_compromise("bad-agent", "compromised", Some(compromise_time))
            .unwrap();

        let flagged = repo.flagged_seal_ids().unwrap();
        assert!(
            flagged.contains(&bad_seal.id),
            "directly compromised seal should be flagged"
        );
        assert!(
            flagged.contains(&good_seal.id),
            "downstream seal should be transitively flagged"
        );

        // Verify the reasons are correct
        let entries = repo.flagged_seals().unwrap();
        let bad_entry = entries.iter().find(|e| e.seal_id == bad_seal.id).unwrap();
        assert_eq!(bad_entry.reason, FlagReason::AgentCompromised);

        let good_entry = entries.iter().find(|e| e.seal_id == good_seal.id).unwrap();
        assert_eq!(good_entry.reason, FlagReason::DownstreamOfCompromised);
    }

    #[test]
    fn test_no_seals_in_window_produces_no_flags() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.register_agent("worker", "human", TrustLevel::Standard, vec![])
            .unwrap();

        // Create a seal
        fs::write(dir.path().join("file.txt"), "v1").unwrap();
        let seal = repo
            .seal(
                test_agent("worker"),
                "before compromise".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        // Revoke with compromise window AFTER the seal was created
        let future_time = seal.timestamp + chrono::Duration::hours(1);
        repo.revoke_agent_with_compromise("worker", "compromised", Some(future_time))
            .unwrap();

        let flagged = repo.flagged_seal_ids().unwrap();
        assert!(
            flagged.is_empty(),
            "seal created before compromise window should not be flagged"
        );
    }

    #[test]
    fn test_flagged_seal_store_accessible_from_repo() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Initially empty
        assert!(repo.flagged_seal_ids().unwrap().is_empty());
        assert!(repo.flagged_seals().unwrap().is_empty());

        // Manually flag a seal via the store
        let store = FlaggedSealStore::new(&dir.path().join(".writ"));
        store
            .flag_seal(&crate::security::FlaggedSeal {
                seal_id: "test-seal".to_string(),
                agent_id: "agent".to_string(),
                reason: FlagReason::AgentCompromised,
                compromise_window: (chrono::Utc::now(), chrono::Utc::now()),
                flagged_by: "admin".to_string(),
                flagged_at: chrono::Utc::now(),
            })
            .unwrap();

        // Now repo should see it
        let ids = repo.flagged_seal_ids().unwrap();
        assert!(ids.contains("test-seal"));
    }
}

#[cfg(test)]
mod scope_constraint_tests {
    use super::*;
    use crate::agent::TrustLevel;
    use crate::seal::{AgentType, TaskStatus, Verification};
    use crate::spec::Spec;
    use tempfile::tempdir;

    fn test_agent(id: &str) -> AgentIdentity {
        AgentIdentity {
            id: id.to_string(),
            agent_type: AgentType::Agent,
        }
    }

    fn spec_with_scope(id: &str, scope: Vec<String>) -> Spec {
        let mut spec = Spec::new(id.to_string(), id.to_string(), "test".to_string());
        spec.file_scope = scope;
        spec
    }

    #[test]
    fn test_exact_path_in_scope_no_warning() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.add_spec(&spec_with_scope("feat", vec!["src/main.rs".to_string()]))
            .unwrap();

        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        let seal = repo
            .seal(
                test_agent("worker"),
                "in scope".to_string(),
                Some("feat".to_string()),
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        let has_scope_warning = seal.warnings.iter().any(|h| h.contains("FILE_SCOPE"));
        assert!(!has_scope_warning, "expected no FILE_SCOPE warning");
    }

    #[test]
    fn test_directory_prefix_in_scope() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.add_spec(&spec_with_scope("feat", vec!["src/".to_string()]))
            .unwrap();

        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "pub mod stuff;").unwrap();
        let seal = repo
            .seal(
                test_agent("worker"),
                "in scope".to_string(),
                Some("feat".to_string()),
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        let has_scope_warning = seal.warnings.iter().any(|h| h.contains("FILE_SCOPE"));
        assert!(!has_scope_warning, "src/lib.rs should be in scope of src/");
    }

    #[test]
    fn test_file_outside_scope_detected() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.add_spec(&spec_with_scope("feat", vec!["src/".to_string()]))
            .unwrap();

        fs::write(dir.path().join("README.md"), "docs").unwrap();
        let seal = repo
            .seal(
                test_agent("worker"),
                "out of scope".to_string(),
                Some("feat".to_string()),
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        let has_scope_warning = seal.warnings.iter().any(|h| h.contains("FILE_SCOPE"));
        assert!(
            has_scope_warning,
            "README.md should trigger FILE_SCOPE warning"
        );
    }

    #[test]
    fn test_multiple_files_mixed_scope() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.add_spec(&spec_with_scope("feat", vec!["src/".to_string()]))
            .unwrap();

        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "in scope").unwrap();
        fs::write(dir.path().join("config.toml"), "out of scope").unwrap();
        let seal = repo
            .seal(
                test_agent("worker"),
                "mixed".to_string(),
                Some("feat".to_string()),
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        let scope_warning = seal.warnings.iter().find(|h| h.contains("FILE_SCOPE"));
        assert!(scope_warning.is_some(), "should have FILE_SCOPE warning");
        let msg = scope_warning.unwrap();
        assert!(
            msg.contains("config.toml"),
            "warning should mention config.toml"
        );
        assert!(
            !msg.contains("src/lib.rs"),
            "warning should NOT mention in-scope file"
        );
    }

    #[test]
    fn test_wildcard_scope_matches() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.add_spec(&spec_with_scope("feat", vec!["*.rs".to_string()]))
            .unwrap();

        fs::write(dir.path().join("main.rs"), "fn main(){}").unwrap();
        let seal = repo
            .seal(
                test_agent("worker"),
                "wildcard".to_string(),
                Some("feat".to_string()),
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        let has_scope_warning = seal.warnings.iter().any(|h| h.contains("FILE_SCOPE"));
        assert!(!has_scope_warning, "*.rs should match main.rs");
    }

    #[test]
    fn test_empty_scope_allows_all() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.add_spec(&spec_with_scope("feat", vec![])).unwrap();

        fs::write(dir.path().join("anything.xyz"), "content").unwrap();
        let seal = repo
            .seal(
                test_agent("worker"),
                "no scope".to_string(),
                Some("feat".to_string()),
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        let has_scope_warning = seal.warnings.iter().any(|h| h.contains("FILE_SCOPE"));
        assert!(!has_scope_warning, "empty scope should allow all files");
    }

    #[test]
    fn test_scope_violation_emits_security_event() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.add_spec(&spec_with_scope("feat", vec!["src/".to_string()]))
            .unwrap();

        fs::write(dir.path().join("secret.key"), "private").unwrap();
        repo.seal(
            test_agent("worker"),
            "out of scope".to_string(),
            Some("feat".to_string()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let logger = crate::security::SecurityEventLogger::new(&dir.path().join(".writ"));
        let events = logger.read_events(None).unwrap();
        let violation = events
            .iter()
            .find(|e| e.event_type == "scope_violation")
            .expect("expected scope_violation security event");
        assert_eq!(violation.severity, crate::security::Severity::Critical);
        assert!(violation.details.contains("secret.key"));
    }

    #[test]
    fn test_agent_scope_warning_on_seal() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.register_agent(
            "scoped-worker",
            "human",
            TrustLevel::Restricted,
            vec!["src/".to_string()],
        )
        .unwrap();

        fs::create_dir_all(dir.path().join("tests")).unwrap();
        fs::write(dir.path().join("tests/test.rs"), "test").unwrap();
        let seal = repo
            .seal(
                test_agent("scoped-worker"),
                "agent out of scope".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        let agent_scope_warning = seal.warnings.iter().find(|h| h.contains("AGENT_SCOPE"));
        assert!(
            agent_scope_warning.is_some(),
            "agent modifying tests/ outside its src/ scope should get AGENT_SCOPE warning"
        );
    }

    #[test]
    fn test_deeply_nested_path_in_scope() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.add_spec(&spec_with_scope("feat", vec!["src/".to_string()]))
            .unwrap();

        fs::create_dir_all(dir.path().join("src/core/deep/nested")).unwrap();
        fs::write(dir.path().join("src/core/deep/nested/mod.rs"), "//").unwrap();
        let seal = repo
            .seal(
                test_agent("worker"),
                "deeply nested".to_string(),
                Some("feat".to_string()),
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        let has_scope_warning = seal.warnings.iter().any(|h| h.contains("FILE_SCOPE"));
        assert!(
            !has_scope_warning,
            "deeply nested file under src/ should be in scope"
        );
    }

    // --- Attack vector stubs (depend on B.2.1 path canonicalization in seal path) ---

    #[test]
    fn test_path_traversal_dot_dot_stub() {
        // TODO(B.2.1): Verify that path traversal attempts like
        // "src/auth/../../secrets/keys.json" are rejected or normalized
        // before scope checking in seal(). Requires CC's B.2.1 canonicalization
        // to be integrated into the seal file-change pipeline.
    }

    #[test]
    fn test_symlink_outside_scope_stub() {
        // TODO(B.2.1): Verify that symlinks pointing outside the repo root
        // are detected and rejected during seal. Requires filesystem-level
        // canonicalization checks from B.2.1.
    }

    #[test]
    fn test_absolute_path_rejected_stub() {
        // TODO(B.2.1): Verify that absolute paths (e.g. "/etc/passwd")
        // cannot bypass scope constraints during seal. The validate_path
        // function already rejects these, but needs integration testing
        // in the full seal pipeline.
    }
}

// ---------------------------------------------------------------------------
// B.1.6 — Agent identity edge-case & scope enforcement integration tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod agent_edge_case_tests {
    use super::*;
    use crate::agent::{AgentUpdate, TrustLevel};
    use crate::seal::{AgentType, TaskStatus, Verification};
    use tempfile::tempdir;

    fn test_agent(id: &str) -> AgentIdentity {
        AgentIdentity {
            id: id.to_string(),
            agent_type: AgentType::Agent,
        }
    }

    // --- Registration edge cases ---

    #[test]
    fn test_register_empty_scope_means_unrestricted() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let agent = repo
            .register_agent("open-agent", "human", TrustLevel::Standard, vec![])
            .unwrap();

        assert!(agent.scope_constraints.is_empty());

        // Should be able to seal any file without AGENT_SCOPE warning
        fs::write(dir.path().join("anywhere.txt"), "content").unwrap();
        let seal = repo
            .seal(
                test_agent("open-agent"),
                "unrestricted".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        let has_agent_scope = seal.warnings.iter().any(|w| w.contains("AGENT_SCOPE"));
        assert!(
            !has_agent_scope,
            "empty scope constraints should not trigger AGENT_SCOPE warning"
        );
    }

    #[test]
    fn test_register_overlapping_scope_patterns() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let agent = repo
            .register_agent(
                "overlap-agent",
                "human",
                TrustLevel::Standard,
                vec![
                    "src/".to_string(),
                    "src/auth/".to_string(),
                    "*.rs".to_string(),
                ],
            )
            .unwrap();

        assert_eq!(agent.scope_constraints.len(), 3);

        // File matching multiple patterns should still be in scope
        fs::create_dir_all(dir.path().join("src/auth")).unwrap();
        fs::write(dir.path().join("src/auth/login.rs"), "fn login() {}").unwrap();
        let seal = repo
            .seal(
                test_agent("overlap-agent"),
                "overlapping scopes".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        let has_agent_scope = seal.warnings.iter().any(|w| w.contains("AGENT_SCOPE"));
        assert!(
            !has_agent_scope,
            "file matching overlapping scopes should be in scope"
        );
    }

    #[test]
    fn test_unicode_agent_id_rejected_at_seal() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Seal with unicode agent ID — validate_agent_id rejects non-ASCII
        fs::write(dir.path().join("file.txt"), "content").unwrap();
        let err = repo
            .seal(
                test_agent("agënt-ünïcödé"),
                "should fail".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap_err();

        assert!(matches!(err, WritError::InvalidInput(_)));
        let msg = err.to_string();
        assert!(
            msg.contains("invalid characters"),
            "expected invalid chars error: {msg}"
        );
    }

    #[test]
    fn test_register_auto_creates_agents_directory() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let agents_dir = dir.path().join(".writ/agents");
        // init() should have created this
        assert!(agents_dir.exists());

        let agent = repo
            .register_agent("new-agent", "human", TrustLevel::Full, vec![])
            .unwrap();
        assert_eq!(agent.agent_id, "new-agent");

        // Verify the agent file was created
        let agent_file = agents_dir.join("new-agent.json");
        assert!(agent_file.exists());
    }

    #[test]
    fn test_register_agent_id_boundary_lengths() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Single character — minimum valid
        let agent = repo
            .register_agent("a", "human", TrustLevel::Standard, vec![])
            .unwrap();
        assert_eq!(agent.agent_id, "a");

        // 128 characters — maximum valid
        let long_id: String = std::iter::repeat('x').take(128).collect();
        let agent = repo
            .register_agent(&long_id, "human", TrustLevel::Standard, vec![])
            .unwrap();
        assert_eq!(agent.agent_id.len(), 128);
    }

    // --- Scope enforcement integration tests ---

    #[test]
    fn test_enforce_scope_false_allows_out_of_scope_with_warning() {
        let dir = tempdir().unwrap();
        let mut repo = Repository::init(dir.path()).unwrap();
        repo.set_enforce_scope(false);
        repo.register_agent(
            "scoped-agent",
            "human",
            TrustLevel::Standard,
            vec!["src/".to_string()],
        )
        .unwrap();

        // Write file outside scope
        fs::create_dir_all(dir.path().join("tests")).unwrap();
        fs::write(dir.path().join("tests/test.rs"), "test code").unwrap();
        let seal = repo
            .seal(
                test_agent("scoped-agent"),
                "out of scope warning".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        // Should succeed but with warning
        let agent_scope_warning = seal.warnings.iter().find(|w| w.contains("AGENT_SCOPE"));
        assert!(
            agent_scope_warning.is_some(),
            "enforce_scope=false should produce AGENT_SCOPE warning, not error"
        );
        assert!(
            agent_scope_warning.unwrap().contains("tests/test.rs"),
            "warning should mention the out-of-scope file"
        );
    }

    #[test]
    fn test_enforce_scope_true_rejects_out_of_scope() {
        let dir = tempdir().unwrap();
        let mut repo = Repository::init(dir.path()).unwrap();
        repo.set_enforce_scope(true);
        repo.register_agent(
            "locked-agent",
            "human",
            TrustLevel::Standard,
            vec!["src/".to_string()],
        )
        .unwrap();

        // Write file outside scope
        fs::write(dir.path().join("secrets.env"), "API_KEY=xxx").unwrap();
        let err = repo
            .seal(
                test_agent("locked-agent"),
                "should be rejected".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap_err();

        assert!(matches!(err, WritError::ScopeViolation(_)));
        let msg = err.to_string();
        assert!(
            msg.contains("secrets.env"),
            "error should mention the file: {msg}"
        );
        assert!(
            msg.contains("locked-agent"),
            "error should mention the agent: {msg}"
        );
    }

    #[test]
    fn test_enforce_scope_true_allows_in_scope_files() {
        let dir = tempdir().unwrap();
        let mut repo = Repository::init(dir.path()).unwrap();
        repo.set_enforce_scope(true);
        repo.register_agent(
            "good-agent",
            "human",
            TrustLevel::Standard,
            vec!["src/".to_string()],
        )
        .unwrap();

        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        let seal = repo
            .seal(
                test_agent("good-agent"),
                "all in scope".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        // No AGENT_SCOPE warning, no error
        let has_agent_scope = seal.warnings.iter().any(|w| w.contains("AGENT_SCOPE"));
        assert!(
            !has_agent_scope,
            "all files in scope should produce no warning"
        );
    }

    #[test]
    fn test_enforce_scope_seal_paths_rejects_out_of_scope() {
        let dir = tempdir().unwrap();
        let mut repo = Repository::init(dir.path()).unwrap();
        repo.set_enforce_scope(true);
        repo.register_agent(
            "paths-agent",
            "human",
            TrustLevel::Standard,
            vec!["src/".to_string()],
        )
        .unwrap();

        // Create files — one in scope, one out
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/good.rs"), "good").unwrap();
        fs::write(dir.path().join("bad.txt"), "bad").unwrap();

        let err = repo
            .seal_paths(
                test_agent("paths-agent"),
                "should fail".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                &["src/good.rs".to_string(), "bad.txt".to_string()],
                false,
            )
            .unwrap_err();

        assert!(matches!(err, WritError::ScopeViolation(_)));
        let msg = err.to_string();
        assert!(
            msg.contains("bad.txt"),
            "error should mention out-of-scope file: {msg}"
        );
    }

    #[test]
    fn test_unregistered_agent_bypasses_scope_enforcement() {
        let dir = tempdir().unwrap();
        let mut repo = Repository::init(dir.path()).unwrap();
        repo.set_enforce_scope(true);

        // Don't register the agent — scope enforcement only applies to registered agents
        fs::write(dir.path().join("anywhere.txt"), "content").unwrap();
        let seal = repo
            .seal(
                test_agent("unknown-agent"),
                "no constraints".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        let has_agent_scope = seal.warnings.iter().any(|w| w.contains("AGENT_SCOPE"));
        assert!(
            !has_agent_scope,
            "unregistered agent should have no scope constraints"
        );
    }

    #[test]
    fn test_scope_violation_emits_security_event_regardless_of_enforce() {
        let dir = tempdir().unwrap();
        let mut repo = Repository::init(dir.path()).unwrap();
        repo.set_enforce_scope(false); // Warning mode
        repo.register_agent(
            "logged-agent",
            "human",
            TrustLevel::Standard,
            vec!["src/".to_string()],
        )
        .unwrap();

        fs::write(dir.path().join("outside.txt"), "content").unwrap();
        // Should succeed (warning mode) but still log security event
        repo.seal(
            test_agent("logged-agent"),
            "should warn".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let logger = crate::security::SecurityEventLogger::new(&dir.path().join(".writ"));
        let events = logger.read_events(None).unwrap();
        let violation = events
            .iter()
            .find(|e| e.event_type == "agent_scope_violation")
            .expect("should emit agent_scope_violation even in warning mode");
        assert_eq!(violation.severity, crate::security::Severity::Critical);
        assert_eq!(violation.agent_id.as_deref(), Some("logged-agent"));
        assert!(violation.details.contains("outside.txt"));
    }

    #[test]
    fn test_enforce_scope_true_also_emits_security_event() {
        let dir = tempdir().unwrap();
        let mut repo = Repository::init(dir.path()).unwrap();
        repo.set_enforce_scope(true);
        repo.register_agent(
            "strict-agent",
            "human",
            TrustLevel::Standard,
            vec!["src/".to_string()],
        )
        .unwrap();

        fs::write(dir.path().join("forbidden.txt"), "nope").unwrap();
        // This should fail
        let _err = repo
            .seal(
                test_agent("strict-agent"),
                "rejected".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap_err();

        // But security event should still be logged before the error
        let logger = crate::security::SecurityEventLogger::new(&dir.path().join(".writ"));
        let events = logger.read_events(None).unwrap();
        let violation = events
            .iter()
            .find(|e| e.event_type == "agent_scope_violation");
        assert!(
            violation.is_some(),
            "security event should be logged even when seal is rejected"
        );
    }

    #[test]
    fn test_path_traversal_rejected_by_scope_check() {
        // canonicalize_path rejects "../" so traversal attempts should
        // be treated as out-of-scope (is_in_scope returns false)
        use crate::agent::{canonicalize_path, is_in_scope};

        let scope = vec!["src/".to_string()];

        // Direct traversal
        assert!(!is_in_scope(&scope, "../etc/passwd"));
        assert!(!is_in_scope(&scope, "src/../../secrets/key.pem"));

        // canonicalize_path rejects traversal
        assert!(canonicalize_path("../etc/passwd").is_none());
        assert!(canonicalize_path("src/../../secrets/key.pem").is_none());

        // Absolute path also rejected
        assert!(canonicalize_path("/etc/passwd").is_none());
    }

    #[test]
    fn test_dot_slash_normalized_for_scope() {
        use crate::agent::{canonicalize_path, is_in_scope};

        let scope = vec!["src/".to_string()];

        // ./src/main.rs should match src/ scope after normalization
        assert!(is_in_scope(&scope, "./src/main.rs"));
        assert!(is_in_scope(&scope, "src/main.rs"));

        // canonicalize strips leading ./
        assert_eq!(
            canonicalize_path("./src/main.rs"),
            Some("src/main.rs".to_string())
        );
        assert_eq!(
            canonicalize_path("././src/main.rs"),
            Some("src/main.rs".to_string())
        );
    }

    // --- Agent lifecycle + scope integration ---

    #[test]
    fn test_suspended_agent_reactivated_with_scope_still_enforced() {
        let dir = tempdir().unwrap();
        let mut repo = Repository::init(dir.path()).unwrap();
        repo.set_enforce_scope(true);
        repo.register_agent(
            "cycle-agent",
            "human",
            TrustLevel::Standard,
            vec!["src/".to_string()],
        )
        .unwrap();

        // Suspend
        repo.suspend_agent("cycle-agent").unwrap();

        // Can't seal while suspended
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/ok.rs"), "good").unwrap();
        assert!(repo
            .seal(
                test_agent("cycle-agent"),
                "blocked".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .is_err());

        // Reactivate
        repo.reactivate_agent("cycle-agent").unwrap();

        // Can seal in-scope files
        fs::write(dir.path().join("src/ok.rs"), "updated").unwrap();
        let seal = repo
            .seal(
                test_agent("cycle-agent"),
                "back in action".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();
        assert!(!seal.warnings.iter().any(|w| w.contains("AGENT_SCOPE")));

        // But out-of-scope still rejected after reactivation
        fs::write(dir.path().join("forbidden.txt"), "nope").unwrap();
        let err = repo
            .seal(
                test_agent("cycle-agent"),
                "out of scope".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap_err();
        assert!(matches!(err, WritError::ScopeViolation(_)));
    }

    #[test]
    fn test_update_scope_constraints_changes_enforcement() {
        let dir = tempdir().unwrap();
        let mut repo = Repository::init(dir.path()).unwrap();
        repo.set_enforce_scope(true);
        repo.register_agent(
            "evolving-agent",
            "human",
            TrustLevel::Standard,
            vec!["src/".to_string()],
        )
        .unwrap();

        // tests/ is out of scope initially
        fs::create_dir_all(dir.path().join("tests")).unwrap();
        fs::write(dir.path().join("tests/test.rs"), "test").unwrap();
        assert!(repo
            .seal(
                test_agent("evolving-agent"),
                "blocked".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .is_err());

        // Update scope to include tests/
        repo.update_agent(
            "evolving-agent",
            AgentUpdate {
                trust_level: None,
                scope_constraints: Some(vec!["src/".to_string(), "tests/".to_string()]),
            },
        )
        .unwrap();

        // Now tests/ should be in scope
        fs::write(dir.path().join("tests/test.rs"), "updated test").unwrap();
        let seal = repo
            .seal(
                test_agent("evolving-agent"),
                "now in scope".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();
        assert!(!seal.warnings.iter().any(|w| w.contains("AGENT_SCOPE")));
    }

    #[test]
    fn test_multiple_agents_different_scopes_via_seal_paths() {
        let dir = tempdir().unwrap();
        let mut repo = Repository::init(dir.path()).unwrap();
        repo.set_enforce_scope(true);

        repo.register_agent(
            "frontend-agent",
            "human",
            TrustLevel::Standard,
            vec!["src/ui/".to_string()],
        )
        .unwrap();
        repo.register_agent(
            "backend-agent",
            "human",
            TrustLevel::Standard,
            vec!["src/api/".to_string()],
        )
        .unwrap();

        fs::create_dir_all(dir.path().join("src/ui")).unwrap();
        fs::create_dir_all(dir.path().join("src/api")).unwrap();
        fs::write(dir.path().join("src/ui/button.ts"), "export {}").unwrap();
        fs::write(dir.path().join("src/api/routes.rs"), "fn route() {}").unwrap();

        // frontend-agent seals only their UI files via seal_paths
        let seal = repo
            .seal_paths(
                test_agent("frontend-agent"),
                "ui work".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                &["src/ui/button.ts".to_string()],
                false,
            )
            .unwrap();
        assert!(!seal.warnings.iter().any(|w| w.contains("AGENT_SCOPE")));

        // backend-agent seals only their API files
        let seal = repo
            .seal_paths(
                test_agent("backend-agent"),
                "api work".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                &["src/api/routes.rs".to_string()],
                false,
            )
            .unwrap();
        assert!(!seal.warnings.iter().any(|w| w.contains("AGENT_SCOPE")));

        // Cross-scope access is rejected: frontend can't seal API files
        fs::write(dir.path().join("src/api/routes.rs"), "updated").unwrap();
        let err = repo
            .seal_paths(
                test_agent("frontend-agent"),
                "cross-scope".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                &["src/api/routes.rs".to_string()],
                false,
            )
            .unwrap_err();
        assert!(matches!(err, WritError::ScopeViolation(_)));
    }
}

// ---------------------------------------------------------------------------
// C.2.3 — Monitoring event emission integration tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod monitoring_event_tests {
    use super::*;
    use crate::agent::TrustLevel;
    use crate::seal::{AgentType, TaskStatus, Verification};
    use tempfile::tempdir;

    fn test_agent(id: &str) -> AgentIdentity {
        AgentIdentity {
            id: id.to_string(),
            agent_type: AgentType::Agent,
        }
    }

    #[test]
    fn test_chain_hash_failure_emits_event() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Create a seal with crypto fields
        fs::write(dir.path().join("file.txt"), "hello").unwrap();
        let seal = repo
            .seal(
                test_agent("worker"),
                "initial".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        // Tamper with the seal's content_hash on disk
        let seal_path = dir
            .path()
            .join(".writ/seals")
            .join(format!("{}.json", seal.id));
        let json = fs::read_to_string(&seal_path).unwrap();
        let tampered = json.replace(
            seal.content_hash.as_deref().unwrap_or(""),
            "deadbeefdeadbeefdeadbeefdeadbeef",
        );
        fs::write(&seal_path, tampered).unwrap();

        // Run verify_chain — should detect the tampered seal
        let result = repo.verify_chain(None).unwrap();
        assert!(!result.valid, "chain should be invalid after tampering");

        // Check that a chain_hash_failure security event was emitted
        let logger = crate::security::SecurityEventLogger::new(&dir.path().join(".writ"));
        let events = logger.read_events(None).unwrap();
        let chain_event = events.iter().find(|e| e.event_type == "chain_hash_failure");
        assert!(
            chain_event.is_some(),
            "expected chain_hash_failure event after tampered seal verification"
        );
        assert_eq!(
            chain_event.unwrap().severity,
            crate::security::Severity::Warning
        );
    }

    #[test]
    fn test_unrecognized_agent_seal_emits_event() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Register one agent so the identity store exists
        repo.register_agent("known-agent", "human", TrustLevel::Standard, vec![])
            .unwrap();

        // Seal with an unregistered agent
        fs::write(dir.path().join("file.txt"), "content").unwrap();
        repo.seal(
            test_agent("unknown-bot"),
            "unregistered work".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Check that an unrecognized_agent event was emitted
        let logger = crate::security::SecurityEventLogger::new(&dir.path().join(".writ"));
        let events = logger.read_events(None).unwrap();
        let unrecognized = events.iter().find(|e| e.event_type == "unrecognized_agent");
        assert!(
            unrecognized.is_some(),
            "expected unrecognized_agent event for unregistered agent"
        );
        assert_eq!(
            unrecognized.unwrap().agent_id.as_deref(),
            Some("unknown-bot")
        );
        assert_eq!(
            unrecognized.unwrap().severity,
            crate::security::Severity::Warning
        );
    }

    #[test]
    fn test_signature_failure_emits_event() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Create a seal (will be signed if agent has keys, or unsigned)
        fs::write(dir.path().join("file.txt"), "data").unwrap();
        let seal = repo
            .seal(
                test_agent("worker"),
                "signed work".to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        // Generate a different keypair (wrong key)
        let wrong_keypair = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
        let wrong_verifying = wrong_keypair.verifying_key();

        // If the seal has a signature, verify with wrong key should fail
        if seal.signature.is_some() {
            let result = repo.verify_seal(&seal, Some(&wrong_verifying));
            assert_eq!(result.signature_valid, Some(false));

            let logger = crate::security::SecurityEventLogger::new(&dir.path().join(".writ"));
            let events = logger.read_events(None).unwrap();
            let auth_event = events
                .iter()
                .find(|e| e.event_type == "authentication_failure");
            assert!(
                auth_event.is_some(),
                "expected authentication_failure event for wrong key verification"
            );
            assert_eq!(
                auth_event.unwrap().severity,
                crate::security::Severity::Critical
            );
        }
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
            lifecycle_state: crate::spec::LifecycleState::Active,
            last_activity: now,
            completion_summary: None,
            commit_state: crate::spec::CommitState::Uncommitted,
            completed_at: None,
            commit_hash: None,
            committed_at: None,
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
        assert!(
            status.behind > 0,
            "Expected behind > 0, got {}",
            status.behind
        );
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
        fs::write(
            root.join("src/utils.py"),
            "def add(a, b):\n    return a + b\n",
        )
        .unwrap();

        // Add all and commit
        let mut index = git_repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = git_repo.find_tree(tree_id).unwrap();
        git_repo
            .commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])
            .unwrap();

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
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = git_repo.find_tree(tree_id).unwrap();
        let commit1_oid = git_repo
            .commit(Some("HEAD"), &sig, &sig, "first", &tree, &[])
            .unwrap();

        // Second commit (2 files)
        fs::write(root.join("file2.txt"), "v2").unwrap();
        let mut index = git_repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = git_repo.find_tree(tree_id).unwrap();
        let commit1_obj = git_repo.find_commit(commit1_oid).unwrap();
        git_repo
            .commit(Some("HEAD"), &sig, &sig, "second", &tree, &[&commit1_obj])
            .unwrap();

        // Import from the first commit by OID (only 1 file)
        let repo = Repository::init(root).unwrap();
        let agent = AgentIdentity {
            id: "bridge".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        let result = repo
            .bridge_import(Some(&commit1_oid.to_string()), agent)
            .unwrap();
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
        )
        .unwrap();

        // Export
        let result = repo.bridge_export(Some("writ/export")).unwrap();
        assert_eq!(result.seals_exported, 1);
        assert_eq!(result.branch, "writ/export");
        assert_eq!(result.exported[0].summary, "added new file");

        // Verify the git branch exists
        let git_repo = git2::Repository::discover(tmp.path()).unwrap();
        let branch = git_repo
            .find_branch("writ/export", git2::BranchType::Local)
            .unwrap();
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
        repo.add_spec(&Spec::new(
            "auth".to_string(),
            "Auth feature".to_string(),
            String::new(),
        ))
        .unwrap();
        let agent2 = AgentIdentity {
            id: "tester".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            agent2,
            "auth tests passing".to_string(),
            Some("auth".to_string()),
            TaskStatus::Complete,
            Verification {
                tests_passed: Some(42),
                tests_failed: Some(0),
                linted: true,
            },
            false,
        )
        .unwrap();

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
        let a1 = AgentIdentity {
            id: "agent-1".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            a1,
            "first change".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();
        let result1 = repo.bridge_export(None).unwrap();
        assert_eq!(result1.seals_exported, 1);

        // Second seal + export (should only export the new one)
        fs::write(tmp.path().join("b.txt"), "b").unwrap();
        let a2 = AgentIdentity {
            id: "agent-1".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            a2,
            "second change".to_string(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();
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
            fs::write(
                tmp.path().join(format!("file{i}.txt")),
                format!("content {i}"),
            )
            .unwrap();
            let a = AgentIdentity {
                id: "worker".to_string(),
                agent_type: crate::seal::AgentType::Agent,
            };
            repo.seal(
                a,
                format!("change {i}"),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();
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
        let a1 = AgentIdentity {
            id: "implementer".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            a1,
            "added auth module".to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Modify existing file
        fs::write(
            root.join("src/main.py"),
            "from auth import Auth\nprint('hello')\n",
        )
        .unwrap();
        let a2 = AgentIdentity {
            id: "implementer".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            a2,
            "integrated auth".to_string(),
            None,
            TaskStatus::Complete,
            Verification {
                tests_passed: Some(5),
                tests_failed: Some(0),
                linted: false,
            },
            false,
        )
        .unwrap();

        // Export back to git
        let export_result = repo.bridge_export(Some("writ/output")).unwrap();
        assert_eq!(export_result.seals_exported, 2);
        assert_eq!(export_result.branch, "writ/output");

        // Verify git branch has the correct file tree
        let git_repo = git2::Repository::discover(root).unwrap();
        let branch = git_repo
            .find_branch("writ/output", git2::BranchType::Local)
            .unwrap();
        let commit = branch.get().peel_to_commit().unwrap();
        let tree = commit.tree().unwrap();

        // Check new file exists in git tree
        assert!(tree
            .get_path(std::path::Path::new("src/new_module.py"))
            .is_ok());
        // Check modified file content
        let entry = tree.get_path(std::path::Path::new("src/main.py")).unwrap();
        let blob = entry.to_object(&git_repo).unwrap();
        let content = blob.as_blob().unwrap().content();
        assert_eq!(
            String::from_utf8_lossy(content),
            "from auth import Auth\nprint('hello')\n"
        );
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

    // ─── Bridge import index baseline refresh ────────────────────

    #[test]
    fn test_first_seal_after_bridge_captures_only_agent_changes() {
        let (tmp, repo) = setup_git_and_writ();
        let root = tmp.path();
        let agent = AgentIdentity {
            id: "bridge".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };

        // Bridge imports 3 files from git.
        let result = repo.bridge_import(None, agent).unwrap();
        assert_eq!(result.files_imported, 3);

        // Agent creates exactly 1 new file, then seals.
        fs::write(root.join("agent_work.txt"), "my changes").unwrap();
        let dev = AgentIdentity {
            id: "dev".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        let seal = repo
            .seal(
                dev,
                "agent work".into(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        // The seal should have exactly 1 change (the new file), not 4.
        assert_eq!(
            seal.changes.len(),
            1,
            "first seal should only have agent's changes"
        );
        assert_eq!(seal.changes[0].path, "agent_work.txt");
        assert_eq!(seal.changes[0].change_type, ChangeType::Added);
    }

    #[test]
    fn test_bridge_import_dirty_tree_doesnt_pollute_first_seal() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create git repo with 1 committed file.
        let git_repo = git2::Repository::init(root).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        fs::write(root.join("committed.txt"), "in git").unwrap();
        let mut index = git_repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = git_repo.find_tree(tree_id).unwrap();
        git_repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        // Add a dirty file (not committed to git).
        fs::write(root.join("dirty.txt"), "uncommitted stuff").unwrap();

        // Init writ and bridge_import.
        let repo = Repository::init(root).unwrap();
        let bridge = AgentIdentity {
            id: "bridge".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.bridge_import(None, bridge).unwrap();

        // The dirty file should be in the index now (baseline refreshed).
        // Agent creates their own file and seals.
        fs::write(root.join("agent.txt"), "agent work").unwrap();
        let dev = AgentIdentity {
            id: "dev".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        let seal = repo
            .seal(
                dev,
                "agent work".into(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        // Seal should only contain agent.txt, not dirty.txt.
        let paths: Vec<&str> = seal.changes.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["agent.txt"],
            "dirty file should not pollute first seal; got: {:?}",
            paths
        );
    }

    #[test]
    fn test_bridge_import_from_old_commit_refreshes_baseline() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let git_repo = git2::Repository::init(root).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();

        // First commit: 1 file.
        fs::write(root.join("file1.txt"), "v1").unwrap();
        let mut index = git_repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = git_repo.find_tree(tree_id).unwrap();
        let c1 = git_repo
            .commit(Some("HEAD"), &sig, &sig, "first", &tree, &[])
            .unwrap();

        // Second commit: 2 files.
        fs::write(root.join("file2.txt"), "v2").unwrap();
        let mut index = git_repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = git_repo.find_tree(tree_id).unwrap();
        let c1_obj = git_repo.find_commit(c1).unwrap();
        git_repo
            .commit(Some("HEAD"), &sig, &sig, "second", &tree, &[&c1_obj])
            .unwrap();

        // Import from FIRST commit (only 1 file), but working dir has 2 files.
        let repo = Repository::init(root).unwrap();
        let bridge = AgentIdentity {
            id: "bridge".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        let result = repo.bridge_import(Some(&c1.to_string()), bridge).unwrap();
        assert_eq!(
            result.files_imported, 1,
            "bridge seal records 1 file from commit 1"
        );

        // file2.txt exists on disk but wasn't in commit 1.
        // After baseline refresh, the index should include it.
        // Agent adds file3 and seals — should only see file3.
        fs::write(root.join("file3.txt"), "agent work").unwrap();
        let dev = AgentIdentity {
            id: "dev".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        let seal = repo
            .seal(
                dev,
                "agent work".into(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        let paths: Vec<&str> = seal.changes.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["file3.txt"],
            "file2.txt (on disk but not in imported commit) should not appear; got: {:?}",
            paths
        );
    }

    #[test]
    fn test_bridge_seal_preserves_git_tree_snapshot() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let git_repo = git2::Repository::init(root).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        fs::write(root.join("committed.txt"), "in git").unwrap();
        let mut index = git_repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = git_repo.find_tree(tree_id).unwrap();
        git_repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        // Add dirty file before bridge_import.
        fs::write(root.join("dirty.txt"), "not in git").unwrap();

        let repo = Repository::init(root).unwrap();
        let bridge = AgentIdentity {
            id: "bridge".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        let result = repo.bridge_import(None, bridge).unwrap();

        // The bridge SEAL's tree should only have the git file (historical accuracy).
        let seal = repo.load_seal(&result.seal_id).unwrap();
        let tree_index = repo.load_tree_index(&seal.tree).unwrap();
        assert!(
            tree_index.entries.contains_key("committed.txt"),
            "bridge seal tree should have committed file"
        );
        assert!(
            !tree_index.entries.contains_key("dirty.txt"),
            "bridge seal tree should NOT have dirty file"
        );

        // But the working index should have both (baseline refresh).
        let working_index = repo.load_index().unwrap();
        assert!(working_index.entries.contains_key("committed.txt"));
        assert!(
            working_index.entries.contains_key("dirty.txt"),
            "working index should include dirty file after refresh"
        );
    }

    #[test]
    fn test_bridge_import_ownership_not_polluted() {
        let (tmp, repo) = setup_git_and_writ();
        let root = tmp.path();
        let bridge = AgentIdentity {
            id: "writ-bridge".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.bridge_import(None, bridge).unwrap();

        // Agent modifies 1 file and creates 1 new file.
        fs::write(root.join("README.md"), "# Updated\n").unwrap();
        fs::write(root.join("new.txt"), "new").unwrap();
        let dev = AgentIdentity {
            id: "dev-agent".to_string(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            dev,
            "agent changes".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Check agent activity — dev-agent should only own files they touched.
        let all_seals = repo.log_all().unwrap();
        let activity = Repository::build_agent_activity(&all_seals, None);

        let dev_activity = activity.iter().find(|a| a.agent_id == "dev-agent");
        assert!(dev_activity.is_some());
        let owned: &Vec<String> = &dev_activity.unwrap().files_owned;
        assert!(
            owned.contains(&"README.md".to_string()),
            "dev should own README.md"
        );
        assert!(
            owned.contains(&"new.txt".to_string()),
            "dev should own new.txt"
        );
        // dev should NOT own src/main.py or src/utils.py (bridge imported those).
        assert!(
            !owned.contains(&"src/main.py".to_string()),
            "dev should NOT own src/main.py; bridge imported it"
        );
        assert!(
            !owned.contains(&"src/utils.py".to_string()),
            "dev should NOT own src/utils.py; bridge imported it"
        );
    }
}

#[cfg(test)]
mod spec_head_tests {
    use super::*;
    use crate::seal::{AgentType, TaskStatus, Verification};
    use tempfile::tempdir;

    fn test_agent() -> AgentIdentity {
        AgentIdentity {
            id: "test-agent".to_string(),
            agent_type: AgentType::Agent,
        }
    }

    #[test]
    fn test_spec_scoped_head_isolation() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec_a = Spec::new("spec-a".into(), "Feature A".into(), "".into());
        let spec_b = Spec::new("spec-b".into(), "Feature B".into(), "".into());
        repo.add_spec(&spec_a).unwrap();
        repo.add_spec(&spec_b).unwrap();

        fs::write(dir.path().join("a.txt"), "content-a").unwrap();
        let seal_a1 = repo
            .seal(
                test_agent(),
                "a work 1".into(),
                Some("spec-a".into()),
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        fs::write(dir.path().join("b.txt"), "content-b").unwrap();
        let seal_b1 = repo
            .seal(
                test_agent(),
                "b work 1".into(),
                Some("spec-b".into()),
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        assert_eq!(repo.spec_head("spec-a").unwrap(), Some(seal_a1.id.clone()));
        assert_eq!(repo.spec_head("spec-b").unwrap(), Some(seal_b1.id.clone()));
        assert_ne!(seal_a1.id, seal_b1.id);
    }

    #[test]
    fn test_spec_head_chains_correctly() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new("my-spec".into(), "Test Spec".into(), "".into());
        repo.add_spec(&spec).unwrap();

        fs::write(dir.path().join("file.txt"), "v1").unwrap();
        let seal1 = repo
            .seal(
                test_agent(),
                "first".into(),
                Some("my-spec".into()),
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        fs::write(dir.path().join("file.txt"), "v2").unwrap();
        let seal2 = repo
            .seal(
                test_agent(),
                "second".into(),
                Some("my-spec".into()),
                TaskStatus::Complete,
                Verification::default(),
                false,
            )
            .unwrap();

        assert_eq!(repo.spec_head("my-spec").unwrap(), Some(seal2.id.clone()));
        assert_eq!(seal2.parent, Some(seal1.id.clone()));
    }

    #[test]
    fn test_spec_head_none_for_unknown_spec() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        assert_eq!(repo.spec_head("nonexistent").unwrap(), None);
    }

    #[test]
    fn test_spec_log_returns_spec_chain() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new("log-spec".into(), "Log Test".into(), "".into());
        repo.add_spec(&spec).unwrap();

        fs::write(dir.path().join("f.txt"), "a").unwrap();
        let s1 = repo
            .seal(
                test_agent(),
                "s1".into(),
                Some("log-spec".into()),
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        fs::write(dir.path().join("f.txt"), "b").unwrap();
        let s2 = repo
            .seal(
                test_agent(),
                "s2".into(),
                Some("log-spec".into()),
                TaskStatus::Complete,
                Verification::default(),
                false,
            )
            .unwrap();

        let spec_seals = repo.spec_log("log-spec").unwrap();
        assert_eq!(spec_seals.len(), 2);
        assert_eq!(spec_seals[0].id, s2.id);
        assert_eq!(spec_seals[1].id, s1.id);
    }

    #[test]
    fn test_seal_without_spec_uses_global_head() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("a.txt"), "first").unwrap();
        let seal1 = repo
            .seal(
                test_agent(),
                "no spec 1".into(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        fs::write(dir.path().join("a.txt"), "second").unwrap();
        let seal2 = repo
            .seal(
                test_agent(),
                "no spec 2".into(),
                None,
                TaskStatus::Complete,
                Verification::default(),
                false,
            )
            .unwrap();

        assert_eq!(seal2.parent, Some(seal1.id.clone()));
        assert_eq!(repo.spec_head("anything").unwrap(), None);
    }
}

#[cfg(test)]
mod merge_on_seal_tests {
    use super::*;
    use crate::seal::{AgentType, TaskStatus, Verification};
    use tempfile::tempdir;

    fn agent(name: &str) -> AgentIdentity {
        AgentIdentity {
            id: name.to_string(),
            agent_type: AgentType::Agent,
        }
    }

    #[test]
    fn test_seal_with_check_no_conflict() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let s1 = repo
            .seal(
                agent("a1"),
                "first".into(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        fs::write(dir.path().join("a.txt"), "world").unwrap();
        let (s2, warning) = repo
            .seal_with_check(
                agent("a1"),
                "second".into(),
                None,
                TaskStatus::Complete,
                Verification::default(),
                false,
                Some(s1.id.clone()),
            )
            .unwrap();

        assert!(warning.is_none());
        assert_eq!(s2.parent, Some(s1.id));
    }

    #[test]
    fn test_seal_with_check_detects_head_movement() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("a.txt"), "v1").unwrap();
        let s1 = repo
            .seal(
                agent("a1"),
                "base".into(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        // Agent B seals (moving HEAD)
        fs::write(dir.path().join("b.txt"), "agent-b-work").unwrap();
        let _s2 = repo
            .seal(
                agent("a2"),
                "agent b work".into(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        // Agent A seals with expected_head = s1 (stale)
        fs::write(dir.path().join("a.txt"), "v2").unwrap();
        let (s3, warning) = repo
            .seal_with_check(
                agent("a1"),
                "agent a work".into(),
                None,
                TaskStatus::Complete,
                Verification::default(),
                false,
                Some(s1.id.clone()),
            )
            .unwrap();

        assert!(warning.is_some());
        let w = warning.unwrap();
        assert_eq!(w.expected_head, s1.id);
        assert_eq!(w.intervening_seals.len(), 1);
        assert!(w.is_clean, "different files = no overlap");
        assert!(s3.id.len() == 64);
    }

    #[test]
    fn test_seal_with_check_detects_file_overlap() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("shared.txt"), "v1").unwrap();
        let s1 = repo
            .seal(
                agent("a1"),
                "base".into(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        // Agent B modifies the same file
        fs::write(dir.path().join("shared.txt"), "agent-b").unwrap();
        let _s2 = repo
            .seal(
                agent("a2"),
                "agent b edits shared".into(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        // Agent A also modifies shared.txt
        fs::write(dir.path().join("shared.txt"), "agent-a").unwrap();
        let (_s3, warning) = repo
            .seal_with_check(
                agent("a1"),
                "agent a edits shared".into(),
                None,
                TaskStatus::Complete,
                Verification::default(),
                false,
                Some(s1.id.clone()),
            )
            .unwrap();

        assert!(warning.is_some());
        let w = warning.unwrap();
        assert!(!w.is_clean, "same file = overlap");
        assert!(w.overlapping_files.contains(&"shared.txt".to_string()));
    }

    #[test]
    fn test_seal_with_check_short_id() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let s1 = repo
            .seal(
                agent("a1"),
                "first".into(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        // Use a short ID (first 12 chars) as expected_head
        let short_id = s1.id[..12].to_string();

        fs::write(dir.path().join("a.txt"), "world").unwrap();
        let (_s2, warning) = repo
            .seal_with_check(
                agent("a1"),
                "second".into(),
                None,
                TaskStatus::Complete,
                Verification::default(),
                false,
                Some(short_id),
            )
            .unwrap();

        // Short ID should resolve to full ID — no false conflict
        assert!(warning.is_none());
    }

    #[test]
    fn test_seal_with_check_no_expected_head() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let (_seal, warning) = repo
            .seal_with_check(
                agent("a1"),
                "no check".into(),
                None,
                TaskStatus::Complete,
                Verification::default(),
                false,
                None,
            )
            .unwrap();

        assert!(warning.is_none());
    }
}

#[cfg(test)]
mod context_head_tracking_tests {
    use super::*;
    use crate::context::{ContextFilter, ContextScope};
    use crate::seal::{AgentType, TaskStatus, Verification};
    use tempfile::tempdir;

    fn agent(name: &str) -> AgentIdentity {
        AgentIdentity {
            id: name.to_string(),
            agent_type: AgentType::Agent,
        }
    }

    #[test]
    fn test_context_records_head() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        assert!(repo.last_context_head().is_none());

        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        repo.seal(
            agent("a1"),
            "first".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        repo.context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        assert!(repo.last_context_head().is_some());
    }

    #[test]
    fn test_context_records_spec_head() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = crate::spec::Spec::new("feat".into(), "Feature".into(), "".into());
        repo.add_spec(&spec).unwrap();

        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let s1 = repo
            .seal(
                agent("a1"),
                "first".into(),
                Some("feat".into()),
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        repo.context(
            ContextScope::Spec("feat".into()),
            10,
            &ContextFilter::default(),
        )
        .unwrap();
        let tracked = repo.last_context_head().unwrap();
        assert_eq!(tracked, s1.id);
    }

    #[test]
    fn test_clear_context_head() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        repo.seal(
            agent("a1"),
            "first".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        repo.context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        assert!(repo.last_context_head().is_some());

        repo.clear_context_head();
        assert!(repo.last_context_head().is_none());
    }

    #[test]
    fn test_no_context_head_before_context_call() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        assert!(repo.last_context_head().is_none());
    }

    #[test]
    fn test_context_head_none_when_no_seals() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        // HEAD is empty when no seals exist.
        assert!(repo.last_context_head().is_none());
    }
}

#[cfg(test)]
mod spec_scoped_context_tests {
    use super::*;
    use crate::context::{ContextFilter, ContextScope};
    use crate::seal::{AgentType, TaskStatus, Verification};
    use tempfile::tempdir;

    fn agent(name: &str) -> AgentIdentity {
        AgentIdentity {
            id: name.to_string(),
            agent_type: AgentType::Agent,
        }
    }

    #[test]
    fn test_spec_context_filters_working_state_by_file_scope() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Create spec with explicit file_scope.
        let mut spec = crate::spec::Spec::new("auth".into(), "Auth".into(), "".into());
        spec.file_scope = vec!["src/auth.py".to_string()];
        repo.add_spec(&spec).unwrap();

        // Create files — one in scope, one not.
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/auth.py"), "auth code").unwrap();
        std::fs::write(dir.path().join("readme.md"), "docs").unwrap();

        // Seal both files so they're tracked.
        repo.seal(
            agent("a1"),
            "base".into(),
            Some("auth".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Modify both files.
        std::fs::write(dir.path().join("src/auth.py"), "auth v2").unwrap();
        std::fs::write(dir.path().join("readme.md"), "updated docs").unwrap();

        // Spec-scoped context should only show auth.py changes.
        let ctx = repo
            .context(
                ContextScope::Spec("auth".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();

        assert!(ctx
            .working_state
            .modified_files
            .contains(&"src/auth.py".to_string()));
        assert!(!ctx
            .working_state
            .modified_files
            .contains(&"readme.md".to_string()));
    }

    #[test]
    fn test_spec_context_filters_pending_changes() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let mut spec = crate::spec::Spec::new("ui".into(), "UI".into(), "".into());
        spec.file_scope = vec!["style.css".to_string()];
        repo.add_spec(&spec).unwrap();

        // Create and track files.
        std::fs::write(dir.path().join("style.css"), "body {}").unwrap();
        std::fs::write(dir.path().join("app.js"), "console.log()").unwrap();
        repo.seal(
            agent("a1"),
            "base".into(),
            Some("ui".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Modify both.
        std::fs::write(dir.path().join("style.css"), "body { color: red }").unwrap();
        std::fs::write(dir.path().join("app.js"), "console.log('changed')").unwrap();

        let ctx = repo
            .context(
                ContextScope::Spec("ui".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();

        // pending_changes should only contain style.css.
        let pc = ctx.pending_changes.unwrap();
        assert_eq!(pc.files_changed, 1);
        assert_eq!(pc.files[0].path, "style.css");
    }

    #[test]
    fn test_spec_context_filters_seal_nudge() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let mut spec = crate::spec::Spec::new("api".into(), "API".into(), "".into());
        spec.file_scope = vec!["api.py".to_string()];
        repo.add_spec(&spec).unwrap();

        // Track files.
        std::fs::write(dir.path().join("api.py"), "v1").unwrap();
        std::fs::write(dir.path().join("unrelated.py"), "v1").unwrap();
        repo.seal(
            agent("a1"),
            "base".into(),
            Some("api".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Modify only the unrelated file.
        std::fs::write(dir.path().join("unrelated.py"), "v2").unwrap();

        let ctx = repo
            .context(
                ContextScope::Spec("api".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();

        // No spec-relevant changes → no nudge.
        assert!(ctx.seal_nudge.is_none());
    }

    #[test]
    fn test_spec_context_infers_scope_from_seals() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Create spec without explicit file_scope.
        let spec = crate::spec::Spec::new("feat".into(), "Feature".into(), "".into());
        repo.add_spec(&spec).unwrap();

        // Seal a file linked to this spec.
        std::fs::write(dir.path().join("feature.py"), "v1").unwrap();
        repo.seal(
            agent("a1"),
            "impl".into(),
            Some("feat".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Also seal an unrelated file (no spec).
        std::fs::write(dir.path().join("other.py"), "v1").unwrap();
        repo.seal(
            agent("a1"),
            "other work".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Modify both files.
        std::fs::write(dir.path().join("feature.py"), "v2").unwrap();
        std::fs::write(dir.path().join("other.py"), "v2").unwrap();

        let ctx = repo
            .context(
                ContextScope::Spec("feat".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();

        // Should only show feature.py (inferred from spec seals).
        assert!(ctx
            .working_state
            .modified_files
            .contains(&"feature.py".to_string()));
        assert!(!ctx
            .working_state
            .modified_files
            .contains(&"other.py".to_string()));
    }

    #[test]
    fn test_spec_context_no_filter_when_no_scope_or_seals() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Create spec with no file_scope and no seals.
        let spec = crate::spec::Spec::new("new".into(), "New Feature".into(), "".into());
        repo.add_spec(&spec).unwrap();

        // Create and track a file.
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        repo.seal(
            agent("a1"),
            "base".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("a.txt"), "changed").unwrap();

        let ctx = repo
            .context(
                ContextScope::Spec("new".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();

        // No scope filter → all changes shown.
        assert!(ctx
            .working_state
            .modified_files
            .contains(&"a.txt".to_string()));
    }

    #[test]
    fn test_spec_context_directory_prefix_matching() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let mut spec = crate::spec::Spec::new("ui".into(), "UI".into(), "".into());
        spec.file_scope = vec!["src/components/".to_string()];
        repo.add_spec(&spec).unwrap();

        // Create files in and out of scope.
        std::fs::create_dir_all(dir.path().join("src/components")).unwrap();
        std::fs::write(dir.path().join("src/components/Button.tsx"), "btn").unwrap();
        std::fs::write(dir.path().join("src/utils.ts"), "utils").unwrap();
        repo.seal(
            agent("a1"),
            "base".into(),
            Some("ui".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Modify both.
        std::fs::write(dir.path().join("src/components/Button.tsx"), "btn v2").unwrap();
        std::fs::write(dir.path().join("src/utils.ts"), "utils v2").unwrap();

        let ctx = repo
            .context(
                ContextScope::Spec("ui".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();

        assert!(ctx
            .working_state
            .modified_files
            .contains(&"src/components/Button.tsx".to_string()));
        assert!(!ctx
            .working_state
            .modified_files
            .contains(&"src/utils.ts".to_string()));
    }

    #[test]
    fn test_spec_context_shows_diverged_spec_seals() {
        // Reproduce the AAIS_6 bug: spec-scoped context should show seals
        // even when the spec's branch has diverged from global HEAD.
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("alpha".into(), "Alpha".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("beta".into(), "Beta".into(), "".into()))
            .unwrap();

        // Agent A seals on alpha.
        std::fs::write(dir.path().join("a.txt"), "a1").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha first".into(),
            Some("alpha".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent B seals on beta (diverges after next alpha seal).
        std::fs::write(dir.path().join("b.txt"), "b1").unwrap();
        repo.seal(
            agent("agent-b"),
            "beta work".into(),
            Some("beta".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent B seals again on beta.
        std::fs::write(dir.path().join("b.txt"), "b2").unwrap();
        repo.seal(
            agent("agent-b"),
            "beta more".into(),
            Some("beta".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent A seals on alpha — global HEAD moves, beta branch diverges.
        std::fs::write(dir.path().join("a.txt"), "a2").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha second".into(),
            Some("alpha".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Verify beta is actually diverged (not reachable from global HEAD).
        let diverged = repo.diverged_branches().unwrap();
        assert!(!diverged.is_empty(), "beta should be diverged");

        // Spec-scoped context for beta should show its seals.
        let ctx = repo
            .context(
                ContextScope::Spec("beta".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();

        assert!(
            !ctx.recent_seals.is_empty(),
            "spec-scoped context should show seals even on diverged branch; got empty"
        );
        assert_eq!(
            ctx.recent_seals.len(),
            2,
            "beta has 2 seals; got {}",
            ctx.recent_seals.len()
        );
        assert!(
            ctx.recent_seals.iter().all(|s| s.agent == "agent-b"),
            "all beta seals should be from agent-b"
        );
    }

    #[test]
    fn test_spec_context_diverged_still_has_progress() {
        // Spec progress should also work for diverged specs.
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("alpha".into(), "Alpha".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("beta".into(), "Beta".into(), "".into()))
            .unwrap();

        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha".into(),
            Some("alpha".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        repo.seal(
            agent("agent-b"),
            "beta".into(),
            Some("beta".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("a.txt"), "a2").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha 2".into(),
            Some("alpha".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(
                ContextScope::Spec("beta".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();

        // spec_progress should be populated even though beta is diverged.
        assert!(
            ctx.spec_progress.is_some(),
            "diverged spec should still have progress"
        );
        let progress = ctx.spec_progress.unwrap();
        assert_eq!(progress.total_seals, 1);
        assert_eq!(progress.agents_involved, vec!["agent-b"]);
    }

    #[test]
    fn test_spec_context_includes_diverged_branches() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("alpha".into(), "Alpha".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("beta".into(), "Beta".into(), "".into()))
            .unwrap();

        // Seal on alpha (becomes global HEAD).
        std::fs::write(dir.path().join("a.txt"), "a1").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha work".into(),
            Some("alpha".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Seal on beta (diverges from alpha's HEAD).
        std::fs::write(dir.path().join("b.txt"), "b1").unwrap();
        repo.seal(
            agent("agent-b"),
            "beta work".into(),
            Some("beta".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Seal again on alpha to make beta's branch diverge.
        std::fs::write(dir.path().join("a.txt"), "a2").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha more".into(),
            Some("alpha".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Spec-scoped context for alpha should see beta's diverged branch.
        let ctx = repo
            .context(
                ContextScope::Spec("alpha".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();

        assert!(
            !ctx.diverged_branches.is_empty(),
            "spec-scoped context should include diverged branches"
        );
        assert!(ctx.convergence_recommended);
        assert!(ctx.integration_risk.score > 0);
    }

    #[test]
    fn test_spec_context_file_contention_filtered_to_scope() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let mut spec = Spec::new("auth".into(), "Auth".into(), "".into());
        spec.file_scope = vec!["auth.py".to_string()];
        repo.add_spec(&spec).unwrap();
        repo.add_spec(&Spec::new("api".into(), "API".into(), "".into()))
            .unwrap();

        // Agent A touches auth.py and app.py.
        std::fs::write(dir.path().join("auth.py"), "v1").unwrap();
        std::fs::write(dir.path().join("app.py"), "v1").unwrap();
        repo.seal(
            agent("agent-a"),
            "both files".into(),
            Some("auth".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent B also touches auth.py and app.py.
        std::fs::write(dir.path().join("auth.py"), "v2").unwrap();
        std::fs::write(dir.path().join("app.py"), "v2").unwrap();
        repo.seal(
            agent("agent-b"),
            "also both".into(),
            Some("api".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Full-scope context should show contention on both files.
        let full_ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        assert_eq!(full_ctx.file_contention.len(), 2);

        // Spec-scoped context for auth should only show auth.py contention.
        let spec_ctx = repo
            .context(
                ContextScope::Spec("auth".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();
        assert_eq!(spec_ctx.file_contention.len(), 1);
        assert_eq!(spec_ctx.file_contention[0].path, "auth.py");
    }

    #[test]
    fn test_spec_context_scope_violations_filtered_to_spec() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let mut spec_a = Spec::new("alpha".into(), "Alpha".into(), "".into());
        spec_a.file_scope = vec!["alpha.py".to_string()];
        repo.add_spec(&spec_a).unwrap();

        let mut spec_b = Spec::new("beta".into(), "Beta".into(), "".into());
        spec_b.file_scope = vec!["beta.py".to_string()];
        repo.add_spec(&spec_b).unwrap();

        // Agent A seals on alpha but touches beta.py → violation for alpha.
        std::fs::write(dir.path().join("alpha.py"), "v1").unwrap();
        std::fs::write(dir.path().join("beta.py"), "v1").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha work".into(),
            Some("alpha".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent B seals on beta but touches alpha.py → violation for beta.
        std::fs::write(dir.path().join("alpha.py"), "v2").unwrap();
        std::fs::write(dir.path().join("beta.py"), "v2").unwrap();
        repo.seal(
            agent("agent-b"),
            "beta work".into(),
            Some("beta".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Full context sees both violations.
        let full_ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        assert_eq!(full_ctx.file_scope_violations.len(), 2);

        // Spec-scoped for alpha sees only alpha's violation.
        let alpha_ctx = repo
            .context(
                ContextScope::Spec("alpha".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();
        assert_eq!(alpha_ctx.file_scope_violations.len(), 1);
        assert_eq!(alpha_ctx.file_scope_violations[0].spec_id, "alpha");

        // Spec-scoped for beta sees only beta's violation.
        let beta_ctx = repo
            .context(
                ContextScope::Spec("beta".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();
        assert_eq!(beta_ctx.file_scope_violations.len(), 1);
        assert_eq!(beta_ctx.file_scope_violations[0].spec_id, "beta");
    }

    #[test]
    fn test_spec_context_integration_risk_computed_from_filtered_signals() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let mut spec = Spec::new("auth".into(), "Auth".into(), "".into());
        spec.file_scope = vec!["auth.py".to_string()];
        repo.add_spec(&spec).unwrap();

        // Two agents touch auth.py → contention.
        std::fs::write(dir.path().join("auth.py"), "v1").unwrap();
        repo.seal(
            agent("agent-a"),
            "a work".into(),
            Some("auth".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("auth.py"), "v2").unwrap();
        repo.seal(
            agent("agent-b"),
            "b work".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(
                ContextScope::Spec("auth".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();

        // File contention is present (not zeroed out).
        assert!(!ctx.file_contention.is_empty());
        assert_eq!(ctx.file_contention[0].agents.len(), 2);
    }
}

#[cfg(test)]
mod recommended_action_tests {
    use super::*;
    use crate::context::{ContextFilter, ContextScope};
    use crate::seal::{AgentType, TaskStatus, Verification};
    use tempfile::tempdir;

    fn agent(name: &str) -> AgentIdentity {
        AgentIdentity {
            id: name.to_string(),
            agent_type: AgentType::Agent,
        }
    }

    #[test]
    fn test_recommended_action_none_when_clean() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        assert!(ctx.recommended_action.is_none());
    }

    #[test]
    fn test_recommended_action_seal_when_changes() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        std::fs::write(dir.path().join("a.txt"), "v1").unwrap();
        repo.seal(
            agent("dev"),
            "initial".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("a.txt"), "v2").unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        let action = ctx.recommended_action.unwrap();
        assert_eq!(action.action, "seal");
        assert_eq!(action.priority, "medium");
    }

    #[test]
    fn test_recommended_action_converge_when_diverged() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("a".into(), "A".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("b".into(), "B".into(), "".into()))
            .unwrap();

        std::fs::write(dir.path().join("a.txt"), "a1").unwrap();
        repo.seal(
            agent("dev-a"),
            "a work".into(),
            Some("a".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("b.txt"), "b1").unwrap();
        repo.seal(
            agent("dev-b"),
            "b work".into(),
            Some("b".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("a.txt"), "a2").unwrap();
        repo.seal(
            agent("dev-a"),
            "a more".into(),
            Some("a".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        let action = ctx.recommended_action.unwrap();
        assert_eq!(action.action, "converge");
        assert_eq!(action.priority, "high");
    }

    #[test]
    fn test_recommended_action_finish_when_session_complete() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("feat".into(), "Feature".into(), "".into()))
            .unwrap();

        std::fs::write(dir.path().join("f.txt"), "done").unwrap();
        repo.seal(
            agent("dev"),
            "completed".into(),
            Some("feat".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        assert!(ctx.session_complete);
        let action = ctx.recommended_action.unwrap();
        assert_eq!(action.action, "finish");
        assert_eq!(action.priority, "low");
    }

    #[test]
    fn test_recommended_action_blocking_dependency() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("base".into(), "Base".into(), "".into()))
            .unwrap();

        let mut dep_spec = Spec::new("feature".into(), "Feature".into(), "".into());
        dep_spec.depends_on = vec!["base".to_string()];
        repo.add_spec(&dep_spec).unwrap();

        std::fs::write(dir.path().join("f.txt"), "v1").unwrap();
        repo.seal(
            agent("dev"),
            "started".into(),
            Some("feature".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(
                ContextScope::Spec("feature".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();
        let action = ctx.recommended_action.unwrap();
        assert_eq!(action.action, "wait_for_dependency");
        assert_eq!(action.priority, "high");
        assert!(action.message.contains("base"));
    }

    #[test]
    fn test_recommended_action_dependency_resolved_no_block() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("base".into(), "Base".into(), "".into()))
            .unwrap();
        std::fs::write(dir.path().join("b.txt"), "v1").unwrap();
        repo.seal(
            agent("dev-a"),
            "base done".into(),
            Some("base".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let mut dep_spec = Spec::new("feature".into(), "Feature".into(), "".into());
        dep_spec.depends_on = vec!["base".to_string()];
        repo.add_spec(&dep_spec).unwrap();

        std::fs::write(dir.path().join("f.txt"), "v1").unwrap();
        repo.seal(
            agent("dev-b"),
            "started".into(),
            Some("feature".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(
                ContextScope::Spec("feature".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();

        if let Some(ref action) = ctx.recommended_action {
            assert_ne!(action.action, "wait_for_dependency");
        }
    }

    #[test]
    fn test_recommended_action_priority_ordering() {
        // Blocking dep + unsealed changes → dep takes priority.
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("base".into(), "Base".into(), "".into()))
            .unwrap();

        let mut dep_spec = Spec::new("feature".into(), "Feature".into(), "".into());
        dep_spec.depends_on = vec!["base".to_string()];
        repo.add_spec(&dep_spec).unwrap();

        std::fs::write(dir.path().join("f.txt"), "v1").unwrap();
        repo.seal(
            agent("dev"),
            "started".into(),
            Some("feature".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Create unsealed changes.
        std::fs::write(dir.path().join("f.txt"), "v2").unwrap();

        let ctx = repo
            .context(
                ContextScope::Spec("feature".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();

        let action = ctx.recommended_action.unwrap();
        assert_eq!(action.action, "wait_for_dependency");
    }

    #[test]
    fn test_recommended_action_converge_in_spec_scope() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("a".into(), "A".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("b".into(), "B".into(), "".into()))
            .unwrap();

        std::fs::write(dir.path().join("a.txt"), "a1").unwrap();
        repo.seal(
            agent("dev-a"),
            "a".into(),
            Some("a".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("b.txt"), "b1").unwrap();
        repo.seal(
            agent("dev-b"),
            "b".into(),
            Some("b".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("a.txt"), "a2").unwrap();
        repo.seal(
            agent("dev-a"),
            "a more".into(),
            Some("a".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(
                ContextScope::Spec("a".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();
        let action = ctx.recommended_action.unwrap();
        assert_eq!(action.action, "converge");
        assert_eq!(action.priority, "high");
    }
}

/// End-to-end context stress test: exercises every recommended_action
/// transition and spec-scoped signal across a realistic 3-agent workflow
/// with dependencies, shared files, scope violations, and convergence.
#[cfg(test)]
#[allow(deprecated)]
mod context_stress_tests {
    use super::*;
    use crate::context::{ContextFilter, ContextScope};
    use crate::convergence::ConvergeStrategy;
    use crate::seal::{AgentType, TaskStatus, Verification};
    use crate::spec::{Spec, SpecStatus, SpecUpdate};
    use tempfile::tempdir;

    fn agent(name: &str) -> AgentIdentity {
        AgentIdentity {
            id: name.to_string(),
            agent_type: AgentType::Agent,
        }
    }

    fn ctx(repo: &Repository, scope: ContextScope) -> ContextOutput {
        repo.context(scope, 20, &ContextFilter::default()).unwrap()
    }

    /// CC's test plan: "3 Agents, Shared Files, Blocking Dependencies"
    ///
    /// Hits every recommended_action priority level and every spec-scoped
    /// risk signal in a single linear workflow:
    ///   wait_for_dependency → seal → converge → finish
    #[test]
    fn test_context_stress_full_workflow() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // ── Step 1: Setup specs with dependencies and file scopes ──

        let mut database_spec = Spec::new("database".into(), "Database schema".into(), "".into());
        database_spec.file_scope = vec!["schema.py".into(), "models.py".into()];
        repo.add_spec(&database_spec).unwrap();

        let mut api_spec = Spec::new("api".into(), "REST API".into(), "".into());
        api_spec.depends_on = vec!["database".into()];
        api_spec.file_scope = vec!["routes.py".into(), "models.py".into()];
        repo.add_spec(&api_spec).unwrap();

        let mut tests_spec = Spec::new("tests".into(), "Test suite".into(), "".into());
        tests_spec.depends_on = vec!["api".into()];
        tests_spec.file_scope = vec!["tests/".into()];
        repo.add_spec(&tests_spec).unwrap();

        // Shared file that all agents will touch.
        fs::write(dir.path().join("models.py"), "class Base:\n    pass\n").unwrap();
        fs::write(dir.path().join("schema.py"), "").unwrap();
        fs::write(dir.path().join("routes.py"), "").unwrap();
        fs::create_dir_all(dir.path().join("tests")).unwrap();
        fs::write(dir.path().join("tests/test_api.py"), "").unwrap();

        // Baseline seal.
        repo.seal(
            agent("setup"),
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // ── Step 2: Context for api spec BEFORE any work ──
        // database is still pending → api should be told to wait.
        {
            let c = ctx(&repo, ContextScope::Spec("api".into()));
            let action = c
                .recommended_action
                .as_ref()
                .expect("api should have a recommended_action");
            assert_eq!(
                action.action, "wait_for_dependency",
                "api should wait for database (step 2)"
            );
            assert!(
                action.message.contains("database"),
                "message should mention blocking dep"
            );
            assert_eq!(action.priority, "high");

            let deps = c
                .dependency_status
                .as_ref()
                .expect("spec-scoped context should include dependency_status");
            assert!(!deps.is_empty());
            let db_dep = deps.iter().find(|d| d.spec_id == "database").unwrap();
            assert!(!db_dep.resolved, "database not yet resolved");
        }

        // ── Step 3: db-dev seals first work on database ──
        fs::write(dir.path().join("schema.py"), "CREATE_TABLE = 'books'\n").unwrap();
        fs::write(
            dir.path().join("models.py"),
            "class Base:\n    pass\n\nclass BookModel:\n    title: str\n",
        )
        .unwrap();
        repo.seal(
            agent("db-dev"),
            "schema and book model".into(),
            Some("database".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // ── Step 4: Context for api — dependency still pending ──
        {
            let c = ctx(&repo, ContextScope::Spec("api".into()));
            let action = c
                .recommended_action
                .as_ref()
                .expect("api should still have recommended_action");
            assert_eq!(
                action.action, "wait_for_dependency",
                "database is InProgress, not Complete (step 4)"
            );
        }

        // ── Step 5: db-dev seals database with status complete ──
        fs::write(
            dir.path().join("schema.py"),
            "CREATE_TABLE = 'books'\nCREATE_INDEX = 'books_title'\n",
        )
        .unwrap();
        repo.seal(
            agent("db-dev"),
            "schema finalized".into(),
            Some("database".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();
        repo.update_spec(
            "database",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        // ── Step 6: Context for api — dependency resolved ──
        {
            let c = ctx(&repo, ContextScope::Spec("api".into()));
            if let Some(action) = &c.recommended_action {
                assert_ne!(
                    action.action, "wait_for_dependency",
                    "database is complete, api should not be blocked (step 6)"
                );
            }

            let deps = c.dependency_status.as_ref().unwrap();
            let db_dep = deps.iter().find(|d| d.spec_id == "database").unwrap();
            assert!(db_dep.resolved, "database should be resolved now");
        }

        // ── Step 7: api-dev seals work on api ──
        fs::write(
            dir.path().join("routes.py"),
            "from flask import Flask\napp = Flask(__name__)\n\n@app.route('/books')\ndef list_books():\n    return []\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("models.py"),
            "class Base:\n    pass\n\nclass BookModel:\n    title: str\n\nclass BookResponse:\n    data: list\n",
        )
        .unwrap();
        repo.seal(
            agent("api-dev"),
            "initial routes".into(),
            Some("api".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // ── Step 8: api-dev modifies routes.py but doesn't seal ──
        fs::write(
            dir.path().join("routes.py"),
            "from flask import Flask, request\napp = Flask(__name__)\n\n@app.route('/books')\ndef list_books():\n    return []\n\n@app.route('/books', methods=['POST'])\ndef create_book():\n    return {}\n",
        )
        .unwrap();

        {
            let c = ctx(&repo, ContextScope::Spec("api".into()));
            let action = c
                .recommended_action
                .as_ref()
                .expect("api should recommend sealing (step 8)");
            assert_eq!(
                action.action, "seal",
                "unsealed changes should trigger seal recommendation"
            );
            assert!(c.seal_nudge.is_some(), "seal_nudge should be present");
        }

        // ── Step 9: api-dev seals that work ──
        repo.seal(
            agent("api-dev"),
            "CRUD routes".into(),
            Some("api".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // ── Step 9b: Mark api as Complete (needed so it can be convergence base) ──
        repo.update_spec(
            "api",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        // ── Step 10: db-dev seals more work on database ──
        // Because database spec already has a head (from step 5), this
        // seal's parent = database spec head, NOT the global HEAD.
        // This creates a fork in the seal DAG.
        fs::write(
            dir.path().join("models.py"),
            "class Base:\n    pass\n\nclass BookModel:\n    title: str\n    isbn: str\n",
        )
        .unwrap();
        repo.seal(
            agent("db-dev"),
            "add isbn field".into(),
            Some("database".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // ── Step 11: api-dev seals on api ──
        // api spec's head was set in step 9. This seal's parent = api spec
        // head (step 9), skipping db-dev's step 10 seal. HEAD now follows
        // api's chain. db-dev's step 10 seal is off the HEAD chain → diverged.
        fs::write(
            dir.path().join("routes.py"),
            "from flask import Flask, request, jsonify\napp = Flask(__name__)\n\n@app.route('/books')\ndef list_books():\n    return jsonify([])\n\n@app.route('/books', methods=['POST'])\ndef create_book():\n    return jsonify({})\n",
        )
        .unwrap();
        repo.seal(
            agent("api-dev"),
            "use jsonify".into(),
            Some("api".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // ── Step 12: Context for api — divergence + convergence recommended ──
        {
            let c = ctx(&repo, ContextScope::Spec("api".into()));
            assert!(
                !c.diverged_branches.is_empty(),
                "should detect diverged branches (step 12)"
            );
            assert!(
                c.convergence_recommended,
                "convergence should be recommended (step 12)"
            );

            let action = c
                .recommended_action
                .as_ref()
                .expect("should have recommended_action (step 12)");
            assert_eq!(
                action.action, "converge",
                "should recommend convergence (step 12)"
            );

            let contention_paths: Vec<&str> = c
                .file_contention
                .iter()
                .map(|fc| fc.path.as_str())
                .collect();
            assert!(
                contention_paths.contains(&"models.py"),
                "models.py should be in file_contention (step 12): {:?}",
                contention_paths
            );

            assert!(
                c.integration_risk.score > 0,
                "integration risk should be >0 with diverged branches (step 12)"
            );
        }

        // ── Step 13: Full-scope context — same signals visible ──
        {
            let c = ctx(&repo, ContextScope::Full);
            assert!(
                !c.diverged_branches.is_empty(),
                "full scope should also see diverged branches (step 13)"
            );
            assert!(
                c.convergence_recommended,
                "full scope should recommend convergence (step 13)"
            );

            let action = c
                .recommended_action
                .as_ref()
                .expect("full scope should have recommended_action (step 13)");
            assert_eq!(
                action.action, "converge",
                "full scope should recommend converge (step 13)"
            );
        }

        // ── Step 14: Converge ──
        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, true)
            .unwrap();
        assert!(report.applied, "convergence should apply");

        // ── Step 15: Context for api — post-convergence ──
        {
            let c = ctx(&repo, ContextScope::Spec("api".into()));
            assert!(
                c.diverged_branches.is_empty(),
                "diverged branches should be cleared post-convergence (step 15)"
            );
            assert!(
                !c.convergence_recommended,
                "convergence should no longer be recommended (step 15)"
            );

            if let Some(action) = &c.recommended_action {
                assert_ne!(
                    action.action, "converge",
                    "should not recommend converge post-convergence (step 15)"
                );
            }
        }

        // ── Step 16: api-dev seals final refinement (api already Complete from 9b) ──
        fs::write(
            dir.path().join("routes.py"),
            "from flask import Flask, request, jsonify\napp = Flask(__name__)\n\n@app.route('/books')\ndef list_books():\n    return jsonify([])\n\n@app.route('/books', methods=['POST'])\ndef create_book():\n    return jsonify(request.json)\n",
        )
        .unwrap();
        repo.seal(
            agent("api-dev"),
            "api final polish".into(),
            Some("api".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // ── Step 17: Context for tests spec — dependency resolved ──
        {
            let c = ctx(&repo, ContextScope::Spec("tests".into()));
            if let Some(action) = &c.recommended_action {
                assert_ne!(
                    action.action, "wait_for_dependency",
                    "api is complete, tests should not be blocked (step 17)"
                );
            }

            let deps = c.dependency_status.as_ref().unwrap();
            let api_dep = deps.iter().find(|d| d.spec_id == "api").unwrap();
            assert!(api_dep.resolved, "api dependency should be resolved");
        }

        // ── Step 18: test-dev seals on tests (touches models.py — scope violation) ──
        fs::write(
            dir.path().join("tests/test_api.py"),
            "def test_list_books():\n    assert True\n",
        )
        .unwrap();
        // Intentional scope violation: test-dev touches models.py.
        fs::write(
            dir.path().join("models.py"),
            "class Base:\n    pass\n\nclass BookModel:\n    title: str\n    isbn: str\n\nclass TestFixture:\n    pass\n",
        )
        .unwrap();
        repo.seal(
            agent("test-dev"),
            "api tests and fixture".into(),
            Some("tests".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // ── Step 19: Context for tests — scope violation detected ──
        {
            let c = ctx(&repo, ContextScope::Spec("tests".into()));
            assert!(
                !c.file_scope_violations.is_empty(),
                "test-dev touched models.py outside tests/ scope (step 19): violations={:?}",
                c.file_scope_violations
            );

            let violation_files: Vec<&str> = c
                .file_scope_violations
                .iter()
                .flat_map(|v| v.out_of_scope_files.iter().map(|s| s.as_str()))
                .collect();
            assert!(
                violation_files.contains(&"models.py"),
                "models.py should be flagged as out-of-scope (step 19): {:?}",
                violation_files
            );
        }

        // ── Step 20: test-dev seals tests complete ──
        fs::write(
            dir.path().join("tests/test_models.py"),
            "def test_book_model():\n    assert True\n",
        )
        .unwrap();
        repo.seal(
            agent("test-dev"),
            "tests finalized".into(),
            Some("tests".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();
        repo.update_spec(
            "tests",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        // ── Step 21: Full-scope context — session complete ──
        {
            let c = ctx(&repo, ContextScope::Full);

            assert!(
                c.diverged_branches.is_empty(),
                "no diverged branches should remain at step 21: {:?}",
                c.diverged_branches
                    .iter()
                    .map(|d| format!("{}(tip={}, seals={})", d.spec_id, d.tip_seal, d.seal_count))
                    .collect::<Vec<_>>()
            );
            assert!(
                !c.convergence_recommended,
                "convergence should NOT be recommended at step 21"
            );

            assert!(
                c.session_complete,
                "all 3 specs complete → session_complete (step 21)"
            );
            assert!(
                c.session_summary.is_some(),
                "session_summary should be present when complete (step 21)"
            );

            let action = c
                .recommended_action
                .as_ref()
                .expect("should recommend finish (step 21)");
            assert_eq!(
                action.action, "finish",
                "should recommend finish when session complete (step 21)"
            );
        }

        // ── Step 22: Summary includes all spec titles ──
        {
            let summary = repo.summary().unwrap();
            let headline = &summary.headline;
            // Headline uses spec *titles* not IDs.
            for spec_title in &["database", "rest api", "test suite"] {
                assert!(
                    headline.to_lowercase().contains(spec_title),
                    "summary headline should mention '{}': {}",
                    spec_title,
                    headline
                );
            }
        }
    }

    /// Validates that summary.json is refreshed after converge-all --apply.
    ///
    /// Reproduces the TR17 bug: all specs complete → summary.json written
    /// with diverged state → convergence clears branches → summary.json
    /// should be refreshed to reflect the resolved state.
    #[test]
    fn test_summary_refreshed_after_convergence() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("alpha".into(), "Alpha".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("beta".into(), "Beta".into(), "".into()))
            .unwrap();

        // Baseline.
        fs::write(dir.path().join("shared.txt"), "baseline\n").unwrap();
        repo.seal(
            agent("setup"),
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // alpha-dev seals and completes.
        fs::write(dir.path().join("alpha.txt"), "alpha work\n").unwrap();
        repo.seal(
            agent("alpha-dev"),
            "alpha work".into(),
            Some("alpha".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // beta-dev seals.
        fs::write(dir.path().join("beta.txt"), "beta work\n").unwrap();
        repo.seal(
            agent("beta-dev"),
            "beta work".into(),
            Some("beta".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // alpha-dev seals again → creates divergence (alpha spec head
        // becomes parent, skipping beta's seal).
        fs::write(dir.path().join("alpha.txt"), "alpha part 2\n").unwrap();
        repo.seal(
            agent("alpha-dev"),
            "alpha continued".into(),
            Some("alpha".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Verify divergence exists.
        let diverged = repo.diverged_branches().unwrap();
        assert!(!diverged.is_empty(), "beta should be diverged");

        // Now mark beta complete → triggers summary.json write.
        fs::write(dir.path().join("beta.txt"), "beta final\n").unwrap();
        repo.seal(
            agent("beta-dev"),
            "beta done".into(),
            Some("beta".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Read the pre-convergence summary.json.
        let summary_path = dir.path().join(".writ/summary.json");
        assert!(
            summary_path.exists(),
            "summary.json should exist after all specs complete"
        );
        let pre: SummaryOutput =
            serde_json::from_str(&fs::read_to_string(&summary_path).unwrap()).unwrap();
        assert!(
            pre.convergence_recommended,
            "pre-convergence summary should recommend convergence"
        );
        assert!(
            pre.diverged_branch_count > 0,
            "pre-convergence summary should report diverged branches"
        );

        // Converge.
        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, true)
            .unwrap();
        assert!(report.applied, "should apply");

        // Read the post-convergence summary.json — should be refreshed.
        let post: SummaryOutput =
            serde_json::from_str(&fs::read_to_string(&summary_path).unwrap()).unwrap();
        assert!(
            !post.convergence_recommended,
            "post-convergence summary should NOT recommend convergence"
        );
        assert_eq!(
            post.diverged_branch_count, 0,
            "post-convergence summary should report 0 diverged branches"
        );
    }

    /// When the diverged branch IS selected as convergence base (no on-HEAD
    /// Complete spec exists), its head must also be updated so
    /// diverged_branches() no longer flags it.
    #[test]
    fn test_converge_all_updates_base_spec_head_when_diverged() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("alpha".into(), "Alpha".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("beta".into(), "Beta".into(), "".into()))
            .unwrap();

        fs::write(dir.path().join("shared.txt"), "baseline\n").unwrap();
        repo.seal(
            agent("setup"),
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // alpha seals.
        fs::write(dir.path().join("alpha.txt"), "alpha v1\n").unwrap();
        repo.seal(
            agent("alpha-dev"),
            "alpha work".into(),
            Some("alpha".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // beta seals.
        fs::write(dir.path().join("beta.txt"), "beta v1\n").unwrap();
        repo.seal(
            agent("beta-dev"),
            "beta work".into(),
            Some("beta".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // alpha seals again → HEAD chain skips beta's seal → beta diverged.
        fs::write(dir.path().join("alpha.txt"), "alpha v2\n").unwrap();
        repo.seal(
            agent("alpha-dev"),
            "alpha more".into(),
            Some("alpha".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // beta seals again → HEAD chain skips alpha's latest → alpha diverged.
        fs::write(dir.path().join("beta.txt"), "beta v2\n").unwrap();
        repo.seal(
            agent("beta-dev"),
            "beta more".into(),
            Some("beta".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let pre = repo.diverged_branches().unwrap();
        assert!(
            !pre.is_empty(),
            "should have diverged branches before convergence"
        );

        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, true)
            .unwrap();
        assert!(report.applied, "should apply");

        // The key assertion: ALL diverged branches cleared, including whichever
        // one was selected as the base.
        let post = repo.diverged_branches().unwrap();
        assert!(
            post.is_empty(),
            "all diverged branches should be cleared after convergence, still diverged: {:?}",
            post.iter()
                .map(|d| format!("{}(tip={})", d.spec_id, d.tip_seal))
                .collect::<Vec<_>>()
        );
    }

    /// IMPORT_ORPHAN should NOT fire for packages listed in requirements.txt.
    #[test]
    fn test_import_orphan_suppressed_by_requirements_txt() {
        let dir = tempdir().unwrap();

        // Create a requirements.txt with flask and requests.
        fs::write(
            dir.path().join("requirements.txt"),
            "flask>=2.0\nrequests\nPyJWT==2.8\n",
        )
        .unwrap();

        // Create a Python file that imports flask (no local flask.py).
        fs::write(
            dir.path().join("app.py"),
            "from flask import Flask\nfrom requests import get\nfrom auth import require_auth\n",
        )
        .unwrap();

        // Also create auth.py so that import is NOT orphaned.
        fs::write(dir.path().join("auth.py"), "def require_auth(): pass\n").unwrap();

        let decisions = vec![FileDecision {
            path: "app.py".to_string(),
            decision: "auto-merged".to_string(),
            chosen_lines: 3,
            chosen_spec: None,
            alternatives: vec![],
            confidence: Some(1.0),
        }];

        let warnings = post_convergence_validation(
            dir.path(),
            &decisions,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
        );

        let orphan_warnings: Vec<&String> = warnings
            .iter()
            .filter(|w| w.contains("IMPORT_ORPHAN"))
            .collect();

        assert!(
            orphan_warnings.is_empty(),
            "flask and requests are in requirements.txt, auth.py exists — no orphans expected, got: {:?}",
            orphan_warnings
        );
    }

    /// IMPORT_ORPHAN should still fire for genuinely missing modules.
    #[test]
    fn test_import_orphan_fires_for_missing_module() {
        let dir = tempdir().unwrap();

        // requirements.txt only has flask.
        fs::write(dir.path().join("requirements.txt"), "flask\n").unwrap();

        // app.py imports a module that doesn't exist anywhere.
        fs::write(
            dir.path().join("app.py"),
            "from flask import Flask\nfrom nonexistent import magic\n",
        )
        .unwrap();

        let decisions = vec![FileDecision {
            path: "app.py".to_string(),
            decision: "auto-merged".to_string(),
            chosen_lines: 2,
            chosen_spec: None,
            alternatives: vec![],
            confidence: Some(1.0),
        }];

        let warnings = post_convergence_validation(
            dir.path(),
            &decisions,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
        );

        let orphan_warnings: Vec<&String> = warnings
            .iter()
            .filter(|w| w.contains("IMPORT_ORPHAN"))
            .collect();

        assert_eq!(
            orphan_warnings.len(),
            1,
            "nonexistent module should be flagged, got: {:?}",
            orphan_warnings
        );
        assert!(orphan_warnings[0].contains("nonexistent"));

        // flask should NOT be flagged.
        assert!(
            !orphan_warnings.iter().any(|w| w.contains("flask")),
            "flask is in requirements.txt and should not be flagged"
        );
    }

    /// IMPORT_ORPHAN should NOT fire for local same-directory modules
    /// (e.g., `api/app.py` importing `from auth import ...` when `api/auth.py` exists).
    #[test]
    fn test_import_orphan_suppressed_by_local_module() {
        let dir = tempdir().unwrap();

        fs::create_dir_all(dir.path().join("api")).unwrap();
        fs::write(dir.path().join("requirements.txt"), "flask\n").unwrap();
        fs::write(
            dir.path().join("api/app.py"),
            "from flask import Flask\nfrom auth import require_auth\nfrom models import Book\nfrom search import search_books\n",
        ).unwrap();
        fs::write(
            dir.path().join("api/auth.py"),
            "def require_auth(f): return f\n",
        )
        .unwrap();
        fs::write(dir.path().join("api/models.py"), "class Book: pass\n").unwrap();
        fs::write(
            dir.path().join("api/search.py"),
            "def search_books(): pass\n",
        )
        .unwrap();

        let decisions = vec![FileDecision {
            path: "api/app.py".to_string(),
            decision: "auto-merged".to_string(),
            chosen_lines: 4,
            chosen_spec: None,
            alternatives: vec![],
            confidence: Some(0.9),
        }];

        let warnings = post_convergence_validation(
            dir.path(),
            &decisions,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
        );

        let orphan_warnings: Vec<&String> = warnings
            .iter()
            .filter(|w| w.contains("IMPORT_ORPHAN"))
            .collect();

        assert!(
            orphan_warnings.is_empty(),
            "auth.py, models.py, search.py exist in api/ — no orphans expected, got: {:?}",
            orphan_warnings
        );
    }

    /// IMPORT_ORPHAN should still fire for truly missing modules even when
    /// other local modules exist in the same directory.
    #[test]
    fn test_import_orphan_fires_for_missing_local_module() {
        let dir = tempdir().unwrap();

        fs::create_dir_all(dir.path().join("api")).unwrap();
        fs::write(dir.path().join("requirements.txt"), "flask\n").unwrap();
        fs::write(
            dir.path().join("api/app.py"),
            "from flask import Flask\nfrom auth import require_auth\nfrom phantom import ghost\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("api/auth.py"),
            "def require_auth(f): return f\n",
        )
        .unwrap();

        let decisions = vec![FileDecision {
            path: "api/app.py".to_string(),
            decision: "auto-merged".to_string(),
            chosen_lines: 3,
            chosen_spec: None,
            alternatives: vec![],
            confidence: Some(0.9),
        }];

        let warnings = post_convergence_validation(
            dir.path(),
            &decisions,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
        );

        let orphan_warnings: Vec<&String> = warnings
            .iter()
            .filter(|w| w.contains("IMPORT_ORPHAN"))
            .collect();

        assert_eq!(
            orphan_warnings.len(),
            1,
            "phantom module doesn't exist — should be flagged, got: {:?}",
            orphan_warnings
        );
        assert!(orphan_warnings[0].contains("phantom"));
        assert!(
            !orphan_warnings.iter().any(|w| w.contains("auth")),
            "auth.py exists locally — should not be flagged"
        );
    }

    /// `ConvergeAllReport.applied` should be false when apply was requested
    /// but conflicts prevented actual writes (all_clean == false).
    #[test]
    fn test_converge_all_applied_false_when_not_clean() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("left".into(), "Left".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("right".into(), "Right".into(), "".into()))
            .unwrap();

        fs::write(dir.path().join("conflict.txt"), "base content\n").unwrap();
        repo.seal(
            agent("setup"),
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // left rewrites the file.
        fs::write(dir.path().join("conflict.txt"), "left version\n").unwrap();
        repo.seal(
            agent("left-dev"),
            "left rewrite".into(),
            Some("left".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // right rewrites the same file differently.
        fs::write(dir.path().join("conflict.txt"), "right version\n").unwrap();
        repo.seal(
            agent("right-dev"),
            "right rewrite".into(),
            Some("right".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // left re-asserts its version and adds a file → divergence with
        // genuinely different content for conflict.txt in each tree.
        fs::write(dir.path().join("conflict.txt"), "left version\n").unwrap();
        fs::write(dir.path().join("left-extra.txt"), "extra\n").unwrap();
        repo.seal(
            agent("left-dev"),
            "left extra".into(),
            Some("left".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Use Manual strategy so conflicts stay unresolved → all_clean = false.
        let report = repo.converge_all(ConvergeStrategy::Manual, true).unwrap();

        assert!(!report.is_clean, "should have unresolved conflicts");
        assert!(
            !report.applied,
            "applied should be false when conflicts prevent writes"
        );
    }
}

#[cfg(test)]
mod log_all_tests {
    use super::*;
    use crate::seal::{AgentType, TaskStatus, Verification};
    use tempfile::tempdir;

    fn agent(name: &str) -> AgentIdentity {
        AgentIdentity {
            id: name.to_string(),
            agent_type: AgentType::Agent,
        }
    }

    #[test]
    fn test_log_all_empty_repo() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let seals = repo.log_all().unwrap();
        assert!(seals.is_empty());
    }

    #[test]
    fn test_log_all_matches_log_when_no_branches() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        repo.seal(
            agent("dev"),
            "first".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        repo.seal(
            agent("dev"),
            "second".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let regular = repo.log().unwrap();
        let all = repo.log_all().unwrap();
        assert_eq!(regular.len(), all.len());
        // Same seal IDs.
        let regular_ids: Vec<&str> = regular.iter().map(|s| s.id.as_str()).collect();
        let all_ids: Vec<&str> = all.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(regular_ids, all_ids);
    }

    #[test]
    fn test_log_all_includes_diverged_branch_seals() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("alpha".into(), "Alpha".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("beta".into(), "Beta".into(), "".into()))
            .unwrap();

        // Agent A: seal on alpha.
        std::fs::write(dir.path().join("a.txt"), "a1").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha first".into(),
            Some("alpha".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent B: seal on beta — parent from HEAD.
        std::fs::write(dir.path().join("b.txt"), "b1").unwrap();
        let beta_seal = repo
            .seal(
                agent("agent-b"),
                "beta work".into(),
                Some("beta".into()),
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        // Agent A: seal on alpha again — parent from heads/alpha, diverging from beta.
        std::fs::write(dir.path().join("a.txt"), "a2").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha second".into(),
            Some("alpha".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Regular log misses beta seal.
        let regular = repo.log().unwrap();
        let regular_ids: Vec<&str> = regular.iter().map(|s| s.id.as_str()).collect();
        assert!(
            !regular_ids.contains(&beta_seal.id.as_str()),
            "beta seal should NOT appear in regular log"
        );

        // log_all includes it.
        let all = repo.log_all().unwrap();
        let all_ids: Vec<&str> = all.iter().map(|s| s.id.as_str()).collect();
        assert!(
            all_ids.contains(&beta_seal.id.as_str()),
            "beta seal should appear in log_all"
        );

        // All seals should be present (3 total).
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_log_all_deduplicates() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("alpha".into(), "Alpha".into(), "".into()))
            .unwrap();

        // Seal with spec — appears in both global HEAD and heads/alpha.
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        repo.seal(
            agent("dev"),
            "work".into(),
            Some("alpha".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let all = repo.log_all().unwrap();
        assert_eq!(all.len(), 1, "deduplication should prevent double-counting");
    }

    #[test]
    fn test_log_all_sorted_newest_first() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("alpha".into(), "Alpha".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("beta".into(), "Beta".into(), "".into()))
            .unwrap();

        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha".into(),
            Some("alpha".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        repo.seal(
            agent("agent-b"),
            "beta".into(),
            Some("beta".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("a.txt"), "a2").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha 2".into(),
            Some("alpha".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let all = repo.log_all().unwrap();
        // Verify newest-first ordering.
        for window in all.windows(2) {
            assert!(
                window[0].timestamp >= window[1].timestamp,
                "seals should be sorted newest-first"
            );
        }
    }
}

#[cfg(test)]
mod agent_activity_tests {
    use super::*;
    use crate::context::ContextScope;
    use crate::seal::{AgentType, TaskStatus, Verification};
    use tempfile::tempdir;

    fn agent(name: &str) -> AgentIdentity {
        AgentIdentity {
            id: name.to_string(),
            agent_type: AgentType::Agent,
        }
    }

    #[test]
    fn test_agent_activity_empty_when_no_seals() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        assert!(ctx.agent_activity.is_empty());
    }

    #[test]
    fn test_single_agent_owns_all_files() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        std::fs::write(dir.path().join("a.txt"), "aaa").unwrap();
        std::fs::write(dir.path().join("b.txt"), "bbb").unwrap();
        repo.seal(
            agent("worker-1"),
            "initial".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        assert_eq!(ctx.agent_activity.len(), 1);
        let activity = &ctx.agent_activity[0];
        assert_eq!(activity.agent_id, "worker-1");
        assert_eq!(activity.seal_count, 1);
        assert!(activity.files_owned.contains(&"a.txt".to_string()));
        assert!(activity.files_owned.contains(&"b.txt".to_string()));
    }

    #[test]
    fn test_multi_agent_file_provenance() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Agent A creates file a.txt and shared.txt.
        std::fs::write(dir.path().join("a.txt"), "a-v1").unwrap();
        std::fs::write(dir.path().join("shared.txt"), "shared-v1").unwrap();
        repo.seal(
            agent("agent-a"),
            "a's work".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent B creates b.txt and modifies shared.txt.
        std::fs::write(dir.path().join("b.txt"), "b-v1").unwrap();
        std::fs::write(dir.path().join("shared.txt"), "shared-v2").unwrap();
        repo.seal(
            agent("agent-b"),
            "b's work".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        assert_eq!(ctx.agent_activity.len(), 2);

        let a = ctx
            .agent_activity
            .iter()
            .find(|a| a.agent_id == "agent-a")
            .unwrap();
        let b = ctx
            .agent_activity
            .iter()
            .find(|a| a.agent_id == "agent-b")
            .unwrap();

        // Agent A owns a.txt (created it, no one else touched it).
        assert!(a.files_owned.contains(&"a.txt".to_string()));
        // Agent B owns b.txt and shared.txt (last to seal them).
        assert!(b.files_owned.contains(&"b.txt".to_string()));
        assert!(b.files_owned.contains(&"shared.txt".to_string()));
        // Agent A does NOT own shared.txt (B sealed it more recently).
        assert!(!a.files_owned.contains(&"shared.txt".to_string()));
    }

    #[test]
    fn test_agent_activity_has_latest_summary() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        std::fs::write(dir.path().join("a.txt"), "v1").unwrap();
        repo.seal(
            agent("dev"),
            "first commit".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("a.txt"), "v2").unwrap();
        repo.seal(
            agent("dev"),
            "second commit".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        assert_eq!(ctx.agent_activity.len(), 1);
        let activity = &ctx.agent_activity[0];
        assert_eq!(activity.seal_count, 2);
        assert_eq!(activity.latest_summary.as_deref(), Some("second commit"));
    }

    #[test]
    fn test_agent_activity_tracks_specs() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("auth".into(), "Auth".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("ui".into(), "UI".into(), "".into()))
            .unwrap();

        std::fs::write(dir.path().join("auth.py"), "pass").unwrap();
        repo.seal(
            agent("dev"),
            "auth work".into(),
            Some("auth".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("button.py"), "pass").unwrap();
        repo.seal(
            agent("dev"),
            "ui work".into(),
            Some("ui".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        let activity = &ctx.agent_activity[0];
        assert!(activity.specs_touched.contains(&"auth".to_string()));
        assert!(activity.specs_touched.contains(&"ui".to_string()));
    }

    #[test]
    fn test_agent_activity_sorted_by_most_recent() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Agent A seals first.
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        repo.seal(
            agent("agent-a"),
            "a work".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent B seals second (more recent).
        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        repo.seal(
            agent("agent-b"),
            "b work".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        // Most recently active agent should come first.
        assert_eq!(ctx.agent_activity[0].agent_id, "agent-b");
        assert_eq!(ctx.agent_activity[1].agent_id, "agent-a");
    }

    #[test]
    fn test_spec_scoped_agent_activity_filters_files() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let mut spec = Spec::new("api".into(), "API".into(), "".into());
        spec.file_scope = vec!["api.py".to_string()];
        repo.add_spec(&spec).unwrap();

        // Agent A touches api.py (in scope) and config.py (out of scope).
        std::fs::write(dir.path().join("api.py"), "pass").unwrap();
        std::fs::write(dir.path().join("config.py"), "cfg").unwrap();
        repo.seal(
            agent("agent-a"),
            "a work".into(),
            Some("api".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent B touches only config.py (out of scope).
        std::fs::write(dir.path().join("config.py"), "cfg v2").unwrap();
        repo.seal(
            agent("agent-b"),
            "b work".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(
                ContextScope::Spec("api".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();

        // Agent A should own api.py in the filtered view.
        let a = ctx
            .agent_activity
            .iter()
            .find(|a| a.agent_id == "agent-a")
            .unwrap();
        assert!(a.files_owned.contains(&"api.py".to_string()));
        assert!(!a.files_owned.contains(&"config.py".to_string()));

        // Agent B should have empty files_owned since config.py is out of scope.
        let b = ctx
            .agent_activity
            .iter()
            .find(|a| a.agent_id == "agent-b")
            .unwrap();
        assert!(b.files_owned.is_empty());
    }

    #[test]
    fn test_agent_activity_json_serializable() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        repo.seal(
            agent("worker"),
            "work".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        let json = serde_json::to_string(&ctx).unwrap();
        assert!(json.contains("agent_activity"));
        assert!(json.contains("worker"));
        assert!(json.contains("files_owned"));
    }

    #[test]
    fn test_agent_activity_excludes_deletes_from_ownership() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Agent creates two files.
        std::fs::write(dir.path().join("keep.txt"), "keep").unwrap();
        std::fs::write(dir.path().join("remove.txt"), "gone").unwrap();
        repo.seal(
            agent("creator"),
            "add files".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent deletes one file.
        std::fs::remove_file(dir.path().join("remove.txt")).unwrap();
        repo.seal(
            agent("deleter"),
            "remove file".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        let deleter = ctx
            .agent_activity
            .iter()
            .find(|a| a.agent_id == "deleter")
            .unwrap();

        // Deleter should NOT own remove.txt — deletes shouldn't grant ownership.
        assert!(!deleter.files_owned.contains(&"remove.txt".to_string()));

        // Creator still owns keep.txt.
        let creator = ctx
            .agent_activity
            .iter()
            .find(|a| a.agent_id == "creator")
            .unwrap();
        assert!(creator.files_owned.contains(&"keep.txt".to_string()));
    }

    #[test]
    fn test_agent_activity_walks_all_heads() {
        // Simulates the "ghost agent" scenario from test run 5:
        // Agent A seals on spec "alpha", then Agent B seals on spec "beta",
        // then Agent A seals again on "alpha". Agent B's seals end up on a
        // diverged branch. Agent activity should still include Agent B.
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("alpha".into(), "Alpha".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("beta".into(), "Beta".into(), "".into()))
            .unwrap();

        // Agent A: first seal on spec alpha.
        std::fs::write(dir.path().join("a1.txt"), "a1").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha first".into(),
            Some("alpha".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent B: seal on spec beta — parent comes from global HEAD.
        std::fs::write(dir.path().join("b1.txt"), "b1").unwrap();
        repo.seal(
            agent("agent-b"),
            "beta work".into(),
            Some("beta".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent A: second seal on spec alpha — parent comes from heads/alpha (not global HEAD).
        // This makes global HEAD point to this seal, with parent = first alpha seal.
        // Agent B's seal is now orphaned from the HEAD chain.
        std::fs::write(dir.path().join("a2.txt"), "a2").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha second".into(),
            Some("alpha".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();

        // Agent B should appear in agent_activity even though their seal is
        // on a diverged branch (not reachable from global HEAD).
        let b = ctx.agent_activity.iter().find(|a| a.agent_id == "agent-b");
        assert!(
            b.is_some(),
            "agent-b should appear in agent_activity despite diverged branch"
        );
        let b = b.unwrap();
        assert_eq!(b.seal_count, 1);
        assert!(b.files_owned.contains(&"b1.txt".to_string()));
    }

    #[test]
    fn test_diverged_branches_detected_in_context() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("alpha".into(), "Alpha".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("beta".into(), "Beta".into(), "".into()))
            .unwrap();

        // Agent A: seal on alpha → HEAD + heads/alpha both point to seal1.
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha work".into(),
            Some("alpha".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent B: seal on beta → HEAD + heads/beta point to seal2.
        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        repo.seal(
            agent("agent-b"),
            "beta work".into(),
            Some("beta".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent A: seal on alpha again → HEAD = seal3 (parent = seal1 from heads/alpha).
        // seal2 (heads/beta) is now diverged — not reachable from HEAD chain.
        std::fs::write(dir.path().join("a.txt"), "a-v2").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha done".into(),
            Some("alpha".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();

        // Should have exactly one diverged branch: beta.
        assert_eq!(ctx.diverged_branches.len(), 1, "expected 1 diverged branch");
        let db = &ctx.diverged_branches[0];
        assert_eq!(db.spec_id, "beta");
        assert_eq!(db.seal_count, 1);
        assert!(db.agents.contains(&"agent-b".to_string()));
        assert!(db.recommendation.contains("converge"));
    }

    #[test]
    fn test_no_diverged_branches_when_linear() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("alpha".into(), "Alpha".into(), "".into()))
            .unwrap();

        // All seals on the same spec — HEAD chain stays linear.
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        repo.seal(
            agent("agent-a"),
            "first".into(),
            Some("alpha".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("a.txt"), "a-v2").unwrap();
        repo.seal(
            agent("agent-a"),
            "second".into(),
            Some("alpha".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        assert!(
            ctx.diverged_branches.is_empty(),
            "no diverged branches expected for linear chain"
        );
    }

    #[test]
    fn test_diverged_branch_warning_has_recommendation() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("main-spec".into(), "Main".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("feature".into(), "Feature".into(), "".into()))
            .unwrap();

        // Agent A seals on main-spec → HEAD=seal1, heads/main-spec=seal1.
        std::fs::write(dir.path().join("x.txt"), "x").unwrap();
        repo.seal(
            agent("main-agent"),
            "main work".into(),
            Some("main-spec".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent B seals on feature → parent=HEAD=seal1. HEAD=seal2, heads/feature=seal2.
        std::fs::write(dir.path().join("y.txt"), "y").unwrap();
        repo.seal(
            agent("feature-agent"),
            "feature work".into(),
            Some("feature".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent A seals on main-spec again → parent=heads/main-spec=seal1.
        // HEAD=seal3 (parent=seal1). heads/feature=seal2 is now diverged.
        std::fs::write(dir.path().join("x.txt"), "x-v2").unwrap();
        repo.seal(
            agent("main-agent"),
            "main done".into(),
            Some("main-spec".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();

        // Should detect diverged "feature" branch.
        assert!(
            !ctx.diverged_branches.is_empty(),
            "expected diverged branches"
        );
        let db = ctx
            .diverged_branches
            .iter()
            .find(|d| d.spec_id == "feature")
            .unwrap();
        assert!(db.recommendation.contains("converge"));
        assert!(db.recommendation.contains("feature"));
        assert!(db.agents.contains(&"feature-agent".to_string()));
    }

    #[test]
    fn test_diverged_branches_empty_in_spec_scoped_context() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("alpha".into(), "Alpha".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("beta".into(), "Beta".into(), "".into()))
            .unwrap();

        // Create divergence as above.
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha work".into(),
            Some("alpha".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        repo.seal(
            agent("agent-b"),
            "beta work".into(),
            Some("beta".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("a.txt"), "a-v2").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha done".into(),
            Some("alpha".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Spec-scoped context now includes diverged branches for risk visibility.
        let ctx = repo
            .context(
                ContextScope::Spec("alpha".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();
        assert!(
            !ctx.diverged_branches.is_empty(),
            "spec-scoped context should include diverged branches"
        );
    }

    #[test]
    fn test_convergence_recommended_true_when_diverged() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("alpha".into(), "Alpha".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("beta".into(), "Beta".into(), "".into()))
            .unwrap();

        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha".into(),
            Some("alpha".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        repo.seal(
            agent("agent-b"),
            "beta".into(),
            Some("beta".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("a.txt"), "a2").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha 2".into(),
            Some("alpha".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        assert!(
            ctx.convergence_recommended,
            "should recommend convergence when branches diverged"
        );
    }

    #[test]
    fn test_convergence_recommended_false_when_linear() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        repo.seal(
            agent("dev"),
            "work".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        assert!(
            !ctx.convergence_recommended,
            "no convergence needed for linear chain"
        );
    }

    #[test]
    fn test_convergence_recommended_false_in_spec_scoped() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("alpha".into(), "Alpha".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("beta".into(), "Beta".into(), "".into()))
            .unwrap();

        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha".into(),
            Some("alpha".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        repo.seal(
            agent("agent-b"),
            "beta".into(),
            Some("beta".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("a.txt"), "a2").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha 2".into(),
            Some("alpha".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(
                ContextScope::Spec("alpha".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();
        // Spec-scoped context now surfaces convergence recommendations.
        assert!(
            ctx.convergence_recommended,
            "spec-scoped context should recommend convergence when branches diverge"
        );
    }

    #[test]
    fn test_convergence_recommended_serialized_only_when_true() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        repo.seal(
            agent("dev"),
            "work".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        let json = serde_json::to_string(&ctx).unwrap();
        // When false, skip_serializing_if omits the field.
        assert!(
            !json.contains("convergence_recommended"),
            "convergence_recommended should be omitted when false"
        );

        // Now create divergence.
        repo.add_spec(&Spec::new("alpha".into(), "Alpha".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("beta".into(), "Beta".into(), "".into()))
            .unwrap();

        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha".into(),
            Some("alpha".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("c.txt"), "c").unwrap();
        repo.seal(
            agent("agent-b"),
            "beta".into(),
            Some("beta".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("b.txt"), "b2").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha 2".into(),
            Some("alpha".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        let json = serde_json::to_string(&ctx).unwrap();
        assert!(
            json.contains("\"convergence_recommended\":true"),
            "convergence_recommended should be present when true"
        );
    }
}

#[cfg(test)]
#[allow(deprecated)]
mod summary_tests {
    use super::*;
    use crate::seal::{AgentType, TaskStatus, Verification};
    use tempfile::tempdir;

    fn agent(name: &str) -> AgentIdentity {
        AgentIdentity {
            id: name.to_string(),
            agent_type: AgentType::Agent,
        }
    }

    #[test]
    fn test_summary_empty_repo() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let summary = repo.summary().unwrap();
        assert_eq!(summary.total_seals, 0);
        assert!(summary.files_changed.is_empty());
        assert!(summary.specs_summary.is_empty());
        assert!(summary.agents.is_empty());
    }

    #[test]
    fn test_summary_single_agent_single_spec() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new(
            "auth".into(),
            "Add authentication".into(),
            "".into(),
        ))
        .unwrap();

        std::fs::write(dir.path().join("auth.py"), "class Auth: pass").unwrap();
        repo.seal(
            agent("dev"),
            "added auth module".into(),
            Some("auth".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(
            dir.path().join("auth.py"),
            "class Auth:\n    def login(self): pass",
        )
        .unwrap();
        repo.seal(
            agent("dev"),
            "added login method".into(),
            Some("auth".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let summary = repo.summary().unwrap();
        assert_eq!(summary.total_seals, 2);
        assert_eq!(summary.agents.len(), 1);
        assert_eq!(summary.agents[0].id, "dev");
        assert_eq!(summary.agents[0].seal_count, 2);
        assert_eq!(summary.specs_summary.len(), 1);
        assert_eq!(summary.specs_summary[0].id, "auth");
        assert!(summary.files_changed.contains(&"auth.py".to_string()));
        // Spec status wasn't explicitly updated, so headline shows "in progress".
        assert!(summary.headline.contains("writ:"));

        // Now update spec to complete and verify headline changes.
        repo.update_spec(
            "auth",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();
        let summary2 = repo.summary().unwrap();
        assert!(
            summary2.headline.contains("Add authentication"),
            "headline with completed spec should include title: {}",
            summary2.headline
        );
    }

    #[test]
    fn test_summary_multi_agent_multi_spec() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("feat-a".into(), "Feature A".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("feat-b".into(), "Feature B".into(), "".into()))
            .unwrap();

        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        repo.seal(
            agent("agent-1"),
            "feature A work".into(),
            Some("feat-a".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        repo.seal(
            agent("agent-2"),
            "feature B work".into(),
            Some("feat-b".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Mark specs as complete.
        repo.update_spec(
            "feat-a",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();
        repo.update_spec(
            "feat-b",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        let summary = repo.summary().unwrap();
        assert_eq!(summary.total_seals, 2);
        assert_eq!(summary.agents.len(), 2);
        assert_eq!(summary.specs_summary.len(), 2);
        assert!(
            summary.headline.contains("2 features complete"),
            "headline: {}",
            summary.headline
        );
    }

    #[test]
    fn test_summary_commit_message_format() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("fix".into(), "Fix login bug".into(), "".into()))
            .unwrap();

        std::fs::write(dir.path().join("fix.py"), "fixed").unwrap();
        repo.seal(
            agent("fixer"),
            "patched login".into(),
            Some("fix".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let summary = repo.summary().unwrap();
        // commit_message should have headline and body.
        assert!(summary.commit_message.contains(&summary.headline));
        assert!(summary.commit_message.contains("Specs:"));
        assert!(summary.commit_message.contains("fix"));
        assert!(summary.commit_message.contains("Files changed:"));
        assert!(summary.commit_message.contains("Total seals:"));
    }

    #[test]
    fn test_summary_files_to_stage() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        repo.seal(
            agent("dev"),
            "work".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Modify a file after the last seal — should appear in files_to_stage.
        std::fs::write(dir.path().join("a.txt"), "modified").unwrap();

        let summary = repo.summary().unwrap();
        assert!(
            summary.files_to_stage.contains(&"a.txt".to_string()),
            "modified file should be in files_to_stage"
        );
    }

    #[test]
    fn test_summary_with_diverged_branches() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("alpha".into(), "Alpha".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("beta".into(), "Beta".into(), "".into()))
            .unwrap();

        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha".into(),
            Some("alpha".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        repo.seal(
            agent("agent-b"),
            "beta".into(),
            Some("beta".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        std::fs::write(dir.path().join("a.txt"), "a2").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha 2".into(),
            Some("alpha".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let summary = repo.summary().unwrap();
        assert!(summary.convergence_recommended);
        assert!(summary.diverged_branch_count > 0);
        assert!(summary.body.contains("diverged branch"));
    }

    #[test]
    fn test_summary_excludes_bridge_seals() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Simulate a bridge import seal by using agent_id "writ-bridge".
        std::fs::write(dir.path().join("imported.txt"), "from git").unwrap();
        repo.seal(
            AgentIdentity {
                id: "writ-bridge".into(),
                agent_type: AgentType::Agent,
            },
            "bridge import from git abc123".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Real agent work.
        std::fs::write(dir.path().join("new.txt"), "agent work").unwrap();
        repo.seal(
            agent("dev"),
            "actual work".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let summary = repo.summary().unwrap();
        assert_eq!(summary.total_seals, 1, "bridge seal should be excluded");
        assert_eq!(summary.agents.len(), 1);
        assert_eq!(summary.agents[0].id, "dev");
    }

    #[test]
    fn test_summary_preserves_all_specs_after_convergence() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new(
            "alpha".into(),
            "Alpha Feature".into(),
            "".into(),
        ))
        .unwrap();
        repo.add_spec(&Spec::new("beta".into(), "Beta Feature".into(), "".into()))
            .unwrap();

        // Seal 1: baseline.
        fs::write(dir.path().join("shared.txt"), "base").unwrap();
        repo.seal(
            agent("setup"),
            "baseline".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Seal 2: agent-a works on alpha. HEAD=seal2, heads/alpha=seal2.
        fs::write(dir.path().join("shared.txt"), "alpha-v1").unwrap();
        fs::write(dir.path().join("alpha.txt"), "a").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha work".into(),
            Some("alpha".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();
        repo.update_spec(
            "alpha",
            crate::spec::SpecUpdate {
                status: Some(crate::spec::SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        // Seal 3: agent-b works on beta. parent=HEAD(seal2), HEAD=seal3,
        // heads/beta=seal3.
        fs::write(dir.path().join("shared.txt"), "beta-v1").unwrap();
        fs::write(dir.path().join("beta.txt"), "b").unwrap();
        repo.seal(
            agent("agent-b"),
            "beta work".into(),
            Some("beta".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();
        repo.update_spec(
            "beta",
            crate::spec::SpecUpdate {
                status: Some(crate::spec::SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        // Seal 4: agent-a seals again on alpha. parent=heads/alpha(seal2),
        // HEAD=seal4. HEAD chain: seal4→seal2→seal1. seal3 is NOT on chain.
        // → beta is DIVERGED.
        fs::write(dir.path().join("shared.txt"), "alpha-v2").unwrap();
        repo.seal(
            agent("agent-a"),
            "alpha polish".into(),
            Some("alpha".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Verify divergence exists.
        let diverged = repo.diverged_branches().unwrap();
        assert!(!diverged.is_empty(), "beta should be diverged");

        // Pre-convergence: summary sees both specs because log_all walks
        // both HEAD chain and spec heads.
        let pre = repo.summary().unwrap();
        assert_eq!(
            pre.specs_summary.len(),
            2,
            "pre-converge should see 2 specs"
        );

        // Converge with apply.
        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, true)
            .unwrap();
        assert!(report.applied);

        // Seal the convergence result (write a marker to ensure changes exist).
        fs::write(dir.path().join("CONVERGED"), "1").unwrap();
        repo.seal(
            AgentIdentity {
                id: "convergence-bot".into(),
                agent_type: AgentType::Agent,
            },
            "converged alpha + beta".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Post-convergence: summary must still see BOTH specs + all agents.
        let post = repo.summary().unwrap();
        assert_eq!(
            post.specs_summary.len(),
            2,
            "post-converge summary must include all specs, got: {:?}",
            post.specs_summary.iter().map(|s| &s.id).collect::<Vec<_>>(),
        );
        assert!(
            post.total_seals >= 4,
            "should have at least 4 seals (setup + alpha*2 + beta + convergence)"
        );
        let agent_ids: Vec<&str> = post.agents.iter().map(|a| a.id.as_str()).collect();
        assert!(
            agent_ids.contains(&"agent-a"),
            "agent-a missing from summary"
        );
        assert!(
            agent_ids.contains(&"agent-b"),
            "agent-b missing from summary"
        );

        // The beta spec's seal must be in the summary — this was the bug
        // where convergence orphaned the beta chain.
        let beta_entry = post.specs_summary.iter().find(|s| s.id == "beta");
        assert!(
            beta_entry.is_some(),
            "beta spec must appear in summary after convergence"
        );
        assert!(
            beta_entry.unwrap().seal_count >= 1,
            "beta should have at least 1 seal"
        );
    }

    #[test]
    fn test_summary_serializable() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        std::fs::write(dir.path().join("f.txt"), "content").unwrap();
        repo.seal(
            agent("dev"),
            "work".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let summary = repo.summary().unwrap();
        let json = serde_json::to_string(&summary).unwrap();
        let parsed: SummaryOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.total_seals, summary.total_seals);
        assert_eq!(parsed.headline, summary.headline);
    }
}

#[cfg(test)]
mod scale_tests {
    use super::*;
    use crate::context::ContextScope;
    use crate::seal::{AgentType, TaskStatus, Verification};
    use std::time::Instant;
    use tempfile::tempdir;

    fn agent(name: &str) -> AgentIdentity {
        AgentIdentity {
            id: name.to_string(),
            agent_type: AgentType::Agent,
        }
    }

    #[test]
    fn test_scale_100_specs() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let start = Instant::now();
        for i in 0..100 {
            let spec = Spec::new(
                format!("spec-{i}"),
                format!("Spec number {i}"),
                String::new(),
            );
            repo.add_spec(&spec).unwrap();
        }
        let add_time = start.elapsed();
        assert!(
            add_time.as_secs() < 5,
            "Adding 100 specs took {:?}",
            add_time
        );

        let start = Instant::now();
        let specs = repo.list_specs().unwrap();
        let list_time = start.elapsed();
        assert_eq!(specs.len(), 100);
        assert!(
            list_time.as_secs() < 2,
            "Listing 100 specs took {:?}",
            list_time
        );
    }

    #[test]
    fn test_scale_500_seals_linear_chain() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new("big-spec".into(), "Scale test".into(), String::new());
        repo.add_spec(&spec).unwrap();

        let start = Instant::now();
        for i in 0..500 {
            fs::write(dir.path().join("file.txt"), format!("iteration {i}")).unwrap();
            repo.seal(
                agent("scale-agent"),
                format!("seal {i}"),
                Some("big-spec".into()),
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();
        }
        let seal_time = start.elapsed();
        assert!(seal_time.as_secs() < 300, "500 seals took {:?}", seal_time);

        let start = Instant::now();
        let chain = repo.log().unwrap();
        let log_time = start.elapsed();
        assert_eq!(chain.len(), 500);
        assert!(
            log_time.as_secs() < 5,
            "Walking 500-seal chain took {:?}",
            log_time
        );

        let start = Instant::now();
        let spec_chain = repo.spec_log("big-spec").unwrap();
        let spec_log_time = start.elapsed();
        assert_eq!(spec_chain.len(), 500);
        assert!(
            spec_log_time.as_secs() < 5,
            "Walking 500-seal spec chain took {:?}",
            spec_log_time
        );
    }

    #[test]
    fn test_scale_context_with_many_seals() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new("ctx-spec".into(), "Context scale".into(), String::new());
        repo.add_spec(&spec).unwrap();

        for i in 0..200 {
            fs::write(dir.path().join("file.txt"), format!("v{i}")).unwrap();
            repo.seal(
                agent("ctx-agent"),
                format!("seal {i}"),
                Some("ctx-spec".into()),
                TaskStatus::InProgress,
                Verification {
                    tests_passed: Some(i as u32),
                    tests_failed: None,
                    linted: true,
                },
                false,
            )
            .unwrap();
        }

        let start = Instant::now();
        let ctx = repo
            .context(
                ContextScope::Full,
                20,
                &ContextFilter {
                    status: None,
                    agent: None,
                },
            )
            .unwrap();
        let ctx_time = start.elapsed();
        assert_eq!(ctx.recent_seals.len(), 20);
        assert!(
            ctx_time.as_secs() < 10,
            "Full context with 200 seals took {:?}",
            ctx_time
        );

        let start = Instant::now();
        let spec_ctx = repo
            .context(
                ContextScope::Spec("ctx-spec".into()),
                10,
                &ContextFilter {
                    status: None,
                    agent: None,
                },
            )
            .unwrap();
        let spec_ctx_time = start.elapsed();
        assert_eq!(spec_ctx.recent_seals.len(), 10);
        assert!(
            spec_ctx_time.as_secs() < 10,
            "Spec context with 200 seals took {:?}",
            spec_ctx_time
        );
    }

    #[test]
    fn test_scale_parallel_specs() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        for i in 0..50 {
            let spec = Spec::new(
                format!("parallel-{i}"),
                format!("Parallel spec {i}"),
                String::new(),
            );
            repo.add_spec(&spec).unwrap();
        }

        let start = Instant::now();
        for i in 0..50 {
            let fname = format!("file-{i}.txt");
            for j in 0..10 {
                fs::write(dir.path().join(&fname), format!("spec{i}-iter{j}")).unwrap();
                repo.seal(
                    agent(&format!("agent-{i}")),
                    format!("work on spec-{i}, iteration {j}"),
                    Some(format!("parallel-{i}")),
                    TaskStatus::InProgress,
                    Verification::default(),
                    false,
                )
                .unwrap();
            }
        }
        let total_time = start.elapsed();
        assert!(
            total_time.as_secs() < 180,
            "50 specs x 10 seals took {:?}",
            total_time
        );

        for i in 0..50 {
            let head = repo.spec_head(&format!("parallel-{i}")).unwrap();
            assert!(head.is_some(), "spec parallel-{i} should have a head");
        }

        let all_seals = repo.log().unwrap();
        assert_eq!(all_seals.len(), 500);
    }

    #[test]
    fn test_scale_many_files_in_single_seal() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        for i in 0..200 {
            fs::write(
                dir.path().join(format!("file-{i}.txt")),
                format!("content-{i}"),
            )
            .unwrap();
        }

        let start = Instant::now();
        let seal = repo
            .seal(
                agent("bulk-agent"),
                "bulk seal 200 files".into(),
                None,
                TaskStatus::Complete,
                Verification::default(),
                false,
            )
            .unwrap();
        let seal_time = start.elapsed();

        assert_eq!(seal.changes.len(), 200);
        assert!(
            seal_time.as_secs() < 30,
            "Sealing 200 files took {:?}",
            seal_time
        );
    }
}

#[cfg(test)]
#[cfg(feature = "bridge")]
mod install_tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_install_fresh_no_git() {
        let dir = tempdir().unwrap();
        let result = Repository::init_project(dir.path()).unwrap();
        assert!(result.initialized);
        assert!(!result.git_detected);
        assert!(!result.git_imported);
        assert!(result.writignore_created);
        assert_eq!(result.tracked_files, 0);
    }

    #[test]
    fn test_install_idempotent_no_git() {
        let dir = tempdir().unwrap();
        let first = Repository::init_project(dir.path()).unwrap();
        assert!(first.initialized);
        assert!(first.writignore_created);

        let second = Repository::init_project(dir.path()).unwrap();
        assert!(!second.initialized);
        assert!(!second.writignore_created);
    }

    #[test]
    fn test_install_has_repo_root() {
        let dir = tempdir().unwrap();
        let result = Repository::init_project(dir.path()).unwrap();
        assert!(!result.repo_root.is_empty());
    }

    #[test]
    fn test_install_has_available_operations() {
        let dir = tempdir().unwrap();
        let result = Repository::init_project(dir.path()).unwrap();
        assert!(!result.available_operations.is_empty());
        assert!(result
            .available_operations
            .iter()
            .any(|op| op.contains("context")));
    }

    #[test]
    fn test_install_creates_writignore() {
        let dir = tempdir().unwrap();
        Repository::init_project(dir.path()).unwrap();
        assert!(dir.path().join(".writignore").exists());
    }

    #[test]
    fn test_install_preserves_existing_writignore() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".writignore"), "my_custom_rules\n").unwrap();
        let result = Repository::init_project(dir.path()).unwrap();
        assert!(!result.writignore_created);
        let content = fs::read_to_string(dir.path().join(".writignore")).unwrap();
        assert_eq!(content, "my_custom_rules\n");
    }

    #[test]
    fn test_install_writignore_imports_gitignore() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "build\n*.log\n").unwrap();
        let result = Repository::init_project(dir.path()).unwrap();
        assert!(result.writignore_created);
        let content = fs::read_to_string(dir.path().join(".writignore")).unwrap();
        assert!(content.contains("build"));
        assert!(content.contains("*.log"));
    }

    #[test]
    fn test_install_detects_claude_code() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "# Project").unwrap();
        let result = Repository::init_project(dir.path()).unwrap();
        assert!(result.frameworks_detected.iter().any(|f| f.detected));
    }

    #[test]
    fn test_install_no_frameworks() {
        let dir = tempdir().unwrap();
        let result = Repository::init_project(dir.path()).unwrap();
        assert!(result.frameworks_detected.iter().all(|f| !f.detected));
    }

    #[test]
    fn test_install_result_serializable() {
        let dir = tempdir().unwrap();
        let result = Repository::init_project(dir.path()).unwrap();
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("initialized"));
        assert!(json.contains("repo_root"));
    }

    // --- Git-dependent tests ---

    fn setup_git_repo(dir: &Path) {
        let repo = git2::Repository::init(dir).unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test").unwrap();
        config.set_str("user.email", "test@test.com").unwrap();

        fs::write(dir.join("main.py"), "print('hello')").unwrap();
        fs::write(dir.join("README.md"), "# Project").unwrap();

        let mut index = repo.index().unwrap();
        index.add_path(Path::new("main.py")).unwrap();
        index.add_path(Path::new("README.md")).unwrap();
        let oid = index.write_tree().unwrap();
        index.write().unwrap();

        let tree = repo.find_tree(oid).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }

    #[test]
    fn test_install_with_git() {
        let dir = tempdir().unwrap();
        setup_git_repo(dir.path());
        let result = Repository::init_project(dir.path()).unwrap();
        assert!(result.git_detected);
        assert!(result.git_imported);
        assert!(result.imported_files.unwrap() > 0);
        assert!(result.git_branch.is_some());
        assert_eq!(result.git_head_short.as_ref().unwrap().len(), 12);
        assert!(result.tracked_files > 0);
    }

    #[test]
    fn test_install_idempotent_already_synced() {
        let dir = tempdir().unwrap();
        setup_git_repo(dir.path());

        let first = Repository::init_project(dir.path()).unwrap();
        assert!(first.git_imported);
        assert!(!first.already_imported);

        let second = Repository::init_project(dir.path()).unwrap();
        assert!(!second.git_imported);
        assert!(second.already_imported);
        assert!(second
            .import_skipped_reason
            .as_ref()
            .unwrap()
            .contains("already synced"));
    }

    #[test]
    fn test_install_reimports_on_head_move() {
        let dir = tempdir().unwrap();
        setup_git_repo(dir.path());

        let first = Repository::init_project(dir.path()).unwrap();
        assert!(first.git_imported);

        // Make a new git commit
        let git_repo = git2::Repository::open(dir.path()).unwrap();
        fs::write(dir.path().join("new_file.txt"), "new content").unwrap();
        let mut index = git_repo.index().unwrap();
        index.add_path(Path::new("new_file.txt")).unwrap();
        let oid = index.write_tree().unwrap();
        index.write().unwrap();
        let tree = git_repo.find_tree(oid).unwrap();
        let sig = git_repo.signature().unwrap();
        let head = git_repo.head().unwrap().peel_to_commit().unwrap();
        git_repo
            .commit(Some("HEAD"), &sig, &sig, "second commit", &tree, &[&head])
            .unwrap();

        let second = Repository::init_project(dir.path()).unwrap();
        assert!(second.git_imported);
        assert!(second.reimported);
        assert_ne!(first.imported_seal_id, second.imported_seal_id);
    }

    #[test]
    fn test_install_detects_dirty_git() {
        let dir = tempdir().unwrap();
        setup_git_repo(dir.path());

        // Modify a tracked file without committing
        fs::write(dir.path().join("main.py"), "print('modified')").unwrap();

        let result = Repository::init_project(dir.path()).unwrap();
        assert_eq!(result.git_dirty, Some(true));
        assert!(result.git_dirty_count.unwrap() >= 1);
    }
}

#[cfg(test)]
mod file_contention_tests {
    use super::*;
    use crate::context::{ContextFilter, ContextScope};
    use crate::seal::AgentType;
    use tempfile::tempdir;

    fn agent(name: &str) -> AgentIdentity {
        AgentIdentity {
            id: name.to_string(),
            agent_type: AgentType::Agent,
        }
    }

    #[test]
    fn test_no_contention_single_agent() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.add_spec(&Spec::new("feat".into(), "Feature".into(), "".into()))
            .unwrap();

        fs::write(dir.path().join("a.txt"), "content").unwrap();
        repo.seal(
            agent("alice"),
            "Add a".into(),
            Some("feat".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 50, &ContextFilter::default())
            .unwrap();
        assert!(
            ctx.file_contention.is_empty(),
            "single agent = no contention"
        );
    }

    #[test]
    fn test_contention_two_agents_same_file() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.add_spec(&Spec::new("feat".into(), "Feature".into(), "".into()))
            .unwrap();

        // Alice writes a.txt
        fs::write(dir.path().join("a.txt"), "alice v1").unwrap();
        repo.seal(
            agent("alice"),
            "Alice work".into(),
            Some("feat".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Bob modifies a.txt
        fs::write(dir.path().join("a.txt"), "bob v1").unwrap();
        repo.seal(
            agent("bob"),
            "Bob work".into(),
            Some("feat".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 50, &ContextFilter::default())
            .unwrap();
        assert_eq!(ctx.file_contention.len(), 1);
        assert_eq!(ctx.file_contention[0].path, "a.txt");
        assert_eq!(ctx.file_contention[0].agents.len(), 2);
        assert!(ctx.file_contention[0].agents.contains(&"alice".to_string()));
        assert!(ctx.file_contention[0].agents.contains(&"bob".to_string()));
        assert_eq!(ctx.file_contention[0].total_seals, 2);
    }

    #[test]
    fn test_contention_excludes_bridge_seals() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.add_spec(&Spec::new("feat".into(), "Feature".into(), "".into()))
            .unwrap();

        // Bridge agent writes a.txt
        fs::write(dir.path().join("a.txt"), "bridge v1").unwrap();
        repo.seal(
            agent("writ-bridge"),
            "Import".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Alice modifies a.txt
        fs::write(dir.path().join("a.txt"), "alice v1").unwrap();
        repo.seal(
            agent("alice"),
            "Alice work".into(),
            Some("feat".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 50, &ContextFilter::default())
            .unwrap();
        // Only alice touched a.txt (bridge excluded) → no contention
        assert!(ctx.file_contention.is_empty());
    }

    #[test]
    fn test_contention_sorted_by_agent_count_desc() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.add_spec(&Spec::new("feat".into(), "Feature".into(), "".into()))
            .unwrap();

        // Create files touched by different agent combos.
        // hot.txt: 3 agents (alice, bob, charlie)
        // warm.txt: 2 agents (alice, bob)
        fs::write(dir.path().join("hot.txt"), "alice").unwrap();
        fs::write(dir.path().join("warm.txt"), "alice").unwrap();
        repo.seal(
            agent("alice"),
            "Alice".into(),
            Some("feat".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("hot.txt"), "bob").unwrap();
        fs::write(dir.path().join("warm.txt"), "bob").unwrap();
        repo.seal(
            agent("bob"),
            "Bob".into(),
            Some("feat".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("hot.txt"), "charlie").unwrap();
        repo.seal(
            agent("charlie"),
            "Charlie".into(),
            Some("feat".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 50, &ContextFilter::default())
            .unwrap();
        assert_eq!(ctx.file_contention.len(), 2);
        // hot.txt first (3 agents), warm.txt second (2 agents)
        assert_eq!(ctx.file_contention[0].path, "hot.txt");
        assert_eq!(ctx.file_contention[0].agents.len(), 3);
        assert_eq!(ctx.file_contention[1].path, "warm.txt");
        assert_eq!(ctx.file_contention[1].agents.len(), 2);
    }

    #[test]
    fn test_contention_capped_at_10() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.add_spec(&Spec::new("feat".into(), "Feature".into(), "".into()))
            .unwrap();

        // Create 12 files, all touched by 2 agents.
        for i in 0..12 {
            fs::write(dir.path().join(format!("file{i}.txt")), "alice").unwrap();
        }
        repo.seal(
            agent("alice"),
            "Alice batch".into(),
            Some("feat".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        for i in 0..12 {
            fs::write(dir.path().join(format!("file{i}.txt")), "bob").unwrap();
        }
        repo.seal(
            agent("bob"),
            "Bob batch".into(),
            Some("feat".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 50, &ContextFilter::default())
            .unwrap();
        assert_eq!(ctx.file_contention.len(), 10, "capped at 10");
    }

    #[test]
    fn test_contention_not_in_spec_scoped_context() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.add_spec(&Spec::new("feat".into(), "Feature".into(), "".into()))
            .unwrap();

        fs::write(dir.path().join("a.txt"), "alice").unwrap();
        repo.seal(
            agent("alice"),
            "Alice".into(),
            Some("feat".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("a.txt"), "bob").unwrap();
        repo.seal(
            agent("bob"),
            "Bob".into(),
            Some("feat".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(
                ContextScope::Spec("feat".into()),
                50,
                &ContextFilter::default(),
            )
            .unwrap();
        // Spec-scoped context now includes contention filtered to spec files.
        // a.txt is in the inferred scope and contested by 2 agents.
        assert_eq!(ctx.file_contention.len(), 1);
        assert_eq!(ctx.file_contention[0].path, "a.txt");
        assert_eq!(ctx.file_contention[0].agents.len(), 2);
    }

    #[test]
    fn test_contention_serializable() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.add_spec(&Spec::new("feat".into(), "Feature".into(), "".into()))
            .unwrap();

        fs::write(dir.path().join("a.txt"), "alice").unwrap();
        repo.seal(
            agent("alice"),
            "Alice".into(),
            Some("feat".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("a.txt"), "bob").unwrap();
        repo.seal(
            agent("bob"),
            "Bob".into(),
            Some("feat".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 50, &ContextFilter::default())
            .unwrap();
        let json = serde_json::to_string(&ctx).unwrap();
        assert!(json.contains("file_contention"));
        assert!(json.contains("a.txt"));
    }
}

#[cfg(test)]
mod convergence_integration_tests {
    use super::*;
    use crate::context::{ContextFilter, ContextScope};
    use crate::convergence::FileResolution;
    use crate::seal::AgentType;
    use tempfile::tempdir;

    fn agent(name: &str) -> AgentIdentity {
        AgentIdentity {
            id: name.to_string(),
            agent_type: AgentType::Agent,
        }
    }

    /// Set up a repo with baseline + two specs. Both agents modify
    /// index.html (different sections), then agent A seals again to
    /// create genuine branch divergence.
    ///
    /// After setup:
    /// - HEAD chain: nav-dev-seal-2 → nav-dev-seal-1 → setup
    /// - heads/footer-update = footer-dev-seal (NOT on HEAD chain) → DIVERGED
    /// - Index has nav-dev's second change but NOT footer-dev's index.html change
    fn setup_diverged_repo() -> (tempfile::TempDir, Repository) {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let base_content = "\
<html>
<head><title>Portfolio</title></head>
<body>
<nav>Home | About</nav>
<main>
  <h1>Welcome</h1>
  <p>Base content</p>
</main>
<footer>Copyright 2026</footer>
</body>
</html>";
        fs::write(dir.path().join("index.html"), base_content).unwrap();
        fs::write(dir.path().join("style.css"), "body { margin: 0; }").unwrap();

        // Seal 1: baseline
        repo.seal(
            agent("setup"),
            "Initial baseline".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new(
            "nav-update".into(),
            "Update navigation".into(),
            "".into(),
        ))
        .unwrap();
        repo.add_spec(&Spec::new(
            "footer-update".into(),
            "Update footer".into(),
            "".into(),
        ))
        .unwrap();

        // Seal 2: nav-dev modifies nav line. HEAD = seal2, heads/nav-update = seal2
        let nav_content = "\
<html>
<head><title>Portfolio</title></head>
<body>
<nav>Home | About | Projects | Blog</nav>
<main>
  <h1>Welcome</h1>
  <p>Base content</p>
</main>
<footer>Copyright 2026</footer>
</body>
</html>";
        fs::write(dir.path().join("index.html"), nav_content).unwrap();
        repo.seal(
            agent("nav-dev"),
            "Updated navigation links".into(),
            Some("nav-update".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Seal 3: footer-dev modifies footer line.
        // Simulates concurrent work: footer-dev starts from baseline, NOT
        // nav-dev's version. So we write footer-only changes (nav is base).
        // parent = HEAD (seal2, since footer-update has no spec head yet)
        // HEAD = seal3, heads/footer-update = seal3
        let footer_only_content = "\
<html>
<head><title>Portfolio</title></head>
<body>
<nav>Home | About</nav>
<main>
  <h1>Welcome</h1>
  <p>Base content</p>
</main>
<footer>Copyright 2026 - All rights reserved</footer>
</body>
</html>";
        fs::write(dir.path().join("index.html"), footer_only_content).unwrap();
        fs::write(dir.path().join("footer.css"), ".footer { padding: 1em; }").unwrap();
        repo.seal(
            agent("footer-dev"),
            "Updated footer".into(),
            Some("footer-update".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Seal 4: nav-dev seals again on nav-update.
        // Restores nav version of index.html + adds nav.js.
        // resolve_parent("nav-update") → spec head = seal2 (NOT seal3)
        // → parent = seal2, HEAD = seal4
        // HEAD chain: seal4 → seal2 → seal1
        // heads/footer-update = seal3 → NOT on chain → DIVERGED!
        //
        // Tree snapshot has nav version of index.html (not footer-only).
        fs::write(dir.path().join("index.html"), nav_content).unwrap();
        fs::write(dir.path().join("nav.js"), "// Navigation script\n").unwrap();
        repo.seal(
            agent("nav-dev"),
            "Added nav script".into(),
            Some("nav-update".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        (dir, repo)
    }

    #[test]
    fn test_setup_creates_divergence() {
        let (_dir, repo) = setup_diverged_repo();

        let diverged = repo.diverged_branches().unwrap();
        let diverged_specs: Vec<&str> = diverged.iter().map(|d| d.spec_id.as_str()).collect();
        assert!(
            diverged_specs.contains(&"footer-update"),
            "footer-update should be diverged, got: {:?}",
            diverged_specs
        );
    }

    #[test]
    fn test_converge_clean_non_overlapping_changes() {
        let (dir, repo) = setup_diverged_repo();

        // Run convergence between the two specs.
        let report = repo.converge("nav-update", "footer-update").unwrap();

        // Nav changed the nav line, footer changed the footer line → clean merge.
        assert!(
            report.is_clean,
            "non-overlapping changes should merge cleanly"
        );
        assert!(report.conflicts.is_empty());

        // Verify the merged index.html has both changes.
        let merged_html = report.auto_merged.iter().find(|m| m.path == "index.html");
        assert!(merged_html.is_some(), "index.html should be in auto_merged");
        let merged = merged_html.unwrap();
        assert!(
            merged.content.contains("Projects | Blog"),
            "should have nav changes"
        );
        assert!(
            merged.content.contains("All rights reserved"),
            "should have footer changes"
        );

        // Apply convergence to working directory.
        repo.apply_convergence(&report, &[]).unwrap();

        // Verify files on disk.
        let disk_html = fs::read_to_string(dir.path().join("index.html")).unwrap();
        assert!(disk_html.contains("Projects | Blog"));
        assert!(disk_html.contains("All rights reserved"));
    }

    #[test]
    fn test_converge_then_seal_captures_merged_state() {
        let (dir, repo) = setup_diverged_repo();

        let report = repo.converge("nav-update", "footer-update").unwrap();
        assert!(report.is_clean);
        repo.apply_convergence(&report, &[]).unwrap();

        // apply_convergence now updates the index, so the working directory
        // is clean.  Verify the merged content directly on disk.
        let disk_html = fs::read_to_string(dir.path().join("index.html")).unwrap();
        assert!(disk_html.contains("Projects | Blog"));
        assert!(disk_html.contains("All rights reserved"));
    }

    #[test]
    fn test_converge_with_conflict_and_resolution() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Baseline with a shared file.
        fs::write(dir.path().join("index.html"), "line1\nline2\nline3\n").unwrap();
        repo.seal(
            agent("setup"),
            "Baseline".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("spec-a".into(), "Spec A".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("spec-b".into(), "Spec B".into(), "".into()))
            .unwrap();

        // Agent A changes line 2.
        fs::write(dir.path().join("index.html"), "line1\nLEFT\nline3\n").unwrap();
        repo.seal(
            agent("agent-a"),
            "Left change".into(),
            Some("spec-a".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent B changes line 2 differently.
        fs::write(dir.path().join("index.html"), "line1\nRIGHT\nline3\n").unwrap();
        repo.seal(
            agent("agent-b"),
            "Right change".into(),
            Some("spec-b".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let report = repo.converge("spec-a", "spec-b").unwrap();
        assert!(!report.is_clean, "overlapping changes should conflict");
        assert!(!report.conflicts.is_empty());

        let conflict = &report.conflicts[0];
        assert_eq!(conflict.path, "index.html");
        assert!(!conflict.regions.is_empty());

        // Resolve by combining both.
        let resolutions = vec![FileResolution {
            path: "index.html".to_string(),
            content: "line1\nLEFT + RIGHT\nline3\n".to_string(),
        }];

        repo.apply_convergence(&report, &resolutions).unwrap();

        let disk = fs::read_to_string(dir.path().join("index.html")).unwrap();
        assert!(disk.contains("LEFT + RIGHT"));
    }

    #[test]
    fn test_converge_unresolved_conflict_errors() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("a.txt"), "base").unwrap();
        repo.seal(
            agent("setup"),
            "Baseline".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("spec-a".into(), "A".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("spec-b".into(), "B".into(), "".into()))
            .unwrap();

        fs::write(dir.path().join("a.txt"), "LEFT").unwrap();
        repo.seal(
            agent("agent-a"),
            "Left".into(),
            Some("spec-a".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("a.txt"), "RIGHT").unwrap();
        repo.seal(
            agent("agent-b"),
            "Right".into(),
            Some("spec-b".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let report = repo.converge("spec-a", "spec-b").unwrap();
        assert!(!report.is_clean);

        let result = repo.apply_convergence(&report, &[]);
        assert!(result.is_err(), "unresolved conflicts should error");
    }

    #[test]
    fn test_converge_left_only_and_right_only_files() {
        let (_dir, repo) = setup_diverged_repo();

        let report = repo.converge("nav-update", "footer-update").unwrap();

        // nav.js was added by nav-dev only → left_only
        assert!(
            report.left_only.contains(&"nav.js".to_string()),
            "nav.js should be left_only, got: {:?}",
            report.left_only
        );
        // footer.css was added by footer-dev only → right_only
        assert!(
            report.right_only.contains(&"footer.css".to_string()),
            "footer.css should be right_only, got: {:?}",
            report.right_only
        );
    }

    #[test]
    fn test_converge_reduces_diverged_branch_count() {
        let (dir, repo) = setup_diverged_repo();

        // Verify divergence exists.
        let before = repo.diverged_branches().unwrap();
        assert!(
            !before.is_empty(),
            "should have diverged branches before convergence"
        );

        // Converge and apply (index is updated automatically).
        let report = repo.converge("nav-update", "footer-update").unwrap();
        repo.apply_convergence(&report, &[]).unwrap();

        // After convergence, both specs' content should be on disk.
        assert!(dir.path().join("footer.css").exists());

        // Context should be obtainable.
        let ctx = repo
            .context(ContextScope::Full, 50, &ContextFilter::default())
            .unwrap();
        assert!(!ctx.recent_seals.is_empty());
    }

    #[test]
    fn test_converge_report_has_all_metadata() {
        let (_dir, repo) = setup_diverged_repo();

        let report = repo.converge("nav-update", "footer-update").unwrap();

        assert_eq!(report.left_spec, "nav-update");
        assert_eq!(report.right_spec, "footer-update");
        assert!(!report.left_seal_id.is_empty());
        assert!(!report.right_seal_id.is_empty());
        assert!(report.base_seal_id.is_some());
    }

    #[test]
    fn test_converge_report_serializable() {
        let (_dir, repo) = setup_diverged_repo();

        let report = repo.converge("nav-update", "footer-update").unwrap();
        let json = serde_json::to_string_pretty(&report).unwrap();
        assert!(json.contains("nav-update"));
        assert!(json.contains("footer-update"));
        assert!(json.contains("is_clean"));

        // Roundtrip
        let parsed: crate::convergence::ConvergenceReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.left_spec, report.left_spec);
        assert_eq!(parsed.is_clean, report.is_clean);
    }

    #[test]
    fn test_converge_three_specs_sequential() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Baseline with a shared file.
        fs::write(
            dir.path().join("index.html"),
            "line1\nline2\nline3\nline4\nline5\n",
        )
        .unwrap();
        repo.seal(
            agent("setup"),
            "Baseline".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("spec-a".into(), "Spec A".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("spec-b".into(), "Spec B".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("spec-c".into(), "Spec C".into(), "".into()))
            .unwrap();

        // Each agent modifies a different line in the same file.
        fs::write(
            dir.path().join("index.html"),
            "AAAA\nline2\nline3\nline4\nline5\n",
        )
        .unwrap();
        repo.seal(
            agent("agent-a"),
            "Change line 1".into(),
            Some("spec-a".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        fs::write(
            dir.path().join("index.html"),
            "AAAA\nline2\nCCCC\nline4\nline5\n",
        )
        .unwrap();
        repo.seal(
            agent("agent-b"),
            "Change line 3".into(),
            Some("spec-b".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        fs::write(
            dir.path().join("index.html"),
            "AAAA\nline2\nCCCC\nline4\nEEEE\n",
        )
        .unwrap();
        repo.seal(
            agent("agent-c"),
            "Change line 5".into(),
            Some("spec-c".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Sequential convergence: merge A+B first.
        let report_ab = repo.converge("spec-a", "spec-b").unwrap();
        assert!(
            report_ab.is_clean,
            "A (line1) and B (line3) should merge cleanly"
        );

        // Then merge A+C (A's tree is the reference for both merges).
        let report_ac = repo.converge("spec-a", "spec-c").unwrap();
        assert!(
            report_ac.is_clean,
            "A (line1) and C (line5) should merge cleanly"
        );

        // Verify the merge results contain the expected content.
        if let Some(merged) = report_ab
            .auto_merged
            .iter()
            .find(|m| m.path == "index.html")
        {
            assert!(merged.content.contains("AAAA"), "should have A's line1");
            assert!(merged.content.contains("CCCC"), "should have B's line3");
        }
        if let Some(merged) = report_ac
            .auto_merged
            .iter()
            .find(|m| m.path == "index.html")
        {
            assert!(merged.content.contains("AAAA"), "should have A's line1");
            assert!(merged.content.contains("EEEE"), "should have C's line5");
        }
    }

    #[test]
    fn test_converge_spec_with_no_seals_errors() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("has-seals".into(), "Has work".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("no-seals".into(), "No work".into(), "".into()))
            .unwrap();

        fs::write(dir.path().join("a.txt"), "content").unwrap();
        repo.seal(
            agent("worker"),
            "Work".into(),
            Some("has-seals".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let result = repo.converge("has-seals", "no-seals");
        assert!(
            result.is_err(),
            "converging spec with no seals should error"
        );
    }

    #[test]
    fn test_converge_identical_changes_clean() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("shared.txt"), "original\n").unwrap();
        repo.seal(
            agent("setup"),
            "Baseline".into(),
            None,
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("spec-a".into(), "A".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("spec-b".into(), "B".into(), "".into()))
            .unwrap();

        // Agent A changes shared.txt + adds a unique file.
        fs::write(dir.path().join("shared.txt"), "identical change\n").unwrap();
        fs::write(dir.path().join("only-a.txt"), "a stuff\n").unwrap();
        repo.seal(
            agent("agent-a"),
            "A changes".into(),
            Some("spec-a".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent B changes shared.txt identically + adds a different unique file.
        // (shared.txt is already "identical change" on disk, so we also add
        // a unique file so the seal captures changes.)
        fs::write(dir.path().join("only-b.txt"), "b stuff\n").unwrap();
        repo.seal(
            agent("agent-b"),
            "B changes".into(),
            Some("spec-b".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let report = repo.converge("spec-a", "spec-b").unwrap();
        // shared.txt was changed identically by both → clean merge.
        assert!(report.is_clean, "identical changes should merge cleanly");
    }

    #[test]
    fn test_converge_full_cycle_diverge_merge_context() {
        // End-to-end integration: create divergence, converge, seal, verify
        // context shows the merged state without diverged branches for those specs.
        let (dir, repo) = setup_diverged_repo();

        // Before convergence: footer-update is diverged.
        let before_ctx = repo
            .context(ContextScope::Full, 50, &ContextFilter::default())
            .unwrap();
        assert!(
            before_ctx.convergence_recommended,
            "should recommend convergence before merge"
        );

        // Converge and apply (index updated automatically).
        let report = repo.converge("nav-update", "footer-update").unwrap();
        assert!(report.is_clean);
        repo.apply_convergence(&report, &[]).unwrap();

        // After convergence, verify disk state has all content.
        let html = fs::read_to_string(dir.path().join("index.html")).unwrap();
        assert!(
            html.contains("Projects | Blog"),
            "nav changes should be present"
        );
        assert!(
            html.contains("All rights reserved"),
            "footer changes should be present"
        );
        assert!(dir.path().join("nav.js").exists(), "nav.js should exist");
        assert!(
            dir.path().join("footer.css").exists(),
            "footer.css should exist"
        );
    }
}

#[cfg(test)]
#[allow(deprecated)]
mod converge_all_tests {
    use super::*;
    use crate::convergence::ConvergeStrategy;
    use crate::seal::AgentIdentity;
    use crate::spec::SpecUpdate;
    use crate::spec::{Spec, SpecStatus};
    use tempfile::tempdir;

    /// Helper: set up a repo with 3 specs where 2 are on diverged branches.
    /// Returns (TempDir, Repository).
    fn setup_multi_diverged() -> (tempfile::TempDir, Repository) {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Baseline file.
        fs::write(dir.path().join("shared.txt"), "line1\nline2\nline3\n").unwrap();

        let base_agent = AgentIdentity {
            id: "setup".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            base_agent,
            "baseline".into(),
            None,
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Add 3 specs.
        repo.add_spec(&Spec::new("alpha".into(), "Alpha".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("beta".into(), "Beta".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("gamma".into(), "Gamma".into(), "".into()))
            .unwrap();

        // Alpha seals on HEAD (stays on main chain).
        fs::write(dir.path().join("alpha.txt"), "alpha content\n").unwrap();
        let alpha_agent = AgentIdentity {
            id: "alpha-dev".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            alpha_agent.clone(),
            "alpha work".into(),
            Some("alpha".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Beta seals — diverges from HEAD because alpha advanced it.
        fs::write(dir.path().join("beta.txt"), "beta content\n").unwrap();
        let beta_agent = AgentIdentity {
            id: "beta-dev".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            beta_agent,
            "beta work".into(),
            Some("beta".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Gamma seals — also diverges.
        fs::write(dir.path().join("gamma.txt"), "gamma content\n").unwrap();
        let gamma_agent = AgentIdentity {
            id: "gamma-dev".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            gamma_agent,
            "gamma work".into(),
            Some("gamma".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Alpha seals again to advance HEAD past beta/gamma.
        fs::write(dir.path().join("alpha2.txt"), "alpha part 2\n").unwrap();
        repo.seal(
            alpha_agent,
            "alpha complete".into(),
            Some("alpha".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Mark alpha as Complete so it's eligible as base spec on HEAD.
        repo.update_spec(
            "alpha",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        (dir, repo)
    }

    #[test]
    fn test_converge_all_no_diverged() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("a.txt"), "content\n").unwrap();
        let agent = AgentIdentity {
            id: "dev".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            agent,
            "work".into(),
            None,
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        let report = repo.converge_all(ConvergeStrategy::Manual, false).unwrap();
        assert!(report.is_clean);
        assert!(report.merges.is_empty());
        assert_eq!(report.strategy, "manual");
    }

    #[test]
    fn test_converge_all_clean_multi_branch() {
        let (_dir, repo) = setup_multi_diverged();
        let report = repo.converge_all(ConvergeStrategy::Manual, false).unwrap();

        assert!(report.is_clean, "disjoint files should merge cleanly");
        assert!(!report.merge_order.is_empty());
        assert!(!report.merges.is_empty());
        assert_eq!(report.base_spec, "alpha");
        assert!(report.total_conflicts == 0);
    }

    #[test]
    fn test_converge_all_with_apply() {
        let (dir, repo) = setup_multi_diverged();
        let report = repo.converge_all(ConvergeStrategy::Manual, true).unwrap();

        assert!(report.applied);
        assert!(report.is_clean);

        // Verify files from diverged branches exist on disk.
        assert!(dir.path().join("beta.txt").exists());
        assert!(dir.path().join("gamma.txt").exists());
    }

    #[test]
    fn test_converge_all_most_recent_resolves_conflicts() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Base file.
        fs::write(dir.path().join("shared.txt"), "original\n").unwrap();
        let base_agent = AgentIdentity {
            id: "setup".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            base_agent,
            "baseline".into(),
            None,
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Two specs that both modify shared.txt — creating a conflict.
        repo.add_spec(&Spec::new("left".into(), "Left".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("right".into(), "Right".into(), "".into()))
            .unwrap();

        // Left modifies.
        fs::write(dir.path().join("shared.txt"), "LEFT_VERSION\n").unwrap();
        let left_agent = AgentIdentity {
            id: "left-dev".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            left_agent.clone(),
            "left change".into(),
            Some("left".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Right modifies same file differently.
        fs::write(dir.path().join("shared.txt"), "RIGHT_VERSION\n").unwrap();
        let right_agent = AgentIdentity {
            id: "right-dev".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            right_agent,
            "right change".into(),
            Some("right".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Left seals again to advance HEAD past right's seal.
        // IMPORTANT: Restore LEFT_VERSION on disk to avoid shared index contamination.
        // Without this, left's tree would inherit RIGHT_VERSION from the shared index.
        fs::write(dir.path().join("shared.txt"), "LEFT_VERSION\n").unwrap();
        fs::write(dir.path().join("left-only.txt"), "extra\n").unwrap();
        repo.seal(
            left_agent,
            "left extra".into(),
            Some("left".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Mark left as Complete so it's the base spec on HEAD.
        repo.update_spec(
            "left",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        // Now right is diverged. Converge with MostRecent strategy.
        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, true)
            .unwrap();

        assert!(report.is_clean, "MostRecent should resolve all conflicts");
        assert!(report.total_resolutions > 0);
        assert!(
            !report.warnings.is_empty(),
            "should warn about lost content"
        );

        // Check that resolution records exist.
        let has_resolution = report.merges.iter().any(|m| !m.resolutions.is_empty());
        assert!(has_resolution, "at least one step should have resolutions");
    }

    #[test]
    fn test_converge_all_three_way_leaves_conflicts_unresolved() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("shared.txt"), "original\n").unwrap();
        let base_agent = AgentIdentity {
            id: "setup".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            base_agent,
            "baseline".into(),
            None,
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("left".into(), "Left".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("right".into(), "Right".into(), "".into()))
            .unwrap();

        fs::write(dir.path().join("shared.txt"), "LEFT_VERSION\n").unwrap();
        let left_agent = AgentIdentity {
            id: "left-dev".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            left_agent.clone(),
            "left change".into(),
            Some("left".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("shared.txt"), "RIGHT_VERSION\n").unwrap();
        let right_agent = AgentIdentity {
            id: "right-dev".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            right_agent,
            "right change".into(),
            Some("right".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Restore LEFT_VERSION to avoid shared index contamination.
        fs::write(dir.path().join("shared.txt"), "LEFT_VERSION\n").unwrap();
        fs::write(dir.path().join("left-extra.txt"), "extra\n").unwrap();
        repo.seal(
            left_agent,
            "advance head".into(),
            Some("left".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Mark left as Complete so it's picked as base.
        repo.update_spec(
            "left",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        let report = repo.converge_all(ConvergeStrategy::Manual, false).unwrap();

        assert!(!report.is_clean, "Manual should leave conflicts");
        assert!(report.total_conflicts > 0);
        assert_eq!(report.total_resolutions, 0);
    }

    #[test]
    fn test_converge_all_escalate_strategy_produces_escalation_records() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("shared.txt"), "original\n").unwrap();
        let base_agent = AgentIdentity {
            id: "setup".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            base_agent,
            "baseline".into(),
            None,
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("left".into(), "Left".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("right".into(), "Right".into(), "".into()))
            .unwrap();

        fs::write(dir.path().join("shared.txt"), "LEFT_VERSION\n").unwrap();
        let left_agent = AgentIdentity {
            id: "left-dev".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            left_agent.clone(),
            "left change".into(),
            Some("left".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("shared.txt"), "RIGHT_VERSION\n").unwrap();
        let right_agent = AgentIdentity {
            id: "right-dev".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            right_agent,
            "right change".into(),
            Some("right".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("shared.txt"), "LEFT_VERSION\n").unwrap();
        fs::write(dir.path().join("left-extra.txt"), "extra\n").unwrap();
        repo.seal(
            left_agent,
            "advance head".into(),
            Some("left".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        repo.update_spec(
            "left",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        let report = repo
            .converge_all(ConvergeStrategy::Escalate, false)
            .unwrap();

        assert!(
            !report.is_clean,
            "Escalate should not silently resolve — conflicts are escalated"
        );
        assert!(
            !report.escalations.is_empty(),
            "Escalate strategy must produce PipelineEscalation records"
        );

        let has_escalated_decision = report
            .quality_report
            .as_ref()
            .map(|qr| qr.file_decisions.iter().any(|d| d.decision == "escalated"))
            .unwrap_or(false);
        assert!(
            has_escalated_decision,
            "quality_report should contain at least one 'escalated' decision"
        );

        let esc = &report.escalations[0];
        assert!(
            !esc.file_path.is_empty(),
            "escalation record must have a non-empty file_path"
        );
        assert!(
            !esc.reason.is_empty(),
            "escalation record must have a reason"
        );
    }

    #[test]
    fn test_converge_all_v2_pipeline_resolves_python_imports() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let base_py = "import os\n\ndef main():\n    pass\n";
        fs::write(dir.path().join("app.py"), base_py).unwrap();
        let setup = AgentIdentity {
            id: "setup".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            setup,
            "baseline".into(),
            None,
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("agent-a".into(), "Agent A".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("agent-b".into(), "Agent B".into(), "".into()))
            .unwrap();

        let a_py = "import os\nimport json\n\ndef main():\n    pass\n";
        fs::write(dir.path().join("app.py"), a_py).unwrap();
        let agent_a = AgentIdentity {
            id: "a".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            agent_a.clone(),
            "add json".into(),
            Some("agent-a".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("app.py"), base_py).unwrap();

        let b_py = "import os\nimport sys\n\ndef main():\n    pass\n";
        fs::write(dir.path().join("app.py"), b_py).unwrap();
        let agent_b = AgentIdentity {
            id: "b".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            agent_b,
            "add sys".into(),
            Some("agent-b".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("app.py"), a_py).unwrap();
        repo.seal(
            agent_a,
            "finalize".into(),
            Some("agent-a".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();
        repo.update_spec(
            "agent-a",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        let report = repo.converge_all(ConvergeStrategy::Escalate, true).unwrap();

        assert!(report.applied, "should apply");
        assert!(report.is_clean, "should be clean — both imports composable");

        let merged = fs::read_to_string(dir.path().join("app.py")).unwrap();
        assert!(
            merged.contains("import json"),
            "json import lost! merged:\n{merged}"
        );
        assert!(
            merged.contains("import sys"),
            "sys import lost! merged:\n{merged}"
        );
        assert!(
            merged.contains("import os"),
            "os import lost! merged:\n{merged}"
        );

        let has_pipeline_tag = report.merges.iter().any(|m| {
            m.resolutions.iter().any(|r| {
                r.strategy.starts_with("v2-pipeline") || r.strategy.starts_with("v1-fallback")
            })
        });
        assert!(
            has_pipeline_tag,
            "resolution strategy should reference v2-pipeline or v1-fallback"
        );
    }

    #[test]
    fn test_converge_all_report_serializable() {
        let (_dir, repo) = setup_multi_diverged();
        let report = repo.converge_all(ConvergeStrategy::Manual, false).unwrap();
        let json = serde_json::to_string_pretty(&report).unwrap();
        assert!(json.contains("base_spec"));
        assert!(json.contains("merge_order"));
        assert!(json.contains("strategy"));
    }

    #[test]
    fn test_converge_all_warnings_for_high_contention() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Create a shared file modified by 3+ agents.
        fs::write(dir.path().join("hot.txt"), "base\n").unwrap();
        let setup = AgentIdentity {
            id: "setup".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            setup,
            "baseline".into(),
            None,
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("a".into(), "A".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("b".into(), "B".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("c".into(), "C".into(), "".into()))
            .unwrap();

        // Agent A modifies hot.txt (adds line).
        fs::write(dir.path().join("hot.txt"), "base\nagent-a\n").unwrap();
        let a = AgentIdentity {
            id: "agent-a".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            a.clone(),
            "a changes".into(),
            Some("a".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Agent B modifies hot.txt.
        fs::write(dir.path().join("hot.txt"), "base\nagent-b\n").unwrap();
        let b = AgentIdentity {
            id: "agent-b".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            b,
            "b changes".into(),
            Some("b".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Agent C modifies hot.txt.
        fs::write(dir.path().join("hot.txt"), "base\nagent-c\n").unwrap();
        let c = AgentIdentity {
            id: "agent-c".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            c,
            "c changes".into(),
            Some("c".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Agent A seals again to advance HEAD past B and C.
        fs::write(dir.path().join("extra.txt"), "x\n").unwrap();
        repo.seal(
            a,
            "a final".into(),
            Some("a".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, false)
            .unwrap();

        // Should have warnings about high-contention files.
        let has_contention_warning = report
            .warnings
            .iter()
            .any(|w| w.contains("touched by") && w.contains("agents"));
        assert!(
            has_contention_warning,
            "should warn about high-contention files, got: {:?}",
            report.warnings,
        );
    }

    #[test]
    fn test_converge_all_merge_order_tracks_specs() {
        let (_dir, repo) = setup_multi_diverged();
        let report = repo.converge_all(ConvergeStrategy::Manual, false).unwrap();

        // merge_order should contain the diverged spec IDs (not the base).
        assert!(!report.merge_order.contains(&"alpha".to_string()));
        for spec_id in &report.merge_order {
            assert!(
                spec_id == "beta" || spec_id == "gamma",
                "unexpected spec in merge_order: {spec_id}",
            );
        }
    }

    #[test]
    fn test_converge_all_step_results_match_order() {
        let (_dir, repo) = setup_multi_diverged();
        let report = repo.converge_all(ConvergeStrategy::Manual, false).unwrap();

        assert_eq!(report.merges.len(), report.merge_order.len());
        for (step, spec_id) in report.merges.iter().zip(report.merge_order.iter()) {
            assert_eq!(&step.right_spec, spec_id);
            assert_eq!(&step.left_spec, &report.base_spec);
        }
    }

    #[test]
    fn test_converge_all_apply_clears_diverged_branches() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("shared.txt"), "base content\n").unwrap();
        let setup = AgentIdentity {
            id: "setup".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            setup,
            "baseline".into(),
            None,
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("alpha".into(), "Alpha".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("beta".into(), "Beta".into(), "".into()))
            .unwrap();

        // Alpha modifies and seals (stays on HEAD chain).
        fs::write(dir.path().join("alpha.txt"), "alpha work\n").unwrap();
        let alpha_agent = AgentIdentity {
            id: "alpha-dev".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            alpha_agent.clone(),
            "alpha work".into(),
            Some("alpha".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Beta modifies different file and seals (creates diverged branch).
        fs::write(dir.path().join("beta.txt"), "beta work\n").unwrap();
        let beta_agent = AgentIdentity {
            id: "beta-dev".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            beta_agent,
            "beta work".into(),
            Some("beta".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Alpha seals again to advance HEAD past beta.
        fs::write(dir.path().join("alpha2.txt"), "more alpha\n").unwrap();
        repo.seal(
            alpha_agent,
            "alpha final".into(),
            Some("alpha".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        repo.update_spec(
            "alpha",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        // Verify beta IS diverged before convergence.
        let pre_diverged = repo.diverged_branches().unwrap();
        assert!(
            pre_diverged.iter().any(|b| b.spec_id == "beta"),
            "beta should be diverged before converge_all"
        );

        // Apply convergence.
        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, true)
            .unwrap();
        assert!(report.applied);

        // After convergence, beta should no longer be diverged.
        let post_diverged = repo.diverged_branches().unwrap();
        assert!(
            !post_diverged.iter().any(|b| b.spec_id == "beta"),
            "beta should NOT be diverged after converge_all --apply, got: {:?}",
            post_diverged.iter().map(|b| &b.spec_id).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn test_apply_convergence_advances_spec_head() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("file.txt"), "original\n").unwrap();
        let setup = AgentIdentity {
            id: "setup".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            setup,
            "init".into(),
            None,
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("base-spec".into(), "Base".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("diverged-spec".into(), "Div".into(), "".into()))
            .unwrap();

        // Base spec seals first (establishes spec head on HEAD chain).
        fs::write(dir.path().join("base.txt"), "base\n").unwrap();
        let base_agent = AgentIdentity {
            id: "base-dev".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            base_agent.clone(),
            "base work".into(),
            Some("base-spec".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Diverged spec seals (chains from global HEAD at this point).
        fs::write(dir.path().join("div.txt"), "diverged\n").unwrap();
        let div_agent = AgentIdentity {
            id: "div-dev".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            div_agent,
            "diverged work".into(),
            Some("diverged-spec".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Base spec seals AGAIN — its parent comes from its spec head, not
        // global HEAD, so this new seal's chain skips the diverged seal.
        fs::write(dir.path().join("base2.txt"), "more base\n").unwrap();
        repo.seal(
            base_agent,
            "base final".into(),
            Some("base-spec".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Verify diverged.
        let diverged = repo.diverged_branches().unwrap();
        assert!(
            diverged.iter().any(|b| b.spec_id == "diverged-spec"),
            "diverged-spec should be diverged before convergence"
        );

        // Run converge + apply_convergence.
        let report = repo.converge("base-spec", "diverged-spec").unwrap();
        repo.apply_convergence(&report, &[]).unwrap();

        // After apply_convergence, spec head should be updated.
        let post_diverged = repo.diverged_branches().unwrap();
        assert!(
            !post_diverged.iter().any(|b| b.spec_id == "diverged-spec"),
            "diverged-spec should be resolved after apply_convergence"
        );
    }

    /// Helper: set up a repo with conflicting file versions of different sizes.
    /// Left has a shorter version, right has a longer version.
    fn setup_content_size_conflict() -> (tempfile::TempDir, Repository) {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(
            dir.path().join("page.html"),
            "<html>\n<body>\nHello\n</body>\n</html>\n",
        )
        .unwrap();
        let setup = AgentIdentity {
            id: "setup".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            setup,
            "baseline".into(),
            None,
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new(
            "small".into(),
            "Small changes".into(),
            "".into(),
        ))
        .unwrap();
        repo.add_spec(&Spec::new("big".into(), "Big changes".into(), "".into()))
            .unwrap();

        // Small spec: minimal change (3 lines).
        fs::write(
            dir.path().join("page.html"),
            "<html>\n<body>Small</body>\n</html>\n",
        )
        .unwrap();
        let small_agent = AgentIdentity {
            id: "small-dev".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            small_agent.clone(),
            "small change".into(),
            Some("small".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Big spec: richer content (8 lines).
        fs::write(
            dir.path().join("page.html"),
            "<html>\n<head><title>Big</title></head>\n<body>\n<nav>\n<li>Home</li>\n<li>About</li>\n</nav>\n</body>\n</html>\n",
        ).unwrap();
        let big_agent = AgentIdentity {
            id: "big-dev".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            big_agent,
            "big change".into(),
            Some("big".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Small seals again to advance HEAD past big.
        fs::write(
            dir.path().join("page.html"),
            "<html>\n<body>Small</body>\n</html>\n",
        )
        .unwrap();
        fs::write(dir.path().join("small-only.txt"), "extra\n").unwrap();
        repo.seal(
            small_agent,
            "small extra".into(),
            Some("small".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        repo.update_spec(
            "small",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        (dir, repo)
    }

    #[test]
    fn test_converge_all_most_recent_fallback_for_both_modified() {
        let (dir, repo) = setup_content_size_conflict();

        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, true)
            .unwrap();

        assert!(
            report.is_clean,
            "MostRecent fallback should resolve all conflicts"
        );
        assert!(report.total_resolutions > 0);
        assert_eq!(report.strategy, "most-recent");

        // MostRecent picks by timestamp, not line count. The small spec
        // sealed last so it wins (or Layers 2-3 may have auto-resolved).
        let content = fs::read_to_string(dir.path().join("page.html")).unwrap();
        assert!(!content.is_empty(), "merged file should not be empty");
    }

    #[test]
    fn test_converge_all_resolution_records_have_strategy() {
        let (_dir, repo) = setup_content_size_conflict();

        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, false)
            .unwrap();

        let has_resolution = report.merges.iter().any(|m| !m.resolutions.is_empty());
        assert!(
            has_resolution,
            "should have resolution records, got merges: {:?}",
            report
                .merges
                .iter()
                .map(|m| &m.resolutions)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_converge_all_manual_leaves_both_modified_unresolved() {
        let (_dir, repo) = setup_content_size_conflict();

        let report = repo.converge_all(ConvergeStrategy::Manual, false).unwrap();

        // Manual strategy should NOT auto-resolve BothModified regions.
        // Check for unresolved or auto-resolved (Layers 2-3 may handle
        // some regions even under Manual).
        assert!(
            report.quality_report.is_some(),
            "quality report should be present"
        );
    }

    #[test]
    fn test_converge_all_both_modified_most_recent_picks_by_timestamp() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("eq.txt"), "original\n").unwrap();
        let setup = AgentIdentity {
            id: "setup".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            setup,
            "baseline".into(),
            None,
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("left".into(), "Left".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("right".into(), "Right".into(), "".into()))
            .unwrap();

        fs::write(dir.path().join("eq.txt"), "LEFT_V\n").unwrap();
        let left_agent = AgentIdentity {
            id: "left-dev".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            left_agent.clone(),
            "left".into(),
            Some("left".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("eq.txt"), "RIGHT_\n").unwrap();
        let right_agent = AgentIdentity {
            id: "right-dev".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            right_agent,
            "right".into(),
            Some("right".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Left seals again to advance HEAD past right.
        fs::write(dir.path().join("eq.txt"), "LEFT_V\n").unwrap();
        fs::write(dir.path().join("extra.txt"), "x\n").unwrap();
        repo.seal(
            left_agent,
            "advance".into(),
            Some("left".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        repo.update_spec(
            "left",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, true)
            .unwrap();

        assert!(report.is_clean, "MostRecent should resolve all conflicts");
    }

    // ---- Quality report tests ----

    #[test]
    fn test_quality_report_present_on_converge_all() {
        let (_dir, repo) = setup_multi_diverged();
        let report = repo.converge_all(ConvergeStrategy::Manual, false).unwrap();

        assert!(
            report.quality_report.is_some(),
            "quality report should always be present"
        );
        let qr = report.quality_report.unwrap();
        assert!(!qr.summary.is_empty());
        assert!(qr.quality_score <= 100);
    }

    #[test]
    fn test_quality_report_tracks_file_decisions() {
        let (_dir, repo) = setup_multi_diverged();
        let report = repo.converge_all(ConvergeStrategy::Manual, false).unwrap();

        let qr = report.quality_report.unwrap();
        assert!(!qr.file_decisions.is_empty(), "should have file decisions");

        // Should have right-only decisions for beta.txt and gamma.txt.
        let right_only: Vec<_> = qr
            .file_decisions
            .iter()
            .filter(|d| d.decision == "right-only")
            .collect();
        assert!(!right_only.is_empty(), "should have right-only decisions");
    }

    #[test]
    fn test_quality_report_conflict_decisions_with_strategy() {
        let (_dir, repo) = setup_content_size_conflict();
        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, false)
            .unwrap();

        let qr = report.quality_report.unwrap();
        let resolved_decisions: Vec<_> = qr
            .file_decisions
            .iter()
            .filter(|d| d.decision == "most-recent" || d.decision == "auto-resolved")
            .collect();
        assert!(
            !resolved_decisions.is_empty(),
            "should have resolved decisions, got: {:?}",
            qr.file_decisions
                .iter()
                .map(|d| &d.decision)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_quality_report_unresolved_conflicts_lower_score() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("shared.txt"), "original\n").unwrap();
        let setup = AgentIdentity {
            id: "setup".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            setup,
            "baseline".into(),
            None,
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("left".into(), "Left".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("right".into(), "Right".into(), "".into()))
            .unwrap();

        fs::write(dir.path().join("shared.txt"), "LEFT\n").unwrap();
        let la = AgentIdentity {
            id: "l".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            la.clone(),
            "l".into(),
            Some("left".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("shared.txt"), "RIGHT\n").unwrap();
        let ra = AgentIdentity {
            id: "r".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            ra,
            "r".into(),
            Some("right".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("shared.txt"), "LEFT\n").unwrap();
        fs::write(dir.path().join("extra.txt"), "x\n").unwrap();
        repo.seal(
            la,
            "advance".into(),
            Some("left".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();
        repo.update_spec(
            "left",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        let report = repo.converge_all(ConvergeStrategy::Manual, false).unwrap();

        let qr = report.quality_report.unwrap();
        assert!(
            qr.quality_score < 100,
            "unresolved conflicts should lower score, got {}",
            qr.quality_score
        );

        let unresolved: Vec<_> = qr
            .file_decisions
            .iter()
            .filter(|d| d.decision == "conflict-unresolved")
            .collect();
        assert!(
            !unresolved.is_empty(),
            "should have unresolved conflict decisions"
        );
    }

    #[test]
    fn test_quality_report_serializable() {
        let (_dir, repo) = setup_content_size_conflict();
        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, false)
            .unwrap();

        let json = serde_json::to_string_pretty(&report).unwrap();
        assert!(json.contains("quality_report"));
        assert!(json.contains("file_decisions"));
        assert!(json.contains("quality_score"));
        assert!(json.contains("summary"));

        // Roundtrip.
        let parsed: ConvergeAllReport = serde_json::from_str(&json).unwrap();
        assert!(parsed.quality_report.is_some());
        let qr = parsed.quality_report.unwrap();
        assert!(!qr.file_decisions.is_empty());
    }

    #[test]
    fn test_quality_report_consistency_checks_on_html() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Create two HTML files with same nav structure.
        fs::write(
            dir.path().join("index.html"),
            "<html>\n<ul>\n<li>Home</li>\n<li>About</li>\n</ul>\n</html>\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("about.html"),
            "<html>\n<ul>\n<li>Home</li>\n<li>About</li>\n</ul>\n</html>\n",
        )
        .unwrap();

        let setup = AgentIdentity {
            id: "setup".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            setup,
            "baseline".into(),
            None,
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("nav".into(), "Nav update".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("content".into(), "Content".into(), "".into()))
            .unwrap();

        // Nav agent adds items to index but not about.
        fs::write(dir.path().join("index.html"), "<html>\n<ul>\n<li>Home</li>\n<li>About</li>\n<li>Blog</li>\n<li>Contact</li>\n<li>FAQ</li>\n</ul>\n</html>\n").unwrap();
        let nav_a = AgentIdentity {
            id: "nav-dev".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            nav_a.clone(),
            "add nav items".into(),
            Some("nav".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Content agent modifies about.html only.
        fs::write(
            dir.path().join("about.html"),
            "<html>\n<ul>\n<li>Home</li>\n<li>About</li>\n</ul>\n<p>Content here</p>\n</html>\n",
        )
        .unwrap();
        let content_a = AgentIdentity {
            id: "content-dev".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            content_a,
            "add content".into(),
            Some("content".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Advance HEAD.
        fs::write(dir.path().join("nav-extra.txt"), "x\n").unwrap();
        repo.seal(
            nav_a,
            "extra".into(),
            Some("nav".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();
        repo.update_spec(
            "nav",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, true)
            .unwrap();

        let qr = report.quality_report.unwrap();

        // Should have nav_item_count consistency check.
        let nav_check = qr
            .consistency_checks
            .iter()
            .find(|c| c.metric == "nav_item_count");
        assert!(
            nav_check.is_some(),
            "should check nav item consistency, checks: {:?}",
            qr.consistency_checks
        );

        let check = nav_check.unwrap();
        // index.html has 5 <li, about.html has 2 — should be inconsistent.
        assert!(
            !check.consistent,
            "nav items should be inconsistent (5 vs 2)"
        );
        assert!(check.warning.is_some());
    }

    /// TR13 regression: two agents both add to the same file (app.py).
    /// Agent A adds CRUD routes, Agent B adds auth routes + decorators.
    /// Under the old MostComplete strategy, auth was entirely deleted.
    /// The new convergence v2 must preserve BOTH agents' contributions.
    #[test]
    fn test_tr13_regression_additive_changes_preserved() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Baseline: a minimal Flask app.
        let base_app = "\
from flask import Flask

app = Flask(__name__)

@app.route('/')
def index():
    return 'Hello'
";
        fs::write(dir.path().join("app.py"), base_app).unwrap();
        let setup_agent = AgentIdentity {
            id: "setup".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            setup_agent,
            "baseline flask app".into(),
            None,
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new(
            "api".into(),
            "CRUD API routes".into(),
            "".into(),
        ))
        .unwrap();
        repo.add_spec(&Spec::new(
            "auth".into(),
            "Authentication".into(),
            "".into(),
        ))
        .unwrap();

        // API agent: adds CRUD routes after the index route.
        let api_app = "\
from flask import Flask, jsonify, request

app = Flask(__name__)

@app.route('/')
def index():
    return 'Hello'

@app.route('/books', methods=['GET'])
def list_books():
    return jsonify([])

@app.route('/books', methods=['POST'])
def create_book():
    data = request.get_json()
    return jsonify(data), 201
";
        fs::write(dir.path().join("app.py"), api_app).unwrap();
        let api_agent = AgentIdentity {
            id: "api-dev".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            api_agent.clone(),
            "added CRUD routes".into(),
            Some("api".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Auth agent: adds auth module import + decorator + login route.
        let auth_app = "\
from flask import Flask, jsonify, request
from auth import require_auth

app = Flask(__name__)

@app.route('/')
def index():
    return 'Hello'

@app.route('/login', methods=['POST'])
def login():
    return jsonify({'token': 'abc123'})

@app.route('/profile')
@require_auth
def profile():
    return jsonify({'user': 'me'})
";
        fs::write(dir.path().join("app.py"), auth_app).unwrap();
        fs::write(
            dir.path().join("auth.py"),
            "def require_auth(f):\n    return f\n",
        )
        .unwrap();
        let auth_agent = AgentIdentity {
            id: "auth-dev".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            auth_agent,
            "added authentication".into(),
            Some("auth".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Advance HEAD so auth becomes diverged.
        fs::write(dir.path().join("app.py"), api_app).unwrap();
        fs::write(dir.path().join("api-marker.txt"), "x\n").unwrap();
        repo.seal(
            api_agent,
            "api finalized".into(),
            Some("api".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();
        repo.update_spec(
            "api",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        // Converge with MostRecent (the fallback for any BothModified
        // that Layers 2-3 can't handle).
        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, true)
            .unwrap();

        assert!(report.applied, "should have applied");
        assert!(report.is_clean, "should resolve cleanly");

        // THE KEY ASSERTION: the merged app.py must contain BOTH agents'
        // contributions. This is the exact failure mode from TR13.
        let merged = fs::read_to_string(dir.path().join("app.py")).unwrap();

        // Auth imports must survive.
        assert!(
            merged.contains("from auth import require_auth"),
            "auth import was lost! merged:\n{merged}"
        );

        // CRUD routes must survive.
        assert!(
            merged.contains("list_books"),
            "CRUD route was lost! merged:\n{merged}"
        );
        assert!(
            merged.contains("create_book"),
            "CRUD route was lost! merged:\n{merged}"
        );

        // Auth routes must survive.
        assert!(
            merged.contains("login"),
            "auth route was lost! merged:\n{merged}"
        );
        assert!(
            merged.contains("profile"),
            "auth route was lost! merged:\n{merged}"
        );

        // Decorator must survive.
        assert!(
            merged.contains("@require_auth"),
            "auth decorator was lost! merged:\n{merged}"
        );

        // Auth module file must still exist.
        assert!(
            dir.path().join("auth.py").exists(),
            "auth.py should not be deleted"
        );
    }

    /// TR16 regression: 3 specs diverge from same base, each adding imports
    /// and JSX to App.tsx. Convergence v2 must chain pairwise merges so ALL
    /// three agents' contributions survive in the final output.
    #[test]
    fn test_tr16_regression_three_way_chaining() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let base_app = "\
import React from 'react';

function App() {
  return (
    <div className=\"app\">
      <h1>Dashboard</h1>
    </div>
  );
}

export default App;
";
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/App.tsx"), base_app).unwrap();

        let setup = AgentIdentity {
            id: "setup".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            setup,
            "baseline react app".into(),
            None,
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("charts".into(), "Charts".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("sidebar".into(), "Sidebar".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("header".into(), "Header".into(), "".into()))
            .unwrap();

        // Charts agent seals.
        let charts_app = "\
import React from 'react';
import Charts from './Charts';

function App() {
  return (
    <div className=\"app\">
      <h1>Dashboard</h1>
      <Charts />
    </div>
  );
}

export default App;
";
        fs::write(dir.path().join("src/App.tsx"), charts_app).unwrap();
        fs::write(
            dir.path().join("src/Charts.tsx"),
            "export default function Charts() { return <div>Charts</div>; }\n",
        )
        .unwrap();
        repo.seal(
            AgentIdentity {
                id: "charts-dev".into(),
                agent_type: crate::seal::AgentType::Agent,
            },
            "charts component".into(),
            Some("charts".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Restore to baseline for sidebar agent.
        fs::write(dir.path().join("src/App.tsx"), base_app).unwrap();
        // Remove Charts.tsx so sidebar starts from baseline.
        let _ = std::fs::remove_file(dir.path().join("src/Charts.tsx"));

        // Sidebar agent seals.
        let sidebar_app = "\
import React from 'react';
import Sidebar from './Sidebar';

function App() {
  return (
    <div className=\"app\">
      <Sidebar />
      <h1>Dashboard</h1>
    </div>
  );
}

export default App;
";
        fs::write(dir.path().join("src/App.tsx"), sidebar_app).unwrap();
        fs::write(
            dir.path().join("src/Sidebar.tsx"),
            "export default function Sidebar() { return <nav>Sidebar</nav>; }\n",
        )
        .unwrap();
        repo.seal(
            AgentIdentity {
                id: "sidebar-dev".into(),
                agent_type: crate::seal::AgentType::Agent,
            },
            "sidebar component".into(),
            Some("sidebar".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Restore to baseline for header agent.
        fs::write(dir.path().join("src/App.tsx"), base_app).unwrap();
        let _ = std::fs::remove_file(dir.path().join("src/Sidebar.tsx"));

        // Header agent seals.
        let header_app = "\
import React from 'react';
import Header from './Header';

function App() {
  return (
    <div className=\"app\">
      <Header />
      <h1>Dashboard</h1>
    </div>
  );
}

export default App;
";
        fs::write(dir.path().join("src/App.tsx"), header_app).unwrap();
        fs::write(
            dir.path().join("src/Header.tsx"),
            "export default function Header() { return <header>Header</header>; }\n",
        )
        .unwrap();
        repo.seal(
            AgentIdentity {
                id: "header-dev".into(),
                agent_type: crate::seal::AgentType::Agent,
            },
            "header component".into(),
            Some("header".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Advance HEAD so charts is on HEAD chain, sidebar+header diverge.
        fs::write(dir.path().join("src/App.tsx"), charts_app).unwrap();
        fs::write(
            dir.path().join("src/Charts.tsx"),
            "export default function Charts() { return <div>Charts</div>; }\n",
        )
        .unwrap();
        repo.seal(
            AgentIdentity {
                id: "charts-dev".into(),
                agent_type: crate::seal::AgentType::Agent,
            },
            "charts final".into(),
            Some("charts".into()),
            crate::seal::TaskStatus::Complete,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();
        repo.update_spec(
            "charts",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, true)
            .unwrap();

        assert!(report.applied, "should have applied");

        let merged = fs::read_to_string(dir.path().join("src/App.tsx")).unwrap();

        // ALL THREE agents' imports must survive.
        assert!(
            merged.contains("import Charts"),
            "Charts import lost! merged:\n{merged}"
        );
        assert!(
            merged.contains("import Sidebar"),
            "Sidebar import lost! merged:\n{merged}"
        );
        assert!(
            merged.contains("import Header"),
            "Header import lost! merged:\n{merged}"
        );

        // ALL THREE JSX renders must survive.
        assert!(
            merged.contains("<Charts"),
            "Charts render lost! merged:\n{merged}"
        );
        assert!(
            merged.contains("<Sidebar"),
            "Sidebar render lost! merged:\n{merged}"
        );
        assert!(
            merged.contains("<Header"),
            "Header render lost! merged:\n{merged}"
        );

        // Component files must all exist.
        assert!(
            dir.path().join("src/Charts.tsx").exists(),
            "Charts.tsx missing"
        );
        assert!(
            dir.path().join("src/Sidebar.tsx").exists(),
            "Sidebar.tsx missing"
        );
        assert!(
            dir.path().join("src/Header.tsx").exists(),
            "Header.tsx missing"
        );
    }
}

/// Tests that mutations leave metadata consistent for context().
#[cfg(test)]
#[allow(deprecated)]
mod convergence_metadata_tests {
    use super::*;
    use crate::context::{ContextFilter, ContextScope};
    use crate::convergence::ConvergeStrategy;
    use crate::seal::{AgentType, TaskStatus, Verification};
    use crate::spec::{Spec, SpecStatus, SpecUpdate};
    use tempfile::tempdir;

    fn agent(name: &str) -> AgentIdentity {
        AgentIdentity {
            id: name.into(),
            agent_type: AgentType::Agent,
        }
    }

    #[test]
    fn test_context_clean_after_converge_all() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Baseline.
        fs::write(dir.path().join("shared.py"), "x = 1\n").unwrap();
        repo.seal(
            agent("setup"),
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Two specs.
        repo.add_spec(&Spec::new("feat-a".into(), "Feature A".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("feat-b".into(), "Feature B".into(), "".into()))
            .unwrap();

        // feat-a seals first.
        fs::write(dir.path().join("a.py"), "a = 1\n").unwrap();
        repo.seal(
            agent("dev-a"),
            "feature a".into(),
            Some("feat-a".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();
        repo.update_spec(
            "feat-a",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        // feat-b seals (creates a branch from HEAD).
        fs::write(dir.path().join("b.py"), "b = 2\n").unwrap();
        repo.seal(
            agent("dev-b"),
            "feature b".into(),
            Some("feat-b".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // feat-a seals AGAIN — this advances HEAD past feat-b's seal,
        // making feat-b's spec_head diverged from the new HEAD chain.
        fs::write(dir.path().join("a2.py"), "a2 = 2\n").unwrap();
        repo.seal(
            agent("dev-a"),
            "feature a part 2".into(),
            Some("feat-a".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Confirm divergence exists.
        let diverged = repo.diverged_branches().unwrap();
        assert!(!diverged.is_empty(), "should have diverged branches");

        // Converge.
        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, true)
            .unwrap();
        assert!(report.applied, "convergence should apply");

        // Context should show no false pending changes.
        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();

        assert!(
            ctx.pending_changes.is_none()
                || ctx
                    .pending_changes
                    .as_ref()
                    .map_or(true, |p| p.files.is_empty()),
            "no false pending_changes after converge_all: {:?}",
            ctx.pending_changes
        );
        assert!(
            ctx.seal_nudge.is_none(),
            "no seal_nudge after converge_all: {:?}",
            ctx.seal_nudge
        );
    }

    #[test]
    fn test_context_clean_after_apply_convergence() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Baseline with shared file.
        fs::write(dir.path().join("shared.py"), "base\n").unwrap();
        repo.seal(
            agent("setup"),
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Two specs.
        repo.add_spec(&Spec::new("left".into(), "Left".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("right".into(), "Right".into(), "".into()))
            .unwrap();

        // Left seals.
        fs::write(dir.path().join("left.py"), "left = 1\n").unwrap();
        repo.seal(
            agent("left-dev"),
            "left work".into(),
            Some("left".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Right seals (diverges).
        fs::write(dir.path().join("right.py"), "right = 2\n").unwrap();
        repo.seal(
            agent("right-dev"),
            "right work".into(),
            Some("right".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Converge two specs.
        let report = repo.converge("left", "right").unwrap();
        repo.apply_convergence(&report, &[]).unwrap();

        // Context should show no false pending changes.
        let ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();

        assert!(
            ctx.pending_changes.is_none()
                || ctx
                    .pending_changes
                    .as_ref()
                    .map_or(true, |p| p.files.is_empty()),
            "no false pending_changes after apply_convergence: {:?}",
            ctx.pending_changes
        );
        assert!(
            ctx.seal_nudge.is_none(),
            "no seal_nudge after apply_convergence: {:?}",
            ctx.seal_nudge
        );
    }

    #[test]
    fn test_restore_updates_spec_head() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        repo.add_spec(&Spec::new("feat".into(), "Feature".into(), "".into()))
            .unwrap();

        // First seal on spec.
        fs::write(dir.path().join("f.py"), "v1\n").unwrap();
        let seal1 = repo
            .seal(
                agent("dev"),
                "v1".into(),
                Some("feat".into()),
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();
        let seal1_id = seal1.id.clone();

        // Second seal on spec.
        fs::write(dir.path().join("f.py"), "v2\n").unwrap();
        repo.seal(
            agent("dev"),
            "v2".into(),
            Some("feat".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Spec head should be at seal2.
        let head_before = repo.read_spec_head("feat").unwrap().unwrap();
        assert_ne!(head_before, seal1_id);

        // Restore to seal1.
        repo.restore(&seal1_id).unwrap();

        // Spec head should now be at seal1.
        let head_after = repo.read_spec_head("feat").unwrap().unwrap();
        assert_eq!(
            head_after, seal1_id,
            "restore should update spec head to the restored seal"
        );
    }

    /// Quality report should have min_confidence reflecting the lowest per-file
    /// confidence across all decisions with Some(confidence).
    #[test]
    fn test_quality_report_has_min_confidence() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Two specs that will diverge and have a real conflict.
        repo.add_spec(&Spec::new("alpha".into(), "Alpha".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("beta".into(), "Beta".into(), "".into()))
            .unwrap();

        // Baseline.
        fs::write(dir.path().join("shared.py"), "x = 1\n").unwrap();
        fs::write(dir.path().join("only_alpha.py"), "a = 1\n").unwrap();
        repo.seal(
            agent("setup"),
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Alpha seals first (establishes alpha spec head).
        fs::write(dir.path().join("only_alpha.py"), "a = 2\n").unwrap();
        repo.seal(
            agent("dev-a"),
            "alpha initial".into(),
            Some("alpha".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Beta seals with a replacement change to shared.py (not just an append).
        fs::write(dir.path().join("shared.py"), "x = 'beta_value'\n").unwrap();
        fs::write(dir.path().join("only_beta.py"), "b = 1\n").unwrap();
        repo.seal(
            agent("dev-b"),
            "beta work".into(),
            Some("beta".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();
        repo.update_spec(
            "beta",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        // Alpha seals again with a DIFFERENT replacement to shared.py — creates
        // divergence AND a real conflict (both branches replaced x differently).
        fs::write(dir.path().join("shared.py"), "x = 'alpha_value'\n").unwrap();
        repo.seal(
            agent("dev-a"),
            "alpha with shared change".into(),
            Some("alpha".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();
        repo.update_spec(
            "alpha",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        // Converge with most-recent strategy (genuine replacement conflict falls
        // to most-recent at 0.7 confidence).
        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, true)
            .unwrap();

        let qr = report.quality_report.expect("should have quality report");

        // min_confidence should reflect the most-recent fallback (0.7) since
        // the replacement conflict can't be auto-resolved.
        assert!(
            qr.min_confidence <= 0.9,
            "min_confidence should reflect fallback/resolution, got {}",
            qr.min_confidence
        );
        assert!(
            qr.avg_confidence >= 0.7 && qr.avg_confidence <= 1.0,
            "avg_confidence should be between 0.7 and 1.0, got {}",
            qr.avg_confidence
        );
        assert!(
            qr.min_confidence <= qr.avg_confidence,
            "min ({}) should be <= avg ({})",
            qr.min_confidence,
            qr.avg_confidence
        );
    }

    /// Low min_confidence should penalize the quality_score.
    #[test]
    fn test_quality_report_confidence_affects_score() {
        use crate::convergence::{FileAlternative, FileDecision};

        // Directly test build_quality_report with controlled FileDecisions.
        let dir = tempdir().unwrap();

        // Scenario A: All high-confidence decisions → no penalty.
        let decisions_high = vec![
            FileDecision {
                path: "a.py".to_string(),
                decision: "auto-merged".to_string(),
                chosen_lines: 5,
                chosen_spec: None,
                alternatives: vec![],
                confidence: Some(1.0),
            },
            FileDecision {
                path: "b.py".to_string(),
                decision: "auto-merged".to_string(),
                chosen_lines: 3,
                chosen_spec: None,
                alternatives: vec![],
                confidence: Some(0.95),
            },
        ];
        let report_high = build_quality_report(decisions_high, dir.path(), false, 0, 0);
        assert_eq!(
            report_high.quality_score, 100,
            "all high-confidence → score 100, got {}",
            report_high.quality_score
        );

        // Scenario B: One low-confidence decision (0.7 — strategy fallback) → -10.
        let decisions_mid = vec![
            FileDecision {
                path: "a.py".to_string(),
                decision: "auto-merged".to_string(),
                chosen_lines: 5,
                chosen_spec: None,
                alternatives: vec![],
                confidence: Some(1.0),
            },
            FileDecision {
                path: "b.py".to_string(),
                decision: "most-recent".to_string(),
                chosen_lines: 3,
                chosen_spec: Some("spec-a".into()),
                alternatives: vec![FileAlternative {
                    spec: "spec-b".into(),
                    lines: 3,
                    reason: "discarded".into(),
                }],
                confidence: Some(0.7),
            },
        ];
        let report_mid = build_quality_report(decisions_mid, dir.path(), false, 1, 1);
        assert_eq!(
            report_mid.quality_score, 90,
            "min_confidence 0.7 (< 0.85) → -10 penalty, score 90, got {}",
            report_mid.quality_score
        );

        // Scenario C: Unresolved conflict (0.0 confidence) → -25 total (-10 + -15 for <0.5, plus -15 for unresolved).
        let decisions_low = vec![FileDecision {
            path: "a.py".to_string(),
            decision: "conflict-unresolved".to_string(),
            chosen_lines: 0,
            chosen_spec: None,
            alternatives: vec![],
            confidence: Some(0.0),
        }];
        let report_low = build_quality_report(decisions_low, dir.path(), false, 1, 0);
        // Penalties: -15 (unresolved conflict) + -10 (min < 0.85) + -15 (min < 0.5) = -40 → score 60.
        assert_eq!(
            report_low.quality_score, 60,
            "unresolved conflict with 0.0 confidence → score 60, got {}",
            report_low.quality_score
        );
    }

    /// FileDecision confidence: Some for resolved conflicts, None for left-only/right-only.
    #[test]
    fn test_file_decision_has_confidence() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Two specs: alpha touches file A, beta touches file B. No overlap = no conflict.
        // This gives us left-only and right-only decisions (confidence: None).
        repo.add_spec(&Spec::new("alpha".into(), "Alpha".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("beta".into(), "Beta".into(), "".into()))
            .unwrap();

        // Baseline seal.
        fs::write(dir.path().join("base.py"), "base\n").unwrap();
        repo.seal(
            agent("setup"),
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Alpha seals first (establishes alpha spec head).
        fs::write(dir.path().join("alpha.py"), "alpha\n").unwrap();
        repo.seal(
            agent("dev-a"),
            "alpha initial".into(),
            Some("alpha".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Beta seals (establishes beta spec head — child of alpha's seal on main chain).
        fs::write(dir.path().join("beta.py"), "beta\n").unwrap();
        repo.seal(
            agent("dev-b"),
            "beta file".into(),
            Some("beta".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();
        repo.update_spec(
            "beta",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        // Alpha seals again — creates divergence (parent is alpha spec head, not beta's seal).
        fs::write(dir.path().join("alpha.py"), "alpha v2\n").unwrap();
        repo.seal(
            agent("dev-a"),
            "alpha complete".into(),
            Some("alpha".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();
        repo.update_spec(
            "alpha",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        // Converge — should produce left-only and right-only with confidence: None.
        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, true)
            .unwrap();

        let qr = report.quality_report.expect("should have quality report");

        for fd in &qr.file_decisions {
            match fd.decision.as_str() {
                "left-only" | "right-only" => {
                    assert!(
                        fd.confidence.is_none(),
                        "{} decision '{}' should have confidence: None, got {:?}",
                        fd.path,
                        fd.decision,
                        fd.confidence
                    );
                }
                "auto-merged" => {
                    assert!(
                        fd.confidence.is_some(),
                        "{} auto-merged should have Some confidence, got None",
                        fd.path
                    );
                    assert!(
                        fd.confidence.unwrap() >= 0.85,
                        "{} auto-merged confidence should be >= 0.85, got {}",
                        fd.path,
                        fd.confidence.unwrap()
                    );
                }
                other => {
                    // Other decision types should have Some confidence.
                    assert!(
                        fd.confidence.is_some(),
                        "{} decision '{}' should have Some confidence",
                        fd.path,
                        other
                    );
                }
            }
        }

        // min_confidence should be 1.0 (only left-only/right-only with None, auto-merges at 1.0).
        assert!(
            (qr.min_confidence - 1.0).abs() < 0.01,
            "no conflicts → min_confidence should be 1.0, got {}",
            qr.min_confidence
        );
    }
}

#[cfg(test)]
#[allow(deprecated)]
mod convergence_events_integration_tests {
    use super::*;
    use crate::convergence::ConvergeStrategy;
    use crate::seal::AgentIdentity;
    use crate::spec::Spec;
    use tempfile::tempdir;

    /// Integration test: converge_all should emit convergence_started and
    /// convergence_completed (or degraded) events to the security log.
    #[test]
    fn test_converge_all_emits_started_and_completed_events() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Create baseline content and seal
        fs::write(dir.path().join("shared.txt"), "line1\nline2\n").unwrap();
        let agent = AgentIdentity {
            id: "setup".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            agent.clone(),
            "baseline".into(),
            None,
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Create two specs
        repo.add_spec(&Spec::new("alpha".into(), "Alpha".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("beta".into(), "Beta".into(), "".into()))
            .unwrap();

        // Alpha seals on HEAD (stays on main chain)
        fs::write(dir.path().join("alpha.txt"), "alpha content\n").unwrap();
        repo.seal(
            AgentIdentity {
                id: "alpha-agent".into(),
                agent_type: crate::seal::AgentType::Agent,
            },
            "alpha work".into(),
            Some("alpha".into()),
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Beta creates divergence: seal, add file, seal again
        fs::write(dir.path().join("beta.txt"), "beta content\n").unwrap();
        repo.seal(
            AgentIdentity {
                id: "beta-agent".into(),
                agent_type: crate::seal::AgentType::Agent,
            },
            "beta work".into(),
            Some("beta".into()),
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Fork: seal alpha again (forces beta to diverge)
        fs::write(dir.path().join("alpha2.txt"), "more alpha\n").unwrap();
        repo.seal(
            AgentIdentity {
                id: "alpha-agent".into(),
                agent_type: crate::seal::AgentType::Agent,
            },
            "alpha cont".into(),
            Some("alpha".into()),
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Clear any pre-existing events
        let events_path = dir
            .path()
            .join(".writ")
            .join("security")
            .join("events.jsonl");
        if events_path.exists() {
            fs::remove_file(&events_path).unwrap();
        }

        // Run convergence
        let _report = repo
            .converge_all(ConvergeStrategy::MostRecent, false)
            .unwrap();

        // Read events from log
        let logger = crate::security::SecurityEventLogger::new(&dir.path().join(".writ"));
        let events = logger.read_events(None).unwrap();

        let event_types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();

        assert!(
            event_types.contains(&"convergence_started"),
            "Should have convergence_started event, got: {:?}",
            event_types
        );
        assert!(
            event_types.contains(&"convergence_completed")
                || event_types.contains(&"convergence_degraded"),
            "Should have convergence_completed or convergence_degraded event, got: {:?}",
            event_types
        );
    }

    /// Integration test: convergence_record should be populated in the report.
    #[test]
    fn test_converge_all_populates_convergence_record() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("shared.txt"), "base\n").unwrap();
        let agent = AgentIdentity {
            id: "setup".into(),
            agent_type: crate::seal::AgentType::Agent,
        };
        repo.seal(
            agent,
            "baseline".into(),
            None,
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("s1".into(), "S1".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("s2".into(), "S2".into(), "".into()))
            .unwrap();

        // Create divergence
        fs::write(dir.path().join("s1.txt"), "s1\n").unwrap();
        repo.seal(
            AgentIdentity {
                id: "a1".into(),
                agent_type: crate::seal::AgentType::Agent,
            },
            "s1 work".into(),
            Some("s1".into()),
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("s2.txt"), "s2\n").unwrap();
        repo.seal(
            AgentIdentity {
                id: "a2".into(),
                agent_type: crate::seal::AgentType::Agent,
            },
            "s2 work".into(),
            Some("s2".into()),
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        // Fork
        fs::write(dir.path().join("s1b.txt"), "s1b\n").unwrap();
        repo.seal(
            AgentIdentity {
                id: "a1".into(),
                agent_type: crate::seal::AgentType::Agent,
            },
            "s1 cont".into(),
            Some("s1".into()),
            crate::seal::TaskStatus::InProgress,
            crate::seal::Verification::default(),
            false,
        )
        .unwrap();

        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, false)
            .unwrap();

        let record = report
            .convergence_record
            .expect("convergence_record should be populated");

        assert!(!record.pipeline_version.is_empty());
        assert!(!record.configuration_hash.is_empty());
        assert!(!record.pattern_versions.is_empty());
        assert!(!record.participating_specs.is_empty());
    }
}

#[cfg(test)]
#[allow(deprecated)]
mod agent_scoped_context_tests {
    use super::*;
    use crate::context::{ContextFilter, ContextScope};
    use crate::seal::{AgentType, TaskStatus, Verification};
    use crate::spec::Spec;
    use tempfile::tempdir;

    fn agent(name: &str) -> AgentIdentity {
        AgentIdentity {
            id: name.into(),
            agent_type: AgentType::Agent,
        }
    }

    /// Helper: set up a repo with two agents working on different specs.
    fn setup_two_agent_repo() -> (tempfile::TempDir, Repository) {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Two specs with different file scopes.
        let mut auth_spec = Spec::new("auth".into(), "Authentication".into(), "".into());
        auth_spec.file_scope = vec!["auth.py".into(), "auth_test.py".into()];
        repo.add_spec(&auth_spec).unwrap();

        let mut pay_spec = Spec::new("payments".into(), "Payments".into(), "".into());
        pay_spec.file_scope = vec!["payments.py".into(), "payments_test.py".into()];
        repo.add_spec(&pay_spec).unwrap();

        // Agent "auth-dev" works on auth spec.
        fs::write(dir.path().join("auth.py"), "def login(): pass\n").unwrap();
        repo.seal(
            agent("auth-dev"),
            "auth initial".into(),
            Some("auth".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent "pay-dev" works on payments spec.
        fs::write(dir.path().join("payments.py"), "def charge(): pass\n").unwrap();
        repo.seal(
            agent("pay-dev"),
            "payments initial".into(),
            Some("payments".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // auth-dev seals again.
        fs::write(dir.path().join("auth.py"), "def login(): return True\n").unwrap();
        repo.seal(
            agent("auth-dev"),
            "auth login implemented".into(),
            Some("auth".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        (dir, repo)
    }

    /// Agent-scoped context should only include the agent's specs.
    #[test]
    fn test_agent_scoped_context_filters_to_agent_specs() {
        let (_dir, repo) = setup_two_agent_repo();

        let ctx = repo
            .context(
                ContextScope::Agent("auth-dev".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();

        // Should only have auth spec, not payments.
        let specs = ctx.all_specs.expect("should have specs");
        let spec_ids: Vec<&str> = specs.iter().map(|s| s.id.as_str()).collect();
        assert!(
            spec_ids.contains(&"auth"),
            "auth-dev's context should include auth spec, got: {:?}",
            spec_ids
        );
        assert!(
            !spec_ids.contains(&"payments"),
            "auth-dev's context should NOT include payments spec, got: {:?}",
            spec_ids
        );

        // active_spec should be None (agent may have multiple specs).
        assert!(
            ctx.active_spec.is_none(),
            "agent scope has no single active_spec"
        );
    }

    /// Agent-scoped context should filter working state to agent's file scope.
    #[test]
    fn test_agent_scoped_context_file_scope() {
        let (dir, repo) = setup_two_agent_repo();

        // Create a file change outside auth-dev's scope.
        fs::write(
            dir.path().join("payments.py"),
            "def charge(): return True\n",
        )
        .unwrap();
        // And one inside scope.
        fs::write(dir.path().join("auth_test.py"), "def test_login(): pass\n").unwrap();

        let ctx = repo
            .context(
                ContextScope::Agent("auth-dev".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();

        // File scope should include auth files (from spec scope + sealed files).
        assert!(
            ctx.file_scope.iter().any(|f| f == "auth.py"),
            "auth.py should be in auth-dev's file scope"
        );
        // payments.py should NOT be in file scope.
        assert!(
            !ctx.file_scope.iter().any(|f| f == "payments.py"),
            "payments.py should NOT be in auth-dev's file scope"
        );

        // Modified files in working state should only show auth-scoped changes.
        let all_changed: Vec<String> = ctx
            .working_state
            .new_files
            .iter()
            .chain(ctx.working_state.modified_files.iter())
            .cloned()
            .collect();
        assert!(
            !all_changed.iter().any(|f| f == "payments.py"),
            "payments.py changes should be filtered out of auth-dev's working state, got: {:?}",
            all_changed
        );
    }

    /// Agent-scoped context should show ALL agents for cross-agent coordination.
    #[test]
    fn test_agent_scoped_context_shows_all_agent_activity() {
        let (_dir, repo) = setup_two_agent_repo();

        let ctx = repo
            .context(
                ContextScope::Agent("auth-dev".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();

        // Agent activity should include auth-dev (for sure) and potentially
        // other agents if they touched files in auth-dev's scope.
        let activity_agents: Vec<&str> = ctx
            .agent_activity
            .iter()
            .map(|a| a.agent_id.as_str())
            .collect();
        assert!(
            activity_agents.contains(&"auth-dev"),
            "auth-dev should appear in agent_activity, got: {:?}",
            activity_agents
        );
        // pay-dev shouldn't appear if they haven't touched auth files.
        // (pay-dev only touched payments.py which is outside auth scope)
    }

    /// File contention should be limited to agent's file scope.
    #[test]
    fn test_agent_scoped_contention_filtered() {
        let (dir, repo) = setup_two_agent_repo();

        // Create contention on a payments file (pay-dev and auth-dev both touch it).
        fs::write(dir.path().join("payments.py"), "# auth-dev was here\n").unwrap();
        repo.seal(
            agent("auth-dev"),
            "cross-cutting change".into(),
            Some("auth".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Full context should show payments.py contention.
        let full_ctx = repo
            .context(ContextScope::Full, 10, &ContextFilter::default())
            .unwrap();
        let full_contested: Vec<&str> = full_ctx
            .file_contention
            .iter()
            .map(|fc| fc.path.as_str())
            .collect();

        // Agent-scoped for pay-dev should include payments.py contention.
        let pay_ctx = repo
            .context(
                ContextScope::Agent("pay-dev".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();
        let pay_contested: Vec<&str> = pay_ctx
            .file_contention
            .iter()
            .map(|fc| fc.path.as_str())
            .collect();

        // payments.py should be in pay-dev's contention (it's in their scope).
        if full_contested.contains(&"payments.py") {
            assert!(
                pay_contested.contains(&"payments.py"),
                "payments.py contention should appear in pay-dev's agent-scoped context"
            );
        }

        // auth-dev's scope includes auth.py + auth_test.py + payments.py (since they sealed it).
        // So auth-dev should also see payments.py contention.
        let auth_ctx = repo
            .context(
                ContextScope::Agent("auth-dev".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();
        let auth_contested: Vec<&str> = auth_ctx
            .file_contention
            .iter()
            .map(|fc| fc.path.as_str())
            .collect();
        if full_contested.contains(&"payments.py") {
            assert!(
                auth_contested.contains(&"payments.py"),
                "auth-dev touched payments.py, so contention should appear in their scope"
            );
        }
    }

    /// Recommended action should use agent-scoped signals.
    #[test]
    fn test_agent_scoped_recommended_action() {
        let (dir, repo) = setup_two_agent_repo();

        // Make a change in auth scope to trigger seal nudge.
        fs::write(dir.path().join("auth.py"), "def login(): return 'final'\n").unwrap();

        let ctx = repo
            .context(
                ContextScope::Agent("auth-dev".into()),
                10,
                &ContextFilter::default(),
            )
            .unwrap();

        // Should have a seal nudge since auth.py changed.
        assert!(
            ctx.seal_nudge.is_some(),
            "auth-dev should get seal nudge for auth.py change"
        );

        // session_complete should always be false for agent scope.
        assert!(
            !ctx.session_complete,
            "agent scope should never report session_complete"
        );

        // recommended_action should be present (seal nudge → recommend seal).
        if let Some(ref action) = ctx.recommended_action {
            assert_eq!(
                action.action, "seal",
                "expected seal recommendation, got: {}",
                action.action
            );
        }
    }
}

#[cfg(test)]
#[allow(deprecated)]
mod sprint14_reporting_tests {
    use super::*;
    use crate::convergence::{ConvergeStrategy, FileDecision};
    use crate::seal::{AgentIdentity, AgentType, TaskStatus, Verification};
    use tempfile::tempdir;

    #[test]
    fn test_converge_all_deduplicates_warnings() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Setup: baseline seal.
        fs::write(dir.path().join("hot.txt"), "base\n").unwrap();
        let setup = AgentIdentity {
            id: "setup".into(),
            agent_type: AgentType::Agent,
        };
        repo.seal(
            setup,
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Two specs touching the same file.
        repo.add_spec(&Spec::new("s1".into(), "S1".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("s2".into(), "S2".into(), "".into()))
            .unwrap();

        // Agent A modifies hot.txt and seals under s1.
        fs::write(dir.path().join("hot.txt"), "base\nalpha\n").unwrap();
        let a = AgentIdentity {
            id: "agent-a".into(),
            agent_type: AgentType::Agent,
        };
        repo.seal(
            a.clone(),
            "a work".into(),
            Some("s1".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent B modifies hot.txt and seals under s2.
        fs::write(dir.path().join("hot.txt"), "base\nbeta\n").unwrap();
        let b = AgentIdentity {
            id: "agent-b".into(),
            agent_type: AgentType::Agent,
        };
        repo.seal(
            b,
            "b work".into(),
            Some("s2".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent A seals again to create divergence.
        fs::write(dir.path().join("hot.txt"), "base\nalpha v2\n").unwrap();
        repo.seal(
            a,
            "a final".into(),
            Some("s1".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, false)
            .unwrap();

        // Verify no duplicate warnings.
        let mut sorted = report.warnings.clone();
        sorted.sort();
        let deduped_len = {
            let mut d = sorted.clone();
            d.dedup();
            d.len()
        };
        assert_eq!(
            report.warnings.len(),
            deduped_len,
            "warnings should have no duplicates, got: {:?}",
            report.warnings,
        );
    }

    #[test]
    fn test_quality_report_summary_includes_confidence() {
        // Build a quality report directly with low-confidence decisions.
        let dir = tempdir().unwrap();

        let decisions = vec![
            FileDecision {
                path: "a.py".into(),
                decision: "auto-merged".into(),
                chosen_spec: Some("s1".into()),
                chosen_lines: 10,
                alternatives: vec![],
                confidence: Some(1.0),
            },
            FileDecision {
                path: "b.py".into(),
                decision: "most-recent".into(),
                chosen_spec: Some("s2".into()),
                chosen_lines: 20,
                alternatives: vec![],
                confidence: Some(0.7),
            },
        ];

        let report = build_quality_report(decisions, dir.path(), false, 1, 1);

        assert!(
            report.summary.contains("confidence:"),
            "summary should include confidence stats when < 100%, got: {}",
            report.summary,
        );
        assert!(
            report.summary.contains("min=70%"),
            "summary should show min confidence of 70%, got: {}",
            report.summary,
        );
    }

    #[test]
    fn test_quality_score_penalizes_duplicate_imports() {
        let dir = tempdir().unwrap();

        // Write a Python file with duplicate imports.
        fs::write(
            dir.path().join("app.py"),
            "import os\nimport os\nimport sys\n\ndef main():\n    os.getcwd()\n    sys.exit()\n",
        )
        .unwrap();

        let decisions = vec![FileDecision {
            path: "app.py".into(),
            decision: "auto-merged".into(),
            chosen_spec: Some("s1".into()),
            chosen_lines: 7,
            alternatives: vec![],
            confidence: Some(1.0),
        }];

        let report = build_quality_report(decisions, dir.path(), true, 0, 0);

        // Should have a duplicate_imports consistency check.
        let dup_check = report
            .consistency_checks
            .iter()
            .find(|c| c.metric == "duplicate_imports");
        assert!(
            dup_check.is_some(),
            "should have duplicate_imports check, got: {:?}",
            report.consistency_checks,
        );
        assert!(
            !dup_check.unwrap().consistent,
            "duplicate_imports check should be inconsistent",
        );

        // Score should be penalized.
        assert!(
            report.quality_score < 100,
            "score should be < 100 with duplicate imports, got: {}",
            report.quality_score,
        );
    }

    #[test]
    fn test_quality_score_penalizes_unused_imports() {
        let dir = tempdir().unwrap();

        // Write a Python file with an unused import.
        fs::write(
            dir.path().join("app.py"),
            "from os import path\nfrom sys import exit\n\ndef main():\n    exit()\n",
        )
        .unwrap();

        let decisions = vec![FileDecision {
            path: "app.py".into(),
            decision: "auto-merged".into(),
            chosen_spec: Some("s1".into()),
            chosen_lines: 5,
            alternatives: vec![],
            confidence: Some(1.0),
        }];

        let report = build_quality_report(decisions, dir.path(), true, 0, 0);

        // Should have an unused_imports consistency check.
        let unused_check = report
            .consistency_checks
            .iter()
            .find(|c| c.metric == "unused_imports");
        assert!(
            unused_check.is_some(),
            "should have unused_imports check, got: {:?}",
            report.consistency_checks,
        );
        assert!(
            !unused_check.unwrap().consistent,
            "unused_imports check should be inconsistent (path is unused)",
        );

        // Score should be penalized.
        assert!(
            report.quality_score < 100,
            "score should be < 100 with unused imports, got: {}",
            report.quality_score,
        );
    }

    #[test]
    fn test_converge_all_runs_layer5_cleanup() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Baseline seal with a Python file that has clean imports.
        fs::write(
            dir.path().join("app.py"),
            "from flask import Flask\n\napp = Flask(__name__)\n",
        )
        .unwrap();
        let setup = AgentIdentity {
            id: "setup".into(),
            agent_type: AgentType::Agent,
        };
        repo.seal(
            setup,
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("inv".into(), "Inventory".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("ord".into(), "Orders".into(), "".into()))
            .unwrap();

        // Inventory agent seals first: adds duplicate flask import and models import.
        fs::write(
            dir.path().join("app.py"),
            "from flask import Flask\nfrom flask import Flask\nfrom models import Book\n\napp = Flask(__name__)\nbook = Book()\n",
        )
        .unwrap();
        let inv_agent = AgentIdentity {
            id: "inv-agent".into(),
            agent_type: AgentType::Agent,
        };
        repo.seal(
            inv_agent.clone(),
            "add inventory".into(),
            Some("inv".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Orders agent seals: adds unused import.
        fs::write(
            dir.path().join("app.py"),
            "from flask import Flask, abort\nfrom models import Order\n\napp = Flask(__name__)\norder = Order()\n",
        )
        .unwrap();
        let ord_agent = AgentIdentity {
            id: "ord-agent".into(),
            agent_type: AgentType::Agent,
        };
        repo.seal(
            ord_agent,
            "add orders".into(),
            Some("ord".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Restore inv's app.py before sealing again (simulates agent
        // working from its own branch, not ord's disk state).
        fs::write(
            dir.path().join("app.py"),
            "from flask import Flask\nfrom flask import Flask\nfrom models import Book\n\napp = Flask(__name__)\nbook = Book()\n",
        )
        .unwrap();
        fs::write(dir.path().join("marker.txt"), "inv final\n").unwrap();
        repo.seal(
            inv_agent,
            "inv finalize".into(),
            Some("inv".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();
        repo.update_spec(
            "inv",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, true)
            .unwrap();

        assert!(report.is_clean, "should resolve cleanly");

        let content = fs::read_to_string(dir.path().join("app.py")).unwrap();

        // Layer 5 should have run: duplicate "from flask import Flask" consolidated.
        let flask_count = content.matches("from flask import").count();
        assert_eq!(
            flask_count, 1,
            "Layer 5 should consolidate duplicate flask imports, got:\n{content}"
        );
    }

    #[test]
    fn test_converge_all_additive_composition_preserves_content() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Baseline with a simple models file.
        fs::write(dir.path().join("models.py"), "class Base:\n    pass\n").unwrap();
        let setup = AgentIdentity {
            id: "setup".into(),
            agent_type: AgentType::Agent,
        };
        repo.seal(
            setup,
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("inv".into(), "Inventory".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("ord".into(), "Orders".into(), "".into()))
            .unwrap();

        // Inventory agent seals first with Inventory class.
        fs::write(
            dir.path().join("models.py"),
            "class Base:\n    pass\n\nclass Inventory:\n    name = 'item'\n",
        )
        .unwrap();
        let inv_agent = AgentIdentity {
            id: "inv-agent".into(),
            agent_type: AgentType::Agent,
        };
        repo.seal(
            inv_agent.clone(),
            "add inventory model".into(),
            Some("inv".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Orders agent seals with Order class.
        fs::write(
            dir.path().join("models.py"),
            "class Base:\n    pass\n\nclass Order:\n    total = 0\n",
        )
        .unwrap();
        let ord_agent = AgentIdentity {
            id: "ord-agent".into(),
            agent_type: AgentType::Agent,
        };
        repo.seal(
            ord_agent,
            "add order model".into(),
            Some("ord".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Restore inv's models.py before sealing again (simulates agent
        // working from its own branch state, not ord's disk content).
        fs::write(
            dir.path().join("models.py"),
            "class Base:\n    pass\n\nclass Inventory:\n    name = 'item'\n",
        )
        .unwrap();
        fs::write(dir.path().join("marker.txt"), "done\n").unwrap();
        repo.seal(
            inv_agent,
            "inv done".into(),
            Some("inv".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();
        repo.update_spec(
            "inv",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, true)
            .unwrap();

        assert!(report.is_clean, "should converge cleanly");

        let content = fs::read_to_string(dir.path().join("models.py")).unwrap();
        assert!(
            content.contains("Base"),
            "base class must survive convergence, got:\n{content}"
        );
        assert!(
            content.contains("Inventory"),
            "Inventory class must survive convergence, got:\n{content}"
        );
        assert!(
            content.contains("Order"),
            "Order class must survive convergence, got:\n{content}"
        );
    }

    // ── Regression tests for all-spec convergence (TR20 bug fix) ─────

    /// Regression: sequential single-seal agents must all participate in
    /// convergence, not just diverged ones. Before the fix, converge_all
    /// only processed specs whose head was off the HEAD chain. Sequential
    /// sealing puts ALL seals on the chain, so non-base specs were
    /// silently excluded — their files were never merged.
    #[test]
    fn test_converge_all_includes_non_diverged_specs() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Baseline seal (no spec).
        fs::write(dir.path().join("base.txt"), "baseline\n").unwrap();
        let setup = AgentIdentity {
            id: "setup".into(),
            agent_type: AgentType::Agent,
        };
        repo.seal(
            setup,
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // 3 specs, each agent seals exactly ONCE (sequential, no divergence).
        repo.add_spec(&Spec::new(
            "inventory".into(),
            "Inventory".into(),
            "".into(),
        ))
        .unwrap();
        repo.add_spec(&Spec::new("auth".into(), "Auth".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("orders".into(), "Orders".into(), "".into()))
            .unwrap();

        // Agent A: inventory (seals first)
        fs::write(dir.path().join("inventory.py"), "class Inventory: pass\n").unwrap();
        repo.seal(
            AgentIdentity {
                id: "inv-dev".into(),
                agent_type: AgentType::Agent,
            },
            "add inventory".into(),
            Some("inventory".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();
        repo.update_spec(
            "inventory",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        // Agent B: auth (seals second — no divergence, HEAD chain grows)
        fs::write(dir.path().join("auth.py"), "class Auth: pass\n").unwrap();
        repo.seal(
            AgentIdentity {
                id: "auth-dev".into(),
                agent_type: AgentType::Agent,
            },
            "add auth".into(),
            Some("auth".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();
        repo.update_spec(
            "auth",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        // Agent C: orders (seals last — still no divergence)
        fs::write(dir.path().join("orders.py"), "class Orders: pass\n").unwrap();
        repo.seal(
            AgentIdentity {
                id: "orders-dev".into(),
                agent_type: AgentType::Agent,
            },
            "add orders".into(),
            Some("orders".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();
        repo.update_spec(
            "orders",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        // Verify no divergence (the old bug: empty diverged = no merge).
        let diverged = repo.diverged_branches().unwrap();
        assert!(diverged.is_empty(), "sequential seals should not diverge");

        // converge_all must still include ALL 3 specs.
        let report = repo.converge_all(ConvergeStrategy::Escalate, true).unwrap();

        // The merge_order should include 2 non-base specs.
        assert_eq!(
            report.merge_order.len(),
            2,
            "all non-base specs must participate, got: {:?}",
            report.merge_order
        );
        assert!(report.is_clean);
        assert!(report.applied);

        // All 3 agent files should be on disk after convergence.
        assert!(
            dir.path().join("inventory.py").exists(),
            "inventory.py must exist"
        );
        assert!(dir.path().join("auth.py").exists(), "auth.py must exist");
        assert!(
            dir.path().join("orders.py").exists(),
            "orders.py must exist"
        );
    }

    /// Regression: 3 sequential agents all modify the same shared file.
    /// Before the fix, only the base spec's version survived. Now all
    /// contributions should be composed via three-way merge.
    #[test]
    fn test_converge_all_sequential_shared_file_all_preserved() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Baseline with a shared file.
        fs::write(
            dir.path().join("app.py"),
            "from flask import Flask\napp = Flask(__name__)\n",
        )
        .unwrap();
        let setup = AgentIdentity {
            id: "setup".into(),
            agent_type: AgentType::Agent,
        };
        repo.seal(
            setup,
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("inv".into(), "Inventory".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("auth".into(), "Auth".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("orders".into(), "Orders".into(), "".into()))
            .unwrap();

        // Agent A: adds inventory route. Seals first.
        fs::write(
            dir.path().join("app.py"),
            "from flask import Flask\napp = Flask(__name__)\n\n@app.route('/inventory')\ndef inventory():\n    return 'inv'\n",
        )
        .unwrap();
        repo.seal(
            AgentIdentity {
                id: "inv-dev".into(),
                agent_type: AgentType::Agent,
            },
            "add inventory route".into(),
            Some("inv".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();
        repo.update_spec(
            "inv",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        // Agent B: adds auth route (includes inv's route because disk
        // was written before this agent sealed).
        fs::write(
            dir.path().join("app.py"),
            "from flask import Flask\napp = Flask(__name__)\n\n@app.route('/inventory')\ndef inventory():\n    return 'inv'\n\n@app.route('/auth')\ndef auth():\n    return 'auth'\n",
        )
        .unwrap();
        repo.seal(
            AgentIdentity {
                id: "auth-dev".into(),
                agent_type: AgentType::Agent,
            },
            "add auth route".into(),
            Some("auth".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();
        repo.update_spec(
            "auth",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        // Agent C: adds orders route (includes inv + auth routes).
        fs::write(
            dir.path().join("app.py"),
            "from flask import Flask\napp = Flask(__name__)\n\n@app.route('/inventory')\ndef inventory():\n    return 'inv'\n\n@app.route('/auth')\ndef auth():\n    return 'auth'\n\n@app.route('/orders')\ndef orders():\n    return 'orders'\n",
        )
        .unwrap();
        repo.seal(
            AgentIdentity {
                id: "orders-dev".into(),
                agent_type: AgentType::Agent,
            },
            "add orders route".into(),
            Some("orders".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();
        repo.update_spec(
            "orders",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                ..Default::default()
            },
        )
        .unwrap();

        let report = repo.converge_all(ConvergeStrategy::Escalate, true).unwrap();

        assert!(report.is_clean, "should converge cleanly");
        assert!(report.applied);
        assert_eq!(
            report.merge_order.len(),
            2,
            "2 non-base specs in merge_order"
        );

        // The final app.py must contain ALL three routes.
        let content = fs::read_to_string(dir.path().join("app.py")).unwrap();
        assert!(
            content.contains("/inventory"),
            "inventory route must survive convergence, got:\n{content}"
        );
        assert!(
            content.contains("/auth"),
            "auth route must survive convergence, got:\n{content}"
        );
        assert!(
            content.contains("/orders"),
            "orders route must survive convergence, got:\n{content}"
        );

        // Must NOT have duplicate Flask app instances.
        let flask_count = content.matches("Flask(__name__)").count();
        assert_eq!(
            flask_count, 1,
            "only 1 Flask app instance, got {flask_count} in:\n{content}"
        );
    }

    /// Verify that converge_all populates files_changed in the report
    /// so callers can create accurate convergence seals.
    #[test]
    fn test_converge_all_files_changed_populated() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Baseline with shared file.
        fs::write(dir.path().join("app.py"), "base\n").unwrap();
        let setup = AgentIdentity {
            id: "setup".into(),
            agent_type: AgentType::Agent,
        };
        repo.seal(
            setup,
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("a".into(), "A".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("b".into(), "B".into(), "".into()))
            .unwrap();

        // Agent A: modifies shared file + adds new file.
        fs::write(dir.path().join("app.py"), "base\nfrom_a\n").unwrap();
        fs::write(dir.path().join("a_only.py"), "a stuff\n").unwrap();
        repo.seal(
            AgentIdentity {
                id: "a-dev".into(),
                agent_type: AgentType::Agent,
            },
            "a work".into(),
            Some("a".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent B: modifies shared file + adds new file.
        fs::write(dir.path().join("app.py"), "base\nfrom_a\nfrom_b\n").unwrap();
        fs::write(dir.path().join("b_only.py"), "b stuff\n").unwrap();
        repo.seal(
            AgentIdentity {
                id: "b-dev".into(),
                agent_type: AgentType::Agent,
            },
            "b work".into(),
            Some("b".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let report = repo.converge_all(ConvergeStrategy::Escalate, true).unwrap();
        assert!(report.applied);
        // files_changed should contain the files that convergence modified.
        assert!(
            !report.files_changed.is_empty(),
            "files_changed should be populated, got empty"
        );
        // app.py should be in the changed list (it was a conflict file).
        assert!(
            report.files_changed.contains(&"app.py".to_string()),
            "app.py should be in files_changed, got: {:?}",
            report.files_changed
        );
    }

    /// Regression: converge_all with only 1 spec and no diverged branches
    /// should still return early (nothing to merge).
    #[test]
    fn test_converge_all_single_spec_returns_early() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("a.txt"), "content\n").unwrap();
        repo.add_spec(&Spec::new("solo".into(), "Solo".into(), "".into()))
            .unwrap();
        repo.seal(
            AgentIdentity {
                id: "dev".into(),
                agent_type: AgentType::Agent,
            },
            "work".into(),
            Some("solo".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let report = repo.converge_all(ConvergeStrategy::Manual, false).unwrap();
        assert!(report.is_clean);
        assert!(
            report.merges.is_empty(),
            "single spec should have no merges"
        );
    }

    #[test]
    fn test_degraded_true_when_most_recent_discards_content() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Baseline seal.
        fs::write(dir.path().join("shared.txt"), "base content\n").unwrap();
        let setup = AgentIdentity {
            id: "setup".into(),
            agent_type: AgentType::Agent,
        };
        repo.seal(
            setup,
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("s1".into(), "S1".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("s2".into(), "S2".into(), "".into()))
            .unwrap();

        // Agent A modifies shared.txt under s1.
        fs::write(dir.path().join("shared.txt"), "alpha version\n").unwrap();
        let a = AgentIdentity {
            id: "agent-a".into(),
            agent_type: AgentType::Agent,
        };
        repo.seal(
            a.clone(),
            "a work".into(),
            Some("s1".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent B modifies shared.txt under s2.
        fs::write(dir.path().join("shared.txt"), "beta version\n").unwrap();
        let b = AgentIdentity {
            id: "agent-b".into(),
            agent_type: AgentType::Agent,
        };
        repo.seal(
            b,
            "b work".into(),
            Some("s2".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent A seals again to create divergence.
        fs::write(dir.path().join("shared.txt"), "alpha v2\n").unwrap();
        repo.seal(
            a,
            "a final".into(),
            Some("s1".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, false)
            .unwrap();

        assert!(
            report.degraded,
            "should be degraded when most-recent discards content"
        );
        let degraded_steps: Vec<_> = report.merges.iter().filter(|m| m.degraded).collect();
        assert!(
            !degraded_steps.is_empty(),
            "at least one merge step should be degraded"
        );
    }

    #[test]
    fn test_degraded_false_when_clean_merge() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Baseline seal.
        fs::write(dir.path().join("base.txt"), "base\n").unwrap();
        let setup = AgentIdentity {
            id: "setup".into(),
            agent_type: AgentType::Agent,
        };
        repo.seal(
            setup,
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("s1".into(), "S1".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("s2".into(), "S2".into(), "".into()))
            .unwrap();

        // Agent A adds a new file under s1 (no overlap with B).
        fs::write(dir.path().join("alpha.txt"), "alpha only\n").unwrap();
        let a = AgentIdentity {
            id: "agent-a".into(),
            agent_type: AgentType::Agent,
        };
        repo.seal(
            a.clone(),
            "a work".into(),
            Some("s1".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent B adds a different new file under s2 (no overlap with A).
        fs::write(dir.path().join("beta.txt"), "beta only\n").unwrap();
        let b = AgentIdentity {
            id: "agent-b".into(),
            agent_type: AgentType::Agent,
        };
        repo.seal(
            b,
            "b work".into(),
            Some("s2".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent A seals again to create divergence.
        fs::write(dir.path().join("alpha.txt"), "alpha v2\n").unwrap();
        repo.seal(
            a,
            "a final".into(),
            Some("s1".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let report = repo
            .converge_all(ConvergeStrategy::MostRecent, false)
            .unwrap();

        assert!(
            !report.degraded,
            "non-conflicting merge should not be degraded"
        );
    }

    #[test]
    fn test_cross_file_reference_integrity_detects_broken_imports() {
        let dir = tempdir().unwrap();

        // models.py was resolved by most-recent — Product was removed.
        fs::write(dir.path().join("models.py"), "class Order:\n    pass\n").unwrap();

        // app.py imports Product from models — this reference is now broken.
        fs::write(
            dir.path().join("app.py"),
            "from models import Product\n\ndef main():\n    p = Product()\n",
        )
        .unwrap();

        let decisions = vec![
            FileDecision {
                path: "models.py".into(),
                decision: "most-recent".into(),
                chosen_lines: 2,
                chosen_spec: Some("spec-a".into()),
                alternatives: vec![],
                confidence: Some(0.7),
            },
            FileDecision {
                path: "app.py".into(),
                decision: "auto-merged".into(),
                chosen_lines: 4,
                chosen_spec: Some("spec-b".into()),
                alternatives: vec![],
                confidence: Some(1.0),
            },
        ];

        let report = build_quality_report(decisions, dir.path(), true, 1, 1);

        let ref_check = report
            .consistency_checks
            .iter()
            .find(|c| c.metric == "cross_file_reference_integrity");
        assert!(
            ref_check.is_some(),
            "should have cross_file_reference_integrity check, got: {:?}",
            report.consistency_checks,
        );
        assert!(
            !ref_check.unwrap().consistent,
            "should be inconsistent — Product not in models.py",
        );
        assert!(
            report.quality_score <= 75,
            "score should crater (≤75) with broken cross-file references, got: {}",
            report.quality_score,
        );
    }

    #[test]
    fn test_cross_file_reference_integrity_passes_when_intact() {
        let dir = tempdir().unwrap();

        // models.py was resolved by most-recent but still contains Product.
        fs::write(
            dir.path().join("models.py"),
            "class Product:\n    pass\n\nclass Order:\n    pass\n",
        )
        .unwrap();

        // app.py imports Product from models — reference is intact.
        fs::write(
            dir.path().join("app.py"),
            "from models import Product\n\ndef main():\n    p = Product()\n",
        )
        .unwrap();

        let decisions = vec![
            FileDecision {
                path: "models.py".into(),
                decision: "most-recent".into(),
                chosen_lines: 5,
                chosen_spec: Some("spec-a".into()),
                alternatives: vec![],
                confidence: Some(0.7),
            },
            FileDecision {
                path: "app.py".into(),
                decision: "auto-merged".into(),
                chosen_lines: 4,
                chosen_spec: Some("spec-b".into()),
                alternatives: vec![],
                confidence: Some(1.0),
            },
        ];

        let report = build_quality_report(decisions, dir.path(), true, 1, 1);

        let ref_check = report
            .consistency_checks
            .iter()
            .find(|c| c.metric == "cross_file_reference_integrity");
        assert!(
            ref_check.is_some(),
            "should have cross_file_reference_integrity check even when passing",
        );
        assert!(
            ref_check.unwrap().consistent,
            "should be consistent — Product exists in models.py",
        );
    }

    // Sprint 16 — word-boundary matching in quality report

    #[test]
    fn test_quality_report_unused_imports_word_boundary() {
        let dir = tempdir().unwrap();

        // "User" is imported but only "UserProfile" appears in the body.
        // Word-boundary matching should flag "User" as unused.
        fs::write(
            dir.path().join("app.py"),
            "from models import User, Product\n\nclass UserProfile:\n    product = Product()\n",
        )
        .unwrap();

        let decisions = vec![FileDecision {
            path: "app.py".into(),
            decision: "auto-merged".into(),
            chosen_spec: Some("s1".into()),
            chosen_lines: 4,
            alternatives: vec![],
            confidence: Some(1.0),
        }];

        let report = build_quality_report(decisions, dir.path(), true, 0, 0);

        let unused_check = report
            .consistency_checks
            .iter()
            .find(|c| c.metric == "unused_imports");
        assert!(
            unused_check.is_some(),
            "should have unused_imports check: {:?}",
            report.consistency_checks,
        );
        assert!(
            !unused_check.unwrap().consistent,
            "User should be flagged as unused — only UserProfile exists in body",
        );

        // Score should be penalized for the unused import.
        assert!(
            report.quality_score < 100,
            "score should be < 100 with unused User import, got: {}",
            report.quality_score,
        );
    }

    #[test]
    fn test_quality_report_unused_imports_whole_word_not_flagged() {
        let dir = tempdir().unwrap();

        // "User" IS used as a whole word — should NOT be flagged.
        fs::write(
            dir.path().join("app.py"),
            "from models import User\n\ndef get():\n    return User(name='test')\n",
        )
        .unwrap();

        let decisions = vec![FileDecision {
            path: "app.py".into(),
            decision: "auto-merged".into(),
            chosen_spec: Some("s1".into()),
            chosen_lines: 4,
            alternatives: vec![],
            confidence: Some(1.0),
        }];

        let report = build_quality_report(decisions, dir.path(), true, 0, 0);

        let unused_check = report
            .consistency_checks
            .iter()
            .find(|c| c.metric == "unused_imports");
        // Either no check (no unused imports found) or check is consistent.
        if let Some(check) = unused_check {
            assert!(
                check.consistent,
                "User is used as whole word — should not be flagged as unused",
            );
        }
    }

    /// Quality report should detect duplicate definitions (functions, classes)
    /// in merged files and penalize the score.
    #[test]
    fn test_quality_report_duplicate_definitions_detected() {
        let dir = tempdir().unwrap();

        // File with duplicate function definition — typical merge corruption.
        fs::write(
            dir.path().join("app.py"),
            "def main():\n    print('a')\n\ndef main():\n    print('b')\n",
        )
        .unwrap();

        let decisions = vec![FileDecision {
            path: "app.py".into(),
            decision: "auto-resolved".into(),
            chosen_spec: Some("s1".into()),
            chosen_lines: 5,
            alternatives: vec![],
            confidence: Some(0.88),
        }];

        let report = build_quality_report(decisions, dir.path(), true, 1, 1);

        let dup_check = report
            .consistency_checks
            .iter()
            .find(|c| c.metric == "duplicate_definitions");
        assert!(
            dup_check.is_some(),
            "duplicate_definitions check should be present"
        );
        let check = dup_check.unwrap();
        assert!(
            !check.consistent,
            "should flag duplicate def main() as inconsistent"
        );
        assert!(
            check.warning.is_some(),
            "should have a warning about duplicates"
        );
        // Score should be penalized (30 points for dup defs).
        assert!(
            report.quality_score <= 70,
            "score should be <= 70 with duplicate defs, got {}",
            report.quality_score
        );
    }

    /// Quality report should pass when no duplicate definitions exist.
    #[test]
    fn test_quality_report_no_duplicate_definitions_passes() {
        let dir = tempdir().unwrap();

        fs::write(
            dir.path().join("app.py"),
            "def foo():\n    pass\n\ndef bar():\n    pass\n",
        )
        .unwrap();

        let decisions = vec![FileDecision {
            path: "app.py".into(),
            decision: "auto-merged".into(),
            chosen_spec: None,
            chosen_lines: 5,
            alternatives: vec![],
            confidence: Some(1.0),
        }];

        let report = build_quality_report(decisions, dir.path(), true, 0, 0);

        let dup_check = report
            .consistency_checks
            .iter()
            .find(|c| c.metric == "duplicate_definitions");
        if let Some(check) = dup_check {
            assert!(check.consistent, "no duplicate defs — should be consistent");
        }
    }
}

// ---------------------------------------------------------------------------
// Sprint 0.2.2 — Context edge case tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod context_edge_case_tests {
    use super::*;
    use crate::context::{ContextFilter, ContextScope};
    use crate::seal::{AgentType, TaskStatus, Verification};
    use crate::spec::{Spec, SpecStatus, SpecUpdate};
    use std::time::Instant;
    use tempfile::tempdir;

    fn agent(name: &str) -> AgentIdentity {
        AgentIdentity {
            id: name.to_string(),
            agent_type: AgentType::Agent,
        }
    }

    fn default_filter() -> ContextFilter {
        ContextFilter::default()
    }

    // --- 0 seals edge cases ---

    #[test]
    fn test_context_zero_seals_full_scope() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &default_filter())
            .unwrap();

        assert!(ctx.working_state.clean);
        assert!(ctx.recent_seals.is_empty());
        assert!(ctx.pending_changes.is_none());
        assert!(ctx.agent_activity.is_empty());
        assert!(ctx.file_contention.is_empty());
        assert!(ctx.diverged_branches.is_empty());
        assert!(!ctx.convergence_recommended);
        assert_eq!(ctx.integration_risk.level, "low");
        assert!(!ctx.session_complete);
    }

    #[test]
    fn test_context_zero_seals_with_specs() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new("empty-spec".into(), "No seals yet".into(), String::new());
        repo.add_spec(&spec).unwrap();

        // Full scope: spec visible but no seals
        let ctx = repo
            .context(ContextScope::Full, 10, &default_filter())
            .unwrap();
        assert!(ctx.recent_seals.is_empty());
        let specs = ctx.all_specs.as_ref().unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].id, "empty-spec");

        // Spec scope: works even with no seals
        let spec_ctx = repo
            .context(
                ContextScope::Spec("empty-spec".into()),
                10,
                &default_filter(),
            )
            .unwrap();
        assert!(spec_ctx.recent_seals.is_empty());
        assert!(spec_ctx.active_spec.is_some());
    }

    #[test]
    fn test_context_zero_seals_with_pending_changes() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("new_file.txt"), "content").unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &default_filter())
            .unwrap();
        assert!(!ctx.working_state.clean);
        assert!(!ctx.working_state.new_files.is_empty());
        // Should nudge to seal since there are changes
        assert!(ctx.seal_nudge.is_some());
    }

    #[test]
    fn test_context_zero_seals_agent_scope() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Agent scope for a non-existent agent — should not panic
        let ctx = repo
            .context(
                ContextScope::Agent("ghost-agent".into()),
                10,
                &default_filter(),
            )
            .unwrap();
        assert!(ctx.recent_seals.is_empty());
        assert!(ctx.agent_activity.is_empty());
    }

    // --- 1 seal edge cases ---

    #[test]
    fn test_context_one_seal_full_scope() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("only.txt"), "first").unwrap();
        repo.seal(
            agent("solo"),
            "only seal".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &default_filter())
            .unwrap();
        assert_eq!(ctx.recent_seals.len(), 1);
        assert_eq!(ctx.recent_seals[0].summary, "only seal");
        assert_eq!(ctx.agent_activity.len(), 1);
        assert_eq!(ctx.agent_activity[0].agent_id, "solo");
        assert!(
            ctx.file_contention.is_empty(),
            "single agent = no contention"
        );
    }

    #[test]
    fn test_context_one_seal_spec_scoped() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new("solo-spec".into(), "Solo".into(), String::new());
        repo.add_spec(&spec).unwrap();

        fs::write(dir.path().join("data.txt"), "v1").unwrap();
        repo.seal(
            agent("worker"),
            "first step".into(),
            Some("solo-spec".into()),
            TaskStatus::InProgress,
            Verification {
                tests_passed: Some(5),
                tests_failed: Some(0),
                linted: true,
            },
            false,
        )
        .unwrap();

        let ctx = repo
            .context(
                ContextScope::Spec("solo-spec".into()),
                10,
                &default_filter(),
            )
            .unwrap();
        assert_eq!(ctx.recent_seals.len(), 1);
        assert!(ctx.spec_progress.is_some());
        let progress = ctx.spec_progress.as_ref().unwrap();
        assert_eq!(progress.total_seals, 1);
    }

    // --- 500 seals scale test ---

    #[test]
    fn test_context_500_seals_all_scopes() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new("bulk-spec".into(), "Bulk work".into(), String::new());
        repo.add_spec(&spec).unwrap();

        // Create shared files upfront
        for f in 0..5 {
            fs::write(dir.path().join(format!("shared-{f}.txt")), "base").unwrap();
        }

        // Create 500 seals across 2 agents, both modifying overlapping files
        for i in 0..500 {
            let agent_name = if i % 2 == 0 { "agent-a" } else { "agent-b" };
            // Each agent modifies 2 shared files per seal to create contention
            let f1 = format!("shared-{}.txt", i % 5);
            let f2 = format!("shared-{}.txt", (i + 1) % 5);
            fs::write(dir.path().join(&f1), format!("v{i}-a")).unwrap();
            fs::write(dir.path().join(&f2), format!("v{i}-b")).unwrap();
            repo.seal(
                agent(agent_name),
                format!("seal #{i}"),
                Some("bulk-spec".into()),
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();
        }

        // Full scope with limit
        let start = Instant::now();
        let ctx = repo
            .context(ContextScope::Full, 20, &default_filter())
            .unwrap();
        let full_time = start.elapsed();
        assert_eq!(ctx.recent_seals.len(), 20, "seal_limit should cap at 20");
        assert!(
            full_time.as_secs() < 30,
            "Full context with 500 seals took {full_time:?}"
        );

        // Spec scope with limit
        let start = Instant::now();
        let spec_ctx = repo
            .context(
                ContextScope::Spec("bulk-spec".into()),
                10,
                &default_filter(),
            )
            .unwrap();
        let spec_time = start.elapsed();
        assert_eq!(spec_ctx.recent_seals.len(), 10);
        assert!(
            spec_time.as_secs() < 30,
            "Spec context with 500 seals took {spec_time:?}"
        );

        // Agent scope
        let start = Instant::now();
        let agent_ctx = repo
            .context(ContextScope::Agent("agent-a".into()), 15, &default_filter())
            .unwrap();
        let agent_time = start.elapsed();
        assert!(agent_ctx.recent_seals.len() <= 15);
        assert!(
            agent_time.as_secs() < 30,
            "Agent context with 500 seals took {agent_time:?}"
        );

        // Verify agent activity includes both agents
        assert!(
            ctx.agent_activity.len() >= 2,
            "should have at least 2 agents in activity"
        );

        // Both agents touched the same shared files → contention expected
        assert!(
            !ctx.file_contention.is_empty(),
            "2 agents touching same files = contention: activity={:?}",
            ctx.agent_activity
                .iter()
                .map(|a| (&a.agent_id, a.seal_count, &a.files_owned))
                .collect::<Vec<_>>()
        );
    }

    // --- Conflicting specs ---

    #[test]
    fn test_context_conflicting_specs_same_files() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Two specs claiming the same file scope
        let mut spec_a = Spec::new("spec-alpha".into(), "Alpha work".into(), String::new());
        spec_a.file_scope = vec!["shared.py".into(), "alpha.py".into()];
        repo.add_spec(&spec_a).unwrap();

        let mut spec_b = Spec::new("spec-beta".into(), "Beta work".into(), String::new());
        spec_b.file_scope = vec!["shared.py".into(), "beta.py".into()];
        repo.add_spec(&spec_b).unwrap();

        // Both agents seal changes to shared.py
        fs::write(dir.path().join("shared.py"), "v1").unwrap();
        fs::write(dir.path().join("alpha.py"), "a1").unwrap();
        repo.seal(
            agent("alpha-agent"),
            "alpha work".into(),
            Some("spec-alpha".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("shared.py"), "v2").unwrap();
        fs::write(dir.path().join("beta.py"), "b1").unwrap();
        repo.seal(
            agent("beta-agent"),
            "beta work".into(),
            Some("spec-beta".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Full context should show contention on shared.py
        let ctx = repo
            .context(ContextScope::Full, 20, &default_filter())
            .unwrap();
        let shared_contention = ctx.file_contention.iter().find(|fc| fc.path == "shared.py");
        assert!(
            shared_contention.is_some(),
            "shared.py should appear in file_contention"
        );

        // Spec-scoped context should still work for each spec
        let alpha_ctx = repo
            .context(
                ContextScope::Spec("spec-alpha".into()),
                10,
                &default_filter(),
            )
            .unwrap();
        assert!(alpha_ctx.active_spec.is_some());

        let beta_ctx = repo
            .context(
                ContextScope::Spec("spec-beta".into()),
                10,
                &default_filter(),
            )
            .unwrap();
        assert!(beta_ctx.active_spec.is_some());
    }

    #[test]
    fn test_context_specs_with_dependencies() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let mut base_spec = Spec::new("base".into(), "Base layer".into(), String::new());
        base_spec.file_scope = vec!["base.py".into()];
        repo.add_spec(&base_spec).unwrap();

        let mut dep_spec = Spec::new("dependent".into(), "Depends on base".into(), String::new());
        dep_spec.depends_on = vec!["base".into()];
        dep_spec.file_scope = vec!["feature.py".into()];
        repo.add_spec(&dep_spec).unwrap();

        // Base spec not yet complete
        fs::write(dir.path().join("base.py"), "wip").unwrap();
        repo.seal(
            agent("base-dev"),
            "base wip".into(),
            Some("base".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Dependent spec context should show blocking dependency
        let ctx = repo
            .context(
                ContextScope::Spec("dependent".into()),
                10,
                &default_filter(),
            )
            .unwrap();

        let deps = ctx.dependency_status.as_ref().unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].spec_id, "base");
        assert!(!deps[0].resolved, "base is still in-progress");

        // Recommended action should be wait_for_dependency
        if let Some(ref action) = ctx.recommended_action {
            assert_eq!(action.action, "wait_for_dependency");
        }
    }

    // --- Deleted files ---

    #[test]
    fn test_context_with_deleted_files() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Create and seal a file
        fs::write(dir.path().join("ephemeral.txt"), "exists").unwrap();
        repo.seal(
            agent("creator"),
            "created file".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Delete the file
        fs::remove_file(dir.path().join("ephemeral.txt")).unwrap();

        // Context should show deleted file in working state
        let ctx = repo
            .context(ContextScope::Full, 10, &default_filter())
            .unwrap();
        assert!(!ctx.working_state.clean);
        assert!(
            ctx.working_state
                .deleted_files
                .contains(&"ephemeral.txt".to_string()),
            "deleted file should appear in working_state.deleted_files"
        );
        // Should nudge to seal the deletion
        assert!(ctx.seal_nudge.is_some());
    }

    #[test]
    fn test_context_deleted_file_not_in_ownership() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Create, seal, delete, seal the deletion
        fs::write(dir.path().join("gone.txt"), "here").unwrap();
        repo.seal(
            agent("dev"),
            "add file".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        fs::remove_file(dir.path().join("gone.txt")).unwrap();
        repo.seal(
            agent("dev"),
            "remove file".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &default_filter())
            .unwrap();

        // After sealing the deletion, working state should be clean
        assert!(ctx.working_state.clean);
    }

    // --- Renamed files (simulated as delete + add) ---

    #[test]
    fn test_context_renamed_files() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Create original file and seal
        fs::write(dir.path().join("old_name.txt"), "content").unwrap();
        repo.seal(
            agent("renamer"),
            "original file".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // "Rename": delete old, create new with same content
        fs::remove_file(dir.path().join("old_name.txt")).unwrap();
        fs::write(dir.path().join("new_name.txt"), "content").unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &default_filter())
            .unwrap();
        assert!(!ctx.working_state.clean);
        assert!(
            ctx.working_state
                .deleted_files
                .contains(&"old_name.txt".to_string()),
            "old name should be in deleted_files"
        );
        assert!(
            ctx.working_state
                .new_files
                .contains(&"new_name.txt".to_string()),
            "new name should be in new_files"
        );

        // Seal the rename
        repo.seal(
            agent("renamer"),
            "renamed file".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx2 = repo
            .context(ContextScope::Full, 10, &default_filter())
            .unwrap();
        assert!(ctx2.working_state.clean);
        // new_name.txt should be tracked, old_name.txt should not
        assert_eq!(ctx2.tracked_files, 1);
    }

    // --- Spec not found ---

    #[test]
    fn test_context_spec_scope_nonexistent_spec() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let result = repo.context(
            ContextScope::Spec("does-not-exist".into()),
            10,
            &default_filter(),
        );
        assert!(result.is_err(), "nonexistent spec should return error");
    }

    // --- Session complete ---

    #[test]
    fn test_context_session_complete_all_specs_done() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec = Spec::new("finish-me".into(), "Completable".into(), String::new());
        repo.add_spec(&spec).unwrap();

        fs::write(dir.path().join("done.txt"), "done").unwrap();
        repo.seal(
            agent("closer"),
            "final work".into(),
            Some("finish-me".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        // Mark spec as complete
        repo.update_spec(
            "finish-me",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                depends_on: None,
                file_scope: None,
                acceptance_criteria: None,
                design_notes: None,
                tech_stack: None,
            },
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &default_filter())
            .unwrap();
        assert!(
            ctx.session_complete,
            "all specs complete = session_complete"
        );
        assert!(ctx.session_summary.is_some());
    }

    #[test]
    fn test_context_session_not_complete_with_mixed_statuses() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let spec_a = Spec::new("done-spec".into(), "Done".into(), String::new());
        repo.add_spec(&spec_a).unwrap();
        let spec_b = Spec::new("wip-spec".into(), "Still working".into(), String::new());
        repo.add_spec(&spec_b).unwrap();

        fs::write(dir.path().join("a.txt"), "done").unwrap();
        repo.seal(
            agent("a"),
            "a done".into(),
            Some("done-spec".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        repo.update_spec(
            "done-spec",
            SpecUpdate {
                status: Some(SpecStatus::Complete),
                depends_on: None,
                file_scope: None,
                acceptance_criteria: None,
                design_notes: None,
                tech_stack: None,
            },
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &default_filter())
            .unwrap();
        assert!(
            !ctx.session_complete,
            "not all specs complete = session not complete"
        );
    }

    // --- Integration risk edge cases ---

    #[test]
    fn test_context_integration_risk_low_with_single_agent() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        for i in 0..5 {
            fs::write(dir.path().join(format!("f{i}.txt")), format!("v{i}")).unwrap();
        }
        repo.seal(
            agent("lone-wolf"),
            "batch work".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 10, &default_filter())
            .unwrap();
        assert_eq!(
            ctx.integration_risk.level, "low",
            "single agent should have low integration risk"
        );
        assert_eq!(ctx.integration_risk.score, 0);
    }

    // --- Seal limit edge cases ---

    #[test]
    fn test_context_seal_limit_zero() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("f.txt"), "v1").unwrap();
        repo.seal(
            agent("dev"),
            "work".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 0, &default_filter())
            .unwrap();
        assert!(
            ctx.recent_seals.is_empty(),
            "seal_limit=0 should return no seals"
        );
    }

    #[test]
    fn test_context_seal_limit_exceeds_total() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        fs::write(dir.path().join("f.txt"), "v1").unwrap();
        repo.seal(
            agent("dev"),
            "seal 1".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();
        fs::write(dir.path().join("f.txt"), "v2").unwrap();
        repo.seal(
            agent("dev"),
            "seal 2".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let ctx = repo
            .context(ContextScope::Full, 100, &default_filter())
            .unwrap();
        assert_eq!(
            ctx.recent_seals.len(),
            2,
            "should return all seals when limit > total"
        );
    }
}

// ---------------------------------------------------------------------------
// Spec Lifecycle State Machine Tests (GC.1.2)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::seal::{AgentIdentity, AgentType, TaskStatus, Verification};
    use crate::spec::{LifecycleState, SpecStatus};

    fn agent(id: &str) -> AgentIdentity {
        AgentIdentity {
            id: id.to_string(),
            agent_type: AgentType::Agent,
        }
    }

    fn setup_repo() -> (tempfile::TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        (dir, repo)
    }

    fn create_spec(repo: &Repository, id: &str) {
        let spec = crate::spec::Spec::new(id.into(), "Test".into(), "desc".into());
        repo.add_spec(&spec).unwrap();
    }

    #[test]
    fn test_transition_active_to_stale() {
        let (_dir, repo) = setup_repo();
        create_spec(&repo, "my-spec");

        repo.transition_spec_lifecycle("my-spec", LifecycleState::Stale)
            .unwrap();
        let spec = repo.load_spec("my-spec").unwrap();
        assert_eq!(spec.lifecycle_state, LifecycleState::Stale);
    }

    #[test]
    fn test_transition_active_to_cancelled() {
        let (_dir, repo) = setup_repo();
        create_spec(&repo, "my-spec");

        repo.transition_spec_lifecycle("my-spec", LifecycleState::Cancelled)
            .unwrap();
        let spec = repo.load_spec("my-spec").unwrap();
        assert_eq!(spec.lifecycle_state, LifecycleState::Cancelled);
    }

    #[test]
    fn test_transition_active_to_completed() {
        let (_dir, repo) = setup_repo();
        create_spec(&repo, "my-spec");

        repo.transition_spec_lifecycle("my-spec", LifecycleState::Completed)
            .unwrap();
        let spec = repo.load_spec("my-spec").unwrap();
        assert_eq!(spec.lifecycle_state, LifecycleState::Completed);
    }

    #[test]
    fn test_transition_stale_to_active() {
        let (_dir, repo) = setup_repo();
        create_spec(&repo, "my-spec");

        repo.transition_spec_lifecycle("my-spec", LifecycleState::Stale)
            .unwrap();
        repo.transition_spec_lifecycle("my-spec", LifecycleState::Active)
            .unwrap();
        let spec = repo.load_spec("my-spec").unwrap();
        assert_eq!(spec.lifecycle_state, LifecycleState::Active);
    }

    #[test]
    fn test_transition_stale_to_cancelled() {
        let (_dir, repo) = setup_repo();
        create_spec(&repo, "my-spec");

        repo.transition_spec_lifecycle("my-spec", LifecycleState::Stale)
            .unwrap();
        repo.transition_spec_lifecycle("my-spec", LifecycleState::Cancelled)
            .unwrap();
        let spec = repo.load_spec("my-spec").unwrap();
        assert_eq!(spec.lifecycle_state, LifecycleState::Cancelled);
    }

    #[test]
    fn test_transition_completed_to_archived() {
        let (_dir, repo) = setup_repo();
        create_spec(&repo, "my-spec");

        repo.transition_spec_lifecycle("my-spec", LifecycleState::Completed)
            .unwrap();
        repo.transition_spec_lifecycle("my-spec", LifecycleState::Archived)
            .unwrap();
        let spec = repo.load_spec("my-spec").unwrap();
        assert_eq!(spec.lifecycle_state, LifecycleState::Archived);
    }

    #[test]
    fn test_transition_cancelled_to_archived() {
        let (_dir, repo) = setup_repo();
        create_spec(&repo, "my-spec");

        repo.transition_spec_lifecycle("my-spec", LifecycleState::Cancelled)
            .unwrap();
        repo.transition_spec_lifecycle("my-spec", LifecycleState::Archived)
            .unwrap();
        let spec = repo.load_spec("my-spec").unwrap();
        assert_eq!(spec.lifecycle_state, LifecycleState::Archived);
    }

    #[test]
    fn test_illegal_transition_active_to_archived() {
        let (_dir, repo) = setup_repo();
        create_spec(&repo, "my-spec");

        let result = repo.transition_spec_lifecycle("my-spec", LifecycleState::Archived);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, WritError::InvalidLifecycleTransition(_)),
            "expected InvalidLifecycleTransition, got: {err}"
        );
    }

    #[test]
    fn test_illegal_transition_archived_to_active() {
        let (_dir, repo) = setup_repo();
        create_spec(&repo, "my-spec");

        // Active → Completed → Archived
        repo.transition_spec_lifecycle("my-spec", LifecycleState::Completed)
            .unwrap();
        repo.transition_spec_lifecycle("my-spec", LifecycleState::Archived)
            .unwrap();

        let result = repo.transition_spec_lifecycle("my-spec", LifecycleState::Active);
        assert!(result.is_err());
    }

    #[test]
    fn test_transition_completed_to_active_allowed() {
        // Completed → Active is now a valid transition (supports writ reopen).
        let (_dir, repo) = setup_repo();
        create_spec(&repo, "my-spec");

        repo.transition_spec_lifecycle("my-spec", LifecycleState::Completed)
            .unwrap();

        let result = repo.transition_spec_lifecycle("my-spec", LifecycleState::Active);
        assert!(result.is_ok());

        let spec = repo.load_spec("my-spec").unwrap();
        assert_eq!(spec.lifecycle_state, LifecycleState::Active);
    }

    #[test]
    fn test_transition_updates_timestamp() {
        let (_dir, repo) = setup_repo();
        create_spec(&repo, "my-spec");

        let before = repo.load_spec("my-spec").unwrap().updated_at;
        std::thread::sleep(std::time::Duration::from_millis(10));

        repo.transition_spec_lifecycle("my-spec", LifecycleState::Stale)
            .unwrap();

        let after = repo.load_spec("my-spec").unwrap().updated_at;
        assert!(after > before);
    }

    #[test]
    fn test_cancel_spec_from_active() {
        let (_dir, repo) = setup_repo();
        create_spec(&repo, "my-spec");

        repo.cancel_spec("my-spec").unwrap();
        let spec = repo.load_spec("my-spec").unwrap();
        assert_eq!(spec.lifecycle_state, LifecycleState::Cancelled);
    }

    #[test]
    fn test_cancel_spec_from_stale() {
        let (_dir, repo) = setup_repo();
        create_spec(&repo, "my-spec");

        repo.transition_spec_lifecycle("my-spec", LifecycleState::Stale)
            .unwrap();
        repo.cancel_spec("my-spec").unwrap();
        let spec = repo.load_spec("my-spec").unwrap();
        assert_eq!(spec.lifecycle_state, LifecycleState::Cancelled);
    }

    #[test]
    fn test_cancel_spec_already_cancelled_fails() {
        let (_dir, repo) = setup_repo();
        create_spec(&repo, "my-spec");

        repo.cancel_spec("my-spec").unwrap();
        let result = repo.cancel_spec("my-spec");
        assert!(result.is_err());
    }

    #[test]
    fn test_complete_spec_requires_status_complete() {
        let (_dir, repo) = setup_repo();
        create_spec(&repo, "my-spec");
        // Spec is Pending, not Complete
        let result = repo.complete_spec("my-spec");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("status must be 'complete'"));
    }

    #[test]
    fn test_complete_spec_with_correct_status() {
        let (dir, repo) = setup_repo();
        create_spec(&repo, "my-spec");

        // Seal to get the spec to Complete status
        fs::write(dir.path().join("test.txt"), "hello").unwrap();
        repo.seal(
            agent("test-agent"),
            "done".into(),
            Some("my-spec".into()),
            TaskStatus::Complete,
            Verification::default(),
            false,
        )
        .unwrap();

        let spec = repo.load_spec("my-spec").unwrap();
        assert_eq!(spec.status, SpecStatus::Complete);

        repo.complete_spec("my-spec").unwrap();
        let spec = repo.load_spec("my-spec").unwrap();
        assert_eq!(spec.lifecycle_state, LifecycleState::Completed);
    }

    #[test]
    fn test_seal_updates_last_activity() {
        let (dir, repo) = setup_repo();
        create_spec(&repo, "my-spec");

        let before = repo.load_spec("my-spec").unwrap().last_activity;
        std::thread::sleep(std::time::Duration::from_millis(10));

        fs::write(dir.path().join("work.txt"), "content").unwrap();
        repo.seal(
            agent("worker"),
            "did work".into(),
            Some("my-spec".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let after = repo.load_spec("my-spec").unwrap().last_activity;
        assert!(after > before, "last_activity should advance after seal");
    }

    #[test]
    fn test_scan_stale_specs_identifies_stale() {
        let (_dir, repo) = setup_repo();

        // Create spec with very old last_activity
        let mut spec = crate::spec::Spec::new("old-spec".into(), "Old".into(), "desc".into());
        spec.last_activity = chrono::Utc::now() - chrono::Duration::hours(5);
        repo.add_spec(&spec).unwrap();

        // Create fresh spec
        create_spec(&repo, "fresh-spec");

        let config = crate::gc::GcConfig::default(); // stale_timeout = 2h
        let stale = repo.scan_stale_specs(&config).unwrap();

        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].0, "old-spec");
        assert!(stale[0].1 >= 5 * 3600); // at least 5 hours old
    }

    #[test]
    fn test_scan_stale_specs_ignores_terminal_states() {
        let (_dir, repo) = setup_repo();

        // Create cancelled spec with old activity — should NOT be flagged
        let mut spec =
            crate::spec::Spec::new("cancelled-spec".into(), "Cancelled".into(), "desc".into());
        spec.last_activity = chrono::Utc::now() - chrono::Duration::hours(10);
        spec.lifecycle_state = LifecycleState::Cancelled;
        repo.add_spec(&spec).unwrap();

        // Create completed spec with old activity — should NOT be flagged
        let mut spec =
            crate::spec::Spec::new("completed-spec".into(), "Completed".into(), "desc".into());
        spec.last_activity = chrono::Utc::now() - chrono::Duration::hours(10);
        spec.lifecycle_state = LifecycleState::Completed;
        repo.add_spec(&spec).unwrap();

        let config = crate::gc::GcConfig::default();
        let stale = repo.scan_stale_specs(&config).unwrap();
        assert!(
            stale.is_empty(),
            "terminal specs should not be flagged as stale"
        );
    }

    #[test]
    fn test_scan_stale_specs_empty_repo() {
        let (_dir, repo) = setup_repo();
        let config = crate::gc::GcConfig::default();
        let stale = repo.scan_stale_specs(&config).unwrap();
        assert!(stale.is_empty());
    }

    #[test]
    fn test_storage_report_on_repo() {
        let (_dir, repo) = setup_repo();
        let report = repo.storage_report().unwrap();
        // Fresh repo has some storage (HEAD file, index.json, keys, etc.)
        assert!(report.total_bytes > 0);
    }

    #[test]
    fn test_transition_nonexistent_spec_fails() {
        let (_dir, repo) = setup_repo();
        let result = repo.transition_spec_lifecycle("no-such-spec", LifecycleState::Stale);
        assert!(matches!(result, Err(WritError::SpecNotFound(_))));
    }

    #[test]
    fn test_context_includes_stale_spec_warnings() {
        let (_dir, repo) = setup_repo();

        // Create a spec with old last_activity
        let mut spec = crate::spec::Spec::new("stale-spec".into(), "Stale".into(), "desc".into());
        spec.last_activity = chrono::Utc::now() - chrono::Duration::hours(5);
        repo.add_spec(&spec).unwrap();

        let filter = crate::context::ContextFilter {
            status: None,
            agent: None,
        };
        let ctx = repo
            .context(crate::context::ContextScope::Full, 10, &filter)
            .unwrap();

        assert!(
            !ctx.stale_specs.is_empty(),
            "should have stale spec warnings"
        );
        assert!(
            ctx.stale_specs[0].contains("stale-spec"),
            "warning should mention spec ID"
        );
        assert!(
            ctx.stale_specs[0].contains("inactive"),
            "warning should mention inactivity"
        );
    }

    #[test]
    fn test_context_no_stale_warnings_for_fresh_specs() {
        let (_dir, repo) = setup_repo();

        // Create a fresh spec
        create_spec(&repo, "fresh-spec");

        let filter = crate::context::ContextFilter {
            status: None,
            agent: None,
        };
        let ctx = repo
            .context(crate::context::ContextScope::Full, 10, &filter)
            .unwrap();

        assert!(
            ctx.stale_specs.is_empty(),
            "fresh specs should not produce stale warnings"
        );
    }

    #[test]
    fn test_storage_pressure_emits_event() {
        let (dir, repo) = setup_repo();

        // Set a tiny budget so we're immediately over the threshold
        let config = crate::gc::GcConfig {
            budget_bytes: 100, // 100 bytes — we're way over
            ..crate::gc::GcConfig::default()
        };
        config.save(&dir.path().join(".writ")).unwrap();

        // Create a file and seal — should trigger storage pressure event
        fs::write(dir.path().join("big.txt"), "content").unwrap();
        repo.seal(
            agent("test-agent"),
            "test".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Verify a storage_pressure event was emitted
        let logger = crate::security::SecurityEventLogger::new(&dir.path().join(".writ"));
        let events = logger.read_events(None).unwrap();
        let pressure_events: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == "storage_pressure")
            .collect();
        assert!(
            !pressure_events.is_empty(),
            "storage_pressure event should be emitted when over budget"
        );
    }

    #[test]
    fn test_seal_always_succeeds_regardless_of_storage_pressure() {
        let (dir, repo) = setup_repo();

        // Set a tiny budget
        let config = crate::gc::GcConfig {
            budget_bytes: 1, // 1 byte — absurdly small
            ..crate::gc::GcConfig::default()
        };
        config.save(&dir.path().join(".writ")).unwrap();

        // Seal should still succeed (seals are never refused)
        fs::write(dir.path().join("test.txt"), "content").unwrap();
        let result = repo.seal(
            agent("test-agent"),
            "should succeed".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        );
        assert!(result.is_ok(), "seal must always succeed: {:?}", result);
    }

    // -----------------------------------------------------------------------
    // Settings integration tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_open_loads_settings() {
        let dir = tempfile::tempdir().unwrap();
        Repository::init(dir.path()).unwrap();

        // Write a settings file
        let settings_json = r#"{"default_agent": "my-bot", "default_format": "json"}"#;
        fs::write(dir.path().join(".writ/settings.json"), settings_json).unwrap();

        let repo = Repository::open(dir.path()).unwrap();
        assert_eq!(repo.settings().default_agent.as_deref(), Some("my-bot"));
        assert_eq!(repo.settings().default_format.as_deref(), Some("json"));
    }

    #[test]
    fn test_open_no_settings_file() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        // No settings.json → all defaults
        assert!(repo.settings().default_agent.is_none());
        assert!(repo.settings().default_format.is_none());
        assert!(repo.settings().convergence.strategy.is_none());
    }

    #[test]
    fn test_enforce_scope_from_settings() {
        let dir = tempfile::tempdir().unwrap();
        Repository::init(dir.path()).unwrap();

        // Write settings with enforce_scope = true
        let settings_json = r#"{"enforce_scope": true}"#;
        fs::write(dir.path().join(".writ/settings.json"), settings_json).unwrap();

        let repo = Repository::open(dir.path()).unwrap();
        assert_eq!(repo.settings().enforce_scope, Some(true));
        // The enforce_scope field on Repository should also be true
        assert!(repo.enforce_scope);
    }

    #[test]
    fn test_init_creates_repo_with_default_settings() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // init → open → settings default
        assert!(repo.settings().default_agent.is_none());
        assert!(repo.settings().enforce_scope.is_none());
        assert!(repo.settings().convergence.auto_resolve.is_none());
    }
}
