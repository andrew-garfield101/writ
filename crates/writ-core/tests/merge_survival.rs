//! merge-survival-check (findings 42 and 45), on temp repositories.
//!
//! Seal-time own-line check (finding 42): a seal that would revert lines an
//! earlier seal of the same spec added is refused with
//! `WritError::OwnLinesRemoved` naming the spec, path and lines, unless the
//! caller allows it (`--force`). Merge-path check (finding 45): every line a
//! spec added relative to the merge base survives a seal-tree merge, or the
//! merge escalates and nothing is staged.

use std::fs;
use std::path::Path;

use tempfile::{tempdir, TempDir};

use writ_core::convergence::survival::{LossKind, MERGE_LOSS_CLASS};
use writ_core::convergence::ConvergeStrategy;
use writ_core::error::WritError;
use writ_core::seal::{AgentIdentity, AgentType, Seal, TaskStatus, Verification};
use writ_core::spec::Spec;
use writ_core::Repository;

const FILE: &str = "shared.txt";

fn base_text() -> String {
    (0..40).map(|i| format!("line {i}\n")).collect()
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

struct Fixture {
    dir: TempDir,
    repo: Repository,
}

impl Fixture {
    /// A repo whose base seal holds `shared.txt` = 40 numbered lines, and
    /// specs `sa` and `sb` created after it (so both share that genesis).
    fn new() -> Self {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let f = Self { dir, repo };
        f.write(&base_text());
        f.repo
            .add_spec(&Spec::new("base".into(), "base".into(), String::new()))
            .unwrap();
        f.seal("setup", "base").unwrap();
        for id in ["sa", "sb"] {
            f.repo
                .add_spec(&Spec::new(id.into(), id.into(), String::new()))
                .unwrap();
        }
        f
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, content: &str) {
        fs::write(self.root().join(FILE), content).unwrap();
    }

    fn seal(&self, agent: &str, spec: &str) -> Result<Seal, WritError> {
        self.repo.seal_paths(
            AgentIdentity {
                id: agent.to_string(),
                agent_type: AgentType::Agent,
            },
            format!("{agent} work"),
            Some(spec.to_string()),
            TaskStatus::InProgress,
            Verification::default(),
            &[FILE.to_string()],
            false,
        )
    }
}

// ── Seal-time own-line check ───────────────────────────────────────────

/// Finding 42 as it happened (variant B): b's version of the file is on disk,
/// unsealed, when a seals the file again. The seal would record the removal
/// of a's own "line 2 A". Refused, nothing sealed.
#[test]
fn seal_capturing_another_agents_version_is_refused() {
    let mut f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&base, 2, "A"));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&base, 30, "B"));
    let before = f.repo.load_spec("sa").unwrap().sealed_by.len();

    let err = f.seal("a", "sa").unwrap_err();

    let WritError::OwnLinesRemoved { spec_id, losses } = &err else {
        panic!("expected OwnLinesRemoved, got {err:?}");
    };
    assert_eq!(spec_id, "sa");
    assert_eq!(losses.len(), 1);
    let l = &losses[0];
    assert_eq!(l.kind, LossKind::OwnLines);
    assert_eq!((l.spec.as_str(), l.path.as_str()), ("sa", FILE));
    assert_eq!(l.ranges, vec![(3, 3)]);
    assert_eq!(l.lines, vec!["line 2 A".to_string()]);
    let msg = err.to_string();
    assert!(
        msg.contains("shared.txt") && msg.contains("line 2 A"),
        "{msg}"
    );
    assert_eq!(
        f.repo.load_spec("sa").unwrap().sealed_by.len(),
        before,
        "a refused seal must not be recorded"
    );

    // --force records the removal on purpose; after that it is the spec's
    // own latest version and later seals are not refused for it.
    f.repo.set_allow_own_line_removal(true);
    f.seal("a", "sa").unwrap();
    f.repo.set_allow_own_line_removal(false);
    f.write(&with_edit(&with_edit(&base, 30, "B"), 35, "more"));
    f.seal("a", "sa").unwrap();
}

/// Variant A (b seals first) on the current build: a's later seal of the
/// file never captures b's version, both edits merge.
#[test]
fn variant_a_passes_cleanly() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&base, 2, "A"));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&with_edit(&base, 2, "A"), 30, "B"));
    f.seal("b", "sb").unwrap();
    f.repo.mark_spec_done("sa", None).unwrap();
    f.repo.mark_spec_done("sb", None).unwrap();

    let report = f.repo.finalize_convergence().unwrap();
    assert!(report.is_clean, "{:?}", report.escalations);
}

#[test]
fn editing_own_line_is_not_refused() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&base, 2, "A"));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&base, 2, "A2"));
    f.seal("a", "sa").unwrap();
}

#[test]
fn other_specs_lines_are_not_this_checks_concern() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&base, 2, "A"));
    f.seal("a", "sa").unwrap();
    // b reverts a's line: b's own chain never added it.
    f.write(&with_edit(&base, 30, "B"));
    f.seal("b", "sb").unwrap();
}

// ── Finding 53: moved content is not a loss ────────────────────────────

fn lines_of(items: &[&str]) -> String {
    items.iter().map(|l| format!("{l}\n")).collect()
}

fn seal_files(f: &Fixture, agent: &str, spec: &str, paths: &[&str]) -> Result<Seal, WritError> {
    f.repo.seal_paths(
        AgentIdentity {
            id: agent.to_string(),
            agent_type: AgentType::Agent,
        },
        format!("{agent} work"),
        Some(spec.to_string()),
        TaskStatus::InProgress,
        Verification::default(),
        &paths.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
        false,
    )
}

/// Bri's gate case: a block the spec added, moved within the file.
#[test]
fn moved_block_within_file_is_not_refused() {
    let f = Fixture::new();
    let base = base_text();
    let mut v: Vec<String> = base.lines().map(String::from).collect();
    v.splice(5..5, ["fn added() {", "    work();", "}"].map(String::from));
    f.write(&lines_of(&v.iter().map(String::as_str).collect::<Vec<_>>()));
    f.seal("a", "sa").unwrap();

    let block: Vec<String> = v.drain(5..8).collect();
    v.splice(30..30, block);
    f.write(&lines_of(&v.iter().map(String::as_str).collect::<Vec<_>>()));
    f.seal("a", "sa").unwrap();
}

/// The file is renamed and the spec's block moved inside it in the same
/// seal: the old path is a deletion, the block's content is in the new path.
#[test]
fn renamed_file_with_moved_block_is_not_refused() {
    let f = Fixture::new();
    let root = f.root().to_path_buf();
    fs::write(root.join("old.rs"), lines_of(&["a", "b", "c", "d"])).unwrap();
    seal_files(&f, "a", "sa", &["old.rs"]).unwrap();
    fs::write(
        root.join("old.rs"),
        lines_of(&["a", "NEW1", "NEW2", "b", "c", "d"]),
    )
    .unwrap();
    seal_files(&f, "a", "sa", &["old.rs"]).unwrap();

    fs::remove_file(root.join("old.rs")).unwrap();
    fs::write(
        root.join("new.rs"),
        lines_of(&["a", "b", "c", "NEW1", "NEW2", "d"]),
    )
    .unwrap();
    seal_files(&f, "a", "sa", &["old.rs", "new.rs"]).unwrap();
}

/// The block moves to another file the same seal records while the old file
/// stays and also holds foreign content: not refused. Leaving the new file
/// out of the seal makes it a loss.
#[test]
fn block_moved_to_another_sealed_file_is_not_refused() {
    let f = Fixture::new();
    let root = f.root().to_path_buf();
    fs::write(root.join("x.rs"), lines_of(&["a", "b", "c", "d"])).unwrap();
    seal_files(&f, "a", "sa", &["x.rs"]).unwrap();
    fs::write(
        root.join("x.rs"),
        lines_of(&["a", "NEW1", "NEW2", "b", "c", "d"]),
    )
    .unwrap();
    seal_files(&f, "a", "sa", &["x.rs"]).unwrap();

    fs::write(root.join("x.rs"), lines_of(&["a", "b", "c", "d", "THEIRS"])).unwrap();
    fs::write(root.join("y.rs"), lines_of(&["NEW1", "NEW2"])).unwrap();
    let err = seal_files(&f, "a", "sa", &["x.rs"]).unwrap_err();
    assert!(matches!(err, WritError::OwnLinesRemoved { .. }), "{err:?}");
    seal_files(&f, "a", "sa", &["x.rs", "y.rs"]).unwrap();
}

// ── CC decision 2: pure removals refused only with foreign content ─────

/// The spec removes a line it added and nothing else changes: editing its
/// own work, sealed without --force.
#[test]
fn pure_removal_without_foreign_content_passes() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&base, 2, "debug"));
    f.seal("a", "sa").unwrap();
    f.write(&base);
    f.seal("a", "sa").unwrap();
}

/// The same removal while the file also holds a line that is neither in the
/// base nor in any of this spec's seals (another agent's): refused, then
/// sealed with --force.
#[test]
fn pure_removal_with_foreign_content_is_refused() {
    let mut f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&base, 2, "debug"));
    f.seal("a", "sa").unwrap();
    f.write(&format!("{base}their new line\n"));
    let err = f.seal("a", "sa").unwrap_err();
    let WritError::OwnLinesRemoved { losses, .. } = &err else {
        panic!("expected OwnLinesRemoved, got {err:?}");
    };
    assert_eq!(losses[0].lines, vec!["line 2 debug".to_string()]);
    f.repo.set_allow_own_line_removal(true);
    f.seal("a", "sa").unwrap();
}

// ── Merge-path survival (finding 45) ───────────────────────────────────

fn ids(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

/// Disjoint edits on both merge paths: clean, both lines in the output.
#[test]
fn disjoint_merge_passes_on_both_paths() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&base, 2, "A"));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&with_edit(&base, 2, "A"), 30, "B"));
    f.seal("b", "sb").unwrap();

    let r = f
        .repo
        .converge_from_seal_trees(&ids(&["sa", "sb"]), ConvergeStrategy::Escalate)
        .unwrap();
    assert!(r.is_clean, "{:?}", r.escalations);
    assert_eq!(r.shadow_results.len(), 1);

    let all = f
        .repo
        .converge_all(ConvergeStrategy::Escalate, false)
        .unwrap();
    assert!(
        all.escalations
            .iter()
            .all(|e| e.conflict_class != MERGE_LOSS_CLASS),
        "{:?}",
        all.escalations
    );
}

/// Same-line edits: the merge reports a conflict; the check accepts the
/// covered range and adds no survival escalation of its own.
#[test]
fn overlapping_merge_reports_conflict_and_check_accepts_covered_range() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&base, 4, "A"));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&base, 4, "B"));
    f.seal("b", "sb").unwrap();

    let r = f
        .repo
        .converge_from_seal_trees(&ids(&["sa", "sb"]), ConvergeStrategy::Manual)
        .unwrap();
    assert!(!r.is_clean);
    assert_eq!(r.escalations.len(), 1, "{:?}", r.escalations);
    assert_eq!(r.escalations[0].conflict_class, "seal_tree_conflict");

    let all = f
        .repo
        .converge_all(ConvergeStrategy::Manual, false)
        .unwrap();
    // The pipeline path reports the conflict as a resolution (v1 fallback
    // concatenates) rather than an escalation; the region is covered.
    assert!(all.total_conflicts > 0, "{:?}", all.merges);
    assert!(all.merges.iter().any(|m| !m.resolutions.is_empty()));
    assert!(
        all.escalations
            .iter()
            .all(|e| e.conflict_class != MERGE_LOSS_CLASS),
        "{:?}",
        all.escalations
    );
}

/// Finding 42, third form: b seals a version without a's line, then a seals
/// another file, so a's head tree snapshot holds b's version. Finish's merge
/// reads each spec's own last sealed version of the file, not the head
/// snapshot, and merges both edits.
#[test]
fn third_form_merges_both_edits() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&base, 2, "A"));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&base, 30, "B"));
    f.seal("b", "sb").unwrap();
    fs::write(f.root().join("other.txt"), "a's other file\n").unwrap();
    seal_files(&f, "a", "sa", &["other.txt"]).unwrap();
    f.repo.mark_spec_done("sa", None).unwrap();
    f.repo.mark_spec_done("sb", None).unwrap();

    let r = f.repo.finalize_convergence().unwrap();

    assert!(r.is_clean, "{:?}", r.escalations);
    let (_, hash) = r
        .shadow_results
        .iter()
        .find(|(p, _)| p == FILE)
        .expect("shared.txt not merged");
    let merged = String::from_utf8(f.repo.object_content(hash).unwrap()).unwrap();
    assert_eq!(merged, with_edit(&with_edit(&base, 2, "A"), 30, "B"));
}

/// A merge that drops a hunk without reporting it is escalated, names the
/// spec, path and line, and stores nothing: Escalate's "longer version"
/// pick on a same-line conflict (finding 54) kept sa's longer line and
/// dropped sb's rewrite of it without a report.
#[test]
fn silently_dropped_hunk_is_escalated_and_nothing_is_staged() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&base, 4, "A, which is longer"));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&base, 4, "B"));
    f.seal("b", "sb").unwrap();
    f.repo.mark_spec_done("sa", None).unwrap();
    f.repo.mark_spec_done("sb", None).unwrap();

    let r = f.repo.finalize_convergence().unwrap();

    assert!(!r.is_clean);
    let esc: Vec<_> = r
        .escalations
        .iter()
        .filter(|e| e.conflict_class == MERGE_LOSS_CLASS)
        .collect();
    assert_eq!(esc.len(), 1, "{:?}", r.escalations);
    assert_eq!(esc[0].right_spec, "sb");
    assert_eq!(esc[0].file_path, FILE);
    assert!(esc[0].reason.contains("line 5"), "{}", esc[0].reason);
    assert!(
        r.shadow_results.is_empty(),
        "a lossy merge must not be staged"
    );
    assert!(!f
        .root()
        .join(".writ")
        .join(writ_core::gc::PENDING_CONVERGENCE_FILE)
        .exists());
}

// ── Finding 56: foreign lines shown, sealed-by-others not foreign, scoped override ──

/// Bri's case: she removes her own line in a busy shared file while another
/// agent's newly SEALED line sits beside it. Those lines are not foreign
/// (already sealed), so her intentional removal passes.
#[test]
fn own_removal_beside_another_agents_sealed_lines_passes() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&base, 2, "mine"));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&with_edit(&base, 2, "mine"), 30, "theirs"));
    f.seal("b", "sb").unwrap();
    f.write(&with_edit(&base, 30, "theirs"));
    f.seal("a", "sa").unwrap();
}

/// A refusal lists the foreign lines that triggered it.
#[test]
fn refusal_names_the_foreign_lines() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&base, 2, "A"));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&base, 30, "B"));
    let err = f.seal("a", "sa").unwrap_err();
    let WritError::OwnLinesRemoved { losses, .. } = &err else {
        panic!("expected OwnLinesRemoved, got {err:?}");
    };
    assert_eq!(losses[0].foreign, vec!["line 30 B".to_string()]);
    let msg = err.to_string();
    assert!(msg.contains("+ line 30 B"), "{msg}");
}

/// `--allow-removals <path>` lifts the check for that file only.
#[test]
fn allow_removals_is_scoped_to_the_named_file() {
    let mut f = Fixture::new();
    let root = f.root().to_path_buf();
    let base4 = lines_of(&["a", "b", "c", "d"]);
    for p in ["x.rs", "y.rs"] {
        fs::write(root.join(p), &base4).unwrap();
    }
    seal_files(&f, "a", "sa", &["x.rs", "y.rs"]).unwrap();
    for p in ["x.rs", "y.rs"] {
        fs::write(root.join(p), lines_of(&["a", "MINE", "b", "c", "d"])).unwrap();
    }
    seal_files(&f, "a", "sa", &["x.rs", "y.rs"]).unwrap();
    // Both files lose MINE and gain an unsealed foreign line.
    for p in ["x.rs", "y.rs"] {
        fs::write(root.join(p), lines_of(&["a", "b", "c", "d", "OTHER"])).unwrap();
    }

    // Allowing x.rs only: y.rs is still refused, and only y.rs is named.
    f.repo.set_allow_removal_paths(["x.rs".to_string()]);
    let err = seal_files(&f, "a", "sa", &["x.rs", "y.rs"]).unwrap_err();
    let WritError::OwnLinesRemoved { losses, .. } = &err else {
        panic!("expected OwnLinesRemoved, got {err:?}");
    };
    let paths: Vec<&str> = losses.iter().map(|l| l.path.as_str()).collect();
    assert_eq!(paths, vec!["y.rs"]);

    // x.rs alone seals; then both named.
    seal_files(&f, "a", "sa", &["x.rs"]).unwrap();
    f.repo
        .set_allow_removal_paths(["x.rs".to_string(), "./y.rs".to_string()]);
    seal_files(&f, "a", "sa", &["y.rs"]).unwrap();

    // The override does not leak to a later file once cleared.
    f.repo.set_allow_removal_paths(Vec::new());
    fs::write(root.join("z.rs"), &base4).unwrap();
    seal_files(&f, "a", "sa", &["z.rs"]).unwrap();
    fs::write(root.join("z.rs"), lines_of(&["a", "Z", "b", "c", "d"])).unwrap();
    seal_files(&f, "a", "sa", &["z.rs"]).unwrap();
    fs::write(root.join("z.rs"), lines_of(&["a", "b", "c", "d", "OTHER2"])).unwrap();
    assert!(seal_files(&f, "a", "sa", &["z.rs"]).is_err());
}

/// Informed rewrite (CC's rule): sb's seal recorded a base holding sa's
/// line and replaced it, so sb's version survives and nothing escalates.
#[test]
fn informed_rewrite_of_another_specs_line_merges_to_the_rewrite() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&base, 4, "v1 from a"));
    f.seal("a", "sa").unwrap();
    // On disk sb sees sa's line (the shared index held it) and rewrites it.
    f.write(&with_edit(&base, 4, "v2 from b"));
    f.seal("b", "sb").unwrap();
    f.repo.mark_spec_done("sa", None).unwrap();
    f.repo.mark_spec_done("sb", None).unwrap();

    let r = f.repo.finalize_convergence().unwrap();
    assert!(r.is_clean, "{:?}", r.escalations);
    let (_, hash) = r.shadow_results.iter().find(|(p, _)| p == FILE).unwrap();
    let merged = String::from_utf8(f.repo.object_content(hash).unwrap()).unwrap();
    assert_eq!(merged, with_edit(&base, 4, "v2 from b"));
}

/// A spec adds lines, then rewrites its own lines in a later seal, then
/// merges: only its net additions (its latest sealed version) are checked.
#[test]
fn own_rewrite_across_two_seals_then_merge_is_clean() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&with_edit(&base, 2, "first try"), 3, "helper"));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&base, 2, "second try"));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&with_edit(&base, 2, "second try"), 30, "B"));
    f.seal("b", "sb").unwrap();
    f.repo.mark_spec_done("sa", None).unwrap();
    f.repo.mark_spec_done("sb", None).unwrap();

    let r = f.repo.finalize_convergence().unwrap();
    assert!(r.is_clean, "{:?}", r.escalations);
    let (_, hash) = r.shadow_results.iter().find(|(p, _)| p == FILE).unwrap();
    let merged = String::from_utf8(f.repo.object_content(hash).unwrap()).unwrap();
    assert_eq!(
        merged,
        with_edit(&with_edit(&base, 2, "second try"), 30, "B")
    );
}

/// The refused finish's shape: sa's line was removed by an informed seal of
/// a spec that is not part of the merge (sx, still open, or already
/// committed). sb then works on top of sx's version. The merge drops sa's
/// line, and the removal is excused by sx's seal, not escalated.
#[test]
fn removal_by_a_spec_outside_the_merge_is_informed() {
    let f = Fixture::new();
    f.repo
        .add_spec(&Spec::new("sx".into(), "sx".into(), String::new()))
        .unwrap();
    let base = base_text();
    f.write(&with_edit(&base, 4, "A"));
    f.seal("a", "sa").unwrap();
    // sx sees sa's line (its seal's base holds it) and replaces it.
    f.write(&with_edit(&base, 4, "X"));
    f.seal("x", "sx").unwrap();
    // sb builds on sx's version.
    f.write(&with_edit(&with_edit(&base, 4, "X"), 30, "B"));
    f.seal("b", "sb").unwrap();
    f.repo.mark_spec_done("sa", None).unwrap();
    f.repo.mark_spec_done("sb", None).unwrap();

    let r = f.repo.finalize_convergence().unwrap();
    assert!(
        r.escalations
            .iter()
            .all(|e| e.conflict_class != MERGE_LOSS_CLASS),
        "{:?}",
        r.escalations
    );
}

/// The only seal that removed the line's text came BEFORE sa added it: it
/// cannot have been informed by sa's addition, so the dropped line still
/// escalates (without the timing condition it would be excused).
#[test]
fn earlier_removal_does_not_excuse_a_later_addition() {
    let f = Fixture::new();
    f.repo
        .add_spec(&Spec::new("sx".into(), "sx".into(), String::new()))
        .unwrap();
    let base = base_text();
    // sx adds and then removes "line 4 A" in its own chain.
    f.write(&with_edit(&base, 4, "A"));
    f.seal("x", "sx").unwrap();
    f.write(&base);
    f.seal("x", "sx").unwrap();
    // sb rewrites line 4 (longer, so Escalate's pick keeps it).
    f.write(&with_edit(&base, 4, "Y, the longer line"));
    f.seal("b", "sb").unwrap();
    // sa adds the same text sx once removed.
    f.write(&with_edit(&base, 4, "A"));
    f.seal("a", "sa").unwrap();
    f.repo.mark_spec_done("sa", None).unwrap();
    f.repo.mark_spec_done("sb", None).unwrap();

    let r = f.repo.finalize_convergence().unwrap();
    let esc: Vec<_> = r
        .escalations
        .iter()
        .filter(|e| e.conflict_class == MERGE_LOSS_CLASS)
        .collect();
    assert_eq!(esc.len(), 1, "{:?}", r.escalations);
    assert_eq!(esc[0].right_spec, "sa");
    assert!(r.shadow_results.is_empty());
}

// ── Finding 62: descent, not resurrection ─────────────────────────────

/// A's markers plus a helper line, so an informed B can keep some of A
/// (CC's option c: a version that keeps none of A's lines is a stale
/// rewrite, not an informed removal).
fn markers(base: &str) -> String {
    let mut out = String::from("helper from a\n");
    for (i, l) in base.lines().enumerate() {
        if i % 4 == 0 && i < 20 {
            out.push_str("#[ignore = \"not landed\"]\n");
        }
        out.push_str(l);
        out.push('\n');
    }
    out
}

fn merged_file(f: &Fixture, r: &writ_core::repo::SealTreeConvergenceReport) -> String {
    let (_, hash) = r
        .shadow_results
        .iter()
        .find(|(p, _)| p == FILE)
        .expect("shared.txt not merged");
    String::from_utf8(f.repo.object_content(hash).unwrap()).unwrap()
}

/// A adds five markers and a helper; B, sealed after, keeps the helper and
/// removes the markers. The finish keeps them removed (the isolation.rs
/// shape) and raises no notice.
#[test]
fn descendant_removal_stays_removed() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&markers(&base));
    f.seal("a", "sa").unwrap();
    let b = format!("helper from a\n{}", with_edit(&base, 30, "B"));
    f.write(&b);
    f.seal("b", "sb").unwrap();
    f.repo.mark_spec_done("sa", None).unwrap();
    f.repo.mark_spec_done("sb", None).unwrap();

    let r = f.repo.finalize_convergence().unwrap();
    assert!(r.is_clean, "{:?}", r.escalations);
    let merged = merged_file(&f, &r);
    assert_eq!(merged.matches("#[ignore").count(), 0, "{merged}");
    assert_eq!(merged, b);
    assert!(r.notices.is_empty(), "{:?}", r.notices);
}

/// Option c: B's version kept none of A's lines (a stale whole-file
/// rewrite). Both are kept, and a notice names A's lines, the adding
/// agent, and the command for B to remove them on purpose; it also shows
/// in B's context under `stale_rewrite_notices`.
#[test]
fn stale_rewrite_keeps_both_with_a_notice() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&markers(&base));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&base, 30, "B"));
    f.seal("b", "sb").unwrap();
    f.repo.mark_spec_done("sa", None).unwrap();
    f.repo.mark_spec_done("sb", None).unwrap();

    let r = f.repo.finalize_convergence().unwrap();
    assert!(r.is_clean, "{:?}", r.escalations);
    assert_eq!(merged_file(&f, &r), markers(&with_edit(&base, 30, "B")));
    assert_eq!(r.notices.len(), 1, "{:?}", r.notices);
    let n = &r.notices[0];
    assert_eq!((n.spec.as_str(), n.added_by_spec.as_str()), ("sb", "sa"));
    assert_eq!(n.lines.len(), 6);
    assert!(
        n.command.contains("--spec sb --paths shared.txt"),
        "{}",
        n.command
    );
    let ctx = f
        .repo
        .context(
            writ_core::context::ContextScope::Agent("b".into()),
            10,
            &writ_core::context::ContextFilter::default(),
        )
        .unwrap();
    assert_eq!(ctx.stale_rewrite_notices.len(), 1);
}

/// The converse: B adds its own lines on top of A's (A's lines in B's
/// before and kept): both survive.
#[test]
fn descendant_that_keeps_ancestor_lines_keeps_both() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&markers(&base));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&markers(&base), 30, "B"));
    f.seal("b", "sb").unwrap();
    f.repo.mark_spec_done("sa", None).unwrap();
    f.repo.mark_spec_done("sb", None).unwrap();

    let r = f.repo.finalize_convergence().unwrap();
    assert!(r.is_clean, "{:?}", r.escalations);
    assert_eq!(merged_file(&f, &r), with_edit(&markers(&base), 30, "B"));
}

/// Neither descends: B's seal started from a version without A's edits
/// (an out-of-merge spec reset the file between them). A three-way merge
/// from the base keeps both specs' additions, as before.
#[test]
fn concurrent_versions_still_merge_both() {
    let f = Fixture::new();
    f.repo
        .add_spec(&Spec::new("sx".into(), "sx".into(), String::new()))
        .unwrap();
    let base = base_text();
    f.write(&with_edit(&base, 2, "A"));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&base, 10, "X"));
    f.seal("x", "sx").unwrap();
    f.write(&with_edit(&base, 30, "B"));
    f.seal("b", "sb").unwrap();
    f.repo.mark_spec_done("sa", None).unwrap();
    f.repo.mark_spec_done("sb", None).unwrap();

    let r = f.repo.finalize_convergence().unwrap();
    assert!(r.is_clean, "{:?}", r.escalations);
    assert_eq!(
        merged_file(&f, &r),
        with_edit(&with_edit(&base, 2, "A"), 30, "B")
    );
}

/// Three specs in a chain: A adds markers, B continues A and removes them
/// while editing line 30, C continues B and edits line 35. The finish
/// takes C's version: no markers, both later edits.
#[test]
fn three_spec_chain_takes_the_last_descendant() {
    let f = Fixture::new();
    f.repo
        .add_spec(&Spec::new("sc".into(), "sc".into(), String::new()))
        .unwrap();
    let base = base_text();
    f.write(&markers(&base));
    f.seal("a", "sa").unwrap();
    let b = format!("helper from a\n{}", with_edit(&base, 30, "B"));
    f.write(&b);
    f.seal("b", "sb").unwrap();
    let c = format!(
        "helper from a\n{}",
        with_edit(&with_edit(&base, 30, "B"), 35, "C")
    );
    f.write(&c);
    f.seal("c", "sc").unwrap();
    for s in ["sa", "sb", "sc"] {
        f.repo.mark_spec_done(s, None).unwrap();
    }

    let r = f.repo.finalize_convergence().unwrap();
    assert!(r.is_clean, "{:?}", r.escalations);
    assert_eq!(merged_file(&f, &r), c);
}
