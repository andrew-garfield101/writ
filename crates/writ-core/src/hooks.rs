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
    let mut settings: serde_json::Value = if settings_path.exists() {
        let content = fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    // Navigate to permissions.allow, creating the structure if needed.
    let permissions = settings
        .as_object_mut()
        .unwrap()
        .entry("permissions")
        .or_insert_with(|| serde_json::json!({}));
    let allow = permissions
        .as_object_mut()
        .unwrap()
        .entry("allow")
        .or_insert_with(|| serde_json::json!([]));

    let allow_arr = allow.as_array_mut().unwrap();

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
const WRIT_INSTRUCTION: &str = "MANDATORY: This project uses writ for version control. \
You MUST follow this workflow — it is not optional. \
1) BEFORE starting any work, run `writ context` to check project state. \
2) If no spec is assigned to you, create one: `writ spec add \"brief description of your task\"`. \
3) AFTER each meaningful unit of work, run `writ seal -s '<summary>'` to checkpoint (auto-scoped to your spec). \
4) When your task is complete, run `writ spec done` BEFORE reporting results (auto-scoped to your spec). \
Your task is NOT complete until you have sealed your work. \
NEVER run `git commit`, `git add`, `git push`, or `writ finish` — the user handles git.";

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
    let mut settings: serde_json::Value = if settings_path.exists() {
        let content = fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    let instructions = settings
        .as_object_mut()
        .unwrap()
        .entry("instructions")
        .or_insert_with(|| serde_json::json!([]));

    let arr = instructions.as_array_mut().unwrap();

    // Check if a writ instruction is already present.
    let already_has = arr.iter().any(|v| {
        v.as_str()
            .map_or(false, |s| s.contains(WRIT_INSTRUCTION_MARKER))
    });

    if already_has {
        return Ok(None);
    }

    arr.push(serde_json::Value::String(WRIT_INSTRUCTION.to_string()));

    let json = serde_json::to_string_pretty(&settings)
        .map_err(|e| crate::error::WritError::Other(format!("JSON serialize: {}", e)))?;
    atomic_write(&settings_path, format!("{}\n", json).as_bytes())?;

    Ok(Some(".claude/settings.json".to_string()))
}

/// Remove the writ instruction from `.claude/settings.json` during uninit.
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

    let removed = if let Some(instructions) = settings.get_mut("instructions") {
        if let Some(arr) = instructions.as_array_mut() {
            let before = arr.len();
            arr.retain(|v| {
                v.as_str()
                    .map_or(true, |s| !s.contains(WRIT_INSTRUCTION_MARKER))
            });
            arr.len() < before
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

/// Substring used to detect whether a writ hook is already present.
const WRIT_HOOK_MARKER: &str = "writ context";

/// Build the hook command with an absolute path to the writ binary.
/// Falls back to bare `writ` if the binary can't be located.
fn writ_hook_command() -> String {
    let writ_path = std::process::Command::new("which")
        .arg("writ")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "writ".to_string());
    format!(
        "echo '## Writ VCS Active' && \
         echo 'This project uses writ for version control.' && \
         echo 'Your FIRST action: create or claim a spec for your task.' && \
         echo '  - If unclaimed specs exist below, claim one: writ spec claim <id>' && \
         echo '  - If none match your task, create one: writ spec add \"brief task description\"' && \
         echo '' && \
         {writ} context 2>/dev/null || true",
        writ = writ_path
    )
}

/// The hook event names where writ injects context.
/// - SessionStart: fires once when a session begins (agent sees writ state immediately)
/// - UserPromptSubmit: fires on every user prompt (agent sees updated state each turn)
const WRIT_HOOK_EVENTS: &[&str] = &["SessionStart", "UserPromptSubmit"];

/// Check if a hook event array already contains a writ hook entry.
fn has_writ_hook_entry(arr: &[serde_json::Value]) -> bool {
    arr.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map_or(false, |inner| {
                inner.iter().any(|hook| {
                    hook.get("command")
                        .and_then(|c| c.as_str())
                        .map_or(false, |cmd| cmd.contains(WRIT_HOOK_MARKER))
                })
            })
    })
}

/// Remove writ hook entries from a hook event array. Returns true if any were removed.
fn remove_writ_hook_entries(arr: &mut Vec<serde_json::Value>) -> bool {
    let before = arr.len();
    arr.retain(|entry| {
        !entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map_or(false, |inner| {
                inner.iter().any(|hook| {
                    hook.get("command")
                        .and_then(|c| c.as_str())
                        .map_or(false, |cmd| cmd.contains(WRIT_HOOK_MARKER))
                })
            })
    });
    arr.len() < before
}

/// Add `SessionStart` and `UserPromptSubmit` hooks to `.claude/settings.json`
/// that run `writ context` to inject project state into agent conversations.
///
/// - `SessionStart`: agent sees writ state the moment a session opens
/// - `UserPromptSubmit`: agent sees updated state on every subsequent prompt
fn ensure_claude_hook(root: &Path) -> WritResult<Option<String>> {
    let claude_dir = root.join(".claude");
    if !claude_dir.exists() {
        fs::create_dir_all(&claude_dir)?;
    }

    let settings_path = claude_dir.join("settings.json");
    let mut settings: serde_json::Value = if settings_path.exists() {
        let content = fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    let hooks = settings
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));

    let hooks_obj = match hooks.as_object_mut() {
        Some(obj) => obj,
        None => {
            // hooks field exists but isn't an object — don't touch it
            return Ok(None);
        }
    };

    let command = writ_hook_command();
    let hook_entry = serde_json::json!({
        "hooks": [
            {
                "type": "command",
                "command": command,
                "timeout": 10
            }
        ]
    });

    let mut any_added = false;

    for event_name in WRIT_HOOK_EVENTS {
        let event_hooks = hooks_obj
            .entry(*event_name)
            .or_insert_with(|| serde_json::json!([]));

        let arr = match event_hooks.as_array_mut() {
            Some(a) => a,
            None => continue, // Not an array — don't touch it
        };

        if !has_writ_hook_entry(arr) {
            arr.push(hook_entry.clone());
            any_added = true;
        }
    }

    if !any_added {
        return Ok(None);
    }

    let json = serde_json::to_string_pretty(&settings)
        .map_err(|e| crate::error::WritError::Other(format!("JSON serialize: {}", e)))?;
    atomic_write(&settings_path, format!("{}\n", json).as_bytes())?;

    Ok(Some(".claude/settings.json".to_string()))
}

/// Remove writ hooks from all event types in `.claude/settings.json` during uninit.
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

    let mut any_removed = false;

    if let Some(hooks) = settings.get_mut("hooks") {
        for event_name in WRIT_HOOK_EVENTS {
            if let Some(event_hooks) = hooks.get_mut(*event_name) {
                if let Some(arr) = event_hooks.as_array_mut() {
                    if remove_writ_hook_entries(arr) {
                        any_removed = true;
                    }
                }
            }
        }
    }

    if any_removed {
        let json = serde_json::to_string_pretty(&settings)
            .map_err(|e| crate::error::WritError::Other(format!("JSON serialize: {}", e)))?;
        atomic_write(&settings_path, format!("{}\n", json).as_bytes())?;
        Ok(Some(".claude/settings.json".to_string()))
    } else {
        Ok(None)
    }
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

    // Add UserPromptSubmit hook to inject writ context at conversation start.
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

    // Remove UserPromptSubmit hook from .claude/settings.json.
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

/// Check if a directory is empty.
fn is_dir_empty(path: &Path) -> bool {
    fs::read_dir(path)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false)
}

fn writ_claude_md_section() -> String {
    r#"## Writ Version Control (MANDATORY)

**FIRST ACTION: Run `writ context` before doing anything else.**

**EVERY TASK requires these 3 commands — no exceptions, no matter how small the task:**
1. `writ spec add "brief description of your task"` — register your task (ID auto-generated)
2. `writ seal -s "<summary>"` — after each unit of work (auto-scoped to your spec)
3. `writ spec done` — BEFORE reporting results to the user (auto-scoped to your spec)

Your task is NOT complete until you have run all three.
Do NOT run `git commit`, `git add`, `git push`, or `writ finish`.

### Commands
- `writ context` — structured project state (files, specs, activity). Run this FIRST.
- `writ seal -s "<summary>"` — checkpoint your work (auto-scoped to your claimed spec)
- `writ spec add "brief task description"` — create a task spec (ID auto-generated)
- `writ spec status` — view active specs
- `writ spec done` — mark your task complete (auto-scoped to your claimed spec)
- `writ status` — project overview (agents, specs, progress)
- `writ diff` — preview file changes
- `writ log` — recent seal history

### Rules
- You MUST run `writ context` before starting work
- You MUST run `writ seal -s "<summary>"` after each meaningful unit of work
- You MUST run `writ spec done` before reporting results to the user
- You MUST include meaningful summaries in seals so other agents understand your work
- Do NOT run `git add`, `git commit`, or `git push` — the user manages the git round-trip via `writ finish`
- Do NOT run `writ finish` — this is the git integration command managed by the user or orchestrator

### Multi-Agent (Same Directory)
- Multiple agents can work in the same directory simultaneously
- Seals are auto-scoped to your claimed spec — no need for explicit `--spec`
- Check `writ context` to see other agents' activity and convergence state
"#.to_string()
}

fn writ_agents_md_section() -> String {
    r#"## Version Control — Writ (MANDATORY)

This project uses writ for version control. You MUST follow this workflow.

### Required Workflow
1. BEFORE starting any work, run `writ context` to check project state
2. Create a spec for your task: `writ spec add "brief description of your task"`
3. AFTER each meaningful unit of work, run `writ seal -s "<summary>"` (auto-scoped to your spec)
4. When complete, run `writ spec done` BEFORE reporting results (auto-scoped to your spec)

Your task is NOT complete until you have sealed your work.

### Commands
- `writ context` — structured project state. Run this FIRST.
- `writ seal -s "<summary>"` — checkpoint work (auto-scoped to your spec)
- `writ spec add "task description"` — create a spec (ID auto-generated)
- `writ spec done` — mark task complete (auto-scoped)
- `writ status` — project overview
- `writ log` — seal history

Do NOT run `git commit` or `writ finish` — the user manages the git round-trip.
`writ restore <seal-id>` overwrites working directory files — use only when reverting to a known-good state.
"#
    .to_string()
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

1. BEFORE starting any work, run `writ context` to check project state
2. If no spec is assigned to you, create one: `writ spec add "brief description of your task"`
3. Do your work in small increments
4. AFTER each meaningful unit of work, run `writ seal -s "<summary>"` to checkpoint (auto-scoped to your spec)
5. Check `writ context` periodically to see what other agents have done
6. When task is complete, run `writ spec done` BEFORE reporting results (auto-scoped to your spec)

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
- `writ seal -s "<summary>"` — checkpoint work (auto-scoped to your spec)
- `writ spec add "brief task description"` — create a task spec (ID auto-generated)
- `writ spec status` — view active specs
- `writ spec done` — mark your task complete (auto-scoped to your spec)
- `writ status` — project overview (agents, specs, progress)
- `writ diff` — preview file changes
- `writ log` — recent seal history
- `writ restore <seal-id>` — roll back to a previous seal (overwrites working directory files)

## Rules

- You MUST run `writ context` before starting work
- You MUST run `writ seal -s "<summary>" --agent claude-code` after each meaningful unit of work
- You MUST run `writ spec done` before reporting results to the user
- Your task is NOT complete until sealed
- Include meaningful summaries in seals for other agents' context
- Include test results when available: `--tests-passed N --tests-failed M`
- Do NOT run `git add`, `git commit`, or `git push` — the user manages git via `writ finish`
- Do NOT run `writ finish` — this is the git integration command managed by the user or orchestrator

## Integration Examples

### System Prompt Snippet
Add to your agent's system prompt:
"MANDATORY: This project uses writ for version control. You MUST run `writ context`
before starting work. You MUST run `writ seal -s '<summary>' --agent claude-code` after each unit of work.
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
    .to_string()
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
        assert!(
            content.contains("## Writ Version Control"),
            "missing writ section"
        );
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
        let modified = before.replace("## Writ Version Control", "## Writ OLD");
        fs::write(dir.path().join("CLAUDE.md"), &modified).unwrap();

        // Reinit should replace the section.
        let result = hook_claude_code(dir.path()).unwrap();
        assert!(result.files_updated.contains(&"CLAUDE.md".to_string()));

        let after = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(after.contains("## Writ Version Control"));
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
            content.contains("## Writ Version Control"),
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

    #[test]
    fn test_claude_md_section_has_spec_done_workflow() {
        let section = writ_claude_md_section();
        assert!(
            section.contains("writ spec done"),
            "missing writ spec done in workflow"
        );
        assert!(
            section.contains("EVERY TASK requires these 3 commands"),
            "missing front-loaded mandatory workflow"
        );
    }

    #[test]
    fn test_claude_md_section_has_round_trip_commands() {
        let section = writ_claude_md_section();
        assert!(
            section.contains("writ status"),
            "missing writ status command"
        );
        assert!(section.contains("writ diff"), "missing writ diff command");
        assert!(
            section.contains("writ spec done"),
            "missing writ spec done command"
        );
    }

    #[test]
    fn test_claude_md_section_prohibits_git_and_finish() {
        let section = writ_claude_md_section();
        assert!(
            section.contains("Do NOT run `git add`, `git commit`, or `git push`"),
            "missing git prohibition"
        );
        assert!(
            section.contains("Do NOT run `writ finish`"),
            "missing writ finish prohibition"
        );
    }

    #[test]
    fn test_claude_md_section_has_writ_finish_prohibition() {
        let section = writ_claude_md_section();
        assert!(
            section.contains("writ finish"),
            "missing writ finish reference"
        );
        // The template should tell agents NOT to run writ finish
        assert!(
            section.contains("Do NOT run `writ finish`"),
            "agents should be told not to run writ finish"
        );
    }

    #[test]
    fn test_claude_md_section_has_guidelines_section() {
        let section = writ_claude_md_section();
        assert!(section.contains("### Rules"), "missing rules heading");
        assert!(
            section.contains("writ seal"),
            "missing seal checkpoint guideline"
        );
    }

    #[test]
    fn test_claude_md_section_has_mandatory_language() {
        let section = writ_claude_md_section();
        assert!(section.contains("MUST"), "missing MUST directive");
        assert!(
            section.contains("NOT complete"),
            "missing completion gate language"
        );
    }

    #[test]
    fn test_claude_md_section_has_mandatory_commands() {
        let section = writ_claude_md_section();
        // Front-loaded mandatory commands must be present
        for cmd in &[
            "writ context",
            "writ seal",
            "writ spec add",
            "writ spec done",
        ] {
            assert!(section.contains(cmd), "missing {} command", cmd);
        }
    }

    #[test]
    fn test_agents_md_section_has_spec_done_workflow() {
        let section = writ_agents_md_section();
        assert!(
            section.contains("writ spec done"),
            "missing writ spec done in workflow"
        );
    }

    #[test]
    fn test_agents_md_section_has_round_trip_commands() {
        let section = writ_agents_md_section();
        assert!(
            section.contains("writ status"),
            "missing writ status command"
        );
        assert!(
            section.contains("writ spec add"),
            "missing spec add command"
        );
        assert!(
            section.contains("writ spec done"),
            "missing spec done command"
        );
    }

    #[test]
    fn test_agents_md_section_prohibits_git_and_finish() {
        let section = writ_agents_md_section();
        assert!(
            section.contains("Do NOT run `git commit` or `writ finish`"),
            "missing git/finish prohibition"
        );
    }

    #[test]
    fn test_agents_md_section_is_focused() {
        let section = writ_agents_md_section();
        // Round-trip templates are streamlined — no convergence/rollback/human round-trip
        assert!(
            !section.contains("Convergence"),
            "agents template should not include convergence details"
        );
        assert!(
            !section.contains("Rollback"),
            "agents template should not include rollback details"
        );
    }

    #[test]
    fn test_agents_md_section_has_mandatory_language() {
        let section = writ_agents_md_section();
        assert!(section.contains("MUST"), "missing MUST directive");
        assert!(
            section.contains("NOT complete"),
            "missing completion gate language"
        );
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
    fn test_claude_md_has_git_commit_prohibition() {
        let section = writ_claude_md_section();
        assert!(
            section.contains("Do NOT run `git commit`"),
            "CLAUDE.md template should prohibit git commit"
        );
        assert!(
            section.contains("Do NOT run `writ finish`"),
            "CLAUDE.md template should prohibit writ finish"
        );
    }

    #[test]
    fn test_agents_md_has_restore_warning() {
        let section = writ_agents_md_section();
        assert!(
            section.contains("writ restore"),
            "AGENTS.md template should mention writ restore"
        );
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
                    .map_or(false, |s| s.contains(WRIT_INSTRUCTION_MARKER))
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
        let instructions = parsed["instructions"].as_array().unwrap();
        assert!(instructions.is_empty());
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

    // --- Claude Code hook tests (SessionStart + UserPromptSubmit) ---

    #[test]
    fn test_ensure_claude_hook_creates_both_events() {
        let dir = tempdir().unwrap();
        let result = ensure_claude_hook(dir.path()).unwrap();
        assert!(result.is_some());

        let content = fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        // Both SessionStart and UserPromptSubmit should have hooks.
        for event in WRIT_HOOK_EVENTS {
            let hooks = parsed["hooks"][event].as_array().unwrap();
            assert_eq!(hooks.len(), 1, "{} should have exactly 1 hook entry", event);
            let inner = hooks[0]["hooks"].as_array().unwrap();
            assert_eq!(inner[0]["type"].as_str().unwrap(), "command");
            assert!(inner[0]["command"]
                .as_str()
                .unwrap()
                .contains(WRIT_HOOK_MARKER));
        }
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

        let content = fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        // Existing fields preserved.
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
        // Both hooks added.
        for event in WRIT_HOOK_EVENTS {
            let hooks = parsed["hooks"][event].as_array().unwrap();
            assert_eq!(hooks.len(), 1);
        }
    }

    #[test]
    fn test_ensure_claude_hook_idempotent() {
        let dir = tempdir().unwrap();
        ensure_claude_hook(dir.path()).unwrap();
        let result = ensure_claude_hook(dir.path()).unwrap();
        assert!(result.is_none(), "second call should be a no-op");

        let content = fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        for event in WRIT_HOOK_EVENTS {
            let hooks = parsed["hooks"][event].as_array().unwrap();
            assert_eq!(hooks.len(), 1, "{} hook should not be duplicated", event);
        }
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

        let content = fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        // PreToolUse preserved.
        assert!(parsed["hooks"]["PreToolUse"].as_array().unwrap().len() == 1);
        // Both writ hooks added.
        for event in WRIT_HOOK_EVENTS {
            assert!(parsed["hooks"][event].as_array().unwrap().len() == 1);
        }
    }

    #[test]
    fn test_remove_claude_hook_removes_both_events() {
        let dir = tempdir().unwrap();
        ensure_claude_hook(dir.path()).unwrap();
        let result = remove_claude_hook(dir.path()).unwrap();
        assert!(result.is_some());

        let content = fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        for event in WRIT_HOOK_EVENTS {
            let hooks = parsed["hooks"][event].as_array().unwrap();
            assert!(
                hooks.is_empty(),
                "{} hooks should be empty after removal",
                event
            );
        }
    }

    #[test]
    fn test_remove_claude_hook_preserves_other() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".claude")).unwrap();
        // Set up settings with both a writ hook and a custom hook in UserPromptSubmit.
        let settings = serde_json::json!({
            "hooks": {
                "UserPromptSubmit": [
                    {
                        "hooks": [{"type": "command", "command": "echo custom"}]
                    },
                    {
                        "hooks": [{"type": "command", "command": "writ context 2>/dev/null || true"}]
                    }
                ],
                "SessionStart": [
                    {
                        "hooks": [{"type": "command", "command": "writ context 2>/dev/null || true"}]
                    }
                ]
            }
        });
        fs::write(
            dir.path().join(".claude").join("settings.json"),
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();

        remove_claude_hook(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        // UserPromptSubmit: only custom hook remains.
        let ups = parsed["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(ups.len(), 1, "should only remove the writ hook");
        assert!(ups[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("echo custom"));
        // SessionStart: writ hook removed, array empty.
        let ss = parsed["hooks"]["SessionStart"].as_array().unwrap();
        assert!(ss.is_empty());
    }

    #[test]
    fn test_remove_claude_hook_noop_when_missing() {
        let dir = tempdir().unwrap();
        let result = remove_claude_hook(dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_hook_claude_code_adds_both_hooks() {
        let dir = tempdir().unwrap();
        hook_claude_code(dir.path()).unwrap();

        let settings_path = dir.path().join(".claude").join("settings.json");
        let content = fs::read_to_string(&settings_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        for event in WRIT_HOOK_EVENTS {
            let hooks = parsed["hooks"][event].as_array().unwrap();
            assert_eq!(hooks.len(), 1, "hook_claude_code should add {} hook", event);
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
