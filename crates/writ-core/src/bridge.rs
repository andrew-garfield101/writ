//! Bridge — seamless round-trip between git and writ.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Persistent bridge state stored at `.writ/bridge.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BridgeState {
    /// Git commit hash that was last imported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_imported_git_commit: Option<String>,
    /// Writ seal ID created by the last import.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_imported_seal_id: Option<String>,
    /// Git ref that was imported from (e.g. "HEAD", "refs/heads/main").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imported_from_ref: Option<String>,
    /// Writ seal ID that was last exported to git.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_exported_seal_id: Option<String>,
    /// Git branch that was last exported to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exported_to_branch: Option<String>,
    /// Git commit hash created by the last export.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_exported_git_commit: Option<String>,
    /// Timestamp of last bridge operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<DateTime<Utc>>,
}

/// Result of a bridge import operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportResult {
    /// Git commit that was imported.
    pub git_commit: String,
    /// Git ref that was resolved.
    pub git_ref: String,
    /// Writ seal ID created for the baseline.
    pub seal_id: String,
    /// Number of files imported.
    pub files_imported: usize,
}

/// A single seal-to-commit mapping from an export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedSeal {
    /// Writ seal ID.
    pub seal_id: String,
    /// Git commit hash created.
    pub git_commit: String,
    /// Seal summary (used as commit message).
    pub summary: String,
}

/// Result of a bridge export operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportResult {
    /// Git branch the commits were created on.
    pub branch: String,
    /// List of seal-to-commit mappings (in order).
    pub exported: Vec<ExportedSeal>,
    /// Total seals exported.
    pub seals_exported: usize,
}

/// Current bridge sync status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeStatus {
    /// Whether bridge state exists (import has been done).
    pub initialized: bool,
    /// Last import info.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_import: Option<ImportSummary>,
    /// Last export info.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_export: Option<ExportSummary>,
    /// Number of seals pending export.
    pub pending_export_count: usize,
}

/// Summary of the last import.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportSummary {
    pub git_commit: String,
    pub git_ref: String,
    pub seal_id: String,
}

/// Summary of the last export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportSummary {
    pub seal_id: String,
    pub git_commit: String,
    pub branch: String,
}
