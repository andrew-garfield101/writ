//! Garbage collection and lifecycle management.
//!
//! Provides storage tracking, GC plan generation, and safe cleanup
//! of expired specs, old security events, and other working state.
//! Seals are immutable and never deleted.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{WritError, WritResult};
use crate::security::{
    GcAuditLogger, GcAuditRecord, GcTrigger, SecurityEvent, SecurityEventGcConfig, Severity,
    SkippedAction,
};
use crate::spec::LifecycleState;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// GC operating mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GcMode {
    /// GC only runs when `writ gc` is explicitly called.
    Manual,
    /// GC runs on a schedule (not implemented for beta).
    Scheduled,
    /// GC runs continuously as a background process (not implemented for beta).
    Daemon,
}

impl Default for GcMode {
    fn default() -> Self {
        GcMode::Manual
    }
}

/// Spec lifecycle GC configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecGcConfig {
    /// Seconds of inactivity before a spec is considered stale.
    pub stale_timeout_secs: u64,
    /// Seconds a stale spec waits before auto-expiring.
    pub expiry_timeout_secs: u64,
    /// Seconds a completed spec is retained before archival.
    pub retention_period_secs: u64,
    /// Seconds after reaching terminal state before GC can clean.
    pub grace_period_secs: u64,
}

impl Default for SpecGcConfig {
    fn default() -> Self {
        Self {
            stale_timeout_secs: 7200,      // 2 hours
            expiry_timeout_secs: 86400,    // 24 hours
            retention_period_secs: 604800, // 7 days
            grace_period_secs: 3600,       // 1 hour
        }
    }
}

// SecurityEventGcConfig is defined in security.rs (Amis's implementation).
// Re-exported via the import above.

/// Storage budget allocation (percentages, must sum to 100).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageAllocation {
    pub seal_pct: u8,
    pub working_state_pct: u8,
    pub security_event_pct: u8,
    pub headroom_pct: u8,
}

impl Default for StorageAllocation {
    fn default() -> Self {
        Self {
            seal_pct: 60,
            working_state_pct: 25,
            security_event_pct: 10,
            headroom_pct: 5,
        }
    }
}

/// Top-level GC configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcConfig {
    /// Operating mode (manual for beta).
    pub mode: GcMode,
    /// Total storage budget in bytes.
    pub budget_bytes: u64,
    /// Spec lifecycle settings.
    pub specs: SpecGcConfig,
    /// Security event retention settings.
    pub security_events: SecurityEventGcConfig,
    /// Storage allocation percentages.
    pub allocation: StorageAllocation,
    /// Emit storage warning at this percentage of budget.
    pub warning_threshold_pct: u8,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self::development()
    }
}

impl GcConfig {
    /// Development profile — sensible defaults for a dev laptop.
    pub fn development() -> Self {
        Self {
            mode: GcMode::Manual,
            budget_bytes: 5 * 1024 * 1024 * 1024, // 5 GB
            specs: SpecGcConfig::default(),
            security_events: SecurityEventGcConfig::default(),
            allocation: StorageAllocation::default(),
            warning_threshold_pct: 80,
        }
    }

    /// Raspberry Pi profile — constrained device with tight budgets.
    pub fn raspberry_pi() -> Self {
        Self {
            mode: GcMode::Manual,
            budget_bytes: 500 * 1024 * 1024, // 500 MB
            specs: SpecGcConfig {
                stale_timeout_secs: 3600,     // 1 hour
                expiry_timeout_secs: 43200,   // 12 hours
                retention_period_secs: 86400, // 1 day
                grace_period_secs: 1800,      // 30 minutes
            },
            security_events: SecurityEventGcConfig {
                retention_critical: chrono::Duration::days(730), // 2 years (never reduce)
                retention_warning: chrono::Duration::days(30),
                retention_info: chrono::Duration::days(7),
            },
            allocation: StorageAllocation::default(),
            warning_threshold_pct: 80,
        }
    }

    /// Production profile — server with moderate storage.
    pub fn production() -> Self {
        Self {
            mode: GcMode::Manual,
            budget_bytes: 100 * 1024 * 1024 * 1024, // 100 GB
            specs: SpecGcConfig {
                stale_timeout_secs: 14400,      // 4 hours
                expiry_timeout_secs: 172800,    // 48 hours
                retention_period_secs: 2592000, // 30 days
                grace_period_secs: 3600,        // 1 hour
            },
            security_events: SecurityEventGcConfig {
                retention_critical: chrono::Duration::days(730), // 2 years
                retention_warning: chrono::Duration::days(365),  // 1 year
                retention_info: chrono::Duration::days(30),
            },
            allocation: StorageAllocation::default(),
            warning_threshold_pct: 80,
        }
    }

    /// Enterprise profile — unlimited storage, long retention.
    pub fn enterprise() -> Self {
        Self {
            mode: GcMode::Manual,
            budget_bytes: u64::MAX, // effectively unlimited
            specs: SpecGcConfig {
                stale_timeout_secs: 28800,      // 8 hours
                expiry_timeout_secs: 604800,    // 7 days
                retention_period_secs: 7776000, // 90 days
                grace_period_secs: 7200,        // 2 hours
            },
            security_events: SecurityEventGcConfig {
                retention_critical: chrono::Duration::days(730), // 2 years
                retention_warning: chrono::Duration::days(730),  // 2 years
                retention_info: chrono::Duration::days(90),
            },
            allocation: StorageAllocation::default(),
            warning_threshold_pct: 80,
        }
    }

    /// Load a named profile.
    pub fn from_profile(name: &str) -> WritResult<Self> {
        match name {
            "raspberry-pi" | "raspberry_pi" => Ok(Self::raspberry_pi()),
            "development" | "dev" => Ok(Self::development()),
            "production" | "prod" => Ok(Self::production()),
            "enterprise" => Ok(Self::enterprise()),
            _ => Err(WritError::InvalidInput(format!(
                "unknown GC profile: '{}'. Valid profiles: raspberry-pi, development, production, enterprise",
                name
            ))),
        }
    }

    /// Load config from `.writ/gc/config.json`. Returns default if no file exists.
    pub fn load(writ_dir: &Path) -> WritResult<Self> {
        let path = writ_dir.join("gc").join("config.json");
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = fs::read_to_string(&path)?;
        let config: Self = serde_json::from_str(&data)?;
        Ok(config)
    }

    /// Save config to `.writ/gc/config.json`.
    pub fn save(&self, writ_dir: &Path) -> WritResult<()> {
        let gc_dir = writ_dir.join("gc");
        fs::create_dir_all(&gc_dir)?;
        let path = gc_dir.join("config.json");
        let data = serde_json::to_string_pretty(self)?;
        fs::write(&path, data)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Storage Report
// ---------------------------------------------------------------------------

/// Breakdown of storage usage within `.writ/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageReport {
    /// Total bytes used by `.writ/`.
    pub total_bytes: u64,
    /// Bytes used by seals.
    pub seal_bytes: u64,
    /// Bytes used by working state (specs, heads, index).
    pub working_state_bytes: u64,
    /// Bytes used by security events and flagged seals.
    pub security_event_bytes: u64,
    /// Bytes used by keys.
    pub key_bytes: u64,
    /// Bytes used by agent identity store.
    pub agent_bytes: u64,
    /// Bytes used by GC metadata (tombstones, audit).
    pub gc_bytes: u64,
    /// Bytes used by everything else (objects, HEAD, etc.).
    pub other_bytes: u64,
    /// Configured budget in bytes.
    pub budget_bytes: u64,
}

impl StorageReport {
    /// Overall usage as a percentage of budget.
    pub fn usage_pct(&self) -> f64 {
        if self.budget_bytes == 0 || self.budget_bytes == u64::MAX {
            return 0.0;
        }
        (self.total_bytes as f64 / self.budget_bytes as f64) * 100.0
    }

    /// Build a storage report by walking the `.writ/` directory.
    pub fn scan(writ_dir: &Path, budget_bytes: u64) -> WritResult<Self> {
        let mut report = StorageReport {
            total_bytes: 0,
            seal_bytes: 0,
            working_state_bytes: 0,
            security_event_bytes: 0,
            key_bytes: 0,
            agent_bytes: 0,
            gc_bytes: 0,
            other_bytes: 0,
            budget_bytes,
        };

        if !writ_dir.exists() {
            return Ok(report);
        }

        for entry in walkdir::WalkDir::new(writ_dir)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            report.total_bytes += size;

            // Categorize by subdirectory relative to .writ/
            let rel = match entry.path().strip_prefix(writ_dir) {
                Ok(r) => r,
                Err(_) => {
                    report.other_bytes += size;
                    continue;
                }
            };

            let first_component = rel
                .components()
                .next()
                .and_then(|c| c.as_os_str().to_str())
                .unwrap_or("");

            match first_component {
                "seals" => report.seal_bytes += size,
                "specs" | "heads" => report.working_state_bytes += size,
                "security" => report.security_event_bytes += size,
                "keys" => report.key_bytes += size,
                "agents" => report.agent_bytes += size,
                "gc" => report.gc_bytes += size,
                _ => report.other_bytes += size,
            }
        }

        // index.json is working state
        let index_path = writ_dir.join("index.json");
        if index_path.exists() {
            if let Ok(meta) = fs::metadata(&index_path) {
                let size = meta.len();
                // Already counted in other_bytes, move to working_state
                report.other_bytes = report.other_bytes.saturating_sub(size);
                report.working_state_bytes += size;
            }
        }

        Ok(report)
    }
}

// ---------------------------------------------------------------------------
// GC Plan
// ---------------------------------------------------------------------------

/// A single GC action to perform.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action")]
pub enum GcAction {
    /// Clean (delete) a spec that has reached terminal lifecycle state.
    CleanSpec {
        spec_id: String,
        lifecycle_state: String,
        reason: String,
    },
    /// Transition a spec's lifecycle state.
    TransitionSpec {
        spec_id: String,
        from: String,
        to: String,
        reason: String,
    },
    /// Clean security events past retention.
    CleanSecurityEvents {
        count: usize,
        severity: String,
        reason: String,
    },
}

/// Summary of planned GC actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcSummary {
    pub total_actions: usize,
    pub transitions: usize,
    pub deletions: usize,
    pub events_to_clean: usize,
    pub summary_line: String,
}

/// A complete GC plan — what the scheduler identified for cleanup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcPlan {
    pub generated_at: DateTime<Utc>,
    pub storage: StorageReport,
    pub actions: Vec<GcAction>,
    pub summary: GcSummary,
}

impl GcPlan {
    /// Build a GC plan by scanning specs and events.
    pub fn generate(
        writ_dir: &Path,
        config: &GcConfig,
        specs: &[crate::spec::Spec],
        events: &[SecurityEvent],
    ) -> WritResult<Self> {
        let now = Utc::now();
        let storage = StorageReport::scan(writ_dir, config.budget_bytes)?;
        let mut actions = Vec::new();

        // --- Spec scan ---
        for spec in specs {
            let age = now
                .signed_duration_since(spec.last_activity)
                .num_seconds()
                .max(0) as u64;

            let terminal_age = now
                .signed_duration_since(spec.updated_at)
                .num_seconds()
                .max(0) as u64;

            match spec.lifecycle_state {
                LifecycleState::Active => {
                    if age >= config.specs.stale_timeout_secs {
                        actions.push(GcAction::TransitionSpec {
                            spec_id: spec.id.clone(),
                            from: "active".into(),
                            to: "stale".into(),
                            reason: format!(
                                "No activity for {}h (threshold: {}h)",
                                age / 3600,
                                config.specs.stale_timeout_secs / 3600
                            ),
                        });
                    }
                }
                LifecycleState::Stale => {
                    if terminal_age >= config.specs.expiry_timeout_secs {
                        actions.push(GcAction::TransitionSpec {
                            spec_id: spec.id.clone(),
                            from: "stale".into(),
                            to: "cancelled".into(),
                            reason: format!(
                                "Stale for {}h, past expiry timeout of {}h",
                                terminal_age / 3600,
                                config.specs.expiry_timeout_secs / 3600
                            ),
                        });
                    }
                }
                LifecycleState::Completed => {
                    if terminal_age >= config.specs.retention_period_secs {
                        actions.push(GcAction::CleanSpec {
                            spec_id: spec.id.clone(),
                            lifecycle_state: "completed".into(),
                            reason: format!(
                                "Completed {}d ago, past retention of {}d",
                                terminal_age / 86400,
                                config.specs.retention_period_secs / 86400
                            ),
                        });
                    }
                }
                LifecycleState::Cancelled => {
                    if terminal_age >= config.specs.grace_period_secs {
                        actions.push(GcAction::CleanSpec {
                            spec_id: spec.id.clone(),
                            lifecycle_state: "cancelled".into(),
                            reason: format!(
                                "Cancelled {}h ago, past grace period of {}h",
                                terminal_age / 3600,
                                config.specs.grace_period_secs / 3600
                            ),
                        });
                    }
                }
                LifecycleState::Archived => {
                    actions.push(GcAction::CleanSpec {
                        spec_id: spec.id.clone(),
                        lifecycle_state: "archived".into(),
                        reason: "Archived spec eligible for cleanup".into(),
                    });
                }
            }
        }

        // --- Security event scan ---
        let mut info_expired = 0usize;
        let mut warning_expired = 0usize;
        let mut critical_expired = 0usize;

        for event in events {
            let age = now.signed_duration_since(event.timestamp);
            let retention = config.security_events.retention_for(&event.severity);

            if age > retention {
                match event.severity {
                    Severity::Info => info_expired += 1,
                    Severity::Warning => warning_expired += 1,
                    Severity::Critical => critical_expired += 1,
                }
            }
        }

        if info_expired > 0 {
            actions.push(GcAction::CleanSecurityEvents {
                count: info_expired,
                severity: "info".into(),
                reason: format!(
                    "{} Info events older than {}d",
                    info_expired,
                    config.security_events.retention_info.num_days()
                ),
            });
        }
        if warning_expired > 0 {
            actions.push(GcAction::CleanSecurityEvents {
                count: warning_expired,
                severity: "warning".into(),
                reason: format!(
                    "{} Warning events older than {}d",
                    warning_expired,
                    config.security_events.retention_warning.num_days()
                ),
            });
        }
        if critical_expired > 0 {
            actions.push(GcAction::CleanSecurityEvents {
                count: critical_expired,
                severity: "critical".into(),
                reason: format!(
                    "{} Critical events older than {}d",
                    critical_expired,
                    config.security_events.retention_critical.num_days()
                ),
            });
        }

        // --- Summary ---
        let transitions = actions
            .iter()
            .filter(|a| matches!(a, GcAction::TransitionSpec { .. }))
            .count();
        let deletions = actions
            .iter()
            .filter(|a| matches!(a, GcAction::CleanSpec { .. }))
            .count();
        let events_to_clean = info_expired + warning_expired + critical_expired;

        let mut summary_parts = Vec::new();
        if deletions > 0 {
            summary_parts.push(format!("{} spec(s)", deletions));
        }
        if events_to_clean > 0 {
            summary_parts.push(format!("{} event(s)", events_to_clean));
        }
        if transitions > 0 {
            summary_parts.push(format!("{} transition(s)", transitions));
        }

        let summary_line = if summary_parts.is_empty() {
            "Nothing to clean".into()
        } else {
            format!("Clean: {}", summary_parts.join(", "))
        };

        let total_actions = actions.len();
        Ok(GcPlan {
            generated_at: now,
            storage,
            actions,
            summary: GcSummary {
                total_actions,
                transitions,
                deletions,
                events_to_clean,
                summary_line,
            },
        })
    }
}

// ---------------------------------------------------------------------------
// Tombstone
// ---------------------------------------------------------------------------

/// Record of a cleaned object — lightweight audit trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tombstone {
    pub id: String,
    pub object_type: String,
    pub final_state: String,
    pub cleaned_at: DateTime<Utc>,
    pub reason: String,
}

// GcAuditRecord is defined in security.rs (Amis's implementation).
// Re-exported via the import above.

// ---------------------------------------------------------------------------
// GC Executor
// ---------------------------------------------------------------------------

/// Result of executing a GC plan.
pub struct GcExecutionResult {
    pub audit: GcAuditRecord,
    pub specs_cleaned: Vec<String>,
    pub events_cleaned: usize,
    pub transitions_applied: Vec<(String, String, String)>, // (spec_id, from, to)
}

/// Execute a GC plan safely.
///
/// Safety rules:
/// 1. Never delete seals (no variant exists — compile-time guarantee).
/// 2. Never clean Active specs (runtime double-check).
/// 3. Never clean Critical events regardless of config (runtime check — but
///    the plan generator also respects this, so this is a belt-and-suspenders guard).
pub fn execute_plan(
    writ_dir: &Path,
    plan: &GcPlan,
    specs: &[crate::spec::Spec],
) -> WritResult<GcExecutionResult> {
    let start = std::time::Instant::now();
    let mut executed = 0usize;
    let mut skipped = 0usize;
    let mut skipped_details: Vec<SkippedAction> = Vec::new();
    let mut specs_cleaned = Vec::new();
    let mut events_cleaned = 0usize;
    let mut transitions_applied = Vec::new();

    let gc_dir = writ_dir.join("gc");
    fs::create_dir_all(&gc_dir)?;

    for action in &plan.actions {
        match action {
            GcAction::CleanSpec {
                spec_id,
                lifecycle_state,
                reason,
            } => {
                // Safety: verify spec is actually in a terminal state
                let spec = specs.iter().find(|s| s.id == *spec_id);
                if let Some(s) = spec {
                    if s.lifecycle_state == LifecycleState::Active {
                        skipped += 1;
                        skipped_details.push(SkippedAction {
                            action: format!("CleanSpec '{}'", spec_id),
                            reason: "still Active (safety rule)".into(),
                        });
                        continue;
                    }
                }

                // Write tombstone before deletion
                write_tombstone(
                    &gc_dir,
                    &Tombstone {
                        id: spec_id.clone(),
                        object_type: "spec".into(),
                        final_state: lifecycle_state.clone(),
                        cleaned_at: Utc::now(),
                        reason: reason.clone(),
                    },
                )?;

                // Remove spec file
                let spec_path = writ_dir.join("specs").join(format!("{}.json", spec_id));
                if spec_path.exists() {
                    fs::remove_file(&spec_path)?;
                }

                specs_cleaned.push(spec_id.clone());
                executed += 1;
            }

            GcAction::TransitionSpec {
                spec_id,
                from,
                to,
                reason: _,
            } => {
                // Load, transition, save
                let spec_path = writ_dir.join("specs").join(format!("{}.json", spec_id));
                if !spec_path.exists() {
                    skipped += 1;
                    skipped_details.push(SkippedAction {
                        action: format!("TransitionSpec '{}'", spec_id),
                        reason: "spec file not found".into(),
                    });
                    continue;
                }

                let data = fs::read_to_string(&spec_path)?;
                let mut spec: crate::spec::Spec = serde_json::from_str(&data)?;

                let new_state = match to.as_str() {
                    "stale" => LifecycleState::Stale,
                    "cancelled" => LifecycleState::Cancelled,
                    "completed" => LifecycleState::Completed,
                    "archived" => LifecycleState::Archived,
                    _ => {
                        skipped += 1;
                        skipped_details.push(SkippedAction {
                            action: format!("TransitionSpec '{}'", spec_id),
                            reason: format!("unknown target state '{}'", to),
                        });
                        continue;
                    }
                };

                spec.lifecycle_state = new_state;
                spec.updated_at = Utc::now();

                let json = serde_json::to_string_pretty(&spec)?;
                fs::write(&spec_path, json)?;

                transitions_applied.push((spec_id.clone(), from.clone(), to.clone()));
                executed += 1;
            }

            GcAction::CleanSecurityEvents {
                count,
                severity,
                reason: _,
            } => {
                // Safety: never clean Critical events
                if severity == "critical" {
                    skipped += 1;
                    skipped_details.push(SkippedAction {
                        action: "CleanSecurityEvents (Critical)".into(),
                        reason: "Critical events are never cleaned (safety rule)".into(),
                    });
                    continue;
                }

                events_cleaned += count;
                executed += 1;
                // Actual event file rewrite is handled by the caller (repo.rs)
                // because the SecurityEventLogger owns the file format.
                // The executor just records that this action should happen.
            }
        }
    }

    let duration_ms = start.elapsed().as_millis() as u64;

    let audit = GcAuditRecord::new(
        GcTrigger::Manual,
        plan.actions.len(),
        executed,
        skipped,
        0, // actions_failed — not tracked yet
        0, // space_freed_bytes — not tracked yet
        duration_ms,
        skipped_details,
    );

    // Write audit record via GcAuditLogger
    let logger = GcAuditLogger::new(writ_dir);
    logger.write_record(&audit)?;

    Ok(GcExecutionResult {
        audit,
        specs_cleaned,
        events_cleaned,
        transitions_applied,
    })
}

// Event cleanup is handled by SecurityEventLogger::clean_events() in security.rs.
// Audit logging is handled by GcAuditLogger in security.rs.

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn write_tombstone(gc_dir: &Path, tombstone: &Tombstone) -> WritResult<()> {
    let path = gc_dir.join("tombstones.jsonl");
    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    let mut writer = std::io::BufWriter::new(&file);
    serde_json::to_writer(&mut writer, tombstone)?;
    writeln!(writer)?;
    writer.flush()?;
    Ok(())
}

// write_audit() and read_audit_log() removed — use GcAuditLogger from security.rs.

/// Read tombstone history.
pub fn read_tombstones(writ_dir: &Path) -> WritResult<Vec<Tombstone>> {
    let path = writ_dir.join("gc").join("tombstones.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(&path)?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let record: Tombstone = serde_json::from_str(&line)?;
        records.push(record);
    }
    Ok(records)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{Spec, SpecStatus};
    use chrono::Duration;
    use tempfile::tempdir;

    // --- Config tests ---

    #[test]
    fn test_default_config_has_sensible_values() {
        let config = GcConfig::default();
        assert!(config.budget_bytes > 0);
        assert!(config.specs.stale_timeout_secs > 0);
        assert!(config.specs.retention_period_secs > 0);
        assert!(config.security_events.retention_info.num_days() > 0);
        assert!(config.security_events.retention_warning.num_days() > 0);
        assert!(config.security_events.retention_critical.num_days() > 0);
        assert_eq!(config.mode, GcMode::Manual);
    }

    #[test]
    fn test_profiles_have_valid_allocations() {
        for profile in &["raspberry-pi", "development", "production", "enterprise"] {
            let config = GcConfig::from_profile(profile).unwrap();
            let total = config.allocation.seal_pct as u16
                + config.allocation.working_state_pct as u16
                + config.allocation.security_event_pct as u16
                + config.allocation.headroom_pct as u16;
            assert_eq!(
                total, 100,
                "Profile '{}' allocations don't sum to 100",
                profile
            );
            assert!(
                config.budget_bytes > 0,
                "Profile '{}' has zero budget",
                profile
            );
        }
    }

    #[test]
    fn test_config_json_roundtrip() {
        let config = GcConfig::production();
        let json = serde_json::to_string_pretty(&config).unwrap();
        let recovered: GcConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.budget_bytes, config.budget_bytes);
        assert_eq!(recovered.mode, config.mode);
        assert_eq!(
            recovered.specs.stale_timeout_secs,
            config.specs.stale_timeout_secs
        );
        assert_eq!(
            recovered.security_events.retention_critical.num_days(),
            config.security_events.retention_critical.num_days()
        );
    }

    #[test]
    fn test_from_profile_unknown_returns_error() {
        let result = GcConfig::from_profile("unknown");
        assert!(result.is_err());
    }

    #[test]
    fn test_config_save_and_load() {
        let dir = tempdir().unwrap();
        let config = GcConfig::raspberry_pi();
        config.save(dir.path()).unwrap();

        let loaded = GcConfig::load(dir.path()).unwrap();
        assert_eq!(loaded.budget_bytes, config.budget_bytes);
        assert_eq!(
            loaded.specs.stale_timeout_secs,
            config.specs.stale_timeout_secs
        );
    }

    #[test]
    fn test_config_load_missing_file_returns_default() {
        let dir = tempdir().unwrap();
        let loaded = GcConfig::load(dir.path()).unwrap();
        let default = GcConfig::default();
        assert_eq!(loaded.budget_bytes, default.budget_bytes);
    }

    #[test]
    fn test_profiles_differ_meaningfully() {
        let rpi = GcConfig::raspberry_pi();
        let dev = GcConfig::development();
        let prod = GcConfig::production();
        let ent = GcConfig::enterprise();

        assert!(rpi.budget_bytes < dev.budget_bytes);
        assert!(dev.budget_bytes < prod.budget_bytes);
        assert!(prod.budget_bytes < ent.budget_bytes);

        assert!(rpi.specs.retention_period_secs < dev.specs.retention_period_secs);
    }

    // --- Storage report tests ---

    #[test]
    fn test_storage_report_empty_dir() {
        let dir = tempdir().unwrap();
        let report = StorageReport::scan(dir.path(), 1_000_000).unwrap();
        assert_eq!(report.total_bytes, 0);
        assert_eq!(report.budget_bytes, 1_000_000);
    }

    #[test]
    fn test_storage_report_categorizes_seals() {
        let dir = tempdir().unwrap();
        let seals_dir = dir.path().join("seals");
        fs::create_dir_all(&seals_dir).unwrap();
        fs::write(seals_dir.join("seal-1.json"), r#"{"id":"seal-1"}"#).unwrap();

        let report = StorageReport::scan(dir.path(), 1_000_000).unwrap();
        assert!(report.seal_bytes > 0);
        assert_eq!(report.seal_bytes, report.total_bytes);
    }

    #[test]
    fn test_storage_report_categorizes_security() {
        let dir = tempdir().unwrap();
        let sec_dir = dir.path().join("security");
        fs::create_dir_all(&sec_dir).unwrap();
        fs::write(sec_dir.join("events.jsonl"), "event data\n").unwrap();

        let report = StorageReport::scan(dir.path(), 1_000_000).unwrap();
        assert!(report.security_event_bytes > 0);
    }

    #[test]
    fn test_storage_report_usage_pct() {
        let report = StorageReport {
            total_bytes: 800,
            seal_bytes: 0,
            working_state_bytes: 0,
            security_event_bytes: 0,
            key_bytes: 0,
            agent_bytes: 0,
            gc_bytes: 0,
            other_bytes: 800,
            budget_bytes: 1000,
        };
        assert!((report.usage_pct() - 80.0).abs() < 0.01);
    }

    #[test]
    fn test_storage_report_unlimited_budget() {
        let report = StorageReport {
            total_bytes: 1000,
            seal_bytes: 0,
            working_state_bytes: 0,
            security_event_bytes: 0,
            key_bytes: 0,
            agent_bytes: 0,
            gc_bytes: 0,
            other_bytes: 1000,
            budget_bytes: u64::MAX,
        };
        assert_eq!(report.usage_pct(), 0.0);
    }

    // --- GC Plan tests ---

    fn make_spec(id: &str, lifecycle: LifecycleState, hours_ago: i64) -> Spec {
        let now = Utc::now();
        let ts = now - Duration::hours(hours_ago);
        Spec {
            id: id.into(),
            title: id.into(),
            description: String::new(),
            status: SpecStatus::Pending,
            depends_on: Vec::new(),
            file_scope: Vec::new(),
            created_at: ts,
            updated_at: ts,
            sealed_by: Vec::new(),
            acceptance_criteria: Vec::new(),
            design_notes: Vec::new(),
            tech_stack: Vec::new(),
            lifecycle_state: lifecycle,
            last_activity: ts,
        }
    }

    fn make_event_at(severity: Severity, days_ago: i64) -> SecurityEvent {
        SecurityEvent {
            timestamp: Utc::now() - Duration::days(days_ago),
            severity,
            event_type: "test".into(),
            agent_id: None,
            details: "test".into(),
        }
    }

    #[test]
    fn test_plan_empty_repo() {
        let dir = tempdir().unwrap();
        let config = GcConfig::default();
        let plan = GcPlan::generate(dir.path(), &config, &[], &[]).unwrap();
        assert!(plan.actions.is_empty());
        assert_eq!(plan.summary.summary_line, "Nothing to clean");
    }

    #[test]
    fn test_plan_active_spec_no_cleanup() {
        let dir = tempdir().unwrap();
        let config = GcConfig::default();
        let specs = vec![make_spec("active-spec", LifecycleState::Active, 0)];
        let plan = GcPlan::generate(dir.path(), &config, &specs, &[]).unwrap();

        let clean_actions: Vec<_> = plan
            .actions
            .iter()
            .filter(|a| matches!(a, GcAction::CleanSpec { .. }))
            .collect();
        assert!(clean_actions.is_empty());
    }

    #[test]
    fn test_plan_stale_active_spec_transitions() {
        let dir = tempdir().unwrap();
        let config = GcConfig::default();
        // 3 hours old, stale timeout is 2 hours
        let specs = vec![make_spec("old-spec", LifecycleState::Active, 3)];
        let plan = GcPlan::generate(dir.path(), &config, &specs, &[]).unwrap();

        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            GcAction::TransitionSpec { spec_id, to, .. } => {
                assert_eq!(spec_id, "old-spec");
                assert_eq!(to, "stale");
            }
            other => panic!("expected TransitionSpec, got {:?}", other),
        }
    }

    #[test]
    fn test_plan_cancelled_spec_past_grace_cleaned() {
        let dir = tempdir().unwrap();
        let config = GcConfig::default();
        // Cancelled 2 hours ago, grace period is 1 hour
        let specs = vec![make_spec("done-spec", LifecycleState::Cancelled, 2)];
        let plan = GcPlan::generate(dir.path(), &config, &specs, &[]).unwrap();

        assert_eq!(plan.actions.len(), 1);
        assert!(
            matches!(&plan.actions[0], GcAction::CleanSpec { spec_id, .. } if spec_id == "done-spec")
        );
    }

    #[test]
    fn test_plan_completed_spec_within_retention_not_cleaned() {
        let dir = tempdir().unwrap();
        let config = GcConfig::default();
        // Completed 1 day ago, retention is 7 days
        let specs = vec![make_spec("recent-spec", LifecycleState::Completed, 24)];
        let plan = GcPlan::generate(dir.path(), &config, &specs, &[]).unwrap();

        let clean_actions: Vec<_> = plan
            .actions
            .iter()
            .filter(|a| matches!(a, GcAction::CleanSpec { .. }))
            .collect();
        assert!(clean_actions.is_empty());
    }

    #[test]
    fn test_plan_completed_spec_past_retention_cleaned() {
        let dir = tempdir().unwrap();
        let config = GcConfig::default();
        // Completed 8 days ago, retention is 7 days
        let specs = vec![make_spec(
            "old-completed",
            LifecycleState::Completed,
            8 * 24,
        )];
        let plan = GcPlan::generate(dir.path(), &config, &specs, &[]).unwrap();

        assert!(plan.actions.iter().any(
            |a| matches!(a, GcAction::CleanSpec { spec_id, .. } if spec_id == "old-completed")
        ));
    }

    #[test]
    fn test_plan_old_info_events_flagged() {
        let dir = tempdir().unwrap();
        let config = GcConfig::default();
        // 31 days old info event, retention is 30 days
        let events = vec![make_event_at(Severity::Info, 31)];
        let plan = GcPlan::generate(dir.path(), &config, &[], &events).unwrap();

        assert!(plan.actions.iter().any(|a| matches!(
            a,
            GcAction::CleanSecurityEvents {
                severity, count, ..
            } if severity == "info" && *count == 1
        )));
    }

    #[test]
    fn test_plan_critical_events_within_retention_not_flagged() {
        let dir = tempdir().unwrap();
        let config = GcConfig::default();
        // 1 year old critical event, retention is 2 years
        let events = vec![make_event_at(Severity::Critical, 365)];
        let plan = GcPlan::generate(dir.path(), &config, &[], &events).unwrap();

        let event_actions: Vec<_> = plan
            .actions
            .iter()
            .filter(|a| matches!(a, GcAction::CleanSecurityEvents { .. }))
            .collect();
        assert!(event_actions.is_empty());
    }

    #[test]
    fn test_plan_warning_within_retention_not_flagged() {
        let dir = tempdir().unwrap();
        let config = GcConfig::default();
        // 30 days old warning, retention is 180 days
        let events = vec![make_event_at(Severity::Warning, 30)];
        let plan = GcPlan::generate(dir.path(), &config, &[], &events).unwrap();

        let event_actions: Vec<_> = plan
            .actions
            .iter()
            .filter(|a| matches!(a, GcAction::CleanSecurityEvents { .. }))
            .collect();
        assert!(event_actions.is_empty());
    }

    #[test]
    fn test_plan_summary_counts() {
        let dir = tempdir().unwrap();
        let config = GcConfig::default();
        let specs = vec![
            make_spec("stale", LifecycleState::Active, 3), // transition
            make_spec("gone", LifecycleState::Cancelled, 2), // clean
        ];
        let events = vec![make_event_at(Severity::Info, 31)]; // clean events
        let plan = GcPlan::generate(dir.path(), &config, &specs, &events).unwrap();

        assert_eq!(plan.summary.transitions, 1);
        assert_eq!(plan.summary.deletions, 1);
        assert_eq!(plan.summary.events_to_clean, 1);
        assert!(plan.summary.summary_line.contains("spec"));
        assert!(plan.summary.summary_line.contains("event"));
    }

    #[test]
    fn test_plan_is_serializable() {
        let dir = tempdir().unwrap();
        let config = GcConfig::default();
        let plan = GcPlan::generate(dir.path(), &config, &[], &[]).unwrap();
        let json = serde_json::to_string(&plan).unwrap();
        let _: GcPlan = serde_json::from_str(&json).unwrap();
    }

    // --- Executor tests ---

    #[test]
    fn test_executor_empty_plan() {
        let dir = tempdir().unwrap();
        let config = GcConfig::default();
        let plan = GcPlan::generate(dir.path(), &config, &[], &[]).unwrap();
        let result = execute_plan(dir.path(), &plan, &[]).unwrap();

        assert_eq!(result.audit.actions_planned, 0);
        assert_eq!(result.audit.actions_executed, 0);
        assert_eq!(result.audit.actions_skipped, 0);
    }

    #[test]
    fn test_executor_cleans_spec_creates_tombstone() {
        let dir = tempdir().unwrap();
        let specs_dir = dir.path().join("specs");
        fs::create_dir_all(&specs_dir).unwrap();
        fs::write(
            specs_dir.join("old-spec.json"),
            serde_json::to_string(&make_spec("old-spec", LifecycleState::Cancelled, 2)).unwrap(),
        )
        .unwrap();

        let config = GcConfig::default();
        let specs = vec![make_spec("old-spec", LifecycleState::Cancelled, 2)];
        let plan = GcPlan::generate(dir.path(), &config, &specs, &[]).unwrap();
        let result = execute_plan(dir.path(), &plan, &specs).unwrap();

        assert_eq!(result.specs_cleaned.len(), 1);
        assert_eq!(result.specs_cleaned[0], "old-spec");
        assert!(!specs_dir.join("old-spec.json").exists());

        // Tombstone exists
        let tombstones = read_tombstones(dir.path()).unwrap();
        assert_eq!(tombstones.len(), 1);
        assert_eq!(tombstones[0].id, "old-spec");
        assert_eq!(tombstones[0].object_type, "spec");
    }

    #[test]
    fn test_executor_blocks_active_spec_cleanup() {
        let dir = tempdir().unwrap();
        let specs_dir = dir.path().join("specs");
        fs::create_dir_all(&specs_dir).unwrap();
        fs::write(
            specs_dir.join("live-spec.json"),
            serde_json::to_string(&make_spec("live-spec", LifecycleState::Active, 0)).unwrap(),
        )
        .unwrap();

        // Force a CleanSpec action for an Active spec (shouldn't happen
        // in practice, but the executor must catch it)
        let plan = GcPlan {
            generated_at: Utc::now(),
            storage: StorageReport::scan(dir.path(), 1_000_000).unwrap(),
            actions: vec![GcAction::CleanSpec {
                spec_id: "live-spec".into(),
                lifecycle_state: "active".into(),
                reason: "test".into(),
            }],
            summary: GcSummary {
                total_actions: 1,
                transitions: 0,
                deletions: 1,
                events_to_clean: 0,
                summary_line: "test".into(),
            },
        };

        let specs = vec![make_spec("live-spec", LifecycleState::Active, 0)];
        let result = execute_plan(dir.path(), &plan, &specs).unwrap();

        assert_eq!(result.audit.actions_skipped, 1);
        assert_eq!(result.audit.actions_executed, 0);
        assert!(result.audit.skipped_details[0]
            .reason
            .contains("still Active"));
        // File still exists
        assert!(specs_dir.join("live-spec.json").exists());
    }

    #[test]
    fn test_executor_blocks_critical_event_cleanup() {
        let dir = tempdir().unwrap();
        let plan = GcPlan {
            generated_at: Utc::now(),
            storage: StorageReport::scan(dir.path(), 1_000_000).unwrap(),
            actions: vec![GcAction::CleanSecurityEvents {
                count: 5,
                severity: "critical".into(),
                reason: "test".into(),
            }],
            summary: GcSummary {
                total_actions: 1,
                transitions: 0,
                deletions: 0,
                events_to_clean: 5,
                summary_line: "test".into(),
            },
        };

        let result = execute_plan(dir.path(), &plan, &[]).unwrap();
        assert_eq!(result.audit.actions_skipped, 1);
        assert!(result.audit.skipped_details[0].reason.contains("Critical"));
    }

    #[test]
    fn test_executor_transitions_spec() {
        let dir = tempdir().unwrap();
        let specs_dir = dir.path().join("specs");
        fs::create_dir_all(&specs_dir).unwrap();

        let spec = make_spec("aging-spec", LifecycleState::Active, 3);
        fs::write(
            specs_dir.join("aging-spec.json"),
            serde_json::to_string_pretty(&spec).unwrap(),
        )
        .unwrap();

        let plan = GcPlan {
            generated_at: Utc::now(),
            storage: StorageReport::scan(dir.path(), 1_000_000).unwrap(),
            actions: vec![GcAction::TransitionSpec {
                spec_id: "aging-spec".into(),
                from: "active".into(),
                to: "stale".into(),
                reason: "inactive".into(),
            }],
            summary: GcSummary {
                total_actions: 1,
                transitions: 1,
                deletions: 0,
                events_to_clean: 0,
                summary_line: "test".into(),
            },
        };

        let specs = vec![spec];
        let result = execute_plan(dir.path(), &plan, &specs).unwrap();
        assert_eq!(result.transitions_applied.len(), 1);

        // Verify spec file was updated
        let data = fs::read_to_string(specs_dir.join("aging-spec.json")).unwrap();
        let updated: Spec = serde_json::from_str(&data).unwrap();
        assert_eq!(updated.lifecycle_state, LifecycleState::Stale);
    }

    #[test]
    fn test_executor_writes_audit_record() {
        let dir = tempdir().unwrap();
        let config = GcConfig::default();
        let plan = GcPlan::generate(dir.path(), &config, &[], &[]).unwrap();
        execute_plan(dir.path(), &plan, &[]).unwrap();

        let logger = GcAuditLogger::new(dir.path());
        let audit = logger.read_records().unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].triggered_by, GcTrigger::Manual);
    }

    #[test]
    fn test_multiple_gc_runs_append_audit() {
        let dir = tempdir().unwrap();
        let config = GcConfig::default();

        for _ in 0..3 {
            let plan = GcPlan::generate(dir.path(), &config, &[], &[]).unwrap();
            execute_plan(dir.path(), &plan, &[]).unwrap();
        }

        let logger = GcAuditLogger::new(dir.path());
        let audit = logger.read_records().unwrap();
        assert_eq!(audit.len(), 3);
    }

    #[test]
    fn test_audit_log_limit() {
        let dir = tempdir().unwrap();
        let config = GcConfig::default();

        for _ in 0..5 {
            let plan = GcPlan::generate(dir.path(), &config, &[], &[]).unwrap();
            execute_plan(dir.path(), &plan, &[]).unwrap();
        }

        let logger = GcAuditLogger::new(dir.path());
        let audit = logger.read_last(3).unwrap();
        assert_eq!(audit.len(), 3);
    }

    // --- Event cleanup tests (delegates to SecurityEventLogger::clean_events) ---

    #[test]
    fn test_clean_events_keeps_recent() {
        use crate::security::SecurityEventLogger;

        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());

        let recent = SecurityEvent {
            timestamp: Utc::now(),
            severity: Severity::Info,
            event_type: "recent".into(),
            agent_id: None,
            details: "keep me".into(),
        };
        let old = SecurityEvent {
            timestamp: Utc::now() - Duration::days(31),
            severity: Severity::Info,
            event_type: "old".into(),
            agent_id: None,
            details: "remove me".into(),
        };

        logger.emit_event(&old).unwrap();
        logger.emit_event(&recent).unwrap();

        let config = SecurityEventGcConfig::default();
        let cleaned = logger.clean_events(&config).unwrap();
        assert_eq!(cleaned, 1);

        // Verify remaining events
        let remaining = logger.read_events(None).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].event_type, "recent");
    }

    #[test]
    fn test_clean_events_preserves_critical() {
        use crate::security::SecurityEventLogger;

        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());

        let critical = SecurityEvent {
            timestamp: Utc::now() - Duration::days(365), // 1 year old
            severity: Severity::Critical,
            event_type: "critical".into(),
            agent_id: None,
            details: "must survive".into(),
        };

        logger.emit_event(&critical).unwrap();

        let config = SecurityEventGcConfig::default();
        let cleaned = logger.clean_events(&config).unwrap();
        assert_eq!(cleaned, 0); // Critical within 2-year retention

        let remaining = logger.read_events(None).unwrap();
        assert_eq!(remaining.len(), 1);
    }

    #[test]
    fn test_clean_events_no_file_ok() {
        use crate::security::SecurityEventLogger;

        let dir = tempdir().unwrap();
        let logger = SecurityEventLogger::new(dir.path());
        let config = SecurityEventGcConfig::default();
        let cleaned = logger.clean_events(&config).unwrap();
        assert_eq!(cleaned, 0);
    }

    // --- Gap 2: TransitionSpec unknown target state ---

    #[test]
    fn test_executor_skips_unknown_target_state() {
        let dir = tempdir().unwrap();
        let specs_dir = dir.path().join("specs");
        fs::create_dir_all(&specs_dir).unwrap();

        let spec = make_spec("test-spec", LifecycleState::Active, 3);
        fs::write(
            specs_dir.join("test-spec.json"),
            serde_json::to_string_pretty(&spec).unwrap(),
        )
        .unwrap();

        let plan = GcPlan {
            generated_at: Utc::now(),
            storage: StorageReport::scan(dir.path(), 1_000_000).unwrap(),
            actions: vec![GcAction::TransitionSpec {
                spec_id: "test-spec".into(),
                from: "active".into(),
                to: "nonexistent".into(),
                reason: "test".into(),
            }],
            summary: GcSummary {
                total_actions: 1,
                transitions: 1,
                deletions: 0,
                events_to_clean: 0,
                summary_line: "test".into(),
            },
        };

        let specs = vec![spec];
        let result = execute_plan(dir.path(), &plan, &specs).unwrap();

        assert_eq!(result.audit.actions_skipped, 1);
        assert_eq!(result.audit.actions_executed, 0);
        assert!(result.audit.skipped_details[0]
            .reason
            .contains("unknown target state 'nonexistent'"));

        // Spec file should remain unchanged
        let data = fs::read_to_string(specs_dir.join("test-spec.json")).unwrap();
        let unchanged: Spec = serde_json::from_str(&data).unwrap();
        assert_eq!(unchanged.lifecycle_state, LifecycleState::Active);
    }

    // --- Gap 3: Non-critical event execution ---

    #[test]
    fn test_executor_counts_info_events_cleaned() {
        let dir = tempdir().unwrap();

        let plan = GcPlan {
            generated_at: Utc::now(),
            storage: StorageReport::scan(dir.path(), 1_000_000).unwrap(),
            actions: vec![GcAction::CleanSecurityEvents {
                count: 5,
                severity: "info".into(),
                reason: "test".into(),
            }],
            summary: GcSummary {
                total_actions: 1,
                transitions: 0,
                deletions: 0,
                events_to_clean: 5,
                summary_line: "test".into(),
            },
        };

        let result = execute_plan(dir.path(), &plan, &[]).unwrap();
        assert_eq!(result.events_cleaned, 5);
        assert_eq!(result.audit.actions_executed, 1);
        assert_eq!(result.audit.actions_skipped, 0);
    }

    #[test]
    fn test_executor_counts_warning_events_cleaned() {
        let dir = tempdir().unwrap();

        let plan = GcPlan {
            generated_at: Utc::now(),
            storage: StorageReport::scan(dir.path(), 1_000_000).unwrap(),
            actions: vec![GcAction::CleanSecurityEvents {
                count: 12,
                severity: "warning".into(),
                reason: "test".into(),
            }],
            summary: GcSummary {
                total_actions: 1,
                transitions: 0,
                deletions: 0,
                events_to_clean: 12,
                summary_line: "test".into(),
            },
        };

        let result = execute_plan(dir.path(), &plan, &[]).unwrap();
        assert_eq!(result.events_cleaned, 12);
        assert_eq!(result.audit.actions_executed, 1);
    }

    // --- Gap 4: Mixed-severity plan + execution ---

    #[test]
    fn test_plan_mixed_severity_generates_per_severity_actions() {
        let dir = tempdir().unwrap();
        let config = GcConfig::default();
        // info retention = 30d, warning retention = 180d, critical retention = 730d
        let events = vec![
            make_event_at(Severity::Info, 31),     // past 30d → clean
            make_event_at(Severity::Info, 35),     // past 30d → clean
            make_event_at(Severity::Warning, 181), // past 180d → clean
            make_event_at(Severity::Critical, 365), // within 730d → keep
        ];

        let plan = GcPlan::generate(dir.path(), &config, &[], &events).unwrap();

        // Should produce separate actions for info and warning, none for critical
        let info_action = plan.actions.iter().find(|a| {
            matches!(a, GcAction::CleanSecurityEvents { severity, .. } if severity == "info")
        });
        let warning_action = plan.actions.iter().find(|a| {
            matches!(a, GcAction::CleanSecurityEvents { severity, .. } if severity == "warning")
        });
        let critical_action = plan.actions.iter().find(|a| {
            matches!(a, GcAction::CleanSecurityEvents { severity, .. } if severity == "critical")
        });

        assert!(info_action.is_some(), "expected info cleanup action");
        assert!(warning_action.is_some(), "expected warning cleanup action");
        assert!(critical_action.is_none(), "critical should NOT appear in plan");

        // Verify counts
        if let Some(GcAction::CleanSecurityEvents { count, .. }) = info_action {
            assert_eq!(*count, 2);
        }
        if let Some(GcAction::CleanSecurityEvents { count, .. }) = warning_action {
            assert_eq!(*count, 1);
        }
    }

    #[test]
    fn test_executor_mixed_severity_cleans_non_critical_only() {
        let dir = tempdir().unwrap();

        // Simulate a plan with info, warning, AND critical actions
        let plan = GcPlan {
            generated_at: Utc::now(),
            storage: StorageReport::scan(dir.path(), 1_000_000).unwrap(),
            actions: vec![
                GcAction::CleanSecurityEvents {
                    count: 3,
                    severity: "info".into(),
                    reason: "test".into(),
                },
                GcAction::CleanSecurityEvents {
                    count: 2,
                    severity: "warning".into(),
                    reason: "test".into(),
                },
                GcAction::CleanSecurityEvents {
                    count: 1,
                    severity: "critical".into(),
                    reason: "test".into(),
                },
            ],
            summary: GcSummary {
                total_actions: 3,
                transitions: 0,
                deletions: 0,
                events_to_clean: 6,
                summary_line: "test".into(),
            },
        };

        let result = execute_plan(dir.path(), &plan, &[]).unwrap();

        // Info + warning cleaned, critical skipped
        assert_eq!(result.events_cleaned, 5); // 3 + 2
        assert_eq!(result.audit.actions_executed, 2);
        assert_eq!(result.audit.actions_skipped, 1);
        assert!(result.audit.skipped_details[0].reason.contains("Critical"));
    }

    // --- P-1: GcSummary.total_actions count fix ---

    #[test]
    fn test_plan_summary_total_actions_counts_event_actions_individually() {
        let dir = tempdir().unwrap();
        let config = GcConfig::default();
        // info past 30d, warning past 180d → 2 separate CleanSecurityEvents actions
        let events = vec![
            make_event_at(Severity::Info, 31),
            make_event_at(Severity::Warning, 181),
        ];
        let plan = GcPlan::generate(dir.path(), &config, &[], &events).unwrap();

        // 2 event cleanup actions
        let event_actions: Vec<_> = plan
            .actions
            .iter()
            .filter(|a| matches!(a, GcAction::CleanSecurityEvents { .. }))
            .collect();
        assert_eq!(event_actions.len(), 2);

        // total_actions should match actions.len()
        assert_eq!(
            plan.summary.total_actions,
            plan.actions.len(),
            "total_actions should equal actions.len(), not group event cleanups"
        );
    }

    // --- Tombstone tests ---

    #[test]
    fn test_tombstone_roundtrip() {
        let tombstone = Tombstone {
            id: "spec-123".into(),
            object_type: "spec".into(),
            final_state: "cancelled".into(),
            cleaned_at: Utc::now(),
            reason: "past grace period".into(),
        };
        let json = serde_json::to_string(&tombstone).unwrap();
        let recovered: Tombstone = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.id, "spec-123");
        assert_eq!(recovered.object_type, "spec");
    }
}
