//! Auto seal through Claude Code hooks (sprint 6, 0.4.3).
//!
//! `writ init` registers one hidden command, `writ hook`, on PreToolUse
//! (Bash), PostToolUse and PostToolUseFailure (the edit tools and Bash),
//! UserPromptSubmit, Stop, SubagentStop and SessionEnd.
//! Claude Code hands it the event as JSON on stdin; [`run`] dispatches on
//! `hook_event_name`.
//!
//! Rules (sprint 6 plan, binding):
//! - A hook never fails the agent's tool call: the CLI always exits 0 and
//!   every problem is one JSON line in `.writ/hooks.log`.
//! - A hook seals only the path the agent itself just edited (or, at Stop,
//!   a path the same identity edited earlier), under that identity's spec.
//! - Auto seal shrinks the unsealed window to the hook's latency; it does
//!   not make an overwrite between an edit and its seal impossible.
//! - On success the hook prints nothing. A refused auto seal (the edit drops
//!   lines another open spec sealed, or the spec's own sealed lines) is told
//!   to the agent once, as one line of `additionalContext` (finding 141:
//!   exit 0 with `hookSpecificOutput.additionalContext` reaches the model,
//!   main agent and subagent alike).
//!
//! Identity (finding 131): a subagent is visible only in the hook's stdin
//! (`agent_id`, `agent_type`); its environment is the parent's. The main
//! agent's identity is exactly what its own `writ` commands resolve to (the
//! same `CLAUDE_CODE_SESSION_ID`, finding 135); a subagent's is
//! `<parent id>-<first 8 of agent_id>`.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{WritError, WritResult};
use crate::seal::{AgentIdentity, AgentType, TaskStatus, Verification};
use crate::spec::{AutoSpec, LifecycleState, Spec, SpecStatus};
use crate::Repository;

/// The hidden CLI subcommand every writ Claude Code hook runs.
pub const HOOK_SUBCOMMAND: &str = "hook";

/// Tools whose PostToolUse writ seals. MultiEdit is absent from Claude Code
/// 2.1.290 (finding 132) and kept for older hosts.
pub const EDIT_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// The PostToolUse matcher writ installs: [`EDIT_TOOLS`] as an alternation.
pub const EDIT_TOOLS_MATCHER: &str = "Edit|Write|MultiEdit|NotebookEdit";

/// The PostToolUse matcher writ installs: the edit tools and Bash
/// (finding 153: under bypass permissions agents edit through Bash).
pub const POST_TOOL_MATCHER: &str = "Edit|Write|MultiEdit|NotebookEdit|Bash";

/// Log of every hook firing, under `.writ`.
pub const HOOKS_LOG: &str = "hooks.log";

/// The event after a tool call that succeeded.
const POST_EVENT: &str = "PostToolUse";

/// Finding 186: the event after a tool call that failed (a Bash exit 1).
/// Same input fields as PostToolUse; dispatched the same way, since a
/// command can write files and then fail.
pub const POST_FAILURE_EVENT: &str = "PostToolUseFailure";

/// Per-identity state under `.writ` (touched paths, refusals told).
const STATE_DIR: &str = "agents/claudecode";

/// Prefix of every auto seal's summary and auto spec's id.
pub const AUTO_PREFIX: &str = "auto:";
pub const AUTO_SPEC_PREFIX: &str = "auto-";

/// Ruling 149: the hook waits at most this long for the repository lock
/// (default; `[hooks] lock_wait_ms` in `.writ/config.toml` overrides it,
/// finding 151); on timeout the path stays on the identity's touched list
/// as pending and the next hook of the session (or Stop) seals it.
pub const HOOK_LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(200);

/// The configured lock wait. Reads `config.toml` only (never migrates or
/// writes); unreadable or absent means the default.
pub fn hook_lock_wait(writ_dir: &Path) -> std::time::Duration {
    fs::read_to_string(writ_dir.join("config.toml"))
        .ok()
        .and_then(|t| toml::from_str::<crate::config::ProjectConfig>(&t).ok())
        .and_then(|c| c.hooks?.lock_wait_ms)
        .map(std::time::Duration::from_millis)
        .unwrap_or(HOOK_LOCK_WAIT)
}

/// SessionEnd waits longer: the session is over and nobody is blocked; it
/// drains every pending path of the session before closing its auto specs.
pub const SESSION_END_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Prompt text that is a background subagent's completion, not a task
/// (finding 133).
const TASK_NOTIFICATION: &str = "<task-notification>";

/// The fields of Claude Code's hook stdin writ reads (live shape, 2.1.290).
#[derive(Debug, Default, Deserialize)]
pub struct HookInput {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub hook_event_name: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<Value>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
}

impl HookInput {
    /// The edited file: `tool_input.file_path`, or `notebook_path` for
    /// NotebookEdit (finding 132).
    pub fn edited_path(&self) -> Option<String> {
        let ti = self.tool_input.as_ref()?;
        ["file_path", "notebook_path"]
            .iter()
            .find_map(|k| ti.get(*k).and_then(Value::as_str))
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }
}

/// The acting identity for one hook firing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookIdentity {
    /// writ agent id.
    pub id: String,
    /// For a subagent: the parent's id.
    pub parent: Option<String>,
    /// Host session id (`session_id`), when given.
    pub session: Option<String>,
}

/// What the CLI prints to stdout (None: nothing). Exit status is always 0.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct HookOutcome {
    pub stdout: Option<String>,
}

/// Run one hook firing. Never returns an error: problems are logged to
/// `.writ/hooks.log` when a `.writ` directory can be found.
pub fn run(cwd: &Path, stdin: &str) -> HookOutcome {
    let started = Instant::now();
    let Some(writ_dir) = find_writ_dir(cwd) else {
        return HookOutcome::default();
    };
    let mut log = HookLog::new(&writ_dir, started);
    let input: HookInput = match serde_json::from_str(stdin.trim()) {
        Ok(v) => v,
        Err(e) => {
            log.write(json!({"event": null, "outcome": "error",
                "detail": format!("hook stdin is not JSON ({e})")}));
            return HookOutcome::default();
        }
    };
    let event = input.hook_event_name.clone().unwrap_or_default();
    log.failure = event == POST_FAILURE_EVENT;
    let result = match event.as_str() {
        "PreToolUse" => bash_pre(&writ_dir, &input, &log).map(|_| None),
        POST_EVENT | POST_FAILURE_EVENT => post_tool_use(&writ_dir, &input, &log),
        "UserPromptSubmit" => stash_prompt(&writ_dir, &input, &log).map(|_| None),
        "Stop" => sweep(&writ_dir, &input, &log).map(|_| None),
        "SubagentStop" => {
            let r = sweep(&writ_dir, &input, &log).map(|_| None);
            release_named_subagent(&writ_dir, &input);
            r
        }
        "SessionEnd" => session_end(&writ_dir, &input, stdin, &log).map(|_| None),
        other => {
            log.write(json!({"event": other, "outcome": "skipped",
                "detail": "event writ does not handle"}));
            Ok(None)
        }
    };
    match result {
        Ok(stdout) => HookOutcome { stdout },
        Err(e) => {
            log.write(json!({"event": event, "outcome": "error", "detail": e.to_string()}));
            HookOutcome::default()
        }
    }
}

fn find_writ_dir(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .map(|d| d.join(".writ"))
        .find(|d| d.is_dir())
}

// ---------------------------------------------------------------------------
// hooks.log
// ---------------------------------------------------------------------------

/// One JSON line per firing: `ts`, `event`, `outcome` (sealed, refused,
/// unchanged, skipped, error, closed, stashed, recorded), `ms` (hook time
/// so far), plus event fields. Bri's collector reads it for the unsealed
/// window (`edit_to_seal_ms`: file mtime to seal written).
struct HookLog {
    path: PathBuf,
    started: Instant,
    /// Finding 186: this firing is a PostToolUseFailure; its records say so
    /// (the PostToolUse code path writes them).
    failure: bool,
}

impl HookLog {
    fn new(writ_dir: &Path, started: Instant) -> Self {
        Self {
            path: writ_dir.join(HOOKS_LOG),
            started,
            failure: false,
        }
    }

    fn write(&self, mut entry: Value) {
        if let Some(m) = entry.as_object_mut() {
            if self.failure && m.get("event").and_then(Value::as_str) == Some(POST_EVENT) {
                m.insert("event".into(), json!(POST_FAILURE_EVENT));
            }
            m.insert(
                "ts".into(),
                json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
            );
            m.insert(
                "ms".into(),
                json!(self.started.elapsed().as_millis() as u64),
            );
        }
        // Best-effort by design: the log is the error channel of last resort.
        // Finding 154: one write() per record on an O_APPEND descriptor, so
        // concurrent hooks never interleave bytes within a line.
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let line = format!("{entry}\n");
            let _ = f.write_all(line.as_bytes());
        }
    }
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Resolve the hook's identity: the main agent's is what its own `writ`
/// commands resolve to in this environment (findings 131, 135); with no
/// agent signal at all, the host session id stands in (`claude-code-xxxx`,
/// the form `CLAUDE_CODE_SESSION_ID` gives). A subagent's is
/// `<parent>-<first 8 of agent_id>`, except a named custom subagent's
/// (ruling 192, [`named_subagent`]): its type name.
pub fn identity(writ_dir: &Path, default_agent: Option<&str>, input: &HookInput) -> HookIdentity {
    let mut main = crate::agent::resolve_agent_id_in(None, default_agent, true, Some(writ_dir));
    if !main.is_agent() {
        if let Some(sid) = input.session_id.as_deref().filter(|s| !s.is_empty()) {
            main = crate::agent::resolve_agent_id_with(None, None, true, &|k| {
                (k == "CLAUDE_CODE_SESSION_ID").then(|| sid.to_string())
            });
        }
    }
    let session = input.session_id.clone().filter(|s| !s.is_empty());
    match subagent_suffix(input) {
        Some(sfx) => HookIdentity {
            id: named_subagent(writ_dir, input, &sfx)
                .unwrap_or_else(|| format!("{}-{sfx}", main.id)),
            parent: Some(main.id),
            session,
        },
        None => HookIdentity {
            id: main.id,
            parent: None,
            session,
        },
    }
}

/// First 8 characters of the host's `agent_id`, when the hook fired inside
/// a subagent.
fn subagent_suffix(input: &HookInput) -> Option<String> {
    let id = input.agent_id.as_deref()?.trim();
    let clean: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect();
    (!clean.is_empty()).then_some(clean)
}

/// Claude Code's built-in subagent types: never a named identity, even when
/// a file of the same name exists (ruling 192).
const BUILTIN_AGENT_TYPES: &[&str] = &[
    "general-purpose",
    "Explore",
    "Plan",
    "statusline-setup",
    "output-style-setup",
    "claude-code-guide",
];

/// Live named subagent instances, one file per type, under `.writ`.
const LIVE_DIR: &str = "agents/claudecode/live";

/// One live instance of a named subagent type.
#[derive(Debug, serde::Serialize, Deserialize)]
struct LiveInstance {
    /// The identity it was given (the type name, or `<type>-<agent8>`).
    name: String,
    #[serde(default)]
    session: Option<String>,
    /// Its latest hook.
    last: chrono::DateTime<chrono::Utc>,
}

/// The custom agent type named by the hook's `agent_type`, when it is a
/// file under `.claude/agents/` and not a built-in type.
fn custom_agent_type(writ_dir: &Path, input: &HookInput) -> Option<String> {
    let ty = input.agent_type.as_deref()?.trim();
    let safe = !ty.is_empty()
        && ty.len() <= 64
        && ty
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && ty != "human";
    if !safe || BUILTIN_AGENT_TYPES.contains(&ty) {
        return None;
    }
    let root = writ_dir.parent()?;
    root.join(".claude")
        .join("agents")
        .join(format!("{ty}.md"))
        .is_file()
        .then(|| ty.to_string())
}

/// Ruling 192: a named custom subagent's identity is its type name, so its
/// seals carry the name (not a hash) and the spec it claimed under that
/// name is honoured. `<type>-<agent8>` only while another live instance of
/// the same type already holds the plain name. An instance is live from
/// its first hook to its SubagentStop (or [`WINDOW_STALE_SECS`] without a
/// hook); the name an instance got is kept for its life. None for any
/// other subagent (the 0.4.3 parent-derived identity, finding 131).
fn named_subagent(writ_dir: &Path, input: &HookInput, sfx: &str) -> Option<String> {
    let ty = custom_agent_type(writ_dir, input)?;
    let agent_id = input.agent_id.as_deref()?.trim().to_string();
    let path = writ_dir.join(LIVE_DIR).join(format!("{ty}.json"));
    let _lock = LiveLock::acquire(&path);
    let mut live = load_live(&path);
    let now = chrono::Utc::now();
    live.retain(|_, i| (now - i.last).num_seconds() < WINDOW_STALE_SECS);
    let name = match live.get(&agent_id) {
        Some(i) => i.name.clone(),
        None if live.values().any(|i| i.name == ty) => format!("{ty}-{sfx}"),
        None => ty.clone(),
    };
    live.insert(
        agent_id,
        LiveInstance {
            name: name.clone(),
            session: input.session_id.clone(),
            last: now,
        },
    );
    save_live(&path, &live);
    Some(name)
}

/// SubagentStop: the instance is no longer live.
fn release_named_subagent(writ_dir: &Path, input: &HookInput) {
    let (Some(ty), Some(agent_id)) = (
        custom_agent_type(writ_dir, input),
        input.agent_id.as_deref(),
    ) else {
        return;
    };
    let path = writ_dir.join(LIVE_DIR).join(format!("{ty}.json"));
    let _lock = LiveLock::acquire(&path);
    let mut live = load_live(&path);
    if live.remove(agent_id.trim()).is_some() {
        save_live(&path, &live);
    }
}

fn load_live(path: &Path) -> BTreeMap<String, LiveInstance> {
    fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_live(path: &Path, live: &BTreeMap<String, LiveInstance>) {
    if let Some(d) = path.parent() {
        let _ = fs::create_dir_all(d);
    }
    if let Ok(json) = serde_json::to_string(live) {
        let _ = crate::fsutil::atomic_write(path, json.as_bytes());
    }
}

/// A short exclusive lock on one type's live file, so two instances that
/// start together cannot both take the plain name. Best effort: after
/// about 100 ms (or a lock older than 2 s, a crashed hook) the hook goes
/// on without it rather than delay the agent's tool call.
struct LiveLock(Option<PathBuf>);

impl LiveLock {
    fn acquire(path: &Path) -> Self {
        let lock = path.with_extension("lock");
        if let Some(d) = lock.parent() {
            let _ = fs::create_dir_all(d);
        }
        for _ in 0..20 {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock)
            {
                Ok(_) => return Self(Some(lock)),
                Err(_) => {
                    let old = fs::metadata(&lock)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|d| d.as_secs() >= 2);
                    if old {
                        let _ = fs::remove_file(&lock);
                    } else {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                }
            }
        }
        Self(None)
    }
}

impl Drop for LiveLock {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            let _ = fs::remove_file(p);
        }
    }
}

fn agent_identity(id: &str) -> AgentIdentity {
    AgentIdentity {
        id: id.to_string(),
        agent_type: if id == "human" {
            AgentType::Human
        } else {
            AgentType::Agent
        },
    }
}

/// Short stable key for file names and auto spec ids.
fn key8(s: &str) -> String {
    crate::hash::hash_str(s).chars().take(8).collect()
}

// ---------------------------------------------------------------------------
// PostToolUse
// ---------------------------------------------------------------------------

fn post_tool_use(writ_dir: &Path, input: &HookInput, log: &HookLog) -> WritResult<Option<String>> {
    let tool = input.tool_name.clone().unwrap_or_default();
    if tool == "Bash" {
        return bash_post(writ_dir, input, log);
    }
    if !EDIT_TOOLS.contains(&tool.as_str()) {
        log.write(
            json!({"event": "PostToolUse", "tool": tool, "outcome": "skipped",
            "detail": "not an edit tool"}),
        );
        return Ok(None);
    }
    let Some(abs) = input.edited_path() else {
        log.write(
            json!({"event": "PostToolUse", "tool": tool, "outcome": "error",
            "detail": "no tool_input.file_path or notebook_path"}),
        );
        return Ok(None);
    };
    let root = writ_dir.parent().unwrap_or(writ_dir).to_path_buf();
    let Some(rel) = relative_path(&root, input.cwd.as_deref(), &abs) else {
        log.write(
            json!({"event": "PostToolUse", "tool": tool, "path": abs, "outcome": "skipped",
            "detail": "path is outside the repository"}),
        );
        return Ok(None);
    };
    if !root.join(&rel).exists() {
        log.write(
            json!({"event": "PostToolUse", "tool": tool, "path": rel, "outcome": "skipped",
            "detail": "file is not on disk"}),
        );
        return Ok(None);
    }
    let who = identity_for(writ_dir, input);
    let tell = seal_for(writ_dir, input, &who, &tool, &[rel], &[], log)?;
    Ok(tell.map(|msg| additional_context(input, &msg)))
}

/// Seal `paths` (this identity's own edit) under its spec, after draining
/// what it left pending (ruling 149). `by_text` are further paths of the
/// same call that only the command text decided were this identity's
/// (ruling 189); they go into their own seal, recorded as
/// `attributed_by: command_text`, so every seal says how writ decided.
/// Returns the one line for the agent.
fn seal_for(
    writ_dir: &Path,
    input: &HookInput,
    who: &HookIdentity,
    tool: &str,
    paths: &[String],
    by_text: &[String],
    log: &HookLog,
) -> WritResult<Option<String>> {
    let all: Vec<String> = paths.iter().chain(by_text).cloned().collect();
    let Some(first) = all.first() else {
        return Ok(None);
    };
    let root = writ_dir.parent().unwrap_or(writ_dir).to_path_buf();
    let mut touched = Touched::load(writ_dir, who);
    let repo = match open_checked(&root, hook_lock_wait(writ_dir)) {
        Ok(r) => r,
        Err(WritError::LockTimeout) => {
            lock_timeout(&mut touched, &all, tool, who, log);
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    let spec = spec_for(&repo, who, input, first)?;
    // Ruling 149 (2): seal what this identity left pending first (a lock
    // timeout, a refusal whose content has since changed, a Bash edit of a
    // touched file), then the new paths.
    let due: Vec<String> = touched
        .due(repo.root())
        .into_iter()
        .filter(|p| !all.contains(p))
        .collect();
    let drain = drain_mine(writ_dir, &root, who, due, &mut touched, log);
    // Retry first, then the call's own paths, then its command-text paths;
    // each batch is one seal. On a lock timeout the batches not yet sealed
    // are marked pending (a timed-out batch marks its own).
    let batches: [(&str, &[String], Option<&str>); 3] = [
        ("retry", &drain, None),
        (tool, paths, None),
        (tool, by_text, Some(crate::seal::ATTRIBUTED_BY_COMMAND_TEXT)),
    ];
    let mut tells: [Option<String>; 3] = Default::default();
    for (i, (label, batch, by)) in batches.iter().enumerate() {
        if batch.is_empty() {
            continue;
        }
        let r = seal_paths_auto(&repo, who, &spec, label, batch, *by, &mut touched, log)?;
        if r.lock_timed_out {
            let rest: Vec<String> = batches[i + 1..]
                .iter()
                .flat_map(|(_, b, _)| b.iter().cloned())
                .collect();
            lock_timeout(&mut touched, &rest, tool, who, log);
            tells[i] = r.tell;
            break;
        }
        tells[i] = r.tell;
    }
    // This call's lines first, a retry's after them (as before).
    let [retry, own, text] = tells;
    let lines: Vec<String> = [own, text, retry].into_iter().flatten().collect();
    let tell = (!lines.is_empty()).then(|| lines.join("; also: "));
    let moves = move_notices(&repo, who, log);
    Ok(match (tell, moves) {
        (t, None) => t,
        (None, m) => m,
        (Some(t), Some(m)) => Some(format!("{t}\n{m}")),
    })
}

/// The time of this identity's earliest seal, from the specs it claimed or
/// created (read only).
fn first_own_seal(repo: &Repository, agent: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let specs = repo.list_specs().ok()?;
    specs
        .iter()
        .filter(|s| {
            s.claimed_by.as_deref() == Some(agent) || s.created_by.as_deref() == Some(agent)
        })
        .flat_map(|s| s.sealed_by.iter())
        .filter_map(|id| repo.load_seal(id).ok())
        .filter(|s| s.agent.id == agent)
        .map(|s| s.timestamp)
        .min()
}

/// Ruling 177: tell an identity, once, that another agent moved its sealed
/// lines whole into another path (nothing lost). Only seals newer than the
/// last check are diffed; the instant is kept on the identity's touched
/// record. One line per move, joined by newlines.
fn move_notices(repo: &Repository, who: &HookIdentity, log: &HookLog) -> Option<String> {
    let mut touched = Touched::load(repo.writ_dir(), who);
    let checked_at = chrono::Utc::now();
    // First check for this identity: only seals after its own first seal
    // can move its lines; with no seal of its own there is nothing to tell
    // (a full-history diff took 6 s on writ's own store).
    let after = match touched.file.moves_after {
        Some(t) => Some(t),
        None => match first_own_seal(repo, &who.id) {
            Some(t) => Some(t),
            None => {
                touched.file.moves_after = Some(checked_at);
                touched.save();
                return None;
            }
        },
    };
    let moves = match crate::convergence::survival::moves_of_agents_lines(repo, &who.id, after) {
        Ok(m) => m,
        Err(e) => {
            log.write(
                json!({"event": "PostToolUse", "agent": who.id, "outcome": "error",
                "detail": format!("move check: {e}")}),
            );
            return None;
        }
    };
    touched.file.moves_after = Some(checked_at);
    touched.save();
    if moves.is_empty() {
        return None;
    }
    let lines: Vec<String> = moves
        .iter()
        .map(crate::convergence::survival::move_notice)
        .collect();
    log.write(
        json!({"event": "PostToolUse", "agent": who.id, "outcome": "moves_told",
        "seals": moves.iter().map(|m| m.seal.clone()).collect::<Vec<_>>()}),
    );
    Some(lines.join("\n"))
}

/// Finding 173: a queued path is sealed by a drain only when the latest
/// write of it is this identity's, by the same rule as a fresh Bash Post
/// (ruling 160): every Bash interval (any identity, open or closed) that
/// contains the file's mtime is a candidate. Only this identity, or none
/// (an Edit-tool edit, a formatter, a lock-timed-out edit): the drain seals
/// it. Another identity among them, or a recorded ambiguity on the current
/// content: not this identity's to seal; the mark is set to the current
/// content so it is not retried until the file changes again.
fn drain_mine(
    writ_dir: &Path,
    root: &Path,
    who: &HookIdentity,
    due: Vec<String>,
    touched: &mut Touched,
    log: &HookLog,
) -> Vec<String> {
    if due.is_empty() {
        return due;
    }
    let mut windows = other_windows(writ_dir, &who.id);
    windows.extend(own_all_windows(writ_dir, &who.id));
    let now_ns = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0) as i128;
    let mut out = Vec::new();
    for rel in due {
        let abs = root.join(&rel);
        if let Some(c) = ambiguous_candidates(writ_dir, &rel) {
            log.write(json!({"event": "drain", "path": rel, "agent": who.id,
                "outcome": "skipped", "agents": c,
                "detail": "the current content is recorded as ambiguous; sealed only by a candidate's own fix"}));
            touched.mark_refused(&rel, content_hash(&abs));
            continue;
        }
        let mtime = fs::metadata(&abs).ok().and_then(|m| {
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i128)
        });
        let others: Vec<String> = match mtime {
            Some(t) => {
                let coarse = t % NS_PER_SEC == 0;
                let hi = if coarse { t + NS_PER_SEC - 1 } else { t };
                let mut v: Vec<String> = windows
                    .iter()
                    .filter(|w| w.agent != who.id && w.start_ns() <= hi && t <= w.end_ns(now_ns))
                    .map(|w| w.agent.clone())
                    .collect();
                v.sort();
                v.dedup();
                v
            }
            None => Vec::new(),
        };
        if others.is_empty() {
            out.push(rel);
        } else {
            log.write(json!({"event": "drain", "path": rel, "agent": who.id,
                "outcome": "skipped", "agents": others,
                "detail": "the latest write falls in another agent's command; not this agent's to seal"}));
            touched.mark_refused(&rel, content_hash(&abs));
        }
    }
    out
}

/// This identity's Bash windows, open or closed (drain attribution).
fn own_all_windows(writ_dir: &Path, agent: &str) -> Vec<BashWindow> {
    let Ok(rd) = fs::read_dir(writ_dir.join(BASH_DIR)) else {
        return Vec::new();
    };
    rd.flatten()
        .filter_map(|e| {
            serde_json::from_str::<BashWindow>(&fs::read_to_string(e.path()).ok()?).ok()
        })
        .filter(|w| w.agent == agent)
        .collect()
}

/// Open the repository for a hook with a bounded lock wait and the
/// ruling 181 checks on.
fn open_checked(root: &Path, wait: std::time::Duration) -> WritResult<Repository> {
    let mut r = Repository::open_bounded(root, wait)?;
    r.enable_survival_checks();
    Ok(r)
}

/// The identity, with `default_agent` read without opening the repository
/// (opening takes the lock).
fn identity_for(writ_dir: &Path, input: &HookInput) -> HookIdentity {
    let settings = crate::settings::WritSettings::load(writ_dir).unwrap_or_default();
    identity(writ_dir, settings.default_agent.as_deref(), input)
}

/// Ruling 149 (1, 3): on a lock timeout the paths are marked pending and
/// logged once per path; nothing reaches the agent.
fn lock_timeout(
    touched: &mut Touched,
    paths: &[String],
    tool: &str,
    who: &HookIdentity,
    log: &HookLog,
) {
    for rel in paths {
        if touched.mark_pending(rel) {
            log.write(json!({"event": "PostToolUse", "tool": tool, "path": rel,
                "agent": who.id, "outcome": "lock_timeout",
                "detail": "repository lock busy; the path is sealed by this session's next hook or Stop"}));
        }
    }
    touched.save();
}

/// The edited path relative to the repository root, or None when outside.
/// Both sides are canonicalized (macOS `/tmp` is `/private/tmp`).
fn relative_path(root: &Path, cwd: Option<&str>, path: &str) -> Option<String> {
    let p = Path::new(path);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        Path::new(cwd.unwrap_or(".")).join(p)
    };
    let canon = |x: &Path| -> PathBuf {
        x.canonicalize().unwrap_or_else(|_| {
            // A file that is gone: canonicalize its directory.
            match (x.parent(), x.file_name()) {
                (Some(d), Some(n)) => d
                    .canonicalize()
                    .map(|d| d.join(n))
                    .unwrap_or_else(|_| x.to_path_buf()),
                _ => x.to_path_buf(),
            }
        })
    };
    let rel = canon(&abs).strip_prefix(canon(root)).ok()?.to_path_buf();
    let s = rel.to_string_lossy().replace('\\', "/");
    (!s.is_empty() && !s.starts_with(".writ/") && s != ".writ").then_some(s)
}

/// The hook's stdout for a message to the agent (finding 141), answering
/// the event that fired (PostToolUse or PostToolUseFailure).
fn additional_context(input: &HookInput, msg: &str) -> String {
    let event = match input.hook_event_name.as_deref() {
        Some(POST_FAILURE_EVENT) => POST_FAILURE_EVENT,
        _ => POST_EVENT,
    };
    json!({"hookSpecificOutput": {
        "hookEventName": event,
        "additionalContext": msg,
    }})
    .to_string()
}

/// Seal labels of a late window close (an identity's next PreToolUse or
/// UserPromptSubmit closed a window whose Post never came).
const LATE_PRE: &str = "PreToolUse";
const LATE_PROMPT: &str = "UserPromptSubmit";

/// A label for a seal that did not come from the hook seeing this
/// identity's own tool call just now (a retry, a Stop or SessionEnd drain,
/// a late window close): finding 173, never the 144 own-rewrite path.
fn is_drain(tool: &str) -> bool {
    matches!(
        tool,
        "retry" | "Stop" | "SessionEnd" | LATE_PRE | LATE_PROMPT
    )
}

/// Result of an auto seal attempt.
#[derive(Default)]
struct SealAttempt {
    /// One line for the agent (a refusal not told before), if any.
    tell: Option<String>,
    /// The repository lock was busy; the paths are marked pending.
    lock_timed_out: bool,
}

/// Auto seal `paths` for `who` under `spec`: each path is checked against
/// other specs' sealed lines (refused paths are told once and stay on the
/// touched list), the rest go into one seal. Summary `auto: <tool> <path>`
/// for one path, `auto: <tool> <n> paths: ...` for several.
#[allow(clippy::too_many_arguments)]
fn seal_paths_auto(
    repo: &Repository,
    who: &HookIdentity,
    spec: &str,
    tool: &str,
    paths: &[String],
    attributed_by: Option<&str>,
    touched: &mut Touched,
    log: &HookLog,
) -> WritResult<SealAttempt> {
    let writ_dir = repo.writ_dir();
    let event = match tool {
        "Stop" | "SessionEnd" | LATE_PRE | LATE_PROMPT => tool,
        _ => POST_EVENT,
    };
    // Ruling 189: a command-text batch is sealed through its own handle so
    // the seal records how writ decided.
    let tagged;
    let repo = match attributed_by {
        Some(by) => {
            let mut r = open_checked(repo.root(), hook_lock_wait(writ_dir))?;
            r.set_attributed_by(Some(by));
            tagged = r;
            &tagged
        }
        None => repo,
    };

    let entry = |rel: &str, extra: Value| -> Value {
        let mut v =
            json!({"event": event, "tool": tool, "path": rel, "agent": who.id, "spec": spec});
        if let (Some(m), Some(x)) = (v.as_object_mut(), extra.as_object()) {
            m.extend(x.clone());
        }
        v
    };
    let mut out = SealAttempt::default();
    let mut ok: Vec<String> = Vec::new();
    for rel in paths {
        let foreign = repo.foreign_line_losses(spec, &who.id, rel)?;
        if foreign.is_empty() {
            ok.push(rel.clone());
            continue;
        }
        // Ruling 181: the same line a hand seal's refusal prints.
        let err = WritError::ForeignLinesRemoved {
            losses: foreign
                .iter()
                .map(|(spec, by, seal, l)| {
                    (
                        spec.clone(),
                        by.clone(),
                        seal.clone(),
                        l.path.clone(),
                        l.line_count(),
                    )
                })
                .collect(),
        };
        let mut msg = err.foreign_lines_message().unwrap_or_default();
        if who.parent.is_some() {
            msg.push_str(&format!(" --agent {}", who.id));
        }
        let signature = foreign
            .iter()
            .map(|(s, _, _, l)| format!("{s}:{}", l.ranges_label()))
            .collect::<Vec<_>>()
            .join(";");
        let tell = Told::load(writ_dir, &who.id).first_time(rel, &signature);
        log.write(entry(
            rel,
            json!({"outcome": "refused", "told": tell, "detail": msg}),
        ));
        touched.mark_refused(rel, content_hash(&repo.root().join(rel)));
        if tell && out.tell.is_none() {
            out.tell = Some(msg);
        }
    }
    if ok.is_empty() {
        touched.save();
        return Ok(out);
    }
    let summary = match ok.as_slice() {
        [one] => format!("{AUTO_PREFIX} {tool} {one}"),
        many => format!(
            "{AUTO_PREFIX} {tool} {} paths: {}",
            many.len(),
            many.join(", ")
        ),
    };
    let seal = |repo: &Repository| {
        repo.seal_paths(
            agent_identity(&who.id),
            summary.clone(),
            Some(spec.to_string()),
            TaskStatus::InProgress,
            Verification::default(),
            &ok,
            false,
        )
    };
    let mut result = seal(repo);
    // Finding 144: the own-line check (finding 102) exists to tell an edit
    // from a capture of another agent's version on disk. The hook knows the
    // content is this agent's own edit, so when no other agent worked on
    // the path since the spec's last seal of it, the removal is the agent's
    // own rewrite: sealed, with the override recorded on the seal. With
    // another agent in the picture it stays refused and the agent is told.
    // Finding 173: only a fresh edit (the hook that saw this agent's own
    // tool call) may take the 144 path. A drain (retry, Stop, SessionEnd)
    // never passes allow_removals: its content may be another agent's.
    let fresh = !is_drain(tool);
    if let Err(WritError::OwnLinesRemoved { other_agents, .. }) = &result {
        if fresh && other_agents.is_empty() {
            let mut own = open_checked(repo.root(), hook_lock_wait(writ_dir))?;
            own.set_allow_removal_paths(ok.iter().cloned());
            own.set_override_by_hook(true);
            own.set_attributed_by(attributed_by);
            result = seal(&own);
        }
    }
    match result {
        Ok(seal) => {
            for rel in &ok {
                let lag = mtime_to_now_ms(&repo.root().join(rel));
                touched.mark_sealed(rel, content_hash(&repo.root().join(rel)));
                let mut e = entry(
                    rel,
                    json!({"outcome": "sealed", "seal": &seal.id[..12],
                    "edit_to_seal_ms": lag}),
                );
                if let (Some(by), Some(m)) = (attributed_by, e.as_object_mut()) {
                    m.insert("attributed_by".into(), json!(by));
                }
                log.write(e);
            }
        }
        Err(WritError::NothingToSeal) => {
            for rel in &ok {
                touched.mark_sealed(rel, content_hash(&repo.root().join(rel)));
                log.write(entry(
                    rel,
                    json!({"outcome": "unchanged",
                    "detail": "no unsealed change (identical content, or an ignored path)"}),
                ));
            }
        }
        Err(WritError::LockTimeout) => {
            touched.save();
            lock_timeout(touched, &ok, tool, who, log);
            out.lock_timed_out = true;
            return Ok(out);
        }
        Err(e @ WritError::OwnLinesRemoved { .. }) => {
            let paths = e.own_lines_paths();
            for rel in &ok {
                touched.mark_refused(rel, content_hash(&repo.root().join(rel)));
            }
            let first = paths.first().cloned().unwrap_or_else(|| ok[0].clone());
            let msg = own_refusal(tool, &first, &e);
            let tell = Told::load(writ_dir, &who.id).first_time(&first, "own");
            for rel in &ok {
                log.write(entry(
                    rel,
                    json!({"outcome": "refused", "told": tell,
                    "detail": e.to_string()}),
                ));
            }
            if tell && out.tell.is_none() {
                out.tell = Some(msg);
            }
        }
        Err(e) => {
            for rel in &ok {
                log.write(entry(
                    rel,
                    json!({"outcome": "error", "detail": e.to_string()}),
                ));
            }
        }
    }
    touched.save();
    Ok(out)
}

/// The one line for a refusal of the spec's own sealed lines (finding 102).
fn own_refusal(tool: &str, rel: &str, e: &WritError) -> String {
    let (n, others) = match e {
        WritError::OwnLinesRemoved {
            losses,
            other_agents,
            ..
        } => (
            losses.iter().map(|l| l.line_count()).sum::<usize>(),
            other_agents.clone(),
        ),
        _ => (0, Vec::new()),
    };
    let who = if others.is_empty() {
        String::new()
    } else {
        format!(" (agent {} worked on it since)", others.join(", agent "))
    };
    format!(
        "writ did not seal your {tool} of {rel}: it drops {n} line(s) your spec sealed earlier{who}. \
         Put them back and your next edit seals itself, or keep your version; \
         fix: writ seal --auto --paths {rel} --allow-removals {rel}"
    )
}

fn content_hash(path: &Path) -> Option<String> {
    fs::read(path).ok().map(|b| crate::hash::hash_bytes(&b))
}

fn mtime_to_now_ms(path: &Path) -> Option<u64> {
    let m = fs::metadata(path).ok()?.modified().ok()?;
    std::time::SystemTime::now()
        .duration_since(m)
        .ok()
        .map(|d| d.as_millis() as u64)
}

// ---------------------------------------------------------------------------
// Bash (finding 153, rulings 155 and 157)
// ---------------------------------------------------------------------------

/// Open Bash call windows under `.writ`: one file per call, written by the
/// PreToolUse hook and removed by its PostToolUse (or the identity's Stop).
const BASH_DIR: &str = "agents/claudecode/bash";

/// Another identity's window older than this is an interrupted call, not a
/// live one (its Post never came and its Stop never ran).
const WINDOW_STALE_SECS: i64 = 30 * 60;

/// One open Bash call: who, what it runs, and the tree before it.
#[derive(Debug, serde::Serialize, Deserialize)]
struct BashWindow {
    agent: String,
    #[serde(default)]
    session: Option<String>,
    tool_use_id: String,
    command: String,
    at: chrono::DateTime<chrono::Utc>,
    /// When the call's Post (or the identity's Stop) closed it; None while
    /// the call is open.
    #[serde(default)]
    end: Option<chrono::DateTime<chrono::Utc>>,
    /// The tree before the call; dropped once the window closes.
    #[serde(default)]
    sig: crate::state::TreeSignature,
}

impl BashWindow {
    fn start_ns(&self) -> i128 {
        self.at.timestamp_nanos_opt().unwrap_or(0) as i128
    }

    /// End of the interval: its close, or now for an open call.
    fn end_ns(&self, now_ns: i128) -> i128 {
        self.end
            .and_then(|e| e.timestamp_nanos_opt())
            .map(|e| e as i128)
            .unwrap_or(now_ns)
    }
}

fn window_path(writ_dir: &Path, agent: &str, tool_use_id: &str) -> PathBuf {
    writ_dir
        .join(BASH_DIR)
        .join(format!("{}-{}.json", key8(agent), key8(tool_use_id)))
}

fn bash_command(input: &HookInput) -> String {
    input
        .tool_input
        .as_ref()
        .and_then(|t| t.get("command"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn tool_use_id(input: &HookInput) -> Option<String> {
    input.tool_use_id.clone().filter(|s| !s.is_empty())
}

fn tree_sig(root: &Path) -> crate::state::TreeSignature {
    crate::state::tree_signature(root, &crate::ignore::IgnoreRules::load(root))
}

/// PreToolUse on Bash: record the tree's stat signature for this call.
/// Prints nothing; never fails the call.
fn bash_pre(writ_dir: &Path, input: &HookInput, log: &HookLog) -> WritResult<()> {
    if input.tool_name.as_deref() != Some("Bash") {
        return Ok(());
    }
    let Some(id) = tool_use_id(input) else {
        log.write(
            json!({"event": "PreToolUse", "tool": "Bash", "outcome": "error",
            "detail": "no tool_use_id"}),
        );
        return Ok(());
    };
    let root = writ_dir.parent().unwrap_or(writ_dir).to_path_buf();
    let who = identity_for(writ_dir, input);
    close_late(writ_dir, input, &who, LATE_PRE, Some(&id), log)?;
    let t = Instant::now();
    let w = BashWindow {
        agent: who.id.clone(),
        session: who.session.clone(),
        tool_use_id: id.clone(),
        command: bash_command(input),
        at: chrono::Utc::now(),
        end: None,
        sig: tree_sig(&root),
    };
    let snapshot_ms = t.elapsed().as_millis() as u64;
    let path = window_path(writ_dir, &who.id, &id);
    if let Some(d) = path.parent() {
        fs::create_dir_all(d)?;
    }
    let json =
        serde_json::to_string(&w).map_err(|e| WritError::Other(format!("JSON serialize: {e}")))?;
    crate::fsutil::atomic_write(&path, json.as_bytes())?;
    log.write(
        json!({"event": "PreToolUse", "tool": "Bash", "agent": who.id,
        "outcome": "snapshot", "files": w.sig.files.len(), "snapshot_ms": snapshot_ms}),
    );
    Ok(())
}

/// Every Bash window of another identity, open or closed within
/// [`WINDOW_STALE_SECS`] (a closed window still decides attribution for a
/// later Post whose diff saw the same write). Older closed windows are
/// pruned here.
fn other_windows(writ_dir: &Path, agent: &str) -> Vec<BashWindow> {
    let Ok(rd) = fs::read_dir(writ_dir.join(BASH_DIR)) else {
        return Vec::new();
    };
    let now = chrono::Utc::now();
    let mut out = Vec::new();
    for e in rd.flatten() {
        let Some(w) = fs::read_to_string(e.path())
            .ok()
            .and_then(|t| serde_json::from_str::<BashWindow>(&t).ok())
        else {
            continue;
        };
        let last = w.end.unwrap_or(w.at);
        if (now - last).num_seconds() >= WINDOW_STALE_SECS {
            if w.end.is_some() {
                let _ = fs::remove_file(e.path());
            }
            continue;
        }
        if w.agent != agent {
            out.push(w);
        }
    }
    out
}

/// This identity's open windows (Stop closes them).
fn own_windows(writ_dir: &Path, agent: &str) -> Vec<(PathBuf, BashWindow)> {
    let Ok(rd) = fs::read_dir(writ_dir.join(BASH_DIR)) else {
        return Vec::new();
    };
    rd.flatten()
        .filter_map(|e| {
            let w: BashWindow = serde_json::from_str(&fs::read_to_string(e.path()).ok()?).ok()?;
            (w.agent == agent && w.end.is_none()).then(|| (e.path(), w))
        })
        .collect()
}

/// Close a window: record its end, drop its tree signature.
fn close_window(path: &Path, w: BashWindow) {
    close_window_at(path, w, chrono::Utc::now());
}

/// Close a window as of `at`.
fn close_window_at(path: &Path, mut w: BashWindow, at: chrono::DateTime<chrono::Utc>) {
    w.end = Some(at);
    w.sig = crate::state::TreeSignature::default();
    if let Ok(json) = serde_json::to_string(&w) {
        let _ = crate::fsutil::atomic_write(path, json.as_bytes());
    }
}

/// Whose change a path is (ruling 160).
#[derive(Debug, PartialEq, Eq)]
enum Owner {
    /// This identity's: seal it.
    Mine,
    /// Another identity's write: never sealed here.
    Theirs,
    /// Several identities' intervals contain the write (or, with no usable
    /// mtime, overlapped this call): sealed to nobody; each is told.
    Ambiguous(Vec<String>),
}

const NS_PER_SEC: i128 = 1_000_000_000;

/// Attribute one path this call saw change (ruling 160). The write is a
/// point in time, the path's mtime (`None` for a deletion). Candidates are
/// the identities whose interval (Pre to Post, or to now while open)
/// contains it; a whole-second mtime (a filesystem that rounds) widens the
/// instant to that second, and the caller logs it. Exactly one candidate
/// owns the path; several make it ambiguous. With no candidate (mtime
/// preserved by `cp -p`, a checkout) or no mtime, this call owns it only
/// if no other identity's interval overlapped the call at all. Command
/// text is not used here; an ambiguous result goes to
/// [`command_text_owner`] (ruling 189).
fn attribute(
    mtime_ns: Option<i128>,
    mine: &BashWindow,
    now_ns: i128,
    others: &[BashWindow],
) -> (Owner, bool) {
    let mut who: Vec<String> = Vec::new();
    let coarse = mtime_ns.is_some_and(|t| t % NS_PER_SEC == 0);
    if let Some(t) = mtime_ns {
        let (lo, hi) = if coarse {
            (t, t + NS_PER_SEC - 1)
        } else {
            (t, t)
        };
        for w in std::iter::once(mine).chain(others.iter()) {
            if w.start_ns() <= hi && lo <= w.end_ns(now_ns) && !who.contains(&w.agent) {
                who.push(w.agent.clone());
            }
        }
    }
    let owner = match who.len() {
        1 if who[0] == mine.agent => Owner::Mine,
        1 => Owner::Theirs,
        0 => {
            let (s, e) = (mine.start_ns(), mine.end_ns(now_ns));
            let mut overlap: Vec<String> = Vec::new();
            for w in others {
                if w.start_ns() <= e && s <= w.end_ns(now_ns) && !overlap.contains(&w.agent) {
                    overlap.push(w.agent.clone());
                }
            }
            if overlap.is_empty() {
                Owner::Mine
            } else {
                overlap.insert(0, mine.agent.clone());
                Owner::Ambiguous(overlap)
            }
        }
        _ => Owner::Ambiguous(who),
    };
    (owner, coarse)
}

/// True when `cmd` names `needle` as a whole word: not preceded by a
/// letter, digit, `_` or `-`, not followed by a letter, digit or `_` (so
/// `app.py` does not match `myapp.py` or `app.pyc`). Anywhere in the text,
/// heredoc bodies included.
fn names_word(cmd: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    cmd.match_indices(needle).any(|(a, m)| {
        let pre = cmd[..a].chars().next_back();
        let post = cmd[a + m.len()..].chars().next();
        !pre.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-')
            && !post.is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// True when a command's text names the repository-relative path `rel`:
/// the path itself or its basename (finding 188, Bri's measurement).
fn names_path(cmd: &str, rel: &str) -> bool {
    let base = rel.rsplit('/').next().unwrap_or(rel);
    names_word(cmd, rel) || (base != rel && names_word(cmd, base))
}

/// Ruling 189, rule 1 under reading W: the one candidate whose command
/// text names `rel`, looking at every window of each candidate (open or
/// closed, the reporter's own included) that overlaps the change range,
/// from the reporting window's start to now. None when no candidate or
/// several name it, or when any such window has no command text (never
/// guess). No reader classification of any kind (rule 2 was measured
/// wrong and does not exist).
fn command_text_owner(
    rel: &str,
    candidates: &[String],
    reporter: &BashWindow,
    windows: &[BashWindow],
    now_ns: i128,
) -> Option<String> {
    let (lo, hi) = (reporter.start_ns(), now_ns);
    let mut naming: Vec<&String> = Vec::new();
    for c in candidates {
        let mut texts: Vec<&str> = Vec::new();
        for w in std::iter::once(reporter).chain(windows.iter()) {
            let dup = w.tool_use_id == reporter.tool_use_id && !std::ptr::eq(w, reporter);
            if &w.agent == c && !dup && w.start_ns() <= hi && lo <= w.end_ns(now_ns) {
                texts.push(w.command.as_str());
            }
        }
        if texts.iter().any(|t| t.trim().is_empty()) {
            return None;
        }
        if texts.iter().any(|t| names_path(t, rel)) {
            naming.push(c);
        }
    }
    match naming.as_slice() {
        [one] => Some((*one).clone()),
        _ => None,
    }
}

/// The one line for the ambiguous paths of one call (ruling 160 wording;
/// `--auto` because a hand seal needs a summary; finding 168: all paths in
/// one comma-separated fix, and `--agent <id>` for a subagent, whose shell
/// resolves to its parent).
fn ambiguous_line(paths: &[(String, Vec<String>)], agent: Option<&str>) -> String {
    let names: Vec<&str> = paths.iter().map(|(p, _)| p.as_str()).collect();
    let mut who: Vec<String> = paths.iter().flat_map(|(_, w)| w.clone()).collect();
    who.sort();
    who.dedup();
    let n = paths.iter().map(|(_, w)| w.len()).max().unwrap_or(2).max(2);
    let agent = agent.map(|a| format!(" --agent {a}")).unwrap_or_default();
    format!(
        "{} changed while {n} agents ran commands ({}); writ could not tell whose change it is; \
         if it is yours: writ seal --paths {} --auto{agent}",
        names.join(", "),
        who.join(", "),
        names.join(",")
    )
}

/// Ambiguous paths, recorded so the other identity's Post does not seal
/// what this one declined: path -> (content hash, identities told).
#[derive(Debug, Default, serde::Serialize, Deserialize)]
struct AmbiguousFile {
    #[serde(default)]
    paths: BTreeMap<String, (Option<String>, Vec<String>)>,
}

fn ambiguous_path(writ_dir: &Path) -> PathBuf {
    writ_dir.join(STATE_DIR).join("ambiguous.json")
}

fn load_ambiguous(writ_dir: &Path) -> AmbiguousFile {
    fs::read_to_string(ambiguous_path(writ_dir))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_ambiguous(writ_dir: &Path, a: &AmbiguousFile) {
    let path = ambiguous_path(writ_dir);
    if let Some(d) = path.parent() {
        let _ = fs::create_dir_all(d);
    }
    if let Ok(json) = serde_json::to_string(a) {
        let _ = crate::fsutil::atomic_write(&path, json.as_bytes());
    }
}

/// What one Bash window's diff came to (ruling 160, ruling 189).
#[derive(Default)]
struct BashVerdict {
    /// This identity's paths, decided by its window alone.
    mine: Vec<String>,
    /// This identity's paths, decided by the command text (rule 1).
    by_text: Vec<String>,
    /// Ambiguous paths to tell this identity about.
    tell: Vec<(String, Vec<String>)>,
}

/// Paths this window changed that are this identity's, and the lines to
/// tell it (ambiguous paths). Removes nothing. `event` labels the records
/// (`PostToolUse`, or the event that closed the window late, `Stop`
/// included); `shown` is false where nothing reaches the agent (a Stop
/// sweep, a late close), and every ambiguous record carries `told`, so the
/// records writ could not attribute are counted apart from the ask lines
/// an agent saw (finding 187).
fn bash_attribution(
    writ_dir: &Path,
    root: &Path,
    w: &BashWindow,
    event: &str,
    shown: bool,
    log: &HookLog,
) -> BashVerdict {
    let t = Instant::now();
    let now = tree_sig(root);
    let snapshot_ms = t.elapsed().as_millis() as u64;
    let changed = crate::state::changed_paths(&w.sig, &now);
    let now_ns = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0) as i128;
    let others = other_windows(writ_dir, &w.agent);
    let mut amb = load_ambiguous(writ_dir);
    let mut amb_changed = false;
    let mut v = BashVerdict::default();
    // Every window the command text rule may read: the others' and this
    // identity's own (open or closed).
    let mut text_windows: Vec<BashWindow> = Vec::new();
    let mut text_loaded = false;
    for rel in changed {
        let hash = content_hash(&root.join(&rel));
        // Declined earlier as ambiguous, content unchanged: still nobody's.
        if let Some((h, who)) = amb.paths.get(&rel).cloned() {
            if h == hash {
                let told = Told::load(writ_dir, &w.agent).first_time(&rel, "ambiguous") && shown;
                log.write(json!({"event": event, "tool": "Bash", "path": rel,
                    "agent": w.agent, "outcome": "ambiguous", "agents": who, "told": told,
                    "detail": "declined earlier; content unchanged"}));
                if told {
                    v.tell.push((rel.clone(), who.clone()));
                }
                continue;
            }
            amb.paths.remove(&rel);
            amb_changed = true;
        }
        let mtime = now.files.get(&rel).map(|f| f.1);
        let (owner, coarse) = attribute(mtime, w, now_ns, &others);
        let owner = match owner {
            Owner::Ambiguous(who) => {
                if !text_loaded {
                    text_windows = other_windows(writ_dir, &w.agent);
                    text_windows.extend(own_all_windows(writ_dir, &w.agent));
                    text_loaded = true;
                }
                match command_text_owner(&rel, &who, w, &text_windows, now_ns) {
                    Some(c) => {
                        log.write(json!({"event": event, "tool": "Bash", "path": rel,
                            "agent": w.agent, "outcome": "attributed_from_command",
                            "owner": c, "agents": who,
                            "attributed_by": crate::seal::ATTRIBUTED_BY_COMMAND_TEXT,
                            "detail": "exactly one candidate's command names the path"}));
                        if c == w.agent {
                            v.by_text.push(rel);
                        }
                        continue;
                    }
                    None => Owner::Ambiguous(who),
                }
            }
            o => o,
        };
        match owner {
            Owner::Mine => v.mine.push(rel),
            Owner::Theirs => log.write(json!({"event": event, "tool": "Bash",
                "path": rel, "agent": w.agent, "outcome": "skipped", "coarse_mtime": coarse,
                "detail": "another agent's write (only its interval contains the mtime)"})),
            Owner::Ambiguous(who) => {
                let told = Told::load(writ_dir, &w.agent).first_time(&rel, "ambiguous") && shown;
                log.write(json!({"event": event, "tool": "Bash", "path": rel,
                    "agent": w.agent, "outcome": "ambiguous", "agents": who,
                    "coarse_mtime": coarse, "told": told}));
                amb.paths.insert(rel.clone(), (hash, who.clone()));
                amb_changed = true;
                if told {
                    v.tell.push((rel.clone(), who));
                }
            }
        }
    }
    if amb_changed {
        save_ambiguous(writ_dir, &amb);
    }
    log.write(json!({"event": event, "tool": "Bash", "agent": w.agent,
        "outcome": "attributed", "paths": v.mine, "by_text": v.by_text,
        "snapshot_ms": snapshot_ms,
        "pre_to_attributed_ms": (chrono::Utc::now() - w.at).num_milliseconds()}));
    v
}

/// Close this identity's open Bash windows whose Post never came, as of
/// now, before a new event of the same identity (PreToolUse,
/// UserPromptSubmit, Stop). Each window's changes are attributed and
/// sealed first, exactly as the Stop sweep always did (ruling 155 (3));
/// nothing is printed (finding 187: these records are `told: false`), and
/// each close is logged `window_closed_late` with its age.
///
/// Safe because one identity's tool calls run one after another: in all
/// three 0.4.3 evidence runs (A, B, c409) every call within one assistant
/// message finished before the next began, so an identity's new event
/// means its earlier calls are over. A host that ran one identity's calls
/// in parallel would see the earlier call closed early; its later Post
/// then finds the window closed and attributes nothing (see `bash_post`),
/// and anything it wrote after the close is left for doctor's left_out
/// check, never sealed to the wrong agent. `skip` is the call now starting.
fn close_late(
    writ_dir: &Path,
    input: &HookInput,
    who: &HookIdentity,
    event: &str,
    skip: Option<&str>,
    log: &HookLog,
) -> WritResult<()> {
    let root = writ_dir.parent().unwrap_or(writ_dir).to_path_buf();
    for (path, w) in own_windows(writ_dir, &who.id) {
        if Some(w.tool_use_id.as_str()) == skip {
            continue;
        }
        let at = chrono::Utc::now();
        let v = bash_attribution(writ_dir, &root, &w, event, false, log);
        log.write(json!({"event": event, "tool": "Bash", "agent": who.id,
            "outcome": "window_closed_late", "tool_use_id": w.tool_use_id,
            "age_ms": (at - w.at).num_milliseconds()}));
        close_window_at(&path, w, at);
        if !v.mine.is_empty() || !v.by_text.is_empty() {
            seal_for(writ_dir, input, who, event, &v.mine, &v.by_text, log)?;
        }
    }
    Ok(())
}

/// PostToolUse on Bash: seal exactly the paths this call changed that are
/// this identity's (ruling 160). The unsealed window it logs is an upper
/// bound: from the Pre snapshot to the seal ("at most").
fn bash_post(writ_dir: &Path, input: &HookInput, log: &HookLog) -> WritResult<Option<String>> {
    let root = writ_dir.parent().unwrap_or(writ_dir).to_path_buf();
    let who = identity_for(writ_dir, input);
    let Some(id) = tool_use_id(input) else {
        log.write(
            json!({"event": "PostToolUse", "tool": "Bash", "outcome": "error",
            "detail": "no tool_use_id"}),
        );
        return Ok(None);
    };
    let path = window_path(writ_dir, &who.id, &id);
    let Some(w) = fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<BashWindow>(&t).ok())
    else {
        log.write(
            json!({"event": "PostToolUse", "tool": "Bash", "agent": who.id,
            "outcome": "skipped", "detail": "no PreToolUse snapshot for this call"}),
        );
        return Ok(None);
    };
    if let Some(end) = w.end {
        // Closed late by this identity's next event (or its Stop): its
        // signature is gone, and its changes up to then were attributed.
        log.write(json!({"event": POST_EVENT, "tool": "Bash", "agent": who.id,
            "outcome": "skipped", "closed_at": end.to_rfc3339(),
            "detail": "window already closed by a later event of this agent"}));
        return Ok(None);
    }
    let v = bash_attribution(writ_dir, &root, &w, POST_EVENT, true, log);
    let (mine, by_text, amb) = (v.mine, v.by_text, v.tell);
    let started = w.at;
    close_window(&path, w);
    let mut tell: Vec<String> = Vec::new();
    if !mine.is_empty() || !by_text.is_empty() {
        if let Some(line) = seal_for(writ_dir, input, &who, "Bash", &mine, &by_text, log)? {
            tell.push(line);
        }
        // Ruling 160 (2): the unsealed window over a Bash edit is an upper
        // bound, from the Pre snapshot to the seal written ("at most").
        log.write(
            json!({"event": "PostToolUse", "tool": "Bash", "agent": who.id,
            "outcome": "bash_window", "paths": mine, "by_text": by_text,
            "edit_to_seal_ms_at_most": (chrono::Utc::now() - started).num_milliseconds()}),
        );
    }
    if !amb.is_empty() {
        let agent = who.parent.is_some().then_some(who.id.as_str());
        tell.push(ambiguous_line(&amb, agent));
    }
    Ok((!tell.is_empty()).then(|| additional_context(input, &tell.join("; also: "))))
}

// ---------------------------------------------------------------------------
// Ambiguous paths outside the hook (findings 168, 169)
// ---------------------------------------------------------------------------

/// The identities that could have made the current content of `rel`, when
/// the hook recorded it as ambiguous and the content has not changed since.
pub fn ambiguous_candidates(writ_dir: &Path, rel: &str) -> Option<Vec<String>> {
    let root = writ_dir.parent().unwrap_or(writ_dir);
    let (hash, who) = load_ambiguous(writ_dir).paths.get(rel).cloned()?;
    (content_hash(&root.join(rel)) == hash).then_some(who)
}

/// Forget the ambiguity records of `paths` (after a candidate sealed them).
pub fn clear_ambiguous(writ_dir: &Path, paths: &[String]) {
    let mut a = load_ambiguous(writ_dir);
    let before = a.paths.len();
    a.paths.retain(|p, _| !paths.contains(p));
    if a.paths.len() != before {
        save_ambiguous(writ_dir, &a);
    }
}

/// Who may seal `paths` when some are ambiguous (finding 169), for a seal
/// by `caller` (hand or `--auto`). Ok(None): the caller may. Ok(Some(id)):
/// the caller resolved to a parent whose one subagent is a candidate for
/// every ambiguous path, and `auto` is set: seal as that subagent (finding
/// 168's fallback for a hand-typed fix; a subagent's shell resolves to its
/// parent). Err(line): refused, one line naming the candidates.
pub fn ambiguous_gate(
    writ_dir: &Path,
    caller: &str,
    paths: &[String],
    auto: bool,
) -> Result<Option<String>, String> {
    let mut cands: Vec<(String, Vec<String>)> = paths
        .iter()
        .filter_map(|p| ambiguous_candidates(writ_dir, p).map(|c| (p.clone(), c)))
        .collect();
    cands.retain(|(_, c)| !c.is_empty());
    if cands.is_empty() || cands.iter().all(|(_, c)| c.iter().any(|x| x == caller)) {
        return Ok(None);
    }
    let prefix = format!("{caller}-");
    let mut subs: Vec<String> = cands
        .iter()
        .flat_map(|(_, c)| c.iter().filter(|x| x.starts_with(&prefix)).cloned())
        .collect();
    subs.sort();
    subs.dedup();
    let every = |id: &str| cands.iter().all(|(_, c)| c.iter().any(|x| x == id));
    if auto && subs.len() == 1 && every(&subs[0]) {
        return Ok(Some(subs[0].clone()));
    }
    let named: Vec<&str> = cands.iter().map(|(p, _)| p.as_str()).collect();
    let mut who: Vec<String> = cands.iter().flat_map(|(_, c)| c.clone()).collect();
    who.sort();
    who.dedup();
    let line = if subs.len() > 1 {
        let forms: Vec<String> = subs.iter().map(|s| format!("--agent {s}")).collect();
        format!(
            "seal refused: {} changed while agents {} ran commands and writ could not tell whose change it is; \
             several of your subagents are candidates; the one whose change it is runs: writ seal --paths {} --auto with one of {}. Nothing was sealed.",
            named.join(", "),
            who.join(", "),
            named.join(","),
            forms.join(" or ")
        )
    } else {
        format!(
            "seal refused: {} changed while agents {} ran commands and writ could not tell whose change it is; \
             only one of them may seal it (writ seal --paths {} --auto), not '{caller}'. Nothing was sealed.",
            named.join(", "),
            who.join(", "),
            named.join(",")
        )
    };
    Err(line)
}

/// Finding 169: a left_out finding's ambiguous paths become their own
/// finding, addressed to the candidates by name: not for any agent, not for
/// a human, the fix `writ seal --paths <p> --auto` meant for one of them.
pub fn address_ambiguous(writ_dir: &Path, findings: &mut Vec<crate::doctor::DoctorFinding>) {
    let mut out = Vec::with_capacity(findings.len());
    for mut f in findings.drain(..) {
        if f.check != crate::doctor::CHECK_LEFT_OUT {
            out.push(f);
            continue;
        }
        let mut groups: BTreeMap<Vec<String>, Vec<String>> = BTreeMap::new();
        f.paths.retain(|p| match ambiguous_candidates(writ_dir, p) {
            Some(mut c) if !c.is_empty() => {
                c.sort();
                groups.entry(c).or_default().push(p.clone());
                false
            }
            _ => true,
        });
        if !f.paths.is_empty() {
            out.push(f.clone());
        }
        for (who, paths) in groups {
            let mut g = f.clone();
            g.message = format!(
                "{} changed while agents {} ran commands; writ could not tell whose change it is, \
                 so the fix is for whichever of {} made the change (not for any other agent)",
                paths.join(", "),
                who.join(" and "),
                who.join(" or ")
            );
            g.fix_command = Some(format!("writ seal --paths {} --auto", paths.join(",")));
            g.anyone_may_fix = false;
            g.needs_human = false;
            g.paths = paths;
            out.push(g);
        }
    }
    *findings = out;
}

/// Finding 168: the spec a hand-run `writ seal --auto` goes under when the
/// identity holds no open spec. None when it holds one (the normal seal
/// resolution applies). With no spec of any kind: a new auto spec, as the
/// first edit would open. When its newest auto spec is closed (by
/// `spec done` or SessionEnd, not committed): `<that id>-followup`,
/// recording what it continues; reused while open.
pub fn spec_for_hand_auto(repo: &Repository, agent: &str) -> WritResult<Option<String>> {
    let specs = repo.list_specs()?;
    if specs
        .iter()
        .any(|s| s.claimed_by.as_deref() == Some(agent) && is_open(s))
    {
        return Ok(None);
    }
    let created = specs
        .iter()
        .filter(|s| s.claimed_by.is_none() && s.created_by.as_deref() == Some(agent) && is_open(s))
        .count();
    if created == 1 {
        return Ok(None);
    }
    let closed = specs
        .iter()
        .filter(|s| {
            s.auto.is_some()
                && s.created_by.as_deref() == Some(agent)
                && !is_open(s)
                && !s.id.ends_with("-followup")
        })
        .max_by_key(|s| s.updated_at);
    let session = std::env::var("CLAUDE_CODE_SESSION_ID")
        .ok()
        .filter(|s| !s.is_empty());
    let Some(prev) = closed else {
        let who = HookIdentity {
            id: agent.to_string(),
            parent: None,
            session,
        };
        return create_auto_spec(repo, &who, &HookInput::default()).map(Some);
    };
    let id = format!("{}-followup", prev.id);
    if let Ok(existing) = repo.load_spec(&id) {
        if is_open(&existing) {
            repo.spec_claim(&id, agent)?;
            return Ok(Some(id));
        }
    }
    let mut spec = Spec::new(
        id.clone(),
        format!("{} (follow-up)", prev.title),
        format!("Continues {} after it closed (writ seal --auto).", prev.id),
    );
    spec.created_by = Some(agent.to_string());
    spec.continues = Some(prev.id.clone());
    spec.auto = prev.auto.clone().map(|mut a| {
        a.closed_by_session_end = None;
        a
    });
    match repo.add_spec(&spec) {
        Ok(()) | Err(WritError::SpecAlreadyExists(_)) => {}
        Err(e) => return Err(e),
    }
    repo.spec_claim(&id, agent)?;
    Ok(Some(id))
}

// ---------------------------------------------------------------------------
// Which spec
// ---------------------------------------------------------------------------

fn is_open(s: &Spec) -> bool {
    matches!(s.status, SpecStatus::Pending | SpecStatus::InProgress)
        && s.commit_state == crate::spec::CommitState::Uncommitted
        && !matches!(
            s.lifecycle_state,
            LifecycleState::Cancelled | LifecycleState::Completed | LifecycleState::Archived
        )
}

/// The spec an auto seal goes under: the identity's one open claimed spec;
/// with several, this session's auto spec, else the one whose scope covers
/// the path, else the most recently active; with none, its one unclaimed
/// spec it created (claimed now, as a first hand seal would, finding 87);
/// else a new auto spec, created and claimed.
fn spec_for(
    repo: &Repository,
    who: &HookIdentity,
    input: &HookInput,
    rel: &str,
) -> WritResult<String> {
    let specs = repo.list_specs()?;
    let mut held: Vec<&Spec> = specs
        .iter()
        .filter(|s| s.claimed_by.as_deref() == Some(who.id.as_str()) && is_open(s))
        .collect();
    if held.len() > 1 {
        if let Some(s) = held.iter().find(|s| {
            s.auto
                .as_ref()
                .is_some_and(|a| Some(&a.session) == who.session.as_ref())
        }) {
            return Ok(s.id.clone());
        }
        if let Some(s) = held
            .iter()
            .find(|s| crate::seal_scope::path_in_scope(&s.file_scope, rel))
        {
            return Ok(s.id.clone());
        }
        held.sort_by_key(|s| std::cmp::Reverse(s.last_activity));
    }
    if let Some(s) = held.first() {
        return Ok(s.id.clone());
    }
    let created: Vec<&Spec> = specs
        .iter()
        .filter(|s| {
            s.claimed_by.is_none() && s.created_by.as_deref() == Some(who.id.as_str()) && is_open(s)
        })
        .collect();
    if created.len() == 1 {
        repo.spec_claim(&created[0].id, &who.id)?;
        return Ok(created[0].id.clone());
    }
    create_auto_spec(repo, who, input)
}

fn create_auto_spec(
    repo: &Repository,
    who: &HookIdentity,
    input: &HookInput,
) -> WritResult<String> {
    let session = who.session.clone().unwrap_or_else(|| who.id.clone());
    let mut base = format!("{AUTO_SPEC_PREFIX}{}", key8(&session));
    if let Some(sfx) = subagent_suffix(input) {
        base.push('-');
        base.push_str(&sfx);
    }
    let title = match who.parent {
        Some(_) => format!("auto work by {}", who.id),
        None => stashed_prompt(repo.writ_dir(), &session)
            .unwrap_or_else(|| format!("auto work by {}", who.id)),
    };
    let description = match &who.parent {
        Some(p) => format!(
            "Opened by writ's auto seal for subagent {} of {p} (Claude Code agent_type {}, agent_id {}).",
            who.id,
            input.agent_type.as_deref().unwrap_or("unknown"),
            input.agent_id.as_deref().unwrap_or("unknown")
        ),
        None => format!(
            "Opened by writ's auto seal: {} edited a file with no spec claimed.",
            who.id
        ),
    };
    for n in 1..=50 {
        let id = if n == 1 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        let mut spec = Spec::new(id.clone(), title.clone(), description.clone());
        spec.created_by = Some(who.id.clone());
        spec.auto = Some(AutoSpec {
            session: session.clone(),
            parent: who.parent.clone(),
            host_agent_id: input.agent_id.clone().filter(|_| who.parent.is_some()),
            host_agent_type: input.agent_type.clone().filter(|_| who.parent.is_some()),
            closed_by_session_end: None,
        });
        match repo.add_spec(&spec) {
            Ok(()) => {
                repo.spec_claim(&id, &who.id)?;
                return Ok(id);
            }
            Err(WritError::SpecAlreadyExists(_)) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(WritError::Other(format!(
        "no free auto spec id after {base}-50"
    )))
}

// ---------------------------------------------------------------------------
// UserPromptSubmit: the title stash
// ---------------------------------------------------------------------------

fn prompt_path(writ_dir: &Path, session: &str) -> PathBuf {
    writ_dir
        .join(STATE_DIR)
        .join("prompts")
        .join(format!("{}.txt", key8(session)))
}

/// Keep the first non-empty line of the session's latest real prompt (not
/// a `<task-notification>`, finding 133), at most 80 characters. Prints
/// nothing: UserPromptSubmit stdout is injected into the agent's context.
fn stash_prompt(writ_dir: &Path, input: &HookInput, log: &HookLog) -> WritResult<()> {
    // A new prompt: this identity's earlier calls are over.
    let who = identity_for(writ_dir, input);
    close_late(writ_dir, input, &who, LATE_PROMPT, None, log)?;
    let (Some(session), Some(prompt)) = (input.session_id.as_deref(), input.prompt.as_deref())
    else {
        return Ok(());
    };
    if prompt.trim_start().starts_with(TASK_NOTIFICATION) {
        return Ok(());
    }
    let Some(first) = prompt.lines().map(str::trim).find(|l| !l.is_empty()) else {
        return Ok(());
    };
    let title = prompt_title(first);
    let path = prompt_path(writ_dir, session);
    if let Some(d) = path.parent() {
        fs::create_dir_all(d)?;
    }
    crate::fsutil::atomic_write(&path, title.as_bytes())?;
    log.write(json!({"event": "UserPromptSubmit", "outcome": "stashed"}));
    Ok(())
}

/// Longest auto spec title, in characters, ellipsis included (finding 170).
pub const TITLE_MAX: usize = 59;

/// The auto spec title from a prompt line (finding 170): its first
/// sentence; if that is over [`TITLE_MAX`] characters, cut at the last word
/// boundary that fits, with "…". Never mid-word (a single word longer than
/// the limit is the one exception, cut with "…").
pub fn prompt_title(line: &str) -> String {
    let line = line.trim();
    let sentence = line
        .char_indices()
        .find(|&(i, c)| {
            matches!(c, '.' | '?' | '!')
                && line[i + c.len_utf8()..]
                    .chars()
                    .next()
                    .is_none_or(char::is_whitespace)
        })
        .map(|(i, c)| &line[..i + c.len_utf8()])
        .unwrap_or(line)
        .trim();
    if sentence.chars().count() <= TITLE_MAX {
        return sentence.to_string();
    }
    let budget = TITLE_MAX - 1; // room for the ellipsis
    let mut out = String::new();
    for word in sentence.split_whitespace() {
        let next = if out.is_empty() {
            word.chars().count()
        } else {
            out.chars().count() + 1 + word.chars().count()
        };
        if next > budget {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    if out.is_empty() {
        out = sentence.chars().take(budget).collect();
    }
    let out = out.trim_end_matches([',', ';', ':', '-']).to_string();
    format!("{out}…")
}

fn stashed_prompt(writ_dir: &Path, session: &str) -> Option<String> {
    fs::read_to_string(prompt_path(writ_dir, session))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// Touched paths and refusals told, per identity
// ---------------------------------------------------------------------------

/// One touched path's state: sealed (with the content hash sealed),
/// refused (with the content hash refused) or pending (a lock timeout).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
enum Mark {
    Sealed { hash: Option<String> },
    Refused { hash: Option<String> },
    Pending,
}

#[derive(Debug, Default, serde::Serialize, Deserialize)]
struct TouchedFile {
    #[serde(default)]
    agent: String,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    paths: BTreeMap<String, Mark>,
    /// Ruling 177: moves of this identity's sealed lines by other agents
    /// were told up to this instant.
    #[serde(default)]
    moves_after: Option<chrono::DateTime<chrono::Utc>>,
}

/// The paths an identity edited, under `.writ/agents/claudecode/touched/`
/// (the only per-session state ruling 149 allows). A path is due for a
/// seal when it is pending, or its content moved on from what was sealed
/// or refused.
struct Touched {
    path: PathBuf,
    file: TouchedFile,
}

impl Touched {
    fn dir(writ_dir: &Path) -> PathBuf {
        writ_dir.join(STATE_DIR).join("touched")
    }

    fn load(writ_dir: &Path, who: &HookIdentity) -> Self {
        let path = Self::dir(writ_dir).join(format!("{}.json", key8(&who.id)));
        let mut file: TouchedFile = fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        file.agent = who.id.clone();
        if who.session.is_some() {
            file.session = who.session.clone();
        }
        Self { path, file }
    }

    /// Every identity's list for one host session (SessionEnd's drain).
    fn of_session(writ_dir: &Path, session: &str) -> Vec<Self> {
        let Ok(rd) = fs::read_dir(Self::dir(writ_dir)) else {
            return Vec::new();
        };
        rd.flatten()
            .filter_map(|e| {
                let file: TouchedFile =
                    serde_json::from_str(&fs::read_to_string(e.path()).ok()?).ok()?;
                (file.session.as_deref() == Some(session)).then(|| Self {
                    path: e.path(),
                    file,
                })
            })
            .collect()
    }

    fn mark_sealed(&mut self, rel: &str, hash: Option<String>) {
        self.file
            .paths
            .insert(rel.to_string(), Mark::Sealed { hash });
    }

    fn mark_refused(&mut self, rel: &str, hash: Option<String>) {
        self.file
            .paths
            .insert(rel.to_string(), Mark::Refused { hash });
    }

    /// Mark pending; true when the path was not pending already (log once).
    fn mark_pending(&mut self, rel: &str) -> bool {
        self.file.paths.insert(rel.to_string(), Mark::Pending) != Some(Mark::Pending)
    }

    fn due(&self, root: &Path) -> Vec<String> {
        self.file
            .paths
            .iter()
            .filter(|(rel, mark)| match mark {
                Mark::Pending => true,
                Mark::Sealed { hash } | Mark::Refused { hash } => {
                    content_hash(&root.join(rel)) != *hash
                }
            })
            .map(|(rel, _)| rel.clone())
            .collect()
    }

    fn save(&self) {
        if let Some(d) = self.path.parent() {
            let _ = fs::create_dir_all(d);
        }
        if let Ok(json) = serde_json::to_string(&self.file) {
            let _ = crate::fsutil::atomic_write(&self.path, json.as_bytes());
        }
    }
}

/// Refusals already told to an identity, by path, so the same refusal is
/// told once and not on every later edit (it stays in hooks.log, handoff's
/// unsealed list and doctor).
struct Told {
    path: PathBuf,
    told: BTreeMap<String, String>,
}

impl Told {
    fn load(writ_dir: &Path, agent: &str) -> Self {
        let path = writ_dir
            .join(STATE_DIR)
            .join("told")
            .join(format!("{}.json", key8(agent)));
        let told = fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Self { path, told }
    }

    /// True (and remembered) when this path's refusal `signature` was not
    /// told before.
    fn first_time(mut self, rel: &str, signature: &str) -> bool {
        if self.told.get(rel).map(String::as_str) == Some(signature) {
            return false;
        }
        self.told.insert(rel.to_string(), signature.to_string());
        if let Some(d) = self.path.parent() {
            let _ = fs::create_dir_all(d);
        }
        if let Ok(json) = serde_json::to_string(&self.told) {
            let _ = crate::fsutil::atomic_write(&self.path, json.as_bytes());
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Stop / SubagentStop
// ---------------------------------------------------------------------------

/// Seal every touched path of this identity that is due (pending, or its
/// content moved on since its auto seal or refusal: a formatter, a Bash
/// edit, a lock timeout). Exactly those paths; nothing printed (a Stop
/// hook's output can keep the agent running).
fn sweep(writ_dir: &Path, input: &HookInput, log: &HookLog) -> WritResult<()> {
    let root = writ_dir.parent().unwrap_or(writ_dir).to_path_buf();
    let who = identity_for(writ_dir, input);
    // Ruling 155 (3): Bash calls whose Post never came. Same attribution as
    // the Post; nothing printed.
    close_late(writ_dir, input, &who, "Stop", None, log)?;
    let mut touched = Touched::load(writ_dir, &who);
    let due = touched.due(&root);
    let due = drain_mine(writ_dir, &root, &who, due, &mut touched, log);
    touched.save();
    if due.is_empty() {
        return Ok(());
    }
    let repo = match open_checked(&root, hook_lock_wait(writ_dir)) {
        Ok(r) => r,
        Err(WritError::LockTimeout) => {
            lock_timeout(&mut touched, &due, "Stop", &who, log);
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    let spec = spec_for(&repo, &who, input, &due[0])?;
    seal_paths_auto(&repo, &who, &spec, "Stop", &due, None, &mut touched, log)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// SessionEnd
// ---------------------------------------------------------------------------

/// Record the session end (sprint 5) and close this session's open auto
/// specs, the main agent's and its subagents' (matched by the host session
/// id). A spec the agent closed itself, a hand-made spec and any other
/// session's spec are left alone. No summary text is written.
fn session_end(writ_dir: &Path, input: &HookInput, raw: &str, log: &HookLog) -> WritResult<()> {
    if let Err(e) = crate::hooks::session_end::record_current(writ_dir, raw) {
        log.write(json!({"event": "SessionEnd", "outcome": "error",
            "detail": format!("session end record: {e}")}));
    }
    if input
        .reason
        .as_deref()
        .is_some_and(|r| crate::hooks::session_end::PROCESS_CONTINUES.contains(&r))
    {
        return Ok(());
    }
    let Some(session) = input.session_id.as_deref().filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    let root = writ_dir.parent().unwrap_or(writ_dir).to_path_buf();
    let repo = open_checked(&root, SESSION_END_LOCK_WAIT)?;
    // Open Bash windows of this session are over with it.
    if let Ok(rd) = fs::read_dir(writ_dir.join(BASH_DIR)) {
        for e in rd.flatten() {
            let ours = fs::read_to_string(e.path())
                .ok()
                .and_then(|t| serde_json::from_str::<BashWindow>(&t).ok())
                .is_some_and(|w| w.session.as_deref() == Some(session));
            if ours {
                if let Some(w) = fs::read_to_string(e.path())
                    .ok()
                    .and_then(|t| serde_json::from_str::<BashWindow>(&t).ok())
                    .filter(|w| w.end.is_none())
                {
                    close_window(&e.path(), w);
                }
            }
        }
    }
    // Drain every identity of this session (main and subagents) first.
    for mut touched in Touched::of_session(writ_dir, session) {
        let who = HookIdentity {
            id: touched.file.agent.clone(),
            parent: None,
            session: Some(session.to_string()),
        };
        let due = touched.due(&root);
        let due = drain_mine(writ_dir, &root, &who, due, &mut touched, log);
        touched.save();
        if due.is_empty() {
            continue;
        }
        match spec_for(&repo, &who, input, &due[0]) {
            Ok(spec) => {
                seal_paths_auto(
                    &repo,
                    &who,
                    &spec,
                    "SessionEnd",
                    &due,
                    None,
                    &mut touched,
                    log,
                )?;
            }
            Err(e) => log.write(json!({"event": "SessionEnd", "agent": who.id,
                "outcome": "error", "detail": e.to_string()})),
        }
    }
    for spec in repo.list_specs()? {
        let ours = spec.auto.as_ref().is_some_and(|a| a.session == session);
        // Finding 174: an auto spec that never got a seal is not work; it
        // is removed, so no receipt lists it.
        if ours && is_open(&spec) && spec.sealed_by.is_empty() {
            if repo.remove_empty_auto_spec(&spec.id)? {
                log.write(
                    json!({"event": "SessionEnd", "outcome": "removed", "spec": spec.id,
                    "detail": "auto spec with no seals"}),
                );
            }
            continue;
        }
        if ours && is_open(&spec) && repo.close_auto_spec(&spec.id)? {
            log.write(
                json!({"event": "SessionEnd", "outcome": "closed", "spec": spec.id,
                "seals": spec.sealed_by.len()}),
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Log collapse
// ---------------------------------------------------------------------------

/// True for a seal the auto seal hook (or `writ seal --auto`) wrote.
pub fn is_auto(seal: &crate::seal::Seal) -> bool {
    seal.summary.starts_with(AUTO_PREFIX)
}

/// A seal log entry for display: one seal, or a run of two or more
/// consecutive auto seals of one spec by one agent (log order kept). The
/// records stay one per seal; only text output collapses.
#[derive(Debug)]
pub enum LogItem<'a> {
    One(&'a crate::seal::Seal),
    AutoRun(Vec<&'a crate::seal::Seal>),
}

/// Group consecutive auto seals of the same spec and agent.
pub fn collapse(seals: &[crate::seal::Seal]) -> Vec<LogItem<'_>> {
    fn flush<'a>(run: &mut Vec<&'a crate::seal::Seal>, out: &mut Vec<LogItem<'a>>) {
        match run.len() {
            0 => {}
            1 => out.push(LogItem::One(run[0])),
            _ => out.push(LogItem::AutoRun(std::mem::take(run))),
        }
        run.clear();
    }
    let mut out: Vec<LogItem<'_>> = Vec::new();
    let mut run: Vec<&crate::seal::Seal> = Vec::new();
    for seal in seals {
        let joins = is_auto(seal)
            && run
                .last()
                .is_some_and(|l| l.spec_id == seal.spec_id && l.agent.id == seal.agent.id);
        if !joins {
            flush(&mut run, &mut out);
        }
        if is_auto(seal) {
            run.push(seal);
        } else {
            out.push(LogItem::One(seal));
        }
    }
    flush(&mut run, &mut out);
    out
}

/// One-line summary of a run: `auto: N seals over <span>: <paths>`.
pub fn run_summary(run: &[&crate::seal::Seal]) -> String {
    let first = run.iter().map(|s| s.timestamp).min();
    let last = run.iter().map(|s| s.timestamp).max();
    let span = match (first, last) {
        (Some(a), Some(b)) => human_span((b - a).num_seconds().max(0)),
        _ => "0s".to_string(),
    };
    let mut paths: Vec<&str> = Vec::new();
    for s in run.iter().rev() {
        for c in &s.changes {
            if !paths.contains(&c.path.as_str()) {
                paths.push(c.path.as_str());
            }
        }
    }
    let shown: Vec<&str> = paths.iter().take(5).copied().collect();
    let more = if paths.len() > 5 {
        format!(" and {} more", paths.len() - 5)
    } else {
        String::new()
    };
    format!(
        "{AUTO_PREFIX} {} seals over {span}: {}{more}",
        run.len(),
        shown.join(", ")
    )
}

fn human_span(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m{:02}s", s / 60, s % 60),
        s => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(v: Value) -> HookInput {
        serde_json::from_value(v).unwrap()
    }

    fn seal(id: &str, agent: &str, spec: &str, summary: &str) -> crate::seal::Seal {
        serde_json::from_value(json!({
            "id": id, "parent": null, "timestamp": "2026-10-14T19:00:00Z", "tree": "t",
            "agent": {"id": agent, "agent_type": "agent"}, "spec_id": spec,
            "status": "in-progress", "changes": [{"path": "a.txt", "change_type": "modified",
            "old_hash": null, "new_hash": null}], "verification": {"tests_passed": null,
            "tests_failed": null, "linted": false}, "summary": summary
        }))
        .unwrap()
    }

    #[test]
    fn collapse_groups_consecutive_auto_seals_of_one_spec_and_agent() {
        let seals = vec![
            seal("1", "a", "s", "auto: Edit a.txt"),
            seal("2", "a", "s", "auto: Edit a.txt"),
            seal("3", "a", "s", "hand"),
            seal("4", "a", "s", "auto: Edit a.txt"),
            seal("5", "b", "s", "auto: Edit a.txt"),
            seal("6", "b", "t", "auto: Edit a.txt"),
        ];
        let items = collapse(&seals);
        let shape: Vec<usize> = items
            .iter()
            .map(|i| match i {
                LogItem::One(_) => 1,
                LogItem::AutoRun(r) => r.len(),
            })
            .collect();
        assert_eq!(shape, vec![2, 1, 1, 1, 1]);
        let LogItem::AutoRun(r) = &items[0] else {
            panic!()
        };
        assert_eq!(run_summary(r), "auto: 2 seals over 0s: a.txt");
    }

    #[test]
    fn lock_wait_reads_config_and_defaults_to_200_ms() {
        let base = std::env::temp_dir().join(format!("writ-auto-wait-{}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        assert_eq!(hook_lock_wait(&base), HOOK_LOCK_WAIT);
        fs::write(base.join("config.toml"), "[hooks]\nlock_wait_ms = 50\n").unwrap();
        assert_eq!(hook_lock_wait(&base), std::time::Duration::from_millis(50));
        let _ = fs::remove_dir_all(&base);
    }

    fn window(agent: &str, start_s: i64, end_s: Option<i64>) -> BashWindow {
        let t = |x: i64| chrono::DateTime::from_timestamp(x, 0).unwrap();
        BashWindow {
            agent: agent.into(),
            session: None,
            tool_use_id: format!("t-{agent}"),
            command: String::new(),
            at: t(start_s),
            end: end_s.map(t),
            sig: Default::default(),
        }
    }

    const S: i128 = NS_PER_SEC;

    #[test]
    fn r192_custom_agent_type_needs_a_file_and_never_a_builtin_or_unsafe_name() {
        let base = std::env::temp_dir().join(format!("writ-r192-{}", std::process::id()));
        let writ_dir = base.join(".writ");
        fs::create_dir_all(&writ_dir).unwrap();
        fs::create_dir_all(base.join(".claude/agents")).unwrap();
        for n in ["bri", "Explore", "human"] {
            fs::write(base.join(format!(".claude/agents/{n}.md")), "x").unwrap();
        }
        let ty = |t: &str| {
            custom_agent_type(
                &writ_dir,
                &input(json!({"agent_id": "a1", "agent_type": t})),
            )
        };
        assert_eq!(ty("bri").as_deref(), Some("bri"));
        assert_eq!(ty("Explore"), None);
        assert_eq!(ty("human"), None);
        assert_eq!(ty("haris"), None);
        assert_eq!(ty("../bri"), None);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn f188_names_path_takes_the_path_or_basename_as_a_whole_word() {
        assert!(names_path("printf x >> pkg/rel.txt", "pkg/rel.txt"));
        assert!(names_path("cd pkg && printf x >> mod.py", "pkg/mod.py"));
        assert!(names_path(
            "sh <<'EOF'\ncat > notes/h.txt\nEOF",
            "notes/h.txt"
        ));
        assert!(names_path("cat ./app.py", "app.py"));
        assert!(!names_path("cat myapp.py", "app.py"));
        assert!(!names_path("cat app.pyc", "app.py"));
        assert!(!names_path("cat my-app.py", "app.py"));
        assert!(!names_path("ls >/dev/null", "app.py"));
        assert!(!names_path("anything", ""));
    }

    fn cmd_window(
        agent: &str,
        id: &str,
        cmd: &str,
        start_s: i64,
        end_s: Option<i64>,
    ) -> BashWindow {
        let mut w = window(agent, start_s, end_s);
        w.tool_use_id = id.into();
        w.command = cmd.into();
        w
    }

    #[test]
    fn f188_rule_1_reading_w_over_every_overlapping_window() {
        let now = 200 * S;
        let cands = vec!["a".to_string(), "b".to_string()];
        let rep = cmd_window("a", "a1", "ls", 100, None);
        // Only b's earlier (closed, overlapping) window names the path: b.
        let b_old = cmd_window("b", "b1", "cat > x.py <<'EOF'\nEOF", 90, Some(120));
        let b_now = cmd_window("b", "b2", "pytest -q", 130, None);
        let ws = vec![b_old, b_now, cmd_window("a", "a1", "ls", 100, None)];
        assert_eq!(
            command_text_owner("x.py", &cands, &rep, &ws, now).as_deref(),
            Some("b")
        );
        // A window that closed before the change range does not count.
        let b_gone = cmd_window("b", "b0", "echo > x.py", 10, Some(20));
        assert_eq!(
            command_text_owner("x.py", &cands, &rep, &[b_gone], now),
            None
        );
        // Both name it: nobody.
        let rep2 = cmd_window("a", "a2", "echo >> x.py", 100, None);
        let b2 = cmd_window("b", "b3", "grep y x.py", 120, None);
        assert_eq!(command_text_owner("x.py", &cands, &rep2, &[b2], now), None);
        // A candidate window with no command text: never guess.
        let b_blank = cmd_window("b", "b4", "  ", 120, None);
        assert_eq!(
            command_text_owner("x.py", &cands, &rep2, &[b_blank], now),
            None
        );
    }

    #[test]
    fn ruling_158_one_candidate_owns_several_are_ambiguous() {
        let a = window("a", 100, None);
        let now = 200 * S;
        // Only a's interval contains the write.
        let b_closed = window("b", 10, Some(50));
        assert_eq!(
            attribute(Some(150 * S + 7), &a, now, &[b_closed]).0,
            Owner::Mine
        );
        // b's interval also contains it: nobody.
        let b_open = window("b", 120, None);
        assert_eq!(
            attribute(Some(150 * S + 7), &a, now, &[b_open]).0,
            Owner::Ambiguous(vec!["a".into(), "b".into()])
        );
        // The write happened in b's closed interval, before a began: b's.
        let b_before = window("b", 60, Some(90));
        assert_eq!(
            attribute(Some(80 * S + 3), &a, now, &[b_before]).0,
            Owner::Theirs
        );
    }

    #[test]
    fn ruling_158_no_candidate_falls_back_to_overlap() {
        let a = window("a", 100, None);
        let now = 200 * S;
        // mtime preserved from long ago (cp -p): a owns it only alone.
        assert_eq!(attribute(Some(5 * S + 1), &a, now, &[]).0, Owner::Mine);
        let b = window("b", 150, Some(160));
        assert!(matches!(
            attribute(Some(5 * S + 1), &a, now, &[b]).0,
            Owner::Ambiguous(_)
        ));
        // A deletion has no mtime: the same fallback.
        assert_eq!(attribute(None, &a, now, &[]).0, Owner::Mine);
    }

    #[test]
    fn ruling_158_whole_second_mtime_widens_and_is_reported() {
        let a = window("a", 100, Some(101));
        let b = window("b", 101, None);
        // 100.000000000 exactly: coarse; widened to [100, 101) hits only a.
        let (owner, coarse) = attribute(Some(100 * S), &a, 300 * S, &[b]);
        assert!(coarse);
        assert_eq!(owner, Owner::Mine);
    }

    #[test]
    fn finding_170_title_is_the_first_sentence_cut_at_a_word() {
        assert_eq!(
            prompt_title("Fix the parser off-by-one"),
            "Fix the parser off-by-one"
        );
        assert_eq!(
            prompt_title("Build the API. Then write tests for every endpoint."),
            "Build the API."
        );
        let long = "Build a small task tracker API with authentication, persistence and a full test suite for it";
        let t = prompt_title(long);
        assert!(t.chars().count() <= TITLE_MAX, "{t}");
        assert!(t.ends_with('…'), "{t}");
        let body = t.trim_end_matches('…');
        assert!(long.starts_with(body), "{t}");
        let next = long[body.len()..].chars().next();
        assert!(matches!(next, Some(' ' | ',')), "cut mid-word: {t}");
        assert!(!body.ends_with(','), "{t}");
        // Version numbers and paths are not sentence ends.
        assert_eq!(prompt_title("Bump to 0.4.3 now"), "Bump to 0.4.3 now");
    }

    fn gate_dir(cands: &[&str]) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "writ-auto-gate-{}-{}",
            std::process::id(),
            cands.join("_")
        ));
        let wd = base.join(".writ");
        fs::create_dir_all(&wd).unwrap();
        fs::write(base.join("s.txt"), "x\n").unwrap();
        let mut a = AmbiguousFile::default();
        a.paths.insert(
            "s.txt".into(),
            (
                content_hash(&base.join("s.txt")),
                cands.iter().map(|c| c.to_string()).collect(),
            ),
        );
        save_ambiguous(&wd, &a);
        wd
    }

    #[test]
    fn finding_168_hand_typed_fix_falls_back_to_the_one_subagent_candidate() {
        let p = vec!["s.txt".to_string()];
        let wd = gate_dir(&["P-a1b2c3d4", "bravo"]);
        // The caller resolves to the parent; its one subagent is a candidate.
        assert_eq!(
            ambiguous_gate(&wd, "P", &p, true),
            Ok(Some("P-a1b2c3d4".to_string()))
        );
        // Without --auto there is no inference: refused, naming them.
        assert!(ambiguous_gate(&wd, "P", &p, false).is_err());
        // A candidate seals as itself.
        assert_eq!(ambiguous_gate(&wd, "bravo", &p, false), Ok(None));
        // A third identity is refused with the candidates named.
        let e = ambiguous_gate(&wd, "charlie", &p, true).unwrap_err();
        assert!(
            e.contains("P-a1b2c3d4") && e.contains("bravo") && e.lines().count() == 1,
            "{e}"
        );
        let _ = fs::remove_dir_all(wd.parent().unwrap());
        // Two subagent candidates: refused, each with its --agent form.
        let wd = gate_dir(&["P-a1b2c3d4", "P-b7e1c2d3"]);
        let e = ambiguous_gate(&wd, "P", &p, true).unwrap_err();
        assert!(
            e.contains("--agent P-a1b2c3d4") && e.contains("--agent P-b7e1c2d3"),
            "{e}"
        );
        // Content changed since: no longer ambiguous.
        fs::write(wd.parent().unwrap().join("s.txt"), "y\n").unwrap();
        assert_eq!(ambiguous_gate(&wd, "charlie", &p, false), Ok(None));
        let _ = fs::remove_dir_all(wd.parent().unwrap());
    }

    #[test]
    fn finding_168_one_line_for_several_paths_with_the_subagent_named() {
        let l = ambiguous_line(
            &[
                ("a.txt".into(), vec!["P-a1b2c3d4".into(), "b".into()]),
                (
                    "b.txt".into(),
                    vec!["P-a1b2c3d4".into(), "b".into(), "c".into()],
                ),
            ],
            Some("P-a1b2c3d4"),
        );
        assert_eq!(l.lines().count(), 1);
        assert!(l.contains("(P-a1b2c3d4, b, c)"), "{l}");
        assert!(
            l.ends_with("writ seal --paths a.txt,b.txt --auto --agent P-a1b2c3d4"),
            "{l}"
        );
        assert!(l.contains("3 agents"));
    }

    #[test]
    fn finding_173_hook_override_never_says_after_being_told() {
        let o = crate::seal::OverrideContext {
            path: "app.py".into(),
            lines: vec!["x".into()],
            by_hook: true,
            ..Default::default()
        };
        let d = o.describe("a", "0123456789abcdef");
        assert!(!d.contains("after being told"), "{d}");
        assert!(d.contains("in its own edit"), "{d}");
    }

    #[test]
    fn matcher_is_the_edit_tools() {
        assert_eq!(EDIT_TOOLS_MATCHER, EDIT_TOOLS.join("|"));
    }

    #[test]
    fn edited_path_reads_file_path_and_notebook_path() {
        let e = input(json!({"tool_input": {"file_path": "/r/a.txt"}}));
        assert_eq!(e.edited_path().as_deref(), Some("/r/a.txt"));
        let n = input(json!({"tool_input": {"notebook_path": "/r/nb.ipynb", "cell_id": "c"}}));
        assert_eq!(n.edited_path().as_deref(), Some("/r/nb.ipynb"));
        assert_eq!(input(json!({"tool_input": {}})).edited_path(), None);
    }

    #[test]
    fn live_fixtures_parse() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../writ-cli/tests/fixtures/claude_hooks");
        let sub: HookInput = serde_json::from_str(
            &fs::read_to_string(dir.join("post_tool_use_edit_subagent.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(sub.hook_event_name.as_deref(), Some("PostToolUse"));
        assert!(sub.edited_path().unwrap().ends_with("src/c.py"));
        assert_eq!(subagent_suffix(&sub).as_deref(), Some("a1b2c3d4"));
        let nb: HookInput = serde_json::from_str(
            &fs::read_to_string(dir.join("post_tool_use_notebookedit_main.json")).unwrap(),
        )
        .unwrap();
        assert!(nb.edited_path().unwrap().ends_with("src/nb.ipynb"));
        assert_eq!(subagent_suffix(&nb), None);
        let end: HookInput =
            serde_json::from_str(&fs::read_to_string(dir.join("sessionend.json")).unwrap())
                .unwrap();
        assert_eq!(end.reason.as_deref(), Some("other"));
    }

    #[test]
    fn relative_path_rejects_outside_and_writ_dir() {
        let base = std::env::temp_dir().join(format!("writ-auto-rel-{}", std::process::id()));
        fs::create_dir_all(base.join("repo/.writ")).unwrap();
        fs::write(base.join("repo/a.txt"), "x").unwrap();
        fs::write(base.join("out.txt"), "x").unwrap();
        let root = base.join("repo");
        let a = root.join("a.txt").display().to_string();
        assert_eq!(relative_path(&root, None, &a).as_deref(), Some("a.txt"));
        let out = base.join("out.txt").display().to_string();
        assert_eq!(relative_path(&root, None, &out), None);
        let w = root.join(".writ/x").display().to_string();
        assert_eq!(relative_path(&root, None, &w), None);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn refusal_is_one_line_with_a_pasteable_fix() {
        // Ruling 181: the hook and a hand seal print the same line.
        let err = WritError::ForeignLinesRemoved {
            losses: vec![(
                "sb".into(),
                "bravo".into(),
                Some("0123456789abcdef".into()),
                "shared.txt".into(),
                1,
            )],
        };
        let msg = err.foreign_lines_message().unwrap();
        assert_eq!(msg, err.to_string());
        assert_eq!(msg.lines().count(), 1, "{msg}");
        assert!(msg.contains("bravo") && msg.contains("shared.txt"), "{msg}");
        assert!(
            msg.contains("seal 0123456789ab") && msg.contains("1 line(s)"),
            "{msg}"
        );
        let fix = msg.split("fix:").nth(1).unwrap().trim();
        assert_eq!(
            fix,
            "writ seal --paths shared.txt --auto --allow-removals shared.txt"
        );
        assert!(!msg.contains("--force") && !msg.contains("restore"));
    }

    #[test]
    fn no_writ_dir_and_bad_json_never_error() {
        let base = std::env::temp_dir().join(format!("writ-auto-bad-{}", std::process::id()));
        fs::create_dir_all(base.join(".writ")).unwrap();
        assert_eq!(run(&base, "not json"), HookOutcome::default());
        let log = fs::read_to_string(base.join(".writ").join(HOOKS_LOG)).unwrap();
        assert!(log.contains("not JSON"), "{log}");
        let none = std::env::temp_dir().join("writ-auto-none-surely-absent");
        assert_eq!(run(&none, "{}"), HookOutcome::default());
        let _ = fs::remove_dir_all(&base);
    }
}
