//! Specs — first-class requirement documents.
//!
//! A spec defines what needs to be built or changed, with acceptance
//! criteria and dependency tracking. Specs are stored in `.writ/specs/`
//! and are the unit of work in writ (replacing branches).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Completion status of a spec (user-facing).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum SpecStatus {
    /// Not started yet.
    Pending,
    /// Work is actively happening.
    InProgress,
    /// All acceptance criteria met.
    Complete,
    /// Blocked by a dependency.
    Blocked,
}

/// Git commit promotion state for the round-trip workflow.
///
/// Tracks whether completed spec work has been committed to git
/// and pushed to a remote. Separate from `SpecStatus` (work status)
/// and `LifecycleState` (GC status).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CommitState {
    /// Work not yet committed to git.
    Uncommitted,
    /// Committed to local git repository.
    Committed,
    /// Pushed to remote.
    Pushed,
}

impl Default for CommitState {
    fn default() -> Self {
        CommitState::Uncommitted
    }
}

/// GC lifecycle state (separate from user-facing status).
///
/// Added as an additive field — existing repos without this field
/// deserialize as `Active` via `#[serde(default)]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LifecycleState {
    /// Spec is active and in use.
    Active,
    /// No seal activity within stale timeout.
    Stale,
    /// All work done, awaiting retention-based cleanup.
    Completed,
    /// Manually or automatically cancelled.
    Cancelled,
    /// Past retention period, metadata only.
    Archived,
}

impl Default for LifecycleState {
    fn default() -> Self {
        LifecycleState::Active
    }
}

impl LifecycleState {
    fn is_default(&self) -> bool {
        matches!(self, LifecycleState::Active)
    }
}

impl CommitState {
    fn is_default(&self) -> bool {
        matches!(self, CommitState::Uncommitted)
    }
}

/// A requirement specification tracked by writ.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    /// Unique spec identifier (e.g. "auth-migration").
    pub id: String,
    /// Human-readable title.
    pub title: String,
    /// Detailed description of the requirement.
    pub description: String,
    /// Current status.
    pub status: SpecStatus,
    /// IDs of specs this one depends on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Files expected to be affected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_scope: Vec<String>,
    /// When the spec was created.
    pub created_at: DateTime<Utc>,
    /// When the spec was last updated.
    pub updated_at: DateTime<Utc>,
    /// IDs of seals linked to this spec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sealed_by: Vec<String>,
    /// Testable conditions for spec completion.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance_criteria: Vec<String>,
    /// Key architectural decisions, constraints, or rationale.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub design_notes: Vec<String>,
    /// Languages, frameworks, dependencies relevant to this spec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tech_stack: Vec<String>,
    /// GC lifecycle state — separate from user-facing `status`.
    /// Defaults to `Active` for backward compatibility with existing repos.
    #[serde(default, skip_serializing_if = "LifecycleState::is_default")]
    pub lifecycle_state: LifecycleState,
    /// Timestamp of last meaningful activity (seal referencing this spec).
    /// Used by GC stale detection. Defaults to `created_at` for existing specs.
    #[serde(default = "Utc::now")]
    pub last_activity: DateTime<Utc>,
    /// Summary provided when spec was completed via `writ spec done`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_summary: Option<String>,
    /// Git promotion state — tracks commit/push progress.
    #[serde(default, skip_serializing_if = "CommitState::is_default")]
    pub commit_state: CommitState,
    /// When the spec was marked complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    /// Git commit hash once committed via `writ finish`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_hash: Option<String>,
    /// When the spec was committed to git.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_at: Option<DateTime<Utc>>,
    /// Workspace this spec is assigned to. None = globally visible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

impl std::str::FromStr for SpecStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "pending" => Ok(SpecStatus::Pending),
            "in-progress" | "inprogress" | "in_progress" => Ok(SpecStatus::InProgress),
            "complete" | "completed" => Ok(SpecStatus::Complete),
            "blocked" => Ok(SpecStatus::Blocked),
            other => Err(format!("unknown spec status: '{other}'")),
        }
    }
}

/// Fields that can be updated on an existing spec.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpecUpdate {
    /// New status (if Some).
    pub status: Option<SpecStatus>,
    /// Replacement dependency list (if Some).
    pub depends_on: Option<Vec<String>>,
    /// Replacement file scope list (if Some).
    pub file_scope: Option<Vec<String>>,
    /// Replacement acceptance criteria (if Some).
    pub acceptance_criteria: Option<Vec<String>>,
    /// Replacement design notes (if Some).
    pub design_notes: Option<Vec<String>>,
    /// Replacement tech stack (if Some).
    pub tech_stack: Option<Vec<String>>,
}

impl Spec {
    /// Create a new spec with the given ID and title.
    pub fn new(id: String, title: String, description: String) -> Self {
        let now = Utc::now();
        Self {
            id,
            title,
            description,
            status: SpecStatus::Pending,
            depends_on: Vec::new(),
            file_scope: Vec::new(),
            created_at: now,
            updated_at: now,
            sealed_by: Vec::new(),
            acceptance_criteria: Vec::new(),
            design_notes: Vec::new(),
            tech_stack: Vec::new(),
            lifecycle_state: LifecycleState::Active,
            last_activity: now,
            completion_summary: None,
            commit_state: CommitState::Uncommitted,
            completed_at: None,
            commit_hash: None,
            committed_at: None,
            workspace: None,
        }
    }

    /// Returns true if this spec is complete and not yet committed to git.
    pub fn is_committable(&self) -> bool {
        self.status == SpecStatus::Complete && self.commit_state == CommitState::Uncommitted
    }

    /// Record that this spec's work was committed to git.
    pub fn mark_committed(&mut self, hash: String) {
        let now = Utc::now();
        self.commit_state = CommitState::Committed;
        self.commit_hash = Some(hash);
        self.committed_at = Some(now);
        self.updated_at = now;
    }

    /// Mark this spec as pushed to remote.
    pub fn mark_pushed(&mut self) {
        self.commit_state = CommitState::Pushed;
        self.updated_at = Utc::now();
    }

    /// Reopen a completed spec for further work.
    /// Clears commit state but preserves completion_summary for history.
    pub fn reopen(&mut self) {
        self.status = SpecStatus::InProgress;
        self.commit_state = CommitState::Uncommitted;
        self.completed_at = None;
        self.commit_hash = None;
        self.committed_at = None;
        self.updated_at = Utc::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spec_status_from_str() {
        assert_eq!("pending".parse::<SpecStatus>(), Ok(SpecStatus::Pending));
        assert_eq!(
            "in-progress".parse::<SpecStatus>(),
            Ok(SpecStatus::InProgress)
        );
        assert_eq!("complete".parse::<SpecStatus>(), Ok(SpecStatus::Complete));
        assert_eq!("blocked".parse::<SpecStatus>(), Ok(SpecStatus::Blocked));
        assert!("unknown".parse::<SpecStatus>().is_err());
    }

    #[test]
    fn test_lifecycle_state_default_is_active() {
        assert_eq!(LifecycleState::default(), LifecycleState::Active);
    }

    #[test]
    fn test_spec_new_has_active_lifecycle() {
        let spec = Spec::new("test".into(), "Test".into(), "desc".into());
        assert_eq!(spec.lifecycle_state, LifecycleState::Active);
    }

    #[test]
    fn test_backward_compat_missing_lifecycle_state() {
        // Simulate a spec from a pre-GC repo (no lifecycle_state or last_activity)
        let json = r#"{
            "id": "old-spec",
            "title": "Old Spec",
            "description": "from before GC sprint",
            "status": "in-progress",
            "depends_on": [],
            "file_scope": [],
            "created_at": "2026-02-20T00:00:00Z",
            "updated_at": "2026-02-20T00:00:00Z",
            "sealed_by": []
        }"#;
        let spec: Spec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.lifecycle_state, LifecycleState::Active);
        assert_eq!(spec.status, SpecStatus::InProgress);
    }

    #[test]
    fn test_lifecycle_state_serialization_roundtrip() {
        let spec = Spec::new("test".into(), "Test".into(), "desc".into());
        let json = serde_json::to_string(&spec).unwrap();
        let recovered: Spec = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.lifecycle_state, LifecycleState::Active);
    }

    #[test]
    fn test_commit_state_default_is_uncommitted() {
        assert_eq!(CommitState::default(), CommitState::Uncommitted);
    }

    #[test]
    fn test_commit_state_serde_values() {
        assert_eq!(
            serde_json::to_string(&CommitState::Uncommitted).unwrap(),
            "\"uncommitted\""
        );
        assert_eq!(
            serde_json::to_string(&CommitState::Committed).unwrap(),
            "\"committed\""
        );
        assert_eq!(
            serde_json::to_string(&CommitState::Pushed).unwrap(),
            "\"pushed\""
        );
    }

    #[test]
    fn test_new_spec_has_uncommitted_state() {
        let spec = Spec::new("test".into(), "Test".into(), "desc".into());
        assert_eq!(spec.commit_state, CommitState::Uncommitted);
        assert!(spec.completion_summary.is_none());
        assert!(spec.completed_at.is_none());
        assert!(spec.commit_hash.is_none());
        assert!(spec.committed_at.is_none());
    }

    #[test]
    fn test_is_committable_complete_and_uncommitted() {
        let mut spec = Spec::new("test".into(), "Test".into(), "desc".into());
        spec.status = SpecStatus::Complete;
        assert!(spec.is_committable());
    }

    #[test]
    fn test_is_committable_false_when_not_complete() {
        let spec = Spec::new("test".into(), "Test".into(), "desc".into());
        assert!(!spec.is_committable()); // Pending
    }

    #[test]
    fn test_is_committable_false_when_already_committed() {
        let mut spec = Spec::new("test".into(), "Test".into(), "desc".into());
        spec.status = SpecStatus::Complete;
        spec.mark_committed("abc123".into());
        assert!(!spec.is_committable());
    }

    #[test]
    fn test_mark_committed() {
        let mut spec = Spec::new("test".into(), "Test".into(), "desc".into());
        spec.status = SpecStatus::Complete;
        spec.mark_committed("abc123def".into());

        assert_eq!(spec.commit_state, CommitState::Committed);
        assert_eq!(spec.commit_hash.as_deref(), Some("abc123def"));
        assert!(spec.committed_at.is_some());
    }

    #[test]
    fn test_mark_pushed() {
        let mut spec = Spec::new("test".into(), "Test".into(), "desc".into());
        spec.status = SpecStatus::Complete;
        spec.mark_committed("abc123".into());
        spec.mark_pushed();

        assert_eq!(spec.commit_state, CommitState::Pushed);
    }

    #[test]
    fn test_reopen_clears_commit_state() {
        let mut spec = Spec::new("test".into(), "Test".into(), "desc".into());
        spec.status = SpecStatus::Complete;
        spec.completion_summary = Some("All done".into());
        spec.completed_at = Some(Utc::now());
        spec.mark_committed("abc123".into());

        spec.reopen();

        assert_eq!(spec.status, SpecStatus::InProgress);
        assert_eq!(spec.commit_state, CommitState::Uncommitted);
        assert!(spec.completed_at.is_none());
        assert!(spec.commit_hash.is_none());
        assert!(spec.committed_at.is_none());
        // Summary preserved for history
        assert_eq!(spec.completion_summary.as_deref(), Some("All done"));
    }

    #[test]
    fn test_backward_compat_missing_commit_fields() {
        // Old spec JSON without any of the new round-trip fields
        let json = r#"{
            "id": "old-spec",
            "title": "Old Spec",
            "description": "from before round-trip sprint",
            "status": "complete",
            "depends_on": [],
            "file_scope": [],
            "created_at": "2026-02-20T00:00:00Z",
            "updated_at": "2026-02-20T00:00:00Z",
            "sealed_by": []
        }"#;
        let spec: Spec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.commit_state, CommitState::Uncommitted);
        assert!(spec.completion_summary.is_none());
        assert!(spec.completed_at.is_none());
        assert!(spec.commit_hash.is_none());
        assert!(spec.committed_at.is_none());
    }

    #[test]
    fn test_roundtrip_with_commit_fields() {
        let mut spec = Spec::new("rt".into(), "Round Trip".into(), "test".into());
        spec.status = SpecStatus::Complete;
        spec.completion_summary = Some("Implemented feature X".into());
        spec.completed_at = Some(Utc::now());
        spec.mark_committed("deadbeef".into());

        let json = serde_json::to_string(&spec).unwrap();
        let recovered: Spec = serde_json::from_str(&json).unwrap();

        assert_eq!(recovered.commit_state, CommitState::Committed);
        assert_eq!(
            recovered.completion_summary.as_deref(),
            Some("Implemented feature X")
        );
        assert_eq!(recovered.commit_hash.as_deref(), Some("deadbeef"));
        assert!(recovered.completed_at.is_some());
        assert!(recovered.committed_at.is_some());
    }

    #[test]
    fn test_lifecycle_state_serde_values() {
        assert_eq!(
            serde_json::to_string(&LifecycleState::Active).unwrap(),
            "\"active\""
        );
        assert_eq!(
            serde_json::to_string(&LifecycleState::Stale).unwrap(),
            "\"stale\""
        );
        assert_eq!(
            serde_json::to_string(&LifecycleState::Completed).unwrap(),
            "\"completed\""
        );
        assert_eq!(
            serde_json::to_string(&LifecycleState::Cancelled).unwrap(),
            "\"cancelled\""
        );
        assert_eq!(
            serde_json::to_string(&LifecycleState::Archived).unwrap(),
            "\"archived\""
        );
    }

    // --- Workspace field tests (WS.12) ---

    #[test]
    fn test_new_spec_has_no_workspace() {
        let spec = Spec::new("test".into(), "Test".into(), "desc".into());
        assert!(spec.workspace.is_none());
    }

    #[test]
    fn test_spec_workspace_serialization_roundtrip() {
        let mut spec = Spec::new("ws-test".into(), "WS Test".into(), "desc".into());
        spec.workspace = Some("auth-team".into());

        let json = serde_json::to_string(&spec).unwrap();
        assert!(json.contains("\"workspace\":\"auth-team\""));

        let recovered: Spec = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.workspace.as_deref(), Some("auth-team"));
    }

    #[test]
    fn test_spec_workspace_none_not_serialized() {
        let spec = Spec::new("test".into(), "Test".into(), "desc".into());
        let json = serde_json::to_string(&spec).unwrap();
        assert!(
            !json.contains("workspace"),
            "workspace=None should be skipped in serialization"
        );
    }

    #[test]
    fn test_legacy_spec_without_workspace_deserializes_to_none() {
        let json = r#"{
            "id": "legacy-spec",
            "title": "Legacy",
            "description": "from before workspace sprint",
            "status": "in-progress",
            "depends_on": [],
            "file_scope": [],
            "created_at": "2026-02-20T00:00:00Z",
            "updated_at": "2026-02-20T00:00:00Z",
            "sealed_by": []
        }"#;
        let spec: Spec = serde_json::from_str(json).unwrap();
        assert!(
            spec.workspace.is_none(),
            "legacy spec should have workspace=None"
        );
    }
}
