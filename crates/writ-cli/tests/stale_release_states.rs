//! `writ spec release <id> --stale` (finding 91, sprint 4 `stale-release`).
//! Assertions written before the code (retro rule 1); every test here is
//! ignored until finding 91 landed (Amis 64c4cc70eaf0); all live since.
//!
//! Contract (sprint 4 plan and Aubs's negative 2):
//! - `--stale` releases only when the holder is provably gone: same host and
//!   the recorded pid is dead, or the pid is alive but its start time does not
//!   match the recorded one (pid reuse).
//! - It refuses a live holder (same host, pid alive, start time matched),
//!   a holder on another host, and a claim with no pid/host evidence. Each
//!   refusal exits non-zero, leaves the claim untouched, names the missing
//!   or contradicting evidence, and says a human may use `--force`.
//! - It never behaves as `--force`: no `claim_force_released` event and no
//!   forced mark on the spec, on success or refusal.
//! - The human `--force` path still works on a live holder and is recorded
//!   as forced, in `.writ/security/events.jsonl` (`claim_force_released`)
//!   and on the spec (`release_forced: true`).

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

struct Project {
    root: PathBuf,
    home: PathBuf,
    bin: PathBuf,
}

impl Project {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "writ-stale-release-{tag}-{}-{nanos}",
            std::process::id()
        ));
        let (root, home, bin) = (base.join("repo"), base.join("home"), base.join("bin"));
        for d in [&root, &home, &bin] {
            fs::create_dir_all(d).unwrap();
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_writ"), bin.join("writ")).unwrap();
            // A process named `claude` (sh under that name): writ records the
            // nearest `claude` ancestor as the claim's pid.
            std::os::unix::fs::symlink("/bin/sh", bin.join("claude")).unwrap();
        }
        let p = Self { root, home, bin };
        p.git(&["init", "-q", "-b", "main"]);
        p.git(&["config", "user.name", "fixture"]);
        p.git(&["config", "user.email", "fixture@example.invalid"]);
        fs::write(p.root.join("README.md"), "# fixture\n").unwrap();
        p.git(&["add", "-A"]);
        p.git(&["commit", "-q", "-m", "init"]);
        p.ok("human", &["init", "-y"]);
        p.git(&["add", "-A"]);
        p.git(&["commit", "-q", "--no-verify", "-m", "writ init"]);
        p.ok(
            "human",
            &["spec", "add", "--id", "ghost", "--title", "ghost"],
        );
        p
    }

    fn cmd(&self, program: &str, agent: &str) -> Command {
        let mut c = Command::new(program);
        c.current_dir(&self.root)
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", self.bin.display()),
            )
            .env("HOME", &self.home)
            .env("TMPDIR", std::env::temp_dir())
            .env("WRIT_AGENT_ID", agent)
            .env("WRIT_EXCLUDES_FILE", self.home.join("no-global-excludes"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null());
        c
    }

    fn writ(&self, agent: &str, args: &[&str]) -> Output {
        self.cmd(self.bin.join("writ").to_str().unwrap(), agent)
            .args(args)
            .output()
            .unwrap()
    }

    fn ok(&self, agent: &str, args: &[&str]) -> Output {
        let out = self.writ(agent, args);
        assert!(out.status.success(), "writ {args:?}: {}", text(&out));
        out
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd("git", "human").args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", text(&out));
    }

    fn spec_json(&self) -> Value {
        serde_json::from_str(&fs::read_to_string(self.root.join(".writ/specs/ghost.json")).unwrap())
            .unwrap()
    }

    fn set_spec(&self, edits: &[(&str, Value)]) {
        let path = self.root.join(".writ/specs/ghost.json");
        let mut v = self.spec_json();
        for (k, val) in edits {
            v[*k] = val.clone();
        }
        fs::write(path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    }

    fn events(&self) -> Vec<Value> {
        fs::read_to_string(self.root.join(".writ/security/events.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// A third session claims `ghost` and exits: same host, dead pid.
    fn claim_by_exited_session(&self) -> u64 {
        let out = self
            .cmd(self.bin.join("claude").to_str().unwrap(), "ghost-session")
            .args(["-c", "writ spec claim ghost --agent ghost-session && :"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out));
        let pid = self.spec_json()["claimed_pid"]
            .as_u64()
            .expect("claim recorded no pid");
        assert!(!pid_alive(pid), "fixture: pid {pid} still alive");
        pid
    }

    /// A session claims `ghost` and stays alive until killed.
    fn claim_by_live_session(&self) -> LiveSession {
        let child = self
            .cmd(self.bin.join("claude").to_str().unwrap(), "live-session")
            .args([
                "-c",
                "writ spec claim ghost --agent live-session && sleep 120",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.spec_json()["claimed_by"] != "live-session" {
            assert!(Instant::now() < deadline, "live session never claimed");
            std::thread::sleep(Duration::from_millis(50));
        }
        let pid = self.spec_json()["claimed_pid"].as_u64().expect("no pid");
        assert!(pid_alive(pid), "fixture: live holder pid {pid} is dead");
        LiveSession(child)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        if std::env::var_os("WRIT_KEEP_FIXTURES").is_none() {
            let _ = fs::remove_dir_all(self.root.parent().unwrap());
        }
    }
}

struct LiveSession(Child);

impl Drop for LiveSession {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn pid_alive(pid: u64) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// True when any key named `forced` or `release_forced` anywhere in `v` is
/// `true`. The spec marker for a human `--force` release is `release_forced`
/// (CC decision, sprint 4), distinct from seals' `forced`.
fn has_forced_mark(v: &Value) -> bool {
    match v {
        Value::Object(m) => m.iter().any(|(k, x)| {
            ((k == "forced" || k == "release_forced") && x == &Value::Bool(true))
                || has_forced_mark(x)
        }),
        Value::Array(a) => a.iter().any(has_forced_mark),
        _ => false,
    }
}

fn force_events(p: &Project) -> Vec<Value> {
    p.events()
        .into_iter()
        .filter(|e| e["event_type"] == "claim_force_released")
        .collect()
}

/// The common refusal shape: non-zero, claim untouched, `--force` named as
/// the human's option, never treated as a force release.
fn assert_stale_refused(p: &Project, holder: &str, out: &Output) -> String {
    let all = text(out);
    assert!(!out.status.success(), "--stale released:\n{all}");
    let spec = p.spec_json();
    assert_eq!(
        spec["claimed_by"], holder,
        "claim changed on refusal:\n{all}"
    );
    assert!(
        all.contains("--force"),
        "refusal must say a human may use --force:\n{all}"
    );
    assert!(force_events(p).is_empty(), "--stale wrote a force event");
    assert!(
        !has_forced_mark(&spec),
        "--stale marked the spec forced: {spec:#}"
    );
    all
}

#[test]
fn stale_release_frees_a_claim_whose_holder_pid_is_dead_on_this_host() {
    let p = Project::new("dead");
    p.claim_by_exited_session();
    let out = p.writ(
        "other",
        &["spec", "release", "ghost", "--stale", "--agent", "other"],
    );
    assert!(out.status.success(), "{}", text(&out));
    let spec = p.spec_json();
    assert!(spec["claimed_by"].is_null(), "still claimed: {spec:#}");
    assert!(force_events(&p).is_empty(), "--stale wrote a force event");
    assert!(
        !has_forced_mark(&spec),
        "--stale marked the spec forced: {spec:#}"
    );
    // The new holder can claim it without --force.
    p.ok("other", &["spec", "claim", "ghost", "--agent", "other"]);
}

#[test]
fn stale_release_frees_a_claim_whose_pid_was_reused() {
    // The recorded pid is alive (this test process) but its start time is not
    // the one recorded: the original holder is gone.
    let p = Project::new("reused");
    p.claim_by_exited_session();
    p.set_spec(&[
        ("claimed_pid", Value::from(std::process::id() as u64)),
        ("claimed_pid_start", Value::from("Thu Jan  1 00:00:00 1970")),
    ]);
    let out = p.writ(
        "other",
        &["spec", "release", "ghost", "--stale", "--agent", "other"],
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(p.spec_json()["claimed_by"].is_null());
}

#[test]
fn stale_release_refuses_a_live_holder_naming_its_pid() {
    let p = Project::new("live");
    let _live = p.claim_by_live_session();
    let pid = p.spec_json()["claimed_pid"].as_u64().unwrap();
    let out = p.writ(
        "other",
        &["spec", "release", "ghost", "--stale", "--agent", "other"],
    );
    let all = assert_stale_refused(&p, "live-session", &out);
    assert!(
        all.contains(&pid.to_string()),
        "refusal does not name pid {pid}:\n{all}"
    );
    assert!(
        all.to_lowercase().contains("alive") || all.to_lowercase().contains("running"),
        "refusal does not say the holder is alive:\n{all}"
    );
}

#[test]
fn stale_release_refuses_a_holder_on_another_host_naming_the_host() {
    let p = Project::new("foreign");
    p.claim_by_exited_session();
    p.set_spec(&[("claimed_host", Value::from("other-host.invalid"))]);
    let out = p.writ(
        "other",
        &["spec", "release", "ghost", "--stale", "--agent", "other"],
    );
    let all = assert_stale_refused(&p, "ghost-session", &out);
    assert!(
        all.contains("other-host.invalid"),
        "refusal does not name the host:\n{all}"
    );
}

#[test]
fn stale_release_refuses_a_claim_with_no_evidence_naming_what_is_missing() {
    // A claim written by an older writ: no pid, no start time, no host.
    let p = Project::new("noevidence");
    p.claim_by_exited_session();
    p.set_spec(&[
        ("claimed_pid", Value::Null),
        ("claimed_pid_start", Value::Null),
        ("claimed_host", Value::Null),
    ]);
    let out = p.writ(
        "other",
        &["spec", "release", "ghost", "--stale", "--agent", "other"],
    );
    let all = assert_stale_refused(&p, "ghost-session", &out);
    assert!(
        all.contains("pid"),
        "refusal does not name the missing pid:\n{all}"
    );
    assert!(
        all.contains("host"),
        "refusal does not name the missing host:\n{all}"
    );
}

#[test]
fn stale_release_on_a_live_holder_is_not_force_even_when_idle_for_hours() {
    // Idle time is not evidence: a live holder idle three hours is refused.
    let p = Project::new("live-idle");
    let _live = p.claim_by_live_session();
    let old = "2026-01-01T00:00:00.000000Z";
    p.set_spec(&[
        ("updated_at", Value::from(old)),
        ("last_activity", Value::from(old)),
        ("claimed_at", Value::from(old)),
    ]);
    let out = p.writ(
        "other",
        &["spec", "release", "ghost", "--stale", "--agent", "other"],
    );
    assert_stale_refused(&p, "live-session", &out);
}

#[test]
fn human_force_release_of_a_live_holder_works_and_is_recorded_as_forced() {
    let p = Project::new("force");
    let _live = p.claim_by_live_session();
    let out = p.writ(
        "human",
        &["spec", "release", "ghost", "--force", "--agent", "human"],
    );
    assert!(out.status.success(), "{}", text(&out));
    let spec = p.spec_json();
    assert!(
        spec["claimed_by"].is_null(),
        "--force did not release: {spec:#}"
    );
    let ev = force_events(&p);
    assert_eq!(
        ev.len(),
        1,
        "expected one claim_force_released event: {ev:#?}"
    );
    assert!(ev[0]["details"].as_str().unwrap_or("").contains("ghost"));
    assert_eq!(ev[0]["agent_id"], "human");
    assert_eq!(
        spec["release_forced"],
        Value::Bool(true),
        "spec carries no release_forced record: {spec:#}"
    );
}

/// Harness self-check (always on): the live-session fixture really holds a
/// live pid and the exited-session fixture a dead one, on this build.
#[test]
fn harness_session_fixtures_hold_live_and_dead_pids() {
    let p = Project::new("self");
    let dead = p.claim_by_exited_session();
    assert!(!pid_alive(dead));
    p.ok(
        "human",
        &["spec", "release", "ghost", "--force", "--agent", "human"],
    );
    let _live = p.claim_by_live_session();
    assert!(pid_alive(p.spec_json()["claimed_pid"].as_u64().unwrap()));
}
