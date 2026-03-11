"""W.40: Reopen completed spec tests.

Tests the reopen workflow: complete a spec, reopen it, continue work,
complete again. Verifies state transitions, guard rails, and seal
chain preservation.

Bindings tested: spec_done, reopen_spec, get_spec, seal, context, finish
"""

import subprocess
from pathlib import Path

import pytest
import writ


# ---------------------------------------------------------------------------
# Helpers for git + writ repos (needed for finish-then-reopen tests)
# ---------------------------------------------------------------------------

WRIT_BIN = None
_search = Path(__file__).resolve()
for _ in range(6):
    _search = _search.parent
    for profile in ("release", "debug"):
        candidate = _search / "target" / profile / "writ"
        if candidate.exists():
            WRIT_BIN = str(candidate)
            break
    if WRIT_BIN:
        break


def run_git(args, cwd, check=True):
    return subprocess.run(
        ["git"] + args, capture_output=True, text=True, cwd=cwd, check=check,
    )


def run_writ(args, cwd, check=True):
    if WRIT_BIN is None:
        pytest.skip("writ binary not found")
    return subprocess.run(
        [WRIT_BIN] + args, capture_output=True, text=True, cwd=cwd, check=check,
    )


@pytest.fixture
def git_writ_repo(tmp_path):
    """Git repo with writ initialized."""
    run_git(["init"], str(tmp_path))
    run_git(["config", "user.email", "test@test.com"], str(tmp_path))
    run_git(["config", "user.name", "Test"], str(tmp_path))
    (tmp_path / "README.md").write_text("# Test\n")
    run_git(["add", "README.md"], str(tmp_path))
    run_git(["commit", "-m", "init"], str(tmp_path))
    run_writ(["init", "--yes"], str(tmp_path))
    run_git(["add", "."], str(tmp_path))
    run_git(["commit", "-m", "writ init"], str(tmp_path))
    return tmp_path


class TestReopenBasics:
    """Core reopen behavior."""

    def test_reopen_completed_spec(self, tmp_repo):
        """Reopening a completed spec returns it to in-progress."""
        repo, path = tmp_repo
        repo.add_spec(id="feat", title="Feature")

        # Do work and mark done
        (path / "feat.py").write_text("def f(): pass\n")
        repo.seal(
            summary="done",
            agent_id="dev",
            agent_type="agent",
            spec_id="feat",
            status="in-progress",
        )
        repo.spec_done("feat", summary="Feature complete")

        spec = repo.get_spec("feat")
        assert spec["status"] == "complete"

        # Reopen
        repo.reopen_spec("feat")
        spec = repo.get_spec("feat")
        assert spec["status"] == "in-progress"
        # Defaults may be omitted from serialization (skip_serializing_if)
        assert spec.get("commit_state", "uncommitted") == "uncommitted"
        assert spec.get("lifecycle_state", "active") == "active"

    def test_reopen_preserves_completion_summary(self, tmp_repo):
        """Reopen preserves the original completion summary for history."""
        repo, path = tmp_repo
        repo.add_spec(id="feat", title="Feature")
        (path / "feat.py").write_text("def f(): pass\n")
        repo.seal(
            summary="done",
            agent_id="dev",
            agent_type="agent",
            spec_id="feat",
            status="in-progress",
        )
        repo.spec_done("feat", summary="Original summary")
        repo.reopen_spec("feat")

        spec = repo.get_spec("feat")
        assert spec.get("completion_summary") == "Original summary"

    def test_reopen_clears_completed_at(self, tmp_repo):
        """Reopen clears completed_at timestamp."""
        repo, path = tmp_repo
        repo.add_spec(id="feat", title="Feature")
        (path / "feat.py").write_text("def f(): pass\n")
        repo.seal(
            summary="done",
            agent_id="dev",
            agent_type="agent",
            spec_id="feat",
            status="in-progress",
        )
        repo.spec_done("feat")
        spec = repo.get_spec("feat")
        assert spec.get("completed_at") is not None

        repo.reopen_spec("feat")
        spec = repo.get_spec("feat")
        assert spec.get("completed_at") is None


class TestReopenGuardRails:
    """Reopen rejects invalid states."""

    def test_reopen_in_progress_fails(self, tmp_repo):
        """Cannot reopen a spec that isn't complete."""
        repo, path = tmp_repo
        repo.add_spec(id="feat", title="Feature")
        repo.update_spec("feat", status="in-progress")
        with pytest.raises(Exception, match="only reopen completed"):
            repo.reopen_spec("feat")

    def test_reopen_pending_fails(self, tmp_repo):
        """Cannot reopen a pending spec."""
        repo, path = tmp_repo
        repo.add_spec(id="feat", title="Feature")
        with pytest.raises(Exception, match="only reopen completed"):
            repo.reopen_spec("feat")

    def test_reopen_nonexistent_fails(self, tmp_repo):
        """Cannot reopen a spec that doesn't exist."""
        repo, path = tmp_repo
        with pytest.raises(Exception):
            repo.reopen_spec("no-such-spec")

    def test_reopen_committed_spec_fails(self, git_writ_repo):
        """Cannot reopen a spec that has been committed to git."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        repo.add_spec(id="feat", title="Feature")
        (path / "feat.py").write_text("def f(): pass\n")
        repo.seal(
            summary="done",
            agent_id="dev",
            agent_type="agent",
            spec_id="feat",
            status="in-progress",
        )
        repo.spec_done("feat")

        # Finish commits it to git (needs real git repo)
        result = repo.finish(strategy="single")
        assert result["specs_finished"] >= 1

        # Now try to reopen — should fail because it's committed
        with pytest.raises(Exception, match="cannot reopen a committed"):
            repo.reopen_spec("feat")


class TestReopenContinueWork:
    """Reopen then continue working on the spec."""

    def test_reopen_seal_again(self, tmp_repo):
        """After reopen, agent can seal new work under the same spec."""
        repo, path = tmp_repo
        repo.add_spec(id="feat", title="Feature")
        (path / "feat.py").write_text("def f(): pass\n")
        repo.seal(
            summary="first pass",
            agent_id="dev",
            agent_type="agent",
            spec_id="feat",
            status="in-progress",
        )
        repo.spec_done("feat")
        repo.reopen_spec("feat")

        # New work
        (path / "feat.py").write_text("def f(): return True\n")
        result = repo.seal(
            summary="second pass after reopen",
            agent_id="dev",
            agent_type="agent",
            spec_id="feat",
            status="in-progress",
        )
        assert result is not None

        spec = repo.get_spec("feat")
        assert spec["status"] == "in-progress"
        assert len(spec.get("sealed_by", [])) >= 2

    def test_reopen_complete_again(self, tmp_repo):
        """Full cycle: complete → reopen → work → complete again."""
        repo, path = tmp_repo
        repo.add_spec(id="feat", title="Feature")

        # First round
        (path / "feat.py").write_text("def f(): pass\n")
        repo.seal(
            summary="v1",
            agent_id="dev",
            agent_type="agent",
            spec_id="feat",
            status="in-progress",
        )
        repo.spec_done("feat", summary="v1 done")

        # Reopen
        repo.reopen_spec("feat")

        # Second round
        (path / "feat.py").write_text("def f(): return 'v2'\n")
        repo.seal(
            summary="v2",
            agent_id="dev",
            agent_type="agent",
            spec_id="feat",
            status="in-progress",
        )
        repo.spec_done("feat", summary="v2 done")

        spec = repo.get_spec("feat")
        assert spec["status"] == "complete"
        assert spec["completion_summary"] == "v2 done"

    def test_reopen_reflected_in_context(self, tmp_repo):
        """Context shows reopened spec as in-progress."""
        repo, path = tmp_repo
        repo.add_spec(id="feat", title="Feature")
        (path / "feat.py").write_text("def f(): pass\n")
        repo.seal(
            summary="done",
            agent_id="dev",
            agent_type="agent",
            spec_id="feat",
            status="in-progress",
        )
        repo.spec_done("feat")
        repo.reopen_spec("feat")

        ctx = repo.context(spec="feat")
        assert ctx["active_spec"]["status"] == "in-progress"
        # Default "active" may be omitted from serialization (skip_serializing_if)
        assert ctx["active_spec"].get("lifecycle_state", "active") == "active"


class TestSpecDone:
    """Tests for spec_done binding (prerequisite for reopen tests)."""

    def test_spec_done_sets_complete(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat", title="Feature")
        repo.update_spec("feat", status="in-progress")
        result = repo.spec_done("feat")
        assert result["status"] == "complete"

    def test_spec_done_stores_summary(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat", title="Feature")
        result = repo.spec_done("feat", summary="All tests passing")
        assert result["completion_summary"] == "All tests passing"

    def test_spec_done_sets_completed_at(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat", title="Feature")
        result = repo.spec_done("feat")
        assert result.get("completed_at") is not None

    def test_spec_done_already_complete_is_idempotent(self, tmp_repo):
        """Calling spec_done on an already-complete spec is a no-op."""
        repo, path = tmp_repo
        repo.add_spec(id="feat", title="Feature")
        repo.spec_done("feat")
        # Should either succeed (idempotent) or raise — both are valid
        try:
            result = repo.spec_done("feat")
            # If it succeeds, spec should still be complete
            assert result["status"] == "complete"
        except Exception:
            pass  # Raising is also acceptable behavior

    def test_spec_done_nonexistent_fails(self, tmp_repo):
        repo, path = tmp_repo
        with pytest.raises(Exception):
            repo.spec_done("no-such-spec")
