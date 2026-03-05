//! Environment scanning for `writ init`.
//!
//! Silently probes the target directory and collects facts about git state,
//! existing writ state, agent framework presence, and global configuration
//! before any interactive prompts run.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::hooks::{detect_frameworks, Framework, FrameworkDetection};

/// Complete environment scan result, used to drive the init flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentScan {
    /// Absolute path of the scanned directory.
    pub path: PathBuf,

    // --- Git state ---
    /// Whether a git repository was found (via `git2::Repository::discover`).
    pub git_detected: bool,
    /// Current git branch name (e.g. "main"), if on a branch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    /// Short HEAD hash (first 12 chars).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_head_short: Option<String>,
    /// Full HEAD hash.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_head_full: Option<String>,
    /// Whether the git working tree has uncommitted changes.
    pub git_dirty: bool,
    /// Number of dirty files.
    pub git_dirty_count: usize,

    // --- Existing writ state ---
    /// True if `.writ/` already exists in the target directory.
    pub writ_already_initialized: bool,
    /// True if a legacy `settings.json` exists (needs migration).
    pub writ_has_legacy_settings: bool,
    /// True if `.writ/config.toml` already exists.
    pub writ_has_toml_config: bool,

    // --- Agent framework presence ---
    /// Whether CLAUDE.md exists in the directory.
    pub claude_md_exists: bool,
    /// Whether .claude/ directory exists.
    pub claude_dir_exists: bool,
    /// Whether AGENTS.md exists.
    pub agents_md_exists: bool,
    /// Whether .codex/ directory exists.
    pub codex_dir_exists: bool,

    /// Full framework detection results (reuses existing hooks module).
    pub frameworks: Vec<FrameworkDetection>,

    // --- Global config state ---
    /// Whether `~/.writ/config` exists (global first-run already done).
    pub global_config_exists: bool,
    /// Path to the global config file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_config_path: Option<PathBuf>,

    // --- Project metadata ---
    /// Auto-detected project name (from Cargo.toml, package.json, pyproject.toml, or dir name).
    pub project_name: String,
    /// Source of the detected project name.
    pub project_name_source: ProjectNameSource,
}

/// How the project name was detected.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectNameSource {
    CargoToml,
    PackageJson,
    PyprojectToml,
    DirectoryName,
}

impl std::fmt::Display for ProjectNameSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CargoToml => write!(f, "Cargo.toml"),
            Self::PackageJson => write!(f, "package.json"),
            Self::PyprojectToml => write!(f, "pyproject.toml"),
            Self::DirectoryName => write!(f, "directory name"),
        }
    }
}

/// Detected user name and its source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedUserName {
    pub name: String,
    pub source: UserNameSource,
}

/// How the user name was detected.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum UserNameSource {
    GitConfig,
    Hostname,
    EnvUser,
}

impl EnvironmentScan {
    /// Scan the given directory and collect environment facts.
    ///
    /// This is the silent phase — no I/O to the terminal, no prompts.
    /// The scan result drives all subsequent interactive prompts.
    pub fn scan(root: &Path) -> Self {
        let path = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());

        // Git detection
        let (git_detected, git_branch, git_head_short, git_head_full, git_dirty, git_dirty_count) =
            scan_git(root);

        // Writ state
        let writ_dir = root.join(".writ");
        let writ_already_initialized = writ_dir.exists();
        let writ_has_legacy_settings = writ_dir.join("settings.json").exists();
        let writ_has_toml_config = writ_dir.join("config.toml").exists();

        // Agent framework file presence (individual checks for display)
        let claude_md_exists = root.join("CLAUDE.md").exists();
        let claude_dir_exists = root.join(".claude").is_dir();
        let agents_md_exists = root.join("AGENTS.md").exists();
        let codex_dir_exists = root.join(".codex").is_dir();

        // Full framework detection (reuse existing hooks module)
        let frameworks = detect_frameworks(root);

        // Global config
        let global_config_path = global_writ_config_path();
        let global_config_exists = global_config_path
            .as_ref()
            .map(|p| p.exists())
            .unwrap_or(false);

        // Project name detection
        let (project_name, project_name_source) = detect_project_name(root);

        EnvironmentScan {
            path,
            git_detected,
            git_branch,
            git_head_short,
            git_head_full,
            git_dirty,
            git_dirty_count,
            writ_already_initialized,
            writ_has_legacy_settings,
            writ_has_toml_config,
            claude_md_exists,
            claude_dir_exists,
            agents_md_exists,
            codex_dir_exists,
            frameworks,
            global_config_exists,
            global_config_path,
            project_name,
            project_name_source,
        }
    }

    /// Returns true if Claude Code framework is detected.
    pub fn claude_detected(&self) -> bool {
        self.frameworks
            .iter()
            .any(|f| f.framework == Framework::ClaudeCode && f.detected)
    }

    /// Returns true if Codex framework is detected.
    pub fn codex_detected(&self) -> bool {
        self.frameworks
            .iter()
            .any(|f| f.framework == Framework::Codex && f.detected)
    }

    /// Returns a display string for git state, e.g. "main @ a3f2b1c".
    pub fn git_display(&self) -> Option<String> {
        if !self.git_detected {
            return None;
        }
        let branch = self.git_branch.as_deref().unwrap_or("(detached)");
        let head = self.git_head_short.as_deref().unwrap_or("unknown");
        Some(format!("{} @ {}", branch, head))
    }
}

/// Detect the user's name from git config, hostname, or $USER.
pub fn detect_user_name() -> DetectedUserName {
    // Try git config first
    if let Some(name) = detect_git_user_name() {
        return DetectedUserName {
            name,
            source: UserNameSource::GitConfig,
        };
    }

    // Try $USER environment variable
    if let Ok(user) = std::env::var("USER") {
        if !user.is_empty() {
            return DetectedUserName {
                name: user,
                source: UserNameSource::EnvUser,
            };
        }
    }

    // Fall back to hostname
    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string());

    DetectedUserName {
        name: hostname,
        source: UserNameSource::Hostname,
    }
}

/// Resolve the path to `~/.writ/config`.
fn global_writ_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".writ").join("config"))
}

/// Scan git state without modifying anything.
#[cfg(feature = "bridge")]
fn scan_git(
    root: &Path,
) -> (
    bool,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
    usize,
) {
    let git_repo = match git2::Repository::discover(root) {
        Ok(repo) => repo,
        Err(_) => return (false, None, None, None, false, 0),
    };

    let branch = git_repo.head().ok().and_then(|h| {
        if h.is_branch() {
            h.shorthand().map(String::from)
        } else {
            None
        }
    });

    let (head_short, head_full) = match git_repo.head().ok().and_then(|h| h.target()) {
        Some(oid) => {
            let full = oid.to_string();
            let short = full[..12.min(full.len())].to_string();
            (Some(short), Some(full))
        }
        None => (None, None),
    };

    let (dirty, dirty_count) = match git_repo.statuses(None) {
        Ok(statuses) => {
            let count = statuses
                .iter()
                .filter(|s| {
                    let st = s.status();
                    st != git2::Status::CURRENT && st != git2::Status::IGNORED
                })
                .count();
            (count > 0, count)
        }
        Err(_) => (false, 0),
    };

    (true, branch, head_short, head_full, dirty, dirty_count)
}

/// Non-bridge fallback: no git detection available.
#[cfg(not(feature = "bridge"))]
fn scan_git(
    _root: &Path,
) -> (
    bool,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
    usize,
) {
    (false, None, None, None, false, 0)
}

/// Try to read `user.name` from git global config.
fn detect_git_user_name() -> Option<String> {
    #[cfg(feature = "bridge")]
    {
        let config = git2::Config::open_default().ok()?;
        config.get_string("user.name").ok()
    }
    #[cfg(not(feature = "bridge"))]
    {
        None
    }
}

/// Detect project name from manifest files or directory name.
fn detect_project_name(root: &Path) -> (String, ProjectNameSource) {
    // Try Cargo.toml
    if let Some(name) = read_cargo_project_name(root) {
        return (name, ProjectNameSource::CargoToml);
    }

    // Try package.json
    if let Some(name) = read_package_json_name(root) {
        return (name, ProjectNameSource::PackageJson);
    }

    // Try pyproject.toml
    if let Some(name) = read_pyproject_name(root) {
        return (name, ProjectNameSource::PyprojectToml);
    }

    // Fall back to directory name
    let dir_name = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_string();

    (dir_name, ProjectNameSource::DirectoryName)
}

/// Read project name from Cargo.toml [package] name.
fn read_cargo_project_name(root: &Path) -> Option<String> {
    let content = std::fs::read_to_string(root.join("Cargo.toml")).ok()?;
    // Simple line-based parsing — avoid pulling in a TOML crate here.
    // Look for `name = "..."` in the [package] section.
    let mut in_package = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "[package]" {
            in_package = true;
            continue;
        }
        if trimmed.starts_with('[') {
            in_package = false;
            continue;
        }
        if in_package && trimmed.starts_with("name") {
            if let Some(value) = extract_toml_string_value(trimmed) {
                return Some(value);
            }
        }
    }
    None
}

/// Read project name from package.json "name" field.
fn read_package_json_name(root: &Path) -> Option<String> {
    let content = std::fs::read_to_string(root.join("package.json")).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&content).ok()?;
    parsed
        .get("name")
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// Read project name from pyproject.toml [project] name.
fn read_pyproject_name(root: &Path) -> Option<String> {
    let content = std::fs::read_to_string(root.join("pyproject.toml")).ok()?;
    let mut in_project = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "[project]" {
            in_project = true;
            continue;
        }
        if trimmed.starts_with('[') {
            in_project = false;
            continue;
        }
        if in_project && trimmed.starts_with("name") {
            if let Some(value) = extract_toml_string_value(trimmed) {
                return Some(value);
            }
        }
    }
    None
}

/// Extract a quoted string value from a TOML `key = "value"` line.
fn extract_toml_string_value(line: &str) -> Option<String> {
    let parts: Vec<&str> = line.splitn(2, '=').collect();
    if parts.len() != 2 {
        return None;
    }
    let value = parts[1].trim();
    if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
        Some(value[1..value.len() - 1].to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_scan_empty_directory() {
        let dir = tempdir().unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        assert!(!scan.writ_already_initialized);
        assert!(!scan.claude_md_exists);
        assert!(!scan.claude_dir_exists);
        assert!(!scan.agents_md_exists);
        assert!(!scan.codex_dir_exists);
        assert!(!scan.writ_has_legacy_settings);
        assert!(!scan.writ_has_toml_config);
    }

    #[test]
    fn test_scan_detects_claude_md() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# Instructions").unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        assert!(scan.claude_md_exists);
        assert!(scan.claude_detected());
    }

    #[test]
    fn test_scan_detects_claude_dir() {
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".claude")).unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        assert!(scan.claude_dir_exists);
        assert!(scan.claude_detected());
    }

    #[test]
    fn test_scan_detects_agents_md() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "# Agents config").unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        assert!(scan.agents_md_exists);
        assert!(scan.codex_detected());
    }

    #[test]
    fn test_scan_detects_codex_dir() {
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".codex")).unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        assert!(scan.codex_dir_exists);
        assert!(scan.codex_detected());
    }

    #[test]
    fn test_scan_detects_existing_writ() {
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".writ")).unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        assert!(scan.writ_already_initialized);
    }

    #[test]
    fn test_scan_detects_legacy_settings() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path().join(".writ");
        std::fs::create_dir(&writ_dir).unwrap();
        std::fs::write(writ_dir.join("settings.json"), "{}").unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        assert!(scan.writ_already_initialized);
        assert!(scan.writ_has_legacy_settings);
        assert!(!scan.writ_has_toml_config);
    }

    #[test]
    fn test_scan_detects_toml_config() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path().join(".writ");
        std::fs::create_dir(&writ_dir).unwrap();
        std::fs::write(writ_dir.join("config.toml"), "[project]").unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        assert!(scan.writ_has_toml_config);
    }

    #[test]
    fn test_scan_project_name_from_directory() {
        let dir = tempdir().unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        // tempdir names are random, just verify we get something
        assert!(!scan.project_name.is_empty());
        // Without any manifest files, source should be directory name
        assert_eq!(scan.project_name_source, ProjectNameSource::DirectoryName);
    }

    #[test]
    fn test_scan_project_name_from_cargo_toml() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"my-awesome-project\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        assert_eq!(scan.project_name, "my-awesome-project");
        assert_eq!(scan.project_name_source, ProjectNameSource::CargoToml);
    }

    #[test]
    fn test_scan_project_name_from_package_json() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name": "my-node-app", "version": "1.0.0"}"#,
        )
        .unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        assert_eq!(scan.project_name, "my-node-app");
        assert_eq!(scan.project_name_source, ProjectNameSource::PackageJson);
    }

    #[test]
    fn test_scan_project_name_from_pyproject_toml() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"my-python-lib\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        assert_eq!(scan.project_name, "my-python-lib");
        assert_eq!(scan.project_name_source, ProjectNameSource::PyprojectToml);
    }

    #[test]
    fn test_scan_project_name_priority_cargo_over_package_json() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"rust-proj\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("package.json"), r#"{"name": "node-proj"}"#).unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        assert_eq!(scan.project_name, "rust-proj");
        assert_eq!(scan.project_name_source, ProjectNameSource::CargoToml);
    }

    #[test]
    fn test_git_display_no_git() {
        let dir = tempdir().unwrap();
        let scan = EnvironmentScan::scan(dir.path());
        // In most test environments there won't be a git repo in tempdir
        if !scan.git_detected {
            assert!(scan.git_display().is_none());
        }
    }

    #[test]
    fn test_scan_frameworks_vec_populated() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# Claude").unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "# Agents").unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        // Both frameworks should show up in the vec
        assert_eq!(scan.frameworks.len(), 2);
        assert!(scan.claude_detected());
        assert!(scan.codex_detected());
    }

    #[test]
    fn test_extract_toml_string_value() {
        assert_eq!(
            extract_toml_string_value(r#"name = "hello""#),
            Some("hello".to_string())
        );
        assert_eq!(extract_toml_string_value("name = 42"), None);
        assert_eq!(extract_toml_string_value("no-equals-sign"), None);
        assert_eq!(
            extract_toml_string_value(r#"name = "spaced value""#),
            Some("spaced value".to_string())
        );
    }

    #[test]
    fn test_detect_user_name_returns_something() {
        let detected = detect_user_name();
        assert!(!detected.name.is_empty());
    }

    #[test]
    fn test_global_config_path_resolves() {
        let path = global_writ_config_path();
        // Should resolve on any system with a home directory
        assert!(path.is_some());
        let p = path.unwrap();
        assert!(p.ends_with(".writ/config"));
    }

    // --- Bri: additional T.1 coverage ---

    #[test]
    fn test_scan_all_framework_files_present() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# Claude").unwrap();
        std::fs::create_dir(dir.path().join(".claude")).unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "# Agents").unwrap();
        std::fs::create_dir(dir.path().join(".codex")).unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        assert!(scan.claude_md_exists);
        assert!(scan.claude_dir_exists);
        assert!(scan.agents_md_exists);
        assert!(scan.codex_dir_exists);
        assert!(scan.claude_detected());
        assert!(scan.codex_detected());
    }

    #[test]
    fn test_scan_writ_initialized_but_no_legacy_no_toml() {
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".writ")).unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        assert!(scan.writ_already_initialized);
        assert!(!scan.writ_has_legacy_settings);
        assert!(!scan.writ_has_toml_config);
    }

    #[test]
    fn test_scan_both_legacy_and_toml_present() {
        let dir = tempdir().unwrap();
        let writ_dir = dir.path().join(".writ");
        std::fs::create_dir(&writ_dir).unwrap();
        std::fs::write(writ_dir.join("settings.json"), "{}").unwrap();
        std::fs::write(writ_dir.join("config.toml"), "[project]").unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        assert!(scan.writ_has_legacy_settings);
        assert!(scan.writ_has_toml_config);
    }

    #[test]
    fn test_scan_no_frameworks_detected_empty() {
        let dir = tempdir().unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        assert!(!scan.claude_detected());
        assert!(!scan.codex_detected());
    }

    #[test]
    fn test_scan_project_name_priority_package_json_over_pyproject() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), r#"{"name": "node-app"}"#).unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"python-app\"\n",
        )
        .unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        assert_eq!(scan.project_name, "node-app");
        assert_eq!(scan.project_name_source, ProjectNameSource::PackageJson);
    }

    #[test]
    fn test_scan_cargo_toml_without_package_section() {
        let dir = tempdir().unwrap();
        // Workspace Cargo.toml has no [package] section
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        // Should fall through to directory name
        assert_eq!(scan.project_name_source, ProjectNameSource::DirectoryName);
    }

    #[test]
    fn test_scan_malformed_package_json() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "not valid json {{{").unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        // Should fall through to directory name
        assert_eq!(scan.project_name_source, ProjectNameSource::DirectoryName);
    }

    #[test]
    fn test_scan_package_json_missing_name() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"version": "1.0.0", "private": true}"#,
        )
        .unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        // No "name" field → fall through
        assert_eq!(scan.project_name_source, ProjectNameSource::DirectoryName);
    }

    #[test]
    fn test_scan_path_is_set() {
        let dir = tempdir().unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        assert!(!scan.path.as_os_str().is_empty());
    }

    #[test]
    fn test_scan_serializes_to_json() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# test").unwrap();
        let scan = EnvironmentScan::scan(dir.path());

        let json = serde_json::to_string(&scan).unwrap();
        assert!(json.contains("claude_md_exists"));
        assert!(json.contains("true"));
    }

    #[test]
    fn test_project_name_source_display() {
        assert_eq!(format!("{}", ProjectNameSource::CargoToml), "Cargo.toml");
        assert_eq!(
            format!("{}", ProjectNameSource::PackageJson),
            "package.json"
        );
        assert_eq!(
            format!("{}", ProjectNameSource::PyprojectToml),
            "pyproject.toml"
        );
        assert_eq!(
            format!("{}", ProjectNameSource::DirectoryName),
            "directory name"
        );
    }

    #[test]
    fn test_extract_toml_string_empty_value() {
        assert_eq!(
            extract_toml_string_value(r#"name = """#),
            Some(String::new())
        );
    }

    #[test]
    fn test_extract_toml_string_with_spaces_around_equals() {
        assert_eq!(
            extract_toml_string_value(r#"name   =   "hello""#),
            Some("hello".to_string())
        );
    }

    #[test]
    fn test_user_name_source_serializes() {
        let detected = DetectedUserName {
            name: "test".to_string(),
            source: UserNameSource::GitConfig,
        };
        let json = serde_json::to_string(&detected).unwrap();
        assert!(json.contains("git-config"));
    }
}
