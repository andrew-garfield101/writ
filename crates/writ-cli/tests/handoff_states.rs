//! `writ handoff` and `writ why` (sprint 5, U.3; Amis's `act-on-it` items 2
//! and 3). Assertions written before the code (retro rule 1); each is ignored
//! on EXPECTED_IGNORED in bench/test-gate.sh until it lands.
//!
//! Fixture (three specs, two shared files, one override after being told):
//! - git base: shared.txt (40 numbered lines), two.txt (20 numbered lines);
//! - sa (alpha) seals "line 2 A" in shared.txt;
//! - sb (bravo) seals its own shared.txt ("line 30 B", without a's line) and
//!   "line 5 B" in two.txt;
//! - sc (charlie) seals two.txt on top of sb's ("line 15 C") and c.txt;
//! - sa writes over shared.txt ("line 10 A2" + b's line, its own "line 2 A"
//!   gone): the seal is refused naming bravo (finding 102), then sa seals with
//!   `--allow-removals shared.txt`: accepted bravo's version after being told;
//! - all three done; `writ finish -y` commits them (finish prints "alpha
//!   accepted bravo's version of shared.txt after being told").
//!
//! The finish audit (bench/finish_audit.py) on that commit names specs
//! [sa, sb, sc] and two files sealed by more than one spec [shared.txt,
//! two.txt], 0 lost: handoff must name exactly those (Aubs's criterion).
//!
//! JSON shape asserted (rename here if Amis's code differs; the content is
//! the contract, not the key names):
//! handoff: { agents: [str], specs: [{id, title, seals, state}],
//!   merges: [{path, specs: [str], resolution, detail}], stale_rewrites: [..],
//!   unsealed: [{path, agent}], doctor: {headline, ..}, next_action: str }
//!   with resolution one of "kept", "superseded", "accepted_after_told".
//! why: { path, history: [{seal, spec, agent, added: [str], removed: [str],
//!   override: obj|null}], merge: obj|null }
//! doctor --format json gains `next_action` (Aubs 3: handoff's next action
//! equals doctor's on every state).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::SystemTime;

use serde_json::Value;

const ALPHA: &str = "alpha";
const BRAVO: &str = "bravo";
const CHARLIE: &str = "charlie";
/// The 102 override_context wording finish already prints for sa's seal.
const TOLD: &str = "alpha accepted bravo's version of shared.txt after being told";

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
        let base =
            std::env::temp_dir().join(format!("writ-handoff-{tag}-{}-{nanos}", std::process::id()));
        let (root, home, bin) = (base.join("repo"), base.join("home"), base.join("bin"));
        for d in [&root, &home, &bin] {
            fs::create_dir_all(d).unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_writ"), bin.join("writ")).unwrap();
        let p = Self { root, home, bin };
        p.git_ok(&["init", "-q", "-b", "main"]);
        p.git_ok(&["config", "user.name", "fixture"]);
        p.git_ok(&["config", "user.email", "fixture@example.invalid"]);
        p.write("README.md", "# fixture\n");
        p.write(".gitignore", "target/\n");
        p.write("shared.txt", &numbered(40, &[]));
        p.write("two.txt", &numbered(20, &[]));
        p.git_ok(&["add", "-A"]);
        p.git_ok(&["commit", "-q", "-m", "init"]);
        p.ok("human", &["init", "-y"]);
        // Ruling 183: these fixtures build concurrent versions by hand
        // sealing; the test-fixture switch turns the seal-time
        // foreign-line check off (writ init never writes it).
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p.root.join(".writ").join("config.toml"))
                .unwrap();
            writeln!(f, "\n[seal]\nforeign_line_check = false").unwrap();
        }
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
            "writ {args:?} as {agent} failed ({:?}):\n{}",
            out.status.code(),
            text(&out)
        );
        out
    }

    fn json(&self, agent: &str, args: &[&str]) -> Value {
        let out = self.writ(agent, args);
        serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("writ {args:?} is not JSON ({e}):\n{}", text(&out)))
    }

    fn git_ok(&self, args: &[&str]) -> String {
        let out = self.cmd("git", "human").args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", text(&out));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn head(&self) -> String {
        self.git_ok(&["rev-parse", "HEAD"]).trim().to_string()
    }

    fn spec(&self, id: &str, agent: &str) {
        self.ok(
            agent,
            &["spec", "add", "--id", id, "--title", &format!("spec {id}")],
        );
        self.ok(agent, &["spec", "claim", id, "--agent", agent]);
    }

    fn seal(&self, agent: &str, spec: &str, paths: &str, extra: &[&str]) -> Output {
        let mut args = vec![
            "seal", "-s", "work", "--agent", agent, "--spec", spec, "--paths", paths,
        ];
        args.extend_from_slice(extra);
        self.writ(agent, &args)
    }

    fn newest_seal(&self) -> String {
        let v = self.json(
            "human",
            &["log", "--all", "--format", "json", "--limit", "1"],
        );
        v[0]["id"].as_str().unwrap().to_string()
    }

    /// Hash of every file under the store's durable parts (objects, seals,
    /// specs): a diagnostic that writes here breaks "no new state".
    fn store_snapshot(&self) -> BTreeMap<String, Vec<u8>> {
        let mut out = BTreeMap::new();
        for part in ["objects", "seals", "specs"] {
            walk(&self.path(&format!(".writ/{part}")), &mut out);
        }
        out
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        if std::env::var_os("WRIT_KEEP_FIXTURES").is_none() {
            let _ = fs::remove_dir_all(self.root.parent().unwrap());
        }
    }
}

fn walk(dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.insert(p.display().to_string(), fs::read(&p).unwrap_or_default());
        }
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn numbered(n: usize, edits: &[(usize, &str)]) -> String {
    (0..n)
        .map(|i| match edits.iter().find(|(k, _)| *k == i) {
            Some((_, s)) => format!("line {i} {s}\n"),
            None => format!("line {i}\n"),
        })
        .collect()
}

/// The fixture above, up to and including the finish. Returns the project
/// and the id of the newest seal before any spec work (the `--since` point).
fn three_specs_two_shared_one_override(tag: &str, finish: bool) -> (Project, String) {
    fixture(tag, finish, Rewrite::None)
}

#[derive(PartialEq, Clone, Copy)]
enum Rewrite {
    None,
    /// bravo moves its own edit; charlie kept all of bravo's lines.
    Own,
    /// alpha seals "line 2 A" and "line 20 A"; bravo's version drops 2 A and
    /// keeps 20 A; alpha's override takes bravo's version for 2 A and
    /// removes its own 20 A on purpose.
    Mixed,
}

/// Finding 122, mixed wording (CC): "accepted bravo's version for 1
/// line(s); removed 1 own line(s) on purpose".
fn with_mixed_rewrite(tag: &str, finish: bool) -> (Project, String) {
    fixture(tag, finish, Rewrite::Mixed)
}

/// Finding 122 variant: after the fixture's seals, sb moves its own edit in
/// two.txt ("line 5 B" back to base, new "line 7 B2"), keeping charlie's
/// "line 15 C". The
/// seal is refused naming charlie (charlie touched two.txt since sb's last
/// seal) and sb overrides: an own rewrite after being told, not accepting
/// charlie's version (the replacement content is sb's own).
fn with_own_rewrite(tag: &str, finish: bool) -> (Project, String) {
    fixture(tag, finish, Rewrite::Own)
}

fn fixture(tag: &str, finish: bool, rewrite: Rewrite) -> (Project, String) {
    let p = Project::new(tag);
    let since = p.newest_seal();
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    p.spec("sc", CHARLIE);
    // Mixed: alpha also seals "line 20 A", which bravo's version keeps and
    // alpha's override later removes itself (its own line, on purpose).
    let mixed = rewrite == Rewrite::Mixed;
    let a_lines: &[(usize, &str)] = if mixed {
        &[(2, "A"), (20, "A")]
    } else {
        &[(2, "A")]
    };
    let b_lines: &[(usize, &str)] = if mixed {
        &[(20, "A"), (30, "B")]
    } else {
        &[(30, "B")]
    };
    p.write("shared.txt", &numbered(40, a_lines));
    assert!(p.seal(ALPHA, "sa", "shared.txt", &[]).status.success());
    p.write("shared.txt", &numbered(40, b_lines));
    assert!(p.seal(BRAVO, "sb", "shared.txt", &[]).status.success());
    p.write("two.txt", &numbered(20, &[(5, "B")]));
    assert!(p.seal(BRAVO, "sb", "two.txt", &[]).status.success());
    p.write("two.txt", &numbered(20, &[(5, "B"), (15, "C")]));
    p.write("c.txt", "c\n");
    assert!(p.seal(CHARLIE, "sc", "two.txt,c.txt", &[]).status.success());
    p.write("shared.txt", &numbered(40, &[(10, "A2"), (30, "B")]));
    let refused = p.seal(ALPHA, "sa", "shared.txt", &[]);
    assert!(
        !refused.status.success(),
        "fixture: a was not told:\n{}",
        text(&refused)
    );
    assert!(text(&refused).contains(BRAVO), "{}", text(&refused));
    let ov = p.seal(
        ALPHA,
        "sa",
        "shared.txt",
        &["--allow-removals", "shared.txt"],
    );
    assert!(ov.status.success(), "{}", text(&ov));
    if rewrite == Rewrite::Own {
        // b moves its own edit: "line 5 B" goes back to the base "line 5"
        // and "line 7 B2" is new, b's own; charlie's "line 15 C" stays.
        p.write("two.txt", &numbered(20, &[(7, "B2"), (15, "C")]));
        let r = p.seal(BRAVO, "sb", "two.txt", &[]);
        assert!(
            !r.status.success(),
            "fixture: b was not told:\n{}",
            text(&r)
        );
        assert!(text(&r).contains(CHARLIE), "{}", text(&r));
        let ov = p.seal(BRAVO, "sb", "two.txt", &["--allow-removals", "two.txt"]);
        assert!(ov.status.success(), "{}", text(&ov));
    }
    for (s, a) in [("sa", ALPHA), ("sb", BRAVO), ("sc", CHARLIE)] {
        p.ok(a, &["spec", "done", s, "--agent", a, "--no-seal"]);
    }
    if finish {
        let fin = p.ok("human", &["finish", "-y"]);
        assert!(
            text(&fin).contains(TOLD),
            "fixture: finish wording moved:\n{}",
            text(&fin)
        );
    }
    (p, since)
}

/// bench/finish_audit.py's view of a finish commit: (specs, files sealed by
/// more than one spec, lost).
fn audit(p: &Project, commit: &str) -> (Vec<String>, Vec<String>, usize) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/finish_audit.py");
    let out = Command::new("python3")
        .arg(&script)
        .arg(commit)
        .env("WRIT_AUDIT_REPO", &p.root)
        .output()
        .unwrap();
    let t = text(&out);
    assert!(out.status.success(), "finish_audit.py: {t}");
    let list = |line: &str, key: &str| -> Vec<String> {
        let tail = &line[line.find(key).unwrap() + key.len()..];
        let inner = &tail[tail.find('[').unwrap() + 1..tail.find(']').unwrap()];
        inner
            .split(',')
            .map(|s| s.trim().trim_matches('\'').to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };
    let specs = list(t.lines().find(|l| l.contains("specs=")).unwrap(), "specs=");
    let multi = list(
        t.lines()
            .find(|l| l.contains("sealed by >1 spec:"))
            .unwrap(),
        "sealed by >1 spec:",
    );
    let lost: usize = t
        .lines()
        .find_map(|l| l.split("LOST: ").nth(1))
        .and_then(|n| n.trim().parse().ok())
        .unwrap();
    (specs, multi, lost)
}

fn ids(v: &Value, key: &str, field: &str) -> Vec<String> {
    let mut out: Vec<String> = v[key]
        .as_array()
        .unwrap_or_else(|| panic!("handoff json has no `{key}` array:\n{v:#}"))
        .iter()
        .map(|x| x[field].as_str().unwrap_or_default().to_string())
        .collect();
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// writ handoff
// ---------------------------------------------------------------------------

/// Aubs's criterion on a fixture: handoff names every finished spec and every
/// merge, exactly the finish audit's set, with how each merge resolved; the
/// override is worded as finish words it; no store write.
#[test]
fn handoff_names_every_finished_spec_and_merge_matching_the_audit() {
    let (p, since) = three_specs_two_shared_one_override("handoff-audit", true);
    let (audit_specs, audit_multi, lost) = audit(&p, &p.head());
    assert_eq!(audit_specs, ["sa", "sb", "sc"], "fixture");
    assert_eq!(audit_multi, ["shared.txt", "two.txt"], "fixture");
    assert_eq!(lost, 0, "fixture");

    let before = p.store_snapshot();
    let v = p.json("human", &["handoff", "--since", &since, "--format", "json"]);
    assert_eq!(p.store_snapshot(), before, "handoff wrote to the store");

    assert_eq!(ids(&v, "specs", "id"), audit_specs, "{v:#}");
    for s in v["specs"].as_array().unwrap() {
        assert!(
            s["title"].as_str().is_some_and(|t| !t.is_empty()),
            "spec without a title: {s:#}"
        );
        assert!(s["seals"].as_u64().is_some_and(|n| n >= 1), "{s:#}");
        assert!(
            s["state"].as_str().unwrap_or("").contains("commit"),
            "finished spec not shown as committed: {s:#}"
        );
    }
    let agents = v["agents"].to_string();
    for a in [ALPHA, BRAVO, CHARLIE] {
        assert!(agents.contains(a), "agent {a} not named: {agents}");
    }
    assert_eq!(ids(&v, "merges", "path"), audit_multi, "{v:#}");
    let merge = |path: &str| -> Value {
        v["merges"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["path"] == path)
            .cloned()
            .unwrap()
    };
    let shared = merge("shared.txt");
    assert_eq!(shared["resolution"], "accepted_after_told", "{shared:#}");
    assert!(
        shared["detail"].as_str().unwrap_or("").contains(TOLD),
        "{shared:#}"
    );
    let two = merge("two.txt");
    assert_eq!(two["resolution"], "kept", "{two:#}");
    for m in [&shared, &two] {
        let specs = m["specs"].to_string();
        assert!(specs.contains("sb"), "{m:#}");
    }
    assert!(
        v["unsealed"].as_array().is_some_and(|a| a.is_empty()),
        "{v:#}"
    );
    assert!(v["doctor"]["headline"].is_string(), "{v:#}");

    // Markdown by default, and brief.
    let md = text(&p.ok("human", &["handoff", "--since", &since]));
    for want in [
        "sa",
        "sb",
        "sc",
        "shared.txt",
        "two.txt",
        TOLD,
        ALPHA,
        BRAVO,
        CHARLIE,
    ] {
        assert!(md.contains(want), "markdown handoff lacks {want:?}:\n{md}");
    }
    let brief = p.ok(
        "human",
        &["handoff", "--since", &since, "--format", "brief"],
    );
    let b = text(&brief);
    assert!(
        brief.stdout.len() <= 2048,
        "brief is {} bytes",
        brief.stdout.len()
    );
    for want in ["sa", "sb", "sc"] {
        assert!(b.contains(want), "brief lacks {want}:\n{b}");
    }
    assert_eq!(
        p.store_snapshot(),
        before,
        "handoff (md/brief) wrote to the store"
    );
}

/// `--since last-finish` after the finish covers nothing new; on the writ
/// path it must not error and must not invent specs.
#[test]
fn handoff_since_last_finish_after_finish_lists_no_specs() {
    let (p, _) = three_specs_two_shared_one_override("handoff-last", true);
    let v = p.json(
        "human",
        &["handoff", "--since", "last-finish", "--format", "json"],
    );
    assert!(v["specs"].as_array().is_some_and(|a| a.is_empty()), "{v:#}");
}

/// Unsealed work and by whom: an edit nobody sealed is listed under
/// `unsealed` with the agent whose spec last sealed the file.
#[test]
fn handoff_lists_unsealed_work_and_whose() {
    let (p, since) = three_specs_two_shared_one_override("handoff-unsealed", false);
    p.write("c.txt", "c\nc2\n");
    let v = p.json("human", &["handoff", "--since", &since, "--format", "json"]);
    let u = v["unsealed"].to_string();
    assert!(u.contains("c.txt") && u.contains(CHARLIE), "{v:#}");
}

/// Aubs 3: handoff's next action equals doctor's on every state, and on a
/// clean repo it is finish or nothing, never converge. States: completed
/// specs waiting for finish; an exited-session stale claim; clean after the
/// finish.
#[test]
fn handoff_next_action_equals_doctor_on_every_state() {
    let check = |p: &Project, since: &str, when: &str| -> String {
        // Ruling 183: the fixture-wide switch shows as a seal_check_disabled
        // yellow that is not part of this comparison; lift the key for the
        // two reads, then put it back for the seals that follow.
        let cfg = p.path(".writ/config.toml");
        let orig = fs::read_to_string(&cfg).unwrap();
        fs::write(&cfg, orig.replace("\n[seal]\nforeign_line_check = false\n", "")).unwrap();
        let h = p.json("human", &["handoff", "--since", since, "--format", "json"]);
        let d = p.json("human", &["doctor", "--format", "json"]);
        fs::write(&cfg, &orig).unwrap();
        let hn = h["next_action"]
            .as_str()
            .unwrap_or_else(|| panic!("{when}: handoff has no next_action:\n{h:#}"));
        let dn = d["next_action"]
            .as_str()
            .unwrap_or_else(|| panic!("{when}: doctor has no next_action:\n{d:#}"));
        assert_eq!(hn, dn, "{when}: handoff and doctor disagree");
        hn.to_string()
    };

    let (p, since) = three_specs_two_shared_one_override("handoff-next", false);
    let waiting = check(&p, &since, "completed specs waiting");
    assert!(!waiting.to_lowercase().contains("converge"), "{waiting}");

    p.ok("human", &["finish", "-y"]);
    let clean = check(&p, &since, "clean after finish");
    let lc = clean.to_lowercase();
    assert!(
        !lc.contains("converge"),
        "clean repo says converge: {clean}"
    );
    assert!(
        lc.is_empty() || lc.contains("finish") || lc.contains("nothing"),
        "clean repo next action is not finish or nothing: {clean}"
    );

    p.ok(
        "human",
        &["spec", "add", "--id", "ghost", "--title", "ghost"],
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink("/bin/sh", p.bin.join("claude")).unwrap();
    let s = p
        .cmd(p.bin.join("claude").to_str().unwrap(), "ghost-session")
        .args(["-c", "writ spec claim ghost --agent ghost-session && :"])
        .output()
        .unwrap();
    assert!(s.status.success(), "{}", text(&s));
    let stale = check(&p, &since, "exited-session claim");
    let d = p.json("human", &["doctor", "--format", "json"]);
    let fix = d["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["check"] == "stale_claim")
        .and_then(|f| f["fix_command"].as_str())
        .unwrap()
        .to_string();
    assert_eq!(stale, fix, "next action is not the stale_claim fix");
}

/// Finding 122: an own rewrite under override is not "accepted <other>'s
/// version". Finish, handoff and why say bravo removed its own line on
/// purpose after being told (charlie as context only); handoff resolution
/// "rewrote_own"; the genuine acceptance (sa on shared.txt) keeps
/// "accepted_after_told". Negative: no surface says bravo accepted
/// charlie's version.
#[test]
fn f122_own_rewrite_override_is_rewrote_own_not_accepted() {
    let (p, since) = with_own_rewrite("f122", false);
    let fin = p.ok("human", &["finish", "-y"]);
    let ft = text(&fin);
    const WRONG: &str = "bravo accepted charlie's version";
    assert!(
        !ft.contains(WRONG),
        "finish says bravo accepted charlie's version:\n{ft}"
    );
    assert!(
        ft.contains(TOLD),
        "the genuine acceptance lost its wording:\n{ft}"
    );

    let v = p.json("human", &["handoff", "--since", &since, "--format", "json"]);
    let merge = |path: &str| -> Value {
        v["merges"]
            .as_array()
            .unwrap_or_else(|| panic!("{v:#}"))
            .iter()
            .find(|m| m["path"] == path)
            .cloned()
            .unwrap_or_else(|| panic!("{path} not in merges:\n{v:#}"))
    };
    assert_eq!(
        merge("shared.txt")["resolution"],
        "accepted_after_told",
        "{v:#}"
    );
    let two = merge("two.txt");
    assert_eq!(two["resolution"], "rewrote_own", "{two:#}");
    let detail = two["detail"].as_str().unwrap_or("");
    assert!(
        detail.contains("own")
            && detail.contains("after being told")
            && !detail.contains("accepted"),
        "two.txt detail: {detail}"
    );
    let md = text(&p.ok("human", &["handoff", "--since", &since]));
    assert!(!md.contains(WRONG), "markdown handoff:\n{md}");

    let w = p.json("human", &["why", "two.txt", "--format", "json"]);
    let last_b = w["history"]
        .as_array()
        .unwrap_or_else(|| panic!("{w:#}"))
        .iter()
        .filter(|e| e["spec"] == "sb")
        .last()
        .cloned()
        .unwrap();
    let ov = &last_b["override"];
    let detail = ov["detail"].as_str().unwrap_or("");
    assert!(
        detail.contains("own") && !detail.contains("accepted"),
        "why two.txt override for bravo's rewrite: {ov}"
    );
    assert!(
        ov["accepted_lines"].as_array().is_none_or(|a| a.is_empty()),
        "own rewrite lists accepted lines: {ov}"
    );
    let wmd = text(&p.ok("human", &["why", "two.txt"]));
    assert!(!wmd.contains(WRONG), "markdown why:\n{wmd}");
}

/// Finding 122, mixed case: one line taken from bravo's version, one own
/// line removed. Finish (and handoff detail) say both, with counts; neither
/// the bare "accepted charlie's version" sentence nor a bare own-rewrite.
#[test]
fn f122_mixed_override_says_both_with_counts() {
    let (p, since) = with_mixed_rewrite("f122-mixed", false);
    let fin = p.ok("human", &["finish", "-y"]);
    let ft = text(&fin);
    for want in [
        "alpha accepted bravo's version of shared.txt for 1 line",
        "removed 1 own line",
    ] {
        assert!(ft.contains(want), "finish lacks {want:?}:\n{ft}");
    }
    let v = p.json("human", &["handoff", "--since", &since, "--format", "json"]);
    let two = v["merges"]
        .as_array()
        .unwrap_or_else(|| panic!("{v:#}"))
        .iter()
        .find(|m| m["path"] == "shared.txt")
        .cloned()
        .unwrap_or_else(|| panic!("{v:#}"));
    let d = two["detail"].as_str().unwrap_or("");
    assert!(
        d.contains("for 1 line") && d.contains("removed 1 own line"),
        "handoff shared.txt detail: {d}"
    );
}

// ---------------------------------------------------------------------------
// writ why
// ---------------------------------------------------------------------------

fn history_specs(v: &Value) -> Vec<String> {
    v["history"]
        .as_array()
        .unwrap_or_else(|| panic!("why json has no `history` array:\n{v:#}"))
        .iter()
        .filter_map(|e| e["spec"].as_str().map(str::to_string))
        .collect()
}

fn has_line(e: &Value, key: &str, line: &str) -> bool {
    e[key].as_array().is_some_and(|a| {
        a.iter()
            .any(|l| l.as_str().is_some_and(|s| s.contains(line)))
    })
}

/// A file sealed by two specs with one override: the specs in order, what
/// each changed, the merge that produced the current content, the override.
#[test]
fn why_shared_file_two_specs_one_override() {
    let (p, _) = three_specs_two_shared_one_override("why-shared", true);
    let before = p.store_snapshot();
    let v = p.json("human", &["why", "shared.txt", "--format", "json"]);
    assert_eq!(p.store_snapshot(), before, "why wrote to the store");
    assert_eq!(v["path"], "shared.txt", "{v:#}");
    assert_eq!(history_specs(&v), ["sa", "sb", "sa"], "order: {v:#}");
    let h: Vec<&Value> = v["history"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["spec"].is_string())
        .collect();
    assert!(has_line(h[0], "added", "line 2 A"), "{:#}", h[0]);
    assert_eq!(h[0]["agent"], ALPHA);
    assert!(has_line(h[1], "added", "line 30 B"), "{:#}", h[1]);
    assert_eq!(h[1]["agent"], BRAVO);
    assert!(has_line(h[2], "removed", "line 2 A"), "{:#}", h[2]);
    assert!(has_line(h[2], "added", "line 10 A2"), "{:#}", h[2]);
    for e in &h[..2] {
        assert!(
            e["override"].is_null() || e["override"] == false,
            "override on a plain seal: {e:#}"
        );
    }
    let ov = h[2]["override"].to_string();
    for want in [BRAVO, "line 2 A", "after being told"] {
        assert!(ov.contains(want), "override lacks {want:?}: {ov}");
    }
    let merge = v["merge"].to_string();
    let head = p.head();
    assert!(
        merge.contains(&head[..7]) && merge.contains("sa") && merge.contains("sb"),
        "merge does not name the finish commit and both specs: {merge}"
    );

    let md = text(&p.ok("human", &["why", "shared.txt"]));
    for want in [
        "sa",
        "sb",
        "line 2 A",
        "line 30 B",
        "after being told",
        &head[..7],
    ] {
        assert!(md.contains(want), "markdown why lacks {want:?}:\n{md}");
    }
}

/// The other shared file: sb then sc (built on sb's version), merge kept
/// both, no override anywhere.
#[test]
fn why_descendant_file_has_no_override_and_kept_merge() {
    let (p, _) = three_specs_two_shared_one_override("why-two", true);
    let v = p.json("human", &["why", "two.txt", "--format", "json"]);
    assert_eq!(history_specs(&v), ["sb", "sc"], "{v:#}");
    for e in v["history"].as_array().unwrap() {
        assert!(e["override"].is_null() || e["override"] == false, "{e:#}");
    }
    assert!(v["merge"].to_string().contains("kept"), "{v:#}");
}

// ---------------------------------------------------------------------------
// Harness self-check (always on): the fixture and the audit agree.
// ---------------------------------------------------------------------------

#[test]
fn harness_handoff_fixture_matches_finish_audit() {
    let (p, _) = three_specs_two_shared_one_override("self", true);
    let (specs, multi, lost) = audit(&p, &p.head());
    assert_eq!(specs, ["sa", "sb", "sc"]);
    assert_eq!(multi, ["shared.txt", "two.txt"]);
    assert_eq!(lost, 0);
    assert_eq!(
        p.git_ok(&["show", "HEAD:shared.txt"]),
        numbered(40, &[(10, "A2"), (30, "B")])
    );
    assert_eq!(
        p.git_ok(&["show", "HEAD:two.txt"]),
        numbered(20, &[(5, "B"), (15, "C")])
    );
}
