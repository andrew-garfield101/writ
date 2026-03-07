//! Schema versioning and migration for `.writ/` repositories.
//!
//! Each writ repo has a `.writ/version.toml` tracking the on-disk schema version.
//! When `Repository::open()` encounters an older schema, the migration runner
//! upgrades it automatically. Migrations are sequential and idempotent.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{WritError, WritResult};

/// Current schema version. Bump this when the `.writ/` on-disk layout changes.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Version metadata stored at `.writ/version.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoVersion {
    /// On-disk schema version (monotonically increasing integer).
    pub schema_version: u32,
    /// Binary version that first created this repository.
    pub created_by: String,
    /// Binary version that last opened this repository.
    pub last_opened_by: String,
    /// Timestamp when the repo was first created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    /// Timestamp of last open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_opened_at: Option<DateTime<Utc>>,
}

impl RepoVersion {
    /// Create a new version stamp for a freshly initialized repo.
    pub fn new() -> Self {
        let now = Utc::now();
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            created_by: env!("CARGO_PKG_VERSION").to_string(),
            last_opened_by: env!("CARGO_PKG_VERSION").to_string(),
            created_at: Some(now),
            last_opened_at: Some(now),
        }
    }

    /// Path to the version file inside `.writ/`.
    pub fn path(writ_dir: &Path) -> PathBuf {
        writ_dir.join("version.toml")
    }

    /// Load version metadata from `.writ/version.toml`.
    /// Returns `None` if the file doesn't exist (legacy/pre-versioning repo).
    pub fn load(writ_dir: &Path) -> WritResult<Option<Self>> {
        let path = Self::path(writ_dir);
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(&path)?;
        let version: Self = toml::from_str(&data)
            .map_err(|e| WritError::Other(format!("failed to parse version.toml: {e}")))?;
        Ok(Some(version))
    }

    /// Save version metadata to `.writ/version.toml`.
    pub fn save(&self, writ_dir: &Path) -> WritResult<()> {
        let data = toml::to_string_pretty(self)
            .map_err(|e| WritError::Other(format!("failed to serialize version.toml: {e}")))?;
        crate::fsutil::atomic_write(&Self::path(writ_dir), data.as_bytes())
    }
}

impl Default for RepoVersion {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Migration runner
// ---------------------------------------------------------------------------

/// Run all necessary migrations from `from_version` up to `to_version`.
///
/// Each step is idempotent — running a migration on an already-migrated repo
/// is a safe no-op. A backup of `version.toml` is created before starting.
pub fn migrate(writ_dir: &Path, from_version: u32, to_version: u32) -> WritResult<()> {
    if from_version >= to_version {
        return Ok(());
    }

    // Backup existing version file if present.
    let version_path = RepoVersion::path(writ_dir);
    if version_path.exists() {
        let backup = writ_dir.join("version.toml.bak");
        fs::copy(&version_path, &backup)?;
    }

    for step in (from_version + 1)..=to_version {
        eprintln!("writ: migrating .writ schema v{} → v{}", step - 1, step);
        match step {
            1 => migrate_v0_to_v1(writ_dir)?,
            _ => {
                return Err(WritError::Other(format!(
                    "unknown migration step: v{} → v{}",
                    step - 1,
                    step
                )));
            }
        }

        // Update version file after each successful step so partial
        // migrations leave the repo at the last good version.
        let mut version = RepoVersion::load(writ_dir)?
            .unwrap_or_else(RepoVersion::new);
        version.schema_version = step;
        version.last_opened_by = env!("CARGO_PKG_VERSION").to_string();
        version.last_opened_at = Some(Utc::now());
        version.save(writ_dir)?;
    }

    Ok(())
}

/// v0 → v1: Legacy repos created before schema versioning.
///
/// Creates the version file and any directories added since initial release.
fn migrate_v0_to_v1(writ_dir: &Path) -> WritResult<()> {
    // Ensure all expected directories exist (some were added post-launch).
    let dirs = [
        "objects", "seals", "specs", "heads", "keys", "agents",
        "proposals", "security", "security/events",
    ];
    for dir in &dirs {
        let p = writ_dir.join(dir);
        if !p.exists() {
            fs::create_dir_all(&p)?;
        }
    }

    // Create HEAD file if missing.
    let head = writ_dir.join("HEAD");
    if !head.exists() {
        fs::write(&head, "")?;
    }

    // If legacy settings.json exists but no config.toml, create a default config.toml.
    let config_toml = writ_dir.join("config.toml");
    let settings_json = writ_dir.join("settings.json");
    if settings_json.exists() && !config_toml.exists() {
        // Create minimal default config — the old settings.json is still loaded
        // by WritSettings for backward compat, this just ensures config.toml exists.
        let default_config = crate::config::ProjectConfig::default();
        default_config.save(writ_dir)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Doctor — repository health checks
// ---------------------------------------------------------------------------

/// Overall result of a doctor check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
    pub passed: usize,
    pub failed: usize,
    pub warnings: usize,
}

/// A single health check result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
}

/// Status of a single check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Pass,
    Fail,
    Warning,
}

impl DoctorReport {
    /// Run all health checks against the given `.writ/` directory.
    pub fn run(writ_dir: &Path) -> Self {
        let mut checks = Vec::new();

        // 1. Version file
        checks.push(check_version_file(writ_dir));

        // 2. Schema version currency
        checks.push(check_schema_current(writ_dir));

        // 3. Expected directories
        checks.push(check_directories(writ_dir));

        // 4. Index file
        checks.push(check_index(writ_dir));

        // 5. Config file
        checks.push(check_config(writ_dir));

        // 6. Master key
        checks.push(check_master_key(writ_dir));

        // 7. Spec files
        checks.push(check_specs(writ_dir));

        // 8. Seal files (sample)
        checks.push(check_seals(writ_dir));

        let passed = checks.iter().filter(|c| c.status == CheckStatus::Pass).count();
        let failed = checks.iter().filter(|c| c.status == CheckStatus::Fail).count();
        let warnings = checks.iter().filter(|c| c.status == CheckStatus::Warning).count();

        DoctorReport {
            checks,
            passed,
            failed,
            warnings,
        }
    }

    /// True if all checks passed (no failures).
    pub fn is_healthy(&self) -> bool {
        self.failed == 0
    }
}

// ---------------------------------------------------------------------------
// Individual checks
// ---------------------------------------------------------------------------

fn check_version_file(writ_dir: &Path) -> DoctorCheck {
    let path = RepoVersion::path(writ_dir);
    if !path.exists() {
        return DoctorCheck {
            name: "version_file".into(),
            status: CheckStatus::Warning,
            message: "missing .writ/version.toml (pre-versioning repo)".into(),
        };
    }
    match fs::read_to_string(&path).and_then(|data| {
        toml::from_str::<RepoVersion>(&data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }) {
        Ok(_) => DoctorCheck {
            name: "version_file".into(),
            status: CheckStatus::Pass,
            message: "version.toml exists and parses".into(),
        },
        Err(e) => DoctorCheck {
            name: "version_file".into(),
            status: CheckStatus::Fail,
            message: format!("version.toml is corrupt: {e}"),
        },
    }
}

fn check_schema_current(writ_dir: &Path) -> DoctorCheck {
    match RepoVersion::load(writ_dir) {
        Ok(Some(v)) if v.schema_version == CURRENT_SCHEMA_VERSION => DoctorCheck {
            name: "schema_version".into(),
            status: CheckStatus::Pass,
            message: format!("schema version {} is current", v.schema_version),
        },
        Ok(Some(v)) if v.schema_version > CURRENT_SCHEMA_VERSION => DoctorCheck {
            name: "schema_version".into(),
            status: CheckStatus::Fail,
            message: format!(
                "schema version {} is newer than this binary supports ({})",
                v.schema_version, CURRENT_SCHEMA_VERSION
            ),
        },
        Ok(Some(v)) => DoctorCheck {
            name: "schema_version".into(),
            status: CheckStatus::Warning,
            message: format!(
                "schema version {} is behind current ({}), migration needed",
                v.schema_version, CURRENT_SCHEMA_VERSION
            ),
        },
        Ok(None) => DoctorCheck {
            name: "schema_version".into(),
            status: CheckStatus::Warning,
            message: "no version file, cannot determine schema version".into(),
        },
        Err(e) => DoctorCheck {
            name: "schema_version".into(),
            status: CheckStatus::Fail,
            message: format!("failed to read version: {e}"),
        },
    }
}

fn check_directories(writ_dir: &Path) -> DoctorCheck {
    let expected = ["objects", "seals", "specs", "heads", "keys", "agents"];
    let mut missing = Vec::new();
    for dir in &expected {
        if !writ_dir.join(dir).is_dir() {
            missing.push(*dir);
        }
    }
    if missing.is_empty() {
        DoctorCheck {
            name: "directories".into(),
            status: CheckStatus::Pass,
            message: "all expected directories present".into(),
        }
    } else {
        DoctorCheck {
            name: "directories".into(),
            status: CheckStatus::Fail,
            message: format!("missing directories: {}", missing.join(", ")),
        }
    }
}

fn check_index(writ_dir: &Path) -> DoctorCheck {
    let path = writ_dir.join("index.json");
    if !path.exists() {
        return DoctorCheck {
            name: "index".into(),
            status: CheckStatus::Fail,
            message: "missing .writ/index.json".into(),
        };
    }
    match fs::read_to_string(&path)
        .map_err(WritError::from)
        .and_then(|data| {
            serde_json::from_str::<crate::index::Index>(&data).map_err(WritError::from)
        }) {
        Ok(_) => DoctorCheck {
            name: "index".into(),
            status: CheckStatus::Pass,
            message: "index.json exists and deserializes".into(),
        },
        Err(e) => DoctorCheck {
            name: "index".into(),
            status: CheckStatus::Fail,
            message: format!("index.json is corrupt: {e}"),
        },
    }
}

fn check_config(writ_dir: &Path) -> DoctorCheck {
    let path = writ_dir.join("config.toml");
    if !path.exists() {
        return DoctorCheck {
            name: "config".into(),
            status: CheckStatus::Pass,
            message: "no config.toml (using defaults)".into(),
        };
    }
    match crate::config::ProjectConfig::load(writ_dir) {
        Ok(_) => DoctorCheck {
            name: "config".into(),
            status: CheckStatus::Pass,
            message: "config.toml exists and parses".into(),
        },
        Err(e) => DoctorCheck {
            name: "config".into(),
            status: CheckStatus::Fail,
            message: format!("config.toml is corrupt: {e}"),
        },
    }
}

fn check_master_key(writ_dir: &Path) -> DoctorCheck {
    let key_path = writ_dir.join("keys").join(".master");
    if key_path.exists() {
        DoctorCheck {
            name: "master_key".into(),
            status: CheckStatus::Pass,
            message: "master key present".into(),
        }
    } else {
        DoctorCheck {
            name: "master_key".into(),
            status: CheckStatus::Fail,
            message: "missing keys/.master".into(),
        }
    }
}

fn check_specs(writ_dir: &Path) -> DoctorCheck {
    let specs_dir = writ_dir.join("specs");
    if !specs_dir.is_dir() {
        return DoctorCheck {
            name: "specs".into(),
            status: CheckStatus::Warning,
            message: "specs directory missing".into(),
        };
    }
    let mut total = 0u32;
    let mut bad = 0u32;
    let entries = match fs::read_dir(&specs_dir) {
        Ok(e) => e,
        Err(e) => {
            return DoctorCheck {
                name: "specs".into(),
                status: CheckStatus::Fail,
                message: format!("cannot read specs/: {e}"),
            };
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            total += 1;
            if let Ok(data) = fs::read_to_string(&path) {
                if serde_json::from_str::<crate::spec::Spec>(&data).is_err() {
                    bad += 1;
                }
            } else {
                bad += 1;
            }
        }
    }
    if bad == 0 {
        DoctorCheck {
            name: "specs".into(),
            status: CheckStatus::Pass,
            message: format!("{total} spec file(s) OK"),
        }
    } else {
        DoctorCheck {
            name: "specs".into(),
            status: CheckStatus::Fail,
            message: format!("{bad}/{total} spec file(s) failed to deserialize"),
        }
    }
}

fn check_seals(writ_dir: &Path) -> DoctorCheck {
    let seals_dir = writ_dir.join("seals");
    if !seals_dir.is_dir() {
        return DoctorCheck {
            name: "seals".into(),
            status: CheckStatus::Warning,
            message: "seals directory missing".into(),
        };
    }
    let entries: Vec<_> = match fs::read_dir(&seals_dir) {
        Ok(e) => e.flatten().collect(),
        Err(e) => {
            return DoctorCheck {
                name: "seals".into(),
                status: CheckStatus::Fail,
                message: format!("cannot read seals/: {e}"),
            };
        }
    };

    let sample_size = 50.min(entries.len());
    let mut checked = 0u32;
    let mut bad = 0u32;
    for entry in entries.iter().take(sample_size) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            checked += 1;
            if let Ok(data) = fs::read_to_string(&path) {
                if serde_json::from_str::<crate::seal::Seal>(&data).is_err() {
                    bad += 1;
                }
            } else {
                bad += 1;
            }
        }
    }
    if bad == 0 {
        DoctorCheck {
            name: "seals".into(),
            status: CheckStatus::Pass,
            message: format!("{checked} seal file(s) sampled, all OK"),
        }
    } else {
        DoctorCheck {
            name: "seals".into(),
            status: CheckStatus::Fail,
            message: format!("{bad}/{checked} sampled seal file(s) failed to deserialize"),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_minimal_writ_dir(tmp: &TempDir) -> PathBuf {
        let writ_dir = tmp.path().join(".writ");
        fs::create_dir_all(writ_dir.join("objects")).unwrap();
        fs::create_dir_all(writ_dir.join("seals")).unwrap();
        fs::create_dir_all(writ_dir.join("specs")).unwrap();
        fs::create_dir_all(writ_dir.join("heads")).unwrap();
        fs::create_dir_all(writ_dir.join("keys")).unwrap();
        fs::create_dir_all(writ_dir.join("agents")).unwrap();
        fs::write(writ_dir.join("HEAD"), "").unwrap();
        // Minimal index
        fs::write(writ_dir.join("index.json"), r#"{"entries":{}}"#).unwrap();
        // Master key placeholder (KeyStore stores at keys/.master)
        fs::write(writ_dir.join("keys").join(".master"), "fake-key").unwrap();
        writ_dir
    }

    #[test]
    fn test_repo_version_new_sets_current_schema() {
        let v = RepoVersion::new();
        assert_eq!(v.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(v.created_by, env!("CARGO_PKG_VERSION"));
        assert!(v.created_at.is_some());
    }

    #[test]
    fn test_repo_version_toml_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let writ_dir = tmp.path().join(".writ");
        fs::create_dir_all(&writ_dir).unwrap();

        let original = RepoVersion::new();
        original.save(&writ_dir).unwrap();

        let loaded = RepoVersion::load(&writ_dir).unwrap().unwrap();
        assert_eq!(loaded.schema_version, original.schema_version);
        assert_eq!(loaded.created_by, original.created_by);
    }

    #[test]
    fn test_load_missing_returns_none() {
        let tmp = TempDir::new().unwrap();
        let writ_dir = tmp.path().join(".writ");
        fs::create_dir_all(&writ_dir).unwrap();

        let result = RepoVersion::load(&writ_dir).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_migrate_v0_to_v1_creates_dirs_and_version() {
        let tmp = TempDir::new().unwrap();
        let writ_dir = tmp.path().join(".writ");
        fs::create_dir_all(&writ_dir).unwrap();
        // Only create the bare minimum (simulating a legacy repo)
        fs::create_dir_all(writ_dir.join("objects")).unwrap();
        fs::create_dir_all(writ_dir.join("seals")).unwrap();
        fs::create_dir_all(writ_dir.join("specs")).unwrap();

        migrate(&writ_dir, 0, 1).unwrap();

        // Directories created
        assert!(writ_dir.join("proposals").is_dir());
        assert!(writ_dir.join("security").is_dir());
        assert!(writ_dir.join("security/events").is_dir());
        assert!(writ_dir.join("heads").is_dir());
        assert!(writ_dir.join("keys").is_dir());
        assert!(writ_dir.join("agents").is_dir());
        assert!(writ_dir.join("HEAD").exists());

        // Version file written
        let v = RepoVersion::load(&writ_dir).unwrap().unwrap();
        assert_eq!(v.schema_version, 1);
    }

    #[test]
    fn test_migrate_idempotent() {
        let tmp = TempDir::new().unwrap();
        let writ_dir = tmp.path().join(".writ");
        fs::create_dir_all(&writ_dir).unwrap();
        fs::create_dir_all(writ_dir.join("objects")).unwrap();

        migrate(&writ_dir, 0, 1).unwrap();
        // Run again — should succeed without error
        let v1 = RepoVersion::load(&writ_dir).unwrap().unwrap();
        migrate(&writ_dir, 0, 1).unwrap();
        let v2 = RepoVersion::load(&writ_dir).unwrap().unwrap();
        assert_eq!(v1.schema_version, v2.schema_version);
    }

    #[test]
    fn test_migrate_noop_when_current() {
        let tmp = TempDir::new().unwrap();
        let writ_dir = tmp.path().join(".writ");
        fs::create_dir_all(&writ_dir).unwrap();

        // Already at v1, migrating 1→1 is a no-op
        migrate(&writ_dir, 1, 1).unwrap();
        // No version file created (nothing ran)
        assert!(RepoVersion::load(&writ_dir).unwrap().is_none());
    }

    #[test]
    fn test_migrate_creates_backup() {
        let tmp = TempDir::new().unwrap();
        let writ_dir = tmp.path().join(".writ");
        fs::create_dir_all(&writ_dir).unwrap();

        // Write a fake version file first
        let v = RepoVersion {
            schema_version: 0,
            created_by: "0.0.1".into(),
            last_opened_by: "0.0.1".into(),
            created_at: None,
            last_opened_at: None,
        };
        v.save(&writ_dir).unwrap();

        // Create minimal dirs for migration to succeed
        fs::create_dir_all(writ_dir.join("objects")).unwrap();

        migrate(&writ_dir, 0, 1).unwrap();
        assert!(writ_dir.join("version.toml.bak").exists());
    }

    #[test]
    fn test_doctor_healthy_repo() {
        let tmp = TempDir::new().unwrap();
        let writ_dir = make_minimal_writ_dir(&tmp);
        RepoVersion::new().save(&writ_dir).unwrap();

        let report = DoctorReport::run(&writ_dir);
        assert!(report.is_healthy(), "failures: {:?}",
            report.checks.iter()
                .filter(|c| c.status == CheckStatus::Fail)
                .collect::<Vec<_>>());
    }

    #[test]
    fn test_doctor_missing_directory() {
        let tmp = TempDir::new().unwrap();
        let writ_dir = make_minimal_writ_dir(&tmp);
        RepoVersion::new().save(&writ_dir).unwrap();

        // Remove a required directory
        fs::remove_dir_all(writ_dir.join("objects")).unwrap();

        let report = DoctorReport::run(&writ_dir);
        assert!(!report.is_healthy());
        let dir_check = report.checks.iter().find(|c| c.name == "directories").unwrap();
        assert_eq!(dir_check.status, CheckStatus::Fail);
        assert!(dir_check.message.contains("objects"));
    }

    #[test]
    fn test_doctor_corrupt_index() {
        let tmp = TempDir::new().unwrap();
        let writ_dir = make_minimal_writ_dir(&tmp);
        RepoVersion::new().save(&writ_dir).unwrap();

        fs::write(writ_dir.join("index.json"), "not json").unwrap();

        let report = DoctorReport::run(&writ_dir);
        let idx = report.checks.iter().find(|c| c.name == "index").unwrap();
        assert_eq!(idx.status, CheckStatus::Fail);
    }

    #[test]
    fn test_doctor_missing_version_is_warning() {
        let tmp = TempDir::new().unwrap();
        let writ_dir = make_minimal_writ_dir(&tmp);
        // No version.toml written

        let report = DoctorReport::run(&writ_dir);
        let ver = report.checks.iter().find(|c| c.name == "version_file").unwrap();
        assert_eq!(ver.status, CheckStatus::Warning);
    }

    #[test]
    fn test_doctor_corrupt_spec() {
        let tmp = TempDir::new().unwrap();
        let writ_dir = make_minimal_writ_dir(&tmp);
        RepoVersion::new().save(&writ_dir).unwrap();

        fs::write(writ_dir.join("specs").join("bad.json"), "not a spec").unwrap();

        let report = DoctorReport::run(&writ_dir);
        let spec_check = report.checks.iter().find(|c| c.name == "specs").unwrap();
        assert_eq!(spec_check.status, CheckStatus::Fail);
    }

    // -----------------------------------------------------------------------
    // UPG.9: Version stamp tests (Bri)
    // -----------------------------------------------------------------------

    #[test]
    fn test_init_creates_version_toml() {
        let tmp = TempDir::new().unwrap();
        let repo = crate::repo::Repository::init(tmp.path()).unwrap();
        let writ_dir = tmp.path().join(".writ");

        let version = RepoVersion::load(&writ_dir).unwrap();
        assert!(version.is_some(), "init() should create version.toml");

        let v = version.unwrap();
        assert_eq!(v.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(v.created_by, env!("CARGO_PKG_VERSION"));
        assert!(v.created_at.is_some());
        drop(repo);
    }

    #[test]
    fn test_open_rejects_future_schema_version() {
        let tmp = TempDir::new().unwrap();
        let _repo = crate::repo::Repository::init(tmp.path()).unwrap();
        drop(_repo);

        // Manually bump schema_version to a future value
        let writ_dir = tmp.path().join(".writ");
        let mut v = RepoVersion::load(&writ_dir).unwrap().unwrap();
        v.schema_version = CURRENT_SCHEMA_VERSION + 99;
        v.save(&writ_dir).unwrap();

        let result = crate::repo::Repository::open(tmp.path());
        assert!(result.is_err());
        match result {
            Err(e) => {
                let err_msg = format!("{e}");
                assert!(
                    err_msg.contains("please update"),
                    "error should tell user to update: {err_msg}"
                );
            }
            Ok(_) => panic!("expected error for future schema version"),
        }
    }

    #[test]
    fn test_open_updates_last_opened_by() {
        let tmp = TempDir::new().unwrap();
        let _repo = crate::repo::Repository::init(tmp.path()).unwrap();
        drop(_repo);

        let writ_dir = tmp.path().join(".writ");

        // Manually change last_opened_by to something else
        let mut v = RepoVersion::load(&writ_dir).unwrap().unwrap();
        v.last_opened_by = "old-binary".into();
        v.save(&writ_dir).unwrap();

        // Re-open — should update last_opened_by
        let _repo2 = crate::repo::Repository::open(tmp.path()).unwrap();
        drop(_repo2);

        let v2 = RepoVersion::load(&writ_dir).unwrap().unwrap();
        assert_eq!(v2.last_opened_by, env!("CARGO_PKG_VERSION"));
        assert!(v2.last_opened_at.is_some());
    }

    #[test]
    fn test_created_by_preserved_across_opens() {
        let tmp = TempDir::new().unwrap();
        let _repo = crate::repo::Repository::init(tmp.path()).unwrap();
        drop(_repo);

        let writ_dir = tmp.path().join(".writ");

        // Manually set created_by to an older version
        let mut v = RepoVersion::load(&writ_dir).unwrap().unwrap();
        let original_created_by = "0.0.1-alpha".to_string();
        v.created_by = original_created_by.clone();
        v.save(&writ_dir).unwrap();

        // Re-open — should NOT change created_by
        let _repo2 = crate::repo::Repository::open(tmp.path()).unwrap();
        drop(_repo2);

        let v2 = RepoVersion::load(&writ_dir).unwrap().unwrap();
        assert_eq!(v2.created_by, original_created_by);
        // But last_opened_by should be updated
        assert_eq!(v2.last_opened_by, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn test_repo_version_default_matches_new() {
        let v1 = RepoVersion::new();
        let v2 = RepoVersion::default();
        assert_eq!(v1.schema_version, v2.schema_version);
        assert_eq!(v1.created_by, v2.created_by);
    }

    // -----------------------------------------------------------------------
    // UPG.10: Migration runner tests (Bri)
    // -----------------------------------------------------------------------

    #[test]
    fn test_migrate_unknown_step_returns_error() {
        let tmp = TempDir::new().unwrap();
        let writ_dir = tmp.path().join(".writ");
        fs::create_dir_all(&writ_dir).unwrap();

        // Try migrating to a version beyond what's implemented
        let result = migrate(&writ_dir, 1, 99);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("unknown migration step"));
    }

    #[test]
    fn test_legacy_repo_opens_via_auto_migration() {
        let tmp = TempDir::new().unwrap();

        // Build a legacy repo manually (no version.toml, minimal dirs)
        let writ_dir = tmp.path().join(".writ");
        fs::create_dir_all(writ_dir.join("objects")).unwrap();
        fs::create_dir_all(writ_dir.join("seals")).unwrap();
        fs::create_dir_all(writ_dir.join("specs")).unwrap();
        fs::create_dir_all(writ_dir.join("heads")).unwrap();
        fs::create_dir_all(writ_dir.join("keys")).unwrap();
        fs::create_dir_all(writ_dir.join("agents")).unwrap();
        fs::write(writ_dir.join("HEAD"), "").unwrap();
        fs::write(writ_dir.join("index.json"), r#"{"entries":{}}"#).unwrap();

        // Set up crypto (Repository::open expects master key)
        let ks = crate::keystore::KeyStore::open(&writ_dir);
        ks.ensure_master_key().unwrap();

        // No version.toml — this is a legacy repo
        assert!(!RepoVersion::path(&writ_dir).exists());

        // Open should auto-migrate from v0 → v1
        let repo = crate::repo::Repository::open(tmp.path()).unwrap();
        drop(repo);

        // After auto-migration, version.toml should exist at v1
        let v = RepoVersion::load(&writ_dir).unwrap().unwrap();
        assert_eq!(v.schema_version, CURRENT_SCHEMA_VERSION);

        // Migration should have created proposals/ and security/
        assert!(writ_dir.join("proposals").is_dir());
        assert!(writ_dir.join("security").is_dir());
    }

    #[test]
    fn test_migrate_backup_preserves_original_content() {
        let tmp = TempDir::new().unwrap();
        let writ_dir = tmp.path().join(".writ");
        fs::create_dir_all(&writ_dir).unwrap();
        fs::create_dir_all(writ_dir.join("objects")).unwrap();

        let original = RepoVersion {
            schema_version: 0,
            created_by: "0.0.1-ancient".into(),
            last_opened_by: "0.0.1-ancient".into(),
            created_at: None,
            last_opened_at: None,
        };
        original.save(&writ_dir).unwrap();
        let original_content = fs::read_to_string(RepoVersion::path(&writ_dir)).unwrap();

        migrate(&writ_dir, 0, 1).unwrap();

        // Backup should contain the original content
        let backup_content = fs::read_to_string(writ_dir.join("version.toml.bak")).unwrap();
        assert_eq!(backup_content, original_content);

        // But the actual file should be updated
        let updated = RepoVersion::load(&writ_dir).unwrap().unwrap();
        assert_eq!(updated.schema_version, 1);
    }

    // -----------------------------------------------------------------------
    // UPG.11: Doctor tests (Bri)
    // -----------------------------------------------------------------------

    #[test]
    fn test_doctor_on_fresh_init() {
        // BRI-UPG1 fixed: doctor now checks keys/.master (matching KeyStore)
        let tmp = TempDir::new().unwrap();
        let _repo = crate::repo::Repository::init(tmp.path()).unwrap();
        let writ_dir = tmp.path().join(".writ");

        let report = DoctorReport::run(&writ_dir);
        assert_eq!(report.checks.len(), 8);

        // All checks should pass on a fresh init
        for check in &report.checks {
            assert!(
                check.status != CheckStatus::Fail,
                "check '{}' unexpectedly failed: {}",
                check.name,
                check.message
            );
        }
        assert!(report.is_healthy());
        drop(_repo);
    }

    #[test]
    fn test_doctor_report_serialization_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let writ_dir = make_minimal_writ_dir(&tmp);
        RepoVersion::new().save(&writ_dir).unwrap();

        let report = DoctorReport::run(&writ_dir);

        // Serialize to JSON and back
        let json = serde_json::to_string(&report).unwrap();
        let deserialized: DoctorReport = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.passed, report.passed);
        assert_eq!(deserialized.failed, report.failed);
        assert_eq!(deserialized.warnings, report.warnings);
        assert_eq!(deserialized.checks.len(), report.checks.len());
    }

    #[test]
    fn test_doctor_corrupt_config() {
        let tmp = TempDir::new().unwrap();
        let writ_dir = make_minimal_writ_dir(&tmp);
        RepoVersion::new().save(&writ_dir).unwrap();

        fs::write(writ_dir.join("config.toml"), "not valid toml {{{{").unwrap();

        let report = DoctorReport::run(&writ_dir);
        let cfg = report.checks.iter().find(|c| c.name == "config").unwrap();
        assert_eq!(cfg.status, CheckStatus::Fail);
        assert!(cfg.message.contains("corrupt"));
    }

    #[test]
    fn test_doctor_missing_master_key() {
        let tmp = TempDir::new().unwrap();
        let writ_dir = make_minimal_writ_dir(&tmp);
        RepoVersion::new().save(&writ_dir).unwrap();

        // make_minimal_writ_dir writes keys/.master — just remove it
        fs::remove_file(writ_dir.join("keys").join(".master")).unwrap();

        let report = DoctorReport::run(&writ_dir);
        let key = report.checks.iter().find(|c| c.name == "master_key").unwrap();
        assert_eq!(key.status, CheckStatus::Fail);
        assert!(key.message.contains(".master"));
    }

    #[test]
    fn test_doctor_corrupt_seal() {
        let tmp = TempDir::new().unwrap();
        let writ_dir = make_minimal_writ_dir(&tmp);
        RepoVersion::new().save(&writ_dir).unwrap();

        fs::write(
            writ_dir.join("seals").join("bad-seal.json"),
            "not a seal",
        )
        .unwrap();

        let report = DoctorReport::run(&writ_dir);
        let seal_check = report.checks.iter().find(|c| c.name == "seals").unwrap();
        assert_eq!(seal_check.status, CheckStatus::Fail);
    }

    #[test]
    fn test_doctor_missing_index() {
        let tmp = TempDir::new().unwrap();
        let writ_dir = make_minimal_writ_dir(&tmp);
        RepoVersion::new().save(&writ_dir).unwrap();

        fs::remove_file(writ_dir.join("index.json")).unwrap();

        let report = DoctorReport::run(&writ_dir);
        let idx = report.checks.iter().find(|c| c.name == "index").unwrap();
        assert_eq!(idx.status, CheckStatus::Fail);
        assert!(idx.message.contains("missing"));
    }

    #[test]
    fn test_doctor_no_config_is_pass() {
        let tmp = TempDir::new().unwrap();
        let writ_dir = make_minimal_writ_dir(&tmp);
        RepoVersion::new().save(&writ_dir).unwrap();

        // Ensure no config.toml exists
        let config_path = writ_dir.join("config.toml");
        if config_path.exists() {
            fs::remove_file(&config_path).unwrap();
        }

        let report = DoctorReport::run(&writ_dir);
        let cfg = report.checks.iter().find(|c| c.name == "config").unwrap();
        assert_eq!(cfg.status, CheckStatus::Pass);
        assert!(cfg.message.contains("defaults"));
    }

    #[test]
    fn test_doctor_future_schema_is_fail() {
        let tmp = TempDir::new().unwrap();
        let writ_dir = make_minimal_writ_dir(&tmp);

        let mut v = RepoVersion::new();
        v.schema_version = CURRENT_SCHEMA_VERSION + 10;
        v.save(&writ_dir).unwrap();

        let report = DoctorReport::run(&writ_dir);
        let schema = report
            .checks
            .iter()
            .find(|c| c.name == "schema_version")
            .unwrap();
        assert_eq!(schema.status, CheckStatus::Fail);
        assert!(schema.message.contains("newer"));
    }

    #[test]
    fn test_doctor_check_count() {
        let tmp = TempDir::new().unwrap();
        let writ_dir = make_minimal_writ_dir(&tmp);
        RepoVersion::new().save(&writ_dir).unwrap();

        let report = DoctorReport::run(&writ_dir);
        // Sprint spec says 8 checks
        assert_eq!(
            report.checks.len(),
            8,
            "doctor should run exactly 8 checks"
        );
        assert_eq!(
            report.passed + report.failed + report.warnings,
            8,
            "counts should sum to total checks"
        );
    }
}
