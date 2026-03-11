//! Context — AI-native structured context dump.
//!
//! Produces a single structured output optimized for LLM consumption,
//! combining spec details, recent seal history, working state, and
//! pending changes into one token-efficient blob.

use serde::{Deserialize, Serialize};

use crate::diff::DiffOutput;
use crate::seal::{Seal, TaskStatus, Verification};
use crate::spec::{Spec, SpecStatus};
use crate::state::{FileStatus, WorkingState};

/// Scope of context to include.
#[derive(Debug, Clone)]
pub enum ContextScope {
    /// Full repository context.
    Full,
    /// Scoped to a specific spec and its related files/seals.
    Spec(String),
    /// Scoped to a specific agent's world: their specs, files, and risks.
    Agent(String),
}

/// Optional filters applied to the seal history in context output.
#[derive(Debug, Clone, Default)]
pub struct ContextFilter {
    /// Only include seals with this task status.
    pub status: Option<TaskStatus>,
    /// Only include seals by this agent ID.
    pub agent: Option<String>,
    /// Scope context to this workspace. When set, only specs assigned to this
    /// workspace (or globally visible) and seals created in this workspace are
    /// included. Cross-workspace dependencies shown as read-only summaries.
    pub workspace: Option<String>,
}

/// Token-efficient verification summary for context output.
///
/// Uses `skip_serializing_if` to omit default values, unlike the full
/// `Verification` struct on seals which always includes all fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests_passed: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests_failed: Option<u32>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub linted: bool,
}

impl VerificationSummary {
    /// Create from a full Verification, returning None if all defaults.
    pub fn from_verification(v: &Verification) -> Option<Self> {
        if v.tests_passed.is_none() && v.tests_failed.is_none() && !v.linted {
            None
        } else {
            Some(VerificationSummary {
                tests_passed: v.tests_passed,
                tests_failed: v.tests_failed,
                linted: v.linted,
            })
        }
    }
}

/// A compact seal summary (truncated for token efficiency).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SealSummary {
    /// Truncated seal ID (first 12 chars).
    pub id: String,
    /// ISO 8601 timestamp.
    pub timestamp: String,
    /// Agent who created this seal.
    pub agent: String,
    /// Human/agent-readable summary.
    pub summary: String,
    /// Number of files changed.
    pub files_changed: usize,
    /// Linked spec ID, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec_id: Option<String>,
    /// Task status at the time of sealing.
    pub status: String,
    /// Verification results, if any were provided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification: Option<VerificationSummary>,
    /// File paths changed in this seal — helps agents know which files to read.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_paths: Vec<String>,
}

impl SealSummary {
    /// Create a compact summary from a full Seal.
    pub fn from_seal(seal: &Seal) -> Self {
        Self::from_seal_with_paths(seal, true)
    }

    /// Create a compact summary, optionally including changed file paths.
    /// Omitting paths on older seals saves tokens — agents can use `writ diff`
    /// or `writ show` to inspect specific seals when needed.
    pub fn from_seal_with_paths(seal: &Seal, include_paths: bool) -> Self {
        let status = match seal.status {
            TaskStatus::InProgress => "in-progress",
            TaskStatus::Complete => "complete",
            TaskStatus::Blocked => "blocked",
        }
        .to_string();

        SealSummary {
            id: seal.id[..12].to_string(),
            timestamp: seal.timestamp.to_rfc3339(),
            agent: seal.agent.id.clone(),
            summary: seal.summary.clone(),
            files_changed: seal.changes.len(),
            spec_id: seal.spec_id.clone(),
            status,
            verification: VerificationSummary::from_verification(&seal.verification),
            changed_paths: if include_paths {
                seal.changes.iter().map(|c| c.path.clone()).collect()
            } else {
                vec![]
            },
        }
    }
}

/// Token-efficient working state summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingStateSummary {
    pub clean: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub new_files: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub modified_files: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deleted_files: Vec<String>,
    pub tracked_count: usize,
}

impl WorkingStateSummary {
    /// Build a summary from a full WorkingState.
    pub fn from_state(state: &WorkingState) -> Self {
        WorkingStateSummary {
            clean: state.is_clean(),
            new_files: state
                .changes
                .iter()
                .filter(|f| f.status == FileStatus::New)
                .map(|f| f.path.clone())
                .collect(),
            modified_files: state
                .changes
                .iter()
                .filter(|f| f.status == FileStatus::Modified)
                .map(|f| f.path.clone())
                .collect(),
            deleted_files: state
                .changes
                .iter()
                .filter(|f| f.status == FileStatus::Deleted)
                .map(|f| f.path.clone())
                .collect(),
            tracked_count: state.tracked_count,
        }
    }
}

/// Token-efficient diff summary (file-level, not line-level).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffSummary {
    pub files_changed: usize,
    pub total_additions: usize,
    pub total_deletions: usize,
    pub files: Vec<FileDiffSummary>,
}

impl DiffSummary {
    /// Build a summary from a full DiffOutput.
    pub fn from_diff(diff: &DiffOutput) -> Self {
        DiffSummary {
            files_changed: diff.files_changed,
            total_additions: diff.total_additions,
            total_deletions: diff.total_deletions,
            files: diff
                .files
                .iter()
                .map(|f| FileDiffSummary {
                    path: f.path.clone(),
                    change_type: format!("{:?}", f.change_type).to_lowercase(),
                    additions: f.additions,
                    deletions: f.deletions,
                })
                .collect(),
        }
    }
}

/// Per-file diff summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiffSummary {
    pub path: String,
    pub change_type: String,
    pub additions: usize,
    pub deletions: usize,
}

/// A nudge telling the agent they have unsealed work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SealNudge {
    /// Number of files changed since last seal.
    pub unsealed_file_count: usize,
    /// Human/agent-readable suggestion.
    pub message: String,
}

/// A file scope violation detected when reviewing seal history.
/// Surfaces cases where agents sealed files outside their spec's declared scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileScopeViolation {
    /// The seal that contained out-of-scope files.
    pub seal_id: String,
    /// Agent who made the seal.
    pub agent_id: String,
    /// The spec whose scope was violated.
    pub spec_id: String,
    /// Files that were outside the spec's declared file_scope.
    pub out_of_scope_files: Vec<String>,
    /// The spec's declared scope (for reference).
    pub declared_scope: Vec<String>,
}

/// Status of a dependency spec (shown in spec-scoped context).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepStatus {
    /// Spec ID of the dependency.
    pub spec_id: String,
    /// Current status (kebab-case).
    pub status: String,
    /// Whether this dependency is resolved (status is "complete").
    pub resolved: bool,
}

impl DepStatus {
    /// Build from a spec status enum.
    pub fn from_spec(spec_id: &str, status: &SpecStatus) -> Self {
        let status_str = match status {
            SpecStatus::Pending => "pending",
            SpecStatus::InProgress => "in-progress",
            SpecStatus::Complete => "complete",
            SpecStatus::Blocked => "blocked",
        };
        DepStatus {
            spec_id: spec_id.to_string(),
            status: status_str.to_string(),
            resolved: matches!(status, SpecStatus::Complete),
        }
    }

    /// Build a "not found" entry for a missing dependency spec.
    pub fn not_found(spec_id: &str) -> Self {
        DepStatus {
            spec_id: spec_id.to_string(),
            status: "not-found".to_string(),
            resolved: false,
        }
    }
}

/// Progress summary for a spec (shown in spec-scoped context).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecProgress {
    /// Total number of seals linked to this spec.
    pub total_seals: usize,
    /// Current spec status (kebab-case).
    pub current_status: String,
    /// Unique agent IDs who have sealed against this spec.
    pub agents_involved: Vec<String>,
    /// Timestamp of the most recent seal (ISO 8601).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_seal_at: Option<String>,
}

/// A spec branch whose tip is not reachable from global HEAD.
///
/// Surfaces "ghost agent" situations where concurrent agents sealed on
/// spec-scoped branches that were never converged into the main chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DivergedBranchWarning {
    /// The spec this branch belongs to.
    pub spec_id: String,
    /// Short ID of the branch tip seal.
    pub tip_seal: String,
    /// Number of seals on this branch not reachable from HEAD.
    pub seal_count: usize,
    /// Agent IDs that sealed on this branch.
    pub agents: Vec<String>,
    /// Suggested action for the user/orchestrator.
    pub recommendation: String,
}

/// A file touched by multiple agents — signals integration risk.
///
/// Surfaced in context so agents starting work can see which files
/// are "hot" (modified by 2+ agents) and plan accordingly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContention {
    /// Relative path of the contested file.
    pub path: String,
    /// Agent IDs that have sealed changes to this file.
    pub agents: Vec<String>,
    /// Total number of seals that include this file.
    pub total_seals: usize,
}

/// Per-agent activity summary for multi-agent awareness.
///
/// Shows which files each agent "owns" (last sealed) and their recent
/// activity, so agents can see each other's work without filesystem
/// inspection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentActivity {
    /// Agent identifier.
    pub agent_id: String,
    /// Files this agent most recently sealed (provenance — who last touched each file).
    pub files_owned: Vec<String>,
    /// Number of seals by this agent in the seal history.
    pub seal_count: usize,
    /// Summary of their most recent seal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_summary: Option<String>,
    /// Timestamp of their most recent seal (ISO 8601).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_at: Option<String>,
    /// Spec IDs this agent has worked on.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub specs_touched: Vec<String>,
}

/// The full context output, optimized for LLM consumption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextOutput {
    /// Writ version marker for LLM parsing.
    pub writ_version: String,

    /// Active workspace name. Always present (defaults to "main").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,

    /// The active spec, if scoped or if there's exactly one in-progress spec.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_spec: Option<Spec>,

    /// All specs (omitted in spec-scoped mode to save tokens).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub all_specs: Option<Vec<Spec>>,

    /// Current working directory state.
    pub working_state: WorkingStateSummary,

    /// Recent seal history (compact).
    pub recent_seals: Vec<SealSummary>,

    /// Current diff summary (file-level, not full hunks).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_changes: Option<DiffSummary>,

    /// Nudge when there are unsealed changes — prompts the agent to checkpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seal_nudge: Option<SealNudge>,

    /// Files in scope.
    pub file_scope: Vec<String>,

    /// Total tracked file count.
    pub tracked_files: usize,

    /// Status of each dependency when spec-scoped (omitted in full scope).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependency_status: Option<Vec<DepStatus>>,

    /// Summary of spec completion progress when spec-scoped (omitted in full scope).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec_progress: Option<SpecProgress>,

    /// Per-agent file ownership and recent activity for multi-agent awareness.
    /// Shows which agent last sealed each file, enabling cross-agent coordination.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub agent_activity: Vec<AgentActivity>,

    /// Warnings about spec branches that diverged from global HEAD.
    /// Non-empty means there are "ghost agent" branches with unmerged work.
    /// Agents should consider running `converge()` to unify these branches.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub diverged_branches: Vec<DivergedBranchWarning>,

    /// True when diverged branches exist and convergence is recommended.
    /// Agents should check this flag and run `writ converge` (or `converge()`
    /// via the SDK) to merge diverged spec branches back into the main chain.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub convergence_recommended: bool,

    /// File scope violations detected in recent seals.
    /// Non-empty when agents sealed files outside their spec's declared file_scope.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub file_scope_violations: Vec<FileScopeViolation>,

    /// Files touched by 2+ agents — signals integration risk.
    /// Sorted by agent count descending, capped at top 10.
    /// Helps agents identify "hot" files before starting work.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub file_contention: Vec<FileContention>,

    /// Top-level integration risk assessment.
    /// Computed from diverged branches, file contention, and scope violations.
    /// Omitted when risk is low (score 0, no factors).
    #[serde(default, skip_serializing_if = "IntegrationRisk::is_low")]
    pub integration_risk: IntegrationRisk,

    /// True when all specs in the repository are marked complete.
    /// Signals to agents/humans that work is done and `writ summary` is available.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub session_complete: bool,

    /// Inline session summary, populated only when session_complete is true.
    /// Gives a quick overview without needing to run `writ summary` separately.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_summary: Option<SessionSummary>,

    /// Actionable recommendation: the single most important thing to do next.
    /// `None` when there's nothing urgent — just keep working.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended_action: Option<RecommendedAction>,

    /// Cryptographic chain integrity status.
    /// Omitted if the chain has no secured seals (all pre-Sprint A).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_integrity: Option<ChainIntegritySummary>,

    /// Specs that have been inactive longer than the stale timeout.
    /// Each entry is a human-readable warning like "spec 'foo' inactive for 3h".
    /// Populated by lazy stale detection during `context()`.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub stale_specs: Vec<String>,

    /// Cross-workspace dependency specs. When context is workspace-scoped,
    /// this shows specs from other workspaces that our specs depend on.
    /// Read-only summary: agents can see dependency status but not modify them.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub dependencies: Vec<DependencyContext>,

    /// Available writ operations for agent discoverability.
    pub available_operations: Vec<String>,
}

/// Read-only summary of a spec from another workspace that our specs depend on.
/// Included in workspace-scoped context so agents can see dependency status
/// without needing full cross-workspace visibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyContext {
    /// Spec ID of the dependency.
    pub id: String,
    /// Spec title.
    pub title: String,
    /// Current status.
    pub status: String,
    /// Which workspace this spec is assigned to (or "global" if unassigned).
    pub workspace: String,
}

/// Top-level integration risk assessment computed from context signals.
///
/// Gives agents/orchestrators a single field to check before starting work
/// or after convergence to gauge how risky the current state is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationRisk {
    /// Overall risk level: "low", "medium", or "high".
    pub level: String,
    /// Human/agent-readable factors contributing to the risk level.
    pub factors: Vec<String>,
    /// Numeric score (0-100) for programmatic comparison.
    pub score: u32,
}

impl IntegrationRisk {
    /// Compute integration risk from context signals.
    pub fn compute(
        diverged_count: usize,
        max_file_agents: usize,
        scope_violation_count: usize,
        contention_file_count: usize,
    ) -> Self {
        let mut score: u32 = 0;
        let mut factors = Vec::new();

        if diverged_count > 3 {
            score += 40;
            factors.push(format!("{diverged_count} diverged branches (>3)"));
        } else if diverged_count > 0 {
            score += 15 * diverged_count as u32;
            factors.push(format!("{diverged_count} diverged branch(es)"));
        }

        if max_file_agents >= 5 {
            score += 30;
            factors.push(format!("file touched by {max_file_agents} agents (>=5)"));
        } else if max_file_agents >= 3 {
            score += 15;
            factors.push(format!("file touched by {max_file_agents} agents (>=3)"));
        }

        if scope_violation_count > 5 {
            score += 20;
            factors.push(format!("{scope_violation_count} scope violations (>5)"));
        } else if scope_violation_count > 0 {
            score += 5 * scope_violation_count as u32;
            factors.push(format!("{scope_violation_count} scope violation(s)"));
        }

        if contention_file_count > 5 {
            score += 10;
            factors.push(format!("{contention_file_count} contested files"));
        }

        score = score.min(100);

        let level = if score >= 50 {
            "high"
        } else if score > 0 {
            "medium"
        } else {
            "low"
        }
        .to_string();

        IntegrationRisk {
            level,
            factors,
            score,
        }
    }

    /// True when risk is low with no factors — used to skip serialization.
    pub fn is_low(&self) -> bool {
        self.score == 0 && self.factors.is_empty()
    }
}

impl Default for IntegrationRisk {
    fn default() -> Self {
        IntegrationRisk {
            level: "low".to_string(),
            factors: vec![],
            score: 0,
        }
    }
}

/// Compact inline summary shown in context when all specs are complete.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub headline: String,
    pub total_seals: usize,
    pub agent_count: usize,
    pub specs_completed: usize,
    pub files_changed: usize,
    pub message: String,
}

/// Lightweight chain integrity summary for context output.
///
/// Tells agents whether the seal chain is cryptographically valid without
/// exposing full per-seal verification details (use `verify_chain()` for that).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainIntegritySummary {
    /// True if all secured seals pass content_hash + chain_hash verification.
    pub valid: bool,
    /// Total seals in the chain.
    pub total_seals: usize,
    /// Seals with crypto fields that verified successfully.
    pub verified: usize,
    /// Legacy seals without crypto fields (pre-Sprint A).
    pub unsecured: usize,
    /// Number of seals that failed verification (0 when valid).
    pub failures: usize,
}

/// Actionable recommendation based on current context state.
///
/// Tells the agent *what to do next* instead of just *what is*.
/// Priority logic selects the single most important action:
/// blocking dependency > convergence needed > high risk > unsealed changes > session complete.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecommendedAction {
    /// Machine-readable action type (e.g. "converge", "seal", "wait_for_dependency").
    pub action: String,
    /// Human/agent-readable explanation of what to do and why.
    pub message: String,
    /// Priority level: "high", "medium", or "low".
    pub priority: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{DiffOutput, FileDiff};
    use crate::seal::{
        AgentIdentity, AgentType, ChangeType, FileChange, Seal, TaskStatus, Verification,
    };
    use crate::spec::SpecStatus;
    use crate::state::{FileState, FileStatus, WorkingState};

    // ── Helper: build a minimal seal for testing ─────────────

    fn make_seal(agent_id: &str, summary: &str, status: TaskStatus) -> Seal {
        Seal::new(
            None,
            "tree_hash".to_string(),
            AgentIdentity {
                id: agent_id.to_string(),
                agent_type: AgentType::Agent,
            },
            None,
            status,
            vec![FileChange {
                path: "app.py".to_string(),
                change_type: ChangeType::Modified,
                old_hash: Some("old".to_string()),
                new_hash: Some("new".to_string()),
            }],
            Verification::default(),
            summary.to_string(),
            vec![],
            None,
        )
    }

    // ── VerificationSummary ──────────────────────────────────

    #[test]
    fn verification_summary_returns_none_for_all_defaults() {
        let v = Verification::default();
        assert!(VerificationSummary::from_verification(&v).is_none());
    }

    #[test]
    fn verification_summary_returns_some_with_tests_passed() {
        let v = Verification {
            tests_passed: Some(10),
            tests_failed: None,
            linted: false,
        };
        let s = VerificationSummary::from_verification(&v).unwrap();
        assert_eq!(s.tests_passed, Some(10));
        assert_eq!(s.tests_failed, None);
        assert!(!s.linted);
    }

    #[test]
    fn verification_summary_returns_some_when_linted() {
        let v = Verification {
            tests_passed: None,
            tests_failed: None,
            linted: true,
        };
        let s = VerificationSummary::from_verification(&v).unwrap();
        assert!(s.linted);
    }

    #[test]
    fn verification_summary_preserves_all_fields() {
        let v = Verification {
            tests_passed: Some(42),
            tests_failed: Some(3),
            linted: true,
        };
        let s = VerificationSummary::from_verification(&v).unwrap();
        assert_eq!(s.tests_passed, Some(42));
        assert_eq!(s.tests_failed, Some(3));
        assert!(s.linted);
    }

    // ── SealSummary ──────────────────────────────────────────

    #[test]
    fn seal_summary_truncates_id_to_12_chars() {
        let seal = make_seal("agent-a", "test seal", TaskStatus::InProgress);
        let summary = SealSummary::from_seal(&seal);
        assert_eq!(summary.id.len(), 12);
        assert_eq!(&summary.id, &seal.id[..12]);
    }

    #[test]
    fn seal_summary_formats_in_progress_status() {
        let seal = make_seal("agent-a", "working", TaskStatus::InProgress);
        let summary = SealSummary::from_seal(&seal);
        assert_eq!(summary.status, "in-progress");
    }

    #[test]
    fn seal_summary_formats_complete_status() {
        let seal = make_seal("agent-a", "done", TaskStatus::Complete);
        let summary = SealSummary::from_seal(&seal);
        assert_eq!(summary.status, "complete");
    }

    #[test]
    fn seal_summary_formats_blocked_status() {
        let seal = make_seal("agent-a", "stuck", TaskStatus::Blocked);
        let summary = SealSummary::from_seal(&seal);
        assert_eq!(summary.status, "blocked");
    }

    #[test]
    fn seal_summary_captures_agent_and_summary() {
        let seal = make_seal("backend-dev", "added auth routes", TaskStatus::InProgress);
        let summary = SealSummary::from_seal(&seal);
        assert_eq!(summary.agent, "backend-dev");
        assert_eq!(summary.summary, "added auth routes");
    }

    #[test]
    fn seal_summary_counts_files_changed() {
        let seal = make_seal("agent-a", "work", TaskStatus::InProgress);
        let summary = SealSummary::from_seal(&seal);
        assert_eq!(summary.files_changed, 1);
    }

    #[test]
    fn seal_summary_includes_changed_paths() {
        let seal = make_seal("agent-a", "work", TaskStatus::InProgress);
        let summary = SealSummary::from_seal(&seal);
        assert_eq!(summary.changed_paths, vec!["app.py"]);
    }

    #[test]
    fn seal_summary_omits_verification_when_default() {
        let seal = make_seal("agent-a", "work", TaskStatus::InProgress);
        let summary = SealSummary::from_seal(&seal);
        assert!(summary.verification.is_none());
    }

    #[test]
    fn seal_summary_includes_spec_id_when_present() {
        let mut seal = make_seal("agent-a", "work", TaskStatus::InProgress);
        seal.spec_id = Some("backend".to_string());
        let summary = SealSummary::from_seal(&seal);
        assert_eq!(summary.spec_id, Some("backend".to_string()));
    }

    // ── WorkingStateSummary ──────────────────────────────────

    #[test]
    fn working_state_summary_clean() {
        let state = WorkingState {
            changes: vec![],
            tracked_count: 5,
        };
        let summary = WorkingStateSummary::from_state(&state);
        assert!(summary.clean);
        assert!(summary.new_files.is_empty());
        assert!(summary.modified_files.is_empty());
        assert!(summary.deleted_files.is_empty());
        assert_eq!(summary.tracked_count, 5);
    }

    #[test]
    fn working_state_summary_categorizes_changes() {
        let state = WorkingState {
            changes: vec![
                FileState {
                    path: "new.py".to_string(),
                    status: FileStatus::New,
                    hash: Some("h".to_string()),
                },
                FileState {
                    path: "mod.py".to_string(),
                    status: FileStatus::Modified,
                    hash: Some("h".to_string()),
                },
                FileState {
                    path: "del.py".to_string(),
                    status: FileStatus::Deleted,
                    hash: None,
                },
            ],
            tracked_count: 2,
        };
        let summary = WorkingStateSummary::from_state(&state);
        assert!(!summary.clean);
        assert_eq!(summary.new_files, vec!["new.py"]);
        assert_eq!(summary.modified_files, vec!["mod.py"]);
        assert_eq!(summary.deleted_files, vec!["del.py"]);
    }

    // ── DiffSummary ──────────────────────────────────────────

    #[test]
    fn diff_summary_maps_diff_output() {
        let diff = DiffOutput {
            description: "changes".to_string(),
            files: vec![FileDiff {
                path: "app.py".to_string(),
                change_type: ChangeType::Modified,
                hunks: vec![],
                is_binary: false,
                additions: 10,
                deletions: 3,
            }],
            files_changed: 1,
            total_additions: 10,
            total_deletions: 3,
        };
        let summary = DiffSummary::from_diff(&diff);
        assert_eq!(summary.files_changed, 1);
        assert_eq!(summary.total_additions, 10);
        assert_eq!(summary.total_deletions, 3);
        assert_eq!(summary.files.len(), 1);
        assert_eq!(summary.files[0].path, "app.py");
        assert_eq!(summary.files[0].additions, 10);
        assert_eq!(summary.files[0].deletions, 3);
    }

    #[test]
    fn diff_summary_empty_diff() {
        let diff = DiffOutput {
            description: "none".to_string(),
            files: vec![],
            files_changed: 0,
            total_additions: 0,
            total_deletions: 0,
        };
        let summary = DiffSummary::from_diff(&diff);
        assert_eq!(summary.files_changed, 0);
        assert!(summary.files.is_empty());
    }

    // ── DepStatus ────────────────────────────────────────────

    #[test]
    fn dep_status_from_complete_spec() {
        let dep = DepStatus::from_spec("auth", &SpecStatus::Complete);
        assert_eq!(dep.spec_id, "auth");
        assert_eq!(dep.status, "complete");
        assert!(dep.resolved);
    }

    #[test]
    fn dep_status_from_pending_spec() {
        let dep = DepStatus::from_spec("db", &SpecStatus::Pending);
        assert_eq!(dep.status, "pending");
        assert!(!dep.resolved);
    }

    #[test]
    fn dep_status_from_in_progress_spec() {
        let dep = DepStatus::from_spec("api", &SpecStatus::InProgress);
        assert_eq!(dep.status, "in-progress");
        assert!(!dep.resolved);
    }

    #[test]
    fn dep_status_from_blocked_spec() {
        let dep = DepStatus::from_spec("ui", &SpecStatus::Blocked);
        assert_eq!(dep.status, "blocked");
        assert!(!dep.resolved);
    }

    #[test]
    fn dep_status_not_found() {
        let dep = DepStatus::not_found("missing-spec");
        assert_eq!(dep.spec_id, "missing-spec");
        assert_eq!(dep.status, "not-found");
        assert!(!dep.resolved);
    }

    // ── IntegrationRisk::compute ─────────────────────────────

    #[test]
    fn risk_low_when_no_signals() {
        let risk = IntegrationRisk::compute(0, 0, 0, 0);
        assert_eq!(risk.level, "low");
        assert_eq!(risk.score, 0);
        assert!(risk.factors.is_empty());
    }

    #[test]
    fn risk_medium_with_one_diverged_branch() {
        let risk = IntegrationRisk::compute(1, 0, 0, 0);
        assert_eq!(risk.level, "medium");
        assert_eq!(risk.score, 15);
        assert_eq!(risk.factors.len(), 1);
    }

    #[test]
    fn risk_medium_with_three_diverged_branches() {
        let risk = IntegrationRisk::compute(3, 0, 0, 0);
        assert_eq!(risk.level, "medium");
        assert_eq!(risk.score, 45);
    }

    #[test]
    fn risk_high_with_four_plus_diverged_branches() {
        let risk = IntegrationRisk::compute(4, 0, 0, 0);
        assert_eq!(risk.level, "medium");
        assert_eq!(risk.score, 40);
    }

    #[test]
    fn risk_high_with_five_agent_file_contention() {
        let risk = IntegrationRisk::compute(0, 5, 0, 0);
        assert_eq!(risk.score, 30);
    }

    #[test]
    fn risk_medium_with_three_agent_file_contention() {
        let risk = IntegrationRisk::compute(0, 3, 0, 0);
        assert_eq!(risk.score, 15);
    }

    #[test]
    fn risk_scores_scope_violations() {
        let risk = IntegrationRisk::compute(0, 0, 1, 0);
        assert_eq!(risk.score, 5);

        let risk = IntegrationRisk::compute(0, 0, 5, 0);
        assert_eq!(risk.score, 25);
    }

    #[test]
    fn risk_high_with_many_scope_violations() {
        let risk = IntegrationRisk::compute(0, 0, 6, 0);
        assert_eq!(risk.score, 20);
    }

    #[test]
    fn risk_adds_contested_files_above_five() {
        let risk = IntegrationRisk::compute(0, 0, 0, 5);
        assert_eq!(risk.score, 0); // 5 is not > 5

        let risk = IntegrationRisk::compute(0, 0, 0, 6);
        assert_eq!(risk.score, 10);
    }

    #[test]
    fn risk_compounds_multiple_signals() {
        // 2 diverged (30) + 3 agents on file (15) + 2 violations (10) = 55
        let risk = IntegrationRisk::compute(2, 3, 2, 0);
        assert_eq!(risk.score, 30 + 15 + 10);
        assert_eq!(risk.level, "high");
    }

    #[test]
    fn risk_score_capped_at_100() {
        let risk = IntegrationRisk::compute(10, 10, 10, 10);
        assert_eq!(risk.score, 100);
    }

    #[test]
    fn risk_level_thresholds() {
        // Score 0 = low
        assert_eq!(IntegrationRisk::compute(0, 0, 0, 0).level, "low");
        // Score 1-49 = medium
        assert_eq!(IntegrationRisk::compute(0, 0, 1, 0).level, "medium");
        // Score 50+ = high
        assert_eq!(IntegrationRisk::compute(4, 5, 0, 0).level, "high");
    }

    // ── Serialization: skip_serializing_if behavior ──────────

    #[test]
    fn verification_summary_skips_none_fields_in_json() {
        let v = VerificationSummary {
            tests_passed: Some(5),
            tests_failed: None,
            linted: false,
        };
        let json = serde_json::to_string(&v).unwrap();
        assert!(json.contains("tests_passed"));
        assert!(!json.contains("tests_failed"));
        assert!(!json.contains("linted"));
    }

    #[test]
    fn seal_summary_skips_empty_changed_paths_in_json() {
        let summary = SealSummary {
            id: "abc123def456".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            agent: "test".to_string(),
            summary: "test".to_string(),
            files_changed: 0,
            spec_id: None,
            status: "in-progress".to_string(),
            verification: None,
            changed_paths: vec![],
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(!json.contains("changed_paths"));
        assert!(!json.contains("spec_id"));
        assert!(!json.contains("verification"));
    }

    #[test]
    fn working_state_summary_skips_empty_lists_in_json() {
        let summary = WorkingStateSummary {
            clean: true,
            new_files: vec![],
            modified_files: vec![],
            deleted_files: vec![],
            tracked_count: 3,
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(!json.contains("new_files"));
        assert!(!json.contains("modified_files"));
        assert!(!json.contains("deleted_files"));
        assert!(json.contains("clean"));
        assert!(json.contains("tracked_count"));
    }
}
