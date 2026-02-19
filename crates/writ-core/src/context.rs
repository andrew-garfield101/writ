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
}

/// Optional filters applied to the seal history in context output.
#[derive(Debug, Clone, Default)]
pub struct ContextFilter {
    /// Only include seals with this task status.
    pub status: Option<TaskStatus>,
    /// Only include seals by this agent ID.
    pub agent: Option<String>,
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
            changed_paths: seal.changes.iter().map(|c| c.path.clone()).collect(),
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

/// The full context output, optimized for LLM consumption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextOutput {
    /// Writ version marker for LLM parsing.
    pub writ_version: String,

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

    /// Available writ operations for agent discoverability.
    pub available_operations: Vec<String>,
}
