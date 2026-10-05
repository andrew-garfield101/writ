//! Finding 28: `writ gc run` pruned live data.
//!
//! `gc::find_orphaned_objects` builds its referenced set from each seal's
//! top-level tree hash plus the `old_hash`/`new_hash` in that seal's changes,
//! and never walks into the tree objects. Every blob carried forward unchanged
//! (any file a later seal did not touch, e.g. most of a bridge import) is
//! therefore reported as orphaned and deleted by a prune. On the writ repo this
//! removed 1,297 live objects and `writ context` failed with object not found,
//! while `writ verify --all-chains` still reported valid.

use std::fs;
use std::path::Path;

use tempfile::{tempdir, TempDir};

use writ_core::gc::{find_orphaned_objects, load_all_seals};
use writ_core::seal::{AgentIdentity, AgentType, Seal, TaskStatus, Verification};
use writ_core::Repository;

fn seal(repo: &Repository, summary: &str) -> Seal {
    repo.seal(
        AgentIdentity {
            id: "bri-test".to_string(),
            agent_type: AgentType::Agent,
        },
        summary.to_string(),
        None,
        TaskStatus::InProgress,
        Verification::default(),
        false,
    )
    .unwrap()
}

/// Two seals: the first adds `carried.txt` and `edited.txt`, the second
/// changes only `edited.txt`. In the second seal `carried.txt` is referenced
/// by the tree alone, which is the shape of every file a bridge import or an
/// archived seal leaves behind.
fn carried_forward_repo() -> (TempDir, Repository, String, Seal) {
    let dir = tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    fs::write(dir.path().join("carried.txt"), "unchanged across seals\n").unwrap();
    fs::write(dir.path().join("edited.txt"), "v1\n").unwrap();
    let first = seal(&repo, "baseline");
    let carried_hash = first
        .changes
        .iter()
        .find(|c| c.path == "carried.txt")
        .and_then(|c| c.new_hash.clone())
        .expect("baseline seal records carried.txt");

    fs::write(dir.path().join("edited.txt"), "v2\n").unwrap();
    let second = seal(&repo, "edit one file");
    assert!(second.changes.iter().all(|c| c.path != "carried.txt"));
    (dir, repo, carried_hash, second)
}

fn object_file(writ_dir: &Path, hash: &str) -> std::path::PathBuf {
    writ_dir.join("objects").join(&hash[..2]).join(&hash[2..])
}

/// Second missing root: the workspace index. A git repo with one committed
/// file and one untracked (not ignored) file. `writ init` imports via the
/// bridge; the import seal's tree and change list hold only the committed
/// file, but the index stores and references the untracked file's blob (and
/// `.writignore`'s). No seal references them, so the orphan scan reports
/// them, and pruning them breaks `diff`/`context` on the next call. Every seal
/// is loaded, exactly as `gc run` does.
#[cfg(feature = "bridge")]
#[test]
#[ignore = "finding 28: find_orphaned_objects never walks seal trees, so carried-forward blobs are pruned (sprint 2 gc-integrity)"]
fn test_index_only_blob_is_not_orphaned_after_bridge_import() {
    use std::process::Command;
    let dir = tempdir().unwrap();
    let root = dir.path();
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@localhost"])
            .args(args)
            .current_dir(root)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q"]);
    fs::write(root.join("committed.txt"), "in git\n").unwrap();
    git(&["add", "committed.txt"]);
    git(&["commit", "-qm", "base"]);
    fs::write(root.join("untracked.txt"), "only in the working tree\n").unwrap();

    Repository::init_project(root).unwrap();
    let repo = Repository::open(root).unwrap();
    let seals = load_all_seals(repo.writ_dir()).unwrap();
    let import = &seals[0];
    assert!(import.changes.iter().all(|c| c.path != "untracked.txt"));
    assert!(
        repo.state().unwrap().changes.is_empty(),
        "untracked.txt is tracked via the index, not pending"
    );
    let untracked_hash = writ_core::hash::hash_bytes(b"only in the working tree\n");
    assert!(
        object_file(repo.writ_dir(), &untracked_hash).exists(),
        "precondition: blob stored by the import"
    );

    let orphans = find_orphaned_objects(repo.writ_dir(), &seals).unwrap();
    assert!(
        orphans.iter().all(|o| o.hash != untracked_hash),
        "blob referenced by the workspace index is reported orphaned"
    );
}

/// Minimal shape without git: the referenced set is computed from a seal
/// whose tree carries a blob its change list does not mention.
#[test]
#[ignore = "finding 28: find_orphaned_objects never walks seal trees, so carried-forward blobs are pruned (sprint 2 gc-integrity)"]
fn test_carried_forward_blob_is_not_orphaned() {
    let (_dir, repo, carried_hash, second) = carried_forward_repo();
    let orphans = find_orphaned_objects(repo.writ_dir(), &[second]).unwrap();
    assert!(
        orphans.iter().all(|o| o.hash != carried_hash),
        "carried.txt's blob is referenced by HEAD's tree but reported orphaned"
    );
}

#[test]
fn test_orphan_scan_with_full_history_keeps_carried_blob() {
    // Control: with every seal loaded, the first seal's change list still
    // references the blob, so the current code passes. This is why the bug
    // hid in unit tests and surfaced only on a bridge-imported repo.
    let (_dir, repo, carried_hash, _second) = carried_forward_repo();
    let seals = load_all_seals(repo.writ_dir()).unwrap();
    let orphans = find_orphaned_objects(repo.writ_dir(), &seals).unwrap();
    assert!(orphans.iter().all(|o| o.hash != carried_hash));
}

#[test]
#[ignore = "finding 28: verify --all-chains checks seal hashes and signatures, not object presence (sprint 2 gc-integrity)"]
fn test_verify_all_chains_fails_when_referenced_blob_missing() {
    let (_dir, repo, carried_hash, _second) = carried_forward_repo();
    assert!(
        repo.verify_all_chains(None).unwrap().all_valid,
        "precondition"
    );

    fs::remove_file(object_file(repo.writ_dir(), &carried_hash)).unwrap();

    let result = repo.verify_all_chains(None).unwrap();
    assert!(
        !result.all_valid,
        "verify reported a valid store with a tree-referenced blob missing"
    );
}

#[test]
fn test_context_fails_loudly_when_referenced_blob_missing() {
    // Pins today's observed symptom so a fix that silently skips missing
    // objects is a deliberate choice, not an accident.
    let (dir, repo, carried_hash, _second) = carried_forward_repo();
    fs::remove_file(object_file(repo.writ_dir(), &carried_hash)).unwrap();
    fs::write(dir.path().join("carried.txt"), "touched after prune\n").unwrap();

    let err = repo.diff().err();
    assert!(
        err.is_some(),
        "diff over a missing blob must error, not guess"
    );
}
