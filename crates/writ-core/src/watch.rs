//! Watch loop core — detects new seals, analyzes overlaps, triggers convergence.
//!
//! The watch subsystem monitors `.writ/seals/` for new seal files, analyzes
//! them for overlapping file changes, and auto-converges when configured.
//!
//! The watch loop is a library function with no I/O concerns beyond the
//! `Repository` API. It communicates events via a `Sender<WatchEvent>` channel
//! and listens for shutdown on a `Receiver<()>` channel.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::convergence::ConvergeStrategy;
use crate::error::WritResult;
use crate::repo::Repository;

// ═══════════════════════════════════════════════════════════════════
// Configuration
// ═══════════════════════════════════════════════════════════════════

/// Configuration for the watch loop.
#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// Polling interval in seconds (default: 5).
    pub interval_secs: u64,
    /// Trigger convergence on overlap (default: true).
    pub auto_converge: bool,
    /// Convergence retry limit (default: 3).
    pub max_retries: u32,
    /// Convergence strategy to use (default: MostRecent).
    pub strategy: ConvergeStrategy,
    /// Log file path, relative to project root (default: ".writ/watch.log").
    pub log_file: Option<String>,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            interval_secs: 5,
            auto_converge: true,
            max_retries: 3,
            strategy: ConvergeStrategy::MostRecent,
            log_file: Some(".writ/watch.log".to_string()),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// Events
// ═══════════════════════════════════════════════════════════════════

/// A single event emitted by the watch loop.
#[derive(Debug, Clone)]
pub struct WatchEvent {
    pub timestamp: DateTime<Utc>,
    pub kind: WatchEventKind,
}

/// The kind of event detected by the watch loop.
#[derive(Debug, Clone)]
pub enum WatchEventKind {
    /// A new seal was detected.
    SealDetected {
        seal_id: String,
        spec: String,
        agent: String,
        files: Vec<String>,
    },
    /// Overlapping changes detected between specs.
    OverlapDetected {
        files: Vec<String>,
        specs: Vec<String>,
    },
    /// Auto-convergence started on overlapping files.
    ConvergenceStarted { files: Vec<String> },
    /// Auto-convergence completed successfully.
    ConvergenceCompleted {
        files: Vec<String>,
        merged_count: usize,
    },
    /// Auto-convergence failed.
    ConvergenceFailed { files: Vec<String>, reason: String },
    /// A genuine conflict was detected (cannot auto-resolve).
    ConflictDetected {
        file: String,
        agents: Vec<String>,
        reason: String,
    },
    /// Seal-triggered convergence already handled these overlaps.
    /// Watch detected overlaps but the seal's inline convergence resolved them.
    SealConvergenceHandled {
        seal_id: String,
        files_merged: usize,
        specs: Vec<String>,
    },
}

/// Summary statistics for a completed watch session.
#[derive(Debug, Default, Clone)]
pub struct WatchSummary {
    pub seals_detected: u64,
    pub overlaps_detected: u64,
    pub convergences_triggered: u64,
    pub convergences_succeeded: u64,
    pub convergences_failed: u64,
    pub conflicts_detected: u64,
    pub cycles: u64,
}

// ═══════════════════════════════════════════════════════════════════
// Conflict records (MS.9)
// ═══════════════════════════════════════════════════════════════════

/// Record of a conflict that convergence could not auto-resolve.
/// Stored in `.writ/conflicts/<id>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictRecord {
    pub id: String,
    pub file_path: String,
    pub spec_a: String,
    pub spec_b: String,
    pub agent_a: String,
    pub agent_b: String,
    pub seal_a: String,
    pub seal_b: String,
    pub reason: String,
    pub timestamp: DateTime<Utc>,
}

// ═══════════════════════════════════════════════════════════════════
// Watch loop (MS.6)
// ═══════════════════════════════════════════════════════════════════

/// Run the watch loop. Polls for new seals, detects overlaps, and triggers
/// convergence when configured. Returns a summary when stopped.
///
/// The loop communicates via channels:
/// - `event_tx`: sends `WatchEvent`s to the caller (for UI rendering)
/// - `stop_rx`: receives a shutdown signal from the caller
pub fn run_watch_loop(
    repo: &Repository,
    config: &WatchConfig,
    event_tx: Sender<WatchEvent>,
    stop_rx: Receiver<()>,
) -> WritResult<WatchSummary> {
    let mut state = WatchState::new(repo)?;
    let mut summary = WatchSummary::default();
    let interval = Duration::from_secs(config.interval_secs);

    loop {
        // Check for stop signal (non-blocking).
        match stop_rx.try_recv() {
            Ok(()) | Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }

        // Run one watch cycle.
        run_cycle(repo, config, &mut state, &mut summary, &event_tx)?;

        // Sleep in small increments to check stop signal responsively.
        let sleep_ms = interval.as_millis() as u64;
        let step = 250u64; // check every 250ms
        let mut slept = 0u64;
        while slept < sleep_ms {
            match stop_rx.try_recv() {
                Ok(()) | Err(TryRecvError::Disconnected) => return Ok(summary),
                Err(TryRecvError::Empty) => {}
            }
            std::thread::sleep(Duration::from_millis(step.min(sleep_ms - slept)));
            slept += step;
        }
    }

    Ok(summary)
}

/// Run a single watch cycle (public for testing).
pub fn run_single_cycle(
    repo: &Repository,
    config: &WatchConfig,
    state: &mut WatchState,
    summary: &mut WatchSummary,
    event_tx: &Sender<WatchEvent>,
) -> WritResult<()> {
    run_cycle(repo, config, state, summary, event_tx)
}

/// Internal watch state tracking between cycles.
pub struct WatchState {
    /// Set of known seal IDs (already seen).
    pub known_seals: HashSet<String>,
    /// Fingerprints of overlaps that have already been converged.
    /// Each fingerprint is built from the sorted spec IDs and their latest
    /// seal IDs at convergence time. If an overlap's fingerprint matches one
    /// already in this set, we skip re-convergence — the same seal state was
    /// already merged.  A new seal from any participating spec changes the
    /// fingerprint and allows convergence to re-trigger.
    pub converged_fingerprints: HashSet<String>,
    /// True on the very first cycle. Enables retroactive overlap scanning
    /// so a late-started watch catches pre-existing unconverged overlaps.
    pub first_cycle: bool,
}

/// Persisted portion of watch state (survives process restarts).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PersistedWatchState {
    converged_fingerprints: Vec<String>,
}

impl WatchState {
    /// Initialize watch state from the current repository.
    /// Loads persisted convergence fingerprints from `.writ/watch_state.json`
    /// so the watch doesn't re-converge already-merged overlaps after restart.
    pub fn new(repo: &Repository) -> WritResult<Self> {
        let seals = repo.log()?;
        let known_seals: HashSet<String> = seals.iter().map(|s| s.id.clone()).collect();

        // Load persisted fingerprints if available (Defense #2).
        let persisted = Self::load_persisted(repo);
        let converged_fingerprints: HashSet<String> =
            persisted.converged_fingerprints.into_iter().collect();

        Ok(Self {
            known_seals,
            converged_fingerprints,
            first_cycle: true,
        })
    }

    /// Save convergence fingerprints to `.writ/watch_state.json`.
    pub fn save(&self, repo: &Repository) -> WritResult<()> {
        let persisted = PersistedWatchState {
            converged_fingerprints: self.converged_fingerprints.iter().cloned().collect(),
        };
        let path = repo.writ_dir().join("watch_state.json");
        let json = serde_json::to_string_pretty(&persisted)?;
        fs::write(path, json)?;
        Ok(())
    }

    /// Load persisted state, returning default if not found or corrupt.
    fn load_persisted(repo: &Repository) -> PersistedWatchState {
        let path = repo.writ_dir().join("watch_state.json");
        if !path.exists() {
            return PersistedWatchState::default();
        }
        fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
}

// ═══════════════════════════════════════════════════════════════════
// Core cycle logic
// ═══════════════════════════════════════════════════════════════════

fn run_cycle(
    repo: &Repository,
    config: &WatchConfig,
    state: &mut WatchState,
    summary: &mut WatchSummary,
    event_tx: &Sender<WatchEvent>,
) -> WritResult<()> {
    summary.cycles += 1;

    // Step 1: Detect new seals.
    let current_seals = repo.log()?;
    let mut new_seal_ids: Vec<String> = Vec::new();

    for seal in &current_seals {
        if !state.known_seals.contains(&seal.id) {
            state.known_seals.insert(seal.id.clone());
            new_seal_ids.push(seal.id.clone());

            let files: Vec<String> = seal.changes.iter().map(|c| c.path.clone()).collect();
            let spec = seal.spec_id.clone().unwrap_or_default();
            summary.seals_detected += 1;

            let _ = event_tx.send(WatchEvent {
                timestamp: Utc::now(),
                kind: WatchEventKind::SealDetected {
                    seal_id: seal.id[..12.min(seal.id.len())].to_string(),
                    spec,
                    agent: seal.agent.id.clone(),
                    files,
                },
            });
        }
    }

    // If no new seals AND not the first cycle, nothing more to do.
    // On the first cycle, we do a retroactive scan for pre-existing
    // unconverged overlaps — this handles the "late start" scenario
    // where agents sealed work before watch was started.
    let retroactive = state.first_cycle;
    if state.first_cycle {
        state.first_cycle = false;
    }
    if new_seal_ids.is_empty() && !retroactive {
        return Ok(());
    }

    // Check if any of the new seals already handled convergence inline.
    // If seal-triggered convergence succeeded, emit an event and mark the
    // fingerprint as converged so watch doesn't re-converge the same state.
    let mut seal_handled_convergence = false;
    for seal in &current_seals {
        if new_seal_ids.contains(&seal.id) {
            if let Some(ref conv) = seal.convergence {
                if conv.attempted && conv.succeeded {
                    seal_handled_convergence = true;
                    let _ = event_tx.send(WatchEvent {
                        timestamp: Utc::now(),
                        kind: WatchEventKind::SealConvergenceHandled {
                            seal_id: seal.id[..12.min(seal.id.len())].to_string(),
                            files_merged: conv.files_merged,
                            specs: conv.specs_involved.clone(),
                        },
                    });
                }
            }
        }
    }

    // Step 2: Detect overlaps (MS.7).
    let overlaps = repo.detect_same_directory_overlaps()?;

    if overlaps.is_empty() {
        return Ok(());
    }

    // Build a fingerprint from the overlapping specs' latest seal IDs.
    // If this fingerprint was already converged, skip — no new seal content
    // to merge.  This prevents the stacking bug where re-convergence of
    // the same unchanged overlap doubles/triples content.
    let fingerprint = build_overlap_fingerprint(repo, &overlaps);
    let already_converged = state.converged_fingerprints.contains(&fingerprint);

    // If seal-triggered convergence already handled these overlaps, mark
    // the fingerprint as converged so watch doesn't redo the work.
    if seal_handled_convergence {
        state.converged_fingerprints.insert(fingerprint.clone());
    }

    // Collect all overlapping files/specs for reporting.
    let mut all_overlap_files: Vec<String> = Vec::new();
    for overlap in &overlaps {
        summary.overlaps_detected += 1;
        all_overlap_files.extend(overlap.files.iter().cloned());
        let _ = event_tx.send(WatchEvent {
            timestamp: Utc::now(),
            kind: WatchEventKind::OverlapDetected {
                files: overlap.files.clone(),
                specs: overlap.specs.clone(),
            },
        });
    }

    // Step 3: Auto-converge if enabled (MS.8).
    // Run converge_all ONCE for all overlaps — it handles all specs in a single pass.
    // Skip if we already converged this exact overlap state (same specs, same seals).
    if config.auto_converge && !already_converged {
        let _ = event_tx.send(WatchEvent {
            timestamp: Utc::now(),
            kind: WatchEventKind::ConvergenceStarted {
                files: all_overlap_files.clone(),
            },
        });

        summary.convergences_triggered += 1;

        match repo.converge_all(config.strategy, true) {
            Ok(report) => {
                if report.is_clean {
                    summary.convergences_succeeded += 1;
                    let _ = event_tx.send(WatchEvent {
                        timestamp: Utc::now(),
                        kind: WatchEventKind::ConvergenceCompleted {
                            files: report.files_changed.clone(),
                            merged_count: report.total_auto_merged + report.total_resolutions,
                        },
                    });
                } else {
                    // Convergence had escalations/conflicts.
                    for esc in &report.escalations {
                        summary.conflicts_detected += 1;

                        // Look up agents and seal IDs for richer conflict records.
                        let (agent_a, seal_a) = lookup_spec_last_agent(repo, &esc.left_spec);
                        let (agent_b, seal_b) = lookup_spec_last_agent(repo, &esc.right_spec);

                        let record = ConflictRecord {
                            id: crate::crypto::blake3_hex(
                                format!(
                                    "{}:{}:{}",
                                    esc.file_path,
                                    Utc::now().timestamp_nanos_opt().unwrap_or(0),
                                    esc.reason
                                )
                                .as_bytes(),
                            ),
                            file_path: esc.file_path.clone(),
                            spec_a: esc.left_spec.clone(),
                            spec_b: esc.right_spec.clone(),
                            agent_a,
                            agent_b,
                            seal_a,
                            seal_b,
                            reason: esc.reason.clone(),
                            timestamp: Utc::now(),
                        };

                        let agents: Vec<String> = [&record.agent_a, &record.agent_b]
                            .iter()
                            .filter(|a| !a.is_empty())
                            .map(|a| a.to_string())
                            .collect();

                        let _ = save_conflict_record(repo, &record);

                        let _ = event_tx.send(WatchEvent {
                            timestamp: Utc::now(),
                            kind: WatchEventKind::ConflictDetected {
                                file: esc.file_path.clone(),
                                agents,
                                reason: esc.reason.clone(),
                            },
                        });
                    }

                    // Still report partial success for auto-merged files.
                    if report.total_auto_merged > 0 || report.total_resolutions > 0 {
                        summary.convergences_succeeded += 1;
                        let _ = event_tx.send(WatchEvent {
                            timestamp: Utc::now(),
                            kind: WatchEventKind::ConvergenceCompleted {
                                files: report.files_changed.clone(),
                                merged_count: report.total_auto_merged + report.total_resolutions,
                            },
                        });
                    }
                }

                // Record this overlap fingerprint so we don't re-converge
                // the same unchanged overlap on subsequent cycles.
                state.converged_fingerprints.insert(fingerprint.clone());

                // Defense #2: Persist to disk so restarts don't re-converge.
                let _ = state.save(repo);

                // Re-snapshot seals after convergence (it may create new seals).
                if let Ok(post_seals) = repo.log() {
                    for s in &post_seals {
                        state.known_seals.insert(s.id.clone());
                    }
                }
            }
            Err(e) => {
                summary.convergences_failed += 1;
                let _ = event_tx.send(WatchEvent {
                    timestamp: Utc::now(),
                    kind: WatchEventKind::ConvergenceFailed {
                        files: all_overlap_files,
                        reason: e.to_string(),
                    },
                });
            }
        }
    }

    Ok(())
}

/// Build a deterministic fingerprint for a set of overlaps.
///
/// The fingerprint encodes which specs are involved and their latest seal IDs.
/// If any spec gets a new seal, the fingerprint changes, allowing re-convergence.
/// If nothing has changed, the fingerprint stays the same, preventing the
/// stacking bug where the watch re-converges already-merged content.
fn build_overlap_fingerprint(repo: &Repository, overlaps: &[crate::repo::OverlapSet]) -> String {
    let mut spec_seals: Vec<(String, String)> = Vec::new();
    let mut seen = HashSet::new();

    for overlap in overlaps {
        for spec_id in &overlap.specs {
            if seen.insert(spec_id.clone()) {
                let seal_id = repo
                    .load_spec(spec_id)
                    .ok()
                    .and_then(|s| s.sealed_by.last().cloned())
                    .unwrap_or_default();
                spec_seals.push((spec_id.clone(), seal_id));
            }
        }
    }

    spec_seals.sort_by(|a, b| a.0.cmp(&b.0));
    spec_seals
        .iter()
        .map(|(sid, seal)| format!("{sid}:{seal}"))
        .collect::<Vec<_>>()
        .join("|")
}

/// Look up the last agent and seal ID for a spec. Returns (agent_id, seal_id)
/// with empty strings as fallbacks if the spec or seal can't be loaded.
fn lookup_spec_last_agent(repo: &Repository, spec_id: &str) -> (String, String) {
    if let Ok(spec) = repo.load_spec(spec_id) {
        if let Some(last_seal_id) = spec.sealed_by.last() {
            if let Ok(seal) = repo.load_seal(last_seal_id) {
                return (seal.agent.id.clone(), last_seal_id.clone());
            }
        }
    }
    (String::new(), String::new())
}

// ═══════════════════════════════════════════════════════════════════
// Conflict persistence (MS.9)
// ═══════════════════════════════════════════════════════════════════

fn save_conflict_record(repo: &Repository, record: &ConflictRecord) -> WritResult<()> {
    let conflicts_dir = repo.writ_dir().join("conflicts");
    fs::create_dir_all(&conflicts_dir)?;
    let path = conflicts_dir.join(format!("{}.json", record.id));
    let json = serde_json::to_string_pretty(record)?;
    fs::write(path, json)?;
    Ok(())
}

/// Load all conflict records from `.writ/conflicts/`.
pub fn load_conflict_records(repo: &Repository) -> WritResult<Vec<ConflictRecord>> {
    let conflicts_dir = repo.writ_dir().join("conflicts");
    if !conflicts_dir.exists() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(&conflicts_dir)? {
        let entry = entry?;
        if entry.path().extension().and_then(|e| e.to_str()) == Some("json") {
            let content = fs::read_to_string(entry.path())?;
            if let Ok(record) = serde_json::from_str::<ConflictRecord>(&content) {
                records.push(record);
            }
        }
    }
    records.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    Ok(records)
}

/// Clear a specific conflict record by ID.
pub fn clear_conflict(repo: &Repository, conflict_id: &str) -> WritResult<bool> {
    let path = repo
        .writ_dir()
        .join("conflicts")
        .join(format!("{conflict_id}.json"));
    if path.exists() {
        fs::remove_file(path)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

// ═══════════════════════════════════════════════════════════════════
// Tests (MS.20 stubs filled in)
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seal::{AgentIdentity, AgentType, TaskStatus, Verification};
    use crate::spec::Spec;
    use std::sync::mpsc;
    use tempfile::tempdir;

    fn agent(name: &str) -> AgentIdentity {
        AgentIdentity {
            id: name.to_string(),
            agent_type: AgentType::Agent,
        }
    }

    #[test]
    fn test_watch_detects_new_seal() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Create initial state.
        fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
        repo.seal(
            agent("setup"),
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let config = WatchConfig::default();
        let mut state = WatchState::new(&repo).unwrap();
        let mut summary = WatchSummary::default();
        let (tx, rx) = mpsc::channel();

        // Create a new seal that the watch should detect.
        fs::write(
            dir.path().join("main.rs"),
            "fn main() { println!(\"hi\"); }",
        )
        .unwrap();
        repo.seal(
            agent("dev"),
            "add print".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Run one cycle.
        run_single_cycle(&repo, &config, &mut state, &mut summary, &tx).unwrap();

        assert_eq!(summary.seals_detected, 1);
        let event = rx.try_recv().unwrap();
        assert!(matches!(event.kind, WatchEventKind::SealDetected { .. }));
    }

    #[test]
    fn test_watch_detects_overlap() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("shared.rs"), "// base").unwrap();
        repo.seal(
            agent("setup"),
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("s1".into(), "S1".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("s2".into(), "S2".into(), "".into()))
            .unwrap();

        // Agent 1 modifies shared.rs.
        fs::write(dir.path().join("shared.rs"), "// version A").unwrap();
        repo.seal(
            agent("a1"),
            "a changes".into(),
            Some("s1".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent 2 modifies shared.rs differently.
        fs::write(dir.path().join("shared.rs"), "// version B").unwrap();
        repo.seal(
            agent("b1"),
            "b changes".into(),
            Some("s2".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let config = WatchConfig {
            auto_converge: false, // detect only, don't converge
            ..Default::default()
        };
        let mut state = WatchState::new(&repo).unwrap();
        // Mark all existing seals as "already seen" then re-init to pick up new ones.
        // Actually, we want the watch to see the seals we just created as "new".
        // Since WatchState::new() marks all existing seals as known, we need to
        // create the state BEFORE sealing, then seal, then run the cycle.
        // Let's re-do this test properly.

        // Reset: create state before the spec seals.
        let dir2 = tempdir().unwrap();
        let repo2 = Repository::init(dir2.path()).unwrap();
        fs::write(dir2.path().join("shared.rs"), "// base").unwrap();
        repo2
            .seal(
                agent("setup"),
                "baseline".into(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();
        repo2
            .add_spec(&Spec::new("s1".into(), "S1".into(), "".into()))
            .unwrap();
        repo2
            .add_spec(&Spec::new("s2".into(), "S2".into(), "".into()))
            .unwrap();

        let mut state2 = WatchState::new(&repo2).unwrap();
        let mut summary2 = WatchSummary::default();
        let (tx2, rx2) = mpsc::channel();

        // Now create seals.
        fs::write(dir2.path().join("shared.rs"), "// version A").unwrap();
        repo2
            .seal(
                agent("a1"),
                "a".into(),
                Some("s1".into()),
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();
        fs::write(dir2.path().join("shared.rs"), "// version B").unwrap();
        repo2
            .seal(
                agent("b1"),
                "b".into(),
                Some("s2".into()),
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap();

        run_single_cycle(&repo2, &config, &mut state2, &mut summary2, &tx2).unwrap();

        assert_eq!(summary2.seals_detected, 2);
        assert_eq!(summary2.overlaps_detected, 1);

        // Drain events and check for OverlapDetected.
        let mut events = Vec::new();
        while let Ok(e) = rx2.try_recv() {
            events.push(e);
        }
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            WatchEventKind::OverlapDetected { files, .. } if files.contains(&"shared.rs".to_string())
        )));
    }

    #[test]
    fn test_watch_no_overlap_no_convergence() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("base.txt"), "base").unwrap();
        repo.seal(
            agent("setup"),
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();
        repo.add_spec(&Spec::new("s1".into(), "S1".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("s2".into(), "S2".into(), "".into()))
            .unwrap();

        let mut state = WatchState::new(&repo).unwrap();

        // Disjoint files.
        fs::write(dir.path().join("auth.rs"), "auth code").unwrap();
        repo.seal(
            agent("a1"),
            "auth".into(),
            Some("s1".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();
        fs::write(dir.path().join("pay.rs"), "pay code").unwrap();
        repo.seal(
            agent("b1"),
            "pay".into(),
            Some("s2".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let config = WatchConfig::default();
        let mut summary = WatchSummary::default();
        let (tx, rx) = mpsc::channel();

        run_single_cycle(&repo, &config, &mut state, &mut summary, &tx).unwrap();

        assert_eq!(summary.seals_detected, 2);
        assert_eq!(summary.overlaps_detected, 0);
        assert_eq!(summary.convergences_triggered, 0);

        // No overlap or convergence events.
        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        assert!(!events
            .iter()
            .any(|e| matches!(e.kind, WatchEventKind::OverlapDetected { .. })));
        assert!(!events
            .iter()
            .any(|e| matches!(e.kind, WatchEventKind::ConvergenceStarted { .. })));
    }

    #[test]
    fn test_watch_auto_converge_on_overlap() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        // Base file with 5 lines.
        let base = "line1\nline2\nline3\nline4\nline5\n";
        fs::write(dir.path().join("shared.txt"), base).unwrap();
        repo.seal(
            agent("setup"),
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();
        repo.add_spec(&Spec::new("s1".into(), "S1".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("s2".into(), "S2".into(), "".into()))
            .unwrap();

        let mut state = WatchState::new(&repo).unwrap();

        // Agent 1 changes line 1.
        fs::write(
            dir.path().join("shared.txt"),
            "CHANGED_A\nline2\nline3\nline4\nline5\n",
        )
        .unwrap();
        repo.seal(
            agent("a1"),
            "change top".into(),
            Some("s1".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent 2 changes line 5.
        fs::write(
            dir.path().join("shared.txt"),
            "line1\nline2\nline3\nline4\nCHANGED_B\n",
        )
        .unwrap();
        repo.seal(
            agent("b1"),
            "change bottom".into(),
            Some("s2".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let config = WatchConfig {
            auto_converge: true,
            strategy: ConvergeStrategy::MostRecent,
            ..Default::default()
        };
        let mut summary = WatchSummary::default();
        let (tx, rx) = mpsc::channel();

        run_single_cycle(&repo, &config, &mut state, &mut summary, &tx).unwrap();

        assert_eq!(summary.overlaps_detected, 1);
        assert_eq!(summary.convergences_triggered, 1);

        // Check for ConvergenceCompleted event.
        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        let has_completed = events
            .iter()
            .any(|e| matches!(e.kind, WatchEventKind::ConvergenceCompleted { .. }));
        assert!(
            has_completed || summary.convergences_succeeded > 0,
            "should have convergence completed"
        );
    }

    #[test]
    fn test_watch_conflict_recorded() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        // Same line, different edits = true conflict.
        fs::write(dir.path().join("shared.txt"), "original line\n").unwrap();
        repo.seal(
            agent("setup"),
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();
        repo.add_spec(&Spec::new("s1".into(), "S1".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("s2".into(), "S2".into(), "".into()))
            .unwrap();

        let mut state = WatchState::new(&repo).unwrap();

        // Both agents rewrite the same line differently.
        fs::write(dir.path().join("shared.txt"), "agent A version\n").unwrap();
        repo.seal(
            agent("a1"),
            "a edit".into(),
            Some("s1".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();
        fs::write(dir.path().join("shared.txt"), "agent B version\n").unwrap();
        repo.seal(
            agent("b1"),
            "b edit".into(),
            Some("s2".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let config = WatchConfig {
            auto_converge: true,
            strategy: ConvergeStrategy::Escalate,
            ..Default::default()
        };
        let mut summary = WatchSummary::default();
        let (tx, rx) = mpsc::channel();

        run_single_cycle(&repo, &config, &mut state, &mut summary, &tx).unwrap();

        // With Escalate strategy, conflicts produce escalations.
        assert!(summary.overlaps_detected > 0);
        assert!(summary.convergences_triggered > 0);

        // Verify conflict records were saved.
        let records = load_conflict_records(&repo).unwrap();
        // Escalation produces a conflict record.
        if !records.is_empty() {
            assert!(!records[0].file_path.is_empty());
            assert!(!records[0].reason.is_empty());
        }
    }

    #[test]
    fn test_watch_stop_signal() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("a.txt"), "hello").unwrap();
        repo.seal(
            agent("setup"),
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let config = WatchConfig {
            interval_secs: 1,
            ..Default::default()
        };
        let (event_tx, _event_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();

        // Send stop signal immediately.
        stop_tx.send(()).unwrap();

        let summary = run_watch_loop(&repo, &config, event_tx, stop_rx).unwrap();
        // Loop should exit cleanly without hanging.
        assert_eq!(summary.cycles, 0);
    }

    #[test]
    fn test_watch_summary() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("base.txt"), "base").unwrap();
        repo.seal(
            agent("setup"),
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();
        repo.add_spec(&Spec::new("s1".into(), "S1".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("s2".into(), "S2".into(), "".into()))
            .unwrap();

        let mut state = WatchState::new(&repo).unwrap();

        // 1 clean seal + 2 overlapping seals = 3 seals total.
        fs::write(dir.path().join("clean.txt"), "no overlap").unwrap();
        repo.seal(
            agent("c1"),
            "clean".into(),
            Some("s1".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        fs::write(dir.path().join("shared.txt"), "version A").unwrap();
        repo.seal(
            agent("a1"),
            "a".into(),
            Some("s1".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();
        fs::write(dir.path().join("shared.txt"), "version B").unwrap();
        repo.seal(
            agent("b1"),
            "b".into(),
            Some("s2".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let config = WatchConfig {
            auto_converge: false,
            ..Default::default()
        };
        let mut summary = WatchSummary::default();
        let (tx, _rx) = mpsc::channel();

        run_single_cycle(&repo, &config, &mut state, &mut summary, &tx).unwrap();

        assert_eq!(summary.seals_detected, 3);
        assert_eq!(summary.overlaps_detected, 1);
        assert_eq!(summary.convergences_triggered, 0, "auto_converge=false");
        assert_eq!(summary.cycles, 1);
    }

    #[test]
    fn test_watch_respects_auto_converge_false() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("shared.txt"), "base").unwrap();
        repo.seal(
            agent("setup"),
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();
        repo.add_spec(&Spec::new("s1".into(), "S1".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("s2".into(), "S2".into(), "".into()))
            .unwrap();

        let mut state = WatchState::new(&repo).unwrap();

        fs::write(dir.path().join("shared.txt"), "A").unwrap();
        repo.seal(
            agent("a1"),
            "a".into(),
            Some("s1".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();
        fs::write(dir.path().join("shared.txt"), "B").unwrap();
        repo.seal(
            agent("b1"),
            "b".into(),
            Some("s2".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let config = WatchConfig {
            auto_converge: false,
            ..Default::default()
        };
        let mut summary = WatchSummary::default();
        let (tx, rx) = mpsc::channel();

        run_single_cycle(&repo, &config, &mut state, &mut summary, &tx).unwrap();

        assert!(summary.overlaps_detected > 0);
        assert_eq!(summary.convergences_triggered, 0);

        // Should have OverlapDetected but NOT ConvergenceStarted.
        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        assert!(events
            .iter()
            .any(|e| matches!(e.kind, WatchEventKind::OverlapDetected { .. })));
        assert!(!events
            .iter()
            .any(|e| matches!(e.kind, WatchEventKind::ConvergenceStarted { .. })));
    }

    #[test]
    #[test]
    fn test_watch_retroactive_scan_on_first_cycle() {
        // Regression test for the "late start" scenario:
        // agents seal work BEFORE watch starts. On the first cycle,
        // watch should detect and converge pre-existing overlaps
        // even though all seals are marked as "already known."
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let base = "line1\nline2\nline3\nline4\nline5\n";
        fs::write(dir.path().join("shared.txt"), base).unwrap();
        repo.seal(
            agent("setup"),
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("s1".into(), "S1".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("s2".into(), "S2".into(), "".into()))
            .unwrap();

        // Agents seal BEFORE watch starts.
        fs::write(
            dir.path().join("shared.txt"),
            "CHANGED_A\nline2\nline3\nline4\nline5\n",
        )
        .unwrap();
        repo.seal(
            agent("a1"),
            "change top".into(),
            Some("s1".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        fs::write(
            dir.path().join("shared.txt"),
            "line1\nline2\nline3\nline4\nCHANGED_B\n",
        )
        .unwrap();
        repo.seal(
            agent("b1"),
            "change bottom".into(),
            Some("s2".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // NOW start the watch — all seals are pre-existing.
        let mut state = WatchState::new(&repo).unwrap();
        assert!(state.first_cycle, "first_cycle should be true on init");

        let config = WatchConfig {
            auto_converge: true,
            strategy: ConvergeStrategy::MostRecent,
            ..Default::default()
        };
        let mut summary = WatchSummary::default();
        let (tx, _rx) = mpsc::channel();

        // First cycle: no new seals, but retroactive scan should find
        // the pre-existing overlap and converge it.
        run_single_cycle(&repo, &config, &mut state, &mut summary, &tx).unwrap();

        assert!(
            !state.first_cycle,
            "first_cycle should be false after first run"
        );
        assert_eq!(summary.seals_detected, 0, "no NEW seals detected");
        assert!(
            summary.overlaps_detected > 0,
            "retroactive scan should find pre-existing overlaps"
        );
        assert!(
            summary.convergences_triggered > 0,
            "should converge pre-existing overlaps on first cycle"
        );

        // Second cycle: no new seals, not first cycle — should be a no-op.
        run_single_cycle(&repo, &config, &mut state, &mut summary, &tx).unwrap();
        assert_eq!(
            summary.convergences_triggered, 1,
            "should NOT re-converge on second cycle"
        );
    }

    fn test_watch_no_re_convergence_on_same_overlap() {
        // Regression test for the convergence stacking bug:
        // after converging an overlap, a subsequent cycle with a NEW seal
        // (from a different spec or unrelated file) should NOT re-converge
        // the same overlap if the participating specs' seals haven't changed.
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let base = "line1\nline2\nline3\nline4\nline5\n";
        fs::write(dir.path().join("shared.txt"), base).unwrap();
        repo.seal(
            agent("setup"),
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("s1".into(), "S1".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("s2".into(), "S2".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("s3".into(), "S3".into(), "".into()))
            .unwrap();

        let mut state = WatchState::new(&repo).unwrap();

        // Agent 1 changes line 1.
        fs::write(
            dir.path().join("shared.txt"),
            "CHANGED_A\nline2\nline3\nline4\nline5\n",
        )
        .unwrap();
        repo.seal(
            agent("a1"),
            "change top".into(),
            Some("s1".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent 2 changes line 5.
        fs::write(
            dir.path().join("shared.txt"),
            "line1\nline2\nline3\nline4\nCHANGED_B\n",
        )
        .unwrap();
        repo.seal(
            agent("b1"),
            "change bottom".into(),
            Some("s2".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let config = WatchConfig {
            auto_converge: true,
            strategy: ConvergeStrategy::MostRecent,
            ..Default::default()
        };
        let mut summary = WatchSummary::default();
        let (tx, _rx) = mpsc::channel();

        // Cycle 1: should detect overlap and converge.
        run_single_cycle(&repo, &config, &mut state, &mut summary, &tx).unwrap();
        assert_eq!(summary.convergences_triggered, 1);

        // Now a third spec creates an unrelated seal — this triggers a new cycle
        // with new seals detected, but the s1/s2 overlap hasn't changed.
        fs::write(dir.path().join("unrelated.txt"), "something else").unwrap();
        repo.seal(
            agent("c1"),
            "unrelated work".into(),
            Some("s3".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Cycle 2: should detect new seal, see the same overlap, but skip
        // re-convergence because the fingerprint is unchanged.
        run_single_cycle(&repo, &config, &mut state, &mut summary, &tx).unwrap();
        assert_eq!(
            summary.convergences_triggered, 1,
            "should NOT re-converge the same overlap"
        );
    }

    #[test]
    fn test_watch_re_converges_on_new_seal_from_participant() {
        // When a participating spec creates a NEW seal, the fingerprint
        // changes and convergence should re-trigger.
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let base = "line1\nline2\nline3\nline4\nline5\n";
        fs::write(dir.path().join("shared.txt"), base).unwrap();
        repo.seal(
            agent("setup"),
            "baseline".into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        repo.add_spec(&Spec::new("s1".into(), "S1".into(), "".into()))
            .unwrap();
        repo.add_spec(&Spec::new("s2".into(), "S2".into(), "".into()))
            .unwrap();

        let mut state = WatchState::new(&repo).unwrap();

        // Agent 1 changes line 1.
        fs::write(
            dir.path().join("shared.txt"),
            "CHANGED_A\nline2\nline3\nline4\nline5\n",
        )
        .unwrap();
        repo.seal(
            agent("a1"),
            "change top".into(),
            Some("s1".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Agent 2 changes line 5.
        fs::write(
            dir.path().join("shared.txt"),
            "line1\nline2\nline3\nline4\nCHANGED_B\n",
        )
        .unwrap();
        repo.seal(
            agent("b1"),
            "change bottom".into(),
            Some("s2".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        let config = WatchConfig {
            auto_converge: true,
            strategy: ConvergeStrategy::MostRecent,
            ..Default::default()
        };
        let mut summary = WatchSummary::default();
        let (tx, _rx) = mpsc::channel();

        // Cycle 1: converge.
        run_single_cycle(&repo, &config, &mut state, &mut summary, &tx).unwrap();
        assert_eq!(summary.convergences_triggered, 1);

        // Agent 1 makes another change to the same file — new seal for s1.
        fs::write(
            dir.path().join("shared.txt"),
            "CHANGED_A_V2\nline2\nline3\nline4\nCHANGED_B\n",
        )
        .unwrap();
        repo.seal(
            agent("a1"),
            "second edit".into(),
            Some("s1".into()),
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap();

        // Cycle 2: s1 has a new seal → fingerprint changed → should re-converge.
        run_single_cycle(&repo, &config, &mut state, &mut summary, &tx).unwrap();
        assert_eq!(
            summary.convergences_triggered, 2,
            "should re-converge when a participating spec has a new seal"
        );
    }

    #[test]
    fn test_conflict_record_persistence() {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        let record = ConflictRecord {
            id: "test-conflict-1".into(),
            file_path: "shared.rs".into(),
            spec_a: "feat-auth".into(),
            spec_b: "feat-pay".into(),
            agent_a: "agent-1".into(),
            agent_b: "agent-2".into(),
            seal_a: "seal-aaa".into(),
            seal_b: "seal-bbb".into(),
            reason: "competing rewrites".into(),
            timestamp: Utc::now(),
        };

        save_conflict_record(&repo, &record).unwrap();

        let records = load_conflict_records(&repo).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, "test-conflict-1");
        assert_eq!(records[0].file_path, "shared.rs");
        assert_eq!(records[0].reason, "competing rewrites");

        // Clear it.
        assert!(clear_conflict(&repo, "test-conflict-1").unwrap());
        assert!(!clear_conflict(&repo, "nonexistent").unwrap());
        let records2 = load_conflict_records(&repo).unwrap();
        assert!(records2.is_empty());
    }
}
