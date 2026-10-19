//! Auto seal, subagent identity and the zero-instruction template (sprint 6,
//! 0.4.3; Amis's `auto-seal` items 1 to 4). Assertions written before the code
//! (retro rule 1); each is ignored on EXPECTED_IGNORED in bench/test-gate.sh
//! until it lands.
//!
//! Mechanism-agnostic by design: every test drives the hooks exactly as
//! Claude Code would. It runs `writ init -y`, reads `.claude/settings.json`,
//! picks the writ hook commands registered on the event (and, for
//! PostToolUse, whose matcher covers the tool), and runs each with `sh -c`
//! in the project root with the documented hook JSON on stdin. Whatever
//! subcommand or flag Amis chooses, the contract is what lands in the store
//! and what the hook hands back to the agent.
//!
//! Contract (text Amis designs against; key names are the documented Claude
//! Code shapes, the live shape from Amis's experiment wins):
//! - PostToolUse on Edit, Write, MultiEdit (`tool_input.file_path`) and
//!   NotebookEdit (`tool_input.notebook_path`) seals exactly that path, under
//!   the session's identity and that identity's claimed spec; summary starts
//!   "auto: <Tool> <path>".
//! - No claimed spec: the first auto seal creates and claims `auto-<key>`,
//!   titled from the session's last UserPromptSubmit first line if stashed,
//!   else "auto work by <agent>"; later edits reuse it.
//! - Every hook exits 0 (a hook never fails the tool). A writ error goes to
//!   `.writ/hooks.log`. On success the hook prints nothing to stdout (zero
//!   agent-facing surface); UserPromptSubmit prints nothing (its stdout would
//!   be injected into the agent's context).
//! - Aubs's hard addition: a REFUSED auto seal (removes another spec's sealed
//!   lines, finding 102) is handed back to the agent through the hook's
//!   return path: stdout JSON `hookSpecificOutput.additionalContext`, exit 0.
//!   Finding 141 (Amis, live): exit 0 and exactly one stdout JSON object
//!   {"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":
//!   "<one line>"}}; exit 2 is never used (reads as a blocking error and leaks
//!   the hook path). The line names the path, the other agent, and a fix
//!   command that is neither --force nor destructive.
//! - Stop seals a touched path whose content changed after its auto seal;
//!   never an untouched path, never another agent's path; no change = no seal.
//! - SessionEnd closes this session's open auto specs; leaves alone a spec
//!   the agent closed itself, a hand-made spec, and other agents' specs.
//! - `writ log` and `writ handoff` collapse consecutive auto seals of one
//!   spec to one line with the count; the JSON records stay one per seal.
//! - Subagent identity (pending Amis's live stdin; see SUBAGENT_* below): an
//!   edit from inside a subagent seals under its own identity, distinct per
//!   subagent, derived from the parent, and its auto spec names the parent.
//! - Template: the managed block, the SessionStart echo and the AGENTS.md
//!   block each under 5 lines, 0 mandatory commands, no identity
//!   instructions; they mention `writ context` and `writ spec done -s`.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

const ALPHA: &str = "alpha";
const BRAVO: &str = "bravo";
/// Generous per-hook bound for a debug build under a parallel test run. The
/// release budget (p50 < 100 ms, max < 500 ms) is measured by
/// bench/fleet/hook_latency.py with the release binary, alone.
const DEBUG_HOOK_BOUND: Duration = Duration::from_secs(5);
/// The edit tools whose PostToolUse writ hooks.
/// MultiEdit is absent from Claude Code 2.1.290 (finding 132); writ's matcher
/// may keep it for older hosts but no test requires it.
const EDIT_TOOLS: [&str; 3] = ["Edit", "Write", "NotebookEdit"];
/// Field Claude Code adds to hook stdin when the hook fires inside a subagent
/// spawned by the Agent tool (documented shape; Amis's experiment confirms or
/// renames it, then this constant changes, not the assertions).
const SUBAGENT_ID_FIELD: &str = "agent_id";
const SUBAGENT_TYPE_FIELD: &str = "agent_type";

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
            std::env::temp_dir().join(format!("writ-auto-{tag}-{}-{nanos}", std::process::id()));
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
        p.write(".gitignore", "target/\nignored.txt\n");
        p.write("a.txt", &numbered(20, &[]));
        p.write("b.txt", &numbered(20, &[]));
        p.write("shared.txt", &numbered(40, &[]));
        p.write("nb.ipynb", "{\"cells\": []}\n");
        p.git_ok(&["add", "-A"]);
        p.git_ok(&["commit", "-q", "-m", "init"]);
        p.ok("human", &["init", "-y"]);
        p.git_ok(&["add", "-A"]);
        p.git_ok(&["commit", "-q", "--no-verify", "-m", "writ init"]);
        p
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn abs(&self, rel: &str) -> String {
        self.path(rel).display().to_string()
    }

    fn write(&self, rel: &str, content: &str) {
        let p = self.path(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, content).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.path(rel)).unwrap_or_default()
    }

    /// Environment of an agent session: `agent` = None means no
    /// WRIT_AGENT_ID (identity from the framework: CLAUDECODE=1 and, in hooks,
    /// CLAUDE_CODE_SESSION_ID = stdin session_id, as Amis saw live).
    fn cmd(&self, program: &str, agent: Option<&str>) -> Command {
        let mut c = Command::new(program);
        c.current_dir(&self.root)
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", self.bin.display()),
            )
            .env("HOME", &self.home)
            .env("TMPDIR", std::env::temp_dir())
            .env("CLAUDE_PROJECT_DIR", &self.root)
            .env("WRIT_EXCLUDES_FILE", self.home.join("no-global-excludes"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .stdin(Stdio::null());
        match agent {
            Some(a) => {
                c.env("WRIT_AGENT_ID", a);
            }
            None => {
                c.env("CLAUDECODE", "1");
            }
        }
        c
    }

    fn writ(&self, agent: &str, args: &[&str]) -> Output {
        self.cmd(self.bin.join("writ").to_str().unwrap(), Some(agent))
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
        let out = self.cmd("git", Some("human")).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", text(&out));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn spec(&self, id: &str, agent: &str) {
        self.ok(
            agent,
            &["spec", "add", "--id", id, "--title", &format!("spec {id}")],
        );
        self.ok(agent, &["spec", "claim", id, "--agent", agent]);
    }

    fn settings(&self) -> Value {
        let s = fs::read_to_string(self.path(".claude/settings.json"))
            .expect(".claude/settings.json after init -y");
        serde_json::from_str(&s).unwrap()
    }

    /// Writ hook commands on `event` (for PostToolUse: whose matcher covers
    /// `tool`). Claude Code matchers are regexes; writ's are expected to be
    /// an alternation of tool names, optionally anchored or grouped.
    fn hook_commands(&self, event: &str, tool: Option<&str>) -> Vec<String> {
        let settings = self.settings();
        let Some(entries) = settings["hooks"][event].as_array() else {
            return vec![];
        };
        let mut out = vec![];
        for e in entries {
            if let Some(t) = tool {
                if !matcher_covers(e.get("matcher").and_then(Value::as_str), t) {
                    continue;
                }
            }
            for h in e["hooks"].as_array().into_iter().flatten() {
                if let Some(c) = h["command"].as_str() {
                    if c.contains("writ") {
                        out.push(c.to_string());
                    }
                }
            }
        }
        out
    }

    /// Run every writ hook on `event` with `stdin`, as `agent` (None: the
    /// framework identity). Returns one result per command.
    fn fire(
        &self,
        event: &str,
        tool: Option<&str>,
        agent: Option<&str>,
        stdin: &Value,
    ) -> Vec<HookRun> {
        let cmds = self.hook_commands(event, tool);
        assert!(
            !cmds.is_empty(),
            "no writ hook on {event} {tool:?} in .claude/settings.json:\n{}",
            self.settings()
        );
        cmds.iter()
            .map(|c| {
                let t = Instant::now();
                let mut child = self
                    .cmd("sh", agent)
                    .env(
                        "CLAUDE_CODE_SESSION_ID",
                        stdin["session_id"].as_str().unwrap_or(""),
                    )
                    .arg("-c")
                    .arg(c)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(stdin.to_string().as_bytes())
                    .unwrap();
                let out = child.wait_with_output().unwrap();
                HookRun {
                    out,
                    elapsed: t.elapsed(),
                }
            })
            .collect()
    }

    /// PostToolUse for one edit, the documented stdin shape.
    fn edit(&self, agent: Option<&str>, session: &str, tool: &str, rel: &str) -> Vec<HookRun> {
        let stdin = post_tool_use(&self.root, session, tool, &self.abs(rel), None);
        self.fire("PostToolUse", Some(tool), agent, &stdin)
    }

    fn event(&self, agent: Option<&str>, session: &str, event: &str, extra: Value) -> Vec<HookRun> {
        let mut v = base_stdin(&self.root, session, event);
        if let (Some(m), Some(x)) = (v.as_object_mut(), extra.as_object()) {
            for (k, val) in x {
                m.insert(k.clone(), val.clone());
            }
        }
        self.fire(event, None, agent, &v)
    }

    fn seals(&self) -> Vec<Value> {
        self.json(
            "human",
            &["log", "--all", "--format", "json", "--limit", "500"],
        )
        .as_array()
        .cloned()
        .unwrap_or_default()
    }

    fn newest_seal(&self) -> Value {
        self.seals().into_iter().next().expect("a seal")
    }

    fn seal_count(&self) -> usize {
        self.seals().len()
    }

    fn specs(&self) -> Vec<Value> {
        self.json("human", &["spec", "status", "--format", "json"])
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    fn spec_json(&self, id: &str) -> Value {
        self.json("human", &["spec", "show", id, "--format", "json"])
    }

    fn auto_specs(&self) -> Vec<Value> {
        self.specs()
            .into_iter()
            .filter(|s| s["id"].as_str().is_some_and(|i| i.starts_with("auto-")))
            .collect()
    }

    fn hooks_log(&self) -> String {
        self.read(".writ/hooks.log")
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        if std::env::var_os("WRIT_KEEP_FIXTURES").is_none() {
            let _ = fs::remove_dir_all(self.root.parent().unwrap());
        }
    }
}

struct HookRun {
    out: Output,
    elapsed: Duration,
}

impl HookRun {
    fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.out.stdout).into_owned()
    }

    /// What the hook hands back to the agent (finding 141): on exit 0,
    /// stdout is empty or exactly one JSON object
    /// {"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":..}}.
    fn to_agent(&self) -> Option<String> {
        assert_eq!(
            self.out.status.code(),
            Some(0),
            "hook exit (never 2): {}",
            text(&self.out)
        );
        let s = self.stdout();
        if s.trim().is_empty() {
            return None;
        }
        let mut de = serde_json::Deserializer::from_str(s.trim()).into_iter::<Value>();
        let v = de
            .next()
            .unwrap_or_else(|| panic!("hook stdout: {s}"))
            .unwrap_or_else(|e| panic!("hook stdout is neither empty nor JSON ({e}): {s}"));
        assert!(
            de.next().is_none(),
            "more than one JSON object on hook stdout: {s}"
        );
        assert_eq!(
            v["hookSpecificOutput"]["hookEventName"], "PostToolUse",
            "{v}"
        );
        let ctx = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or_else(|| panic!("no hookSpecificOutput.additionalContext: {v}"));
        Some(ctx.to_string())
    }
}

fn matcher_covers(matcher: Option<&str>, tool: &str) -> bool {
    match matcher.map(str::trim) {
        None | Some("") | Some("*") | Some(".*") => true,
        Some(m) => m
            .trim_start_matches('^')
            .trim_end_matches('$')
            .trim_start_matches('(')
            .trim_end_matches(')')
            .split('|')
            .any(|t| t.trim() == tool),
    }
}

fn base_stdin(root: &std::path::Path, session: &str, event: &str) -> Value {
    json!({
        "session_id": session,
        "transcript_path": format!("/tmp/claude-fixture/{session}.jsonl"),
        "cwd": root.display().to_string(),
        "permission_mode": "default",
        "hook_event_name": event,
    })
}

/// Documented PostToolUse stdin. `subagent`: (agent_id, agent_type) when the
/// edit happened inside a subagent.
fn post_tool_use(
    root: &std::path::Path,
    session: &str,
    tool: &str,
    abs: &str,
    subagent: Option<(&str, &str)>,
) -> Value {
    let input = match tool {
        "Write" => json!({"file_path": abs, "content": "x"}),
        "MultiEdit" => json!({"file_path": abs, "edits": [{"old_string": "a", "new_string": "b"}]}),
        "NotebookEdit" => json!({"notebook_path": abs, "new_source": "x", "cell_id": "c0"}),
        _ => json!({"file_path": abs, "old_string": "a", "new_string": "b"}),
    };
    let mut v = base_stdin(root, session, "PostToolUse");
    v["tool_name"] = json!(tool);
    v["tool_input"] = input;
    v["tool_response"] = json!({"filePath": abs, "success": true});
    if let Some((id, ty)) = subagent {
        v[SUBAGENT_ID_FIELD] = json!(id);
        v[SUBAGENT_TYPE_FIELD] = json!(ty);
    }
    v
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

fn paths_of(seal: &Value) -> BTreeSet<String> {
    seal["changes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["path"].as_str().map(str::to_string))
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn all_exit_zero(runs: &[HookRun], what: &str) {
    for r in runs {
        assert_eq!(
            r.out.status.code(),
            Some(0),
            "{what}: a writ hook exited non-zero (a hook never fails the tool):\n{}",
            text(&r.out)
        );
    }
}

fn nothing_to_agent(runs: &[HookRun], what: &str) {
    for r in runs {
        assert!(
            r.stdout().trim().is_empty(),
            "{what}: a writ hook printed to stdout on success (zero agent surface):\n{}",
            r.stdout()
        );
    }
}

// ---------------------------------------------------------------------------
// Auto seal (Amis item 1)
// ---------------------------------------------------------------------------

#[test]
fn as_init_installs_post_tool_use_on_every_edit_tool_and_uninit_removes_it() {
    let p = Project::new("install");
    for tool in EDIT_TOOLS {
        assert!(
            !p.hook_commands("PostToolUse", Some(tool)).is_empty(),
            "no writ PostToolUse hook covers {tool}"
        );
    }
    assert!(
        !p.hook_commands("Stop", None).is_empty(),
        "no writ Stop hook"
    );
    assert!(
        !p.hook_commands("SessionEnd", None).is_empty(),
        "no writ SessionEnd hook"
    );
    // A user's own PostToolUse hook survives init re-run and uninit.
    let mut s = p.settings();
    s["hooks"]["PostToolUse"]
        .as_array_mut()
        .unwrap()
        .push(json!({"matcher": "Bash", "hooks": [{"type": "command", "command": "echo mine"}]}));
    fs::write(p.path(".claude/settings.json"), s.to_string()).unwrap();
    p.ok("human", &["init", "-y"]);
    p.ok("human", &["uninit", "-y"]);
    let after = fs::read_to_string(p.path(".claude/settings.json")).unwrap_or_default();
    assert!(
        after.contains("echo mine"),
        "uninit removed a user hook:\n{after}"
    );
    for tool in EDIT_TOOLS {
        assert!(
            p.hook_commands("PostToolUse", Some(tool)).is_empty() || !after.contains("writ"),
            "uninit left a writ PostToolUse hook for {tool}:\n{after}"
        );
    }
}

#[test]
fn as_edit_seals_exactly_that_path_under_the_sessions_spec() {
    let p = Project::new("edit");
    p.spec("sa", ALPHA);
    let before = p.seal_count();
    p.write("a.txt", &numbered(20, &[(3, "A")]));
    let runs = p.edit(Some(ALPHA), "sess-a", "Edit", "a.txt");
    all_exit_zero(&runs, "edit");
    nothing_to_agent(&runs, "edit");
    for r in &runs {
        assert!(r.elapsed < DEBUG_HOOK_BOUND, "hook took {:?}", r.elapsed);
    }
    assert_eq!(p.seal_count(), before + 1, "exactly one seal per edit");
    let s = p.newest_seal();
    assert_eq!(s["agent"]["id"], ALPHA);
    assert_eq!(s["spec_id"], "sa");
    assert_eq!(paths_of(&s), set(&["a.txt"]));
    let summary = s["summary"].as_str().unwrap();
    assert!(
        summary.starts_with("auto: Edit") && summary.contains("a.txt"),
        "summary {summary:?}, want \"auto: Edit a.txt\""
    );
}

#[test]
fn as_each_edit_tool_seals_its_own_path_including_notebook_path() {
    let p = Project::new("tools");
    p.spec("sa", ALPHA);
    for (tool, rel) in [
        ("Write", "new_by_write.txt"),
        ("Edit", "a.txt"),
        ("NotebookEdit", "nb.ipynb"),
    ] {
        p.write(rel, &format!("{tool} was here\n"));
        let runs = p.edit(Some(ALPHA), "sess-a", tool, rel);
        all_exit_zero(&runs, tool);
        let s = p.newest_seal();
        assert_eq!(paths_of(&s), set(&[rel]), "{tool} sealed the wrong paths");
        assert!(
            s["summary"]
                .as_str()
                .unwrap()
                .starts_with(&format!("auto: {tool}")),
            "{tool}: summary {}",
            s["summary"]
        );
    }
}

#[test]
fn as_edit_by_a_never_seals_b_pending_path() {
    let p = Project::new("never-b");
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    // bravo's edit is on disk and pending (its own hook has not fired yet).
    p.write("b.txt", &numbered(20, &[(4, "B")]));
    p.write("a.txt", &numbered(20, &[(3, "A")]));
    let runs = p.edit(Some(ALPHA), "sess-a", "Edit", "a.txt");
    all_exit_zero(&runs, "alpha edit");
    let s = p.newest_seal();
    assert_eq!(paths_of(&s), set(&["a.txt"]));
    for seal in p.seals() {
        if seal["agent"]["id"] == ALPHA {
            assert!(
                !paths_of(&seal).contains("b.txt"),
                "alpha's auto seal carried bravo's pending b.txt: {seal}"
            );
        }
    }
    // bravo's path is still pending, for bravo.
    let st = text(&p.ok("human", &["diff", "--stat"]));
    assert!(st.contains("b.txt"), "b.txt no longer pending:\n{st}");
}

#[test]
fn as_hook_error_never_changes_tool_exit_and_lands_in_hooks_log() {
    let p = Project::new("errors");
    p.spec("sa", ALPHA);
    let cmds = p.hook_commands("PostToolUse", Some("Edit"));
    assert!(!cmds.is_empty());
    let run_raw = |stdin: &str| -> Vec<Output> {
        cmds.iter()
            .map(|c| {
                let mut child = p
                    .cmd("sh", Some(ALPHA))
                    .arg("-c")
                    .arg(c)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(stdin.as_bytes())
                    .unwrap();
                child.wait_with_output().unwrap()
            })
            .collect()
    };
    let outside = std::env::temp_dir().join("writ-auto-outside.txt");
    fs::write(&outside, "x\n").unwrap();
    let cases: Vec<(&str, String)> = vec![
        ("not json", "this is not json".to_string()),
        ("empty stdin", String::new()),
        (
            "no file_path",
            json!({"session_id": "s", "hook_event_name": "PostToolUse", "tool_name": "Edit", "tool_input": {}}).to_string(),
        ),
        (
            "missing file",
            post_tool_use(&p.root, "s", "Edit", &p.abs("gone.txt"), None).to_string(),
        ),
        (
            "outside repo",
            post_tool_use(&p.root, "s", "Edit", &outside.display().to_string(), None).to_string(),
        ),
        (
            "ignored file",
            {
                p.write("ignored.txt", "x\n");
                post_tool_use(&p.root, "s", "Edit", &p.abs("ignored.txt"), None).to_string()
            },
        ),
    ];
    let before = p.seal_count();
    for (what, stdin) in &cases {
        for out in run_raw(stdin) {
            assert_eq!(
                out.status.code(),
                Some(0),
                "{what}: exit {:?}\n{}",
                out.status.code(),
                text(&out)
            );
            assert!(
                String::from_utf8_lossy(&out.stdout).trim().is_empty(),
                "{what}: printed to the agent:\n{}",
                text(&out)
            );
        }
    }
    assert_eq!(p.seal_count(), before, "a bad hook input produced a seal");
    assert!(
        !p.hooks_log().trim().is_empty(),
        "bad hook inputs left no line in .writ/hooks.log"
    );
    // Broken store: seals dir unreadable. Still exit 0, still logged.
    let log_before = p.hooks_log().len();
    let seals_dir = p.path(".writ/seals");
    let moved = p.path(".writ/seals.moved");
    fs::rename(&seals_dir, &moved).unwrap();
    fs::write(&seals_dir, "not a directory").unwrap();
    p.write("a.txt", &numbered(20, &[(3, "A")]));
    let runs = p.edit(Some(ALPHA), "s", "Edit", "a.txt");
    fs::remove_file(&seals_dir).unwrap();
    fs::rename(&moved, &seals_dir).unwrap();
    all_exit_zero(&runs, "broken store");
    assert!(
        p.hooks_log().len() > log_before,
        "broken store error not in hooks.log"
    );
    // No writ on PATH and the init-time fallback gone: exit 0.
    for c in &cmds {
        let mut child = Command::new("sh")
            .current_dir(&p.root)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &p.home)
            .env("WRIT_AGENT_ID", ALPHA)
            .arg("-c")
            .arg(c.replace(env!("CARGO_BIN_EXE_writ"), "/nonexistent/writ"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(
                post_tool_use(&p.root, "s", "Edit", &p.abs("a.txt"), None)
                    .to_string()
                    .as_bytes(),
            )
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "no writ anywhere: {}",
            text(&out)
        );
    }
}

#[test]
fn as_consecutive_auto_seals_collapse_in_log_and_handoff() {
    let p = Project::new("collapse");
    p.spec("sa", ALPHA);
    for i in 0..5 {
        p.write("a.txt", &numbered(20, &[(i, "A")]));
        all_exit_zero(&p.edit(Some(ALPHA), "sess-a", "Edit", "a.txt"), "edit");
    }
    // A hand seal ends the run; one more auto seal starts a new run.
    p.write("b.txt", &numbered(20, &[(1, "hand")]));
    p.ok(
        ALPHA,
        &[
            "seal",
            "-s",
            "hand seal",
            "--agent",
            ALPHA,
            "--paths",
            "b.txt",
        ],
    );
    p.write("a.txt", &numbered(20, &[(9, "A")]));
    all_exit_zero(&p.edit(Some(ALPHA), "sess-a", "Edit", "a.txt"), "edit");
    // Records stay one per seal.
    let auto: Vec<Value> = p
        .seals()
        .into_iter()
        .filter(|s| {
            s["summary"]
                .as_str()
                .is_some_and(|x| x.starts_with("auto:"))
        })
        .collect();
    assert_eq!(auto.len(), 6, "JSON log must keep every auto seal");
    // Text log: one line for the run of five (with the count), one for the run of one.
    let log = text(&p.ok("human", &["log", "--all"]));
    let auto_lines: Vec<&str> = log.lines().filter(|l| l.contains("auto:")).collect();
    assert_eq!(
        auto_lines.len(),
        2,
        "want one line per run of consecutive auto seals:\n{log}"
    );
    assert!(
        log.lines().any(|l| l.contains("auto") && l.contains('5')),
        "the collapsed line must carry the count 5:\n{log}"
    );
    assert!(log.contains("hand seal"), "{log}");
    // Handoff: the five plus one are not listed seal by seal.
    let h = text(&p.ok("human", &["handoff"]));
    assert!(
        h.matches("auto:").count() <= 2,
        "handoff lists auto seals one by one:\n{h}"
    );
    let hj = p.json("human", &["handoff", "--format", "json"]);
    let sa = hj["specs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "sa")
        .unwrap();
    assert_eq!(
        sa["seals"], 7,
        "handoff JSON seal count is the record count"
    );
}

#[test]
fn as_session_without_spec_gets_one_auto_spec_on_first_edit() {
    let p = Project::new("autospec");
    assert!(p.auto_specs().is_empty());
    p.write("a.txt", &numbered(20, &[(1, "A")]));
    all_exit_zero(
        &p.edit(Some(ALPHA), "sess-a", "Edit", "a.txt"),
        "first edit",
    );
    let autos = p.auto_specs();
    assert_eq!(
        autos.len(),
        1,
        "first edit must create one auto spec: {autos:?}"
    );
    let id = autos[0]["id"].as_str().unwrap().to_string();
    let spec = p.spec_json(&id);
    assert_eq!(spec["claimed_by"], ALPHA);
    assert_eq!(spec["title"], format!("auto work by {ALPHA}"));
    assert_eq!(p.newest_seal()["spec_id"], id.as_str());
    // Second edit reuses it.
    p.write("a.txt", &numbered(20, &[(2, "A")]));
    all_exit_zero(
        &p.edit(Some(ALPHA), "sess-a", "Edit", "a.txt"),
        "second edit",
    );
    assert_eq!(
        p.auto_specs().len(),
        1,
        "second edit made another auto spec"
    );
    assert_eq!(p.newest_seal()["spec_id"], id.as_str());
    // Another session gets its own.
    p.write("b.txt", &numbered(20, &[(2, "B")]));
    all_exit_zero(
        &p.edit(Some(BRAVO), "sess-b", "Edit", "b.txt"),
        "bravo edit",
    );
    let autos = p.auto_specs();
    assert_eq!(autos.len(), 2, "{autos:?}");
    let b = p.newest_seal();
    assert_eq!(b["agent"]["id"], BRAVO);
    assert_ne!(
        b["spec_id"],
        id.as_str(),
        "bravo sealed into alpha's auto spec"
    );
}

#[test]
fn as_auto_spec_title_from_stashed_prompt_and_prompt_hook_is_silent() {
    let p = Project::new("prompt");
    if p.hook_commands("UserPromptSubmit", None).is_empty() {
        // No stash hook: the fallback title is the contract (covered above).
        return;
    }
    let runs = p.event(
        Some(ALPHA),
        "sess-a",
        "UserPromptSubmit",
        json!({"prompt": "Fix the parser off-by-one\nsecond line\nthird"}),
    );
    all_exit_zero(&runs, "UserPromptSubmit");
    nothing_to_agent(
        &runs,
        "UserPromptSubmit (stdout is injected into the agent's context)",
    );
    // Finding 133: a background subagent's completion arrives as a
    // UserPromptSubmit whose prompt is a <task-notification>; never a title.
    let tn = p.event(
        Some(ALPHA),
        "sess-a",
        "UserPromptSubmit",
        json!({"prompt": "<task-notification>\n<task-id>f0e1d2c3b4a596870</task-id>\n</task-notification>"}),
    );
    all_exit_zero(&tn, "UserPromptSubmit task-notification");
    nothing_to_agent(&tn, "UserPromptSubmit task-notification");
    p.write("a.txt", &numbered(20, &[(1, "A")]));
    all_exit_zero(&p.edit(Some(ALPHA), "sess-a", "Edit", "a.txt"), "edit");
    let autos = p.auto_specs();
    assert_eq!(autos.len(), 1);
    assert_eq!(autos[0]["title"], "Fix the parser off-by-one");
}

#[test]
fn as_hand_spec_done_closes_the_auto_spec_with_the_agents_words() {
    let p = Project::new("handdone");
    p.write("a.txt", &numbered(20, &[(1, "A")]));
    all_exit_zero(&p.edit(Some(ALPHA), "sess-a", "Edit", "a.txt"), "edit");
    let id = p.auto_specs()[0]["id"].as_str().unwrap().to_string();
    // The agent's own `writ spec done -s` (same identity as its hooks) closes it.
    p.ok(
        ALPHA,
        &[
            "spec",
            "done",
            "-s",
            "fixed the parser, tests green",
            "--agent",
            ALPHA,
        ],
    );
    let spec = p.spec_json(&id);
    assert_eq!(spec["status"], "complete", "{spec}");
    assert!(
        spec.to_string().contains("fixed the parser, tests green"),
        "agent's words not on the spec: {spec}"
    );
}

#[test]
fn as_stop_seals_a_touched_path_changed_after_its_auto_seal_and_nothing_else() {
    let p = Project::new("stop");
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    p.write("a.txt", &numbered(20, &[(1, "A")]));
    all_exit_zero(&p.edit(Some(ALPHA), "sess-a", "Edit", "a.txt"), "edit");
    // After the hook: a formatter (or a Bash sed) changes the touched file;
    // alpha also has an untouched pending file and bravo a pending file.
    p.write("a.txt", &numbered(20, &[(1, "A formatted")]));
    p.write("untouched.txt", "made by a Bash command\n");
    p.write("b.txt", &numbered(20, &[(4, "B")]));
    let before = p.seal_count();
    let runs = p.event(
        Some(ALPHA),
        "sess-a",
        "Stop",
        json!({"stop_hook_active": false}),
    );
    all_exit_zero(&runs, "Stop");
    nothing_to_agent(
        &runs,
        "Stop (a decision:block would keep the agent running)",
    );
    assert_eq!(
        p.seal_count(),
        before + 1,
        "Stop must seal the missed path once"
    );
    let s = p.newest_seal();
    assert_eq!(s["agent"]["id"], ALPHA);
    assert_eq!(s["spec_id"], "sa");
    assert_eq!(
        paths_of(&s),
        set(&["a.txt"]),
        "Stop sealed beyond the touched list"
    );
    // Second Stop with nothing missed: no seal.
    let before = p.seal_count();
    all_exit_zero(
        &p.event(
            Some(ALPHA),
            "sess-a",
            "Stop",
            json!({"stop_hook_active": false}),
        ),
        "Stop again",
    );
    assert_eq!(
        p.seal_count(),
        before,
        "Stop with nothing missed made a seal"
    );
}

#[test]
fn as_session_end_closes_own_auto_specs_and_nothing_else() {
    let p = Project::new("end");
    // alpha: an auto spec (open), a hand-made claimed spec (open).
    p.write("a.txt", &numbered(20, &[(1, "A")]));
    all_exit_zero(
        &p.edit(Some(ALPHA), "sess-a", "Edit", "a.txt"),
        "alpha auto",
    );
    let alpha_auto = p.auto_specs()[0]["id"].as_str().unwrap().to_string();
    // bravo: an auto spec it closed itself, then a second open auto spec is
    // impossible in one session, so bravo's closed one plus charlie's open one.
    p.write("b.txt", &numbered(20, &[(1, "B")]));
    all_exit_zero(
        &p.edit(Some(BRAVO), "sess-b", "Edit", "b.txt"),
        "bravo auto",
    );
    let bravo_auto = p
        .auto_specs()
        .into_iter()
        .find(|s| s["id"] != alpha_auto.as_str())
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    p.ok(
        BRAVO,
        &["spec", "done", "-s", "bravo's own words", "--agent", BRAVO],
    );
    let bravo_before = p.spec_json(&bravo_auto);
    p.write("shared.txt", &numbered(40, &[(7, "C")]));
    all_exit_zero(
        &p.edit(Some("charlie"), "sess-c", "Edit", "shared.txt"),
        "charlie auto",
    );
    let charlie_auto = p
        .auto_specs()
        .into_iter()
        .find(|s| s["id"] != alpha_auto.as_str() && s["id"] != bravo_auto.as_str())
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    p.spec("hand", ALPHA);
    // alpha's session ends.
    let runs = p.event(
        Some(ALPHA),
        "sess-a",
        "SessionEnd",
        json!({"reason": "other"}),
    );
    all_exit_zero(&runs, "SessionEnd");
    assert_eq!(
        p.spec_json(&alpha_auto)["status"],
        "complete",
        "alpha's auto spec not closed"
    );
    // Ruling 131: SessionEnd stores no summary text; the absence is the record.
    let closed = p.spec_json(&alpha_auto);
    assert!(
        closed["completion_summary"].is_null(),
        "SessionEnd synthesized a summary: {}",
        closed["completion_summary"]
    );
    // Claimed, never sealed: "pending"; the contract is only "not closed".
    assert_ne!(
        p.spec_json("hand")["status"],
        "complete",
        "a hand-made spec was closed"
    );
    assert_eq!(
        p.spec_json(&charlie_auto)["status"],
        "in-progress",
        "another session's auto spec was closed"
    );
    // bravo's session ends: its hand-closed auto spec is not reclosed.
    all_exit_zero(
        &p.event(
            Some(BRAVO),
            "sess-b",
            "SessionEnd",
            json!({"reason": "other"}),
        ),
        "bravo SessionEnd",
    );
    let bravo_after = p.spec_json(&bravo_auto);
    assert_eq!(
        bravo_before["updated_at"], bravo_after["updated_at"],
        "hand-closed spec touched"
    );
    assert!(bravo_after.to_string().contains("bravo's own words"));
}

/// Aubs's hard addition. Run B shape: bravo sealed shared.txt; alpha
/// overwrites it, removing bravo's sealed line. alpha's auto seal is refused
/// (finding 102) and alpha must be TOLD, through the hook's return path, with
/// one line naming the path, bravo, and a fix; the tool is never failed.
#[test]
fn as_refused_auto_seal_is_handed_back_to_the_agent_in_one_line() {
    let p = Project::new("refused");
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    p.write("shared.txt", &numbered(40, &[(30, "B")]));
    all_exit_zero(
        &p.edit(Some(BRAVO), "sess-b", "Edit", "shared.txt"),
        "bravo",
    );
    assert_eq!(p.newest_seal()["agent"]["id"], BRAVO);
    // alpha writes from a stale read: bravo's line 30 is gone.
    p.write("shared.txt", &numbered(40, &[(2, "A")]));
    let before = p.seal_count();
    let runs = p.edit(Some(ALPHA), "sess-a", "Write", "shared.txt");
    for r in &runs {
        let code = r.out.status.code();
        assert!(
            code == Some(0),
            "refused auto seal: exit {code:?} (finding 141: exit 0 with additionalContext)"
        );
    }
    assert_eq!(p.seal_count(), before, "a refused seal must not seal");
    let told: Vec<String> = runs.iter().filter_map(HookRun::to_agent).collect();
    assert_eq!(
        told.len(),
        1,
        "exactly one message back to the agent, got {told:?}"
    );
    let msg = told[0].trim();
    assert_eq!(msg.lines().count(), 1, "one line, got:\n{msg}");
    assert!(msg.contains("shared.txt"), "names the path: {msg}");
    assert!(msg.contains(BRAVO), "names the other agent: {msg}");
    assert!(msg.contains("fix:"), "carries a fix: {msg}");
    assert!(
        !msg.contains("--force") && !msg.contains("restore"),
        "the fix must not be --force or a destructive restore: {msg}"
    );
    assert!(
        p.hooks_log().contains("shared.txt"),
        "refusal not in hooks.log"
    );
    // The refusal is not silent elsewhere either: handoff names the unsealed path.
    let hj = p.json("human", &["handoff", "--format", "json"]);
    assert!(
        hj["unsealed"].to_string().contains("shared.txt"),
        "handoff does not list the refused path as unsealed: {}",
        hj["unsealed"]
    );
    let doc = text(&p.writ("human", &["doctor"]));
    assert!(
        !doc.contains("safe to finish"),
        "safe to finish with a refused seal:\n{doc}"
    );
    // Pasting the fix clears the state (rule: every fix_command clears it).
    let fix = msg.split("fix:").nth(1).unwrap().trim().trim_matches('`');
    let fix = fix.split(';').next().unwrap().trim().trim_matches('`');
    let argv: Vec<&str> = fix.split_whitespace().collect();
    assert_eq!(argv.first(), Some(&"writ"), "fix is a writ command: {fix}");
    let out = p.writ(ALPHA, &argv[1..]);
    assert!(
        out.status.success(),
        "pasted fix failed: {fix}\n{}",
        text(&out)
    );
}

/// The same refusal on every later edit must not repeat into the agent's
/// context more than once per path and state.
#[test]
fn as_refusal_is_told_once_per_path_not_on_every_edit() {
    let p = Project::new("told-once");
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    p.write("shared.txt", &numbered(40, &[(30, "B")]));
    all_exit_zero(
        &p.edit(Some(BRAVO), "sess-b", "Edit", "shared.txt"),
        "bravo",
    );
    p.write("shared.txt", &numbered(40, &[(2, "A")]));
    let first: Vec<String> = p
        .edit(Some(ALPHA), "sess-a", "Edit", "shared.txt")
        .iter()
        .filter_map(HookRun::to_agent)
        .collect();
    assert_eq!(first.len(), 1);
    p.write("shared.txt", &numbered(40, &[(2, "A"), (3, "A")]));
    let second: Vec<String> = p
        .edit(Some(ALPHA), "sess-a", "Edit", "shared.txt")
        .iter()
        .filter_map(HookRun::to_agent)
        .collect();
    assert!(
        second.len() <= 1,
        "told more than once per edit: {second:?}"
    );
}

// ---------------------------------------------------------------------------
// Subagent identity (Amis item 2; shape pending the experiment)
// ---------------------------------------------------------------------------

fn sub_edit(p: &Project, session: &str, sub: (&str, &str), rel: &str) -> Vec<HookRun> {
    let stdin = post_tool_use(&p.root, session, "Edit", &p.abs(rel), Some(sub));
    p.fire("PostToolUse", Some("Edit"), None, &stdin)
}

#[test]
fn si_subagent_edit_seals_under_its_own_identity_derived_from_the_parent() {
    let p = Project::new("sub");
    // Parent (framework identity, no WRIT_AGENT_ID) edits first.
    p.write("a.txt", &numbered(20, &[(1, "parent")]));
    all_exit_zero(&p.edit(None, "sess-p", "Edit", "a.txt"), "parent");
    let parent = p.newest_seal()["agent"]["id"].as_str().unwrap().to_string();
    // Two subagents of the same session.
    p.write("b.txt", &numbered(20, &[(1, "sub1")]));
    all_exit_zero(
        &sub_edit(
            &p,
            "sess-p",
            ("a1b2c3d4e5f6a7b8c", "general-purpose"),
            "b.txt",
        ),
        "sub1",
    );
    let s1 = p.newest_seal();
    p.write("shared.txt", &numbered(40, &[(1, "sub2")]));
    all_exit_zero(
        &sub_edit(
            &p,
            "sess-p",
            ("a9e8d7c6b5a4f3e2d", "general-purpose"),
            "shared.txt",
        ),
        "sub2",
    );
    let s2 = p.newest_seal();
    let (i1, i2) = (
        s1["agent"]["id"].as_str().unwrap(),
        s2["agent"]["id"].as_str().unwrap(),
    );
    assert_ne!(i1, parent, "subagent sealed as its parent");
    // Finding 131 (Amis, live): <parent id>-<first 8 of agent_id>.
    assert_eq!(i1, format!("{parent}-a1b2c3d4"));
    assert_eq!(i2, format!("{parent}-a9e8d7c6"));
    assert_ne!(i1, i2, "two subagents share one identity");
    assert!(
        i1.starts_with(&parent) && i2.starts_with(&parent),
        "{parent} / {i1} / {i2}"
    );
    assert_ne!(
        s1["spec_id"], s2["spec_id"],
        "subagents share one auto spec"
    );
    // Amis's plan: "auto-<short>-<agent8>" for a subagent.
    let sid = s1["spec_id"].as_str().unwrap();
    assert!(
        sid.starts_with("auto-") && sid.ends_with("-a1b2c3d4"),
        "subagent auto spec id {sid}"
    );
    // Its auto spec names the parent.
    let spec = p.spec_json(s1["spec_id"].as_str().unwrap());
    assert!(
        spec.to_string().contains(&parent),
        "subagent auto spec does not name its parent {parent}: {spec}"
    );
    // The same subagent again: same identity, same spec.
    p.write("b.txt", &numbered(20, &[(2, "sub1")]));
    all_exit_zero(
        &sub_edit(
            &p,
            "sess-p",
            ("a1b2c3d4e5f6a7b8c", "general-purpose"),
            "b.txt",
        ),
        "sub1 again",
    );
    let s3 = p.newest_seal();
    assert_eq!(s3["agent"]["id"], i1);
    assert_eq!(s3["spec_id"], s1["spec_id"]);
}

#[test]
fn si_subagent_stop_and_parent_session_end_close_the_subagents_auto_spec() {
    let p = Project::new("sub-end");
    p.write("b.txt", &numbered(20, &[(1, "sub1")]));
    all_exit_zero(
        &sub_edit(
            &p,
            "sess-p",
            ("a1b2c3d4e5f6a7b8c", "general-purpose"),
            "b.txt",
        ),
        "sub1",
    );
    let spec = p.newest_seal()["spec_id"].as_str().unwrap().to_string();
    let mut stop = json!({"stop_hook_active": false});
    stop[SUBAGENT_ID_FIELD] = json!("a1b2c3d4e5f6a7b8c");
    stop[SUBAGENT_TYPE_FIELD] = json!("general-purpose");
    if !p.hook_commands("SubagentStop", None).is_empty() {
        all_exit_zero(
            &p.event(None, "sess-p", "SubagentStop", stop),
            "SubagentStop",
        );
    }
    all_exit_zero(
        &p.event(None, "sess-p", "SessionEnd", json!({"reason": "other"})),
        "SessionEnd",
    );
    assert_eq!(p.spec_json(&spec)["status"], "complete");
}

// ---------------------------------------------------------------------------
// Zero-instruction template (Amis item 3)
// ---------------------------------------------------------------------------

const MARKER_BEGIN: &str = "<!-- BEGIN WRIT CONFIGURATION";
const MARKER_END: &str = "<!-- END WRIT CONFIGURATION -->";
/// Words that make a line an instruction the agent must follow.
const MANDATORY: [&str; 6] = [
    "MANDATORY",
    "MUST",
    "EVERY TASK",
    "NOT complete",
    "BEFORE",
    "FIRST ACTION",
];

fn managed_lines(content: &str) -> Vec<String> {
    let start = content.find(MARKER_BEGIN).expect("managed block");
    let rest = &content[start..];
    let end = rest.find(MARKER_END).expect("managed block end");
    rest[..end]
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

fn assert_zero_instruction(surface: &str, lines: &[String]) {
    assert!(
        lines.len() <= 5,
        "{surface}: {} lines (ruling 171: at most 5):\n{}",
        lines.len(),
        lines.join("\n")
    );
    let all = lines.join("\n");
    for w in MANDATORY {
        assert!(
            !all.contains(w),
            "{surface}: mandatory wording {w:?}:\n{all}"
        );
    }
    assert!(
        !all.contains("WRIT_AGENT_ID"),
        "{surface}: identity instruction left:\n{all}"
    );
    // Decision 167 (finding 146): exactly one `writ seal`, on the line that
    // tells agents in other tools (no hook) how to seal; Claude Code seals
    // automatically.
    // "writ seal -s": 175's "writ seals the edits" also contains "writ seal".
    let seal_lines: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("writ seal -s"))
        .collect();
    assert_eq!(
        all.matches("writ seal -s").count(),
        1,
        "{surface}: want exactly one `writ seal`:\n{all}"
    );
    assert!(
        seal_lines[0].contains("in other tools"),
        "{surface}: the `writ seal` line must be the 'in other tools' line: {}",
        seal_lines[0]
    );
    assert!(
        all.contains("writ context"),
        "{surface}: must say writ context exists:\n{all}"
    );
    assert!(
        all.contains("writ spec done -s"),
        "{surface}: must name spec done -s:\n{all}"
    );
}

#[test]
fn tp_claude_md_block_is_under_five_lines_with_no_mandatory_command() {
    let p = Project::new("tpl-claude");
    let lines = managed_lines(&p.read("CLAUDE.md"));
    assert_zero_instruction("CLAUDE.md", &lines);
}

#[test]
fn tp_agents_md_block_matches_and_is_under_five_lines() {
    let p = Project::new("tpl-agents");
    p.write("AGENTS.md", "# agents\n");
    p.ok("human", &["init", "-y"]);
    let a = managed_lines(&p.read("AGENTS.md"));
    assert_zero_instruction("AGENTS.md", &a);
    let c = managed_lines(&p.read("CLAUDE.md"));
    let strip = |v: &[String]| -> Vec<String> {
        v.iter().filter(|l| !l.starts_with('#')).cloned().collect()
    };
    assert_eq!(
        strip(&a),
        strip(&c),
        "AGENTS.md and CLAUDE.md blocks differ beyond the heading"
    );
}

#[test]
fn tp_session_start_echo_and_settings_instructions_carry_no_mandatory_workflow() {
    let p = Project::new("tpl-hook");
    let cmds = p.hook_commands("SessionStart", None);
    assert!(!cmds.is_empty());
    let runs = p.event(
        Some(ALPHA),
        "sess-a",
        "SessionStart",
        json!({"source": "startup"}),
    );
    all_exit_zero(&runs, "SessionStart");
    // What the agent reads from the hook, minus the brief context body.
    let echoed: Vec<String> = runs[0]
        .stdout()
        .lines()
        .take_while(|l| !l.starts_with("# writ context"))
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    assert_zero_instruction("SessionStart echo", &echoed);
    let instr = p.settings()["instructions"].to_string();
    for w in MANDATORY {
        assert!(
            !instr.contains(w),
            "settings.json instructions: {w:?}:\n{instr}"
        );
    }
}

#[test]
fn tp_skills_carry_no_mandatory_seal_workflow() {
    let p = Project::new("tpl-skills");
    let dir = p.path(".claude/skills");
    let Ok(rd) = fs::read_dir(&dir) else { return };
    for e in rd.flatten() {
        let f = e.path().join("SKILL.md");
        let Ok(body) = fs::read_to_string(&f) else {
            continue;
        };
        for w in ["MANDATORY", "You MUST run", "EVERY TASK"] {
            assert!(!body.contains(w), "{}: {w:?}", f.display());
        }
    }
}

// ---------------------------------------------------------------------------
// Ruling 149 (findings 147/148): the hook never blocks on the store lock.
// `seal --auto` waits at most 200 ms; on timeout it records the path on the
// session's touched list and exits 0 (release: under 50 ms after the wait);
// every later hook of that session drains its own list first; a lock timeout
// is logged once per path and never handed to the model.
// ---------------------------------------------------------------------------

/// Hold `.writ/writ.lock` (flock, as RepoLock takes it) in a child process.
fn hold_store_lock(p: &Project) -> std::process::Child {
    let lock = p.path(".writ/writ.lock");
    let mut child = Command::new("python3")
        .arg("-c")
        .arg(
            "import fcntl,sys,time\nf=open(sys.argv[1],'a')\nfcntl.flock(f,fcntl.LOCK_EX)\n\
             print('locked',flush=True)\ntime.sleep(60)",
        )
        .arg(&lock)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut buf = [0u8; 7];
    use std::io::Read;
    child.stdout.as_mut().unwrap().read_exact(&mut buf).unwrap();
    assert_eq!(&buf[..6], b"locked");
    child
}

#[test]
fn as_lock_held_hook_returns_defers_logs_once_and_next_hook_drains() {
    let p = Project::new("lock");
    p.spec("sa", ALPHA);
    let before = p.seal_count();
    let mut holder = hold_store_lock(&p);
    p.write("a.txt", &numbered(20, &[(3, "A")]));
    let runs = p.edit(Some(ALPHA), "sess-a", "Edit", "a.txt");
    all_exit_zero(&runs, "lock held");
    nothing_to_agent(&runs, "lock held (a lock timeout is never surfaced)");
    for r in &runs {
        assert!(
            r.elapsed < Duration::from_secs(2),
            "hook blocked on the lock for {:?}",
            r.elapsed
        );
    }
    let logged = p.hooks_log().matches("a.txt").count();
    assert!(logged >= 1, "lock timeout for a.txt not in hooks.log");
    // Same path again under the same lock: logged once per path.
    p.write("a.txt", &numbered(20, &[(3, "A"), (4, "A")]));
    all_exit_zero(
        &p.edit(Some(ALPHA), "sess-a", "Edit", "a.txt"),
        "lock held again",
    );
    assert_eq!(
        p.hooks_log().matches("a.txt").count(),
        logged,
        "lock timeout logged again for the same path"
    );
    let _ = holder.kill();
    let _ = holder.wait();
    assert_eq!(p.seal_count(), before, "sealed while the lock was held");
    // Lock free: the next hook of the session drains a.txt, then seals b.txt.
    p.write("b.txt", &numbered(20, &[(5, "A")]));
    all_exit_zero(
        &p.edit(Some(ALPHA), "sess-a", "Edit", "b.txt"),
        "after release",
    );
    let sealed: BTreeSet<String> = p
        .seals()
        .into_iter()
        .take(p.seal_count() - before)
        .flat_map(|s| {
            assert_eq!(s["spec_id"], "sa", "{s}");
            paths_of(&s)
        })
        .collect();
    assert_eq!(
        sealed,
        set(&["a.txt", "b.txt"]),
        "deferred path not drained"
    );
    let st = text(&p.ok("human", &["diff", "--stat"]));
    assert!(
        !st.contains("a.txt") && !st.contains("b.txt"),
        "still pending:\n{st}"
    );
}

// ---------------------------------------------------------------------------
// Ruling 155 (finding 153): in bypass permissions mode Claude Code tells
// agents to change files through Bash, so the Edit|Write hook sees nothing.
// A PreToolUse hook on Bash snapshots the tree state per identity; the
// PostToolUse hook on Bash seals exactly the paths that call changed, under
// the identity's spec, summary "auto: Bash <paths>"; 149 (touched list,
// drain) and 141 (refusal line) apply unchanged; never a path pending for
// another live identity at the snapshot; Stop/SubagentStop sweep as
// fallback. Finding 154: N concurrent hooks write N parseable JSONL records.
// The tests drive the hooks exactly as Claude Code does: Pre (stdin with
// tool_name Bash, tool_input.command, tool_use_id), the command itself run
// by the test in the project root, then Post with the same tool_use_id.
// ---------------------------------------------------------------------------

fn bash_stdin(p: &Project, session: &str, event: &str, cmd: &str, id: &str) -> Value {
    let mut v = base_stdin(&p.root, session, event);
    v["tool_name"] = json!("Bash");
    v["tool_input"] = json!({"command": cmd, "description": "run"});
    v["tool_use_id"] = json!(id);
    if event == "PostToolUse" {
        v["tool_response"] = json!({"stdout": "", "stderr": "", "interrupted": false});
    }
    v
}

fn bash_pre(p: &Project, agent: &str, session: &str, cmd: &str, id: &str) -> Vec<HookRun> {
    let stdin = bash_stdin(p, session, "PreToolUse", cmd, id);
    p.fire("PreToolUse", Some("Bash"), Some(agent), &stdin)
}

fn bash_post(p: &Project, agent: &str, session: &str, cmd: &str, id: &str) -> Vec<HookRun> {
    let stdin = bash_stdin(p, session, "PostToolUse", cmd, id);
    p.fire("PostToolUse", Some("Bash"), Some(agent), &stdin)
}

/// Distinct instants for interval and mtime comparisons (ruling 160).
fn tick() {
    std::thread::sleep(Duration::from_millis(30));
}

fn run_sh(p: &Project, cmd: &str) {
    let out = p
        .cmd("sh", Some("human"))
        .arg("-c")
        .arg(cmd)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "fixture command `{cmd}`: {}",
        text(&out)
    );
}

/// Pre, the command, Post: one Bash tool call as Claude Code runs it.
fn bash_call(p: &Project, agent: &str, session: &str, cmd: &str, id: &str) -> Vec<HookRun> {
    let mut runs = bash_pre(p, agent, session, cmd, id);
    run_sh(p, cmd);
    runs.extend(bash_post(p, agent, session, cmd, id));
    runs
}

fn seals_since(p: &Project, before: usize) -> Vec<Value> {
    let all = p.seals();
    all.into_iter().take(p.seal_count() - before).collect()
}

#[test]
fn bh_init_installs_pre_and_post_tool_use_on_bash_and_keeps_edit_write() {
    let p = Project::new("bh-install");
    assert!(
        !p.hook_commands("PreToolUse", Some("Bash")).is_empty(),
        "no writ PreToolUse hook on Bash"
    );
    assert!(
        !p.hook_commands("PostToolUse", Some("Bash")).is_empty(),
        "no writ PostToolUse hook on Bash"
    );
    for tool in EDIT_TOOLS {
        assert!(
            !p.hook_commands("PostToolUse", Some(tool)).is_empty(),
            "Edit|Write hook dropped for {tool}"
        );
    }
}

#[test]
fn bh_heredoc_write_seals_exactly_the_new_path() {
    let p = Project::new("bh-heredoc");
    p.spec("sa", ALPHA);
    let before = p.seal_count();
    let cmd = "cat > new_by_bash.txt <<'EOF'\nmade by a heredoc\nEOF";
    let runs = bash_call(&p, ALPHA, "sess-a", cmd, "toolu_h1");
    all_exit_zero(&runs, "heredoc");
    nothing_to_agent(&runs, "heredoc");
    let new = seals_since(&p, before);
    assert_eq!(new.len(), 1, "{new:?}");
    assert_eq!(new[0]["agent"]["id"], ALPHA);
    assert_eq!(new[0]["spec_id"], "sa");
    assert_eq!(paths_of(&new[0]), set(&["new_by_bash.txt"]));
    let summary = new[0]["summary"].as_str().unwrap();
    assert!(
        summary.starts_with("auto: Bash") && summary.contains("new_by_bash.txt"),
        "summary {summary:?}"
    );
}

#[test]
fn bh_sed_in_place_seals_exactly_the_changed_path_and_no_untouched_file() {
    let p = Project::new("bh-sed");
    p.spec("sa", ALPHA);
    // alpha already has an unrelated pending file before the call.
    p.write("b.txt", &numbered(20, &[(1, "pending before the call")]));
    let before = p.seal_count();
    // Portable in-place edit (BSD and GNU sed); the backup is gone by Post.
    let cmd = "sed -i.orig 's/^line 3$/line 3 by sed/' a.txt && rm a.txt.orig";
    let runs = bash_call(&p, ALPHA, "sess-a", cmd, "toolu_s1");
    all_exit_zero(&runs, "sed");
    let new = seals_since(&p, before);
    assert_eq!(new.len(), 1, "{new:?}");
    assert_eq!(
        paths_of(&new[0]),
        set(&["a.txt"]),
        "sealed beyond the changed path"
    );
    assert!(p.read("a.txt").contains("line 3 by sed"));
    let st = text(&p.ok("human", &["diff", "--stat"]));
    assert!(
        st.contains("b.txt"),
        "the untouched pending file was sealed:\n{st}"
    );
}

#[test]
fn bh_call_that_changes_nothing_seals_nothing_and_prints_nothing() {
    let p = Project::new("bh-noop");
    p.spec("sa", ALPHA);
    p.write("b.txt", &numbered(20, &[(1, "pending")]));
    let before = p.seal_count();
    let runs = bash_call(
        &p,
        ALPHA,
        "sess-a",
        "ls -la && cat a.txt >/dev/null",
        "toolu_n1",
    );
    all_exit_zero(&runs, "no-op");
    nothing_to_agent(&runs, "no-op");
    assert_eq!(
        p.seal_count(),
        before,
        "a read-only Bash call sealed something"
    );
    assert!(
        p.auto_specs().is_empty(),
        "a read-only Bash call created an auto spec"
    );
}

#[test]
fn bh_overlapping_calls_on_different_files_each_seal_their_own() {
    // Ruling 160: each write instant inside exactly one interval.
    // A: Pre .. write a .. [B: Pre] .. Post(A) .. [B: write b .. Post(B)].
    let p = Project::new("bh-overlap");
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    let (ca, cb) = ("printf 'A\\n' >> a.txt", "printf 'B\\n' >> b.txt");
    let before = p.seal_count();
    let mut runs = bash_pre(&p, ALPHA, "sess-a", ca, "toolu_a");
    tick();
    run_sh(&p, ca);
    tick();
    runs.extend(bash_pre(&p, BRAVO, "sess-b", cb, "toolu_b"));
    tick();
    runs.extend(bash_post(&p, ALPHA, "sess-a", ca, "toolu_a"));
    tick();
    run_sh(&p, cb);
    tick();
    runs.extend(bash_post(&p, BRAVO, "sess-b", cb, "toolu_b"));
    all_exit_zero(&runs, "overlap");
    let new = seals_since(&p, before);
    for s in &new {
        let who = s["agent"]["id"].as_str().unwrap();
        let want = if who == ALPHA { "a.txt" } else { "b.txt" };
        assert_eq!(
            paths_of(s),
            set(&[want]),
            "{who} sealed the other's file: {s}"
        );
    }
    let agents: BTreeSet<String> = new
        .iter()
        .map(|s| s["agent"]["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(agents, set(&[ALPHA, BRAVO]), "{new:?}");
}

#[test]
fn bh_same_file_same_content_from_two_identities_is_one_seal() {
    // A writes inside its own interval and seals; B, overlapping A, then
    // writes the same content (new mtime, same bytes): nothing to seal.
    let p = Project::new("bh-same");
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    let content = numbered(40, &[(7, "same")]);
    let cmd = format!("cat > shared.txt <<'EOF'\n{content}EOF");
    let before = p.seal_count();
    let mut runs = bash_pre(&p, ALPHA, "sess-a", &cmd, "toolu_a");
    tick();
    run_sh(&p, &cmd);
    tick();
    runs.extend(bash_pre(&p, BRAVO, "sess-b", &cmd, "toolu_b"));
    tick();
    runs.extend(bash_post(&p, ALPHA, "sess-a", &cmd, "toolu_a"));
    tick();
    run_sh(&p, &cmd);
    tick();
    runs.extend(bash_post(&p, BRAVO, "sess-b", &cmd, "toolu_b"));
    all_exit_zero(&runs, "same content");
    nothing_to_agent(&runs, "same content");
    let new = seals_since(&p, before);
    assert_eq!(new.len(), 1, "same content sealed twice: {new:?}");
    assert_eq!(new[0]["agent"]["id"], ALPHA);
    assert_eq!(paths_of(&new[0]), set(&["shared.txt"]));
}

#[test]
fn bh_same_file_successor_keeps_lines_two_seals_in_order() {
    let p = Project::new("bh-succ");
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    let ca = "sed -i.orig 's/^line 2$/line 2 A/' shared.txt && rm shared.txt.orig";
    let cb = "sed -i.orig 's/^line 30$/line 30 B/' shared.txt && rm shared.txt.orig";
    let before = p.seal_count();
    let mut runs = bash_pre(&p, ALPHA, "sess-a", ca, "toolu_a");
    tick();
    run_sh(&p, ca);
    tick();
    runs.extend(bash_pre(&p, BRAVO, "sess-b", cb, "toolu_b"));
    tick();
    runs.extend(bash_post(&p, ALPHA, "sess-a", ca, "toolu_a"));
    tick();
    run_sh(&p, cb);
    tick();
    runs.extend(bash_post(&p, BRAVO, "sess-b", cb, "toolu_b"));
    all_exit_zero(&runs, "successor");
    let new: Vec<Value> = seals_since(&p, before).into_iter().rev().collect();
    let order: Vec<&str> = new
        .iter()
        .map(|s| s["agent"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(order, vec![ALPHA, BRAVO], "{new:?}");
    let final_text = p.read("shared.txt");
    assert!(final_text.contains("line 2 A") && final_text.contains("line 30 B"));
}

#[test]
fn bh_same_file_successor_that_drops_the_others_line_is_refused_with_the_141_line() {
    let p = Project::new("bh-refuse");
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    let ca = "sed -i.orig 's/^line 2$/line 2 A/' shared.txt && rm shared.txt.orig";
    // bravo rewrites the whole file from a stale copy: alpha's line is gone.
    let stale = numbered(40, &[(30, "B")]);
    let cb = format!("cat > shared.txt <<'EOF'\n{stale}EOF");
    let mut runs = bash_pre(&p, ALPHA, "sess-a", ca, "toolu_a");
    tick();
    run_sh(&p, ca);
    tick();
    runs.extend(bash_pre(&p, BRAVO, "sess-b", &cb, "toolu_b"));
    tick();
    runs.extend(bash_post(&p, ALPHA, "sess-a", ca, "toolu_a"));
    all_exit_zero(&runs, "alpha");
    let before = p.seal_count();
    tick();
    run_sh(&p, &cb);
    tick();
    let post = bash_post(&p, BRAVO, "sess-b", &cb, "toolu_b");
    assert_eq!(
        p.seal_count(),
        before,
        "bravo's loss-making seal went through"
    );
    let told: Vec<String> = post.iter().filter_map(HookRun::to_agent).collect();
    assert_eq!(told.len(), 1, "{told:?}");
    let msg = told[0].trim();
    assert_eq!(msg.lines().count(), 1, "{msg}");
    assert!(
        msg.contains("shared.txt") && msg.contains(ALPHA) && msg.contains("fix:"),
        "{msg}"
    );
    assert!(!msg.contains("--force"), "{msg}");
}

#[test]
fn bh_path_pending_for_another_live_identity_at_the_snapshot_is_never_sealed() {
    let p = Project::new("bh-foreign");
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    // bravo's call is open: its Pre ran and it changed b.txt; no Post yet.
    let cb = "printf 'B\\n' >> b.txt";
    let mut runs = bash_pre(&p, BRAVO, "sess-b", cb, "toolu_b");
    tick();
    run_sh(&p, cb);
    tick();
    // alpha's call (snapshot after bravo's change) touches a.txt AND b.txt.
    let ca = "printf 'A\\n' >> a.txt && printf 'A too\\n' >> b.txt";
    let before = p.seal_count();
    runs.extend(bash_call(&p, ALPHA, "sess-a", ca, "toolu_a"));
    tick();
    runs.extend(bash_post(&p, BRAVO, "sess-b", cb, "toolu_b"));
    all_exit_zero(&runs, "foreign pending");
    for s in seals_since(&p, before) {
        if s["agent"]["id"] == ALPHA {
            assert!(
                !paths_of(&s).contains("b.txt"),
                "alpha sealed bravo's pending b.txt: {s}"
            );
        }
    }
}

#[test]
fn bh_stop_sweeps_a_bash_changed_path_the_post_hook_missed_and_nothing_foreign() {
    let p = Project::new("bh-stop");
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    // alpha's Pre runs and it writes c.txt (only alpha's interval is open);
    // its Post never arrives. Then bravo's call opens and writes b.txt.
    let ca = "printf 'A\\n' >> c.txt";
    let mut runs = bash_pre(&p, ALPHA, "sess-a", ca, "toolu_a");
    tick();
    run_sh(&p, ca);
    tick();
    let cb = "printf 'B\\n' >> b.txt";
    runs.extend(bash_pre(&p, BRAVO, "sess-b", cb, "toolu_b"));
    tick();
    run_sh(&p, cb);
    tick();
    let before = p.seal_count();
    runs.extend(p.event(
        Some(ALPHA),
        "sess-a",
        "Stop",
        json!({"stop_hook_active": false}),
    ));
    all_exit_zero(&runs, "stop sweep");
    let new = seals_since(&p, before);
    assert_eq!(new.len(), 1, "{new:?}");
    assert_eq!(new[0]["agent"]["id"], ALPHA);
    assert_eq!(
        paths_of(&new[0]),
        set(&["c.txt"]),
        "Stop swept beyond its own Bash change"
    );
}

/// Finding 154: N concurrent hook runs write N parseable JSONL records.
#[test]
fn hl_concurrent_hooks_write_one_parseable_jsonl_record_each() {
    let p = Project::new("hl-concurrent");
    let cmds = p.hook_commands("UserPromptSubmit", None);
    assert!(!cmds.is_empty());
    let n = 24;
    let children: Vec<std::process::Child> = (0..n)
        .map(|i| {
            let stdin = {
                let mut v = base_stdin(&p.root, &format!("sess-{i}"), "UserPromptSubmit");
                v["prompt"] = json!(format!("task {i}"));
                v.to_string()
            };
            let mut c = p
                .cmd("sh", Some(&format!("agent-{i}")))
                .arg("-c")
                .arg(&cmds[0])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            c.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
            c
        })
        .collect();
    for mut c in children {
        assert_eq!(c.wait().unwrap().code(), Some(0));
    }
    let log = p.hooks_log();
    let lines: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
    let bad: Vec<&&str> = lines
        .iter()
        .filter(|l| serde_json::from_str::<Value>(l).is_err())
        .collect();
    assert!(
        bad.is_empty(),
        "{} unparseable hooks.log line(s): {:?}",
        bad.len(),
        &bad[..bad.len().min(3)]
    );
    assert!(
        lines.len() >= n,
        "{} records for {n} hook runs",
        lines.len()
    );
}

// -- Ruling 160: attribution by the write instant (the path's mtime from the
// Post diff). Candidates are identities whose Bash interval (Pre to Post, or
// Pre to now if open) contains the instant; one seals; several seal to
// nobody and each gets the line "<path> changed while N agents ran commands;
// writ could not tell whose change it is; if it is yours: writ seal --paths
// <path>"; zero (mtime outside every interval, e.g. cp -p) falls back to "no
// other interval open at any point during this call", else nobody. Command
// text is never a signal.

const AMBIGUOUS: &str = "writ could not tell whose change it is";

fn assert_ambiguous_line(runs: &[HookRun], path: &str, n: usize, who: &str) {
    let told: Vec<String> = runs.iter().filter_map(HookRun::to_agent).collect();
    assert_eq!(told.len(), 1, "{who}: want exactly one line, got {told:?}");
    let msg = told[0].trim();
    assert_eq!(msg.lines().count(), 1, "{msg}");
    assert!(msg.contains(path), "{msg}");
    assert!(
        msg.contains(&format!("changed while {n} agents ran commands")),
        "{msg}"
    );
    assert!(msg.contains(AMBIGUOUS), "{msg}");
    assert!(msg.contains(&format!("writ seal --paths {path}")), "{msg}");
}

/// (a1) A's command only reads and its interval closed before B's write.
#[test]
fn r160_reader_interval_closed_before_the_write_writer_seals() {
    let p = Project::new("r160-a1");
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    let ca = "cat shared.txt >/dev/null && true";
    let cb = "sed -i.orig 's/^line 5$/line 5 B/' shared.txt && rm shared.txt.orig";
    let before = p.seal_count();
    let mut runs = bash_pre(&p, ALPHA, "sess-a", ca, "toolu_a");
    run_sh(&p, ca);
    tick();
    runs.extend(bash_post(&p, ALPHA, "sess-a", ca, "toolu_a"));
    tick();
    runs.extend(bash_call(&p, BRAVO, "sess-b", cb, "toolu_b"));
    all_exit_zero(&runs, "a1");
    nothing_to_agent(&runs, "a1");
    let new = seals_since(&p, before);
    assert_eq!(new.len(), 1, "{new:?}");
    assert_eq!(new[0]["agent"]["id"], BRAVO);
    assert_eq!(paths_of(&new[0]), set(&["shared.txt"]));
}

/// (a2) A's (reading) interval is open at B's write instant: ambiguous.
/// Nobody seals, both are told, the path stays pending, and the owner's
/// hand seal clears it.
#[test]
fn r160_reader_interval_open_at_the_write_instant_nobody_seals_both_told() {
    let p = Project::new("r160-a2");
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    let ca = "cat shared.txt >/dev/null && sleep 0";
    let cb = "sed -i.orig 's/^line 5$/line 5 B/' shared.txt && rm shared.txt.orig";
    let before = p.seal_count();
    let pre_a = bash_pre(&p, ALPHA, "sess-a", ca, "toolu_a");
    tick();
    let pre_b = bash_pre(&p, BRAVO, "sess-b", cb, "toolu_b");
    tick();
    run_sh(&p, cb);
    tick();
    let post_b = bash_post(&p, BRAVO, "sess-b", cb, "toolu_b");
    tick();
    let post_a = bash_post(&p, ALPHA, "sess-a", ca, "toolu_a");
    for r in [&pre_a, &pre_b, &post_a, &post_b] {
        all_exit_zero(r, "a2");
    }
    assert_eq!(p.seal_count(), before, "an ambiguous write was sealed");
    assert_ambiguous_line(&post_b, "shared.txt", 2, "bravo");
    assert_ambiguous_line(&post_a, "shared.txt", 2, "alpha");
    let st = text(&p.ok("human", &["diff", "--stat"]));
    assert!(st.contains("shared.txt"), "not pending:\n{st}");
    let fix = p.writ(
        BRAVO,
        &[
            "seal",
            "-s",
            "mine",
            "--agent",
            BRAVO,
            "--paths",
            "shared.txt",
        ],
    );
    assert!(fix.status.success(), "{}", text(&fix));
    assert_eq!(p.newest_seal()["agent"]["id"], BRAVO);
}

/// An old mtime (cp -p) is outside every interval.
fn old_copy(p: &Project, name: &str, content: &str) -> PathBuf {
    let f = p.home.join(name);
    fs::write(&f, content).unwrap();
    let out = Command::new("touch")
        .args(["-t", "200001010000"])
        .arg(&f)
        .output()
        .unwrap();
    assert!(out.status.success());
    f
}

/// (b1) cp -p during A's call, no other interval open at any point: A seals.
#[test]
fn r160_old_mtime_with_no_other_interval_open_is_sealed_by_the_caller() {
    let p = Project::new("r160-b1");
    p.spec("sa", ALPHA);
    let src = old_copy(&p, "old.txt", &numbered(20, &[(9, "restored")]));
    let cmd = format!("cp -p {} a.txt", src.display());
    let before = p.seal_count();
    let runs = bash_call(&p, ALPHA, "sess-a", &cmd, "toolu_a");
    all_exit_zero(&runs, "b1");
    nothing_to_agent(&runs, "b1");
    let new = seals_since(&p, before);
    assert_eq!(new.len(), 1, "{new:?}");
    assert_eq!(new[0]["agent"]["id"], ALPHA);
    assert_eq!(paths_of(&new[0]), set(&["a.txt"]));
}

/// (b2) The same cp -p while B's interval is open: nobody seals; A is told.
#[test]
fn r160_old_mtime_with_another_interval_open_seals_to_nobody() {
    let p = Project::new("r160-b2");
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    let src = old_copy(&p, "old.txt", &numbered(20, &[(9, "restored")]));
    // Through a script: the text names no path, so rule 1 cannot decide.
    fs::write(
        p.home.join("restore.sh"),
        format!("cp -p {} a.txt\n", src.display()),
    )
    .unwrap();
    let cmd = "sh \"$HOME/restore.sh\"".to_string();
    let cb = "ls >/dev/null";
    let before = p.seal_count();
    let mut runs = bash_pre(&p, BRAVO, "sess-b", cb, "toolu_b");
    tick();
    let call_a = bash_call(&p, ALPHA, "sess-a", &cmd, "toolu_a");
    tick();
    let post_b = bash_post(&p, BRAVO, "sess-b", cb, "toolu_b");
    runs.extend(post_b);
    all_exit_zero(&runs, "b2 bravo");
    all_exit_zero(&call_a, "b2 alpha");
    assert!(
        !seals_since(&p, before)
            .iter()
            .any(|s| paths_of(s).contains("a.txt")),
        "a.txt sealed although another interval was open"
    );
    let told: Vec<String> = call_a.iter().filter_map(HookRun::to_agent).collect();
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(
        told[0].contains("a.txt") && told[0].contains("writ seal --paths a.txt"),
        "{}",
        told[0]
    );
}

// ---------------------------------------------------------------------------
// Findings 168, 169, 170 (fleet rerun on c40984128bf8; CC's rulings).
// 168: a printed fix clears the state in every state it can be printed in:
//   the 160 fix pasted after the agent closed its auto spec opens a
//   follow-up auto spec (`<auto id>-followup`) and seals; pasted by a
//   subagent (whose shell resolves to the parent) it seals under the
//   subagent's identity; several paths print as ONE comma-separated fix.
// 169: an ambiguous path never becomes an anyone_may_fix left_out; its
//   finding names the candidates and is for one of them; a third identity
//   running the fix is refused with a line naming the candidates.
// 170: auto spec title = the prompt's first sentence, cut at a word
//   boundary under 60 characters with "…", never mid-word; the commit
//   subject follows.
// ---------------------------------------------------------------------------

/// One Bash hook event as `agent` (None: framework identity from the
/// session), optionally inside subagent `sub` (stdin agent_id/agent_type).
fn bash_hook(
    p: &Project,
    agent: Option<&str>,
    session: &str,
    event: &str,
    cmd: &str,
    id: &str,
    sub: Option<&str>,
) -> Vec<HookRun> {
    let mut stdin = bash_stdin(p, session, event, cmd, id);
    if let Some(a) = sub {
        stdin[SUBAGENT_ID_FIELD] = json!(a);
        stdin[SUBAGENT_TYPE_FIELD] = json!("general-purpose");
    }
    p.fire(event, Some("Bash"), agent, &stdin)
}

/// The command after "if it is yours: " (or "fix: ") in a hook line.
fn printed_fix(line: &str) -> String {
    let tail = line
        .split("if it is yours: ")
        .nth(1)
        .or_else(|| line.split("fix: ").nth(1))
        .unwrap_or_else(|| panic!("no fix in: {line}"));
    tail.split(';')
        .next()
        .unwrap()
        .trim()
        .trim_matches('`')
        .to_string()
}

/// Run a printed `writ ...` fix through sh in `agent`'s shell (None: the
/// Claude Code shell of `session`, as a subagent's Bash tool has).
fn paste(p: &Project, agent: Option<&str>, session: &str, fix: &str) -> Output {
    assert!(fix.starts_with("writ "), "not a writ command: {fix}");
    p.cmd("sh", agent)
        .env("CLAUDE_CODE_SESSION_ID", session)
        .arg("-c")
        .arg(fix)
        .output()
        .unwrap()
}

/// alpha's read-only call is open while bravo writes `paths` (ruling 160):
/// both hooks print the ambiguity line. Returns (alpha's runs, bravo's runs).
fn ambiguity(p: &Project, paths: &[&str], tag: &str) -> (Vec<HookRun>, Vec<HookRun>) {
    let ca = "cat a.txt >/dev/null";
    // bravo writes through a script so neither command text names a path:
    // rule 1 (ruling 189) cannot decide and the 0.4.3 fallback is exercised.
    let script = paths
        .iter()
        .map(|f| format!("printf 'bravo {tag}\\n' >> {f}\n"))
        .collect::<String>();
    fs::write(p.home.join(format!("w_{tag}.sh")), script).unwrap();
    let cb = format!("sh \"$HOME/w_{tag}.sh\"");
    let ida = format!("toolu_a_{tag}");
    let idb = format!("toolu_b_{tag}");
    let mut a = bash_hook(p, Some(ALPHA), "sess-a", "PreToolUse", ca, &ida, None);
    tick();
    let mut b = bash_hook(p, Some(BRAVO), "sess-b", "PreToolUse", &cb, &idb, None);
    tick();
    run_sh(p, &cb);
    tick();
    b.extend(bash_hook(
        p,
        Some(BRAVO),
        "sess-b",
        "PostToolUse",
        &cb,
        &idb,
        None,
    ));
    tick();
    a.extend(bash_hook(
        p,
        Some(ALPHA),
        "sess-a",
        "PostToolUse",
        ca,
        &ida,
        None,
    ));
    all_exit_zero(&a, "ambiguity alpha");
    all_exit_zero(&b, "ambiguity bravo");
    (a, b)
}

fn one_line(runs: &[HookRun], who: &str) -> String {
    let told: Vec<String> = runs.iter().filter_map(HookRun::to_agent).collect();
    assert_eq!(told.len(), 1, "{who}: want exactly one line, got {told:?}");
    told[0].trim().to_string()
}

#[test]
fn f168_fix_pasted_after_the_auto_spec_closed_opens_a_followup_and_seals() {
    let p = Project::new("f168-closed");
    // alpha's own Bash write opens its auto spec; alpha then closes it.
    all_exit_zero(
        &bash_call(&p, ALPHA, "sess-a", "printf 'A\\n' >> c.txt", "toolu_c"),
        "alpha write",
    );
    let auto = p.newest_seal()["spec_id"].as_str().unwrap().to_string();
    assert!(auto.starts_with("auto-"), "{auto}");
    p.ok(
        ALPHA,
        &["spec", "done", "-s", "alpha's own words", "--agent", ALPHA],
    );
    p.spec("sb", BRAVO);
    let (a, _b) = ambiguity(&p, &["shared.txt"], "1");
    let line = one_line(&a, "alpha");
    let fix = printed_fix(&line);
    let out = paste(&p, Some(ALPHA), "sess-a", &fix);
    assert!(
        out.status.success(),
        "pasted fix `{fix}` failed:\n{}",
        text(&out)
    );
    let s = p.newest_seal();
    assert_eq!(s["agent"]["id"], ALPHA, "{s}");
    assert_eq!(paths_of(&s), set(&["shared.txt"]), "{s}");
    let spec = s["spec_id"].as_str().unwrap();
    assert_eq!(spec, format!("{auto}-followup"), "follow-up spec id");
    assert_eq!(
        p.spec_json(spec)["continues"],
        auto.as_str(),
        "follow-up records what it continues"
    );
    // The closed spec keeps the agent's words and is not reopened.
    let closed = p.spec_json(&auto);
    assert_eq!(closed["status"], "complete");
    assert!(closed.to_string().contains("alpha's own words"));
    // Pasted twice: still exit 0, nothing new to seal.
    let again = paste(&p, Some(ALPHA), "sess-a", &fix);
    assert!(again.status.success(), "second paste: {}", text(&again));
}

#[test]
fn f168_several_ambiguous_paths_print_one_comma_separated_fix_that_works() {
    let p = Project::new("f168-several");
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    let (a, _b) = ambiguity(&p, &["shared.txt", "b.txt"], "2");
    let line = one_line(&a, "alpha");
    assert_eq!(line.lines().count(), 1, "{line}");
    assert!(
        !line.contains("also:"),
        "one fix for all paths, not 'also:': {line}"
    );
    assert_eq!(line.matches("writ seal").count(), 1, "{line}");
    let fix = printed_fix(&line);
    let paths_arg = fix
        .split("--paths ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    let listed: BTreeSet<String> = paths_arg.split(',').map(str::to_string).collect();
    assert_eq!(listed, set(&["shared.txt", "b.txt"]), "fix: {fix}");
    let out = paste(&p, Some(ALPHA), "sess-a", &fix);
    assert!(out.status.success(), "`{fix}`:\n{}", text(&out));
    assert_eq!(paths_of(&p.newest_seal()), set(&["shared.txt", "b.txt"]));
}

#[test]
fn f168_fix_pasted_by_a_subagent_seals_under_the_subagent() {
    let p = Project::new("f168-sub");
    p.spec("sb", BRAVO);
    // The parent's identity, as its own Bash resolves it.
    all_exit_zero(
        &bash_hook(
            &p,
            None,
            "sess-p",
            "PreToolUse",
            "printf 'P\\n' >> c.txt",
            "toolu_p",
            None,
        ),
        "parent pre",
    );
    run_sh(&p, "printf 'P\\n' >> c.txt");
    all_exit_zero(
        &bash_hook(
            &p,
            None,
            "sess-p",
            "PostToolUse",
            "printf 'P\\n' >> c.txt",
            "toolu_p",
            None,
        ),
        "parent post",
    );
    let parent = p.newest_seal()["agent"]["id"].as_str().unwrap().to_string();
    // The subagent's read-only call is open while bravo writes shared.txt.
    let sub = "a1b2c3d4e5f6a7b8c";
    let cs = "cat a.txt >/dev/null";
    // Through a script: no command text names the path (rule 1 cannot decide).
    fs::write(p.home.join("w.sh"), "printf 'bravo\\n' >> shared.txt\n").unwrap();
    let cb = "sh \"$HOME/w.sh\"";
    let mut s = bash_hook(&p, None, "sess-p", "PreToolUse", cs, "toolu_s", Some(sub));
    tick();
    let mut b = bash_hook(&p, Some(BRAVO), "sess-b", "PreToolUse", cb, "toolu_b", None);
    tick();
    run_sh(&p, cb);
    tick();
    b.extend(bash_hook(
        &p,
        Some(BRAVO),
        "sess-b",
        "PostToolUse",
        cb,
        "toolu_b",
        None,
    ));
    tick();
    s.extend(bash_hook(
        &p,
        None,
        "sess-p",
        "PostToolUse",
        cs,
        "toolu_s",
        Some(sub),
    ));
    all_exit_zero(&s, "subagent");
    let line = one_line(&s, "subagent");
    let fix = printed_fix(&line);
    assert!(
        fix.contains(&format!("--agent {parent}-a1b2c3d4")),
        "the subagent's line must carry its identity explicitly: {fix}"
    );
    // The subagent's Bash shell is the parent's environment, byte for byte.
    let out = paste(&p, None, "sess-p", &fix);
    assert!(out.status.success(), "`{fix}`:\n{}", text(&out));
    let sealed = p.newest_seal();
    assert_eq!(
        sealed["agent"]["id"],
        format!("{parent}-a1b2c3d4"),
        "the subagent's fix sealed under {}",
        sealed["agent"]["id"]
    );
    assert_eq!(paths_of(&sealed), set(&["shared.txt"]));
}

#[test]
fn f169_ambiguous_left_out_names_the_candidates_and_is_not_anyone_may_fix() {
    let p = Project::new("f169-doctor");
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    let _ = ambiguity(&p, &["shared.txt"], "3");
    // Both close their specs, as in the run; the path stays unsealed.
    p.ok(
        ALPHA,
        &["spec", "done", "sa", "--agent", ALPHA, "--no-seal"],
    );
    p.ok(
        BRAVO,
        &["spec", "done", "sb", "--agent", BRAVO, "--no-seal"],
    );
    let d = p.json("charlie", &["doctor", "--format", "json"]);
    let f = d["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["check"] == "left_out" && f.to_string().contains("shared.txt"))
        .cloned()
        .unwrap_or_else(|| panic!("no left_out finding for shared.txt:\n{d:#}"));
    assert_eq!(f["anyone_may_fix"], false, "{f:#}");
    assert_eq!(f["needs_human"], false, "{f:#}");
    let msg = f["message"].as_str().unwrap();
    assert!(
        msg.contains(ALPHA) && msg.contains(BRAVO),
        "candidates not named: {msg}"
    );
    assert!(msg.contains("changed while"), "{msg}");
}

#[test]
fn f169_a_third_identity_running_the_fix_is_refused_naming_the_candidates() {
    let p = Project::new("f169-third");
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    let (a, _b) = ambiguity(&p, &["shared.txt"], "4");
    let fix = printed_fix(&one_line(&a, "alpha"));
    p.ok(
        ALPHA,
        &["spec", "done", "sa", "--agent", ALPHA, "--no-seal"],
    );
    p.ok(
        BRAVO,
        &["spec", "done", "sb", "--agent", BRAVO, "--no-seal"],
    );
    let before = p.seal_count();
    let out = paste(&p, Some("charlie"), "sess-c", &fix);
    assert!(
        !out.status.success(),
        "charlie sealed an ambiguous path: {}",
        text(&out)
    );
    let t = text(&out);
    assert!(
        t.contains(ALPHA) && t.contains(BRAVO),
        "refusal does not name the candidates:\n{t}"
    );
    assert_eq!(p.seal_count(), before);
    // The doctor follow-up shape, as agent-2 used it in the run, is refused too.
    p.ok(
        "charlie",
        &[
            "spec",
            "add",
            "--id",
            "fu",
            "--title",
            "follow up",
            "--claim",
        ],
    );
    let fu = p.writ(
        "charlie",
        &[
            "seal",
            "-s",
            "per doctor",
            "--agent",
            "charlie",
            "--spec",
            "fu",
            "--paths",
            "shared.txt",
        ],
    );
    assert!(
        !fu.status.success(),
        "follow-up seal took over an ambiguous path: {}",
        text(&fu)
    );
}

#[test]
fn f170_auto_title_is_the_first_sentence_cut_at_a_word_under_60_with_ellipsis() {
    for (tag, prompt, sentence) in [
        (
            "long",
            "You are working on a Flask API project in this directory and the work is large. Split it.",
            "You are working on a Flask API project in this directory and the work is large.",
        ),
        (
            "short",
            "Fix the parser. Then add tests for it.",
            "Fix the parser.",
        ),
    ] {
        let p = Project::new(&format!("f170-{tag}"));
        all_exit_zero(
            &p.event(Some(ALPHA), "sess-a", "UserPromptSubmit", json!({"prompt": prompt})),
            "prompt",
        );
        all_exit_zero(&bash_call(&p, ALPHA, "sess-a", "printf 'A\\n' >> c.txt", "toolu_t"), "write");
        let spec = p.newest_seal()["spec_id"].as_str().unwrap().to_string();
        let title = p.spec_json(&spec)["title"].as_str().unwrap().to_string();
        assert!(title.chars().count() < 60, "{tag}: {} chars: {title:?}", title.chars().count());
        if sentence.chars().count() < 60 {
            let bare = sentence.trim_end_matches('.');
            assert!(title.trim_end_matches('.') == bare, "{tag}: want the sentence, got {title:?}");
            assert!(!title.contains('…'), "{tag}: {title:?}");
        } else {
            assert!(title.ends_with('…'), "{tag}: a cut title ends with …: {title:?}");
            let kept = title.trim_end_matches('…').trim_end();
            assert!(sentence.starts_with(kept), "{tag}: not a prefix of the sentence: {title:?}");
            let next = sentence[kept.len()..].chars().next();
            assert!(
                matches!(next, Some(' ') | Some(',') | Some('.')),
                "{tag}: cut mid-word: {title:?} (next char {next:?})"
            );
        }
        // The commit subject follows the title.
        p.ok(ALPHA, &["spec", "done", &spec, "--agent", ALPHA, "--no-seal"]);
        p.ok("human", &["finish", "-y"]);
        let subject = p.git_ok(&["log", "-1", "--format=%s"]);
        assert!(subject.contains(title.trim_end_matches('…').trim_end()), "{tag}: subject {subject:?} vs {title:?}");
        assert!(!subject.contains(&prompt[..60.min(prompt.len())]) || prompt.len() < 60, "{tag}: subject is the raw prompt: {subject:?}");
    }
}

/// Finding 168 (Aubs 4): an identity that has never had a spec pastes the
/// ambiguity fix: exit 0, an auto spec is opened, the path is sealed.
#[test]
fn f168_fix_pasted_with_no_spec_ever_opens_an_auto_spec_and_seals() {
    let p = Project::new("f168-nospec");
    p.spec("sb", BRAVO);
    let (a, _b) = ambiguity(&p, &["shared.txt"], "5");
    assert!(
        p.specs()
            .iter()
            .all(|s| s["claimed_by"] != ALPHA && s["created_by"] != ALPHA),
        "fixture: alpha already had a spec"
    );
    let fix = printed_fix(&one_line(&a, "alpha"));
    let out = paste(&p, Some(ALPHA), "sess-a", &fix);
    assert!(
        out.status.success(),
        "`{fix}` from no spec:\n{}",
        text(&out)
    );
    let s = p.newest_seal();
    assert_eq!(s["agent"]["id"], ALPHA, "{s}");
    assert_eq!(paths_of(&s), set(&["shared.txt"]));
    let spec = s["spec_id"].as_str().unwrap();
    assert!(spec.starts_with("auto-"), "{spec}");
    assert_eq!(p.spec_json(spec)["claimed_by"], ALPHA);
}

/// Finding 173 (exit run A): an ambiguous rewrite of A's sealed file by B
/// must never be drained into A's spec by A's later hooks, and no hook-made
/// seal ever carries allow_removals (the 144 override is the agent's choice
/// after being told, never the hook's).
#[test]
fn f173_drain_never_seals_another_writers_ambiguous_rewrite_and_never_overrides() {
    // Fallback (no command names the path, rule 1 cannot decide): ambiguous.
    let (p, runs_a, _runs_b) = f173_rewrite("f173", false);
    // (b) alpha was told, by a hook line naming the path and bravo.
    let told: Vec<String> = runs_a.iter().filter_map(HookRun::to_agent).collect();
    assert!(
        told.iter()
            .any(|l| l.contains("shared.txt") && l.contains(BRAVO)),
        "alpha was never told about bravo's rewrite: {told:?}"
    );
    f173_after(&p);
}

/// Rule 1 (ruling 189): bravo's heredoc names shared.txt, so bravo owns the
/// rewrite; it drops alpha's sealed line, so bravo's own seal is refused
/// with the 141 line naming alpha; alpha is not asked; alpha's drain still
/// never seals it and no hook seal overrides.
#[test]
fn f173_rule1_writer_named_in_command_is_refused_and_alphas_drain_never_seals() {
    let (p, runs_a, runs_b) = f173_rewrite("f173-r1", true);
    let told_b: Vec<String> = runs_b.iter().filter_map(HookRun::to_agent).collect();
    assert_eq!(
        told_b.len(),
        1,
        "bravo: want one refusal line, got {told_b:?}"
    );
    assert!(
        told_b[0].contains("shared.txt") && told_b[0].contains(ALPHA),
        "bravo's refusal does not name the path and alpha: {}",
        told_b[0]
    );
    assert!(
        !told_b[0].contains(AMBIGUOUS),
        "rule 1 attributed it, yet bravo got the ambiguity line: {}",
        told_b[0]
    );
    let told_a: Vec<String> = runs_a.iter().filter_map(HookRun::to_agent).collect();
    assert!(
        told_a.is_empty(),
        "alpha asked about a path rule 1 gave bravo: {told_a:?}"
    );
    f173_after(&p);
}

/// alpha seals its line in shared.txt, then bravo rewrites the file from a
/// stale copy that drops it while alpha's call is open. `named`: bravo's
/// command names the path (heredoc), else it writes through a script.
/// Returns the project, alpha's and bravo's hook runs.
fn f173_rewrite(tag: &str, named: bool) -> (Project, Vec<HookRun>, Vec<HookRun>) {
    let p = Project::new(tag);
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    // alpha's Edit seals its line.
    p.write("shared.txt", &numbered(40, &[(2, "A")]));
    all_exit_zero(
        &p.edit(Some(ALPHA), "sess-a", "Edit", "shared.txt"),
        "alpha edit",
    );
    assert_eq!(p.newest_seal()["agent"]["id"], ALPHA);
    // alpha runs a long command (pytest); bravo overwrites shared.txt from a
    // stale copy, dropping alpha's line, while alpha's interval is open.
    let ca = "sleep 0";
    let stale = numbered(40, &[(30, "B")]);
    let cb = if named {
        format!("cat > shared.txt <<'EOF'\n{stale}EOF")
    } else {
        fs::write(p.home.join("stale.txt"), &stale).unwrap();
        fs::write(
            p.home.join("restore.sh"),
            "cat \"$HOME/stale.txt\" > \"$1\"\n",
        )
        .unwrap();
        // The target arrives through a variable: the text names no path.
        "T=shared; sh \"$HOME/restore.sh\" \"$T.txt\"".to_string()
    };
    let mut a = bash_pre(&p, ALPHA, "sess-a", ca, "toolu_a");
    tick();
    let mut b = bash_pre(&p, BRAVO, "sess-b", &cb, "toolu_b");
    tick();
    run_sh(&p, &cb);
    tick();
    b.extend(bash_post(&p, BRAVO, "sess-b", &cb, "toolu_b"));
    tick();
    a.extend(bash_post(&p, ALPHA, "sess-a", ca, "toolu_a"));
    all_exit_zero(&a, "rewrite alpha");
    all_exit_zero(&b, "rewrite bravo");
    (p, a, b)
}

/// alpha's next hooks never seal bravo's rewrite; no hook seal overrides;
/// the file stays unsealed and the handoff says nothing false.
fn f173_after(p: &Project) {
    let before = p.seal_count();
    // alpha's next hooks: an Edit elsewhere, then Stop.
    p.write("a.txt", &numbered(20, &[(1, "A")]));
    all_exit_zero(
        &p.edit(Some(ALPHA), "sess-a", "Edit", "a.txt"),
        "alpha next edit",
    );
    all_exit_zero(
        &p.event(
            Some(ALPHA),
            "sess-a",
            "Stop",
            json!({"stop_hook_active": false}),
        ),
        "alpha stop",
    );
    for s in seals_since(p, before) {
        assert!(
            !(s["agent"]["id"] == ALPHA && paths_of(&s).contains("shared.txt")),
            "alpha's drain sealed bravo's rewrite: {s}"
        );
    }
    for s in p.seals() {
        let auto = s["summary"]
            .as_str()
            .is_some_and(|x| x.starts_with("auto:"));
        let ar = &s["allow_removals"];
        assert!(
            !(auto && ar.as_array().is_some_and(|a| !a.is_empty())),
            "(b) a hook-made seal carries allow_removals: {s}"
        );
        // Ruling 178: a hook may record an override for an agent's own fresh
        // edit ("recorded by writ's auto seal"); never "told", never on a
        // drained or ambiguous path (the drained path is asserted unsealed above).
        if auto {
            if let Some(o) = s.get("override_context").filter(|o| !o.is_null()) {
                let o = o.to_string();
                assert!(!o.contains("told"), "(c) a hook-made seal says 'told': {o}");
                assert!(
                    o.contains("recorded by writ's auto seal"),
                    "(c) unlabelled hook override: {o}"
                );
            }
        }
    }
    // The file is still unsealed, and the receipt-to-be says nothing false.
    let st = text(&p.ok("human", &["diff", "--stat"]));
    assert!(st.contains("shared.txt"), "{st}");
    let h = text(&p.ok("human", &["handoff"]));
    assert!(!h.contains("on purpose after being told"), "{h}");
}

/// Finding 173 (a): a queued path is attributed by its latest write instant
/// exactly as a fresh Post would be. Queued for alpha (lock held at its
/// Edit), then rewritten inside bravo's interval only: alpha's drain does
/// not seal it; bravo's Post does.
#[test]
fn f173_drain_attributes_a_queued_path_by_its_latest_write_instant() {
    let p = Project::new("f173-a");
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    let mut holder = hold_store_lock(&p);
    p.write("b.txt", &numbered(20, &[(1, "A")]));
    all_exit_zero(&p.edit(Some(ALPHA), "sess-a", "Edit", "b.txt"), "queued");
    let _ = holder.kill();
    let _ = holder.wait();
    // bravo rewrites b.txt inside its own interval only (keeps alpha's line).
    let cb = "printf 'bravo\\n' >> b.txt";
    let mut runs = bash_pre(&p, BRAVO, "sess-b", cb, "toolu_b");
    tick();
    run_sh(&p, cb);
    tick();
    // alpha's next hook drains first; bravo's Post comes after.
    p.write("a.txt", &numbered(20, &[(1, "A")]));
    let before = p.seal_count();
    runs.extend(p.edit(Some(ALPHA), "sess-a", "Edit", "a.txt"));
    let after_alpha = seals_since(&p, before);
    for s in &after_alpha {
        assert!(
            !(s["agent"]["id"] == ALPHA && paths_of(s).contains("b.txt")),
            "alpha drained b.txt though its latest write is bravo's: {s}"
        );
    }
    runs.extend(bash_post(&p, BRAVO, "sess-b", cb, "toolu_b"));
    all_exit_zero(&runs, "drain attribution");
    assert!(
        seals_since(&p, before)
            .iter()
            .any(|s| s["agent"]["id"] == BRAVO && paths_of(s).contains("b.txt")),
        "bravo's own write was not sealed by bravo"
    );
}

/// Finding 174: an auto spec with no seals never reaches the receipt and is
/// gone after SessionEnd.
#[test]
fn f174_auto_spec_with_no_seals_is_removed_at_session_end_and_never_in_the_receipt() {
    let p = Project::new("f174");
    let since = p.newest_seal()["id"].as_str().unwrap().to_string();
    all_exit_zero(
        &bash_call(&p, ALPHA, "sess-a", "printf 'A\\n' >> c.txt", "toolu_1"),
        "write",
    );
    let first = p.newest_seal()["spec_id"].as_str().unwrap().to_string();
    p.ok(
        ALPHA,
        &["spec", "done", "-s", "alpha's words", "--agent", ALPHA],
    );
    // After the close: hooks that find nothing to seal (an Edit hook on an
    // unchanged file, a Bash call that writes nothing, a hand --auto seal of
    // a clean path) must not leave a 0-seal spec behind.
    all_exit_zero(
        &p.edit(Some(ALPHA), "sess-a", "Edit", "c.txt"),
        "unchanged edit",
    );
    all_exit_zero(
        &bash_call(&p, ALPHA, "sess-a", "true", "toolu_2"),
        "no-op bash",
    );
    let _ = p.writ(
        ALPHA,
        &["seal", "--paths", "c.txt", "--auto", "--agent", ALPHA],
    );
    all_exit_zero(
        &p.event(
            Some(ALPHA),
            "sess-a",
            "SessionEnd",
            json!({"reason": "other"}),
        ),
        "end",
    );
    for s in p.auto_specs() {
        let id = s["id"].as_str().unwrap();
        let sealed = s["sealed_by"].as_array().map_or(0, |a| a.len());
        assert!(
            id == first || sealed > 0,
            "0-seal auto spec left after SessionEnd: {s}"
        );
    }
    let r = text(&p.ok(
        "human",
        &["handoff", "--since", &since, "--format", "receipt"],
    ));
    for l in r.lines() {
        assert!(
            !(l.contains("0 seals") || l.contains("no seals")),
            "receipt line for a spec with no seals: {l}\n{r}"
        );
    }
}

/// Ruling 175: with the 173 fix the template's first line says writ seals
/// what it can attribute and asks about the rest (one line per surface);
/// the tp_* tests keep the shape (exactly one `writ seal`, on the
/// other-tools clause; at most 5 lines; no mandatory words).
#[test]
fn tp175_first_line_says_writ_seals_the_edits_it_can_attribute() {
    let p = Project::new("tp175");
    p.write("AGENTS.md", "# agents\n");
    p.ok("human", &["init", "-y"]);
    let start = p.event(
        Some(ALPHA),
        "sess-a",
        "SessionStart",
        json!({"source": "startup"}),
    );
    let echo = start[0].stdout();
    for (surface, text) in [
        ("CLAUDE.md", managed_lines(&p.read("CLAUDE.md")).join("\n")),
        ("AGENTS.md", managed_lines(&p.read("AGENTS.md")).join("\n")),
        ("SessionStart", echo),
        (
            "settings instructions",
            p.settings()["instructions"].to_string(),
        ),
    ] {
        assert!(
            text.contains("seals the edits it can attribute")
                && text.contains("asks you with one line"),
            "{surface}: ruling 175 sentence missing:\n{text}"
        );
        assert!(
            !text.contains("sealed automatically"),
            "{surface}: old wording left:\n{text}"
        );
    }
}

// ---------------------------------------------------------------------------
// Ruling 177 (exit run B's strict miss) with Aubs's conditions: an override
// seal whose removed own lines ALL reappear whole, byte for byte, in order,
// in another path sealed IN THE SAME SEAL is a move: "moved N own line(s)
// from X to Y", no survival finding, no --accept-removals. Partial: still a
// finding, "N of M reappeared in Y". Destination unsealed, or sealed in a
// later seal: not a move (the rule is one seal). Another agent's lines moved:
// "moved N of <agent>'s line(s) from X to Y", no finding, and that agent
// gets one 141 line.
// ---------------------------------------------------------------------------

const MA: &str = "agent-alpha";
const MB: &str = "agent-bravo";

fn survival(p: &Project) -> Value {
    p.json("human", &["doctor", "--survival", "--format", "json"])
}

fn survival_notes(p: &Project) -> Vec<String> {
    survival(p)["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["check"] == "survival")
        .map(|f| f["message"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn receipt_text(p: &Project, since: &str) -> String {
    text(&p.ok(
        "human",
        &["handoff", "--since", since, "--format", "receipt"],
    ))
}

fn seal_paths(p: &Project, agent: &str, spec: &str, paths: &str, extra: &[&str]) -> Output {
    let mut args = vec![
        "seal", "-s", "work", "--agent", agent, "--spec", spec, "--paths", paths,
    ];
    args.extend_from_slice(extra);
    p.writ(agent, &args)
}

/// bravo seals shared.txt with "line 30 B"; alpha seals its two lines on top
/// ("line 2 A1", "line 3 A2"). Returns the seal id before the specs.
fn move_fixture(tag: &str) -> (Project, String) {
    let p = Project::new(tag);
    let since = p.newest_seal()["id"].as_str().unwrap().to_string();
    p.spec("sa", MA);
    p.spec("sb", MB);
    p.write("shared.txt", &numbered(40, &[(30, "B")]));
    assert!(seal_paths(&p, MB, "sb", "shared.txt", &[]).status.success());
    p.write(
        "shared.txt",
        &numbered(40, &[(2, "A1"), (3, "A2"), (30, "B")]),
    );
    assert!(seal_paths(&p, MA, "sa", "shared.txt", &[]).status.success());
    (p, since)
}

#[test]
fn r177_override_that_moves_all_own_lines_into_a_path_of_the_same_seal_is_a_move() {
    let (p, since) = move_fixture("r177-move");
    // alpha moves both lines, whole and in order, into moved.txt; one seal.
    p.write("shared.txt", &numbered(40, &[(30, "B")]));
    p.write("moved.txt", "line 2 A1\nline 3 A2\n");
    let s = seal_paths(
        &p,
        MA,
        "sa",
        "shared.txt,moved.txt",
        &["--allow-removals", "shared.txt"],
    );
    assert!(s.status.success(), "{}", text(&s));
    p.ok(MA, &["spec", "done", "sa", "--agent", MA, "--no-seal"]);
    p.ok(MB, &["spec", "done", "sb", "--agent", MB, "--no-seal"]);
    let notes = survival_notes(&p);
    assert!(notes.is_empty(), "a move is not a finding: {notes:?}");
    let doc = text(&p.writ("human", &["doctor", "--survival"]));
    assert!(!doc.contains("--accept-removals"), "{doc}");
    let r = receipt_text(&p, &since);
    assert!(
        r.contains("moved 2 own line(s) from shared.txt to moved.txt"),
        "{r}"
    );
    assert!(!r.contains("captured"), "{r}");
    let fin = p.writ("human", &["finish", "-y"]);
    assert!(fin.status.success(), "finish must proceed: {}", text(&fin));
}

#[test]
fn r177_partial_move_stays_a_finding_and_says_how_many_reappeared() {
    let (p, _since) = move_fixture("r177-partial");
    p.write("shared.txt", &numbered(40, &[(30, "B")]));
    p.write("moved.txt", "line 2 A1\n");
    let s = seal_paths(
        &p,
        MA,
        "sa",
        "shared.txt,moved.txt",
        &["--allow-removals", "shared.txt"],
    );
    assert!(s.status.success(), "{}", text(&s));
    let notes = survival_notes(&p);
    assert!(
        notes
            .iter()
            .any(|n| n.contains("1 of 2 reappeared in moved.txt")),
        "partial move must stay a finding with the count: {notes:?}"
    );
}

#[test]
fn r177_reordered_lines_are_not_a_move() {
    let (p, _since) = move_fixture("r177-order");
    p.write("shared.txt", &numbered(40, &[(30, "B")]));
    p.write("moved.txt", "line 3 A2\nline 2 A1\n");
    let s = seal_paths(
        &p,
        MA,
        "sa",
        "shared.txt,moved.txt",
        &["--allow-removals", "shared.txt"],
    );
    assert!(s.status.success(), "{}", text(&s));
    assert!(
        !survival_notes(&p).is_empty(),
        "lines out of order are not a move (Aubs a)"
    );
}

#[test]
fn r177_plain_capture_with_the_lines_nowhere_still_yellows() {
    let (p, _since) = move_fixture("r177-capture");
    p.write("shared.txt", &numbered(40, &[(30, "B")]));
    let s = seal_paths(
        &p,
        MA,
        "sa",
        "shared.txt",
        &["--allow-removals", "shared.txt"],
    );
    assert!(s.status.success(), "{}", text(&s));
    let notes = survival_notes(&p);
    assert!(
        notes
            .iter()
            .any(|n| n.contains("override captured another spec's version")),
        "{notes:?}"
    );
}

#[test]
fn r177_move_into_a_file_sealed_later_is_not_a_move_the_rule_is_one_seal() {
    let (p, _since) = move_fixture("r177-later");
    p.write("shared.txt", &numbered(40, &[(30, "B")]));
    p.write("moved.txt", "line 2 A1\nline 3 A2\n");
    // The override seals shared.txt only; moved.txt stays unsealed.
    let s = seal_paths(
        &p,
        MA,
        "sa",
        "shared.txt",
        &["--allow-removals", "shared.txt"],
    );
    assert!(s.status.success(), "{}", text(&s));
    assert!(
        survival_notes(&p).iter().any(|n| n.contains("shared.txt")),
        "destination unsealed: the capture finding stands"
    );
    // moved.txt seals in a later seal: still a finding.
    assert!(seal_paths(&p, MA, "sa", "moved.txt", &[]).status.success());
    assert!(
        survival_notes(&p).iter().any(|n| n.contains("shared.txt")),
        "a move across two seals is not a move (Aubs b)"
    );
}

#[test]
fn r177_moving_another_agents_lines_is_recorded_and_that_agent_is_told_once() {
    let p = Project::new("r177-foreign");
    let since = p.newest_seal()["id"].as_str().unwrap().to_string();
    p.spec("sa", MA);
    p.spec("sb", MB);
    p.write("shared.txt", &numbered(40, &[(10, "B1"), (11, "B2")]));
    assert!(seal_paths(&p, MB, "sb", "shared.txt", &[]).status.success());
    // alpha moves bravo's two lines, whole and in order, into moved.txt.
    p.write("shared.txt", &numbered(40, &[]));
    p.write("moved.txt", "line 10 B1\nline 11 B2\n");
    let s = seal_paths(&p, MA, "sa", "shared.txt,moved.txt", &[]);
    assert!(
        s.status.success(),
        "nothing is lost in a move: {}",
        text(&s)
    );
    assert!(survival_notes(&p).is_empty(), "{:?}", survival_notes(&p));
    let r = receipt_text(&p, &since);
    assert!(
        r.contains(&format!(
            "moved 2 of {MB}'s line(s) from shared.txt to moved.txt"
        )),
        "{r}"
    );
    // bravo's next hook tells it once, in one line.
    p.write("b.txt", &numbered(20, &[(1, "B")]));
    let first: Vec<String> = p
        .edit(Some(MB), "sess-b", "Edit", "b.txt")
        .iter()
        .filter_map(HookRun::to_agent)
        .collect();
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(
        first[0].contains("shared.txt") && first[0].contains("moved.txt") && first[0].contains(MA),
        "{}",
        first[0]
    );
    p.write("b.txt", &numbered(20, &[(2, "B")]));
    let second: Vec<String> = p
        .edit(Some(MB), "sess-b", "Edit", "b.txt")
        .iter()
        .filter_map(HookRun::to_agent)
        .collect();
    assert!(second.is_empty(), "told twice: {second:?}");
}

/// Finding 179: the 177 move scan must ignore anything in .writ/specs/ that
/// is not a readable `<id>.json` (another process's atomic-write temp file,
/// a half-written or unreadable file): no PostToolUse error, and a due move
/// notice is still delivered.
#[test]
fn f179_move_scan_ignores_temp_and_unreadable_spec_files_and_still_notifies() {
    let p = Project::new("f179");
    p.spec("sa", MA);
    p.spec("sb", MB);
    p.write("shared.txt", &numbered(40, &[(10, "B1"), (11, "B2")]));
    assert!(seal_paths(&p, MB, "sb", "shared.txt", &[]).status.success());
    // Strays as a concurrent atomic write leaves them.
    p.write(
        ".writ/specs/auto-deadbeef.tmp",
        "{\"id\": \"auto-deadbeef\", \"tit",
    );
    p.write(".writ/specs/broken.json", "{ not json");
    // alpha moves bravo's lines whole into moved.txt (a 177 move).
    p.write("shared.txt", &numbered(40, &[]));
    p.write("moved.txt", "line 10 B1\nline 11 B2\n");
    assert!(seal_paths(&p, MA, "sa", "shared.txt,moved.txt", &[])
        .status
        .success());
    // bravo's next hook runs the move scan.
    p.write("b.txt", &numbered(20, &[(1, "B")]));
    let runs = p.edit(Some(MB), "sess-b", "Edit", "b.txt");
    all_exit_zero(&runs, "move scan with strays");
    let told: Vec<String> = runs.iter().filter_map(HookRun::to_agent).collect();
    assert!(
        told.iter()
            .any(|l| l.contains("moved.txt") && l.contains(MA)),
        "move notice not delivered with stray spec files present: {told:?}"
    );
    let log = p.hooks_log();
    let errors: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("\"outcome\":\"error\"") || l.contains("\"outcome\": \"error\""))
        .collect();
    assert!(
        errors.is_empty(),
        "hook errors from stray spec files: {errors:?}"
    );
}

// ---------------------------------------------------------------------------
// Ruling 181 (findings 145, 180) with Aubs's conditions. (1) A hand seal that
// would record the removal of lines another spec sealed is refused with one
// line naming the other agent, its seal, the path, the count and two exits
// (merge and seal again, or `--allow-removals <path>`); the hand refusal and
// the hook refusal are the same text for the same state. A retry with
// --allow-removals seals and records "accepted <other>'s version for N
// line(s)" after being told; finish proceeds and says what was accepted.
// (2) `spec done` never prints done over red: it runs survival for the
// identity's own specs, prints its time, refuses (non-zero, no --force
// offered) when one of its sealed lines is lost on disk or in another
// spec's seal, and never trusts a stale green cache.
// ---------------------------------------------------------------------------

/// bravo seals "line 30 B" in shared.txt; alpha's on-disk version drops it.
fn foreign_loss(tag: &str) -> (Project, String) {
    let p = Project::new(tag);
    let since = p.newest_seal()["id"].as_str().unwrap().to_string();
    p.spec("sa", MA);
    p.spec("sb", MB);
    p.write("shared.txt", &numbered(40, &[(30, "B")]));
    assert!(seal_paths(&p, MB, "sb", "shared.txt", &[]).status.success());
    p.write("shared.txt", &numbered(40, &[(2, "A")]));
    (p, since)
}

/// The refusal's one line: the output line naming the path and the other agent.
fn refusal_line(out: &str, path: &str, other: &str) -> String {
    out.lines()
        .map(|l| l.trim().trim_start_matches("error:").trim())
        .find(|l| l.contains(path) && l.contains(other))
        .unwrap_or_else(|| panic!("no line naming {path} and {other}:\n{out}"))
        .to_string()
}

#[test]
fn r181_hand_seal_removing_another_specs_lines_is_refused_with_one_line_and_two_exits() {
    let (p, since) = foreign_loss("r181-hand");
    let b_seal = p.newest_seal()["id"].as_str().unwrap()[..12].to_string();
    let before = p.seal_count();
    let out = seal_paths(&p, MA, "sa", "shared.txt", &[]);
    assert!(
        !out.status.success(),
        "hand seal removing bravo's line went through: {}",
        text(&out)
    );
    assert_eq!(p.seal_count(), before, "nothing may be sealed");
    let line = refusal_line(&text(&out), "shared.txt", MB);
    assert!(
        line.contains(&b_seal),
        "names bravo's seal {b_seal}: {line}"
    );
    assert!(line.contains("1 line"), "names the count: {line}");
    assert!(
        line.contains("--allow-removals shared.txt"),
        "second exit: {line}"
    );
    assert!(!line.contains("--force"), "{line}");
    // Exit two: on purpose, after being told.
    let ok = seal_paths(
        &p,
        MA,
        "sa",
        "shared.txt",
        &["--allow-removals", "shared.txt"],
    );
    assert!(ok.status.success(), "{}", text(&ok));
    let s = p.newest_seal();
    let ctx = &s["override_context"];
    assert!(
        ctx.to_string()
            .contains(&format!("\"accepted_from\":[\"{MB}\"]")),
        "override_context does not record bravo's version as accepted: {ctx}"
    );
    p.ok(MA, &["spec", "done", "sa", "--agent", MA, "--no-seal"]);
    // sa alone is finished (bravo's spec stays open: its line was removed).
    let fin = p.writ("human", &["finish", "-y"]);
    assert!(
        fin.status.success(),
        "finish on an accepted removal: {}",
        text(&fin)
    );
    let r = receipt_text(&p, &since);
    assert!(
        r.contains(&format!("accepted {MB}'s version")) && r.contains("after being told"),
        "receipt: {r}"
    );
}

#[test]
fn r181_hand_and_hook_refusals_are_the_same_text_for_the_same_state() {
    let (hand, _) = foreign_loss("r181-same-hand");
    let (hook, _) = foreign_loss("r181-same-hook");
    let h = refusal_line(
        &text(&seal_paths(&hand, MA, "sa", "shared.txt", &[])),
        "shared.txt",
        MB,
    );
    let k: Vec<String> = hook
        .edit(Some(MA), "sess-a", "Write", "shared.txt")
        .iter()
        .filter_map(HookRun::to_agent)
        .collect();
    assert_eq!(k.len(), 1, "{k:?}");
    // Seal ids differ between the two fixtures; compare with them masked.
    let mask = |s: &str| -> String {
        s.split_whitespace()
            .map(|w| {
                let t = w.trim_matches(|c: char| !c.is_ascii_alphanumeric());
                if t.len() == 12 && t.chars().all(|c| c.is_ascii_hexdigit()) {
                    "<seal>".to_string()
                } else {
                    w.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    assert_eq!(
        mask(&h),
        mask(k[0].trim()),
        "hand and hook refusals differ:\nhand: {h}\nhook: {}",
        k[0]
    );
}

#[test]
fn r181_hand_seal_removing_only_its_own_lines_behaves_as_today() {
    // Live pin: alpha removes only its own sealed line, no other agent near.
    let p = Project::new("r181-own");
    p.spec("sa", MA);
    p.write("a.txt", &numbered(20, &[(3, "A")]));
    assert!(seal_paths(&p, MA, "sa", "a.txt", &[]).status.success());
    p.write("a.txt", &numbered(20, &[]));
    // Today (finding 144): an own rewrite with no other agent near seals,
    // with no flag and no foreign override recorded.
    let out = seal_paths(&p, MA, "sa", "a.txt", &[]);
    assert!(out.status.success(), "{}", text(&out));
    let ctx = p.newest_seal()["override_context"].to_string();
    assert!(
        !ctx.contains("accepted_from\":[\""),
        "own removal recorded as accepting another's: {ctx}"
    );
}

#[test]
fn r181_spec_done_refuses_over_a_loss_on_disk_even_with_a_warm_green_cache() {
    let (p, _) = foreign_loss("r181-done-disk");
    // Undo alpha's disk change for the warm green bound, then lose it again.
    p.write("shared.txt", &numbered(40, &[(30, "B")]));
    let warm = survival(&p);
    assert!(
        warm["headline"]
            .as_str()
            .unwrap_or_default()
            .contains("survival green"),
        "{warm:#}"
    );
    p.write("shared.txt", &numbered(40, &[(2, "A")]));
    let out = p.writ(
        MB,
        &[
            "spec",
            "done",
            "sb",
            "-s",
            "done",
            "--agent",
            MB,
            "--no-seal",
        ],
    );
    let t = text(&out);
    assert!(!out.status.success(), "spec done printed over a loss: {t}");
    assert!(t.contains("shared.txt"), "{t}");
    assert!(!t.contains("--force"), "{t}");
    assert!(
        t.contains(" ms") || t.contains("ms)"),
        "survival time not printed: {t}"
    );
    assert_eq!(
        p.spec_json("sb")["status"],
        "in-progress",
        "spec closed anyway"
    );
    // bravo re-seals its line: done succeeds and prints its time.
    p.write("shared.txt", &numbered(40, &[(2, "A"), (30, "B")]));
    assert!(seal_paths(&p, MB, "sb", "shared.txt", &[]).status.success());
    let ok = p.writ(
        MB,
        &[
            "spec",
            "done",
            "sb",
            "-s",
            "done",
            "--agent",
            MB,
            "--no-seal",
        ],
    );
    assert!(ok.status.success(), "{}", text(&ok));
    assert!(text(&ok).contains("ms"), "{}", text(&ok));
}

#[test]
fn r181_spec_done_refuses_when_another_spec_sealed_the_removal_naming_it() {
    let (p, _) = foreign_loss("r181-done-seal");
    // alpha removes bravo's line on purpose after being told (exit two).
    let _ = seal_paths(&p, MA, "sa", "shared.txt", &[]);
    assert!(seal_paths(
        &p,
        MA,
        "sa",
        "shared.txt",
        &["--allow-removals", "shared.txt"]
    )
    .status
    .success());
    let out = p.writ(
        MB,
        &[
            "spec",
            "done",
            "sb",
            "-s",
            "done",
            "--agent",
            MB,
            "--no-seal",
        ],
    );
    let t = text(&out);
    assert!(!out.status.success(), "{t}");
    assert!(
        t.contains("shared.txt") && t.contains(MA),
        "refusal must name the path and the agent whose seal removed the line ({MA}): {t}"
    );
    assert!(!t.contains("--force"), "{t}");
}

// ---------------------------------------------------------------------------
// Ruling 183 (+ Aubs): `[seal] foreign_line_check = false` in .writ/config.toml
// turns the 181 check off for fixtures. It is never written by init; with it
// set, doctor (JSON, text) and `writ context` show a yellow naming it
// (needs_human; fix: remove the key); removing the key clears it; the finish
// survival check still refuses a loss; refusal texts never mention the key;
// a seal made with it off is recorded so, and the receipt carries exactly one
// line "N seal(s) made with the foreign-line check disabled in config".
// The r181/f173 fixtures above never set the key (they test the on state).
// ---------------------------------------------------------------------------

const KEY_OFF: &str = "\n[seal]\nforeign_line_check = false\n";
const OFF_NOTE: &str = "foreign-line check on seal is disabled in config";

fn set_check_off(p: &Project) {
    let c = p.read(".writ/config.toml");
    assert!(
        !c.contains("foreign_line_check"),
        "init wrote the key:\n{c}"
    );
    p.write(".writ/config.toml", &format!("{c}{KEY_OFF}"));
}

fn clear_check_off(p: &Project) {
    let c = p.read(".writ/config.toml").replace(KEY_OFF, "");
    p.write(".writ/config.toml", &c);
}

#[test]
fn r183_disabled_check_is_a_needs_human_yellow_everywhere_and_clears_when_removed() {
    let p = Project::new("r183-yellow");
    set_check_off(&p);
    let d = p.json("human", &["doctor", "--format", "json"]);
    let f = d["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["message"].as_str().is_some_and(|m| m.contains(OFF_NOTE)))
        .cloned()
        .unwrap_or_else(|| panic!("no disabled-check finding:\n{d:#}"));
    assert_eq!(f["severity"], "yellow", "{f:#}");
    assert_eq!(f["needs_human"], true, "{f:#}");
    assert!(
        f["message"]
            .as_str()
            .unwrap()
            .contains("the finish check still applies"),
        "{f:#}"
    );
    assert!(text(&p.writ("human", &["doctor"])).contains(OFF_NOTE));
    assert!(text(&p.ok("human", &["context", "--format", "brief"])).contains(OFF_NOTE));
    clear_check_off(&p);
    let d = p.json("human", &["doctor", "--format", "json"]);
    assert!(!d.to_string().contains(OFF_NOTE), "{d:#}");
    assert!(!text(&p.ok("human", &["context", "--format", "brief"])).contains(OFF_NOTE));
}

#[test]
fn r183_with_the_check_off_finish_still_refuses_a_loss_and_the_receipt_says_so() {
    let (p, since) = foreign_loss("r183-finish");
    // bravo's work is done before alpha's seal: the loss is inside the set
    // a finish commits.
    let disk = p.read("shared.txt");
    p.write("shared.txt", &numbered(40, &[(30, "B")]));
    p.ok(MB, &["spec", "done", "sb", "--agent", MB, "--no-seal"]);
    p.write("shared.txt", &disk);
    set_check_off(&p);
    // alpha's seal drops bravo's line: allowed with the check off.
    let s = seal_paths(&p, MA, "sa", "shared.txt", &[]);
    assert!(s.status.success(), "check off: {}", text(&s));
    let rec = p.newest_seal();
    assert!(
        rec.to_string().contains("foreign_line_check"),
        "seal not recorded as made with the check off: {rec}"
    );
    let r = receipt_text(&p, &since);
    let n = r
        .lines()
        .filter(|l| l.contains("made with the foreign-line check disabled in config"))
        .count();
    assert_eq!(n, 1, "{r}");
    assert!(
        r.contains("1 seal(s) made with the foreign-line check disabled in config"),
        "{r}"
    );
    p.ok(MA, &["spec", "done", "sa", "--agent", MA, "--no-seal"]);
    let head = p.git_ok(&["rev-parse", "HEAD"]);
    let fin = p.writ("human", &["finish", "-y"]);
    let committed = p.git_ok(&["rev-parse", "HEAD"]) != head;
    assert!(
        !committed || p.git_ok(&["show", "HEAD:shared.txt"]).contains("line 30 B"),
        "finish committed bravo's finished spec without its sealed line, check off: {}",
        text(&fin)
    );
}

#[test]
fn r183_receipt_has_no_disabled_line_when_every_seal_had_the_check_on() {
    let p = Project::new("r183-none");
    let since = p.newest_seal()["id"].as_str().unwrap().to_string();
    p.spec("sa", MA);
    p.write("a.txt", &numbered(20, &[(1, "A")]));
    assert!(seal_paths(&p, MA, "sa", "a.txt", &[]).status.success());
    let r = receipt_text(&p, &since);
    assert!(!r.contains("disabled in config"), "{r}");
}

#[test]
fn r183_refusal_texts_never_mention_the_config_key() {
    let (hand, _) = foreign_loss("r183-text-hand");
    let t = text(&seal_paths(&hand, MA, "sa", "shared.txt", &[]));
    let (hook, _) = foreign_loss("r183-text-hook");
    let k: Vec<String> = hook
        .edit(Some(MA), "sess-a", "Write", "shared.txt")
        .iter()
        .filter_map(HookRun::to_agent)
        .collect();
    for (who, s) in [("hand", t), ("hook", k.join("\n"))] {
        assert!(
            s.contains("shared.txt") && s.contains(MB),
            "{who}: not a refusal: {s}"
        );
        for w in [
            "foreign_line_check",
            "config.toml",
            "[seal]",
            "disabled in config",
        ] {
            assert!(!s.contains(w), "{who} refusal mentions {w:?}: {s}");
        }
    }
}

// ---------------------------------------------------------------------------
// Sprint 7 (0.4.4), ruling 189 on findings 186, 187, 188. Assertions before
// the code (retro rule 1); ignored until Amis lands each; the two
// f188 guards pass on 0.4.3 and run now, so rule 1 cannot break them unseen.
// 186: a failed Bash call gets PostToolUseFailure, never PostToolUse (Claude
//   Code 2.1.290, fixture post_tool_use_failure_bash.json: `error`,
//   `is_interrupt`, `duration_ms`, no `tool_response`). writ init registers
//   PostToolUseFailure for the same matchers and dispatches it like
//   PostToolUse (the command may have written before it failed). An
//   identity's stale open windows close at its next PreToolUse (calls of one
//   identity run one after another), for hosts that send no failure event.
// 187: Stop sweep records print nothing; every ambiguous hooks.log record
//   carries `told` (bool, as refused records do) so the collector counts
//   records not attributed apart from ask lines shown.
// 188 rule 1 (reading W): when exactly one candidate's command text names the
//   path (relative path, basename, or inside a heredoc body), that candidate
//   owns it; the seal and its hooks.log record carry
//   `attributed_by: "command_text"`. Two candidates naming it: ambiguous, the
//   0.4.3 line unchanged, left_out addressed to both (169). Rule 2 (a
//   candidate whose command names no path is a reader) does not exist.
// ---------------------------------------------------------------------------

const PTUF_FIXTURE: &str = include_str!("fixtures/claude_hooks/post_tool_use_failure_bash.json");

/// PostToolUseFailure stdin in the captured 2.1.290 shape, for `cmd`.
fn bash_failure_stdin(p: &Project, session: &str, cmd: &str, id: &str) -> Value {
    let mut v: Value = serde_json::from_str(PTUF_FIXTURE).unwrap();
    assert!(
        v.get("tool_response").is_none(),
        "fixture shape changed: {v}"
    );
    v["session_id"] = json!(session);
    v["transcript_path"] = json!(format!("/tmp/claude-fixture/{session}.jsonl"));
    v["cwd"] = json!(p.root.display().to_string());
    v["permission_mode"] = json!("default");
    v["tool_input"] = json!({"command": cmd, "description": "run"});
    v["tool_use_id"] = json!(id);
    v["error"] = json!("Exit code 1");
    v
}

fn bash_failure(p: &Project, agent: &str, session: &str, cmd: &str, id: &str) -> Vec<HookRun> {
    let stdin = bash_failure_stdin(p, session, cmd, id);
    p.fire("PostToolUseFailure", Some("Bash"), Some(agent), &stdin)
}

/// Run a fixture command that must fail (exit non-zero), as the agent's did.
fn run_sh_failing(p: &Project, cmd: &str) {
    let out = p
        .cmd("sh", Some("human"))
        .arg("-c")
        .arg(cmd)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "fixture command `{cmd}` was meant to fail"
    );
}

/// hooks.log records (one JSON object per line).
fn hook_records(p: &Project) -> Vec<Value> {
    p.hooks_log()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect()
}

/// A write by alpha's command while bravo's call is open; returns every run.
/// `ca` writes (`run_sh`), `cb` is only announced (Pre/Post), never run.
fn overlapped(p: &Project, ca: &str, cb: &str, tag: &str) -> (Vec<HookRun>, Vec<HookRun>) {
    let (ida, idb) = (format!("toolu_a_{tag}"), format!("toolu_b_{tag}"));
    let mut a = bash_pre(p, ALPHA, "sess-a", ca, &ida);
    tick();
    let mut b = bash_pre(p, BRAVO, "sess-b", cb, &idb);
    tick();
    run_sh(p, ca);
    tick();
    a.extend(bash_post(p, ALPHA, "sess-a", ca, &ida));
    tick();
    b.extend(bash_post(p, BRAVO, "sess-b", cb, &idb));
    all_exit_zero(&a, "overlapped alpha");
    all_exit_zero(&b, "overlapped bravo");
    (a, b)
}

#[test]
fn f186_failed_bash_call_closes_its_window() {
    let p = Project::new("f186-close");
    for tool in ["Bash", "Edit", "Write", "NotebookEdit"] {
        assert!(
            !p.hook_commands("PostToolUseFailure", Some(tool)).is_empty(),
            "init registers no PostToolUseFailure hook covering {tool}:\n{}",
            p.settings()
        );
    }
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    let ca = "cat missing_file";
    let before = p.seal_count();
    let mut runs = bash_pre(&p, ALPHA, "sess-a", ca, "toolu_a");
    run_sh_failing(&p, ca);
    tick();
    runs.extend(bash_failure(&p, ALPHA, "sess-a", ca, "toolu_a"));
    tick();
    let cb = "printf 'bravo\\n' >> shared.txt";
    let call_b = bash_call(&p, BRAVO, "sess-b", cb, "toolu_b");
    all_exit_zero(&runs, "alpha");
    nothing_to_agent(&runs, "alpha");
    all_exit_zero(&call_b, "bravo");
    nothing_to_agent(&call_b, "bravo: no ambiguity line once alpha's call failed");
    let new = seals_since(&p, before);
    assert_eq!(new.len(), 1, "{new:?}");
    assert_eq!(new[0]["agent"]["id"], BRAVO, "{}", new[0]);
    assert_eq!(new[0]["spec_id"], "sb");
    assert_eq!(paths_of(&new[0]), set(&["shared.txt"]));
    assert!(
        !hook_records(&p)
            .iter()
            .any(|r| r["outcome"] == "ambiguous" && r["path"] == "shared.txt"),
        "shared.txt recorded ambiguous:\n{}",
        p.hooks_log()
    );
}

#[test]
fn f186_posttoolusefailure_still_attributes_writes_made_before_the_failure() {
    let p = Project::new("f186-wrote");
    p.spec("sa", ALPHA);
    let ca = "printf 'alpha\\n' >> c.txt && false";
    let before = p.seal_count();
    let mut runs = bash_pre(&p, ALPHA, "sess-a", ca, "toolu_a");
    run_sh_failing(&p, ca);
    tick();
    runs.extend(bash_failure(&p, ALPHA, "sess-a", ca, "toolu_a"));
    all_exit_zero(&runs, "alpha");
    nothing_to_agent(&runs, "alpha");
    let new = seals_since(&p, before);
    assert_eq!(new.len(), 1, "failure hook sealed nothing: {new:?}");
    assert_eq!(new[0]["agent"]["id"], ALPHA);
    assert_eq!(new[0]["spec_id"], "sa");
    assert_eq!(paths_of(&new[0]), set(&["c.txt"]));
    // The window is closed: the Stop that follows has nothing left to sweep.
    let stop = p.event(
        Some(ALPHA),
        "sess-a",
        "Stop",
        json!({"stop_hook_active": false}),
    );
    all_exit_zero(&stop, "stop");
    assert_eq!(p.seal_count(), before + 1, "Stop sealed again");
}

#[test]
fn f186_stale_window_closed_at_next_pretooluse() {
    let p = Project::new("f186-stale");
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    // An old host: the failed call gets no event at all.
    let a1 = "cat NO_SUCH_FILE.md 2>/dev/null";
    all_exit_zero(&bash_pre(&p, ALPHA, "sess-a", a1, "toolu_a1"), "a1 pre");
    run_sh_failing(&p, a1);
    tick();
    // alpha's next call: its Pre closes the a1 window at this call's start.
    let a2 = "ls >/dev/null";
    let call_a = bash_call(&p, ALPHA, "sess-a", a2, "toolu_a2");
    all_exit_zero(&call_a, "a2");
    nothing_to_agent(&call_a, "a2");
    tick();
    let before = p.seal_count();
    let cb = "printf 'bravo\\n' >> shared.txt";
    let call_b = bash_call(&p, BRAVO, "sess-b", cb, "toolu_b");
    all_exit_zero(&call_b, "bravo");
    nothing_to_agent(
        &call_b,
        "bravo: alpha's stale window must not make it ambiguous",
    );
    let new = seals_since(&p, before);
    assert_eq!(new.len(), 1, "{new:?}");
    assert_eq!(new[0]["agent"]["id"], BRAVO, "{}", new[0]);
    assert_eq!(paths_of(&new[0]), set(&["shared.txt"]));
}

#[test]
fn f188_command_text_attributes_when_exactly_one_candidate_names_the_path() {
    // (form, alpha's command, the path it writes)
    let forms = [
        (
            "relative",
            "printf 'alpha\\n' >> pkg/rel.txt",
            "pkg/rel.txt",
        ),
        (
            "basename",
            "cd pkg && printf 'alpha\\n' >> mod.py",
            "pkg/mod.py",
        ),
        (
            "heredoc",
            "sh <<'EOF'\nprintf 'alpha\\n' >> notes/h.txt\nEOF",
            "notes/h.txt",
        ),
    ];
    for (form, ca, path) in forms {
        let p = Project::new(&format!("f188-one-{form}"));
        p.write("pkg/keep.txt", "k\n");
        p.write("notes/keep.txt", "k\n");
        p.spec("sa", ALPHA);
        p.spec("sb", BRAVO);
        let before = p.seal_count();
        let (a, b) = overlapped(&p, ca, "ls >/dev/null", form);
        nothing_to_agent(&a, &format!("{form}: alpha"));
        nothing_to_agent(&b, &format!("{form}: bravo"));
        let new = seals_since(&p, before);
        assert_eq!(new.len(), 1, "{form}: {new:?}");
        assert_eq!(new[0]["agent"]["id"], ALPHA, "{form}: {}", new[0]);
        assert_eq!(new[0]["spec_id"], "sa", "{form}");
        assert_eq!(paths_of(&new[0]), set(&[path]), "{form}");
        assert_eq!(
            new[0]["attributed_by"], "command_text",
            "{form}: the seal does not record how writ decided: {}",
            new[0]
        );
        let recs = hook_records(&p);
        assert!(
            recs.iter().any(|r| r["path"] == path
                && r["agent"] == ALPHA
                && r["attributed_by"] == "command_text"),
            "{form}: hooks.log has no attributed_by=command_text record for {path}:\n{}",
            p.hooks_log()
        );
        assert!(
            !recs
                .iter()
                .any(|r| r["outcome"] == "ambiguous" && r["path"] == path),
            "{form}: {path} also recorded ambiguous:\n{}",
            p.hooks_log()
        );
    }
}

#[test]
fn f188_two_candidates_naming_the_path_stay_ambiguous() {
    let p = Project::new("f188-two");
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    let before = p.seal_count();
    let (a, b) = overlapped(
        &p,
        "printf 'alpha\\n' >> shared.txt",
        "grep -c line shared.txt >/dev/null",
        "two",
    );
    assert_eq!(p.seal_count(), before, "an ambiguous write was sealed");
    assert_ambiguous_line(&a, "shared.txt", 2, "alpha");
    assert_ambiguous_line(&b, "shared.txt", 2, "bravo");
    assert!(
        !p.hooks_log().contains("command_text"),
        "attributed_by recorded on an ambiguous path:\n{}",
        p.hooks_log()
    );
    p.ok(
        ALPHA,
        &["spec", "done", "sa", "--agent", ALPHA, "--no-seal"],
    );
    p.ok(
        BRAVO,
        &["spec", "done", "sb", "--agent", BRAVO, "--no-seal"],
    );
    let d = p.json("charlie", &["doctor", "--format", "json"]);
    let f = d["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["check"] == "left_out" && f.to_string().contains("shared.txt"))
        .cloned()
        .unwrap_or_else(|| panic!("no left_out finding for shared.txt:\n{d:#}"));
    assert_eq!(f["anyone_may_fix"], false, "{f:#}");
    let msg = f["message"].as_str().unwrap();
    assert!(msg.contains(ALPHA) && msg.contains(BRAVO), "{msg}");
}

#[test]
fn f188_no_reader_rule() {
    let p = Project::new("f188-noreader");
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    // alpha's command writes shared.txt through a script: its text names no path.
    fs::write(p.home.join("gen.sh"), "printf 'alpha\\n' >> shared.txt\n").unwrap();
    let before = p.seal_count();
    let (a, b) = overlapped(&p, "sh \"$HOME/gen.sh\"", "pytest -q", "noreader");
    assert_eq!(
        p.seal_count(),
        before,
        "sealed although neither command names the path (a reader rule?)"
    );
    assert_ambiguous_line(&a, "shared.txt", 2, "alpha");
    assert_ambiguous_line(&b, "shared.txt", 2, "bravo");
    assert!(!p.hooks_log().contains("command_text"), "{}", p.hooks_log());
}

#[test]
fn f187_sweep_records_are_not_asks() {
    let p = Project::new("f187-sweep");
    p.spec("sa", ALPHA);
    p.spec("sb", BRAVO);
    fs::write(p.home.join("gen.sh"), "printf 'bravo\\n' >> shared.txt\n").unwrap();
    // alpha's call is open (its Post never comes) while bravo's script writes.
    let mut all = bash_pre(&p, ALPHA, "sess-a", "ls >/dev/null", "toolu_a");
    tick();
    let cb = "sh \"$HOME/gen.sh\"";
    all.extend(bash_pre(&p, BRAVO, "sess-b", cb, "toolu_b"));
    tick();
    run_sh(&p, cb);
    tick();
    let post_b = bash_post(&p, BRAVO, "sess-b", cb, "toolu_b");
    tick();
    let stop_a = p.event(
        Some(ALPHA),
        "sess-a",
        "Stop",
        json!({"stop_hook_active": false}),
    );
    all_exit_zero(&all, "pre");
    all_exit_zero(&stop_a, "stop");
    nothing_to_agent(&stop_a, "alpha's Stop sweep");
    assert_ambiguous_line(&post_b, "shared.txt", 2, "bravo");
    let shown: usize = post_b
        .iter()
        .chain(stop_a.iter())
        .filter_map(HookRun::to_agent)
        .count();
    let amb: Vec<Value> = hook_records(&p)
        .into_iter()
        .filter(|r| r["outcome"] == "ambiguous" && r["path"] == "shared.txt")
        .collect();
    assert_eq!(
        amb.len(),
        2,
        "one record from bravo's Post, one from alpha's Stop:\n{amb:#?}"
    );
    for r in &amb {
        assert!(r["told"].is_boolean(), "ambiguous record without told: {r}");
    }
    let sweep: Vec<&Value> = amb.iter().filter(|r| r["agent"] == ALPHA).collect();
    assert_eq!(sweep.len(), 1, "{amb:#?}");
    assert_eq!(
        sweep[0]["event"], "Stop",
        "sweep record logged as {}",
        sweep[0]
    );
    assert_eq!(sweep[0]["told"], false, "{}", sweep[0]);
    let told = amb.iter().filter(|r| r["told"] == true).count();
    assert_eq!(
        told, shown,
        "told records != lines the agents received:\n{amb:#?}"
    );
}

// ---------------------------------------------------------------------------
// Ruling 192 (finding 191, sprint 7 item 10): a named custom subagent. When
// hook stdin's `agent_type` names a custom agent (a file under
// .claude/agents/, not a built-in type such as general-purpose or Explore),
// the hook identity is that name; `-<agent8>` is appended only while a
// second live instance of the same type exists (SubagentStart to
// SubagentStop); that identity's claimed spec is honoured as a main agent's
// is. Built-in types keep the 0.4.3 `<parent>-<agent8>` identity (131).
// Stdin is the 0.4.3 subagent capture shape (agent_id, agent_type).
// ---------------------------------------------------------------------------

fn custom_agent(p: &Project, name: &str) {
    p.write(
        &format!(".claude/agents/{name}.md"),
        &format!("---\nname: {name}\ndescription: test agent\n---\nYou are {name}.\n"),
    );
}

/// SubagentStart, if writ registers it (Amis's choice: liveness may instead
/// run from a subagent's first hook to its SubagentStop).
fn sub_start(p: &Project, session: &str, id: &str, ty: &str) {
    if p.hook_commands("SubagentStart", None).is_empty() {
        return;
    }
    let runs = p.event(
        None,
        session,
        "SubagentStart",
        json!({SUBAGENT_ID_FIELD: id, SUBAGENT_TYPE_FIELD: ty}),
    );
    all_exit_zero(&runs, "SubagentStart");
}

fn sub_stop(p: &Project, session: &str, id: &str, ty: &str) {
    let runs = p.event(
        None,
        session,
        "SubagentStop",
        json!({SUBAGENT_ID_FIELD: id, SUBAGENT_TYPE_FIELD: ty, "stop_hook_active": false}),
    );
    all_exit_zero(&runs, "SubagentStop");
}

/// The parent's own identity, as its first edit records it.
fn parent_identity(p: &Project, session: &str) -> String {
    p.write("README.md", "# fixture\nparent\n");
    all_exit_zero(&p.edit(None, session, "Edit", "README.md"), "parent");
    p.newest_seal()["agent"]["id"].as_str().unwrap().to_string()
}

const SUB_X: &str = "a1b2c3d4e5f6a7b8c";
const SUB_Y: &str = "a9e8d7c6b5a4f3e2d";

#[test]
fn f191_named_custom_subagent_seals_under_its_type_name() {
    let p = Project::new("f191-name");
    custom_agent(&p, "bri");
    sub_start(&p, "sess-p", SUB_X, "bri");
    p.write("b.txt", &numbered(20, &[(1, "bri")]));
    all_exit_zero(&sub_edit(&p, "sess-p", (SUB_X, "bri"), "b.txt"), "bri");
    let s = p.newest_seal();
    assert_eq!(s["agent"]["id"], "bri", "{s}");
    assert_eq!(paths_of(&s), set(&["b.txt"]));
    // Its Bash writes too (the case in finding 191).
    let cmd = "printf 'bri\\n' >> c.txt";
    let mut pre = bash_stdin(&p, "sess-p", "PreToolUse", cmd, "toolu_x2");
    pre[SUBAGENT_ID_FIELD] = json!(SUB_X);
    pre[SUBAGENT_TYPE_FIELD] = json!("bri");
    let mut runs = p.fire("PreToolUse", Some("Bash"), None, &pre);
    run_sh(&p, cmd);
    let mut post = bash_stdin(&p, "sess-p", "PostToolUse", cmd, "toolu_x2");
    post[SUBAGENT_ID_FIELD] = json!(SUB_X);
    post[SUBAGENT_TYPE_FIELD] = json!("bri");
    runs.extend(p.fire("PostToolUse", Some("Bash"), None, &post));
    all_exit_zero(&runs, "bri bash");
    let s = p.newest_seal();
    assert_eq!(s["agent"]["id"], "bri", "{s}");
    assert_eq!(paths_of(&s), set(&["c.txt"]));
}

#[test]
fn f191_second_live_instance_of_a_type_gets_the_suffix() {
    let p = Project::new("f191-two");
    custom_agent(&p, "bri");
    sub_start(&p, "sess-p", SUB_X, "bri");
    sub_start(&p, "sess-p", SUB_Y, "bri");
    p.write("b.txt", &numbered(20, &[(1, "x")]));
    all_exit_zero(&sub_edit(&p, "sess-p", (SUB_X, "bri"), "b.txt"), "x");
    let sx = p.newest_seal();
    p.write("shared.txt", &numbered(40, &[(1, "y")]));
    all_exit_zero(&sub_edit(&p, "sess-p", (SUB_Y, "bri"), "shared.txt"), "y");
    let sy = p.newest_seal();
    assert_eq!(
        sx["agent"]["id"], "bri",
        "the first live instance keeps the name: {sx}"
    );
    assert_eq!(
        sy["agent"]["id"], "bri-a9e8d7c6",
        "the second live instance: {sy}"
    );
    // Both stopped: a later lone instance is plain `bri` again.
    sub_stop(&p, "sess-p", SUB_X, "bri");
    sub_stop(&p, "sess-p", SUB_Y, "bri");
    let lone = "a0a1a2a3a4a5a6a7a";
    sub_start(&p, "sess-p", lone, "bri");
    p.write("a.txt", &numbered(20, &[(1, "lone")]));
    all_exit_zero(&sub_edit(&p, "sess-p", (lone, "bri"), "a.txt"), "lone");
    assert_eq!(p.newest_seal()["agent"]["id"], "bri");
}

#[test]
fn f191_named_subagent_seals_under_its_claimed_spec() {
    let p = Project::new("f191-spec");
    custom_agent(&p, "bri");
    p.spec("harness", "bri");
    let specs_before = p.auto_specs().len();
    sub_start(&p, "sess-p", SUB_X, "bri");
    p.write("b.txt", &numbered(20, &[(1, "bri")]));
    all_exit_zero(&sub_edit(&p, "sess-p", (SUB_X, "bri"), "b.txt"), "bri");
    let s = p.newest_seal();
    assert_eq!(s["agent"]["id"], "bri", "{s}");
    assert_eq!(s["spec_id"], "harness", "claimed spec not honoured: {s}");
    assert_eq!(
        p.auto_specs().len(),
        specs_before,
        "an auto spec was opened"
    );
}

#[test]
fn f191_builtin_type_keeps_the_parent_derived_identity() {
    let p = Project::new("f191-builtin");
    // A custom file exists for another name; built-in and unknown types ignore it.
    custom_agent(&p, "bri");
    let parent = parent_identity(&p, "sess-p");
    for (i, (id, ty)) in [
        (SUB_X, "general-purpose"),
        (SUB_Y, "Explore"),
        ("a0a1a2a3a4a5a6a7a", "not-a-file-agent"),
    ]
    .into_iter()
    .enumerate()
    {
        sub_start(&p, "sess-p", id, ty);
        let rel = format!("f{i}.txt");
        p.write(&rel, &format!("{ty}\n"));
        all_exit_zero(&sub_edit(&p, "sess-p", (id, ty), &rel), ty);
        let s = p.newest_seal();
        assert_eq!(
            s["agent"]["id"],
            format!("{parent}-{}", &id[..8]),
            "{ty}: {s}"
        );
    }
}
