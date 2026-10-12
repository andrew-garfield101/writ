//! Finding 115: the seal-tree merge takes an informed descendant without a
//! conflict, on the sprint 4 shape of `test_adoption_gates.py`.
//!
//! What happened (store seals 47ccf1a17c77, bd0c1a7e72c4, 5917307945a1):
//! Bri's first seal added an xfail block, helpers and decorators; her second
//! seal relabelled two `reason=` lines inside that block ("finding 99: ...",
//! "finding 100: ..."). Amis's seal, built on Bri's second version, lifted
//! the xfails: the block became a three-line header comment and the
//! decorators went. Amis's version keeps Bri's first seal's lines (anchor at
//! seal 1) and lacks the two lines Bri's second seal replaced, but also the
//! two that replaced them (the whole block is gone). The merge anchored at
//! seal 1, so `diff3(seal 1, seal 2, amis)` was a modify/delete conflict,
//! the base merge conflicted too, escalate picked the longer side and the
//! merge-time survival check refused (Amis's header lost).
//!
//! The 105 rule for an own seal (`saw_addition`, clause 2): a version that
//! lacks every line the seal replaced saw it. The merge anchor uses it now,
//! for lines the version's known ancestor (the anchor so far) held.

use std::fs;

use tempfile::{tempdir, TempDir};

use writ_core::convergence::survival::{anchor_seal, MERGE_LOSS_CLASS};
use writ_core::repo::SealTreeConvergenceReport;
use writ_core::seal::{AgentIdentity, AgentType, Seal, TaskStatus, Verification};
use writ_core::spec::Spec;
use writ_core::Repository;

const FILE: &str = "test_adoption_gates.py";
const BRI: &str = "harness-0-4-1";
const AMIS: &str = "stale-release";

fn lines(v: &[&str]) -> String {
    v.iter().map(|l| format!("{l}\n")).collect()
}

/// The file before sprint 4 (the merge base).
fn base() -> String {
    lines(&[
        "\"\"\"Adoption gates: seal enforcement for humans and agents.\"\"\"",
        "import os",
        "from pathlib import Path",
        "",
        "import pytest",
        "",
        "AGENT_VARS = [",
        "    \"WRIT_AGENT_ID\",",
        "    \"CLAUDE_CODE_SESSION\",",
        "]",
        "",
        "",
        "class TestSealGates:",
        "    def test_human_no_spec_rejected(self, writ_repo):",
        "        result = run_writ([\"seal\", \"-s\", \"human work\"], writ_repo)",
        "        assert result.returncode != 0",
        "",
        "    def test_human_no_spec_strict(self, writ_repo):",
        "        result = run_writ([\"seal\", \"-s\", \"strict\"], writ_repo)",
        "        assert result.returncode != 0",
        "",
        "    def test_agent_flag_without_spec(self, writ_repo):",
        "        result = run_writ([\"seal\", \"--agent\", \"a\"], writ_repo)",
        "        assert result.returncode != 0",
    ])
}

/// Bri's first seal: import, env var, the xfail block, helper, decorators.
fn bri1(claim_reason: &str, strict_reason: &str) -> String {
    lines(&[
        "\"\"\"Adoption gates: seal enforcement for humans and agents.\"\"\"",
        "import json",
        "import os",
        "from pathlib import Path",
        "",
        "import pytest",
        "",
        "AGENT_VARS = [",
        "    \"WRIT_AGENT_ID\",",
        "    \"CLAUDE_CODE_SESSION\",",
        "    # Finding 95: Claude Code sets CLAUDECODE=1 and writ treats it as an agent.",
        "    \"CLAUDECODE\",",
        "]",
        "",
        "# Sprint 4 (harness-0-4-1): the human default is warn-and-succeed (CC). Two",
        "# gaps found while correcting it, written as assertions before code:",
        "HUMAN_WARNING_CLAIM_XFAIL = pytest.mark.xfail(",
        "    strict=True,",
        claim_reason,
        ")",
        "HUMAN_STRICT_XFAIL = pytest.mark.xfail(",
        "    strict=True,",
        strict_reason,
        ")",
        "",
        "",
        "def _seal_summaries(path: Path) -> list:",
        "    out = run_writ([\"log\", \"--all\", \"--format\", \"json\"], str(path))",
        "    return [s.get(\"summary\") for s in json.loads(out.stdout)]",
        "",
        "",
        "class TestSealGates:",
        "    def test_human_no_spec_warns_and_succeeds(self, writ_repo):",
        "        result = run_writ([\"seal\", \"-s\", \"human work\"], writ_repo)",
        "        assert result.returncode == 0",
        "        assert \"human work\" in _seal_summaries(writ_repo)",
        "",
        "    @HUMAN_WARNING_CLAIM_XFAIL",
        "    def test_human_no_spec_warning_names_claim(self, writ_repo):",
        "        result = run_writ([\"seal\", \"-s\", \"human work\"], writ_repo)",
        "        assert \"--claim\" in result.stderr",
        "",
        "    @HUMAN_STRICT_XFAIL",
        "    def test_human_no_spec_strict(self, writ_repo):",
        "        result = run_writ([\"seal\", \"-s\", \"strict\"], writ_repo)",
        "        assert result.returncode != 0",
        "",
        "    def test_agent_flag_without_spec(self, writ_repo):",
        "        result = run_writ([\"seal\", \"--agent\", \"a\"], writ_repo)",
        "        assert result.returncode != 0",
    ])
}

const CLAIM_V1: &str =
    "    reason=\"human no-spec warning says `writ spec add` but not `--claim`\",";
const STRICT_V1: &str = "    reason=\"strict claim_enforcement does not refuse a human seal\",";
const CLAIM_V2: &str =
    "    reason=\"finding 99: human no-spec warning says `writ spec add` but not `--claim`\",";
const STRICT_V2: &str =
    "    reason=\"finding 100: strict claim_enforcement does not refuse a human seal\",";

/// Amis's seal on Bri's second version: the xfails lifted, the block and
/// the decorators replaced by a three-line header.
fn amis() -> String {
    let v = bri1(CLAIM_V2, STRICT_V2);
    let mut out = Vec::new();
    let mut skip = false;
    for l in v.lines() {
        if l.starts_with("# Sprint 4 (harness-0-4-1)") {
            out.push(
                "# Sprint 4: findings 98 (explicit --agent enforces like a detected agent), 99",
            );
            out.push("# (the human warning names `writ spec add \"...\" --claim`) and 100 (strict");
            out.push(
                "# claim_enforcement refuses a no-spec seal) are fixed; their xfails are lifted.",
            );
            skip = true;
            continue;
        }
        if skip {
            if l.is_empty() {
                skip = false;
            } else {
                continue;
            }
        }
        if l.trim_start().starts_with("@HUMAN_") {
            continue;
        }
        out.push(l);
    }
    lines(&out)
}

/// A git repository with the base committed: the survival tier's finish
/// tree reads HEAD (bridge builds only).
#[cfg(feature = "bridge")]
fn git_with_base(root: &std::path::Path) {
    let git = git2::Repository::init(root).unwrap();
    fs::write(root.join(".gitignore"), ".writ/\n").unwrap();
    fs::write(root.join(FILE), base()).unwrap();
    let mut idx = git.index().unwrap();
    idx.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    idx.write().unwrap();
    let tree = git.find_tree(idx.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("t", "t@t").unwrap();
    git.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
        .unwrap();
}

struct Fixture {
    dir: TempDir,
    repo: Repository,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempdir().unwrap();
        #[cfg(feature = "bridge")]
        git_with_base(dir.path());
        fs::write(dir.path().join(FILE), base()).unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let f = Self { dir, repo };
        f.repo
            .add_spec(&Spec::new("base".into(), "base".into(), String::new()))
            .unwrap();
        f.seal("setup", "base");
        for id in [BRI, AMIS] {
            f.repo
                .add_spec(&Spec::new(id.into(), id.into(), String::new()))
                .unwrap();
        }
        f
    }

    fn write(&self, content: &str) {
        fs::write(self.dir.path().join(FILE), content).unwrap();
    }

    fn seal(&self, agent: &str, spec: &str) -> Seal {
        self.repo
            .seal_paths(
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
            .unwrap()
    }

    /// Bri's two seals, as in the store.
    fn bri_seals(&self) {
        self.write(&bri1(CLAIM_V1, STRICT_V1));
        self.seal("bri", BRI);
        self.write(&bri1(CLAIM_V2, STRICT_V2));
        self.seal("bri", BRI);
    }

    fn finish(&self) -> SealTreeConvergenceReport {
        self.repo.mark_spec_done(BRI, None).unwrap();
        self.repo.mark_spec_done(AMIS, None).unwrap();
        self.repo.finalize_convergence().unwrap()
    }

    fn merged(&self, r: &SealTreeConvergenceReport) -> String {
        let (_, hash) = r
            .shadow_results
            .iter()
            .find(|(p, _)| p == FILE)
            .expect("file not merged");
        String::from_utf8(self.repo.object_content(hash).unwrap()).unwrap()
    }
}

/// The sprint 4 shape: Amis's seal descends from Bri's second version. The
/// merge takes it as it is: no conflict, no escalation, no notice, and the
/// survival tier on the same state is green.
#[test]
fn informed_descendant_merges_to_the_descendant() {
    let f = Fixture::new();
    f.bri_seals();
    f.write(&amis());
    f.seal("amis", AMIS);

    let r = f.finish();

    assert!(r.is_clean, "{:?}", r.escalations);
    assert!(r.escalations.is_empty(), "{:?}", r.escalations);
    assert!(r.notices.is_empty(), "{:?}", r.notices);
    let merged = f.merged(&r);
    assert_eq!(merged, amis());
    assert_eq!(merged.matches("xfails are lifted").count(), 1);
    assert!(!merged.contains("finding 99:"), "{merged}");
}

/// The survival tier (`writ doctor --survival`) on the sprint 4 shape is
/// green: it judges the tree finish would commit, which is now Amis's.
#[cfg(feature = "bridge")]
#[test]
fn informed_descendant_is_green_on_the_survival_tier() {
    let f = Fixture::new();
    f.bri_seals();
    f.write(&amis());
    f.seal("amis", AMIS);
    f.repo.mark_spec_done(BRI, None).unwrap();
    f.repo.mark_spec_done(AMIS, None).unwrap();

    let tier = writ_core::convergence::survival::run_tier(&f.repo).unwrap();

    assert!(tier.cache.green, "{:?}", tier.cache.findings);
    assert_eq!(tier.cache.lost, 0);
}

/// Negative: a stale rewrite. Amis's header written over the merge base,
/// with none of Bri's seals in it. The merge conflicts where both added
/// lines and the survival check refuses: nothing is staged.
#[test]
fn stale_rewrite_still_conflicts_and_the_check_refuses() {
    let f = Fixture::new();
    f.bri_seals();
    let stale = base().replace(
        "]\n\n\nclass",
        "]\n\n# Sprint 4: findings 98, 99 and 100 are fixed; their xfails are lifted.\n\n\nclass",
    );
    f.write(&stale);
    f.seal("amis", AMIS);

    let r = f.finish();

    assert!(!r.is_clean);
    assert!(
        r.escalations
            .iter()
            .any(|e| e.file_path == FILE && e.conflict_class == MERGE_LOSS_CLASS),
        "{:?}",
        r.escalations
    );
    assert!(r.shadow_results.is_empty(), "{:?}", r.shadow_results);
}

/// The anchor does not over-reach: a copy taken from Bri's first seal,
/// which still holds the two reason lines her second seal replaced, saw
/// seal 1 only; Amis's version saw both; the base rewrite saw neither.
#[test]
fn anchor_per_version() {
    let s1 = bri1(CLAIM_V1, STRICT_V1);
    let s2 = bri1(CLAIM_V2, STRICT_V2);
    let v = [s1.as_str(), s2.as_str()];
    let b = base();
    assert_eq!(anchor_seal(&b, &b, &v, &amis()), Some(1));
    assert_eq!(anchor_seal(&b, &b, &v, &older_copy()), Some(0));
    assert_eq!(anchor_seal(&b, &b, &v, &b), None);
}

fn older_copy() -> String {
    bri1(CLAIM_V1, STRICT_V1).replace(
        "        assert result.returncode != 0\n\n    def test_agent_flag",
        "        assert result.returncode != 0\n        assert \"strict\" in result.stderr\n\n    def test_agent_flag",
    )
}

/// Negative: the copy from before the relabel is stale with respect to
/// seal 2 and carries its replaced lines; the merge keeps the relabel and
/// the survival check refuses (the copy's carried lines are not in the
/// output), so nothing is staged.
#[test]
fn copy_from_before_the_relabel_is_refused_not_taken() {
    let f = Fixture::new();
    f.bri_seals();
    f.write(&older_copy());
    f.seal("amis", AMIS);

    let r = f.finish();

    assert!(!r.is_clean);
    assert!(
        r.escalations
            .iter()
            .any(|e| e.file_path == FILE && e.conflict_class == MERGE_LOSS_CLASS),
        "{:?}",
        r.escalations
    );
    assert!(r.shadow_results.is_empty(), "{:?}", r.shadow_results);
}
