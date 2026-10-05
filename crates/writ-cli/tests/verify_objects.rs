//! H.2 (finding 28): `writ verify` reports referenced-but-missing objects,
//! and `writ gc run --dry-run` warns without aborting.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use writ_core::seal::{AgentIdentity, AgentType, TaskStatus, Verification};
use writ_core::Repository;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("writ-verify-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn writ(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_writ"))
        .args(args)
        .current_dir(root)
        .env("WRIT_AGENT_ID", "amis-test")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// Two seals; `kept.txt` is carried forward by the second tree only, then its
/// blob is deleted. Returns the missing hash.
fn repo_with_missing_blob(root: &Path) -> String {
    let repo = Repository::init(root).unwrap();
    let agent = AgentIdentity {
        id: "amis-test".into(),
        agent_type: AgentType::Agent,
    };
    let seal = |summary: &str| {
        repo.seal(
            agent.clone(),
            summary.into(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap()
    };
    fs::write(root.join("kept.txt"), "kept\n").unwrap();
    fs::write(root.join("edit.txt"), "v1\n").unwrap();
    let first = seal("one");
    fs::write(root.join("edit.txt"), "v2\n").unwrap();
    seal("two");
    let kept = first
        .changes
        .iter()
        .find(|c| c.path == "kept.txt")
        .and_then(|c| c.new_hash.clone())
        .unwrap();
    let objects = repo.writ_dir().join("objects");
    fs::remove_file(objects.join(&kept[..2]).join(&kept[2..])).unwrap();
    kept
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("not JSON ({e}):\n{}", String::from_utf8_lossy(&out.stdout)))
}

#[test]
fn test_verify_all_chains_json_lists_missing_object_hash_and_path() {
    let scratch = Scratch::new("all");
    let missing = repo_with_missing_blob(&scratch.0);

    let out = writ(&scratch.0, &["verify", "--all-chains", "--format", "json"]);
    let v = json(&out);

    assert_eq!(out.status.code(), Some(1), "failures must exit 1");
    assert_eq!(v["all_valid"], false);
    assert_eq!(v["missing_objects"][0]["hash"], missing.as_str());
    assert_eq!(v["missing_objects"][0]["path"], "kept.txt");
    assert_eq!(v["missing_objects"].as_array().unwrap().len(), 1);
}

#[test]
fn test_verify_chain_json_lists_missing_object() {
    let scratch = Scratch::new("chain");
    let missing = repo_with_missing_blob(&scratch.0);

    let out = writ(&scratch.0, &["verify", "--chain", "--format", "json"]);
    let v = json(&out);

    assert_eq!(out.status.code(), Some(1), "failures must exit 1");
    assert_eq!(v["valid"], false);
    assert_eq!(v["missing_objects"][0]["hash"], missing.as_str());
}

#[test]
fn test_verify_all_chains_clean_store_has_empty_missing_objects() {
    let scratch = Scratch::new("clean");
    let root = scratch.0.as_path();
    let repo = Repository::init(root).unwrap();
    fs::write(root.join("a.txt"), "a\n").unwrap();
    repo.seal(
        AgentIdentity {
            id: "amis-test".into(),
            agent_type: AgentType::Agent,
        },
        "one".into(),
        None,
        TaskStatus::InProgress,
        Verification::default(),
        false,
    )
    .unwrap();

    let out = writ(root, &["verify", "--all-chains", "--format", "json"]);
    let v = json(&out);

    assert_eq!(out.status.code(), Some(0), "clean store must exit 0");
    assert_eq!(v["all_valid"], true);
    assert_eq!(v["missing_objects"], serde_json::json!([]));
}

#[test]
fn test_verify_human_output_exits_1_on_missing_object() {
    let scratch = Scratch::new("human");
    let missing = repo_with_missing_blob(&scratch.0);

    for args in [&["verify", "--all-chains"][..], &["verify"][..]] {
        let out = writ(&scratch.0, args);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.code(), Some(1), "{args:?}:\n{stdout}");
        assert!(stdout.contains(&missing), "{args:?} did not list {missing}");
        assert!(stdout.contains("kept.txt"));
    }
}

#[test]
fn test_verify_default_clean_store_exits_0() {
    let scratch = Scratch::new("clean-default");
    let root = scratch.0.as_path();
    Repository::init(root).unwrap();

    let out = writ(root, &["verify"]);

    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn test_gc_run_dry_run_warns_but_does_not_abort_on_missing_object() {
    let scratch = Scratch::new("dry");
    let missing = repo_with_missing_blob(&scratch.0);

    let out = writ(&scratch.0, &["gc", "run", "--dry-run"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(out.status.success(), "dry run aborted:\n{stderr}");
    assert!(
        stderr.contains(&missing),
        "dry run did not list {missing}:\n{stderr}"
    );
}
