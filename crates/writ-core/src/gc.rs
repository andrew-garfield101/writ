//! Garbage collection and lifecycle management.
//!
//! Provides storage tracking, GC plan generation, and safe cleanup
//! of expired specs, old security events, and other working state.
//! Seals are immutable and never deleted.

use std::collections::HashSet;
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

/// Object storage configuration (compression, size limits).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Compression algorithm ("zstd" or "none").
    pub compression: String,
    /// Compression level (1 = fast, 22 = max). Default: 3.
    pub compression_level: i32,
    /// Maximum decompressed object size in bytes. Default: 100 MB.
    pub max_object_size_bytes: usize,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            compression: "zstd".to_string(),
            compression_level: 3,
            max_object_size_bytes: 100 * 1024 * 1024, // 100 MB
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
    /// Object store compression settings.
    #[serde(default)]
    pub storage: StorageConfig,
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
            storage: StorageConfig {
                compression: "zstd".to_string(),
                compression_level: 3,
                max_object_size_bytes: 100 * 1024 * 1024, // 100 MB
            },
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
            storage: StorageConfig {
                compression: "zstd".to_string(),
                compression_level: 1, // minimize CPU on weak processor
                max_object_size_bytes: 10 * 1024 * 1024, // 10 MB
            },
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
            storage: StorageConfig {
                compression: "zstd".to_string(),
                compression_level: 3,
                max_object_size_bytes: 100 * 1024 * 1024, // 100 MB
            },
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
            storage: StorageConfig {
                compression: "zstd".to_string(),
                compression_level: 6, // better ratio, servers have CPU headroom
                max_object_size_bytes: 256 * 1024 * 1024, // 256 MB
            },
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
    /// Object store compression statistics (None if objects dir missing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compression: Option<crate::object::CompressionStats>,
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
            compression: None,
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

        // index.json is working state — check workspace path (v2) then flat (v1).
        let ws_index = writ_dir.join("workspaces").join("main").join("index.json");
        let flat_index = writ_dir.join("index.json");
        let index_path = if ws_index.exists() {
            Some(ws_index)
        } else if flat_index.exists() {
            Some(flat_index)
        } else {
            None
        };
        if let Some(path) = index_path {
            if let Ok(meta) = fs::metadata(&path) {
                let size = meta.len();
                // Already counted in other_bytes, move to working_state
                report.other_bytes = report.other_bytes.saturating_sub(size);
                report.working_state_bytes += size;
            }
        }

        // Compression statistics from the object store.
        let objects_dir = writ_dir.join("objects");
        if objects_dir.exists() {
            let store = crate::object::ObjectStore::new(&objects_dir);
            report.compression = store.compression_stats().ok();
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
    /// Prune orphaned objects not referenced by any seal.
    PruneObjects {
        count: usize,
        total_bytes: u64,
        reason: String,
    },
    /// Recompress legacy (uncompressed) objects with zstd.
    RecompressObjects {
        count: usize,
        estimated_savings_bytes: u64,
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
    pub objects_to_prune: usize,
    pub objects_to_recompress: usize,
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

        // --- Object orphan scan ---
        let all_seals = load_all_seals(writ_dir)?;
        let orphans = find_orphaned_objects(writ_dir, &all_seals)?;
        let objects_to_prune = orphans.len();
        if !orphans.is_empty() {
            let total_bytes: u64 = orphans.iter().map(|o| o.size_bytes).sum();
            actions.push(GcAction::PruneObjects {
                count: orphans.len(),
                total_bytes,
                reason: format!(
                    "{} orphaned object(s), {:.1} MB reclaimable",
                    orphans.len(),
                    total_bytes as f64 / 1_048_576.0
                ),
            });
        }

        // --- Recompression scan ---
        let uncompressed = find_uncompressed_objects(writ_dir)?;
        let objects_to_recompress = uncompressed.len();
        if !uncompressed.is_empty() {
            let total_bytes: u64 = uncompressed.iter().map(|o| o.size_bytes).sum();
            actions.push(GcAction::RecompressObjects {
                count: uncompressed.len(),
                estimated_savings_bytes: total_bytes / 3, // rough estimate: ~33% savings
                reason: format!("{} legacy uncompressed object(s)", uncompressed.len()),
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
        if objects_to_prune > 0 {
            summary_parts.push(format!("{} object(s) to prune", objects_to_prune));
        }
        if objects_to_recompress > 0 {
            summary_parts.push(format!("{} object(s) to recompress", objects_to_recompress));
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
                objects_to_prune,
                objects_to_recompress,
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
    pub objects_pruned: usize,
    pub bytes_freed: u64,
    pub objects_recompressed: usize,
    pub recompression_savings: u64,
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
    let mut objects_pruned = 0usize;
    let mut bytes_freed = 0u64;
    let mut objects_recompressed = 0usize;
    let mut recompression_savings = 0u64;

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

            GcAction::PruneObjects {
                count: _,
                total_bytes: _,
                reason,
            } => {
                // Safety: re-verify reachability at execution time.
                // The plan may be stale — a seal created between plan generation
                // and execution could reference a previously-orphaned object.
                let fresh_seals = load_all_seals(writ_dir)?;
                let fresh_orphans = find_orphaned_objects(writ_dir, &fresh_seals)?;

                let objects_dir = writ_dir.join("objects");
                for orphan in &fresh_orphans {
                    let (prefix, rest) = orphan.hash.split_at(2);
                    let obj_path = objects_dir.join(prefix).join(rest);
                    if obj_path.exists() {
                        fs::remove_file(&obj_path)?;
                        objects_pruned += 1;
                        bytes_freed += orphan.size_bytes;

                        // Write tombstone for the pruned object
                        write_tombstone(
                            &gc_dir,
                            &Tombstone {
                                id: orphan.hash.clone(),
                                object_type: "object".into(),
                                final_state: "orphaned".into(),
                                cleaned_at: Utc::now(),
                                reason: reason.clone(),
                            },
                        )?;

                        // Clean up empty prefix directory
                        let prefix_dir = objects_dir.join(prefix);
                        if let Ok(mut entries) = prefix_dir.read_dir() {
                            if entries.next().is_none() {
                                fs::remove_dir(&prefix_dir).ok();
                            }
                        }
                    }
                }
                executed += 1;
            }

            GcAction::RecompressObjects {
                count: _,
                estimated_savings_bytes: _,
                reason: _,
            } => {
                // Re-scan for uncompressed objects at execution time
                let fresh_uncompressed = find_uncompressed_objects(writ_dir)?;
                let objects_dir = writ_dir.join("objects");
                let compression_level = GcConfig::load(writ_dir)
                    .unwrap_or_default()
                    .storage
                    .compression_level;

                for obj in &fresh_uncompressed {
                    let (prefix, rest) = obj.hash.split_at(2);
                    let obj_path = objects_dir.join(prefix).join(rest);
                    if !obj_path.exists() {
                        continue;
                    }

                    let raw_data = fs::read(&obj_path)?;
                    // Legacy objects: entire file is raw content (no magic byte)
                    let content = &raw_data[..];
                    let compressed = crate::object::compress_object(content, compression_level);

                    if compressed.len() < raw_data.len() {
                        // Worth compressing — atomic rewrite
                        crate::fsutil::atomic_write(&obj_path, &compressed)?;
                        objects_recompressed += 1;
                        recompression_savings += raw_data.len() as u64 - compressed.len() as u64;
                    } else {
                        // Not worth it — mark with MAGIC_RAW so we don't retry
                        let mut raw_prefixed = Vec::with_capacity(1 + content.len());
                        raw_prefixed.push(0x00); // MAGIC_RAW
                        raw_prefixed.extend_from_slice(content);
                        crate::fsutil::atomic_write(&obj_path, &raw_prefixed)?;
                    }
                }
                executed += 1;
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
        bytes_freed + recompression_savings,
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
        objects_pruned,
        bytes_freed,
        objects_recompressed,
        recompression_savings,
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
// Object Reachability + Pruning
// ---------------------------------------------------------------------------

/// An unreferenced object identified by reachability analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrphanedObject {
    pub hash: String,
    pub size_bytes: u64,
}

/// Load all seals from `.writ/seals/` without going through Repository.
///
/// This reads every `.json` file in the seals directory and deserializes it.
/// Used by GC plan generation which only has `writ_dir`, not a full repo.
pub fn load_all_seals(writ_dir: &Path) -> WritResult<Vec<crate::seal::Seal>> {
    let seals_dir = writ_dir.join("seals");
    if !seals_dir.exists() {
        return Ok(Vec::new());
    }
    let mut seals = Vec::new();
    for entry in fs::read_dir(&seals_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            let data = fs::read_to_string(&path)?;
            let seal: crate::seal::Seal = serde_json::from_str(&data)?;
            seals.push(seal);
        }
    }
    Ok(seals)
}

/// Find orphaned objects not referenced by any seal.
///
/// Walks all seals to build a set of referenced hashes (tree, old_hash,
/// new_hash), then walks `.writ/objects/` to find on-disk objects not in
/// the referenced set.
///
/// **CRITICAL SAFETY:** The `seals` parameter MUST include ALL seals from
/// `.writ/seals/` — including flagged/suspicious seals. Flagged seals'
/// objects must NOT be pruned (they contain forensic evidence for
/// investigating compromised agents). Use `load_all_seals()` to ensure
/// no seals are filtered out.
pub fn find_orphaned_objects(
    writ_dir: &Path,
    seals: &[crate::seal::Seal],
) -> WritResult<Vec<OrphanedObject>> {
    // 1. Build referenced hash set from all seals
    let mut referenced = HashSet::new();
    for seal in seals {
        referenced.insert(seal.tree.clone());
        for change in &seal.changes {
            if let Some(ref h) = change.old_hash {
                referenced.insert(h.clone());
            }
            if let Some(ref h) = change.new_hash {
                referenced.insert(h.clone());
            }
        }
    }

    // 2. Walk objects directory, find orphans
    let objects_dir = writ_dir.join("objects");
    if !objects_dir.exists() {
        return Ok(Vec::new());
    }

    let mut orphans = Vec::new();
    for prefix_entry in fs::read_dir(&objects_dir)? {
        let prefix_entry = prefix_entry?;
        if !prefix_entry.file_type()?.is_dir() {
            continue;
        }
        let prefix = prefix_entry.file_name().to_str().unwrap_or("").to_string();
        if prefix.len() != 2 {
            continue;
        }

        for obj_entry in fs::read_dir(prefix_entry.path())? {
            let obj_entry = obj_entry?;
            if !obj_entry.file_type()?.is_file() {
                continue;
            }
            let rest = obj_entry.file_name().to_str().unwrap_or("").to_string();
            let hash = format!("{}{}", prefix, rest);

            if !referenced.contains(&hash) {
                let size_bytes = obj_entry.metadata()?.len();
                orphans.push(OrphanedObject { hash, size_bytes });
            }
        }
    }

    Ok(orphans)
}

/// A legacy or explicit-raw object that can be recompressed.
#[derive(Debug, Clone)]
pub struct UncompressedObject {
    pub hash: String,
    pub size_bytes: u64,
    /// True if the object has no magic byte (legacy pre-compression format).
    /// False should not occur in practice since MAGIC_RAW objects are skipped.
    pub is_legacy: bool,
}

/// Find objects that can be recompressed (legacy objects without magic byte).
///
/// Only legacy objects (no magic byte prefix) are candidates. Objects with
/// `MAGIC_RAW` (0x00) were already evaluated and found incompressible — they
/// won't be retried. Objects with `MAGIC_ZSTD` (0x01) are already compressed.
pub fn find_uncompressed_objects(writ_dir: &Path) -> WritResult<Vec<UncompressedObject>> {
    let objects_dir = writ_dir.join("objects");
    if !objects_dir.exists() {
        return Ok(Vec::new());
    }

    let mut uncompressed = Vec::new();
    for prefix_entry in fs::read_dir(&objects_dir)? {
        let prefix_entry = prefix_entry?;
        if !prefix_entry.file_type()?.is_dir() {
            continue;
        }
        let prefix = prefix_entry.file_name().to_str().unwrap_or("").to_string();
        if prefix.len() != 2 {
            continue;
        }

        for obj_entry in fs::read_dir(prefix_entry.path())? {
            let obj_entry = obj_entry?;
            if !obj_entry.file_type()?.is_file() {
                continue;
            }

            let path = obj_entry.path();
            let size_bytes = obj_entry.metadata()?.len();

            // Read just the first byte to check format
            if size_bytes == 0 {
                continue;
            }
            let mut first_byte = [0u8; 1];
            let mut file = File::open(&path)?;
            use std::io::Read;
            if file.read(&mut first_byte)? == 0 {
                continue;
            }

            // Skip already-compressed (MAGIC_ZSTD = 0x01)
            // Skip already-evaluated raw (MAGIC_RAW = 0x00)
            if first_byte[0] == 0x01 || first_byte[0] == 0x00 {
                continue;
            }

            // Legacy object — no magic byte, entire content is raw
            let rest = obj_entry.file_name().to_str().unwrap_or("").to_string();
            let hash = format!("{}{}", prefix, rest);
            uncompressed.push(UncompressedObject {
                hash,
                size_bytes,
                is_legacy: true,
            });
        }
    }

    Ok(uncompressed)
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

    // --- StorageConfig tests ---

    #[test]
    fn test_storage_config_default_level_3() {
        let config = StorageConfig::default();
        assert_eq!(config.compression, "zstd");
        assert_eq!(config.compression_level, 3);
        assert_eq!(config.max_object_size_bytes, 100 * 1024 * 1024);
    }

    #[test]
    fn test_storage_config_per_profile_levels() {
        let rpi = GcConfig::raspberry_pi();
        assert_eq!(rpi.storage.compression_level, 1);
        assert_eq!(rpi.storage.max_object_size_bytes, 10 * 1024 * 1024);

        let dev = GcConfig::development();
        assert_eq!(dev.storage.compression_level, 3);

        let prod = GcConfig::production();
        assert_eq!(prod.storage.compression_level, 3);

        let ent = GcConfig::enterprise();
        assert_eq!(ent.storage.compression_level, 6);
        assert_eq!(ent.storage.max_object_size_bytes, 256 * 1024 * 1024);
    }

    #[test]
    fn test_storage_config_json_roundtrip() {
        let config = StorageConfig {
            compression: "zstd".to_string(),
            compression_level: 6,
            max_object_size_bytes: 256 * 1024 * 1024,
        };
        let json = serde_json::to_string(&config).unwrap();
        let recovered: StorageConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.compression_level, 6);
        assert_eq!(recovered.compression, "zstd");
    }

    #[test]
    fn test_storage_config_backward_compat_missing_field() {
        // Old GcConfig JSON without storage field should deserialize OK
        // because #[serde(default)] on the storage field.
        let json = r#"{
            "mode": "manual",
            "budget_bytes": 1000000,
            "specs": {"stale_timeout_secs": 7200, "expiry_timeout_secs": 86400, "retention_period_secs": 604800, "grace_period_secs": 3600},
            "security_events": {"retention_critical": 730, "retention_warning": 180, "retention_info": 30},
            "allocation": {"seal_pct": 60, "working_state_pct": 20, "security_event_pct": 15, "headroom_pct": 5},
            "warning_threshold_pct": 80
        }"#;
        let config: GcConfig = serde_json::from_str(json).unwrap();
        // Should get defaults when storage field is missing.
        assert_eq!(config.storage.compression_level, 3);
        assert_eq!(config.storage.compression, "zstd");
    }

    #[test]
    fn test_storage_config_from_profile_names() {
        // All valid profile name variants should load correctly
        assert_eq!(
            GcConfig::from_profile("raspberry-pi")
                .unwrap()
                .storage
                .compression_level,
            1
        );
        assert_eq!(
            GcConfig::from_profile("raspberry_pi")
                .unwrap()
                .storage
                .compression_level,
            1
        );
        assert_eq!(
            GcConfig::from_profile("development")
                .unwrap()
                .storage
                .compression_level,
            3
        );
        assert_eq!(
            GcConfig::from_profile("dev")
                .unwrap()
                .storage
                .compression_level,
            3
        );
        assert_eq!(
            GcConfig::from_profile("production")
                .unwrap()
                .storage
                .compression_level,
            3
        );
        assert_eq!(
            GcConfig::from_profile("prod")
                .unwrap()
                .storage
                .compression_level,
            3
        );
        assert_eq!(
            GcConfig::from_profile("enterprise")
                .unwrap()
                .storage
                .compression_level,
            6
        );
        assert!(GcConfig::from_profile("unknown").is_err());
    }

    #[test]
    fn test_storage_config_all_profiles_use_zstd() {
        // Every profile should use zstd compression
        for name in &["dev", "raspberry-pi", "prod", "enterprise"] {
            let config = GcConfig::from_profile(name).unwrap();
            assert_eq!(
                config.storage.compression, "zstd",
                "profile '{name}' should use zstd"
            );
        }
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
            compression: None,
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
            compression: None,
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
            completion_summary: None,
            commit_state: crate::spec::CommitState::Uncommitted,
            completed_at: None,
            commit_hash: None,
            committed_at: None,
            workspace: None,
            claimed_by: None,
            genesis_tree: None,
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
                objects_to_prune: 0,
                objects_to_recompress: 0,
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
                objects_to_prune: 0,
                objects_to_recompress: 0,
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
                objects_to_prune: 0,
                objects_to_recompress: 0,
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
                objects_to_prune: 0,
                objects_to_recompress: 0,
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
                objects_to_prune: 0,
                objects_to_recompress: 0,
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
                objects_to_prune: 0,
                objects_to_recompress: 0,
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
            make_event_at(Severity::Info, 31),      // past 30d → clean
            make_event_at(Severity::Info, 35),      // past 30d → clean
            make_event_at(Severity::Warning, 181),  // past 180d → clean
            make_event_at(Severity::Critical, 365), // within 730d → keep
        ];

        let plan = GcPlan::generate(dir.path(), &config, &[], &events).unwrap();

        // Should produce separate actions for info and warning, none for critical
        let info_action = plan.actions.iter().find(
            |a| matches!(a, GcAction::CleanSecurityEvents { severity, .. } if severity == "info"),
        );
        let warning_action = plan.actions.iter().find(|a| {
            matches!(a, GcAction::CleanSecurityEvents { severity, .. } if severity == "warning")
        });
        let critical_action = plan.actions.iter().find(|a| {
            matches!(a, GcAction::CleanSecurityEvents { severity, .. } if severity == "critical")
        });

        assert!(info_action.is_some(), "expected info cleanup action");
        assert!(warning_action.is_some(), "expected warning cleanup action");
        assert!(
            critical_action.is_none(),
            "critical should NOT appear in plan"
        );

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
                objects_to_prune: 0,
                objects_to_recompress: 0,
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

    // --- Reachability / orphan detection tests ---

    use crate::seal::{
        AgentIdentity, AgentType, ChangeType, FileChange, Seal, TaskStatus, Verification,
    };

    /// Helper: create a minimal seal for reachability tests.
    fn make_test_seal(tree: &str, changes: Vec<FileChange>, spec_id: Option<&str>) -> Seal {
        Seal {
            id: format!("seal-{}", tree.chars().take(8).collect::<String>()),
            parent: None,
            timestamp: Utc::now(),
            tree: tree.to_string(),
            agent: AgentIdentity {
                id: "test-agent".into(),
                agent_type: AgentType::Agent,
            },
            spec_id: spec_id.map(|s| s.to_string()),
            status: TaskStatus::InProgress,
            changes,
            verification: Verification::default(),
            summary: "test seal".into(),
            warnings: Vec::new(),
            parent_seal_hash: None,
            content_hash: None,
            chain_hash: None,
            signature: None,
            workspace: "main".to_string(),
            convergence: None,
        }
    }

    /// Helper: write an object file on disk (simulating ObjectStore).
    fn write_test_object(writ_dir: &Path, hash: &str, content: &[u8]) {
        let (prefix, rest) = hash.split_at(2);
        let dir = writ_dir.join("objects").join(prefix);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(rest), content).unwrap();
    }

    /// Helper: write a seal JSON file on disk.
    fn write_test_seal(writ_dir: &Path, seal: &Seal) {
        let dir = writ_dir.join("seals");
        fs::create_dir_all(&dir).unwrap();
        let json = serde_json::to_string_pretty(seal).unwrap();
        fs::write(dir.join(format!("{}.json", seal.id)), json).unwrap();
    }

    #[test]
    fn test_no_orphans_when_all_referenced() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();

        let tree_hash = "aa".to_string() + &"a".repeat(62);
        let file_hash = "bb".to_string() + &"b".repeat(62);

        write_test_object(writ_dir, &tree_hash, b"tree data");
        write_test_object(writ_dir, &file_hash, b"file data");

        let seal = make_test_seal(
            &tree_hash,
            vec![FileChange {
                path: "src/main.rs".into(),
                change_type: ChangeType::Added,
                old_hash: None,
                new_hash: Some(file_hash.clone()),
            }],
            None,
        );

        let orphans = find_orphaned_objects(writ_dir, &[seal]).unwrap();
        assert!(
            orphans.is_empty(),
            "all objects are referenced, should be no orphans"
        );
    }

    #[test]
    fn test_orphan_detected() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();

        let tree_hash = "aa".to_string() + &"a".repeat(62);
        let file_hash = "bb".to_string() + &"b".repeat(62);
        let orphan_hash = "cc".to_string() + &"c".repeat(62);

        write_test_object(writ_dir, &tree_hash, b"tree data");
        write_test_object(writ_dir, &file_hash, b"file data");
        write_test_object(writ_dir, &orphan_hash, b"orphan data");

        let seal = make_test_seal(
            &tree_hash,
            vec![FileChange {
                path: "src/main.rs".into(),
                change_type: ChangeType::Added,
                old_hash: None,
                new_hash: Some(file_hash.clone()),
            }],
            None,
        );

        let orphans = find_orphaned_objects(writ_dir, &[seal]).unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].hash, orphan_hash);
    }

    #[test]
    fn test_tree_hash_included_in_referenced() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();

        let tree_hash = "aa".to_string() + &"a".repeat(62);
        write_test_object(writ_dir, &tree_hash, b"tree index content");

        let seal = make_test_seal(&tree_hash, vec![], None);

        let orphans = find_orphaned_objects(writ_dir, &[seal]).unwrap();
        assert!(orphans.is_empty(), "tree hash should be referenced");
    }

    #[test]
    fn test_old_hash_included_in_referenced() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();

        let tree_hash = "aa".to_string() + &"a".repeat(62);
        let old_hash = "bb".to_string() + &"b".repeat(62);
        let new_hash = "cc".to_string() + &"c".repeat(62);

        write_test_object(writ_dir, &tree_hash, b"tree");
        write_test_object(writ_dir, &old_hash, b"old version");
        write_test_object(writ_dir, &new_hash, b"new version");

        let seal = make_test_seal(
            &tree_hash,
            vec![FileChange {
                path: "src/lib.rs".into(),
                change_type: ChangeType::Modified,
                old_hash: Some(old_hash.clone()),
                new_hash: Some(new_hash.clone()),
            }],
            None,
        );

        let orphans = find_orphaned_objects(writ_dir, &[seal]).unwrap();
        assert!(orphans.is_empty(), "old_hash should be referenced");
    }

    #[test]
    fn test_new_hash_included_in_referenced() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();

        let tree_hash = "aa".to_string() + &"a".repeat(62);
        let new_hash = "dd".to_string() + &"d".repeat(62);

        write_test_object(writ_dir, &tree_hash, b"tree");
        write_test_object(writ_dir, &new_hash, b"new content");

        let seal = make_test_seal(
            &tree_hash,
            vec![FileChange {
                path: "README.md".into(),
                change_type: ChangeType::Added,
                old_hash: None,
                new_hash: Some(new_hash.clone()),
            }],
            None,
        );

        let orphans = find_orphaned_objects(writ_dir, &[seal]).unwrap();
        assert!(orphans.is_empty(), "new_hash should be referenced");
    }

    #[test]
    fn test_multiple_specs_all_contribute() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();

        let tree1 = "aa".to_string() + &"a".repeat(62);
        let tree2 = "bb".to_string() + &"b".repeat(62);
        let file1 = "cc".to_string() + &"c".repeat(62);
        let file2 = "dd".to_string() + &"d".repeat(62);

        write_test_object(writ_dir, &tree1, b"tree1");
        write_test_object(writ_dir, &tree2, b"tree2");
        write_test_object(writ_dir, &file1, b"file1");
        write_test_object(writ_dir, &file2, b"file2");

        let seal1 = make_test_seal(
            &tree1,
            vec![FileChange {
                path: "a.rs".into(),
                change_type: ChangeType::Added,
                old_hash: None,
                new_hash: Some(file1.clone()),
            }],
            Some("spec-a"),
        );
        let seal2 = make_test_seal(
            &tree2,
            vec![FileChange {
                path: "b.rs".into(),
                change_type: ChangeType::Added,
                old_hash: None,
                new_hash: Some(file2.clone()),
            }],
            Some("spec-b"),
        );

        let orphans = find_orphaned_objects(writ_dir, &[seal1, seal2]).unwrap();
        assert!(
            orphans.is_empty(),
            "objects from both specs should be referenced"
        );
    }

    #[test]
    fn test_empty_repo_no_orphans() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();
        // No objects dir, no seals
        let orphans = find_orphaned_objects(writ_dir, &[]).unwrap();
        assert!(orphans.is_empty());
    }

    #[test]
    fn test_orphan_has_correct_size() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();

        let orphan_hash = "ee".to_string() + &"e".repeat(62);
        let content = b"this is exactly 29 bytes long";
        write_test_object(writ_dir, &orphan_hash, content);

        let orphans = find_orphaned_objects(writ_dir, &[]).unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].size_bytes, content.len() as u64);
    }

    #[test]
    fn test_flagged_seal_objects_not_orphaned() {
        // Security interlock: objects referenced ONLY by a flagged seal
        // must NOT be treated as orphans. The key is that load_all_seals()
        // includes flagged seals in the list — we verify here that if the
        // seal list includes the flagged seal, its objects are protected.
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();

        let flagged_tree = "ff".to_string() + &"f".repeat(62);
        let flagged_file = "ab".to_string() + &"1".repeat(62);

        write_test_object(writ_dir, &flagged_tree, b"flagged tree");
        write_test_object(writ_dir, &flagged_file, b"flagged content");

        // Create a seal that would be flagged (but still exists on disk)
        let flagged_seal = make_test_seal(
            &flagged_tree,
            vec![FileChange {
                path: "malicious.rs".into(),
                change_type: ChangeType::Added,
                old_hash: None,
                new_hash: Some(flagged_file.clone()),
            }],
            Some("compromised-spec"),
        );

        // Write the seal to disk (flagged seals stay in .writ/seals/)
        write_test_seal(writ_dir, &flagged_seal);

        // load_all_seals includes the flagged seal
        let all_seals = load_all_seals(writ_dir).unwrap();
        assert_eq!(all_seals.len(), 1);

        let orphans = find_orphaned_objects(writ_dir, &all_seals).unwrap();
        assert!(
            orphans.is_empty(),
            "objects referenced by flagged seal must NOT be orphaned — forensic evidence"
        );
    }

    #[test]
    fn test_objects_dir_missing_returns_empty() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();
        // Create seals but no objects dir
        let seal = make_test_seal(&("aa".to_string() + &"a".repeat(62)), vec![], None);
        let orphans = find_orphaned_objects(writ_dir, &[seal]).unwrap();
        assert!(orphans.is_empty());
    }

    #[test]
    fn test_load_all_seals_reads_from_disk() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();

        let seal1 = make_test_seal(
            &("aa".to_string() + &"a".repeat(62)),
            vec![],
            Some("spec-1"),
        );
        let seal2 = make_test_seal(
            &("bb".to_string() + &"b".repeat(62)),
            vec![],
            Some("spec-2"),
        );

        write_test_seal(writ_dir, &seal1);
        write_test_seal(writ_dir, &seal2);

        let loaded = load_all_seals(writ_dir).unwrap();
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn test_load_all_seals_empty_dir() {
        let dir = tempdir().unwrap();
        let loaded = load_all_seals(dir.path()).unwrap();
        assert!(loaded.is_empty());
    }

    // --- PruneObjects plan + executor tests ---

    /// Helper: set up a writ_dir with objects, seals, and specs for plan generation.
    fn setup_gc_writ_dir(writ_dir: &Path) {
        fs::create_dir_all(writ_dir.join("seals")).unwrap();
        fs::create_dir_all(writ_dir.join("specs")).unwrap();
        fs::create_dir_all(writ_dir.join("objects")).unwrap();
        fs::create_dir_all(writ_dir.join("gc")).unwrap();
    }

    #[test]
    fn test_plan_includes_prune_objects_when_orphans_exist() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();
        setup_gc_writ_dir(writ_dir);

        // Create an orphan object (not referenced by any seal)
        let orphan_hash = "cc".to_string() + &"c".repeat(62);
        write_test_object(writ_dir, &orphan_hash, b"orphan data");

        let config = GcConfig::default();
        let plan = GcPlan::generate(writ_dir, &config, &[], &[]).unwrap();

        let prune_action = plan
            .actions
            .iter()
            .find(|a| matches!(a, GcAction::PruneObjects { .. }));
        assert!(prune_action.is_some(), "should have PruneObjects action");
        if let Some(GcAction::PruneObjects {
            count, total_bytes, ..
        }) = prune_action
        {
            assert_eq!(*count, 1);
            assert_eq!(*total_bytes, 11); // "orphan data" = 11 bytes
        }
        assert_eq!(plan.summary.objects_to_prune, 1);
    }

    #[test]
    fn test_plan_no_prune_when_no_orphans() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();
        setup_gc_writ_dir(writ_dir);

        // Create object referenced by a seal
        let tree_hash = "aa".to_string() + &"a".repeat(62);
        write_test_object(writ_dir, &tree_hash, b"tree");

        let seal = make_test_seal(&tree_hash, vec![], None);
        write_test_seal(writ_dir, &seal);

        let config = GcConfig::default();
        let plan = GcPlan::generate(writ_dir, &config, &[], &[]).unwrap();

        let prune_action = plan
            .actions
            .iter()
            .find(|a| matches!(a, GcAction::PruneObjects { .. }));
        assert!(prune_action.is_none(), "no orphans means no PruneObjects");
        assert_eq!(plan.summary.objects_to_prune, 0);
    }

    #[test]
    fn test_executor_prunes_orphan_files() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();
        setup_gc_writ_dir(writ_dir);

        let orphan_hash = "dd".to_string() + &"d".repeat(62);
        write_test_object(writ_dir, &orphan_hash, b"orphan");

        let plan = GcPlan {
            generated_at: Utc::now(),
            storage: StorageReport::scan(writ_dir, 1_000_000).unwrap(),
            actions: vec![GcAction::PruneObjects {
                count: 1,
                total_bytes: 6,
                reason: "test".into(),
            }],
            summary: GcSummary {
                total_actions: 1,
                transitions: 0,
                deletions: 0,
                events_to_clean: 0,
                objects_to_prune: 1,
                objects_to_recompress: 0,
                summary_line: "test".into(),
            },
        };

        let result = execute_plan(writ_dir, &plan, &[]).unwrap();
        assert_eq!(result.objects_pruned, 1);
        assert!(result.bytes_freed > 0);

        // Verify file was actually deleted
        let (prefix, rest) = orphan_hash.split_at(2);
        let obj_path = writ_dir.join("objects").join(prefix).join(rest);
        assert!(!obj_path.exists(), "orphan object should be deleted");
    }

    #[test]
    fn test_executor_does_not_prune_referenced() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();
        setup_gc_writ_dir(writ_dir);

        let tree_hash = "aa".to_string() + &"a".repeat(62);
        write_test_object(writ_dir, &tree_hash, b"tree data");

        let seal = make_test_seal(&tree_hash, vec![], None);
        write_test_seal(writ_dir, &seal);

        // Plan says to prune (from a stale plan), but the object is referenced
        let plan = GcPlan {
            generated_at: Utc::now(),
            storage: StorageReport::scan(writ_dir, 1_000_000).unwrap(),
            actions: vec![GcAction::PruneObjects {
                count: 1,
                total_bytes: 9,
                reason: "stale plan".into(),
            }],
            summary: GcSummary {
                total_actions: 1,
                transitions: 0,
                deletions: 0,
                events_to_clean: 0,
                objects_to_prune: 1,
                objects_to_recompress: 0,
                summary_line: "test".into(),
            },
        };

        let result = execute_plan(writ_dir, &plan, &[]).unwrap();
        // Re-verification should find no orphans → nothing pruned
        assert_eq!(result.objects_pruned, 0);
        assert_eq!(result.bytes_freed, 0);

        // Object should still exist
        let (prefix, rest) = tree_hash.split_at(2);
        assert!(writ_dir.join("objects").join(prefix).join(rest).exists());
    }

    #[test]
    fn test_executor_writes_tombstones_for_pruned() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();
        setup_gc_writ_dir(writ_dir);

        let orphan_hash = "ee".to_string() + &"e".repeat(62);
        write_test_object(writ_dir, &orphan_hash, b"orphan");

        let plan = GcPlan {
            generated_at: Utc::now(),
            storage: StorageReport::scan(writ_dir, 1_000_000).unwrap(),
            actions: vec![GcAction::PruneObjects {
                count: 1,
                total_bytes: 6,
                reason: "orphan cleanup".into(),
            }],
            summary: GcSummary {
                total_actions: 1,
                transitions: 0,
                deletions: 0,
                events_to_clean: 0,
                objects_to_prune: 1,
                objects_to_recompress: 0,
                summary_line: "test".into(),
            },
        };

        execute_plan(writ_dir, &plan, &[]).unwrap();

        let tombstones = read_tombstones(writ_dir).unwrap();
        assert_eq!(tombstones.len(), 1);
        assert_eq!(tombstones[0].id, orphan_hash);
        assert_eq!(tombstones[0].object_type, "object");
        assert_eq!(tombstones[0].final_state, "orphaned");
    }

    #[test]
    fn test_executor_reports_bytes_freed_in_audit() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();
        setup_gc_writ_dir(writ_dir);

        let orphan_hash = "ff".to_string() + &"f".repeat(62);
        let content = b"some orphan content here";
        write_test_object(writ_dir, &orphan_hash, content);

        let plan = GcPlan {
            generated_at: Utc::now(),
            storage: StorageReport::scan(writ_dir, 1_000_000).unwrap(),
            actions: vec![GcAction::PruneObjects {
                count: 1,
                total_bytes: content.len() as u64,
                reason: "test".into(),
            }],
            summary: GcSummary {
                total_actions: 1,
                transitions: 0,
                deletions: 0,
                events_to_clean: 0,
                objects_to_prune: 1,
                objects_to_recompress: 0,
                summary_line: "test".into(),
            },
        };

        let result = execute_plan(writ_dir, &plan, &[]).unwrap();
        assert_eq!(result.bytes_freed, content.len() as u64);
        assert_eq!(result.audit.space_freed_bytes, content.len() as u64);
    }

    #[test]
    fn test_staleness_safety_object_becomes_referenced() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();
        setup_gc_writ_dir(writ_dir);

        let obj_hash = "ab".to_string() + &"1".repeat(62);
        write_test_object(writ_dir, &obj_hash, b"data");

        // Generate plan — object is orphaned at this point
        let config = GcConfig::default();
        let plan = GcPlan::generate(writ_dir, &config, &[], &[]).unwrap();
        assert_eq!(plan.summary.objects_to_prune, 1);

        // Now create a seal referencing the object (simulating activity between plan and execute)
        let seal = make_test_seal(&obj_hash, vec![], None);
        write_test_seal(writ_dir, &seal);

        // Execute — should re-verify and NOT prune the now-referenced object
        let result = execute_plan(writ_dir, &plan, &[]).unwrap();
        assert_eq!(
            result.objects_pruned, 0,
            "re-verification should protect newly-referenced object"
        );

        // Object still on disk
        let (prefix, rest) = obj_hash.split_at(2);
        assert!(writ_dir.join("objects").join(prefix).join(rest).exists());
    }

    #[test]
    fn test_empty_prefix_dir_cleaned_after_prune() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();
        setup_gc_writ_dir(writ_dir);

        // Create orphan with unique prefix
        let orphan_hash = "zz".to_string() + &"9".repeat(62);
        write_test_object(writ_dir, &orphan_hash, b"lone orphan");

        let prefix_dir = writ_dir.join("objects").join("zz");
        assert!(prefix_dir.exists());

        let plan = GcPlan {
            generated_at: Utc::now(),
            storage: StorageReport::scan(writ_dir, 1_000_000).unwrap(),
            actions: vec![GcAction::PruneObjects {
                count: 1,
                total_bytes: 11,
                reason: "test".into(),
            }],
            summary: GcSummary {
                total_actions: 1,
                transitions: 0,
                deletions: 0,
                events_to_clean: 0,
                objects_to_prune: 1,
                objects_to_recompress: 0,
                summary_line: "test".into(),
            },
        };

        execute_plan(writ_dir, &plan, &[]).unwrap();

        // Prefix directory should be removed since it's now empty
        assert!(
            !prefix_dir.exists(),
            "empty prefix dir should be cleaned up"
        );
    }

    // --- Recompression tests ---

    /// Helper: write a legacy object (no magic byte, raw content on disk).
    fn write_legacy_object(writ_dir: &Path, hash: &str, content: &[u8]) {
        let (prefix, rest) = hash.split_at(2);
        let dir = writ_dir.join("objects").join(prefix);
        fs::create_dir_all(&dir).unwrap();
        // Legacy: write content directly, no magic byte
        fs::write(dir.join(rest), content).unwrap();
    }

    /// Helper: write a compressed object (MAGIC_ZSTD prefix).
    fn write_compressed_object(writ_dir: &Path, hash: &str, content: &[u8]) {
        let (prefix, rest) = hash.split_at(2);
        let dir = writ_dir.join("objects").join(prefix);
        fs::create_dir_all(&dir).unwrap();
        let compressed = crate::object::compress_object(content, 3);
        fs::write(dir.join(rest), compressed).unwrap();
    }

    /// Helper: write an explicit raw object (MAGIC_RAW = 0x00 prefix).
    fn write_explicit_raw_object(writ_dir: &Path, hash: &str, content: &[u8]) {
        let (prefix, rest) = hash.split_at(2);
        let dir = writ_dir.join("objects").join(prefix);
        fs::create_dir_all(&dir).unwrap();
        let mut data = Vec::with_capacity(1 + content.len());
        data.push(0x00); // MAGIC_RAW
        data.extend_from_slice(content);
        fs::write(dir.join(rest), data).unwrap();
    }

    #[test]
    fn test_find_uncompressed_detects_legacy() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();

        let hash = "aa".to_string() + &"a".repeat(62);
        write_legacy_object(writ_dir, &hash, b"legacy source code content here");

        let uncompressed = find_uncompressed_objects(writ_dir).unwrap();
        assert_eq!(uncompressed.len(), 1);
        assert_eq!(uncompressed[0].hash, hash);
        assert!(uncompressed[0].is_legacy);
    }

    #[test]
    fn test_find_uncompressed_skips_compressed() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();

        let hash = "bb".to_string() + &"b".repeat(62);
        write_compressed_object(writ_dir, &hash, b"already compressed content");

        let uncompressed = find_uncompressed_objects(writ_dir).unwrap();
        assert!(
            uncompressed.is_empty(),
            "compressed objects should be skipped"
        );
    }

    #[test]
    fn test_find_uncompressed_skips_explicit_raw() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();

        let hash = "cc".to_string() + &"c".repeat(62);
        write_explicit_raw_object(writ_dir, &hash, b"already tried, not worth compressing");

        let uncompressed = find_uncompressed_objects(writ_dir).unwrap();
        assert!(
            uncompressed.is_empty(),
            "MAGIC_RAW objects should be skipped (already evaluated)"
        );
    }

    #[test]
    fn test_recompression_rewrites_legacy() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();
        setup_gc_writ_dir(writ_dir);

        // Create a legacy object with repetitive content (compresses well)
        let hash = "dd".to_string() + &"d".repeat(62);
        let content = "fn main() { println!(\"hello world\"); }\n".repeat(100);
        write_legacy_object(writ_dir, &hash, content.as_bytes());

        let plan = GcPlan {
            generated_at: Utc::now(),
            storage: StorageReport::scan(writ_dir, 1_000_000).unwrap(),
            actions: vec![GcAction::RecompressObjects {
                count: 1,
                estimated_savings_bytes: 1000,
                reason: "test".into(),
            }],
            summary: GcSummary {
                total_actions: 1,
                transitions: 0,
                deletions: 0,
                events_to_clean: 0,
                objects_to_prune: 0,
                objects_to_recompress: 1,
                summary_line: "test".into(),
            },
        };

        let result = execute_plan(writ_dir, &plan, &[]).unwrap();
        assert_eq!(result.objects_recompressed, 1);
        assert!(result.recompression_savings > 0);

        // Verify on-disk file now has MAGIC_ZSTD prefix
        let (prefix, rest) = hash.split_at(2);
        let on_disk = fs::read(writ_dir.join("objects").join(prefix).join(rest)).unwrap();
        assert_eq!(
            on_disk[0], 0x01,
            "should have MAGIC_ZSTD prefix after recompression"
        );
    }

    #[test]
    fn test_incompressible_gets_raw_prefix() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path();
        setup_gc_writ_dir(writ_dir);

        // Create a legacy object with random-ish incompressible content.
        // Use content that starts with a byte that isn't 0x00 or 0x01 (so it's detected as legacy).
        let hash = "ee".to_string() + &"e".repeat(62);
        // Random bytes are generally incompressible
        let content: Vec<u8> = (0..256u16).map(|i| (i % 251) as u8 + 2).collect();
        write_legacy_object(writ_dir, &hash, &content);

        let plan = GcPlan {
            generated_at: Utc::now(),
            storage: StorageReport::scan(writ_dir, 1_000_000).unwrap(),
            actions: vec![GcAction::RecompressObjects {
                count: 1,
                estimated_savings_bytes: 0,
                reason: "test".into(),
            }],
            summary: GcSummary {
                total_actions: 1,
                transitions: 0,
                deletions: 0,
                events_to_clean: 0,
                objects_to_prune: 0,
                objects_to_recompress: 1,
                summary_line: "test".into(),
            },
        };

        let result = execute_plan(writ_dir, &plan, &[]).unwrap();
        // Incompressible: should not count as recompressed
        assert_eq!(result.objects_recompressed, 0);

        // Verify on-disk file now has MAGIC_RAW prefix (won't retry)
        let (prefix, rest) = hash.split_at(2);
        let on_disk = fs::read(writ_dir.join("objects").join(prefix).join(rest)).unwrap();
        assert_eq!(
            on_disk[0], 0x00,
            "incompressible object should get MAGIC_RAW prefix"
        );
    }
}
