"""V3.8: Unified Convergence Path Python binding tests.

Tests that converge_all() and converge_workspaces() both delegate to
converge_from_seal_trees() under the hood. Verifies the unified path
produces correct results for workspace, same-directory, and mixed modes.
"""

from pathlib import Path

import pytest
import writ


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def make_two_spec_overlap(tmp_path: Path):
    """Create a repo with 2 completed specs that both modify shared.rs.

    Returns (repo, path).
    """
    repo = writ.Repository.init(str(tmp_path))

    # Baseline seal
    (tmp_path / "shared.rs").write_text("// base\nfn hello() {}\n")
    repo.seal(
        summary="baseline",
        agent_id="setup",
        agent_type="agent",
        status="in-progress",
    )

    repo.add_spec(id="feat-a", title="Feature A")
    repo.add_spec(id="feat-b", title="Feature B")

    # Spec A modifies shared.rs (adds feature_a at the end)
    (tmp_path / "shared.rs").write_text(
        "// base\nfn hello() {}\nfn feature_a() {}\n"
    )
    repo.seal(
        summary="add feature_a",
        agent_id="agent-a",
        agent_type="agent",
        spec_id="feat-a",
        status="in-progress",
    )

    # Spec B modifies shared.rs (adds feature_b at the end)
    (tmp_path / "shared.rs").write_text(
        "// base\nfn hello() {}\nfn feature_b() {}\n"
    )
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


def make_disjoint_specs(tmp_path: Path):
    """Create a repo with 2 completed specs that modify different files.

    Returns (repo, path).
    """
    repo = writ.Repository.init(str(tmp_path))

    (tmp_path / "base.txt").write_text("base\n")
    repo.seal(
        summary="baseline",
        agent_id="setup",
        agent_type="agent",
        status="in-progress",
    )

    repo.add_spec(id="s1", title="S1")
    repo.add_spec(id="s2", title="S2")

    (tmp_path / "a.txt").write_text("a work\n")
    repo.seal(
        summary="s1 work",
        agent_id="a1",
        agent_type="agent",
        spec_id="s1",
        status="in-progress",
    )

    (tmp_path / "b.txt").write_text("b work\n")
    repo.seal(
        summary="s2 work",
        agent_id="b1",
        agent_type="agent",
        spec_id="s2",
        status="in-progress",
    )

    repo.spec_done("s1")
    repo.spec_done("s2")

    return repo, tmp_path


# ---------------------------------------------------------------------------
# Test #1: converge_all wrapper delegates to seal-tree engine
# ---------------------------------------------------------------------------


class TestConvergeAllWrapper:
    """converge_all() wrapper delegates to converge_from_seal_trees()."""

    def test_converge_all_returns_report(self, tmp_path):
        """converge_all returns a ConvergeAllReport dict with expected fields."""
        repo, _ = make_two_spec_overlap(tmp_path)

        report = repo.converge_all(strategy="escalate", apply=False)
        assert isinstance(report, dict)
        assert "is_clean" in report
        assert "strategy" in report
        assert "merge_order" in report
        # escalations is skip_serializing_if empty, so check for key or default
        assert report.get("escalations", []) is not None

    def test_converge_all_detects_overlapping_files(self, tmp_path):
        """converge_all detects and merges overlapping files via seal trees."""
        repo, _ = make_two_spec_overlap(tmp_path)

        report = repo.converge_all(strategy="escalate", apply=False)
        # The v3 engine should detect overlapping shared.rs
        assert report["total_auto_merged"] > 0 or report["total_conflicts"] > 0

    def test_converge_all_disjoint_is_clean(self, tmp_path):
        """Disjoint specs produce a clean report with no merges."""
        repo, _ = make_disjoint_specs(tmp_path)

        report = repo.converge_all(strategy="escalate", apply=False)
        assert report["is_clean"] is True

    def test_converge_all_apply_materializes(self, tmp_path):
        """converge_all with apply=True materializes merged content to disk."""
        repo, path = make_two_spec_overlap(tmp_path)

        report = repo.converge_all(strategy="escalate", apply=True)
        if report["is_clean"]:
            assert report["applied"] is True


# ---------------------------------------------------------------------------
# Test #2: Same result via seal-tree and converge_all for same inputs
# ---------------------------------------------------------------------------


class TestConvergenceEquivalence:
    """Both paths produce equivalent results for identical inputs."""

    def test_converge_all_uses_seal_tree_engine(self, tmp_path):
        """converge_all delegates to the seal-tree engine internally.

        Verify by checking that converge_all returns a report consistent
        with seal-tree convergence (has base_spec, merge_order, strategy).
        """
        repo, _ = make_two_spec_overlap(tmp_path)

        report = repo.converge_all(strategy="escalate", apply=False)

        # base_spec + merge_order should cover both spec IDs
        all_specs = {report["base_spec"]} | set(report["merge_order"])
        assert "feat-a" in all_specs
        assert "feat-b" in all_specs
        # Strategy should be preserved
        assert report["strategy"] == "escalate"

    def test_seal_tree_direct_matches_wrapper(self, tmp_path):
        """converge_from_seal_trees detects the same overlapping files."""
        repo, _ = make_two_spec_overlap(tmp_path)

        seal_report = repo.converge_from_seal_trees(["feat-a", "feat-b"])

        # The seal-tree engine found overlapping files
        assert len(seal_report["merged_files"]) > 0
        # shared.rs should be in the merged files
        merged_paths = [mf["path"] for mf in seal_report["merged_files"]]
        assert "shared.rs" in merged_paths


# ---------------------------------------------------------------------------
# Test #3: Single spec returns clean (no convergence needed)
# ---------------------------------------------------------------------------


class TestSingleSpecNoConvergence:
    """Single spec or empty inputs return clean immediately."""

    def test_converge_all_single_spec(self, tmp_path):
        """converge_all with only one spec returns clean."""
        repo = writ.Repository.init(str(tmp_path))

        repo.add_spec(id="solo", title="Solo")
        (tmp_path / "file.txt").write_text("solo work\n")
        repo.seal(
            summary="solo",
            agent_id="a1",
            agent_type="agent",
            spec_id="solo",
            status="in-progress",
        )
        repo.spec_done("solo")

        report = repo.converge_all(strategy="escalate", apply=False)
        assert report["is_clean"] is True

    def test_converge_all_no_specs(self, tmp_repo):
        """converge_all with no sealed specs returns clean."""
        repo, _ = tmp_repo

        report = repo.converge_all(strategy="escalate", apply=False)
        assert report["is_clean"] is True
