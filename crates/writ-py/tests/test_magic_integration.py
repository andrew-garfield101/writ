"""MS.22: Integration tests — full Magic Sprint workflow.

End-to-end tests combining spec-scoped sealing, writ plan,
and backward compatibility. These validate the complete user journey.
"""

import subprocess
from pathlib import Path

import pytest
import writ


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

WRIT_BIN = None
_search = Path(__file__).resolve()
for _ in range(6):
    _search = _search.parent
    candidates = []
    for profile in ("release", "debug"):
        candidate = _search / "target" / profile / "writ"
        if candidate.exists():
            candidates.append(candidate)
    if candidates:
        # Prefer most recently built binary
        candidates.sort(key=lambda p: p.stat().st_mtime, reverse=True)
        WRIT_BIN = str(candidates[0])
        break


def run_writ(args: list, cwd: Path, check: bool = True):
    """Run writ CLI command."""
    if WRIT_BIN is None:
        pytest.skip("writ binary not found")
    return subprocess.run(
        [WRIT_BIN] + args,
        capture_output=True, text=True, cwd=cwd, check=check,
    )


def run_git(args: list, cwd: Path, check: bool = True):
    """Run git command."""
    return subprocess.run(
        ["git"] + args,
        capture_output=True, text=True, cwd=cwd, check=check,
    )


@pytest.fixture
def git_writ_repo(tmp_path):
    """Git repo with writ initialized."""
    run_git(["init"], tmp_path)
    run_git(["config", "user.email", "test@test.com"], tmp_path)
    run_git(["config", "user.name", "Test"], tmp_path)
    (tmp_path / "README.md").write_text("# Test\n")
    run_git(["add", "README.md"], tmp_path)
    run_git(["commit", "-m", "init"], tmp_path)
    run_writ(["init", "--yes"], tmp_path)
    run_git(["add", "."], tmp_path)
    run_git(["commit", "-m", "writ init"], tmp_path)
    return tmp_path


# ---------------------------------------------------------------------------
# MS.22: Integration Tests
# ---------------------------------------------------------------------------


class TestMagicWorkflowIntegration:
    """Full magic workflow: plan → claim → seal → context."""

    def test_full_magic_workflow(self, tmp_repo):
        """Plan specs → agents claim → agents seal → verify context."""
        repo, path = tmp_repo

        # 1. Human creates specs via plan.
        repo.plan([
            "Implement auth endpoint",
            "Add payment processing",
            "Build dashboard UI",
        ])

        # 2. Agent-1 claims and works on auth.
        repo.spec_claim("implement-auth-endpoint", "agent-1")
        (path / "auth.py").write_text("def login(): return True\n")
        repo.seal(
            summary="auth endpoint",
            agent_id="agent-1",
            agent_type="agent",
            spec_id="implement-auth-endpoint",
            status="in-progress",
        )

        # 3. Agent-2 claims and works on payments.
        repo.spec_claim("add-payment-processing", "agent-2")
        (path / "payments.py").write_text("def charge(): return True\n")
        repo.seal(
            summary="payment processing",
            agent_id="agent-2",
            agent_type="agent",
            spec_id="add-payment-processing",
            status="in-progress",
        )

        # 4. Context shows remaining unclaimed spec.
        ctx = repo.context()
        unclaimed = ctx.get("unclaimed_specs", [])
        unclaimed_ids = [s["id"] for s in unclaimed]
        assert "build-dashboard-ui" in unclaimed_ids

        # 5. Both agents' files exist (same directory).
        assert (path / "auth.py").exists()
        assert (path / "payments.py").exists()


class TestBackwardCompatibility:
    """Existing workflows must not break."""

    def test_single_agent_no_spec_still_works(self, tmp_repo):
        """Legacy single-agent workflow without specs or workspaces."""
        repo, path = tmp_repo

        (path / "app.py").write_text("print('hello')\n")
        result = repo.seal(
            summary="simple change",
            agent_id="solo",
            agent_type="agent",
            status="in-progress",
        )
        assert result is not None
        assert result.get("summary") == "simple change"

    def test_workspace_workflow_still_works(self, tmp_repo):
        """Workspace-based workflow (v1) continues to function."""
        repo, path = tmp_repo

        task = repo.create_task("Backend API", None)
        assert task["spec_id"] == "backend-api"
        assert Path(task["workspace_path"]).exists()

        # Seal via workspace.
        (Path(task["workspace_path"]) / "api.py").write_text("# api\n")
        repo.seal(
            summary="api work",
            agent_id="ws-agent",
            agent_type="agent",
            spec_id="backend-api",
            status="in-progress",
        )


class TestMixedWorkflow:
    """Mixed workspace + same-directory scenarios."""

    def test_workspace_and_same_directory_agents_coexist(self, tmp_repo):
        """Some agents use workspaces, others work in same directory."""
        repo, path = tmp_repo

        # Agent-1 uses workspace (v1 style).
        task = repo.create_task("Backend API", None)

        # Agent-2 works in same directory (magic style).
        repo.add_spec(id="frontend", title="Frontend")
        (path / "frontend.js").write_text("// frontend\n")
        repo.seal(
            summary="frontend work",
            agent_id="agent-2",
            agent_type="agent",
            spec_id="frontend",
            status="in-progress",
        )

        # Both workflows should coexist.
        assert Path(task["workspace_path"]).exists()
        assert (path / "frontend.js").exists()
