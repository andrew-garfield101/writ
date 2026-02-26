//! Security event logging — append-only audit trail.
//!
//! Writes structured security events to `.writ/security/events.jsonl`.
//! Thread-safe via advisory file locking (`fs2`).

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::error::{WritError, WritResult};

/// Severity level for security events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

/// A structured security event for audit logging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityEvent {
    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
    /// Severity classification.
    pub severity: Severity,
    /// Machine-readable event type (e.g., "scope_violation", "agent_revoked").
    pub event_type: String,
    /// Agent involved, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Human-readable details.
    pub details: String,
}

/// Append-only event logger writing to `.writ/security/events.jsonl`.
pub struct SecurityEventLogger {
    events_path: PathBuf,
}

impl SecurityEventLogger {
    /// Create a logger for the given `.writ/` directory.
    pub fn new(writ_dir: &Path) -> Self {
        SecurityEventLogger {
            events_path: writ_dir.join("security").join("events.jsonl"),
        }
    }

    /// Emit a security event. Creates the directory and file if needed.
    /// Thread-safe via advisory file locking.
    pub fn emit_event(&self, event: &SecurityEvent) -> WritResult<()> {
        if let Some(parent) = self.events_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.events_path)?;

        // Exclusive lock — blocks until available, released on File drop.
        file.lock_exclusive().map_err(|e| WritError::Io(e))?;

        let mut writer = std::io::BufWriter::new(&file);
        serde_json::to_writer(&mut writer, event)?;
        writeln!(writer)?;
        writer.flush()?;

        Ok(())
    }

    /// Read all events, optionally filtered by severity.
    /// Uses a shared (read) lock to prevent torn reads from concurrent writers.
    pub fn read_events(
        &self,
        severity_filter: Option<&Severity>,
    ) -> WritResult<Vec<SecurityEvent>> {
        if !self.events_path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(&self.events_path)?;
        file.lock_shared().map_err(|e| WritError::Io(e))?;
        let reader = BufReader::new(file);
        let mut events = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let event: SecurityEvent = serde_json::from_str(&line)?;
            match severity_filter {
                Some(filter) if event.severity != *filter => continue,
                _ => events.push(event),
            }
        }
        Ok(events)
    }

    /// Convenience: emit a Critical scope violation event.
    pub fn emit_scope_violation(
        &self,
        agent_id: &str,
        spec_id: &str,
        out_of_scope_files: &[String],
    ) -> WritResult<()> {
        self.emit_event(&SecurityEvent {
            timestamp: Utc::now(),
            severity: Severity::Critical,
            event_type: "scope_violation".to_string(),
            agent_id: Some(agent_id.to_string()),
            details: format!(
                "Agent '{}' sealed {} file(s) outside scope of spec '{}': {}",
                agent_id,
                out_of_scope_files.len(),
                spec_id,
                out_of_scope_files
                    .iter()
                    .take(10)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        })
    }

    /// Convenience: emit a Critical agent scope violation event.
    pub fn emit_agent_scope_violation(
        &self,
        agent_id: &str,
        out_of_scope_files: &[&str],
    ) -> WritResult<()> {
        self.emit_event(&SecurityEvent {
            timestamp: Utc::now(),
            severity: Severity::Critical,
            event_type: "agent_scope_violation".to_string(),
            agent_id: Some(agent_id.to_string()),
            details: format!(
                "Agent '{}' sealed {} file(s) outside scope constraints: {}",
                agent_id,
                out_of_scope_files.len(),
                out_of_scope_files
                    .iter()
                    .take(10)
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        })
    }

    /// Convenience: emit a Critical agent revocation event.
    pub fn emit_agent_revoked(&self, agent_id: &str, reason: &str) -> WritResult<()> {
        self.emit_event(&SecurityEvent {
            timestamp: Utc::now(),
            severity: Severity::Critical,
            event_type: "agent_revoked".to_string(),
            agent_id: Some(agent_id.to_string()),
            details: format!("Agent '{}' revoked: {}", agent_id, reason),
        })
    }

    /// Convenience: emit a convergence-related event.
    pub fn emit_convergence_event(
        &self,
        event_type: &str,
        severity: Severity,
        details: &str,
    ) -> WritResult<()> {
        self.emit_convergence_event_with_agent(event_type, severity, details, None)
    }

    /// Convenience: emit a convergence-related event with optional agent context.
    pub fn emit_convergence_event_with_agent(
        &self,
        event_type: &str,
        severity: Severity,
        details: &str,
        agent_id: Option<&str>,
    ) -> WritResult<()> {
        self.emit_event(&SecurityEvent {
            timestamp: Utc::now(),
            severity,
            event_type: event_type.to_string(),
            agent_id: agent_id.map(|s| s.to_string()),
            details: details.to_string(),
        })
    }

    /// Convenience: emit a Warning for chain hash verification failure.
    pub fn emit_chain_hash_failure(&self, seal_id: &str, details: &str) -> WritResult<()> {
        self.emit_event(&SecurityEvent {
            timestamp: Utc::now(),
            severity: Severity::Warning,
            event_type: "chain_hash_failure".to_string(),
            agent_id: None,
            details: format!("Seal '{}': {}", seal_id, details),
        })
    }

    /// Convenience: emit a Warning for unrecognized agent at seal time.
    pub fn emit_unrecognized_agent(&self, agent_id: &str) -> WritResult<()> {
        self.emit_event(&SecurityEvent {
            timestamp: Utc::now(),
            severity: Severity::Warning,
            event_type: "unrecognized_agent".to_string(),
            agent_id: Some(agent_id.to_string()),
            details: format!(
                "Agent '{}' is not registered in agent identity store",
                agent_id
            ),
        })
    }

    /// Convenience: emit a Critical event for signature verification failure.
    pub fn emit_authentication_failure(&self, seal_id: &str, details: &str) -> WritResult<()> {
        self.emit_event(&SecurityEvent {
            timestamp: Utc::now(),
            severity: Severity::Critical,
            event_type: "authentication_failure".to_string(),
            agent_id: None,
            details: format!("Seal '{}': {}", seal_id, details),
        })
    }
}

// ---------------------------------------------------------------------------
// Flagged Seals — append-only manifest of compromised seals
// ---------------------------------------------------------------------------

/// A seal flagged as potentially compromised.
///
/// Written to `.writ/security/flagged-seals.jsonl` when an agent is revoked
/// with a compromise window. Seals are immutable — this is external metadata
/// that annotates them without modification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlaggedSeal {
    /// The flagged seal's ID.
    pub seal_id: String,
    /// The compromised agent that created this seal.
    pub agent_id: String,
    /// Why this seal was flagged.
    pub reason: FlagReason,
    /// Start/end of the compromise window.
    pub compromise_window: (DateTime<Utc>, DateTime<Utc>),
    /// Who triggered the flagging (e.g. admin agent ID).
    pub flagged_by: String,
    /// When the flag was recorded.
    pub flagged_at: DateTime<Utc>,
}

/// Why a seal was flagged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlagReason {
    /// Created by an agent during a known compromise window.
    AgentCompromised,
    /// Downstream: incorporates a flagged seal as input (transitive).
    DownstreamOfCompromised,
}

/// Append-only store for flagged seals at `.writ/security/flagged-seals.jsonl`.
pub struct FlaggedSealStore {
    path: PathBuf,
}

impl FlaggedSealStore {
    /// Create a store for the given `.writ/` directory.
    pub fn new(writ_dir: &Path) -> Self {
        FlaggedSealStore {
            path: writ_dir.join("security").join("flagged-seals.jsonl"),
        }
    }

    /// Append a flagged seal entry. Thread-safe via file locking.
    pub fn flag_seal(&self, entry: &FlaggedSeal) -> WritResult<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;

        file.lock_exclusive().map_err(WritError::Io)?;

        let mut writer = std::io::BufWriter::new(&file);
        serde_json::to_writer(&mut writer, entry)?;
        writeln!(writer)?;
        writer.flush()?;

        Ok(())
    }

    /// Load all flagged seal entries.
    pub fn load_all(&self) -> WritResult<Vec<FlaggedSeal>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: FlaggedSeal = serde_json::from_str(&line)?;
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Load just the set of flagged seal IDs for cheap membership checks.
    pub fn flagged_ids(&self) -> WritResult<std::collections::HashSet<String>> {
        Ok(self.load_all()?.into_iter().map(|e| e.seal_id).collect())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_event(severity: Severity, event_type: &str) -> SecurityEvent {
        SecurityEvent {
            timestamp: Utc::now(),
            severity,
            event_type: event_type.to_string(),
            agent_id: Some("test-agent".to_string()),
            details: "test event details".to_string(),
        }
    }

    #[test]
    fn test_emit_and_read_events() {
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());

        let event = make_event(Severity::Info, "test_event");
        logger.emit_event(&event).unwrap();

        let events = logger.read_events(None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "test_event");
        assert_eq!(events[0].severity, Severity::Info);
        assert_eq!(events[0].agent_id.as_deref(), Some("test-agent"));
    }

    #[test]
    fn test_auto_creates_directory() {
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());

        let security_dir = dir.path().join("security");
        assert!(!security_dir.exists());

        logger
            .emit_event(&make_event(Severity::Info, "test"))
            .unwrap();

        assert!(security_dir.exists());
        assert!(dir.path().join("security/events.jsonl").exists());
    }

    #[test]
    fn test_read_events_no_file() {
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());

        let events = logger.read_events(None).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_severity_filter() {
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());

        logger
            .emit_event(&make_event(Severity::Info, "info_event"))
            .unwrap();
        logger
            .emit_event(&make_event(Severity::Warning, "warn_event"))
            .unwrap();
        logger
            .emit_event(&make_event(Severity::Critical, "crit_event"))
            .unwrap();

        let critical = logger.read_events(Some(&Severity::Critical)).unwrap();
        assert_eq!(critical.len(), 1);
        assert_eq!(critical[0].event_type, "crit_event");

        let info = logger.read_events(Some(&Severity::Info)).unwrap();
        assert_eq!(info.len(), 1);

        let all = logger.read_events(None).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_multiple_events_appended() {
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());

        for i in 0..5 {
            logger
                .emit_event(&make_event(Severity::Info, &format!("event_{i}")))
                .unwrap();
        }

        let events = logger.read_events(None).unwrap();
        assert_eq!(events.len(), 5);
        for (i, event) in events.iter().enumerate() {
            assert_eq!(event.event_type, format!("event_{i}"));
        }
    }

    #[test]
    fn test_emit_scope_violation_convenience() {
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());

        logger
            .emit_scope_violation("agent-1", "my-spec", &["secret.key".to_string()])
            .unwrap();

        let events = logger.read_events(None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].severity, Severity::Critical);
        assert_eq!(events[0].event_type, "scope_violation");
        assert_eq!(events[0].agent_id.as_deref(), Some("agent-1"));
        assert!(events[0].details.contains("secret.key"));
        assert!(events[0].details.contains("my-spec"));
    }

    #[test]
    fn test_emit_agent_revoked_convenience() {
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());

        logger
            .emit_agent_revoked("bad-agent", "compromised key")
            .unwrap();

        let events = logger.read_events(None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].severity, Severity::Critical);
        assert_eq!(events[0].event_type, "agent_revoked");
        assert!(events[0].details.contains("compromised key"));
    }

    #[test]
    fn test_event_json_roundtrip() {
        let event = SecurityEvent {
            timestamp: Utc::now(),
            severity: Severity::Warning,
            event_type: "chain_hash_failure".to_string(),
            agent_id: None,
            details: "seal abc123 has invalid chain hash".to_string(),
        };

        let json = serde_json::to_string(&event).unwrap();
        let recovered: SecurityEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.severity, Severity::Warning);
        assert_eq!(recovered.event_type, "chain_hash_failure");
        assert!(recovered.agent_id.is_none());
    }

    #[test]
    fn test_concurrent_writes_no_corruption() {
        use std::sync::Arc;
        use std::thread;

        let dir = tempdir().unwrap();
        let logger = Arc::new(SecurityEventLogger::new(dir.path()));

        let mut handles = Vec::new();
        for i in 0..10 {
            let logger = Arc::clone(&logger);
            handles.push(thread::spawn(move || {
                logger
                    .emit_event(&SecurityEvent {
                        timestamp: Utc::now(),
                        severity: Severity::Info,
                        event_type: format!("concurrent_{i}"),
                        agent_id: Some(format!("thread-{i}")),
                        details: format!("event from thread {i}"),
                    })
                    .unwrap();
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let events = logger.read_events(None).unwrap();
        assert_eq!(events.len(), 10);

        // Every event should parse cleanly (no corruption)
        for event in &events {
            assert!(event.event_type.starts_with("concurrent_"));
        }
    }

    // --- Flagged seal store tests ---

    #[test]
    fn test_flagged_seal_store_empty() {
        let dir = tempdir().unwrap();
        let store = FlaggedSealStore::new(dir.path());

        let entries = store.load_all().unwrap();
        assert!(entries.is_empty());
        assert!(store.flagged_ids().unwrap().is_empty());
    }

    #[test]
    fn test_flag_seal_and_load() {
        let dir = tempdir().unwrap();
        let store = FlaggedSealStore::new(dir.path());
        let now = Utc::now();

        store
            .flag_seal(&FlaggedSeal {
                seal_id: "seal-abc".to_string(),
                agent_id: "bad-agent".to_string(),
                reason: FlagReason::AgentCompromised,
                compromise_window: (now - chrono::Duration::hours(2), now),
                flagged_by: "admin".to_string(),
                flagged_at: now,
            })
            .unwrap();

        let entries = store.load_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].seal_id, "seal-abc");
        assert_eq!(entries[0].agent_id, "bad-agent");
        assert_eq!(entries[0].reason, FlagReason::AgentCompromised);
    }

    #[test]
    fn test_flagged_ids_returns_set() {
        let dir = tempdir().unwrap();
        let store = FlaggedSealStore::new(dir.path());
        let now = Utc::now();

        for id in &["seal-1", "seal-2", "seal-3"] {
            store
                .flag_seal(&FlaggedSeal {
                    seal_id: id.to_string(),
                    agent_id: "agent".to_string(),
                    reason: FlagReason::AgentCompromised,
                    compromise_window: (now, now),
                    flagged_by: "admin".to_string(),
                    flagged_at: now,
                })
                .unwrap();
        }

        let ids = store.flagged_ids().unwrap();
        assert_eq!(ids.len(), 3);
        assert!(ids.contains("seal-1"));
        assert!(ids.contains("seal-2"));
        assert!(ids.contains("seal-3"));
    }

    #[test]
    fn test_flag_reason_serde_roundtrip() {
        let compromised = FlagReason::AgentCompromised;
        let downstream = FlagReason::DownstreamOfCompromised;

        let json1 = serde_json::to_string(&compromised).unwrap();
        let json2 = serde_json::to_string(&downstream).unwrap();

        assert_eq!(json1, "\"agent_compromised\"");
        assert_eq!(json2, "\"downstream_of_compromised\"");

        let recovered1: FlagReason = serde_json::from_str(&json1).unwrap();
        let recovered2: FlagReason = serde_json::from_str(&json2).unwrap();
        assert_eq!(recovered1, FlagReason::AgentCompromised);
        assert_eq!(recovered2, FlagReason::DownstreamOfCompromised);
    }

    #[test]
    fn test_flagged_seal_json_roundtrip() {
        let now = Utc::now();
        let entry = FlaggedSeal {
            seal_id: "abc123".to_string(),
            agent_id: "worker-bot".to_string(),
            reason: FlagReason::AgentCompromised,
            compromise_window: (now - chrono::Duration::hours(2), now),
            flagged_by: "admin-key".to_string(),
            flagged_at: now,
        };

        let json = serde_json::to_string(&entry).unwrap();
        let recovered: FlaggedSeal = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.seal_id, "abc123");
        assert_eq!(recovered.agent_id, "worker-bot");
        assert_eq!(recovered.reason, FlagReason::AgentCompromised);
    }

    #[test]
    fn test_append_only_multiple_flags() {
        let dir = tempdir().unwrap();
        let store = FlaggedSealStore::new(dir.path());
        let now = Utc::now();

        // Flag two seals for different reasons
        store
            .flag_seal(&FlaggedSeal {
                seal_id: "direct".to_string(),
                agent_id: "agent-x".to_string(),
                reason: FlagReason::AgentCompromised,
                compromise_window: (now, now),
                flagged_by: "admin".to_string(),
                flagged_at: now,
            })
            .unwrap();
        store
            .flag_seal(&FlaggedSeal {
                seal_id: "downstream".to_string(),
                agent_id: "agent-x".to_string(),
                reason: FlagReason::DownstreamOfCompromised,
                compromise_window: (now, now),
                flagged_by: "system".to_string(),
                flagged_at: now,
            })
            .unwrap();

        let entries = store.load_all().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].reason, FlagReason::AgentCompromised);
        assert_eq!(entries[1].reason, FlagReason::DownstreamOfCompromised);
    }

    // --- Convergence event tests ---

    #[test]
    fn test_emit_convergence_started_event() {
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());

        logger
            .emit_convergence_event(
                "convergence_started",
                Severity::Info,
                "Convergence started: 3 specs, base='auth', strategy=escalate",
            )
            .unwrap();

        let events = logger.read_events(None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "convergence_started");
        assert_eq!(events[0].severity, Severity::Info);
        assert!(events[0].agent_id.is_none());
        assert!(events[0].details.contains("3 specs"));
    }

    #[test]
    fn test_emit_convergence_completed_event() {
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());

        logger
            .emit_convergence_event(
                "convergence_completed",
                Severity::Info,
                "Convergence completed: 2 merges, 5 auto-merged, clean=true",
            )
            .unwrap();

        let events = logger.read_events(None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "convergence_completed");
        assert_eq!(events[0].severity, Severity::Info);
    }

    #[test]
    fn test_emit_convergence_degraded_event() {
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());

        logger
            .emit_convergence_event(
                "convergence_degraded",
                Severity::Warning,
                "Convergence degraded: content loss possible",
            )
            .unwrap();

        let events = logger.read_events(None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "convergence_degraded");
        assert_eq!(events[0].severity, Severity::Warning);
    }

    #[test]
    fn test_emit_convergence_escalation_event() {
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());

        logger
            .emit_convergence_event(
                "convergence_escalation",
                Severity::Warning,
                "Escalation in 'main.rs': BothModified between 'agent-a' and 'agent-b'",
            )
            .unwrap();

        let events = logger.read_events(None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "convergence_escalation");
        assert_eq!(events[0].severity, Severity::Warning);
        assert!(events[0].details.contains("main.rs"));
    }

    // --- C.2.3 convenience method tests ---

    #[test]
    fn test_emit_chain_hash_failure() {
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());

        logger
            .emit_chain_hash_failure("seal-abc123", "content_hash mismatch: expected deadbeef")
            .unwrap();

        let events = logger.read_events(None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "chain_hash_failure");
        assert_eq!(events[0].severity, Severity::Warning);
        assert!(events[0].agent_id.is_none());
        assert!(events[0].details.contains("seal-abc123"));
        assert!(events[0].details.contains("content_hash mismatch"));
    }

    #[test]
    fn test_emit_unrecognized_agent() {
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());

        logger.emit_unrecognized_agent("mystery-bot").unwrap();

        let events = logger.read_events(None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "unrecognized_agent");
        assert_eq!(events[0].severity, Severity::Warning);
        assert_eq!(events[0].agent_id.as_deref(), Some("mystery-bot"));
        assert!(events[0].details.contains("not registered"));
    }

    #[test]
    fn test_emit_authentication_failure() {
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());

        logger
            .emit_authentication_failure("seal-xyz", "signature verification failed")
            .unwrap();

        let events = logger.read_events(None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "authentication_failure");
        assert_eq!(events[0].severity, Severity::Critical);
        assert!(events[0].details.contains("seal-xyz"));
        assert!(events[0].details.contains("signature verification failed"));
    }
}
