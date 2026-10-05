//! Agent identity — registration, trust levels, and scope constraints.
//!
//! A `RegisteredAgent` is the rich identity record stored in `.writ/agents/`.
//! It links to the lightweight `AgentIdentity` embedded in seals via `agent_id`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Identity resolution (S.3): the one resolver every surface uses
// ---------------------------------------------------------------------------

/// Environment variable that sets an agent's writ identity explicitly.
pub const AGENT_ID_ENV: &str = "WRIT_AGENT_ID";

/// Where a resolved agent ID came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentitySource {
    /// `--agent` flag or an explicit `agent_id` argument.
    Explicit,
    /// The `WRIT_AGENT_ID` environment variable.
    Env,
    /// The repository's `default_agent` setting.
    Setting,
    /// Detected from an agent framework's session variable (named).
    Framework(String),
    /// Nothing set: the human operator.
    Default,
}

/// A resolved agent identity and its source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedIdentity {
    pub id: String,
    pub source: IdentitySource,
}

impl ResolvedIdentity {
    /// True when the identity came from an agent signal, not the default.
    pub fn is_agent(&self) -> bool {
        self.id != "human" && self.source != IdentitySource::Default
    }
}

/// Resolve the acting agent's ID. Priority:
/// explicit > `WRIT_AGENT_ID` > `default_agent` setting > framework session
/// variable (only when `detect_frameworks`) > `"human"`.
///
/// `WRIT_AGENT_ID` beats the repository setting because it is per process:
/// a subagent that sets it must not be overridden by a repo-wide default.
/// Empty values are ignored at every level.
pub fn resolve_agent_id(
    explicit: Option<&str>,
    default_agent: Option<&str>,
    detect_frameworks: bool,
) -> ResolvedIdentity {
    resolve_agent_id_with(explicit, default_agent, detect_frameworks, &|k| {
        std::env::var(k).ok()
    })
}

/// [`resolve_agent_id`] with an injectable environment, for tests.
pub fn resolve_agent_id_with(
    explicit: Option<&str>,
    default_agent: Option<&str>,
    detect_frameworks: bool,
    env: &dyn Fn(&str) -> Option<String>,
) -> ResolvedIdentity {
    let non_empty = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let found = |id: String, source| ResolvedIdentity { id, source };
    if let Some(id) = non_empty(explicit.map(String::from)) {
        return found(id, IdentitySource::Explicit);
    }
    if let Some(id) = non_empty(env(AGENT_ID_ENV)) {
        return found(id, IdentitySource::Env);
    }
    if let Some(id) = non_empty(default_agent.map(String::from)) {
        return found(id, IdentitySource::Setting);
    }
    if detect_frameworks {
        if let Some(r) = detect_framework_identity(env) {
            return r;
        }
    }
    found("human".to_string(), IdentitySource::Default)
}

/// Framework session variables, in priority order, with the ID prefix used.
const FRAMEWORK_VARS: &[(&str, &str)] = &[
    ("CLAUDE_CODE_SESSION_ID", "claude-code"),
    ("CLAUDE_SESSION_ID", "claude-code"),
    ("ANTHROPIC_SESSION_ID", "claude-code"),
    ("CODEX_SESSION", "codex"),
    ("CODEX_SESSION_ID", "codex"),
];

/// Session-specific ID such as `claude-code-a3f2` from a framework variable.
///
/// Subagents spawned by a hub inherit its session variable and so resolve
/// to the hub's ID; they must set `WRIT_AGENT_ID` (see the agent template).
fn detect_framework_identity(env: &dyn Fn(&str) -> Option<String>) -> Option<ResolvedIdentity> {
    for (var, prefix) in FRAMEWORK_VARS {
        if let Some(session) = env(var).filter(|v| !v.is_empty()) {
            return Some(ResolvedIdentity {
                id: format!("{prefix}-{}", short_hash(&session)),
                source: IdentitySource::Framework((*var).to_string()),
            });
        }
    }
    // CLAUDECODE=1 is set by Claude Code even when session IDs are absent.
    // Finding 51: never derive the ID from the process id (every writ
    // invocation would be a different agent). Without a repository the ID
    // is the bare prefix; [`resolve_agent_id_in`] swaps in the ID persisted
    // for this repository.
    if env("CLAUDECODE").is_some() {
        return Some(ResolvedIdentity {
            id: CLAUDECODE_FALLBACK_ID.to_string(),
            source: IdentitySource::Framework(CLAUDECODE_VAR.to_string()),
        });
    }
    None
}

/// Claude Code's marker variable, set even without a session ID.
const CLAUDECODE_VAR: &str = "CLAUDECODE";

/// ID for a `CLAUDECODE`-only environment outside any repository.
pub const CLAUDECODE_FALLBACK_ID: &str = "claude-code";

/// File under `.writ/agents/` holding the persisted `CLAUDECODE`-only ID.
pub const CLAUDECODE_ID_FILE: &str = "claudecode.id";

/// [`resolve_agent_id`] inside a repository: a `CLAUDECODE`-only identity
/// becomes an ID persisted under `.writ/agents/claudecode/`, keyed by the
/// controlling session process (finding 57), so one session keeps one ID
/// across invocations (finding 51) and a second session gets its own. If the
/// session process cannot be found the repository-wide
/// `.writ/agents/claudecode.id` is used; if nothing can be persisted, the
/// bare [`CLAUDECODE_FALLBACK_ID`].
pub fn resolve_agent_id_in(
    explicit: Option<&str>,
    default_agent: Option<&str>,
    detect_frameworks: bool,
    writ_dir: Option<&std::path::Path>,
) -> ResolvedIdentity {
    let mut resolved = resolve_agent_id(explicit, default_agent, detect_frameworks);
    if resolved.source == IdentitySource::Framework(CLAUDECODE_VAR.to_string()) {
        if let Some(dir) = writ_dir {
            let key = session::current_session_key();
            match persisted_session_id(dir, key.as_deref()) {
                Ok(id) => resolved.id = id,
                Err(e) => eprintln!(
                    "warning: could not persist the CLAUDECODE agent id under {}: {e}; using '{}'",
                    dir.join("agents").display(),
                    CLAUDECODE_FALLBACK_ID
                ),
            }
        }
    }
    resolved
}

/// Read, or create once, the persisted `CLAUDECODE`-only agent ID for the
/// session `key` (`None`: the repository-wide fallback file).
///
/// Creation uses `create_new`, so two processes racing on first use agree:
/// the loser reads the winner's file.
pub fn persisted_session_id(
    writ_dir: &std::path::Path,
    key: Option<&str>,
) -> std::io::Result<String> {
    use std::io::Write;
    let (dir, path) = match key {
        Some(k) => {
            let dir = writ_dir.join("agents").join("claudecode");
            let path = dir.join(format!("{k}.id"));
            (dir, path)
        }
        None => {
            let dir = writ_dir.join("agents");
            let path = dir.join(CLAUDECODE_ID_FILE);
            (dir, path)
        }
    };
    if let Some(id) = read_session_id(&path)? {
        return Ok(id);
    }
    std::fs::create_dir_all(&dir)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seed = format!(
        "{}:{}:{}:{}",
        std::process::id(),
        nanos,
        writ_dir.display(),
        key.unwrap_or("")
    );
    let id = format!(
        "{CLAUDECODE_FALLBACK_ID}-{}",
        &blake3::hash(seed.as_bytes()).to_hex()[..8]
    );
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut f) => {
            f.write_all(id.as_bytes())?;
            Ok(id)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => match read_session_id(&path)? {
            Some(existing) => Ok(existing),
            // An empty or blank file (truncated write): replace it.
            None => {
                std::fs::write(&path, id.as_bytes())?;
                Ok(id)
            }
        },
        Err(e) => Err(e),
    }
}

/// Finding 57: which process is "the session" for a `CLAUDECODE`-only
/// environment. Walk up from writ's parent, skipping shells and writ
/// itself (Bash tool calls go through a fresh shell each time; MCP calls
/// go through `writ mcp-serve`): the first ancestor named `claude`, else
/// the first ancestor that is neither. The key is a hash of that process's
/// pid and start time, so a recycled pid is a new session.
pub mod session {
    /// One row of the process table.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ProcInfo {
        pub pid: u32,
        pub ppid: u32,
        /// Start time as `ps -o lstart` prints it.
        pub start: String,
        /// Command (path or name).
        pub comm: String,
    }

    /// Process names that are never the session: shells, launch wrappers,
    /// and writ itself.
    const PASS_THROUGH: &[&str] = &[
        "sh", "bash", "zsh", "dash", "fish", "ksh", "tcsh", "csh", "login", "env", "sudo", "nohup",
        "timeout", "time", "xargs", "writ",
    ];

    fn base(comm: &str) -> String {
        let name = comm.rsplit('/').next().unwrap_or(comm);
        name.trim_start_matches('-').to_ascii_lowercase()
    }

    /// The session process among `ancestors` (nearest first).
    pub fn session_process(ancestors: &[ProcInfo]) -> Option<&ProcInfo> {
        ancestors
            .iter()
            .find(|p| base(&p.comm).contains("claude"))
            .or_else(|| {
                ancestors
                    .iter()
                    .find(|p| !PASS_THROUGH.contains(&base(&p.comm).as_str()))
            })
    }

    /// Key for a session process: hash of pid and start time.
    pub fn session_key(p: &ProcInfo) -> String {
        let seed = format!("{}:{}", p.pid, p.start);
        blake3::hash(seed.as_bytes()).to_hex()[..16].to_string()
    }

    /// Ancestors of `pid` (its parent first), from a process table.
    pub fn ancestors(table: &[ProcInfo], pid: u32) -> Vec<ProcInfo> {
        let by_pid: std::collections::HashMap<u32, &ProcInfo> =
            table.iter().map(|p| (p.pid, p)).collect();
        let mut out = Vec::new();
        let mut cur = by_pid.get(&pid).map(|p| p.ppid);
        while let Some(ppid) = cur {
            if ppid <= 1 || out.len() >= 64 {
                break;
            }
            match by_pid.get(&ppid) {
                Some(p) => {
                    out.push((*p).clone());
                    cur = Some(p.ppid);
                }
                None => break,
            }
        }
        out
    }

    /// Parse `ps -A -o pid=,ppid=,lstart=,comm=` output. `lstart` is five
    /// whitespace-separated fields; the command is the rest of the line.
    pub fn parse_ps(text: &str) -> Vec<ProcInfo> {
        text.lines()
            .filter_map(|line| {
                let mut it = line.split_whitespace();
                let pid = it.next()?.parse().ok()?;
                let ppid = it.next()?.parse().ok()?;
                let start: Vec<&str> = it.by_ref().take(5).collect();
                if start.len() != 5 {
                    return None;
                }
                let comm = it.collect::<Vec<_>>().join(" ");
                Some(ProcInfo {
                    pid,
                    ppid,
                    start: start.join(" "),
                    comm,
                })
            })
            .collect()
    }

    /// The session key for this process, or None when the process table
    /// cannot be read (no `ps`, non-Unix) or no session process is found.
    pub fn current_session_key() -> Option<String> {
        if !cfg!(unix) {
            return None;
        }
        let out = std::process::Command::new("ps")
            .args(["-A", "-o", "pid=,ppid=,lstart=,comm="])
            .env("LC_ALL", "C")
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let table = parse_ps(&String::from_utf8_lossy(&out.stdout));
        let chain = ancestors(&table, std::process::id());
        session_process(&chain).map(session_key)
    }
}

fn read_session_id(path: &std::path::Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s.trim().to_string()).filter(|s| !s.is_empty())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Four hex characters derived from `input` (stable within a build).
fn short_hash(input: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:04x}", hasher.finish() & 0xFFFF)
}

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

    // ── S.3 identity resolver ───────────────────────────────────────

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn resolver_priority_explicit_env_setting_framework_default() {
        let all = env_of(&[("WRIT_AGENT_ID", "envy"), ("CLAUDE_CODE_SESSION_ID", "s1")]);
        let r = resolve_agent_id_with(Some("flag"), Some("cfg"), true, &all);
        assert_eq!(
            (r.id.as_str(), r.source),
            ("flag", IdentitySource::Explicit)
        );
        let r = resolve_agent_id_with(None, Some("cfg"), true, &all);
        assert_eq!((r.id.as_str(), r.source), ("envy", IdentitySource::Env));
        let fw = env_of(&[("CLAUDE_CODE_SESSION_ID", "s1")]);
        let r = resolve_agent_id_with(None, Some("cfg"), true, &fw);
        assert_eq!((r.id.as_str(), r.source), ("cfg", IdentitySource::Setting));
        let r = resolve_agent_id_with(None, None, true, &fw);
        assert!(r.id.starts_with("claude-code-"), "{}", r.id);
        assert!(matches!(r.source, IdentitySource::Framework(_)));
        let r = resolve_agent_id_with(None, None, false, &fw);
        assert_eq!(
            (r.id.as_str(), r.source),
            ("human", IdentitySource::Default)
        );
    }

    #[test]
    fn resolver_ignores_empty_values() {
        let env = env_of(&[("WRIT_AGENT_ID", "  ")]);
        let r = resolve_agent_id_with(Some(""), Some(""), false, &env);
        assert_eq!(r.id, "human");
        assert!(!r.is_agent());
    }

    #[test]
    fn framework_id_is_stable_per_session() {
        let env = env_of(&[("CLAUDE_CODE_SESSION_ID", "abc")]);
        let a = resolve_agent_id_with(None, None, true, &env);
        let b = resolve_agent_id_with(None, None, true, &env);
        assert_eq!(a.id, b.id);
        assert!(a.is_agent());
    }
}

#[cfg(test)]
mod claudecode_id_tests {
    //! Finding 51: a CLAUDECODE-only environment is one stable agent.
    use super::*;

    #[test]
    fn claudecode_only_resolves_to_fixed_id_without_repo() {
        let env = |k: &str| (k == "CLAUDECODE").then(|| "1".to_string());
        let a = resolve_agent_id_with(None, None, true, &env);
        let b = resolve_agent_id_with(None, None, true, &env);
        assert_eq!(a.id, CLAUDECODE_FALLBACK_ID);
        assert_eq!(a, b);
        assert!(a.is_agent());
    }

    #[test]
    fn persisted_session_id_is_created_once_and_reused() {
        let dir = tempfile::tempdir().unwrap();

        let first = persisted_session_id(dir.path(), None).unwrap();
        let second = persisted_session_id(dir.path(), None).unwrap();

        assert_eq!(first, second);
        assert!(
            first.starts_with("claude-code-") && first.len() == 20,
            "{first}"
        );
        let stored = std::fs::read_to_string(dir.path().join("agents").join(CLAUDECODE_ID_FILE));
        assert_eq!(stored.unwrap(), first);
    }

    #[test]
    fn persisted_session_id_differs_between_repositories() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        assert_ne!(
            persisted_session_id(a.path(), None).unwrap(),
            persisted_session_id(b.path(), None).unwrap()
        );
    }

    #[test]
    fn empty_id_file_is_regenerated() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("agents")).unwrap();
        std::fs::write(dir.path().join("agents").join(CLAUDECODE_ID_FILE), "  \n").unwrap();

        let id = persisted_session_id(dir.path(), None).unwrap();

        assert!(id.starts_with("claude-code-"), "{id}");
        assert_eq!(persisted_session_id(dir.path(), None).unwrap(), id);
    }
}

#[cfg(test)]
mod session_tests {
    //! Finding 57: two CLAUDECODE-only sessions never share an id.
    use super::session::*;
    use super::*;

    fn p(pid: u32, ppid: u32, start: &str, comm: &str) -> ProcInfo {
        ProcInfo {
            pid,
            ppid,
            start: start.into(),
            comm: comm.into(),
        }
    }

    const PS: &str = "    1     0 Sat Jul  4 11:10:49 2026     /sbin/launchd
  100     1 Mon Oct  5 09:00:00 2026     /opt/claude/bin/claude
  200   100 Mon Oct  5 10:00:00 2026     /bin/zsh
  300   200 Mon Oct  5 10:00:01 2026     /usr/local/bin/writ
  110     1 Mon Oct  5 09:30:00 2026     /opt/claude/bin/claude
  210   110 Mon Oct  5 10:05:00 2026     -zsh
  310   210 Mon Oct  5 10:05:01 2026     /usr/local/bin/writ
  400     1 Mon Oct  5 09:00:00 2026     node
  410   400 Mon Oct  5 10:00:00 2026     writ
  420   410 Mon Oct  5 10:00:01 2026     /path with space/writ
";

    #[test]
    fn parse_ps_keeps_lstart_and_commands_with_spaces() {
        let t = parse_ps(PS);
        assert_eq!(t.len(), 10);
        assert_eq!(t[1].start, "Mon Oct 5 09:00:00 2026");
        assert_eq!(t[9].comm, "/path with space/writ");
    }

    #[test]
    fn two_fake_claude_parents_get_distinct_keys_one_session_is_stable() {
        let t = parse_ps(PS);
        let a = session_process(&ancestors(&t, 300)).cloned().unwrap();
        let b = session_process(&ancestors(&t, 310)).cloned().unwrap();
        assert_eq!(a.pid, 100);
        assert_eq!(b.pid, 110);
        assert_ne!(session_key(&a), session_key(&b));
        // Another invocation in session A through a new shell: same key.
        let mut t2 = t.clone();
        t2.push(p(250, 100, "Mon Oct  5 10:09:00 2026", "/bin/bash"));
        t2.push(p(350, 250, "Mon Oct  5 10:09:01 2026", "writ"));
        let a2 = session_process(&ancestors(&t2, 350)).cloned().unwrap();
        assert_eq!(session_key(&a2), session_key(&a));
    }

    #[test]
    fn mcp_server_and_shell_calls_resolve_to_the_same_non_shell_session() {
        // node (Claude Code) -> writ mcp-serve -> writ CLI
        let t = parse_ps(PS);
        let s = session_process(&ancestors(&t, 420)).cloned().unwrap();
        assert_eq!(s.pid, 400);
    }

    #[test]
    fn recycled_pid_with_new_start_time_is_a_new_session() {
        let old = p(100, 1, "Mon Oct  5 09:00:00 2026", "claude");
        let new = p(100, 1, "Mon Oct  5 12:00:00 2026", "claude");
        assert_ne!(session_key(&old), session_key(&new));
    }

    #[test]
    fn keyed_ids_are_stable_per_key_and_distinct_across_keys() {
        let dir = tempfile::tempdir().unwrap();
        let a1 = persisted_session_id(dir.path(), Some("aaaa")).unwrap();
        let a2 = persisted_session_id(dir.path(), Some("aaaa")).unwrap();
        let b = persisted_session_id(dir.path(), Some("bbbb")).unwrap();
        assert_eq!(a1, a2);
        assert_ne!(a1, b);
        assert!(dir.path().join("agents/claudecode/aaaa.id").is_file());
    }
}
