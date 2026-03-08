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

/// The permission entry writ adds to Claude Code's settings.json.
const WRIT_PERMISSION: &str = "Bash(writ *)";

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

    // Check if already present.
    let already_has = allow_arr
        .iter()
        .any(|v| v.as_str() == Some(WRIT_PERMISSION));

    if already_has {
        return Ok(None);
    }

    allow_arr.push(serde_json::Value::String(WRIT_PERMISSION.to_string()));

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

/// Remove `Bash(writ *)` from `.claude/settings.json` during uninstall.
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
                arr.retain(|v| v.as_str() != Some(WRIT_PERMISSION));
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
/// usage as a user directive rather than optional project documentation.
const WRIT_INSTRUCTION: &str = "This project uses writ for version control. \
At the start of every task, run `writ context` to check project state and other agents' activity. \
After each meaningful unit of work, run `writ seal -s '<summary>' --agent claude-code` to create \
a recoverable checkpoint. When your task is complete, run `writ spec done <spec-id>`. \
Never run `git commit` or `writ finish` — the user handles the git round-trip.";

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
    let already_has = arr
        .iter()
        .any(|v| v.as_str().map_or(false, |s| s.contains(WRIT_INSTRUCTION_MARKER)));

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
            let new_content = format!("{}\n\n---\n\n{}", marked_section.trim_end(), content.trim());
            atomic_write(&claude_md, new_content.as_bytes())?;
            updated.push("CLAUDE.md".to_string());
        }
    } else {
        atomic_write(&claude_md, marked_section.as_bytes())?;
        created.push("CLAUDE.md".to_string());
    }

    let commands_dir = root.join(".claude").join("commands");
    if !commands_dir.exists() {
        fs::create_dir_all(&commands_dir)?;
    }

    let seal_cmd = commands_dir.join("writ-seal.md");
    if !seal_cmd.exists() {
        atomic_write(&seal_cmd, CLAUDE_SEAL_COMMAND.as_bytes())?;
        created.push(".claude/commands/writ-seal.md".to_string());
    }

    let context_cmd = commands_dir.join("writ-context.md");
    if !context_cmd.exists() {
        atomic_write(&context_cmd, CLAUDE_CONTEXT_COMMAND.as_bytes())?;
        created.push(".claude/commands/writ-context.md".to_string());
    }

    // Ensure Bash(writ *) permission in .claude/settings.json.
    match ensure_claude_permissions(root) {
        Ok(Some(path)) => {
            if root.join(".claude").join("settings.json").metadata().is_ok() {
                // File existed before or was just created — either way it's been set up.
                updated.push(path);
            } else {
                created.push(path);
            }
        }
        Ok(None) => {} // Already had the permission
        Err(e) => {
            eprintln!("warning: could not configure Claude Code permissions: {}", e);
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

    // Remove writ command files.
    let commands_dir = root.join(".claude").join("commands");
    let seal_cmd = commands_dir.join("writ-seal.md");
    if seal_cmd.exists() {
        fs::remove_file(&seal_cmd)?;
        removed.push(".claude/commands/writ-seal.md".to_string());
    }

    let context_cmd = commands_dir.join("writ-context.md");
    if context_cmd.exists() {
        fs::remove_file(&context_cmd)?;
        removed.push(".claude/commands/writ-context.md".to_string());
    }

    // Clean up empty commands dir (only if we emptied it).
    if commands_dir.exists() && is_dir_empty(&commands_dir) {
        let _ = fs::remove_dir(&commands_dir);
    }

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
    let trimmed = result.trim_end().to_string();
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
    r#"## Writ Version Control

This project uses writ for version control alongside git.

### Recommended Workflow
1. Run `writ context` to get structured project state
2. Create or claim a spec: `writ spec add --id <id> --title "<description>"`
3. Work on your task, checkpointing regularly: `writ seal -s "<summary>"`
4. When your task is complete: `writ spec done <id>`

### Context Retrieval
- `writ context` returns project state in token-optimized TOON format by default
- For standard JSON: `writ context --format json`

### Available Commands
- `writ context` — structured project state (files, specs, activity)
- `writ seal -s "<summary>"` — checkpoint your work
- `writ spec add --id <id> --title "<description>"` — create a task spec
- `writ spec status` — view active specs
- `writ spec done <id>` — mark your task complete
- `writ status` — project overview (agents, specs, progress)
- `writ diff` — preview file changes
- `writ log` — recent seal history

### Slash Commands
- `/writ-seal` — interactive seal creation
- `/writ-context` — get project context

### Guidelines
- Checkpoint with `writ seal` after each meaningful unit of work
- Always run `writ spec done` when your task is complete
- Include meaningful summaries in seals so other agents understand your work
- Do NOT run `git add`, `git commit`, or `git push` — the user manages the git round-trip via `writ finish`
- Do NOT run `writ finish` — this is the git integration command managed by the user or orchestrator
- `writ restore <seal-id>` overwrites working directory files — use only when you need to revert to a known-good state
"#.to_string()
}

fn writ_agents_md_section() -> String {
    r#"## Version Control — Writ

This project uses writ (AI-native version control) for checkpointing and coordination.

### Workflow
1. Run `writ context` at the start of every task to understand project state
2. Create specs for tasks: `writ spec add --id <id> --title "<description>"`
3. Checkpoint with `writ seal -s "<summary>"` after meaningful progress
4. Complete tasks with `writ spec done <id>`

### Context Retrieval
- `writ context` returns project state in token-optimized TOON format (20-33% fewer bytes)
- For standard JSON: `writ context --format json`

### Key Commands
- `writ context` — structured project state
- `writ seal -s "<summary>"` — checkpoint work
- `writ spec add / status / done` — task management
- `writ status` — project overview
- `writ log` — seal history

Do not run `git commit` or `writ finish` — the user manages the git round-trip.
`writ restore <seal-id>` overwrites working directory files — use only when reverting to a known-good state.
"#
    .to_string()
}

const CLAUDE_SEAL_COMMAND: &str = r#"Seal the current work as a writ checkpoint.

Run this command to create a structured checkpoint:

```bash
writ seal -s "$ARGUMENTS" --agent claude-code --spec your-spec-id
```

To include test results, add `--tests-passed N --tests-failed M`.
When your task is fully complete, run `writ spec done <spec-id>` instead.
"#;

const CLAUDE_CONTEXT_COMMAND: &str = r#"Show the current writ context for this project.

```bash
writ context
```

This returns project state in TOON format (20-33% fewer bytes than JSON).
For standard JSON output:

```bash
writ context --format json
```

To scope context to a specific spec:

```bash
writ context --spec your-spec-id
```
"#;

/// Content for `.writ/AGENT_INSTRUCTIONS.md`.
fn agent_instructions_content() -> String {
    r#"# Writ — Agent Workflow Instructions

Include these instructions in your agent's system prompt or configuration
to enable writ-based version control.

---

## Setup

This project uses writ for version control. The `writ` CLI is available in PATH.

## Workflow

1. Run `writ context` at the start of every task to understand project state
2. Create or claim a spec: `writ spec add --id <id> --title "<description>"`
3. Do your work in small increments
4. Run `writ seal -s "<summary>" --agent <your-id> --spec <spec-id>` after each meaningful chunk
5. Check `writ context` periodically to see what other agents have done
6. When task is complete: `writ spec done <id>`

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
- `writ seal -s "<summary>" --agent <id>` — checkpoint work
- `writ spec add --id <id> --title "<description>"` — create a task spec
- `writ spec status` — view active specs
- `writ spec done <id>` — mark your task complete (creates final seal)
- `writ status` — project overview (agents, specs, progress)
- `writ diff` — preview file changes
- `writ log` — recent seal history
- `writ restore <seal-id>` — roll back to a previous seal (overwrites working directory files)

## Rules

- Checkpoint with `writ seal` after each meaningful unit of work
- Always run `writ spec done` when your task is complete
- Include meaningful summaries in seals for other agents' context
- Include test results when available: `--tests-passed N --tests-failed M`
- Do NOT run `git add`, `git commit`, or `git push` — the user manages git via `writ finish`
- Do NOT run `writ finish` — this is the git integration command managed by the user or orchestrator

## Integration Examples

### System Prompt Snippet
Add to your agent's system prompt:
"This project uses writ for version control. Run `writ context` at the start
of each task. Checkpoint work with `writ seal -s '<summary>'`. Mark tasks
complete with `writ spec done <id>`. Do not run git commit or writ finish."

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
        assert!(result
            .files_created
            .contains(&".claude/commands/writ-seal.md".to_string()));
        assert!(result
            .files_created
            .contains(&".claude/commands/writ-context.md".to_string()));

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
            section.contains("Recommended Workflow"),
            "missing recommended workflow heading"
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
            section.contains("writ spec done <id>"),
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
        assert!(
            section.contains("### Guidelines"),
            "missing guidelines heading"
        );
        assert!(
            section.contains("writ seal"),
            "missing seal checkpoint guideline"
        );
    }

    #[test]
    fn test_claude_md_section_has_toon_reference() {
        let section = writ_claude_md_section();
        assert!(section.contains("TOON"), "missing TOON reference");
        assert!(
            section.contains("--format json"),
            "missing JSON fallback reference"
        );
    }

    #[test]
    fn test_claude_md_section_has_slash_commands() {
        let section = writ_claude_md_section();
        assert!(
            section.contains("/writ-seal"),
            "missing writ-seal slash command"
        );
        assert!(
            section.contains("/writ-context"),
            "missing writ-context slash command"
        );
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
            section.contains("spec add / status / done"),
            "missing spec task management shorthand"
        );
    }

    #[test]
    fn test_agents_md_section_prohibits_git_and_finish() {
        let section = writ_agents_md_section();
        assert!(
            section.contains("Do not run `git commit` or `writ finish`"),
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
    fn test_agents_md_section_has_toon_reference() {
        let section = writ_agents_md_section();
        assert!(section.contains("TOON"), "missing TOON reference");
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
        assert!(result
            .files_removed
            .contains(&".claude/commands/writ-seal.md".to_string()));
        assert!(result
            .files_removed
            .contains(&".claude/commands/writ-context.md".to_string()));
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

    // --- Slash command content tests ---

    #[test]
    fn test_seal_command_has_spec_and_done_docs() {
        assert!(
            CLAUDE_SEAL_COMMAND.contains("--spec"),
            "missing spec flag doc"
        );
        assert!(
            CLAUDE_SEAL_COMMAND.contains("writ spec done"),
            "missing spec done reference"
        );
    }

    #[test]
    fn test_context_command_has_toon_reference() {
        assert!(
            CLAUDE_CONTEXT_COMMAND.contains("TOON"),
            "missing TOON reference"
        );
        assert!(
            CLAUDE_CONTEXT_COMMAND.contains("--format json"),
            "missing JSON fallback"
        );
    }

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

        let content =
            fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let allow = parsed["permissions"]["allow"].as_array().unwrap();
        assert!(allow.iter().any(|v| v.as_str() == Some("Bash(writ *)")));
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

        let content =
            fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let allow = parsed["permissions"]["allow"].as_array().unwrap();
        // Both permissions should be present.
        assert!(allow.iter().any(|v| v.as_str() == Some("Bash(git *)")));
        assert!(allow.iter().any(|v| v.as_str() == Some("Bash(writ *)")));
        // Other fields preserved.
        assert_eq!(parsed["other"], serde_json::json!(true));
    }

    #[test]
    fn test_ensure_claude_permissions_idempotent() {
        let dir = tempdir().unwrap();
        ensure_claude_permissions(dir.path()).unwrap();
        let result = ensure_claude_permissions(dir.path()).unwrap();
        assert!(result.is_none(), "second call should be a no-op");

        let content =
            fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let allow = parsed["permissions"]["allow"].as_array().unwrap();
        // Should only have one entry, not duplicated.
        let count = allow
            .iter()
            .filter(|v| v.as_str() == Some("Bash(writ *)"))
            .count();
        assert_eq!(count, 1, "permission should not be duplicated");
    }

    #[test]
    fn test_remove_claude_permissions_removes_entry() {
        let dir = tempdir().unwrap();
        ensure_claude_permissions(dir.path()).unwrap();
        let result = remove_claude_permissions(dir.path()).unwrap();
        assert!(result.is_some());

        let content =
            fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let allow = parsed["permissions"]["allow"].as_array().unwrap();
        assert!(!allow.iter().any(|v| v.as_str() == Some("Bash(writ *)")));
    }

    #[test]
    fn test_remove_claude_permissions_preserves_other() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".claude")).unwrap();
        fs::write(
            dir.path().join(".claude").join("settings.json"),
            r#"{"permissions": {"allow": ["Bash(git *)", "Bash(writ *)"]}}"#,
        )
        .unwrap();

        remove_claude_permissions(dir.path()).unwrap();

        let content =
            fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let allow = parsed["permissions"]["allow"].as_array().unwrap();
        assert!(allow.iter().any(|v| v.as_str() == Some("Bash(git *)")));
        assert!(!allow.iter().any(|v| v.as_str() == Some("Bash(writ *)")));
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
        let content =
            fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        assert!(content.contains("Bash(writ *)"));

        unhook_claude_code(dir.path()).unwrap();

        // Permission should be removed.
        let content =
            fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        assert!(!content.contains("Bash(writ *)"));
    }

    #[test]
    fn test_claude_md_has_restore_warning() {
        let section = writ_claude_md_section();
        assert!(
            section.contains("writ restore"),
            "CLAUDE.md template should warn about writ restore"
        );
        assert!(
            section.contains("overwrites working directory"),
            "should explain restore is destructive"
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

        let content =
            fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
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

        let content =
            fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
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

        let content =
            fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
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

        let content =
            fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
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

        let content =
            fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
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
        let content =
            fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        assert!(content.contains(WRIT_INSTRUCTION_MARKER));

        unhook_claude_code(dir.path()).unwrap();

        // Instruction should be removed.
        let content =
            fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        assert!(
            !content.contains(WRIT_INSTRUCTION_MARKER),
            "unhook should remove writ instruction"
        );
    }

    #[test]
    fn test_claudemd_prepends_to_existing() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "# My Project\n\nUser stuff.\n").unwrap();

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
}
