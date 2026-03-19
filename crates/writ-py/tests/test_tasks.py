"""WV.11: Task creation tests.

Tests for `writ task` / `repo.create_task()` — the one-shot command that creates
a spec + workspace + gitignore entry in a single call.
"""

import json
import os
import subprocess
from pathlib import Path
from typing import Optional

import pytest

try:
    import writ
    HAS_WRIT_BINDINGS = True
except ImportError:
    HAS_WRIT_BINDINGS = False


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

WRIT_BIN = None
_search = Path(__file__).resolve()
for _ in range(6):
    _search = _search.parent
    _candidates = []
    for profile in ("release", "debug"):
        candidate = _search / "target" / profile / "writ"
        if candidate.exists():
            _candidates.append(candidate)
    if _candidates:
        # Prefer the most recently built binary.
        WRIT_BIN = str(max(_candidates, key=lambda p: p.stat().st_mtime))
        break


def run_writ(
    args: list,
    cwd: str,
    env: Optional[dict] = None,
    check: bool = True,
) -> subprocess.CompletedProcess:
    if WRIT_BIN is None:
        pytest.skip("writ binary not found")
    return subprocess.run(
        [WRIT_BIN] + args,
        capture_output=True,
        text=True,
        cwd=cwd,
        env=env,
        check=check,
    )


def run_git(args: list, cwd: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git"] + args, capture_output=True, text=True, cwd=cwd, check=True,
    )


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture
def writ_repo(tmp_path):
    """Git repo with writ init'd."""
    path = tmp_path
    run_git(["init"], str(path))
    run_git(["config", "user.email", "test@test.com"], str(path))
    run_git(["config", "user.name", "Test User"], str(path))
    (path / "README.md").write_text("# Project\n")
    run_git(["add", "."], str(path))
    run_git(["commit", "-m", "initial"], str(path))
    run_writ(["init", "--yes"], str(path))
    return path


# ---------------------------------------------------------------------------
# CLI tests
# ---------------------------------------------------------------------------

class TestTaskCLI:
    """Tests using the writ CLI binary."""

    def test_create_task_basic(self, writ_repo):
        """Basic task creation produces spec + workspace."""
        result = run_writ(
            ["task", "Add user login", "--format", "json"],
            str(writ_repo),
        )
        data = json.loads(result.stdout)
        # spec_id is now a 12-char hex hash (auto-generated from title)
        assert len(data["spec_id"]) == 12
        assert all(c in "0123456789abcdef" for c in data["spec_id"])
        assert data["title"] == "Add user login"
        assert data["workspace_name"] == "add-user-login"
        assert "workspace_path" in data
        assert "suggested_prompt" in data

        # Verify spec actually exists.
        spec_result = run_writ(
            ["spec", "show", "add-user-login"],
            str(writ_repo),
        )
        assert spec_result.returncode == 0

        # Verify workspace directory was created.
        ws_path = Path(data["workspace_path"])
        assert ws_path.exists(), f"workspace dir should exist at {ws_path}"

    def test_create_task_custom_id(self, writ_repo):
        """--id override replaces the auto-derived slug."""
        result = run_writ(
            ["task", "Add user login", "--id", "login-v2", "--format", "json"],
            str(writ_repo),
        )
        data = json.loads(result.stdout)
        assert data["spec_id"] == "login-v2"
        assert data["workspace_name"] == "login-v2"

    def test_create_task_slugify(self, writ_repo):
        """Title with special chars is correctly slugified."""
        result = run_writ(
            ["task", "Fix bug #42 — URGENT!", "--format", "json"],
            str(writ_repo),
        )
        data = json.loads(result.stdout)
        slug = data["spec_id"]
        # Should be lowercase, no special chars, hyphens for separators.
        assert slug == slug.lower()
        assert all(c.isalnum() or c == "-" for c in slug)
        assert "--" not in slug
        assert not slug.startswith("-")
        assert not slug.endswith("-")

    def test_create_task_workspace_at_project_root(self, writ_repo):
        """Workspace directory is at workspaces/<id>, not .writ/ws/<id>."""
        result = run_writ(
            ["task", "build dashboard", "--format", "json"],
            str(writ_repo),
        )
        data = json.loads(result.stdout)
        ws_path = Path(data["workspace_path"])

        # Should be under <repo>/workspaces/, not <repo>/.writ/ws/.
        assert "workspaces" in ws_path.parts
        assert ".writ" not in str(ws_path) or "ws" not in ws_path.parts

    def test_create_task_gitignore_updated(self, writ_repo):
        """workspaces/ is added to .gitignore after task creation."""
        run_writ(["task", "setup auth"], str(writ_repo))

        gitignore = writ_repo / ".gitignore"
        assert gitignore.exists(), ".gitignore should exist"
        content = gitignore.read_text()
        assert "workspaces/" in content

    def test_create_task_duplicate_title(self, writ_repo):
        """Creating a task with the same title/id fails gracefully."""
        run_writ(["task", "add caching"], str(writ_repo))

        # Second call with same title should fail.
        result = run_writ(
            ["task", "add caching"],
            str(writ_repo),
            check=False,
        )
        assert result.returncode != 0


# ---------------------------------------------------------------------------
# Python binding tests
# ---------------------------------------------------------------------------

@pytest.mark.skipif(not HAS_WRIT_BINDINGS, reason="writ Python bindings not available")
class TestTaskPythonBindings:
    """Tests using the writ Python API."""

    def test_create_task_basic(self, writ_repo):
        """Python create_task returns expected dict."""
        repo = writ.Repository.open(str(writ_repo))
        result = repo.create_task("Add search feature")

        # spec_id is now a 12-char hex hash (auto-generated from title)
        assert len(result["spec_id"]) == 12
        assert all(c in "0123456789abcdef" for c in result["spec_id"])
        assert result["title"] == "Add search feature"
        assert result["workspace_name"] == "add-search-feature"
        assert "workspace_path" in result
        assert "suggested_prompt" in result

    def test_create_task_custom_id(self, writ_repo):
        """Python create_task with id override."""
        repo = writ.Repository.open(str(writ_repo))
        result = repo.create_task("Add search feature", id="search-v3")

        assert result["spec_id"] == "search-v3"
        assert result["workspace_name"] == "search-v3"

    def test_create_task_context_shows_task(self, writ_repo):
        """After task creation, context from workspace shows task field."""
        repo = writ.Repository.open(str(writ_repo))
        result = repo.create_task("implement API")

        # Get context scoped to the task's spec.
        ctx = repo.context(spec=result["spec_id"])

        # Context should have the task's spec listed.
        assert ctx is not None

    def test_create_task_duplicate_raises(self, writ_repo):
        """Creating duplicate task raises an error."""
        repo = writ.Repository.open(str(writ_repo))
        repo.create_task("build pipeline")

        with pytest.raises(Exception):
            repo.create_task("build pipeline")
