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
    pub depends_on: Vec<String>,
    /// Files expected to be affected.
    pub file_scope: Vec<String>,
    /// When the spec was created.
    pub created_at: DateTime<Utc>,
    /// When the spec was last updated.
    pub updated_at: DateTime<Utc>,
    /// IDs of seals linked to this spec.
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
    #[serde(default)]
    pub lifecycle_state: LifecycleState,
    /// Timestamp of last meaningful activity (seal referencing this spec).
    /// Used by GC stale detection. Defaults to `created_at` for existing specs.
    #[serde(default = "Utc::now")]
    pub last_activity: DateTime<Utc>,
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
        }
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
}
