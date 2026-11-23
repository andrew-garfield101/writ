//! Sprint 9 (0.4.6) doctor items, end to end through the binary:
//! 213 (a hook killed before it finished is a doctor finding) and 215 (the
//! watch tip leaves `writ init` and becomes a doctor info line, said only
//! when more than one identity holds a live claim and no watch runs).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use writ_core::seal::{AgentIdentity, AgentType, TaskStatus, Verification};
use writ_core::spec::Spec;
use writ_core::Repository;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("writ-d046-{tag}-{}-{nanos}", std::process::id()));
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
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("not JSON ({e}):\n{}", String::from_utf8_lossy(&out.stdout)))
}

fn clean_repo(root: &Path) -> Repository {
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
    repo
}

fn findings_of<'a>(v: &'a serde_json::Value, check: &str) -> Vec<&'a serde_json::Value> {
    v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["check"] == check)
        .collect()
}

/// A pid no process holds (above every platform's pid_max).
const DEAD_PID: u32 = 4_000_001;

fn ts(ago_secs: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::seconds(ago_secs))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn start(pid: u32, event: &str, tool: &str, agent_type: &str, at: &str) -> String {
    format!(
        r#"{{"event":"{event}","outcome":"start","session":"s-1","agent_id":"a-1","agent_type":"{agent_type}","tool":"{tool}","tool_use_id":"t","ts":"{at}","ms":0,"pid":{pid}}}"#
    )
}

fn rec(pid: u32, event: &str, outcome: &str, extra: &str, at: &str) -> String {
    format!(r#"{{"event":"{event}","outcome":"{outcome}"{extra},"ts":"{at}","ms":5,"pid":{pid}}}"#)
}

fn write_log(root: &Path, lines: &[String]) {
    let mut text = lines.join("\n");
    text.push('\n');
    fs::write(root.join(".writ").join("hooks.log"), text).unwrap();
}

// ── 213 ────────────────────────────────────────────────────────────────

/// The 213 shape: a Post that logged `attributed` and then nothing (Claude
/// Code killed it at its timeout); the same pid's next firing closes it.
/// A complete firing beside it is not reported.
#[test]
fn f213_doctor_reports_a_hook_killed_before_it_finished() {
    let s = Scratch::new("killed");
    clean_repo(&s.0);
    let killed_at = ts(120);
    write_log(
        &s.0,
        &[
            start(DEAD_PID, "PostToolUse", "Edit", "amis", &killed_at),
            rec(
                DEAD_PID,
                "PostToolUse",
                "attributed",
                r#","tool":"Edit","path":"a.txt","agent":"amis""#,
                &ts(119),
            ),
            start(DEAD_PID, "Stop", "", "amis", &ts(60)),
            rec(DEAD_PID, "Stop", "end", "", &ts(60)),
            start(DEAD_PID + 1, "PostToolUse", "Write", "bri", &ts(50)),
            rec(
                DEAD_PID + 1,
                "PostToolUse",
                "sealed",
                r#","path":"b.txt""#,
                &ts(50),
            ),
            rec(DEAD_PID + 1, "PostToolUse", "end", "", &ts(50)),
        ],
    );

    let out = writ(&s.0, &["doctor", "--format", "json"]);
    let v = json(&out);
    let killed = findings_of(&v, "hook_killed");
    assert_eq!(killed.len(), 1, "{v:#}");
    let f = killed[0];
    assert_eq!(f["severity"], "yellow");
    let msg = f["message"].as_str().unwrap();
    for want in [
        "hook killed before it finished",
        "PostToolUse",
        "Edit",
        "amis",
        &killed_at[..19],
        "attributed",
    ] {
        assert!(msg.contains(want), "missing {want:?}: {msg}");
    }
    assert_eq!(f["paths"], serde_json::json!(["a.txt"]));
    assert!(v["checks_run"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c == "hook_killed"));
    assert_eq!(out.status.code(), Some(1), "a finding is not clean");

    let human = String::from_utf8_lossy(&writ(&s.0, &["doctor"]).stdout).to_string();
    assert!(
        human.contains("hook_killed: hook killed before it finished"),
        "{human}"
    );
}

/// Ruling 213 (CC): the fix is the seal that never happened, for the
/// firing's own identity only (not any agent, not a human).
#[test]
fn f213_killed_hook_fix_is_the_missing_seal_for_its_agent() {
    let s = Scratch::new("fix");
    clean_repo(&s.0);
    write_log(
        &s.0,
        &[
            start(DEAD_PID, "PostToolUse", "Bash", "", &ts(120)),
            rec(
                DEAD_PID,
                "PostToolUse",
                "attributed",
                r#","path":"a.txt","agent":"web-builder""#,
                &ts(119),
            ),
            rec(
                DEAD_PID,
                "PostToolUse",
                "attributed",
                r#","path":"dir name/b.txt","agent":"web-builder""#,
                &ts(119),
            ),
            start(DEAD_PID, "Stop", "", "", &ts(60)),
            rec(DEAD_PID, "Stop", "end", "", &ts(60)),
        ],
    );
    let v = json(&writ(&s.0, &["doctor", "--format", "json"]));
    let killed = findings_of(&v, "hook_killed");
    assert_eq!(killed.len(), 1, "{v:#}");
    let f = killed[0];
    assert_eq!(
        f["fix_command"],
        "writ seal --paths 'a.txt,dir name/b.txt' --auto --agent web-builder"
    );
    assert_eq!(f["needs_human"], false);
    assert_eq!(f["anyone_may_fix"], false, "only the firing's agent: {f:#}");
    assert!(
        f["message"]
            .as_str()
            .unwrap()
            .contains("for 'web-builder' only"),
        "{f:#}"
    );
    let human = String::from_utf8_lossy(&writ(&s.0, &["doctor"]).stdout).to_string();
    assert!(
        human
            .contains("fix:   writ seal --paths 'a.txt,dir name/b.txt' --auto --agent web-builder"),
        "{human}"
    );
    assert!(
        human.contains("only the agent or person the message names"),
        "{human}"
    );
}

/// A killed firing that named no path (a Stop sweep) has nothing to paste.
#[test]
fn f213_killed_hook_with_no_paths_has_no_fix() {
    let s = Scratch::new("nopaths");
    clean_repo(&s.0);
    write_log(
        &s.0,
        &[
            start(DEAD_PID, "Stop", "", "amis", &ts(120)),
            start(DEAD_PID, "Stop", "", "amis", &ts(60)),
            rec(DEAD_PID, "Stop", "end", "", &ts(60)),
        ],
    );
    let v = json(&writ(&s.0, &["doctor", "--format", "json"]));
    let killed = findings_of(&v, "hook_killed");
    assert_eq!(killed.len(), 1, "{v:#}");
    let f = killed[0];
    assert!(f["fix_command"].is_null(), "{f:#}");
    assert!(f["paths"].as_array().unwrap().is_empty());
    let msg = f["message"].as_str().unwrap();
    assert!(
        msg.contains("named no path") && msg.contains("no seal to paste"),
        "{msg}"
    );
}

/// A start with no end whose pid is alive is a hook still running, not a
/// killed one (CC's rule on top of the kit's).
#[test]
fn f213_a_live_pid_is_running_not_killed() {
    let s = Scratch::new("running");
    clean_repo(&s.0);
    let me = std::process::id();
    write_log(
        &s.0,
        &[
            start(DEAD_PID, "Stop", "", "bri", &ts(30)),
            rec(DEAD_PID, "Stop", "end", "", &ts(30)),
            start(me, "PostToolUse", "Edit", "amis", &ts(2)),
            rec(
                me,
                "PostToolUse",
                "attributed",
                r#","path":"a.txt""#,
                &ts(1),
            ),
        ],
    );
    let v = json(&writ(&s.0, &["doctor", "--format", "json"]));
    assert!(findings_of(&v, "hook_killed").is_empty(), "{v:#}");
    assert_eq!(v["clean"], true, "{v:#}");
}

/// The kit's two "cannot tell" shapes report nothing: a log with starts but
/// no end record at all (a build between ruling 213's two records), and a
/// log from before 0.4.5 (no starts).
#[test]
fn f213_logs_that_cannot_tell_report_nothing() {
    let s = Scratch::new("cannot-tell");
    clean_repo(&s.0);
    write_log(
        &s.0,
        &[
            start(DEAD_PID, "PostToolUse", "Edit", "amis", &ts(30)),
            start(DEAD_PID + 1, "PostToolUse", "Edit", "amis", &ts(20)),
        ],
    );
    let v = json(&writ(&s.0, &["doctor", "--format", "json"]));
    assert!(findings_of(&v, "hook_killed").is_empty(), "{v:#}");

    write_log(
        &s.0,
        &[
            r#"{"event":"PostToolUse","tool":"Edit","path":"a.txt","outcome":"sealed","ms":12}"#
                .to_string(),
        ],
    );
    let v = json(&writ(&s.0, &["doctor", "--format", "json"]));
    assert!(findings_of(&v, "hook_killed").is_empty(), "{v:#}");
}

/// A killed firing before the last `writ finish` belongs to a range that is
/// already committed: not reported (so the finding clears at a finish).
#[test]
fn f213_a_killed_hook_before_the_last_finish_is_not_reported() {
    let s = Scratch::new("finished");
    let repo = clean_repo(&s.0);
    let mut done = Spec::new("old".into(), "old".into(), String::new());
    done.committed_at = Some(chrono::Utc::now() - chrono::Duration::seconds(60));
    repo.add_spec(&done).unwrap();
    write_log(
        &s.0,
        &[
            start(DEAD_PID, "PostToolUse", "Edit", "amis", &ts(600)),
            start(DEAD_PID, "Stop", "", "amis", &ts(30)),
            rec(DEAD_PID, "Stop", "end", "", &ts(30)),
        ],
    );
    let v = json(&writ(&s.0, &["doctor", "--format", "json"]));
    assert!(findings_of(&v, "hook_killed").is_empty(), "{v:#}");

    // The same log with the killed firing after the finish is reported.
    write_log(
        &s.0,
        &[
            start(DEAD_PID, "PostToolUse", "Edit", "amis", &ts(40)),
            start(DEAD_PID, "Stop", "", "amis", &ts(30)),
            rec(DEAD_PID, "Stop", "end", "", &ts(30)),
        ],
    );
    let v = json(&writ(&s.0, &["doctor", "--format", "json"]));
    assert_eq!(findings_of(&v, "hook_killed").len(), 1, "{v:#}");
}

/// Doctor is read only: the killed-hook check writes nothing under .writ.
#[test]
fn f213_the_check_writes_nothing() {
    let s = Scratch::new("readonly");
    clean_repo(&s.0);
    write_log(
        &s.0,
        &[
            start(DEAD_PID, "PostToolUse", "Edit", "amis", &ts(40)),
            start(DEAD_PID, "Stop", "", "amis", &ts(30)),
            rec(DEAD_PID, "Stop", "end", "", &ts(30)),
        ],
    );
    let log = s.0.join(".writ").join("hooks.log");
    let before = fs::read(&log).unwrap();
    let v = json(&writ(&s.0, &["doctor", "--format", "json"]));
    assert_eq!(findings_of(&v, "hook_killed").len(), 1, "{v:#}");
    assert_eq!(fs::read(&log).unwrap(), before);
}

// ── 215 ────────────────────────────────────────────────────────────────

const OLD_TIP: &str = "to monitor agent activity";

/// A git repository with a scratch HOME, as `writ init -y` sees a project.
fn git_project(root: &Path) -> PathBuf {
    let home = root.join("home");
    let proj = root.join("proj");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&proj).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.name", "fixture"],
        &["config", "user.email", "fixture@example.invalid"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(&proj)
            .status()
            .unwrap()
            .success());
    }
    fs::write(proj.join("README.md"), "# fixture\n").unwrap();
    proj
}

fn writ_home(proj: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_writ"))
        .args(args)
        .current_dir(proj)
        .env("HOME", proj.parent().unwrap().join("home"))
        .env("WRIT_AGENT_ID", "amis-test")
        .env_remove("WRIT_FORMAT")
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// `writ init -y` in a Claude Code project no longer prints the watch tip,
/// and the config.toml it writes no longer tells people to run watch.
#[test]
fn f215_init_prints_no_watch_tip() {
    let s = Scratch::new("init");
    let proj = git_project(&s.0);
    let out = writ_home(&proj, &["init", "-y"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("Claude Code integration configured"),
        "{text}"
    );
    assert!(!text.contains(OLD_TIP), "{text}");
    assert!(!text.contains("writ watch"), "{text}");
    let config = fs::read_to_string(proj.join(".writ").join("config.toml")).unwrap();
    assert!(!config.contains(OLD_TIP), "{config}");

    // The rerun prints no tip either.
    let again = writ_home(&proj, &["init", "-y"]);
    assert!(!String::from_utf8_lossy(&again.stdout).contains("writ watch"));
}

/// `writ plan` (the second place the tip lived) no longer prints it.
#[test]
fn f215_plan_prints_no_watch_tip() {
    let s = Scratch::new("plan");
    let proj = git_project(&s.0);
    assert!(writ_home(&proj, &["init", "-y"]).status.success());
    let out = writ_home(&proj, &["plan", "Task A", "Task B"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("specs created"), "{text}");
    assert!(!text.contains("writ watch"), "{text}");
}

/// A spec claimed by `holder` whose recorded holder process is this test
/// (alive), as a hook or `spec claim` records it.
fn live_claim(repo: &Repository, id: &str, holder: &str) {
    let me = std::process::id();
    let table = writ_core::agent::session::process_table().expect("process table");
    let start = table.iter().find(|p| p.pid == me).unwrap().start.clone();
    let mut spec = Spec::new(id.into(), id.into(), String::new());
    spec.claimed_by = Some(holder.into());
    spec.claimed_pid = Some(me);
    spec.claimed_pid_start = Some(start);
    spec.claimed_host = writ_core::agent::session::host_name();
    repo.add_spec(&spec).unwrap();
}

fn info_lines(v: &serde_json::Value) -> Vec<String> {
    v["info"]
        .as_array()
        .map(|a| a.iter().map(|x| x.as_str().unwrap().to_string()).collect())
        .unwrap_or_default()
}

/// Two identities with live claims and no watch: doctor says watch is not
/// running, as an info line (not a finding: clean stays clean).
#[cfg(unix)]
#[test]
fn f215_doctor_says_watch_is_not_running_with_two_live_identities() {
    let s = Scratch::new("watch-two");
    let repo = clean_repo(&s.0);
    live_claim(&repo, "sa", "amis");
    live_claim(&repo, "sb", "bri");
    let out = writ(&s.0, &["doctor", "--format", "json"]);
    let v = json(&out);
    let info = info_lines(&v);
    assert_eq!(info.len(), 1, "{v:#}");
    for want in ["writ watch is not running", "2 identities", "amis", "bri"] {
        assert!(info[0].contains(want), "missing {want:?}: {}", info[0]);
    }
    assert_eq!(v["clean"], true, "an info line is not a finding: {v:#}");
    assert_eq!(out.status.code(), Some(0));
    let human = String::from_utf8_lossy(&writ(&s.0, &["doctor"]).stdout).to_string();
    assert!(
        human.contains("[info] watch: writ watch is not running"),
        "{human}"
    );
}

/// One live identity (twice), or a dead holder: no watch line.
#[cfg(unix)]
#[test]
fn f215_one_live_identity_says_nothing_about_watch() {
    let s = Scratch::new("watch-one");
    let repo = clean_repo(&s.0);
    live_claim(&repo, "sa", "amis");
    live_claim(&repo, "sb", "amis");
    let mut gone = Spec::new("sc".into(), "sc".into(), String::new());
    gone.claimed_by = Some("bri".into());
    gone.claimed_pid = Some(DEAD_PID);
    gone.claimed_pid_start = Some("Mon Nov 23 10:00:00 2026".into());
    gone.claimed_host = writ_core::agent::session::host_name();
    repo.add_spec(&gone).unwrap();
    let v = json(&writ(&s.0, &["doctor", "--format", "json"]));
    assert!(info_lines(&v).is_empty(), "{v:#}");
}

/// A running watch (its pid in `.writ/watch.pid` is alive): no watch line.
#[cfg(unix)]
#[test]
fn f215_a_running_watch_says_nothing() {
    let s = Scratch::new("watch-running");
    let repo = clean_repo(&s.0);
    live_claim(&repo, "sa", "amis");
    live_claim(&repo, "sb", "bri");
    fs::write(
        s.0.join(".writ").join("watch.pid"),
        std::process::id().to_string(),
    )
    .unwrap();
    let v = json(&writ(&s.0, &["doctor", "--format", "json"]));
    assert!(info_lines(&v).is_empty(), "{v:#}");
}

/// Finding 210: a path git tracks that a hand `.writignore` line hides is
/// the one remaining disagreement between writ and git, by the user's
/// choice: one info line with the count, never a finding. A path only
/// `.gitignore` lists is tracked by writ, so it gets no line.
#[test]
fn f210_doctor_names_tracked_paths_a_hand_writignore_line_hides() {
    let s = Scratch::new("tracked-hidden");
    let proj = git_project(&s.0);
    let git = |args: &[&str]| {
        assert!(Command::new("git")
            .args(args)
            .current_dir(&proj)
            .status()
            .unwrap()
            .success());
    };
    for (rel, body) in [
        ("notes/a.md", "a\n"),
        ("notes/b.md", "b\n"),
        ("tools/bump.sh", "echo\n"),
    ] {
        fs::create_dir_all(proj.join(rel).parent().unwrap()).unwrap();
        fs::write(proj.join(rel), body).unwrap();
    }
    fs::write(proj.join(".gitignore"), "tools/\n").unwrap();
    git(&["add", "README.md", ".gitignore", "notes"]);
    git(&["add", "-f", "tools/bump.sh"]);
    git(&["commit", "-q", "-m", "base"]);
    assert!(writ_home(&proj, &["init", "-y"]).status.success());

    // tools/ comes from .gitignore (and init's import): no line.
    let v = json(&writ_home(&proj, &["doctor", "--format", "json"]));
    assert!(
        info_lines(&v).iter().all(|l| !l.contains("git tracks")),
        "{v:#}"
    );

    // A hand line hiding notes/: one info line naming the count.
    let writignore = fs::read_to_string(proj.join(".writignore")).unwrap();
    fs::write(
        proj.join(".writignore"),
        format!("{writignore}\n# mine\nnotes/\n"),
    )
    .unwrap();
    let v = json(&writ_home(&proj, &["doctor", "--format", "json"]));
    let lines: Vec<String> = info_lines(&v)
        .into_iter()
        .filter(|l| l.contains("git tracks"))
        .collect();
    assert_eq!(lines.len(), 1, "{v:#}");
    for want in [
        "2 path(s) git tracks",
        "notes/a.md",
        "notes/b.md",
        ".writignore",
    ] {
        assert!(lines[0].contains(want), "missing {want:?}: {}", lines[0]);
    }
    assert!(!lines[0].contains("tools/bump.sh"), "{}", lines[0]);
    assert!(
        findings_of(&v, "tracked_hidden").is_empty(),
        "an info line, never a finding: {v:#}"
    );
}
