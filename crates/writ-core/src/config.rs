//! TOML-based config system — global (`~/.writ/config`) and project (`.writ/config.toml`).
//!
//! Replaces `settings.json` with a TOML-based config hierarchy:
//! - Global config: `~/.writ/config` — user preferences across all projects
//! - Project config: `.writ/config.toml` — per-project settings
//!
//! Resolution chain: CLI flag > project config > global config > default.
//! Migration: if `.writ/settings.json` exists, values are merged on first load.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{WritError, WritResult};

// ---------------------------------------------------------------------------
// Global config (~/.writ/config)
// ---------------------------------------------------------------------------

/// Global config stored at `~/.writ/config`. Contains user-level defaults
/// that apply across all writ projects unless overridden by project config.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GlobalConfig {
    /// User identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<UserConfig>,

    /// Default init preferences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init: Option<InitDefaults>,

    /// Output format preferences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputConfig>,

    /// Workflow settings (commit mode, strategy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<WorkflowConfig>,

    /// Watch daemon defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<WatchConfig>,
}

/// User identity section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserConfig {
    /// User's display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Default init preferences section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitDefaults {
    /// Default frameworks to enable on init.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<InitDefaultValues>,
}

/// Init default values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitDefaultValues {
    /// Which agent frameworks to enable by default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frameworks: Vec<String>,
}

/// Output format preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    /// Default output format: "json", "json-compact", or "toon".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

// ---------------------------------------------------------------------------
// Project config (.writ/config.toml)
// ---------------------------------------------------------------------------

/// Project-level config stored at `.writ/config.toml`. Contains settings
/// specific to this writ repository.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectConfig {
    /// Project metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectMeta>,

    /// Git integration settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitConfig>,

    /// Agent framework configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frameworks: Option<FrameworksConfig>,

    /// Output format preferences (overrides global).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputConfig>,

    /// Security settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<SecurityConfig>,

    /// Workflow settings (commit mode, strategy, stale timeout).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<WorkflowConfig>,

    /// Auto-mode configuration (project-level only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto: Option<AutoModeConfig>,

    /// Workspace settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceSettings>,

    /// Watch daemon configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<WatchConfig>,
}

/// Project metadata section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMeta {
    /// Project name (usually directory name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// When this project was initialized with writ.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initialized: Option<DateTime<Utc>>,
}

/// Git integration settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitConfig {
    /// Whether git integration is enabled.
    #[serde(default)]
    pub enabled: bool,

    /// Git commit hash used as the baseline for writ tracking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_ref: Option<String>,
}

/// Agent framework toggles.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FrameworksConfig {
    /// Claude Code / Anthropic integration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<bool>,

    /// OpenAI Codex integration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<bool>,

    /// Generic agent instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generic: Option<bool>,

    /// Additional frameworks (extensible).
    #[serde(flatten, default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, bool>,
}

/// Security settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// When true, seal() rejects out-of-scope files. When false, warnings only.
    #[serde(default)]
    pub scope_enforcement: bool,
}

// ---------------------------------------------------------------------------
// Workflow config
// ---------------------------------------------------------------------------

/// Workflow settings — how completed specs become git commits.
///
/// Configurable at both global and project level.
/// Valid `commit_mode` values: `"user"`, `"propose"`, `"auto"`.
/// Valid `commit_strategy` values: `"single"`, `"per-spec"`, `"grouped"`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkflowConfig {
    /// Commit mode: "user" (manual), "propose" (orchestrator proposes), "auto" (autonomous).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_mode: Option<String>,

    /// Default commit strategy: "single", "per-spec", or "grouped".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_strategy: Option<String>,

    /// Stale spec timeout in seconds. Specs with no seal activity past this
    /// threshold are flagged as stale. 0 disables stale detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_timeout: Option<u64>,
}

/// Auto-mode configuration — safety rails for fully autonomous commits.
///
/// Only valid at the project level (not global).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AutoModeConfig {
    /// Shell command that must exit 0 before auto-commit proceeds.
    /// Leave empty or None to skip verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_command: Option<String>,

    /// Maximum number of specs per commit in auto mode. Overflow batches
    /// into additional commits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_specs_per_commit: Option<u32>,

    /// Target branch for auto commits (e.g. "writ/auto"). Strongly
    /// recommended to NOT be main/master.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,

    /// Notification method: "log" (writ events), "stdout", or "none".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify: Option<String>,

    /// Optional webhook URL for external notification on auto-commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Watch config
// ---------------------------------------------------------------------------

/// Watch daemon configuration — controls `writ watch` polling behavior.
///
/// ```toml
/// [watch]
/// interval = 5            # seconds between polls (default: 5, min: 1)
/// auto_converge = true     # auto-converge overlapping seals (default: true)
/// auto_converge_on_seal = true  # seal() triggers convergence inline (default: true)
/// max_retries = 3          # max convergence retries (default: 3, min: 1)
/// log_file = ".writ/watch.log"  # log file path, relative to project root
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchConfig {
    /// Polling interval in seconds. Must be >= 1.
    #[serde(default = "default_watch_interval")]
    pub interval: u64,

    /// Whether to automatically converge overlapping seals via `writ watch`.
    #[serde(default = "default_true")]
    pub auto_converge: bool,

    /// Whether seal() automatically triggers convergence inline when overlaps
    /// are detected. Default: true. Set to false for benchmarking or when
    /// using `writ watch` exclusively for convergence.
    #[serde(default = "default_true")]
    pub auto_converge_on_seal: bool,

    /// Maximum number of convergence retries before giving up. Must be >= 1.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,

    /// Log file path, relative to project root.
    #[serde(default = "default_watch_log_file")]
    pub log_file: String,
}

fn default_watch_interval() -> u64 {
    5
}
fn default_true() -> bool {
    true
}
fn default_max_retries() -> u32 {
    3
}
fn default_watch_log_file() -> String {
    ".writ/watch.log".to_string()
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            interval: default_watch_interval(),
            auto_converge: default_true(),
            auto_converge_on_seal: default_true(),
            max_retries: default_max_retries(),
            log_file: default_watch_log_file(),
        }
    }
}

/// Validate a `WatchConfig`, returning an error if values are out of range.
pub fn validate_watch_config(config: &WatchConfig) -> WritResult<()> {
    if config.interval < 1 {
        return Err(WritError::Other(
            "watch interval must be >= 1 second".to_string(),
        ));
    }
    if config.max_retries < 1 {
        return Err(WritError::Other(
            "watch max_retries must be >= 1".to_string(),
        ));
    }
    if config.log_file.starts_with('/') {
        return Err(WritError::Other(
            "watch log_file must be a relative path (relative to project root)".to_string(),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Workspace settings
// ---------------------------------------------------------------------------

/// Workspace settings — where workspace directories are created.
///
/// ```toml
/// [workspace]
/// root = "workspaces"   # default, relative to project root
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceSettings {
    /// Root directory for workspace directories, relative to project root.
    /// Defaults to `"workspaces"` if not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}

/// Default workspace root directory.
pub const DEFAULT_WORKSPACE_ROOT: &str = "workspaces";

/// Valid commit modes.
const VALID_COMMIT_MODES: &[&str] = &["user", "propose", "auto"];

/// Valid commit strategies.
const VALID_COMMIT_STRATEGIES: &[&str] = &["single", "per-spec", "grouped"];

/// Default commit mode.
const DEFAULT_COMMIT_MODE: &str = "user";

/// Default commit strategy.
const DEFAULT_COMMIT_STRATEGY: &str = "single";

/// Default stale timeout in seconds (1 hour).
const DEFAULT_STALE_TIMEOUT: u64 = 3600;

/// Check whether a string is a valid commit mode.
pub fn is_valid_commit_mode(s: &str) -> bool {
    VALID_COMMIT_MODES.contains(&s)
}

/// Check whether a string is a valid commit strategy.
pub fn is_valid_commit_strategy(s: &str) -> bool {
    VALID_COMMIT_STRATEGIES.contains(&s)
}

// ---------------------------------------------------------------------------
// Global config load/save
// ---------------------------------------------------------------------------

impl GlobalConfig {
    /// Path to the global config directory: `~/.writ/`.
    pub fn global_dir() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".writ"))
    }

    /// Load global config from `~/.writ/config`. Returns defaults if missing.
    pub fn load() -> WritResult<Self> {
        let dir = Self::global_dir()
            .ok_or_else(|| WritError::Other("could not determine home directory".into()))?;
        let path = dir.join("config");
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = fs::read_to_string(&path)?;
        toml::from_str(&data)
            .map_err(|e| WritError::Other(format!("failed to parse global config: {e}")))
    }

    /// Save global config to `~/.writ/config`. Creates directory if needed.
    pub fn save(&self) -> WritResult<()> {
        let dir = Self::global_dir()
            .ok_or_else(|| WritError::Other("could not determine home directory".into()))?;
        fs::create_dir_all(&dir)?;
        let data = toml::to_string_pretty(self)
            .map_err(|e| WritError::Other(format!("failed to serialize global config: {e}")))?;
        fs::write(dir.join("config"), data)?;
        Ok(())
    }

    /// Get the preferred output format, if set.
    pub fn output_format(&self) -> Option<&str> {
        self.output.as_ref()?.format.as_deref()
    }

    /// Get the configured commit mode, if set.
    pub fn commit_mode(&self) -> Option<&str> {
        self.workflow.as_ref()?.commit_mode.as_deref()
    }

    /// Get the configured commit strategy, if set.
    pub fn commit_strategy(&self) -> Option<&str> {
        self.workflow.as_ref()?.commit_strategy.as_deref()
    }

    /// Get the configured stale timeout, if set.
    pub fn stale_timeout(&self) -> Option<u64> {
        self.workflow.as_ref()?.stale_timeout
    }

    /// Get the configured watch settings, if set.
    pub fn watch_config(&self) -> Option<&WatchConfig> {
        self.watch.as_ref()
    }
}

// ---------------------------------------------------------------------------
// Project config load/save
// ---------------------------------------------------------------------------

impl ProjectConfig {
    /// Load project config from `.writ/config.toml`.
    /// Falls back to migrating from `settings.json` if config.toml doesn't exist.
    pub fn load(writ_dir: &Path) -> WritResult<Self> {
        let config_path = writ_dir.join("config.toml");
        if config_path.exists() {
            let data = fs::read_to_string(&config_path)?;
            return toml::from_str(&data)
                .map_err(|e| WritError::Other(format!("failed to parse project config: {e}")));
        }

        // Migration: try settings.json
        let settings_path = writ_dir.join("settings.json");
        if settings_path.exists() {
            let config = Self::migrate_from_settings(writ_dir)?;
            eprintln!("notice: migrated settings.json to config.toml (settings.json preserved)");
            return Ok(config);
        }

        Ok(Self::default())
    }

    /// Save project config to `.writ/config.toml`.
    pub fn save(&self, writ_dir: &Path) -> WritResult<()> {
        let data = toml::to_string_pretty(self)
            .map_err(|e| WritError::Other(format!("failed to serialize project config: {e}")))?;
        fs::write(writ_dir.join("config.toml"), data)?;
        Ok(())
    }

    /// Get the project name, if set.
    pub fn project_name(&self) -> Option<&str> {
        self.project.as_ref()?.name.as_deref()
    }

    /// Get the preferred output format, if set.
    pub fn output_format(&self) -> Option<&str> {
        self.output.as_ref()?.format.as_deref()
    }

    /// Get the configured commit mode, if set.
    pub fn commit_mode(&self) -> Option<&str> {
        self.workflow.as_ref()?.commit_mode.as_deref()
    }

    /// Get the configured commit strategy, if set.
    pub fn commit_strategy(&self) -> Option<&str> {
        self.workflow.as_ref()?.commit_strategy.as_deref()
    }

    /// Get the configured stale timeout, if set.
    pub fn stale_timeout(&self) -> Option<u64> {
        self.workflow.as_ref()?.stale_timeout
    }

    /// Get the configured workspace root directory, defaulting to "workspaces".
    pub fn workspace_root(&self) -> &str {
        self.workspace
            .as_ref()
            .and_then(|ws| ws.root.as_deref())
            .unwrap_or(DEFAULT_WORKSPACE_ROOT)
    }

    /// Get the configured watch settings, if set.
    pub fn watch_config(&self) -> Option<&WatchConfig> {
        self.watch.as_ref()
    }

    /// Migrate values from `.writ/settings.json` into a new ProjectConfig.
    /// The old settings.json file is left in place (non-destructive).
    fn migrate_from_settings(writ_dir: &Path) -> WritResult<Self> {
        let settings_path = writ_dir.join("settings.json");
        let data = fs::read_to_string(&settings_path)?;
        let settings: crate::settings::WritSettings = match serde_json::from_str(&data) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("warning: settings.json is malformed ({e}), migrating with defaults");
                crate::settings::WritSettings::default()
            }
        };

        let mut config = ProjectConfig::default();

        // Migrate output format
        if let Some(ref fmt) = settings.default_format {
            config.output = Some(OutputConfig {
                format: Some(fmt.clone()),
            });
        }

        // Migrate scope enforcement
        if let Some(enforce) = settings.enforce_scope {
            config.security = Some(SecurityConfig {
                scope_enforcement: enforce,
            });
        }

        // Save the migrated config
        config.save(writ_dir)?;

        Ok(config)
    }
}

// ---------------------------------------------------------------------------
// Format resolution chain
// ---------------------------------------------------------------------------

/// Resolve the output format using the priority chain:
/// CLI flag > project config > global config > default.
pub fn resolve_output_format(
    cli_flag: Option<&str>,
    project: &ProjectConfig,
    global: &GlobalConfig,
    default: &str,
) -> String {
    if let Some(f) = cli_flag {
        return f.to_string();
    }
    if let Some(f) = project.output_format() {
        return f.to_string();
    }
    if let Some(f) = global.output_format() {
        return f.to_string();
    }
    default.to_string()
}

// ---------------------------------------------------------------------------
// Workflow resolution chain (W.19)
// ---------------------------------------------------------------------------

/// Resolve the commit mode using the priority chain:
/// CLI flag > project config > global config > default ("user").
///
/// Returns an error if the resolved value is not a valid mode.
pub fn resolve_commit_mode(
    cli_flag: Option<&str>,
    project: &ProjectConfig,
    global: &GlobalConfig,
) -> WritResult<String> {
    let resolved = cli_flag
        .map(|s| s.to_string())
        .or_else(|| project.commit_mode().map(|s| s.to_string()))
        .or_else(|| global.commit_mode().map(|s| s.to_string()))
        .unwrap_or_else(|| DEFAULT_COMMIT_MODE.to_string());

    if !is_valid_commit_mode(&resolved) {
        return Err(WritError::Other(format!(
            "invalid commit mode '{}' — expected one of: {}",
            resolved,
            VALID_COMMIT_MODES.join(", ")
        )));
    }
    Ok(resolved)
}

/// Resolve the commit strategy using the priority chain:
/// CLI flag > project config > global config > default ("single").
///
/// Returns an error if the resolved value is not a valid strategy.
pub fn resolve_commit_strategy(
    cli_flag: Option<&str>,
    project: &ProjectConfig,
    global: &GlobalConfig,
) -> WritResult<String> {
    let resolved = cli_flag
        .map(|s| s.to_string())
        .or_else(|| project.commit_strategy().map(|s| s.to_string()))
        .or_else(|| global.commit_strategy().map(|s| s.to_string()))
        .unwrap_or_else(|| DEFAULT_COMMIT_STRATEGY.to_string());

    if !is_valid_commit_strategy(&resolved) {
        return Err(WritError::Other(format!(
            "invalid commit strategy '{}' — expected one of: {}",
            resolved,
            VALID_COMMIT_STRATEGIES.join(", ")
        )));
    }
    Ok(resolved)
}

/// Resolve the stale spec timeout using the priority chain:
/// project config > global config > default (3600s).
///
/// No CLI flag for this — it's a config-only setting.
pub fn resolve_stale_timeout(project: &ProjectConfig, global: &GlobalConfig) -> u64 {
    project
        .stale_timeout()
        .or_else(|| global.stale_timeout())
        .unwrap_or(DEFAULT_STALE_TIMEOUT)
}

// ---------------------------------------------------------------------------
// Watch config resolution (MS.18)
// ---------------------------------------------------------------------------

/// Resolve the watch configuration using the priority chain:
/// project config > global config > defaults.
///
/// Field-level merging: each field is resolved independently so a project
/// config can override just `interval` while inheriting `auto_converge`
/// from global or defaults.
pub fn resolve_watch_config(
    project: &ProjectConfig,
    global: &GlobalConfig,
) -> WritResult<WatchConfig> {
    let proj = project.watch_config();
    let glob = global.watch_config();

    let resolved = WatchConfig {
        interval: proj
            .map(|w| w.interval)
            .or_else(|| glob.map(|w| w.interval))
            .unwrap_or_else(default_watch_interval),
        auto_converge: proj
            .map(|w| w.auto_converge)
            .or_else(|| glob.map(|w| w.auto_converge))
            .unwrap_or_else(default_true),
        auto_converge_on_seal: proj
            .map(|w| w.auto_converge_on_seal)
            .or_else(|| glob.map(|w| w.auto_converge_on_seal))
            .unwrap_or_else(default_true),
        max_retries: proj
            .map(|w| w.max_retries)
            .or_else(|| glob.map(|w| w.max_retries))
            .unwrap_or_else(default_max_retries),
        log_file: proj
            .map(|w| w.log_file.clone())
            .or_else(|| glob.map(|w| w.log_file.clone()))
            .unwrap_or_else(default_watch_log_file),
    };

    validate_watch_config(&resolved)?;
    Ok(resolved)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // -- GlobalConfig tests --

    #[test]
    fn test_global_config_default() {
        let config = GlobalConfig::default();
        assert!(config.user.is_none());
        assert!(config.init.is_none());
        assert!(config.output.is_none());
        assert!(config.output_format().is_none());
    }

    #[test]
    fn test_global_config_roundtrip_toml() {
        let config = GlobalConfig {
            user: Some(UserConfig {
                name: Some("Andrew".into()),
            }),
            init: Some(InitDefaults {
                defaults: Some(InitDefaultValues {
                    frameworks: vec!["claude".into(), "codex".into()],
                }),
            }),
            output: Some(OutputConfig {
                format: Some("toon".into()),
            }),
            workflow: None,
            watch: None,
        };
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: GlobalConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(
            parsed.user.as_ref().unwrap().name.as_deref(),
            Some("Andrew")
        );
        assert_eq!(parsed.output_format(), Some("toon"));
        let frameworks = &parsed
            .init
            .as_ref()
            .unwrap()
            .defaults
            .as_ref()
            .unwrap()
            .frameworks;
        assert_eq!(frameworks.len(), 2);
        assert!(frameworks.contains(&"claude".to_string()));
    }

    #[test]
    fn test_global_config_empty_toml_parses() {
        let config: GlobalConfig = toml::from_str("").unwrap();
        assert!(config.user.is_none());
    }

    // -- ProjectConfig tests --

    #[test]
    fn test_project_config_default() {
        let config = ProjectConfig::default();
        assert!(config.project.is_none());
        assert!(config.git.is_none());
        assert!(config.frameworks.is_none());
        assert!(config.output.is_none());
        assert!(config.security.is_none());
        assert!(config.output_format().is_none());
    }

    #[test]
    fn test_project_config_roundtrip_toml() {
        let config = ProjectConfig {
            project: Some(ProjectMeta {
                name: Some("my-project".into()),
                initialized: Some(Utc::now()),
            }),
            git: Some(GitConfig {
                enabled: true,
                baseline_ref: Some("a3f2b1c".into()),
            }),
            frameworks: Some(FrameworksConfig {
                claude: Some(true),
                codex: Some(true),
                generic: Some(true),
                extra: HashMap::new(),
            }),
            output: Some(OutputConfig {
                format: Some("toon".into()),
            }),
            security: Some(SecurityConfig {
                scope_enforcement: true,
            }),
            workflow: None,
            auto: None,
            workspace: None,
            watch: None,
        };
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: ProjectConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(
            parsed.project.as_ref().unwrap().name.as_deref(),
            Some("my-project")
        );
        assert_eq!(parsed.git.as_ref().unwrap().enabled, true);
        assert_eq!(
            parsed.git.as_ref().unwrap().baseline_ref.as_deref(),
            Some("a3f2b1c")
        );
        assert_eq!(parsed.frameworks.as_ref().unwrap().claude, Some(true));
        assert_eq!(parsed.output_format(), Some("toon"));
        assert_eq!(parsed.security.as_ref().unwrap().scope_enforcement, true);
    }

    #[test]
    fn test_project_config_save_and_load() {
        let dir = TempDir::new().unwrap();
        let config = ProjectConfig {
            output: Some(OutputConfig {
                format: Some("json-compact".into()),
            }),
            ..Default::default()
        };
        config.save(dir.path()).unwrap();

        let loaded = ProjectConfig::load(dir.path()).unwrap();
        assert_eq!(loaded.output_format(), Some("json-compact"));
    }

    #[test]
    fn test_project_config_load_missing() {
        let dir = TempDir::new().unwrap();
        let config = ProjectConfig::load(dir.path()).unwrap();
        assert!(config.project.is_none());
    }

    #[test]
    fn test_project_config_migrate_from_settings() {
        let dir = TempDir::new().unwrap();
        // Write a settings.json
        let settings_json = r#"{
            "default_format": "json",
            "enforce_scope": true
        }"#;
        fs::write(dir.path().join("settings.json"), settings_json).unwrap();

        let config = ProjectConfig::load(dir.path()).unwrap();
        assert_eq!(config.output_format(), Some("json"));
        assert_eq!(config.security.as_ref().unwrap().scope_enforcement, true);
        // settings.json should still exist (non-destructive)
        assert!(dir.path().join("settings.json").exists());
        // config.toml should now exist
        assert!(dir.path().join("config.toml").exists());
    }

    #[test]
    fn test_project_config_prefers_toml_over_settings_json() {
        let dir = TempDir::new().unwrap();
        // Write both files — config.toml should win
        let settings_json = r#"{"default_format": "json"}"#;
        fs::write(dir.path().join("settings.json"), settings_json).unwrap();

        let config_toml = r#"
[output]
format = "toon"
"#;
        fs::write(dir.path().join("config.toml"), config_toml).unwrap();

        let config = ProjectConfig::load(dir.path()).unwrap();
        assert_eq!(config.output_format(), Some("toon"));
    }

    #[test]
    fn test_frameworks_config_with_extras() {
        let toml_str = r#"
[frameworks]
claude = true
codex = false
generic = true
cursor = true
"#;
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        let fw = config.frameworks.unwrap();
        assert_eq!(fw.claude, Some(true));
        assert_eq!(fw.codex, Some(false));
        assert_eq!(fw.generic, Some(true));
        assert_eq!(fw.extra.get("cursor"), Some(&true));
    }

    // -- Resolution chain tests --

    #[test]
    fn test_resolve_cli_wins() {
        let project = ProjectConfig {
            output: Some(OutputConfig {
                format: Some("toon".into()),
            }),
            ..Default::default()
        };
        let global = GlobalConfig {
            output: Some(OutputConfig {
                format: Some("json-compact".into()),
            }),
            ..Default::default()
        };
        let result = resolve_output_format(Some("json"), &project, &global, "json");
        assert_eq!(result, "json");
    }

    #[test]
    fn test_resolve_project_over_global() {
        let project = ProjectConfig {
            output: Some(OutputConfig {
                format: Some("toon".into()),
            }),
            ..Default::default()
        };
        let global = GlobalConfig {
            output: Some(OutputConfig {
                format: Some("json-compact".into()),
            }),
            ..Default::default()
        };
        let result = resolve_output_format(None, &project, &global, "json");
        assert_eq!(result, "toon");
    }

    #[test]
    fn test_resolve_global_over_default() {
        let project = ProjectConfig::default();
        let global = GlobalConfig {
            output: Some(OutputConfig {
                format: Some("json-compact".into()),
            }),
            ..Default::default()
        };
        let result = resolve_output_format(None, &project, &global, "json");
        assert_eq!(result, "json-compact");
    }

    #[test]
    fn test_resolve_falls_to_default() {
        let project = ProjectConfig::default();
        let global = GlobalConfig::default();
        let result = resolve_output_format(None, &project, &global, "json");
        assert_eq!(result, "json");
    }

    // --- Bri: additional T.2 coverage ---

    #[test]
    fn test_global_config_output_format_accessor() {
        let config = GlobalConfig {
            output: Some(OutputConfig {
                format: Some("toon".into()),
            }),
            ..Default::default()
        };
        assert_eq!(config.output_format(), Some("toon"));

        let empty = GlobalConfig::default();
        assert_eq!(empty.output_format(), None);
    }

    #[test]
    fn test_project_config_output_format_accessor() {
        let config = ProjectConfig {
            output: Some(OutputConfig {
                format: Some("json-compact".into()),
            }),
            ..Default::default()
        };
        assert_eq!(config.output_format(), Some("json-compact"));

        let empty = ProjectConfig::default();
        assert_eq!(empty.output_format(), None);
    }

    #[test]
    fn test_project_config_load_corrupt_toml() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("config.toml"), "not valid toml {{{").unwrap();
        let result = ProjectConfig::load(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_migration_preserves_old_settings_json() {
        let dir = TempDir::new().unwrap();
        let settings = r#"{"default_format": "json", "enforce_scope": false}"#;
        fs::write(dir.path().join("settings.json"), settings).unwrap();

        let _config = ProjectConfig::load(dir.path()).unwrap();

        // Old file must survive
        assert!(dir.path().join("settings.json").exists());
        let preserved = fs::read_to_string(dir.path().join("settings.json")).unwrap();
        assert_eq!(preserved, settings);
    }

    #[test]
    fn test_migration_creates_config_toml() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("settings.json"),
            r#"{"default_format": "json"}"#,
        )
        .unwrap();

        let _config = ProjectConfig::load(dir.path()).unwrap();
        assert!(dir.path().join("config.toml").exists());
    }

    #[test]
    fn test_migration_maps_enforce_scope_to_security() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("settings.json"),
            r#"{"enforce_scope": true}"#,
        )
        .unwrap();

        let config = ProjectConfig::load(dir.path()).unwrap();
        assert_eq!(config.security.as_ref().unwrap().scope_enforcement, true);
    }

    #[test]
    fn test_migration_no_format_leaves_output_none() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("settings.json"), r#"{}"#).unwrap();

        let config = ProjectConfig::load(dir.path()).unwrap();
        assert!(config.output.is_none());
    }

    #[test]
    fn test_frameworks_config_only_set_fields_serialized() {
        let config = ProjectConfig {
            frameworks: Some(FrameworksConfig {
                claude: Some(true),
                codex: None,
                generic: None,
                extra: HashMap::new(),
            }),
            ..Default::default()
        };
        let toml_str = toml::to_string_pretty(&config).unwrap();
        assert!(toml_str.contains("claude = true"));
        // codex and generic should not appear since they're None
        assert!(!toml_str.contains("codex"));
        assert!(!toml_str.contains("generic"));
    }

    #[test]
    fn test_resolve_custom_default() {
        let project = ProjectConfig::default();
        let global = GlobalConfig::default();
        // Can pass any custom default
        let result = resolve_output_format(None, &project, &global, "toon");
        assert_eq!(result, "toon");
    }

    #[test]
    fn test_resolve_full_chain_cli_wins_everything() {
        let project = ProjectConfig {
            output: Some(OutputConfig {
                format: Some("json-compact".into()),
            }),
            ..Default::default()
        };
        let global = GlobalConfig {
            output: Some(OutputConfig {
                format: Some("toon".into()),
            }),
            ..Default::default()
        };
        let result = resolve_output_format(Some("json"), &project, &global, "toon");
        assert_eq!(result, "json");
    }

    #[test]
    fn test_project_config_git_section() {
        let config = ProjectConfig {
            git: Some(GitConfig {
                enabled: true,
                baseline_ref: Some("abc1234".into()),
            }),
            ..Default::default()
        };
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: ProjectConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.git.as_ref().unwrap().enabled, true);
        assert_eq!(
            parsed.git.as_ref().unwrap().baseline_ref.as_deref(),
            Some("abc1234")
        );
    }

    #[test]
    fn test_project_config_security_default_false() {
        let toml_str = "[security]\n";
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        // scope_enforcement defaults to false via #[serde(default)]
        assert_eq!(config.security.as_ref().unwrap().scope_enforcement, false);
    }

    // -- W.18: WorkflowConfig tests --

    #[test]
    fn test_workflow_config_defaults() {
        let wf = WorkflowConfig::default();
        assert!(wf.commit_mode.is_none());
        assert!(wf.commit_strategy.is_none());
        assert!(wf.stale_timeout.is_none());
    }

    #[test]
    fn test_workflow_config_roundtrip_toml() {
        let toml_str = r#"
[workflow]
commit_mode = "propose"
commit_strategy = "per-spec"
stale_timeout = 7200
"#;
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        let wf = config.workflow.as_ref().unwrap();
        assert_eq!(wf.commit_mode.as_deref(), Some("propose"));
        assert_eq!(wf.commit_strategy.as_deref(), Some("per-spec"));
        assert_eq!(wf.stale_timeout, Some(7200));

        // Re-serialize and parse again
        let serialized = toml::to_string_pretty(&config).unwrap();
        let reparsed: ProjectConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(reparsed.commit_mode(), Some("propose"));
        assert_eq!(reparsed.commit_strategy(), Some("per-spec"));
        assert_eq!(reparsed.stale_timeout(), Some(7200));
    }

    #[test]
    fn test_workflow_config_partial_fields() {
        // Only commit_mode set — others should be None
        let toml_str = r#"
[workflow]
commit_mode = "auto"
"#;
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.commit_mode(), Some("auto"));
        assert!(config.commit_strategy().is_none());
        assert!(config.stale_timeout().is_none());
    }

    #[test]
    fn test_workflow_config_stale_timeout_zero_disables() {
        let toml_str = r#"
[workflow]
stale_timeout = 0
"#;
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.stale_timeout(), Some(0));
    }

    #[test]
    fn test_auto_mode_config_roundtrip_toml() {
        let toml_str = r#"
[auto]
verify_command = "cargo test --quiet"
max_specs_per_commit = 10
branch = "writ/auto"
notify = "stdout"
webhook_url = "https://example.com/hook"
"#;
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        let auto = config.auto.as_ref().unwrap();
        assert_eq!(auto.verify_command.as_deref(), Some("cargo test --quiet"));
        assert_eq!(auto.max_specs_per_commit, Some(10));
        assert_eq!(auto.branch.as_deref(), Some("writ/auto"));
        assert_eq!(auto.notify.as_deref(), Some("stdout"));
        assert_eq!(
            auto.webhook_url.as_deref(),
            Some("https://example.com/hook")
        );

        let serialized = toml::to_string_pretty(&config).unwrap();
        let reparsed: ProjectConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(
            reparsed.auto.as_ref().unwrap().max_specs_per_commit,
            Some(10)
        );
    }

    #[test]
    fn test_auto_mode_config_defaults() {
        let auto = AutoModeConfig::default();
        assert!(auto.verify_command.is_none());
        assert!(auto.max_specs_per_commit.is_none());
        assert!(auto.branch.is_none());
        assert!(auto.notify.is_none());
        assert!(auto.webhook_url.is_none());
    }

    #[test]
    fn test_global_config_workflow_section() {
        let toml_str = r#"
[workflow]
commit_mode = "user"
commit_strategy = "single"
stale_timeout = 1800
"#;
        let config: GlobalConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.commit_mode(), Some("user"));
        assert_eq!(config.commit_strategy(), Some("single"));
        assert_eq!(config.stale_timeout(), Some(1800));
    }

    #[test]
    fn test_global_config_no_auto_section() {
        // GlobalConfig does not have auto — auto is project-level only
        let toml_str = r#"
[workflow]
commit_mode = "propose"
"#;
        let config: GlobalConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.commit_mode(), Some("propose"));
    }

    #[test]
    fn test_workflow_config_save_and_load() {
        let dir = TempDir::new().unwrap();
        let config = ProjectConfig {
            workflow: Some(WorkflowConfig {
                commit_mode: Some("auto".into()),
                commit_strategy: Some("grouped".into()),
                stale_timeout: Some(0),
            }),
            auto: Some(AutoModeConfig {
                verify_command: Some("make test".into()),
                max_specs_per_commit: Some(5),
                branch: Some("writ/ci".into()),
                notify: Some("log".into()),
                webhook_url: None,
            }),
            ..Default::default()
        };
        config.save(dir.path()).unwrap();

        let loaded = ProjectConfig::load(dir.path()).unwrap();
        assert_eq!(loaded.commit_mode(), Some("auto"));
        assert_eq!(loaded.commit_strategy(), Some("grouped"));
        assert_eq!(loaded.stale_timeout(), Some(0));
        let auto = loaded.auto.as_ref().unwrap();
        assert_eq!(auto.verify_command.as_deref(), Some("make test"));
        assert_eq!(auto.max_specs_per_commit, Some(5));
        assert_eq!(auto.branch.as_deref(), Some("writ/ci"));
    }

    // -- W.19: Resolution chain tests --

    #[test]
    fn test_resolve_commit_mode_cli_wins() {
        let project = ProjectConfig {
            workflow: Some(WorkflowConfig {
                commit_mode: Some("propose".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let global = GlobalConfig {
            workflow: Some(WorkflowConfig {
                commit_mode: Some("auto".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            resolve_commit_mode(Some("user"), &project, &global).unwrap(),
            "user"
        );
    }

    #[test]
    fn test_resolve_commit_mode_project_over_global() {
        let project = ProjectConfig {
            workflow: Some(WorkflowConfig {
                commit_mode: Some("propose".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let global = GlobalConfig {
            workflow: Some(WorkflowConfig {
                commit_mode: Some("auto".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            resolve_commit_mode(None, &project, &global).unwrap(),
            "propose"
        );
    }

    #[test]
    fn test_resolve_commit_mode_falls_to_default() {
        let project = ProjectConfig::default();
        let global = GlobalConfig::default();
        assert_eq!(
            resolve_commit_mode(None, &project, &global).unwrap(),
            "user"
        );
    }

    #[test]
    fn test_resolve_commit_mode_rejects_invalid() {
        let project = ProjectConfig::default();
        let global = GlobalConfig::default();
        let result = resolve_commit_mode(Some("yolo"), &project, &global);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid commit mode"));
        assert!(err.contains("yolo"));
    }

    #[test]
    fn test_resolve_commit_strategy_cli_wins() {
        let project = ProjectConfig {
            workflow: Some(WorkflowConfig {
                commit_strategy: Some("grouped".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let global = GlobalConfig::default();
        assert_eq!(
            resolve_commit_strategy(Some("per-spec"), &project, &global).unwrap(),
            "per-spec"
        );
    }

    #[test]
    fn test_resolve_commit_strategy_falls_to_default() {
        let project = ProjectConfig::default();
        let global = GlobalConfig::default();
        assert_eq!(
            resolve_commit_strategy(None, &project, &global).unwrap(),
            "single"
        );
    }

    #[test]
    fn test_resolve_commit_strategy_rejects_invalid() {
        let project = ProjectConfig {
            workflow: Some(WorkflowConfig {
                commit_strategy: Some("chaos".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let global = GlobalConfig::default();
        let result = resolve_commit_strategy(None, &project, &global);
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_stale_timeout_project_over_global() {
        let project = ProjectConfig {
            workflow: Some(WorkflowConfig {
                stale_timeout: Some(600),
                ..Default::default()
            }),
            ..Default::default()
        };
        let global = GlobalConfig {
            workflow: Some(WorkflowConfig {
                stale_timeout: Some(7200),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(resolve_stale_timeout(&project, &global), 600);
    }

    #[test]
    fn test_resolve_stale_timeout_falls_to_default() {
        let project = ProjectConfig::default();
        let global = GlobalConfig::default();
        assert_eq!(resolve_stale_timeout(&project, &global), 3600);
    }

    #[test]
    fn test_resolve_stale_timeout_zero_from_project() {
        let project = ProjectConfig {
            workflow: Some(WorkflowConfig {
                stale_timeout: Some(0),
                ..Default::default()
            }),
            ..Default::default()
        };
        let global = GlobalConfig::default();
        assert_eq!(resolve_stale_timeout(&project, &global), 0);
    }

    #[test]
    fn test_valid_commit_modes() {
        assert!(is_valid_commit_mode("user"));
        assert!(is_valid_commit_mode("propose"));
        assert!(is_valid_commit_mode("auto"));
        assert!(!is_valid_commit_mode("manual"));
        assert!(!is_valid_commit_mode(""));
    }

    #[test]
    fn test_valid_commit_strategies() {
        assert!(is_valid_commit_strategy("single"));
        assert!(is_valid_commit_strategy("per-spec"));
        assert!(is_valid_commit_strategy("grouped"));
        assert!(!is_valid_commit_strategy("atomic"));
        assert!(!is_valid_commit_strategy(""));
    }

    #[test]
    fn test_global_config_partial_toml() {
        // Only user section set, others missing
        let toml_str = "[user]\nname = \"Bri\"\n";
        let config: GlobalConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.user.as_ref().unwrap().name.as_deref(), Some("Bri"));
        assert!(config.init.is_none());
        assert!(config.output.is_none());
    }

    // -----------------------------------------------------------------------
    // WV.18: Workspace root configuration tests (Haris)
    // -----------------------------------------------------------------------

    #[test]
    fn test_workspace_root_default() {
        let config = ProjectConfig::default();
        assert_eq!(config.workspace_root(), "workspaces");
    }

    #[test]
    fn test_workspace_root_custom() {
        let config = ProjectConfig {
            workspace: Some(WorkspaceSettings {
                root: Some("agents".into()),
            }),
            ..Default::default()
        };
        assert_eq!(config.workspace_root(), "agents");
    }

    #[test]
    fn test_workspace_root_none_falls_to_default() {
        let config = ProjectConfig {
            workspace: Some(WorkspaceSettings { root: None }),
            ..Default::default()
        };
        assert_eq!(config.workspace_root(), "workspaces");
    }

    #[test]
    fn test_workspace_settings_toml_roundtrip() {
        let toml_str = "[workspace]\nroot = \"my-workspaces\"\n";
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.workspace_root(), "my-workspaces");

        let serialized = toml::to_string_pretty(&config).unwrap();
        let reparsed: ProjectConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(reparsed.workspace_root(), "my-workspaces");
    }

    #[test]
    fn test_workspace_settings_omitted_from_toml_when_none() {
        let config = ProjectConfig::default();
        let serialized = toml::to_string_pretty(&config).unwrap();
        assert!(
            !serialized.contains("workspace"),
            "workspace section should not appear when None"
        );
    }

    #[test]
    fn test_workspace_root_save_and_load() {
        let dir = TempDir::new().unwrap();
        let writ_dir = dir.path().join(".writ");
        std::fs::create_dir_all(&writ_dir).unwrap();

        let mut config = ProjectConfig::default();
        config.workspace = Some(WorkspaceSettings {
            root: Some("ws-root".into()),
        });
        config.save(&writ_dir).unwrap();

        let loaded = ProjectConfig::load(&writ_dir).unwrap();
        assert_eq!(loaded.workspace_root(), "ws-root");
    }

    // -----------------------------------------------------------------------
    // MS.18: Watch config tests (Haris)
    // -----------------------------------------------------------------------

    #[test]
    fn test_watch_config_defaults() {
        let wc = WatchConfig::default();
        assert_eq!(wc.interval, 5);
        assert!(wc.auto_converge);
        assert_eq!(wc.max_retries, 3);
        assert_eq!(wc.log_file, ".writ/watch.log");
    }

    #[test]
    fn test_watch_config_toml_roundtrip() {
        let toml_str = r#"
[watch]
interval = 10
auto_converge = false
max_retries = 5
log_file = ".writ/logs/watch.log"
"#;
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        let wc = config.watch_config().unwrap();
        assert_eq!(wc.interval, 10);
        assert!(!wc.auto_converge);
        assert_eq!(wc.max_retries, 5);
        assert_eq!(wc.log_file, ".writ/logs/watch.log");

        let serialized = toml::to_string_pretty(&config).unwrap();
        let reparsed: ProjectConfig = toml::from_str(&serialized).unwrap();
        let wc2 = reparsed.watch_config().unwrap();
        assert_eq!(wc2.interval, 10);
        assert!(!wc2.auto_converge);
    }

    #[test]
    fn test_watch_config_partial_fields_use_defaults() {
        // Only interval set — others should get defaults via serde
        let toml_str = r#"
[watch]
interval = 15
"#;
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        let wc = config.watch_config().unwrap();
        assert_eq!(wc.interval, 15);
        assert!(wc.auto_converge); // default true
        assert_eq!(wc.max_retries, 3); // default
        assert_eq!(wc.log_file, ".writ/watch.log"); // default
    }

    #[test]
    fn test_watch_config_omitted_from_toml_when_none() {
        let config = ProjectConfig::default();
        let serialized = toml::to_string_pretty(&config).unwrap();
        assert!(
            !serialized.contains("[watch]"),
            "watch section should not appear when None"
        );
    }

    #[test]
    fn test_watch_config_validation_interval_zero() {
        let wc = WatchConfig {
            interval: 0,
            ..Default::default()
        };
        let result = validate_watch_config(&wc);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("interval"));
    }

    #[test]
    fn test_watch_config_validation_max_retries_zero() {
        let wc = WatchConfig {
            max_retries: 0,
            ..Default::default()
        };
        let result = validate_watch_config(&wc);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("max_retries"));
    }

    #[test]
    fn test_watch_config_validation_absolute_log_path() {
        let wc = WatchConfig {
            log_file: "/var/log/watch.log".to_string(),
            ..Default::default()
        };
        let result = validate_watch_config(&wc);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("relative path"));
    }

    #[test]
    fn test_watch_config_validation_valid() {
        let wc = WatchConfig::default();
        assert!(validate_watch_config(&wc).is_ok());
    }

    #[test]
    fn test_watch_config_save_and_load() {
        let dir = TempDir::new().unwrap();
        let writ_dir = dir.path().join(".writ");
        std::fs::create_dir_all(&writ_dir).unwrap();

        let mut config = ProjectConfig::default();
        config.watch = Some(WatchConfig {
            interval: 10,
            auto_converge: false,
            auto_converge_on_seal: true,
            max_retries: 5,
            log_file: ".writ/custom.log".to_string(),
        });
        config.save(&writ_dir).unwrap();

        let loaded = ProjectConfig::load(&writ_dir).unwrap();
        let wc = loaded.watch_config().unwrap();
        assert_eq!(wc.interval, 10);
        assert!(!wc.auto_converge);
        assert_eq!(wc.max_retries, 5);
        assert_eq!(wc.log_file, ".writ/custom.log");
    }

    #[test]
    fn test_resolve_watch_config_defaults() {
        let project = ProjectConfig::default();
        let global = GlobalConfig::default();
        let resolved = resolve_watch_config(&project, &global).unwrap();
        assert_eq!(resolved.interval, 5);
        assert!(resolved.auto_converge);
        assert_eq!(resolved.max_retries, 3);
        assert_eq!(resolved.log_file, ".writ/watch.log");
    }

    #[test]
    fn test_resolve_watch_config_project_over_global() {
        let project = ProjectConfig {
            watch: Some(WatchConfig {
                interval: 10,
                ..Default::default()
            }),
            ..Default::default()
        };
        let global = GlobalConfig {
            watch: Some(WatchConfig {
                interval: 30,
                auto_converge: false,
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve_watch_config(&project, &global).unwrap();
        // Project interval wins
        assert_eq!(resolved.interval, 10);
        // Project auto_converge (default true) wins over global false
        assert!(resolved.auto_converge);
    }

    #[test]
    fn test_resolve_watch_config_global_over_default() {
        let project = ProjectConfig::default();
        let global = GlobalConfig {
            watch: Some(WatchConfig {
                interval: 20,
                auto_converge: false,
                auto_converge_on_seal: true,
                max_retries: 7,
                log_file: ".writ/global-watch.log".to_string(),
            }),
            ..Default::default()
        };
        let resolved = resolve_watch_config(&project, &global).unwrap();
        assert_eq!(resolved.interval, 20);
        assert!(!resolved.auto_converge);
        assert_eq!(resolved.max_retries, 7);
        assert_eq!(resolved.log_file, ".writ/global-watch.log");
    }

    #[test]
    fn test_resolve_watch_config_rejects_invalid() {
        let project = ProjectConfig {
            watch: Some(WatchConfig {
                interval: 0, // invalid
                ..Default::default()
            }),
            ..Default::default()
        };
        let global = GlobalConfig::default();
        let result = resolve_watch_config(&project, &global);
        assert!(result.is_err());
    }

    #[test]
    fn test_watch_config_in_global_config_toml() {
        let toml_str = r#"
[watch]
interval = 15
auto_converge = true
max_retries = 2
log_file = ".writ/watch.log"
"#;
        let config: GlobalConfig = toml::from_str(toml_str).unwrap();
        let wc = config.watch_config().unwrap();
        assert_eq!(wc.interval, 15);
        assert!(wc.auto_converge);
        assert_eq!(wc.max_retries, 2);
    }
}
