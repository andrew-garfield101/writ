"""V3.5: Seal-tree convergence Python binding tests.

Tests for converge_from_seal_trees(), finalize_convergence(), and
materialize_convergence() — the v3 convergence path that reads from
each spec's sealed file versions using genesis as common ancestor.
"""

from pathlib import Path

import pytest
import writ


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


BASE_RS = "// base\nfn hello() {}\n"
FEATURE_A_RS = BASE_RS + "fn feature_a() {}\n"
FEATURE_B_RS = BASE_RS + "fn feature_b() {}\n"


def make_sealed_overlapping_specs(tmp_path: Path, concurrent: bool = True):
    """Create a repo with 2 specs that both append to shared.rs, both done.

    concurrent=True (default): both specs seal from the baseline. After A
    seals, the tree is restored to the baseline before B writes, so B's
    recorded before (old_hash) is the baseline blob, not A's: neither spec saw
    the other's line. This is the parallel case (two worktrees, or two agents
    starting from one checkout).

    concurrent=False: B rewrites the file after A sealed, so B's before held
    "fn feature_a() {}" and B's after does not: an informed removal (Haris
    99cec5b6041e), which merges cleanly to B's version.

    Returns (repo, path).
    """
    repo = writ.Repository.init(str(tmp_path))
    shared = tmp_path / "shared.rs"

    shared.write_text(BASE_RS)
    baseline = repo.seal(
        summary="baseline",
        agent_id="setup",
        agent_type="agent",
        status="in-progress",
    )

    repo.add_spec(id="feat-a", title="Feature A")
    repo.add_spec(id="feat-b", title="Feature B")

    shared.write_text(FEATURE_A_RS)
    repo.seal(
        summary="add feature_a",
        agent_id="agent-a",
        agent_type="agent",
        spec_id="feat-a",
        status="in-progress",
        paths=["shared.rs"],
    )

    if concurrent:
        repo.restore(baseline["id"])
        assert shared.read_text() == BASE_RS
    shared.write_text(FEATURE_B_RS)
    seal_b = repo.seal(
        summary="add feature_b",
        agent_id="agent-b",
        agent_type="agent",
        spec_id="feat-b",
        status="in-progress",
        paths=["shared.rs"],
    )
    (change,) = seal_b["changes"]
    before_is_baseline = change["old_hash"] == baseline_hash(baseline)
    assert before_is_baseline == concurrent, (
        f"fixture precondition: feat-b's before must be the baseline blob "
        f"iff concurrent ({change})"
    )

    repo.spec_done("feat-a")
    repo.spec_done("feat-b")

    return repo, tmp_path


def baseline_hash(baseline_seal: dict) -> str:
    """The blob hash of shared.rs in the baseline seal."""
    (change,) = [c for c in baseline_seal["changes"] if c["path"] == "shared.rs"]
    return change["new_hash"]


# ---------------------------------------------------------------------------
# V3.5 Test #1: converge_from_seal_trees returns report
# ---------------------------------------------------------------------------


class TestConvergeFromSealTrees:
    """converge_from_seal_trees() Python binding contract tests."""

    def test_returns_report_dict(self, tmp_path):
        """Returns a dict with expected SealTreeConvergenceReport fields."""
        repo, _ = make_sealed_overlapping_specs(tmp_path)

        report = repo.converge_from_seal_trees(["feat-a", "feat-b"])
        assert isinstance(report, dict)
        assert "merged_files" in report
        assert "escalations" in report
        assert "is_clean" in report
        assert "specs_converged" in report
        assert "shadow_results" in report

    def test_detects_overlapping_files(self, tmp_path):
        """Overlapping files are detected and merged."""
        repo, _ = make_sealed_overlapping_specs(tmp_path)

        report = repo.converge_from_seal_trees(["feat-a", "feat-b"])
        assert len(report["merged_files"]) > 0, "should merge overlapping files"

    def test_merged_file_fields(self, tmp_path):
        """Each merged file has the expected SealTreeMergeResult fields."""
        repo, _ = make_sealed_overlapping_specs(tmp_path)

        report = repo.converge_from_seal_trees(["feat-a", "feat-b"])
        mf = report["merged_files"][0]
        assert "path" in mf
        assert "base_hash" in mf
        assert "spec_versions" in mf
        assert "merged_hash" in mf
        assert "confidence" in mf
        assert "clean" in mf

    def test_disjoint_files_no_merge(self, tmp_repo):
        """Specs with disjoint files produce no merged_files."""
        repo, path = tmp_repo

        (path / "base.txt").write_text("base\n")
        repo.seal(
            summary="baseline",
            agent_id="setup",
            agent_type="agent",
            status="in-progress",
        )

        repo.add_spec(id="s1", title="S1")
        repo.add_spec(id="s2", title="S2")

        (path / "a.txt").write_text("a work\n")
        repo.seal(
            summary="a", agent_id="a1", agent_type="agent",
            spec_id="s1", status="in-progress",
            paths=["a.txt"],
        )

        (path / "b.txt").write_text("b work\n")
        repo.seal(
            summary="b", agent_id="b1", agent_type="agent",
            spec_id="s2", status="in-progress",
            paths=["b.txt"],
        )

        repo.spec_done("s1")
        repo.spec_done("s2")

        report = repo.converge_from_seal_trees(["s1", "s2"])
        assert report["is_clean"] is True
        assert len(report["merged_files"]) == 0

    def test_single_spec_no_convergence(self, tmp_repo):
        """Single spec returns clean immediately."""
        repo, path = tmp_repo

        repo.add_spec(id="solo", title="Solo")
        (path / "file.txt").write_text("solo work\n")
        repo.seal(
            summary="solo", agent_id="a1", agent_type="agent",
            spec_id="solo", status="in-progress",
            paths=["file.txt"],
        )
        repo.spec_done("solo")

        report = repo.converge_from_seal_trees(["solo"])
        assert report["is_clean"] is True
        assert len(report["merged_files"]) == 0

    def test_same_spot_appends_escalate_and_stage_nothing(self, tmp_path):
        """Both specs append at the same spot: a conflict. Merge survival
        (finding 45) escalates the side the merge would drop instead of
        keeping one version, so nothing is staged and no file reports a
        merged hash."""
        repo, _ = make_sealed_overlapping_specs(tmp_path)

        report = repo.converge_from_seal_trees(["feat-a", "feat-b"])
        assert report["is_clean"] is False
        assert any(
            e["conflict_class"] == "merge_survival_loss" for e in report["escalations"]
        ), report["escalations"]
        assert report["shadow_results"] == []
        assert report["merged_files"], "escalated file not listed"
        for f in report["merged_files"]:
            assert f["clean"] is False
            assert f["merged_hash"] == ""

    def test_informed_rewrite_of_same_spot_merges_clean_to_the_rewrite(self, tmp_path):
        """Sequential case: feat-b sealed with feat-a's line in its before and
        removed it. That removal is informed (99cec5b6041e), not a loss: the
        merge is clean and takes feat-b's version."""
        repo, _ = make_sealed_overlapping_specs(tmp_path, concurrent=False)

        report = repo.converge_from_seal_trees(["feat-a", "feat-b"])

        assert report["is_clean"] is True, report["escalations"]
        assert not [
            e for e in report["escalations"] if e["conflict_class"] == "merge_survival_loss"
        ]
        (merged,) = [f for f in report["merged_files"] if f["path"] == "shared.rs"]
        assert merged["clean"] is True
        feat_b_blob = dict(merged["spec_versions"])["feat-b"]
        assert merged["merged_hash"] == feat_b_blob
        assert report["shadow_results"] == [["shared.rs", feat_b_blob]] or report[
            "shadow_results"
        ] == [("shared.rs", feat_b_blob)]

    def test_shadow_results_populated(self, tmp_path):
        """shadow_results contains (path, hash) tuples for materialization."""
        repo = writ.Repository.init(str(tmp_path))
        base = "".join(f"line {i}\n" for i in range(20))
        (tmp_path / "shared.rs").write_text(base)
        repo.seal(summary="baseline", agent_id="setup", agent_type="agent",
                  status="in-progress")
        repo.add_spec(id="feat-a", title="Feature A")
        repo.add_spec(id="feat-b", title="Feature B")
        (tmp_path / "shared.rs").write_text(base.replace("line 2\n", "line 2 A\n"))
        repo.seal(summary="a", agent_id="agent-a", agent_type="agent",
                  spec_id="feat-a", status="in-progress", paths=["shared.rs"])
        (tmp_path / "shared.rs").write_text(base.replace("line 15\n", "line 15 B\n"))
        repo.seal(summary="b", agent_id="agent-b", agent_type="agent",
                  spec_id="feat-b", status="in-progress", paths=["shared.rs"])

        report = repo.converge_from_seal_trees(["feat-a", "feat-b"])
        assert report["is_clean"] is True, report["escalations"]
        assert len(report["shadow_results"]) == 1
        path, hash_val = report["shadow_results"][0]
        assert path == "shared.rs"
        assert isinstance(hash_val, str) and len(hash_val) > 0
        assert report["merged_files"][0]["merged_hash"] == hash_val


# ---------------------------------------------------------------------------
# V3.5 Test #2: finalize_convergence
# ---------------------------------------------------------------------------


class TestFinalizeConvergence:
    """finalize_convergence() Python binding contract tests."""

    def test_finalize_returns_report(self, tmp_path):
        """finalize_convergence returns a convergence report."""
        repo, _ = make_sealed_overlapping_specs(tmp_path)

        report = repo.finalize_convergence()
        assert isinstance(report, dict)
        assert "is_clean" in report
        assert "specs_converged" in report

    def test_finalize_with_no_completed_specs(self, tmp_repo):
        """No completed specs returns clean with empty results."""
        repo, path = tmp_repo

        repo.add_spec(id="wip", title="WIP")
        (path / "file.txt").write_text("wip\n")
        repo.seal(
            summary="wip", agent_id="a1", agent_type="agent",
            spec_id="wip", status="in-progress",
            paths=["file.txt"],
        )

        report = repo.finalize_convergence()
        assert report["is_clean"] is True
        assert len(report["merged_files"]) == 0


# ---------------------------------------------------------------------------
# V3.5 Test #3: materialize_convergence
# ---------------------------------------------------------------------------


class TestMaterializeConvergence:
    """materialize_convergence() Python binding contract tests."""

    def test_materialize_writes_merged_content(self, tmp_path):
        """After materialization, the merged content is on disk. Uses the
        informed-rewrite case: the concurrent one escalates and has nothing
        to materialize (which made this test vacuous)."""
        repo, path = make_sealed_overlapping_specs(tmp_path, concurrent=False)
        (path / "shared.rs").write_text(BASE_RS)  # disk differs from the merge

        report = repo.finalize_convergence()
        assert report["shadow_results"], report

        repo.materialize_convergence(report)

        assert (path / "shared.rs").read_text() == FEATURE_B_RS

    def test_materialize_empty_report_is_noop(self, tmp_repo):
        """Materializing an empty report does nothing."""
        repo, path = tmp_repo

        empty_report = {
            "merged_files": [],
            "escalations": [],
            "convergence_seal_id": None,
            "is_clean": True,
            "specs_converged": [],
            "shadow_results": [],
        }
        # Should not raise
        repo.materialize_convergence(empty_report)


# ---------------------------------------------------------------------------
# V3.5 Test #4: Full workflow (plan → seal → done → finalize → materialize)
# ---------------------------------------------------------------------------


class TestFullV3Workflow:
    """End-to-end v3 convergence workflow via Python."""

    def test_full_workflow(self, tmp_path):
        """Plan → agents seal → spec done → finalize → materialize."""
        repo = writ.Repository.init(str(tmp_path))

        # Baseline
        (tmp_path / "shared.rs").write_text("// base\n")
        repo.seal(
            summary="baseline",
            agent_id="setup",
            agent_type="agent",
            status="in-progress",
        )

        # Plan 3 tasks
        repo.plan(["Auth module", "Payment system", "Dashboard UI"])

        # Agent 1 works on auth (modifies shared.rs + creates auth.py)
        (tmp_path / "shared.rs").write_text("// base\n// auth import\n")
        (tmp_path / "auth.py").write_text("def login(): pass\n")
        repo.seal(
            summary="auth",
            agent_id="agent-1",
            agent_type="agent",
            spec_id="auth-module",
            status="in-progress",
            paths=["shared.rs", "auth.py"],
        )
        repo.spec_done("auth-module")

        # Agent 2 works on payments (modifies shared.rs + creates pay.py)
        (tmp_path / "shared.rs").write_text("// base\n// payment import\n")
        (tmp_path / "pay.py").write_text("def charge(): pass\n")
        repo.seal(
            summary="payments",
            agent_id="agent-2",
            agent_type="agent",
            spec_id="payment-system",
            status="in-progress",
            paths=["shared.rs", "pay.py"],
        )
        repo.spec_done("payment-system")

        # Agent 3 works on dashboard (creates dash.js only, no overlap)
        (tmp_path / "dash.js").write_text("export function render() {}\n")
        repo.seal(
            summary="dashboard",
            agent_id="agent-3",
            agent_type="agent",
            spec_id="dashboard-ui",
            status="in-progress",
            paths=["dash.js"],
        )
        repo.spec_done("dashboard-ui")

        # Finalize and materialize
        report = repo.finalize_convergence()
        assert isinstance(report, dict)

        if report["shadow_results"]:
            repo.materialize_convergence(report)

        # All agent files should still exist
        assert (tmp_path / "auth.py").exists()
        assert (tmp_path / "pay.py").exists()
        assert (tmp_path / "dash.js").exists()
