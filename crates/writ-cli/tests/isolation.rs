//! Sprint 2 isolation group: S.1 seal-isolation (findings 13, 13a, 13b, 18,
//! 29), S.2 finish-per-spec (finding 31), S.4a status-truth (finding 10), and
//! the 0.3.0 exit criterion end to end. Every test drives the real binary in a
//! throwaway git + writ project.
//!
//! Written against CC's proposed S.1 semantics as amended by the approved
//! design (2026-10-05). A spec's *own files* are its declared `file_scope` (or
//! `--scope` lane) plus every path its seals have captured. Default seal scope
//! without `--paths`:
//! - own file: included;
//! - file owned by another open spec: excluded and listed;
//! - unowned file, no other agent holds an open claim: included;
//! - unowned file, another open claim exists: excluded, with a `--paths` hint;
//! - `file_scope` declared: unowned files outside it are excluded;
//! - file owned by two open specs: included, with a SHARED warning.
//!
//! S.1 landed (Amis 398b219a2c16, 9233c0835b62, 72c98f1f7455): every S.1 and
//! exit-criterion test is live. S.2 and S.4a stay ignored until their seals. Text assertions are deliberately loose (a path or
//! an owner name appears in the output); structure is asserted from the store
//! and from git. Names of new surfaces that are not final yet are constants at
//! the top so a rename is a one-line change.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use writ_core::seal::Seal;
use writ_core::spec::{CommitState, Spec};
use writ_core::Repository;

/// `[security]` key that turns claim violations from a warning into a reject.
/// Final per CC 2026-10-05: values "warn" (default) or "strict".
const CLAIM_ENFORCEMENT_STRICT: &str = "claim_enforcement = \"strict\"";
/// `spec add` flag declaring a lane (new in S.1, repeatable; final per CC).
const SCOPE_FLAG: &str = "--scope";
/// Marker in the seal output for a file owned by two open specs.
const SHARED_MARKER: &str = "SHARED";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Temp project dir removed on drop (writ-cli has no tempfile dev-dependency).
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
    /// Git repo with one committed file, `.writ/` git-ignored, `.writignore`
    /// committed, then `writ init --bare`. Context is clean afterwards and the
    /// only seal is the bridge import.
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("writ-iso-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let p = Self { root };
        p.git(&["init", "-q"]);
        // Throwaway repo: finish commits through libgit2, which needs an identity.
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

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.path(rel)).unwrap()
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed:\n{}",
            combined(&out)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Run writ as `agent` (WRIT_AGENT_ID set, the way a subagent runs it).
    fn writ(&self, agent: &str, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_writ"))
            .args(args)
            .current_dir(&self.root)
            .env("WRIT_AGENT_ID", agent)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    /// Run writ and require success; returns combined output.
    fn ok(&self, agent: &str, args: &[&str]) -> String {
        let out = self.writ(agent, args);
        let text = combined(&out);
        assert!(
            out.status.success(),
            "writ {args:?} as {agent} failed:\n{text}"
        );
        text
    }

    /// Run a shell line (an exact command writ printed) with the built binary
    /// first on PATH.
    fn sh(&self, agent: &str, line: &str) -> Output {
        let bin_dir = Path::new(env!("CARGO_BIN_EXE_writ")).parent().unwrap();
        let path = format!(
            "{}:{}",
            bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new("sh")
            .args(["-c", line])
            .current_dir(&self.root)
            .env("WRIT_AGENT_ID", agent)
            .env("PATH", path)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    /// `spec add --id <id>` then `spec claim` as `agent`.
    fn spec(&self, agent: &str, id: &str) {
        self.ok(agent, &["spec", "add", "--id", id, "--title", id]);
        self.ok(agent, &["spec", "claim", id, "--agent", agent]);
    }

    fn seal(&self, agent: &str, spec: &str, summary: &str) -> Output {
        self.writ(
            agent,
            &["seal", "-s", summary, "--agent", agent, "--spec", spec],
        )
    }

    fn seal_paths(&self, agent: &str, spec: &str, summary: &str, paths: &str) -> String {
        self.ok(
            agent,
            &[
                "seal", "-s", summary, "--agent", agent, "--spec", spec, "--paths", paths,
            ],
        )
    }

    fn done(&self, agent: &str, spec: &str, extra: &[&str]) -> Output {
        let mut args = vec!["spec", "done", spec, "--agent", agent, "-s", "done"];
        args.extend_from_slice(extra);
        self.writ(agent, &args)
    }

    fn repo(&self) -> Repository {
        Repository::open(&self.root).unwrap()
    }

    fn spec_state(&self, id: &str) -> Spec {
        self.repo().load_spec(id).unwrap()
    }

    /// Seals made on `spec` (by `spec_id`), oldest first. Not
    /// `Repository::spec_log`: that walks the parent chain, and a spec's first
    /// seal is parented on the global HEAD, so it also returns other specs'
    /// ancestor seals (finding 26 generalised, S.4a).
    fn spec_seals(&self, spec: &str) -> Vec<Seal> {
        let mut seals: Vec<Seal> = self
            .all_seals()
            .into_iter()
            .filter(|s| s.spec_id.as_deref() == Some(spec))
            .collect();
        seals.sort_by_key(|s| s.timestamp);
        seals
    }

    /// Every seal on every branch (`log` walks HEAD only and would miss a
    /// spec's diverged chain).
    fn all_seals(&self) -> Vec<Seal> {
        self.repo().log_all().unwrap()
    }

    /// Every pending path in `writ context --format json` (run as `agent`,
    /// unfiltered).
    fn pending(&self, agent: &str) -> BTreeSet<String> {
        self.pending_from(agent, &["context", "--format", "json"])
    }

    /// Pending paths in `agent`'s own view (`--for-agent`, which filters the
    /// working state to that agent's files).
    fn pending_for(&self, agent: &str) -> BTreeSet<String> {
        self.pending_from(
            agent,
            &["context", "--format", "json", "--for-agent", agent],
        )
    }

    fn pending_from(&self, agent: &str, args: &[&str]) -> BTreeSet<String> {
        let text = self.ok(agent, args);
        let v: Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("context json did not parse: {e}\n{text}"));
        let ws = &v["working_state"];
        let mut out = BTreeSet::new();
        for key in ["new_files", "modified_files", "deleted_files"] {
            if let Some(arr) = ws[key].as_array() {
                out.extend(arr.iter().filter_map(|x| x.as_str().map(String::from)));
            }
        }
        out
    }

    fn status_json(&self, agent: &str) -> Value {
        let text = self.ok(agent, &["status", "--format", "json"]);
        serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("status json did not parse: {e}\n{text}"))
    }

    /// Append `line` under `[security]` in `.writ/config.toml`.
    fn security_setting(&self, line: &str) {
        let p = self.path(".writ/config.toml");
        let cfg = fs::read_to_string(&p).unwrap();
        let cfg = if cfg.contains("[security]") {
            cfg.replacen("[security]", &format!("[security]\n{line}"), 1)
        } else {
            format!("{cfg}\n[security]\n{line}\n")
        };
        fs::write(p, cfg).unwrap();
    }

    /// Files changed by commit `rev` (relative to its first parent).
    fn commit_files(&self, rev: &str) -> BTreeSet<String> {
        self.git(&["show", "--name-only", "--pretty=format:", rev])
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(String::from)
            .collect()
    }

    fn commit_body(&self, rev: &str) -> String {
        self.git(&["log", "-1", "--pretty=format:%B", rev])
    }

    fn show_file(&self, rev: &str, path: &str) -> String {
        self.git(&["show", &format!("{rev}:{path}")])
    }

    /// Commits after the base commit, oldest first.
    fn new_commits(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .git(&["rev-list", "--reverse", "HEAD"])
            .lines()
            .map(String::from)
            .collect();
        v.remove(0);
        v
    }

    /// Untracked-by-git paths (porcelain `??`).
    fn git_untracked(&self) -> BTreeSet<String> {
        self.git(&["status", "--porcelain", "--untracked-files=all"])
            .lines()
            .filter_map(|l| l.strip_prefix("?? ").map(String::from))
            .collect()
    }

    fn verify_clean(&self) {
        let text = self.ok("setup", &["verify", "--all-chains", "--format", "json"]);
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            v["all_valid"],
            Value::Bool(true),
            "verify not clean:\n{text}"
        );
    }
}

fn paths(seal: &Seal) -> BTreeSet<String> {
    seal.changes.iter().map(|c| c.path.clone()).collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn last(seals: &[Seal]) -> &Seal {
    seals.last().expect("expected at least one seal")
}

/// Two agents, two claimed specs, each already owning one file through a
/// first seal with `--paths`. Afterwards both files are modified and pending.
fn two_agents_with_own_files(tag: &str) -> Project {
    let p = Project::new(tag);
    p.spec("a", "sa");
    p.spec("b", "sb");
    p.write("a.txt", "a1\n");
    p.write("b.txt", "b1\n");
    p.seal_paths("a", "sa", "a owns a.txt", "a.txt");
    p.seal_paths("b", "sb", "b owns b.txt", "b.txt");
    p.write("a.txt", "a2\n");
    p.write("b.txt", "b2\n");
    assert_eq!(p.pending("a"), set(&["a.txt", "b.txt"]), "precondition");
    p
}

// ---------------------------------------------------------------------------
// S.1 seal without --paths
// ---------------------------------------------------------------------------

#[test]
fn s1_seal_without_paths_captures_only_own_files_with_two_agents_pending() {
    let p = two_agents_with_own_files("own");

    let out = p.seal("a", "sa", "a without --paths");
    let text = combined(&out);

    assert!(out.status.success(), "seal failed:\n{text}");
    assert_eq!(paths(last(&p.spec_seals("sa"))), set(&["a.txt"]), "{text}");
    assert!(
        text.contains("b.txt"),
        "b.txt not listed as left out:\n{text}"
    );
    assert!(p.pending("b").contains("b.txt"), "b.txt no longer pending");
}

/// Finding 13b: a spec-scoped seal wrote every pending file into the shared
/// index before capturing, so another agent's pending file vanished from
/// context, status and finish. With explicit `--paths`.
#[test]
// Non-regression pin: passes on 0.2.1 and must keep passing through S.1.
fn s1_13b_other_agents_pending_file_survives_seal_with_paths() {
    let p = two_agents_with_own_files("13b-paths");

    p.seal_paths("a", "sa", "a with --paths", "a.txt");

    assert!(p.pending("b").contains("b.txt"), "b.txt gone from context");
    assert!(
        p.pending_for("b").contains("b.txt"),
        "b.txt gone from b's view"
    );
    let status = p.status_json("b");
    let untracked: BTreeSet<String> = status["untracked_changes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        untracked.contains("b.txt"),
        "b.txt gone from status: {status}"
    );
    let out = p.seal("b", "sb", "b later");
    assert!(out.status.success(), "{}", combined(&out));
    let b_last = paths(last(&p.spec_seals("sb")));
    assert!(
        b_last.contains("b.txt"),
        "b's later seal missed b.txt: {b_last:?}"
    );
}

/// Finding 13b on the default (no `--paths`) path.
#[test]
fn s1_13b_other_agents_pending_file_survives_default_scope_seal() {
    let p = two_agents_with_own_files("13b-default");

    let out = p.seal("a", "sa", "a default scope");
    assert!(out.status.success(), "{}", combined(&out));

    assert!(p.pending("b").contains("b.txt"), "b.txt gone from context");
    assert!(
        p.pending_for("b").contains("b.txt"),
        "b.txt gone from b's view"
    );
    let out = p.seal("b", "sb", "b later");
    assert!(out.status.success(), "{}", combined(&out));
    assert_eq!(paths(last(&p.spec_seals("sb"))), set(&["b.txt"]));
    assert!(
        p.pending("a").is_empty(),
        "left pending: {:?}",
        p.pending("a")
    );
}

// ---------------------------------------------------------------------------
// S.1 default-scope classification
// ---------------------------------------------------------------------------

#[test]
fn s1_file_owned_by_another_open_spec_is_excluded_and_listed() {
    let p = two_agents_with_own_files("other-owned");
    // Seal a's pending edit away, then a edits again: a.txt (own) and b.txt
    // (sb's) pending.
    p.seal_paths("a", "sa", "a catches up", "a.txt");
    p.write("a.txt", "a3\n");

    let text = combined(&p.seal("a", "sa", "a default"));

    let a_last = paths(last(&p.spec_seals("sa")));
    assert!(
        !a_last.contains("b.txt"),
        "swept b's file: {a_last:?}\n{text}"
    );
    assert!(text.contains("b.txt"), "b.txt not listed:\n{text}");
    assert!(p.pending("b").contains("b.txt"));
}

#[test]
// Non-regression pin: passes on 0.2.1 and must keep passing through S.1.
fn s1_unowned_file_included_when_no_other_agent_holds_an_open_claim() {
    let p = Project::new("unowned-solo");
    p.spec("a", "sa");
    p.write("a.txt", "a1\n");
    p.seal_paths("a", "sa", "a owns a.txt", "a.txt");
    p.write("new.txt", "fresh\n");

    let out = p.seal("a", "sa", "a default");

    assert!(out.status.success(), "{}", combined(&out));
    assert_eq!(paths(last(&p.spec_seals("sa"))), set(&["new.txt"]));
}

#[test]
fn s1_unowned_file_excluded_with_paths_hint_when_another_open_claim_exists() {
    let p = two_agents_with_own_files("unowned-contested");
    p.write("new.txt", "whose?\n");

    let text = combined(&p.seal("a", "sa", "a default"));

    let a_last = paths(last(&p.spec_seals("sa")));
    assert!(
        !a_last.contains("new.txt"),
        "unowned file swept: {a_last:?}"
    );
    assert!(a_last.contains("a.txt"), "own file not sealed: {a_last:?}");
    assert!(text.contains("new.txt"), "new.txt not listed:\n{text}");
    assert!(text.contains("--paths"), "no --paths hint:\n{text}");
    assert!(p.pending("a").contains("new.txt"));
}

#[test]
fn s1_declared_file_scope_excludes_unowned_files_outside_it() {
    // Solo agent, so the "no other claim" rule would include new files; the
    // declared scope must still keep out-of-scope files out.
    let p = Project::new("file-scope");
    p.spec("a", "sa");
    p.ok("a", &["spec", "update", "sa", "--file-scope", "src/a.rs"]);
    p.write("src/a.rs", "fn a() {}\n");
    p.write("notes.txt", "outside scope\n");

    let text = combined(&p.seal("a", "sa", "a default"));

    let seals = p.spec_seals("sa");
    assert_eq!(paths(last(&seals)), set(&["src/a.rs"]), "{text}");
    assert!(text.contains("notes.txt"), "notes.txt not listed:\n{text}");
    assert!(p.pending("a").contains("notes.txt"));
}

#[test]
// Non-regression pin: passes on 0.2.1 and must keep passing through S.1.
fn s1_file_scope_unset_first_seal_on_fresh_spec_solo_captures_all_pending() {
    let p = Project::new("first-seal");
    p.spec("a", "sa");
    p.write("a.txt", "a\n");
    p.write("src/lib.rs", "pub fn x() {}\n");

    let out = p.seal("a", "sa", "first");

    assert!(out.status.success(), "{}", combined(&out));
    assert_eq!(
        paths(last(&p.spec_seals("sa"))),
        set(&["a.txt", "src/lib.rs"])
    );
}

#[test]
fn s1_file_owned_by_two_open_specs_is_included_with_shared_warning() {
    let p = two_agents_with_own_files("shared");
    p.write("shared.txt", "v1\n");
    p.seal_paths("a", "sa", "a touches shared", "shared.txt");
    p.write("shared.txt", "v2\n");
    p.seal_paths("b", "sb", "b touches shared", "shared.txt");
    p.write("shared.txt", "v3\n");

    let text = combined(&p.seal("a", "sa", "a default"));

    let a_last = paths(last(&p.spec_seals("sa")));
    assert!(
        a_last.contains("shared.txt"),
        "shared file left out: {a_last:?}"
    );
    assert!(!a_last.contains("b.txt"), "b's own file swept: {a_last:?}");
    assert!(text.contains(SHARED_MARKER), "no SHARED warning:\n{text}");
}

// ---------------------------------------------------------------------------
// S.1 single-agent non-regression (pinned hard)
// ---------------------------------------------------------------------------

#[test]
// Non-regression pin: passes on 0.2.1 and must keep passing through S.1.
fn s1_solo_agent_later_seal_still_captures_new_and_modified_files() {
    let p = Project::new("solo");
    p.spec("a", "sa");
    p.write("a.txt", "a1\n");
    assert!(p.seal("a", "sa", "first").status.success());
    p.write("a.txt", "a2\n");
    p.write("b/new.txt", "new\n");
    p.write("c.txt", "another new\n");

    let out = p.seal("a", "sa", "second");

    assert!(out.status.success(), "{}", combined(&out));
    assert_eq!(
        paths(last(&p.spec_seals("sa"))),
        set(&["a.txt", "b/new.txt", "c.txt"])
    );
    assert!(p.pending("a").is_empty());
}

#[test]
fn s1_spec_done_with_no_pending_own_files_closes_without_a_seal() {
    let p = Project::new("done-clean");
    p.spec("a", "sa");
    p.write("a.txt", "a1\n");
    assert!(p.seal("a", "sa", "work").status.success());
    let before = p.all_seals().len();

    let out = p.done("a", "sa", &[]);

    assert!(out.status.success(), "{}", combined(&out));
    assert_eq!(p.all_seals().len(), before, "spec done created a seal");
    assert_eq!(
        p.spec_state("sa").status,
        writ_core::spec::SpecStatus::Complete
    );
}

// ---------------------------------------------------------------------------
// S.1 spec done
// ---------------------------------------------------------------------------

#[test]
fn s1_spec_done_with_another_agents_file_pending_never_sweeps() {
    let p = two_agents_with_own_files("done-other");

    let out = p.done("a", "sa", &[]);
    let text = combined(&out);

    assert!(out.status.success(), "{text}");
    for s in p.spec_seals("sa") {
        assert!(
            !paths(&s).contains("b.txt"),
            "spec done swept b.txt\n{text}"
        );
    }
    assert!(paths(last(&p.spec_seals("sa"))).contains("a.txt"));
    assert!(p.pending("b").contains("b.txt"), "b.txt no longer pending");
    assert_eq!(
        p.spec_state("sa").status,
        writ_core::spec::SpecStatus::Complete
    );
}

#[test]
fn s1_spec_done_paths_seals_exactly_the_given_paths() {
    let p = two_agents_with_own_files("done-paths");
    p.write("extra.txt", "a made this\n");

    let out = p.done("a", "sa", &["--paths", "a.txt,extra.txt"]);

    assert!(out.status.success(), "{}", combined(&out));
    assert_eq!(
        paths(last(&p.spec_seals("sa"))),
        set(&["a.txt", "extra.txt"])
    );
    assert!(p.pending("b").contains("b.txt"));
}

#[test]
fn s1_spec_done_no_own_files_no_paths_seals_nothing_and_says_so() {
    let p = two_agents_with_own_files("done-nothing");
    p.spec("c", "sc");
    let before = p.all_seals().len();

    let out = p.done("c", "sc", &[]);
    let text = combined(&out);

    assert_eq!(p.all_seals().len(), before, "seal created:\n{text}");
    assert!(
        text.to_lowercase().contains("nothing") || text.contains("--paths"),
        "did not say it sealed nothing:\n{text}"
    );
    assert_eq!(p.pending("a"), set(&["a.txt", "b.txt"]));
}

// ---------------------------------------------------------------------------
// S.1 claims respected
// ---------------------------------------------------------------------------

#[test]
fn s1_seal_onto_spec_held_by_another_agent_warns_with_owner_by_default() {
    let p = two_agents_with_own_files("claim-default");

    let text = p.seal_paths("b", "sa", "b intrudes", "b.txt");

    assert!(text.contains("sa") && text.contains('a'), "{text}");
    assert!(
        text.contains("claimed by a") || text.contains("held by a") || text.contains("owner: a"),
        "owner not named:\n{text}"
    );
    assert_eq!(p.spec_state("sa").claimed_by.as_deref(), Some("a"));
}

#[test]
fn s1_seal_onto_spec_held_by_another_agent_rejected_under_strict() {
    let p = two_agents_with_own_files("claim-strict");
    p.security_setting(CLAIM_ENFORCEMENT_STRICT);
    let before = p.all_seals().len();

    let out = p.writ(
        "b",
        &[
            "seal",
            "-s",
            "b intrudes",
            "--agent",
            "b",
            "--spec",
            "sa",
            "--paths",
            "b.txt",
        ],
    );
    let text = combined(&out);

    assert!(!out.status.success(), "strict claim seal accepted:\n{text}");
    assert_eq!(p.all_seals().len(), before, "a seal was written");
    assert!(text.contains('a') && text.contains("sa"), "{text}");
    assert!(p.pending("b").contains("b.txt"), "rejected seal ate b.txt");
}

#[test]
// Non-regression pin: passes on 0.2.1 and must keep passing through S.1.
fn s1_holder_can_still_seal_under_strict() {
    let p = two_agents_with_own_files("claim-holder");
    p.security_setting(CLAIM_ENFORCEMENT_STRICT);

    p.seal_paths("a", "sa", "holder", "a.txt");

    assert_eq!(paths(last(&p.spec_seals("sa"))), set(&["a.txt"]));
}

// ---------------------------------------------------------------------------
// S.1 finish stages only sealed paths of completed specs
// ---------------------------------------------------------------------------

/// sa: a.txt sealed and done. sb: b.txt sealed, in progress. u.txt unsealed.
fn finish_fixture(tag: &str) -> Project {
    let p = two_agents_with_own_files(tag);
    p.seal_paths("a", "sa", "a final", "a.txt");
    p.seal_paths("b", "sb", "b wip", "b.txt");
    assert!(p.done("a", "sa", &[]).status.success());
    p.write("u.txt", "nobody sealed me\n");
    p
}

#[test]
fn s1_finish_leaves_unsealed_and_in_progress_files_out_and_lists_them() {
    let p = finish_fixture("finish-out");

    let text = p.ok("human", &["finish", "-y"]);

    let files = p.commit_files("HEAD");
    assert!(files.contains("a.txt"), "{files:?}\n{text}");
    assert!(
        !files.contains("u.txt"),
        "unsealed file committed: {files:?}"
    );
    assert!(
        !files.contains("b.txt"),
        "in-progress file committed: {files:?}"
    );
    assert!(!files.iter().any(|f| f.starts_with(".writ/")), "{files:?}");
    assert!(text.contains("u.txt"), "unsealed not listed:\n{text}");
    assert!(text.contains("b.txt"), "in-progress not listed:\n{text}");
    let untracked = p.git_untracked();
    assert!(untracked.contains("u.txt") && untracked.contains("b.txt"));
    assert_eq!(p.spec_state("sa").commit_state, CommitState::Committed);
    assert_ne!(p.spec_state("sb").commit_state, CommitState::Committed);
}

#[test]
fn s1_finish_include_unsealed_takes_the_unsealed_file() {
    let p = finish_fixture("finish-incl");

    p.ok("human", &["finish", "-y", "--include-unsealed"]);

    let files = p.commit_files("HEAD");
    assert!(
        files.contains("a.txt") && files.contains("u.txt"),
        "{files:?}"
    );
}

#[test]
fn s1_finish_commits_sealed_blob_not_disk_content_and_lists_drift() {
    let p = Project::new("finish-blob");
    p.spec("a", "sa");
    p.write("a.txt", "sealed version\n");
    p.seal_paths("a", "sa", "a final", "a.txt");
    assert!(p.done("a", "sa", &[]).status.success());
    p.write("a.txt", "edited after seal\n");

    let text = p.ok("human", &["finish", "-y"]);

    assert_eq!(p.show_file("HEAD", "a.txt"), "sealed version\n");
    assert_eq!(p.read("a.txt"), "edited after seal\n", "disk was rewritten");
    assert!(text.contains("a.txt"), "drift not listed:\n{text}");
}

/// Finding 44: a sealed path git ignores (here a whole ignored directory with a
/// `!` re-include git cannot honour) was silently left out of the finish
/// commit. Finish must list it with the reason and still commit the rest.
#[test]
fn s1_f44_finish_lists_sealed_path_it_cannot_stage_with_reason() {
    // The writ-repo shape: the file is sealed, then a .gitignore rule covering
    // its directory lands (git cannot re-include under an ignored directory).
    let p = Project::new("f44");
    p.spec("a", "sa");
    p.write("a.txt", "a\n");
    p.write("results/.gitignore", "*\n!.gitignore\n");
    p.seal_paths("a", "sa", "a work", "a.txt,results/.gitignore");
    let sealed: BTreeSet<String> = p.spec_seals("sa").iter().flat_map(paths).collect();
    assert!(
        sealed.contains("results/.gitignore"),
        "precondition: {sealed:?}"
    );
    p.write(".gitignore", ".writ/\nresults/\n");
    p.git(&["add", ".gitignore"]);
    p.git(&["commit", "-q", "-m", "ignore results"]);
    p.seal_paths("a", "sa", "ignore results", ".gitignore");
    let done = p.done("a", "sa", &[]);
    assert!(done.status.success(), "{}", combined(&done));

    let text = p.ok("human", &["finish", "-y"]);

    let files = p.commit_files("HEAD");
    assert!(files.contains("a.txt"), "{files:?}\n{text}");
    assert!(
        !files.contains("results/.gitignore"),
        "precondition: git ignores it"
    );
    assert!(
        text.contains("results/.gitignore"),
        "sealed but unstageable path not listed:\n{text}"
    );
    assert!(
        text.to_lowercase().contains("ignore"),
        "no reason given:\n{text}"
    );
}

// ---------------------------------------------------------------------------
// 0.3.0 exit criterion, end to end (Aubs)
// ---------------------------------------------------------------------------

/// Assertions shared by both exit-criterion variants after one finish.
fn assert_exit_criterion(p: &Project, finish_text: &str) {
    let files = p.commit_files("HEAD");
    assert_eq!(
        files,
        set(&["src/a/lib.rs", "src/b/lib.rs"]),
        "commit is not exactly both agents' work\n{finish_text}"
    );
    assert!(p.git_untracked().contains("stray.txt"), "stray swept");
    for (agent, spec, file) in [("a", "sa", "src/a/lib.rs"), ("b", "sb", "src/b/lib.rs")] {
        for s in p.all_seals() {
            if paths(&s).contains(file) {
                assert_eq!(s.agent.id, agent, "{file} sealed by {}", s.agent.id);
                assert_eq!(s.spec_id.as_deref(), Some(spec), "{file} on wrong spec");
            }
        }
        let other = if file.contains("/a/") {
            "src/b/lib.rs"
        } else {
            "src/a/lib.rs"
        };
        for s in p.spec_seals(spec) {
            assert!(!paths(&s).contains(other), "{spec} captured {other}");
        }
        assert_eq!(p.spec_state(spec).commit_state, CommitState::Committed);
    }
    p.verify_clean();
}

/// Variant 1: each agent declares a lane with `spec add --scope`, then seals
/// with no `--paths`; the seal lands first time.
#[test]
fn exit_criterion_two_agents_with_declared_lanes() {
    let p = Project::new("exit-lanes");
    for (agent, spec, lane) in [("a", "sa", "src/a/**"), ("b", "sb", "src/b/**")] {
        p.ok(
            agent,
            &[
                "spec", "add", "--id", spec, "--title", spec, SCOPE_FLAG, lane,
            ],
        );
        p.ok(agent, &["spec", "claim", spec, "--agent", agent]);
    }
    p.write("src/a/lib.rs", "pub fn a() {}\n");
    p.write("src/b/lib.rs", "pub fn b() {}\n");
    p.write("stray.txt", "nobody's\n");

    for (agent, spec) in [("a", "sa"), ("b", "sb")] {
        let out = p.seal(agent, spec, "work");
        assert!(out.status.success(), "{}", combined(&out));
        let file = format!("src/{agent}/lib.rs");
        assert_eq!(paths(last(&p.spec_seals(spec))), set(&[file.as_str()]));
    }
    for (agent, spec) in [("a", "sa"), ("b", "sb")] {
        assert!(p.done(agent, spec, &[]).status.success());
    }
    let text = p.ok("human", &["finish", "-y"]);

    assert_exit_criterion(&p, &text);
}

/// Variant 3, the live run's exact shape (0.3.0 exit run, 2026-10-05): both
/// agents declare overlapping lanes as ONE comma-separated `--scope` string
/// with `--claim`, the way the generated template led two real agents to,
/// then seal with `--paths`. auth owns auth.py and README.md, crud owns
/// tests/test_tasks.py, both touch the shared app.py (crud builds on auth's
/// sealed version). Finding 66: the string must parse into its globs, so no
/// seal reports a file outside scope, and a seal without `--paths` captures
/// a lane-only file.
#[test]
fn exit_criterion_live_shape_comma_separated_scope_has_no_scope_warnings() {
    let p = Project::new("exit-live-scope");
    p.ok(
        "auth",
        &[
            "spec",
            "add",
            "--id",
            "auth",
            "--title",
            "auth",
            SCOPE_FLAG,
            "app.py,models.py,auth.py,tests/*,README.md",
            "--claim",
        ],
    );
    p.ok(
        "crud",
        &[
            "spec",
            "add",
            "--id",
            "crud",
            "--title",
            "crud",
            SCOPE_FLAG,
            "app.py,models.py,tests/**",
            "--claim",
        ],
    );
    assert_eq!(p.spec_state("auth").claimed_by.as_deref(), Some("auth"));
    assert_eq!(p.spec_state("crud").claimed_by.as_deref(), Some("crud"));

    p.write("app.py", "app = 1\n# auth routes\n");
    p.write("auth.py", "def login(): ...\n");
    p.write("tests/test_auth.py", "def test_login(): ...\n");
    let a1 = p.seal_paths("auth", "auth", "auth", "app.py,auth.py,tests/test_auth.py");
    p.write("app.py", "app = 1\n# auth routes\n# task routes\n");
    p.write("tests/test_tasks.py", "def test_tasks(): ...\n");
    let c1 = p.seal_paths("crud", "crud", "crud", "app.py,tests/test_tasks.py");
    // Lane-only capture: README.md is in auth's lane and nobody else's.
    p.write("README.md", "base\nauth docs\n");
    let a2 = combined(&p.seal("auth", "auth", "docs"));

    for (label, out) in [
        ("auth seal", &a1),
        ("crud seal", &c1),
        ("auth lane seal", &a2),
    ] {
        assert!(
            !out.contains("outside spec") && !out.contains("SCOPE:"),
            "{label} reported files outside scope:\n{out}"
        );
    }
    for s in p.all_seals().iter().filter(|s| s.spec_id.is_some()) {
        let scope: Vec<&String> = s.warnings.iter().filter(|w| w.contains("SCOPE")).collect();
        assert!(
            scope.is_empty(),
            "seal {} ({:?}) warnings: {scope:?}",
            s.id,
            s.spec_id
        );
    }
    assert!(
        paths(last(&p.spec_seals("auth"))).contains("README.md"),
        "lane seal without --paths missed README.md:\n{a2}"
    );
    assert!(p
        .spec_seals("auth")
        .iter()
        .all(|s| !paths(s).contains("tests/test_tasks.py")));
    assert!(p
        .spec_seals("crud")
        .iter()
        .all(|s| !paths(s).contains("auth.py")));

    for (agent, spec) in [("auth", "auth"), ("crud", "crud")] {
        assert!(p.done(agent, spec, &[]).status.success());
    }
    let text = p.ok("human", &["finish", "-y"]);

    assert_eq!(
        p.commit_files("HEAD"),
        set(&[
            "README.md",
            "app.py",
            "auth.py",
            "tests/test_auth.py",
            "tests/test_tasks.py"
        ]),
        "{text}"
    );
    assert_eq!(
        p.git(&["show", "HEAD:app.py"]),
        "app = 1\n# auth routes\n# task routes\n"
    );
    p.verify_clean();
}

/// Pull the first pasteable `writ seal ... --paths ...` line out of output.
fn pasteable_seal_command(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let start = l.find("writ seal")?;
        let cmd = l[start..].trim().trim_end_matches('`').trim().to_string();
        cmd.contains("--paths").then_some(cmd)
    })
}

/// Variant 2: no lanes. The first seal excludes the contested files and
/// prints an exact command; the agent pastes it and the retry lands.
#[test]
fn exit_criterion_two_agents_follow_the_printed_paths_command() {
    let p = Project::new("exit-hint");
    p.spec("a", "sa");
    p.spec("b", "sb");
    p.write("src/a/lib.rs", "pub fn a() {}\n");
    p.write("src/b/lib.rs", "pub fn b() {}\n");
    p.write("stray.txt", "nobody's\n");

    for (agent, spec) in [("a", "sa"), ("b", "sb")] {
        let file = format!("src/{agent}/lib.rs");
        let first = combined(&p.seal(agent, spec, "work"));
        let swept = p
            .spec_seals(spec)
            .iter()
            .any(|s| paths(s).contains("stray.txt"));
        assert!(!swept, "first seal swept stray.txt:\n{first}");
        let cmd = pasteable_seal_command(&first)
            .unwrap_or_else(|| panic!("no pasteable command printed:\n{first}"));
        // The agent edits only the path list, as the hint tells it to.
        let cmd = cmd.replace("<path>", &file).replace("<paths>", &file);
        let retry = p.sh(agent, &cmd);
        assert!(
            retry.status.success(),
            "`{cmd}` failed:\n{}",
            combined(&retry)
        );
        let sealed: BTreeSet<String> = p.spec_seals(spec).iter().flat_map(paths).collect();
        assert!(sealed.contains(&file), "retry `{cmd}` did not land {file}");
    }
    for (agent, spec) in [("a", "sa"), ("b", "sb")] {
        assert!(p.done(agent, spec, &[]).status.success());
    }
    let text = p.ok("human", &["finish", "-y"]);

    assert_exit_criterion(&p, &text);
}

// ---------------------------------------------------------------------------
// S.2 finish --strategy per-spec
// ---------------------------------------------------------------------------

/// Three specs in dependency order sa <- sb <- sc, completed out of that order
/// (sa, sc, sb) so completion time and dependency order disagree. sa and sc
/// both seal shared.txt. Returns the project.
fn per_spec_fixture(tag: &str) -> Project {
    let p = Project::new(tag);
    p.spec("a", "sa");
    p.spec("b", "sb");
    p.spec("c", "sc");
    p.ok("setup", &["spec", "update", "sb", "--depends-on", "sa"]);
    p.ok("setup", &["spec", "update", "sc", "--depends-on", "sb"]);
    p.write("a.txt", "a\n");
    p.write("shared.txt", "v1 from a\n");
    p.seal_paths("a", "sa", "a work", "a.txt,shared.txt");
    assert!(p.done("a", "sa", &[]).status.success());
    p.write("c.txt", "c\n");
    p.write("shared.txt", "v2 from c\n");
    p.seal_paths("c", "sc", "c work", "c.txt,shared.txt");
    assert!(p.done("c", "sc", &[]).status.success());
    p.write("b.txt", "b\n");
    p.seal_paths("b", "sb", "b work", "b.txt");
    assert!(p.done("b", "sb", &[]).status.success());
    p
}

#[test]
fn s2_per_spec_commits_once_per_spec_in_dependency_order() {
    let p = per_spec_fixture("per-spec");

    let text = p.ok("human", &["finish", "-y", "--strategy", "per-spec"]);

    let commits = p.new_commits();
    assert_eq!(commits.len(), 3, "expected 3 commits:\n{text}");
    let expect = [
        ("sa", set(&["a.txt", "shared.txt"])),
        ("sb", set(&["b.txt"])),
        ("sc", set(&["c.txt"])),
    ];
    for (commit, (spec, files)) in commits.iter().zip(expect.iter()) {
        assert_eq!(&p.commit_files(commit), files, "{spec} commit {commit}");
        let st = p.spec_state(spec);
        assert_eq!(
            st.commit_state,
            CommitState::Committed,
            "{spec} not committed"
        );
        assert_eq!(
            st.commit_hash.as_deref(),
            Some(commit.as_str()),
            "{spec} hash"
        );
    }
    assert!(
        p.commit_body(&commits[2]).contains("shared.txt"),
        "sc's commit body does not list shared.txt"
    );
    assert_eq!(p.show_file("HEAD", "shared.txt"), "v2 from c\n");
}

#[test]
fn s2_per_spec_commits_nothing_unsealed_or_in_progress() {
    let p = per_spec_fixture("per-spec-stray");
    p.spec("d", "sd");
    p.write("d.txt", "wip\n");
    p.seal_paths("d", "sd", "d wip", "d.txt");
    p.write("stray.txt", "unsealed\n");

    p.ok("human", &["finish", "-y", "--strategy", "per-spec"]);

    for c in p.new_commits() {
        let files = p.commit_files(&c);
        assert!(
            !files.contains("stray.txt") && !files.contains("d.txt"),
            "{c}: {files:?}"
        );
    }
    assert_ne!(p.spec_state("sd").commit_state, CommitState::Committed);
}

// ---------------------------------------------------------------------------
// S.4a status truth
// ---------------------------------------------------------------------------

fn status_agent_for(status: &Value, spec: &str) -> Option<String> {
    ["specs_in_progress", "specs_completed", "specs_committed"]
        .iter()
        .filter_map(|k| status[*k].as_array())
        .flatten()
        .find(|s| s["id"] == spec)
        .and_then(|s| s["agent"].as_str().map(String::from))
}

#[test]
fn s4a_status_agent_column_reads_claims_before_any_seal() {
    let p = Project::new("status-claim");
    p.spec("a", "sa");
    p.spec("b", "sb");

    let status = p.status_json("human");

    assert_eq!(
        status_agent_for(&status, "sa").as_deref(),
        Some("a"),
        "{status}"
    );
    assert_eq!(
        status_agent_for(&status, "sb").as_deref(),
        Some("b"),
        "{status}"
    );
    let human = p.ok("human", &["status", "--format", "human"]);
    assert!(
        !human.contains("unknown"),
        "human status says unknown:\n{human}"
    );
}

#[test]
fn s4a_status_agent_column_is_the_claim_holder_not_the_last_sealer() {
    let p = two_agents_with_own_files("status-holder");
    // Default enforcement warns but lets b seal onto sa.
    p.seal_paths("b", "sa", "b helps", "b.txt");

    let status = p.status_json("human");

    assert_eq!(
        status_agent_for(&status, "sa").as_deref(),
        Some("a"),
        "{status}"
    );
}

#[test]
fn s4a_spec_show_format_json_emits_json() {
    let p = Project::new("show-json");
    p.spec("a", "sa");

    let text = p.ok("human", &["spec", "show", "sa", "--format", "json"]);

    let v: Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("spec show --format json is not JSON: {e}\n{text}"));
    assert_eq!(v["id"], "sa");
    assert_eq!(v["claimed_by"], "a");
}

// ---------------------------------------------------------------------------
// Harness self-checks (run today, keep the fixture honest)
// ---------------------------------------------------------------------------

#[test]
fn harness_fixture_starts_clean_with_two_owned_files_pending() {
    let p = two_agents_with_own_files("harness");
    assert_eq!(paths(last(&p.spec_seals("sa"))), set(&["a.txt"]));
    assert_eq!(paths(last(&p.spec_seals("sb"))), set(&["b.txt"]));
    assert_eq!(p.spec_state("sa").claimed_by.as_deref(), Some("a"));
    assert!(p.git_untracked().contains("a.txt"));
    p.verify_clean();
}

#[test]
fn harness_pasteable_command_parser() {
    let out = "  left out: new.txt\n  hint: run `writ seal -s \"x\" --paths new.txt`\n";
    assert_eq!(
        pasteable_seal_command(out).as_deref(),
        Some("writ seal -s \"x\" --paths new.txt")
    );
    assert_eq!(pasteable_seal_command("writ seal -s x"), None);
}
