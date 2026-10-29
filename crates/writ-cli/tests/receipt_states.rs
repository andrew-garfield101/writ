//! The receipt (sprint 6, 0.4.3; Haris's `receipt-and-scale` item 1, plus
//! Aubs's receipt rule). Assertions written before the code (retro rule 1);
//! each is ignored on EXPECTED_IGNORED in bench/test-gate.sh until it lands.
//!
//! Fixture: handoff_states.rs's three specs, two shared files, one override
//! after being told, with closing words: sa (alpha) `spec done -s "alpha
//! words: moved the parser"`, sb (bravo) `-s "bravo words: two.txt and the
//! tail"`, sc (charlie) closed with no summary. `writ finish -y --receipt
//! <scratch>/receipt.md` commits them.
//!
//! Contract (rename the format/flag here if Haris's code differs; content is
//! the contract):
//! - the finish commit body is the receipt: `writ handoff --since <seal
//!   before the specs> --format receipt`, run after the finish, prints the
//!   same text byte for byte (one code path);
//! - `--receipt <path>` writes the PR markdown, equal to `writ handoff
//!   --since <same> --format pr`;
//! - under 40 lines; the spec list first; one line per spec carrying id,
//!   agent, seal count, and the agent's closing words verbatim, or "no
//!   summary from the agent" when there are none (never a synthesized
//!   sentence); an auto spec with no `spec done` reads "no summary from the
//!   agent; N auto seals over <span>";
//! - one line per shared file with its resolution: shared.txt "accepted
//!   after being told" naming alpha (override by alpha), two.txt "kept";
//! - the refusal during the range (alpha on shared.txt) is listed;
//! - the survival line equals the one the finish printed ("N sealed line(s)
//!   of M spec(s) checked, none lost"), and bench/finish_audit.py agrees (0
//!   lost, the same specs);
//! - records only: deterministic (two runs identical), nothing from outside
//!   the range, no agent that is not in the records.

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
    #[allow(dead_code)]
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

#[allow(dead_code)]
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

const A_WORDS: &str = "alpha words: moved the parser";
const B_WORDS: &str = "bravo words: two.txt and the tail";
const NO_SUMMARY: &str = "no summary from the agent";

struct Finished {
    p: Project,
    since: String,
    commit: String,
    finish_out: String,
    receipt_md: String,
}

/// handoff_states.rs's fixture with closing words, finished with --receipt.
fn finished(tag: &str) -> Finished {
    let p = Project::new(tag);
    let since = p.newest_seal();
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    p.spec("sc", CHARLIE);
    p.write("shared.txt", &numbered(40, &[(2, "A")]));
    assert!(p.seal(ALPHA, "sa", "shared.txt", &[]).status.success());
    p.write("shared.txt", &numbered(40, &[(30, "B")]));
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
    let ov = p.seal(
        ALPHA,
        "sa",
        "shared.txt",
        &["--allow-removals", "shared.txt"],
    );
    assert!(ov.status.success(), "{}", text(&ov));
    p.ok(
        ALPHA,
        &[
            "spec",
            "done",
            "sa",
            "--agent",
            ALPHA,
            "--no-seal",
            "-s",
            A_WORDS,
        ],
    );
    p.ok(
        BRAVO,
        &[
            "spec",
            "done",
            "sb",
            "--agent",
            BRAVO,
            "--no-seal",
            "-s",
            B_WORDS,
        ],
    );
    p.ok(
        CHARLIE,
        &["spec", "done", "sc", "--agent", CHARLIE, "--no-seal"],
    );
    let md_path = p.home.join("receipt.md");
    let fin = p.ok(
        "human",
        &["finish", "-y", "--receipt", md_path.to_str().unwrap()],
    );
    let commit = p.head();
    let receipt_md = fs::read_to_string(&md_path).unwrap_or_default();
    Finished {
        since,
        commit,
        finish_out: text(&fin),
        receipt_md,
        p,
    }
}

fn body(p: &Project, commit: &str) -> String {
    p.git_ok(&["log", "-1", "--format=%b", commit])
        .trim()
        .to_string()
}

fn receipt(f: &Finished) -> String {
    text(&f.p.ok(
        "human",
        &["handoff", "--since", &f.since, "--format", "receipt"],
    ))
    .trim()
    .to_string()
}

/// The shared-file line: the one whose first word (after a list marker) is
/// the path, not a spec line whose words happen to mention it.
fn file_line<'a>(body: &'a str, path: &str) -> &'a str {
    body.lines()
        .find(|l| {
            l.trim_start()
                .trim_start_matches(['-', '*', ' '])
                .trim_start_matches('`')
                .starts_with(path)
        })
        .unwrap_or_else(|| panic!("no shared-file line for {path}:\n{body}"))
}

fn line_with<'a>(body: &'a str, needle: &str) -> &'a str {
    body.lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no receipt line contains {needle:?}:\n{body}"))
}

/// The survival line the finish printed, e.g. "4 sealed line(s) of 1
/// spec(s) checked, none lost".
fn finish_survival(out: &str) -> String {
    let l = out
        .lines()
        .find(|l| l.contains("survival:") && l.contains("checked"))
        .unwrap_or_else(|| panic!("finish printed no survival line:\n{out}"));
    let s = &l[l.find("survival:").unwrap() + "survival:".len()..];
    let s = s.split(" (").next().unwrap();
    s.trim().to_string()
}

#[test]
fn rc_commit_body_equals_handoff_receipt_for_the_same_range() {
    let f = finished("equal");
    let b = body(&f.p, &f.commit);
    assert!(!b.is_empty(), "finish commit has no body");
    assert_eq!(
        b,
        receipt(&f),
        "commit body differs from handoff --format receipt"
    );
    // Records only: deterministic.
    assert_eq!(receipt(&f), receipt(&f));
}

#[test]
fn rc_receipt_file_equals_handoff_pr_format() {
    let f = finished("pr");
    assert!(!f.receipt_md.trim().is_empty(), "--receipt wrote nothing");
    let pr = text(&f.p.ok("human", &["handoff", "--since", &f.since, "--format", "pr"]));
    assert_eq!(f.receipt_md.trim(), pr.trim());
}

#[test]
fn rc_under_forty_lines_spec_list_first_one_line_per_spec() {
    let f = finished("shape");
    let b = body(&f.p, &f.commit);
    assert!(b.lines().count() < 40, "{} lines:\n{b}", b.lines().count());
    let first_spec = b
        .lines()
        .position(|l| l.contains("sa") || l.contains("sb") || l.contains("sc"));
    let first_file = b
        .lines()
        .position(|l| l.contains("shared.txt") || l.contains("two.txt"));
    assert!(
        first_spec.is_some() && first_spec < first_file,
        "spec list must come first:\n{b}"
    );
    for (id, agent, seals) in [("sa", ALPHA, 2), ("sb", BRAVO, 2), ("sc", CHARLIE, 1)] {
        let n = b
            .lines()
            .filter(|l| l.contains(&format!("{id} ")) || l.contains(&format!("`{id}`")))
            .count();
        assert!(n >= 1, "no line for {id}:\n{b}");
        let l = line_with(&b, &format!("{id}"));
        assert!(l.contains(agent), "{id} line lacks {agent}: {l}");
        assert!(
            l.contains(&seals.to_string()),
            "{id} line lacks seal count {seals}: {l}"
        );
    }
}

#[test]
fn rc_closing_words_verbatim_and_absence_recorded_never_synthesized() {
    let f = finished("words");
    let b = body(&f.p, &f.commit);
    assert!(line_with(&b, "sa").contains(A_WORDS), "{b}");
    assert!(line_with(&b, "sb").contains(B_WORDS), "{b}");
    let sc = b
        .lines()
        .find(|l| l.contains("sc") && l.contains(CHARLIE))
        .unwrap_or_else(|| panic!("no sc line:\n{b}"));
    assert!(sc.contains(NO_SUMMARY), "sc closed with no words: {sc}");
    assert!(
        !sc.contains('"'),
        "a quoted sentence on a spec with no words: {sc}"
    );
    // Seal summaries ("work") are not closing words.
    assert!(!sc.contains("work\""), "{sc}");
}

#[test]
fn rc_shared_files_resolutions_overrides_and_refusals() {
    let f = finished("files");
    let b = body(&f.p, &f.commit);
    let shared = file_line(&b, "shared.txt");
    assert!(shared.contains("accepted after being told"), "{shared}");
    assert!(
        shared.contains(ALPHA),
        "override by alpha not named: {shared}"
    );
    assert!(shared.contains(BRAVO), "{shared}");
    let two = file_line(&b, "two.txt");
    assert!(two.contains("kept"), "{two}");
    assert!(
        b.lines().any(|l| l.to_lowercase().contains("refus")
            && l.contains("shared.txt")
            && l.contains(ALPHA)),
        "the refusal during the range is not listed:\n{b}"
    );
}

#[test]
fn rc_survival_line_present_and_true() {
    let f = finished("survival");
    let b = body(&f.p, &f.commit);
    let printed = finish_survival(&f.finish_out);
    assert!(printed.contains("none lost"), "{printed}");
    assert!(
        b.contains(&printed),
        "receipt survival line differs from finish's {printed:?}:\n{b}"
    );
    assert!(
        printed.contains("of 3 spec"),
        "survival must check the three specs: {printed}"
    );
    // The independent audit agrees.
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/finish_audit.py");
    let out = Command::new("python3")
        .arg(&script)
        .arg(&f.commit)
        .env("WRIT_AUDIT_REPO", &f.p.root)
        .output()
        .unwrap();
    let t = text(&out);
    assert!(out.status.success(), "finish_audit.py: {t}");
    assert!(t.contains("LOST: 0"), "audit: {t}");
}

#[test]
fn rc_nothing_outside_the_range_or_the_records() {
    let f = finished("records");
    let b = body(&f.p, &f.commit);
    // The writ-bridge baseline seals before `since` are outside the range.
    assert!(!b.contains("writ-bridge"), "{b}");
    assert!(!b.contains("post-init baseline"), "{b}");
    assert!(!b.contains("human"), "an agent not in the records: {b}");
    // A second finish with nothing new writes no receipt for the old specs.
    let again = f.p.writ("human", &["finish", "-y"]);
    let _ = again;
    if f.p.head() != f.commit {
        let b2 = body(&f.p, &f.p.head());
        for id in ["sa", "sb", "sc"] {
            assert!(
                !b2.contains(&format!("{id} ")),
                "{id} repeated in a later receipt: {b2}"
            );
        }
    }
}

/// Aubs: an auto spec closed by nobody (SessionEnd, or still open at finish
/// time after the agent left) reads "no summary from the agent; N auto seals
/// over <span>", never a synthesized sentence.
#[test]
fn rc_auto_spec_without_spec_done_reads_no_summary_n_auto_seals_over_span() {
    let p = Project::new("auto");
    let since = p.newest_seal();
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(p.path(".claude/settings.json")).unwrap())
            .unwrap();
    let cmds: Vec<String> = settings["hooks"]["PostToolUse"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|e| e["hooks"].as_array().cloned().unwrap_or_default())
        .filter_map(|h| h["command"].as_str().map(str::to_string))
        .filter(|c| c.contains("writ"))
        .collect();
    assert!(!cmds.is_empty(), "no writ PostToolUse hook");
    for i in 0..3 {
        p.write("shared.txt", &numbered(40, &[(i, "auto")]));
        let stdin = serde_json::json!({
            "session_id": "sess-a", "hook_event_name": "PostToolUse", "tool_name": "Edit",
            "cwd": p.root.display().to_string(),
            "tool_input": {"file_path": p.path("shared.txt").display().to_string()},
        });
        for c in &cmds {
            use std::io::Write as _;
            let mut ch = p
                .cmd("sh", ALPHA)
                .arg("-c")
                .arg(c)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            ch.stdin
                .take()
                .unwrap()
                .write_all(stdin.to_string().as_bytes())
                .unwrap();
            assert_eq!(ch.wait_with_output().unwrap().status.code(), Some(0));
        }
    }
    let r = text(&p.ok(
        "human",
        &["handoff", "--since", &since, "--format", "receipt"],
    ));
    let l = r
        .lines()
        .find(|l| l.contains("auto-"))
        .unwrap_or_else(|| panic!("no auto spec line:\n{r}"));
    assert!(
        l.contains("no summary from the agent; 3 auto seals over "),
        "auto spec line: {l}"
    );
    assert!(
        !l.contains("auto: Edit"),
        "seal summaries presented as words: {l}"
    );
}

/// Run every writ hook command registered on `event` with `stdin`, as
/// `agent`; each must exit 0.
fn fire(p: &Project, agent: &str, event: &str, stdin: &Value) {
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(p.path(".claude/settings.json")).unwrap())
            .unwrap();
    let cmds: Vec<String> = settings["hooks"][event]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|e| e["hooks"].as_array().cloned().unwrap_or_default())
        .filter_map(|h| h["command"].as_str().map(str::to_string))
        .filter(|c| c.contains("writ"))
        .collect();
    assert!(!cmds.is_empty(), "no writ {event} hook");
    for c in &cmds {
        use std::io::Write as _;
        let mut ch = p
            .cmd("sh", agent)
            .arg("-c")
            .arg(c)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        ch.stdin
            .take()
            .unwrap()
            .write_all(stdin.to_string().as_bytes())
            .unwrap();
        assert_eq!(ch.wait_with_output().unwrap().status.code(), Some(0));
    }
}

fn bash_stdin(p: &Project, event: &str, session: &str, cmd: &str, id: &str) -> Value {
    serde_json::json!({
        "session_id": session, "hook_event_name": event, "tool_name": "Bash",
        "cwd": p.root.display().to_string(),
        "tool_input": {"command": cmd}, "tool_use_id": id,
    })
}

/// Ruling 189: a seal the hook attributed from the command text says so on
/// the receipt's shared-file line, in handoff and in `writ why`; a seal by a
/// single window (or by hand) keeps today's wording.
#[test]
fn rc_189_command_text_seal_reads_attributed_from_the_command() {
    let p = Project::new("cmdtext");
    let since = p.newest_seal();
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    let tick = || std::thread::sleep(std::time::Duration::from_millis(30));
    // alpha's command names shared.txt; bravo's open call names nothing.
    let ca = "printf 'alpha\\n' >> shared.txt";
    let cb = "ls >/dev/null";
    fire(
        &p,
        ALPHA,
        "PreToolUse",
        &bash_stdin(&p, "PreToolUse", "sess-a", ca, "t_a"),
    );
    tick();
    fire(
        &p,
        BRAVO,
        "PreToolUse",
        &bash_stdin(&p, "PreToolUse", "sess-b", cb, "t_b"),
    );
    tick();
    let out = p.cmd("sh", "human").arg("-c").arg(ca).output().unwrap();
    assert!(out.status.success());
    tick();
    fire(
        &p,
        ALPHA,
        "PostToolUse",
        &bash_stdin(&p, "PostToolUse", "sess-a", ca, "t_a"),
    );
    tick();
    fire(
        &p,
        BRAVO,
        "PostToolUse",
        &bash_stdin(&p, "PostToolUse", "sess-b", cb, "t_b"),
    );
    let s = p.json(
        "human",
        &["log", "--all", "--format", "json", "--limit", "1"],
    );
    assert_eq!(s[0]["agent"]["id"], ALPHA, "{s:#}");
    assert_eq!(s[0]["attributed_by"], "command_text", "{s:#}");
    // bravo then edits the same file by hand.
    let cur = fs::read_to_string(p.path("shared.txt")).unwrap();
    p.write("shared.txt", &format!("{cur}bravo\n"));
    assert!(p.seal(BRAVO, "sb", "shared.txt", &[]).status.success());

    let r = text(&p.ok(
        "human",
        &["handoff", "--since", &since, "--format", "receipt"],
    ));
    let l = file_line(&r, "shared.txt");
    assert!(
        l.contains("sa by alpha, attributed from the command"),
        "receipt line: {l}"
    );
    assert_eq!(l.matches("attributed from the command").count(), 1, "{l}");
    let pr = text(&p.ok("human", &["handoff", "--since", &since, "--format", "pr"]));
    assert!(
        file_line(&pr, "shared.txt").contains("sa by alpha, attributed from the command"),
        "{pr}"
    );
    let h = text(&p.ok("human", &["handoff", "--since", &since]));
    assert!(
        line_with(&h, "`shared.txt` sealed by").contains("sa (attributed from the command), sb"),
        "{h}"
    );
    let why = text(&p.ok("human", &["why", "shared.txt"]));
    let lines: Vec<&str> = why.lines().filter(|l| l.contains(" seal ")).collect();
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.contains("attributed from the command"))
            .count(),
        1,
        "{why}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("by alpha") && l.contains("attributed from the command")),
        "{why}"
    );
}

/// Finding 199: the survival line ends with the seals in the receipt and
/// how many carry a signature; with keys made on each identity's first
/// seal, every one does.
#[test]
fn rc_199_survival_line_counts_seals_and_signed() {
    let p = Project::new("signed");
    let since = p.newest_seal();
    p.spec("sa", ALPHA);
    p.write("a.txt", "alpha\n");
    assert!(p.seal(ALPHA, "sa", "a.txt", &[]).status.success());
    p.write("b.txt", "alpha\n");
    assert!(p.seal(ALPHA, "sa", "b.txt", &[]).status.success());
    let r = text(&p.ok(
        "human",
        &["handoff", "--since", &since, "--format", "receipt"],
    ));
    let l = r
        .lines()
        .find(|l| l.contains("Survival:"))
        .unwrap_or_else(|| panic!("no survival line: {r}"));
    assert!(l.ends_with("; 2 seals, 2 signed"), "survival line: {l}");
}
