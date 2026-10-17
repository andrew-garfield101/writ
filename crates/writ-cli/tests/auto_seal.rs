//! Auto seal hook under a busy repository lock (sprint 6 ruling 149).
//!
//! The hook waits at most `HOOK_LOCK_WAIT` (200 ms) for the lock; on
//! timeout it marks the path pending, logs once per path, prints nothing,
//! exits 0; the next hook of the same identity drains the pending path.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

struct Repo(PathBuf);

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn writ(root: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_writ"));
    c.args(args)
        .current_dir(root)
        .env("WRIT_AGENT_ID", "alpha")
        .env("HOME", root.join(".home"))
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.unwrap_or("").as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn repo(tag: &str) -> Repo {
    let root = std::env::temp_dir().join(format!(
        "writ-auto-lock-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(root.join(".home")).unwrap();
    let git = |args: &[&str]| {
        assert!(Command::new("git")
            .args(args)
            .current_dir(&root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", root.join(".home"))
            .output()
            .unwrap()
            .status
            .success())
    };
    git(&["init", "-q", "-b", "main"]);
    fs::write(root.join(".gitignore"), ".home/\n").unwrap();
    fs::write(root.join("a.txt"), "a\n").unwrap();
    fs::write(root.join("b.txt"), "b\n").unwrap();
    assert!(writ(&root, &["init", "-y"], None).status.success());
    assert!(
        writ(&root, &["spec", "add", "--id", "sa", "--title", "a"], None)
            .status
            .success()
    );
    assert!(
        writ(&root, &["spec", "claim", "sa", "--agent", "alpha"], None)
            .status
            .success()
    );
    Repo(root)
}

fn edit(root: &Path, rel: &str) -> (Output, Duration) {
    let stdin = json!({
        "session_id": "00000000-0000-4000-8000-000000000001",
        "hook_event_name": "PostToolUse",
        "cwd": root.display().to_string(),
        "tool_name": "Edit",
        "tool_input": {"file_path": root.join(rel).display().to_string()},
    })
    .to_string();
    let t = Instant::now();
    let out = writ(root, &["hook"], Some(&stdin));
    (out, t.elapsed())
}

fn seals(root: &Path) -> Vec<Value> {
    let out = writ(
        root,
        &["log", "--all", "--format", "json", "--limit", "100"],
        None,
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn log_lines(root: &Path, outcome: &str) -> usize {
    fs::read_to_string(root.join(".writ/hooks.log"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains(&format!("\"outcome\":\"{outcome}\"")))
        .count()
}

#[test]
fn busy_lock_marks_pending_and_the_next_edit_drains_it() {
    let r = repo("busy");
    let root = &r.0;
    let before = seals(root).len();
    let held = writ_core::lock::RepoLock::acquire(&root.join(".writ"), Duration::from_secs(5))
        .expect("test holds the lock");
    fs::write(root.join("a.txt"), "a edited\n").unwrap();
    for _ in 0..2 {
        let (out, took) = edit(root, "a.txt");
        assert_eq!(out.status.code(), Some(0));
        assert!(out.stdout.is_empty(), "{out:?}");
        assert!(
            took < Duration::from_secs(2),
            "hook blocked {took:?} on a held lock"
        );
    }
    assert_eq!(log_lines(root, "lock_timeout"), 1, "logged once per path");
    drop(held);
    assert_eq!(
        seals(root).len(),
        before,
        "nothing sealed while the lock was held"
    );
    // The next edit of another path drains a.txt first, then seals b.txt.
    fs::write(root.join("b.txt"), "b edited\n").unwrap();
    let (out, _) = edit(root, "b.txt");
    assert_eq!(out.status.code(), Some(0));
    let all = seals(root);
    assert_eq!(all.len(), before + 2, "{all:#?}");
    let paths: Vec<&str> = all[..2]
        .iter()
        .map(|s| s["changes"][0]["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        vec!["b.txt", "a.txt"],
        "drain first, then the new path"
    );
    assert!(all[1]["summary"]
        .as_str()
        .unwrap()
        .starts_with("auto: retry"));
    assert!(all.iter().take(2).all(|s| s["spec_id"] == "sa"));
}

#[test]
fn seal_auto_prints_no_advisories() {
    let r = repo("quiet");
    let root = &r.0;
    fs::write(root.join("a.txt"), "a edited\n").unwrap();
    let out = writ(root, &["seal", "--auto", "--paths", "a.txt"], None);
    assert!(out.status.success(), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains("writ context"),
        "advisory under --auto: {err}"
    );
    let s = &seals(root)[0];
    assert_eq!(s["summary"], "auto: seal a.txt");
}

/// Ruling 157 (3): `spec done` claims the agent's one unclaimed spec it
/// created, as its first seal would (finding 87).
#[test]
fn spec_done_claims_the_one_spec_the_agent_created() {
    let r = repo("done-claim");
    let root = &r.0;
    assert!(
        writ(root, &["spec", "release", "sa", "--agent", "alpha"], None)
            .status
            .success()
    );
    let out = writ(root, &["spec", "done", "-s", "my own words"], None);
    assert!(out.status.success(), "{out:?}");
    let spec: Value = serde_json::from_slice(
        &writ(root, &["spec", "show", "sa", "--format", "json"], None).stdout,
    )
    .unwrap();
    assert_eq!(spec["status"], "complete", "{spec}");
    assert_eq!(spec["claimed_by"], "alpha");
    assert!(spec.to_string().contains("my own words"));
}
