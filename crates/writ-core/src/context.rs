//! Context — AI-native structured context dump.
//!
//! Produces a single structured output optimized for LLM consumption,
//! combining spec details, recent seal history, working state, and
//! pending changes into one token-efficient blob.

use std::cmp::{Ordering, Reverse};
use std::collections::{HashMap, HashSet};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::diff::DiffOutput;
use crate::seal::{Seal, TaskStatus, Verification};
use crate::spec::{Spec, SpecStatus};
use crate::state::{FileStatus, WorkingState};
use crate::WritResult;

/// Version stamped into every context dump (`writ_version`). Comes from the
/// workspace `Cargo.toml`, so it can never drift from the released binary.
pub const WRIT_VERSION: &str = env!("CARGO_PKG_VERSION");

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub new_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modified_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleted_files: Vec<String>,
    pub tracked_count: usize,
    /// True when the file lists above were capped. Counts stay exact in `counts`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// Number of paths dropped from the lists above.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted: usize,
    /// Exact per-status counts, present only when the lists were truncated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts: Option<ChangeCounts>,
}

/// Exact per-status change counts for a truncated working state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeCounts {
    pub new: usize,
    pub modified: usize,
    pub deleted: usize,
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
            truncated: false,
            omitted: 0,
            counts: None,
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
    /// True when `files` was capped. The totals above stay exact.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// Number of file entries dropped from `files`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted: usize,
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
            truncated: false,
            omitted: 0,
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
    /// Number of paths dropped from `files_owned` by the file cap.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub files_owned_omitted: usize,
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

/// Task context surfaced at the top of `writ context` when called from a workspace.
/// Derived from the workspace's assigned spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskContext {
    /// Task (spec) ID.
    pub id: String,
    /// Human-readable title.
    pub title: String,
    /// Current status (pending, in-progress, complete, blocked).
    pub status: String,
}

/// An active spec that has not yet been claimed by any agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnclaimedSpec {
    /// Spec ID.
    pub id: String,
    /// Human-readable title.
    pub title: String,
}

/// The full context output, optimized for LLM consumption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextOutput {
    /// Writ version marker for LLM parsing.
    pub writ_version: String,

    /// Task context — present when running inside a workspace with an assigned spec.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskContext>,

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

    /// Files in scope (capped; see `file_scope_truncated`).
    pub file_scope: Vec<String>,

    /// True when `file_scope` was capped. `tracked_files` stays exact.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub file_scope_truncated: bool,

    /// Number of paths dropped from `file_scope`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub file_scope_omitted: usize,

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

    /// Specs that are active but not yet claimed by any agent.
    /// Agents should claim one of these before starting work.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub unclaimed_specs: Vec<UnclaimedSpec>,

    /// Cross-workspace dependency specs. When context is workspace-scoped,
    /// this shows specs from other workspaces that our specs depend on.
    /// Read-only summary: agents can see dependency status but not modify them.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub dependencies: Vec<DependencyContext>,

    /// Available writ operations for agent discoverability.
    pub available_operations: Vec<String>,

    /// True when `--budget` was requested and the output still exceeds it
    /// after every trimmable section was reduced to its floor.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub budget_exceeded: bool,
    /// Lines another spec added that this spec's version did not carry; the
    /// merge kept them (finding 62). Each names the file, lines, the adding
    /// spec and agent, and the seal command to remove them again on purpose.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stale_rewrite_notices: Vec<crate::convergence::survival::ConvergenceNotice>,
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecommendedAction {
    /// Machine-readable action type (e.g. "converge", "seal", "wait_for_dependency").
    pub action: String,
    /// Human/agent-readable explanation of what to do and why.
    pub message: String,
    /// Priority level: "high", "medium", or "low".
    pub priority: String,
}

// ── Caps and budget (ctx-budget) ─────────────────────────────────────

/// Default cap for every path list in context output.
pub const DEFAULT_MAX_FILES: usize = 50;

/// `--budget` never trims `recent_seals` below this many entries.
pub const BUDGET_SEAL_FLOOR: usize = 3;

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Size limits applied to a context dump after it is assembled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextLimits {
    /// Max entries per path list. `None` means unlimited (`--max-files 0`).
    pub max_files: Option<usize>,
    /// Target serialized size in bytes. `None` means no budget.
    pub budget: Option<usize>,
}

impl Default for ContextLimits {
    fn default() -> Self {
        ContextLimits {
            max_files: Some(DEFAULT_MAX_FILES),
            budget: None,
        }
    }
}

impl ContextLimits {
    /// Build limits from user input where `0` means unlimited.
    pub fn from_user(max_files: Option<usize>, budget: Option<usize>) -> Self {
        let max_files = match max_files {
            None => Some(DEFAULT_MAX_FILES),
            Some(0) => None,
            Some(n) => Some(n),
        };
        ContextLimits { max_files, budget }
    }
}

/// Ordering inputs for path lists: which paths matter most to the reader.
///
/// Pending/working-state paths: spec-touched first, then most recently
/// modified on disk. File scope: spec-touched first, then most recently sealed.
/// Path name breaks ties so output is deterministic.
#[derive(Debug, Clone, Default)]
pub struct FilePriority {
    /// Exact paths touched by (or declared for) the scoped spec(s).
    pub spec_files: HashSet<String>,
    /// Declared directory scopes (entries ending in `/`).
    pub spec_dirs: Vec<String>,
    /// Last modification time of changed files on disk.
    pub modified_at: HashMap<String, SystemTime>,
    /// 0 = most recently sealed path.
    pub sealed_rank: HashMap<String, usize>,
}

impl FilePriority {
    /// Is this path part of the scoped spec's file set?
    pub fn is_spec_file(&self, path: &str) -> bool {
        self.spec_files.contains(path) || self.spec_dirs.iter().any(|d| path.starts_with(d))
    }

    fn change_order(&self, a: &str, b: &str) -> Ordering {
        let key = |p: &str| {
            (
                !self.is_spec_file(p),
                Reverse(self.modified_at.get(p).copied()),
            )
        };
        key(a).cmp(&key(b)).then_with(|| a.cmp(b))
    }

    fn scope_order(&self, a: &str, b: &str) -> Ordering {
        let key = |p: &str| {
            (
                !self.is_spec_file(p),
                self.sealed_rank.get(p).copied().unwrap_or(usize::MAX),
            )
        };
        key(a).cmp(&key(b)).then_with(|| a.cmp(b))
    }
}

impl DiffSummary {
    fn sort_by_priority(&mut self, prio: &FilePriority) {
        self.files
            .sort_by(|a, b| prio.change_order(&a.path, &b.path));
    }

    /// Keep at most `max` file entries. Totals are never touched.
    fn truncate_files(&mut self, max: usize) {
        if self.files.len() > max {
            self.omitted += self.files.len() - max;
            self.files.truncate(max);
            self.truncated = true;
        }
    }
}

impl WorkingStateSummary {
    fn path_count(&self) -> usize {
        self.new_files.len() + self.modified_files.len() + self.deleted_files.len()
    }

    fn sort_by_priority(&mut self, prio: &FilePriority) {
        for list in [
            &mut self.new_files,
            &mut self.modified_files,
            &mut self.deleted_files,
        ] {
            list.sort_by(|a, b| prio.change_order(a, b));
        }
    }

    /// Keep the `max` highest-priority paths across all three lists.
    fn truncate_files(&mut self, max: usize, prio: &FilePriority) {
        let total = self.path_count();
        if total <= max {
            return;
        }
        if self.counts.is_none() {
            self.counts = Some(ChangeCounts {
                new: self.new_files.len(),
                modified: self.modified_files.len(),
                deleted: self.deleted_files.len(),
            });
        }
        let mut all: Vec<(String, FileStatus)> = Vec::with_capacity(total);
        all.extend(self.new_files.drain(..).map(|p| (p, FileStatus::New)));
        all.extend(
            self.modified_files
                .drain(..)
                .map(|p| (p, FileStatus::Modified)),
        );
        all.extend(
            self.deleted_files
                .drain(..)
                .map(|p| (p, FileStatus::Deleted)),
        );
        all.sort_by(|a, b| prio.change_order(&a.0, &b.0));
        all.truncate(max);
        for (path, status) in all {
            match status {
                FileStatus::New => self.new_files.push(path),
                FileStatus::Modified => self.modified_files.push(path),
                FileStatus::Deleted => self.deleted_files.push(path),
            }
        }
        self.omitted += total - max;
        self.truncated = true;
    }
}

impl ContextOutput {
    /// Order every path list by priority and cap it at `max_files`.
    ///
    /// Lists are ordered even when unlimited so a later budget trim always
    /// drops the least relevant paths first.
    pub fn apply_file_cap(&mut self, max_files: Option<usize>, prio: &FilePriority) {
        if let Some(pc) = self.pending_changes.as_mut() {
            pc.sort_by_priority(prio);
        }
        self.working_state.sort_by_priority(prio);
        self.file_scope.sort_by(|a, b| prio.scope_order(a, b));
        for activity in &mut self.agent_activity {
            activity.files_owned.sort_by(|a, b| prio.scope_order(a, b));
        }
        if let Some(max) = max_files {
            self.truncate_change_lists(max, prio);
            self.truncate_file_scope(max);
        }
    }

    /// Cap pending changes, working state, and per-agent ownership lists.
    fn truncate_change_lists(&mut self, max: usize, prio: &FilePriority) {
        if let Some(pc) = self.pending_changes.as_mut() {
            pc.truncate_files(max);
        }
        self.working_state.truncate_files(max, prio);
        for activity in &mut self.agent_activity {
            if activity.files_owned.len() > max {
                activity.files_owned_omitted += activity.files_owned.len() - max;
                activity.files_owned.truncate(max);
            }
        }
    }

    fn change_list_len(&self) -> usize {
        let pending = self.pending_changes.as_ref().map_or(0, |pc| pc.files.len());
        let owned = self
            .agent_activity
            .iter()
            .map(|a| a.files_owned.len())
            .max()
            .unwrap_or(0);
        pending.max(self.working_state.path_count()).max(owned)
    }

    fn truncate_file_scope(&mut self, max: usize) {
        if self.file_scope.len() > max {
            self.file_scope_omitted += self.file_scope.len() - max;
            self.file_scope.truncate(max);
            self.file_scope_truncated = true;
        }
    }

    /// Trim progressively until `measure(self) <= budget`.
    ///
    /// Order: change file lists (halving to zero), then `file_scope` (halving
    /// to zero), then `recent_seals` down to [`BUDGET_SEAL_FLOOR`], then
    /// `available_operations` (dropped whole). Never
    /// touches `all_specs`, `recommended_action`, `integration_risk`, or
    /// `chain_integrity`. Sets `budget_exceeded` if the floor still does not fit.
    /// Call [`ContextOutput::apply_file_cap`] first so lists are priority-ordered.
    pub fn fit_to_budget<F>(
        &mut self,
        budget: usize,
        prio: &FilePriority,
        measure: F,
    ) -> WritResult<()>
    where
        F: Fn(&ContextOutput) -> WritResult<usize>,
    {
        if measure(self)? <= budget {
            return Ok(());
        }
        let mut n = self.change_list_len();
        while n > 0 {
            n /= 2;
            self.truncate_change_lists(n, prio);
            if measure(self)? <= budget {
                return Ok(());
            }
        }
        let mut n = self.file_scope.len();
        while n > 0 {
            n /= 2;
            self.truncate_file_scope(n);
            if measure(self)? <= budget {
                return Ok(());
            }
        }
        while self.recent_seals.len() > BUDGET_SEAL_FLOOR {
            self.recent_seals.pop();
            if measure(self)? <= budget {
                return Ok(());
            }
        }
        if !self.available_operations.is_empty() {
            self.available_operations.clear();
            if measure(self)? <= budget {
                return Ok(());
            }
        }
        self.budget_exceeded = true;
        Ok(())
    }
}

// ── Brief view (ctx-brief) ───────────────────────────────────────────

/// Seals shown in the brief view.
pub const BRIEF_SEAL_COUNT: usize = 3;
/// Max active specs listed in the brief view.
pub const BRIEF_SPEC_CAP: usize = 20;
/// Max characters of a seal summary in the brief view.
pub const BRIEF_SUMMARY_CHARS: usize = 100;

/// One spec row in the brief view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BriefSpec {
    pub id: String,
    pub slug: String,
    pub status: String,
    /// Claiming agent, empty when unclaimed (kept as a string so TOON
    /// renders the spec list as one table).
    pub agent: String,
    pub seals: usize,
}

/// One seal row in the brief view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BriefSeal {
    pub id: String,
    pub agent: String,
    pub summary: String,
    pub at: String,
}

/// Exact pending-change counts in the brief view.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BriefPending {
    pub files: usize,
    pub new: usize,
    pub modified: usize,
    pub deleted: usize,
    pub additions: usize,
    pub deletions: usize,
}

/// Integration risk without the factor list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BriefRisk {
    pub level: String,
    pub score: u32,
}

/// The task-start view: enough to choose a spec and act, nothing per-file.
///
/// Built from a full [`ContextOutput`], so it reflects the same scope. Size
/// grows with spec count only; file and seal counts do not affect it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BriefContext {
    /// `full`, or the scoped spec id.
    pub scope: String,
    pub tracked: usize,
    /// Open specs (not complete), capped at [`BRIEF_SPEC_CAP`]. In spec
    /// scope, the scoped spec whatever its status.
    pub specs: Vec<BriefSpec>,
    /// Open specs beyond the cap.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub specs_omitted: usize,
    /// Completed specs, counted rather than listed.
    #[serde(default)]
    pub specs_complete: usize,
    pub seals: Vec<BriefSeal>,
    pub pending: BriefPending,
    pub risk: BriefRisk,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<RecommendedAction>,
    /// Chain verification result; absent when no seals are secured yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_ok: Option<bool>,
}

impl BriefContext {
    pub fn from_context(ctx: &ContextOutput) -> Self {
        let (specs, specs_omitted, specs_complete) = brief_specs(ctx);
        BriefContext {
            scope: ctx
                .active_spec
                .as_ref()
                .map_or_else(|| "full".to_string(), |s| s.id.clone()),
            tracked: ctx.tracked_files,
            specs,
            specs_omitted,
            specs_complete,
            seals: ctx
                .recent_seals
                .iter()
                .take(BRIEF_SEAL_COUNT)
                .map(brief_seal)
                .collect(),
            pending: brief_pending(ctx),
            risk: BriefRisk {
                level: ctx.integration_risk.level.clone(),
                score: ctx.integration_risk.score,
            },
            next: ctx.recommended_action.clone(),
            chain_ok: ctx.chain_integrity.as_ref().map(|c| c.valid),
        }
    }
}

/// Open specs (capped), omitted count, and completed count.
///
/// Full and agent scope list every non-complete spec (pending, in-progress,
/// blocked) so brief stays bounded as completed specs accumulate. Spec scope
/// always shows the scoped spec.
fn brief_specs(ctx: &ContextOutput) -> (Vec<BriefSpec>, usize, usize) {
    let Some(all) = ctx.all_specs.as_deref() else {
        let scoped = ctx.active_spec.iter().map(brief_spec).collect();
        return (scoped, 0, 0);
    };
    let complete = all
        .iter()
        .filter(|s| s.status == SpecStatus::Complete)
        .count();
    let open: Vec<&Spec> = all
        .iter()
        .filter(|s| s.status != SpecStatus::Complete)
        .collect();
    let omitted = open.len().saturating_sub(BRIEF_SPEC_CAP);
    let listed = open
        .into_iter()
        .take(BRIEF_SPEC_CAP)
        .map(brief_spec)
        .collect();
    (listed, omitted, complete)
}

fn brief_spec(spec: &Spec) -> BriefSpec {
    let status = match spec.status {
        SpecStatus::Pending => "pending",
        SpecStatus::InProgress => "in-progress",
        SpecStatus::Complete => "complete",
        SpecStatus::Blocked => "blocked",
    };
    BriefSpec {
        id: spec.id.clone(),
        slug: spec.slug.clone(),
        status: status.to_string(),
        agent: spec.claimed_by.clone().unwrap_or_default(),
        seals: spec.sealed_by.len(),
    }
}

fn brief_seal(seal: &SealSummary) -> BriefSeal {
    BriefSeal {
        id: seal.id.clone(),
        agent: seal.agent.clone(),
        summary: truncate_chars(&seal.summary, BRIEF_SUMMARY_CHARS),
        at: compact_timestamp(&seal.timestamp),
    }
}

/// RFC 3339 to whole-second UTC (`2026-10-04T23:38:04Z`); unparsable input
/// passes through unchanged.
fn compact_timestamp(ts: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|t| {
            t.with_timezone(&chrono::Utc)
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        })
        .unwrap_or_else(|_| ts.to_string())
}

fn brief_pending(ctx: &ContextOutput) -> BriefPending {
    let ws = &ctx.working_state;
    let counts = ws.counts.unwrap_or(ChangeCounts {
        new: ws.new_files.len(),
        modified: ws.modified_files.len(),
        deleted: ws.deleted_files.len(),
    });
    let (files, additions, deletions) = ctx.pending_changes.as_ref().map_or(
        (counts.new + counts.modified + counts.deleted, 0, 0),
        |pc| (pc.files_changed, pc.total_additions, pc.total_deletions),
    );
    BriefPending {
        files,
        new: counts.new,
        modified: counts.modified,
        deleted: counts.deleted,
        additions,
        deletions,
    }
}

/// Truncate on a char boundary, marking the cut with `...`.
fn truncate_chars(text: &str, max: usize) -> String {
    let first_line = text.lines().next().unwrap_or("");
    if first_line.chars().count() <= max && first_line.len() == text.len() {
        return text.to_string();
    }
    let kept: String = first_line.chars().take(max.saturating_sub(3)).collect();
    format!("{kept}...")
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
            truncated: false,
            omitted: 0,
            counts: None,
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(!json.contains("new_files"));
        assert!(!json.contains("modified_files"));
        assert!(!json.contains("deleted_files"));
        assert!(json.contains("clean"));
        assert!(json.contains("tracked_count"));
    }

    // ── Caps and budget (ctx-budget) ─────────────────────────

    fn diff_entry(path: &str) -> FileDiffSummary {
        FileDiffSummary {
            path: path.to_string(),
            change_type: "modified".into(),
            additions: 1,
            deletions: 1,
        }
    }

    fn seal_summary(i: usize) -> SealSummary {
        SealSummary {
            id: format!("seal{i:08}"),
            timestamp: "2026-10-04T00:00:00Z".into(),
            agent: "amis".into(),
            summary: "x".repeat(200),
            files_changed: 1,
            spec_id: None,
            status: "in-progress".into(),
            verification: None,
            changed_paths: vec![],
        }
    }

    /// A context with `n` modified files, `n` tracked paths, and 10 seals.
    fn big_context(n: usize) -> ContextOutput {
        let paths: Vec<String> = (0..n).map(|i| format!("src/f{i:04}.rs")).collect();
        let mut ctx: ContextOutput = serde_json::from_value(serde_json::json!({
            "writ_version": "test",
            "working_state": {"clean": false, "tracked_count": n},
            "recent_seals": [],
            "file_scope": [],
            "tracked_files": n,
            "available_operations": [],
        }))
        .unwrap();
        ctx.working_state.modified_files = paths.clone();
        ctx.pending_changes = Some(DiffSummary {
            files_changed: n,
            total_additions: n,
            total_deletions: n,
            files: paths.iter().map(|p| diff_entry(p)).collect(),
            truncated: false,
            omitted: 0,
        });
        ctx.file_scope = paths;
        ctx.recent_seals = (0..10).map(seal_summary).collect();
        ctx
    }

    fn json_len(c: &ContextOutput) -> WritResult<usize> {
        Ok(serde_json::to_string(c)?.len())
    }

    #[test]
    fn cap_truncates_lists_and_keeps_totals_exact() {
        let mut ctx = big_context(120);
        ctx.apply_file_cap(Some(50), &FilePriority::default());

        let pc = ctx.pending_changes.as_ref().unwrap();
        assert_eq!(pc.files.len(), 50);
        assert!(pc.truncated);
        assert_eq!(pc.omitted, 70);
        assert_eq!(pc.files_changed, 120);
        assert_eq!(pc.total_additions, 120);

        let ws = &ctx.working_state;
        assert_eq!(ws.modified_files.len(), 50);
        assert!(ws.truncated);
        assert_eq!(ws.omitted, 70);
        assert_eq!(
            ws.counts,
            Some(ChangeCounts {
                new: 0,
                modified: 120,
                deleted: 0
            })
        );

        assert_eq!(ctx.file_scope.len(), 50);
        assert!(ctx.file_scope_truncated);
        assert_eq!(ctx.file_scope_omitted, 70);
        assert_eq!(ctx.tracked_files, 120);
    }

    #[test]
    fn cap_under_limit_emits_no_markers() {
        let mut ctx = big_context(10);
        ctx.apply_file_cap(Some(50), &FilePriority::default());
        let json = serde_json::to_string(&ctx).unwrap();
        assert!(!json.contains("truncated"));
        assert!(!json.contains("omitted"));
        assert!(!json.contains("counts"));
    }

    #[test]
    fn cap_none_is_unlimited() {
        let mut ctx = big_context(300);
        ctx.apply_file_cap(None, &FilePriority::default());
        assert_eq!(ctx.file_scope.len(), 300);
        assert_eq!(ctx.pending_changes.unwrap().files.len(), 300);
    }

    #[test]
    fn cap_orders_spec_files_first_then_recent_mtime() {
        let mut ctx = big_context(100);
        let mut prio = FilePriority::default();
        prio.spec_files.insert("src/f0099.rs".into());
        prio.spec_dirs.push("src/f009".into());
        let base = SystemTime::UNIX_EPOCH;
        prio.modified_at.insert(
            "src/f0042.rs".into(),
            base + std::time::Duration::from_secs(100),
        );
        prio.modified_at.insert(
            "src/f0007.rs".into(),
            base + std::time::Duration::from_secs(50),
        );
        ctx.apply_file_cap(Some(13), &prio);

        let files: Vec<&str> = ctx.pending_changes.as_ref().unwrap().files[..13]
            .iter()
            .map(|f| f.path.as_str())
            .collect();
        // f0090..f0099 match the spec scope (dir prefix + exact), then mtime order.
        assert!(files[..10].iter().all(|p| p.starts_with("src/f009")));
        assert_eq!(files[10], "src/f0042.rs");
        assert_eq!(files[11], "src/f0007.rs");
        assert_eq!(ctx.working_state.modified_files[10], "src/f0042.rs");
    }

    #[test]
    fn file_scope_orders_spec_files_then_most_recently_sealed() {
        let mut ctx = big_context(100);
        let mut prio = FilePriority::default();
        prio.spec_files.insert("src/f0050.rs".into());
        prio.sealed_rank.insert("src/f0080.rs".into(), 0);
        prio.sealed_rank.insert("src/f0003.rs".into(), 1);
        ctx.apply_file_cap(Some(4), &prio);
        assert_eq!(
            ctx.file_scope,
            vec![
                "src/f0050.rs",
                "src/f0080.rs",
                "src/f0003.rs",
                "src/f0000.rs"
            ]
        );
    }

    #[test]
    fn cap_mixes_new_modified_deleted_by_priority() {
        let mut ctx = big_context(0);
        ctx.working_state.new_files = vec!["a_new.rs".into(), "b_new.rs".into()];
        ctx.working_state.modified_files = vec!["c_mod.rs".into()];
        ctx.working_state.deleted_files = vec!["d_del.rs".into()];
        let mut prio = FilePriority::default();
        prio.spec_files.insert("d_del.rs".into());
        ctx.apply_file_cap(Some(2), &prio);
        let ws = &ctx.working_state;
        assert_eq!(ws.deleted_files, vec!["d_del.rs"]);
        assert_eq!(ws.new_files, vec!["a_new.rs"]);
        assert!(ws.modified_files.is_empty());
        assert_eq!(ws.omitted, 2);
        assert_eq!(ws.counts.unwrap().new, 2);
    }

    #[test]
    fn budget_trims_file_lists_before_file_scope() {
        let mut ctx = big_context(200);
        let prio = FilePriority::default();
        ctx.apply_file_cap(None, &prio);
        // Room for the file scope but not for the change lists.
        let budget = json_len(&ctx).unwrap() - 2_000;
        ctx.fit_to_budget(budget, &prio, json_len).unwrap();
        assert!(json_len(&ctx).unwrap() <= budget);
        assert!(ctx.pending_changes.as_ref().unwrap().truncated);
        assert!(!ctx.file_scope_truncated);
        assert_eq!(ctx.recent_seals.len(), 10);
        assert!(!ctx.budget_exceeded);
    }

    #[test]
    fn budget_trims_seals_last_and_never_below_floor() {
        let mut ctx = big_context(200);
        let prio = FilePriority::default();
        ctx.apply_file_cap(None, &prio);
        ctx.fit_to_budget(1_000, &prio, json_len).unwrap();
        assert!(ctx.pending_changes.as_ref().unwrap().files.is_empty());
        assert!(ctx.file_scope.is_empty());
        assert_eq!(ctx.file_scope_omitted, 200);
        assert_eq!(ctx.recent_seals.len(), BUDGET_SEAL_FLOOR);
        assert!(ctx.budget_exceeded);
        // Totals survive every trim.
        assert_eq!(ctx.pending_changes.as_ref().unwrap().files_changed, 200);
        assert_eq!(ctx.tracked_files, 200);
    }

    #[test]
    fn budget_never_drops_protected_sections() {
        let mut ctx = big_context(50);
        ctx.all_specs = Some(vec![]);
        ctx.recommended_action = Some(RecommendedAction {
            action: "seal".into(),
            message: "seal your work".into(),
            priority: "high".into(),
        });
        ctx.chain_integrity = Some(ChainIntegritySummary {
            valid: true,
            total_seals: 1,
            verified: 1,
            unsecured: 0,
            failures: 0,
        });
        let prio = FilePriority::default();
        ctx.fit_to_budget(10, &prio, json_len).unwrap();
        assert!(ctx.all_specs.is_some());
        assert!(ctx.recommended_action.is_some());
        assert!(ctx.chain_integrity.is_some());
        assert!(ctx.budget_exceeded);
    }

    #[test]
    fn budget_already_met_changes_nothing() {
        let mut ctx = big_context(5);
        let before = json_len(&ctx).unwrap();
        ctx.fit_to_budget(before, &FilePriority::default(), json_len)
            .unwrap();
        assert_eq!(json_len(&ctx).unwrap(), before);
        assert!(!ctx.budget_exceeded);
    }

    #[test]
    fn limits_from_user_maps_zero_to_unlimited() {
        assert_eq!(
            ContextLimits::from_user(None, None).max_files,
            Some(DEFAULT_MAX_FILES)
        );
        assert_eq!(ContextLimits::from_user(Some(0), None).max_files, None);
        assert_eq!(ContextLimits::from_user(Some(7), Some(10)).budget, Some(10));
    }

    #[test]
    fn budget_drops_available_operations_after_seal_floor() {
        let mut ctx = big_context(20);
        ctx.available_operations = (0..200).map(|i| format!("operation_{i}()")).collect();
        let prio = FilePriority::default();
        ctx.apply_file_cap(None, &prio);
        let mut floor = ctx.clone();
        floor.pending_changes.as_mut().unwrap().files.clear();
        floor.working_state.modified_files.clear();
        floor.file_scope.clear();
        floor.recent_seals.truncate(BUDGET_SEAL_FLOOR);
        floor.available_operations.clear();
        let budget = json_len(&floor).unwrap() + 200;

        ctx.fit_to_budget(budget, &prio, json_len).unwrap();
        assert!(ctx.available_operations.is_empty());
        assert_eq!(ctx.recent_seals.len(), BUDGET_SEAL_FLOOR);
        assert!(!ctx.budget_exceeded);
        assert!(json_len(&ctx).unwrap() <= budget);
    }

    // ── Brief view (ctx-brief) ──────────────────────────────

    fn spec_row(id: &str, status: SpecStatus, agent: Option<&str>, seals: usize) -> Spec {
        let mut spec = Spec::new(id.into(), format!("Title {id}"), String::new());
        spec.status = status;
        spec.claimed_by = agent.map(String::from);
        spec.sealed_by = (0..seals).map(|i| format!("seal{i}")).collect();
        spec
    }

    #[test]
    fn brief_contains_required_sections() {
        let mut ctx = big_context(500);
        ctx.all_specs = Some(vec![
            spec_row("a", SpecStatus::InProgress, Some("amis"), 2),
            spec_row("b", SpecStatus::Pending, None, 0),
        ]);
        ctx.integration_risk = IntegrationRisk::compute(1, 0, 0, 0);
        ctx.recommended_action = Some(RecommendedAction {
            action: "seal".into(),
            message: "seal your work".into(),
            priority: "medium".into(),
        });
        ctx.chain_integrity = Some(ChainIntegritySummary {
            valid: true,
            total_seals: 10,
            verified: 10,
            unsecured: 0,
            failures: 0,
        });
        ctx.apply_file_cap(Some(50), &FilePriority::default());

        let brief = BriefContext::from_context(&ctx);
        assert_eq!(brief.scope, "full");
        assert_eq!(brief.specs.len(), 2);
        assert_eq!(brief.specs[0].status, "in-progress");
        assert_eq!(brief.specs[0].agent, "amis");
        assert_eq!(brief.specs[0].seals, 2);
        assert_eq!(brief.specs[1].agent, "");
        assert_eq!(brief.seals.len(), BRIEF_SEAL_COUNT);
        // Counts are exact even though the lists were capped at 50.
        assert_eq!(brief.pending.files, 500);
        assert_eq!(brief.pending.modified, 500);
        assert_eq!(brief.pending.additions, 500);
        assert_eq!(brief.risk.level, "medium");
        assert_eq!(brief.next.as_ref().unwrap().action, "seal");
        assert_eq!(brief.chain_ok, Some(true));
    }

    #[test]
    fn brief_size_is_independent_of_file_count() {
        let small = BriefContext::from_context(&big_context(10));
        let large = BriefContext::from_context(&big_context(5_000));
        let len = |b: &BriefContext| serde_json::to_string(b).unwrap().len();
        // Only digit widths in the counts differ.
        assert!(len(&large) - len(&small) < 20);
        assert!(len(&large) < 2048);
    }

    #[test]
    fn brief_truncates_long_and_multiline_summaries() {
        assert_eq!(truncate_chars("short", 100), "short");
        let long = "x".repeat(300);
        let cut = truncate_chars(&long, BRIEF_SUMMARY_CHARS);
        assert_eq!(cut.chars().count(), BRIEF_SUMMARY_CHARS);
        assert!(cut.ends_with("..."));
        assert_eq!(truncate_chars("line one\nline two", 100), "line one...");
        // Multibyte characters never split.
        assert_eq!(truncate_chars(&"é".repeat(10), 5), "éé...");
    }

    #[test]
    fn brief_spec_scope_uses_active_spec() {
        let mut ctx = big_context(0);
        ctx.active_spec = Some(spec_row("only", SpecStatus::Blocked, Some("bri"), 1));
        let brief = BriefContext::from_context(&ctx);
        assert_eq!(brief.scope, "only");
        assert_eq!(brief.specs.len(), 1);
        assert_eq!(brief.specs[0].status, "blocked");
        assert_eq!(brief.chain_ok, None);
        assert!(brief.next.is_none());
    }

    #[test]
    fn brief_timestamps_are_whole_second_utc() {
        assert_eq!(
            compact_timestamp("2026-10-04T23:38:04.999643+00:00"),
            "2026-10-04T23:38:04Z"
        );
        assert_eq!(
            compact_timestamp("2026-10-04T19:38:04-04:00"),
            "2026-10-04T23:38:04Z"
        );
        assert_eq!(compact_timestamp("not a time"), "not a time");
    }

    #[test]
    fn brief_lists_open_specs_and_counts_completed() {
        let mut ctx = big_context(0);
        ctx.all_specs = Some(vec![
            spec_row("done1", SpecStatus::Complete, Some("a"), 3),
            spec_row("wip", SpecStatus::InProgress, Some("a"), 1),
            spec_row("done2", SpecStatus::Complete, None, 1),
            spec_row("todo", SpecStatus::Pending, None, 0),
            spec_row("stuck", SpecStatus::Blocked, Some("b"), 0),
        ]);
        let brief = BriefContext::from_context(&ctx);
        let ids: Vec<&str> = brief.specs.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["wip", "todo", "stuck"]);
        assert_eq!(brief.specs_complete, 2);
        assert_eq!(brief.specs_omitted, 0);
        let json = serde_json::to_string(&brief).unwrap();
        assert!(!json.contains("specs_omitted"));
    }

    #[test]
    fn brief_caps_open_specs_and_stays_bounded() {
        let mut ctx = big_context(0);
        let mut specs: Vec<Spec> = (0..30)
            .map(|i| spec_row(&format!("open{i:02}"), SpecStatus::Pending, None, 0))
            .collect();
        specs.extend(
            (0..500).map(|i| spec_row(&format!("done{i:03}"), SpecStatus::Complete, None, 2)),
        );
        ctx.all_specs = Some(specs);
        let brief = BriefContext::from_context(&ctx);
        assert_eq!(brief.specs.len(), BRIEF_SPEC_CAP);
        assert_eq!(brief.specs_omitted, 10);
        assert_eq!(brief.specs_complete, 500);
        assert_eq!(brief.specs[0].id, "open00");
    }
}
