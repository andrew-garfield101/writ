"""Tests for C.13 (seal enforcement) and C.14 (context token).

C.13: writ seal without --spec should error for agents, warn for humans.
C.14: writ context writes .context_token; writ seal warns if stale/missing + agent env.

These are CLI-level tests exercising the enforcement gates in main.rs.
"""

import os
import subprocess
import time
from pathlib import Path
from typing import Optional

import pytest


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


# Env vars that trigger agent detection in detect_agent_from_env().
AGENT_ENV = {"CLAUDE_CODE_SESSION_ID": "test-session-abc123"}

# Clean env with no agent vars set.
HUMAN_ENV_KEYS = [
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_SESSION_ID",
    "ANTHROPIC_SESSION_ID",
    "CODEX_SESSION",
    "CODEX_SESSION_ID",
    "WRIT_AGENT_ID",
]


def clean_env() -> dict:
    """Return os.environ copy with all agent env vars removed."""
    env = os.environ.copy()
    for key in HUMAN_ENV_KEYS:
        env.pop(key, None)
    return env


def agent_env() -> dict:
    """Return os.environ copy with agent env var set."""
    env = clean_env()
    env.update(AGENT_ENV)
    return env


def run_writ(
    args: list,
    cwd: str,
    env: Optional[dict] = None,
    check: bool = False,
) -> subprocess.CompletedProcess:
    if WRIT_BIN is None:
        pytest.skip("writ binary not found")
    return subprocess.run(
        [WRIT_BIN] + args,
        capture_output=True,
        text=True,
        cwd=cwd,
        env=env or clean_env(),
        check=check,
    )


def run_git(args: list, cwd: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git"] + args, capture_output=True, text=True, cwd=cwd, check=True,
    )


@pytest.fixture
def writ_repo(tmp_path):
    """Git repo with writ init'd, ready for seal tests."""
    path = tmp_path
    run_git(["init"], str(path))
    run_git(["config", "user.email", "test@test.com"], str(path))
    run_git(["config", "user.name", "Test User"], str(path))
    (path / "README.md").write_text("# Test\n")
    run_git(["add", "README.md"], str(path))
    run_git(["commit", "-m", "initial"], str(path))
    run_writ(["init", "--yes"], str(path))
    return path


# ---------------------------------------------------------------------------
# C.13: Seal enforcement — agents must link seals to specs
# ---------------------------------------------------------------------------

class TestSealEnforcementC13:
    """C.13: writ seal without --spec gates agents but not humans."""

    def test_agent_no_spec_errors(self, writ_repo):
        """Agent env + no --spec = exit 1 with actionable error."""
        path = writ_repo
        (path / "app.py").write_text("print('hello')\n")

        result = run_writ(
            ["seal", "-s", "test work"],
            str(path),
            env=agent_env(),
        )
        assert result.returncode != 0
        combined = result.stdout + result.stderr
        assert "spec" in combined.lower()

    def test_agent_with_spec_succeeds(self, writ_repo):
        """Agent env + --spec provided = seal succeeds."""
        path = writ_repo
        run_writ(
            ["spec", "add", "--id", "feat-1", "--title", "Feature 1"],
            str(path),
        )
        (path / "app.py").write_text("print('hello')\n")

        result = run_writ(
            ["seal", "-s", "test work", "--spec", "feat-1"],
            str(path),
            env=agent_env(),
        )
        assert result.returncode == 0

    def test_human_no_spec_rejected(self, writ_repo):
        """No agent env + no --spec = error (C.13 enforces spec for all seals)."""
        path = writ_repo
        (path / "app.py").write_text("print('hello')\n")

        result = run_writ(
            ["seal", "-s", "human work"],
            str(path),
            env=clean_env(),
        )
        assert result.returncode != 0
        combined = result.stdout + result.stderr
        assert "spec" in combined.lower()

    def test_human_with_spec_no_warning(self, writ_repo):
        """No agent env + spec provided = no warning, seal succeeds."""
        path = writ_repo
        run_writ(
            ["spec", "add", "--id", "feat-2", "--title", "Feature 2"],
            str(path),
        )
        (path / "app.py").write_text("print('hello')\n")

        result = run_writ(
            ["seal", "-s", "human work", "--spec", "feat-2"],
            str(path),
            env=clean_env(),
        )
        assert result.returncode == 0
        combined = result.stdout + result.stderr
        # Should not have the no-spec warning
        assert "no spec" not in combined.lower() or "no active spec" not in combined.lower()

    def test_explicit_agent_flag_alone_still_enforces(self, writ_repo):
        """--agent flag without --spec still enforces (C.13: all seals need spec)."""
        path = writ_repo
        (path / "app.py").write_text("print('hello')\n")

        result = run_writ(
            ["seal", "-s", "manual agent", "--agent", "my-agent"],
            str(path),
            env=clean_env(),
        )
        # C.13 enforces spec for all seals regardless of agent env
        assert result.returncode != 0
        combined = result.stdout + result.stderr
        assert "spec" in combined.lower()

    def test_agent_error_message_is_actionable(self, writ_repo):
        """Error message includes the exact command to run."""
        path = writ_repo
        (path / "app.py").write_text("print('hello')\n")

        result = run_writ(
            ["seal", "-s", "test work"],
            str(path),
            env=agent_env(),
        )
        combined = result.stdout + result.stderr
        assert "writ spec add" in combined


# ---------------------------------------------------------------------------
# C.14: Context token — writ context writes token, seal checks it
# ---------------------------------------------------------------------------

class TestContextTokenC14:
    """C.14: context token freshness gate."""

    def test_context_creates_token(self, writ_repo):
        """writ context creates .writ/.context_token file."""
        path = writ_repo
        token_path = path / ".writ" / ".context_token"
        assert not token_path.exists()

        run_writ(["context"], str(path))
        assert token_path.exists()

        # Token should contain a unix timestamp
        content = token_path.read_text().strip()
        ts = int(content)
        assert ts > 0

    def test_seal_with_recent_token_no_warning(self, writ_repo):
        """Seal with recent context token = no staleness warning."""
        path = writ_repo
        run_writ(
            ["spec", "add", "--id", "feat-3", "--title", "Feature 3"],
            str(path),
        )

        # Run context first to create fresh token
        run_writ(["context"], str(path), env=agent_env())

        (path / "app.py").write_text("print('hello')\n")
        result = run_writ(
            ["seal", "-s", "test work", "--spec", "feat-3"],
            str(path),
            env=agent_env(),
        )
        assert result.returncode == 0
        combined = result.stdout + result.stderr
        assert "no `writ context`" not in combined.lower()

    def test_seal_with_stale_token_agent_warns(self, writ_repo):
        """Seal with stale token (>4h) + agent env = warning."""
        path = writ_repo
        run_writ(
            ["spec", "add", "--id", "feat-4", "--title", "Feature 4"],
            str(path),
        )

        # Write a stale token (5 hours ago)
        token_path = path / ".writ" / ".context_token"
        stale_time = int(time.time()) - (5 * 3600)
        token_path.write_text(str(stale_time))

        (path / "app.py").write_text("print('hello')\n")
        result = run_writ(
            ["seal", "-s", "test work", "--spec", "feat-4"],
            str(path),
            env=agent_env(),
        )
        assert result.returncode == 0
        combined = result.stdout + result.stderr
        assert "context" in combined.lower()

    def test_seal_with_missing_token_agent_warns(self, writ_repo):
        """Seal with no token + agent env = warning."""
        path = writ_repo
        run_writ(
            ["spec", "add", "--id", "feat-5", "--title", "Feature 5"],
            str(path),
        )

        # Ensure no token exists
        token_path = path / ".writ" / ".context_token"
        if token_path.exists():
            token_path.unlink()

        (path / "app.py").write_text("print('hello')\n")
        result = run_writ(
            ["seal", "-s", "test work", "--spec", "feat-5"],
            str(path),
            env=agent_env(),
        )
        assert result.returncode == 0
        combined = result.stdout + result.stderr
        assert "context" in combined.lower()

    def test_seal_with_missing_token_human_shows_warning(self, writ_repo):
        """Seal with no token + no agent env = context warning shown."""
        path = writ_repo
        run_writ(
            ["spec", "add", "--id", "feat-6", "--title", "Feature 6"],
            str(path),
        )

        # Ensure no token exists
        token_path = path / ".writ" / ".context_token"
        if token_path.exists():
            token_path.unlink()

        (path / "app.py").write_text("print('hello')\n")
        result = run_writ(
            ["seal", "-s", "human work", "--spec", "feat-6"],
            str(path),
            env=clean_env(),
        )
        # Seal succeeds but now warns everyone (not just agents) about missing context
        assert result.returncode == 0
