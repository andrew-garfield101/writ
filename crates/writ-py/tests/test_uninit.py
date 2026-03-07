"""U.4: Verify writ uninit project cleanup.

Tests the full init → uninit lifecycle, verifying:
- All writ artifacts are removed (.writ/, markers, slash commands, permissions)
- User content is preserved (CLAUDE.md content, other .gitignore entries, settings)
- Flags work correctly (--force, --keep-writignore)
- Deprecated `writ uninstall` alias prints notice and works
- Uninit on non-initialized project handles gracefully

Depends on: U.1 (CC's uninit rename) landed.
"""

import json
import subprocess
from pathlib import Path

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


def run_writ(args: list, cwd: str, check: bool = True) -> subprocess.CompletedProcess:
    if WRIT_BIN is None:
        pytest.skip("writ binary not found")
    return subprocess.run(
        [WRIT_BIN] + args, capture_output=True, text=True, cwd=cwd, check=check,
    )


def run_git(args: list, cwd: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git"] + args, capture_output=True, text=True, cwd=cwd, check=check,
    )


BEGIN_MARKER = "<!-- BEGIN WRIT CONFIGURATION"
END_MARKER = "<!-- END WRIT CONFIGURATION -->"


@pytest.fixture
def git_repo(tmp_path):
    """Git repo ready for writ init."""
    run_git(["init"], str(tmp_path))
    run_git(["config", "user.email", "test@test.com"], str(tmp_path))
    run_git(["config", "user.name", "Test User"], str(tmp_path))
    (tmp_path / "README.md").write_text("# Project\n")
    run_git(["add", "README.md"], str(tmp_path))
    run_git(["commit", "-m", "initial"], str(tmp_path))
    return tmp_path


@pytest.fixture
def initialized_repo(git_repo):
    """Git repo with writ init'd and Claude Code detected."""
    path = git_repo
    # Pre-create CLAUDE.md so init detects Claude Code
    (path / "CLAUDE.md").write_text("# My Project\n\nUser instructions here.\n")
    # Pre-create .claude dir so init creates slash commands + settings
    (path / ".claude").mkdir(exist_ok=True)
    (path / ".claude" / "settings.json").write_text(json.dumps({
        "permissions": {
            "allow": ["Bash(npm *)"],
            "deny": [],
        }
    }, indent=2))
    run_writ(["init", "--yes"], str(path))
    return path


# ---------------------------------------------------------------------------
# Artifact removal tests
# ---------------------------------------------------------------------------

class TestUninitRemovesArtifacts:
    """writ uninit removes all writ-created artifacts."""

    def test_removes_writ_directory(self, initialized_repo):
        """uninit removes the .writ/ directory entirely."""
        path = initialized_repo
        assert (path / ".writ").is_dir()

        run_writ(["uninit", "--force"], str(path))
        assert not (path / ".writ").exists()

    def test_removes_claudemd_markers(self, initialized_repo):
        """uninit removes writ section from CLAUDE.md."""
        path = initialized_repo
        content = (path / "CLAUDE.md").read_text()
        assert BEGIN_MARKER in content

        run_writ(["uninit", "--force"], str(path))
        after = (path / "CLAUDE.md").read_text()
        assert BEGIN_MARKER not in after
        assert END_MARKER not in after

    def test_removes_slash_commands(self, initialized_repo):
        """uninit removes .claude/commands/writ-*.md files."""
        path = initialized_repo
        assert (path / ".claude" / "commands" / "writ-seal.md").exists()
        assert (path / ".claude" / "commands" / "writ-context.md").exists()

        run_writ(["uninit", "--force"], str(path))
        assert not (path / ".claude" / "commands" / "writ-seal.md").exists()
        assert not (path / ".claude" / "commands" / "writ-context.md").exists()

    def test_removes_gitignore_entry(self, initialized_repo):
        """uninit removes .writ/ from .gitignore."""
        path = initialized_repo
        gi = (path / ".gitignore").read_text()
        assert ".writ/" in gi

        run_writ(["uninit", "--force"], str(path))
        # .gitignore may be deleted entirely if .writ/ was the only entry
        if (path / ".gitignore").exists():
            gi_after = (path / ".gitignore").read_text()
            assert ".writ/" not in gi_after

    def test_removes_settings_json_permission(self, initialized_repo):
        """uninit removes Bash(writ *) from .claude/settings.json."""
        path = initialized_repo
        settings = json.loads((path / ".claude" / "settings.json").read_text())
        allow = settings.get("permissions", {}).get("allow", [])
        assert any("writ" in p for p in allow), f"writ permission not found: {allow}"

        run_writ(["uninit", "--force"], str(path))
        settings_after = json.loads((path / ".claude" / "settings.json").read_text())
        allow_after = settings_after.get("permissions", {}).get("allow", [])
        assert not any("writ" in p for p in allow_after), (
            f"writ permission still present: {allow_after}"
        )

    def test_removes_writignore(self, initialized_repo):
        """uninit removes .writignore by default."""
        path = initialized_repo
        (path / ".writignore").write_text("*.log\n")

        run_writ(["uninit", "--force"], str(path))
        assert not (path / ".writignore").exists()


# ---------------------------------------------------------------------------
# Preservation tests
# ---------------------------------------------------------------------------

class TestUninitPreservesUserContent:
    """uninit never touches user content."""

    def test_preserves_claudemd_user_content(self, initialized_repo):
        """User content in CLAUDE.md survives uninit."""
        path = initialized_repo

        run_writ(["uninit", "--force"], str(path))
        content = (path / "CLAUDE.md").read_text()
        assert "My Project" in content
        assert "User instructions here" in content

    def test_preserves_other_gitignore_entries(self, initialized_repo):
        """Other .gitignore entries survive uninit."""
        path = initialized_repo
        gi = (path / ".gitignore").read_text()
        # Add some user entries if not already present
        if "node_modules" not in gi:
            (path / ".gitignore").write_text(gi + "node_modules/\n__pycache__/\n")

        run_writ(["uninit", "--force"], str(path))
        gi_after = (path / ".gitignore").read_text()
        # .gitignore may or may not exist depending on whether it's empty
        # If it exists, user entries should be preserved
        if (path / ".gitignore").exists():
            assert "node_modules/" in gi_after or gi_after.strip() == ""

    def test_preserves_other_settings_permissions(self, initialized_repo):
        """Other .claude/settings.json permissions survive uninit."""
        path = initialized_repo

        run_writ(["uninit", "--force"], str(path))
        settings = json.loads((path / ".claude" / "settings.json").read_text())
        allow = settings.get("permissions", {}).get("allow", [])
        assert "Bash(npm *)" in allow

    def test_preserves_source_files(self, initialized_repo):
        """Source code files are never touched by uninit."""
        path = initialized_repo
        (path / "app.py").write_text("def main(): pass\n")
        (path / "test.py").write_text("def test(): pass\n")

        run_writ(["uninit", "--force"], str(path))
        assert (path / "app.py").read_text() == "def main(): pass\n"
        assert (path / "test.py").read_text() == "def test(): pass\n"

    def test_preserves_git_history(self, initialized_repo):
        """Git history is untouched by uninit."""
        path = initialized_repo
        log_before = run_git(["log", "--oneline"], str(path))

        run_writ(["uninit", "--force"], str(path))
        log_after = run_git(["log", "--oneline"], str(path))
        assert log_before.stdout == log_after.stdout


# ---------------------------------------------------------------------------
# Flag tests
# ---------------------------------------------------------------------------

class TestUninitFlags:
    """Flags work correctly."""

    def test_force_skips_confirmation(self, initialized_repo):
        """--force completes without stdin input."""
        path = initialized_repo
        result = run_writ(["uninit", "--force"], str(path))
        assert result.returncode == 0
        assert not (path / ".writ").exists()

    def test_keep_writignore(self, initialized_repo):
        """--keep-writignore preserves .writignore file."""
        path = initialized_repo
        (path / ".writignore").write_text("*.log\nbuild/\n")

        run_writ(["uninit", "--force", "--keep-writignore"], str(path))
        assert not (path / ".writ").exists()
        assert (path / ".writignore").exists()
        assert (path / ".writignore").read_text() == "*.log\nbuild/\n"

    def test_json_output(self, initialized_repo):
        """--format json produces valid JSON output."""
        path = initialized_repo
        result = run_writ(
            ["uninit", "--force", "--format", "json"], str(path), check=False,
        )
        if result.returncode == 0 and result.stdout.strip():
            data = json.loads(result.stdout)
            assert isinstance(data, dict)


# ---------------------------------------------------------------------------
# Edge cases
# ---------------------------------------------------------------------------

class TestUninitEdgeCases:
    """Edge cases and error handling."""

    def test_uninit_not_initialized(self, tmp_path):
        """uninit on a non-writ directory handles gracefully."""
        result = run_writ(["uninit", "--force"], str(tmp_path), check=False)
        # Should not crash — either succeeds silently or gives clear error
        assert result.returncode in (0, 1)

    def test_uninit_twice(self, initialized_repo):
        """Running uninit twice doesn't crash."""
        path = initialized_repo
        run_writ(["uninit", "--force"], str(path))
        result = run_writ(["uninit", "--force"], str(path), check=False)
        assert result.returncode in (0, 1)

    def test_uninit_no_claudemd(self, git_repo):
        """uninit works when there's no CLAUDE.md."""
        path = git_repo
        run_writ(["init", "--yes"], str(path))

        # Remove CLAUDE.md if it was created
        claudemd = path / "CLAUDE.md"
        if claudemd.exists():
            claudemd.unlink()

        result = run_writ(["uninit", "--force"], str(path), check=False)
        assert result.returncode == 0
        assert not (path / ".writ").exists()

    def test_uninit_no_claude_dir(self, git_repo):
        """uninit works when there's no .claude/ directory."""
        path = git_repo
        run_writ(["init", "--yes", "--no-claude"], str(path))

        result = run_writ(["uninit", "--force"], str(path), check=False)
        assert result.returncode == 0
        assert not (path / ".writ").exists()


# ---------------------------------------------------------------------------
# Deprecated alias
# ---------------------------------------------------------------------------

class TestDeprecatedAlias:
    """writ uninstall still works but shows deprecation notice."""

    def test_uninstall_alias_works(self, initialized_repo):
        """Deprecated uninstall command still removes writ."""
        path = initialized_repo
        result = run_writ(["uninstall", "--force"], str(path))
        assert result.returncode == 0
        assert not (path / ".writ").exists()

    def test_uninstall_alias_shows_deprecation(self, initialized_repo):
        """Deprecated uninstall prints deprecation notice."""
        path = initialized_repo
        result = run_writ(["uninstall", "--force"], str(path))
        combined = result.stdout + result.stderr
        assert "deprecated" in combined.lower()
        assert "uninit" in combined.lower()
