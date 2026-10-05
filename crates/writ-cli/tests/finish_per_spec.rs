//! S.2 finish-per-spec (findings 31, 37, 48, 49): every finish path commits
//! through one engine. Drives the real binary in a throwaway git + writ
//! project. Bri's isolation group covers the headline fixture; these pin
//! the engine's edges.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use writ_core::spec::CommitState;
use writ_core::Repository;

struct Project {
    root: PathBuf,
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

impl Project {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("writ-fps-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let p = Self { root };
        p.git(&["init", "-q"]);
        p.git(&["config", "user.name", "writ-test"]);
        p.git(&["config", "user.email", "writ-test@localhost"]);
        p.write(".gitignore", ".writ/\n");
        p.write(".writignore", ".writ\n.git\n");
        p.write("README.md", "base\n");
        p.git(&["add", "-A"]);
        p.git(&["commit", "-q", "-m", "base"]);
        p.ok("setup", &["init", "-y", "--bare"]);
        p
    }

    fn write(&self, rel: &str, content: &str) {
        fs::write(self.root.join(rel), content).unwrap();
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}:\n{}", combined(&out));
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn writ(&self, agent: &str, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_writ"))
            .args(args)
            .current_dir(&self.root)
            .env("WRIT_AGENT_ID", agent)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn ok(&self, agent: &str, args: &[&str]) -> String {
        let out = self.writ(agent, args);
        let text = combined(&out);
        assert!(out.status.success(), "writ {args:?} as {agent}:\n{text}");
        text
    }

    fn spec(&self, agent: &str, id: &str) {
        self.ok(
            agent,
            &["spec", "add", "--id", id, "--title", id, "--claim"],
        );
    }

    fn seal_done(&self, agent: &str, spec: &str, paths: &str) {
        self.ok(
            agent,
            &[
                "seal", "-s", "work", "--agent", agent, "--spec", spec, "--paths", paths,
            ],
        );
        self.ok(agent, &["spec", "done", spec, "-s", spec, "--agent", agent]);
    }

    /// Set `[workflow] finish_check` (init already writes a `[workflow]` table).
    fn finish_check(&self, cmd: &str) {
        let p = self.root.join(".writ/config.toml");
        let cfg = fs::read_to_string(&p).unwrap();
        let line = format!("[workflow]\nfinish_check = {cmd:?}");
        assert!(cfg.contains("[workflow]"), "{cfg}");
        fs::write(p, cfg.replacen("[workflow]", &line, 1)).unwrap();
    }

    fn commits(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .git(&["rev-list", "--reverse", "HEAD"])
            .lines()
            .map(String::from)
            .collect();
        v.remove(0);
        v
    }

    fn commit_state(&self, id: &str) -> (CommitState, Option<String>) {
        let s = Repository::open(&self.root).unwrap().load_spec(id).unwrap();
        (s.commit_state, s.commit_hash)
    }

    fn head_file(&self, path: &str) -> String {
        self.git(&["show", &format!("HEAD:{path}")])
    }
}

/// Two independent completed specs, sb depending on sa, completed in the
/// reverse order.
fn two_specs(tag: &str) -> Project {
    let p = Project::new(tag);
    p.spec("a", "sa");
    p.spec("b", "sb");
    p.ok("setup", &["spec", "update", "sb", "--depends-on", "sa"]);
    p.write("b.txt", "b\n");
    p.seal_done("b", "sb", "b.txt");
    p.write("a.txt", "a\n");
    p.seal_done("a", "sa", "a.txt");
    p
}

#[test]
fn per_spec_commits_dependencies_first_and_marks_each_spec() {
    let p = two_specs("order");

    p.ok("human", &["finish", "-y", "--strategy", "per-spec"]);

    let commits = p.commits();
    assert_eq!(commits.len(), 2);
    assert_eq!(
        p.commit_state("sa"),
        (CommitState::Committed, Some(commits[0].clone()))
    );
    assert_eq!(
        p.commit_state("sb"),
        (CommitState::Committed, Some(commits[1].clone()))
    );
}

#[test]
fn failing_check_commits_nothing_and_resets_the_index() {
    let p = two_specs("check-fail");
    p.finish_check("exit 3");

    let out = p.writ("human", &["finish", "-y"]);

    assert!(!out.status.success(), "{}", combined(&out));
    assert!(
        combined(&out).contains("staged tree fails"),
        "{}",
        combined(&out)
    );
    assert!(p.commits().is_empty());
    assert_eq!(p.git(&["diff", "--cached", "--name-only"]).trim(), "");
    assert_ne!(p.commit_state("sa").0, CommitState::Committed);
}

#[test]
fn check_runs_on_the_staged_tree_not_the_working_tree() {
    let p = two_specs("check-tree");
    // Unsealed working-tree file the check must not see.
    p.write("stray.txt", "unsealed\n");
    p.finish_check("test -f a.txt && test -f b.txt && test ! -f stray.txt");

    let text = p.ok("human", &["finish", "-y"]);

    assert!(text.contains("staged tree checks clean"), "{text}");
    assert_eq!(p.commits().len(), 1);
}

#[test]
fn no_check_skips_a_failing_check() {
    let p = two_specs("no-check");
    p.finish_check("false");

    p.ok("human", &["finish", "-y", "--no-check"]);

    assert_eq!(p.commits().len(), 1);
}

/// sa (complete) and so (open) both sealed shared.txt; so's version is newer.
fn shared_with_open(tag: &str) -> Project {
    let p = Project::new(tag);
    p.spec("a", "sa");
    p.spec("o", "so");
    p.write("shared.txt", "one\n");
    p.seal_done("a", "sa", "shared.txt");
    p.write("shared.txt", "one\ntwo from open\n");
    p.ok(
        "o",
        &[
            "seal",
            "-s",
            "wip",
            "--agent",
            "o",
            "--spec",
            "so",
            "--paths",
            "shared.txt",
        ],
    );
    p
}

#[test]
fn newer_content_from_open_spec_is_listed_and_committed_by_default() {
    let p = shared_with_open("shared-default");

    let text = p.ok("human", &["finish", "-y"]);

    assert!(
        text.contains("newer content sealed under open specs"),
        "{text}"
    );
    assert!(text.contains("so seal"), "{text}");
    assert_eq!(p.head_file("shared.txt"), "one\ntwo from open\n");
}

#[test]
fn strict_refuses_a_stale_own_blob_and_names_the_later_seal() {
    let p = shared_with_open("shared-strict");

    let out = p.writ("human", &["finish", "-y", "--strict"]);

    let text = combined(&out);
    assert!(!out.status.success(), "{text}");
    assert!(
        text.contains("shared.txt") && text.contains("so seal"),
        "{text}"
    );
    assert!(p.commits().is_empty());
}

#[test]
fn strict_commits_when_nothing_is_stale() {
    let p = two_specs("strict-clean");

    let text = p.ok("human", &["finish", "-y", "--strict"]);

    assert!(text.contains("sealed version is current"), "{text}");
    assert_eq!(p.commits().len(), 1);
}

#[test]
fn strict_ignores_overlap_between_specs_in_the_same_finish() {
    let p = Project::new("strict-same-run");
    p.spec("a", "sa");
    p.spec("c", "sc");
    p.write("shared.txt", "one\n");
    p.seal_done("a", "sa", "shared.txt");
    p.write("shared.txt", "one\ntwo\n");
    p.seal_done("c", "sc", "shared.txt");

    p.ok("human", &["finish", "-y", "--strict"]);

    assert_eq!(p.head_file("shared.txt"), "one\ntwo\n");
}

#[test]
fn accept_honors_the_proposal_strategy_and_stages_only_sealed_content() {
    let p = two_specs("accept");
    p.write("stray.txt", "unsealed\n");
    let text = p.ok("human", &["finish", "--propose", "--strategy", "per-spec"]);
    let id = text
        .split_whitespace()
        .find(|w| w.starts_with("prop-"))
        .unwrap_or_else(|| panic!("no proposal id in:\n{text}"))
        .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-')
        .to_string();

    p.ok("human", &["finish", "--accept", &id]);

    let commits = p.commits();
    assert_eq!(commits.len(), 2, "per-spec proposal made one commit");
    let files = p.git(&["show", "--name-only", "--pretty=format:", "HEAD"]);
    assert!(!files.contains("stray.txt"), "{files}");
}

#[test]
fn auto_honors_per_spec_strategy() {
    let p = two_specs("auto");

    p.ok("human", &["finish", "--auto", "--strategy", "per-spec"]);

    assert_eq!(p.commits().len(), 2);
}
