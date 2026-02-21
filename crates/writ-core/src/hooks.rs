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

/// Generate writ integration hooks for Claude Code.
pub fn hook_claude_code(root: &Path) -> WritResult<HookResult> {
    let mut created = Vec::new();
    let mut updated = Vec::new();

    let claude_md = root.join("CLAUDE.md");
    let writ_section = writ_claude_md_section();

    if claude_md.exists() {
        let content = fs::read_to_string(&claude_md)?;
        if !content.contains("## Writ") {
            let new_content = format!("{content}\n{writ_section}");
            atomic_write(&claude_md, new_content.as_bytes())?;
            updated.push("CLAUDE.md".to_string());
        }
    } else {
        atomic_write(&claude_md, writ_section.as_bytes())?;
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

    if agents_md.exists() {
        let content = fs::read_to_string(&agents_md)?;
        if !content.contains("## Writ") {
            let new_content = format!("{content}\n{writ_section}");
            atomic_write(&agents_md, new_content.as_bytes())?;
            updated.push("AGENTS.md".to_string());
        }
    } else {
        atomic_write(&agents_md, writ_section.as_bytes())?;
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

    Ok(results)
}

fn writ_claude_md_section() -> String {
    r#"## Writ

This project uses **writ** for AI-native version control. Use writ alongside git.

### Quick reference

```bash
writ context                                              # full project state
writ context --spec my-feature                            # scoped to a spec
writ seal -s "what you did" --agent your-id --spec feat   # checkpoint
writ seal -s "done" --agent your-id --spec feat --status complete  # final seal
writ log --limit 5                                        # recent seals
```

### Python API

```python
import writ
repo = writ.Repository.open(".")
ctx = repo.context(spec="my-feature")
repo.seal(summary="what you did", agent_id="your-id", agent_type="agent", spec_id="feat", status="in-progress")
```

### Workflow

1. Run `writ context` at the start of every task to understand project state
2. Do your work in small increments
3. Run `writ seal` after each meaningful chunk (defaults to status: in-progress)
4. Check `writ context` periodically to see what other agents have done
5. Use `--status complete` only on your final seal for a spec

### Rules

- Always seal your work before finishing a task
- Use --spec to link seals to specs when working on a defined spec
- Use --status in-progress for intermediate work (this is the default)
- Use --status complete only when the spec is fully done
- Include test results when available (--tests-passed N --tests-failed M)
- Use `writ log --all` to see seals from all branches (including diverged ones)
- If context shows unsealed changes, seal before starting new work
- When context shows `session_complete: true`, all specs are done — run `writ summary`
- Seal results include `hints` array and `file_scope_warning` — check these after each seal
- If seal returns 0 file changes, another agent may have captured your work — check `writ context`

### Rollback and recovery

If something goes wrong — tests fail, work goes sideways, convergence produces bad output:

```bash
writ log --all                        # find the last known-good seal
writ show SEAL_ID --diff              # inspect it to confirm
writ restore SEAL_ID                  # rewind working directory to that seal's state
writ seal -s "rolled back to SEAL_ID" --agent your-id  # seal the rollback
```

Every seal is an immutable snapshot. Restoring doesn't delete history — all previous seals
remain in the log. Use restore as a safety net when your changes cause regressions.

### Convergence (multi-agent)

When multiple agents work in parallel, their seals may diverge. Check for this:
- `writ context` shows `convergence_recommended: true` and `integration_risk` level
- `writ converge-all --dry-run` previews what will be merged
- `writ converge-all --apply --strategy most-complete` merges all diverged branches
- After convergence, seal the result: `writ seal -s "converged N branches" --agent convergence-bot`

Available strategies: `three-way-merge` (default, leaves conflicts unresolved), `most-recent` (prefers newest), `most-complete` (prefers version with more content).

For two-branch convergence: `writ converge <left-spec> <right-spec> --apply`

### Integration risk

Context includes an `integration_risk` field with level (low/medium/high), score (0-100), and factors.
Check this before starting work on shared files. High risk means multiple diverged branches
and files touched by many agents — converge before further work.

### Human round-trip (git integration)

When you're done, the human developer commits your work to git:

```bash
git commit -m "$(writ summary --format commit)"          # one-line commit message
gh pr create --body "$(writ summary --format pr)"         # full PR description
```
"#.to_string()
}

fn writ_agents_md_section() -> String {
    r#"## Writ

This project uses **writ** for AI-native version control.

### Before starting work

```bash
writ context --spec your-spec-id
```

### After each chunk of work

```bash
writ seal -s "description of changes" --agent your-id --spec your-spec-id --tests-passed N
```

### When finishing a spec

```bash
writ seal -s "spec complete" --agent your-id --spec your-spec-id --status complete
```

### Python API (alternative)

```python
import writ
repo = writ.Repository.open(".")
repo.context(spec="your-spec-id")
repo.seal(summary="changes", agent_id="your-id", agent_type="agent", spec_id="your-spec-id", status="complete")
```

### Guidelines

- Always run `writ context` first to understand project state
- Seal after every meaningful unit of work (defaults to status: in-progress)
- Use `--status complete` only on your final seal for a spec
- Link seals to specs with --spec when working on a defined spec
- Include verification data (--tests-passed, --tests-failed, --linted)
- Use `writ log --all` to see unified history across all branches
- When context shows `session_complete: true`, all specs are done — run `writ summary`
- Check seal results for `hints` and `file_scope_warning` fields after each seal
- If seal returns 0 file changes, another agent may have captured your work first

### Rollback and recovery

If tests fail or work goes wrong, restore to a previous seal:

```bash
writ log --all                        # find the last known-good seal
writ restore SEAL_ID                  # rewind working directory to that state
writ seal -s "rolled back" --agent your-id  # seal the rollback
```

Every seal is immutable — restoring doesn't delete history.

### Convergence (multi-agent)

- Check `integration_risk` field in context for divergence risk assessment
- `writ converge-all --dry-run` to preview, `--apply` to execute
- Strategies: `most-complete` (prefers more content), `most-recent` (prefers newest), `three-way-merge` (default, leaves conflicts unresolved)
- After convergence, seal: `writ seal -s "converged" --agent convergence-bot`

### Human round-trip

```bash
git commit -m "$(writ summary --format commit)"          # one-line commit message
gh pr create --body "$(writ summary --format pr)"         # full PR description
```
"#.to_string()
}

const CLAUDE_SEAL_COMMAND: &str = r#"Seal the current work as a writ checkpoint.

Run this command to create a structured checkpoint:

```bash
writ seal -s "$ARGUMENTS" --agent claude-code --status in-progress
```

To link to a spec, add `--spec your-spec-id`. To include test results, add `--tests-passed N`.
"#;

const CLAUDE_CONTEXT_COMMAND: &str = r#"Show the current writ context for this project.

```bash
writ context --format json
```

To scope context to a specific spec:

```bash
writ context --spec your-spec-id --format json
```
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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

    #[test]
    fn test_hook_claude_code_creates_files() {
        let dir = tempdir().unwrap();
        let result = hook_claude_code(dir.path()).unwrap();
        assert!(result.files_created.contains(&"CLAUDE.md".to_string()));
        assert!(result
            .files_created
            .contains(&".claude/commands/writ-seal.md".to_string()));
        assert!(result
            .files_created
            .contains(&".claude/commands/writ-context.md".to_string()));
        assert!(dir.path().join("CLAUDE.md").exists());
    }

    #[test]
    fn test_hook_claude_code_appends_to_existing() {
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
        assert!(content.contains("## Writ"));
    }

    #[test]
    fn test_hook_claude_code_idempotent() {
        let dir = tempdir().unwrap();
        hook_claude_code(dir.path()).unwrap();
        let result2 = hook_claude_code(dir.path()).unwrap();
        assert!(result2.files_created.is_empty());
        assert!(result2.files_updated.is_empty());
    }

    #[test]
    fn test_hook_codex_creates_agents_md() {
        let dir = tempdir().unwrap();
        let result = hook_codex(dir.path()).unwrap();
        assert!(result.files_created.contains(&"AGENTS.md".to_string()));
    }

    #[test]
    fn test_install_hooks_detects_and_hooks() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "# Project").unwrap();
        let results = install_hooks(dir.path()).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].framework, Framework::ClaudeCode);
    }

    #[test]
    fn test_claude_md_section_has_restore_guidance() {
        let section = writ_claude_md_section();
        assert!(section.contains("writ restore"), "missing restore command");
        assert!(
            section.contains("Rollback and recovery"),
            "missing rollback section"
        );
        assert!(section.contains("immutable"), "missing immutability note");
    }

    #[test]
    fn test_claude_md_section_has_round_trip_commands() {
        let section = writ_claude_md_section();
        assert!(
            section.contains("git commit -m \"$(writ summary --format commit)\""),
            "missing correct git commit command"
        );
        assert!(section.contains("gh pr create"), "missing gh pr command");
    }

    #[test]
    fn test_claude_md_section_has_convergence_strategies() {
        let section = writ_claude_md_section();
        assert!(
            section.contains("most-complete"),
            "missing most-complete strategy"
        );
        assert!(
            section.contains("most-recent"),
            "missing most-recent strategy"
        );
        assert!(
            section.contains("three-way-merge"),
            "missing three-way-merge strategy"
        );
    }

    #[test]
    fn test_agents_md_section_has_restore_guidance() {
        let section = writ_agents_md_section();
        assert!(section.contains("writ restore"), "missing restore command");
        assert!(section.contains("immutable"), "missing immutability note");
    }

    #[test]
    fn test_agents_md_section_has_round_trip_commands() {
        let section = writ_agents_md_section();
        assert!(
            section.contains("git commit -m \"$(writ summary --format commit)\""),
            "missing correct git commit command"
        );
    }
}
