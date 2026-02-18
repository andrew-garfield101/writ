//! Context — AI-native structured context dump.
//!
//! Produces a single structured output optimized for LLM consumption,
//! combining spec details, recent seal history, working state, and
//! pending changes into one token-efficient blob.

use serde::{Deserialize, Serialize};

use crate::diff::DiffOutput;
use crate::seal::Seal;
use crate::spec::Spec;
use crate::state::{FileStatus, WorkingState};

/// Scope of context to include.
#[derive(Debug, Clone)]
pub enum ContextScope {
    /// Full repository context.
    Full,
    /// Scoped to a specific spec and its related files/seals.
    Spec(String),
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
}

impl SealSummary {
    /// Create a compact summary from a full Seal.
    pub fn from_seal(seal: &Seal) -> Self {
        SealSummary {
            id: seal.id[..12].to_string(),
            timestamp: seal.timestamp.to_rfc3339(),
            agent: seal.agent.id.clone(),
            summary: seal.summary.clone(),
            files_changed: seal.changes.len(),
            spec_id: seal.spec_id.clone(),
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

    /// Files in scope.
    pub file_scope: Vec<String>,

    /// Total tracked file count.
    pub tracked_files: usize,
}
