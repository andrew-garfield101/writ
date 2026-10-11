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

/// Clean headline on a repo where the survival tier never ran (0.4.1,
/// honesty condition): the fresh-repo form, never "safe to finish".
const CLEAN: &str = "fast checks clean; survival not run; run `writ doctor --survival`";
/// Every clean fast-tier headline starts with this. After a finish (which
/// runs the survival tier) the suffix is one of: "survival green as of
/// <seal12>; safe to finish", "survival red: N sealed line(s) lost as of
/// <seal12>; run ...", "survival last green as of <seal12>, store changed
/// since; run ...".
const CLEAN_PREFIX: &str = "fast checks clean; survival ";
/// Aubs's honesty condition: only when both tiers are green on the current
/// state.
const SAFE: &str = "safe to finish";

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
        let headline = v["headline"].as_str().unwrap_or("");
        assert!(
            headline.starts_with(CLEAN_PREFIX),
            "{when}: clean report with headline {headline:?}"
        );
        // "safe to finish" only in the green form.
        assert!(
            !headline.to_lowercase().contains(SAFE)
                || headline.starts_with("fast checks clean; survival green as of "),
            "{when}: 'safe to finish' outside the green form: {headline:?}"
        );
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
            let headline = v["headline"].as_str().unwrap_or("");
            assert!(
                !headline.starts_with("fast checks clean")
                    && !headline.to_lowercase().contains(SAFE),
                "run {run}: clean headline while the state exists: {headline:?}"
            );
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
            !line.contains("fast checks clean") && !line.to_lowercase().contains(SAFE),
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
///
/// Finding 91 (sprint 4): there is no evidence the holder is gone, so no
/// command an agent may paste clears it (`--stale` refuses another host,
/// `--force` is never printed). Doctor names it, prints no fix_command,
/// sets needs_human, and the message names the host and says a human may
/// release it with `--force`.
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

    let (code, v) = p.doctor("other");
    assert_eq!(code, Some(1), "{v:#}");
    let f = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["check"] == CHECK_STALE_CLAIM)
        .cloned()
        .unwrap_or_else(|| panic!("foreign-host idle claim not named:\n{v:#}"));
    assert!(
        f["fix_command"].is_null(),
        "printed a fix no agent may run: {f:#}"
    );
    assert_eq!(f["needs_human"], true, "{f:#}");
    let msg = f["message"].as_str().unwrap();
    assert!(msg.contains("other-host.invalid"), "host not named: {msg}");
    assert!(msg.contains("--force"), "human option not named: {msg}");
    let show = p.ok("other", &["spec", "show", "sa", "--format", "json"]);
    let spec: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(spec["claimed_by"], "ghost", "doctor changed the claim");
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
        let (_, v) = p.doctor(agent);
        assert_eq!(v["headline"], CLEAN, "fresh repo, survival never run");
    }
    if let Some(line) = p.brief_doctor_line("a") {
        assert!(line.contains(CLEAN), "{line}");
        assert!(!line.to_lowercase().contains(SAFE), "{line}");
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
        line.contains("survival not run"),
        "fresh repo must say survival not run: {line}"
    );
    // The survival tier has never run on this repo: never "safe to finish".
    assert!(!text.to_lowercase().contains(SAFE), "{text}");
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
        all.contains(CLEAN_PREFIX),
        "finish did not print the doctor headline:\n{all}"
    );
    assert_ne!(p.head(), before);
    assert_eq!(p.git_ok(&["show", "HEAD:a.txt"]), "a1\n");
}

// ---------------------------------------------------------------------------
// Sprint 4 (0.4.1): findings 91, 96, 97 and the survival tier. Written before
// the code (retro rule 1); each is ignored on EXPECTED_IGNORED until it lands.
// ---------------------------------------------------------------------------

/// Survival tier finding id (Haris; rename here if the code differs).
const CHECK_SURVIVAL: &str = "survival";

/// Finding 91, session form: the third session exited (same host, dead
/// pid). The printed fix is `writ spec release ghost --stale`, never
/// `--force`, and pasted verbatim it clears the state.
#[test]
fn doctor_state_stale_claim_session_exited_fix_is_stale_not_force() {
    let p = Project::new("stale-fix-91");
    p.ok(
        "human",
        &["spec", "add", "--id", "ghost", "--title", "ghost"],
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink("/bin/sh", p.bin.join("claude")).unwrap();
    let session = p
        .cmd(p.bin.join("claude").to_str().unwrap(), "ghost-session")
        .args(["-c", "writ spec claim ghost --agent ghost-session && :"])
        .output()
        .unwrap();
    assert!(session.status.success());
    let f = p.full_cycle("other", CHECK_STALE_CLAIM);
    let fix = f["fix_command"].as_str().unwrap();
    assert!(
        fix.contains("spec release") && fix.contains("ghost") && fix.contains("--stale"),
        "{fix}"
    );
    assert!(!fix.contains("--force"), "{fix}");
}

/// Finding 91: no fix_command doctor prints contains `--force`, with every
/// bad state the harness knows present at once (missing blob, exited-session
/// claim, foreign-host idle claim, committed-spec stuck file, old pending
/// work while another agent is active, stale managed block, left-out file).
#[test]
fn doctor_no_fix_command_contains_force_in_any_state() {
    let p = Project::new("noforce");
    // committed-spec stuck file
    p.spec("sc", "c");
    p.write("s.txt", "v1\n");
    p.seal("c", "sc", "s.txt");
    p.ok("c", &["spec", "done", "sc", "--agent", "c", "--no-seal"]);
    p.ok("human", &["finish", "-y", "--no-check"]);
    p.write("s.txt", "v2\n");
    p.git_ok(&["commit", "-q", "--no-verify", "-am", "outside writ"]);
    // foreign-host idle claim
    p.spec("sf", "far");
    p.write("far.txt", "far\n");
    p.seal("far", "sf", "far.txt");
    p.age_spec("sf", Duration::from_secs(3 * 3600));
    p.set_spec_field(
        "sf",
        "claimed_host",
        serde_json::json!("other-host.invalid"),
    );
    // exited-session claim
    p.ok(
        "human",
        &["spec", "add", "--id", "ghost", "--title", "ghost"],
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink("/bin/sh", p.bin.join("claude")).unwrap();
    p.cmd(p.bin.join("claude").to_str().unwrap(), "ghost-session")
        .args(["-c", "writ spec claim ghost --agent ghost-session && :"])
        .output()
        .unwrap();
    // old pending work while another agent is active
    p.spec("sa", "a");
    p.spec("sb", "b");
    p.write("a.txt", "a1\n");
    p.seal("a", "sa", "a.txt");
    p.write("a.txt", "a1\na2\n");
    p.set_mtime("a.txt", Duration::from_secs(45 * 60));
    p.write("b.txt", "b1\n");
    p.seal("b", "sb", "b.txt");
    // missing blob
    let log = p.ok("b", &["log", "--format", "json", "--limit", "1"]);
    let seals: serde_json::Value = serde_json::from_slice(&log.stdout).unwrap();
    let hash = seals[0]["changes"][0]["new_hash"]
        .as_str()
        .unwrap()
        .to_string();
    fs::remove_file(p.path(&format!(".writ/objects/{}/{}", &hash[..2], &hash[2..]))).unwrap();
    // stale managed block
    let claude = p.read("CLAUDE.md");
    let begin = claude.find("<!-- BEGIN WRIT").unwrap();
    let header_end = begin + claude[begin..].find('\n').unwrap() + 1;
    let end = claude.find("<!-- END WRIT CONFIGURATION -->").unwrap();
    p.write(
        "CLAUDE.md",
        &format!("{}old text\n{}", &claude[..header_end], &claude[end..]),
    );
    // left-out file
    p.write("stray.txt", "nobody sealed me\n");

    for agent in ["a", "other", "human"] {
        let (_, v) = p.doctor(agent);
        let findings = v["findings"].as_array().unwrap();
        let checks: std::collections::BTreeSet<&str> = findings
            .iter()
            .filter_map(|f| f["check"].as_str())
            .collect();
        assert!(
            checks.len() >= 5 && checks.contains(CHECK_STALE_CLAIM),
            "fixture did not produce the states: {checks:?}"
        );
        for f in findings {
            let fix = f["fix_command"].as_str().unwrap_or("");
            assert!(
                !fix.contains("--force"),
                "`{}` prints --force: {fix}",
                f["check"]
            );
        }
        let human = p.writ(agent, &["doctor"]);
        let text = String::from_utf8_lossy(&human.stdout);
        for line in text.lines().filter(|l| l.contains("writ ")) {
            assert!(
                !line.contains("--force") || line.to_lowercase().contains("human"),
                "human doctor output offers --force as a fix: {line}"
            );
        }
    }
}

/// Finding 96: one writ listed twice on PATH (the same directory twice) is
/// one writ, not version_skew.
///
/// Live from the start: 0.4.0's `writs_on_path` already canonicalizes, as
/// finding 96 suspected; this pins it.
#[test]
fn doctor_version_skew_same_dir_twice_on_path_is_not_skew() {
    let p = Project::new("path-dup");
    let out = p
        .cmd(p.bin.join("writ").to_str().unwrap(), "a")
        .env("PATH", format!("{}:{}", p.bin.display(), p.env_path()))
        .args(["doctor", "--format", "json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(0), "{v:#}");
    assert_eq!(v["clean"], true, "{v:#}");
}

/// Finding 96: a second PATH directory holding a symlink to the same binary
/// resolves to the same file: one writ, not version_skew. (A real copy is
/// still two writs: `doctor_state_version_skew_two_writs_on_path_is_named`.)
#[test]
fn doctor_version_skew_symlink_to_same_binary_is_not_skew() {
    let p = Project::new("path-link");
    let second = p.home.join("linkbin");
    fs::create_dir_all(&second).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_writ"), second.join("writ")).unwrap();
    let out = p
        .cmd(p.bin.join("writ").to_str().unwrap(), "a")
        .env("PATH", format!("{}:{}", p.env_path(), second.display()))
        .args(["doctor", "--format", "json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(0), "{v:#}");
    assert!(
        !v["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["check"] == CHECK_VERSION_SKEW),
        "{v:#}"
    );
}

/// `n` specs by `n` agents, each sealing its own file twice in interleaved
/// rounds (seal isolation forks every spec chain off HEAD), then spec done.
fn forked_specs(p: &Project, n: usize) {
    for i in 0..n {
        p.spec(&format!("s{i}"), &format!("ag{i}"));
    }
    for round in 0..2 {
        for i in 0..n {
            let file = format!("f{i}.txt");
            p.write(&file, &format!("{i}\n{round}\n"));
            p.seal(&format!("ag{i}"), &format!("s{i}"), &file);
        }
    }
    for i in 0..n {
        let (a, s) = (format!("ag{i}"), format!("s{i}"));
        p.ok(&a, &["spec", "done", &s, "--agent", &a, "--no-seal"]);
    }
}

/// (format, for-agent) views an agent sees: plain `writ context` (toon), the
/// SessionStart brief and json, unscoped and scoped to a fresh agent.
fn context_views(p: &Project) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for fmt in ["brief", "toon", "json"] {
        for scoped in [false, true] {
            let mut args = vec!["context", "--format", fmt];
            if scoped {
                args.extend(["--for-agent", "fresh"]);
            }
            let o = p.ok("fresh", &args);
            out.push((
                format!("{fmt}{}", if scoped { " --for-agent fresh" } else { "" }),
                String::from_utf8_lossy(&o.stdout).into_owned(),
            ));
        }
    }
    out
}

/// Guard for finding 97 (live now): live divergence (forked specs not yet
/// committed) is still reported. A fix that stops reporting divergence
/// altogether fails here.
#[test]
fn context_uncommitted_forked_specs_still_report_divergence() {
    let p = Project::new("f97-live");
    forked_specs(&p, 3);
    let o = p.ok("fresh", &["context", "--format", "json"]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(
        v["diverged_branches"]
            .as_array()
            .is_some_and(|d| !d.is_empty()),
        "uncommitted forked specs no longer reported: {}",
        v["diverged_branches"]
    );
}

/// Finding 97: a clean repo whose 26 committed specs each left a forked seal
/// chain (this repo's shape) reports risk low and no converge (or any
/// high-priority) next action, in brief, toon and json, scoped and not.
/// Doctor is clean, nothing is committable, so there is no next action.
#[test]
fn context_clean_repo_with_26_committed_chains_is_risk_low_no_converge() {
    let p = Project::new("f97");
    forked_specs(&p, 26);
    p.ok("human", &["finish", "-y", "--no-check"]);
    p.assert_clean("fresh", "after finish of 26 specs");
    for (view, text) in context_views(&p) {
        let lower = text.to_lowercase();
        assert!(
            !lower.contains("converge-all"),
            "{view}: converge suggested:\n{text}"
        );
        assert!(
            !lower.contains("diverged branch"),
            "{view}: divergence reported:\n{text}"
        );
        if view.starts_with("json") {
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            if let Some(r) = v.get("integration_risk") {
                assert_eq!(r["level"], "low", "{view}: {r}");
            }
            assert!(
                v["recommended_action"].is_null(),
                "{view}: next action on a clean repo: {}",
                v["recommended_action"]
            );
            if let Some(d) = v.get("diverged_branches") {
                assert!(d.as_array().map_or(true, |a| a.is_empty()), "{view}: {d}");
            }
        } else {
            for l in text.lines().map(str::trim) {
                assert!(
                    !(l.starts_with("level:") && !l.ends_with("low")),
                    "{view}: risk not low: {l}\n{text}"
                );
                assert!(
                    !l.starts_with("action:") && !l.starts_with("priority: high"),
                    "{view}: next action on a clean repo: {l}\n{text}"
                );
            }
        }
    }
}

/// Every surface an agent or human reads the doctor state from.
fn safe_surfaces(p: &Project, agent: &str) -> Vec<(String, String)> {
    let mut out = context_views(p);
    for args in [
        &["doctor"][..],
        &["doctor", "--format", "json"][..],
        &["status"][..],
    ] {
        let o = p.writ(agent, args);
        out.push((
            format!("writ {}", args.join(" ")),
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            ),
        ));
    }
    out
}

fn assert_never_safe(p: &Project, agent: &str, when: &str) {
    for (surface, text) in safe_surfaces(p, agent) {
        assert!(
            !text.to_lowercase().contains(SAFE),
            "{when}: '{SAFE}' on `{surface}`:\n{text}"
        );
    }
}

fn newest_seal(p: &Project) -> String {
    let log = p.ok(
        "human",
        &["log", "--all", "--format", "json", "--limit", "1"],
    );
    let v: serde_json::Value = serde_json::from_slice(&log.stdout).unwrap();
    v[0]["id"].as_str().unwrap().to_string()
}

fn doctor_survival(p: &Project, agent: &str) -> (Option<i32>, serde_json::Value, String) {
    let j = p.writ(agent, &["doctor", "--survival", "--format", "json"]);
    let v: serde_json::Value = serde_json::from_slice(&j.stdout).unwrap_or_else(|e| {
        panic!(
            "doctor --survival --format json: {e}\n{}{}",
            String::from_utf8_lossy(&j.stdout),
            String::from_utf8_lossy(&j.stderr)
        )
    });
    assert_no_destructive_fixes(&v);
    let h = p.writ(agent, &["doctor", "--survival"]);
    let human = format!(
        "{}{}",
        String::from_utf8_lossy(&h.stdout),
        String::from_utf8_lossy(&h.stderr)
    );
    (j.status.code(), v, human)
}

// -- Variant B under the override rule (CC, sprint 4) and finding 102 ------
//
// Rule: an own-line removal under the spec's own override (`--allow-removals`
// or `--force`) is intentional and clean, unless the override's replacement
// content is or becomes another spec's sealed content: then survival reports a
// yellow "override captured another spec's version" naming both specs, with
// needs_human and the fix `writ finish --accept-removals <seal>`, not blocking
// on its own. Without an override and without an informed base, a loss is red
// and finish refuses (the tier's primary catch).

const AGENT_A: &str = "agent-alpha";
const AGENT_B: &str = "agent-bravo";

/// 40 numbered lines with `suffix` appended to the given line numbers.
fn numbered(edits: &[(usize, &str)]) -> String {
    (0..40)
        .map(|i| match edits.iter().find(|(n, _)| *n == i) {
            Some((_, sfx)) => format!("line {i} {sfx}\n"),
            None => format!("line {i}\n"),
        })
        .collect()
}

fn out_text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// `shared.txt` (40 numbered lines) sealed by a setup spec and committed by
/// finish, so writ knows the base exactly as it would after real work.
fn shared_base(p: &Project) {
    p.spec("setup", "human");
    p.write("shared.txt", &numbered(&[]));
    p.seal("human", "setup", "shared.txt");
    p.ok(
        "human",
        &["spec", "done", "setup", "--agent", "human", "--no-seal"],
    );
    p.ok("human", &["finish", "-y", "--no-check"]);
    assert_eq!(p.git_ok(&["show", "HEAD:shared.txt"]), numbered(&[]));
}

/// Committed `shared.txt`; spec sa claimed by AGENT_A, sb by AGENT_B (when
/// `with_b`); sa seals "line 2 A". Returns the project and that seal's id.
fn shared_with_a_line(tag: &str, with_b: bool) -> (Project, String) {
    let p = Project::new(tag);
    shared_base(&p);
    p.spec("sa", AGENT_A);
    if with_b {
        p.spec("sb", AGENT_B);
    }
    p.write("shared.txt", &numbered(&[(2, "A")]));
    p.seal(AGENT_A, "sa", "shared.txt");
    let added = newest_seal(&p);
    (p, added)
}

fn seal_shared(p: &Project, agent: &str, spec: &str, extra: &[&str]) -> Output {
    let mut args = vec![
        "seal",
        "-s",
        "work",
        "--agent",
        agent,
        "--spec",
        spec,
        "--paths",
        "shared.txt",
    ];
    args.extend_from_slice(extra);
    p.writ(agent, &args)
}

/// The override context a seal record carries (finding 102): any field whose
/// name contains "override", serialized.
fn override_context(p: &Project, seal: &str) -> String {
    let o = p.ok("human", &["show", seal, "--format", "json"]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    v.as_object()
        .unwrap()
        .iter()
        .filter(|(k, _)| k.contains("override"))
        .map(|(_, x)| x.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The variant B shape where the other spec sealed: a seals "line 2 A"; b
/// seals its own version of the file (without a's line); a keeps working on
/// top of b's version and seals. Today that seal passes silently and a's line
/// never reaches the commit while survival says "safe to finish" (Bri probe,
/// 2026-10-11). Finding 102: the seal is refused, naming the other agent that
/// touched the file since a's last seal and both choices.
fn b_sealed_then_a_seals_on_top(tag: &str) -> (Project, String, Output) {
    let (p, added) = shared_with_a_line(tag, true);
    p.write("shared.txt", &numbered(&[(30, "B")]));
    p.seal(AGENT_B, "sb", "shared.txt");
    p.write("shared.txt", &numbered(&[(10, "A2"), (30, "B")]));
    let refused = seal_shared(&p, AGENT_A, "sa", &[]);
    (p, added, refused)
}

#[test]
fn variant_b_seal_refusal_names_other_agent_and_both_choices() {
    let (p, _added, refused) = b_sealed_then_a_seals_on_top("f102-refusal");
    let text = out_text(&refused);
    assert!(
        !refused.status.success(),
        "a's seal on top of b's version passed:\n{text}"
    );
    assert!(text.contains("line 2 A"), "lost line not named:\n{text}");
    assert!(text.contains(AGENT_B), "other agent not named:\n{text}");
    let lower = text.to_lowercase();
    assert!(
        lower.contains("keep"),
        "'keep my lines' choice missing:\n{text}"
    );
    assert!(
        lower.contains("accept"),
        "'accept {AGENT_B}'s version' choice missing:\n{text}"
    );
    assert!(
        text.contains("--allow-removals shared.txt"),
        "override command for the path missing:\n{text}"
    );
    assert!(!text.contains("--force"), "refusal offers --force:\n{text}");
    let _ = p;
}

/// Finding 102: after the refusal, a overrides; the seal record carries the
/// override context: the path, the other agent and the lines.
#[test]
fn variant_b_override_after_refusal_records_context() {
    let (p, _added, refused) = b_sealed_then_a_seals_on_top("f102-context");
    assert!(!refused.status.success(), "{}", out_text(&refused));
    let ov = seal_shared(&p, AGENT_A, "sa", &["--allow-removals", "shared.txt"]);
    assert!(ov.status.success(), "{}", out_text(&ov));
    let ctx = override_context(&p, &newest_seal(&p));
    for want in ["shared.txt", AGENT_B, "line 2 A"] {
        assert!(
            ctx.contains(want),
            "seal override context lacks {want}: {ctx}"
        );
    }
}

/// Finding 102, no other agent: a's own line is gone from disk and the file
/// holds unsealed text. The refusal says the lines are gone from disk and
/// names nobody; the override records the path and the lines, no agent.
#[test]
fn variant_b_pure_refusal_says_gone_from_disk_and_names_nobody() {
    let (p, _added) = shared_with_a_line("f102-pure", false);
    p.write("shared.txt", &numbered(&[(30, "B")]));
    let refused = seal_shared(&p, AGENT_A, "sa", &[]);
    let text = out_text(&refused);
    assert!(!refused.status.success(), "{text}");
    let lower = text.to_lowercase();
    assert!(
        lower.contains("disk") && (lower.contains("gone") || lower.contains("no longer")),
        "refusal does not say the lines are gone from disk:\n{text}"
    );
    assert!(!lower.contains("another agent"), "names someone:\n{text}");
    let ov = seal_shared(&p, AGENT_A, "sa", &["--allow-removals", "shared.txt"]);
    assert!(ov.status.success(), "{}", out_text(&ov));
    let ctx = override_context(&p, &newest_seal(&p));
    assert!(
        ctx.contains("shared.txt") && ctx.contains("line 2 A"),
        "override context: {ctx}"
    );
}

/// The tier sees what the fast tier cannot (Haris's captured shape): b seals
/// its version first; a writes its line over it and seals; b's version
/// returns to disk; a closes with `--allow-removals`, recording b's sealed
/// content under sa. Fast tier clean; survival yellow "override captured
/// another spec's version" naming both specs, the seal and the line, with
/// `writ finish --accept-removals <seal12>` in the message and needs_human;
/// that command, run verbatim, commits and leaves survival clean.
#[test]
fn survival_captured_override_is_yellow_naming_both_and_accept_clears() {
    let p = Project::new("surv-captured");
    shared_base(&p);
    p.spec("sa", AGENT_A);
    p.spec("sb", AGENT_B);
    p.write("shared.txt", &numbered(&[(30, "B")]));
    p.seal(AGENT_B, "sb", "shared.txt");
    p.write("shared.txt", &numbered(&[(2, "A"), (30, "B")]));
    p.seal(AGENT_A, "sa", "shared.txt");
    let added = newest_seal(&p);
    p.write("shared.txt", &numbered(&[(30, "B")]));
    p.ok(
        AGENT_A,
        &[
            "spec",
            "done",
            "sa",
            "--agent",
            AGENT_A,
            "-s",
            "done",
            "--paths",
            "shared.txt",
            "--allow-removals",
            "shared.txt",
        ],
    );
    let override_seal = newest_seal(&p);
    assert_ne!(override_seal, added, "the close recorded no seal");
    p.ok(
        AGENT_B,
        &["spec", "done", "sb", "--agent", AGENT_B, "--no-seal"],
    );
    p.assert_clean("human", "fast tier, captured override");

    let (code, v, human) = doctor_survival(&p, "human");
    assert_eq!(code, Some(1), "a yellow is a finding:\n{v:#}");
    let f = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["check"] == CHECK_SURVIVAL)
        .cloned()
        .unwrap_or_else(|| panic!("captured override not reported:\n{v:#}"));
    assert_eq!(f["severity"], "yellow", "{f:#}");
    let msg = f["message"].as_str().unwrap();
    assert!(
        msg.contains("override captured another spec's version"),
        "{msg}"
    );
    assert!(
        msg.contains("'sa'") || msg.contains(" sa"),
        "sa not named: {msg}"
    );
    assert!(
        msg.contains("'sb'") || msg.contains(" sb"),
        "sb not named: {msg}"
    );
    assert!(msg.contains("line 2 A"), "line not named: {msg}");
    let accept = format!("writ finish --accept-removals {}", &override_seal[..12]);
    assert!(msg.contains(&accept), "message lacks `{accept}`: {msg}");
    assert_eq!(f["needs_human"], true, "{f:#}");
    assert!(!msg.contains("--force"), "{msg}");
    assert!(!human.to_lowercase().contains(SAFE), "{human}");
    assert_never_safe(&p, "human", "captured override (yellow)");

    let head = p.head();
    let out = p.cmd("sh", "human").args(["-c", &accept]).output().unwrap();
    assert!(
        out.status.success(),
        "`{accept}` failed:\n{}",
        out_text(&out)
    );
    assert_ne!(p.head(), head, "accept did not commit");
    let (code, v, _) = doctor_survival(&p, "human");
    assert_eq!(code, Some(0), "after `{accept}`:\n{v:#}");
}

/// The designed residual: pure variant B. b claims and leaves its version on
/// disk unsealed; a's seal is refused, a overrides with `--allow-removals`;
/// b never seals. Intentional and clean: survival green with the exact
/// headline, "safe to finish", and finish commits a's recorded version.
#[test]
fn variant_b_pure_override_is_clean_and_safe_to_finish() {
    let (p, _added) = shared_with_a_line("surv-pure", true);
    p.write("shared.txt", &numbered(&[(30, "B")]));
    let refused = seal_shared(&p, AGENT_A, "sa", &[]);
    assert!(!refused.status.success(), "{}", out_text(&refused));
    let ov = seal_shared(&p, AGENT_A, "sa", &["--allow-removals", "shared.txt"]);
    assert!(ov.status.success(), "{}", out_text(&ov));
    let newest = newest_seal(&p);
    p.ok(
        AGENT_A,
        &["spec", "done", "sa", "--agent", AGENT_A, "--no-seal"],
    );
    p.ok(AGENT_B, &["spec", "release", "sb", "--agent", AGENT_B]);

    let (code, v, human) = doctor_survival(&p, "human");
    assert_eq!(code, Some(0), "{v:#}");
    assert!(v["findings"].as_array().unwrap().is_empty(), "{v:#}");
    let want = format!(
        "fast checks clean; survival green as of {}; safe to finish",
        &newest[..12]
    );
    assert_eq!(v["headline"], want.as_str(), "{v:#}");
    assert!(human.contains(&want), "{human}");
    let head = p.head();
    p.ok("human", &["finish", "-y", "--no-check"]);
    assert_ne!(p.head(), head);
    assert_eq!(
        p.git_ok(&["show", "HEAD:shared.txt"]),
        numbered(&[(30, "B")])
    );
}

/// The tier's primary catch: a loss with no record. No override, no sealed
/// removal: the convergence output finish would commit drops a sealed line
/// (the merge refuses it). b seals, a seals
/// on b's tip, b seals again from a stale buffer (its recorded base is its own
/// first seal, which never held a's line). Today `doctor --survival` says
/// "survival green; safe to finish" and finish is then refused by the merge
/// survival check (Bri probe, 2026-10-11): the two must agree. Survival red
/// names a lost line and the seal that added it, "safe to finish" is absent
/// everywhere, finish refuses naming the same line, stages nothing, and
/// records finish_refused/survival_check.
#[test]
fn survival_red_without_override_matches_finish_refusal() {
    let p = Project::new("surv-nooverride");
    shared_base(&p);
    p.spec("sa", AGENT_A);
    p.spec("sb", AGENT_B);
    p.write("shared.txt", &numbered(&[(30, "B")]));
    p.seal(AGENT_B, "sb", "shared.txt");
    p.write("shared.txt", &numbered(&[(5, "alpha-from-a"), (30, "B")]));
    p.seal(AGENT_A, "sa", "shared.txt");
    p.write("shared.txt", &numbered(&[(30, "B2")]));
    p.seal(AGENT_B, "sb", "shared.txt");
    for (a, s) in [(AGENT_A, "sa"), (AGENT_B, "sb")] {
        p.ok(a, &["spec", "done", s, "--agent", a, "--no-seal"]);
    }

    let (code, v, human) = doctor_survival(&p, "human");
    assert_eq!(code, Some(1), "{v:#}");
    let f = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["check"] == CHECK_SURVIVAL && f["severity"] == "red")
        .cloned()
        .unwrap_or_else(|| panic!("no red survival finding:\n{v:#}\n{human}"));
    let msg = f["message"].as_str().unwrap().to_string();
    assert_never_safe(&p, "human", "survival red, no override");

    let head = p.head();
    let out = p.writ("human", &["finish", "-y", "--no-check"]);
    let all = out_text(&out);
    assert!(!out.status.success(), "finish committed:\n{all}");
    assert_eq!(p.head(), head);
    assert!(p
        .git_ok(&["diff", "--cached", "--name-only"])
        .trim()
        .is_empty());
    let named = ["line 5 alpha-from-a", "line 30 B2"]
        .into_iter()
        .filter(|l| msg.contains(l))
        .collect::<Vec<_>>();
    assert!(!named.is_empty(), "survival names no lost line: {msg}");
    for l in &named {
        assert!(
            all.contains(l),
            "finish refusal and survival disagree on {l}:\n{all}"
        );
    }
    assert!(!all.to_lowercase().contains(SAFE), "{all}");
    let events = fs::read_to_string(p.path(".writ/security/events.jsonl")).unwrap_or_default();
    assert!(
        events
            .lines()
            .any(|l| l.contains("\"finish_refused\"") && l.contains("survival_check")),
        "{events}"
    );
}

/// Finding 105, positive half (Haris, 20b18b922000): a seals "line 2 A"; b
/// seals its own version without it; a edits on top of b's version and is
/// refused (finding 102), closing with no override. Seal-tree convergence
/// keeps a's line: it reaches HEAD, and the tier and finish both say green.
#[test]
fn survival_green_when_convergence_keeps_the_line_finish_commits_it() {
    let (p, _added) = shared_with_a_line("f105-green", true);
    p.write("shared.txt", &numbered(&[(30, "B")]));
    p.seal(AGENT_B, "sb", "shared.txt");
    p.write("shared.txt", &numbered(&[(10, "A2"), (30, "B")]));
    let refused = seal_shared(&p, AGENT_A, "sa", &[]);
    assert!(
        !refused.status.success(),
        "finding 102 refusal did not fire:\n{}",
        out_text(&refused)
    );
    p.ok(
        AGENT_A,
        &["spec", "done", "sa", "--agent", AGENT_A, "--no-seal"],
    );
    p.ok(
        AGENT_B,
        &["spec", "done", "sb", "--agent", AGENT_B, "--no-seal"],
    );
    for s in log_all(&p) {
        assert!(
            s.get("forced").is_none() && s.get("allow_removals").is_none(),
            "override used: {s}"
        );
    }
    // a's refused edit is still on disk, unsealed; restore the sealed tree.
    p.write("shared.txt", &numbered(&[(30, "B")]));

    let (code, v, human) = doctor_survival(&p, "human");
    assert_eq!(code, Some(0), "{v:#}\n{human}");
    assert!(
        v["headline"]
            .as_str()
            .unwrap()
            .contains("survival green as of"),
        "{v:#}"
    );
    let out = p.ok("human", &["finish", "-y", "--no-check"]);
    let all = out_text(&out);
    let head = p.git_ok(&["show", "HEAD:shared.txt"]);
    assert!(
        head.lines().any(|l| l == "line 2 A"),
        "a's line not committed:\n{head}\n{all}"
    );
    assert!(
        head.lines().any(|l| l == "line 30 B"),
        "b's line not committed:\n{head}"
    );
}

/// Finding 105, red half: a loss with no record. b seals, a seals on b's tip,
/// b seals again from a stale buffer (its base, b's first seal, never held
/// a's line). No override, no informed removal of b's own "line 30 B2" in
/// the merge: the convergence output finish would commit drops it. The tier
/// is red and counts it (headline never "0 ... lost" next to a lost line),
/// naming the line and the seal that added it; finish refuses naming both,
/// stages nothing, and records finish_refused/survival_check.
#[test]
fn survival_red_when_the_merge_drops_a_sealed_line_names_line_and_seal() {
    let p = Project::new("f105-red");
    shared_base(&p);
    p.spec("sa", AGENT_A);
    p.spec("sb", AGENT_B);
    p.write("shared.txt", &numbered(&[(30, "B")]));
    p.seal(AGENT_B, "sb", "shared.txt");
    p.write("shared.txt", &numbered(&[(5, "alpha-from-a"), (30, "B")]));
    p.seal(AGENT_A, "sa", "shared.txt");
    p.write("shared.txt", &numbered(&[(30, "B2")]));
    p.seal(AGENT_B, "sb", "shared.txt");
    let b2 = newest_seal(&p);
    for (a, s) in [(AGENT_A, "sa"), (AGENT_B, "sb")] {
        p.ok(a, &["spec", "done", s, "--agent", a, "--no-seal"]);
    }
    for s in log_all(&p) {
        assert!(
            s.get("forced").is_none() && s.get("allow_removals").is_none(),
            "override used: {s}"
        );
    }

    let (code, v, human) = doctor_survival(&p, "human");
    assert_eq!(code, Some(1), "{v:#}\n{human}");
    let headline = v["headline"].as_str().unwrap();
    assert!(headline.contains("survival red"), "{headline}");
    assert!(
        !headline.contains("survival red: 0 "),
        "red headline counts no loss: {headline}"
    );
    let f = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["check"] == CHECK_SURVIVAL && f["severity"] == "red")
        .cloned()
        .unwrap_or_else(|| panic!("no red survival finding:\n{v:#}"));
    let msg = f["message"].as_str().unwrap();
    assert!(msg.contains("line 30 B2"), "{msg}");
    assert!(msg.contains(&b2[..12]), "adding seal {b2} not named: {msg}");
    assert!(
        finding_paths(&f).contains(&"shared.txt".to_string()),
        "{f:#}"
    );
    assert_never_safe(&p, "human", "finding 105 red");

    let head = p.head();
    let out = p.writ("human", &["finish", "-y", "--no-check"]);
    let all = out_text(&out);
    assert!(!out.status.success(), "finish committed the loss:\n{all}");
    assert_eq!(p.head(), head, "finish moved HEAD");
    assert!(
        p.git_ok(&["diff", "--cached", "--name-only"])
            .trim()
            .is_empty(),
        "staged on refusal"
    );
    assert!(all.contains("line 30 B2"), "{all}");
    assert!(
        all.contains(&b2[..12]),
        "refusal does not name adding seal {b2}:\n{all}"
    );
    assert!(!all.to_lowercase().contains(SAFE), "{all}");
    let events = fs::read_to_string(p.path(".writ/security/events.jsonl")).unwrap_or_default();
    assert!(
        events
            .lines()
            .any(|l| l.contains("\"finish_refused\"") && l.contains("survival_check")),
        "{events}"
    );
}

fn log_all(p: &Project) -> Vec<serde_json::Value> {
    let o = p.ok("human", &["log", "--all", "--format", "json"]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    v.as_array().cloned().unwrap_or_default()
}

/// Finish's merge notice must never claim a line "was kept" that the commit
/// lacks (Bri probe, 2026-10-11): a seals "line 5 alpha-from-a", b seals the
/// same line as "line 5 bravo-from-b" on top of it. Whatever finish commits,
/// a line the output says was kept is in HEAD.
#[test]
fn finish_notice_never_claims_a_dropped_line_was_kept() {
    let p = Project::new("notice-kept");
    shared_base(&p);
    p.spec("sa", AGENT_A);
    p.spec("sb", AGENT_B);
    p.write("shared.txt", &numbered(&[(5, "alpha-from-a")]));
    p.seal(AGENT_A, "sa", "shared.txt");
    p.write("shared.txt", &numbered(&[(5, "bravo-from-b")]));
    p.seal(AGENT_B, "sb", "shared.txt");
    for (a, s) in [(AGENT_A, "sa"), (AGENT_B, "sb")] {
        p.ok(a, &["spec", "done", s, "--agent", a, "--no-seal"]);
    }
    let out = p.writ("human", &["finish", "-y", "--no-check"]);
    let all = out_text(&out);
    if !out.status.success() {
        return; // a refusal makes no claim about the commit
    }
    let committed = p.git_ok(&["show", "HEAD:shared.txt"]);
    for line in all
        .lines()
        .filter(|l| l.contains("were kept") || l.contains("was kept"))
    {
        for quoted in line.split('"').skip(1).step_by(2) {
            assert!(
                committed.lines().any(|c| c == quoted),
                "notice says {quoted:?} was kept; HEAD lacks it:\n{line}\n--- HEAD ---\n{committed}"
            );
        }
    }
}

/// Green: two specs on separate files, both done. `writ doctor --survival`
/// is clean, `survival_last_green` is the newest seal, the headline reads
/// "fast checks clean; survival green as of <seal>", and only then does
/// "safe to finish" appear. finish commits both.
#[test]
fn survival_tier_green_passes_and_is_the_only_safe_to_finish() {
    let p = Project::new("surv-green");
    p.spec("sa", "a");
    p.spec("sb", "b");
    p.write("a.txt", "a1\n");
    p.write("b.txt", "b1\n");
    p.seal("a", "sa", "a.txt");
    p.seal("b", "sb", "b.txt");
    for (a, s) in [("a", "sa"), ("b", "sb")] {
        p.ok(a, &["spec", "done", s, "--agent", a, "--no-seal"]);
    }
    let newest = newest_seal(&p);
    let (code, v, human) = doctor_survival(&p, "human");
    assert_eq!(code, Some(0), "{v:#}");
    assert_eq!(v["clean"], true, "{v:#}");
    let green = v["survival_last_green"].as_str().unwrap_or("");
    assert!(
        !green.is_empty() && (newest.starts_with(green) || green.starts_with(&newest[..12])),
        "survival_last_green {green:?} is not the newest seal {newest}"
    );
    let headline = v["headline"].as_str().unwrap();
    assert!(
        headline.starts_with("fast checks clean; survival green as of ")
            && headline.contains(&newest[..12]),
        "{headline}"
    );
    assert!(
        human.to_lowercase().contains(SAFE),
        "positive case missing:\n{human}"
    );
    let head = p.head();
    p.ok("human", &["finish", "-y", "--no-check"]);
    assert_ne!(p.head(), head);
    assert_eq!(p.git_ok(&["show", "HEAD:a.txt"]), "a1\n");
    assert_eq!(p.git_ok(&["show", "HEAD:b.txt"]), "b1\n");
}

/// "safe to finish" is absent when survival is unavailable (never run),
/// stale (a seal after the last green), or red, on every surface: doctor
/// human and json, context brief, toon and json (scoped and not), status.
#[test]
fn safe_to_finish_absent_when_survival_unavailable_or_stale() {
    let p = Project::new("surv-honesty");
    p.spec("sa", "a");
    p.write("a.txt", "a1\n");
    p.seal("a", "sa", "a.txt");
    p.ok("a", &["spec", "done", "sa", "--agent", "a", "--no-seal"]);
    // unavailable: the survival tier has never run here
    let (_, v) = p.doctor("human");
    assert!(v["survival_last_green"].is_null(), "{v:#}");
    assert_never_safe(&p, "human", "survival never run");
    // green, then stale: a new seal after the green result
    let (code, v, _) = doctor_survival(&p, "human");
    assert_eq!(code, Some(0), "{v:#}");
    p.spec("sb", "b");
    p.write("b.txt", "b1\n");
    p.seal("b", "sb", "b.txt");
    let (_, v) = p.doctor("human");
    let green = v["survival_last_green"].as_str().unwrap_or("");
    let newest = newest_seal(&p);
    assert!(
        !newest.starts_with(green) || green.is_empty(),
        "survival_last_green claims the newest seal {newest} without a run"
    );
    assert_never_safe(&p, "human", "survival stale (seal after last green)");
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
