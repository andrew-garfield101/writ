//! Persistent repository settings (`settings.json`).
//!
//! Provides user-configurable defaults for agent ID, output format,
//! convergence strategy, and scope enforcement. Loaded on
//! `Repository::open()`, overridable by CLI flags.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{WritError, WritResult};

// ---------------------------------------------------------------------------
// Settings types
// ---------------------------------------------------------------------------

/// Top-level repository settings stored at `.writ/settings.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WritSettings {
    /// Default agent ID for seals (overridden by `--agent`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_agent: Option<String>,

    /// Default output format: `"human"` or `"json"` (overridden by `--format`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_format: Option<String>,

    /// When true, seal() rejects out-of-scope files. When false (default),
    /// out-of-scope files produce warnings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforce_scope: Option<bool>,

    /// Convergence-related settings.
    #[serde(default, skip_serializing_if = "ConvergenceSettings::is_empty")]
    pub convergence: ConvergenceSettings,

    /// When true, agent integrations auto-seal on exit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_seal_on_exit: Option<bool>,
}

/// Convergence-specific settings.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConvergenceSettings {
    /// Default merge strategy: `"escalate"` (default), `"manual"`, or
    /// `"orchestrator"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,

    /// When true, auto-resolve all convergence escalations without prompting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_resolve: Option<bool>,

    /// Minimum confidence threshold for auto-resolve (0.0–1.0).
    /// Only applied when `auto_resolve` is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_resolve_min_confidence: Option<f64>,
}

impl ConvergenceSettings {
    /// Returns true when all fields are None (used for skip_serializing).
    pub fn is_empty(&self) -> bool {
        self.strategy.is_none()
            && self.auto_resolve.is_none()
            && self.auto_resolve_min_confidence.is_none()
    }
}

// ---------------------------------------------------------------------------
// Schema introspection
// ---------------------------------------------------------------------------

/// Metadata for a single settings key, used by `writ config list`.
pub struct SettingsKey {
    /// Dot-notation key name (e.g., `"convergence.strategy"`).
    pub key: &'static str,
    /// Type label: `"string"`, `"bool"`, or `"float"`.
    pub value_type: &'static str,
    /// Human-readable description.
    pub description: &'static str,
    /// Default value as a string.
    pub default_value: &'static str,
    /// Allowed values (None = any).
    pub allowed_values: Option<&'static [&'static str]>,
}

/// All known settings keys with their metadata.
pub static SETTINGS_KEYS: &[SettingsKey] = &[
    SettingsKey {
        key: "default_agent",
        value_type: "string",
        description: "Default agent ID for seals",
        default_value: "(not set)",
        allowed_values: None,
    },
    SettingsKey {
        key: "default_format",
        value_type: "string",
        description: "Default output format",
        default_value: "human",
        allowed_values: Some(&["human", "json"]),
    },
    SettingsKey {
        key: "enforce_scope",
        value_type: "bool",
        description: "Reject out-of-scope files on seal",
        default_value: "false",
        allowed_values: Some(&["true", "false"]),
    },
    SettingsKey {
        key: "convergence.strategy",
        value_type: "string",
        description: "Default convergence merge strategy",
        default_value: "escalate",
        allowed_values: Some(&["escalate", "manual", "orchestrator"]),
    },
    SettingsKey {
        key: "convergence.auto_resolve",
        value_type: "bool",
        description: "Auto-resolve convergence escalations",
        default_value: "false",
        allowed_values: Some(&["true", "false"]),
    },
    SettingsKey {
        key: "convergence.auto_resolve_min_confidence",
        value_type: "float",
        description: "Minimum confidence for auto-resolve (0.0-1.0)",
        default_value: "0.85",
        allowed_values: None,
    },
    SettingsKey {
        key: "auto_seal_on_exit",
        value_type: "bool",
        description: "Auto-seal on agent exit",
        default_value: "false",
        allowed_values: Some(&["true", "false"]),
    },
];

// ---------------------------------------------------------------------------
// Load / Save
// ---------------------------------------------------------------------------

impl WritSettings {
    /// Load settings from `.writ/settings.json`. Returns defaults if the file
    /// is missing or contains invalid JSON.
    pub fn load(writ_dir: &Path) -> WritResult<Self> {
        let path = writ_dir.join("settings.json");
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = match fs::read_to_string(&path) {
            Ok(d) => d,
            Err(_) => return Ok(Self::default()),
        };
        match serde_json::from_str(&data) {
            Ok(settings) => Ok(settings),
            Err(_) => Ok(Self::default()),
        }
    }

    /// Write settings to `.writ/settings.json` as pretty JSON.
    pub fn save(&self, writ_dir: &Path) -> WritResult<()> {
        let path = writ_dir.join("settings.json");
        let data = serde_json::to_string_pretty(self)?;
        fs::write(path, data)?;
        Ok(())
    }

    /// Get a setting value by dot-notation key.
    /// Returns `None` if the key is unset (even if known).
    /// Returns `None` for unknown keys — use `is_valid_key()` to distinguish.
    pub fn get(&self, key: &str) -> Option<String> {
        match key {
            "default_agent" => self.default_agent.clone(),
            "default_format" => self.default_format.clone(),
            "enforce_scope" => self.enforce_scope.map(|v| v.to_string()),
            "convergence.strategy" => self.convergence.strategy.clone(),
            "convergence.auto_resolve" => self.convergence.auto_resolve.map(|v| v.to_string()),
            "convergence.auto_resolve_min_confidence" => {
                self.convergence.auto_resolve_min_confidence.map(|v| v.to_string())
            }
            "auto_seal_on_exit" => self.auto_seal_on_exit.map(|v| v.to_string()),
            _ => None,
        }
    }

    /// Set a setting value by dot-notation key. Validates the value against
    /// the key's expected type and allowed values.
    pub fn set(&mut self, key: &str, value: &str) -> WritResult<()> {
        match key {
            "default_agent" => {
                self.default_agent = Some(value.to_string());
            }
            "default_format" => {
                if !["human", "json"].contains(&value) {
                    return Err(WritError::InvalidInput(format!(
                        "invalid format '{value}' (expected 'human' or 'json')"
                    )));
                }
                self.default_format = Some(value.to_string());
            }
            "enforce_scope" => {
                self.enforce_scope = Some(parse_bool(value)?);
            }
            "convergence.strategy" => {
                if !["escalate", "manual", "orchestrator"].contains(&value) {
                    return Err(WritError::InvalidInput(format!(
                        "invalid strategy '{value}' (expected 'escalate', 'manual', or 'orchestrator')"
                    )));
                }
                self.convergence.strategy = Some(value.to_string());
            }
            "convergence.auto_resolve" => {
                self.convergence.auto_resolve = Some(parse_bool(value)?);
            }
            "convergence.auto_resolve_min_confidence" => {
                let v: f64 = value.parse().map_err(|_| {
                    WritError::InvalidInput(format!("invalid float '{value}'"))
                })?;
                if !(0.0..=1.0).contains(&v) {
                    return Err(WritError::InvalidInput(
                        "confidence must be between 0.0 and 1.0".into(),
                    ));
                }
                self.convergence.auto_resolve_min_confidence = Some(v);
            }
            "auto_seal_on_exit" => {
                self.auto_seal_on_exit = Some(parse_bool(value)?);
            }
            _ => {
                return Err(WritError::InvalidInput(format!(
                    "unknown setting '{key}'"
                )));
            }
        }
        Ok(())
    }

    /// Remove a setting, reverting it to its default.
    pub fn unset(&mut self, key: &str) -> WritResult<()> {
        match key {
            "default_agent" => self.default_agent = None,
            "default_format" => self.default_format = None,
            "enforce_scope" => self.enforce_scope = None,
            "convergence.strategy" => self.convergence.strategy = None,
            "convergence.auto_resolve" => self.convergence.auto_resolve = None,
            "convergence.auto_resolve_min_confidence" => {
                self.convergence.auto_resolve_min_confidence = None;
            }
            "auto_seal_on_exit" => self.auto_seal_on_exit = None,
            _ => {
                return Err(WritError::InvalidInput(format!(
                    "unknown setting '{key}'"
                )));
            }
        }
        Ok(())
    }

    /// Check if a key name is valid.
    pub fn is_valid_key(key: &str) -> bool {
        SETTINGS_KEYS.iter().any(|k| k.key == key)
    }

    /// Return the schema for all known keys.
    pub fn keys() -> &'static [SettingsKey] {
        SETTINGS_KEYS
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_bool(value: &str) -> WritResult<bool> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(WritError::InvalidInput(format!(
            "invalid bool '{value}' (expected 'true' or 'false')"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_writ_dir() -> TempDir {
        TempDir::new().unwrap()
    }

    #[test]
    fn test_default_settings() {
        let s = WritSettings::default();
        assert!(s.default_agent.is_none());
        assert!(s.default_format.is_none());
        assert!(s.enforce_scope.is_none());
        assert!(s.convergence.strategy.is_none());
        assert!(s.convergence.auto_resolve.is_none());
        assert!(s.convergence.auto_resolve_min_confidence.is_none());
        assert!(s.auto_seal_on_exit.is_none());
    }

    #[test]
    fn test_load_missing_file() {
        let dir = make_writ_dir();
        let settings = WritSettings::load(dir.path()).unwrap();
        assert!(settings.default_agent.is_none());
        assert!(settings.convergence.strategy.is_none());
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let dir = make_writ_dir();
        let mut s = WritSettings::default();
        s.default_agent = Some("agent-1".into());
        s.default_format = Some("json".into());
        s.enforce_scope = Some(true);
        s.convergence.strategy = Some("escalate".into());
        s.convergence.auto_resolve = Some(true);
        s.convergence.auto_resolve_min_confidence = Some(0.70);
        s.auto_seal_on_exit = Some(false);

        s.save(dir.path()).unwrap();
        let loaded = WritSettings::load(dir.path()).unwrap();

        assert_eq!(loaded.default_agent.as_deref(), Some("agent-1"));
        assert_eq!(loaded.default_format.as_deref(), Some("json"));
        assert_eq!(loaded.enforce_scope, Some(true));
        assert_eq!(loaded.convergence.strategy.as_deref(), Some("escalate"));
        assert_eq!(loaded.convergence.auto_resolve, Some(true));
        assert!((loaded.convergence.auto_resolve_min_confidence.unwrap() - 0.70).abs() < f64::EPSILON);
        assert_eq!(loaded.auto_seal_on_exit, Some(false));
    }

    #[test]
    fn test_get_set_string_key() {
        let mut s = WritSettings::default();
        assert!(s.get("default_agent").is_none());

        s.set("default_agent", "my-bot").unwrap();
        assert_eq!(s.get("default_agent").as_deref(), Some("my-bot"));
    }

    #[test]
    fn test_get_set_bool_key() {
        let mut s = WritSettings::default();
        s.set("enforce_scope", "true").unwrap();
        assert_eq!(s.get("enforce_scope").as_deref(), Some("true"));
        assert_eq!(s.enforce_scope, Some(true));
    }

    #[test]
    fn test_get_set_float_key() {
        let mut s = WritSettings::default();
        s.set("convergence.auto_resolve_min_confidence", "0.70").unwrap();
        assert_eq!(
            s.get("convergence.auto_resolve_min_confidence").as_deref(),
            Some("0.7")
        );
    }

    #[test]
    fn test_get_unset_key_returns_none() {
        let s = WritSettings::default();
        assert!(s.get("default_format").is_none());
    }

    #[test]
    fn test_set_invalid_format() {
        let mut s = WritSettings::default();
        let err = s.set("default_format", "xml").unwrap_err();
        assert!(err.to_string().contains("invalid format"));
    }

    #[test]
    fn test_set_invalid_strategy() {
        let mut s = WritSettings::default();
        let err = s.set("convergence.strategy", "random").unwrap_err();
        assert!(err.to_string().contains("invalid strategy"));
    }

    #[test]
    fn test_set_confidence_out_of_range() {
        let mut s = WritSettings::default();
        let err = s.set("convergence.auto_resolve_min_confidence", "1.5").unwrap_err();
        assert!(err.to_string().contains("between 0.0 and 1.0"));

        let err = s.set("convergence.auto_resolve_min_confidence", "-0.1").unwrap_err();
        assert!(err.to_string().contains("between 0.0 and 1.0"));
    }

    #[test]
    fn test_set_invalid_bool() {
        let mut s = WritSettings::default();
        let err = s.set("enforce_scope", "yes").unwrap_err();
        assert!(err.to_string().contains("invalid bool"));
    }

    #[test]
    fn test_unset_resets_to_none() {
        let mut s = WritSettings::default();
        s.set("default_agent", "bot").unwrap();
        assert!(s.get("default_agent").is_some());

        s.unset("default_agent").unwrap();
        assert!(s.get("default_agent").is_none());
    }

    #[test]
    fn test_unknown_key_error() {
        let mut s = WritSettings::default();

        let err = s.set("nonexistent", "val").unwrap_err();
        assert!(err.to_string().contains("unknown setting"));

        let err = s.unset("nonexistent").unwrap_err();
        assert!(err.to_string().contains("unknown setting"));
    }

    #[test]
    fn test_is_valid_key() {
        assert!(WritSettings::is_valid_key("default_agent"));
        assert!(WritSettings::is_valid_key("convergence.auto_resolve"));
        assert!(!WritSettings::is_valid_key("nonexistent"));
    }

    #[test]
    fn test_backward_compat_new_field() {
        // JSON without convergence or auto_seal_on_exit fields still parses.
        let json = r#"{"default_agent": "old-agent"}"#;
        let s: WritSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.default_agent.as_deref(), Some("old-agent"));
        assert!(s.convergence.strategy.is_none());
        assert!(s.auto_seal_on_exit.is_none());
    }

    #[test]
    fn test_keys_schema_completeness() {
        // Every key in the schema should be recognized by get/set/unset.
        let mut s = WritSettings::default();
        for key_info in WritSettings::keys() {
            // get should not panic
            let _ = s.get(key_info.key);

            // set with a valid value should work
            let test_val = match key_info.value_type {
                "string" => key_info.allowed_values.map_or("test", |v| v[0]),
                "bool" => "true",
                "float" => "0.5",
                _ => "test",
            };
            s.set(key_info.key, test_val).unwrap();

            // unset should work
            s.unset(key_info.key).unwrap();
        }
    }

    #[test]
    fn test_corrupt_settings_json_falls_back_to_defaults() {
        let dir = make_writ_dir();
        // Write invalid JSON
        std::fs::write(dir.path().join("settings.json"), "not valid json {{{").unwrap();
        let settings = WritSettings::load(dir.path()).unwrap();
        // Should fall back to defaults, not error
        assert!(settings.default_agent.is_none());
        assert!(settings.convergence.strategy.is_none());
    }

    #[test]
    fn test_empty_convergence_not_serialized() {
        // When convergence has no settings, it should not appear in JSON.
        let s = WritSettings::default();
        let json = serde_json::to_string_pretty(&s).unwrap();
        assert!(
            !json.contains("convergence"),
            "empty convergence should not be serialized: {json}"
        );

        // But when convergence has a value, it should appear.
        let mut s2 = WritSettings::default();
        s2.convergence.auto_resolve = Some(true);
        let json2 = serde_json::to_string_pretty(&s2).unwrap();
        assert!(json2.contains("convergence"), "non-empty convergence should be serialized");
        assert!(json2.contains("auto_resolve"));
    }

    #[test]
    fn test_strategy_validates_v2_names() {
        let mut s = WritSettings::default();
        // Valid v2 strategies
        s.set("convergence.strategy", "escalate").unwrap();
        s.set("convergence.strategy", "manual").unwrap();
        s.set("convergence.strategy", "orchestrator").unwrap();

        // Old v1 names should be rejected
        let err = s.set("convergence.strategy", "three-way-merge").unwrap_err();
        assert!(err.to_string().contains("invalid strategy"));
        let err = s.set("convergence.strategy", "most-complete").unwrap_err();
        assert!(err.to_string().contains("invalid strategy"));
    }
}
