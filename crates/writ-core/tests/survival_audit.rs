//! `survival::audit` (0.4.1 survival tier groundwork): per-spec added-line
//! survival against the working tree or git HEAD, the core of
//! `bench/finish_audit.py` in the store. Fixtures follow merge_survival.rs:
//! `shared.txt` = 40 numbered lines, specs `sa` and `sb` after the base seal.

use std::fs;
use std::path::Path;

use tempfile::{tempdir, TempDir};

use writ_core::convergence::survival::{audit, Against, FileAuditStatus};
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
    f.write(&with_edit(&base, 2, "A"));
    let a = f.seal("a", "sa").unwrap();
    f.write(&with_edit(&base, 30, "B"));
    f.seal("b", "sb").unwrap();
    // b's seal recorded a's version as its base and lacks the line: that
    // is an informed removal, so from the store's point of view it is
    // superseded, not lost.
    let r = audit(&f.repo, &ids(&["sa"]), Against::WorkingTree).unwrap();
    assert_eq!((r.lost, r.superseded), (0, 1), "{r:?}");
    let s = &r.specs[0].files[0].superseded[0];
    assert_eq!((s.line, s.text.as_str()), (3, "line 2 A"));
    assert_eq!(s.by_spec.as_deref(), Some("sb"));

    // An unsealed hand edit that drops b's line has no seal behind it: lost.
    // Relative to its own base (a's version) sb added two lines, the
    // restored "line 2" and "line 30 B"; only the second is missing.
    f.write(&base);
    let r = audit(&f.repo, &ids(&["sb"]), Against::WorkingTree).unwrap();
    assert!(!r.is_green());
    assert_eq!((r.checked, r.lost), (2, 1));
    let file = &r.specs[0].files[0];
    assert_eq!(file.path, FILE);
    assert_eq!(file.lost[0].line, 31);
    assert_eq!(file.lost[0].text, "line 30 B");
    assert_ne!(file.seal_id, a.id);
}

/// Bri's marker fixture: a adds five markers, b continues a and removes
/// them. Every marker is superseded by b's seal; nothing is lost.
#[test]
fn informed_removal_by_a_later_seal_supersedes_every_copy() {
    let f = Fixture::new();
    let base = base_text();
    f.write(&with_markers(&base));
    f.seal("a", "sa").unwrap();
    f.write(&with_edit(&base, 30, "B"));
    let b = f.seal("b", "sb").unwrap();

    let r = audit(&f.repo, &ids(&["sa", "sb"]), Against::WorkingTree).unwrap();
    assert!(r.is_green(), "{r:?}");
    let sa = &r.specs[0];
    assert_eq!((sa.checked, sa.lost, sa.superseded), (5, 0, 5));
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
                v.first_old
                    .as_deref()
                    .map_or(Some(String::new()), |h| text(h)),
                v.last_new.as_deref().and_then(|h| text(h)),
            ) else {
                continue;
            };
            let Some(new) = change.new_hash.as_deref().and_then(|h| text(h)) else {
                continue;
            };
            let mut known: Vec<String> = v.sealed.iter().filter_map(|h| text(h)).collect();
            if let Some(t) = change.old_hash.as_deref().and_then(|h| text(h)) {
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
                            c.old_hash.as_deref().and_then(|h| text(h)),
                            c.new_hash
                                .as_deref()
                                .and_then(|h| text(h))
                                .unwrap_or_default(),
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
            for l in file.lost.iter().take(3) {
                eprintln!(
                    "  LOST {}:{} spec={} {:?}",
                    file.path, l.line, s.spec_id, l.text
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
        Ok(g) => eprintln!(
            "survival audit against HEAD: checked {} lost {} superseded {} in {} ms",
            g.checked, g.lost, g.superseded, g.elapsed_ms
        ),
        Err(e) => eprintln!("survival audit against HEAD unavailable: {e}"),
    }
}
