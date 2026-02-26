//! Agent identity — registration, trust levels, and scope constraints.
//!
//! A `RegisteredAgent` is the rich identity record stored in `.writ/agents/`.
//! It links to the lightweight `AgentIdentity` embedded in seals via `agent_id`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Trust levels
// ---------------------------------------------------------------------------

/// Trust level determines what confidence cap an agent's contributions
/// receive during convergence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustLevel {
    /// No confidence cap (1.0). Typically human operators.
    Full,
    /// Slight confidence reduction (0.90). Default for agents.
    Standard,
    /// Significant confidence reduction (0.60).
    Restricted,
    /// Always escalate to human review (0.0).
    Untrusted,
}

impl Default for TrustLevel {
    fn default() -> Self {
        TrustLevel::Standard
    }
}

impl TrustLevel {
    /// Parse a trust level from a string (case-insensitive).
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "full" => Some(TrustLevel::Full),
            "standard" => Some(TrustLevel::Standard),
            "restricted" => Some(TrustLevel::Restricted),
            "untrusted" => Some(TrustLevel::Untrusted),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Agent status
// ---------------------------------------------------------------------------

/// Agent lifecycle status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    /// Agent is active and can create seals.
    Active,
    /// Agent is temporarily suspended (seals produce warnings).
    Suspended,
    /// Agent is permanently revoked (seals produce warnings).
    Revoked,
}

impl Default for AgentStatus {
    fn default() -> Self {
        AgentStatus::Active
    }
}

// ---------------------------------------------------------------------------
// RegisteredAgent
// ---------------------------------------------------------------------------

/// Extended agent identity stored in `.writ/agents/{agent_id}.json`.
///
/// The seal's `AgentIdentity` stays lightweight (just `id` + `agent_type`).
/// This record holds the rich metadata — trust level, scope constraints,
/// public key, lifecycle status. The `agent_id` field links the two.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredAgent {
    /// Unique agent identifier (matches `AgentIdentity.id` in seals).
    pub agent_id: String,
    /// Hex-encoded Ed25519 public key.
    pub public_key: String,
    /// When this agent was registered.
    pub registered_at: DateTime<Utc>,
    /// Who registered this agent (agent_id of the registering entity).
    pub registered_by: String,
    /// Trust level governing convergence confidence caps.
    pub trust_level: TrustLevel,
    /// Glob patterns restricting which files this agent can modify.
    /// Empty = unrestricted.
    #[serde(default)]
    pub scope_constraints: Vec<String>,
    /// Current lifecycle status.
    pub status: AgentStatus,
    /// When the agent was revoked (if applicable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
    /// Reason for revocation (if applicable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocation_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// AgentUpdate
// ---------------------------------------------------------------------------

/// Fields that can be updated on a registered agent.
#[derive(Debug, Clone, Default)]
pub struct AgentUpdate {
    pub trust_level: Option<TrustLevel>,
    pub scope_constraints: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Trust context for convergence
// ---------------------------------------------------------------------------

/// Trust context passed to the convergence pipeline for confidence capping.
#[derive(Debug, Clone)]
pub struct TrustContext {
    pub left_trust: TrustLevel,
    pub right_trust: TrustLevel,
}

impl TrustContext {
    /// Compute the trust adjustment factor for convergence confidence.
    ///
    /// Returns a value in \[0.0, 1.0\] that caps pattern confidence:
    /// - Both Full: 1.0
    /// - Both Standard: 0.90
    /// - Mixed (Full + Standard): 0.75
    /// - Either Restricted: 0.60
    /// - Either Untrusted: 0.0 (always escalate)
    pub fn trust_adjustment(&self) -> f64 {
        use TrustLevel::*;
        match (self.left_trust, self.right_trust) {
            (Untrusted, _) | (_, Untrusted) => 0.0,
            (Restricted, _) | (_, Restricted) => 0.60,
            (Full, Full) => 1.0,
            (Standard, Standard) => 0.90,
            _ => 0.75, // Mixed: Full + Standard
        }
    }
}

// ---------------------------------------------------------------------------
// Scope checking
// ---------------------------------------------------------------------------

/// Check whether a file path is within an agent's scope constraints.
///
/// Returns `true` if the agent has no scope constraints (empty = unrestricted)
/// or if the path matches at least one constraint pattern.
pub fn is_in_scope(scope_constraints: &[String], file_path: &str) -> bool {
    if scope_constraints.is_empty() {
        return true;
    }
    let normalized = match canonicalize_path(file_path) {
        Some(p) => p,
        None => return false, // Path rejected (traversal, absolute, etc.)
    };
    scope_constraints.iter().any(|scope| {
        if scope.ends_with('/') {
            normalized.starts_with(scope) || normalized.starts_with(&scope[..scope.len() - 1])
        } else if scope.contains('*') {
            crate::ignore::glob_match(scope, &normalized)
        } else {
            normalized == *scope || normalized.starts_with(&format!("{scope}/"))
        }
    })
}

/// Canonicalize a path for scope checking.
///
/// Returns `None` if the path is rejected:
/// - Contains `../` or `..\\` (traversal attack)
/// - Is an absolute path
///
/// Normalizes:
/// - Leading `./`
/// - Double slashes `//`
/// - Trailing slashes
pub fn canonicalize_path(path: &str) -> Option<String> {
    // Reject traversal
    if path.contains("../") || path.contains("..\\") || path == ".." {
        return None;
    }

    let mut result = path.to_string();

    // Strip leading ./
    while result.starts_with("./") {
        result = result[2..].to_string();
    }

    // Normalize double slashes
    while result.contains("//") {
        result = result.replace("//", "/");
    }

    // Strip trailing slash
    if result.ends_with('/') && result.len() > 1 {
        result.pop();
    }

    // Reject absolute paths
    if result.starts_with('/') || result.starts_with('\\') {
        return None;
    }

    // Reject Windows-style absolute paths (C:\, D:\, etc.)
    if result.len() >= 2 && result.as_bytes()[1] == b':' {
        return None;
    }

    Some(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- Serialization roundtrips ---

    #[test]
    fn test_trust_level_serialization_roundtrip() {
        for level in [
            TrustLevel::Full,
            TrustLevel::Standard,
            TrustLevel::Restricted,
            TrustLevel::Untrusted,
        ] {
            let json = serde_json::to_string(&level).unwrap();
            let recovered: TrustLevel = serde_json::from_str(&json).unwrap();
            assert_eq!(level, recovered);
        }
    }

    #[test]
    fn test_trust_level_serde_values() {
        assert_eq!(
            serde_json::to_string(&TrustLevel::Full).unwrap(),
            "\"full\""
        );
        assert_eq!(
            serde_json::to_string(&TrustLevel::Standard).unwrap(),
            "\"standard\""
        );
        assert_eq!(
            serde_json::to_string(&TrustLevel::Restricted).unwrap(),
            "\"restricted\""
        );
        assert_eq!(
            serde_json::to_string(&TrustLevel::Untrusted).unwrap(),
            "\"untrusted\""
        );
    }

    #[test]
    fn test_trust_level_default() {
        assert_eq!(TrustLevel::default(), TrustLevel::Standard);
    }

    #[test]
    fn test_trust_level_from_str_loose() {
        assert_eq!(TrustLevel::from_str_loose("full"), Some(TrustLevel::Full));
        assert_eq!(TrustLevel::from_str_loose("FULL"), Some(TrustLevel::Full));
        assert_eq!(
            TrustLevel::from_str_loose("Standard"),
            Some(TrustLevel::Standard)
        );
        assert_eq!(
            TrustLevel::from_str_loose("restricted"),
            Some(TrustLevel::Restricted)
        );
        assert_eq!(
            TrustLevel::from_str_loose("UNTRUSTED"),
            Some(TrustLevel::Untrusted)
        );
        assert_eq!(TrustLevel::from_str_loose("invalid"), None);
        assert_eq!(TrustLevel::from_str_loose(""), None);
    }

    #[test]
    fn test_agent_status_serialization_roundtrip() {
        for status in [
            AgentStatus::Active,
            AgentStatus::Suspended,
            AgentStatus::Revoked,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let recovered: AgentStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, recovered);
        }
    }

    #[test]
    fn test_agent_status_default() {
        assert_eq!(AgentStatus::default(), AgentStatus::Active);
    }

    #[test]
    fn test_registered_agent_json_roundtrip() {
        let agent = RegisteredAgent {
            agent_id: "agent-worker-1".to_string(),
            public_key: "aa".repeat(32),
            registered_at: Utc::now(),
            registered_by: "human-andrew".to_string(),
            trust_level: TrustLevel::Standard,
            scope_constraints: vec!["src/**".to_string(), "tests/".to_string()],
            status: AgentStatus::Active,
            revoked_at: None,
            revocation_reason: None,
        };

        let json = serde_json::to_string_pretty(&agent).unwrap();
        let recovered: RegisteredAgent = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.agent_id, agent.agent_id);
        assert_eq!(recovered.public_key, agent.public_key);
        assert_eq!(recovered.trust_level, agent.trust_level);
        assert_eq!(recovered.scope_constraints, agent.scope_constraints);
        assert_eq!(recovered.status, agent.status);
        assert!(recovered.revoked_at.is_none());
        assert!(recovered.revocation_reason.is_none());
    }

    #[test]
    fn test_registered_agent_revoked_roundtrip() {
        let agent = RegisteredAgent {
            agent_id: "bad-agent".to_string(),
            public_key: "bb".repeat(32),
            registered_at: Utc::now(),
            registered_by: "human-andrew".to_string(),
            trust_level: TrustLevel::Untrusted,
            scope_constraints: vec![],
            status: AgentStatus::Revoked,
            revoked_at: Some(Utc::now()),
            revocation_reason: Some("compromised".to_string()),
        };

        let json = serde_json::to_string_pretty(&agent).unwrap();
        let recovered: RegisteredAgent = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.status, AgentStatus::Revoked);
        assert!(recovered.revoked_at.is_some());
        assert_eq!(recovered.revocation_reason.as_deref(), Some("compromised"));
    }

    // --- Trust adjustment ---

    #[test]
    fn test_trust_adjustment_both_full() {
        let ctx = TrustContext {
            left_trust: TrustLevel::Full,
            right_trust: TrustLevel::Full,
        };
        assert_eq!(ctx.trust_adjustment(), 1.0);
    }

    #[test]
    fn test_trust_adjustment_both_standard() {
        let ctx = TrustContext {
            left_trust: TrustLevel::Standard,
            right_trust: TrustLevel::Standard,
        };
        assert_eq!(ctx.trust_adjustment(), 0.90);
    }

    #[test]
    fn test_trust_adjustment_mixed_full_standard() {
        let ctx = TrustContext {
            left_trust: TrustLevel::Full,
            right_trust: TrustLevel::Standard,
        };
        assert_eq!(ctx.trust_adjustment(), 0.75);

        // Symmetric
        let ctx2 = TrustContext {
            left_trust: TrustLevel::Standard,
            right_trust: TrustLevel::Full,
        };
        assert_eq!(ctx2.trust_adjustment(), 0.75);
    }

    #[test]
    fn test_trust_adjustment_restricted() {
        // Restricted with anything other than Untrusted → 0.60
        for other in [
            TrustLevel::Full,
            TrustLevel::Standard,
            TrustLevel::Restricted,
        ] {
            let ctx = TrustContext {
                left_trust: TrustLevel::Restricted,
                right_trust: other,
            };
            assert_eq!(
                ctx.trust_adjustment(),
                0.60,
                "Restricted + {other:?} should be 0.60"
            );

            // Symmetric
            let ctx2 = TrustContext {
                left_trust: other,
                right_trust: TrustLevel::Restricted,
            };
            assert_eq!(
                ctx2.trust_adjustment(),
                0.60,
                "{other:?} + Restricted should be 0.60"
            );
        }
    }

    #[test]
    fn test_trust_adjustment_untrusted() {
        // Untrusted with anything → 0.0
        for other in [
            TrustLevel::Full,
            TrustLevel::Standard,
            TrustLevel::Restricted,
            TrustLevel::Untrusted,
        ] {
            let ctx = TrustContext {
                left_trust: TrustLevel::Untrusted,
                right_trust: other,
            };
            assert_eq!(
                ctx.trust_adjustment(),
                0.0,
                "Untrusted + {other:?} should be 0.0"
            );

            // Symmetric
            let ctx2 = TrustContext {
                left_trust: other,
                right_trust: TrustLevel::Untrusted,
            };
            assert_eq!(
                ctx2.trust_adjustment(),
                0.0,
                "{other:?} + Untrusted should be 0.0"
            );
        }
    }

    #[test]
    fn test_trust_adjustment_untrusted_overrides_restricted() {
        // Untrusted takes priority over Restricted
        let ctx = TrustContext {
            left_trust: TrustLevel::Untrusted,
            right_trust: TrustLevel::Restricted,
        };
        assert_eq!(ctx.trust_adjustment(), 0.0);
    }

    // --- Scope checking ---

    #[test]
    fn test_is_in_scope_empty_constraints() {
        assert!(is_in_scope(&[], "anything/goes.rs"));
    }

    #[test]
    fn test_is_in_scope_exact_match() {
        let scope = vec!["src/main.rs".to_string()];
        assert!(is_in_scope(&scope, "src/main.rs"));
        assert!(!is_in_scope(&scope, "src/lib.rs"));
    }

    #[test]
    fn test_is_in_scope_directory_prefix() {
        let scope = vec!["src/".to_string()];
        assert!(is_in_scope(&scope, "src/main.rs"));
        assert!(is_in_scope(&scope, "src/core/repo.rs"));
        assert!(!is_in_scope(&scope, "tests/test.rs"));
    }

    #[test]
    fn test_is_in_scope_directory_without_trailing_slash() {
        let scope = vec!["src".to_string()];
        assert!(is_in_scope(&scope, "src/main.rs"));
        assert!(!is_in_scope(&scope, "srclib.rs")); // Should not match partial prefix
    }

    #[test]
    fn test_is_in_scope_wildcard() {
        let scope = vec!["*.rs".to_string()];
        assert!(is_in_scope(&scope, "main.rs"));
        assert!(!is_in_scope(&scope, "main.py"));
    }

    #[test]
    fn test_is_in_scope_glob_star() {
        let scope = vec!["src/**".to_string()];
        assert!(is_in_scope(&scope, "src/main.rs"));
        assert!(is_in_scope(&scope, "src/core/deep/file.rs"));
        assert!(!is_in_scope(&scope, "tests/test.rs"));
    }

    #[test]
    fn test_is_in_scope_multiple_constraints() {
        let scope = vec!["src/".to_string(), "tests/".to_string()];
        assert!(is_in_scope(&scope, "src/main.rs"));
        assert!(is_in_scope(&scope, "tests/test.rs"));
        assert!(!is_in_scope(&scope, "docs/readme.md"));
    }

    #[test]
    fn test_is_in_scope_rejects_traversal() {
        let scope = vec!["src/".to_string()];
        assert!(!is_in_scope(&scope, "src/../secrets/key.pem"));
        assert!(!is_in_scope(&scope, "../etc/passwd"));
    }

    #[test]
    fn test_is_in_scope_normalizes_dot_slash() {
        let scope = vec!["src/".to_string()];
        assert!(is_in_scope(&scope, "./src/main.rs"));
    }

    #[test]
    fn test_is_in_scope_normalizes_double_slash() {
        let scope = vec!["src/".to_string()];
        assert!(is_in_scope(&scope, "src//main.rs"));
    }

    // --- canonicalize_path ---

    #[test]
    fn test_canonicalize_normal_path() {
        assert_eq!(
            canonicalize_path("src/main.rs"),
            Some("src/main.rs".to_string())
        );
    }

    #[test]
    fn test_canonicalize_rejects_traversal() {
        assert_eq!(canonicalize_path("../secret"), None);
        assert_eq!(canonicalize_path("src/../secret"), None);
        assert_eq!(canonicalize_path("src/..\\secret"), None);
        assert_eq!(canonicalize_path(".."), None);
    }

    #[test]
    fn test_canonicalize_strips_dot_slash() {
        assert_eq!(
            canonicalize_path("./src/main.rs"),
            Some("src/main.rs".to_string())
        );
        assert_eq!(
            canonicalize_path("././src/main.rs"),
            Some("src/main.rs".to_string())
        );
    }

    #[test]
    fn test_canonicalize_normalizes_double_slashes() {
        assert_eq!(
            canonicalize_path("src//core//file.rs"),
            Some("src/core/file.rs".to_string())
        );
    }

    #[test]
    fn test_canonicalize_strips_trailing_slash() {
        assert_eq!(canonicalize_path("src/core/"), Some("src/core".to_string()));
    }

    #[test]
    fn test_canonicalize_rejects_absolute_unix() {
        assert_eq!(canonicalize_path("/etc/passwd"), None);
    }

    #[test]
    fn test_canonicalize_rejects_absolute_windows() {
        assert_eq!(canonicalize_path("C:\\Windows\\System32"), None);
        assert_eq!(canonicalize_path("\\\\server\\share"), None);
    }

    #[test]
    fn test_canonicalize_empty_stays_empty() {
        assert_eq!(canonicalize_path(""), Some("".to_string()));
    }

    #[test]
    fn test_canonicalize_single_file() {
        assert_eq!(canonicalize_path("file.rs"), Some("file.rs".to_string()));
    }
}
