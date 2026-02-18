//! Specs — first-class requirement documents.
//!
//! A spec defines what needs to be built or changed, with acceptance
//! criteria and dependency tracking. Specs are stored in `.writ/specs/`
//! and are the unit of work in writ (replacing branches).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Completion status of a spec.
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

/// A requirement specification tracked by writ.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}
