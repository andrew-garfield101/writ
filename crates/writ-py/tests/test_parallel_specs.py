"""W.36/W.42: Parallel specs and multi-agent workflow tests.

Tests multiple agents working in parallel on separate specs,
then finishing via a single commit.

Covers:
- W.36 (partial): Parallel specs, single commit via finish
- W.42 (partial): 5-agent parallel workflow with status + finish
- Foundation for W.51 (parallel specs single commit) when W.6 lands

Current test scope uses existing basic finish (git add . && git commit).
Tests will be extended when enhanced finish with per-spec/grouped
strategies lands (W.6, W.8).
"""

import subprocess
from pathlib import Path

import pytest
import writ


# ---------------------------------------------------------------------------
# Helpers (shared with test_roundtrip_basic.py)
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


def run_writ(args: list, cwd: str, check: bool = True) -> subprocess.CompletedProcess:
    if WRIT_BIN is None:
        pytest.skip("writ binary not found")
    return subprocess.run(
        [WRIT_BIN] + args,
        capture_output=True,
        text=True,
        cwd=cwd,
        check=check,
    )


def run_git(args: list, cwd: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git"] + args,
        capture_output=True,
        text=True,
        cwd=cwd,
        check=check,
    )


@pytest.fixture
def git_writ_repo(tmp_path):
    """Git repo with writ initialized and baseline commit."""
    run_git(["init"], str(tmp_path))
    run_git(["config", "user.email", "test@test.com"], str(tmp_path))
    run_git(["config", "user.name", "Test User"], str(tmp_path))
    (tmp_path / "README.md").write_text("# Multi-Agent Project\n")
    run_git(["add", "README.md"], str(tmp_path))
    run_git(["commit", "-m", "initial commit"], str(tmp_path))
    run_writ(["init", "--yes"], str(tmp_path))
    run_git(["add", "."], str(tmp_path))
    run_git(["commit", "-m", "writ init"], str(tmp_path))
    return tmp_path


# ---------------------------------------------------------------------------
# W.36: Parallel specs, single commit
# ---------------------------------------------------------------------------

class TestParallelSpecsSingleCommit:
    """Multiple agents work on separate specs, finish bundles into one commit."""

    def test_two_agents_disjoint_files(self, git_writ_repo):
        """Two agents on separate specs, touching different files."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))

        repo.add_spec(id="backend", title="Backend API")
        repo.add_spec(id="frontend", title="Frontend UI")

        # Agent A works on backend
        (path / "api.py").write_text("def get_users(): return []\n")
        repo.seal(
            summary="backend api",
            agent_id="agent-a",
            agent_type="agent",
            spec_id="backend",
            status="complete",
        )

        # Agent B works on frontend
        (path / "app.tsx").write_text("export default function App() { return <div/>; }\n")
        repo.seal(
            summary="frontend shell",
            agent_id="agent-b",
            agent_type="agent",
            spec_id="frontend",
            status="complete",
        )

        # Finish — both specs in one commit
        result = run_writ(["finish"], str(path))
        assert result.returncode == 0

        # Both files committed
        log = run_git(["diff", "--name-only", "HEAD~1..HEAD"], str(path))
        files = log.stdout.strip().split("\n")
        assert "api.py" in files
        assert "app.tsx" in files

    def test_three_agents_some_shared_files(self, git_writ_repo):
        """Three agents, some file overlap, single finish."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))

        repo.add_spec(id="models", title="Data Models")
        repo.add_spec(id="routes", title="API Routes")
        repo.add_spec(id="tests", title="Test Suite")

        # Agent 1: models
        (path / "models.py").write_text("class User:\n    name: str\n")
        repo.seal(
            summary="added User model",
            agent_id="model-agent",
            agent_type="agent",
            spec_id="models",
            status="complete",
        )

        # Agent 2: routes (uses models)
        (path / "routes.py").write_text("from models import User\ndef get_user(): pass\n")
        repo.seal(
            summary="added routes",
            agent_id="route-agent",
            agent_type="agent",
            spec_id="routes",
            status="complete",
        )

        # Agent 3: tests
        (path / "test_models.py").write_text("def test_user(): assert True\n")
        repo.seal(
            summary="added tests",
            agent_id="test-agent",
            agent_type="agent",
            spec_id="tests",
            status="complete",
        )

        result = run_writ(["finish"], str(path))
        assert result.returncode == 0

        log = run_git(["diff", "--name-only", "HEAD~1..HEAD"], str(path))
        files = log.stdout.strip().split("\n")
        assert "models.py" in files
        assert "routes.py" in files
        assert "test_models.py" in files


# ---------------------------------------------------------------------------
# W.42: 5-agent parallel workflow
# ---------------------------------------------------------------------------

class TestFiveAgentParallel:
    """5 agents work in parallel on separate specs."""

    def test_five_agents_disjoint_specs(self, git_writ_repo):
        """5 agents each on their own spec, all seal complete, then finish."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))

        agents = [
            ("auth", "agent-auth", "auth.py", "def login(): pass\n"),
            ("db", "agent-db", "database.py", "def connect(): pass\n"),
            ("api", "agent-api", "api.py", "def routes(): pass\n"),
            ("ui", "agent-ui", "frontend.tsx", "export default function UI() {}\n"),
            ("deploy", "agent-deploy", "deploy.sh", "#!/bin/bash\necho deploy\n"),
        ]

        for spec_id, agent_id, filename, content in agents:
            repo.add_spec(id=spec_id, title=f"Spec {spec_id}")
            (path / filename).write_text(content)
            repo.seal(
                summary=f"{spec_id} work done",
                agent_id=agent_id,
                agent_type="agent",
                spec_id=spec_id,
                status="complete",
            )

        # Verify all 5 specs are complete
        ctx = repo.context()
        specs = ctx.get("all_specs", [])
        complete_specs = [s for s in specs if s["status"] == "complete"]
        assert len(complete_specs) == 5

        # Verify context shows agent activity for all 5
        activity = ctx.get("agent_activity", [])
        agent_ids = {a["agent_id"] for a in activity}
        for _, agent_id, _, _ in agents:
            assert agent_id in agent_ids

        # Finish
        result = run_writ(["finish"], str(path))
        assert result.returncode == 0

        # All 5 files in the commit
        log = run_git(["diff", "--name-only", "HEAD~1..HEAD"], str(path))
        files = set(log.stdout.strip().split("\n"))
        for _, _, filename, _ in agents:
            assert filename in files, f"{filename} not in commit"

    def test_five_agents_mixed_status(self, git_writ_repo):
        """5 agents, but only 3 are complete. Finish still works."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))

        # 3 complete specs
        for i in range(3):
            spec_id = f"done-{i}"
            repo.add_spec(id=spec_id, title=f"Done {i}")
            (path / f"done_{i}.py").write_text(f"# done {i}\n")
            repo.seal(
                summary=f"finished {spec_id}",
                agent_id=f"agent-{i}",
                agent_type="agent",
                spec_id=spec_id,
                status="complete",
            )

        # 2 in-progress specs
        for i in range(2):
            spec_id = f"wip-{i}"
            repo.add_spec(id=spec_id, title=f"WIP {i}")
            (path / f"wip_{i}.py").write_text(f"# wip {i}\n")
            repo.seal(
                summary=f"working on {spec_id}",
                agent_id=f"agent-wip-{i}",
                agent_type="agent",
                spec_id=spec_id,
                status="in-progress",
            )

        # Current finish commits everything (doesn't filter by status)
        result = run_writ(["finish"], str(path))
        assert result.returncode == 0

        # All files committed (current behavior — W.6 may change this)
        log = run_git(["diff", "--name-only", "HEAD~1..HEAD"], str(path))
        files = set(log.stdout.strip().split("\n"))
        assert len(files) >= 5

    def test_context_shows_all_agent_activity(self, git_writ_repo):
        """Context accurately tracks 5 agents' work."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))

        for i in range(5):
            spec_id = f"spec-{i}"
            repo.add_spec(id=spec_id, title=f"Feature {i}")
            (path / f"module_{i}.py").write_text(f"def fn_{i}(): pass\n")
            repo.seal(
                summary=f"agent {i} work",
                agent_id=f"agent-{i}",
                agent_type="agent",
                spec_id=spec_id,
                status="in-progress",
            )

        ctx = repo.context()

        # All 5 specs visible
        specs = ctx.get("all_specs", [])
        assert len(specs) == 5

        # All 5 agents tracked
        activity = ctx.get("agent_activity", [])
        assert len(activity) >= 5

        # Recent seals should include work from all agents
        recent = ctx.get("recent_seals", [])
        assert len(recent) >= 5

    def test_lifecycle_complete_then_finish(self, git_writ_repo):
        """Complete specs through lifecycle, then finish."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))

        for i in range(3):
            spec_id = f"feat-{i}"
            repo.add_spec(id=spec_id, title=f"Feature {i}")
            (path / f"feat_{i}.py").write_text(f"def feat_{i}(): pass\n")
            repo.seal(
                summary=f"feature {i} complete",
                agent_id=f"dev-{i}",
                agent_type="agent",
                spec_id=spec_id,
                status="complete",
            )
            # Lifecycle complete
            repo.complete_spec(spec_id)

        # Verify lifecycle state
        for i in range(3):
            spec = repo.get_spec(f"feat-{i}")
            assert spec["lifecycle_state"] == "completed"
            assert spec.get("commit_state", "uncommitted") == "uncommitted"

        # Finish
        result = run_writ(["finish"], str(path))
        assert result.returncode == 0
