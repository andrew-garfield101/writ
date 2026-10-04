//! Agent performance analytics.
//!
//! Derives productivity metrics from the seal chain — no new infrastructure,
//! just reporting on data that's already there. Every seal records agent
//! identity, timestamps, file changes, and spec linkage. This module
//! aggregates that into per-agent and per-spec metrics.

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::error::WritResult;
use crate::gc::load_all_seals;
use crate::seal::{ChangeType, Seal};

/// Per-agent productivity metrics aggregated from the seal chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMetrics {
    /// Agent identifier (e.g. "claude-code-7a3f").
    pub agent_id: String,
    /// Total seals produced.
    pub seal_count: usize,
    /// Total specs claimed.
    pub spec_count: usize,
    /// Total files touched across all seals.
    pub files_changed: usize,
    /// Files added.
    pub files_added: usize,
    /// Files modified.
    pub files_modified: usize,
    /// Files deleted.
    pub files_deleted: usize,
    /// Average files per seal.
    pub avg_files_per_seal: f64,
    /// Average seals per spec.
    pub avg_seals_per_spec: f64,
    /// Average duration between consecutive seals (seconds).
    /// None if the agent only produced one seal.
    pub avg_seal_interval_secs: Option<u64>,
    /// Total wall-clock active time (first seal to last seal, seconds).
    pub active_duration_secs: u64,
    /// Number of warnings logged across all seals (scope violations, etc.).
    pub warning_count: usize,
    /// Number of seals that triggered convergence.
    pub convergence_triggered: usize,
    /// Number of seals where convergence succeeded.
    pub convergence_succeeded: usize,
    /// Timestamp of the agent's first seal.
    pub first_seal_at: DateTime<Utc>,
    /// Timestamp of the agent's most recent seal.
    pub last_seal_at: DateTime<Utc>,
}

/// Aggregated metrics across all agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsReport {
    /// Generated at this timestamp.
    pub generated_at: DateTime<Utc>,
    /// Per-agent metrics, sorted by seal count descending.
    pub agents: Vec<AgentMetrics>,
    /// Total seals across all agents.
    pub total_seals: usize,
    /// Total specs across all agents.
    pub total_specs: usize,
    /// Total unique agents.
    pub total_agents: usize,
    /// Time range covered (first seal across all agents to last seal across all agents).
    pub time_range: Option<TimeRange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub duration_secs: u64,
}

impl AnalyticsReport {
    /// Generate an analytics report from the seal chain.
    pub fn generate(writ_dir: &Path) -> WritResult<Self> {
        let seals = load_all_seals(writ_dir)?;
        Self::from_seals(&seals)
    }

    /// Generate a report from a pre-loaded set of seals.
    /// Exposed for testing.
    pub fn from_seals(seals: &[Seal]) -> WritResult<Self> {
        let mut by_agent: HashMap<String, Vec<&Seal>> = HashMap::new();
        for seal in seals {
            by_agent
                .entry(seal.agent.id.clone())
                .or_default()
                .push(seal);
        }

        let mut agents = Vec::new();
        for (agent_id, agent_seals) in by_agent {
            let metrics = compute_agent_metrics(&agent_id, &agent_seals);
            agents.push(metrics);
        }
        agents.sort_by(|a, b| b.seal_count.cmp(&a.seal_count));

        let total_seals = seals.len();
        let total_specs: std::collections::HashSet<&String> = seals
            .iter()
            .filter_map(|s| s.spec_id.as_ref())
            .collect();
        let total_agents = agents.len();

        let time_range = if seals.is_empty() {
            None
        } else {
            let mut start = seals[0].timestamp;
            let mut end = seals[0].timestamp;
            for seal in seals.iter().skip(1) {
                if seal.timestamp < start {
                    start = seal.timestamp;
                }
                if seal.timestamp > end {
                    end = seal.timestamp;
                }
            }
            let duration_secs = (end - start).num_seconds().max(0) as u64;
            Some(TimeRange {
                start,
                end,
                duration_secs,
            })
        };

        Ok(AnalyticsReport {
            generated_at: Utc::now(),
            agents,
            total_seals,
            total_specs: total_specs.len(),
            total_agents,
            time_range,
        })
    }
}

fn compute_agent_metrics(agent_id: &str, seals: &[&Seal]) -> AgentMetrics {
    let mut sorted: Vec<&Seal> = seals.to_vec();
    sorted.sort_by_key(|s| s.timestamp);

    let seal_count = sorted.len();
    let first = sorted.first().unwrap();
    let last = sorted.last().unwrap();

    let specs: std::collections::HashSet<&String> =
        sorted.iter().filter_map(|s| s.spec_id.as_ref()).collect();
    let spec_count = specs.len();

    let mut files_added = 0;
    let mut files_modified = 0;
    let mut files_deleted = 0;
    let mut warning_count = 0;
    let mut convergence_triggered = 0;
    let mut convergence_succeeded = 0;

    for seal in &sorted {
        for change in &seal.changes {
            match change.change_type {
                ChangeType::Added => files_added += 1,
                ChangeType::Modified => files_modified += 1,
                ChangeType::Deleted => files_deleted += 1,
            }
        }
        warning_count += seal.warnings.len();
        if let Some(c) = &seal.convergence {
            if c.attempted {
                convergence_triggered += 1;
                if c.succeeded {
                    convergence_succeeded += 1;
                }
            }
        }
    }

    let files_changed = files_added + files_modified + files_deleted;
    let avg_files_per_seal = if seal_count > 0 {
        files_changed as f64 / seal_count as f64
    } else {
        0.0
    };
    let avg_seals_per_spec = if spec_count > 0 {
        seal_count as f64 / spec_count as f64
    } else {
        seal_count as f64
    };

    let avg_seal_interval_secs = if seal_count > 1 {
        let mut intervals: Vec<Duration> = Vec::with_capacity(seal_count - 1);
        for w in sorted.windows(2) {
            intervals.push(w[1].timestamp - w[0].timestamp);
        }
        let total: i64 = intervals.iter().map(|d| d.num_seconds()).sum();
        Some((total / intervals.len() as i64).max(0) as u64)
    } else {
        None
    };

    let active_duration_secs = (last.timestamp - first.timestamp).num_seconds().max(0) as u64;

    AgentMetrics {
        agent_id: agent_id.to_string(),
        seal_count,
        spec_count,
        files_changed,
        files_added,
        files_modified,
        files_deleted,
        avg_files_per_seal,
        avg_seals_per_spec,
        avg_seal_interval_secs,
        active_duration_secs,
        warning_count,
        convergence_triggered,
        convergence_succeeded,
        first_seal_at: first.timestamp,
        last_seal_at: last.timestamp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seal::{AgentIdentity, AgentType, FileChange, TaskStatus, Verification};

    fn make_seal(
        id: &str,
        agent_id: &str,
        spec_id: Option<&str>,
        timestamp: DateTime<Utc>,
        changes: Vec<FileChange>,
    ) -> Seal {
        Seal {
            id: id.into(),
            parent: None,
            timestamp,
            tree: "tree-hash".into(),
            agent: AgentIdentity {
                id: agent_id.into(),
                agent_type: AgentType::Agent,
            },
            spec_id: spec_id.map(String::from),
            status: TaskStatus::InProgress,
            changes,
            verification: Verification::default(),
            summary: "test seal".into(),
            warnings: Vec::new(),
            parent_seal_hash: None,
            content_hash: None,
            chain_hash: None,
            signature: None,
            workspace: "main".into(),
            convergence: None,
        }
    }

    fn change(path: &str, change_type: ChangeType) -> FileChange {
        FileChange {
            path: path.into(),
            change_type,
            old_hash: None,
            new_hash: Some("hash".into()),
        }
    }

    #[test]
    fn empty_seal_chain_produces_empty_report() {
        let report = AnalyticsReport::from_seals(&[]).unwrap();
        assert_eq!(report.total_seals, 0);
        assert_eq!(report.total_agents, 0);
        assert!(report.agents.is_empty());
        assert!(report.time_range.is_none());
    }

    #[test]
    fn single_seal_produces_single_agent_metric() {
        let now = Utc::now();
        let seal = make_seal(
            "s1",
            "agent-1",
            Some("spec-1"),
            now,
            vec![change("foo.rs", ChangeType::Added)],
        );
        let report = AnalyticsReport::from_seals(&[seal]).unwrap();
        assert_eq!(report.total_seals, 1);
        assert_eq!(report.total_agents, 1);
        assert_eq!(report.agents[0].agent_id, "agent-1");
        assert_eq!(report.agents[0].seal_count, 1);
        assert_eq!(report.agents[0].spec_count, 1);
        assert_eq!(report.agents[0].files_added, 1);
        assert!(report.agents[0].avg_seal_interval_secs.is_none());
    }

    #[test]
    fn multiple_seals_compute_intervals() {
        let now = Utc::now();
        let seal1 = make_seal("s1", "agent-1", Some("spec-1"), now, vec![]);
        let seal2 = make_seal(
            "s2",
            "agent-1",
            Some("spec-1"),
            now + Duration::seconds(120),
            vec![],
        );
        let seal3 = make_seal(
            "s3",
            "agent-1",
            Some("spec-1"),
            now + Duration::seconds(360),
            vec![],
        );
        let report = AnalyticsReport::from_seals(&[seal1, seal2, seal3]).unwrap();
        let metrics = &report.agents[0];
        assert_eq!(metrics.seal_count, 3);
        // Intervals: 120s, 240s. Average: 180s.
        assert_eq!(metrics.avg_seal_interval_secs, Some(180));
        assert_eq!(metrics.active_duration_secs, 360);
    }

    #[test]
    fn change_type_counts_correctly() {
        let now = Utc::now();
        let seal = make_seal(
            "s1",
            "agent-1",
            None,
            now,
            vec![
                change("a.rs", ChangeType::Added),
                change("b.rs", ChangeType::Added),
                change("c.rs", ChangeType::Modified),
                change("d.rs", ChangeType::Deleted),
            ],
        );
        let report = AnalyticsReport::from_seals(&[seal]).unwrap();
        let metrics = &report.agents[0];
        assert_eq!(metrics.files_added, 2);
        assert_eq!(metrics.files_modified, 1);
        assert_eq!(metrics.files_deleted, 1);
        assert_eq!(metrics.files_changed, 4);
        assert_eq!(metrics.avg_files_per_seal, 4.0);
    }

    #[test]
    fn multiple_agents_sorted_by_seal_count() {
        let now = Utc::now();
        let seals = vec![
            make_seal("s1", "agent-a", None, now, vec![]),
            make_seal("s2", "agent-b", None, now, vec![]),
            make_seal("s3", "agent-b", None, now, vec![]),
            make_seal("s4", "agent-b", None, now, vec![]),
            make_seal("s5", "agent-c", None, now, vec![]),
            make_seal("s6", "agent-c", None, now, vec![]),
        ];
        let report = AnalyticsReport::from_seals(&seals).unwrap();
        assert_eq!(report.total_agents, 3);
        // Sorted by seal count descending.
        assert_eq!(report.agents[0].agent_id, "agent-b");
        assert_eq!(report.agents[0].seal_count, 3);
        assert_eq!(report.agents[1].agent_id, "agent-c");
        assert_eq!(report.agents[1].seal_count, 2);
        assert_eq!(report.agents[2].agent_id, "agent-a");
        assert_eq!(report.agents[2].seal_count, 1);
    }

    #[test]
    fn time_range_spans_all_seals() {
        let now = Utc::now();
        let seals = vec![
            make_seal("s1", "agent-a", None, now, vec![]),
            make_seal("s2", "agent-b", None, now + Duration::seconds(600), vec![]),
            make_seal("s3", "agent-a", None, now + Duration::seconds(300), vec![]),
        ];
        let report = AnalyticsReport::from_seals(&seals).unwrap();
        let range = report.time_range.unwrap();
        assert_eq!(range.duration_secs, 600);
    }
}
