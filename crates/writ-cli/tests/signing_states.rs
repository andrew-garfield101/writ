//! Signing by default (sprint 8, 0.4.5; finding 199, Amis item 1; finding
//! 209). Assertions written before the code (retro rule 1); each is ignored
//! on EXPECTED_IGNORED in bench/test-gate.sh until it lands.
//!
//! Contract (field names are the shapes Amis designs against; rename the
//! constants below, not the assertions):
//! - The hook registers an Ed25519 key for an identity on its first seal,
//!   where `writ agent register` stores keys (`.writ/keys/<id>.*`), never in
//!   a file git tracks or would add.
//! - Every hook seal, and every hand seal by a registered identity, carries
//!   a signature that `writ verify` checks against that identity's key
//!   (`verify --seal <id> --format json`: `signature_valid: true`).
//! - `writ verify` reports unsigned seals (bridge imports, seals older than
//!   0.4.5) by count, not as failures: `valid` stays true, exit 0.
//! - The receipt's survival line says "N seals, N signed" for the range.
//! - Finding 209 (0.4.4): `writ verify` never checks a signature (the CLI
//!   passes no key; `verify --seal` prints `signature_valid: null`), so a
//!   seal whose signature was altered verifies. After 199 it must fail.
//!
//! Hooks are driven exactly as Claude Code calls them (see
//! auto_seal_states.rs): `writ init -y`, read `.claude/settings.json`, run
//! each writ PostToolUse command with the documented stdin.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::SystemTime;

use serde_json::{json, Value};

const ALPHA: &str = "alpha";
/// `writ verify --format json` (chain) counts, added by 199.
const SIGNED_FIELD: &str = "seals_signed";
const UNSIGNED_FIELD: &str = "seals_unsigned";

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
            std::env::temp_dir().join(format!("writ-sign-{tag}-{}-{nanos}", std::process::id()));
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
        p.write("a.txt", &numbered(20, &[]));
        p.write("b.txt", &numbered(20, &[]));
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
            .env("CLAUDE_PROJECT_DIR", &self.root)
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

    fn git(&self, args: &[&str]) -> Output {
        self.cmd("git", "human").args(args).output().unwrap()
    }

    fn git_ok(&self, args: &[&str]) -> String {
        let out = self.git(args);
        assert!(out.status.success(), "git {args:?}: {}", text(&out));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Writ PostToolUse commands whose matcher covers `Edit`.
    fn edit_hooks(&self) -> Vec<String> {
        let s: Value = serde_json::from_str(
            &fs::read_to_string(self.path(".claude/settings.json")).expect("settings.json"),
        )
        .unwrap();
        let mut out = vec![];
        for e in s["hooks"]["PostToolUse"].as_array().into_iter().flatten() {
            let m = e.get("matcher").and_then(Value::as_str).unwrap_or("");
            let covers = m.is_empty()
                || m == "*"
                || m.trim_start_matches('^')
                    .trim_end_matches('$')
                    .trim_matches(|c| c == '(' || c == ')')
                    .split('|')
                    .any(|t| t.trim() == "Edit");
            if !covers {
                continue;
            }
            for h in e["hooks"].as_array().into_iter().flatten() {
                if let Some(c) = h["command"].as_str().filter(|c| c.contains("writ")) {
                    out.push(c.to_string());
                }
            }
        }
        assert!(!out.is_empty(), "no writ PostToolUse hook on Edit");
        out
    }

    /// One Edit as Claude Code reports it, through every writ hook, as `agent`.
    fn hook_edit(&self, agent: &str, session: &str, rel: &str) {
        let abs = self.path(rel).display().to_string();
        let stdin = json!({
            "session_id": session,
            "transcript_path": format!("/tmp/claude-fixture/{session}.jsonl"),
            "cwd": self.root.display().to_string(),
            "permission_mode": "default",
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": {"file_path": abs, "old_string": "a", "new_string": "b"},
            "tool_response": {"filePath": abs, "success": true},
        });
        for c in self.edit_hooks() {
            let mut child = self
                .cmd("sh", agent)
                .env("CLAUDE_CODE_SESSION_ID", session)
                .arg("-c")
                .arg(&c)
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
            assert_eq!(out.status.code(), Some(0), "hook: {}", text(&out));
        }
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

    fn verify_seal(&self, id: &str) -> Value {
        self.json("human", &["verify", "--seal", id, "--format", "json"])
    }

    fn seal_file(&self, id: &str) -> PathBuf {
        self.path(&format!(".writ/seals/{id}.json"))
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        if std::env::var_os("WRIT_KEEP_FIXTURES").is_none() {
            let _ = fs::remove_dir_all(self.root.parent().unwrap());
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

fn signed(seal: &Value) -> bool {
    seal["signature"].as_str().is_some_and(|s| !s.is_empty())
}

/// The seal is signed and `writ verify` checked the signature against the
/// identity's key (not merely found one).
fn assert_signed_and_checked(p: &Project, seal: &Value, what: &str) {
    assert!(signed(seal), "{what}: seal carries no signature: {seal}");
    let id = seal["id"].as_str().unwrap();
    let v = p.verify_seal(id);
    assert_eq!(v["valid"], true, "{what}: {v}");
    assert_eq!(
        v["signature_valid"], true,
        "{what}: verify did not check the signature (finding 209): {v}"
    );
}

/// One hook-sealed edit by `agent`; returns the seal.
fn hook_sealed(p: &Project, agent: &str, rel: &str, tag: &str) -> Value {
    p.write(rel, &numbered(20, &[(1, tag)]));
    p.hook_edit(agent, &format!("sess-{agent}"), rel);
    let s = p.newest_seal();
    assert_eq!(
        s["agent"]["id"], agent,
        "the hook did not seal as {agent}: {s}"
    );
    s
}

#[test]
fn f199_hook_seal_by_a_new_identity_is_signed() {
    let p = Project::new("hook-new");
    let before = p.writ("human", &["agent", "show", ALPHA, "--format", "json"]);
    assert!(
        !before.status.success(),
        "fixture: {ALPHA} must start unregistered"
    );
    let s = hook_sealed(&p, ALPHA, "a.txt", "A");
    // The first hook seal registered a key for the identity.
    let agent = p.json("human", &["agent", "show", ALPHA, "--format", "json"]);
    assert!(
        agent["public_key"].as_str().is_some_and(|k| k.len() == 64),
        "no key registered for {ALPHA} by its first hook seal: {agent}"
    );
    assert_signed_and_checked(&p, &s, "first hook seal");
    // A second hook seal reuses the same key (continuity of one identity).
    let s2 = hook_sealed(&p, ALPHA, "b.txt", "A2");
    assert_signed_and_checked(&p, &s2, "second hook seal");
    let again = p.json("human", &["agent", "show", ALPHA, "--format", "json"]);
    assert_eq!(
        agent["public_key"], again["public_key"],
        "the key changed between seals"
    );
}

#[test]
fn f199_hand_seal_by_a_registered_identity_is_signed() {
    let p = Project::new("hand");
    p.ok("human", &["agent", "register", ALPHA]);
    p.ok(ALPHA, &["spec", "add", "--id", "sa", "--title", "sa"]);
    p.ok(ALPHA, &["spec", "claim", "sa", "--agent", ALPHA]);
    p.write("a.txt", &numbered(20, &[(1, "hand")]));
    p.ok(
        ALPHA,
        &["seal", "-s", "hand", "--agent", ALPHA, "--paths", "a.txt"],
    );
    let s = p.newest_seal();
    assert_eq!(s["agent"]["id"], ALPHA);
    assert_signed_and_checked(&p, &s, "hand seal");
}

#[test]
fn f199_key_is_never_in_a_tracked_file() {
    let p = Project::new("key-untracked");
    hook_sealed(&p, ALPHA, "a.txt", "A");
    let keys = p.path(".writ/keys");
    let own: Vec<PathBuf> = fs::read_dir(&keys)
        .unwrap_or_else(|e| panic!("no .writ/keys after a hook seal ({e})"))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|f| {
            f.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&format!("{ALPHA}.")))
        })
        .collect();
    assert!(
        !own.is_empty(),
        "the hook's key for {ALPHA} is not where `writ agent register` stores keys (.writ/keys)"
    );
    let rel = |f: &PathBuf| f.strip_prefix(&p.root).unwrap().display().to_string();
    for f in fs::read_dir(&keys).unwrap().filter_map(|e| e.ok()) {
        let r = rel(&f.path());
        let ignored = p.git(&["check-ignore", "-q", &r]);
        assert!(
            ignored.status.success(),
            "{r} is not ignored by git: a key could be added"
        );
    }
    let status = p.git_ok(&["status", "--porcelain", "--untracked-files=all"]);
    assert!(
        !status.lines().any(|l| l.contains(".writ/")),
        "git sees a .writ path:\n{status}"
    );
    let tracked = p.git_ok(&["ls-files"]);
    assert!(
        !tracked.lines().any(|l| l.starts_with(".writ/keys")),
        "a key file is tracked:\n{tracked}"
    );
    // No tracked or addable file carries the private key material.
    let addable: Vec<String> = tracked
        .lines()
        .map(str::to_string)
        .chain(
            status
                .lines()
                .filter(|l| l.starts_with("??") || l.starts_with(" M"))
                .map(|l| l[3..].to_string()),
        )
        .collect();
    for k in own
        .iter()
        .filter(|f| f.extension().is_some_and(|e| e != "pub"))
    {
        let secret = fs::read(k).unwrap();
        assert!(
            secret.len() >= 32,
            "key file {k:?} is too short to be a key"
        );
        let needle = &secret[..32];
        for f in &addable {
            let body = fs::read(p.path(f)).unwrap_or_default();
            assert!(
                !body.windows(needle.len()).any(|w| w == needle),
                "{f} contains bytes of the private key file {k:?}"
            );
        }
    }
}

#[test]
fn f199_verify_counts_unsigned_seals() {
    let p = Project::new("unsigned");
    // The bridge import and post-init seals are unsigned (older seals' shape).
    let a = hook_sealed(&p, ALPHA, "a.txt", "A");
    let b = hook_sealed(&p, ALPHA, "b.txt", "B");
    assert!(signed(&a) && signed(&b), "both hook seals are signed");
    let seals = p.seals();
    let n_signed = seals.iter().filter(|s| signed(s)).count();
    let n_unsigned = seals.len() - n_signed;
    assert!(n_unsigned >= 1, "fixture: no unsigned seal to count");
    let out = p.writ("human", &["verify", "--format", "json"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "unsigned seals are not a failure: {}",
        text(&out)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["valid"], true, "{v}");
    assert_eq!(
        v["failures"].as_array().map(Vec::len),
        Some(0),
        "unsigned seals reported as failures: {v}"
    );
    assert_eq!(v[SIGNED_FIELD], n_signed, "{SIGNED_FIELD}: {v}");
    assert_eq!(v[UNSIGNED_FIELD], n_unsigned, "{UNSIGNED_FIELD}: {v}");
    // The human output says the count, once, and not as an error.
    let human = text(&p.ok("human", &["verify"]));
    assert!(
        human
            .lines()
            .any(|l| l.contains(&n_unsigned.to_string()) && l.contains("unsigned")),
        "no unsigned count line:\n{human}"
    );
    assert!(
        !human.to_lowercase().contains("fail"),
        "unsigned seals read as a failure:\n{human}"
    );
}

#[test]
fn f199_receipt_survival_line_says_n_seals_n_signed() {
    let p = Project::new("receipt");
    let since = p.newest_seal()["id"].as_str().unwrap().to_string();
    hook_sealed(&p, ALPHA, "a.txt", "A");
    hook_sealed(&p, ALPHA, "b.txt", "B");
    let r = text(&p.ok(
        "human",
        &["handoff", "--since", &since, "--format", "receipt"],
    ));
    let survival = r
        .lines()
        .find(|l| l.to_lowercase().contains("survival"))
        .unwrap_or_else(|| panic!("no survival line:\n{r}"));
    assert!(
        survival.contains("2 seals, 2 signed"),
        "survival line does not say 2 seals, 2 signed: {survival}\n{r}"
    );
}

#[test]
fn f199_tampered_signed_seal_fails_verify() {
    let p = Project::new("tamper");
    let s = hook_sealed(&p, ALPHA, "a.txt", "A");
    assert!(signed(&s), "fixture: the hook seal must be signed: {s}");
    let id = s["id"].as_str().unwrap().to_string();
    // Flip one hex digit of the signature only: content and chain hashes
    // still match, so only a signature check can catch it (finding 209).
    let file = p.seal_file(&id);
    let mut raw: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    let sig = raw["signature"].as_str().unwrap().to_string();
    let flipped = format!(
        "{}{}",
        if sig.starts_with('0') { '1' } else { '0' },
        &sig[1..]
    );
    raw["signature"] = json!(flipped);
    fs::write(&file, serde_json::to_string(&raw).unwrap()).unwrap();
    let one = p.verify_seal(&id);
    assert_eq!(
        one["valid"], false,
        "verify --seal passed a bad signature: {one}"
    );
    assert_eq!(one["signature_valid"], false, "{one}");
    let out = p.writ("human", &["verify", "--format", "json"]);
    assert_ne!(
        out.status.code(),
        Some(0),
        "verify exits 0 over a bad signature: {}",
        text(&out)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["valid"], false, "{v}");
    assert!(
        v["failures"].to_string().contains(&id[..12]),
        "the failure does not name the seal: {v}"
    );
}
