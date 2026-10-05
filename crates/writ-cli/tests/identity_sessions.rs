//! Finding 57: with only CLAUDECODE set, each Claude session (the `claude`
//! process) is one agent across invocations, and two sessions in one repo
//! are two agents. Two fake sessions: a `claude` symlink to /bin/sh that
//! runs a shell, which runs several writ commands.
#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use writ_core::Repository;

struct Project {
    root: PathBuf,
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Project {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("writ-ids-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(root.join("bin")).unwrap();
        std::os::unix::fs::symlink("/bin/sh", root.join("bin/claude")).unwrap();
        let p = Self { root };
        let out = Command::new(env!("CARGO_BIN_EXE_writ"))
            .args(["init", "-y", "--bare", "--no-git"])
            .current_dir(&p.root)
            .env_remove("WRIT_AGENT_ID")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        p
    }

    /// Run `script` inside one fake Claude session (`claude -c script`),
    /// with `writ` resolving to the built binary and only CLAUDECODE set.
    fn session(&self, script: &str) -> String {
        let writ = env!("CARGO_BIN_EXE_writ");
        let out = Command::new(self.root.join("bin/claude"))
            // The session process runs a shell per command batch, as Claude
            // Code's Bash tool does; `; status=$?` keeps it from exec'ing
            // into the shell (which would end the fake session process).
            .args(["-c", "sh -c \"$SCRIPT\"; status=$?; exit $status"])
            .current_dir(&self.root)
            .env("SCRIPT", format!("set -e\n{script}\n"))
            .env("W", writ)
            .env("CLAUDECODE", "1")
            .env_remove("WRIT_AGENT_ID")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CLAUDE_SESSION_ID")
            .env_remove("ANTHROPIC_SESSION_ID")
            .env_remove("CODEX_SESSION")
            .env_remove("CODEX_SESSION_ID")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{script}\n{text}");
        text
    }

    fn claimed_by(&self, spec: &str) -> Option<String> {
        Repository::open(&self.root)
            .unwrap()
            .load_spec(spec)
            .unwrap()
            .claimed_by
    }
}

#[test]
fn one_session_is_one_agent_and_two_sessions_are_two() {
    let p = Project::new("two-sessions");

    p.session(
        "\"$W\" spec add --id sa --title sa --claim\n\
         echo a > a.txt\n\
         \"$W\" seal -s a --spec sa --paths a.txt > seal.out 2>&1\n\
         ! grep -q CLAIM seal.out\n\
         \"$W\" spec release sa\n\
         \"$W\" spec claim sa",
    );
    p.session("\"$W\" spec add --id sb --title sb --claim");

    let a = p.claimed_by("sa").expect("session A holds sa");
    let b = p.claimed_by("sb").expect("session B holds sb");
    assert!(a.starts_with("claude-code-"), "{a}");
    assert_ne!(a, b, "two sessions share one agent id");
}
