//! S.4b convergence-records (finding 32) with the finding 41 amendment, end to
//! end through the real binary.
//!
//! Finding 32: converge wrote convergence_v3 record objects that nothing
//! referenced, so every converge grew the orphan count. Finding 41 (Haris): a
//! converge also stores the merged file contents before anything references
//! them; with the record write removed, a real merge still left one
//! unreferenced object per merged file, and a `gc run` between converge and
//! finish deleted them. Fix: `.writ/convergence_v3_pending.json` is a
//! live-set root.
//!
//! Expectation (CC, 2026-10-05): after a dry-run converge that produces new
//! merged content, the orphan count is unchanged, every merged object named in
//! the pending file is present, verify is clean; `gc run` between converge and
//! materialize keeps those objects, and finish then commits the merged file.
//!
//! Also pins a merge-correctness regression seen on released 0.2.1: in this
//! exact fixture `writ finish` committed only spec b's edit (line 2 A lost),
//! with or without gc. The sprint-2 tree merges both.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

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

const BASE_LINES: usize = 40;

fn base_text() -> String {
    (0..BASE_LINES).map(|i| format!("line {i}\n")).collect()
}

fn with_edit(text: &str, line: usize, suffix: &str) -> String {
    text.lines()
        .enumerate()
        .map(|(i, l)| {
            if i == line {
                format!("{l} {suffix}\n")
            } else {
                format!("{l}\n")
            }
        })
        .collect()
}

impl Project {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("writ-s4b-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let p = Self { root };
        p.git(&["init", "-q"]);
        p.git(&["config", "user.name", "writ-test"]);
        p.git(&["config", "user.email", "writ-test@localhost"]);
        p.write(".gitignore", ".writ/\n");
        p.write(".writignore", ".writ\n.git\n");
        p.write("shared.txt", &base_text());
        p.git(&["add", "-A"]);
        p.git(&["commit", "-q", "-m", "base"]);
        p.ok("setup", &["init", "-y", "--bare"]);
        p
    }

    fn write(&self, rel: &str, content: &str) {
        fs::write(self.root.join(rel), content).unwrap();
    }

    fn writ_dir(&self) -> PathBuf {
        self.root.join(".writ")
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
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let text = combined(&out);
        assert!(out.status.success(), "writ {args:?} as {agent}:\n{text}");
        text
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = Command::new(env!("CARGO_BIN_EXE_writ"))
            .args(args)
            .current_dir(&self.root)
            .env("WRIT_AGENT_ID", "setup")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("writ {args:?} not JSON ({e}):\n{}", combined(&out)))
    }

    fn orphans(&self) -> u64 {
        let v = self.json(&["gc", "audit", "--format", "json"]);
        v["orphaned_objects"]
            .as_u64()
            .unwrap_or_else(|| panic!("no orphaned_objects in gc audit: {v}"))
    }

    fn verify_clean(&self, when: &str) {
        let v = self.json(&["verify", "--all-chains", "--format", "json"]);
        assert_eq!(v["all_valid"], Value::Bool(true), "verify {when}: {v}");
    }

    fn object_exists(&self, hash: &str) -> bool {
        self.writ_dir()
            .join("objects")
            .join(&hash[..2])
            .join(&hash[2..])
            .exists()
    }

    /// `(path, hash)` pairs from the pending convergence preview.
    fn pending_shadow_results(&self) -> Vec<(String, String)> {
        let p = self.writ_dir().join("convergence_v3_pending.json");
        let text = fs::read_to_string(&p)
            .unwrap_or_else(|e| panic!("no pending convergence file after dry run: {e}"));
        let v: Value = serde_json::from_str(&text).unwrap();
        v["shadow_results"]
            .as_array()
            .unwrap_or_else(|| panic!("pending file has no shadow_results: {v}"))
            .iter()
            .map(|pair| {
                (
                    pair[0].as_str().unwrap().to_string(),
                    pair[1].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }
}

/// Two specs from one base, disjoint one-line edits to shared.txt, both done,
/// then `finish --dry-run` (converges, writes the pending preview, does not
/// materialize). Returns the project and the orphan count before converging.
fn converged_dry_run(tag: &str) -> (Project, u64) {
    let p = Project::new(tag);
    for (agent, spec) in [("a", "sa"), ("b", "sb")] {
        p.ok(agent, &["spec", "add", "--id", spec, "--title", spec]);
        p.ok(agent, &["spec", "claim", spec, "--agent", agent]);
    }
    let base = base_text();
    p.write("shared.txt", &with_edit(&base, 2, "A"));
    p.ok(
        "a",
        &[
            "seal",
            "-s",
            "a",
            "--agent",
            "a",
            "--spec",
            "sa",
            "--paths",
            "shared.txt",
        ],
    );
    p.write("shared.txt", &with_edit(&base, 30, "B"));
    p.ok(
        "b",
        &[
            "seal",
            "-s",
            "b",
            "--agent",
            "b",
            "--spec",
            "sb",
            "--paths",
            "shared.txt",
        ],
    );
    let before = p.orphans();
    p.ok("a", &["spec", "done", "sa", "--agent", "a", "-s", "done"]);
    p.ok("b", &["spec", "done", "sb", "--agent", "b", "-s", "done"]);
    let text = p.ok("human", &["finish", "--dry-run"]);
    assert!(
        text.contains("merged"),
        "dry run reported no convergence:\n{text}"
    );
    (p, before)
}

fn expected_merge() -> String {
    with_edit(&with_edit(&base_text(), 2, "A"), 30, "B")
}

#[test]
fn s4b_dry_run_converge_leaves_orphan_count_unchanged_and_verify_clean() {
    let (p, before) = converged_dry_run("orphans");

    let shadows = p.pending_shadow_results();

    assert!(
        shadows.iter().any(|(path, _)| path == "shared.txt"),
        "shared.txt not in pending shadow results: {shadows:?}"
    );
    for (path, hash) in &shadows {
        assert!(
            p.object_exists(hash),
            "merged object for {path} missing: {hash}"
        );
    }
    assert_eq!(p.orphans(), before, "dry-run converge added orphans");
    p.verify_clean("after dry-run converge");
}

#[test]
fn s4b_gc_run_between_converge_and_finish_keeps_merged_objects() {
    let (p, _) = converged_dry_run("gc-between");
    let shadows = p.pending_shadow_results();

    p.ok("human", &["gc", "run", "--yes"]);

    for (path, hash) in &shadows {
        assert!(
            p.object_exists(hash),
            "gc run deleted merged {path} ({hash})"
        );
    }
    p.verify_clean("after gc run");
    p.ok("human", &["finish", "-y"]);
    assert_eq!(
        p.git(&["show", "HEAD:shared.txt"]),
        expected_merge(),
        "finish after gc did not commit the merged file"
    );
    assert_eq!(
        fs::read_to_string(p.root.join("shared.txt")).unwrap(),
        expected_merge()
    );
    p.verify_clean("after finish");
}

#[test]
fn s4b_disjoint_edits_merge_keeps_both_sides_without_gc() {
    let (p, _) = converged_dry_run("merge");

    let out = p.ok("human", &["finish", "-y"]);

    assert_eq!(p.git(&["show", "HEAD:shared.txt"]), expected_merge());
    // CC option c: b's version carries none of a's additions, so it is a
    // stale rewrite merged as concurrent, with a notice naming what b's
    // version did not carry.
    assert!(
        out.contains("shared.txt") && out.contains("line 2 A"),
        "no stale-rewrite notice naming a's line:\n{out}"
    );
    assert!(Path::new(&p.root.join("shared.txt")).exists());
}

/// Finding 42, variant B (CC, 2026-10-05): spec b's version of shared.txt is
/// on disk but not yet sealed when spec a runs `spec done`. shared.txt is
/// one of a's own files and pending, so a's final seal captures b's version
/// and records the removal of a's own "line 2 A"; b's later `--paths` seal
/// then finds nothing pending, sb closes with zero seals, and finish commits
/// "line 30 B" only, attributed to a. Variant A (b seals first) keeps both.
#[test]
fn f42_variant_b_spec_done_with_other_agents_unsealed_edit_keeps_own_edit() {
    let p = Project::new("f42-b");
    for (agent, spec) in [("a", "sa"), ("b", "sb")] {
        p.ok(agent, &["spec", "add", "--id", spec, "--title", spec]);
        p.ok(agent, &["spec", "claim", spec, "--agent", agent]);
    }
    let base = base_text();
    p.write("shared.txt", &with_edit(&base, 2, "A"));
    p.ok(
        "a",
        &[
            "seal",
            "-s",
            "a",
            "--agent",
            "a",
            "--spec",
            "sa",
            "--paths",
            "shared.txt",
        ],
    );
    // b rewrites the file from its own base; not sealed yet.
    p.write("shared.txt", &with_edit(&base, 30, "B"));

    let done = raw(
        &p,
        "a",
        &["spec", "done", "sa", "--agent", "a", "-s", "done"],
    );
    // On the current build b's seal finds nothing pending (a's final seal
    // took b's content) and fails; tolerated, the commit is what is asserted.
    let b_seal = raw(
        &p,
        "b",
        &[
            "seal",
            "-s",
            "b",
            "--agent",
            "b",
            "--spec",
            "sb",
            "--paths",
            "shared.txt",
        ],
    );
    let _ = raw(
        &p,
        "b",
        &["spec", "done", "sb", "--agent", "b", "-s", "done"],
    );
    let fin = raw(&p, "human", &["finish", "-y"]);

    // Either the loss is refused/escalated before anything is committed, or
    // the commit holds both edits. Silently committing B only is the bug.
    let escalated = !done.status.success() || !fin.status.success();
    if !escalated {
        assert_eq!(
            p.git(&["show", "HEAD:shared.txt"]),
            expected_merge(),
            "a's sealed edit lost at spec done:\n{}\n{}\n{}",
            combined(&done),
            combined(&b_seal),
            combined(&fin)
        );
    } else {
        let log = combined(&done) + &combined(&fin);
        assert!(
            log.contains("line 2 A") || log.contains("shared.txt"),
            "{log}"
        );
    }
}

/// Run writ as `agent` without asserting success.
fn raw(p: &Project, agent: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_writ"))
        .args(args)
        .current_dir(&p.root)
        .env("WRIT_AGENT_ID", agent)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// Finding 42 variant B, escalated at seal time (merge-survival-check, Haris
/// 9d89a7ce4e16). Both sweep-style entry points of spec a, a default-scope
/// `seal` and `spec done`, must refuse to record the removal of a's own
/// "line 2 A" while b's unsealed version is on disk: non-zero exit, the path
/// and the lost line named, nothing written, a's spec still open. b can then
/// seal its edit, a re-applies its line, and finish commits both edits.
#[test]
fn f42_variant_b_is_refused_at_seal_time_and_both_edits_reach_the_commit() {
    let p = Project::new("f42-b-seal");
    for (agent, spec) in [("a", "sa"), ("b", "sb")] {
        p.ok(agent, &["spec", "add", "--id", spec, "--title", spec]);
        p.ok(agent, &["spec", "claim", spec, "--agent", agent]);
    }
    let base = base_text();
    p.write("shared.txt", &with_edit(&base, 2, "A"));
    p.ok(
        "a",
        &[
            "seal",
            "-s",
            "a",
            "--agent",
            "a",
            "--spec",
            "sa",
            "--paths",
            "shared.txt",
        ],
    );
    p.write("shared.txt", &with_edit(&base, 30, "B"));
    let seals_before = seal_count(&p);

    for args in [
        &["seal", "-s", "sweep", "--agent", "a", "--spec", "sa"][..],
        &["spec", "done", "sa", "--agent", "a", "-s", "done"][..],
    ] {
        let out = raw(&p, "a", args);
        let text = combined(&out);
        assert!(!out.status.success(), "{args:?} recorded the loss:\n{text}");
        assert!(
            text.contains("shared.txt") && text.contains("line 2 A"),
            "{args:?} refusal must name path and lost line:\n{text}"
        );
        assert_eq!(seal_count(&p), seals_before, "{args:?} wrote a seal");
    }
    let status = p.json(&["spec", "status", "--format", "json"]);
    assert_eq!(
        spec_status(&status, "sa"),
        "in-progress",
        "refused spec done closed sa"
    );
    assert_eq!(
        fs::read_to_string(p.root.join("shared.txt")).unwrap(),
        with_edit(&base, 30, "B")
    );

    // Recovery: b seals its own edit, a re-applies its line and closes.
    p.ok(
        "b",
        &[
            "seal",
            "-s",
            "b",
            "--agent",
            "b",
            "--spec",
            "sb",
            "--paths",
            "shared.txt",
        ],
    );
    p.write("shared.txt", &expected_merge());
    p.ok(
        "a",
        &[
            "spec",
            "done",
            "sa",
            "--agent",
            "a",
            "-s",
            "done",
            "--paths",
            "shared.txt",
        ],
    );
    p.ok("b", &["spec", "done", "sb", "--agent", "b", "-s", "done"]);
    p.ok("human", &["finish", "-y"]);

    assert_eq!(p.git(&["show", "HEAD:shared.txt"]), expected_merge());
    p.verify_clean("after recovery finish");
}

fn seal_count(p: &Project) -> usize {
    let v = p.json(&["log", "--all", "--format", "json"]);
    v.as_array()
        .or_else(|| v["seals"].as_array())
        .map(|a| a.len())
        .unwrap_or_else(|| panic!("log json: {v}"))
}

fn spec_status(status: &Value, id: &str) -> String {
    status
        .as_array()
        .and_then(|a| a.iter().find(|s| s["id"] == id))
        .and_then(|s| s["status"].as_str())
        .unwrap_or_else(|| panic!("spec {id} not in status json: {status}"))
        .to_string()
}

// ---------------------------------------------------------------------------
// Findings 42 and 62 are duals (Aubs): every input addition survives the
// merge, and every informed later deletion survives too. Fixture is the shape
// hit on this repo: spec A adds five `#[ignore]` markers, spec B removes them.
// Whether B's removal wins depends only on ancestry: B sealed with A's content
// in its recorded before (informed) versus B sealed from the baseline
// (concurrent, B never saw the markers).
// ---------------------------------------------------------------------------

const IGNORE_MARKER: &str = "#[ignore = \"waiting on fix\"]";

fn lib_base() -> String {
    (0..5)
        .map(|i| format!("#[test]\nfn case_{i}() {{\n    assert!(true);\n}}\n\n"))
        .collect()
}

/// Spec a's other addition: a helper the ignored tests will use. Gives a
/// content besides the markers, so an informed b retains some of a (CC's
/// option c: a b version that retains none of a's additions is treated as a
/// stale rewrite, not an informed removal).
const A_HELPER: &str = "fn helper_from_spec_a() -> bool {\n    true\n}\n\n";

/// lib.rs as spec a seals it: helper plus five markers.
fn lib_with_markers() -> String {
    A_HELPER.to_string() + &lib_base().replace("#[test]\n", &format!("#[test]\n{IGNORE_MARKER}\n"))
}

/// Spec a's version with the markers removed and the helper kept.
fn lib_a_without_markers() -> String {
    A_HELPER.to_string() + &lib_base()
}

/// B's own addition, so B's version is never byte-identical to the base.
const B_ADDITION: &str = "// case_5 added by spec b\n";

/// Commits lib.rs with five tests, then spec a seals five ignore markers and
/// spec b seals a version without them plus its own line. `informed`: b
/// writes over a's sealed file (b's before holds the markers). Otherwise the
/// tree is restored to the bridge baseline first, so b's before is the base.
/// Both specs close, finish runs; returns the committed lib.rs.
fn markers_fixture(tag: &str, informed: bool) -> (Project, String) {
    let p = Project::new(tag);
    p.write("lib.rs", &lib_base());
    p.git(&["add", "lib.rs"]);
    p.git(&["commit", "-q", "-m", "lib"]);
    // Re-import so the baseline holds lib.rs (init imported before it existed).
    p.ok("setup", &["bridge", "import"]);
    let baseline_id = newest_seal_id(&p);

    for (agent, spec) in [("a", "sa"), ("b", "sb")] {
        p.ok(agent, &["spec", "add", "--id", spec, "--title", spec]);
        p.ok(agent, &["spec", "claim", spec, "--agent", agent]);
    }
    p.write("lib.rs", &lib_with_markers());
    p.ok(
        "a",
        &[
            "seal", "-s", "ignore 5", "--agent", "a", "--spec", "sa", "--paths", "lib.rs",
        ],
    );

    if !informed {
        p.ok("b", &["restore", &baseline_id, "--force"]);
        assert_eq!(
            fs::read_to_string(p.root.join("lib.rs")).unwrap(),
            lib_base()
        );
    }
    // Informed b keeps a's helper and drops only the markers; concurrent b
    // writes from the base and never saw either.
    let b_start = if informed {
        lib_a_without_markers()
    } else {
        lib_base()
    };
    p.write("lib.rs", &(b_start + B_ADDITION));
    p.ok(
        "b",
        &[
            "seal",
            "-s",
            "un-ignore + case 5",
            "--agent",
            "b",
            "--spec",
            "sb",
            "--paths",
            "lib.rs",
        ],
    );

    let before = b_before_holds_markers(&p);
    assert_eq!(
        before, informed,
        "fixture precondition: b's recorded before"
    );

    p.ok("a", &["spec", "done", "sa", "--agent", "a", "-s", "done"]);
    p.ok("b", &["spec", "done", "sb", "--agent", "b", "-s", "done"]);
    let fin = raw(&p, "human", &["finish", "-y"]);
    assert!(fin.status.success(), "finish:\n{}", combined(&fin));
    let committed = p.git(&["show", "HEAD:lib.rs"]);
    (p, committed)
}

fn newest_seal_id(p: &Project) -> String {
    let v = p.json(&["log", "--all", "--format", "json"]);
    let seals = v
        .as_array()
        .or_else(|| v["seals"].as_array())
        .unwrap()
        .clone();
    seals
        .iter()
        .max_by_key(|s| s["timestamp"].as_str().unwrap().to_string())
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Whether spec b's seal recorded a before for lib.rs that held the markers.
fn b_before_holds_markers(p: &Project) -> bool {
    let v = p.json(&["log", "--all", "--format", "json"]);
    let seals = v
        .as_array()
        .or_else(|| v["seals"].as_array())
        .unwrap()
        .clone();
    let b = seals
        .iter()
        .find(|s| s["spec_id"] == "sb")
        .unwrap_or_else(|| panic!("no seal for sb: {v}"));
    let change = b["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["path"] == "lib.rs")
        .unwrap();
    let old = change["old_hash"].as_str().unwrap();
    let blob = p.root.join(".writ/objects").join(&old[..2]).join(&old[2..]);
    assert!(blob.exists(), "b's before blob missing: {old}");
    let a_seal = seals.iter().find(|s| s["spec_id"] == "sa").unwrap();
    let a_new = a_seal["changes"][0]["new_hash"].as_str().unwrap();
    old == a_new
}

fn marker_count(text: &str) -> usize {
    text.matches(IGNORE_MARKER).count()
}

#[test]
fn f62_informed_removal_of_markers_reaches_the_commit() {
    let (_p, committed) = markers_fixture("f62-informed", true);
    assert_eq!(
        marker_count(&committed),
        0,
        "informed removal undone by merge:\n{committed}"
    );
    assert!(
        committed.contains(B_ADDITION),
        "b's addition lost:\n{committed}"
    );
}

#[test]
fn f62_concurrent_spec_never_deletes_markers_it_never_saw() {
    let (_p, committed) = markers_fixture("f62-concurrent", false);
    assert_eq!(
        marker_count(&committed),
        5,
        "concurrent merge dropped a's markers:\n{committed}"
    );
    assert!(
        committed.contains(B_ADDITION),
        "b's addition lost:\n{committed}"
    );
}

// ---------------------------------------------------------------------------
// Aubs's hole in option c: B's version retains some of A's additions (so the
// retains-any test calls it informed) but was copied between A's two seals,
// so it never saw A's second-seal lines. B's one deliberate deletion of an
// A first-seal line must stand; A's second-seal lines must survive; and the
// notice naming them must reach both the finish output and B's next context.
// ---------------------------------------------------------------------------

/// Context key the stale-rewrite notice is expected under. Not final until
/// Haris seals; a rename is a one-line change here.
const STALE_NOTICE_KEY: &str = "stale_rewrite_notices";

/// Base lines with `inserts` (after-base-line-index, text) inserted.
fn base_with(inserts: &[(usize, &str)]) -> String {
    let mut out = String::new();
    for i in 0..BASE_LINES {
        out.push_str(&format!("line {i}\n"));
        for (after, text) in inserts {
            if *after == i {
                out.push_str(text);
                out.push('\n');
            }
        }
    }
    out
}

const A1_KEEP: &str = "a1 kept by b";
const A1_DELETED: &str = "a1 deleted by b";
const A2_FIRST: &str = "a2 first, b never saw it";
const A2_SECOND: &str = "a2 second, b never saw it";
const B_OWN: &str = "b own line";

#[test]
fn b_copied_between_a_seals_keeps_a_second_seal_lines_and_its_own_deletion() {
    let p = Project::new("between-seals");
    for (agent, spec) in [("a", "sa"), ("b", "sb")] {
        p.ok(agent, &["spec", "add", "--id", spec, "--title", spec]);
        p.ok(agent, &["spec", "claim", spec, "--agent", agent]);
    }
    let a1 = [(5, A1_DELETED), (10, A1_KEEP)];
    let a2 = [
        (5, A1_DELETED),
        (10, A1_KEEP),
        (20, A2_FIRST),
        (25, A2_SECOND),
    ];

    p.write("shared.txt", &base_with(&a1));
    p.ok(
        "a",
        &[
            "seal",
            "-s",
            "a1",
            "--agent",
            "a",
            "--spec",
            "sa",
            "--paths",
            "shared.txt",
        ],
    );
    let b_copy = base_with(&a1); // b's working copy, taken here
    p.write("shared.txt", &base_with(&a2));
    p.ok(
        "a",
        &[
            "seal",
            "-s",
            "a2",
            "--agent",
            "a",
            "--spec",
            "sa",
            "--paths",
            "shared.txt",
        ],
    );

    // b writes its copy back: deletes A1_DELETED, adds its own line.
    let b_version = b_copy
        .replace(&format!("{A1_DELETED}\n"), "")
        .replace("line 35\n", &format!("line 35\n{B_OWN}\n"));
    p.write("shared.txt", &b_version);
    p.ok(
        "b",
        &[
            "seal",
            "-s",
            "b",
            "--agent",
            "b",
            "--spec",
            "sb",
            "--paths",
            "shared.txt",
        ],
    );
    p.ok("a", &["spec", "done", "sa", "--agent", "a", "-s", "done"]);
    p.ok("b", &["spec", "done", "sb", "--agent", "b", "-s", "done"]);

    let fin = raw(&p, "human", &["finish", "-y"]);
    let fin_text = combined(&fin);
    assert!(fin.status.success(), "finish:\n{fin_text}");

    let committed = p.git(&["show", "HEAD:shared.txt"]);
    let expected = base_with(&[(10, A1_KEEP), (20, A2_FIRST), (25, A2_SECOND), (35, B_OWN)]);
    assert_eq!(committed, expected, "finish output:\n{fin_text}");
    for line in [A2_FIRST, A2_SECOND] {
        assert!(
            fin_text.contains(line),
            "finish notice does not name {line:?}:\n{fin_text}"
        );
    }

    let ctx_text = p.ok("b", &["context", "--format", "json", "--for-agent", "b"]);
    let ctx: Value =
        serde_json::from_str(&ctx_text).unwrap_or_else(|e| panic!("context json: {e}\n{ctx_text}"));
    let notice = ctx
        .get(STALE_NOTICE_KEY)
        .unwrap_or_else(|| panic!("b's context has no {STALE_NOTICE_KEY}: {ctx}"))
        .to_string();
    for line in [A2_FIRST, A2_SECOND] {
        assert!(
            notice.contains(line),
            "{STALE_NOTICE_KEY} does not name {line:?}: {notice}"
        );
    }
    assert!(
        !notice.contains(A1_DELETED),
        "b's deliberate deletion reported as stale: {notice}"
    );
}
