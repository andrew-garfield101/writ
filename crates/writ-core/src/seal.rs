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

    // -- Workspace field (Workspace Sprint WS.0) --
    /// Which workspace this seal was created in. Defaults to "main" for
    /// backward compatibility with pre-workspace seals.
    #[serde(default = "default_workspace")]
    pub workspace: String,

    // -- Seal-triggered convergence result --
    /// Result of post-seal convergence, if overlaps were detected and
    /// convergence was attempted. None when no overlaps exist or when
    /// convergence-on-seal is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub convergence: Option<SealConvergenceResult>,

    // -- Hurdle record (finding 76) --
    /// The seal was made with `--force`: the own-line survival check was
    /// bypassed for every file. Absent when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub forced: bool,
    /// Ruling 183: the seal was made while the fixture switch
    /// `[seal] foreign_line_check = false` turned the foreign-line check
    /// off. Absent when the check ran (the default).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub foreign_line_check_off: bool,
    /// Paths named with `--allow-removals`: the own-line check was bypassed
    /// for these files only. Absent when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_removals: Vec<String>,
    /// Finding 102: for each path where `--allow-removals` let this seal
    /// remove lines its spec sealed, what the agent was told when it chose
    /// to: the other agents who worked on the path and the lines removed.
    /// Absent when empty (no override was used).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub override_context: Vec<OverrideContext>,
}

/// One path's override record on a seal (finding 102).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverrideContext {
    pub path: String,
    /// Agents other than the sealer who worked on the path since its spec's
    /// last seal of it; empty when nobody did.
    pub other_agents: Vec<String>,
    /// The removed lines.
    pub lines: Vec<String>,
    /// Full ids of this spec's seals that added the removed lines.
    pub added_by: Vec<String>,
    /// Finding 122: the agents among `other_agents` whose sealed content is
    /// the replacement (a line this seal added on the path that the other
    /// agent's seal added). Empty when the sealer replaced its own lines
    /// with its own content. `None` on records written before 0.4.2, which
    /// did not check: they are described without naming an author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_from: Option<Vec<String>>,
    /// The removed lines that were another agent's call (they removed them,
    /// or their sealed lines replaced them); the rest of `lines` the sealer
    /// rewrote itself. Both non-empty: a mixed override.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepted_lines: Vec<String>,
    /// Finding 173: written by the auto seal hook for the agent's own fresh
    /// edit (finding 144); nobody was told, so the line never says so.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub by_hook: bool,
}

impl OverrideContext {
    /// The finish and handoff line: who accepted whose version, or who
    /// removed their own lines, after being told (seal `seal_id`).
    pub fn describe(&self, agent: &str, seal_id: &str) -> String {
        let id12 = &seal_id[..seal_id.len().min(12)];
        let from = self
            .accepted_from
            .as_deref()
            .and_then(join_names)
            .unwrap_or_default();
        let own = self.own_rewritten();
        if self.by_hook {
            return format!(
                "{agent} rewrote {} own line(s) of {} in its own edit, recorded by writ's auto seal (seal {id12})",
                self.lines.len(),
                self.path
            );
        }
        match self.resolution() {
            "accepted_after_told" => format!(
                "{agent} accepted {from}'s version of {} after being told (seal {id12})",
                self.path
            ),
            "mixed" => format!(
                "{agent} accepted {from}'s version of {} for {} line(s) and removed {own} own line(s) on purpose after being told (seal {id12})",
                self.path,
                self.accepted_lines.len()
            ),
            _ => {
                let mut out = format!(
                    "{agent} removed {} own line(s) from {} on purpose after being told (seal {id12})",
                    self.lines.len(),
                    self.path
                );
                // Finding 122: another toucher is context, never the author.
                if let Some(others) = join_names(&self.other_agents) {
                    out.push_str(&format!("; {others} touched the file since {agent}'s last seal"));
                }
                out
            }
        }
    }

    /// Removed lines the sealer rewrote itself (not another agent's call).
    pub fn own_rewritten(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| !self.accepted_lines.contains(l))
            .count()
    }

    /// Finding 122: how the override resolved, for handoff and why:
    /// `accepted_after_told` when every removed line was another agent's
    /// call, `mixed` when some were, `rewrote_own` when none were (or the
    /// record predates the check).
    pub fn resolution(&self) -> &'static str {
        let named = self.accepted_from.as_deref().is_some_and(|f| !f.is_empty());
        if !named {
            "rewrote_own"
        } else if self.accepted_lines.is_empty() || self.own_rewritten() == 0 {
            "accepted_after_told"
        } else {
            "mixed"
        }
    }
}

pub fn join_names(names: &[String]) -> Option<String> {
    match names {
        [] => None,
        [one] => Some(one.clone()),
        [first, rest @ ..] => Some(format!(
            "{first}{}",
            rest.iter()
                .map(|n| format!(", and {n}"))
                .collect::<String>()
        )),
    }
}

fn default_workspace() -> String {
    "main".to_string()
}

/// Result of seal-triggered convergence (best-effort, never blocks seal).
///
/// When a seal detects overlapping file changes across specs, it runs
/// convergence inline. This struct carries the result. The seal is always
/// committed first — convergence cannot block or corrupt it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SealConvergenceResult {
    /// Whether convergence was attempted.
    pub attempted: bool,
    /// Whether convergence completed successfully.
    pub succeeded: bool,
    /// Number of files merged.
    pub files_merged: usize,
    /// Number of overlapping files detected.
    pub overlaps_detected: usize,
    /// Specs involved in the convergence.
    pub specs_involved: Vec<String>,
    /// Warning/error message if convergence failed or had escalations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
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
    #[allow(clippy::too_many_arguments)]
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
            workspace: default_workspace(),
            convergence: None,
            forced: false,
            foreign_line_check_off: false,
            allow_removals: Vec::new(),
            override_context: Vec::new(),
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
