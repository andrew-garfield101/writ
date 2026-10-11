//! `survival::audit` (0.4.1 survival tier groundwork): per-spec added-line
//! survival against the working tree or git HEAD, the core of
//! `bench/finish_audit.py` in the store. Fixtures follow merge_survival.rs:
//! `shared.txt` = 40 numbered lines, specs `sa` and `sb` after the base seal.

use std::fs;
use std::path::Path;

use tempfile::{tempdir, TempDir};

use writ_core::convergence::survival::{
    accept_removals, audit, findings, specs_since, Against, FileAuditStatus, SupersededBy,
};
use writ_core::error::WritError;
use writ_core::seal::{AgentIdentity, AgentType, Seal, TaskStatus, Verification};
use writ_core::spec::{Spec, SpecStatus, SpecUpdate};
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

fn with_markers(base: &str) -> String {
    let mut out = String::new();
    for (i, l) in base.lines().enumerate() {
        if i % 4 == 0 && i < 20 {
            out.push_str("#[ignore = \"not landed\"]\n");
        }
        out.push_str(l);
        out.push('\n');
    }
    out
}

struct Fixture {
    dir: TempDir,
    repo: Repository,
}

impl Fixture {
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

/// Mark a spec committed, as `writ finish` does.
fn commit_spec(repo: &Repository, id: &str) {
    repo.update_spec(
        id,
        SpecUpdate {
            status: Some(SpecStatus::Complete),
            ..SpecUpdate::default()
        },
    )
    .unwrap();
    repo.mark_spec_committed(id, "0000000").unwrap();
}

fn ids(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[test]
fn every_sealed_addition_on_disk_is_green() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&base, 2, "A"));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&with_edit(&base, 2, "A"), 30, "B"));
    f.seal("b", "sb").unwrap();

    let r = audit(&f.repo, &ids(&["sa", "sb"]), Against::WorkingTree).unwrap();
    assert!(r.is_green(), "{r:?}");
    assert_eq!((r.checked, r.lost, r.superseded), (2, 0, 0));
    assert_eq!(r.specs.len(), 2);
    assert_eq!(r.specs[0].files[0].status, FileAuditStatus::Checked);
    assert_eq!(r.against, Against::WorkingTree);
    // The same through the repository.
    let again = f
        .repo
        .survival_audit(&ids(&["sa"]), Against::WorkingTree)
        .unwrap();
    assert_eq!(again.checked, 1);
}

/// Finding 42's loss shape: a's sealed line is not on disk any more and no
/// seal removed it. Lost, named by spec, path, line and text.
#[test]
fn a_sealed_line_missing_from_disk_without_a_removing_seal_is_lost() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&with_edit(&base, 2, "A"), 3, "A"));
    let a = f.seal("a", "sa").unwrap();
    f.write(&with_edit(&with_edit(&base, 3, "A"), 30, "B"));
    f.seal("b", "sb").unwrap();
    // b's version kept a's "line 3 A" (it saw a's seal) and lacks "line 2
    // A": an informed removal, superseded, not lost (finding 105).
    let r = audit(&f.repo, &ids(&["sa"]), Against::WorkingTree).unwrap();
    assert_eq!((r.lost, r.superseded), (0, 1), "{r:?}");
    let s = &r.specs[0].files[0].superseded[0];
    assert_eq!((s.line, s.text.as_str()), (3, "line 2 A"));
    assert_eq!(s.by_spec.as_deref(), Some("sb"));

    // An unsealed hand edit that drops b's line has no seal behind it: lost.
    // Relative to its own base (a's version) sb added two lines, the
    // restored "line 2" (text the file held before: not distinctive) and
    // "line 30 B", which is missing.
    f.write(&base);
    let r = audit(&f.repo, &ids(&["sb"]), Against::WorkingTree).unwrap();
    assert!(!r.is_green());
    assert_eq!((r.checked, r.skipped, r.lost), (1, 1, 1));
    let file = &r.specs[0].files[0];
    assert_eq!(file.path, FILE);
    assert_eq!(file.lost[0].line, 31);
    assert_eq!(file.lost[0].text, "line 30 B");
    assert_ne!(file.seal_id, a.id);
}

/// Bri's marker fixture: a adds five markers (and edits "line 39"), b
/// continues a (its version keeps "line 39 A": it saw a's seal) and
/// removes the markers. Every marker is superseded by b's seal.
#[test]
fn informed_removal_by_a_later_seal_supersedes_every_copy() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&with_markers(&base), 44, "A"));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&with_edit(&base, 39, "A"), 30, "B"));
    let b = f.seal("b", "sb").unwrap();

    let r = audit(&f.repo, &ids(&["sa", "sb"]), Against::WorkingTree).unwrap();
    assert!(r.is_green(), "{r:?}");
    let sa = &r.specs[0];
    assert_eq!((sa.checked, sa.lost, sa.superseded), (6, 0, 5));
    assert!(sa.files[0].superseded.iter().all(|s| s.by_seal == b.id));
}

/// Finding 53 in the audit: a moved block survives by content.
#[test]
fn moved_block_survives_by_content() {
    let f = Fixture::new();
    let base = base_text();
    let mut v: Vec<String> = base.lines().map(String::from).collect();
    v.splice(5..5, ["fn added() {", "    work();", "}"].map(String::from));
    let text = |v: &[String]| v.iter().map(|l| format!("{l}\n")).collect::<String>();
    f.write(&text(&v));
    f.seal("a", "sa").unwrap();
    let block: Vec<String> = v.drain(5..8).collect();
    v.splice(30..30, block);
    f.write(&text(&v));

    let r = audit(&f.repo, &ids(&["sa"]), Against::WorkingTree).unwrap();
    assert_eq!((r.checked, r.lost), (3, 0), "{r:?}");
}

/// A deletion the spec sealed must hold: the file back on disk is a loss
/// of the deletion; absent, it is green.
#[test]
fn sealed_deletion_is_checked_too() {
    let f = Fixture::new();
    fs::remove_file(f.root().join(FILE)).unwrap();
    f.seal("a", "sa").unwrap();
    let r = audit(&f.repo, &ids(&["sa"]), Against::WorkingTree).unwrap();
    assert_eq!(r.specs[0].files[0].status, FileAuditStatus::DeletedAsSealed);
    assert!(r.is_green());

    f.write(&base_text());
    let r = audit(&f.repo, &ids(&["sa"]), Against::WorkingTree).unwrap();
    assert_eq!(
        r.specs[0].files[0].status,
        FileAuditStatus::DeletedButPresent
    );
    assert!(!r.is_green());
}

#[test]
fn unknown_spec_is_an_error_and_no_specs_is_green() {
    let f = Fixture::new();
    assert!(audit(&f.repo, &ids(&["nope"]), Against::WorkingTree).is_err());
    let r = audit(&f.repo, &[], Against::WorkingTree).unwrap();
    assert!(r.is_green() && r.specs.is_empty());
}

/// Against HEAD: the sealed line is lost until the file is committed.
#[cfg(feature = "bridge")]
#[test]
fn against_head_reads_the_commit() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let git = git2::Repository::init(root).unwrap();
    let sig = git2::Signature::now("t", "t@t").unwrap();
    fs::write(root.join(".gitignore"), ".writ/\n").unwrap();
    fs::write(root.join(FILE), base_text()).unwrap();
    let commit = |msg: &str| {
        let mut idx = git.index().unwrap();
        idx.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        idx.write().unwrap();
        let tree = git.find_tree(idx.write_tree().unwrap()).unwrap();
        let parent = git.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        git.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parents)
            .unwrap();
    };
    commit("base");
    let repo = Repository::init(root).unwrap();
    let seal = |agent: &str, spec: &str| {
        repo.add_spec(&Spec::new(spec.into(), spec.into(), String::new()))
            .unwrap();
        repo.seal_paths(
            AgentIdentity {
                id: agent.into(),
                agent_type: AgentType::Agent,
            },
            format!("{agent} work"),
            Some(spec.into()),
            TaskStatus::InProgress,
            Verification::default(),
            &[FILE.to_string()],
            false,
        )
        .unwrap();
    };
    // A base seal first, else writ's view is that sa added all 40 lines.
    seal("setup", "base");
    fs::write(root.join(FILE), with_edit(&base_text(), 2, "A")).unwrap();
    seal("a", "sa");

    // Not committed: its work is not meant to be in HEAD yet.
    let r = audit(&repo, &ids(&["sa"]), Against::Head).unwrap();
    assert!(r.is_green() && r.specs.is_empty(), "{r:?}");
    assert_eq!(r.not_committed, ids(&["sa"]));

    // Marked committed but the commit lacks the line: lost.
    commit_spec(&repo, "sa");
    let r = audit(&repo, &ids(&["sa"]), Against::Head).unwrap();
    assert_eq!((r.checked, r.lost), (1, 1), "{r:?}");
    assert_eq!(r.specs[0].files[0].lost[0].text, "line 2 A");
    assert_eq!(r.against, Against::Head);

    commit("a");
    let r = audit(&repo, &ids(&["sa"]), Against::Head).unwrap();
    assert!(r.is_green(), "{r:?}");
}

/// Replay the own-line check on real seals, when asked:
/// `WRIT_AUDIT_REPO=<path> WRIT_REPLAY_SEALS=<id,id,..>` rebuilds, for each
/// named seal and each file it recorded, the inputs the seal-time check saw
/// (the spec's base and last version, known versions, cross-spec excuses)
/// and prints whether today's rule would refuse it. Without the variables
/// the test does nothing. Diagnostic for the hurdle ratio, not a gate.
#[test]
fn replay_own_line_check_on_real_seals_when_requested() {
    use std::collections::HashMap;
    use writ_core::convergence::survival::{
        informed_removals, own_line_loss_excused, own_versions,
    };
    use writ_core::spec::CommitState;
    let (Ok(path), Ok(list)) = (
        std::env::var("WRIT_AUDIT_REPO"),
        std::env::var("WRIT_REPLAY_SEALS"),
    ) else {
        return;
    };
    let repo = Repository::open(Path::new(&path)).unwrap();
    let text = |h: &str| -> Option<String> {
        let b = repo.object_content(h).ok()?;
        (!writ_core::diff::is_binary(&b)).then(|| String::from_utf8_lossy(&b).into_owned())
    };
    let specs = repo.list_specs().unwrap();
    for short in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let id = repo.resolve_seal_id(short).unwrap();
        let seal = repo.load_seal(&id).unwrap();
        let Some(spec_id) = seal.spec_id.clone() else {
            continue;
        };
        let mut own_seals = repo.spec_seals(&spec_id).unwrap();
        own_seals.reverse();
        let before: Vec<Seal> = own_seals
            .iter()
            .filter(|s| s.timestamp < seal.timestamp)
            .cloned()
            .collect();
        let own = own_versions(&before);
        for change in &seal.changes {
            let Some(v) = own.get(&change.path) else {
                continue;
            };
            let (Some(base), Some(last)) = (
                v.first_old.as_deref().map_or(Some(String::new()), text),
                v.last_new.as_deref().and_then(text),
            ) else {
                continue;
            };
            let Some(new) = change.new_hash.as_deref().and_then(text) else {
                continue;
            };
            let mut known: Vec<String> = v.sealed.iter().filter_map(|h| text(h)).collect();
            if let Some(t) = change.old_hash.as_deref().and_then(text) {
                known.push(t);
            }
            let known: Vec<&str> = known.iter().map(String::as_str).collect();
            let since = v.times.last().copied().unwrap_or_default();
            let mut excused: HashMap<String, usize> = HashMap::new();
            for other in specs.iter().filter(|s| s.id != spec_id) {
                let committed = other.commit_state == CommitState::Committed;
                for oid in &other.sealed_by {
                    let Ok(os) = repo.load_seal(oid) else {
                        continue;
                    };
                    if os.timestamp >= seal.timestamp || (!committed && os.timestamp <= since) {
                        continue;
                    }
                    for c in os.changes.iter().filter(|c| c.path == change.path) {
                        let (Some(o), n) = (
                            c.old_hash.as_deref().and_then(text),
                            c.new_hash.as_deref().and_then(text).unwrap_or_default(),
                        ) else {
                            continue;
                        };
                        for (l, k) in informed_removals(&o, &n) {
                            *excused.entry(l.to_string()).or_insert(0) += k;
                        }
                    }
                }
            }
            let verdict = own_line_loss_excused(
                &spec_id,
                &change.path,
                &base,
                &last,
                &known,
                &new,
                &[],
                &excused,
            );
            match verdict {
                None => eprintln!("REPLAY {short} {spec_id} {}: PASS", change.path),
                Some(l) => eprintln!(
                    "REPLAY {short} {spec_id} {}: REFUSED {} line(s) {:?}",
                    change.path,
                    l.lines.len(),
                    l.lines.iter().take(3).collect::<Vec<_>>()
                ),
            }
        }
    }
}

/// Runtime on a real store, when one is named: `WRIT_AUDIT_REPO=<path>`
/// audits every spec there against the working tree and prints the
/// numbers. Without the variable the test does nothing.
#[test]
fn runtime_on_a_real_store_when_requested() {
    let Ok(path) = std::env::var("WRIT_AUDIT_REPO") else {
        return;
    };
    let repo = Repository::open(Path::new(&path)).unwrap();
    let specs: Vec<String> = repo
        .list_specs()
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    let r = audit(&repo, &specs, Against::WorkingTree).unwrap();
    let files: usize = r.specs.iter().map(|s| s.files.len()).sum();
    eprintln!(
        "survival audit {path}: specs {} files {files} lines checked {} lost {} superseded {} in {} ms",
        r.specs.len(),
        r.checked,
        r.lost,
        r.superseded,
        r.elapsed_ms
    );
    for s in r.specs.iter().filter(|s| s.lost > 0) {
        for file in s.files.iter().filter(|f| !f.lost.is_empty()) {
            for l in &file.lost {
                eprintln!(
                    "  LOST {}:{} spec={} added_by={} removed_by={:?} {:?}",
                    file.path, l.line, s.spec_id, l.added_by, l.removed_by, l.text
                );
            }
        }
    }
    // Finding 74: the stuck-file scan runs on every seal; its cost matters.
    let started = std::time::Instant::now();
    let stuck = repo.stuck_files().unwrap();
    eprintln!(
        "stuck files: {} in {} ms {:?}",
        stuck.len(),
        started.elapsed().as_millis(),
        stuck.iter().map(|s| s.path.as_str()).collect::<Vec<_>>()
    );
    let git = audit(&repo, &specs, Against::Head);
    match git {
        Ok(g) => {
            eprintln!(
                "survival audit against HEAD: checked {} lost {} superseded {} in {} ms",
                g.checked, g.lost, g.superseded, g.elapsed_ms
            );
            let mut why: std::collections::BTreeMap<String, usize> = Default::default();
            for f in g.specs.iter().flat_map(|s| &s.files) {
                *why.entry(format!("status {:?}", f.status)).or_default() += 1;
                for l in &f.superseded {
                    *why.entry(format!("superseded {:?}", l.reason)).or_default() += 1;
                }
            }
            eprintln!(
                "  skipped non-distinctive {}; not committed {:?}; {why:?}",
                g.skipped, g.not_committed
            );
            for (spec, path, l) in g.lost_lines() {
                eprintln!(
                    "  HEADLOST {path}:{} spec={spec} added_by={} removed_by={:?} {:?}",
                    l.line, l.added_by, l.removed_by, l.text
                );
            }
        }
        Err(e) => eprintln!("survival audit against HEAD unavailable: {e}"),
    }
}

/// Finding 77: blank lines and text the file already had are not evidence
/// either way; only the distinctive addition is checked.
#[test]
fn non_distinctive_additions_are_skipped() {
    let f = Fixture::new();
    let base = base_text();
    let mut v: Vec<String> = base.lines().map(String::from).collect();
    v.splice(10..10, ["", "line 5", "fn only_a() {}"].map(String::from));
    f.write(&v.iter().map(|l| format!("{l}\n")).collect::<String>());
    f.seal("a", "sa").unwrap();
    f.write(&base);

    let r = audit(&f.repo, &ids(&["sa"]), Against::WorkingTree).unwrap();
    assert_eq!((r.checked, r.skipped, r.lost), (1, 2, 1), "{r:?}");
    assert_eq!(r.specs[0].files[0].lost[0].text, "fn only_a() {}");
}

/// Finding 42 variant B, in the store, under CC decision C: another
/// spec's version is on disk (unsealed), and a's closing seal records the
/// removal of a's own line under `--allow-removals`, after the seal-time
/// guard refused it once. No other spec ever sealed that content, so the
/// override is a's own confirmed deletion: superseded (own_override), not
/// lost, and nothing is captured.
#[test]
fn own_override_without_foreign_sealed_content_is_intentional() {
    let mut f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&base, 2, "A"));
    let added = f.seal("a", "sa").unwrap();
    // b's version (from b's base, never saw "line 2 A") on disk.
    f.write(&with_edit(&base, 30, "B"));
    // Without the override the seal-time guard refuses.
    assert!(f.seal("a", "sa").is_err());
    f.repo.set_allow_removal_paths([FILE.to_string()]);
    let closing = f.seal("a", "sa").unwrap();
    f.repo.set_allow_removal_paths(Vec::<String>::new());

    let r = audit(&f.repo, &ids(&["sa"]), Against::WorkingTree).unwrap();
    assert!(r.is_green(), "{r:?}");
    assert_eq!(r.lost_lines().count(), 0, "{r:?}");
    let file = &r.specs[0].files[0];
    assert!(file.captured.is_empty(), "{r:?}");
    let s = &file.superseded[0];
    assert_eq!(
        (s.text.as_str(), s.reason),
        ("line 2 A", SupersededBy::OwnOverride)
    );
    assert_eq!(s.by_seal, closing.id);
    assert_ne!(added.id, closing.id);
    assert!(findings(&f.repo, &r).is_empty());
}

/// The negative: a's own later seal replaces its line with new text, no
/// override. Ordinary iteration, nothing lost.
#[test]
fn own_line_replaced_without_override_is_not_lost() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&base, 2, "A"));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&base, 2, "A2"));
    f.seal("a", "sa").unwrap();

    let r = audit(&f.repo, &ids(&["sa"]), Against::WorkingTree).unwrap();
    assert!(r.is_green(), "{r:?}");
    assert_eq!(r.checked, 1);
}

/// CC decision C, the variant B signature: b sealed its version first;
/// a wrote its line over it and sealed; b's version came back on disk and
/// a's override recorded it, so a's seal holds b's sealed content in place
/// of a's own line. Captured: a yellow naming both specs and the seal, with
/// the scoped confirmation; never lost, never a block. Confirming it with
/// `writ finish --accept-removals <seal>` clears it.
#[test]
fn override_recording_another_specs_sealed_version_is_captured() {
    let mut f = Fixture::new();
    let base = base_text();
    let theirs = with_edit(&base, 30, "B");
    f.write(&theirs);
    let b = f.seal("b", "sb").unwrap();
    f.write(&with_edit(&theirs, 2, "A"));
    let added = f.seal("a", "sa").unwrap();
    f.write(&theirs);
    f.repo.set_allow_removal_paths([FILE.to_string()]);
    let closing = f.seal("a", "sa").unwrap();
    f.repo.set_allow_removal_paths(Vec::<String>::new());

    let r = audit(&f.repo, &ids(&["sa"]), Against::WorkingTree).unwrap();
    assert!(r.is_green(), "{r:?}");
    let c = &r.specs[0].files[0].captured;
    assert_eq!(c.len(), 1, "{r:?}");
    assert_eq!(c[0].text, "line 2 A");
    assert_eq!(c[0].added_by, added.id);
    assert_eq!(c[0].removed_by, closing.id);
    assert_eq!(
        (c[0].other_spec.as_str(), c[0].other_seal.as_str()),
        ("sb", b.id.as_str())
    );
    let found = findings(&f.repo, &r);
    assert_eq!(found.len(), 1);
    let m = &found[0].message;
    assert_eq!(found[0].severity, writ_core::doctor::Severity::Yellow);
    assert!(
        m.contains("override captured another spec's version"),
        "{m}"
    );
    assert!(
        m.contains("'sa'") && m.contains("'sb'") && m.contains("line 2 A"),
        "{m}"
    );
    assert!(
        m.contains(&format!(
            "writ finish --accept-removals {}",
            &closing.id[..12]
        )),
        "{m}"
    );
    assert!(found[0].fix_command.is_none() && found[0].needs_human);

    accept_removals(&f.repo, &closing.id[..12]).unwrap();
    assert!(findings(&f.repo, &r).is_empty());
}

/// The bound: only specs sealed or committed after the last green result.
#[test]
fn specs_since_bounds_to_work_after_the_last_green() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&base, 2, "A"));
    f.seal("a", "sa").unwrap();
    assert_eq!(
        specs_since(&f.repo, None).unwrap(),
        ids(&["base", "sa", "sb"])
    );

    let green = chrono::Utc::now();
    assert!(specs_since(&f.repo, Some(green)).unwrap().is_empty());
    f.write(&with_edit(&with_edit(&base, 2, "A"), 30, "B"));
    f.seal("b", "sb").unwrap();
    assert_eq!(specs_since(&f.repo, Some(green)).unwrap(), ids(&["sb"]));
    commit_spec(&f.repo, "sa");
    assert_eq!(
        specs_since(&f.repo, Some(green)).unwrap(),
        ids(&["sa", "sb"])
    );
}

/// Against HEAD: a sealed file git ignores can never be committed; it is
/// reported as such, not as a lost line.
#[cfg(feature = "bridge")]
#[test]
fn against_head_a_git_ignored_file_is_reported_not_lost() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let git = git2::Repository::init(root).unwrap();
    let sig = git2::Signature::now("t", "t@t").unwrap();
    fs::write(root.join(".gitignore"), ".writ/\n").unwrap();
    fs::create_dir_all(root.join("out")).unwrap();
    fs::write(root.join("out/keep.txt"), "kept\n").unwrap();
    let repo = Repository::init(root).unwrap();
    repo.add_spec(&Spec::new("sa".into(), "sa".into(), String::new()))
        .unwrap();
    repo.seal_paths(
        AgentIdentity {
            id: "a".into(),
            agent_type: AgentType::Agent,
        },
        "a".into(),
        Some("sa".into()),
        TaskStatus::InProgress,
        Verification::default(),
        &["out/keep.txt".to_string()],
        false,
    )
    .unwrap();
    fs::write(root.join(".gitignore"), ".writ/\nout/\n").unwrap();
    let mut idx = git.index().unwrap();
    idx.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    let tree = git.find_tree(idx.write_tree().unwrap()).unwrap();
    git.commit(Some("HEAD"), &sig, &sig, "c", &tree, &[])
        .unwrap();
    commit_spec(&repo, "sa");

    let r = audit(&repo, &ids(&["sa"]), Against::Head).unwrap();
    assert!(r.is_green(), "{r:?}");
    // writ reads .gitignore too, so writ ignores `out/` as well (finding
    // 111: an info note, not a finding).
    assert_eq!(
        r.specs[0].files[0].status,
        FileAuditStatus::IgnoredByGitAndWrit
    );
}

/// The override covers the overriding spec's own lines only. b's seal
/// held a's line in its base and dropped it with `--allow-removals` on
/// the path: the flag never applied to a's line (the own-line check does
/// not look at other specs' lines), so the informed rule decides:
/// superseded (b kept a's "line 3 A": it saw a's seal). The variant B
/// shape (a's own override) is
/// `own_override_without_foreign_sealed_content_is_intentional`.
#[test]
fn another_specs_override_does_not_change_the_informed_rule() {
    let mut f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&with_edit(&base, 2, "A"), 3, "A"));
    let added = f.seal("a", "sa").unwrap();
    f.write(&with_edit(&with_edit(&base, 3, "A"), 30, "B"));
    f.repo.set_allow_removal_paths([FILE.to_string()]);
    let b = f.seal("b", "sb").unwrap();
    f.repo.set_allow_removal_paths(Vec::<String>::new());

    let r = audit(&f.repo, &ids(&["sa"]), Against::WorkingTree).unwrap();
    assert!(r.is_green(), "{r:?}");
    assert_eq!(r.specs[0].files[0].seal_id, added.id);
    let s = &r.specs[0].files[0].superseded[0];
    assert_eq!(
        (s.by_seal.as_str(), s.reason),
        (b.id.as_str(), SupersededBy::InformedRemoval)
    );
}

/// CC (d): the committed-spec rule is the fallback where the informed
/// base is gone (gc'd blob). c continued a's version, then b's seal (base:
/// c's version) dropped a's line; c's blob is missing from the store, so
/// the removal cannot be shown informed. b committed: superseded, not lost.
#[test]
fn committed_spec_rule_covers_a_missing_base_blob() {
    let f = Fixture::new();
    f.repo
        .add_spec(&Spec::new("sc".into(), "sc".into(), String::new()))
        .unwrap();
    let base = base_text();
    f.write(&with_edit(&base, 2, "A"));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&with_edit(&base, 2, "A"), 20, "C"));
    let c = f.seal("c", "sc").unwrap();
    f.write(&with_edit(&base, 30, "B"));
    let b = f.seal("b", "sb").unwrap();
    let c_blob = c.changes[0].new_hash.clone().unwrap();
    assert_eq!(b.changes[0].old_hash.as_deref(), Some(c_blob.as_str()));
    let objects = f.root().join(".writ/objects");
    let mut removed = 0;
    for entry in walk(&objects) {
        if entry
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains(&c_blob[2..])
        {
            fs::remove_file(&entry).unwrap();
            removed += 1;
        }
    }
    assert_eq!(removed, 1, "blob {c_blob} not found under {objects:?}");

    // Without the committed rule nothing excuses it.
    let r = audit(&f.repo, &ids(&["sa"]), Against::WorkingTree).unwrap();
    assert_eq!(r.lost, 1, "{r:?}");
    commit_spec(&f.repo, "sb");
    let r = audit(&f.repo, &ids(&["sa"]), Against::WorkingTree).unwrap();
    assert!(r.is_green(), "{r:?}");
    let s = &r.specs[0].files[0].superseded[0];
    assert_eq!(
        (s.by_seal.as_str(), s.reason),
        (b.id.as_str(), SupersededBy::CommittedSpec)
    );
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

/// A stale rewrite restores text the file held before the spec (b wrote
/// from the original, so "line 2" comes back over a's "line 2 A"). The
/// merge keeps a's line; b's restored "line 2" is not b's work and its
/// absence is not a loss (anchor rule; convergence_records s4b).
#[test]
fn restored_old_text_is_not_distinctive() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&base, 2, "A"));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&base, 30, "B"));
    f.seal("b", "sb").unwrap();
    // The merged result: both edits.
    f.write(&with_edit(&with_edit(&base, 2, "A"), 30, "B"));
    let r = audit(&f.repo, &ids(&["sb"]), Against::WorkingTree).unwrap();
    assert!(r.is_green(), "{r:?}");
    assert_eq!((r.checked, r.skipped), (1, 1));
}

/// Finding 105 (Bri): a seals "line 2 A"; b seals its own version written
/// from a copy that never held it (the store records the shared index, a's
/// version, as b's base, so the removal looks informed); a then edits on
/// top of b's version and seals. b's version kept none of a's seal, so b
/// never saw the line: not an informed removal. a's line is lost, and the
/// seal-time check refuses a's seal instead of excusing it.
#[test]
fn removal_by_a_spec_that_never_saw_the_line_is_not_informed() {
    let f = Fixture::new();
    let base = base_text();
    let ours = with_edit(&with_edit(&base, 2, "A"), 3, "A");
    f.write(&ours);
    let added = f.seal("a", "sa").unwrap();
    let theirs = with_edit(&base, 30, "B");
    f.write(&theirs);
    f.seal("b", "sb").unwrap();
    f.write(&with_edit(&theirs, 10, "A2"));
    // The allowance must not excuse it: b never saw a's lines.
    assert!(
        f.seal("a", "sa").is_err(),
        "a's seal dropped its own lines unrefused"
    );

    // The backstop: whatever the seal-time check did, the audit sees it.
    let r = audit(&f.repo, &ids(&["sa"]), Against::WorkingTree).unwrap();
    let lost: Vec<&str> = r.lost_lines().map(|(_, _, l)| l.text.as_str()).collect();
    assert_eq!(lost, vec!["line 2 A", "line 3 A"], "{r:?}");
    assert!(r.lost_lines().all(|(_, _, l)| l.added_by == added.id));
}

/// The negative: b's version kept one of a's lines (b saw a's seal) and
/// removed the other on purpose: informed, superseded.
#[test]
fn removal_by_a_spec_that_saw_the_seal_is_informed() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&with_edit(&base, 2, "A"), 3, "A"));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&with_edit(&base, 3, "A"), 30, "B"));
    let b = f.seal("b", "sb").unwrap();

    let r = audit(&f.repo, &ids(&["sa"]), Against::WorkingTree).unwrap();
    assert!(r.is_green(), "{r:?}");
    let s = &r.specs[0].files[0].superseded[0];
    assert_eq!(
        (s.text.as_str(), s.by_seal.as_str()),
        ("line 2 A", b.id.as_str())
    );
}

/// Finding 105, the undecidable case, decided conservatively: a's seal
/// only inserted lines and b's version keeps none of them. b may have
/// deleted them on purpose or written from a copy that never had them;
/// the store cannot tell, so a's work is kept: lost until restored, or
/// until a finish commits b (committed-spec rule).
#[test]
fn pure_insertions_removed_by_a_spec_that_kept_none_are_lost() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&with_markers(&base));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&base, 30, "B"));
    f.seal("b", "sb").unwrap();

    let r = audit(&f.repo, &ids(&["sa"]), Against::WorkingTree).unwrap();
    assert_eq!((r.checked, r.lost), (5, 5), "{r:?}");
    commit_spec(&f.repo, "sb");
    let r = audit(&f.repo, &ids(&["sa"]), Against::WorkingTree).unwrap();
    assert!(r.is_green(), "{r:?}");
}

/// Finding 105, the other side: b rewrote a's line in place with new text
/// (a's seal added only that line, so nothing else of it could be kept).
/// An edit, not a stale copy: informed, superseded (isolation s2 fixture).
#[test]
fn rewriting_a_line_in_place_with_new_text_is_informed() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&with_edit(&base, 2, "A"));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&base, 2, "rewritten by b"));
    let b = f.seal("b", "sb").unwrap();

    let r = audit(&f.repo, &ids(&["sa"]), Against::WorkingTree).unwrap();
    assert!(r.is_green(), "{r:?}");
    let s = &r.specs[0].files[0].superseded[0];
    assert_eq!(
        (s.text.as_str(), s.by_seal.as_str()),
        ("line 2 A", b.id.as_str())
    );
}

/// Finding 69's shape under the 105 rule (CC, third clause): a's seal only
/// inserted markers; c's line had entered the file before it. b removed
/// the markers and kept everything else, c's line included: a targeted
/// removal by a version that saw a's seal. Informed.
#[test]
fn insert_only_removed_by_a_version_keeping_everything_else_is_informed() {
    let f = Fixture::new();
    f.repo
        .add_spec(&Spec::new("sc".into(), "sc".into(), String::new()))
        .unwrap();
    let base = base_text();
    let with_c = with_edit(&base, 35, "C");
    f.write(&with_c);
    f.seal("c", "sc").unwrap();
    f.write(&with_markers(&with_c));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&with_c, 30, "B"));
    let b = f.seal("b", "sb").unwrap();

    let r = audit(&f.repo, &ids(&["sa"]), Against::WorkingTree).unwrap();
    assert!(r.is_green(), "{r:?}");
    let sup = &r.specs[0].files[0].superseded;
    assert_eq!(sup.len(), 5);
    assert!(sup.iter().all(|s| s.by_seal == b.id));
}

/// The negative: a stale copy that lacks the inserted lines and c's line
/// (written from before either seal) is not informed: a's markers are lost.
#[test]
fn insert_only_removed_by_a_stale_copy_is_not_informed() {
    let f = Fixture::new();
    f.repo
        .add_spec(&Spec::new("sc".into(), "sc".into(), String::new()))
        .unwrap();
    let base = base_text();
    let with_c = with_edit(&base, 35, "C");
    f.write(&with_c);
    f.seal("c", "sc").unwrap();
    f.write(&with_markers(&with_c));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&base, 30, "B"));
    f.seal("b", "sb").unwrap();

    let r = audit(&f.repo, &ids(&["sa"]), Against::WorkingTree).unwrap();
    assert_eq!(r.lost, 5, "{r:?}");
}

/// Finding 106, audit side: the file's history starts in git (committed
/// before writ, never sealed), so a's first seal records no writ base. The
/// base comes from git, so a's addition is "line 2 A" alone, not the whole
/// file, and b's stale copy (written from the git version) does not count
/// as having seen it: lost.
#[cfg(feature = "bridge")]
#[test]
fn git_only_base_is_the_base_for_own_additions() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let git = git2::Repository::init(root).unwrap();
    let sig = git2::Signature::now("t", "t@t").unwrap();
    fs::write(root.join(".gitignore"), ".writ/\n").unwrap();
    fs::write(root.join(FILE), base_text()).unwrap();
    let mut idx = git.index().unwrap();
    idx.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    let tree = git.find_tree(idx.write_tree().unwrap()).unwrap();
    git.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    let repo = Repository::init(root).unwrap();
    let seal = |agent: &str, spec: &str| {
        repo.seal_paths(
            AgentIdentity {
                id: agent.into(),
                agent_type: AgentType::Agent,
            },
            format!("{agent} work"),
            Some(spec.into()),
            TaskStatus::InProgress,
            Verification::default(),
            &[FILE.to_string()],
            false,
        )
    };
    for id in ["sa", "sb"] {
        repo.add_spec(&Spec::new(id.into(), id.into(), String::new()))
            .unwrap();
    }
    fs::write(root.join(FILE), with_edit(&base_text(), 2, "A")).unwrap();
    let added = seal("a", "sa").unwrap();
    fs::write(root.join(FILE), with_edit(&base_text(), 30, "B")).unwrap();
    seal("b", "sb").unwrap();

    let r = audit(&repo, &ids(&["sa"]), Against::WorkingTree).unwrap();
    assert_eq!(r.checked, 1, "base not taken from git: {r:?}");
    let lost: Vec<_> = r.lost_lines().collect();
    assert_eq!(lost.len(), 1, "{r:?}");
    assert_eq!(lost[0].2.text, "line 2 A");
    assert_eq!(lost[0].2.added_by, added.id);
}
