//! ScenarioBuilder — fluent API for convergence scenario replays.
//!
//! Provides a builder pattern for constructing multi-agent convergence
//! scenarios with real I/O, sealing, divergence, and merge verification.
//!
//! Usage:
//! ```rust,ignore
//! let result = ScenarioBuilder::new()
//!     .baseline("models.py", "class Base: pass\n")
//!     .agent("agent-a", "spec-a")
//!         .writes("models.py", "class User: ...\n")
//!         .seal()
//!     .agent("agent-b", "spec-b")
//!         .writes("models.py", "class Product: ...\n")
//!         .seal()
//!     .converge()
//!     .expect_success();
//! ```

use std::fs;
use std::path::PathBuf;

use tempfile::{tempdir, TempDir};
use writ_core::convergence::{ConvergeAllReport, ConvergeStrategy};
use writ_core::seal::{AgentIdentity, AgentType, Verification};
use writ_core::spec::Spec;
use writ_core::Repository;

// ── Builder ────────────────────────────────────────────────────────

pub struct ScenarioBuilder {
    dir: TempDir,
    baselines: Vec<(String, String)>,
    agents: Vec<AgentWork>,
    initialized: bool,
}

struct AgentWork {
    agent_id: String,
    spec_id: String,
    changes: Vec<FileChange>,
}

enum FileChange {
    Write(String, String),
    Append(String, String),
    Delete(String),
}

impl ScenarioBuilder {
    pub fn new() -> Self {
        Self {
            dir: tempdir().expect("failed to create temp dir"),
            baselines: Vec::new(),
            agents: Vec::new(),
            initialized: false,
        }
    }

    /// Add a baseline file before any agent work.
    pub fn baseline(mut self, path: &str, content: &str) -> Self {
        self.baselines.push((path.to_string(), content.to_string()));
        self
    }

    /// Begin defining an agent's work on a spec.
    pub fn agent(self, agent_id: &str, spec_id: &str) -> AgentBuilder {
        AgentBuilder {
            scenario: self,
            agent_id: agent_id.to_string(),
            spec_id: spec_id.to_string(),
            changes: Vec::new(),
        }
    }

    /// Run convergence with escalate strategy and return the result.
    pub fn converge(self) -> ConvergeResult {
        self.converge_with_strategy(ConvergeStrategy::Escalate)
    }

    /// Run convergence with a specific strategy.
    pub fn converge_with_strategy(mut self, strategy: ConvergeStrategy) -> ConvergeResult {
        let repo = self.execute();

        let report = repo.converge_all(strategy, true);
        let root = self.dir.path().to_path_buf();

        ConvergeResult {
            _dir: self.dir,
            root,
            report,
            repo,
        }
    }

    /// Execute the scenario: init repo, create baselines, seal each agent's work.
    fn execute(&mut self) -> Repository {
        let root = self.dir.path();

        // 1. Write baseline files
        for (path, content) in &self.baselines {
            let full = root.join(path);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&full, content).unwrap();
        }

        // 2. Init repo + base seal
        let repo = Repository::init(root).unwrap();

        if !self.baselines.is_empty() {
            repo.seal(
                AgentIdentity {
                    id: "setup".to_string(),
                    agent_type: AgentType::Agent,
                },
                "baseline".to_string(),
                None,
                writ_core::seal::TaskStatus::Complete,
                Verification::default(),
                false,
            )
            .unwrap();
        }

        // 3. Collect unique spec IDs and create specs
        let mut spec_ids: Vec<String> = Vec::new();
        for agent_work in &self.agents {
            if !spec_ids.contains(&agent_work.spec_id) {
                spec_ids.push(agent_work.spec_id.clone());
            }
        }
        for spec_id in &spec_ids {
            let spec = Spec::new(spec_id.clone(), spec_id.clone(), String::new());
            repo.add_spec(&spec).unwrap();
        }

        // 4. Execute each agent's changes and seal.
        //    Each agent restores to baseline state before applying changes
        //    to create proper divergent state.
        let agents = std::mem::take(&mut self.agents);
        for agent_work in &agents {
            // Restore baseline files for divergence
            for (path, content) in &self.baselines {
                let full = root.join(path);
                fs::write(&full, content).unwrap();
            }

            // Apply this agent's changes
            for change in &agent_work.changes {
                match change {
                    FileChange::Write(path, content) => {
                        let full = root.join(path);
                        if let Some(parent) = full.parent() {
                            fs::create_dir_all(parent).unwrap();
                        }
                        fs::write(&full, content).unwrap();
                    }
                    FileChange::Append(path, content) => {
                        let full = root.join(path);
                        let existing = fs::read_to_string(&full).unwrap_or_default();
                        fs::write(&full, format!("{existing}{content}")).unwrap();
                    }
                    FileChange::Delete(path) => {
                        let full = root.join(path);
                        if full.exists() {
                            fs::remove_file(&full).unwrap();
                        }
                    }
                }
            }

            // Seal the agent's work (allow_empty handles identical-change scenarios)
            let seal_result = repo.seal(
                AgentIdentity {
                    id: agent_work.agent_id.clone(),
                    agent_type: AgentType::Agent,
                },
                format!("{} work on {}", agent_work.agent_id, agent_work.spec_id),
                Some(agent_work.spec_id.clone()),
                writ_core::seal::TaskStatus::InProgress,
                Verification::default(),
                true, // allow_empty — handles cases where changes match prior state
            );
            // NothingToSeal is ok for scenarios with identical changes
            if let Err(ref e) = seal_result {
                let msg = format!("{e}");
                if !msg.contains("NothingToSeal") && !msg.contains("nothing to seal") {
                    seal_result.unwrap();
                }
            }
        }
        self.agents = agents;
        self.initialized = true;

        repo
    }
}

// ── AgentBuilder ───────────────────────────────────────────────────

pub struct AgentBuilder {
    scenario: ScenarioBuilder,
    agent_id: String,
    spec_id: String,
    changes: Vec<FileChange>,
}

impl AgentBuilder {
    /// Agent writes a file (complete replacement).
    pub fn writes(mut self, path: &str, content: &str) -> Self {
        self.changes
            .push(FileChange::Write(path.to_string(), content.to_string()));
        self
    }

    /// Agent appends to a file.
    pub fn appends(mut self, path: &str, content: &str) -> Self {
        self.changes
            .push(FileChange::Append(path.to_string(), content.to_string()));
        self
    }

    /// Agent deletes a file.
    pub fn deletes(mut self, path: &str) -> Self {
        self.changes.push(FileChange::Delete(path.to_string()));
        self
    }

    /// Seal the agent's work and return to the scenario builder.
    pub fn seal(mut self) -> ScenarioBuilder {
        let mut scenario = self.scenario;
        scenario.agents.push(AgentWork {
            agent_id: self.agent_id,
            spec_id: self.spec_id,
            changes: std::mem::take(&mut self.changes),
        });
        scenario
    }
}

// ── ConvergeResult ─────────────────────────────────────────────────

pub struct ConvergeResult {
    _dir: TempDir,
    root: PathBuf,
    report: writ_core::WritResult<ConvergeAllReport>,
    repo: Repository,
}

impl ConvergeResult {
    /// Assert convergence succeeded and return a SuccessResult for assertions.
    pub fn expect_success(self) -> SuccessResult {
        let report = self.report.expect("convergence should succeed");
        assert!(
            report.escalations.is_empty(),
            "Expected no escalations, got {}: {:?}",
            report.escalations.len(),
            report
                .escalations
                .iter()
                .map(|e| &e.file_path)
                .collect::<Vec<_>>()
        );
        SuccessResult {
            _dir: self._dir,
            root: self.root,
            report,
            repo: self.repo,
        }
    }

    /// Assert convergence produced escalations.
    pub fn expect_escalation(self) -> EscalationResult {
        let report = self.report.expect("convergence should complete");
        assert!(
            !report.escalations.is_empty(),
            "Expected escalations but convergence was clean"
        );
        EscalationResult {
            _dir: self._dir,
            root: self.root,
            report,
            repo: self.repo,
        }
    }

    /// Assert convergence failed entirely.
    pub fn expect_error(self) {
        assert!(
            self.report.is_err(),
            "Expected convergence error, but it succeeded"
        );
    }
}

// ── SuccessResult ──────────────────────────────────────────────────

pub struct SuccessResult {
    _dir: TempDir,
    root: PathBuf,
    report: ConvergeAllReport,
    repo: Repository,
}

impl SuccessResult {
    // ── Content Assertions ─────────────────────────────────────

    /// Assert a file contains specific content after convergence.
    pub fn assert_file_contains(&self, path: &str, content: &str) {
        let full = self.root.join(path);
        let file_content =
            fs::read_to_string(&full).unwrap_or_else(|e| panic!("Failed to read {path}: {e}"));
        assert!(
            file_content.contains(content),
            "File {path} does not contain expected content.\nExpected: {content:?}\nGot: {file_content:?}"
        );
    }

    /// Assert a file does NOT contain specific content.
    pub fn assert_file_not_contains(&self, path: &str, content: &str) {
        let full = self.root.join(path);
        let file_content =
            fs::read_to_string(&full).unwrap_or_else(|e| panic!("Failed to read {path}: {e}"));
        assert!(
            !file_content.contains(content),
            "File {path} should NOT contain: {content:?}\nBut it does: {file_content:?}"
        );
    }

    /// Assert all named definitions are present in the merged file.
    pub fn assert_definitions_preserved(&self, path: &str, names: &[&str]) {
        let full = self.root.join(path);
        let content =
            fs::read_to_string(&full).unwrap_or_else(|e| panic!("Failed to read {path}: {e}"));
        for name in names {
            assert!(
                content.contains(name),
                "Definition '{name}' missing from {path}.\nContent: {content}"
            );
        }
    }

    // ── Resolution Assertions ──────────────────────────────────

    /// Assert that convergence auto-resolved (no conflicts or all resolved).
    pub fn assert_fully_resolved(&self) {
        assert!(
            self.report.escalations.is_empty(),
            "Expected fully resolved, but {} escalations remain",
            self.report.escalations.len()
        );
    }

    /// Assert the total number of files changed by convergence.
    pub fn assert_files_changed(&self, expected: usize) {
        assert_eq!(
            self.report.files_changed.len(),
            expected,
            "Expected {} files changed, got {}: {:?}",
            expected,
            self.report.files_changed.len(),
            self.report.files_changed
        );
    }

    /// Assert no degradation occurred (no silent content loss).
    pub fn assert_not_degraded(&self) {
        assert!(
            !self.report.degraded,
            "Convergence was degraded — content may have been silently lost"
        );
    }

    // ── Metadata Assertions ────────────────────────────────────

    /// Get the raw report for custom assertions.
    pub fn report(&self) -> &ConvergeAllReport {
        &self.report
    }

    /// Get the repo for further inspection.
    pub fn repo(&self) -> &Repository {
        &self.repo
    }

    /// Read a file's content after convergence.
    pub fn file_content(&self, path: &str) -> String {
        let full = self.root.join(path);
        fs::read_to_string(&full).unwrap_or_else(|e| panic!("Failed to read {path}: {e}"))
    }
}

// ── EscalationResult ───────────────────────────────────────────────

pub struct EscalationResult {
    _dir: TempDir,
    root: PathBuf,
    report: ConvergeAllReport,
    repo: Repository,
}

impl EscalationResult {
    /// Assert a specific file was escalated.
    pub fn assert_escalated(&self, path: &str) {
        assert!(
            self.report.escalations.iter().any(|e| e.file_path == path),
            "Expected {path} to be escalated, but it wasn't.\nEscalated: {:?}",
            self.report
                .escalations
                .iter()
                .map(|e| &e.file_path)
                .collect::<Vec<_>>()
        );
    }

    /// Get the raw report for custom assertions.
    pub fn report(&self) -> &ConvergeAllReport {
        &self.report
    }

    /// Read a file's content after convergence.
    pub fn file_content(&self, path: &str) -> String {
        let full = self.root.join(path);
        fs::read_to_string(&full).unwrap_or_else(|e| panic!("Failed to read {path}: {e}"))
    }
}
