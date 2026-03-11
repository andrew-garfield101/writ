//! Workspace management — isolated parallel environments.
//!
//! Workspaces provide isolated parallel working environments within a single
//! writ-managed project. Each workspace has its own index, HEAD, and spec heads
//! but shares the object store, seals, and specs with all other workspaces.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{WritError, WritResult};

/// Information about a workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    /// Workspace name (lowercase, alphanumeric, hyphens).
    pub name: String,
    /// Working directory path for this workspace.
    pub path: PathBuf,
    /// Current HEAD seal ID, if any.
    pub head_seal: Option<String>,
    /// Number of specs assigned to this workspace.
    pub spec_count: usize,
    /// Whether this is the main (primary) workspace.
    pub is_main: bool,
}

/// Configuration stored in `.writ/workspaces/<name>/config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    /// Workspace name.
    pub name: String,
    /// Absolute path to the workspace's working directory.
    pub path: String,
    /// Seal ID of the ancestor state when this workspace was created.
    pub ancestor_seal: Option<String>,
    /// Name of the workspace this was created from.
    pub created_from: String,
}

/// Content of the `.writ-workspace` pointer file in parallel workspace directories.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspacePointer {
    /// Absolute path to the parent `.writ/` directory.
    pub parent: String,
    /// Workspace name.
    pub workspace: String,
}

/// Validate a workspace name: lowercase alphanumeric + hyphens, non-empty,
/// no leading/trailing hyphens, no consecutive hyphens.
pub fn validate_workspace_name(name: &str) -> WritResult<()> {
    if name.is_empty() {
        return Err(WritError::InvalidInput(
            "workspace name cannot be empty".to_string(),
        ));
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err(WritError::InvalidInput(format!(
            "workspace name '{}' cannot start or end with a hyphen",
            name
        )));
    }
    if name.contains("--") {
        return Err(WritError::InvalidInput(format!(
            "workspace name '{}' cannot contain consecutive hyphens",
            name
        )));
    }
    for ch in name.chars() {
        if !ch.is_ascii_lowercase() && !ch.is_ascii_digit() && ch != '-' {
            return Err(WritError::InvalidInput(format!(
                "workspace name '{}' contains invalid character '{}' \
                 (only lowercase letters, digits, and hyphens allowed)",
                name, ch
            )));
        }
    }
    Ok(())
}

/// Parse a `.writ-workspace` pointer file.
pub fn parse_workspace_pointer(path: &Path) -> WritResult<WorkspacePointer> {
    let content = std::fs::read_to_string(path).map_err(WritError::Io)?;
    toml::from_str(&content).map_err(|e| {
        WritError::Other(format!(
            "failed to parse .writ-workspace file at '{}': {}",
            path.display(),
            e
        ))
    })
}

/// Write a `.writ-workspace` pointer file.
pub fn write_workspace_pointer(path: &Path, pointer: &WorkspacePointer) -> WritResult<()> {
    let content = toml::to_string_pretty(pointer)
        .map_err(|e| WritError::Other(format!("failed to serialize workspace pointer: {}", e)))?;
    crate::fsutil::atomic_write(path, content.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_workspace_names() {
        assert!(validate_workspace_name("auth-team").is_ok());
        assert!(validate_workspace_name("payments").is_ok());
        assert!(validate_workspace_name("ui-1").is_ok());
        assert!(validate_workspace_name("a").is_ok());
        assert!(validate_workspace_name("team-alpha-2").is_ok());
    }

    #[test]
    fn test_empty_name_rejected() {
        assert!(validate_workspace_name("").is_err());
    }

    #[test]
    fn test_uppercase_rejected() {
        assert!(validate_workspace_name("Auth-Team").is_err());
        assert!(validate_workspace_name("PAYMENTS").is_err());
    }

    #[test]
    fn test_special_chars_rejected() {
        assert!(validate_workspace_name("auth_team").is_err());
        assert!(validate_workspace_name("auth team").is_err());
        assert!(validate_workspace_name("auth.team").is_err());
        assert!(validate_workspace_name("auth/team").is_err());
    }

    #[test]
    fn test_leading_trailing_hyphen_rejected() {
        assert!(validate_workspace_name("-auth").is_err());
        assert!(validate_workspace_name("auth-").is_err());
    }

    #[test]
    fn test_consecutive_hyphens_rejected() {
        assert!(validate_workspace_name("auth--team").is_err());
    }

    #[test]
    fn test_workspace_pointer_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let pointer_path = dir.path().join(".writ-workspace");

        let pointer = WorkspacePointer {
            parent: "/home/user/project/.writ".to_string(),
            workspace: "auth-team".to_string(),
        };

        write_workspace_pointer(&pointer_path, &pointer).unwrap();
        let loaded = parse_workspace_pointer(&pointer_path).unwrap();

        assert_eq!(loaded.parent, pointer.parent);
        assert_eq!(loaded.workspace, pointer.workspace);
    }

    #[test]
    fn test_workspace_config_serialization() {
        let config = WorkspaceConfig {
            name: "auth-team".to_string(),
            path: "/home/user/ws-auth".to_string(),
            ancestor_seal: Some("seal-abc123".to_string()),
            created_from: "main".to_string(),
        };
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let loaded: WorkspaceConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(loaded.name, "auth-team");
        assert_eq!(loaded.ancestor_seal, Some("seal-abc123".to_string()));
    }
}
