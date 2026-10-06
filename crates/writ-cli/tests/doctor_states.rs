//! `writ doctor` bad-state harness (sprint 3, doctor-harness; 0.4.0 exit
//! criterion items 1 and 2).
//!
//! One scenario per fast-tier check. Every scenario runs the real binary in a
//! hermetic temp repo (git + `writ init -y`, cleared environment, PATH holding
//! only this build's `writ` plus the system tools) and makes four assertions:
//!
//! 1. **names it**: `writ doctor --format json` reports the state under the
//!    right check id, exit 1;
//! 2. **never clean while it exists**: doctor, run twice (the second run hits
//!    any cache keyed by newest seal id and the stat cache), never reports
//!    clean, and the `doctor:` line in `writ context --format brief` (when
//!    present) is not the clean headline;
//! 3. **the fix works verbatim**: the finding's `fix_command` is run through
//!    `sh -c` unchanged and exits 0;
//! 4. **clean after**: doctor exits 0 with zero findings.
//!
//! Each scenario also asserts doctor is clean *before* the state is created,
//! so a pass can never come from a check that fires on everything.
//!
//! Plus a seventh, a healthy repo with two active agents and pending work
//! (zero findings: false positives get doctor ignored), and the finish
//! scenarios: refuses on red naming the fix, `--force` proceeds, green runs
//! the clean gate.
//!
//! All live since doctor-core seal 4 (749dfb0ef53b).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime};

/// Clean headline (honesty condition, 0.4.0).
const CLEAN: &str = "fast checks clean; survival check not available until 0.4.1";

const CHECK_STORE_INTEGRITY: &str = "store_integrity";
const CHECK_STALE_CLAIM: &str = "stale_claim";
const CHECK_COMMITTED_SPEC_SEAL: &str = "committed_spec_seal";
const CHECK_UNSEALED_AT_RISK: &str = "unsealed_at_risk";
const CHECK_VERSION_SKEW: &str = "version_skew";
const CHECK_LEFT_OUT: &str = "left_out";

// ---------------------------------------------------------------------------
// Hermetic project
// ---------------------------------------------------------------------------

struct Project {
    root: PathBuf,
    home: PathBuf,
    /// A directory holding only this build's `writ`, first on PATH, so a
    /// fix_command that says `writ ...` runs the binary under test and the
    /// version-skew check never sees a second writ (brew's) by accident.
    bin: PathBuf,
}

impl Project {
    /// git repo with one commit, then `writ init -y` (managed CLAUDE.md block,
    /// hooks, baseline import), exactly as a user would start.
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "writ-doctor-states-{tag}-{}-{nanos}",
            std::process::id()
        ));
        let root = base.join("repo");
        let home = base.join("home");
        let bin = base.join("bin");
        for d in [&root, &home, &bin] {
            fs::create_dir_all(d).unwrap();
        }
        let exe = PathBuf::from(env!("CARGO_BIN_EXE_writ"));
        #[cfg(unix)]
        std::os::unix::fs::symlink(&exe, bin.join("writ")).unwrap();
        #[cfg(not(unix))]
        fs::copy(&exe, bin.join("writ.exe")).unwrap();
        let p = Self { root, home, bin };
        p.git_ok(&["init", "-q", "-b", "main"]);
        // finish reads identity through libgit2 config, not GIT_AUTHOR_*.
        p.git_ok(&["config", "user.name", "fixture"]);
        p.git_ok(&["config", "user.email", "fixture@example.invalid"]);
        p.write("README.md", "# fixture\n");
        p.write(".gitignore", "target/\n");
        p.git_ok(&["add", "-A"]);
        p.git_ok(&["commit", "-q", "-m", "init"]);
        p.ok("human", &["init", "-y"]);
        // Commit what init wrote so the baseline repo has nothing left out.
        p.git_ok(&["add", "-A"]);
        p.git_ok(&["commit", "-q", "--no-verify", "-m", "writ init"]);
        p
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn write(&self, rel: &str, content: &str) {
        let p = self.path(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, content).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.path(rel)).unwrap()
    }

    fn env_path(&self) -> String {
        format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", self.bin.display())
    }

    /// Cleared environment: no WRIT_*, no CLAUDECODE or session variables, no
    /// user git config, HOME in the scratch dir.
    fn cmd(&self, program: &str, agent: &str) -> Command {
        let mut c = Command::new(program);
        c.current_dir(&self.root)
            .env_clear()
            .env("PATH", self.env_path())
            .env("HOME", &self.home)
            .env("TMPDIR", std::env::temp_dir())
            .env("WRIT_AGENT_ID", agent)
            .env("WRIT_EXCLUDES_FILE", self.home.join("no-global-excludes"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
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
        assert!(
            out.status.success(),
            "writ {args:?} as {agent} failed ({:?}):\n{}\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn git(&self, args: &[&str]) -> Output {
        self.cmd("git", "human").args(args).output().unwrap()
    }

    fn git_ok(&self, args: &[&str]) -> String {
        let out = self.git(args);
        assert!(
            out.status.success(),
            "git {args:?} failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn head(&self) -> String {
        self.git_ok(&["rev-parse", "HEAD"]).trim().to_string()
    }

    /// `writ spec add --id <id> --title <id>` then claim as `agent`.
    fn spec(&self, id: &str, agent: &str) {
        self.ok(agent, &["spec", "add", "--id", id, "--title", id]);
        self.ok(agent, &["spec", "claim", id, "--agent", agent]);
    }

    fn seal(&self, agent: &str, spec: &str, paths: &str) {
        self.ok(
            agent,
            &[
                "seal", "-s", "work", "--agent", agent, "--spec", spec, "--paths", paths,
            ],
        );
    }

    fn set_mtime(&self, rel: &str, ago: Duration) {
        let f = fs::File::options()
            .write(true)
            .open(self.path(rel))
            .unwrap();
        f.set_modified(SystemTime::now() - ago).unwrap();
    }

    /// Rewrite top-level JSON fields of a spec file (timestamps only; used to
    /// age a claim without sleeping for two hours).
    fn age_spec(&self, id: &str, ago: Duration) {
        let p = self.path(&format!(".writ/specs/{id}.json"));
        let mut v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        let when = chrono_like(SystemTime::now() - ago);
        for key in ["updated_at", "last_activity", "claimed_at"] {
            if v.get(key).is_some() {
                v[key] = serde_json::Value::String(when.clone());
            }
        }
        fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    }

    fn set_spec_field(&self, id: &str, key: &str, value: serde_json::Value) {
        let p = self.path(&format!(".writ/specs/{id}.json"));
        let mut v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        v[key] = value;
        fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    }

    // -- doctor ------------------------------------------------------------

    fn doctor(&self, agent: &str) -> (Option<i32>, serde_json::Value) {
        let out = self.writ(agent, &["doctor", "--format", "json"]);
        let v = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "doctor --format json is not JSON ({e}):\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        assert_no_destructive_fixes(&v);
        (out.status.code(), v)
    }

    /// The `doctor:` line of `writ context --format brief`, if any.
    fn brief_doctor_line(&self, agent: &str) -> Option<String> {
        let out = self.writ(
            agent,
            &["context", "--format", "brief", "--for-agent", agent],
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find(|l| l.trim_start().starts_with("doctor:"))
            .map(|l| l.trim().to_string())
    }

    fn assert_clean(&self, agent: &str, when: &str) {
        let (code, v) = self.doctor(agent);
        assert_eq!(
            code,
            Some(0),
            "{when}: doctor exit {code:?}, expected 0 (clean):\n{v:#}"
        );
        assert_eq!(v["clean"], true, "{when}: {v:#}");
        assert!(
            v["findings"].as_array().is_some_and(|f| f.is_empty()),
            "{when}: findings on a clean repo:\n{v:#}"
        );
        assert_eq!(v["headline"], CLEAN, "{when}");
    }

    /// Assertions 1 and 2: named under `check`, never clean (twice), brief
    /// line not clean. Returns the first finding with that check id.
    fn assert_names(&self, agent: &str, check: &str) -> serde_json::Value {
        let mut first = None;
        for run in 1..=2 {
            let (code, v) = self.doctor(agent);
            assert_eq!(
                code,
                Some(1),
                "run {run}: state exists, doctor must exit 1:\n{v:#}"
            );
            assert_eq!(
                v["clean"], false,
                "run {run}: reported clean while the state exists:\n{v:#}"
            );
            assert_ne!(v["headline"], CLEAN, "run {run}");
            let hit = v["findings"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["check"] == check)
                .cloned()
                .unwrap_or_else(|| panic!("run {run}: no `{check}` finding:\n{v:#}"));
            let fix = hit["fix_command"].as_str().unwrap_or("");
            assert!(
                !fix.trim().is_empty(),
                "`{check}` finding has no fix_command:\n{hit:#}"
            );
            assert!(!hit["message"].as_str().unwrap_or("").is_empty());
            first.get_or_insert(hit);
        }
        let line = self
            .brief_doctor_line(agent)
            .unwrap_or_else(|| panic!("no doctor: line in the brief while `{check}` exists"));
        assert!(
            !line.contains(CLEAN),
            "context brief says clean while `{check}` exists: {line}"
        );
        assert!(
            line.contains(check),
            "brief doctor line does not name `{check}`: {line}"
        );
        // Finding 86: agents run plain `writ context` mid-run, so every
        // format carries the findings, not only the SessionStart brief.
        let out = self.writ(
            agent,
            &["context", "--format", "json", "--for-agent", agent],
        );
        let ctx: serde_json::Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("context --format json: {e}"));
        assert!(
            ctx["doctor"]["findings"]
                .as_array()
                .is_some_and(|f| f.iter().any(|x| x["check"] == check)),
            "context json doctor block does not name `{check}`: {}",
            ctx["doctor"]
        );
        let toon = self.writ(agent, &["context", "--for-agent", agent]);
        assert!(
            String::from_utf8_lossy(&toon.stdout).contains(check),
            "default `writ context` does not name `{check}`"
        );
        first.unwrap()
    }

    /// Assertion 3: the printed fix, verbatim, through sh.
    fn run_fix(&self, agent: &str, finding: &serde_json::Value) -> Output {
        let fix = finding["fix_command"].as_str().unwrap();
        let out = self.cmd("sh", agent).args(["-c", fix]).output().unwrap();
        assert!(
            out.status.success(),
            "fix_command failed verbatim ({:?}): {fix}\nstdout:\n{}\nstderr:\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    /// All four assertions, given a repo already in the bad state.
    fn full_cycle(&self, agent: &str, check: &str) -> serde_json::Value {
        let finding = self.assert_names(agent, check);
        self.run_fix(agent, &finding);
        self.assert_clean(agent, &format!("after `{}`", finding["fix_command"]));
        finding
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        if std::env::var_os("WRIT_KEEP_FIXTURES").is_none() {
            let _ = fs::remove_dir_all(self.root.parent().unwrap());
        }
    }
}

/// RFC 3339 UTC with microseconds, the format writ writes.
fn chrono_like(t: SystemTime) -> String {
    let secs = t.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() as i64;
    let micros = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .subsec_micros();
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant), no chrono dependency in tests.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{micros:06}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Finding 79: doctor never prints a destructive command on files writ does
/// not own. Every shell segment of every fix_command (split on `&&`, `||`,
/// `;`, `|`, newlines) must not start with a deleting or moving command, and
/// git must not be asked to discard work.
fn assert_no_destructive_fixes(report: &serde_json::Value) {
    const BANNED: [&str; 9] = [
        "rm", "rmdir", "mv", "unlink", "shred", "truncate", "dd", "sudo", "ln",
    ];
    const BANNED_GIT: [&str; 5] = [
        "reset --hard",
        "clean",
        "checkout --",
        "restore",
        "push --force",
    ];
    for f in report["findings"].as_array().into_iter().flatten() {
        let fix = f["fix_command"].as_str().unwrap_or("");
        for seg in fix
            .split(['\n', ';', '|', '&'])
            .map(str::trim)
            .filter(|x| !x.is_empty())
        {
            let first = seg.split_whitespace().next().unwrap_or("");
            assert!(
                !BANNED.contains(&first),
                "finding 79: destructive fix_command for `{}`: {fix}",
                f["check"]
            );
            if first == "git" {
                let rest = seg.trim_start_matches("git").trim();
                assert!(
                    !BANNED_GIT.iter().any(|b| rest.starts_with(b)),
                    "finding 79: fix_command discards git work for `{}`: {fix}",
                    f["check"]
                );
            }
        }
    }
}

#[test]
fn harness_destructive_fix_guard_catches_rm_and_allows_brew_unlink() {
    let ok = serde_json::json!({"findings": [
        {"check": "version_skew", "fix_command": "brew unlink writ"},
        {"check": "left_out", "fix_command": "writ spec add \"x (follow-up)\" --claim && writ seal -s 'x' --paths a.txt"},
        {"check": "version_skew", "fix_command": ""},
    ]});
    assert_no_destructive_fixes(&ok);
    for bad in [
        "rm /opt/homebrew/bin/writ",
        "writ repair && rm -rf .writ",
        "git reset --hard HEAD",
        "mv a b",
    ] {
        let r = serde_json::json!({"findings": [{"check": "x", "fix_command": bad}]});
        assert!(
            std::panic::catch_unwind(|| assert_no_destructive_fixes(&r)).is_err(),
            "guard missed: {bad}"
        );
    }
}

fn finding_paths(f: &serde_json::Value) -> Vec<String> {
    f["paths"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| p.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The six bad states
// ---------------------------------------------------------------------------

/// Check 1. A sealed blob deleted from `.writ/objects` while the file is still
/// on disk: repair can regenerate it, so the printed fix must actually do it.
#[test]
fn doctor_state_store_integrity_missing_blob() {
    let p = Project::new("store");
    p.spec("sa", "a");
    p.write("kept.txt", "kept\n");
    p.seal("a", "sa", "kept.txt");
    p.assert_clean("a", "before");

    let log = p.ok("a", &["log", "--format", "json", "--limit", "1"]);
    let seals: serde_json::Value = serde_json::from_slice(&log.stdout).unwrap();
    let hash = seals[0]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["path"] == "kept.txt")
        .and_then(|c| c["new_hash"].as_str())
        .unwrap()
        .to_string();
    fs::remove_file(p.path(&format!(".writ/objects/{}/{}", &hash[..2], &hash[2..]))).unwrap();

    let f = p.full_cycle("a", CHECK_STORE_INTEGRITY);
    assert_eq!(f["severity"], "red");
    assert!(finding_paths(&f).contains(&"kept.txt".to_string()), "{f:#}");
    assert!(p
        .path(&format!(".writ/objects/{}/{}", &hash[..2], &hash[2..]))
        .exists());
}

/// Check 2. A claim idle past `stale_claim_minutes` (default 120) whose
/// holder doctor cannot check: it was made on another host (a shared
/// directory), sealed once three hours ago, and never came back.
#[test]
fn doctor_state_stale_claim_idle_holder() {
    let p = Project::new("stale");
    p.spec("sa", "ghost");
    p.write("g.txt", "ghost\n");
    p.seal("ghost", "sa", "g.txt");
    p.assert_clean("other", "before (fresh claim)");

    p.age_spec("sa", Duration::from_secs(3 * 3600));
    p.set_spec_field(
        "sa",
        "claimed_host",
        serde_json::json!("other-host.invalid"),
    );

    let f = p.full_cycle("other", CHECK_STALE_CLAIM);
    let fix = f["fix_command"].as_str().unwrap();
    assert!(fix.contains("spec release") && fix.contains("sa"), "{fix}");
    let show = p.ok("other", &["spec", "show", "sa", "--format", "json"]);
    let spec: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert!(
        spec["claimed_by"].is_null(),
        "claim still held after fix: {spec:#}"
    );
}

/// Check 2, session form (the live-run injection): a third session claims a
/// spec and exits. The session is a process named `claude` (a symlink to sh,
/// so the process table shows that name) that runs `writ spec claim` and
/// exits; doctor sees the claim's host is this host and its pid is dead, and
/// must call it stale without waiting for the idle threshold.
#[test]
fn doctor_state_stale_claim_holder_session_exited() {
    let p = Project::new("stale-session");
    p.ok(
        "human",
        &["spec", "add", "--id", "ghost", "--title", "ghost"],
    );
    p.assert_clean("other", "before (unclaimed spec)");

    #[cfg(unix)]
    std::os::unix::fs::symlink("/bin/sh", p.bin.join("claude")).unwrap();
    let session = p
        .cmd(p.bin.join("claude").to_str().unwrap(), "ghost-session")
        // `&& :` keeps sh alive as writ's parent (a lone command is exec'd in
        // place), so the nearest `claude` ancestor is this short-lived session
        // and not whatever long-lived claude runs the test suite.
        .args(["-c", "writ spec claim ghost --agent ghost-session && :"])
        .output()
        .unwrap();
    assert!(
        session.status.success(),
        "{}",
        String::from_utf8_lossy(&session.stderr)
    );
    let spec: serde_json::Value = serde_json::from_str(&p.read(".writ/specs/ghost.json")).unwrap();
    assert_eq!(spec["claimed_by"], "ghost-session");
    let pid = spec["claimed_pid"].as_u64().expect("claim recorded no pid");
    let alive = Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .unwrap();
    assert!(
        !alive.success(),
        "fixture: claim pid {pid} is still alive, not the exited session"
    );

    let f = p.full_cycle("other", CHECK_STALE_CLAIM);
    let fix = f["fix_command"].as_str().unwrap();
    assert!(
        fix.contains("spec release") && fix.contains("ghost"),
        "{fix}"
    );
}

/// Check 3 (findings 65, 74). A file whose newest seal is on a committed spec
/// and whose content differs from HEAD: spec sa sealed v1 and finish
/// committed it; then HEAD moved to v2 outside writ (a human commit). The fix
/// is a follow-up spec plus a seal of the file.
#[test]
fn doctor_state_committed_spec_seal_stuck_file() {
    let p = Project::new("committed");
    p.spec("sa", "a");
    p.write("s.txt", "v1\n");
    p.seal("a", "sa", "s.txt");
    p.ok("a", &["spec", "done", "sa", "--agent", "a", "--no-seal"]);
    p.ok("human", &["finish", "-y", "--no-check"]);
    assert_eq!(
        p.git_ok(&["show", "HEAD:s.txt"]),
        "v1\n",
        "finish did not commit sa"
    );
    p.assert_clean("a", "after finish");

    p.write("s.txt", "v2\n");
    p.git_ok(&[
        "commit",
        "-q",
        "--no-verify",
        "-am",
        "human edit outside writ",
    ]);

    let f = p.full_cycle("a", CHECK_COMMITTED_SPEC_SEAL);
    assert!(finding_paths(&f).contains(&"s.txt".to_string()), "{f:#}");
    // A new spec, claimed, and the seal names it: `writ seal` without --spec
    // cannot choose when the agent holds more than one claim.
    let fix = f["fix_command"].as_str().unwrap();
    assert!(
        fix.contains("spec add") && fix.contains("--claim") && fix.contains("--spec "),
        "{f:#}"
    );
    assert_eq!(p.read("s.txt"), "v2\n", "fix touched the working tree");
}

/// Check 4. Pending work older than `unsealed_minutes` (default 30) while
/// another agent is active: a's edit to its own file has sat 45 minutes, b
/// sealed just now.
#[test]
fn doctor_state_unsealed_at_risk_old_pending_while_other_active() {
    let p = Project::new("unsealed");
    p.spec("sa", "a");
    p.spec("sb", "b");
    p.write("a.txt", "a1\n");
    p.seal("a", "sa", "a.txt");
    p.assert_clean("a", "before");

    p.write("a.txt", "a1\na2\n");
    p.set_mtime("a.txt", Duration::from_secs(45 * 60));
    p.write("b.txt", "b1\n");
    p.seal("b", "sb", "b.txt");

    let f = p.full_cycle("a", CHECK_UNSEALED_AT_RISK);
    assert!(finding_paths(&f).contains(&"a.txt".to_string()), "{f:#}");
    assert!(
        f["fix_command"].as_str().unwrap().contains("a.txt"),
        "--paths not filled in: {f:#}"
    );
    assert_eq!(p.read("a.txt"), "a1\na2\n", "fix lost the pending edit");
}

/// Check 5. The managed CLAUDE.md block older than this binary's template
/// (an older writ wrote it). Fix: `writ init -y`, which must restore the block
/// and leave the user's text outside it untouched.
#[test]
fn doctor_state_version_skew_stale_managed_block() {
    let p = Project::new("skew");
    p.assert_clean("a", "before");
    let original = p.read("CLAUDE.md");
    let begin = original
        .find("<!-- BEGIN WRIT")
        .expect("init wrote no managed block");
    let end_marker = "<!-- END WRIT CONFIGURATION -->";
    let end = original.find(end_marker).expect("no end marker");
    let header_end = begin + original[begin..].find('\n').unwrap() + 1;
    let stale = format!(
        "{}## Writ (0.2.0 text)\nRun `writ install` first.\n{}",
        &original[..header_end],
        &original[end..]
    );
    let stale = format!("Project notes kept by the user.\n\n{stale}");
    fs::write(p.path("CLAUDE.md"), &stale).unwrap();

    let f = p.full_cycle("a", CHECK_VERSION_SKEW);
    assert!(
        finding_paths(&f).iter().any(|x| x.contains("CLAUDE.md")),
        "{f:#}"
    );
    let after = p.read("CLAUDE.md");
    assert!(
        after.starts_with("Project notes kept by the user."),
        "fix dropped user text"
    );
    assert!(!after.contains("0.2.0 text"), "fix left the stale block");
}

/// Check 5, second form: two `writ` binaries on PATH. Naming only; the fix
/// (relink) is a package-manager command the harness cannot run hermetically.
#[test]
fn doctor_state_version_skew_two_writs_on_path_is_named() {
    let p = Project::new("skew-path");
    p.assert_clean("a", "before");
    let second = p.home.join("otherbin");
    fs::create_dir_all(&second).unwrap();
    // A real second file (a copy, as a second install would be), not a
    // symlink that resolves to the same binary.
    fs::copy(env!("CARGO_BIN_EXE_writ"), second.join("writ")).unwrap();
    let out = p
        .cmd(p.bin.join("writ").to_str().unwrap(), "a")
        .env("PATH", format!("{}:{}", p.env_path(), second.display()))
        .args(["doctor", "--format", "json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_no_destructive_fixes(&v);
    assert_eq!(out.status.code(), Some(1), "{v:#}");
    assert_eq!(v["clean"], false);
    assert!(
        v["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["check"] == CHECK_VERSION_SKEW),
        "{v:#}"
    );
}

/// Check 6 (finding 75). A completed spec waits for finish and a file no spec
/// sealed is genuinely uncommitted: doctor names it. A file rewritten with
/// HEAD's exact content (mtime changed, bytes identical) is not left out and
/// must never be listed.
#[test]
fn doctor_state_left_out_uncommitted_unsealed_file() {
    let p = Project::new("leftout");
    p.spec("sa", "a");
    p.write("a.txt", "a1\n");
    p.seal("a", "sa", "a.txt");
    p.ok("a", &["spec", "done", "sa", "--agent", "a", "--no-seal"]);
    p.assert_clean("a", "before (completed spec, nothing left out)");

    p.write("stray.txt", "nobody sealed me\n");
    let readme = p.read("README.md");
    p.write("README.md", &readme); // identical bytes, new mtime
    p.set_mtime("README.md", Duration::from_secs(5));

    let f = p.assert_names("a", CHECK_LEFT_OUT);
    let paths = finding_paths(&f);
    assert!(paths.contains(&"stray.txt".to_string()), "{f:#}");
    assert!(
        !paths.contains(&"README.md".to_string()),
        "finding 75: identical-to-HEAD file listed: {f:#}"
    );
    p.run_fix("a", &f);
    p.assert_clean("a", &format!("after `{}`", f["fix_command"]));
}

/// Two bad states at once (a committed-spec stuck file and old pending work
/// while another agent is active). Each fix_command must work pasted alone,
/// in either order: a fix that only works after another finding's fix (for
/// example sealing to a spec the other fix creates) fails the agent that
/// pastes just the one it read.
#[test]
fn doctor_two_states_each_fix_works_alone_in_reverse_order() {
    two_states_each_fix_alone("a");
}

/// Same, pasted by someone holding no claim (a human, or a fresh session):
/// the case where a fix might lean on a spec only another fix creates.
#[test]
fn doctor_two_states_each_fix_works_alone_for_an_actor_without_a_claim() {
    two_states_each_fix_alone("human");
}

fn two_states_each_fix_alone(actor: &str) {
    let p = Project::new("two-states");
    p.spec("sa", "a");
    p.write("s.txt", "v1\n");
    p.seal("a", "sa", "s.txt");
    p.ok("a", &["spec", "done", "sa", "--agent", "a", "--no-seal"]);
    p.ok("human", &["finish", "-y", "--no-check"]);
    p.write("s.txt", "v2\n");
    p.git_ok(&[
        "commit",
        "-q",
        "--no-verify",
        "-am",
        "human edit outside writ",
    ]);
    p.spec("sc", "a");
    p.spec("sb", "b");
    p.write("a.txt", "a1\n");
    p.seal("a", "sc", "a.txt");
    p.write("a.txt", "a1\na2\n");
    p.set_mtime("a.txt", Duration::from_secs(45 * 60));
    p.write("b.txt", "b1\n");
    p.seal("b", "sb", "b.txt");

    let (code, v) = p.doctor(actor);
    assert_eq!(code, Some(1), "{v:#}");
    let mut findings: Vec<serde_json::Value> = v["findings"].as_array().unwrap().clone();
    let ids: Vec<&str> = findings
        .iter()
        .map(|f| f["check"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&CHECK_COMMITTED_SPEC_SEAL) && ids.contains(&CHECK_UNSEALED_AT_RISK),
        "{v:#}"
    );
    findings.reverse();
    for f in &findings {
        if f["fix_command"].is_string() {
            p.run_fix(actor, f);
        }
    }
    p.assert_clean(actor, "after each fix pasted alone, reverse order");
}

// ---------------------------------------------------------------------------
// Healthy repo: zero findings
// ---------------------------------------------------------------------------

/// Two agents, each with a claimed spec, a recent seal and fresh pending
/// edits in its own files, plus an identical-content rewrite of a committed
/// file. Doctor must report nothing: a false positive here is the fastest
/// way to get doctor ignored. Live from the first check on: every check that
/// lands must keep it green.
#[test]
fn doctor_healthy_two_active_agents_with_pending_work_has_zero_findings() {
    let p = Project::new("healthy");
    p.spec("sa", "a");
    p.spec("sb", "b");
    p.write("src/a.rs", "pub fn a() {}\n");
    p.write("src/b.rs", "pub fn b() {}\n");
    p.seal("a", "sa", "src/a.rs");
    p.seal("b", "sb", "src/b.rs");
    p.write("src/a.rs", "pub fn a() {}\npub fn a2() {}\n");
    p.write("src/b.rs", "pub fn b() {}\npub fn b2() {}\n");
    p.write("src/a_new.rs", "// new, not sealed yet\n");
    let readme = p.read("README.md");
    p.write("README.md", &readme);

    for agent in ["a", "b", "human"] {
        p.assert_clean(agent, &format!("healthy, as {agent}"));
    }
    if let Some(line) = p.brief_doctor_line("a") {
        assert!(line.contains(CLEAN), "{line}");
    }
}

/// Exit criterion 4 and 5 on a small repo: the brief carries the doctor line
/// with the tier, never "safe to finish", and stays under its 2 KB budget.
#[test]
fn context_brief_carries_doctor_line_under_2kb() {
    let p = Project::new("brief");
    p.spec("sa", "a");
    p.write("a.txt", "a\n");
    p.seal("a", "sa", "a.txt");
    let out = p.ok("a", &["context", "--format", "brief", "--for-agent", "a"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.stdout.len() < 2048,
        "brief is {} bytes:\n{text}",
        out.stdout.len()
    );
    let line = p.brief_doctor_line("a").expect("no doctor: line in brief");
    assert!(line.contains("fast"), "tier missing from {line}");
    assert!(
        line.contains("survival check not available until 0.4.1"),
        "{line}"
    );
    assert!(!text.to_lowercase().contains("safe to finish"), "{text}");
}

// ---------------------------------------------------------------------------
// finish runs doctor first
// ---------------------------------------------------------------------------

/// A completed spec ready to commit, with one sealed file.
fn ready_to_finish(tag: &str) -> Project {
    let p = Project::new(tag);
    p.spec("sa", "a");
    p.write("a.txt", "a1\n");
    p.seal("a", "sa", "a.txt");
    p.ok("a", &["spec", "done", "sa", "--agent", "a", "--no-seal"]);
    p
}

/// Red state: an unrelated blob missing from the store.
fn make_red(p: &Project) -> serde_json::Value {
    p.spec("sb", "b");
    p.write("b.txt", "b1\n");
    p.seal("b", "sb", "b.txt");
    let log = p.ok("b", &["log", "--format", "json", "--limit", "1"]);
    let seals: serde_json::Value = serde_json::from_slice(&log.stdout).unwrap();
    let hash = seals[0]["changes"][0]["new_hash"]
        .as_str()
        .unwrap()
        .to_string();
    fs::remove_file(p.path(&format!(".writ/objects/{}/{}", &hash[..2], &hash[2..]))).unwrap();
    let (code, v) = p.doctor("human");
    assert_eq!(code, Some(1), "{v:#}");
    v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["severity"] == "red")
        .cloned()
        .expect("no red finding")
}

#[test]
fn finish_refuses_on_red_naming_the_fix() {
    let p = ready_to_finish("finish-red");
    let red = make_red(&p);
    let before = p.head();
    let out = p.writ("human", &["finish", "-y", "--no-check"]);
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "finish committed on red:\n{all}");
    assert_eq!(p.head(), before, "finish moved HEAD on red");
    assert!(
        all.contains(red["check"].as_str().unwrap()),
        "check id not named:\n{all}"
    );
    assert!(
        all.contains(red["fix_command"].as_str().unwrap()),
        "fix not printed:\n{all}"
    );
    assert!(all.contains("--force"), "override not named:\n{all}");
}

#[test]
fn finish_force_proceeds_on_red() {
    let p = ready_to_finish("finish-force");
    make_red(&p);
    let before = p.head();
    p.ok("human", &["finish", "-y", "--no-check", "--force"]);
    assert_ne!(p.head(), before, "--force did not commit");
    assert_eq!(p.git_ok(&["show", "HEAD:a.txt"]), "a1\n");
}

#[test]
fn finish_on_green_runs_the_clean_gate_and_commits() {
    let p = ready_to_finish("finish-green");
    p.assert_clean("human", "before finish");
    let before = p.head();
    let out = p.ok("human", &["finish", "-y", "--no-check"]);
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        all.contains(CLEAN),
        "finish did not print the doctor headline:\n{all}"
    );
    assert!(!all.to_lowercase().contains("safe to finish"), "{all}");
    assert_ne!(p.head(), before);
    assert_eq!(p.git_ok(&["show", "HEAD:a.txt"]), "a1\n");
}

// ---------------------------------------------------------------------------
// Harness self-checks (always on): the fixture itself is sound.
// ---------------------------------------------------------------------------

#[test]
fn harness_fixture_is_hermetic_and_initialized() {
    let p = Project::new("self");
    assert!(p.path(".writ").is_dir());
    assert!(p.read("CLAUDE.md").contains("<!-- BEGIN WRIT"));
    let which = p
        .cmd("sh", "a")
        .args(["-c", "command -v writ"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&which.stdout).trim(),
        p.bin.join("writ").to_str().unwrap(),
        "fix commands would not run the binary under test"
    );
    assert!(
        p.git_ok(&["status", "--porcelain"]).trim().is_empty(),
        "fixture leaves files uncommitted"
    );
}

#[test]
fn harness_chrono_like_formats_rfc3339() {
    let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_759_708_800) + Duration::from_micros(42);
    assert_eq!(chrono_like(t), "2025-10-06T00:00:00.000042Z");
}
