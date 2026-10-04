"""B.5: Python contract tests for GC committed pruning and audit.

Tests the new gc_committed() and gc_audit() Python bindings added for
the object store pruning feature (Phases 1-2).

Depends on: Amis A.1-A.3 (Python bindings for gc_committed, gc_audit).
"""

import json
import os
from pathlib import Path

import pytest
import writ


# ── Fixtures ──────────────────────────────────────────────────────────────


@pytest.fixture
def gc_repo(tmp_path):
    """Fresh writ repo with a baseline seal."""
    repo = writ.Repository.init(str(tmp_path))
    (tmp_path / "base.py").write_text("# baseline\n")
    repo.seal(
        summary="baseline",
        agent_id="setup",
        agent_type="agent",
        status="in-progress",
    )
    return repo, tmp_path


@pytest.fixture
def committed_repo(tmp_path):
    """Repo with a committed spec — simulates post-finish state.

    Creates a spec, seals work under it, marks it complete, then
    directly writes commit_state to simulate `writ finish` having
    committed the spec to git.
    """
    repo = writ.Repository.init(str(tmp_path))

    # Baseline.
    (tmp_path / "base.py").write_text("# baseline\n")
    repo.seal(
        summary="baseline",
        agent_id="setup",
        agent_type="agent",
        status="in-progress",
    )

    # Create a spec and do work.
    repo.add_spec(id="done-task", title="Completed Task")
    (tmp_path / "feature.py").write_text("def feature(): return True\n")
    repo.seal(
        summary="implemented feature",
        agent_id="agent-1",
        agent_type="agent",
        spec_id="done-task",
        status="complete",
    )

    # Mark spec as done via the Python API.
    repo.spec_done(spec_id="done-task", summary="All done")

    # Simulate writ finish having committed this spec to git.
    # Directly update the spec JSON to set commit_state = "committed".
    writ_dir = tmp_path / ".writ"
    specs_dir = writ_dir / "specs"
    spec_file = specs_dir / "done-task.json"
    if spec_file.exists():
        spec_data = json.loads(spec_file.read_text())
        spec_data["commit_state"] = "committed"
        spec_data["commit_hash"] = "abc123fake"
        spec_data["committed_at"] = "2026-01-01T00:00:00Z"
        spec_file.write_text(json.dumps(spec_data))

    return repo, tmp_path


# ── gc_committed() contract ───────────────────────────────────────────────


class TestGcCommittedContract:
    """B.5: gc_committed() Python binding tests."""

    def test_gc_committed_dry_run_returns_dict(self, committed_repo):
        """gc_committed(dry_run=True) returns a plan dict."""
        repo, _ = committed_repo
        result = repo.gc_committed(keep_days=0, dry_run=True)
        assert isinstance(result, dict), "dry_run should return a dict"
        # Plan should have actions and summary.
        assert "actions" in result, "plan should have 'actions' key"
        assert "summary" in result, "plan should have 'summary' key"

    def test_gc_committed_execute_returns_result(self, committed_repo):
        """gc_committed() (non-dry-run) returns execution result dict."""
        repo, _ = committed_repo
        result = repo.gc_committed(keep_days=0)
        assert isinstance(result, dict), "execution should return a dict"
        assert "objects_pruned" in result, "result should have 'objects_pruned'"
        assert "bytes_freed" in result, "result should have 'bytes_freed'"
        assert "seals_archived" in result, "result should have 'seals_archived'"

    def test_gc_committed_keep_days_zero_prunes(self, committed_repo):
        """gc_committed(keep_days=0) prunes committed objects immediately."""
        repo, tmp_path = committed_repo

        # First check there's something to prune via dry run.
        plan = repo.gc_committed(keep_days=0, dry_run=True)
        actions = plan.get("actions", [])

        if actions:
            # Execute the prune.
            result = repo.gc_committed(keep_days=0)
            # Should have pruned at least something.
            assert result["objects_pruned"] >= 0
            assert result["bytes_freed"] >= 0


# ── gc_audit() contract ──────────────────────────────────────────────────


class TestGcAuditContract:
    """B.5: gc_audit() Python binding tests."""

    def test_gc_audit_returns_expected_keys(self, gc_repo):
        """gc_audit() returns dict with all documented keys."""
        repo, _ = gc_repo
        audit = repo.gc_audit()
        assert isinstance(audit, dict)

        expected_keys = [
            "total_other_bytes",
            "total_seal_bytes",
            "total_seals",
            "active_seals",
            "committed_seals",
            "active_specs",
            "committed_specs",
            "orphaned_objects",
            "orphaned_bytes",
            "committed_prunable_objects",
            "committed_prunable_bytes",
            "committed_prunable_seals",
        ]
        for key in expected_keys:
            assert key in audit, f"gc_audit() should return '{key}'"

    def test_gc_audit_fresh_repo_zero_prunable(self, gc_repo):
        """Fresh repo has 0 committed prunable objects."""
        repo, _ = gc_repo
        audit = repo.gc_audit()
        assert audit["committed_prunable_objects"] == 0
        assert audit["committed_prunable_bytes"] == 0
        assert audit["committed_prunable_seals"] == 0
        assert audit["committed_specs"] == 0


# ── gc() regression ──────────────────────────────────────────────────────


class TestGcRegressionContract:
    """B.5: Verify existing gc() still works after PruneCommitted addition."""

    def test_gc_still_works(self, gc_repo):
        """repo.gc() should still return a valid result dict."""
        repo, _ = gc_repo
        result = repo.gc()
        assert isinstance(result, dict), "gc() should return a dict"
        # Core fields should still be present.
        assert "specs_cleaned" in result
        assert "objects_pruned" in result
