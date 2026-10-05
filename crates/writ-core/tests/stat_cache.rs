//! perf-context: the stat cache never hides a change `state()` must report.

use std::fs::{self, File};
use std::path::Path;
use std::time::{Duration, SystemTime};

use tempfile::tempdir;

use writ_core::seal::{AgentIdentity, AgentType, TaskStatus, Verification};
use writ_core::stat_cache::STAT_CACHE_FILE;
use writ_core::state::FileStatus;
use writ_core::Repository;

fn set_mtime(path: &Path, t: SystemTime) {
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

fn seal_all(repo: &Repository) {
    repo.seal(
        AgentIdentity {
            id: "human".into(),
            agent_type: AgentType::Human,
        },
        "base".into(),
        None,
        TaskStatus::InProgress,
        Verification::default(),
        false,
    )
    .unwrap();
}

#[test]
fn modified_file_with_preserved_mtime_is_reported_by_state_and_context() {
    let dir = tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    let f = dir.path().join("a.txt");
    fs::write(&f, "one\n").unwrap();
    let old = SystemTime::now() - Duration::from_secs(120);
    set_mtime(&f, old);
    seal_all(&repo);
    std::thread::sleep(Duration::from_millis(20));

    // Two clean scans: the second is served from the cache.
    assert!(repo.state().unwrap().is_clean());
    assert!(repo.state().unwrap().is_clean());
    assert!(dir.path().join(".writ").join(STAT_CACHE_FILE).exists());

    // Rewrite with a different size and put the old mtime back.
    fs::write(&f, "one, and more\n").unwrap();
    set_mtime(&f, old);

    let state = repo.state().unwrap();
    assert_eq!(state.changes.len(), 1);
    assert_eq!(state.changes[0].path, "a.txt");
    assert_eq!(state.changes[0].status, FileStatus::Modified);
    let diff = repo.diff().unwrap();
    assert_eq!(diff.files_changed, 1);
    assert_eq!(diff.total_additions, 1);
}

#[test]
fn deleted_cache_file_changes_nothing() {
    let dir = tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    fs::write(dir.path().join("a.txt"), "one\n").unwrap();
    seal_all(&repo);
    fs::write(dir.path().join("a.txt"), "two\n").unwrap();
    let with = repo.state().unwrap();
    let _ = fs::remove_file(dir.path().join(".writ").join(STAT_CACHE_FILE));
    let without = repo.state().unwrap();
    assert_eq!(with.changes.len(), 1);
    assert_eq!(without.changes.len(), 1);
    assert_eq!(with.changes[0].hash, without.changes[0].hash);
}
