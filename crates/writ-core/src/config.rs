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

    /// Get the preferred output format, if set.
    pub fn output_format(&self) -> Option<&str> {
        self.output.as_ref()?.format.as_deref()
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

    #[test]
    fn test_global_config_partial_toml() {
        // Only user section set, others missing
        let toml_str = "[user]\nname = \"Bri\"\n";
        let config: GlobalConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.user.as_ref().unwrap().name.as_deref(), Some("Bri"));
        assert!(config.init.is_none());
        assert!(config.output.is_none());
    }
}
