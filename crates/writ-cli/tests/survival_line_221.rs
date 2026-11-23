//! Finding 221 (0.4.6): the survival line says how many of the commit's
//! added lines it checked and, when that is fewer than all of them, where
//! the rest were sealed. The 216 shape: lines sealed under spec A, backed
//! out and finished, then restored under spec B with one new line and
//! finished; the anchor rule counts only the new line, and the line must
//! say so instead of reading as a claim about all of them.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

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
            std::env::temp_dir().join(format!("writ-f221-{tag}-{}-{nanos}", std::process::id()));
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

    fn subject(&self) -> String {
        self.git(&["log", "-1", "--format=%s"]).trim().to_string()
    }
}

const LINES: [&str; 6] = [
    "fn alpha() -> u32 { 1 }",
    "fn bravo() -> u32 { 2 }",
    "",
    "fn charlie() -> u32 { 3 }",
    "fn delta() -> u32 { 4 }",
    "fn echo() -> u32 { 5 }",
];

fn text(extra: &[&str]) -> String {
    let mut out = String::from("// base\n");
    for l in LINES.iter().chain(extra) {
        out.push_str(l);
        out.push('\n');
    }
    out
}

fn seal(p: &Project, agent: &str, spec: &str, extra: &[&str]) {
    let mut args = vec![
        "seal", "-s", "work", "--agent", agent, "--spec", spec, "--paths", "lib.rs",
    ];
    args.extend_from_slice(extra);
    p.ok(agent, &args);
}

#[test]
fn f221_survival_line_names_lines_sealed_earlier_under_another_spec() {
    let p = Project::new("restore");
    fs::write(p.root.join("lib.rs"), "// base\n").unwrap();
    p.git(&["add", "lib.rs"]);
    p.git(&["commit", "-q", "-m", "lib"]);

    // Spec A seals the lines, then backs them out, and is finished.
    p.spec("amis", "spec-a");
    fs::write(p.root.join("lib.rs"), text(&[])).unwrap();
    seal(&p, "amis", "spec-a", &[]);
    fs::write(p.root.join("lib.rs"), "// base\n// a only\n").unwrap();
    seal(&p, "amis", "spec-a", &["--allow-removals", "lib.rs"]);
    p.ok(
        "amis",
        &["spec", "done", "spec-a", "-s", "a", "--agent", "amis"],
    );
    p.ok("human", &["finish", "-y"]);

    // Spec B restores them with one new line.
    p.spec("amis", "spec-b");
    fs::write(p.root.join("lib.rs"), text(&["fn foxtrot() -> u32 { 6 }"])).unwrap();
    seal(&p, "amis", "spec-b", &["--allow-removals", "lib.rs"]);
    p.ok(
        "amis",
        &["spec", "done", "spec-b", "-s", "b", "--agent", "amis"],
    );
    let out = p.ok("human", &["finish", "-y"]);

    // 7 added lines: 1 new (checked), 5 sealed earlier under spec-a, 1 blank.
    let want = "1 of 7 added line(s) of 1 spec(s) checked, none lost; \
                5 were sealed earlier under spec-a, 1 blank";
    let printed = out
        .lines()
        .find(|l| l.contains("survival:") && l.contains("checked"))
        .unwrap_or_else(|| panic!("no survival line:\n{out}"));
    assert!(printed.contains(want), "finish printed: {printed}");
    let body = p.git(&["log", "-1", "--format=%b"]);
    assert!(
        body.contains(&format!("Survival: {want}")),
        "receipt:\n{body}"
    );
}

/// When every added line is checked, the line says N of N and nothing more.
#[test]
fn f221_all_added_lines_checked_says_n_of_n() {
    let p = Project::new("all");
    p.spec("amis", "spec-c");
    fs::write(p.root.join("lib.rs"), "fn one() {}\nfn two() {}\n").unwrap();
    seal(&p, "amis", "spec-c", &[]);
    p.ok(
        "amis",
        &["spec", "done", "spec-c", "-s", "c", "--agent", "amis"],
    );
    p.ok("human", &["finish", "-y"]);
    let body = p.git(&["log", "-1", "--format=%b"]);
    let line = body
        .lines()
        .find(|l| l.starts_with("Survival:"))
        .unwrap_or_else(|| panic!("{body}"));
    assert!(
        line.starts_with("Survival: 2 of 2 added line(s) of 1 spec(s) checked, none lost"),
        "{line}"
    );
    assert!(!line.contains("earlier"), "{line}");
}
