//! Proposals — deferred commit suggestions for the propose workflow mode.
//!
//! In propose mode, an orchestrator creates proposals that a human reviews
//! and accepts/rejects. Proposals are stored in `.writ/proposals/<id>.json`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Status of a proposal in its lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProposalStatus {
    /// Awaiting human review.
    Pending,
    /// Accepted and committed to git.
    Accepted,
    /// Rejected by human reviewer.
    Rejected,
    /// Replaced by a newer proposal covering the same specs.
    Superseded,
}

/// A commit proposal created in propose mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    /// Unique proposal identifier (timestamp-based).
    pub id: String,
    /// Spec IDs included in this proposal.
    pub spec_ids: Vec<String>,
    /// Proposed commit message.
    pub message: String,
    /// Who created this proposal (agent ID or orchestrator name).
    pub proposed_by: String,
    /// Commit strategy used to generate this proposal.
    pub strategy: String,
    /// Current status.
    pub status: ProposalStatus,
    /// When this proposal was created.
    pub created_at: DateTime<Utc>,
    /// When this proposal was resolved (accepted/rejected/superseded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    /// Git commit hash if accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_hash: Option<String>,
    /// ID of the proposal that superseded this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
}

impl Proposal {
    /// Create a new pending proposal.
    pub fn new(
        spec_ids: Vec<String>,
        message: String,
        proposed_by: String,
        strategy: String,
    ) -> Self {
        let now = Utc::now();
        let id = format!("prop-{}", now.format("%Y%m%d-%H%M%S"));
        Self {
            id,
            spec_ids,
            message,
            proposed_by,
            strategy,
            status: ProposalStatus::Pending,
            created_at: now,
            resolved_at: None,
            commit_hash: None,
            superseded_by: None,
        }
    }

    /// Check if this proposal is still pending.
    pub fn is_pending(&self) -> bool {
        self.status == ProposalStatus::Pending
    }

    /// Mark this proposal as accepted with a commit hash.
    pub fn accept(&mut self, commit_hash: String) {
        self.status = ProposalStatus::Accepted;
        self.commit_hash = Some(commit_hash);
        self.resolved_at = Some(Utc::now());
    }

    /// Mark this proposal as rejected.
    pub fn reject(&mut self) {
        self.status = ProposalStatus::Rejected;
        self.resolved_at = Some(Utc::now());
    }

    /// Mark this proposal as superseded by another.
    pub fn supersede(&mut self, new_proposal_id: &str) {
        self.status = ProposalStatus::Superseded;
        self.superseded_by = Some(new_proposal_id.to_string());
        self.resolved_at = Some(Utc::now());
    }

    /// Check if this proposal overlaps with the given spec IDs.
    pub fn overlaps_with(&self, spec_ids: &[String]) -> bool {
        self.spec_ids.iter().any(|id| spec_ids.contains(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proposal_new_is_pending() {
        let p = Proposal::new(
            vec!["spec-1".into()],
            "feat: add auth".into(),
            "orchestrator".into(),
            "single".into(),
        );
        assert!(p.is_pending());
        assert!(p.id.starts_with("prop-"));
        assert_eq!(p.spec_ids, vec!["spec-1"]);
        assert!(p.commit_hash.is_none());
        assert!(p.resolved_at.is_none());
    }

    #[test]
    fn test_proposal_accept() {
        let mut p = Proposal::new(
            vec!["spec-1".into()],
            "msg".into(),
            "bot".into(),
            "single".into(),
        );
        p.accept("abc123".into());
        assert_eq!(p.status, ProposalStatus::Accepted);
        assert_eq!(p.commit_hash.as_deref(), Some("abc123"));
        assert!(p.resolved_at.is_some());
    }

    #[test]
    fn test_proposal_reject() {
        let mut p = Proposal::new(
            vec!["spec-1".into()],
            "msg".into(),
            "bot".into(),
            "single".into(),
        );
        p.reject();
        assert_eq!(p.status, ProposalStatus::Rejected);
        assert!(!p.is_pending());
        assert!(p.resolved_at.is_some());
    }

    #[test]
    fn test_proposal_supersede() {
        let mut p = Proposal::new(
            vec!["spec-1".into()],
            "old".into(),
            "bot".into(),
            "single".into(),
        );
        p.supersede("prop-newer");
        assert_eq!(p.status, ProposalStatus::Superseded);
        assert_eq!(p.superseded_by.as_deref(), Some("prop-newer"));
    }

    #[test]
    fn test_proposal_overlaps() {
        let p = Proposal::new(
            vec!["spec-1".into(), "spec-2".into()],
            "msg".into(),
            "bot".into(),
            "single".into(),
        );
        assert!(p.overlaps_with(&["spec-2".into()]));
        assert!(!p.overlaps_with(&["spec-99".into()]));
    }

    #[test]
    fn test_proposal_serde_roundtrip() {
        let mut p = Proposal::new(
            vec!["spec-1".into()],
            "msg".into(),
            "bot".into(),
            "single".into(),
        );
        p.accept("hash123".into());

        let json = serde_json::to_string(&p).unwrap();
        let recovered: Proposal = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.status, ProposalStatus::Accepted);
        assert_eq!(recovered.commit_hash.as_deref(), Some("hash123"));
        assert_eq!(recovered.spec_ids, vec!["spec-1"]);
    }

    #[test]
    fn test_proposal_status_serde_values() {
        assert_eq!(
            serde_json::to_string(&ProposalStatus::Pending).unwrap(),
            "\"pending\""
        );
        assert_eq!(
            serde_json::to_string(&ProposalStatus::Accepted).unwrap(),
            "\"accepted\""
        );
        assert_eq!(
            serde_json::to_string(&ProposalStatus::Rejected).unwrap(),
            "\"rejected\""
        );
        assert_eq!(
            serde_json::to_string(&ProposalStatus::Superseded).unwrap(),
            "\"superseded\""
        );
    }
}
