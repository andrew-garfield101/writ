"""Persona A: Casual single-agent user.

Simulates the simplest writ user — opens Claude Code, works on a project,
agents seal as they go, human gets a commit message at the end.

Key scenarios tested:
- No explicit specs (spec_id=None on all seals)
- Single agent
- writ install → seal → summary round-trip
- git commit -m "$(writ summary --format commit)" equivalent
- Summary works with zero specs defined
- Multiple seals accumulate correctly
"""

import json
import os
import subprocess

import pytest
import writ


def _git(*args, cwd):
    """Run a git command, propagating PATH so git is always found."""
    result = subprocess.run(
        ["git"] + list(args),
        cwd=cwd,
        capture_output=True,
        text=True,
        env={
            **os.environ,
            "GIT_AUTHOR_NAME": "dev",
            "GIT_AUTHOR_EMAIL": "dev@example.com",
            "GIT_COMMITTER_NAME": "dev",
            "GIT_COMMITTER_EMAIL": "dev@example.com",
        },
    )
    assert result.returncode == 0, f"git {args} failed: {result.stderr}"
    return result.stdout.strip()


def _make_project(path):
    """Create a minimal git project."""
    _git("init", cwd=path)
    with open(os.path.join(path, "app.py"), "w") as f:
        f.write("from flask import Flask\napp = Flask(__name__)\n")
    with open(os.path.join(path, "requirements.txt"), "w") as f:
        f.write("flask==3.0.0\n")
    _git("add", ".", cwd=path)
    _git("commit", "-m", "initial project setup", cwd=path)


class TestCasualWorkflow:
    """The 'open Claude Code twice a week' user."""

    def test_full_round_trip_no_specs(self, tmp_path):
        """Install → work → seal (no spec) → summary → the summary has content."""
        path = str(tmp_path)
        _make_project(path)

        # Step 1: Human runs writ install
        result = writ.Repository.install(path)
        assert result["initialized"] is True
        assert result["git_imported"] is True

        # Step 2: Agent does some work
        repo = writ.Repository.open(path)
        with open(os.path.join(path, "auth.py"), "w") as f:
            f.write("def login(user, password):\n    return True\n")
        with open(os.path.join(path, "app.py"), "w") as f:
            f.write(
                "from flask import Flask\n"
                "from auth import login\n"
                "app = Flask(__name__)\n"
            )

        # Step 3: Agent seals WITHOUT a spec
        seal1 = repo.seal(
            summary="added auth module with login function",
            agent_id="claude-code",
            agent_type="agent",
        )
        assert seal1["id"] is not None
        assert seal1["spec_id"] is None

        # Step 4: Agent does more work
        with open(os.path.join(path, "tests.py"), "w") as f:
            f.write(
                "def test_login():\n"
                "    from auth import login\n"
                "    assert login('admin', 'pass') is True\n"
            )

        seal2 = repo.seal(
            summary="added login test",
            agent_id="claude-code",
            agent_type="agent",
            tests_passed=1,
        )
        assert seal2["id"] != seal1["id"]

        # Step 5: Summary generates a useful commit message
        summary = repo.summary()
        assert summary["total_seals"] >= 2
        assert len(summary["agents"]) >= 1
        assert len(summary["files_changed"]) >= 1
        assert len(summary["headline"]) > 0
        # Headline should be descriptive (seal summaries, not just metadata).
        assert "added auth module" in summary["headline"] or "writ:" in summary["headline"]

    def test_summary_commit_format_usable(self, tmp_path):
        """The commit format headline is a valid git commit message."""
        path = str(tmp_path)
        _make_project(path)
        writ.Repository.install(path)
        repo = writ.Repository.open(path)

        with open(os.path.join(path, "feature.py"), "w") as f:
            f.write("def feature(): return 42\n")

        repo.seal(
            summary="added feature",
            agent_id="claude-code",
            agent_type="agent",
        )

        summary = repo.summary()
        headline = summary["headline"]

        # Headline should be non-empty and reasonable for a commit message
        assert len(headline) > 0
        assert len(headline) < 200  # git best practice: subject < 72, but we're flexible
        assert "\n" not in headline  # single line

    def test_summary_pr_format_has_body(self, tmp_path):
        """The PR format includes agent and file information."""
        path = str(tmp_path)
        _make_project(path)
        writ.Repository.install(path)
        repo = writ.Repository.open(path)

        with open(os.path.join(path, "feature.py"), "w") as f:
            f.write("def feature(): return 42\n")

        repo.seal(
            summary="added feature",
            agent_id="claude-code",
            agent_type="agent",
        )

        summary = repo.summary()
        body = summary["body"]

        assert "claude-code" in body
        assert "Files changed" in body or "file" in body.lower()

    def test_git_commit_round_trip(self, tmp_path):
        """Simulate: git commit -m "$(writ summary --format commit)"."""
        path = str(tmp_path)
        _make_project(path)
        writ.Repository.install(path)
        repo = writ.Repository.open(path)

        with open(os.path.join(path, "feature.py"), "w") as f:
            f.write("def feature(): return 42\n")

        repo.seal(
            summary="added feature module",
            agent_id="claude-code",
            agent_type="agent",
        )

        summary = repo.summary()
        commit_msg = summary["headline"]

        # Stage and commit with the writ-generated message
        _git("add", ".", cwd=path)
        _git("commit", "-m", commit_msg, cwd=path)

        # Verify the commit exists with the right message
        log_output = _git("log", "--oneline", "-1", cwd=path)
        assert "writ:" in log_output or "seal" in log_output

    def test_multiple_seals_accumulate(self, tmp_path):
        """Multiple seals from the same agent accumulate in summary."""
        path = str(tmp_path)
        _make_project(path)
        writ.Repository.install(path)
        repo = writ.Repository.open(path)

        for i in range(5):
            with open(os.path.join(path, f"module_{i}.py"), "w") as f:
                f.write(f"def func_{i}(): return {i}\n")
            repo.seal(
                summary=f"added module {i}",
                agent_id="claude-code",
                agent_type="agent",
            )

        summary = repo.summary()
        # Bridge seal + 5 work seals, summary skips bridge
        assert summary["total_seals"] >= 5
        assert len(summary["files_changed"]) >= 5

        # Agent appears exactly once in agents list
        agent_ids = [a["id"] for a in summary["agents"]]
        assert agent_ids.count("claude-code") == 1

    def test_context_works_without_specs(self, tmp_path):
        """Context is useful even when no specs are defined."""
        path = str(tmp_path)
        _make_project(path)
        writ.Repository.install(path)
        repo = writ.Repository.open(path)

        with open(os.path.join(path, "feature.py"), "w") as f:
            f.write("def feature(): return 42\n")

        repo.seal(
            summary="added feature",
            agent_id="claude-code",
            agent_type="agent",
        )

        ctx = repo.context()
        assert "recent_seals" in ctx
        assert len(ctx["recent_seals"]) >= 1
        assert "writ_version" in ctx

    def test_seal_without_spec_has_no_scope_warning(self, tmp_path):
        """Sealing without a spec doesn't produce scope warnings."""
        path = str(tmp_path)
        _make_project(path)
        writ.Repository.install(path)
        repo = writ.Repository.open(path)

        with open(os.path.join(path, "anything.py"), "w") as f:
            f.write("x = 1\n")

        seal = repo.seal(
            summary="random work",
            agent_id="claude-code",
            agent_type="agent",
        )
        assert seal.get("file_scope_warning") is None

    def test_install_then_immediate_summary(self, tmp_path):
        """Summary works immediately after install (with only bridge seal)."""
        path = str(tmp_path)
        _make_project(path)
        writ.Repository.install(path)
        repo = writ.Repository.open(path)

        # No work done yet — just the bridge import seal
        summary = repo.summary()
        # Should return a valid summary (possibly with 0 work seals since bridge is filtered)
        assert "headline" in summary
        assert "total_seals" in summary
        assert "files_changed" in summary

    def test_summary_json_serializable(self, tmp_path):
        """Summary output can be serialized to JSON (for piping to tools)."""
        path = str(tmp_path)
        _make_project(path)
        writ.Repository.install(path)
        repo = writ.Repository.open(path)

        with open(os.path.join(path, "feature.py"), "w") as f:
            f.write("def feature(): pass\n")

        repo.seal(
            summary="feature work",
            agent_id="claude-code",
            agent_type="agent",
        )

        summary = repo.summary()
        json_str = json.dumps(summary, default=str)
        reparsed = json.loads(json_str)
        assert reparsed["total_seals"] == summary["total_seals"]
        assert reparsed["headline"] == summary["headline"]


class TestCasualEdgeCases:
    """Edge cases a casual user might hit."""

    def test_seal_with_no_changes_raises(self, tmp_path):
        """Sealing with no file changes raises WritError — not a silent no-op."""
        path = str(tmp_path)
        writ.Repository.init(path)
        repo = writ.Repository.open(path)

        with pytest.raises(writ.WritError, match="no changes"):
            repo.seal(
                summary="did some thinking",
                agent_id="claude-code",
                agent_type="agent",
            )

    def test_no_git_workflow(self, tmp_path):
        """Writ works standalone without git."""
        path = str(tmp_path)
        # No git init — just writ
        result = writ.Repository.install(path)
        assert result["initialized"] is True
        assert result["git_detected"] is False

        repo = writ.Repository.open(path)

        with open(os.path.join(path, "script.py"), "w") as f:
            f.write("print('hello')\n")

        seal = repo.seal(
            summary="wrote a script",
            agent_id="claude-code",
            agent_type="agent",
        )
        assert seal["id"] is not None

        summary = repo.summary()
        assert summary["total_seals"] >= 1

    def test_resume_after_close(self, tmp_path):
        """Simulate closing and reopening: open a new repo handle, seals persist."""
        path = str(tmp_path)
        _make_project(path)
        writ.Repository.install(path)

        # Session 1: agent does some work
        repo1 = writ.Repository.open(path)
        with open(os.path.join(path, "session1.py"), "w") as f:
            f.write("x = 1\n")
        repo1.seal(
            summary="session 1 work",
            agent_id="claude-code",
            agent_type="agent",
        )

        # "Close" and "reopen" — new Repository handle
        repo2 = writ.Repository.open(path)

        with open(os.path.join(path, "session2.py"), "w") as f:
            f.write("y = 2\n")
        repo2.seal(
            summary="session 2 work",
            agent_id="claude-code",
            agent_type="agent",
        )

        # Summary should see both sessions' seals
        summary = repo2.summary()
        assert summary["total_seals"] >= 2
        assert len(summary["files_changed"]) >= 2

    def test_install_idempotent_workflow(self, tmp_path):
        """Running install multiple times doesn't break anything."""
        path = str(tmp_path)
        _make_project(path)

        # Install, work, install again, work more
        writ.Repository.install(path)
        repo = writ.Repository.open(path)

        with open(os.path.join(path, "first.py"), "w") as f:
            f.write("a = 1\n")
        repo.seal(summary="first", agent_id="claude-code", agent_type="agent")

        # Install again (maybe user re-ran it)
        writ.Repository.install(path)
        repo = writ.Repository.open(path)

        with open(os.path.join(path, "second.py"), "w") as f:
            f.write("b = 2\n")
        repo.seal(summary="second", agent_id="claude-code", agent_type="agent")

        summary = repo.summary()
        assert summary["total_seals"] >= 2
