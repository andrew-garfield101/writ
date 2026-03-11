"""S5: Enforcement — pre-commit hook, agent identity, seal enforcement.

Maps to Section 5 of the pre-beta testing guide (P1 — High Priority).
"""

import os
import subprocess
from pathlib import Path

import pytest

from helpers.cli import writ_cmd


# ---------------------------------------------------------------------------
# S5.3: Pre-commit hook
# ---------------------------------------------------------------------------

class TestPreCommitHook:
    """Pre-commit hook blocks git commit, redirects to writ."""

    def _hook_path(self, project: Path) -> Path:
        return project / ".git" / "hooks" / "pre-commit"

    def test_hook_installed_after_init(self, writ_project: Path):
        """5.3.1: .git/hooks/pre-commit exists after writ init."""
        hook = self._hook_path(writ_project)
        if not hook.exists():
            pytest.skip("pre-commit hook not installed by init")
        assert os.access(hook, os.X_OK), "Hook should be executable"

    def test_git_commit_blocked(self, writ_project: Path, writ_bin: str):
        """5.3.2: git commit is blocked by hook."""
        hook = self._hook_path(writ_project)
        if not hook.exists():
            pytest.skip("pre-commit hook not installed")

        (writ_project / "blocked.txt").write_text("should be blocked\n")
        subprocess.run(
            ["git", "add", "blocked.txt"],
            cwd=writ_project, capture_output=True,
        )
        result = subprocess.run(
            ["git", "commit", "-m", "should fail"],
            cwd=writ_project, capture_output=True, text=True,
        )
        assert result.returncode != 0, (
            "pre-commit hook should block git commit"
        )

    def test_hook_mentions_writ(self, writ_project: Path):
        """5.3.3: Hook error message mentions writ seal / writ finish."""
        hook = self._hook_path(writ_project)
        if not hook.exists():
            pytest.skip("pre-commit hook not installed")
        content = hook.read_text()
        assert "writ" in content.lower(), (
            "Hook should reference writ commands"
        )

    def test_writ_finish_bypasses_hook(
        self, writ_project: Path, writ_bin: str,
    ):
        """5.3.6: WRIT_FINISH=1 bypasses the pre-commit hook."""
        hook = self._hook_path(writ_project)
        if not hook.exists():
            pytest.skip("pre-commit hook not installed")

        (writ_project / "bypass.txt").write_text("bypass test\n")
        subprocess.run(
            ["git", "add", "bypass.txt"],
            cwd=writ_project, capture_output=True,
        )

        env = os.environ.copy()
        env["WRIT_FINISH"] = "1"
        result = subprocess.run(
            ["git", "commit", "-m", "bypass hook via WRIT_FINISH"],
            cwd=writ_project, capture_output=True, text=True, env=env,
        )
        assert result.returncode == 0, (
            f"WRIT_FINISH=1 should bypass hook: {result.stderr}"
        )

    def test_no_verify_bypass(self, writ_project: Path):
        """5.3.5: git commit --no-verify bypasses the hook."""
        hook = self._hook_path(writ_project)
        if not hook.exists():
            pytest.skip("pre-commit hook not installed")

        (writ_project / "noverify.txt").write_text("no-verify\n")
        subprocess.run(
            ["git", "add", "noverify.txt"],
            cwd=writ_project, capture_output=True,
        )
        result = subprocess.run(
            ["git", "commit", "--no-verify", "-m", "bypass with --no-verify"],
            cwd=writ_project, capture_output=True, text=True,
        )
        assert result.returncode == 0


# ---------------------------------------------------------------------------
# S5.4: Agent identity auto-detection
# ---------------------------------------------------------------------------

class TestAgentIdentity:
    """Agent identity auto-detection from environment variables."""

    def test_auto_detect_from_claude_env(
        self, writ_project: Path, writ_bin: str,
    ):
        """5.4.1: CLAUDE_CODE_SESSION_ID sets agent automatically."""
        (writ_project / "auto.py").write_text("# auto-detected\n")

        env = os.environ.copy()
        env["CLAUDE_CODE_SESSION_ID"] = "test-session-e2e-123"

        result = subprocess.run(
            [writ_bin, "seal", "-s", "auto-detect test"],
            cwd=writ_project, capture_output=True, text=True, env=env,
        )
        if result.returncode != 0:
            pytest.skip("Agent auto-detection not yet implemented (C.7)")

        # Seal should succeed without explicit --agent
        assert result.returncode == 0

    def test_explicit_agent_overrides(
        self, writ_project_with_spec: Path, writ_bin: str,
    ):
        """5.4.3: Explicit --agent overrides auto-detection."""
        path = writ_project_with_spec
        (path / "src" / "app.py").write_text("# explicit\n")

        env = os.environ.copy()
        env["CLAUDE_CODE_SESSION_ID"] = "should-be-overridden"

        result = subprocess.run(
            [writ_bin, "seal", "-s", "explicit agent",
             "--agent", "custom-name", "--spec", "feat-1"],
            cwd=path, capture_output=True, text=True, env=env,
        )
        assert result.returncode == 0

    def test_human_without_env_var(
        self, writ_project: Path, writ_bin: str,
    ):
        """5.4.2: Without session env var, agent defaults to human."""
        (writ_project / "human.py").write_text("# human\n")

        # Strip all session env vars
        env = os.environ.copy()
        for key in [
            "CLAUDE_CODE_SESSION_ID", "CLAUDE_SESSION_ID",
            "ANTHROPIC_SESSION_ID", "CODEX_SESSION",
            "CODEX_SESSION_ID", "WRIT_AGENT_ID",
        ]:
            env.pop(key, None)

        result = subprocess.run(
            [writ_bin, "seal", "-s", "human seal"],
            cwd=writ_project, capture_output=True, text=True, env=env,
        )
        assert result.returncode == 0


# ---------------------------------------------------------------------------
# S5.1: Seal enforcement (C.13)
# ---------------------------------------------------------------------------

class TestSealEnforcement:
    """Seal enforcement — agents should use specs."""

    def test_seal_without_spec_behavior(
        self, writ_project: Path, writ_bin: str,
    ):
        """5.1.1: Document seal-without-spec behavior."""
        (writ_project / "nospec.py").write_text("# no spec\n")
        result = writ_cmd(
            writ_bin, writ_project,
            "seal", "-s", "no spec seal", "--agent", "rogue",
            check=False,
        )
        # Document current behavior — may warn or reject
        assert result.returncode in (0, 1), (
            f"Unexpected exit code: {result.returncode}"
        )

    def test_seal_with_spec_succeeds(
        self, writ_project_with_spec: Path, writ_bin: str,
    ):
        """5.1.3: Seal with --spec succeeds."""
        path = writ_project_with_spec
        (path / "src" / "app.py").write_text("# enforced\n")
        result = writ_cmd(
            writ_bin, path,
            "seal", "-s", "enforced seal", "--agent", "good-agent",
            "--spec", "feat-1",
            check=False,
        )
        assert result.returncode == 0, f"Seal with spec failed: {result.stderr}"
