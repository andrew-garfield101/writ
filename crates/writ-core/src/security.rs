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

use chrono::Duration;

use crate::error::{WritError, WritResult};

// ---------------------------------------------------------------------------
// GC Retention Configuration for Security Events
// ---------------------------------------------------------------------------

/// Retention periods for security events, by severity.
///
/// Events older than their severity's retention period are eligible for
/// garbage collection. Critical events are retained longest, info events
/// shortest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityEventGcConfig {
    /// Retention period for Critical events (default: 2 years).
    #[serde(with = "duration_days_serde")]
    pub retention_critical: Duration,
    /// Retention period for Warning events (default: 6 months / 180 days).
    #[serde(with = "duration_days_serde")]
    pub retention_warning: Duration,
    /// Retention period for Info events (default: 30 days).
    #[serde(with = "duration_days_serde")]
    pub retention_info: Duration,
}

impl Default for SecurityEventGcConfig {
    fn default() -> Self {
        SecurityEventGcConfig {
            retention_critical: Duration::days(730), // 2 years
            retention_warning: Duration::days(180),  // 6 months
            retention_info: Duration::days(30),      // 30 days
        }
    }
}

impl SecurityEventGcConfig {
    /// Get the retention duration for a given severity.
    pub fn retention_for(&self, severity: &Severity) -> Duration {
        match severity {
            Severity::Critical => self.retention_critical,
            Severity::Warning => self.retention_warning,
            Severity::Info => self.retention_info,
        }
    }
}

/// Serde helper to serialize/deserialize `chrono::Duration` as integer days.
mod duration_days_serde {
    use chrono::Duration;
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_i64(duration.num_days())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let days = i64::deserialize(deserializer)?;
        Ok(Duration::days(days))
    }
}

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

    /// Return events whose age exceeds their severity-based retention threshold.
    ///
    /// These events are eligible for garbage collection. The returned list is
    /// suitable for feeding into `GcPlan` generation.
    pub fn events_past_retention(
        &self,
        config: &SecurityEventGcConfig,
    ) -> WritResult<Vec<SecurityEvent>> {
        let all = self.read_events(None)?;
        let now = Utc::now();
        let mut past = Vec::new();
        for event in all {
            let age = now - event.timestamp;
            let retention = config.retention_for(&event.severity);
            if age > retention {
                past.push(event);
            }
        }
        Ok(past)
    }

    /// Return only events that are within their retention period.
    ///
    /// Used by `writ security events` CLI to hide expired events even before
    /// GC actually runs. Also used by the event cleanup executor to determine
    /// which events to keep.
    pub fn read_events_within_retention(
        &self,
        config: &SecurityEventGcConfig,
    ) -> WritResult<Vec<SecurityEvent>> {
        let all = self.read_events(None)?;
        let now = Utc::now();
        let mut within = Vec::new();
        for event in all {
            let age = now - event.timestamp;
            let retention = config.retention_for(&event.severity);
            if age <= retention {
                within.push(event);
            }
        }
        Ok(within)
    }

    /// Rewrite the events file, removing events past their retention period.
    ///
    /// Uses exclusive file locking during the rewrite to prevent concurrent
    /// append corruption. This is the **only** operation that modifies the
    /// events file — normally it is append-only.
    ///
    /// Returns the number of events removed.
    pub fn clean_events(&self, config: &SecurityEventGcConfig) -> WritResult<usize> {
        if !self.events_path.exists() {
            return Ok(0);
        }

        // Open for read+write (we'll truncate after filtering).
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.events_path)?;
        file.lock_exclusive().map_err(WritError::Io)?;

        // Read all events under the lock.
        let reader = BufReader::new(&file);
        let now = Utc::now();
        let mut keep = Vec::new();
        let mut removed = 0usize;

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let event: SecurityEvent = serde_json::from_str(&line)?;
            let age = now - event.timestamp;
            let retention = config.retention_for(&event.severity);
            if age <= retention {
                keep.push(line);
            } else {
                removed += 1;
            }
        }

        // Rewrite the file with only surviving events.
        // Truncate + seek to start.
        file.set_len(0)?;
        use std::io::Seek;
        let mut writer = std::io::BufWriter::new(&file);
        writer.seek(std::io::SeekFrom::Start(0))?;
        for line in &keep {
            writeln!(writer, "{}", line)?;
        }
        writer.flush()?;

        // Lock released on File drop.
        Ok(removed)
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
// GC Audit Record — written after every GC run
// ---------------------------------------------------------------------------

/// How a GC run was triggered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GcTrigger {
    /// Manual invocation via `writ gc`.
    Manual,
    /// Scheduled (post-beta).
    Scheduled,
}

/// A single skipped action with its reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedAction {
    /// Description of what was skipped.
    pub action: String,
    /// Why it was skipped.
    pub reason: String,
}

/// Record of a completed GC execution.
///
/// Appended to `.writ/gc/audit.jsonl` after every `writ gc` run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcAuditRecord {
    /// Unique identifier (BLAKE3 hash of timestamp + random bytes).
    pub id: String,
    /// When the GC run was executed.
    pub executed_at: DateTime<Utc>,
    /// How GC was triggered.
    pub triggered_by: GcTrigger,
    /// Number of actions in the GC plan.
    pub actions_planned: usize,
    /// Number of actions successfully executed.
    pub actions_executed: usize,
    /// Number of actions skipped (safety checks, already cleaned, etc.).
    pub actions_skipped: usize,
    /// Number of actions that failed during execution.
    pub actions_failed: usize,
    /// Total bytes freed by this GC run.
    pub space_freed_bytes: u64,
    /// How long the GC run took.
    pub duration_ms: u64,
    /// Details of each skipped action.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped_details: Vec<SkippedAction>,
}

impl GcAuditRecord {
    /// Generate a unique ID for this audit record.
    fn generate_id() -> String {
        use rand::RngCore;
        let mut random = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut random);
        let now = Utc::now().timestamp_nanos_opt().unwrap_or(0);
        let input = format!("{now}:{random:?}");
        crate::crypto::blake3_hex(input.as_bytes())[..16].to_string()
    }

    /// Create a new audit record for a completed GC run.
    pub fn new(
        triggered_by: GcTrigger,
        actions_planned: usize,
        actions_executed: usize,
        actions_skipped: usize,
        actions_failed: usize,
        space_freed_bytes: u64,
        duration_ms: u64,
        skipped_details: Vec<SkippedAction>,
    ) -> Self {
        GcAuditRecord {
            id: Self::generate_id(),
            executed_at: Utc::now(),
            triggered_by,
            actions_planned,
            actions_executed,
            actions_skipped,
            actions_failed,
            space_freed_bytes,
            duration_ms,
            skipped_details,
        }
    }
}

/// Append-only GC audit log at `.writ/gc/audit.jsonl`.
pub struct GcAuditLogger {
    audit_path: PathBuf,
}

impl GcAuditLogger {
    /// Create a logger for the given `.writ/` directory.
    pub fn new(writ_dir: &Path) -> Self {
        GcAuditLogger {
            audit_path: writ_dir.join("gc").join("audit.jsonl"),
        }
    }

    /// Append an audit record. Creates `.writ/gc/` if needed.
    pub fn write_record(&self, record: &GcAuditRecord) -> WritResult<()> {
        if let Some(parent) = self.audit_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.audit_path)?;
        file.lock_exclusive().map_err(WritError::Io)?;

        let mut writer = std::io::BufWriter::new(&file);
        serde_json::to_writer(&mut writer, record)?;
        writeln!(writer)?;
        writer.flush()?;

        Ok(())
    }

    /// Read all audit records, most recent last.
    pub fn read_records(&self) -> WritResult<Vec<GcAuditRecord>> {
        if !self.audit_path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(&self.audit_path)?;
        file.lock_shared().map_err(WritError::Io)?;
        let reader = BufReader::new(file);
        let mut records = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let record: GcAuditRecord = serde_json::from_str(&line)?;
            records.push(record);
        }
        Ok(records)
    }

    /// Read the last N audit records (most recent last).
    pub fn read_last(&self, limit: usize) -> WritResult<Vec<GcAuditRecord>> {
        let all = self.read_records()?;
        let start = all.len().saturating_sub(limit);
        Ok(all[start..].to_vec())
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

    // --- GC retention filtering tests (GC.2.3a/b) ---

    /// Helper: emit an event with a specific timestamp in the past.
    fn emit_event_at(
        logger: &SecurityEventLogger,
        severity: Severity,
        event_type: &str,
        days_ago: i64,
    ) {
        let event = SecurityEvent {
            timestamp: Utc::now() - chrono::Duration::days(days_ago),
            severity,
            event_type: event_type.to_string(),
            agent_id: None,
            details: format!("test event {event_type} from {days_ago} days ago"),
        };
        logger.emit_event(&event).unwrap();
    }

    #[test]
    fn test_info_event_past_default_retention() {
        // Default info retention = 30 days. Event at 31 days → past retention.
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());
        let config = SecurityEventGcConfig::default();

        emit_event_at(&logger, Severity::Info, "old_info", 31);
        emit_event_at(&logger, Severity::Info, "recent_info", 5);

        let past = logger.events_past_retention(&config).unwrap();
        assert_eq!(past.len(), 1);
        assert_eq!(past[0].event_type, "old_info");

        let within = logger.read_events_within_retention(&config).unwrap();
        assert_eq!(within.len(), 1);
        assert_eq!(within[0].event_type, "recent_info");
    }

    #[test]
    fn test_warning_event_within_default_retention() {
        // Default warning retention = 180 days. Event at 31 days → within retention.
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());
        let config = SecurityEventGcConfig::default();

        emit_event_at(&logger, Severity::Warning, "recent_warning", 31);

        let past = logger.events_past_retention(&config).unwrap();
        assert!(
            past.is_empty(),
            "warning at 31d should be within 180d retention"
        );

        let within = logger.read_events_within_retention(&config).unwrap();
        assert_eq!(within.len(), 1);
    }

    #[test]
    fn test_critical_event_long_retention() {
        // Default critical retention = 730 days (2 years).
        // Event at 500 days → within retention. Event at 800 days → past.
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());
        let config = SecurityEventGcConfig::default();

        emit_event_at(&logger, Severity::Critical, "within_crit", 500);
        emit_event_at(&logger, Severity::Critical, "past_crit", 800);

        let past = logger.events_past_retention(&config).unwrap();
        assert_eq!(past.len(), 1);
        assert_eq!(past[0].event_type, "past_crit");

        let within = logger.read_events_within_retention(&config).unwrap();
        assert_eq!(within.len(), 1);
        assert_eq!(within[0].event_type, "within_crit");
    }

    #[test]
    fn test_custom_retention_config() {
        // Custom: info retention = 7 days.
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());
        let config = SecurityEventGcConfig {
            retention_info: chrono::Duration::days(7),
            ..SecurityEventGcConfig::default()
        };

        emit_event_at(&logger, Severity::Info, "old_info", 10);
        emit_event_at(&logger, Severity::Info, "recent_info", 3);

        let past = logger.events_past_retention(&config).unwrap();
        assert_eq!(past.len(), 1);
        assert_eq!(past[0].event_type, "old_info");
    }

    #[test]
    fn test_retention_empty_events_file() {
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());
        let config = SecurityEventGcConfig::default();

        let past = logger.events_past_retention(&config).unwrap();
        assert!(past.is_empty());

        let within = logger.read_events_within_retention(&config).unwrap();
        assert!(within.is_empty());
    }

    #[test]
    fn test_retention_config_defaults() {
        let config = SecurityEventGcConfig::default();
        assert_eq!(config.retention_critical.num_days(), 730);
        assert_eq!(config.retention_warning.num_days(), 180);
        assert_eq!(config.retention_info.num_days(), 30);
    }

    #[test]
    fn test_retention_config_serde_roundtrip() {
        let config = SecurityEventGcConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let recovered: SecurityEventGcConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.retention_critical.num_days(), 730);
        assert_eq!(recovered.retention_warning.num_days(), 180);
        assert_eq!(recovered.retention_info.num_days(), 30);
    }

    // --- Event cleanup tests (GC.2.2c) ---

    #[test]
    fn test_clean_events_removes_old_info() {
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());
        let config = SecurityEventGcConfig::default();

        emit_event_at(&logger, Severity::Info, "old_info", 31);
        emit_event_at(&logger, Severity::Info, "recent_info", 5);
        emit_event_at(&logger, Severity::Warning, "recent_warning", 10);

        let removed = logger.clean_events(&config).unwrap();
        assert_eq!(removed, 1, "should remove 1 old info event");

        // Verify only 2 events remain
        let remaining = logger.read_events(None).unwrap();
        assert_eq!(remaining.len(), 2);
        assert!(remaining.iter().all(|e| e.event_type != "old_info"));
    }

    #[test]
    fn test_clean_events_preserves_critical_within_retention() {
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());
        let config = SecurityEventGcConfig::default();

        // Critical at 500 days — within 2-year retention
        emit_event_at(&logger, Severity::Critical, "crit_within", 500);
        emit_event_at(&logger, Severity::Info, "old_info", 60);

        let removed = logger.clean_events(&config).unwrap();
        assert_eq!(removed, 1, "should only remove the old info event");

        let remaining = logger.read_events(None).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].event_type, "crit_within");
    }

    #[test]
    fn test_clean_events_preserves_warnings_within_retention() {
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());
        let config = SecurityEventGcConfig::default();

        // Warning at 100 days — within 6-month retention
        emit_event_at(&logger, Severity::Warning, "warn_within", 100);
        emit_event_at(&logger, Severity::Info, "info_old", 45);

        let removed = logger.clean_events(&config).unwrap();
        assert_eq!(removed, 1);

        let remaining = logger.read_events(None).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].event_type, "warn_within");
    }

    #[test]
    fn test_clean_events_no_file() {
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());
        let config = SecurityEventGcConfig::default();

        let removed = logger.clean_events(&config).unwrap();
        assert_eq!(removed, 0);
    }

    #[test]
    fn test_clean_events_nothing_to_clean() {
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());
        let config = SecurityEventGcConfig::default();

        emit_event_at(&logger, Severity::Info, "fresh", 1);
        emit_event_at(&logger, Severity::Warning, "fresh_w", 1);

        let removed = logger.clean_events(&config).unwrap();
        assert_eq!(removed, 0);

        let remaining = logger.read_events(None).unwrap();
        assert_eq!(remaining.len(), 2);
    }

    #[test]
    fn test_clean_events_idempotent() {
        // Running clean twice should produce 0 removals on second run.
        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());
        let config = SecurityEventGcConfig::default();

        emit_event_at(&logger, Severity::Info, "old", 31);
        emit_event_at(&logger, Severity::Info, "fresh", 1);

        let removed1 = logger.clean_events(&config).unwrap();
        assert_eq!(removed1, 1);

        let removed2 = logger.clean_events(&config).unwrap();
        assert_eq!(removed2, 0);

        let remaining = logger.read_events(None).unwrap();
        assert_eq!(remaining.len(), 1);
    }

    // --- GC Audit Record tests (GC.2.2e) ---

    #[test]
    fn test_audit_record_creation() {
        let record = GcAuditRecord::new(
            GcTrigger::Manual,
            5,
            3,
            1,
            1,
            1024,
            150,
            vec![SkippedAction {
                action: "CleanSpec(active-spec)".to_string(),
                reason: "spec is Active — safety check blocked".to_string(),
            }],
        );

        assert!(!record.id.is_empty());
        assert_eq!(record.id.len(), 16);
        assert_eq!(record.triggered_by, GcTrigger::Manual);
        assert_eq!(record.actions_planned, 5);
        assert_eq!(record.actions_executed, 3);
        assert_eq!(record.actions_skipped, 1);
        assert_eq!(record.actions_failed, 1);
        assert_eq!(record.space_freed_bytes, 1024);
        assert_eq!(record.duration_ms, 150);
        assert_eq!(record.skipped_details.len(), 1);
    }

    #[test]
    fn test_audit_record_unique_ids() {
        let r1 = GcAuditRecord::new(GcTrigger::Manual, 0, 0, 0, 0, 0, 0, vec![]);
        let r2 = GcAuditRecord::new(GcTrigger::Manual, 0, 0, 0, 0, 0, 0, vec![]);
        assert_ne!(r1.id, r2.id, "audit records should have unique IDs");
    }

    #[test]
    fn test_audit_record_json_roundtrip() {
        let record = GcAuditRecord::new(
            GcTrigger::Manual,
            3,
            2,
            1,
            0,
            2048,
            250,
            vec![SkippedAction {
                action: "test".to_string(),
                reason: "test reason".to_string(),
            }],
        );

        let json = serde_json::to_string(&record).unwrap();
        let recovered: GcAuditRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.id, record.id);
        assert_eq!(recovered.triggered_by, GcTrigger::Manual);
        assert_eq!(recovered.actions_planned, 3);
        assert_eq!(recovered.skipped_details.len(), 1);
    }

    #[test]
    fn test_audit_logger_write_and_read() {
        let dir = tempdir().unwrap();
        let audit_logger = GcAuditLogger::new(dir.path());

        let record = GcAuditRecord::new(GcTrigger::Manual, 5, 5, 0, 0, 4096, 100, vec![]);
        audit_logger.write_record(&record).unwrap();

        let records = audit_logger.read_records().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, record.id);
        assert_eq!(records[0].space_freed_bytes, 4096);
    }

    #[test]
    fn test_audit_logger_appends_multiple() {
        let dir = tempdir().unwrap();
        let audit_logger = GcAuditLogger::new(dir.path());

        for i in 0..3 {
            let record =
                GcAuditRecord::new(GcTrigger::Manual, i, i, 0, 0, (i as u64) * 100, 50, vec![]);
            audit_logger.write_record(&record).unwrap();
        }

        let records = audit_logger.read_records().unwrap();
        assert_eq!(records.len(), 3);
    }

    #[test]
    fn test_audit_logger_read_last() {
        let dir = tempdir().unwrap();
        let audit_logger = GcAuditLogger::new(dir.path());

        for i in 0..5 {
            let record = GcAuditRecord::new(GcTrigger::Manual, i, i, 0, 0, 0, 0, vec![]);
            audit_logger.write_record(&record).unwrap();
        }

        let last3 = audit_logger.read_last(3).unwrap();
        assert_eq!(last3.len(), 3);
        // Should be the last 3 records (actions_planned = 2, 3, 4)
        assert_eq!(last3[0].actions_planned, 2);
        assert_eq!(last3[1].actions_planned, 3);
        assert_eq!(last3[2].actions_planned, 4);
    }

    #[test]
    fn test_audit_logger_no_file() {
        let dir = tempdir().unwrap();
        let audit_logger = GcAuditLogger::new(dir.path());

        let records = audit_logger.read_records().unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn test_audit_logger_creates_gc_directory() {
        let dir = tempdir().unwrap();
        let gc_dir = dir.path().join("gc");
        assert!(!gc_dir.exists());

        let audit_logger = GcAuditLogger::new(dir.path());
        let record = GcAuditRecord::new(GcTrigger::Manual, 0, 0, 0, 0, 0, 0, vec![]);
        audit_logger.write_record(&record).unwrap();

        assert!(gc_dir.exists());
        assert!(dir.path().join("gc/audit.jsonl").exists());
    }

    #[test]
    fn test_gc_trigger_serde() {
        let manual_json = serde_json::to_string(&GcTrigger::Manual).unwrap();
        let scheduled_json = serde_json::to_string(&GcTrigger::Scheduled).unwrap();
        assert_eq!(manual_json, "\"manual\"");
        assert_eq!(scheduled_json, "\"scheduled\"");
    }
}
