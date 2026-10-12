//! The survival tier of `writ doctor` (0.4.1): cache, state, headline and
//! the "safe to finish" honesty condition, in the store (no CLI). The CLI
//! scenarios are Bri's, in `crates/writ-cli/tests/doctor_states.rs`.

use std::fs;
use std::path::Path;

use tempfile::{tempdir, TempDir};

use writ_core::convergence::survival::{run_tier, SurvivalCache, SurvivalState, CHECK_SURVIVAL};
use writ_core::doctor::{self, Severity, SAFE_TO_FINISH};
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
    fn new() -> Self {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let f = Self { dir, repo };
        f.write(FILE, &base_text());
        for id in ["base", "sa", "sb"] {
            f.repo
                .add_spec(&Spec::new(id.into(), id.into(), String::new()))
                .unwrap();
        }
        f.seal("setup", "base", FILE).unwrap();
        f
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, path: &str, content: &str) {
        fs::write(self.root().join(path), content).unwrap();
    }

    fn seal(&self, agent: &str, spec: &str, path: &str) -> Result<Seal, WritError> {
        self.repo.seal_paths(
            AgentIdentity {
                id: agent.to_string(),
                agent_type: AgentType::Agent,
            },
            format!("{agent} work"),
            Some(spec.to_string()),
            TaskStatus::InProgress,
            Verification::default(),
            &[path.to_string()],
            false,
        )
    }

    /// Finding 42 variant B, landed with `--allow-removals`.
    fn variant_b(&mut self) -> Seal {
        let base = base_text();
        self.write(FILE, &with_edit(&base, 2, "A"));
        let added = self.seal("a", "sa", FILE).unwrap();
        self.write(FILE, &with_edit(&base, 30, "B"));
        self.repo.set_allow_removal_paths([FILE.to_string()]);
        self.seal("a", "sa", FILE).unwrap();
        self.repo.set_allow_removal_paths(Vec::<String>::new());
        added
    }

    /// A sealed line gone from disk with no seal behind the removal.
    fn lost_on_disk(&self) -> Seal {
        self.write(FILE, &with_edit(&base_text(), 2, "A"));
        let added = self.seal("a", "sa", FILE).unwrap();
        self.write(FILE, &base_text());
        added
    }
}

#[test]
fn never_run_is_not_run_and_never_safe() {
    let f = Fixture::new();
    assert_eq!(SurvivalState::current(&f.repo), SurvivalState::NotRun);
    let r = doctor::run(&f.repo).unwrap();
    assert!(r.survival_last_green.is_none());
    assert!(r
        .headline
        .starts_with("fast checks clean; survival not run"));
    assert!(!r.headline.contains(SAFE_TO_FINISH), "{}", r.headline);
}

#[test]
fn green_run_is_cached_by_state_and_says_safe_only_then() {
    let f = Fixture::new();
    f.write(FILE, &with_edit(&base_text(), 2, "A"));
    let newest = f.seal("a", "sa", FILE).unwrap();

    let r = doctor::run_survival(&f.repo).unwrap();
    assert!(r.clean, "{r:?}");
    assert_eq!(r.survival_last_green.as_deref(), Some(newest.id.as_str()));
    assert_eq!(
        r.headline,
        format!(
            "fast checks clean; survival green as of {}; {SAFE_TO_FINISH}",
            &newest.id[..12]
        )
    );
    assert!(r.checks_run.iter().any(|c| c == CHECK_SURVIVAL));
    // The fast tier reads the cache: same headline, no audit.
    let fast = doctor::run(&f.repo).unwrap();
    assert_eq!(fast.headline, r.headline);
    assert_eq!(fast.brief_line(), r.headline);
    // Served from the cache while nothing changed.
    assert!(run_tier(&f.repo).unwrap().cached);
}

#[test]
fn a_new_seal_makes_the_result_stale_and_unsafe() {
    let f = Fixture::new();
    f.write(FILE, &with_edit(&base_text(), 2, "A"));
    let green = f.seal("a", "sa", FILE).unwrap();
    doctor::run_survival(&f.repo).unwrap();
    f.write("b.txt", "b1\n");
    let newer = f.seal("b", "sb", "b.txt").unwrap();

    let r = doctor::run(&f.repo).unwrap();
    assert_eq!(r.survival_last_green.as_deref(), Some(green.id.as_str()));
    assert_ne!(r.survival_last_green.as_deref(), Some(newer.id.as_str()));
    assert!(r.headline.contains("store changed since"), "{}", r.headline);
    assert!(!r.headline.contains(SAFE_TO_FINISH));
    assert!(!run_tier(&f.repo).unwrap().cached);
    // Rerun: green as of the new seal.
    let r = doctor::run_survival(&f.repo).unwrap();
    assert_eq!(r.survival_last_green.as_deref(), Some(newer.id.as_str()));
}

#[test]
fn an_unsealed_edit_to_an_audited_file_makes_it_stale() {
    let f = Fixture::new();
    f.write(FILE, &with_edit(&base_text(), 2, "A"));
    f.seal("a", "sa", FILE).unwrap();
    doctor::run_survival(&f.repo).unwrap();
    // Different length, so the stat key changes even on coarse mtimes.
    f.write(FILE, &base_text());
    assert!(matches!(
        SurvivalState::current(&f.repo),
        SurvivalState::Stale { .. }
    ));
    let r = doctor::run_survival(&f.repo).unwrap();
    assert!(!r.clean);
    assert!(!r.headline.contains(SAFE_TO_FINISH));
}

#[test]
fn lost_line_is_red_names_line_and_adding_seal_and_never_safe() {
    let f = Fixture::new();
    let added = f.lost_on_disk();
    // The fast tier has no survival finding (it sees only the pending file).
    assert!(doctor::run(&f.repo)
        .unwrap()
        .findings
        .iter()
        .all(|x| x.check != CHECK_SURVIVAL));

    let r = doctor::run_survival(&f.repo).unwrap();
    assert!(!r.clean);
    let finding = r
        .findings
        .iter()
        .find(|x| x.check == CHECK_SURVIVAL)
        .expect("survival finding");
    assert_eq!(finding.severity, Severity::Red);
    assert!(finding.message.contains("line 2 A"), "{}", finding.message);
    assert!(finding.message.contains(&added.id[..12]));
    assert_eq!(finding.paths, vec![FILE.to_string()]);
    assert!(finding.fix_command.is_none() && finding.needs_human);
    assert!(!finding.message.contains("--force"));
    assert!(
        r.headline.contains("; survival red: 1 sealed line"),
        "{}",
        r.headline
    );
    assert!(!r.headline.contains(SAFE_TO_FINISH));
    assert!(r.survival_last_green.is_none());
    // The fast tier now carries the red state from the cache.
    let fast = doctor::run(&f.repo).unwrap();
    assert!(fast.headline.contains("survival red"));
    assert!(!fast.brief_line().contains(SAFE_TO_FINISH));
    let cache = SurvivalCache::load(&f.repo).unwrap();
    assert!(!cache.green && cache.lost == 1);
}

#[test]
fn red_then_restored_and_sealed_is_green() {
    let mut f = Fixture::new();
    f.variant_b();
    doctor::run_survival(&f.repo).unwrap();
    f.write(FILE, &with_edit(&with_edit(&base_text(), 2, "A"), 30, "B"));
    let fixed = f.seal("a", "sa", FILE).unwrap();
    let r = doctor::run_survival(&f.repo).unwrap();
    assert!(r.clean, "{r:?}");
    assert_eq!(r.survival_last_green.as_deref(), Some(fixed.id.as_str()));
}

/// A fast-tier finding keeps "safe to finish" away even with survival green.
#[test]
fn safe_needs_both_tiers() {
    for (red, yellow) in [(0, 0), (1, 0), (0, 1)] {
        let h = doctor::headline_with(red, yellow, "survival green as of abc", true);
        assert_eq!(h.contains(SAFE_TO_FINISH), red + yellow == 0, "{h}");
        assert!(!doctor::headline(red, yellow).contains(SAFE_TO_FINISH));
    }
}

/// CC decision C: variant B landed with the spec's own override, no other
/// spec ever sealed that content: the author's confirmed deletion. Green,
/// no note, and "safe to finish" may be said.
#[test]
fn variant_b_with_own_override_and_no_foreign_seal_is_green() {
    let mut f = Fixture::new();
    f.variant_b();
    let r = doctor::run_survival(&f.repo).unwrap();
    assert!(r.clean, "{r:?}");
    assert!(r.headline.ends_with(SAFE_TO_FINISH), "{}", r.headline);
}

/// Finding 111: a sealed file that git and writ both ignore now (the
/// directory was ignored after the seal). One info note naming the two
/// exits, no finding, and "safe to finish" still holds; deleting the file
/// and sealing the path by name records the deletion and retires the note.
#[cfg(feature = "bridge")]
#[test]
fn sealed_path_ignored_by_git_and_writ_is_info_and_its_deletion_can_be_sealed() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let git = git2::Repository::init(root).unwrap();
    let sig = git2::Signature::now("t", "t@t").unwrap();
    fs::write(root.join(".gitignore"), ".writ/\n").unwrap();
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
    for id in ["sa", "sb"] {
        repo.add_spec(&Spec::new(id.into(), id.into(), String::new()))
            .unwrap();
    }
    let seal = |agent: &str, spec: &str, path: &str| {
        repo.seal_paths(
            AgentIdentity {
                id: agent.into(),
                agent_type: AgentType::Agent,
            },
            format!("{agent} work"),
            Some(spec.into()),
            TaskStatus::InProgress,
            Verification::default(),
            &[path.to_string()],
            false,
        )
    };
    let path = "out/results/.gitignore";
    fs::create_dir_all(root.join("out/results")).unwrap();
    fs::write(root.join(path), "*\n!.gitignore\n").unwrap();
    let sealed = seal("a", "sa", path).unwrap();
    repo.update_spec(
        "sa",
        writ_core::spec::SpecUpdate {
            status: Some(writ_core::spec::SpecStatus::Complete),
            ..Default::default()
        },
    )
    .unwrap();
    repo.mark_spec_committed("sa", "0000000").unwrap();
    // Later both git and writ ignore the directory; HEAD never got the file.
    fs::write(root.join(".gitignore"), ".writ/\nout/results/\n").unwrap();
    commit("ignore results");

    let r = doctor::run_survival(&repo).unwrap();
    assert!(
        r.findings.iter().all(|f| f.check != CHECK_SURVIVAL),
        "{r:?}"
    );
    assert_eq!(r.survival_info.len(), 1, "{r:?}");
    let note = &r.survival_info[0];
    assert!(
        note.contains(path) && note.contains(&sealed.id[..12]),
        "{note}"
    );
    assert!(note.contains("remove the ignore rule") && note.contains("stays as history"));
    assert!(
        note.contains(&format!("writ seal --paths {path}")),
        "{note}"
    );
    assert!(
        r.headline.ends_with(SAFE_TO_FINISH) || !r.clean,
        "{}",
        r.headline
    );
    assert!(SurvivalState::current(&repo).is_safe());

    // Exit 1 without editing the ignore rules: delete and seal by name.
    fs::remove_file(root.join(path)).unwrap();
    let del = seal("b", "sb", path).unwrap();
    assert_eq!(del.changes.len(), 1);
    assert_eq!(
        del.changes[0].change_type,
        writ_core::seal::ChangeType::Deleted
    );
    let r = doctor::run_survival(&repo).unwrap();
    assert!(r.survival_info.is_empty(), "{r:?}");
    // An ignored path with no prior seal is still nothing to seal.
    assert!(matches!(
        seal("b", "sb", "out/results/never.txt"),
        Err(WritError::NothingToSeal)
    ));
}

/// Finding 123: the same note while the sealing spec is still open (not
/// done, not committed), with the file still on disk: its holder can still
/// remove the ignore rule and seal, so the state is named now, same text.
#[cfg(feature = "bridge")]
#[test]
fn sealed_path_ignored_by_git_and_writ_is_noted_while_its_spec_is_open() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let git = git2::Repository::init(root).unwrap();
    let sig = git2::Signature::now("t", "t@t").unwrap();
    fs::write(root.join(".gitignore"), ".writ/\n").unwrap();
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
    repo.add_spec(&Spec::new("sa".into(), "sa".into(), String::new()))
        .unwrap();
    let path = "out/results/.gitignore";
    fs::create_dir_all(root.join("out/results")).unwrap();
    fs::write(root.join(path), "*\n!.gitignore\n").unwrap();
    let sealed = repo
        .seal_paths(
            AgentIdentity {
                id: "a".into(),
                agent_type: AgentType::Agent,
            },
            "a work".into(),
            Some("sa".into()),
            TaskStatus::InProgress,
            Verification::default(),
            &[path.to_string()],
            false,
        )
        .unwrap();
    fs::write(root.join(".gitignore"), ".writ/\nout/results/\n").unwrap();
    commit("ignore results");
    assert!(root.join(path).exists());

    let r = doctor::run_survival(&repo).unwrap();

    assert!(
        r.findings.iter().all(|f| f.check != CHECK_SURVIVAL),
        "{r:?}"
    );
    assert_eq!(r.survival_info.len(), 1, "{r:?}");
    let note = &r.survival_info[0];
    assert!(
        note.contains(path)
            && note.contains(&sealed.id[..12])
            && note.contains("stays as history")
            && note.contains(&format!("writ seal --paths {path}")),
        "{note}"
    );
}
