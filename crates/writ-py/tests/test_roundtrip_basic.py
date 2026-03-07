"""W.35/W.47/W.B1: Basic round-trip, human workflow, and re-modification tests.

Tests the existing writ round-trip flow end-to-end:
  init → seal → context → finish → git log

Uses CLI subprocess for finish (which shells out to git).
Tests are structured to extend as enhanced finish (W.6), per-spec
commits (W.8), and propose/auto modes (W.11/W.13) land.

Covers:
- W.35 (partial): Basic round-trip integration
- W.47 (partial): Human-only workflow (no agents, manual seal + finish)
- W.44 (partial): Edge case — no changes to commit
- W.46 (partial): Edge case — finish without git
- W.B1: Committed spec re-modified by new spec
"""

import json
import os
import subprocess
from pathlib import Path

import pytest
import writ


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# Auto-discover the writ CLI binary
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
    """Run the writ CLI binary."""
    if WRIT_BIN is None:
        pytest.skip("writ binary not found in target/release or target/debug")
    return subprocess.run(
        [WRIT_BIN] + args,
        capture_output=True,
        text=True,
        cwd=cwd,
        check=check,
    )


def run_git(args: list, cwd: str, check: bool = True) -> subprocess.CompletedProcess:
    """Run a git command."""
    return subprocess.run(
        ["git"] + args,
        capture_output=True,
        text=True,
        cwd=cwd,
        check=check,
    )


@pytest.fixture
def git_writ_repo(tmp_path):
    """Create a git repo with writ initialized and a baseline commit."""
    # Init git
    run_git(["init"], str(tmp_path))
    run_git(["config", "user.email", "test@test.com"], str(tmp_path))
    run_git(["config", "user.name", "Test User"], str(tmp_path))

    # Initial commit so git isn't in an empty state
    (tmp_path / "README.md").write_text("# Test Project\n")
    run_git(["add", "README.md"], str(tmp_path))
    run_git(["commit", "-m", "initial commit"], str(tmp_path))

    # Init writ
    run_writ(["init", "--yes"], str(tmp_path))

    # Commit the .writ directory so finish doesn't include it as noise
    run_git(["add", "."], str(tmp_path))
    run_git(["commit", "-m", "writ init"], str(tmp_path))

    return tmp_path


# ---------------------------------------------------------------------------
# W.47: Human-only workflow
# ---------------------------------------------------------------------------

class TestHumanOnlyWorkflow:
    """Manual seal + finish workflow, no agents involved."""

    def test_basic_seal_and_finish(self, git_writ_repo):
        """Seal work, then finish creates a git commit."""
        path = git_writ_repo

        # Do some work
        (path / "app.py").write_text("def main(): print('hello')\n")

        # Seal the work
        run_writ(
            ["seal", "-s", "added app module", "--agent", "human-dev"],
            str(path),
        )

        # Finish — should create a git commit
        result = run_writ(["finish"], str(path))
        assert result.returncode == 0
        assert "committed" in result.stdout.lower() or "commit" in result.stdout.lower()

        # Verify git log has the commit
        log = run_git(["log", "--oneline", "-1"], str(path))
        assert log.returncode == 0
        # The commit message should contain something from the summary
        assert len(log.stdout.strip()) > 0

    def test_finish_with_full_message(self, git_writ_repo):
        """finish --full uses the complete summary as commit message."""
        path = git_writ_repo

        (path / "models.py").write_text("class User: pass\n")
        run_writ(
            ["seal", "-s", "added user model", "--agent", "human-dev"],
            str(path),
        )

        result = run_writ(["finish", "--full"], str(path))
        assert result.returncode == 0

    def test_finish_dry_run(self, git_writ_repo):
        """finish --dry-run with completed spec shows plan without committing."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))

        repo.add_spec(id="feat", title="Feature")
        (path / "utils.py").write_text("def helper(): pass\n")
        repo.seal(
            summary="added utils",
            agent_id="human-dev",
            agent_type="human",
            spec_id="feat",
            status="in-progress",
        )
        repo.spec_done("feat")

        result = run_writ(["finish", "--dry-run"], str(path))
        assert result.returncode == 0

        # Verify no actual git commit was created
        log = run_git(["log", "--oneline", "-1"], str(path))
        assert "writ init" in log.stdout  # Still the last commit

    def test_multiple_seals_single_finish(self, git_writ_repo):
        """Multiple seals under a spec, one finish — all work in one commit."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))

        repo.add_spec(id="api", title="API Module")

        (path / "api.py").write_text("def get_users(): pass\n")
        repo.seal(
            summary="added api module",
            agent_id="human-dev",
            agent_type="human",
            spec_id="api",
            status="in-progress",
        )

        (path / "api.py").write_text("def get_users(): return []\ndef create_user(): pass\n")
        repo.seal(
            summary="expanded api",
            agent_id="human-dev",
            agent_type="human",
            spec_id="api",
            status="in-progress",
        )

        repo.spec_done("api", summary="API module complete")

        result = run_writ(["finish"], str(path))
        assert result.returncode == 0

        # Should be one commit for all the work
        log = run_git(["log", "--oneline"], str(path))
        lines = [l for l in log.stdout.strip().split("\n") if l.strip()]
        # 3 commits: initial, writ init, finish
        assert len(lines) == 3


# ---------------------------------------------------------------------------
# W.35: Basic round-trip integration (partial — extended when W.6 lands)
# ---------------------------------------------------------------------------

class TestBasicRoundTrip:
    """init → seal → context → finish → verify."""

    def test_init_seal_context_finish(self, git_writ_repo):
        """Full basic round-trip flow."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))

        # Add a spec
        repo.add_spec(id="auth", title="Auth Feature")

        # Seal work under the spec
        (path / "auth.py").write_text("def login(user, pw): return True\n")
        repo.seal(
            summary="implemented login",
            agent_id="dev-agent",
            agent_type="agent",
            spec_id="auth",
            status="in-progress",
        )

        # Check context shows the work
        ctx = repo.context()
        recent = ctx.get("recent_seals", [])
        assert len(recent) >= 1
        specs = ctx.get("all_specs", [])
        auth = next((s for s in specs if s["id"] == "auth"), None)
        assert auth is not None
        assert auth["status"] == "in-progress"

        # Final seal
        (path / "auth_test.py").write_text("def test_login(): assert True\n")
        repo.seal(
            summary="auth complete with tests",
            agent_id="dev-agent",
            agent_type="agent",
            spec_id="auth",
            status="complete",
        )

        # Verify spec is complete
        spec = repo.get_spec("auth")
        assert spec["status"] == "complete"

        # Finish
        result = run_writ(["finish"], str(path))
        assert result.returncode == 0

        # Verify git has the commit
        log = run_git(["log", "--oneline", "-1"], str(path))
        assert len(log.stdout.strip()) > 0

    def test_context_reflects_sealed_work(self, git_writ_repo):
        """Context output includes seal history and spec state."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))

        repo.add_spec(id="feat-x", title="Feature X")
        (path / "feat_x.py").write_text("def x(): pass\n")
        repo.seal(
            summary="started feature x",
            agent_id="agent-x",
            agent_type="agent",
            spec_id="feat-x",
            status="in-progress",
        )

        ctx = repo.context(spec="feat-x")
        assert ctx.get("active_spec") is not None
        assert ctx["active_spec"]["id"] == "feat-x"


# ---------------------------------------------------------------------------
# W.44: Edge — nothing to commit
# ---------------------------------------------------------------------------

class TestFinishEdgeCases:
    """Edge cases for the finish command."""

    def test_finish_nothing_to_commit(self, git_writ_repo):
        """Finish with no completed specs exits 0 with helpful message."""
        path = git_writ_repo
        result = run_writ(["finish"], str(path), check=False)
        assert result.returncode == 0
        assert "nothing to commit" in result.stdout.lower()

    def test_finish_dry_run_no_changes(self, git_writ_repo):
        """Dry run with no completed specs shows nothing-to-commit message."""
        path = git_writ_repo
        # Seal something but don't mark any spec done
        run_writ(
            ["seal", "-s", "checkpoint", "--agent", "human-dev"],
            str(path),
        )
        result = run_writ(["finish", "--dry-run"], str(path))
        assert result.returncode == 0
        assert "nothing to commit" in result.stdout.lower()


# ---------------------------------------------------------------------------
# W.46: Edge — finish without git
# ---------------------------------------------------------------------------

class TestFinishWithoutGit:
    """Finish behavior when git is not available or not initialized."""

    def test_finish_outside_git_repo(self, tmp_path):
        """Finish outside a git repo handles gracefully."""
        # Init writ without git
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "test.py").write_text("x = 1\n")
        repo.seal(
            summary="work outside git",
            agent_id="test",
            agent_type="agent",
        )
        result = run_writ(["finish"], str(tmp_path), check=False)
        # Should not crash — either errors about git or says nothing to commit
        combined = result.stdout + result.stderr
        assert "git" in combined.lower() or "nothing" in combined.lower()


# ---------------------------------------------------------------------------
# W.35 extended: Spec lifecycle through finish (partial)
# ---------------------------------------------------------------------------

class TestSpecThroughFinish:
    """Spec state after finish — foundational for when W.6 adds spec tracking."""

    def test_spec_still_complete_after_finish(self, git_writ_repo):
        """After finish, spec status should still be complete."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))

        repo.add_spec(id="feat", title="Feature")
        (path / "feat.py").write_text("def f(): pass\n")
        repo.seal(
            summary="done",
            agent_id="dev",
            agent_type="agent",
            spec_id="feat",
            status="complete",
        )

        run_writ(["finish"], str(path))

        spec = repo.get_spec("feat")
        assert spec["status"] == "complete"
        # Current finish doesn't update commit_state — that's W.6
        # Just verify the spec is still readable after git operations
        assert spec.get("lifecycle_state", "active") == "active"

    def test_multiple_specs_through_finish(self, git_writ_repo):
        """Multiple specs, all sealed, then finish."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))

        repo.add_spec(id="feat-a", title="Feature A")
        repo.add_spec(id="feat-b", title="Feature B")

        (path / "a.py").write_text("def a(): pass\n")
        repo.seal(
            summary="feat a done",
            agent_id="agent-a",
            agent_type="agent",
            spec_id="feat-a",
            status="complete",
        )

        (path / "b.py").write_text("def b(): pass\n")
        repo.seal(
            summary="feat b done",
            agent_id="agent-b",
            agent_type="agent",
            spec_id="feat-b",
            status="complete",
        )

        result = run_writ(["finish"], str(path))
        assert result.returncode == 0

        # Both files should be in the git commit
        log = run_git(["diff", "--name-only", "HEAD~1..HEAD"], str(path))
        committed_files = log.stdout.strip().split("\n")
        assert "a.py" in committed_files
        assert "b.py" in committed_files


# ---------------------------------------------------------------------------
# W.B1: Committed spec re-modified by new spec
# ---------------------------------------------------------------------------

class TestCommittedSpecReModified:
    """After finish, new spec modifies same files → new round of work."""

    def test_two_rounds_of_finish(self, git_writ_repo):
        """Finish round 1, then new work modifies same file, finish round 2."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))

        # Round 1: spec-v1 creates app.py
        repo.add_spec(id="v1", title="Initial feature")
        (path / "app.py").write_text("def handler(): return 'v1'\n")
        repo.seal(
            summary="v1 complete",
            agent_id="agent-a",
            agent_type="agent",
            spec_id="v1",
            status="complete",
        )
        run_writ(["finish"], str(path))

        # Verify round 1 committed
        log1 = run_git(["log", "--oneline"], str(path))
        commit_count_1 = len(log1.stdout.strip().split("\n"))

        # Round 2: spec-v2 modifies the same app.py
        repo.add_spec(id="v2", title="Enhancement")
        (path / "app.py").write_text("def handler(): return 'v2'\ndef helper(): pass\n")
        repo.seal(
            summary="v2 enhancement",
            agent_id="agent-b",
            agent_type="agent",
            spec_id="v2",
            status="complete",
        )
        run_writ(["finish"], str(path))

        # Verify round 2 created a new commit
        log2 = run_git(["log", "--oneline"], str(path))
        commit_count_2 = len(log2.stdout.strip().split("\n"))
        assert commit_count_2 == commit_count_1 + 1

        # Verify the file has the v2 content
        content = (path / "app.py").read_text()
        assert "v2" in content
        assert "helper" in content

    def test_three_rounds_accumulating_changes(self, git_writ_repo):
        """Three rounds of spec → seal → finish, building on each other."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))

        rounds = [
            ("r1", "models.py", "class User: pass\n"),
            ("r2", "models.py", "class User: pass\nclass Product: pass\n"),
            ("r3", "models.py", "class User: pass\nclass Product: pass\nclass Order: pass\n"),
        ]

        for spec_id, filename, content in rounds:
            repo.add_spec(id=spec_id, title=f"Round {spec_id}")
            (path / filename).write_text(content)
            repo.seal(
                summary=f"{spec_id} done",
                agent_id="dev",
                agent_type="agent",
                spec_id=spec_id,
                status="complete",
            )
            result = run_writ(["finish"], str(path))
            assert result.returncode == 0

        # Final content should have all 3 classes
        final = (path / "models.py").read_text()
        assert "User" in final
        assert "Product" in final
        assert "Order" in final

        # Git log should show 3 finish commits + init commits
        log = run_git(["log", "--oneline"], str(path))
        lines = log.stdout.strip().split("\n")
        assert len(lines) >= 5  # 2 init + 3 finish

    def test_remodified_file_in_new_spec_shows_in_context(self, git_writ_repo):
        """After finish, new spec's work on same file shows in context."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))

        # Round 1
        repo.add_spec(id="v1", title="V1")
        (path / "shared.py").write_text("# v1\n")
        repo.seal(
            summary="v1",
            agent_id="dev",
            agent_type="agent",
            spec_id="v1",
            status="complete",
        )
        run_writ(["finish"], str(path))

        # Round 2: new spec modifies same file
        repo.add_spec(id="v2", title="V2")
        (path / "shared.py").write_text("# v2 - modified\n")
        repo.seal(
            summary="v2 modifies shared",
            agent_id="dev",
            agent_type="agent",
            spec_id="v2",
            status="in-progress",
        )

        # Context should show v2 spec and the modification
        ctx = repo.context(spec="v2")
        assert ctx.get("active_spec") is not None
        assert ctx["active_spec"]["id"] == "v2"
