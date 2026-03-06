//! Status output — fleet-aware progress overview for the round-trip workflow.
//!
//! `writ status` provides a high-level view of agent activity, spec progress,
//! and commit readiness. Complements `writ state` (low-level plumbing) with
//! porcelain-level awareness.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// High-level project status for the round-trip workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusOutput {
    /// Project name from config.
    pub project_name: String,
    /// When this status was generated.
    pub timestamp: DateTime<Utc>,
    /// Agent activity summary (counts).
    pub agents: AgentSummary,
    /// Specs currently in progress.
    pub specs_in_progress: Vec<SpecBrief>,
    /// Specs completed but not yet committed to git.
    pub specs_completed: Vec<SpecBrief>,
    /// Specs that have been committed to git.
    pub specs_committed: Vec<SpecBrief>,
    /// Total files changed across completed specs.
    pub total_files_changed: usize,
    /// Specs flagged as stale (no activity past timeout).
    pub stale_specs: Vec<SpecBrief>,
    /// Configured workflow commit mode (user/propose/auto).
    pub commit_mode: String,
}

/// Summary of agent counts by activity state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSummary {
    /// Agents with at least one in-progress spec.
    pub active: usize,
    /// Agents whose specs are all complete.
    pub done: usize,
    /// Agents with specs but no recent activity (all stale).
    pub idle: usize,
    /// Total unique agents seen across all specs.
    pub total: usize,
}

/// Brief spec info for status display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecBrief {
    /// Spec ID.
    pub id: String,
    /// Spec title.
    pub title: String,
    /// Primary agent working on this spec (from most recent seal or spec creator).
    pub agent: String,
    /// Number of seals linked to this spec.
    pub seal_count: usize,
    /// Number of files changed in this spec's seals.
    pub files_changed: usize,
    /// Most recent activity timestamp.
    pub last_activity: DateTime<Utc>,
    /// Human-readable status label.
    pub status: String,
    /// Optional completion summary (from `writ spec done -s`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_summary: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_output_serializes() {
        let status = StatusOutput {
            project_name: "test-project".into(),
            timestamp: Utc::now(),
            agents: AgentSummary {
                active: 2,
                done: 1,
                idle: 0,
                total: 3,
            },
            specs_in_progress: vec![],
            specs_completed: vec![],
            specs_committed: vec![],
            total_files_changed: 0,
            stale_specs: vec![],
            commit_mode: "user".into(),
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("test-project"));
        assert!(json.contains("\"active\":2"));
    }

    #[test]
    fn test_spec_brief_serializes() {
        let brief = SpecBrief {
            id: "S-001".into(),
            title: "Auth module".into(),
            agent: "cc".into(),
            seal_count: 3,
            files_changed: 5,
            last_activity: Utc::now(),
            status: "in-progress".into(),
            completion_summary: None,
        };
        let json = serde_json::to_string(&brief).unwrap();
        assert!(json.contains("S-001"));
        assert!(!json.contains("completion_summary"));
    }

    #[test]
    fn test_spec_brief_with_summary() {
        let brief = SpecBrief {
            id: "S-002".into(),
            title: "Storage".into(),
            agent: "cc".into(),
            seal_count: 2,
            files_changed: 3,
            last_activity: Utc::now(),
            status: "completed".into(),
            completion_summary: Some("Implemented zstd compression".into()),
        };
        let json = serde_json::to_string(&brief).unwrap();
        assert!(json.contains("completion_summary"));
        assert!(json.contains("zstd"));
    }

    #[test]
    fn test_status_roundtrip() {
        let status = StatusOutput {
            project_name: "roundtrip-test".into(),
            timestamp: Utc::now(),
            agents: AgentSummary {
                active: 5,
                done: 3,
                idle: 1,
                total: 9,
            },
            specs_in_progress: vec![SpecBrief {
                id: "S-010".into(),
                title: "Working".into(),
                agent: "lee".into(),
                seal_count: 1,
                files_changed: 2,
                last_activity: Utc::now(),
                status: "in-progress".into(),
                completion_summary: None,
            }],
            specs_completed: vec![],
            specs_committed: vec![],
            total_files_changed: 2,
            stale_specs: vec![],
            commit_mode: "propose".into(),
        };
        let json = serde_json::to_string_pretty(&status).unwrap();
        let parsed: StatusOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.project_name, "roundtrip-test");
        assert_eq!(parsed.agents.active, 5);
        assert_eq!(parsed.specs_in_progress.len(), 1);
        assert_eq!(parsed.specs_in_progress[0].agent, "lee");
    }
}
