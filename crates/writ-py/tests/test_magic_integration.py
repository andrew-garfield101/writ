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
        plan_result = repo.plan([
            "Implement auth endpoint",
            "Add payment processing",
            "Build dashboard UI",
        ])

        auth_id = plan_result[0]["spec_id"]
        payments_id = plan_result[1]["spec_id"]
        dashboard_id = plan_result[2]["spec_id"]

        # 2. Agent-1 claims and works on auth.
        repo.spec_claim(auth_id, "agent-1")
        (path / "auth.py").write_text("def login(): return True\n")
        repo.seal(
            summary="auth endpoint",
            agent_id="agent-1",
            agent_type="agent",
            spec_id=auth_id,
            status="in-progress",
        )

        # 3. Agent-2 claims and works on payments.
        repo.spec_claim(payments_id, "agent-2")
        (path / "payments.py").write_text("def charge(): return True\n")
        repo.seal(
            summary="payment processing",
            agent_id="agent-2",
            agent_type="agent",
            spec_id=payments_id,
            status="in-progress",
        )

        # 4. Context shows remaining unclaimed spec.
        ctx = repo.context()
        unclaimed = ctx.get("unclaimed_specs", [])
        unclaimed_ids = [s["id"] for s in unclaimed]
        assert dashboard_id in unclaimed_ids

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
        # spec_id is now a hash, not a slug
        spec_id = task["spec_id"]
        assert len(spec_id) == 12
        assert Path(task["workspace_path"]).exists()

        # Seal via workspace using the returned spec ID.
        (Path(task["workspace_path"]) / "api.py").write_text("# api\n")
        repo.seal(
            summary="api work",
            agent_id="ws-agent",
            agent_type="agent",
            spec_id=spec_id,
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


class TestAutoScoping:
    """SK.3b / PY.2: Auto-scoping for seal() and spec_done()."""

    def test_seal_auto_scopes_to_claimed_spec(self, tmp_repo):
        """seal() without spec_id auto-scopes to agent's single claimed spec."""
        repo, path = tmp_repo
        plan_result = repo.plan(["Auth feature"])
        spec_id = plan_result[0]["spec_id"]
        repo.spec_claim(spec_id, "agent-1")

        (path / "auth.py").write_text("def login(): pass\n")
        # First seal WITH spec_id to make it InProgress.
        repo.seal(
            summary="initial work",
            agent_id="agent-1",
            agent_type="agent",
            spec_id=spec_id,
            status="in-progress",
        )

        # Second seal WITHOUT spec_id — should auto-scope.
        (path / "auth.py").write_text("def login(): return True\n")
        result = repo.seal(
            summary="completed login",
            agent_id="agent-1",
            agent_type="agent",
            status="in-progress",
        )
        assert result is not None
        assert result.get("spec_id") == spec_id

    def test_seal_graceful_fallback_no_claimed_spec(self, tmp_repo):
        """seal() without spec_id and no claimed spec seals with spec_id=None."""
        repo, path = tmp_repo
        (path / "readme.txt").write_text("hello\n")
        # Agent with no claimed spec — should NOT error, just seal without spec.
        result = repo.seal(
            summary="exploratory work",
            agent_id="agent-1",
            agent_type="agent",
            status="in-progress",
        )
        assert result is not None
        # spec_id should be None/absent since no spec was claimed.
        assert result.get("spec_id") is None

    def test_seal_graceful_fallback_multiple_specs(self, tmp_repo):
        """seal() without spec_id and 2+ claimed specs falls back gracefully."""
        repo, path = tmp_repo
        plan_result = repo.plan(["Auth", "Payments"])
        repo.spec_claim(plan_result[0]["spec_id"], "agent-1")
        repo.spec_claim(plan_result[1]["spec_id"], "agent-1")

        # Seal both to make InProgress.
        (path / "a.py").write_text("a\n")
        repo.seal(summary="a", agent_id="agent-1", agent_type="agent",
                  spec_id=plan_result[0]["spec_id"], status="in-progress")
        (path / "b.py").write_text("b\n")
        repo.seal(summary="b", agent_id="agent-1", agent_type="agent",
                  spec_id=plan_result[1]["spec_id"], status="in-progress")

        # Now seal without spec_id — should fall back, not error.
        (path / "c.py").write_text("c\n")
        result = repo.seal(
            summary="ambiguous work",
            agent_id="agent-1",
            agent_type="agent",
            status="in-progress",
        )
        assert result is not None

    def test_spec_done_auto_scopes_for_agent(self, tmp_repo):
        """spec_done() without spec_id auto-scopes to agent's claimed spec."""
        repo, path = tmp_repo
        plan_result = repo.plan(["Auth feature"])
        spec_id = plan_result[0]["spec_id"]
        repo.spec_claim(spec_id, "agent-1")

        (path / "auth.py").write_text("def login(): pass\n")
        repo.seal(summary="auth work", agent_id="agent-1", agent_type="agent",
                  spec_id=spec_id, status="in-progress")

        # spec_done without spec_id, with agent_id — should auto-scope.
        result = repo.spec_done(agent_id="agent-1", summary="Auth complete")
        assert result is not None
        assert result["id"] == spec_id
        assert result["status"].lower() == "complete"

    def test_spec_done_requires_spec_id_for_human(self, tmp_repo):
        """spec_done() without spec_id for human agent raises error."""
        repo, path = tmp_repo
        with pytest.raises(Exception, match="spec_id required"):
            repo.spec_done()

    def test_resolve_spec_for_agent_direct(self, tmp_repo):
        """resolve_spec_for_agent() returns the claimed spec ID."""
        repo, path = tmp_repo
        plan_result = repo.plan(["Auth feature"])
        spec_id = plan_result[0]["spec_id"]
        repo.spec_claim(spec_id, "agent-1")

        (path / "auth.py").write_text("def login(): pass\n")
        repo.seal(summary="auth", agent_id="agent-1", agent_type="agent",
                  spec_id=spec_id, status="in-progress")

        resolved = repo.resolve_spec_for_agent("agent-1")
        assert resolved == spec_id
