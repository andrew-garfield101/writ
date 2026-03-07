"""W.37/W.48/W.B2: Finish strategies, backward compat, and per-spec review.

Tests the enhanced finish flow via Python bindings:
- W.37: Per-spec commit strategy produces correct git history
- W.48: finish with default args matches old --yes behavior
- W.B2: finish with per-spec strategy (foundation for --full per-spec review)

Bindings tested: finish, spec_done, get_spec, seal, context
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
    for profile in ("release", "debug"):
        candidate = _search / "target" / profile / "writ"
        if candidate.exists():
            WRIT_BIN = str(candidate)
            break
    if WRIT_BIN:
        break


def run_git(args: list, cwd: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git"] + args, capture_output=True, text=True, cwd=cwd, check=check,
    )


def run_writ(args: list, cwd: str, check: bool = True) -> subprocess.CompletedProcess:
    if WRIT_BIN is None:
        pytest.skip("writ binary not found")
    return subprocess.run(
        [WRIT_BIN] + args, capture_output=True, text=True, cwd=cwd, check=check,
    )


@pytest.fixture
def git_writ_repo(tmp_path):
    """Git repo with writ initialized and baseline commit."""
    run_git(["init"], str(tmp_path))
    run_git(["config", "user.email", "test@test.com"], str(tmp_path))
    run_git(["config", "user.name", "Test User"], str(tmp_path))
    (tmp_path / "README.md").write_text("# Project\n")
    run_git(["add", "README.md"], str(tmp_path))
    run_git(["commit", "-m", "initial commit"], str(tmp_path))
    run_writ(["init", "--yes"], str(tmp_path))
    run_git(["add", "."], str(tmp_path))
    run_git(["commit", "-m", "writ init"], str(tmp_path))
    return tmp_path


def _setup_two_complete_specs(path, repo):
    """Helper: create 2 specs, seal work, mark done."""
    repo.add_spec(id="auth", title="Auth")
    repo.update_spec("auth", file_scope=["auth.py"])
    repo.add_spec(id="api", title="API")
    repo.update_spec("api", file_scope=["api.py"])

    (path / "auth.py").write_text("def login(): pass\n")
    repo.seal(
        summary="auth impl",
        agent_id="dev-a",
        agent_type="agent",
        spec_id="auth",
        status="in-progress",
    )
    repo.spec_done("auth", summary="Auth module complete")

    (path / "api.py").write_text("def routes(): pass\n")
    repo.seal(
        summary="api impl",
        agent_id="dev-b",
        agent_type="agent",
        spec_id="api",
        status="in-progress",
    )
    repo.spec_done("api", summary="API routes complete")


# ---------------------------------------------------------------------------
# W.37: Per-spec commit strategy
# ---------------------------------------------------------------------------

class TestPerSpecStrategy:
    """Per-spec strategy produces one git commit per spec."""

    def test_per_spec_two_commits(self, git_writ_repo):
        """Two complete specs → two separate git commits."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _setup_two_complete_specs(path, repo)

        result = repo.finish(strategy="per-spec")
        assert result["strategy"] == "per-spec"
        assert result["specs_finished"] == 2
        assert len(result["commits"]) == 2

        # Each commit should reference its spec
        spec_ids = {c["specs"][0] for c in result["commits"]}
        assert "auth" in spec_ids
        assert "api" in spec_ids

    def test_per_spec_commit_messages(self, git_writ_repo):
        """Per-spec commits use spec ID + completion summary as message."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _setup_two_complete_specs(path, repo)

        result = repo.finish(strategy="per-spec")
        messages = {c["specs"][0]: c["message"] for c in result["commits"]}
        assert "auth" in messages["auth"]
        assert "api" in messages["api"]

    def test_per_spec_marks_committed(self, git_writ_repo):
        """Per-spec finish marks each spec as committed."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _setup_two_complete_specs(path, repo)

        repo.finish(strategy="per-spec")

        auth = repo.get_spec("auth")
        api = repo.get_spec("api")
        assert auth["commit_state"] == "committed"
        assert api["commit_state"] == "committed"
        assert auth.get("commit_hash") is not None
        assert api.get("commit_hash") is not None

    def test_per_spec_git_history(self, git_writ_repo):
        """Git log shows separate commits for each spec."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _setup_two_complete_specs(path, repo)

        repo.finish(strategy="per-spec")

        log = run_git(["log", "--oneline"], str(path))
        lines = [l for l in log.stdout.strip().split("\n") if l.strip()]
        # 2 init commits + 2 per-spec commits = 4
        assert len(lines) >= 4

    def test_per_spec_dry_run(self, git_writ_repo):
        """Per-spec dry run shows planned commits without executing."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _setup_two_complete_specs(path, repo)

        result = repo.finish(strategy="per-spec", dry_run=True)
        assert result["dry_run"] is True
        assert result["specs_finished"] == 2
        assert len(result["commits"]) == 2
        # All hashes should be dry-run placeholders
        for c in result["commits"]:
            assert c["hash"] == "(dry-run)"

        # Specs should NOT be marked committed
        auth = repo.get_spec("auth")
        assert auth.get("commit_state", "uncommitted") == "uncommitted"

    def test_per_spec_no_committable_specs(self, git_writ_repo):
        """Per-spec with no committable specs returns empty result."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        repo.add_spec(id="wip", title="WIP")
        repo.update_spec("wip", status="in-progress")

        result = repo.finish(strategy="per-spec")
        assert result["specs_finished"] == 0
        assert result["commits"] == []

    def test_per_spec_with_spec_filter(self, git_writ_repo):
        """Finish only specific specs by ID."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _setup_two_complete_specs(path, repo)

        result = repo.finish(strategy="per-spec", specs=["auth"])
        assert result["specs_finished"] == 1
        assert result["commits"][0]["specs"] == ["auth"]

        # api should still be uncommitted
        api = repo.get_spec("api")
        assert api.get("commit_state", "uncommitted") == "uncommitted"


# ---------------------------------------------------------------------------
# W.37: Single strategy (default)
# ---------------------------------------------------------------------------

class TestSingleStrategy:
    """Single strategy: all specs in one commit."""

    def test_single_one_commit(self, git_writ_repo):
        """Two specs → one commit."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _setup_two_complete_specs(path, repo)

        result = repo.finish(strategy="single")
        assert result["strategy"] == "single"
        assert result["specs_finished"] == 2
        assert len(result["commits"]) == 1
        assert set(result["commits"][0]["specs"]) == {"auth", "api"}

    def test_single_marks_all_committed(self, git_writ_repo):
        """Single commit marks all specs as committed with same hash."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _setup_two_complete_specs(path, repo)

        result = repo.finish(strategy="single")
        commit_hash = result["commits"][0]["hash"]

        auth = repo.get_spec("auth")
        api = repo.get_spec("api")
        assert auth["commit_state"] == "committed"
        assert api["commit_state"] == "committed"
        assert auth["commit_hash"] == commit_hash
        assert api["commit_hash"] == commit_hash

    def test_single_custom_message(self, git_writ_repo):
        """Single commit with custom message."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _setup_two_complete_specs(path, repo)

        result = repo.finish(strategy="single", message="feat: big release")
        assert result["commits"][0]["message"] == "feat: big release"

    def test_single_dry_run(self, git_writ_repo):
        """Single dry run shows one planned commit."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _setup_two_complete_specs(path, repo)

        result = repo.finish(strategy="single", dry_run=True)
        assert result["dry_run"] is True
        assert len(result["commits"]) == 1
        assert set(result["commits"][0]["specs"]) == {"auth", "api"}

    def test_invalid_strategy_fails(self, tmp_repo):
        """Unknown strategy raises error."""
        repo, path = tmp_repo
        with pytest.raises(Exception, match="unknown strategy"):
            repo.finish(strategy="grouped")


# ---------------------------------------------------------------------------
# W.48: Backward compatibility (--yes equivalent)
# ---------------------------------------------------------------------------

class TestBackwardCompat:
    """Default finish behavior matches old --yes: all specs, single commit."""

    def test_default_strategy_is_single(self, git_writ_repo):
        """Calling finish() with no args uses single strategy."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _setup_two_complete_specs(path, repo)

        result = repo.finish()
        assert result["strategy"] == "single"
        assert len(result["commits"]) == 1

    def test_finish_includes_all_committable(self, git_writ_repo):
        """Default finish includes all committable specs."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _setup_two_complete_specs(path, repo)

        # Also add a WIP spec that should NOT be included
        repo.add_spec(id="wip", title="WIP")
        (path / "wip.py").write_text("# wip\n")
        repo.seal(
            summary="wip",
            agent_id="dev",
            agent_type="agent",
            spec_id="wip",
            status="in-progress",
        )

        result = repo.finish()
        assert result["specs_finished"] == 2
        finished_ids = set(result["commits"][0]["specs"])
        assert "auth" in finished_ids
        assert "api" in finished_ids
        assert "wip" not in finished_ids

    def test_finish_no_committable_returns_empty(self, git_writ_repo):
        """Finish with nothing committable returns zero specs."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        result = repo.finish()
        assert result["specs_finished"] == 0
        assert result["commits"] == []

    def test_cli_finish_yes_flag(self, git_writ_repo):
        """CLI writ finish --yes works."""
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

        result = run_writ(["finish", "--yes"], str(path))
        assert result.returncode == 0


# ---------------------------------------------------------------------------
# W.B2: Per-spec commit review (foundation)
# ---------------------------------------------------------------------------

class TestPerSpecReview:
    """Per-spec finish with detailed messages for review."""

    def test_per_spec_uses_completion_summary(self, git_writ_repo):
        """Per-spec commit message includes the completion summary."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))

        repo.add_spec(id="feat", title="Big Feature")
        (path / "feat.py").write_text("def f(): pass\n")
        repo.seal(
            summary="impl",
            agent_id="dev",
            agent_type="agent",
            spec_id="feat",
            status="in-progress",
        )
        repo.spec_done("feat", summary="Implemented the big feature with tests")

        result = repo.finish(strategy="per-spec")
        msg = result["commits"][0]["message"]
        assert "Implemented the big feature with tests" in msg

    def test_per_spec_falls_back_to_title(self, git_writ_repo):
        """Per-spec without summary falls back to spec title."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))

        repo.add_spec(id="feat", title="Quick Fix")
        (path / "fix.py").write_text("def fix(): pass\n")
        repo.seal(
            summary="fixed",
            agent_id="dev",
            agent_type="agent",
            spec_id="feat",
            status="in-progress",
        )
        repo.spec_done("feat")  # No summary

        result = repo.finish(strategy="per-spec")
        msg = result["commits"][0]["message"]
        assert "Quick Fix" in msg

    def test_per_spec_three_specs_three_commits(self, git_writ_repo):
        """Three specs produce three commits with individual messages."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))

        specs = [
            ("auth", "Auth System", "auth.py", "Secure auth with JWT"),
            ("api", "REST API", "api.py", "CRUD endpoints"),
            ("tests", "Test Suite", "tests.py", "Full coverage"),
        ]

        for spec_id, title, filename, summary in specs:
            repo.add_spec(id=spec_id, title=title)
            repo.update_spec(spec_id, file_scope=[filename])
            (path / filename).write_text(f"# {spec_id}\n")
            repo.seal(
                summary=f"{spec_id} work",
                agent_id="dev",
                agent_type="agent",
                spec_id=spec_id,
                status="in-progress",
            )
            repo.spec_done(spec_id, summary=summary)

        result = repo.finish(strategy="per-spec")
        assert result["specs_finished"] == 3
        assert len(result["commits"]) == 3

        messages = {c["specs"][0]: c["message"] for c in result["commits"]}
        assert "Secure auth with JWT" in messages["auth"]
        assert "CRUD endpoints" in messages["api"]
        assert "Full coverage" in messages["tests"]
