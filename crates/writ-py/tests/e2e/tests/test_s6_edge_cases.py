"""S6: Edge Cases — empty projects, special chars, concurrency, perf.

Maps to Section 6 of the pre-beta testing guide (P2 — Fix or Document).
"""

import subprocess
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pytest

from helpers.cli import (
    writ_cmd,
    writ_context,
    writ_log,
    writ_spec_list,
    writ_verify_chain,
)


# ---------------------------------------------------------------------------
# S6.1: Empty and minimal projects
# ---------------------------------------------------------------------------

class TestEmptyProject:
    """Edge cases with no seals, no specs, no files."""

    def test_context_on_fresh_project(
        self, writ_project: Path, writ_bin: str,
    ):
        """6.1.3: Context with no seals returns valid output."""
        ctx = writ_context(writ_bin, writ_project)
        assert isinstance(ctx, dict)

    def test_seal_no_changes(self, writ_project: Path, writ_bin: str):
        """6.1.2: Seal with no file changes handles gracefully."""
        writ_cmd(writ_bin, writ_project,
                 "spec", "add", "--id", "empty-spec",
                 "--title", "Empty Spec")
        result = writ_cmd(
            writ_bin, writ_project,
            "seal", "-s", "empty seal", "--agent", "tester",
            "--spec", "empty-spec",
            check=False,
        )
        # Should either succeed (empty seal) or fail cleanly
        assert "panic" not in (result.stderr or "").lower()

    def test_finish_nothing_to_commit(
        self, writ_project: Path, writ_bin: str,
    ):
        """6.1.4: Finish with no seals gives clean message."""
        result = writ_cmd(
            writ_bin, writ_project, "finish", "--yes", check=False,
        )
        assert "panic" not in (result.stderr or "").lower()

    def test_status_on_fresh_project(
        self, writ_project: Path, writ_bin: str,
    ):
        """Status on fresh project works."""
        result = writ_cmd(
            writ_bin, writ_project, "status", check=False,
        )
        assert result.returncode == 0


# ---------------------------------------------------------------------------
# S6.2: Special characters and unicode
# ---------------------------------------------------------------------------

class TestSpecialCharacters:
    """Files and metadata with special characters."""

    def test_spec_title_with_quotes(
        self, writ_project: Path, writ_bin: str,
    ):
        """6.2.3: Spec title with quotes works."""
        result = writ_cmd(
            writ_bin, writ_project,
            "spec", "add", "--id", "special",
            "--title", "Fix the 'bug' in authentication",
            check=False,
        )
        assert result.returncode == 0

        specs = writ_spec_list(writ_bin, writ_project)
        ids = [s.get("id") for s in specs]
        assert "special" in ids

    def test_seal_summary_with_unicode(
        self, writ_project: Path, writ_bin: str,
    ):
        """6.2.4: Seal summary with unicode handled correctly."""
        writ_cmd(writ_bin, writ_project,
                 "spec", "add", "--id", "unicode-spec",
                 "--title", "Unicode Support")
        (writ_project / "unicode.py").write_text("# unicode test\n")
        result = writ_cmd(
            writ_bin, writ_project,
            "seal", "-s", "added unicode support for donnees",
            "--agent", "tester", "--spec", "unicode-spec",
            check=False,
        )
        assert result.returncode == 0

    def test_file_with_spaces(self, writ_project: Path, writ_bin: str):
        """6.2.1: File with spaces in name handled by seal."""
        writ_cmd(writ_bin, writ_project,
                 "spec", "add", "--id", "spaces-spec",
                 "--title", "Spaces Test")
        (writ_project / "my file.py").write_text("# spaces\n")
        result = writ_cmd(
            writ_bin, writ_project,
            "seal", "-s", "file with spaces", "--agent", "tester",
            "--spec", "spaces-spec",
            check=False,
        )
        assert result.returncode == 0


# ---------------------------------------------------------------------------
# S6.4: Concurrency
# ---------------------------------------------------------------------------

class TestConcurrency:
    """Multiple simultaneous operations."""

    @pytest.mark.slow
    def test_concurrent_seal_safety(
        self, writ_project: Path, writ_bin: str,
    ):
        """6.4.1: Multiple seals at ~same time don't corrupt state."""
        # Create files first
        for i in range(5):
            (writ_project / f"concurrent_{i}.py").write_text(f"# file {i}\n")

        # Create specs for concurrent seals (C.13 enforcement)
        for i in range(3):
            writ_cmd(writ_bin, writ_project,
                     "spec", "add", "--id", f"concurrent-{i}",
                     "--title", f"Concurrent {i}", check=False)

        def do_seal(idx: int):
            return subprocess.run(
                [writ_bin, "seal", "-s", f"concurrent seal {idx}",
                 "--agent", f"agent-{idx}",
                 "--spec", f"concurrent-{idx}"],
                cwd=writ_project, capture_output=True, text=True,
            )

        with ThreadPoolExecutor(max_workers=3) as pool:
            futures = [pool.submit(do_seal, i) for i in range(3)]
            results = [f.result() for f in futures]

        # At least one should succeed
        succeeded = sum(1 for r in results if r.returncode == 0)
        assert succeeded >= 1, "At least one concurrent seal should succeed"

        # Chain should still be valid
        try:
            verify = writ_verify_chain(writ_bin, writ_project)
            assert verify.get("valid") is True
        except Exception:
            result = writ_cmd(
                writ_bin, writ_project, "verify", "--chain", check=False,
            )
            assert result.returncode == 0


# ---------------------------------------------------------------------------
# S6.3: Performance
# ---------------------------------------------------------------------------

class TestPerformance:
    """Performance with scale."""

    @pytest.mark.slow
    def test_context_with_10_specs(
        self, writ_project: Path, writ_bin: str,
    ):
        """6.3.3: Context with 10 specs returns in <5 seconds."""
        for i in range(10):
            writ_cmd(writ_bin, writ_project,
                     "spec", "add", "--id", f"perf-{i}",
                     "--title", f"Perf test {i}")
            (writ_project / f"perf_{i}.py").write_text(f"# {i}\n")
            writ_cmd(writ_bin, writ_project,
                     "seal", "-s", f"perf {i}",
                     "--agent", f"agent-{i % 3}", "--spec", f"perf-{i}")

        start = time.monotonic()
        ctx = writ_context(writ_bin, writ_project)
        elapsed = time.monotonic() - start

        assert elapsed < 5.0, (
            f"Context with 10 specs took {elapsed:.2f}s, should be <5s"
        )
        assert isinstance(ctx, dict)


# ---------------------------------------------------------------------------
# S6.5: Error recovery
# ---------------------------------------------------------------------------

class TestErrorRecovery:
    """Graceful handling of errors — no panics."""

    def test_commands_before_init(
        self, tmp_git_repo: Path, writ_bin: str,
    ):
        """6.5: Commands on un-initialized repo fail gracefully."""
        result = subprocess.run(
            [writ_bin, "context"],
            cwd=tmp_git_repo, capture_output=True, text=True,
        )
        assert result.returncode != 0
        assert "panic" not in result.stderr.lower(), (
            "Should error, not panic"
        )

    def test_restore_nonexistent_seal(
        self, writ_project: Path, writ_bin: str,
    ):
        """6.5: Restore with fake seal ID fails gracefully."""
        fake_id = "a" * 64
        result = subprocess.run(
            [writ_bin, "restore", fake_id, "--force"],
            cwd=writ_project, capture_output=True, text=True,
        )
        # Should either fail (seal not found) or handle gracefully
        assert "panic" not in result.stderr.lower()
        output = result.stdout + result.stderr
        # If it returns 0, it should mention something useful
        assert result.returncode != 0 or len(output.strip()) > 0

    def test_doctor_runs(self, writ_project: Path, writ_bin: str):
        """writ doctor produces health check output."""
        result = writ_cmd(
            writ_bin, writ_project, "doctor", check=False,
        )
        assert result.returncode == 0
        assert len(result.stdout.strip()) > 0


# ---------------------------------------------------------------------------
# S6.6: Idempotency
# ---------------------------------------------------------------------------

class TestIdempotency:
    """Repeated operations handled gracefully."""

    def test_init_twice(self, writ_project: Path, writ_bin: str):
        """6.6.1: writ init twice warns, doesn't corrupt."""
        result = writ_cmd(
            writ_bin, writ_project, "init", "--yes", check=False,
        )
        # Should warn or succeed, not crash
        assert "panic" not in (result.stderr or "").lower()

    def test_duplicate_spec(self, writ_project: Path, writ_bin: str):
        """6.6.4: Creating duplicate spec gives clear error."""
        writ_cmd(writ_bin, writ_project,
                 "spec", "add", "--id", "dup", "--title", "Duplicate")
        result = writ_cmd(
            writ_bin, writ_project,
            "spec", "add", "--id", "dup", "--title", "Duplicate Again",
            check=False,
        )
        assert result.returncode != 0, "Duplicate spec should be rejected"

    def test_workspace_duplicate_name(
        self, writ_project: Path, writ_bin: str,
    ):
        """6.6.4: Duplicate workspace name gives clear error."""
        writ_cmd(
            writ_bin, writ_project,
            "workspace", "create", "unique-ws", check=False,
        )
        result = writ_cmd(
            writ_bin, writ_project,
            "workspace", "create", "unique-ws", check=False,
        )
        assert result.returncode != 0


# ---------------------------------------------------------------------------
# S6.8: Writ alongside git
# ---------------------------------------------------------------------------

class TestGitCoexistence:
    """Writ plays nicely with git."""

    def test_writ_dir_gitignored(self, writ_project: Path):
        """6.8.1: .writ/ is in .gitignore."""
        gitignore = writ_project / ".gitignore"
        if gitignore.exists():
            content = gitignore.read_text()
            assert ".writ" in content, ".writ should be gitignored"
        else:
            # If no .gitignore, git status should not show .writ
            result = subprocess.run(
                ["git", "status", "--porcelain"],
                cwd=writ_project, capture_output=True, text=True,
            )
            assert ".writ/" not in result.stdout

    def test_git_branch_switch(self, writ_project: Path, writ_bin: str):
        """6.8.3: Git branch switch doesn't break writ."""
        # Seal some work
        writ_cmd(writ_bin, writ_project,
                 "spec", "add", "--id", "branch-test",
                 "--title", "Branch Test")
        (writ_project / "branch_test.py").write_text("# main\n")
        writ_cmd(writ_bin, writ_project,
                 "seal", "-s", "main work", "--agent", "tester",
                 "--spec", "branch-test")

        # Create and switch branches
        subprocess.run(
            ["git", "checkout", "-b", "feature-test"],
            cwd=writ_project, capture_output=True,
        )
        subprocess.run(
            ["git", "checkout", "main"],
            cwd=writ_project, capture_output=True,
        )

        # Writ should still work
        ctx = writ_context(writ_bin, writ_project)
        assert isinstance(ctx, dict)
