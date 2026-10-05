//! S.3 agent-identity (findings 8, 9, 46): one resolver, `WRIT_AGENT_ID`
//! honoured by every command, `spec release`, no auto-claim on `spec add`,
//! and `finish` never cancelling or archiving specs. Drives the real binary
//! with a scrubbed environment so the host's agent variables do not leak in.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use writ_core::Repository;

/// Variables the resolver reads; removed so each test controls identity.
const IDENTITY_VARS: &[&str] = &[
    "WRIT_AGENT_ID",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_SESSION_ID",
    "ANTHROPIC_SESSION_ID",
    "CLAUDECODE",
    "CODEX_SESSION",
    "CODEX_SESSION_ID",
];

struct Project(PathBuf);

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl Project {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("writ-id-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let p = Self(root);
        let init = p.run(&[], &["init", "-y", "--bare"]);
        assert!(init.status.success(), "{}", text(&init));
        p
    }

    /// Run writ with only `env` set among the identity variables.
    fn run(&self, env: &[(&str, &str)], args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_writ"));
        cmd.args(args).current_dir(&self.0).stdin(Stdio::null());
        for v in IDENTITY_VARS {
            cmd.env_remove(v);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    fn ok(&self, env: &[(&str, &str)], args: &[&str]) -> String {
        let out = self.run(env, args);
        assert!(out.status.success(), "writ {args:?}: {}", text(&out));
        text(&out)
    }

    fn repo(&self) -> Repository {
        Repository::open(&self.0).unwrap()
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn spec_add_does_not_claim_and_records_creator() {
    let p = Project::new("add");
    p.ok(
        &[("WRIT_AGENT_ID", "ada")],
        &["spec", "add", "--id", "s", "--title", "s"],
    );
    let spec = p.repo().load_spec("s").unwrap();
    assert_eq!(spec.claimed_by, None);
    assert_eq!(spec.created_by.as_deref(), Some("ada"));
}

#[test]
fn spec_add_claim_claims_for_resolved_agent() {
    let p = Project::new("add-claim");
    p.ok(
        &[("WRIT_AGENT_ID", "ada")],
        &["spec", "add", "--id", "s", "--title", "s", "--claim"],
    );
    assert_eq!(
        p.repo().load_spec("s").unwrap().claimed_by.as_deref(),
        Some("ada")
    );
}

#[test]
fn spec_claim_honours_writ_agent_id() {
    let p = Project::new("claim-env");
    p.ok(&[], &["spec", "add", "--id", "s", "--title", "s"]);
    p.ok(&[("WRIT_AGENT_ID", "bea")], &["spec", "claim", "s"]);
    assert_eq!(
        p.repo().load_spec("s").unwrap().claimed_by.as_deref(),
        Some("bea")
    );
}

#[test]
fn claim_and_seal_resolve_the_same_id_from_a_framework_session() {
    // Finding 9: claim used plain "claude-code", seal "claude-code-<hash>".
    let p = Project::new("one-resolver");
    let env = [("CLAUDE_CODE_SESSION_ID", "hub-session")];
    p.ok(&env, &["spec", "add", "--id", "s", "--title", "s"]);
    p.ok(&env, &["spec", "claim", "s"]);
    fs::write(p.0.join("a.txt"), "a").unwrap();
    p.ok(&env, &["seal", "-s", "w", "--spec", "s"]);
    let repo = p.repo();
    let holder = repo.load_spec("s").unwrap().claimed_by.unwrap();
    assert!(holder.starts_with("claude-code-"), "{holder}");
    assert_eq!(repo.spec_log("s").unwrap()[0].agent.id, holder);
}

#[test]
fn writ_agent_id_beats_framework_session_and_flag_beats_both() {
    let p = Project::new("priority");
    p.ok(&[], &["spec", "add", "--id", "s", "--title", "s"]);
    let env = [("WRIT_AGENT_ID", "sub"), ("CLAUDE_CODE_SESSION_ID", "hub")];
    p.ok(&env, &["spec", "claim", "s"]);
    assert_eq!(
        p.repo().load_spec("s").unwrap().claimed_by.as_deref(),
        Some("sub")
    );
    p.ok(&env, &["spec", "release", "s"]);
    p.ok(&env, &["spec", "claim", "s", "--agent", "flag"]);
    assert_eq!(
        p.repo().load_spec("s").unwrap().claimed_by.as_deref(),
        Some("flag")
    );
}

#[test]
fn release_by_holder_then_another_agent_can_claim() {
    let p = Project::new("release");
    p.ok(&[], &["spec", "add", "--id", "s", "--title", "s"]);
    p.ok(&[("WRIT_AGENT_ID", "ada")], &["spec", "claim", "s"]);
    let denied = p.run(&[("WRIT_AGENT_ID", "bea")], &["spec", "claim", "s"]);
    assert!(!denied.status.success());
    p.ok(&[("WRIT_AGENT_ID", "ada")], &["spec", "release", "s"]);
    p.ok(&[("WRIT_AGENT_ID", "bea")], &["spec", "claim", "s"]);
}

#[test]
fn release_by_non_holder_requires_force() {
    let p = Project::new("release-force");
    p.ok(&[], &["spec", "add", "--id", "s", "--title", "s"]);
    p.ok(&[("WRIT_AGENT_ID", "ada")], &["spec", "claim", "s"]);
    let out = p.run(&[("WRIT_AGENT_ID", "bea")], &["spec", "release", "s"]);
    assert!(!out.status.success());
    assert!(text(&out).contains("ada"), "{}", text(&out));
    let forced = p.ok(
        &[("WRIT_AGENT_ID", "bea")],
        &["spec", "release", "s", "--force"],
    );
    assert!(forced.contains("--force"), "{forced}");
    assert_eq!(p.repo().load_spec("s").unwrap().claimed_by, None);
}

#[test]
fn spec_done_seal_is_attributed_to_caller_not_claim_holder() {
    let p = Project::new("done-attr");
    p.ok(&[], &["spec", "add", "--id", "s", "--title", "s"]);
    p.ok(&[("WRIT_AGENT_ID", "ada")], &["spec", "claim", "s"]);
    fs::write(p.0.join("a.txt"), "a").unwrap();
    p.ok(
        &[("WRIT_AGENT_ID", "ada")],
        &["seal", "-s", "w", "--spec", "s"],
    );
    fs::write(p.0.join("a.txt"), "a2").unwrap();
    p.ok(&[], &["spec", "done", "s", "-s", "done"]);
    let last = p.repo().spec_log("s").unwrap().remove(0);
    assert_eq!(last.agent.id, "human");
    assert!(
        last.warnings.iter().any(|w| w.contains("CLAIM")),
        "{:?}",
        last.warnings
    );
}

fn git(p: &Project, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(&p.0)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", text(&out));
}

#[test]
fn finish_lists_unclaimed_zero_seal_specs_without_cancelling() {
    // Finding 46: the milestone finish cancelled every zero-seal spec.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("writ-id-f46-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let p = Project(root);
    git(&p, &["init", "-q"]);
    git(&p, &["config", "user.name", "writ-test"]);
    git(&p, &["config", "user.email", "writ-test@localhost"]);
    fs::write(p.0.join(".gitignore"), ".writ/\n").unwrap();
    fs::write(p.0.join(".writignore"), ".writ\n.git\n").unwrap();
    fs::write(p.0.join("README.md"), "base\n").unwrap();
    git(&p, &["add", "-A"]);
    git(&p, &["commit", "-q", "-m", "base"]);
    p.ok(&[], &["init", "-y", "--bare"]);

    p.ok(&[], &["spec", "add", "--id", "idle", "--title", "idle"]);
    p.ok(&[], &["spec", "add", "--id", "work", "--title", "work"]);
    fs::write(p.0.join("a.txt"), "a").unwrap();
    p.ok(
        &[("WRIT_AGENT_ID", "ada")],
        &["seal", "-s", "w", "--spec", "work"],
    );
    p.ok(
        &[("WRIT_AGENT_ID", "ada")],
        &["spec", "done", "work", "-s", "done"],
    );

    let out = p.ok(&[], &["finish", "-y"]);
    assert_eq!(
        p.repo().load_spec("idle").unwrap().lifecycle_state,
        writ_core::spec::LifecycleState::Active,
        "{out}"
    );
    assert!(
        out.contains("idle") && out.contains("--archive-unclaimed"),
        "{out}"
    );

    p.ok(&[], &["spec", "add", "--id", "idle2", "--title", "idle2"]);
    fs::write(p.0.join("b.txt"), "b").unwrap();
    p.ok(&[], &["spec", "add", "--id", "work2", "--title", "work2"]);
    p.ok(
        &[("WRIT_AGENT_ID", "ada")],
        &["seal", "-s", "w", "--spec", "work2"],
    );
    p.ok(
        &[("WRIT_AGENT_ID", "ada")],
        &["spec", "done", "work2", "-s", "d"],
    );
    p.ok(&[], &["finish", "-y", "--archive-unclaimed"]);
    assert_eq!(
        p.repo().load_spec("idle2").unwrap().lifecycle_state,
        writ_core::spec::LifecycleState::Cancelled
    );
}
