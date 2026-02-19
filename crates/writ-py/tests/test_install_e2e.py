"""End-to-end tests for `writ install`.

Covers all 8 scenarios from the install test plan, exercising both the
Python API (writ.Repository.install) and the CLI (writ install --format json).
"""

import json
import os
import subprocess
import tempfile

import pytest
import writ

WRIT_CLI = os.path.join(
    os.path.dirname(__file__), "..", "..", "..", "target", "debug", "writ"
)


def _git(*args, cwd):
    result = subprocess.run(
        ["git"] + list(args),
        cwd=cwd,
        capture_output=True,
        text=True,
        env={**os.environ, "GIT_AUTHOR_NAME": "test", "GIT_AUTHOR_EMAIL": "t@t",
             "GIT_COMMITTER_NAME": "test", "GIT_COMMITTER_EMAIL": "t@t"},
    )
    assert result.returncode == 0, f"git {args} failed: {result.stderr}"
    return result.stdout.strip()


def _writ_cli(*args, cwd):
    result = subprocess.run(
        [WRIT_CLI] + list(args),
        cwd=cwd,
        capture_output=True,
        text=True,
    )
    return result


def _make_git_project(path):
    _git("init", cwd=path)
    with open(os.path.join(path, "main.py"), "w") as f:
        f.write("print('hello')\n")
    _git("add", ".", cwd=path)
    _git("commit", "-m", "initial commit", cwd=path)


class TestScenario1FreshProject:
    """Fresh project, human dev: init + import + idempotent re-run."""

    def test_python_api(self, tmp_path):
        _make_git_project(str(tmp_path))
        result = writ.Repository.install(str(tmp_path))

        assert result["initialized"] is True
        assert result["git_detected"] is True
        assert result["git_imported"] is True
        assert result["imported_seal_id"] is not None
        assert result["imported_files"] >= 1
        assert (tmp_path / ".writ").exists()
        assert (tmp_path / ".writignore").exists()
        assert result["writignore_created"] is True
        assert result["tracked_files"] >= 1

        real_root = os.path.realpath(str(tmp_path))
        assert os.path.realpath(result["repo_root"]) == real_root

    def test_idempotent_rerun(self, tmp_path):
        _make_git_project(str(tmp_path))
        writ.Repository.install(str(tmp_path))

        result2 = writ.Repository.install(str(tmp_path))
        assert result2["initialized"] is False
        assert result2["already_imported"] is True
        assert result2["git_imported"] is False
        assert result2["writignore_created"] is False

    def test_cli(self, tmp_path):
        if not os.path.isfile(WRIT_CLI):
            pytest.skip("CLI binary not built")
        _make_git_project(str(tmp_path))

        r = _writ_cli("install", "--format", "json", cwd=str(tmp_path))
        assert r.returncode == 0, f"install failed: {r.stderr}"
        data = json.loads(r.stdout)
        assert data["initialized"] is True
        assert data["git_imported"] is True

        r2 = _writ_cli("install", "--format", "json", cwd=str(tmp_path))
        assert r2.returncode == 0
        data2 = json.loads(r2.stdout)
        assert data2["already_imported"] is True


class TestScenario2HeadMoves:
    """Git HEAD moves between installs triggers re-import."""

    def test_reimport_on_new_commit(self, tmp_path):
        _make_git_project(str(tmp_path))
        result1 = writ.Repository.install(str(tmp_path))
        seal1 = result1["imported_seal_id"]

        with open(os.path.join(str(tmp_path), "lib.py"), "w") as f:
            f.write("print('world')\n")
        _git("add", ".", cwd=str(tmp_path))
        _git("commit", "-m", "add lib", cwd=str(tmp_path))

        result2 = writ.Repository.install(str(tmp_path))
        assert result2["git_imported"] is True
        assert result2["reimported"] is True
        assert result2["imported_seal_id"] != seal1
        assert result2["tracked_files"] >= 2


class TestScenario3DirtyWorkingTree:
    """Dirty git working tree is reported."""

    def test_dirty_detection(self, tmp_path):
        _make_git_project(str(tmp_path))
        with open(os.path.join(str(tmp_path), "dirty.py"), "w") as f:
            f.write("uncommitted\n")

        result = writ.Repository.install(str(tmp_path))
        assert result["git_dirty"] is True
        assert result["git_dirty_count"] >= 1

    def test_cli_dirty_warning(self, tmp_path):
        if not os.path.isfile(WRIT_CLI):
            pytest.skip("CLI binary not built")
        _make_git_project(str(tmp_path))
        with open(os.path.join(str(tmp_path), "dirty.py"), "w") as f:
            f.write("uncommitted\n")

        r = _writ_cli("install", cwd=str(tmp_path))
        assert r.returncode == 0
        assert "uncommitted change" in r.stderr


class TestScenario4JsonOutput:
    """JSON output is valid and complete for agent consumption."""

    def test_json_has_all_fields(self, tmp_path):
        _make_git_project(str(tmp_path))
        result = writ.Repository.install(str(tmp_path))

        required_fields = [
            "initialized", "git_detected", "git_imported",
            "repo_root", "tracked_files", "writignore_created",
            "already_imported", "reimported",
        ]
        for field in required_fields:
            assert field in result, f"missing field: {field}"

        dumped = json.dumps(result, default=str)
        reparsed = json.loads(dumped)
        assert reparsed["initialized"] == result["initialized"]

    def test_cli_json_parseable(self, tmp_path):
        if not os.path.isfile(WRIT_CLI):
            pytest.skip("CLI binary not built")
        _make_git_project(str(tmp_path))

        r = _writ_cli("install", "--format", "json", cwd=str(tmp_path))
        assert r.returncode == 0
        data = json.loads(r.stdout)
        assert isinstance(data["available_operations"], list)
        assert len(data["available_operations"]) > 0


class TestScenario5PythonRoundTrip:
    """Full Python API round-trip: install -> context -> seal -> context."""

    def test_full_cycle(self, tmp_path):
        _make_git_project(str(tmp_path))
        install_result = writ.Repository.install(str(tmp_path))
        assert install_result["git_imported"] is True

        repo = writ.Repository.open(str(tmp_path))

        ctx1 = repo.context()
        assert "writ_version" in ctx1

        with open(os.path.join(str(tmp_path), "feature.py"), "w") as f:
            f.write("def feature(): pass\n")

        seal = repo.seal(
            summary="test checkpoint",
            agent_id="test-agent",
            agent_type="agent",
            spec_id=None,
            status="complete",
        )
        assert "id" in seal
        assert seal["summary"] == "test checkpoint"

        ctx2 = repo.context()
        assert len(ctx2["recent_seals"]) > len(ctx1["recent_seals"])


class TestScenario6ClaudeCodeDetection:
    """Claude Code framework detected and hooks installed."""

    def test_claude_hooks(self, tmp_path):
        _make_git_project(str(tmp_path))
        os.makedirs(os.path.join(str(tmp_path), ".claude"))
        with open(os.path.join(str(tmp_path), "CLAUDE.md"), "w") as f:
            f.write("# My Project\n")

        result = writ.Repository.install(str(tmp_path))

        detected = [f for f in result.get("frameworks_detected", []) if f["detected"]]
        claude_detected = any(
            f["framework"] == "claude-code" for f in detected
        )
        assert claude_detected, f"Claude Code not detected: {detected}"

        claude_md = (tmp_path / "CLAUDE.md").read_text()
        assert "## Writ" in claude_md
        assert "seal" in claude_md.lower()

        assert (tmp_path / ".claude" / "commands" / "writ-seal.md").exists()
        assert (tmp_path / ".claude" / "commands" / "writ-context.md").exists()

    def test_hooks_idempotent(self, tmp_path):
        _make_git_project(str(tmp_path))
        with open(os.path.join(str(tmp_path), "CLAUDE.md"), "w") as f:
            f.write("# My Project\n")

        writ.Repository.install(str(tmp_path))
        result2 = writ.Repository.install(str(tmp_path))

        hooks = result2.get("hooks_installed", [])
        for hook in hooks:
            assert hook["files_created"] == []
            assert hook["files_updated"] == []


class TestScenario7WritignoreFromGitignore:
    """.gitignore patterns merged into .writignore with dedup."""

    def test_gitignore_import(self, tmp_path):
        with open(os.path.join(str(tmp_path), ".gitignore"), "w") as f:
            f.write("*.pyc\nbuild/\n__pycache__/\ntarget\n")
        _git("init", cwd=str(tmp_path))
        _git("add", ".", cwd=str(tmp_path))
        _git("commit", "-m", "init", cwd=str(tmp_path))

        writ.Repository.install(str(tmp_path))

        writignore = (tmp_path / ".writignore").read_text()
        assert "*.pyc" in writignore
        assert "build" in writignore

        gitignore_section = writignore.split("imported from .gitignore")
        if len(gitignore_section) > 1:
            imported = gitignore_section[1]
            assert "target" not in imported, "target is a default, should be deduped"


class TestScenario8Standalone:
    """Works without git: pure writ init."""

    def test_no_git(self, tmp_path):
        result = writ.Repository.install(str(tmp_path))

        assert result["initialized"] is True
        assert result["git_detected"] is False
        assert result["git_imported"] is False
        assert (tmp_path / ".writ").exists()
        assert (tmp_path / ".writignore").exists()
        assert result["tracked_files"] == 0

    def test_no_git_idempotent(self, tmp_path):
        writ.Repository.install(str(tmp_path))
        result2 = writ.Repository.install(str(tmp_path))

        assert result2["initialized"] is False
        assert result2["git_detected"] is False

    def test_no_git_cli_json(self, tmp_path):
        if not os.path.isfile(WRIT_CLI):
            pytest.skip("CLI binary not built")

        r = _writ_cli("install", "--format", "json", cwd=str(tmp_path))
        assert r.returncode == 0
        data = json.loads(r.stdout)
        assert data["git_detected"] is False
        assert data["initialized"] is True


class TestInstallThenWork:
    """After install, the full writ workflow functions correctly."""

    def test_spec_driven_workflow(self, tmp_path):
        _make_git_project(str(tmp_path))
        writ.Repository.install(str(tmp_path))
        repo = writ.Repository.open(str(tmp_path))

        repo.add_spec(id="auth", title="Add authentication")
        specs = repo.list_specs()
        assert any(s["id"] == "auth" for s in specs)

        with open(os.path.join(str(tmp_path), "auth.py"), "w") as f:
            f.write("def login(): pass\n")

        seal = repo.seal(
            summary="auth skeleton",
            agent_id="agent-auth",
            agent_type="agent",
            spec_id="auth",
            status="in-progress",
        )
        assert seal["spec_id"] == "auth"

        ctx = repo.context(spec="auth")
        assert ctx["active_spec"]["id"] == "auth"
        assert len(ctx["recent_seals"]) >= 1

    def test_bridge_export_after_install(self, tmp_path):
        _make_git_project(str(tmp_path))
        writ.Repository.install(str(tmp_path))
        repo = writ.Repository.open(str(tmp_path))

        with open(os.path.join(str(tmp_path), "feature.py"), "w") as f:
            f.write("def feature(): pass\n")
        repo.seal(
            summary="feature work",
            agent_id="agent-1",
            agent_type="agent",
        )

        export = repo.bridge_export()
        assert export["seals_exported"] >= 1
        assert len(export["exported"]) >= 1
        assert "agent_id" in export["exported"][0]
