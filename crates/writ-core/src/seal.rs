//! Seals — writ's equivalent of commits.
//!
//! A seal is a verified, structured checkpoint that records what changed,
//! why it changed, who made the change, and what spec drove it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::hash::hash_str;

/// Identity of the agent or human that created a seal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentIdentity {
    /// Unique identifier (e.g. "human-andrew", "agent-worker-3").
    pub id: String,
    /// Type of agent.
    pub agent_type: AgentType,
}

/// Whether the actor is a human or an AI agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentType {
    Human,
    Agent,
}

/// A single file change recorded in a seal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileChange {
    /// Relative path to the file.
    pub path: String,
    /// What kind of change.
    pub change_type: ChangeType,
    /// Previous content hash (None for new files).
    pub old_hash: Option<String>,
    /// New content hash (None for deleted files).
    pub new_hash: Option<String>,
}

/// The type of change to a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeType {
    Added,
    Modified,
    Deleted,
}

/// Verification status at the time of sealing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verification {
    /// Number of tests that passed (if known).
    pub tests_passed: Option<u32>,
    /// Number of tests that failed (if known).
    pub tests_failed: Option<u32>,
    /// Whether the code was linted.
    pub linted: bool,
}

/// Task status associated with the seal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskStatus {
    InProgress,
    Complete,
    Blocked,
}

/// A seal — writ's structured, verified checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Seal {
    /// Unique identifier (SHA-256 of the seal's content).
    pub id: String,
    /// Parent seal ID (None for the first seal).
    pub parent: Option<String>,
    /// When this seal was created.
    pub timestamp: DateTime<Utc>,
    /// Hash of the tree (full snapshot of tracked files).
    pub tree: String,
    /// Who created this seal.
    pub agent: AgentIdentity,
    /// Which spec this work relates to (if any).
    pub spec_id: Option<String>,
    /// Current task status.
    pub status: TaskStatus,
    /// List of file changes in this seal.
    pub changes: Vec<FileChange>,
    /// Verification state at seal time.
    pub verification: Verification,
    /// Human/agent-readable summary of what changed.
    pub summary: String,
    /// Warnings generated at seal time (scope violations, ghost work, etc.).
    /// Always present (empty array when no warnings).
    #[serde(default)]
    pub warnings: Vec<String>,

    // -- Cryptographic integrity fields (Sprint A) --
    /// BLAKE3 hash of the parent seal's chain_hash (None for genesis seal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_seal_hash: Option<String>,
    /// BLAKE3 hash of this seal's canonical content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// BLAKE3(parent_seal_hash || content_hash) — links this seal to the chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_hash: Option<String>,
    /// Ed25519 signature of the content_hash, hex-encoded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

impl Seal {
    /// Create a new seal and compute its content hash.
    ///
    /// The `id` field is set to the SHA-256 of the seal's JSON
    /// representation (with `id` set to an empty string during hashing).
    ///
    /// `parent_seal_hash` is the chain_hash of the parent seal (None for the
    /// first seal in a chain). The cryptographic fields (`content_hash`,
    /// `chain_hash`, `signature`) are populated after construction using
    /// the `crypto` module.
    pub fn new(
        parent: Option<String>,
        tree: String,
        agent: AgentIdentity,
        spec_id: Option<String>,
        status: TaskStatus,
        changes: Vec<FileChange>,
        verification: Verification,
        summary: String,
        warnings: Vec<String>,
        parent_seal_hash: Option<String>,
    ) -> Self {
        let mut seal = Seal {
            id: String::new(),
            parent,
            timestamp: Utc::now(),
            tree,
            agent,
            spec_id,
            status,
            changes,
            verification,
            summary,
            warnings,
            parent_seal_hash,
            content_hash: None,
            chain_hash: None,
            signature: None,
        };

        // Compute the seal's ID from its content
        let json = serde_json::to_string(&seal).expect("seal serialization should not fail");
        seal.id = hash_str(&json);
        seal
    }

    /// Populate cryptographic integrity fields on this seal.
    ///
    /// Computes `content_hash` (BLAKE3 of canonical content), `chain_hash`
    /// (BLAKE3 linking to parent), and optionally `signature` (Ed25519 of
    /// the content_hash). Recomputes `id` after crypto fields are set.
    pub fn secure(&mut self, signing_key: Option<&ed25519_dalek::SigningKey>) {
        use crate::crypto;

        let content = crypto::compute_content_hash(self);

        self.chain_hash = Some(crypto::compute_chain_hash(
            self.parent_seal_hash.as_deref(),
            &content,
        ));

        if let Some(key) = signing_key {
            self.signature = Some(crypto::sign(&content, key));
        }

        self.content_hash = Some(content);

        // Recompute ID now that crypto fields are populated
        self.id = String::new();
        let json = serde_json::to_string(self).expect("seal serialization should not fail");
        self.id = hash_str(&json);
    }

    /// Returns `true` if this seal has all cryptographic integrity fields populated.
    pub fn is_secured(&self) -> bool {
        self.content_hash.is_some() && self.chain_hash.is_some()
    }

    /// Returns `true` if this seal is both secured and signed.
    pub fn is_signed(&self) -> bool {
        self.is_secured() && self.signature.is_some()
    }
}
