//! Finding 28 detection (hotfix 0.2.1, H.2): `writ gc run` must check that
//! every object the live set references is present before it deletes
//! anything, abort with the missing list when one is not, and proceed only
//! with `--force`. Runs the real binary against a throwaway store.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use writ_core::seal::{AgentIdentity, AgentType, Seal, TaskStatus, Verification};
use writ_core::Repository;

/// Temp project dir removed on drop (writ-cli has no tempfile dev-dependency).
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("writ-gc-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

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

/// Every object file in the store, as full hashes.
fn objects_on_disk(writ_dir: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for prefix in fs::read_dir(writ_dir.join("objects")).unwrap() {
        let prefix = prefix.unwrap();
        if !prefix.file_type().unwrap().is_dir() {
            continue;
        }
        let p = prefix.file_name().to_string_lossy().to_string();
        for obj in fs::read_dir(prefix.path()).unwrap() {
            out.insert(format!("{p}{}", obj.unwrap().file_name().to_string_lossy()));
        }
    }
    out
}

fn writ(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_writ"))
        .args(args)
        .current_dir(root)
        .env("WRIT_AGENT_ID", "bri-test")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn test_gc_run_aborts_on_missing_object_and_proceeds_with_force() {
    // Arrange: s1 adds carried.txt + edited.txt, s2 edits edited.txt only, so
    // carried.txt's blob is referenced by s2's tree alone. Then lose it.
    let scratch = Scratch::new("abort");
    let root = scratch.0.as_path();
    let repo = Repository::init(root).unwrap();
    fs::write(root.join("carried.txt"), "unchanged across seals\n").unwrap();
    fs::write(root.join("edited.txt"), "v1\n").unwrap();
    let s1 = seal(&repo, "baseline");
    fs::write(root.join("edited.txt"), "v2\n").unwrap();
    seal(&repo, "edit one file");
    let carried = s1
        .changes
        .iter()
        .find(|c| c.path == "carried.txt")
        .and_then(|c| c.new_hash.clone())
        .unwrap();
    let writ_dir = repo.writ_dir().to_path_buf();
    fs::remove_file(
        writ_dir
            .join("objects")
            .join(&carried[..2])
            .join(&carried[2..]),
    )
    .unwrap();
    let before = objects_on_disk(&writ_dir);
    assert!(!before.contains(&carried));
    assert!(before.len() >= 4, "precondition: blobs and trees stored");

    // Act 1: plain run must refuse, name the missing object, touch nothing.
    let aborted = writ(root, &["gc", "run", "--yes"]);
    let text = combined(&aborted);
    assert!(
        !aborted.status.success(),
        "gc run proceeded over a missing referenced object:\n{text}"
    );
    assert!(
        text.contains(&carried[..12]),
        "abort output does not list the missing object {}:\n{text}",
        &carried[..12]
    );
    assert_eq!(
        objects_on_disk(&writ_dir),
        before,
        "aborted run mutated the store"
    );

    // Act 2: --force proceeds, and still deletes nothing live.
    let forced = writ(root, &["gc", "run", "--yes", "--force"]);
    assert!(
        forced.status.success(),
        "gc run --force did not proceed:\n{}",
        combined(&forced)
    );
    assert_eq!(
        objects_on_disk(&writ_dir),
        before,
        "forced run deleted live objects (every object here is referenced)"
    );
}
