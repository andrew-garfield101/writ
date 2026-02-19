//! Remote — shared state types for writ push/pull.
//!
//! A writ remote is a bare directory (no working tree) that stores
//! objects, seals, and specs. Agents on different machines push/pull
//! to share state.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Remote configuration stored at `.writ/config.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RemoteConfig {
    /// Named remotes (e.g. "origin" -> path).
    #[serde(default)]
    pub remotes: BTreeMap<String, RemoteEntry>,
}

/// A single remote entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteEntry {
    /// Filesystem path to the bare remote directory.
    pub path: String,
}

// ---------------------------------------------------------------------------
// Sync state
// ---------------------------------------------------------------------------

/// Persistent sync state stored at `.writ/sync.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_push_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_push_seal_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_pull_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_pull_seal_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_head: Option<String>,
}

// ---------------------------------------------------------------------------
// Operation results
// ---------------------------------------------------------------------------

/// Result of a push operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushResult {
    pub remote: String,
    pub objects_pushed: usize,
    pub seals_pushed: usize,
    pub specs_pushed: usize,
    pub head_updated: bool,
}

/// Result of a pull operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullResult {
    pub remote: String,
    pub objects_pulled: usize,
    pub seals_pulled: usize,
    pub specs_pulled: usize,
    pub head_updated: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub spec_conflicts: Vec<SpecMergeConflict>,
}

/// A field-level conflict encountered during spec merge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecMergeConflict {
    pub spec_id: String,
    pub field: String,
    pub local_value: String,
    pub remote_value: String,
    pub resolution: String,
}

/// Status of a remote relative to local.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteStatus {
    pub name: String,
    pub path: String,
    pub local_head: Option<String>,
    pub remote_head: Option<String>,
    pub ahead: usize,
    pub behind: usize,
}
