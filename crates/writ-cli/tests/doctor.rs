//! `writ doctor` fast tier (sprint 3, doctor-core): one test per check, the
//! clean repository, the honesty condition, and the latency budget.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use writ_core::seal::{AgentIdentity, AgentType, TaskStatus, Verification};
use writ_core::spec::Spec;
use writ_core::Repository;

const CLEAN: &str = "fast checks clean; survival check not available until 0.4.1";

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("writ-doctor-{tag}-{}-{nanos}", std::process::id()));
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
        .env_remove("WRIT_FORMAT")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("not JSON ({e}):\n{}", String::from_utf8_lossy(&out.stdout)))
}

fn agent() -> AgentIdentity {
    AgentIdentity {
        id: "amis-test".into(),
        agent_type: AgentType::Agent,
    }
}

fn seal(repo: &Repository, summary: &str) -> writ_core::seal::Seal {
    repo.seal(
        agent(),
        summary.into(),
        None,
        TaskStatus::InProgress,
        Verification::default(),
        false,
    )
    .unwrap()
}

/// A repo with one sealed file and nothing pending.
fn clean_repo(root: &Path) -> Repository {
    let repo = Repository::init(root).unwrap();
    fs::write(root.join("a.txt"), "a\n").unwrap();
    seal(&repo, "one");
    repo
}

fn finding_ids(v: &serde_json::Value) -> Vec<String> {
    v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["check"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn clean_repo_headline_exit_zero_and_json_shape() {
    let s = Scratch::new("clean");
    clean_repo(&s.0);

    let human = writ(&s.0, &["doctor"]);
    let text = String::from_utf8_lossy(&human.stdout);
    assert_eq!(human.status.code(), Some(0), "{text}");
    assert_eq!(text.lines().next().unwrap(), CLEAN);

    let out = writ(&s.0, &["doctor", "--format", "json"]);
    assert_eq!(out.status.code(), Some(0));
    let v = json(&out);
    assert_eq!(v["headline"], CLEAN);
    assert_eq!(v["tier"], "fast");
    assert!(v["survival_last_green"].is_null());
    assert_eq!(v["clean"], true);
    assert!(v["findings"].as_array().unwrap().is_empty());
}

#[test]
fn legacy_json_flag_still_emits_json() {
    let s = Scratch::new("legacy-json");
    clean_repo(&s.0);
    let v = json(&writ(&s.0, &["doctor", "--json"]));
    assert_eq!(v["tier"], "fast");
}

#[test]
fn store_integrity_missing_blob_is_red_with_repair_fix() {
    let s = Scratch::new("store");
    let repo = Repository::init(&s.0).unwrap();
    fs::write(s.0.join("kept.txt"), "kept\n").unwrap();
    let first = seal(&repo, "one");
    let hash = first.changes[0].new_hash.clone().unwrap();
    fs::remove_file(
        repo.writ_dir()
            .join("objects")
            .join(&hash[..2])
            .join(&hash[2..]),
    )
    .unwrap();

    let out = writ(&s.0, &["doctor", "--format", "json"]);
    assert_eq!(out.status.code(), Some(1));
    let v = json(&out);
    let f = &v["findings"][0];
    assert_eq!(f["check"], "store_integrity");
    assert_eq!(f["severity"], "red");
    assert_eq!(f["fix_command"], "writ repair");
    assert_eq!(f["paths"][0], "kept.txt");
    assert!(v["headline"]
        .as_str()
        .unwrap()
        .contains("survival check not available until 0.4.1"));
    run_fix_then_clean(&s.0, f["fix_command"].as_str().unwrap());
}

/// Paste `fix` verbatim (split on spaces, no shell) and assert doctor is
/// clean afterwards.
fn run_fix_then_clean(root: &Path, fix: &str) {
    run_fix_as(root, "amis-test", fix);
    let after = writ(root, &["doctor", "--format", "json"]);
    assert_eq!(after.status.code(), Some(0), "{}", json(&after));
}

/// Paste `fix` into `sh -c` verbatim, as `agent`, with the test binary first
/// on PATH so `writ` is the build under test.
fn run_fix_as(root: &Path, agent: &str, fix: &str) {
    assert!(fix.starts_with("writ "), "{fix}");
    let out = Command::new("sh")
        .args(["-c", fix])
        .current_dir(root)
        .env("PATH", test_path(None))
        .env("WRIT_AGENT_ID", agent)
        .env_remove("WRIT_FORMAT")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{fix}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// PATH with the directory of the writ under test first, then `extra`,
/// then the system directories `sh`, `rm` and `git` need.
fn test_path(extra: Option<&Path>) -> String {
    let bin = Path::new(env!("CARGO_BIN_EXE_writ"))
        .parent()
        .unwrap()
        .to_path_buf();
    let mut dirs = vec![bin];
    dirs.extend(extra.map(Path::to_path_buf));
    dirs.extend(["/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(PathBuf::from));
    std::env::join_paths(dirs)
        .unwrap()
        .to_string_lossy()
        .to_string()
}

fn writ_as(root: &Path, agent: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_writ"))
        .args(args)
        .current_dir(root)
        .env("WRIT_AGENT_ID", agent)
        .env_remove("WRIT_FORMAT")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn ok_as(root: &Path, agent: &str, args: &[&str]) -> String {
    let out = writ_as(root, agent, args);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "writ {args:?} as {agent}: {text}");
    text
}

/// Findings for `check` from a JSON doctor run (as `agent`).
fn findings_for(root: &Path, agent: &str, check: &str) -> Vec<serde_json::Value> {
    let v = json(&writ_as(root, agent, &["doctor", "--format", "json"]));
    v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["check"] == check)
        .cloned()
        .collect()
}

/// A git repo with one commit and writ initialised (bare: no agent files).
fn git_writ_project(root: &Path) {
    git(root, &["init", "-q"]);
    git(root, &["config", "user.name", "writ-test"]);
    git(root, &["config", "user.email", "writ-test@localhost"]);
    git(root, &["config", "core.hooksPath", "/dev/null"]);
    fs::write(root.join(".gitignore"), ".writ/\n").unwrap();
    fs::write(root.join("README.md"), "base\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "base"]);
    let out = writ(root, &["init", "-y", "--bare"]);
    assert!(out.status.success(), "{out:?}");
}

#[test]
fn store_integrity_unrecoverable_fix_accepts_the_loss() {
    let s = Scratch::new("store-lost");
    let repo = Repository::init(&s.0).unwrap();
    fs::write(s.0.join("gone.txt"), "gone\n").unwrap();
    let first = seal(&repo, "one");
    fs::remove_file(s.0.join("gone.txt")).unwrap();
    seal(&repo, "two");
    let hash = first.changes[0].new_hash.clone().unwrap();
    fs::remove_file(
        repo.writ_dir()
            .join("objects")
            .join(&hash[..2])
            .join(&hash[2..]),
    )
    .unwrap();

    let out = writ(&s.0, &["doctor", "--format", "json"]);
    assert_eq!(out.status.code(), Some(1));
    let v = json(&out);
    let fix = v["findings"][0]["fix_command"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(fix, format!("writ doctor --allow-missing {}", &hash[..12]));
    let repair = writ(&s.0, &["repair"]);
    let text = String::from_utf8_lossy(&repair.stdout);
    assert!(text.contains(&fix), "{text}");
    assert!(text.contains("[doctor] allow_missing = ["), "{text}");
    run_fix_then_clean(&s.0, &fix);
}

#[test]
fn store_integrity_allow_missing_excuses_known_objects() {
    let s = Scratch::new("allow");
    let repo = Repository::init(&s.0).unwrap();
    fs::write(s.0.join("kept.txt"), "kept\n").unwrap();
    let first = seal(&repo, "one");
    let hash = first.changes[0].new_hash.clone().unwrap();
    fs::remove_file(
        repo.writ_dir()
            .join("objects")
            .join(&hash[..2])
            .join(&hash[2..]),
    )
    .unwrap();
    let cfg = repo.writ_dir().join("config.toml");
    let mut text = fs::read_to_string(&cfg).unwrap_or_default();
    text.push_str(&format!(
        "\n[doctor]\nallow_missing = [\"{}\"]\n",
        &hash[..12]
    ));
    fs::write(&cfg, text).unwrap();

    let v = json(&writ(&s.0, &["doctor", "--format", "json"]));
    assert!(
        !finding_ids(&v).contains(&"store_integrity".to_string()),
        "{v}"
    );
}

#[test]
fn layout_damage_is_store_integrity_red() {
    let s = Scratch::new("layout");
    let repo = clean_repo(&s.0);
    fs::remove_dir_all(repo.writ_dir().join("specs")).unwrap();
    fs::remove_file(repo.writ_dir().join("workspaces/main/index.json")).unwrap();
    fs::write(repo.writ_dir().join("seals/broken.json"), "{").unwrap();
    let out = writ(&s.0, &["doctor", "--format", "json"]);
    assert_eq!(out.status.code(), Some(1));
    let v = json(&out);
    assert!(
        finding_ids(&v).iter().all(|c| c == "store_integrity"),
        "{v}"
    );
    let fixes: Vec<&str> = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["fix_command"].as_str().unwrap())
        .collect();
    assert!(fixes.iter().all(|f| *f == "writ repair"), "{fixes:?}");
    run_fix_then_clean(&s.0, "writ repair");
}

#[test]
fn no_output_ever_says_safe_to_finish() {
    let s = Scratch::new("never-safe");
    clean_repo(&s.0);
    for args in [
        vec!["doctor"],
        vec!["doctor", "--format", "json"],
        vec!["context", "--format", "brief"],
    ] {
        let out = writ(&s.0, &args);
        let all = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !all.to_lowercase().contains("safe to finish"),
            "{args:?}: {all}"
        );
    }
}

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A git project with writ initialised and one sealed file whose blob is
/// then deleted, so doctor reports `store_integrity` red.
fn git_project_with_red_store(root: &Path) {
    git(root, &["init", "-q"]);
    git(root, &["config", "user.name", "writ-test"]);
    git(root, &["config", "user.email", "writ-test@localhost"]);
    fs::write(root.join(".gitignore"), ".writ/\n").unwrap();
    fs::write(root.join("README.md"), "base\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "base"]);
    let out = writ(root, &["init", "-y", "--bare"]);
    assert!(out.status.success(), "{out:?}");
    let repo = Repository::open(root).unwrap();
    fs::write(root.join("kept.txt"), "kept\n").unwrap();
    let s = seal(&repo, "one");
    let c = s.changes.iter().find(|c| c.path == "kept.txt").unwrap();
    let hash = c.new_hash.clone().unwrap();
    fs::remove_file(
        repo.writ_dir()
            .join("objects")
            .join(&hash[..2])
            .join(&hash[2..]),
    )
    .unwrap();
}

/// Finding 76 + doctor gate: finish refuses on red, records a
/// `finish_refused` event with reason, specs and files; `--force` passes
/// the gate and records nothing new.
#[test]
fn finish_refuses_on_red_and_records_finish_refused_event() {
    let s = Scratch::new("finish-gate");
    git_project_with_red_store(&s.0);

    let out = writ(&s.0, &["finish", "-y", "--no-check"]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("finish refused"), "{text}");
    assert!(text.contains("fix:   writ repair"), "{text}");
    assert!(!text.to_lowercase().contains("safe to finish"));

    let events_path = s.0.join(".writ/security/events.jsonl");
    let events: Vec<serde_json::Value> = fs::read_to_string(&events_path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .filter(|v: &serde_json::Value| v["event_type"] == "finish_refused")
        .collect();
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["reason"], "doctor_red");
    assert_eq!(events[0]["agent_id"], "amis-test");
    assert_eq!(events[0]["files"][0], "kept.txt");
    assert!(events[0]["specs"].is_array());

    let forced = writ(&s.0, &["finish", "-y", "--no-check", "--force"]);
    let ftext = String::from_utf8_lossy(&forced.stderr);
    assert!(ftext.contains("--force: finishing despite"), "{ftext}");
    let after = fs::read_to_string(&events_path)
        .unwrap()
        .lines()
        .filter(|l| l.contains("\"finish_refused\""))
        .count();
    assert_eq!(after, 1);
}

/// left_out: the acting agent's only spec is complete and `stray.txt` was
/// never sealed. The fix creates a follow-up spec first, then seals.
#[test]
fn left_out_fix_works_for_an_agent_without_an_open_spec() {
    let s = Scratch::new("left-out");
    git_writ_project(&s.0);
    ok_as(
        &s.0,
        "ann",
        &["spec", "add", "--id", "a", "--title", "a", "--claim"],
    );
    fs::write(s.0.join("a.txt"), "a\n").unwrap();
    ok_as(
        &s.0,
        "ann",
        &["seal", "-s", "a", "--spec", "a", "--paths", "a.txt"],
    );
    ok_as(&s.0, "ann", &["spec", "done", "a", "-s", "a done"]);
    fs::write(s.0.join("stray.txt"), "stray\n").unwrap();

    let found = findings_for(&s.0, "ann", "left_out");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0]["paths"][0], "stray.txt");
    assert_eq!(found[0]["severity"], "yellow");
    let fix = found[0]["fix_command"].as_str().unwrap();
    assert!(
        fix.starts_with("writ spec add 'follow-up to writ doctor findings' --id doctor-follow-up-")
            && fix.contains("--spec doctor-follow-up-"),
        "{fix}"
    );
    assert!(!fix.contains("--include-unsealed"));
    assert!(found[0]["message"]
        .as_str()
        .unwrap()
        .contains("--include-unsealed"));
    run_fix_as(&s.0, "ann", fix);
    assert!(findings_for(&s.0, "ann", "left_out").is_empty());
}

/// Finding 75: a pending file identical to git HEAD is not left out.
#[test]
fn left_out_ignores_files_identical_to_head() {
    let s = Scratch::new("left-out-head");
    git_writ_project(&s.0);
    ok_as(
        &s.0,
        "ann",
        &["spec", "add", "--id", "a", "--title", "a", "--claim"],
    );
    fs::write(s.0.join("a.txt"), "a\n").unwrap();
    ok_as(
        &s.0,
        "ann",
        &["seal", "-s", "a", "--spec", "a", "--paths", "a.txt"],
    );
    ok_as(&s.0, "ann", &["spec", "done", "a", "-s", "a done"]);
    fs::write(s.0.join("README.md"), "changed\n").unwrap();
    git(&s.0, &["commit", "-q", "-am", "readme by hand"]);
    // README.md now differs from writ's index but equals HEAD.
    let found = findings_for(&s.0, "ann", "left_out");
    assert!(
        found
            .iter()
            .all(|f| !f["paths"].to_string().contains("README.md")),
        "{found:?}"
    );
}

/// committed_spec_seal: the newest seal of `f.txt` is on a committed spec and
/// HEAD holds other content. The fix opens a follow-up spec and seals it.
#[test]
fn committed_spec_seal_fix_moves_the_file_to_a_follow_up_spec() {
    let s = Scratch::new("stuck");
    git_writ_project(&s.0);
    ok_as(
        &s.0,
        "ann",
        &["spec", "add", "--id", "a", "--title", "first", "--claim"],
    );
    fs::write(s.0.join("f.txt"), "v1\n").unwrap();
    ok_as(
        &s.0,
        "ann",
        &["seal", "-s", "v1", "--spec", "a", "--paths", "f.txt"],
    );
    ok_as(&s.0, "ann", &["spec", "done", "a", "-s", "v1"]);
    ok_as(&s.0, "ann", &["finish", "-y", "--no-check"]);
    fs::write(s.0.join("f.txt"), "v2\n").unwrap();
    git(&s.0, &["commit", "-q", "-am", "v2 by hand"]);
    fs::write(s.0.join("f.txt"), "v3\n").unwrap();

    let found = findings_for(&s.0, "ann", "committed_spec_seal");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0]["severity"], "red");
    assert_eq!(found[0]["paths"][0], "f.txt");
    let fix = found[0]["fix_command"].as_str().unwrap();
    assert!(
        fix.starts_with("writ spec add 'follow-up to writ doctor findings' --id doctor-follow-up-")
            && fix.contains("--spec doctor-follow-up-"),
        "{fix}"
    );
    run_fix_as(&s.0, "ann", fix);
    assert!(findings_for(&s.0, "ann", "committed_spec_seal").is_empty());
}

/// unsealed_at_risk: two agents sealed `f.txt` on open specs and it has
/// changed since. The fix seals onto the acting agent's own open spec.
#[test]
fn unsealed_at_risk_multi_agent_fix_seals_on_own_spec() {
    let s = Scratch::new("at-risk");
    git_writ_project(&s.0);
    ok_as(
        &s.0,
        "ann",
        &["spec", "add", "--id", "sa", "--title", "sa", "--claim"],
    );
    ok_as(
        &s.0,
        "bob",
        &["spec", "add", "--id", "sb", "--title", "sb", "--claim"],
    );
    fs::write(s.0.join("f.txt"), "one\n").unwrap();
    ok_as(
        &s.0,
        "ann",
        &["seal", "-s", "a", "--spec", "sa", "--paths", "f.txt"],
    );
    fs::write(s.0.join("f.txt"), "one\ntwo\n").unwrap();
    ok_as(
        &s.0,
        "bob",
        &["seal", "-s", "b", "--spec", "sb", "--paths", "f.txt"],
    );
    fs::write(s.0.join("f.txt"), "one\ntwo\nthree\n").unwrap();

    let found = findings_for(&s.0, "ann", "unsealed_at_risk");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0]["message"].as_str().unwrap().contains("2 agents"));
    let fix = found[0]["fix_command"].as_str().unwrap();
    assert!(fix.contains("--spec sa"), "{fix}");
    run_fix_as(&s.0, "ann", fix);
    assert!(findings_for(&s.0, "ann", "unsealed_at_risk").is_empty());
}

/// version_skew (managed text): an edited CLAUDE.md block is stale; the
/// fix `writ init -y` refreshes it.
#[test]
fn version_skew_stale_managed_block_fixed_by_init() {
    let s = Scratch::new("skew-text");
    git(&s.0, &["init", "-q"]);
    let out = writ(&s.0, &["init", "-y"]);
    assert!(out.status.success(), "{out:?}");
    let md = fs::read_to_string(s.0.join("CLAUDE.md")).unwrap();
    fs::write(
        s.0.join("CLAUDE.md"),
        md.replace("FIRST ACTION", "FIRST STEP"),
    )
    .unwrap();

    let found = findings_for(&s.0, "amis-test", "version_skew");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0]["fix_command"], "writ init -y");
    assert_eq!(found[0]["paths"][0], "CLAUDE.md");
    run_fix_as(&s.0, "amis-test", "writ init -y");
    assert!(findings_for(&s.0, "amis-test", "version_skew").is_empty());
}

/// version_skew (PATH): two distinct writ binaries on PATH; the fix
/// removes the shadowed one.
#[test]
fn version_skew_two_writs_on_path() {
    let s = Scratch::new("skew-path");
    clean_repo(&s.0);
    let (d1, d2) = (s.0.join("bin1"), s.0.join("bin2"));
    for d in [&d1, &d2] {
        fs::create_dir_all(d).unwrap();
        fs::copy(env!("CARGO_BIN_EXE_writ"), d.join("writ")).unwrap();
    }
    let path = std::env::join_paths([&d1, &d2]).unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_writ"))
            .args(["doctor", "--format", "json"])
            .current_dir(&s.0)
            .env("PATH", &path)
            .output()
            .unwrap()
    };
    let v = json(&run());
    let f = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["check"] == "version_skew")
        .cloned()
        .unwrap_or_else(|| panic!("{v}"));
    // Two copies writ cannot attribute to a package manager: no pasteable
    // command (finding 79), the user decides; never `rm`.
    assert!(f["fix_command"].is_null(), "{f}");
    assert_eq!(f["needs_human"], true);
    assert!(f["message"]
        .as_str()
        .unwrap()
        .contains("choose which to keep"));
    assert_eq!(f["severity"], "yellow");
}

/// stale_claim (session gone): the claim names a pid on this host that has
/// exited. Fix: force release.
#[test]
fn stale_claim_exited_holder_is_reported_and_released() {
    let s = Scratch::new("stale-gone");
    let repo = clean_repo(&s.0);
    ok_as(
        &s.0,
        "ghost",
        &["spec", "add", "--id", "g", "--title", "g", "--claim"],
    );
    let path = repo.writ_dir().join("specs").join("g.json");
    let mut spec: Spec = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert!(spec.claimed_host.is_some(), "claim records the host");
    let mut child = Command::new("true").spawn().unwrap();
    let dead = child.id();
    child.wait().unwrap();
    spec.claimed_pid = Some(dead);
    spec.claimed_pid_start = Some("Thu Jan  1 00:00:00 1970".into());
    fs::write(&path, serde_json::to_string_pretty(&spec).unwrap()).unwrap();

    let found = findings_for(&s.0, "amis-test", "stale_claim");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0]["message"].as_str().unwrap().contains("has exited"));
    assert_eq!(found[0]["fix_command"], "writ spec release g --force");
    run_fix_then_clean(&s.0, "writ spec release g --force");
}

/// stale_claim (idle): no process record and idle past the threshold.
#[test]
fn stale_claim_idle_past_threshold() {
    let s = Scratch::new("stale-idle");
    let repo = clean_repo(&s.0);
    ok_as(
        &s.0,
        "ghost",
        &["spec", "add", "--id", "g", "--title", "g", "--claim"],
    );
    let path = repo.writ_dir().join("specs").join("g.json");
    let mut spec: Spec = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    spec.claimed_pid = None;
    spec.claimed_host = None;
    let old = chrono::Utc::now() - chrono::Duration::minutes(500);
    spec.last_activity = old;
    spec.updated_at = old;
    fs::write(&path, serde_json::to_string_pretty(&spec).unwrap()).unwrap();
    let found = findings_for(&s.0, "amis-test", "stale_claim");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0]["message"].as_str().unwrap().contains("idle"));
}

/// A live holder (this test process's session) is never stale.
#[test]
fn live_claim_is_not_stale() {
    let s = Scratch::new("stale-live");
    clean_repo(&s.0);
    ok_as(
        &s.0,
        "me",
        &["spec", "add", "--id", "m", "--title", "m", "--claim"],
    );
    assert!(findings_for(&s.0, "me", "stale_claim").is_empty());
}

/// Latency: warm run on a repo with 40 seals stays inside the 300 ms budget
/// (debug build; release is several times faster).
#[test]
fn warm_run_is_inside_the_latency_budget() {
    let s = Scratch::new("latency");
    let repo = Repository::init(&s.0).unwrap();
    for i in 0..40 {
        fs::write(s.0.join(format!("f{}.txt", i % 7)), format!("{i}\n")).unwrap();
        seal(&repo, &format!("s{i}"));
    }
    writ(&s.0, &["doctor"]);
    let v = json(&writ(&s.0, &["doctor", "--format", "json"]));
    let ms = v["elapsed_ms"].as_u64().unwrap();
    assert!(ms < 300, "warm doctor took {ms} ms: {}", v["check_ms"]);
}

/// The context brief carries exactly one doctor line naming the tier.
#[test]
fn context_brief_has_one_doctor_line() {
    let s = Scratch::new("brief");
    clean_repo(&s.0);
    let out = writ(&s.0, &["context", "--format", "brief"]);
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().filter(|l| l.starts_with("doctor:")).collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert!(lines[0].contains(CLEAN), "{}", lines[0]);
}

/// Finding 80: a file under a git-ignored path that the newest (committed)
/// spec sealed can never be in HEAD; it is not stuck.
#[test]
fn committed_spec_seal_skips_git_ignored_paths() {
    let s = Scratch::new("stuck-ignored");
    git_writ_project(&s.0);
    fs::create_dir_all(s.0.join("results")).unwrap();
    ok_as(
        &s.0,
        "ann",
        &["spec", "add", "--id", "a", "--title", "a", "--claim"],
    );
    fs::write(s.0.join("results/.gitignore"), "*\n!.gitignore\n").unwrap();
    fs::write(s.0.join("results/out.txt"), "v1\n").unwrap();
    fs::write(s.0.join("kept.txt"), "k\n").unwrap();
    ok_as(
        &s.0,
        "ann",
        &[
            "seal",
            "-s",
            "v1",
            "--spec",
            "a",
            "--paths",
            "results/out.txt,results/.gitignore,kept.txt",
        ],
    );
    ok_as(&s.0, "ann", &["spec", "done", "a", "-s", "v1"]);
    ok_as(&s.0, "ann", &["finish", "-y", "--no-check"]);
    // Ignore results/ after the fact and drop it from HEAD: git can never
    // hold it again, so writ must not call it stuck.
    fs::write(s.0.join(".gitignore"), ".writ/\nresults/\n").unwrap();
    git(&s.0, &["rm", "-q", "-r", "--cached", "results"]);
    git(&s.0, &["commit", "-q", "-am", "ignore results"]);
    assert!(findings_for(&s.0, "ann", "committed_spec_seal").is_empty());
}

/// Finding 81: two findings for one agent with no open spec propose one
/// follow-up spec (fixed id) and two seals, each with `--spec`; pasting
/// both fixes in order clears both.
#[test]
fn one_follow_up_spec_for_several_findings() {
    let s = Scratch::new("one-follow-up");
    git_writ_project(&s.0);
    ok_as(
        &s.0,
        "ann",
        &["spec", "add", "--id", "a", "--title", "a", "--claim"],
    );
    fs::write(s.0.join("f.txt"), "v1\n").unwrap();
    ok_as(
        &s.0,
        "ann",
        &["seal", "-s", "v1", "--spec", "a", "--paths", "f.txt"],
    );
    ok_as(&s.0, "ann", &["spec", "done", "a", "-s", "v1"]);
    ok_as(&s.0, "ann", &["finish", "-y", "--no-check"]);
    fs::write(s.0.join("f.txt"), "v2\n").unwrap();
    git(&s.0, &["commit", "-q", "-am", "v2 by hand"]);
    fs::write(s.0.join("f.txt"), "v3\n").unwrap();
    ok_as(
        &s.0,
        "ann",
        &["spec", "add", "--id", "b", "--title", "b", "--claim"],
    );
    fs::write(s.0.join("b.txt"), "b\n").unwrap();
    ok_as(
        &s.0,
        "ann",
        &["seal", "-s", "b", "--spec", "b", "--paths", "b.txt"],
    );
    ok_as(&s.0, "ann", &["spec", "done", "b", "-s", "b"]);
    fs::write(s.0.join("stray.txt"), "stray\n").unwrap();

    let v = json(&writ_as(&s.0, "ann", &["doctor", "--format", "json"]));
    let fixes: Vec<String> = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["check"] == "committed_spec_seal" || f["check"] == "left_out")
        .map(|f| f["fix_command"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(fixes.len(), 2, "{v}");
    let all = fixes.join("\n");
    assert_eq!(all.matches("writ seal").count(), 2, "{all}");
    let id = format!("doctor-follow-up-{}", chrono::Utc::now().format("%Y%m%d"));
    // Finding 84: one spec id; each fix carries its own idempotent spec add.
    for f in &fixes {
        assert!(f.contains(&format!("--id {id} --claim")), "{all}");
    }
    for f in &fixes {
        assert!(
            f.contains(&format!("writ seal")) && f.contains(&format!("--spec {id}")),
            "{f}"
        );
    }
    // Reverse order: each fix stands alone.
    for f in fixes.iter().rev() {
        run_fix_as(&s.0, "ann", f);
    }
    assert_eq!(
        v_specs_with_prefix(&s.0, &id),
        1,
        "one follow-up spec, not one per finding"
    );
    assert!(findings_for(&s.0, "ann", "committed_spec_seal").is_empty());
    assert!(findings_for(&s.0, "ann", "left_out").is_empty());
}

/// Finding 82: `writ init -y` on an initialised project keeps
/// `.writ/config.toml` byte for byte (custom [doctor], codex = false,
/// initialized, baseline_ref, comments); a flag changes only its own key.
#[test]
fn init_rerun_keeps_custom_config() {
    let s = Scratch::new("rerun-config");
    git(&s.0, &["init", "-q"]);
    git(&s.0, &["config", "user.name", "t"]);
    git(&s.0, &["config", "user.email", "t@t"]);
    fs::write(s.0.join("a.txt"), "a\n").unwrap();
    git(&s.0, &["add", "-A"]);
    git(&s.0, &["commit", "-q", "-m", "a"]);
    assert!(writ(&s.0, &["init", "-y", "--no-codex", "--no-generic"])
        .status
        .success());
    let cfg = s.0.join(".writ/config.toml");
    let mut text = fs::read_to_string(&cfg).unwrap();
    text.push_str("\n# kept\n[doctor]\nallow_missing = [\"ae01d21c123e\"]\n");
    fs::write(&cfg, &text).unwrap();
    // HEAD moves after init; a rerun must not move baseline_ref.
    fs::write(s.0.join("b.txt"), "b\n").unwrap();
    git(&s.0, &["add", "-A"]);
    git(&s.0, &["commit", "-q", "-m", "b"]);

    let out = writ(&s.0, &["init", "-y"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(fs::read_to_string(&cfg).unwrap(), text);
    assert!(
        !s.0.join("AGENTS.md").exists(),
        "codex = false was honoured"
    );

    let out = writ(&s.0, &["init", "-y", "--output-format", "json"]);
    assert!(out.status.success(), "{out:?}");
    let after = fs::read_to_string(&cfg).unwrap();
    let changed: Vec<(&str, &str)> = text
        .lines()
        .zip(after.lines())
        .filter(|(a, b)| a != b)
        .collect();
    assert_eq!(
        changed,
        vec![("format = \"toon\"", "format = \"json\"")],
        "{after}"
    );
    assert_eq!(text.lines().count(), after.lines().count());
}

fn v_specs_with_prefix(root: &Path, prefix: &str) -> usize {
    fs::read_dir(root.join(".writ/specs"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .count()
}

/// Finding 85: two claimed specs and no --spec: one message naming both,
/// exit 1, nothing claims to be saved, nothing sealed.
#[test]
fn seal_with_two_claims_names_them_and_saves_nothing() {
    let s = Scratch::new("two-claims");
    clean_repo(&s.0);
    ok_as(
        &s.0,
        "ann",
        &["spec", "add", "--id", "x1", "--title", "x1", "--claim"],
    );
    ok_as(
        &s.0,
        "ann",
        &["spec", "add", "--id", "x2", "--title", "x2", "--claim"],
    );
    fs::write(s.0.join("n.txt"), "n\n").unwrap();
    let out = writ_as(&s.0, "ann", &["seal", "-s", "n", "--paths", "n.txt"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(
        err.contains("You hold 2 claimed specs (x1, x2); pass --spec <id>"),
        "{err}"
    );
    assert!(!err.contains("saved"), "{err}");
    assert!(!err.contains("No active spec"), "{err}");
}

/// Finding 84: `spec add --id X --claim` again by the holder exits 0.
#[test]
fn spec_add_with_id_is_idempotent_for_the_holder() {
    let s = Scratch::new("idem");
    clean_repo(&s.0);
    ok_as(&s.0, "ann", &["spec", "add", "t", "--id", "f1", "--claim"]);
    let again = ok_as(&s.0, "ann", &["spec", "add", "t", "--id", "f1", "--claim"]);
    assert!(again.contains("already exists, claimed by you"), "{again}");
    assert!(
        !writ_as(&s.0, "bob", &["spec", "add", "t", "--id", "f1", "--claim"])
            .status
            .success()
    );
}

/// Finding 86: every context format carries doctor near the top, and a
/// red doctor sets `next` to the first red fix.
#[test]
fn every_context_format_carries_doctor_and_red_sets_next() {
    let s = Scratch::new("ctx-doctor");
    let repo = clean_repo(&s.0);
    let v = json(&writ(&s.0, &["context", "--format", "json"]));
    assert_eq!(v["doctor"]["headline"], CLEAN, "{v}");
    let toon = writ(&s.0, &["context", "--format", "toon"]);
    let text = String::from_utf8_lossy(&toon.stdout);
    let pos = text.find("doctor:").expect("toon has doctor");
    assert!(pos < 400, "doctor near the top:\n{text}");
    let human =
        String::from_utf8_lossy(&writ(&s.0, &["context", "--format", "human"]).stdout).to_string();
    assert!(human.contains(CLEAN), "{human}");

    // Make doctor red: delete a sealed blob.
    let blob = repo.log().unwrap()[0].changes[0].new_hash.clone().unwrap();
    fs::remove_file(
        repo.writ_dir()
            .join("objects")
            .join(&blob[..2])
            .join(&blob[2..]),
    )
    .unwrap();
    let v = json(&writ(&s.0, &["context", "--format", "json"]));
    assert_eq!(v["doctor"]["findings"][0]["check"], "store_integrity");
    assert_eq!(v["recommended_action"]["action"], "doctor_fix", "{v}");
    assert!(v["recommended_action"]["message"]
        .as_str()
        .unwrap()
        .contains("writ repair"));
}

/// Finding 87: an agent's first seal claims the one unclaimed spec it
/// created, even with other agents' unclaimed specs around; two of its own
/// get the "pass --spec" message.
#[test]
fn first_seal_claims_the_agents_own_unclaimed_spec() {
    let s = Scratch::new("auto-claim");
    clean_repo(&s.0);
    ok_as(&s.0, "bob", &["spec", "add", "--id", "b1", "--title", "b1"]);
    ok_as(&s.0, "cat", &["spec", "add", "--id", "c1", "--title", "c1"]);
    ok_as(&s.0, "ann", &["spec", "add", "--id", "a1", "--title", "a1"]);
    fs::write(s.0.join("x.txt"), "x\n").unwrap();
    let out = ok_as(&s.0, "ann", &["seal", "-s", "x", "--paths", "x.txt"]);
    assert!(out.contains("claimed spec a1 (you created it)"), "{out}");
    let spec: Spec =
        serde_json::from_str(&fs::read_to_string(s.0.join(".writ/specs/a1.json")).unwrap())
            .unwrap();
    assert_eq!(spec.claimed_by.as_deref(), Some("ann"));
    assert_eq!(spec.sealed_by.len(), 1);

    ok_as(&s.0, "dan", &["spec", "add", "--id", "d1", "--title", "d1"]);
    ok_as(&s.0, "dan", &["spec", "add", "--id", "d2", "--title", "d2"]);
    fs::write(s.0.join("y.txt"), "y\n").unwrap();
    let out = writ_as(&s.0, "dan", &["seal", "-s", "y", "--paths", "y.txt"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        err.contains("You created 2 unclaimed specs (d1, d2); pass --spec <id>"),
        "{err}"
    );
}

/// A repo where `ghost` holds spec `g` from a process that has exited, and
/// `ann` holds `a`: doctor reports one yellow stale_claim.
fn repo_with_ghost_claim(root: &Path) {
    let repo = clean_repo(root);
    ok_as(
        root,
        "ann",
        &["spec", "add", "--id", "a", "--title", "a", "--claim"],
    );
    ok_as(
        root,
        "ghost",
        &["spec", "add", "--id", "g", "--title", "g", "--claim"],
    );
    let path = repo.writ_dir().join("specs").join("g.json");
    let mut spec: Spec = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let mut child = Command::new("true").spawn().unwrap();
    spec.claimed_pid = Some(child.id());
    child.wait().unwrap();
    spec.claimed_pid_start = Some("Thu Jan  1 00:00:00 1970".into());
    fs::write(&path, serde_json::to_string_pretty(&spec).unwrap()).unwrap();
}

fn assert_notice(out: &str) {
    assert!(out.contains("doctor: fast checks: 1 finding(s)"), "{out}");
    assert!(out.contains("[yellow] stale_claim"), "{out}");
    assert!(out.contains("fix: writ spec release g --force"), "{out}");
}

/// Finding 90: seal prints doctor after its output when not clean, and
/// nothing when clean.
#[test]
fn seal_prints_doctor_when_not_clean() {
    let s = Scratch::new("notice-seal");
    clean_repo(&s.0);
    ok_as(
        &s.0,
        "ann",
        &["spec", "add", "--id", "a", "--title", "a", "--claim"],
    );
    fs::write(s.0.join("c.txt"), "c\n").unwrap();
    let clean = ok_as(
        &s.0,
        "ann",
        &["seal", "-s", "c", "--spec", "a", "--paths", "c.txt"],
    );
    assert!(!clean.contains("doctor:"), "{clean}");

    let s = Scratch::new("notice-seal-red");
    repo_with_ghost_claim(&s.0);
    fs::write(s.0.join("x.txt"), "x\n").unwrap();
    let out = ok_as(
        &s.0,
        "ann",
        &["seal", "-s", "x", "--spec", "a", "--paths", "x.txt"],
    );
    assert_notice(&out);
    assert!(
        out.find("sealed").unwrap() < out.find("doctor:").unwrap(),
        "{out}"
    );
}

/// Finding 90: spec done prints doctor when not clean.
#[test]
fn spec_done_prints_doctor_when_not_clean() {
    let s = Scratch::new("notice-done");
    repo_with_ghost_claim(&s.0);
    fs::write(s.0.join("x.txt"), "x\n").unwrap();
    ok_as(
        &s.0,
        "ann",
        &["seal", "-s", "x", "--spec", "a", "--paths", "x.txt"],
    );
    let out = ok_as(&s.0, "ann", &["spec", "done", "a", "-s", "done"]);
    assert_notice(&out);
}

/// Finding 90: status prints doctor (human) and carries it (json) when not
/// clean; neither when clean.
#[test]
fn status_carries_doctor_when_not_clean() {
    let s = Scratch::new("notice-status-clean");
    clean_repo(&s.0);
    let human = ok_as(&s.0, "ann", &["status", "--format", "human"]);
    assert!(!human.contains("doctor:"), "{human}");
    let v = json(&writ_as(&s.0, "ann", &["status", "--format", "json"]));
    assert!(v.get("doctor").is_none(), "{v}");

    let s = Scratch::new("notice-status");
    repo_with_ghost_claim(&s.0);
    assert_notice(&ok_as(&s.0, "ann", &["status", "--format", "human"]));
    let v = json(&writ_as(&s.0, "ann", &["status", "--format", "json"]));
    assert_eq!(v["doctor"]["findings"][0]["check"], "stale_claim", "{v}");
    assert_eq!(
        v["doctor"]["findings"][0]["fix_command"],
        "writ spec release g --force"
    );
}
