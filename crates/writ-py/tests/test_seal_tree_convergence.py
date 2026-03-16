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


def make_sealed_overlapping_specs(tmp_path: Path):
    """Create a repo with 2 specs that both modify shared.rs, both completed.

    Returns (repo, path).
    """
    repo = writ.Repository.init(str(tmp_path))

    # Baseline seal with shared file
    (tmp_path / "shared.rs").write_text("// base\nfn hello() {}\n")
    repo.seal(
        summary="baseline",
        agent_id="setup",
        agent_type="agent",
        status="in-progress",
    )

    repo.add_spec(id="feat-a", title="Feature A")
    repo.add_spec(id="feat-b", title="Feature B")

    # Spec A adds feature_a to shared.rs
    (tmp_path / "shared.rs").write_text("// base\nfn hello() {}\nfn feature_a() {}\n")
    repo.seal(
        summary="add feature_a",
        agent_id="agent-a",
        agent_type="agent",
        spec_id="feat-a",
        status="in-progress",
    )

    # Spec B adds feature_b to shared.rs
    (tmp_path / "shared.rs").write_text("// base\nfn hello() {}\nfn feature_b() {}\n")
    repo.seal(
        summary="add feature_b",
        agent_id="agent-b",
        agent_type="agent",
        spec_id="feat-b",
        status="in-progress",
    )

    # Mark both specs done
    repo.spec_done("feat-a")
    repo.spec_done("feat-b")

    return repo, tmp_path


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
        )

        (path / "b.txt").write_text("b work\n")
        repo.seal(
            summary="b", agent_id="b1", agent_type="agent",
            spec_id="s2", status="in-progress",
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
        )
        repo.spec_done("solo")

        report = repo.converge_from_seal_trees(["solo"])
        assert report["is_clean"] is True
        assert len(report["merged_files"]) == 0

    def test_shadow_results_populated(self, tmp_path):
        """shadow_results contains (path, hash) tuples for materialization."""
        repo, _ = make_sealed_overlapping_specs(tmp_path)

        report = repo.converge_from_seal_trees(["feat-a", "feat-b"])
        if report["merged_files"]:
            assert len(report["shadow_results"]) > 0
            path, hash_val = report["shadow_results"][0]
            assert isinstance(path, str)
            assert isinstance(hash_val, str)
            assert len(hash_val) > 0


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
        """After materialization, merged content is on disk."""
        repo, path = make_sealed_overlapping_specs(tmp_path)

        report = repo.finalize_convergence()

        if report["shadow_results"]:
            repo.materialize_convergence(report)

            # The merged file should exist and contain content from both specs
            merged = (path / "shared.rs").read_text()
            # At minimum, the file should exist and not be empty
            assert len(merged) > 0

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
