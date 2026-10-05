//! Finding 39 and the committed `.mcp.json` (Andrew, 2026-10-05): init and
//! uninit treat project files git tracks as the project's, and `--bare`
//! still keeps `.writ/` out of git.

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

const WRIT_ONLY: &str = "{\n  \"mcpServers\": {\n    \"writ\": {\"command\": \"writ\", \"args\": [\"mcp-serve\"]}\n  }\n}\n";
const OTHER_ONLY: &str = "{\"mcpServers\": {\"other\": {\"command\": \"other\"}}}\n";
const BOTH: &str =
    "{\"mcpServers\": {\"other\": {\"command\": \"other\"}, \"writ\": {\"command\": \"writ\", \"args\": [\"mcp-serve\"]}}}\n";

impl Project {
    /// A git repo with one commit; nothing writ-related yet.
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("writ-itf-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(root.join("home")).unwrap();
        let p = Self { root };
        p.git(&["init", "-q"]);
        p.git(&["config", "user.name", "writ-test"]);
        p.git(&["config", "user.email", "writ-test@localhost"]);
        p.write("README.md", "base\n");
        p.write(".git/info/exclude", "home/\n");
        p.git(&["add", "README.md"]);
        p.git(&["commit", "-q", "-m", "base"]);
        p
    }

    fn write(&self, rel: &str, content: &str) {
        fs::write(self.root.join(rel), content).unwrap();
    }

    fn read(&self, rel: &str) -> Option<String> {
        fs::read_to_string(self.root.join(rel)).ok()
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

    fn commit_file(&self, rel: &str, content: &str) {
        self.write(rel, content);
        self.git(&["add", rel]);
        self.git(&["commit", "-q", "-m", rel]);
    }

    /// Run writ with HOME inside the project so no global config is touched.
    fn ok(&self, args: &[&str]) -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_writ"))
            .args(args)
            .current_dir(&self.root)
            .env("HOME", self.root.join("home"))
            .env("WRIT_AGENT_ID", "setup")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let text = combined(&out);
        assert!(out.status.success(), "writ {args:?}:\n{text}");
        text
    }

    fn init_claude(&self) -> String {
        self.ok(&["init", "-y", "--frameworks", "claude"])
    }
}

#[test]
fn bare_init_git_ignores_the_writ_dir() {
    let p = Project::new("bare");

    p.ok(&["init", "-y", "--bare"]);

    let ignore = p.read(".gitignore").unwrap_or_default();
    assert!(ignore.lines().any(|l| l.trim() == ".writ/"), "{ignore}");
    let status = p.git(&["status", "--porcelain", "--untracked-files=all"]);
    assert!(!status.contains(".writ/"), "{status}");
}

#[test]
fn bare_init_keeps_an_existing_gitignore_entry_single() {
    let p = Project::new("bare-existing");
    p.commit_file(".gitignore", "target/\n.writ/\n");

    p.ok(&["init", "-y", "--bare"]);

    assert_eq!(p.read(".gitignore").unwrap(), "target/\n.writ/\n");
}

#[test]
fn init_leaves_a_committed_mcp_json_byte_identical() {
    let p = Project::new("mcp-tracked");
    p.commit_file(".mcp.json", WRIT_ONLY);

    p.init_claude();

    assert_eq!(p.read(".mcp.json").as_deref(), Some(WRIT_ONLY));
}

#[test]
fn init_does_not_edit_a_committed_mcp_json_without_writ_and_says_so() {
    let p = Project::new("mcp-tracked-other");
    p.commit_file(".mcp.json", OTHER_ONLY);

    let text = p.init_claude();

    assert_eq!(p.read(".mcp.json").as_deref(), Some(OTHER_ONLY));
    assert!(text.contains("mcp-install"), "{text}");
}

#[test]
fn init_adds_writ_to_an_untracked_mcp_json_keeping_other_servers() {
    let p = Project::new("mcp-untracked-other");
    p.write(".mcp.json", OTHER_ONLY);

    p.init_claude();

    let v: serde_json::Value = serde_json::from_str(&p.read(".mcp.json").unwrap()).unwrap();
    assert!(v.pointer("/mcpServers/other").is_some(), "{v}");
    assert!(v.pointer("/mcpServers/writ").is_some(), "{v}");
}

#[test]
fn uninit_keeps_a_committed_mcp_json() {
    let p = Project::new("uninit-tracked");
    p.commit_file(".mcp.json", WRIT_ONLY);
    p.init_claude();

    let text = p.ok(&["uninit", "-y"]);

    assert_eq!(p.read(".mcp.json").as_deref(), Some(WRIT_ONLY));
    assert!(text.contains("kept .mcp.json"), "{text}");
}

#[test]
fn uninit_removes_only_writs_entry_from_an_untracked_mcp_json() {
    let p = Project::new("uninit-untracked-both");
    p.write(".mcp.json", BOTH);
    p.init_claude();

    p.ok(&["uninit", "-y"]);

    let v: serde_json::Value = serde_json::from_str(&p.read(".mcp.json").unwrap()).unwrap();
    assert!(v.pointer("/mcpServers/other").is_some(), "{v}");
    assert!(v.pointer("/mcpServers/writ").is_none(), "{v}");
}

#[test]
fn uninit_deletes_an_untracked_mcp_json_it_generated() {
    let p = Project::new("uninit-generated");
    p.init_claude();
    assert!(
        p.read(".mcp.json").is_some(),
        "init did not generate .mcp.json"
    );

    p.ok(&["uninit", "-y"]);

    assert!(p.read(".mcp.json").is_none());
}
