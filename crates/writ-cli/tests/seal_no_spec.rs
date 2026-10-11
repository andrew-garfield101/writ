//! C.13 seal-without-spec gate (findings 98, 99, 100, sprint 4).
//!
//! - 98: an explicit `--agent` is enforced exactly like an agent detected
//!   from the environment: refused, nothing recorded, the fix named.
//! - 99: the human warning names the full fix, `writ spec add "..." --claim`.
//! - 100: under `[security] claim_enforcement = "strict"` a human's seal with
//!   no spec is refused and nothing is recorded.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// Every variable writ reads as an agent identity (agent.rs).
const AGENT_ENV: &[&str] = &[
    "WRIT_AGENT_ID",
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_SESSION_ID",
    "ANTHROPIC_SESSION_ID",
    "CODEX_SESSION",
    "CODEX_SESSION_ID",
    "CURSOR_SESSION_ID",
    "AIDER_SESSION",
];

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "writ-seal-no-spec-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        writ_core::Repository::init(&dir).unwrap();
        Self(dir)
    }

    /// Run writ with no agent identity in the environment (a human shell).
    fn writ(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_writ"));
        cmd.args(args)
            .current_dir(&self.0)
            .env_remove("WRIT_FORMAT")
            .stdin(Stdio::null());
        for var in AGENT_ENV {
            cmd.env_remove(var);
        }
        cmd.output().unwrap()
    }

    fn seal_count(&self) -> usize {
        let dir = self.0.join(".writ").join("seals");
        fs::read_dir(dir).map(|d| d.count()).unwrap_or(0)
    }

    fn write(&self, rel: &str, text: &str) {
        fs::write(self.0.join(rel), text).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn assert_refused(s: &Scratch, before: usize, out: &Output) {
    let all = text(out);
    assert!(!out.status.success(), "seal with no spec succeeded:\n{all}");
    assert!(
        all.contains("writ spec add \"") && all.contains("--claim"),
        "refusal does not name the fix:\n{all}"
    );
    assert_eq!(
        s.seal_count(),
        before,
        "a refused seal was recorded:\n{all}"
    );
}

fn set_strict(root: &Path) {
    let path = root.join(".writ").join("config.toml");
    let mut text = fs::read_to_string(&path).unwrap_or_default();
    text.push_str("\n[security]\nclaim_enforcement = \"strict\"\n");
    fs::write(path, text).unwrap();
}

#[test]
fn explicit_agent_flag_alone_refuses_a_seal_with_no_spec() {
    // Finding 98.
    let s = Scratch::new("agent-flag");
    s.write("app.py", "print('hello')\n");
    let before = s.seal_count();
    let out = s.writ(&["seal", "-s", "manual agent", "--agent", "my-agent"]);
    assert_refused(&s, before, &out);
}

#[test]
fn human_no_spec_warning_names_spec_add_claim() {
    // Finding 99.
    let s = Scratch::new("human-warn");
    s.write("app.py", "print('hello')\n");
    let before = s.seal_count();
    let out = s.writ(&["seal", "-s", "human work"]);
    let all = text(&out);
    assert!(out.status.success(), "{all}");
    assert!(all.to_lowercase().contains("no spec"), "{all}");
    assert!(
        all.contains("writ spec add \"") && all.contains("--claim"),
        "{all}"
    );
    assert_eq!(s.seal_count(), before + 1, "{all}");
}

#[test]
fn strict_claim_enforcement_refuses_a_human_seal_with_no_spec() {
    // Finding 100.
    let s = Scratch::new("strict");
    set_strict(&s.0);
    s.write("app.py", "print('hello')\n");
    let before = s.seal_count();
    let out = s.writ(&["seal", "-s", "human work"]);
    assert_refused(&s, before, &out);
}
