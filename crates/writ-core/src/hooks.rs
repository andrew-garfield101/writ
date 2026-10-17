//! Agent framework hooks.
//!
//! Detects and integrates with agent frameworks (Claude Code, Codex, etc.)
//! by generating framework-specific configuration that instructs agents
//! to use writ for version control.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::WritResult;
use crate::fsutil::atomic_write;

/// The permission entries writ adds to Claude Code's settings.json.
/// Bash permission for CLI commands, MCP permission for native MCP tools.
const WRIT_PERMISSION: &str = "Bash(writ *)";
const WRIT_MCP_PERMISSION: &str = "mcp__writ__*";

/// HTML comment markers for writ-managed sections in framework files.
/// Used for idempotent append-in-place and surgical removal on uninstall.
const MARKER_BEGIN: &str = "<!-- BEGIN WRIT CONFIGURATION — managed by writ init -->";
const MARKER_END: &str = "<!-- END WRIT CONFIGURATION -->";

/// Supported agent frameworks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Framework {
    ClaudeCode,
    Codex,
    Custom,
}

/// Detection result for a single framework.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameworkDetection {
    pub framework: Framework,
    pub detected: bool,
    pub indicators: Vec<String>,
}

/// Result of running framework hooks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookResult {
    pub framework: Framework,
    pub files_created: Vec<String>,
    pub files_updated: Vec<String>,
}

/// Detect which agent frameworks are present in a project.
pub fn detect_frameworks(root: &Path) -> Vec<FrameworkDetection> {
    vec![detect_claude_code(root), detect_codex(root)]
}

fn detect_claude_code(root: &Path) -> FrameworkDetection {
    let mut indicators = Vec::new();

    if root.join("CLAUDE.md").exists() {
        indicators.push("CLAUDE.md".to_string());
    }
    if root.join(".claude").is_dir() {
        indicators.push(".claude/".to_string());
    }

    FrameworkDetection {
        framework: Framework::ClaudeCode,
        detected: !indicators.is_empty(),
        indicators,
    }
}

fn detect_codex(root: &Path) -> FrameworkDetection {
    let mut indicators = Vec::new();

    if root.join(".codex").is_dir() {
        indicators.push(".codex/".to_string());
    }
    if root.join("AGENTS.md").exists() {
        indicators.push("AGENTS.md".to_string());
    }

    FrameworkDetection {
        framework: Framework::Codex,
        detected: !indicators.is_empty(),
        indicators,
    }
}

/// Ensure `Bash(writ *)` is in `.claude/settings.json` so agents can run writ commands.
///
/// Creates the file if it doesn't exist. Merges the permission into existing
/// settings, preserving all other configuration.
fn ensure_claude_permissions(root: &Path) -> WritResult<Option<String>> {
    let claude_dir = root.join(".claude");
    if !claude_dir.exists() {
        fs::create_dir_all(&claude_dir)?;
    }

    let settings_path = claude_dir.join("settings.json");
    let mut settings = read_settings(&settings_path)?;

    // Navigate to permissions.allow, creating the structure if needed. A
    // shape writ does not expect is left alone, never a panic.
    let Some(allow_arr) = settings
        .as_object_mut()
        .map(|o| {
            o.entry("permissions")
                .or_insert_with(|| serde_json::json!({}))
        })
        .and_then(|p| p.as_object_mut())
        .map(|p| p.entry("allow").or_insert_with(|| serde_json::json!([])))
        .and_then(|a| a.as_array_mut())
    else {
        return Ok(None);
    };

    // Add both CLI and MCP permissions if not already present.
    let mut changed = false;

    let has_bash = allow_arr
        .iter()
        .any(|v| v.as_str() == Some(WRIT_PERMISSION));
    if !has_bash {
        allow_arr.push(serde_json::Value::String(WRIT_PERMISSION.to_string()));
        changed = true;
    }

    let has_mcp = allow_arr
        .iter()
        .any(|v| v.as_str() == Some(WRIT_MCP_PERMISSION));
    if !has_mcp {
        allow_arr.push(serde_json::Value::String(WRIT_MCP_PERMISSION.to_string()));
        changed = true;
    }

    if !changed {
        return Ok(None);
    }

    let json = serde_json::to_string_pretty(&settings)
        .map_err(|e| crate::error::WritError::Other(format!("JSON serialize: {}", e)))?;
    atomic_write(&settings_path, format!("{}\n", json).as_bytes())?;

    if settings_path.exists() {
        // File existed before, we updated it
        Ok(Some(".claude/settings.json".to_string()))
    } else {
        Ok(Some(".claude/settings.json".to_string()))
    }
}

/// Remove writ permissions (`Bash(writ *)` and `mcp__writ__*`) from
/// `.claude/settings.json` during uninstall.
///
/// Leaves the file in place even if the allow list becomes empty — we don't
/// delete the user's settings file.
fn remove_claude_permissions(root: &Path) -> WritResult<Option<String>> {
    let settings_path = root.join(".claude").join("settings.json");
    if !settings_path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&settings_path)?;
    let mut settings: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return Ok(None), // Don't touch malformed files
    };

    let removed = if let Some(permissions) = settings.get_mut("permissions") {
        if let Some(allow) = permissions.get_mut("allow") {
            if let Some(arr) = allow.as_array_mut() {
                let before = arr.len();
                arr.retain(|v| {
                    let s = v.as_str();
                    s != Some(WRIT_PERMISSION) && s != Some(WRIT_MCP_PERMISSION)
                });
                arr.len() < before
            } else {
                false
            }
        } else {
            false
        }
    } else {
        false
    };

    if removed {
        let json = serde_json::to_string_pretty(&settings)
            .map_err(|e| crate::error::WritError::Other(format!("JSON serialize: {}", e)))?;
        atomic_write(&settings_path, format!("{}\n", json).as_bytes())?;
        Ok(Some(".claude/settings.json".to_string()))
    } else {
        Ok(None)
    }
}

/// The instruction string added to `.claude/settings.json` so agents treat writ
/// usage as a mandatory directive, not optional project documentation.
/// Rendered from [`crate::template`] (finding 67).
fn writ_instruction() -> String {
    // Sprint 6: the zero-instruction lines (template::auto_steps), and the
    // marker phrase init uses to find this entry again.
    format!(
        "This project uses writ for version control. {}",
        crate::template::auto_steps().join(" ")
    )
}

/// Substring used to detect whether a writ instruction is already present.
const WRIT_INSTRUCTION_MARKER: &str = "writ for version control";

/// Add a writ usage instruction to `.claude/settings.json`.
///
/// Instructions in settings.json carry user-directive weight — the agent treats
/// them as "the user told me to do this" rather than "the project docs mention this."
fn ensure_claude_instructions(root: &Path) -> WritResult<Option<String>> {
    let claude_dir = root.join(".claude");
    if !claude_dir.exists() {
        fs::create_dir_all(&claude_dir)?;
    }

    let settings_path = claude_dir.join("settings.json");
    let mut settings = read_settings(&settings_path)?;
    let Some(obj) = settings.as_object_mut() else {
        return Ok(None); // Top level isn't an object — don't touch it
    };
    let original = serde_json::Value::Object(obj.clone());
    let instructions = obj
        .entry("instructions")
        .or_insert_with(|| serde_json::json!([]));
    let Some(arr) = instructions.as_array_mut() else {
        return Ok(None); // Not an array — don't touch it
    };

    // Finding 72: writ's own entry is always the current template text. Any
    // earlier writ instruction (older version, hand-edited) is replaced in
    // place; the user's other instructions stay where they are.
    let current = serde_json::Value::String(writ_instruction());
    let is_writ = |v: &serde_json::Value| {
        v.as_str()
            .is_some_and(|s| s.contains(WRIT_INSTRUCTION_MARKER))
    };
    match arr.iter().position(is_writ) {
        Some(i) => {
            arr[i] = current;
            let mut seen = false;
            arr.retain(|v| {
                if !is_writ(v) {
                    return true;
                }
                let keep = !seen;
                seen = true;
                keep
            });
        }
        None => arr.push(current),
    }

    if settings == original {
        return Ok(None);
    }
    write_settings(&settings_path, &settings)?;
    Ok(Some(".claude/settings.json".to_string()))
}

/// Remove the writ instruction from `.claude/settings.json` during uninit,
/// dropping the `instructions` key if that leaves it empty.
fn remove_claude_instructions(root: &Path) -> WritResult<Option<String>> {
    let settings_path = root.join(".claude").join("settings.json");
    if !settings_path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&settings_path)?;
    let mut settings: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    let Some(settings_obj) = settings.as_object_mut() else {
        return Ok(None);
    };
    let Some(arr) = settings_obj
        .get_mut("instructions")
        .and_then(|v| v.as_array_mut())
    else {
        return Ok(None);
    };
    let before = arr.len();
    arr.retain(|v| {
        v.as_str()
            .is_none_or(|s| !s.contains(WRIT_INSTRUCTION_MARKER))
    });
    let removed = arr.len() < before;
    // Init creates the key when absent; if writ's entry was all it held,
    // drop the key so uninit restores the original file exactly.
    if removed && arr.is_empty() {
        settings_obj.remove("instructions");
    }

    if removed {
        let json = serde_json::to_string_pretty(&settings)
            .map_err(|e| crate::error::WritError::Other(format!("JSON serialize: {}", e)))?;
        atomic_write(&settings_path, format!("{}\n", json).as_bytes())?;
        Ok(Some(".claude/settings.json".to_string()))
    } else {
        Ok(None)
    }
}

/// Substring used to detect whether a writ hook is already present.
const WRIT_HOOK_MARKER: &str = "writ context";

/// Files whose writ-managed text is older than this binary's template
/// (`writ doctor` version_skew): the CLAUDE.md marked block, and the
/// SessionStart hook in `.claude/settings.json` (or a writ hook still on a
/// legacy event). Only text writ already manages is checked; a project that
/// never opted in reports nothing. `writ init -y` refreshes both.
pub fn stale_managed_text(root: &Path) -> Vec<String> {
    let mut stale = Vec::new();
    if let Ok(content) = fs::read_to_string(root.join("CLAUDE.md")) {
        if content.contains(MARKER_BEGIN) {
            let fresh = wrap_with_markers(&writ_claude_md_section());
            if replace_marked_section(&content, &fresh) != content {
                stale.push("CLAUDE.md".to_string());
            }
        }
    }
    let settings_path = root.join(".claude").join("settings.json");
    if let Ok(settings) = read_settings(&settings_path) {
        if settings_hook_stale(&settings) {
            stale.push(".claude/settings.json".to_string());
        }
    }
    stale
}

/// True when the settings hold a writ hook that is not today's command (for
/// whatever writ path it names), sits on a legacy event, or when a project
/// with writ's SessionStart hook lacks one of the sprint 6 auto seal hooks
/// (`writ init -y` adds them).
fn settings_hook_stale(settings: &serde_json::Value) -> bool {
    let Some(hooks) = settings.get("hooks").and_then(|h| h.as_object()) else {
        return false;
    };
    let entries = |event: &str| -> Vec<(Option<String>, String)> {
        hooks
            .get(event)
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .flat_map(|e| {
                let matcher = e
                    .get("matcher")
                    .and_then(|m| m.as_str())
                    .map(str::to_string);
                e.get("hooks")
                    .and_then(|h| h.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|h| h.get("command").and_then(|c| c.as_str()))
                    .filter(|c| is_writ_hook_command(c))
                    .map(move |c| (matcher.clone(), c.to_string()))
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    let starts = entries(WRIT_HOOK_EVENT);
    if starts.is_empty() {
        // Not opted in through init's hooks: only a legacy hook is stale.
        return LEGACY_HOOK_EVENTS.iter().any(|e| {
            entries(e)
                .iter()
                .any(|(_, c)| !c.contains(WRIT_AUTO_HOOK_MARKER))
        });
    }
    let fallback = hook_writ_path(&starts[0].1);
    if fallback_is_gone(&fallback) {
        return true;
    }
    let expected = expected_hooks(&fallback);
    let all_events: Vec<&str> = expected
        .iter()
        .map(|(e, _, _)| *e)
        .chain(LEGACY_HOOK_EVENTS.iter().copied())
        .collect();
    for event in all_events {
        let want: Vec<(Option<String>, String)> = expected
            .iter()
            .filter(|(e, _, _)| *e == event)
            .map(|(_, m, c)| (m.map(str::to_string), c.clone()))
            .collect();
        let mut have = entries(event);
        have.sort();
        let mut want = want;
        want.sort();
        if have != want {
            return true;
        }
    }
    false
}

/// Finding 89: an absolute fallback that no longer exists (its install was
/// removed) leaves the hook silent whenever PATH has no `writ`; doctor
/// reports it as version_skew so `writ init -y` records a live one.
fn fallback_is_gone(fallback: &str) -> bool {
    let p = Path::new(fallback);
    p.is_absolute() && !p.exists()
}

/// The writ path a generated hook command records: the fallback in today's
/// form (`F='<path>'; W=$(command -v writ ...`), or the pinned path in the
/// form 0.4.0 and earlier wrote (`&& <path> context --format brief ...`).
fn hook_writ_path(cmd: &str) -> String {
    if let Some(start) = cmd.rfind(HOOK_FALLBACK_PREFIX) {
        let rest = &cmd[start + HOOK_FALLBACK_PREFIX.len()..];
        if let Some(end) = rest.find(HOOK_FALLBACK_SUFFIX) {
            return rest[..end].replace(r"'\''", "'");
        }
    }
    const TAIL: &str = " context --format brief 2>/dev/null || true";
    cmd.strip_suffix(TAIL)
        .and_then(|head| head.rsplit("&& ").next())
        .unwrap_or("writ")
        .to_string()
}

/// Build the hook command, recording the writ that ran init as the fallback.
#[cfg(test)]
fn writ_hook_command() -> String {
    session_start_command(&init_writ_path())
}

/// The writ running init, spelled the way PATH spells it when PATH's first
/// `writ` is the same file (so a Homebrew symlink survives `brew upgrade`).
/// When the running binary is not a `writ` (the Python binding runs inside
/// the interpreter, tests inside the test harness), PATH's first `writ`; with
/// neither, bare `writ`.
fn init_writ_path() -> String {
    let on_path = crate::doctor::writs_on_path(std::env::var_os("PATH").as_deref())
        .into_iter()
        .next();
    let exe = std::env::current_exe()
        .ok()
        .filter(|p| p.file_stem().is_some_and(|s| s == "writ"));
    let same_file = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    };
    let chosen = match (exe, on_path) {
        (Some(exe), Some(p)) if same_file(&exe, &p) => p,
        (Some(exe), _) => exe,
        (None, Some(p)) => p,
        (None, None) => return "writ".to_string(),
    };
    chosen.to_string_lossy().into_owned()
}

/// Shell text between which the hook records the init-time writ.
const HOOK_FALLBACK_PREFIX: &str = "F='";
const HOOK_FALLBACK_SUFFIX: &str = "'; W=$(command -v writ";

/// The SessionStart hook: the same workflow steps and claim note as the
/// CLAUDE.md block (finding 67), then the brief context.
///
/// Finding 89: the hook runs the `writ` that PATH resolves when the session
/// starts, the same one the agent's own `writ` commands run, so an upgrade
/// through any channel (brew, pip, cargo) reaches the hook without a re-init,
/// and removing an older install (doctor's multiple-writs fix) never leaves
/// the hook pointing at a deleted binary. `fallback`, the writ that ran init,
/// runs only when PATH has no `writ` (an app launched outside a login shell).
/// When the two differ the hook prints one line naming both.
fn session_start_command(fallback: &str) -> String {
    let mut lines = vec![WRIT_HOOK_BANNER.to_string()];
    lines.extend(crate::template::auto_plain());
    lines.push(String::new());
    let echoes: Vec<String> = lines
        .iter()
        .map(|l| format!("echo '{}'", l.replace('\'', "'\\''")))
        .collect();
    let quoted = fallback.replace('\'', "'\\''");
    format!(
        "{} && {HOOK_FALLBACK_PREFIX}{quoted}{HOOK_FALLBACK_SUFFIX} 2>/dev/null) || W=\"$F\"; \
         [ \"$W\" = \"$F\" ] || echo \"Note: this session runs $W; writ init ran $F. \
         writ doctor lists every writ on PATH; writ init -y records the one you keep.\"; \
         \"$W\" context --format brief 2>/dev/null || true",
        echoes.join(" && ")
    )
}

/// The SessionEnd hook writ 0.4.2 wrote: records that the host ended the
/// session. Recognized for upgrade and removal; 0.4.3 runs [`auto_hook_command`]
/// on SessionEnd instead (which records the end too).
#[cfg(test)]
fn session_end_command(fallback: &str) -> String {
    let quoted = fallback.replace('\'', "'\\''");
    format!(
        "{HOOK_FALLBACK_PREFIX}{quoted}{HOOK_FALLBACK_SUFFIX} 2>/dev/null) || W=\"$F\"; \
         \"$W\" {SESSION_END_SUBCOMMAND} >/dev/null 2>&1 || true"
    )
}

/// The sprint 6 hook command, on every event writ hooks besides
/// SessionStart: runs `writ hook`, which reads the event from stdin
/// ([`crate::auto_seal`]). Same PATH-first resolution as the start hook
/// (finding 89). Always exits 0: a missing writ or a writ error never fails
/// the agent's tool call; stderr is discarded (writ logs to
/// `.writ/hooks.log`), stdout is the hook's answer to the agent.
fn auto_hook_command(fallback: &str) -> String {
    let quoted = fallback.replace('\'', "'\\''");
    format!(
        "{HOOK_FALLBACK_PREFIX}{quoted}{HOOK_FALLBACK_SUFFIX} 2>/dev/null) || W=\"$F\"; \
         \"$W\" {} 2>/dev/null || true",
        crate::auto_seal::HOOK_SUBCOMMAND
    )
}

/// Every hook entry writ installs: (event, matcher, command).
fn expected_hooks(fallback: &str) -> Vec<(&'static str, Option<&'static str>, String)> {
    let auto = auto_hook_command(fallback);
    let matcher = crate::auto_seal::POST_TOOL_MATCHER;
    vec![
        (WRIT_HOOK_EVENT, None, session_start_command(fallback)),
        ("PreToolUse", Some("Bash"), auto.clone()),
        ("PostToolUse", Some(matcher), auto.clone()),
        ("UserPromptSubmit", None, auto.clone()),
        ("Stop", None, auto.clone()),
        ("SubagentStop", None, auto.clone()),
        (WRIT_END_HOOK_EVENT, None, auto),
    ]
}

/// Events writ may hold a hook on, current and legacy (for removal).
const WRIT_AUTO_EVENTS: &[&str] = &[
    "PreToolUse",
    "PostToolUse",
    "UserPromptSubmit",
    "Stop",
    "SubagentStop",
    "SessionEnd",
];

/// The hidden CLI subcommand the 0.4.2 SessionEnd hook runs.
pub const SESSION_END_SUBCOMMAND: &str = "session-end";

/// Text every writ 0.4.2 SessionEnd hook command contains.
const WRIT_END_HOOK_MARKER: &str = "\"$W\" session-end";

/// Text every sprint 6 `writ hook` command contains.
const WRIT_AUTO_HOOK_MARKER: &str = "\"$W\" hook ";

/// The hook event where writ records a host-reported session end.
const WRIT_END_HOOK_EVENT: &str = "SessionEnd";

/// The hook event where writ injects context: once, when a session begins.
///
/// Context is never injected per prompt; agents pull scoped context with
/// `writ context --spec <id>` when they need it.
const WRIT_HOOK_EVENT: &str = "SessionStart";

/// Events earlier writ versions hooked. Init and uninit remove writ entries
/// from these so upgrades do not keep injecting context on every prompt.
const LEGACY_HOOK_EVENTS: &[&str] = &["UserPromptSubmit"];

/// True for a hook command writ generated, current or older, including a
/// hand-edited one that kept either marker (finding 72).
fn is_writ_hook_command(cmd: &str) -> bool {
    cmd.contains(WRIT_HOOK_MARKER)
        || cmd.contains(WRIT_HOOK_BANNER)
        || cmd.contains(WRIT_END_HOOK_MARKER)
        || cmd.contains(WRIT_AUTO_HOOK_MARKER)
}

/// First line every writ SessionStart hook prints.
const WRIT_HOOK_BANNER: &str = "## Writ VCS Active";

/// Remove writ hook entries from a hook event array. Returns true if any were removed.
fn remove_writ_hook_entries(arr: &mut Vec<serde_json::Value>) -> bool {
    let before = arr.len();
    arr.retain(|entry| {
        !entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .is_some_and(|inner| {
                inner.iter().any(|hook| {
                    hook.get("command")
                        .and_then(|c| c.as_str())
                        .is_some_and(is_writ_hook_command)
                })
            })
    });
    arr.len() < before
}

/// Install the writ `SessionStart` hook in `.claude/settings.json`.
///
/// The hook runs `writ context --format brief` once per session. Any writ
/// hook already present (on `SessionStart` or a legacy event such as
/// `UserPromptSubmit`) is replaced, never appended to. Non-writ hooks are
/// left untouched. Returns `Some(path)` only when the file changed.
fn ensure_claude_hook(root: &Path) -> WritResult<Option<String>> {
    let claude_dir = root.join(".claude");
    if !claude_dir.exists() {
        fs::create_dir_all(&claude_dir)?;
    }

    let settings_path = claude_dir.join("settings.json");
    let mut settings = read_settings(&settings_path)?;
    let Some(settings_obj) = settings.as_object_mut() else {
        return Ok(None); // Top level isn't an object — don't touch it
    };
    let original = serde_json::Value::Object(settings_obj.clone());

    let hooks = settings_obj
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let Some(hooks_obj) = hooks.as_object_mut() else {
        return Ok(None); // hooks field exists but isn't an object — don't touch it
    };

    for event_name in LEGACY_HOOK_EVENTS {
        remove_writ_hooks_from_event(hooks_obj, event_name);
    }

    let fallback = init_writ_path();
    for (event, matcher, command) in expected_hooks(&fallback) {
        let event_hooks = hooks_obj
            .entry(event)
            .or_insert_with(|| serde_json::json!([]));
        let Some(arr) = event_hooks.as_array_mut() else {
            continue; // Not an array: the user's own shape, left alone.
        };
        remove_writ_hook_entries(arr);
        let mut entry = serde_json::json!({
            "hooks": [
                {
                    "type": "command",
                    "command": command,
                    "timeout": 10
                }
            ]
        });
        if let Some(m) = matcher {
            entry["matcher"] = serde_json::json!(m);
        }
        arr.push(entry);
    }

    if settings == original {
        return Ok(None);
    }
    write_settings(&settings_path, &settings)?;
    Ok(Some(".claude/settings.json".to_string()))
}

/// Remove writ entries from one hook event. Drops the event key when writ's
/// removal leaves it empty. Returns true if anything was removed.
fn remove_writ_hooks_from_event(
    hooks_obj: &mut serde_json::Map<String, serde_json::Value>,
    event_name: &str,
) -> bool {
    let Some(arr) = hooks_obj.get_mut(event_name).and_then(|v| v.as_array_mut()) else {
        return false;
    };
    if !remove_writ_hook_entries(arr) {
        return false;
    }
    if arr.is_empty() {
        hooks_obj.remove(event_name);
    }
    true
}

/// Read `.claude/settings.json`, or `{}` when absent. An unparseable file is
/// an error, never silently replaced (it holds the user's own settings).
fn read_settings(path: &Path) -> WritResult<serde_json::Value> {
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let content = fs::read_to_string(path)?;
    serde_json::from_str(&content).map_err(|e| {
        crate::error::WritError::Other(format!(
            "{} is not valid JSON ({e}); left unchanged, fix it and re-run writ init",
            path.display()
        ))
    })
}

fn write_settings(path: &Path, settings: &serde_json::Value) -> WritResult<()> {
    let json = serde_json::to_string_pretty(settings)
        .map_err(|e| crate::error::WritError::Other(format!("JSON serialize: {}", e)))?;
    atomic_write(path, format!("{}\n", json).as_bytes())
}

/// Remove writ hooks from `.claude/settings.json` during uninit.
///
/// Removes exactly what init writes: writ entries on `SessionStart` and the
/// legacy events, plus any event key or `hooks` object left empty by that
/// removal. Other hooks and settings are preserved.
fn remove_claude_hook(root: &Path) -> WritResult<Option<String>> {
    let settings_path = root.join(".claude").join("settings.json");
    if !settings_path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&settings_path)?;
    let mut settings: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let Some(settings_obj) = settings.as_object_mut() else {
        return Ok(None);
    };
    let Some(hooks_obj) = settings_obj
        .get_mut("hooks")
        .and_then(|h| h.as_object_mut())
    else {
        return Ok(None);
    };

    let mut any_removed = false;
    for event_name in std::iter::once(&WRIT_HOOK_EVENT)
        .chain(WRIT_AUTO_EVENTS)
        .chain(LEGACY_HOOK_EVENTS)
    {
        any_removed |= remove_writ_hooks_from_event(hooks_obj, event_name);
    }
    if !any_removed {
        return Ok(None);
    }
    if hooks_obj.is_empty() {
        settings_obj.remove("hooks");
    }

    write_settings(&settings_path, &settings)?;
    Ok(Some(".claude/settings.json".to_string()))
}

/// Generate writ integration hooks for Claude Code.
pub fn hook_claude_code(root: &Path) -> WritResult<HookResult> {
    let mut created = Vec::new();
    let mut updated = Vec::new();

    let claude_md = root.join("CLAUDE.md");
    let writ_section = writ_claude_md_section();
    let marked_section = wrap_with_markers(&writ_section);

    if claude_md.exists() {
        let content = fs::read_to_string(&claude_md)?;
        if content.contains(MARKER_BEGIN) {
            // Reinit: replace existing marker-delimited section in place.
            let new_content = replace_marked_section(&content, &marked_section);
            if new_content != content {
                atomic_write(&claude_md, new_content.as_bytes())?;
                updated.push("CLAUDE.md".to_string());
            }
        } else if has_legacy_writ_heading(&content) {
            // Legacy: old heading-based section exists. Replace it with marked version.
            let cleaned = remove_writ_section(&content);
            let new_content = if cleaned.trim().is_empty() {
                marked_section
            } else {
                format!("{}\n\n{}", cleaned.trim_end(), marked_section)
            };
            atomic_write(&claude_md, new_content.as_bytes())?;
            updated.push("CLAUDE.md".to_string());
        } else {
            // Prepend marked section to existing file (writ section first for visibility).
            let new_content = format!(
                "{}\n\n---\n\n{}\n",
                marked_section.trim_end(),
                content.trim()
            );
            atomic_write(&claude_md, new_content.as_bytes())?;
            updated.push("CLAUDE.md".to_string());
        }
    } else {
        atomic_write(&claude_md, marked_section.as_bytes())?;
        created.push("CLAUDE.md".to_string());
    }

    // Generate all slash command files.
    let sc_result = crate::slash_commands::generate_slash_commands(root)?;
    created.extend(sc_result.created);
    updated.extend(sc_result.updated);

    // Generate all skill directories (dual mode: skills + slash commands).
    let sk_result = crate::skills::generate_skills(root)?;
    if sk_result.created > 0 {
        created.push(format!(
            ".claude/skills/writ-*/ ({} skills, auto-invoke enabled)",
            sk_result.created
        ));
    }
    if sk_result.updated > 0 {
        updated.push(format!(
            ".claude/skills/writ-*/ ({} updated)",
            sk_result.updated
        ));
    }

    // Ensure Bash(writ *) permission in .claude/settings.json.
    match ensure_claude_permissions(root) {
        Ok(Some(path)) => {
            if root
                .join(".claude")
                .join("settings.json")
                .metadata()
                .is_ok()
            {
                // File existed before or was just created — either way it's been set up.
                updated.push(path);
            } else {
                created.push(path);
            }
        }
        Ok(None) => {} // Already had the permission
        Err(e) => {
            eprintln!(
                "warning: could not configure Claude Code permissions: {}",
                e
            );
        }
    }

    // Add writ usage instruction to .claude/settings.json.
    match ensure_claude_instructions(root) {
        Ok(Some(_)) => {} // settings.json already tracked above
        Ok(None) => {}    // Already had the instruction
        Err(e) => {
            eprintln!("warning: could not add Claude Code instruction: {}", e);
        }
    }

    // Add the SessionStart hook (brief context once per session).
    match ensure_claude_hook(root) {
        Ok(Some(_)) => {} // settings.json already tracked above
        Ok(None) => {}    // Already had the hook
        Err(e) => {
            eprintln!("warning: could not add Claude Code hook: {}", e);
        }
    }

    Ok(HookResult {
        framework: Framework::ClaudeCode,
        files_created: created,
        files_updated: updated,
    })
}

/// Generate writ integration hooks for Codex / AGENTS.md.
pub fn hook_codex(root: &Path) -> WritResult<HookResult> {
    let mut created = Vec::new();
    let mut updated = Vec::new();

    let agents_md = root.join("AGENTS.md");
    let writ_section = writ_agents_md_section();
    let marked_section = wrap_with_markers(&writ_section);

    if agents_md.exists() {
        let content = fs::read_to_string(&agents_md)?;
        if content.contains(MARKER_BEGIN) {
            let new_content = replace_marked_section(&content, &marked_section);
            if new_content != content {
                atomic_write(&agents_md, new_content.as_bytes())?;
                updated.push("AGENTS.md".to_string());
            }
        } else if has_legacy_writ_heading(&content) {
            let cleaned = remove_writ_section(&content);
            let new_content = if cleaned.trim().is_empty() {
                marked_section
            } else {
                format!("{}\n\n{}", cleaned.trim_end(), marked_section)
            };
            atomic_write(&agents_md, new_content.as_bytes())?;
            updated.push("AGENTS.md".to_string());
        } else {
            let new_content = format!("{}\n\n---\n\n{}", content.trim_end(), marked_section);
            atomic_write(&agents_md, new_content.as_bytes())?;
            updated.push("AGENTS.md".to_string());
        }
    } else {
        atomic_write(&agents_md, marked_section.as_bytes())?;
        created.push("AGENTS.md".to_string());
    }

    Ok(HookResult {
        framework: Framework::Codex,
        files_created: created,
        files_updated: updated,
    })
}

/// Install hooks for all detected frameworks.
pub fn install_hooks(root: &Path) -> WritResult<Vec<HookResult>> {
    let detections = detect_frameworks(root);
    let mut results = Vec::new();

    for d in &detections {
        if d.detected {
            let result = match d.framework {
                Framework::ClaudeCode => hook_claude_code(root)?,
                Framework::Codex => hook_codex(root)?,
                Framework::Custom => continue,
            };
            results.push(result);
        }
    }

    // Always generate generic agent instructions.
    let generic_result = hook_generic(root)?;
    if !generic_result.files_created.is_empty() || !generic_result.files_updated.is_empty() {
        results.push(generic_result);
    }

    // Ensure .gitignore has .writ/ entry.
    append_gitignore(root)?;

    Ok(results)
}

/// Generate generic agent instructions in .writ/AGENT_INSTRUCTIONS.md.
pub fn hook_generic(root: &Path) -> WritResult<HookResult> {
    let mut created = Vec::new();
    let mut updated = Vec::new();

    let writ_dir = root.join(".writ");
    let instructions_path = writ_dir.join("AGENT_INSTRUCTIONS.md");
    let content = agent_instructions_content();

    if instructions_path.exists() {
        let existing = fs::read_to_string(&instructions_path)?;
        if existing != content {
            atomic_write(&instructions_path, content.as_bytes())?;
            updated.push(".writ/AGENT_INSTRUCTIONS.md".to_string());
        }
    } else if writ_dir.exists() {
        atomic_write(&instructions_path, content.as_bytes())?;
        created.push(".writ/AGENT_INSTRUCTIONS.md".to_string());
    }

    Ok(HookResult {
        framework: Framework::Custom,
        files_created: created,
        files_updated: updated,
    })
}

/// Append `.writ/` entry to .gitignore if not already present.
/// Creates .gitignore if it doesn't exist. Idempotent.
pub fn append_gitignore(root: &Path) -> WritResult<bool> {
    let gitignore_path = root.join(".gitignore");
    let writ_entry = ".writ/";

    if gitignore_path.exists() {
        let content = fs::read_to_string(&gitignore_path)?;
        // Check if any line is exactly ".writ/" (ignoring whitespace).
        let already_present = content.lines().any(|line| line.trim() == writ_entry);
        if already_present {
            return Ok(false);
        }
        // Append with a newline separator if file doesn't end with one.
        let separator = if content.ends_with('\n') { "" } else { "\n" };
        let new_content = format!(
            "{}{}\n# Writ version control state\n{}\n",
            content, separator, writ_entry
        );
        atomic_write(&gitignore_path, new_content.as_bytes())?;
    } else {
        let content = format!("# Writ version control state\n{}\n", writ_entry);
        atomic_write(&gitignore_path, content.as_bytes())?;
    }

    Ok(true)
}

/// Result of removing framework hooks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UninstallHookResult {
    pub framework: Framework,
    pub files_removed: Vec<String>,
    pub files_updated: Vec<String>,
}

/// Remove writ integration hooks for Claude Code.
pub fn unhook_claude_code(root: &Path) -> WritResult<UninstallHookResult> {
    let mut removed = Vec::new();
    let mut updated = Vec::new();

    // Remove writ section from CLAUDE.md (or delete if we created the whole file).
    let claude_md = root.join("CLAUDE.md");
    if claude_md.exists() {
        let content = fs::read_to_string(&claude_md)?;
        let has_markers = content.contains(MARKER_BEGIN);
        let has_legacy = has_legacy_writ_heading(&content);

        if has_markers || has_legacy {
            let cleaned = if has_markers {
                remove_marked_section(&content)
            } else {
                remove_writ_section(&content)
            };
            if cleaned.trim().is_empty() {
                fs::remove_file(&claude_md)?;
                removed.push("CLAUDE.md".to_string());
                eprintln!("notice: CLAUDE.md contained only writ content — removed");
            } else {
                atomic_write(&claude_md, cleaned.as_bytes())?;
                updated.push("CLAUDE.md".to_string());
            }
        }
    }

    // Remove all writ slash command files.
    let sc_removed = crate::slash_commands::remove_slash_commands(root)?;
    removed.extend(sc_removed);

    // Remove all writ skill directories.
    let sk_removed = crate::skills::remove_skills(root)?;
    removed.extend(sk_removed);

    // Remove Bash(writ *) permission from .claude/settings.json.
    match remove_claude_permissions(root) {
        Ok(Some(path)) => {
            updated.push(path);
        }
        Ok(None) => {} // Permission wasn't there or file doesn't exist
        Err(e) => {
            eprintln!("warning: could not remove Claude Code permissions: {}", e);
        }
    }

    // Remove writ instruction from .claude/settings.json.
    match remove_claude_instructions(root) {
        Ok(Some(_)) => {} // settings.json already tracked above
        Ok(None) => {}
        Err(e) => {
            eprintln!("warning: could not remove Claude Code instruction: {}", e);
        }
    }

    // Remove writ hooks (SessionStart and legacy events) from .claude/settings.json.
    match remove_claude_hook(root) {
        Ok(Some(_)) => {} // settings.json already tracked above
        Ok(None) => {}
        Err(e) => {
            eprintln!("warning: could not remove Claude Code hook: {}", e);
        }
    }

    Ok(UninstallHookResult {
        framework: Framework::ClaudeCode,
        files_removed: removed,
        files_updated: updated,
    })
}

/// Remove writ integration hooks for Codex / AGENTS.md.
pub fn unhook_codex(root: &Path) -> WritResult<UninstallHookResult> {
    let mut removed = Vec::new();
    let mut updated = Vec::new();

    let agents_md = root.join("AGENTS.md");
    if agents_md.exists() {
        let content = fs::read_to_string(&agents_md)?;
        let has_markers = content.contains(MARKER_BEGIN);
        let has_legacy = has_legacy_writ_heading(&content);

        if has_markers || has_legacy {
            let cleaned = if has_markers {
                remove_marked_section(&content)
            } else {
                remove_writ_section(&content)
            };
            if cleaned.trim().is_empty() {
                fs::remove_file(&agents_md)?;
                removed.push("AGENTS.md".to_string());
                eprintln!("notice: AGENTS.md contained only writ content — removed");
            } else {
                atomic_write(&agents_md, cleaned.as_bytes())?;
                updated.push("AGENTS.md".to_string());
            }
        }
    }

    Ok(UninstallHookResult {
        framework: Framework::Codex,
        files_removed: removed,
        files_updated: updated,
    })
}

/// Remove generic agent instructions.
pub fn unhook_generic(root: &Path) -> WritResult<UninstallHookResult> {
    let mut removed = Vec::new();

    let instructions_path = root.join(".writ").join("AGENT_INSTRUCTIONS.md");
    if instructions_path.exists() {
        fs::remove_file(&instructions_path)?;
        removed.push(".writ/AGENT_INSTRUCTIONS.md".to_string());
    }

    Ok(UninstallHookResult {
        framework: Framework::Custom,
        files_removed: removed,
        files_updated: Vec::new(),
    })
}

/// Remove `.writ/` entry from .gitignore. Cleans up the comment header too.
pub fn remove_gitignore_entry(root: &Path) -> WritResult<bool> {
    let gitignore_path = root.join(".gitignore");
    if !gitignore_path.exists() {
        return Ok(false);
    }

    let content = fs::read_to_string(&gitignore_path)?;
    let mut new_lines: Vec<&str> = Vec::new();
    let mut removed_something = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == ".writ/" || trimmed == ".writ" || trimmed == "# Writ version control state" {
            removed_something = true;
            continue;
        }
        new_lines.push(line);
    }

    if !removed_something {
        return Ok(false);
    }

    let new_content = new_lines.join("\n");
    let trimmed = new_content.trim();
    if trimmed.is_empty() {
        fs::remove_file(&gitignore_path)?;
    } else {
        let final_content = format!("{}\n", trimmed);
        atomic_write(&gitignore_path, final_content.as_bytes())?;
    }

    Ok(true)
}

/// Remove hooks for all detected frameworks.
pub fn uninstall_hooks(root: &Path) -> WritResult<Vec<UninstallHookResult>> {
    let mut results = Vec::new();

    // Always attempt all — even if framework isn't "detected" now,
    // hooks from a previous install may still exist.
    let claude_result = unhook_claude_code(root)?;
    if !claude_result.files_removed.is_empty() || !claude_result.files_updated.is_empty() {
        results.push(claude_result);
    }

    let codex_result = unhook_codex(root)?;
    if !codex_result.files_removed.is_empty() || !codex_result.files_updated.is_empty() {
        results.push(codex_result);
    }

    let generic_result = unhook_generic(root)?;
    if !generic_result.files_removed.is_empty() {
        results.push(generic_result);
    }

    // Clean .gitignore.
    remove_gitignore_entry(root)?;

    Ok(results)
}

/// Check if content contains a legacy writ heading (`## Writ` or `## Writ <something>`).
/// Does not match unrelated headings like `## Writing Style Guide`.
fn has_legacy_writ_heading(content: &str) -> bool {
    content.lines().any(|line| {
        line == "## Writ" || line.starts_with("## Writ ") || line.starts_with("## Writ\t")
    })
}

/// Wrap content with BEGIN/END marker comments.
fn wrap_with_markers(content: &str) -> String {
    format!("{}\n{}\n{}\n", MARKER_BEGIN, content.trim_end(), MARKER_END)
}

/// Replace the marker-delimited section in content with a new marked section.
fn replace_marked_section(content: &str, new_marked_section: &str) -> String {
    let mut result = String::new();
    let mut in_marked = false;
    let mut replaced = false;

    for line in content.lines() {
        if line.contains(MARKER_BEGIN) {
            in_marked = true;
            if !replaced {
                result.push_str(new_marked_section.trim_end());
                result.push('\n');
                replaced = true;
            }
            continue;
        }
        if line.contains(MARKER_END) {
            in_marked = false;
            continue;
        }
        if !in_marked {
            result.push_str(line);
            result.push('\n');
        }
    }

    result
}

/// Remove the marker-delimited section from content.
/// Also removes the `---` separator line immediately before the markers if present.
fn remove_marked_section(content: &str) -> String {
    let mut result = String::new();
    let mut in_marked = false;

    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        if lines[i].contains(MARKER_BEGIN) {
            // Remove preceding separator (blank line + `---` + blank line pattern).
            let trimmed = result.trim_end().to_string();
            if trimmed.ends_with("---") {
                // Remove the trailing `---` and any blank line before it.
                let without_separator = trimmed.trim_end_matches("---").trim_end();
                result = if without_separator.is_empty() {
                    String::new()
                } else {
                    format!("{}\n", without_separator)
                };
            }
            in_marked = true;
            i += 1;
            continue;
        }
        if lines[i].contains(MARKER_END) {
            in_marked = false;
            i += 1;
            continue;
        }
        if !in_marked {
            result.push_str(lines[i]);
            result.push('\n');
        }
        i += 1;
    }

    // Trim trailing whitespace/newlines left by the removal.
    let trimmed = result.trim().to_string();
    // Also strip any leading separator (--- at the start of remaining content).
    let trimmed = if trimmed.starts_with("---") {
        trimmed
            .strip_prefix("---")
            .unwrap_or(&trimmed)
            .trim_start()
            .to_string()
    } else {
        trimmed
    };
    if trimmed.is_empty() {
        trimmed
    } else {
        trimmed + "\n"
    }
}

/// Remove the `## Writ` section and everything after it until the next `## ` heading
/// (or end of file). Preserves content before and after.
/// Legacy support for pre-marker installs.
fn remove_writ_section(content: &str) -> String {
    let mut result = String::new();
    let mut in_writ_section = false;

    for line in content.lines() {
        if line == "## Writ" || line.starts_with("## Writ ") || line.starts_with("## Writ\t") {
            in_writ_section = true;
            continue;
        }
        if in_writ_section && line.starts_with("## ") {
            // Hit the next section — stop skipping.
            in_writ_section = false;
        }
        if !in_writ_section {
            result.push_str(line);
            result.push('\n');
        }
    }

    // Trim trailing whitespace/newlines left by the removal.
    let trimmed = result.trim_end().to_string();
    if trimmed.is_empty() {
        trimmed
    } else {
        trimmed + "\n"
    }
}

/// Fill a template's placeholders from [`crate::template`] (finding 67).
trait PipeTemplate {
    fn pipe_template(&self) -> String;
}

impl PipeTemplate for str {
    fn pipe_template(&self) -> String {
        self.replace("@STEPS@", &crate::template::workflow_markdown())
            .replace("@AUTO@", &crate::template::auto_markdown())
            .replace("@SPEC_ADD@", crate::template::SPEC_ADD)
            .replace("@SEAL@", crate::template::SEAL)
            .replace("@SPEC_DONE@", crate::template::SPEC_DONE)
            .replace("@CLAIM_NOTE@", crate::template::CLAIM_NOTE)
    }
}

/// The CLAUDE.md managed block (sprint 6: zero instructions; sealing is
/// the hooks' job). Under 5 lines, no mandatory command.
fn writ_claude_md_section() -> String {
    "## Writ\n@AUTO@".pipe_template()
}

/// The AGENTS.md managed block: the CLAUDE.md block's lines.
fn writ_agents_md_section() -> String {
    "## Writ\n@AUTO@".pipe_template()
}

/// Content for `.writ/AGENT_INSTRUCTIONS.md`.
fn agent_instructions_content() -> String {
    r#"# Writ — Agent Workflow Instructions (MANDATORY)

Include these instructions in your agent's system prompt or configuration.
These are mandatory requirements, not suggestions.

---

## Setup

This project uses writ for version control. The `writ` CLI is available in PATH.

## Required Workflow

You MUST follow these steps. They are not optional.

@STEPS@
Work in small increments, and check `writ context` periodically to see what other agents have done.

## Agent Identity

writ resolves who you are in this order: `--agent <name>`, then the
`WRIT_AGENT_ID` environment variable, then the project's `default_agent`
setting, then your framework session (for example `claude-code-a3f2`), then
`human`. Subagents spawned by a hub inherit the hub's session and would all
share one identity: give each its own with `WRIT_AGENT_ID=<name>` on every
writ command, or `--agent <name>`.

`writ spec add` records you as the creator. @CLAIM_NOTE@
`writ spec release <id>` gives a claim back (the holder only, or `--force`).

## Spec Lifecycle

Specs move through a defined lifecycle:

    active → completed → committed

- **active**: Work in progress. Seals are being created.
- **completed**: `writ spec done` was run. Work is done but not in git yet.
- **committed**: The user ran `writ finish` to promote the work to a git commit.

The user controls when completed work becomes a git commit via `writ finish`.
Agents should never run `writ finish` or `git commit`.

## Token-Efficient Context

writ supports multiple output formats. For LLM consumption, use TOON:

    writ context --format toon

TOON (Token-Oriented Object Notation) provides the same structured data as JSON
in 20-33% fewer bytes. Field names are declared once, rows are streamed as values.

Available formats:
- toon        Token-optimized (recommended for agents)
- json        Standard JSON (default, maximum compatibility)
- json-compact  Minified JSON

## Key Commands

- `writ context` — structured project state
- `writ context --spec <id>` — context scoped to a specific task
- `writ seal -s "<summary>" --paths <changed files, comma-separated>` — checkpoint work (auto-scoped to your spec)
- `@SPEC_ADD@` — create a task spec (ID auto-generated)
- `writ spec status` — view active specs
- `@SPEC_DONE@` — mark your task complete (auto-scoped to your spec)
- `writ status` — project overview (agents, specs, progress)
- `writ diff` — preview file changes
- `writ log` — recent seal history
- `writ restore <seal-id>` — roll back to a previous seal (overwrites working directory files)

## Rules

- You MUST run `writ context` before starting work
- You MUST run `writ seal -s "<summary>" --agent claude-code --paths <changed files, comma-separated>` after each meaningful unit of work
- You MUST run `@SPEC_DONE@` before reporting results to the user
- Your task is NOT complete until sealed
- Include meaningful summaries in seals for other agents' context
- `--scope` on `writ spec add` declares the files your task owns; `writ seal` without `--paths` then captures only those. Otherwise pass `--paths` with the files you changed: another agent's pending files are never sealed for you
- If `writ seal` or `writ spec done` prints `left out:` or `NOT SEALED:`, read the reason and run the command it prints if those files are yours
- Include test results when available: `--tests-passed N --tests-failed M`
- Do NOT run `git add`, `git commit`, or `git push` — the user manages git via `writ finish`
- Do NOT run `writ finish` — this is the git integration command managed by the user or orchestrator

## Integration Examples

### System Prompt Snippet
Add to your agent's system prompt:
"MANDATORY: This project uses writ for version control. You MUST run `writ context`
before starting work. You MUST run `writ seal -s '<summary>' --paths <changed files, comma-separated> --agent claude-code` after each unit of work.
You MUST run `writ spec done <id>` before reporting results. Your task is NOT complete
until sealed. NEVER run git commit or writ finish."

### Tool Definition (for function-calling agents)
```json
{
  "name": "writ_seal",
  "description": "Create a version control checkpoint",
  "parameters": {
    "summary": { "type": "string", "description": "What was accomplished" },
    "spec_id": { "type": "string", "description": "Spec to link this seal to" }
  }
}
```

```json
{
  "name": "writ_spec_done",
  "description": "Mark a task spec as complete (creates final seal)",
  "parameters": {
    "spec_id": { "type": "string", "description": "ID of the spec to complete" },
    "summary": { "type": "string", "description": "Optional completion summary" }
  }
}
```
"#
    .pipe_template()
}

/// Host-recorded session ends (sprint 5): the second proof for
/// `writ spec release --stale` besides the pid.
///
/// Claude Code runs the generated SessionEnd hook when a session ends; the
/// hook runs `writ session-end`, which writes one record for the session
/// process (the same process a claim records, found the same way) under
/// `.writ/agents/claudecode/ended/<session key>.json`. A claim whose pid
/// cannot be judged here (another host, no process table) is proven gone
/// when such a record matches its host, pid and pid start time. Reading is
/// read-only: doctor and `--stale` never write a record. A live pid always
/// wins over a record, and `/clear` or `/resume` (the process continues)
/// write none.
pub mod session_end {
    use std::path::{Path, PathBuf};

    use serde::{Deserialize, Serialize};

    use crate::agent::session::{self, ProcInfo};
    use crate::doctor::StaleVerdict;
    use crate::error::WritResult;
    use crate::fsutil::atomic_write;

    /// Directory under `.writ` holding the records.
    pub const DIR: &str = "agents/claudecode/ended";

    /// SessionEnd reasons after which the session process keeps running
    /// (`/clear`, `/resume` switch conversations in the same process); no
    /// record is written for them, so the pid proof still applies.
    pub const PROCESS_CONTINUES: &[&str] = &["clear", "resume"];

    /// A claim the session held when it ended (informational).
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct EndedClaim {
        pub spec: String,
        pub agent: String,
    }

    /// One host-recorded session end.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct SessionEndRecord {
        /// Host the session ran on.
        pub host: String,
        /// The session process (as a claim records it).
        pub pid: u32,
        /// Its start time as `ps -o lstart` prints it.
        pub pid_start: String,
        /// When writ wrote the record (RFC 3339, UTC).
        pub ended_at: String,
        /// The host's reason (Claude Code's `reason`), when given.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub reason: Option<String>,
        /// The host's own session id, when given.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub host_session_id: Option<String>,
        /// Open claims recorded by this session process at its end.
        #[serde(default)]
        pub claims: Vec<EndedClaim>,
    }

    /// Path of the record for a session process.
    pub fn record_path(writ_dir: &Path, pid: u32, pid_start: &str) -> PathBuf {
        let key = session::session_key(&ProcInfo {
            pid,
            ppid: 0,
            start: pid_start.to_string(),
            comm: String::new(),
        });
        writ_dir.join(DIR).join(format!("{key}.json"))
    }

    /// Fields of the hook's stdin JSON writ uses; anything unparseable is
    /// treated as empty (a record is still written).
    #[derive(Debug, Default, Deserialize)]
    struct HookInput {
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        session_id: Option<String>,
    }

    /// Build the record for `proc` on `host` (pure; see [`record_current`]).
    /// None when the reason means the process continues.
    pub fn build_record(
        writ_dir: &Path,
        proc: &ProcInfo,
        host: &str,
        hook_input: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Option<SessionEndRecord> {
        let input: HookInput = serde_json::from_str(hook_input.trim()).unwrap_or_default();
        if input
            .reason
            .as_deref()
            .is_some_and(|r| PROCESS_CONTINUES.contains(&r))
        {
            return None;
        }
        let mut claims: Vec<EndedClaim> = open_specs(writ_dir)
            .into_iter()
            .filter(|s| {
                s.claimed_pid == Some(proc.pid)
                    && s.claimed_pid_start.as_deref() == Some(proc.start.as_str())
                    && s.claimed_host.as_deref() == Some(host)
            })
            .filter_map(|s| s.claimed_by.map(|agent| EndedClaim { spec: s.id, agent }))
            .collect();
        claims.sort_by(|a, b| a.spec.cmp(&b.spec));
        Some(SessionEndRecord {
            host: host.to_string(),
            pid: proc.pid,
            pid_start: proc.start.clone(),
            ended_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            reason: input.reason.filter(|r| !r.is_empty()),
            host_session_id: input.session_id.filter(|r| !r.is_empty()),
            claims,
        })
    }

    /// `writ session-end`: record that the host ended this session. Returns
    /// the record's path, or None when nothing was written (the session
    /// process or host name cannot be found, or the process continues).
    pub fn record_current(writ_dir: &Path, hook_input: &str) -> WritResult<Option<PathBuf>> {
        let (Some(proc), Some(host)) = (session::current_session_process(), session::host_name())
        else {
            return Ok(None);
        };
        let Some(record) = build_record(writ_dir, &proc, &host, hook_input, chrono::Utc::now())
        else {
            return Ok(None);
        };
        let path = record_path(writ_dir, record.pid, &record.pid_start);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(&record)
            .map_err(|e| crate::error::WritError::Other(format!("JSON serialize: {e}")))?;
        atomic_write(&path, format!("{json}\n").as_bytes())?;
        Ok(Some(path))
    }

    /// The record matching a claim's host, pid and pid start, if any.
    /// Read-only.
    pub fn record_for(writ_dir: &Path, spec: &crate::spec::Spec) -> Option<SessionEndRecord> {
        let (Some(pid), Some(start), Some(host)) = (
            spec.claimed_pid,
            spec.claimed_pid_start.as_deref(),
            spec.claimed_host.as_deref(),
        ) else {
            return None;
        };
        let text = std::fs::read_to_string(record_path(writ_dir, pid, start)).ok()?;
        let record: SessionEndRecord = serde_json::from_str(&text).ok()?;
        (record.pid == pid && record.pid_start == start && record.host == host).then_some(record)
    }

    /// Upgrade an `Unproven` verdict to `Gone` when a matching record shows
    /// the host ended the holder's session. `Gone` and `Alive` are returned
    /// unchanged: the pid proof, when it can be judged, decides.
    pub fn apply(writ_dir: &Path, spec: &crate::spec::Spec, verdict: StaleVerdict) -> StaleVerdict {
        match verdict {
            StaleVerdict::Unproven(why) => match record_for(writ_dir, spec) {
                Some(r) => StaleVerdict::Gone(proof_text(&r)),
                None => StaleVerdict::Unproven(why),
            },
            other => other,
        }
    }

    /// The proof sentence for a record.
    pub fn proof_text(r: &SessionEndRecord) -> String {
        let reason = r
            .reason
            .as_deref()
            .map(|x| format!(", reason '{x}'"))
            .unwrap_or_default();
        format!(
            "Claude Code ended the holder's session on host '{}' (pid {}, started {}) at {}{reason}, recorded by its SessionEnd hook",
            r.host, r.pid, r.pid_start, r.ended_at
        )
    }

    /// Open specs with a claim, read straight from `.writ/specs` (the hook
    /// runs without opening a repository; unreadable files are skipped).
    fn open_specs(writ_dir: &Path) -> Vec<crate::spec::Spec> {
        let Ok(entries) = std::fs::read_dir(writ_dir.join("specs")) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .filter_map(|t| serde_json::from_str::<crate::spec::Spec>(&t).ok())
            .filter(|s| s.claimed_by.is_some())
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn proc() -> ProcInfo {
            ProcInfo {
                pid: 4242,
                ppid: 1,
                start: "Sun Oct 4 13:59:41 2026".into(),
                comm: "claude".into(),
            }
        }

        fn claimed(
            dir: &Path,
            id: &str,
            agent: &str,
            host: &str,
            p: &ProcInfo,
        ) -> crate::spec::Spec {
            let mut s = crate::spec::Spec::new(id.into(), id.into(), String::new());
            s.claimed_by = Some(agent.into());
            s.claimed_pid = Some(p.pid);
            s.claimed_pid_start = Some(p.start.clone());
            s.claimed_host = Some(host.into());
            std::fs::create_dir_all(dir.join("specs")).unwrap();
            std::fs::write(
                dir.join("specs").join(format!("{id}.json")),
                serde_json::to_string(&s).unwrap(),
            )
            .unwrap();
            s
        }

        fn write(dir: &Path, r: &SessionEndRecord) {
            let path = record_path(dir, r.pid, &r.pid_start);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, serde_json::to_string(r).unwrap()).unwrap();
        }

        #[test]
        fn record_lists_only_this_sessions_claims() {
            let dir = tempfile::tempdir().unwrap();
            let p = proc();
            claimed(dir.path(), "mine", "a", "h", &p);
            let other = ProcInfo { pid: 7, ..proc() };
            claimed(dir.path(), "theirs", "b", "h", &other);
            claimed(dir.path(), "far", "c", "elsewhere", &p);
            let r = build_record(
                dir.path(),
                &p,
                "h",
                r#"{"session_id":"s1","reason":"prompt_input_exit","hook_event_name":"SessionEnd"}"#,
                chrono::Utc::now(),
            )
            .unwrap();
            assert_eq!(
                r.claims,
                vec![EndedClaim {
                    spec: "mine".into(),
                    agent: "a".into()
                }]
            );
            assert_eq!(r.reason.as_deref(), Some("prompt_input_exit"));
            assert_eq!(r.host_session_id.as_deref(), Some("s1"));
        }

        #[test]
        fn clear_writes_no_record_and_bad_input_still_records() {
            let dir = tempfile::tempdir().unwrap();
            assert!(build_record(
                dir.path(),
                &proc(),
                "h",
                r#"{"reason":"clear"}"#,
                chrono::Utc::now()
            )
            .is_none());
            for reason in ["clear", "resume"] {
                let input = format!(r#"{{"reason":"{reason}"}}"#);
                assert!(
                    build_record(dir.path(), &proc(), "h", &input, chrono::Utc::now()).is_none()
                );
            }
            let r = build_record(dir.path(), &proc(), "h", "not json", chrono::Utc::now()).unwrap();
            assert_eq!(r.reason, None);
            assert!(r.claims.is_empty());
        }

        #[test]
        fn unproven_becomes_gone_only_with_a_matching_record() {
            let dir = tempfile::tempdir().unwrap();
            let p = proc();
            let spec = claimed(dir.path(), "far", "c", "other-host", &p);
            let unproven = || StaleVerdict::Unproven("another host".into());
            assert_eq!(apply(dir.path(), &spec, unproven()), unproven());
            let mut r =
                build_record(dir.path(), &p, "other-host", "{}", chrono::Utc::now()).unwrap();
            // A record for the same pid on another host does not match.
            r.host = "third-host".into();
            write(dir.path(), &r);
            assert_eq!(apply(dir.path(), &spec, unproven()), unproven());
            r.host = "other-host".into();
            write(dir.path(), &r);
            match apply(dir.path(), &spec, unproven()) {
                StaleVerdict::Gone(proof) => {
                    assert!(
                        proof.contains("SessionEnd hook") && proof.contains("other-host"),
                        "{proof}"
                    )
                }
                v => panic!("expected Gone, got {v:?}"),
            }
        }

        #[test]
        fn alive_and_gone_are_never_changed_by_a_record() {
            let dir = tempfile::tempdir().unwrap();
            let p = proc();
            let spec = claimed(dir.path(), "x", "a", "h", &p);
            write(
                dir.path(),
                &build_record(dir.path(), &p, "h", "{}", chrono::Utc::now()).unwrap(),
            );
            let alive = StaleVerdict::Alive("pid 4242 is running".into());
            assert_eq!(apply(dir.path(), &spec, alive.clone()), alive);
            let gone = StaleVerdict::Gone("pid 4242 has exited".into());
            assert_eq!(apply(dir.path(), &spec, gone.clone()), gone);
        }

        #[test]
        fn a_recycled_pid_does_not_match_the_record() {
            let dir = tempfile::tempdir().unwrap();
            let p = proc();
            write(
                dir.path(),
                &build_record(dir.path(), &p, "h", "{}", chrono::Utc::now()).unwrap(),
            );
            let later = ProcInfo {
                start: "Mon Oct 5 09:00:00 2026".into(),
                ..proc()
            };
            let spec = claimed(dir.path(), "y", "a", "h", &later);
            assert!(record_for(dir.path(), &spec).is_none());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // --- Detection tests ---

    #[test]
    fn test_detect_claude_code_with_claude_md() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "# Project").unwrap();
        let detections = detect_frameworks(dir.path());
        let claude = detections
            .iter()
            .find(|d| d.framework == Framework::ClaudeCode)
            .unwrap();
        assert!(claude.detected);
        assert!(claude.indicators.contains(&"CLAUDE.md".to_string()));
    }

    #[test]
    fn test_detect_codex_with_agents_md() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("AGENTS.md"), "# Agents").unwrap();
        let detections = detect_frameworks(dir.path());
        let codex = detections
            .iter()
            .find(|d| d.framework == Framework::Codex)
            .unwrap();
        assert!(codex.detected);
    }

    #[test]
    fn test_detect_nothing() {
        let dir = tempdir().unwrap();
        let detections = detect_frameworks(dir.path());
        assert!(detections.iter().all(|d| !d.detected));
    }

    // --- Marker helper tests ---

    #[test]
    fn test_wrap_with_markers() {
        let content = "## Writ\n\nSome content.";
        let wrapped = wrap_with_markers(content);
        assert!(wrapped.starts_with(MARKER_BEGIN));
        assert!(wrapped.contains("## Writ"));
        assert!(wrapped.trim_end().ends_with(MARKER_END));
    }

    #[test]
    fn test_replace_marked_section() {
        let original = format!(
            "# Project\n\n{}\nOld content.\n{}\n\n## Other\n",
            MARKER_BEGIN, MARKER_END
        );
        let new_section = wrap_with_markers("New content.");
        let result = replace_marked_section(&original, &new_section);
        assert!(result.contains("# Project"));
        assert!(result.contains("New content."));
        assert!(!result.contains("Old content."));
        assert!(result.contains("## Other"));
    }

    #[test]
    fn test_remove_marked_section() {
        let content = format!(
            "# Project\n\nIntro.\n\n---\n\n{}\n## Writ\nStuff.\n{}\n",
            MARKER_BEGIN, MARKER_END
        );
        let cleaned = remove_marked_section(&content);
        assert!(cleaned.contains("# Project"));
        assert!(cleaned.contains("Intro."));
        assert!(!cleaned.contains(MARKER_BEGIN));
        assert!(!cleaned.contains("Stuff."));
        // Separator should be removed too.
        assert!(!cleaned.contains("---"));
    }

    #[test]
    fn test_remove_marked_section_only_content() {
        let content = format!("{}\n## Writ\nAll writ.\n{}\n", MARKER_BEGIN, MARKER_END);
        let cleaned = remove_marked_section(&content);
        assert!(cleaned.trim().is_empty());
    }

    // --- Claude Code hook tests ---

    #[test]
    fn test_hook_claude_code_creates_files_with_markers() {
        let dir = tempdir().unwrap();
        let result = hook_claude_code(dir.path()).unwrap();
        assert!(result.files_created.contains(&"CLAUDE.md".to_string()));
        // Verify all slash command templates were created.
        for template in crate::slash_commands::SLASH_COMMAND_TEMPLATES {
            let path = format!(".claude/commands/{}.md", template.name);
            assert!(
                result.files_created.contains(&path),
                "missing {} in created files",
                path
            );
        }

        let content = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(content.contains(MARKER_BEGIN), "missing begin marker");
        assert!(content.contains(MARKER_END), "missing end marker");
        assert!(content.contains("## Writ\n"), "missing writ section");
    }

    #[test]
    fn test_hook_claude_code_appends_to_existing_with_markers() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("CLAUDE.md"),
            "# My Project\n\nExisting content.",
        )
        .unwrap();
        let result = hook_claude_code(dir.path()).unwrap();
        assert!(result.files_updated.contains(&"CLAUDE.md".to_string()));

        let content = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(content.contains("Existing content."));
        assert!(content.contains(MARKER_BEGIN));
        assert!(content.contains(MARKER_END));
        assert!(content.contains("---"), "missing separator before markers");
    }

    #[test]
    fn test_hook_claude_code_idempotent() {
        let dir = tempdir().unwrap();
        hook_claude_code(dir.path()).unwrap();
        let content_after_first = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();

        let result2 = hook_claude_code(dir.path()).unwrap();
        assert!(result2.files_created.is_empty());
        assert!(result2.files_updated.is_empty());

        let content_after_second = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert_eq!(content_after_first, content_after_second);
    }

    #[test]
    fn test_hook_claude_code_reinit_updates_in_place() {
        let dir = tempdir().unwrap();
        // First install.
        hook_claude_code(dir.path()).unwrap();
        let before = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(before.contains(MARKER_BEGIN));

        // Simulate template change by manually modifying the marked section.
        let modified = before.replace("## Writ\n", "## Writ OLD\n");
        fs::write(dir.path().join("CLAUDE.md"), &modified).unwrap();

        // Reinit should replace the section.
        let result = hook_claude_code(dir.path()).unwrap();
        assert!(result.files_updated.contains(&"CLAUDE.md".to_string()));

        let after = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(after.contains("## Writ\n"));
        assert!(!after.contains("## Writ OLD"));
    }

    #[test]
    fn test_hook_claude_code_upgrades_legacy_heading() {
        let dir = tempdir().unwrap();
        // Simulate old-style install (heading-based, no markers).
        fs::write(
            dir.path().join("CLAUDE.md"),
            "# My Project\n\n## Writ\n\nOld writ content.\n",
        )
        .unwrap();

        let result = hook_claude_code(dir.path()).unwrap();
        assert!(result.files_updated.contains(&"CLAUDE.md".to_string()));

        let content = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(content.contains(MARKER_BEGIN), "should have markers now");
        assert!(
            content.contains("writ seals the edits it can attribute"),
            "should have new content"
        );
        assert!(
            !content.contains("Old writ content."),
            "old content should be gone"
        );
        assert!(content.contains("# My Project"), "user content preserved");
    }

    // --- Codex hook tests ---

    #[test]
    fn test_hook_codex_creates_agents_md_with_markers() {
        let dir = tempdir().unwrap();
        let result = hook_codex(dir.path()).unwrap();
        assert!(result.files_created.contains(&"AGENTS.md".to_string()));

        let content = fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(content.contains(MARKER_BEGIN));
        assert!(content.contains(MARKER_END));
    }

    #[test]
    fn test_hook_codex_appends_with_markers() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("AGENTS.md"),
            "# Agents\n\nExisting config.\n",
        )
        .unwrap();
        let result = hook_codex(dir.path()).unwrap();
        assert!(result.files_updated.contains(&"AGENTS.md".to_string()));

        let content = fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(content.contains("Existing config."));
        assert!(content.contains(MARKER_BEGIN));
    }

    #[test]
    fn test_hook_codex_idempotent() {
        let dir = tempdir().unwrap();
        hook_codex(dir.path()).unwrap();
        let result2 = hook_codex(dir.path()).unwrap();
        assert!(result2.files_created.is_empty());
        assert!(result2.files_updated.is_empty());
    }

    // --- Template content tests ---

    /// Finding 67, sprint 6: the hook, the CLAUDE.md and AGENTS.md blocks
    /// and the settings instruction carry the same zero-instruction lines;
    /// the generic agent instructions (hosts with no hooks) carry the hand
    /// workflow.
    #[test]
    fn every_surface_renders_the_same_auto_steps() {
        let hook = session_start_command("writ");
        let blocks = [writ_claude_md_section(), writ_agents_md_section()];
        for step in crate::template::auto_steps() {
            for s in &blocks {
                assert!(s.contains(&step), "missing {step:?}");
            }
            assert!(writ_instruction().contains(&step), "settings: {step:?}");
            // The hook echoes single-quoted lines: an apostrophe appears as '\''.
            let echoed = step.replace('`', "").replace('\'', "'\\''");
            assert!(hook.contains(&echoed), "hook: {step:?}");
        }
        for step in crate::template::workflow_steps() {
            assert!(agent_instructions_content().contains(&step), "{step:?}");
        }
        let all = [
            blocks[0].clone(),
            blocks[1].clone(),
            agent_instructions_content(),
            hook,
        ];
        for s in &all {
            assert!(!s.contains('@'), "unfilled placeholder:\n{s}");
        }
    }

    /// Finding 70: the generated skills teach the same seal, spec add and
    /// spec done commands as the template; none teaches a bare
    /// `writ seal -s "..."` without `--paths` or a `spec add` without
    /// `--scope`.
    #[test]
    fn skills_teach_the_same_commands_as_the_block() {
        let block = writ_claude_md_section();
        assert!(block.contains(crate::template::SPEC_DONE));
        let texts: Vec<(&str, &str)> = crate::skills::SKILL_TEMPLATES
            .iter()
            .flat_map(|t| {
                std::iter::once((t.name, t.skill_md))
                    .chain(t.supporting_files.iter().map(|f| (t.name, f.content)))
            })
            .collect();
        let by_name = |n: &str| texts.iter().find(|(name, _)| *name == n).unwrap().1;
        assert!(by_name("writ-seal")
            .contains(r#"writ seal -s "what you did" --paths <changed files, comma-separated>"#));
        assert!(by_name("writ-seal").contains(crate::template::PATHS_NOTE));
        assert!(by_name("writ-spec-add").contains(crate::template::SPEC_ADD));
        assert!(by_name("writ-spec-done").contains(crate::template::SPEC_DONE));
        for (name, text) in &texts {
            for line in text.lines() {
                if line.contains("writ seal -s") {
                    assert!(line.contains("--paths"), "{name}: {line}");
                }
                if line.contains("writ spec add \"") {
                    assert!(line.contains("--scope"), "{name}: {line}");
                }
            }
        }
    }

    #[test]
    fn hook_command_runs_in_sh_and_prints_the_lines() {
        let out = std::process::Command::new("sh")
            .args(["-c", &session_start_command("true")])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success());
        assert!(
            text.contains("writ seals the edits it can attribute"),
            "{text}"
        );
        assert!(
            text.contains(r#"writ spec done -s "<what you did>""#),
            "{text}"
        );
        // The echoed lines, before the PATH note and the brief context.
        let lines = text
            .lines()
            .take_while(|l| !l.starts_with("Note:") && !l.starts_with("# writ context"))
            .filter(|l| !l.trim().is_empty())
            .count();
        assert!(lines <= 5, "{lines} lines:\n{text}");
    }

    /// Sprint 6: the managed block is under 5 lines with no mandatory
    /// command and no identity instruction; it still says writ context
    /// exists, how to close with a summary, and to leave git to the user.
    #[test]
    fn claude_md_block_is_zero_instruction() {
        let block = writ_claude_md_section();
        let lines = block.lines().filter(|l| !l.trim().is_empty()).count();
        assert!(lines <= 5, "{lines} lines:\n{block}");
        for w in [
            "MANDATORY",
            "MUST",
            "EVERY TASK",
            "NOT complete",
            "BEFORE",
            "FIRST ACTION",
            "WRIT_AGENT_ID",
        ] {
            assert!(!block.contains(w), "{w}:\n{block}");
        }
        // Finding 146: sealing is automatic only in Claude Code; the one seal
        // command is for other tools, on the same line.
        let line = block.lines().find(|l| l.contains("writ seal -s")).unwrap();
        assert!(line.contains("writ seals the edits it can attribute and asks you with one line about the rest; in other tools"));
        assert_eq!(block.matches("writ seal -s").count(), 1);
        assert!(block.contains("writ context"));
        assert!(block.contains(r#"writ spec done -s "<what you did>""#));
        assert!(block.contains("Leave git commits and `writ finish` to the user"));
        assert!(!writ_instruction().contains("MANDATORY"));
    }

    #[test]
    fn agents_md_block_is_the_claude_md_block() {
        assert_eq!(writ_agents_md_section(), writ_claude_md_section());
    }

    #[test]
    fn test_agents_md_section_is_focused() {
        let section = writ_agents_md_section();
        assert!(!section.contains("Convergence"));
        assert!(!section.contains("Rollback"));
    }

    // --- Generic agent instructions tests ---

    #[test]
    fn test_hook_generic_creates_instructions() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".writ")).unwrap();
        let result = hook_generic(dir.path()).unwrap();
        assert!(result
            .files_created
            .contains(&".writ/AGENT_INSTRUCTIONS.md".to_string()));

        let content =
            fs::read_to_string(dir.path().join(".writ").join("AGENT_INSTRUCTIONS.md")).unwrap();
        assert!(content.contains("# Writ"));
        assert!(content.contains("writ context"));
        assert!(content.contains("writ seal"));
        assert!(content.contains("TOON"));
        assert!(content.contains("writ_seal"), "missing tool definition");
    }

    #[test]
    fn test_hook_generic_idempotent() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".writ")).unwrap();
        hook_generic(dir.path()).unwrap();
        let result2 = hook_generic(dir.path()).unwrap();
        assert!(result2.files_created.is_empty());
        assert!(result2.files_updated.is_empty());
    }

    #[test]
    fn test_hook_generic_skips_without_writ_dir() {
        let dir = tempdir().unwrap();
        let result = hook_generic(dir.path()).unwrap();
        assert!(result.files_created.is_empty());
    }

    // --- .gitignore tests ---

    #[test]
    fn test_append_gitignore_creates_new() {
        let dir = tempdir().unwrap();
        let created = append_gitignore(dir.path()).unwrap();
        assert!(created);

        let content = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(content.contains(".writ/"));
        assert!(content.contains("# Writ version control state"));
    }

    #[test]
    fn test_append_gitignore_appends_to_existing() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "node_modules/\n").unwrap();
        let created = append_gitignore(dir.path()).unwrap();
        assert!(created);

        let content = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(content.contains("node_modules/"));
        assert!(content.contains(".writ/"));
    }

    #[test]
    fn test_append_gitignore_idempotent() {
        let dir = tempdir().unwrap();
        append_gitignore(dir.path()).unwrap();
        let created = append_gitignore(dir.path()).unwrap();
        assert!(!created, "should be no-op second time");

        let content = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        let count = content.matches(".writ/").count();
        assert_eq!(count, 1, "should not duplicate .writ/ entry");
    }

    #[test]
    fn test_append_gitignore_handles_no_trailing_newline() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "node_modules/").unwrap();
        append_gitignore(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(content.contains("node_modules/\n"), "should add newline");
        assert!(content.contains(".writ/"));
    }

    #[test]
    fn test_remove_gitignore_entry() {
        let dir = tempdir().unwrap();
        append_gitignore(dir.path()).unwrap();
        let removed = remove_gitignore_entry(dir.path()).unwrap();
        assert!(removed);

        // File should be removed since it was only writ content.
        assert!(!dir.path().join(".gitignore").exists());
    }

    #[test]
    fn test_remove_gitignore_entry_preserves_other_content() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "node_modules/\n").unwrap();
        append_gitignore(dir.path()).unwrap();

        let removed = remove_gitignore_entry(dir.path()).unwrap();
        assert!(removed);

        let content = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(content.contains("node_modules/"));
        assert!(!content.contains(".writ/"));
        assert!(!content.contains("Writ version control state"));
    }

    #[test]
    fn test_remove_gitignore_noop_when_not_present() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "node_modules/\n").unwrap();
        let removed = remove_gitignore_entry(dir.path()).unwrap();
        assert!(!removed);
    }

    // --- Uninstall hook tests ---

    #[test]
    fn test_unhook_claude_code_removes_marker_based() {
        let dir = tempdir().unwrap();
        hook_claude_code(dir.path()).unwrap();
        assert!(dir.path().join("CLAUDE.md").exists());
        assert!(dir.path().join(".claude/commands/writ-seal.md").exists());

        let result = unhook_claude_code(dir.path()).unwrap();
        assert!(result.files_removed.contains(&"CLAUDE.md".to_string()));
        // Verify all slash command templates were removed.
        for template in crate::slash_commands::SLASH_COMMAND_TEMPLATES {
            let path = format!(".claude/commands/{}.md", template.name);
            assert!(
                result.files_removed.contains(&path),
                "missing {} in removed files",
                path
            );
        }
        assert!(!dir.path().join("CLAUDE.md").exists());
    }

    #[test]
    fn test_unhook_claude_code_preserves_existing_content() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("CLAUDE.md"),
            "# My Project\n\nImportant instructions.\n",
        )
        .unwrap();
        hook_claude_code(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(content.contains(MARKER_BEGIN));
        assert!(content.contains("Important instructions."));

        let result = unhook_claude_code(dir.path()).unwrap();
        assert!(result.files_updated.contains(&"CLAUDE.md".to_string()));
        assert!(!result.files_removed.contains(&"CLAUDE.md".to_string()));

        let after = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(after.contains("Important instructions."));
        assert!(!after.contains(MARKER_BEGIN));
        assert!(!after.contains("Writ Version Control"));
    }

    #[test]
    fn test_unhook_claude_code_handles_legacy_heading() {
        let dir = tempdir().unwrap();
        // Simulate old-style install without markers — writ-only file.
        fs::write(dir.path().join("CLAUDE.md"), "## Writ\n\nOld writ stuff.\n").unwrap();

        let result = unhook_claude_code(dir.path()).unwrap();
        // Only writ content, so file gets deleted.
        assert!(result.files_removed.contains(&"CLAUDE.md".to_string()));
        assert!(!dir.path().join("CLAUDE.md").exists());
    }

    #[test]
    fn test_unhook_claude_code_legacy_preserves_user_content() {
        let dir = tempdir().unwrap();
        // Old-style install with user content before writ section.
        fs::write(
            dir.path().join("CLAUDE.md"),
            "# Project\n\nMy instructions.\n\n## Writ\n\nOld writ stuff.\n",
        )
        .unwrap();

        let result = unhook_claude_code(dir.path()).unwrap();
        assert!(result.files_updated.contains(&"CLAUDE.md".to_string()));

        let after = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(after.contains("My instructions."));
        assert!(!after.contains("## Writ"));
    }

    #[test]
    fn test_unhook_codex_removes_marker_based() {
        let dir = tempdir().unwrap();
        hook_codex(dir.path()).unwrap();
        assert!(dir.path().join("AGENTS.md").exists());

        let result = unhook_codex(dir.path()).unwrap();
        assert!(result.files_removed.contains(&"AGENTS.md".to_string()));
        assert!(!dir.path().join("AGENTS.md").exists());
    }

    #[test]
    fn test_unhook_codex_preserves_existing_content() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("AGENTS.md"),
            "# Agents\n\nExisting agent config.\n",
        )
        .unwrap();
        hook_codex(dir.path()).unwrap();

        let result = unhook_codex(dir.path()).unwrap();
        assert!(result.files_updated.contains(&"AGENTS.md".to_string()));

        let after = fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(after.contains("Existing agent config."));
        assert!(!after.contains(MARKER_BEGIN));
    }

    #[test]
    fn test_unhook_generic_removes_instructions() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".writ")).unwrap();
        hook_generic(dir.path()).unwrap();
        assert!(dir
            .path()
            .join(".writ")
            .join("AGENT_INSTRUCTIONS.md")
            .exists());

        let result = unhook_generic(dir.path()).unwrap();
        assert!(result
            .files_removed
            .contains(&".writ/AGENT_INSTRUCTIONS.md".to_string()));
    }

    #[test]
    fn test_uninstall_hooks_removes_all() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".writ")).unwrap();
        hook_claude_code(dir.path()).unwrap();
        hook_codex(dir.path()).unwrap();
        hook_generic(dir.path()).unwrap();
        append_gitignore(dir.path()).unwrap();

        let results = uninstall_hooks(dir.path()).unwrap();
        // Claude + Codex + Generic.
        assert_eq!(results.len(), 3, "should clean up all three frameworks");

        // .gitignore should be cleaned too.
        assert!(!dir.path().join(".gitignore").exists());
    }

    #[test]
    fn test_uninstall_hooks_noop_when_nothing_installed() {
        let dir = tempdir().unwrap();
        let results = uninstall_hooks(dir.path()).unwrap();
        assert!(results.is_empty(), "nothing to clean up");
    }

    // --- Legacy removal tests (backward compat) ---

    #[test]
    fn test_remove_writ_section_middle() {
        let content = "# Project\n\nIntro.\n\n## Writ\n\nWrit stuff.\n\n## Other\n\nMore stuff.\n";
        let cleaned = remove_writ_section(content);
        assert!(cleaned.contains("# Project"));
        assert!(cleaned.contains("## Other"));
        assert!(!cleaned.contains("## Writ"));
        assert!(!cleaned.contains("Writ stuff."));
    }

    #[test]
    fn test_remove_writ_section_at_end() {
        let content = "# Project\n\nIntro.\n\n## Writ\n\nWrit stuff.\n";
        let cleaned = remove_writ_section(content);
        assert!(cleaned.contains("# Project"));
        assert!(!cleaned.contains("## Writ"));
    }

    #[test]
    fn test_remove_writ_section_only_content() {
        let content = "## Writ\n\nAll writ.\n";
        let cleaned = remove_writ_section(content);
        assert!(cleaned.trim().is_empty());
    }

    // Slash command content tests now live in slash_commands::tests.

    // --- Install hooks integration test ---

    #[test]
    fn test_install_hooks_creates_generic_and_gitignore() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".writ")).unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "# Project").unwrap();

        let results = install_hooks(dir.path()).unwrap();

        // Should have Claude + Generic.
        assert!(results.iter().any(|r| r.framework == Framework::ClaudeCode));
        assert!(results.iter().any(|r| r.framework == Framework::Custom));

        // .gitignore should exist.
        assert!(dir.path().join(".gitignore").exists());
        let gi = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(gi.contains(".writ/"));

        // AGENT_INSTRUCTIONS.md should exist.
        assert!(dir
            .path()
            .join(".writ")
            .join("AGENT_INSTRUCTIONS.md")
            .exists());
    }

    // --- IZ-1: .gitignore handles no-trailing-slash variant ---

    #[test]
    fn test_remove_gitignore_catches_no_trailing_slash() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join(".gitignore"),
            "node_modules/\n.writ\nother/\n",
        )
        .unwrap();
        let removed = remove_gitignore_entry(dir.path()).unwrap();
        assert!(removed);

        let content = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(!content.contains(".writ"), "should remove .writ (no slash)");
        assert!(content.contains("node_modules/"));
        assert!(content.contains("other/"));
    }

    // --- IZ-2: Legacy removal doesn't match unrelated headings ---

    #[test]
    fn test_remove_writ_section_does_not_match_writing() {
        let content =
            "# Project\n\n## Writing Style Guide\n\nUse active voice.\n\n## Writ\n\nWrit stuff.\n";
        let cleaned = remove_writ_section(content);
        assert!(
            cleaned.contains("## Writing Style Guide"),
            "should preserve Writing heading"
        );
        assert!(
            cleaned.contains("Use active voice."),
            "should preserve Writing content"
        );
        // Check the writ section content is removed.
        assert!(
            !cleaned.contains("Writ stuff."),
            "should remove Writ content"
        );
        // Verify no standalone `## Writ` line remains (but `## Writing` is fine).
        assert!(
            !cleaned.lines().any(|l| l == "## Writ"),
            "should remove ## Writ heading"
        );
    }

    #[test]
    fn test_remove_writ_section_matches_writ_version_control() {
        let content = "## Writ Version Control\n\nNew-style heading content.\n";
        let cleaned = remove_writ_section(content);
        assert!(cleaned.trim().is_empty());
    }

    // --- Claude Code permissions tests ---

    #[test]
    fn test_ensure_claude_permissions_creates_settings() {
        let dir = tempdir().unwrap();
        let result = ensure_claude_permissions(dir.path()).unwrap();
        assert!(result.is_some());

        let content = fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let allow = parsed["permissions"]["allow"].as_array().unwrap();
        assert!(allow.iter().any(|v| v.as_str() == Some("Bash(writ *)")));
        assert!(allow.iter().any(|v| v.as_str() == Some("mcp__writ__*")));
    }

    #[test]
    fn test_ensure_claude_permissions_merges_existing() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".claude")).unwrap();
        fs::write(
            dir.path().join(".claude").join("settings.json"),
            r#"{"permissions": {"allow": ["Bash(git *)"]}, "other": true}"#,
        )
        .unwrap();

        ensure_claude_permissions(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let allow = parsed["permissions"]["allow"].as_array().unwrap();
        // Both permissions should be present.
        assert!(allow.iter().any(|v| v.as_str() == Some("Bash(git *)")));
        assert!(allow.iter().any(|v| v.as_str() == Some("Bash(writ *)")));
        assert!(allow.iter().any(|v| v.as_str() == Some("mcp__writ__*")));
        // Other fields preserved.
        assert_eq!(parsed["other"], serde_json::json!(true));
    }

    #[test]
    fn test_ensure_claude_permissions_idempotent() {
        let dir = tempdir().unwrap();
        ensure_claude_permissions(dir.path()).unwrap();
        let result = ensure_claude_permissions(dir.path()).unwrap();
        assert!(result.is_none(), "second call should be a no-op");

        let content = fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let allow = parsed["permissions"]["allow"].as_array().unwrap();
        // Should only have one entry each, not duplicated.
        let bash_count = allow
            .iter()
            .filter(|v| v.as_str() == Some("Bash(writ *)"))
            .count();
        assert_eq!(bash_count, 1, "bash permission should not be duplicated");
        let mcp_count = allow
            .iter()
            .filter(|v| v.as_str() == Some("mcp__writ__*"))
            .count();
        assert_eq!(mcp_count, 1, "mcp permission should not be duplicated");
    }

    #[test]
    fn test_remove_claude_permissions_removes_entry() {
        let dir = tempdir().unwrap();
        ensure_claude_permissions(dir.path()).unwrap();
        let result = remove_claude_permissions(dir.path()).unwrap();
        assert!(result.is_some());

        let content = fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let allow = parsed["permissions"]["allow"].as_array().unwrap();
        assert!(!allow.iter().any(|v| v.as_str() == Some("Bash(writ *)")));
        assert!(!allow.iter().any(|v| v.as_str() == Some("mcp__writ__*")));
    }

    #[test]
    fn test_remove_claude_permissions_preserves_other() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".claude")).unwrap();
        fs::write(
            dir.path().join(".claude").join("settings.json"),
            r#"{"permissions": {"allow": ["Bash(git *)", "Bash(writ *)", "mcp__writ__*"]}}"#,
        )
        .unwrap();

        remove_claude_permissions(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let allow = parsed["permissions"]["allow"].as_array().unwrap();
        assert!(allow.iter().any(|v| v.as_str() == Some("Bash(git *)")));
        assert!(!allow.iter().any(|v| v.as_str() == Some("Bash(writ *)")));
        assert!(!allow.iter().any(|v| v.as_str() == Some("mcp__writ__*")));
    }

    #[test]
    fn test_remove_claude_permissions_noop_when_missing() {
        let dir = tempdir().unwrap();
        let result = remove_claude_permissions(dir.path()).unwrap();
        assert!(result.is_none(), "no file → no-op");
    }

    #[test]
    fn test_hook_claude_code_adds_permissions() {
        let dir = tempdir().unwrap();
        hook_claude_code(dir.path()).unwrap();

        let settings_path = dir.path().join(".claude").join("settings.json");
        assert!(settings_path.exists(), "settings.json should be created");

        let content = fs::read_to_string(&settings_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let allow = parsed["permissions"]["allow"].as_array().unwrap();
        assert!(allow.iter().any(|v| v.as_str() == Some("Bash(writ *)")));
    }

    #[test]
    fn test_unhook_claude_code_removes_permissions() {
        let dir = tempdir().unwrap();
        hook_claude_code(dir.path()).unwrap();

        // Verify permission exists.
        let content = fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        assert!(content.contains("Bash(writ *)"));

        unhook_claude_code(dir.path()).unwrap();

        // Permission should be removed.
        let content = fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        assert!(!content.contains("Bash(writ *)"));
    }

    #[test]
    fn test_agent_instructions_has_restore_warning() {
        let content = agent_instructions_content();
        assert!(
            content.contains("overwrites working directory"),
            "agent instructions should warn about restore"
        );
    }

    // --- Claude Code settings.json instruction tests ---

    #[test]
    fn test_ensure_claude_instructions_creates() {
        let dir = tempdir().unwrap();
        let result = ensure_claude_instructions(dir.path()).unwrap();
        assert!(result.is_some());

        let content = fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let instructions = parsed["instructions"].as_array().unwrap();
        assert_eq!(instructions.len(), 1);
        assert!(instructions[0]
            .as_str()
            .unwrap()
            .contains(WRIT_INSTRUCTION_MARKER));
    }

    #[test]
    fn test_ensure_claude_instructions_merges_existing() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".claude")).unwrap();
        fs::write(
            dir.path().join(".claude").join("settings.json"),
            r#"{"instructions": ["Always use TypeScript"], "permissions": {"allow": []}}"#,
        )
        .unwrap();

        ensure_claude_instructions(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let instructions = parsed["instructions"].as_array().unwrap();
        assert_eq!(instructions.len(), 2);
        assert!(instructions
            .iter()
            .any(|v| v.as_str() == Some("Always use TypeScript")));
        assert!(instructions
            .iter()
            .any(|v| v.as_str().unwrap().contains(WRIT_INSTRUCTION_MARKER)));
        // Other fields preserved.
        assert!(parsed["permissions"].is_object());
    }

    #[test]
    fn test_ensure_claude_instructions_idempotent() {
        let dir = tempdir().unwrap();
        ensure_claude_instructions(dir.path()).unwrap();
        let result = ensure_claude_instructions(dir.path()).unwrap();
        assert!(result.is_none(), "second call should be a no-op");

        let content = fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let instructions = parsed["instructions"].as_array().unwrap();
        let count = instructions
            .iter()
            .filter(|v| {
                v.as_str()
                    .is_some_and(|s| s.contains(WRIT_INSTRUCTION_MARKER))
            })
            .count();
        assert_eq!(count, 1, "instruction should not be duplicated");
    }

    #[test]
    fn test_remove_claude_instructions_removes() {
        let dir = tempdir().unwrap();
        ensure_claude_instructions(dir.path()).unwrap();
        let result = remove_claude_instructions(dir.path()).unwrap();
        assert!(result.is_some());

        let content = fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        // The key existed only because init created it, so it is dropped.
        assert!(parsed.get("instructions").is_none(), "{parsed}");
    }

    #[test]
    fn test_remove_claude_instructions_preserves_other() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".claude")).unwrap();
        fs::write(
            dir.path().join(".claude").join("settings.json"),
            r#"{"instructions": ["Always use TypeScript", "This project uses writ for version control. Do stuff."]}"#,
        )
        .unwrap();

        remove_claude_instructions(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let instructions = parsed["instructions"].as_array().unwrap();
        assert_eq!(instructions.len(), 1);
        assert_eq!(instructions[0].as_str().unwrap(), "Always use TypeScript");
    }

    #[test]
    fn test_remove_claude_instructions_noop_when_missing() {
        let dir = tempdir().unwrap();
        let result = remove_claude_instructions(dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_hook_claude_code_adds_instruction() {
        let dir = tempdir().unwrap();
        hook_claude_code(dir.path()).unwrap();

        let settings_path = dir.path().join(".claude").join("settings.json");
        let content = fs::read_to_string(&settings_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let instructions = parsed["instructions"].as_array().unwrap();
        assert!(
            instructions
                .iter()
                .any(|v| v.as_str().unwrap().contains(WRIT_INSTRUCTION_MARKER)),
            "hook_claude_code should add writ instruction"
        );
    }

    #[test]
    fn test_unhook_claude_code_removes_instruction() {
        let dir = tempdir().unwrap();
        hook_claude_code(dir.path()).unwrap();

        // Verify instruction exists.
        let content = fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        assert!(content.contains(WRIT_INSTRUCTION_MARKER));

        unhook_claude_code(dir.path()).unwrap();

        // Instruction should be removed.
        let content = fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        assert!(
            !content.contains(WRIT_INSTRUCTION_MARKER),
            "unhook should remove writ instruction"
        );
    }

    #[test]
    fn test_claudemd_prepends_to_existing() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("CLAUDE.md"),
            "# My Project\n\nUser stuff.\n",
        )
        .unwrap();

        hook_claude_code(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        // Writ section should come before user content.
        let writ_pos = content.find(MARKER_BEGIN).unwrap();
        let user_pos = content.find("User stuff").unwrap();
        assert!(
            writ_pos < user_pos,
            "writ section should be prepended, not appended"
        );
    }

    // --- Claude Code hook tests (SessionStart only, brief format) ---

    fn read_settings(root: &Path) -> serde_json::Value {
        let content = fs::read_to_string(root.join(".claude").join("settings.json")).unwrap();
        serde_json::from_str(&content).unwrap()
    }

    fn write_settings_json(root: &Path, value: &serde_json::Value) {
        fs::create_dir_all(root.join(".claude")).unwrap();
        fs::write(
            root.join(".claude").join("settings.json"),
            serde_json::to_string_pretty(value).unwrap(),
        )
        .unwrap();
    }

    fn writ_commands(parsed: &serde_json::Value, event: &str) -> Vec<String> {
        parsed["hooks"][event]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .flat_map(|e| e["hooks"].as_array().cloned().unwrap_or_default())
                    .filter_map(|h| h["command"].as_str().map(String::from))
                    .filter(|c| is_writ_hook_command(c))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The pre-sprint-1 block: full `writ context` on two events.
    fn legacy_two_hook_block() -> serde_json::Value {
        let old =
            r#"echo '## Writ VCS Active' && /opt/homebrew/bin/writ context 2>/dev/null || true"#;
        serde_json::json!({
            "hooks": {
                "SessionStart": [{"hooks": [{"type": "command", "command": old, "timeout": 10}]}],
                "UserPromptSubmit": [{"hooks": [{"type": "command", "command": old, "timeout": 10}]}]
            }
        })
    }

    #[test]
    fn test_ensure_claude_hook_creates_single_session_start_brief_hook() {
        let dir = tempdir().unwrap();
        let result = ensure_claude_hook(dir.path()).unwrap();
        assert!(result.is_some());

        let parsed = read_settings(dir.path());
        let hooks = parsed["hooks"].as_object().unwrap();
        assert_eq!(
            hooks.len(),
            7,
            "SessionStart plus the six auto seal events: {hooks:?}"
        );
        let ends = writ_commands(&parsed, "SessionEnd");
        assert_eq!(ends.len(), 1, "{hooks:?}");
        assert!(ends[0].contains("\"$W\" hook "), "{ends:?}");
        assert_eq!(
            parsed["hooks"]["PostToolUse"][0]["matcher"],
            "Edit|Write|MultiEdit|NotebookEdit|Bash"
        );
        let entries = parsed["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        let inner = &entries[0]["hooks"][0];
        assert_eq!(inner["type"].as_str().unwrap(), "command");
        assert!(inner["command"]
            .as_str()
            .unwrap()
            .contains("context --format brief"));
        let prompt = writ_commands(&parsed, "UserPromptSubmit");
        assert!(
            prompt.len() == 1 && !prompt[0].contains("context"),
            "{prompt:?}"
        );
    }

    #[test]
    fn test_hook_command_never_runs_default_context() {
        let cmd = writ_hook_command();
        // The only non-echo segment is the context call, and it is brief.
        let executed: Vec<&str> = cmd
            .split("&&")
            .map(str::trim)
            .filter(|seg| !seg.starts_with("echo"))
            .collect();
        assert_eq!(executed.len(), 1, "{cmd}");
        assert!(executed[0].contains(" context --format brief 2>/dev/null"));
    }

    #[test]
    fn test_ensure_claude_hook_merges_existing() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".claude")).unwrap();
        fs::write(
            dir.path().join(".claude").join("settings.json"),
            r#"{"permissions": {"allow": ["Read"]}, "instructions": ["Be nice"]}"#,
        )
        .unwrap();

        ensure_claude_hook(dir.path()).unwrap();

        let parsed = read_settings(dir.path());
        assert!(parsed["permissions"]["allow"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some("Read")));
        assert!(parsed["instructions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some("Be nice")));
        assert_eq!(writ_commands(&parsed, "SessionStart").len(), 1);
    }

    #[test]
    fn test_ensure_claude_hook_idempotent() {
        let dir = tempdir().unwrap();
        ensure_claude_hook(dir.path()).unwrap();
        let result = ensure_claude_hook(dir.path()).unwrap();
        assert!(result.is_none(), "second call should be a no-op");
        let parsed = read_settings(dir.path());
        assert_eq!(writ_commands(&parsed, "SessionStart").len(), 1);
    }

    #[test]
    fn test_ensure_claude_hook_replaces_legacy_two_hook_block() {
        let dir = tempdir().unwrap();
        write_settings_json(dir.path(), &legacy_two_hook_block());

        let result = ensure_claude_hook(dir.path()).unwrap();
        assert!(result.is_some());

        let parsed = read_settings(dir.path());
        let session = writ_commands(&parsed, "SessionStart");
        assert_eq!(session.len(), 1, "replaced, not appended");
        assert!(session[0].contains("--format brief"));
        // The legacy context hook is gone; the prompt stash hook replaces it.
        let prompt = writ_commands(&parsed, "UserPromptSubmit");
        assert_eq!(prompt.len(), 1);
        assert!(prompt[0].contains("\"$W\" hook "), "{prompt:?}");
    }

    #[test]
    fn test_ensure_claude_hook_keeps_user_hooks_on_legacy_event() {
        let dir = tempdir().unwrap();
        let mut settings = legacy_two_hook_block();
        settings["hooks"]["UserPromptSubmit"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"hooks": [{"type": "command", "command": "echo custom"}]}));
        write_settings_json(dir.path(), &settings);

        ensure_claude_hook(dir.path()).unwrap();

        let parsed = read_settings(dir.path());
        let ups = parsed["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(ups.len(), 2, "the user's hook plus writ's stash hook");
        assert_eq!(ups[0]["hooks"][0]["command"], "echo custom");
    }

    #[test]
    fn test_ensure_claude_hook_preserves_other_hooks() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".claude")).unwrap();
        fs::write(
            dir.path().join(".claude").join("settings.json"),
            r#"{"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "echo hi"}]}]}}"#,
        )
        .unwrap();

        ensure_claude_hook(dir.path()).unwrap();

        let parsed = read_settings(dir.path());
        // The user's hook stays, first; writ's Bash snapshot hook joins it.
        let pre = parsed["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2);
        assert_eq!(pre[0]["hooks"][0]["command"], "echo hi");
        assert_eq!(writ_commands(&parsed, "SessionStart").len(), 1);
    }

    #[test]
    fn test_remove_claude_hook_removes_exactly_what_init_wrote() {
        let dir = tempdir().unwrap();
        write_settings_json(
            dir.path(),
            &serde_json::json!({"permissions": {"allow": ["Read"]}}),
        );
        let before = read_settings(dir.path());

        ensure_claude_hook(dir.path()).unwrap();
        let result = remove_claude_hook(dir.path()).unwrap();
        assert!(result.is_some());

        assert_eq!(read_settings(dir.path()), before);
    }

    #[test]
    fn test_remove_claude_hook_cleans_legacy_events() {
        let dir = tempdir().unwrap();
        write_settings_json(dir.path(), &legacy_two_hook_block());
        remove_claude_hook(dir.path()).unwrap();
        let parsed = read_settings(dir.path());
        assert!(parsed.get("hooks").is_none(), "{parsed}");
    }

    #[test]
    fn test_remove_claude_hook_preserves_other() {
        let dir = tempdir().unwrap();
        let mut settings = legacy_two_hook_block();
        settings["hooks"]["UserPromptSubmit"]
            .as_array_mut()
            .unwrap()
            .insert(
                0,
                serde_json::json!({"hooks": [{"type": "command", "command": "echo custom"}]}),
            );
        write_settings_json(dir.path(), &settings);

        remove_claude_hook(dir.path()).unwrap();

        let parsed = read_settings(dir.path());
        let ups = parsed["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(ups.len(), 1, "should only remove the writ hook");
        assert!(ups[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("echo custom"));
        assert!(parsed["hooks"].get("SessionStart").is_none());
    }

    #[test]
    fn test_remove_claude_instructions_drops_key_init_created() {
        let dir = tempdir().unwrap();
        write_settings_json(
            dir.path(),
            &serde_json::json!({"permissions": {"allow": ["Read"]}}),
        );
        let before = read_settings(dir.path());

        ensure_claude_instructions(dir.path()).unwrap();
        assert!(read_settings(dir.path())["instructions"].is_array());
        remove_claude_instructions(dir.path()).unwrap();

        assert_eq!(read_settings(dir.path()), before);
    }

    #[test]
    fn test_remove_claude_instructions_keeps_user_instructions() {
        let dir = tempdir().unwrap();
        write_settings_json(
            dir.path(),
            &serde_json::json!({"instructions": ["Be nice"]}),
        );
        ensure_claude_instructions(dir.path()).unwrap();
        remove_claude_instructions(dir.path()).unwrap();
        assert_eq!(
            read_settings(dir.path())["instructions"],
            serde_json::json!(["Be nice"])
        );
    }

    #[test]
    fn test_remove_claude_hook_noop_when_missing() {
        let dir = tempdir().unwrap();
        let result = remove_claude_hook(dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_hook_claude_code_adds_one_hook_per_event() {
        let dir = tempdir().unwrap();
        hook_claude_code(dir.path()).unwrap();
        let parsed = read_settings(dir.path());
        for event in std::iter::once(WRIT_HOOK_EVENT).chain(WRIT_AUTO_EVENTS.iter().copied()) {
            assert_eq!(writ_commands(&parsed, event).len(), 1, "{event}");
        }
    }

    #[test]
    fn test_unhook_claude_code_removes_hooks() {
        let dir = tempdir().unwrap();
        hook_claude_code(dir.path()).unwrap();

        // Verify hooks exist.
        let content = fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        assert!(content.contains(WRIT_HOOK_MARKER));

        unhook_claude_code(dir.path()).unwrap();

        // Hooks should be removed.
        let content = fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        assert!(
            !content.contains(WRIT_HOOK_MARKER),
            "unhook should remove all writ hook commands"
        );
    }

    // -----------------------------------------------------------------------
    // SK.9: Bri's init/uninit integration tests for skills
    // -----------------------------------------------------------------------

    #[test]
    fn test_init_generates_both_slash_commands_and_skills() {
        let dir = tempdir().unwrap();
        let result = hook_claude_code(dir.path()).unwrap();

        // Slash commands should exist.
        let commands_dir = dir.path().join(".claude/commands");
        assert!(commands_dir.is_dir(), "commands dir should exist");
        let cmd_count = fs::read_dir(&commands_dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("writ-")
            })
            .count();
        assert!(cmd_count > 0, "slash commands should be generated");

        // Skill directories should exist.
        let skills_dir = dir.path().join(".claude/skills");
        assert!(skills_dir.is_dir(), "skills dir should exist");
        let skill_count = fs::read_dir(&skills_dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("writ-")
            })
            .count();
        assert_eq!(
            skill_count,
            crate::skills::SKILL_TEMPLATES.len(),
            "all skill directories should be created"
        );

        // HookResult should mention skills in created files.
        let has_skills_entry = result
            .files_created
            .iter()
            .any(|f| f.contains(".claude/skills"));
        assert!(has_skills_entry, "HookResult should list skills creation");
    }

    #[test]
    fn test_uninit_removes_both_slash_commands_and_skills() {
        let dir = tempdir().unwrap();
        hook_claude_code(dir.path()).unwrap();

        // Verify both exist before uninit.
        assert!(dir.path().join(".claude/commands").is_dir());
        assert!(dir.path().join(".claude/skills").is_dir());

        unhook_claude_code(dir.path()).unwrap();

        // All writ slash commands should be removed.
        let commands_dir = dir.path().join(".claude/commands");
        if commands_dir.exists() {
            let writ_cmds: Vec<_> = fs::read_dir(&commands_dir)
                .unwrap()
                .filter(|e| {
                    e.as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with("writ-")
                })
                .collect();
            assert!(
                writ_cmds.is_empty(),
                "writ slash commands should be removed"
            );
        }

        // All writ skill directories should be removed.
        let skills_dir = dir.path().join(".claude/skills");
        if skills_dir.exists() {
            let writ_skills: Vec<_> = fs::read_dir(&skills_dir)
                .unwrap()
                .filter(|e| {
                    e.as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with("writ-")
                })
                .collect();
            assert!(
                writ_skills.is_empty(),
                "writ skill directories should be removed"
            );
        }
    }

    #[test]
    fn test_init_skills_idempotent() {
        let dir = tempdir().unwrap();
        hook_claude_code(dir.path()).unwrap();

        // Capture skill directory contents after first init.
        let skills_dir = dir.path().join(".claude/skills");
        let first_count = fs::read_dir(&skills_dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("writ-")
            })
            .count();

        // Second init should not duplicate skill directories.
        hook_claude_code(dir.path()).unwrap();
        let second_count = fs::read_dir(&skills_dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("writ-")
            })
            .count();

        assert_eq!(
            first_count, second_count,
            "reinit should not create duplicate skill directories"
        );
    }

    #[test]
    fn test_init_skill_directories_have_correct_structure() {
        let dir = tempdir().unwrap();
        hook_claude_code(dir.path()).unwrap();

        for template in crate::skills::SKILL_TEMPLATES {
            let skill_dir = dir.path().join(".claude/skills").join(template.name);
            assert!(skill_dir.is_dir(), "missing skill dir: {}", template.name);

            let skill_md = skill_dir.join("SKILL.md");
            assert!(skill_md.exists(), "missing SKILL.md in {}", template.name);

            let content = fs::read_to_string(&skill_md).unwrap();
            assert!(
                content.starts_with("---"),
                "{}/SKILL.md must start with YAML frontmatter delimiter",
                template.name
            );

            // Supporting files should be present.
            for sf in template.supporting_files {
                let sf_path = skill_dir.join(sf.filename);
                assert!(
                    sf_path.exists(),
                    "{}/{} supporting file missing",
                    template.name,
                    sf.filename
                );
            }
        }
    }

    #[test]
    fn test_uninit_preserves_non_writ_skills() {
        let dir = tempdir().unwrap();
        hook_claude_code(dir.path()).unwrap();

        // Create a custom skill.
        let custom = dir.path().join(".claude/skills/my-custom-skill");
        fs::create_dir_all(&custom).unwrap();
        fs::write(custom.join("SKILL.md"), "# Custom Skill\n").unwrap();

        unhook_claude_code(dir.path()).unwrap();

        assert!(custom.exists(), "custom skill should survive uninit");
        assert!(
            custom.join("SKILL.md").exists(),
            "custom SKILL.md should survive"
        );
    }

    #[test]
    fn test_reinit_updates_stale_skill_content() {
        let dir = tempdir().unwrap();
        hook_claude_code(dir.path()).unwrap();

        // Corrupt a SKILL.md to simulate stale content.
        let skill_md = dir.path().join(".claude/skills/writ-context/SKILL.md");
        fs::write(&skill_md, "stale content").unwrap();

        // Reinit should fix the stale content.
        hook_claude_code(dir.path()).unwrap();

        let content = fs::read_to_string(&skill_md).unwrap();
        assert!(
            content.starts_with("---"),
            "reinit should restore YAML frontmatter at byte 0"
        );
        assert!(
            content.contains("writ-context"),
            "reinit should restore correct skill content"
        );
    }

    #[test]
    fn test_init_with_existing_skills_no_corruption() {
        let dir = tempdir().unwrap();
        hook_claude_code(dir.path()).unwrap();

        // Read original content of all SKILL.md files.
        let mut originals = std::collections::HashMap::new();
        for template in crate::skills::SKILL_TEMPLATES {
            let path = dir
                .path()
                .join(".claude/skills")
                .join(template.name)
                .join("SKILL.md");
            originals.insert(template.name, fs::read_to_string(&path).unwrap());
        }

        // Run init again — content should be identical.
        hook_claude_code(dir.path()).unwrap();

        for template in crate::skills::SKILL_TEMPLATES {
            let path = dir
                .path()
                .join(".claude/skills")
                .join(template.name)
                .join("SKILL.md");
            let after = fs::read_to_string(&path).unwrap();
            assert_eq!(
                originals[template.name], after,
                "{}/SKILL.md content changed after reinit",
                template.name
            );
        }
    }

    #[test]
    fn test_init_skill_count_matches_template_count() {
        let dir = tempdir().unwrap();
        hook_claude_code(dir.path()).unwrap();

        let skills_dir = dir.path().join(".claude/skills");
        let skill_count = fs::read_dir(&skills_dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("writ-")
            })
            .count();

        assert_eq!(
            skill_count,
            crate::skills::SKILL_TEMPLATES.len(),
            "init should create exactly as many skill dirs as templates"
        );
    }
}

#[cfg(test)]
mod stale_managed_text_tests {
    use super::*;

    #[test]
    fn fresh_hook_and_block_are_current_and_edits_are_stale() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        hook_claude_code(root).unwrap();
        assert!(
            stale_managed_text(root).is_empty(),
            "{:?}",
            stale_managed_text(root)
        );

        let md = fs::read_to_string(root.join("CLAUDE.md")).unwrap();
        fs::write(
            root.join("CLAUDE.md"),
            md.replace(
                "writ seals the edits it can attribute",
                "writ seals your edits",
            ),
        )
        .unwrap();
        let path = root.join(".claude/settings.json");
        let text = fs::read_to_string(&path).unwrap();
        fs::write(&path, text.replace("## Writ VCS Active", "## Writ VCS On")).unwrap();
        assert_eq!(
            stale_managed_text(root),
            vec!["CLAUDE.md".to_string(), ".claude/settings.json".to_string()]
        );

        hook_claude_code(root).unwrap();
        assert!(stale_managed_text(root).is_empty());
    }
}

#[cfg(test)]
mod claim_parity_tests {
    use super::*;

    /// Finding 88: every generated surface (CLAUDE.md, AGENTS.md, generic
    /// instructions, settings instruction and hook, slash commands, skills)
    /// that shows a `writ spec add` command shows it with `--claim`.
    #[test]
    fn every_generated_spec_add_claims() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        hook_claude_code(root).unwrap();
        hook_codex(root).unwrap();
        hook_generic(root).unwrap();
        let mut checked = 0;
        for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
            if !entry.file_type().is_file() {
                continue;
            }
            let Ok(text) = fs::read_to_string(entry.path()) else {
                continue;
            };
            // The settings hook is JSON: split its echo steps into lines.
            let text = text.replace("&& echo", "\n");
            for line in text.lines() {
                for (i, _) in line.match_indices("writ spec add ") {
                    let rest = &line[i + "writ spec add ".len()..];
                    // A command, not prose: an argument follows.
                    if !(rest.starts_with('"') || rest.starts_with("--") || rest.starts_with('\\'))
                    {
                        continue;
                    }
                    let cmd_end = rest.find('`').unwrap_or(rest.len());
                    assert!(
                        rest[..cmd_end].contains("--claim"),
                        "{}: {line}",
                        entry.path().display()
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked >= 5, "only {checked} spec add commands found");
    }
}

#[cfg(test)]
mod hook_path_resolution_tests {
    //! Finding 89: the SessionStart hook runs PATH's writ, falls back to the
    //! writ that ran init, and says so when the two differ.
    use super::*;

    /// A fake `writ` that prints its own name for `context`.
    #[cfg(unix)]
    fn fake_writ(dir: &Path, name: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir_all(dir).unwrap();
        let p = dir.join("writ");
        fs::write(
            &p,
            format!("#!/bin/sh\necho \"context-brief from {name}\"\n"),
        )
        .unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(unix)]
    fn run_hook(cmd: &str, path: &str) -> String {
        let out = std::process::Command::new("/bin/sh")
            .args(["-c", cmd])
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[cfg(unix)]
    #[test]
    fn hook_prefers_path_writ_and_falls_back_to_init_writ() {
        let dir = tempfile::tempdir().unwrap();
        let init = fake_writ(&dir.path().join("init bin"), "init");
        let newer = fake_writ(&dir.path().join("newer"), "newer");
        let cmd = session_start_command(init.to_str().unwrap());
        let sys = "/usr/bin:/bin";

        // PATH has a different writ: it runs, with one note naming both.
        let out = run_hook(
            &cmd,
            &format!("{}:{sys}", newer.parent().unwrap().display()),
        );
        assert!(out.contains("context-brief from newer"), "{out}");
        assert!(!out.contains("from init"), "{out}");
        let note: Vec<&str> = out.lines().filter(|l| l.starts_with("Note:")).collect();
        assert_eq!(note.len(), 1, "{out}");
        assert!(
            note[0].contains(newer.to_str().unwrap()) && note[0].contains(init.to_str().unwrap())
        );

        // PATH's writ is the init writ: no note.
        let out = run_hook(&cmd, &format!("{}:{sys}", init.parent().unwrap().display()));
        assert!(
            out.contains("context-brief from init") && !out.contains("Note:"),
            "{out}"
        );

        // No writ on PATH: the init writ runs, no note.
        let out = run_hook(&cmd, sys);
        assert!(
            out.contains("context-brief from init") && !out.contains("Note:"),
            "{out}"
        );

        // Neither exists: the hook still exits 0 and prints the steps.
        let out = run_hook(&session_start_command("/nonexistent/writ"), sys);
        assert!(out.contains(WRIT_HOOK_BANNER), "{out}");
    }

    #[test]
    fn recorded_fallback_round_trips() {
        for p in [
            "writ",
            "/opt/homebrew/bin/writ",
            "/tmp/with space/writ",
            "/tmp/it's/writ",
        ] {
            assert_eq!(hook_writ_path(&session_start_command(p)), p);
        }
    }

    /// Settings holding `cmd` as the start hook and today's end hook for
    /// the same recorded writ.
    fn settings_with(cmd: &str) -> serde_json::Value {
        let mut hooks = serde_json::Map::new();
        for (event, matcher, c) in expected_hooks(&hook_writ_path(cmd)) {
            let c = if event == WRIT_HOOK_EVENT {
                cmd.to_string()
            } else {
                c
            };
            let mut entry = serde_json::json!({"hooks": [{"type": "command", "command": c}]});
            if let Some(m) = matcher {
                entry["matcher"] = serde_json::json!(m);
            }
            hooks
                .entry(event)
                .or_insert_with(|| serde_json::json!([]))
                .as_array_mut()
                .unwrap()
                .push(entry);
        }
        serde_json::json!({ "hooks": hooks })
    }

    /// Sprint 6: a 0.4.2 project (start hook and `session-end` only) is
    /// stale until `writ init -y` adds the auto seal hooks.
    #[test]
    fn project_without_auto_seal_hooks_is_stale() {
        let start = session_start_command("writ");
        let v042 = serde_json::json!({"hooks": {
            "SessionStart": [{"hooks": [{"type": "command", "command": start}]}],
            "SessionEnd": [{"hooks": [{"type": "command",
                "command": session_end_command("writ")}]}]
        }});
        assert!(settings_hook_stale(&v042));
        let mut wrong_matcher = settings_with(&start);
        wrong_matcher["hooks"]["PostToolUse"][0]["matcher"] = serde_json::json!("Edit");
        assert!(settings_hook_stale(&wrong_matcher));
    }

    #[test]
    fn start_hook_without_end_hook_is_stale() {
        // A project initialized by 0.4.1: start hook only.
        let start = session_start_command("writ");
        let only_start = serde_json::json!({"hooks": {
            "SessionStart": [{"hooks": [{"type": "command", "command": start}]}]
        }});
        assert!(settings_hook_stale(&only_start));
        let gone_end = serde_json::json!({"hooks": {
            "SessionStart": [{"hooks": [{"type": "command", "command": start}]}],
            "SessionEnd": [{"hooks": [{"type": "command",
                "command": session_end_command("/nonexistent/writ")}]}]
        }});
        assert!(settings_hook_stale(&gone_end));
        assert!(!settings_hook_stale(&settings_with(&start)));
    }

    #[test]
    fn end_hook_fallback_round_trips() {
        for p in ["writ", "/opt/homebrew/bin/writ", "/tmp/it's/writ"] {
            let cmd = session_end_command(p);
            assert_eq!(hook_writ_path(&cmd), p);
            assert!(is_writ_hook_command(&cmd));
        }
    }

    /// The end hook runs PATH's writ with the session-end subcommand, prints
    /// nothing, and exits 0 even when no writ can run.
    #[cfg(unix)]
    #[test]
    fn end_hook_runs_path_writ_silently_and_never_fails() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let log = dir.path().join("args.log");
        let w = bin.join("writ");
        fs::write(
            &w,
            format!(
                "#!/bin/sh\necho \"$@\" > '{}'\necho noise\nexit 3\n",
                log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&w, fs::Permissions::from_mode(0o755)).unwrap();
        let cmd = session_end_command("/nonexistent/writ");
        let out = run_hook(&cmd, &format!("{}:/usr/bin:/bin", bin.display()));
        assert_eq!(out, "");
        assert_eq!(
            fs::read_to_string(&log).unwrap().trim(),
            SESSION_END_SUBCOMMAND
        );
        // No writ anywhere: still exit 0, silent.
        assert_eq!(run_hook(&cmd, "/usr/bin:/bin"), "");
    }

    #[test]
    fn pinned_hook_from_040_is_stale_and_current_hook_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("writ");
        fs::write(&live, "").unwrap();
        let live = live.to_str().unwrap();
        // The 0.4.0 form: the steps, then `<path> context --format brief`.
        let current = session_start_command(live);
        let head = &current[..current.find(HOOK_FALLBACK_PREFIX).unwrap()];
        let pinned = format!("{head}{live} context --format brief 2>/dev/null || true");
        assert_eq!(hook_writ_path(&pinned), live);
        assert!(settings_hook_stale(&settings_with(&pinned)));
        assert!(!settings_hook_stale(&settings_with(&current)));
    }

    #[test]
    fn hook_whose_fallback_was_removed_is_stale() {
        let gone = session_start_command("/nonexistent/finding-89/writ");
        assert!(settings_hook_stale(&settings_with(&gone)));
        assert!(!settings_hook_stale(&settings_with(
            &session_start_command("writ")
        )));
    }
}
