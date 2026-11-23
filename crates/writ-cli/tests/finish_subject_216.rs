//! Finding 216 (0.4.6): `writ finish` takes its subject from the spec with
//! the most seals in the range, never from a spec with 0 seals while one
//! with seals is committed. Sprint 8's finish took an empty auto spec's
//! words because it was listed first.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use writ_core::spec::AutoSpec;
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
            std::env::temp_dir().join(format!("writ-f216-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let p = Self { root };
        p.git(&["init", "-q"]);
        p.git(&["config", "user.name", "writ-test"]);
        p.git(&["config", "user.email", "writ-test@localhost"]);
        fs::write(p.root.join(".gitignore"), ".writ/\n").unwrap();
        fs::write(p.root.join(".writignore"), ".writ\n.git\n").unwrap();
        fs::write(p.root.join("README.md"), "base\n").unwrap();
        p.git(&["add", "-A"]);
        p.git(&["commit", "-q", "-m", "base"]);
        p.ok("setup", &["init", "-y", "--bare"]);
        p
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

    fn ok(&self, agent: &str, args: &[&str]) -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_writ"))
            .args(args)
            .current_dir(&self.root)
            .env("WRIT_AGENT_ID", agent)
            .env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .stdin(Stdio::null())
            .output()
            .unwrap();
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

    /// `n` seals of `spec`, each a new file, then done with `words`.
    fn sealed_done(&self, agent: &str, spec: &str, n: usize, words: &str) {
        self.spec(agent, spec);
        for i in 0..n {
            let path = format!("{spec}-{i}.txt");
            fs::write(self.root.join(&path), format!("{spec} {i}\n")).unwrap();
            self.ok(
                agent,
                &[
                    "seal", "-s", "work", "--agent", agent, "--spec", spec, "--paths", &path,
                ],
            );
        }
        self.ok(
            agent,
            &["spec", "done", spec, "-s", words, "--agent", agent],
        );
    }

    /// An auto spec (as the hook opens one) closed with words and no seal:
    /// sprint 8's `auto-74cd7ce5-5` shape. Its id sorts first.
    fn empty_auto_done(&self, id: &str, words: &str) {
        let repo = Repository::open(&self.root).unwrap();
        let mut spec = writ_core::spec::Spec::new(id.into(), "auto work by cc".into(), "".into());
        spec.auto = Some(AutoSpec {
            session: "s-216".into(),
            parent: None,
            host_agent_id: None,
            host_agent_type: None,
            closed_by_session_end: None,
        });
        repo.add_spec(&spec).unwrap();
        self.ok(
            "cc",
            &[
                "spec",
                "done",
                id,
                "-s",
                words,
                "--agent",
                "cc",
                "--no-seal",
            ],
        );
    }

    fn subject(&self) -> String {
        self.git(&["log", "-1", "--format=%s"]).trim().to_string()
    }
}

#[test]
fn f216_subject_comes_from_the_spec_with_most_seals_not_an_empty_auto_spec() {
    let p = Project::new("most");
    p.empty_auto_done("aaa-auto", "Empty auto spec opened from an agent message");
    p.sealed_done("a", "mm-one", 1, "One seal of work");
    p.sealed_done("b", "zz-three", 3, "Three seals of the real feature");

    p.ok("human", &["finish", "-y"]);

    let subject = p.subject();
    assert!(
        subject.starts_with("writ: 3 features complete — Three seals of the real feature, +2 more"),
        "{subject}"
    );
    assert!(!subject.contains("Empty auto spec"), "{subject}");
}

/// Ties keep the listed (id) order; 0 seal specs never lead even when
/// every other spec has one seal.
#[test]
fn f216_ties_keep_list_order_and_empty_specs_go_last() {
    let p = Project::new("tie");
    p.empty_auto_done("aaa-auto", "Empty auto spec");
    p.sealed_done("a", "bb-first", 1, "First by id");
    p.sealed_done("b", "cc-second", 1, "Second by id");

    p.ok("human", &["finish", "-y"]);

    let subject = p.subject();
    assert!(
        subject.starts_with("writ: 3 features complete — First by id, +2 more"),
        "{subject}"
    );
}

/// The same order in `Repository::summary().headline`, which is the
/// message the Python `finish` commits with.
#[test]
fn f216_summary_headline_lists_most_sealed_spec_first() {
    let p = Project::new("headline");
    p.empty_auto_done("aaa-auto", "Empty auto spec");
    p.sealed_done("a", "mm-one", 1, "One seal of work");
    p.sealed_done("b", "zz-three", 3, "Three seals");

    let headline = Repository::open(&p.root)
        .unwrap()
        .summary()
        .unwrap()
        .headline;
    assert!(
        // The summary counts a 0 seal spec as not complete (its effective
        // status), so two features; before 216 they were listed mm-one first.
        headline.starts_with("writ: 2 features complete — zz-three, mm-one"),
        "{headline}"
    );
}
