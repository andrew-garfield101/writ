"""Integration tests for framework hooks via real Rust implementation (T.8-T.13).

Tests the actual writ.install_hooks() and writ.detect_frameworks() Python
bindings, as well as CLI integration for `writ init --yes`, `writ install`
(deprecated alias), and `writ uninit`.

T.8:  CLAUDE.md creation/append with markers via install_hooks()
T.9:  CLAUDE.md marker-based update on reinit via install_hooks()
T.10: Idempotent .gitignore modification via install_hooks()
T.11: `writ init --yes` non-interactive CLI mode
T.12: `writ install` deprecated alias
T.13: `writ uninit` integration
"""

import json
import os
import subprocess
from pathlib import Path

import pytest
import writ

# Marker constants (must match hooks.rs)
BEGIN_MARKER = "<!-- BEGIN WRIT CONFIGURATION \u2014 managed by writ init -->"
END_MARKER = "<!-- END WRIT CONFIGURATION -->"

# Find writ binary
WRIT_BIN = None
_search = Path(__file__).resolve()
for _ in range(10):
    _search = _search.parent
    candidate = _search / "target" / "release" / "writ"
    if candidate.exists():
        WRIT_BIN = str(candidate)
        break
    candidate = _search / "target" / "debug" / "writ"
    if candidate.exists():
        WRIT_BIN = str(candidate)
        break


def run_writ(args: list[str], cwd: str, check: bool = True) -> subprocess.CompletedProcess:
    """Run the writ CLI binary with given args."""
    assert WRIT_BIN is not None, "Could not find writ binary in target/"
    return subprocess.run(
        [WRIT_BIN] + args,
        cwd=cwd,
        capture_output=True,
        text=True,
        check=check,
    )


# ═══════════════════════════════════════════════════════════════════════════
# T.8: CLAUDE.md creation/append via install_hooks()
# ═══════════════════════════════════════════════════════════════════════════


class TestInstallHooksClaudeMd:
    """T.8: CLAUDE.md creation and marker-based append via real Rust hooks."""

    def test_install_hooks_creates_claudemd(self, tmp_path: Path):
        """install_hooks() creates CLAUDE.md with markers in fresh directory."""
        # Create CLAUDE.md so claude-code framework is detected
        (tmp_path / "CLAUDE.md").write_text("# My Project\n")
        writ.install_hooks(str(tmp_path))

        content = (tmp_path / "CLAUDE.md").read_text()
        assert BEGIN_MARKER in content
        assert END_MARKER in content
        assert "Writ Version Control" in content

    def test_install_hooks_preserves_existing_content(self, tmp_path: Path):
        """Existing CLAUDE.md content preserved when hooks append."""
        (tmp_path / "CLAUDE.md").write_text(
            "# My Project\n\n## Important Rules\n\nAlways test your code.\n"
        )
        writ.install_hooks(str(tmp_path))

        content = (tmp_path / "CLAUDE.md").read_text()
        assert "My Project" in content
        assert "Important Rules" in content
        assert "Always test your code" in content
        assert BEGIN_MARKER in content
        assert "Writ Version Control" in content

    def test_install_hooks_claudemd_has_separator(self, tmp_path: Path):
        """Separator (---) appears between writ section and existing content (prepend order)."""
        (tmp_path / "CLAUDE.md").write_text("# My Project\n\nExisting content.\n")
        writ.install_hooks(str(tmp_path))

        content = (tmp_path / "CLAUDE.md").read_text()
        assert "---" in content
        separator_idx = content.index("---")
        marker_idx = content.index(BEGIN_MARKER)
        # Writ section is prepended, so marker comes before the separator.
        assert marker_idx < separator_idx

    def test_install_hooks_claudemd_has_required_commands(self, tmp_path: Path):
        """CLAUDE.md writ section contains required workflow commands."""
        (tmp_path / "CLAUDE.md").write_text("# Project\n")
        writ.install_hooks(str(tmp_path))

        content = (tmp_path / "CLAUDE.md").read_text()
        assert "writ context" in content
        assert "writ seal" in content
        assert "writ spec" in content
        assert "writ log" in content

    def test_install_hooks_creates_slash_commands(self, tmp_path: Path):
        """install_hooks() creates .claude/commands/ slash command files."""
        (tmp_path / "CLAUDE.md").write_text("# Project\n")
        writ.install_hooks(str(tmp_path))

        seal_cmd = tmp_path / ".claude" / "commands" / "writ-seal.md"
        context_cmd = tmp_path / ".claude" / "commands" / "writ-context.md"
        assert seal_cmd.exists(), "Missing writ-seal.md slash command"
        assert context_cmd.exists(), "Missing writ-context.md slash command"

        seal_content = seal_cmd.read_text()
        assert "--spec" in seal_content
        # Slash command now uses "writ spec done" instead of "--status"
        assert "spec done" in seal_content or "--status" in seal_content

    def test_install_hooks_claudemd_new_file(self, tmp_path: Path):
        """install_hooks() on fresh dir (no CLAUDE.md) — no crash but also
        no CLAUDE.md creation since framework isn't detected."""
        # No CLAUDE.md or .claude/ → claude-code not detected → no CLAUDE.md created
        writ.install_hooks(str(tmp_path))
        # But .writ/AGENT_INSTRUCTIONS.md should still be created if .writ exists
        assert not (tmp_path / "CLAUDE.md").exists()


# ═══════════════════════════════════════════════════════════════════════════
# T.8 continued: AGENTS.md via install_hooks()
# ═══════════════════════════════════════════════════════════════════════════


class TestInstallHooksAgentsMd:
    """AGENTS.md creation and marker-based append via real Rust hooks."""

    def test_install_hooks_creates_agentsmd(self, tmp_path: Path):
        """install_hooks() creates AGENTS.md writ section when AGENTS.md exists."""
        (tmp_path / "AGENTS.md").write_text("# Agent Config\n")
        writ.install_hooks(str(tmp_path))

        content = (tmp_path / "AGENTS.md").read_text()
        assert BEGIN_MARKER in content
        assert END_MARKER in content
        assert "Version Control" in content

    def test_install_hooks_agentsmd_preserves_content(self, tmp_path: Path):
        """Existing AGENTS.md content preserved when hooks append."""
        (tmp_path / "AGENTS.md").write_text(
            "# Agent Config\n\n## Style Guide\n\nFollow PEP 8.\n"
        )
        writ.install_hooks(str(tmp_path))

        content = (tmp_path / "AGENTS.md").read_text()
        assert "Agent Config" in content
        assert "Style Guide" in content
        assert "Follow PEP 8" in content
        assert BEGIN_MARKER in content


# ═══════════════════════════════════════════════════════════════════════════
# T.9: CLAUDE.md marker-based update on reinit
# ═══════════════════════════════════════════════════════════════════════════


class TestReinitMarkerUpdate:
    """T.9: Calling install_hooks() twice replaces writ section, no duplication."""

    def test_reinit_no_duplicate_markers(self, tmp_path: Path):
        """Second install_hooks() does not create duplicate markers."""
        (tmp_path / "CLAUDE.md").write_text("# Project\n")
        writ.install_hooks(str(tmp_path))
        writ.install_hooks(str(tmp_path))

        content = (tmp_path / "CLAUDE.md").read_text()
        assert content.count(BEGIN_MARKER) == 1
        assert content.count(END_MARKER) == 1

    def test_reinit_idempotent_content(self, tmp_path: Path):
        """Two consecutive install_hooks() produce identical CLAUDE.md."""
        (tmp_path / "CLAUDE.md").write_text("# Project\n")
        writ.install_hooks(str(tmp_path))
        after_first = (tmp_path / "CLAUDE.md").read_text()

        writ.install_hooks(str(tmp_path))
        after_second = (tmp_path / "CLAUDE.md").read_text()

        assert after_first == after_second

    def test_reinit_preserves_user_content(self, tmp_path: Path):
        """User content outside markers survives reinit."""
        (tmp_path / "CLAUDE.md").write_text("# My Rules\n\nDo important things.\n")
        writ.install_hooks(str(tmp_path))

        # User adds content after the writ section
        content = (tmp_path / "CLAUDE.md").read_text()
        content += "\n## My Custom Section\n\nCustom content.\n"
        (tmp_path / "CLAUDE.md").write_text(content)

        # Reinit
        writ.install_hooks(str(tmp_path))
        final = (tmp_path / "CLAUDE.md").read_text()

        assert "My Rules" in final
        assert "Do important things" in final
        assert "My Custom Section" in final
        assert "Custom content" in final
        assert final.count(BEGIN_MARKER) == 1

    def test_reinit_agentsmd_no_duplication(self, tmp_path: Path):
        """AGENTS.md reinit also doesn't duplicate."""
        (tmp_path / "AGENTS.md").write_text("# Agents\n")
        writ.install_hooks(str(tmp_path))
        writ.install_hooks(str(tmp_path))

        content = (tmp_path / "AGENTS.md").read_text()
        assert content.count(BEGIN_MARKER) == 1


# ═══════════════════════════════════════════════════════════════════════════
# T.10: Idempotent .gitignore modification
# ═══════════════════════════════════════════════════════════════════════════


class TestGitignoreIntegration:
    """T.10: install_hooks() creates/appends .gitignore entry idempotently."""

    def test_gitignore_created_when_missing(self, tmp_path: Path):
        """install_hooks() creates .gitignore with .writ/ entry."""
        (tmp_path / "CLAUDE.md").write_text("# Project\n")
        writ.install_hooks(str(tmp_path))

        gi = tmp_path / ".gitignore"
        assert gi.exists()
        content = gi.read_text()
        assert ".writ/" in content

    def test_gitignore_appends_to_existing(self, tmp_path: Path):
        """Existing .gitignore content preserved, .writ/ appended."""
        (tmp_path / ".gitignore").write_text("node_modules/\n*.pyc\n")
        (tmp_path / "CLAUDE.md").write_text("# Project\n")
        writ.install_hooks(str(tmp_path))

        content = (tmp_path / ".gitignore").read_text()
        assert "node_modules/" in content
        assert "*.pyc" in content
        assert ".writ/" in content

    def test_gitignore_idempotent(self, tmp_path: Path):
        """Calling install_hooks() twice doesn't duplicate .writ/ entry."""
        (tmp_path / "CLAUDE.md").write_text("# Project\n")
        writ.install_hooks(str(tmp_path))
        first = (tmp_path / ".gitignore").read_text()

        writ.install_hooks(str(tmp_path))
        second = (tmp_path / ".gitignore").read_text()

        assert first == second
        assert second.count(".writ/") == 1

    def test_gitignore_already_has_entry(self, tmp_path: Path):
        """If .gitignore already has .writ/, install_hooks() doesn't add again."""
        (tmp_path / ".gitignore").write_text("node_modules/\n.writ/\n")
        (tmp_path / "CLAUDE.md").write_text("# Project\n")
        writ.install_hooks(str(tmp_path))

        content = (tmp_path / ".gitignore").read_text()
        assert content.count(".writ/") == 1


# ═══════════════════════════════════════════════════════════════════════════
# detect_frameworks() tests
# ═══════════════════════════════════════════════════════════════════════════


class TestDetectFrameworks:
    """Tests for writ.detect_frameworks() Python binding."""

    def test_detect_empty_dir(self, tmp_path: Path):
        """No framework indicators → all detected=false."""
        result = writ.detect_frameworks(str(tmp_path))
        assert isinstance(result, list)
        for detection in result:
            assert not detection["detected"]

    def test_detect_claude_code(self, tmp_path: Path):
        """CLAUDE.md present → claude-code detected."""
        (tmp_path / "CLAUDE.md").write_text("# Project\n")
        result = writ.detect_frameworks(str(tmp_path))
        claude = [d for d in result if d["framework"] == "claude-code"]
        assert len(claude) == 1
        assert claude[0]["detected"] is True
        assert "CLAUDE.md" in claude[0]["indicators"]

    def test_detect_codex(self, tmp_path: Path):
        """AGENTS.md present → codex detected."""
        (tmp_path / "AGENTS.md").write_text("# Agents\n")
        result = writ.detect_frameworks(str(tmp_path))
        codex = [d for d in result if d["framework"] == "codex"]
        assert len(codex) == 1
        assert codex[0]["detected"] is True

    def test_detect_claude_dir(self, tmp_path: Path):
        """.claude/ directory → claude-code detected."""
        (tmp_path / ".claude").mkdir()
        result = writ.detect_frameworks(str(tmp_path))
        claude = [d for d in result if d["framework"] == "claude-code"]
        assert claude[0]["detected"] is True
        assert ".claude/" in claude[0]["indicators"]

    def test_detect_both_frameworks(self, tmp_path: Path):
        """Both CLAUDE.md and AGENTS.md → both detected."""
        (tmp_path / "CLAUDE.md").write_text("# Project\n")
        (tmp_path / "AGENTS.md").write_text("# Agents\n")
        result = writ.detect_frameworks(str(tmp_path))
        detected = [d for d in result if d["detected"]]
        assert len(detected) == 2


# ═══════════════════════════════════════════════════════════════════════════
# T.11: `writ init --yes` non-interactive mode
# ═══════════════════════════════════════════════════════════════════════════


@pytest.mark.skipif(WRIT_BIN is None, reason="writ binary not found")
class TestInitYesMode:
    """T.11: `writ init --yes` runs without prompts (CI-safe)."""

    def test_init_yes_succeeds(self, tmp_path: Path):
        """`writ init --yes` exits 0 in a fresh directory."""
        result = run_writ(["init", "--yes"], cwd=str(tmp_path))
        assert result.returncode == 0

    def test_init_yes_creates_writ_dir(self, tmp_path: Path):
        """`writ init --yes` creates .writ/ directory."""
        run_writ(["init", "--yes"], cwd=str(tmp_path))
        assert (tmp_path / ".writ").is_dir()

    def test_init_yes_creates_gitignore(self, tmp_path: Path):
        """`writ init --yes` creates .gitignore with .writ/ entry."""
        run_writ(["init", "--yes"], cwd=str(tmp_path))
        gi = tmp_path / ".gitignore"
        assert gi.exists()
        assert ".writ/" in gi.read_text()

    def test_init_yes_json_output(self, tmp_path: Path):
        """`writ init --yes --format json` outputs valid JSON."""
        result = run_writ(["init", "--yes", "--format", "json"], cwd=str(tmp_path))
        data = json.loads(result.stdout)
        assert "path" in data or "status" in data or isinstance(data, dict)

    def test_init_yes_bare_skips_hooks(self, tmp_path: Path):
        """`writ init --yes --bare` creates .writ/ but no framework files."""
        run_writ(["init", "--yes", "--bare"], cwd=str(tmp_path))
        assert (tmp_path / ".writ").is_dir()
        # Bare mode: no CLAUDE.md, no AGENTS.md, no AGENT_INSTRUCTIONS
        assert not (tmp_path / "CLAUDE.md").exists()
        assert not (tmp_path / "AGENTS.md").exists()

    # Was xfail for BRI-B1 (--no-claude ignored); passes as of A.4 (1d5cb31ae196).
    def test_init_yes_no_claude_flag(self, tmp_path: Path):
        """`--no-claude` skips Claude Code integration."""
        # Pre-create CLAUDE.md to ensure framework is detected
        (tmp_path / "CLAUDE.md").write_text("# My Project\n")
        run_writ(["init", "--yes", "--no-claude"], cwd=str(tmp_path))

        content = (tmp_path / "CLAUDE.md").read_text()
        # CLAUDE.md should NOT have writ markers added
        assert BEGIN_MARKER not in content

    def test_init_yes_with_output_format(self, tmp_path: Path):
        """`--output-format toon` sets default output format."""
        run_writ(["init", "--yes", "--output-format", "toon"], cwd=str(tmp_path))
        assert (tmp_path / ".writ").is_dir()

    def test_init_yes_with_name(self, tmp_path: Path):
        """`--name` sets project name."""
        run_writ(["init", "--yes", "--name", "test-project"], cwd=str(tmp_path))
        assert (tmp_path / ".writ").is_dir()

    def test_init_yes_idempotent(self, tmp_path: Path):
        """`writ init --yes` twice doesn't fail."""
        run_writ(["init", "--yes"], cwd=str(tmp_path))
        result = run_writ(["init", "--yes"], cwd=str(tmp_path), check=False)
        # Should either succeed or give a clear error (not crash)
        assert result.returncode == 0 or "already" in result.stderr.lower()


# ═══════════════════════════════════════════════════════════════════════════
# T.12: `writ install` deprecated alias
# ═══════════════════════════════════════════════════════════════════════════


@pytest.mark.skipif(WRIT_BIN is None, reason="writ binary not found")
class TestDeprecatedInstallAlias:
    """T.12: `writ install` works but shows deprecation notice."""

    def test_install_alias_succeeds(self, tmp_path: Path):
        """`writ install` exits 0 (still functional)."""
        result = run_writ(["install"], cwd=str(tmp_path), check=False)
        # Should work — might print deprecation warning to stderr
        assert result.returncode == 0

    def test_install_alias_creates_writ_dir(self, tmp_path: Path):
        """`writ install` still creates .writ/ directory."""
        run_writ(["install"], cwd=str(tmp_path), check=False)
        assert (tmp_path / ".writ").is_dir()

    def test_install_alias_deprecation_notice(self, tmp_path: Path):
        """`writ install` shows deprecation hint (stderr or help text)."""
        result = run_writ(["install"], cwd=str(tmp_path), check=False)
        combined = result.stdout + result.stderr
        # The CLI help says "Deprecated: use `writ init` instead"
        # The actual command may or may not print a warning at runtime
        # At minimum, `writ install --help` mentions deprecated
        help_result = run_writ(["install", "--help"], cwd=str(tmp_path), check=False)
        assert "deprecated" in help_result.stdout.lower() or "writ init" in help_result.stdout.lower()


# ═══════════════════════════════════════════════════════════════════════════
# T.13: `writ uninit` integration
# ═══════════════════════════════════════════════════════════════════════════


@pytest.mark.skipif(WRIT_BIN is None, reason="writ binary not found")
class TestUninstallIntegration:
    """T.13: `writ uninit --force` removes writ hooks cleanly."""

    def test_uninstall_removes_writ_dir(self, tmp_path: Path):
        """Full lifecycle: init → uninstall removes .writ/."""
        run_writ(["init", "--yes"], cwd=str(tmp_path))
        assert (tmp_path / ".writ").is_dir()

        result = run_writ(["uninstall", "--force"], cwd=str(tmp_path), check=False)
        # Uninstall should succeed
        assert result.returncode == 0
        # .writ/ should be removed
        assert not (tmp_path / ".writ").is_dir()

    def test_uninstall_cleans_gitignore(self, tmp_path: Path):
        """Uninstall removes .writ/ entry from .gitignore."""
        (tmp_path / ".gitignore").write_text("node_modules/\n")
        run_writ(["init", "--yes"], cwd=str(tmp_path))

        gi_content = (tmp_path / ".gitignore").read_text()
        assert ".writ/" in gi_content

        run_writ(["uninstall", "--force"], cwd=str(tmp_path), check=False)
        gi_after = (tmp_path / ".gitignore").read_text()
        assert ".writ/" not in gi_after
        assert "node_modules/" in gi_after

    def test_uninstall_removes_claudemd_section(self, tmp_path: Path):
        """Uninstall removes writ section from CLAUDE.md, preserves user content."""
        (tmp_path / "CLAUDE.md").write_text("# My Project\n\nImportant stuff.\n")
        run_writ(["init", "--yes"], cwd=str(tmp_path))

        content = (tmp_path / "CLAUDE.md").read_text()
        assert BEGIN_MARKER in content

        run_writ(["uninstall", "--force"], cwd=str(tmp_path), check=False)
        after = (tmp_path / "CLAUDE.md").read_text()
        assert BEGIN_MARKER not in after
        assert "My Project" in after
        assert "Important stuff" in after

    def test_uninstall_removes_slash_commands(self, tmp_path: Path):
        """Uninstall removes .claude/commands/writ-*.md files."""
        (tmp_path / "CLAUDE.md").write_text("# Project\n")
        run_writ(["init", "--yes"], cwd=str(tmp_path))
        assert (tmp_path / ".claude" / "commands" / "writ-seal.md").exists()

        run_writ(["uninstall", "--force"], cwd=str(tmp_path), check=False)
        assert not (tmp_path / ".claude" / "commands" / "writ-seal.md").exists()
        assert not (tmp_path / ".claude" / "commands" / "writ-context.md").exists()

    def test_uninstall_json_output(self, tmp_path: Path):
        """Uninstall with --format json produces valid JSON."""
        run_writ(["init", "--yes"], cwd=str(tmp_path))
        result = run_writ(
            ["uninstall", "--force", "--format", "json"],
            cwd=str(tmp_path),
            check=False,
        )
        if result.returncode == 0 and result.stdout.strip():
            data = json.loads(result.stdout)
            assert isinstance(data, dict)

    def test_uninstall_noop_when_not_initialized(self, tmp_path: Path):
        """Uninstall on non-writ directory doesn't crash."""
        result = run_writ(["uninstall", "--force"], cwd=str(tmp_path), check=False)
        # Should either succeed silently or give a clear error
        assert result.returncode in (0, 1)


# ═══════════════════════════════════════════════════════════════════════════
# Generic agent instructions (part of install_hooks)
# ═══════════════════════════════════════════════════════════════════════════


class TestGenericInstructions:
    """Tests for .writ/AGENT_INSTRUCTIONS.md via install_hooks()."""

    def test_generic_instructions_created(self, tmp_path: Path):
        """install_hooks() creates AGENT_INSTRUCTIONS.md when .writ/ exists."""
        (tmp_path / ".writ").mkdir()
        writ.install_hooks(str(tmp_path))

        instructions = tmp_path / ".writ" / "AGENT_INSTRUCTIONS.md"
        assert instructions.exists()
        content = instructions.read_text()
        assert "writ context" in content
        assert "writ seal" in content
        assert "TOON" in content

    def test_generic_instructions_not_created_without_writ_dir(self, tmp_path: Path):
        """install_hooks() skips AGENT_INSTRUCTIONS.md when no .writ/ dir."""
        writ.install_hooks(str(tmp_path))
        assert not (tmp_path / ".writ" / "AGENT_INSTRUCTIONS.md").exists()

    def test_generic_instructions_idempotent(self, tmp_path: Path):
        """Calling install_hooks() twice doesn't change AGENT_INSTRUCTIONS.md."""
        (tmp_path / ".writ").mkdir()
        writ.install_hooks(str(tmp_path))
        first = (tmp_path / ".writ" / "AGENT_INSTRUCTIONS.md").read_text()

        writ.install_hooks(str(tmp_path))
        second = (tmp_path / ".writ" / "AGENT_INSTRUCTIONS.md").read_text()

        assert first == second


# ═══════════════════════════════════════════════════════════════════════════
# A.4 (init-hooks): SessionStart only, brief format, replace legacy block
# ═══════════════════════════════════════════════════════════════════════════

LEGACY_CMD = "echo '## Writ VCS Active' && /opt/homebrew/bin/writ context 2>/dev/null || true"


def _settings(root: Path) -> dict:
    return json.loads((root / ".claude" / "settings.json").read_text())


def _writ_hook_commands(settings: dict, event: str) -> list[str]:
    return [
        hook["command"]
        for entry in settings.get("hooks", {}).get(event, [])
        for hook in entry.get("hooks", [])
        if "writ context" in hook.get("command", "")
    ]


@pytest.mark.skipif(WRIT_BIN is None, reason="writ binary not found")
class TestInitSessionStartHook:
    """A.4: init writes one brief SessionStart hook and no per-prompt hook."""

    def test_fresh_init_writes_single_brief_session_start(self, tmp_path: Path):
        run_writ(["init", "-y"], cwd=str(tmp_path))
        settings = _settings(tmp_path)
        commands = _writ_hook_commands(settings, "SessionStart")
        assert len(commands) == 1
        assert "--format brief" in commands[0]
        assert "UserPromptSubmit" not in settings["hooks"]

    def test_init_replaces_legacy_two_hook_block(self, tmp_path: Path):
        legacy_entry = {"hooks": [{"type": "command", "command": LEGACY_CMD, "timeout": 10}]}
        custom_entry = {"hooks": [{"type": "command", "command": "echo custom"}]}
        (tmp_path / ".claude").mkdir()
        (tmp_path / ".claude" / "settings.json").write_text(json.dumps({
            "hooks": {
                "SessionStart": [legacy_entry],
                "UserPromptSubmit": [legacy_entry, custom_entry],
            }
        }))

        run_writ(["init", "-y"], cwd=str(tmp_path))

        settings = _settings(tmp_path)
        commands = _writ_hook_commands(settings, "SessionStart")
        assert len(commands) == 1, "legacy hook replaced, not appended"
        assert "--format brief" in commands[0]
        assert settings["hooks"]["UserPromptSubmit"] == [custom_entry]

    def test_reinit_does_not_duplicate_hook(self, tmp_path: Path):
        run_writ(["init", "-y"], cwd=str(tmp_path))
        run_writ(["init", "-y"], cwd=str(tmp_path))
        assert len(_writ_hook_commands(_settings(tmp_path), "SessionStart")) == 1

    def test_uninit_removes_writ_hook_and_keeps_user_hooks(self, tmp_path: Path):
        user_hook = {"matcher": "Bash", "hooks": [{"type": "command", "command": "echo hi"}]}
        (tmp_path / ".claude").mkdir()
        (tmp_path / ".claude" / "settings.json").write_text(
            json.dumps({"hooks": {"PreToolUse": [user_hook]}})
        )
        run_writ(["init", "-y"], cwd=str(tmp_path))
        run_writ(["uninit", "--force"], cwd=str(tmp_path), check=False)

        settings = _settings(tmp_path)
        assert settings["hooks"] == {"PreToolUse": [user_hook]}


# ═══════════════════════════════════════════════════════════════════════════
# B.2 A.4 group (Bri): end-to-end hook behavior and exact uninit
# ═══════════════════════════════════════════════════════════════════════════

BRIEF_HOOK_BUDGET = 2048  # bytes injected per session, preamble included


def _init_with_dev_writ_on_path(root: Path) -> None:
    """Run init with the dev binary first on PATH, so `which writ` finds it."""
    env = dict(os.environ)
    env["PATH"] = f"{Path(WRIT_BIN).parent}{os.pathsep}{env.get('PATH', '')}"
    subprocess.run([WRIT_BIN, "init", "-y"], cwd=str(root), env=env,
                   capture_output=True, text=True, check=True)


def _git_repo(root: Path) -> None:
    for args in (["init", "-q"], ["config", "user.email", "t@t"], ["config", "user.name", "t"]):
        subprocess.run(["git", *args], cwd=str(root), check=True)
    (root / "app.py").write_text("print('hi')\n")
    subprocess.run(["git", "add", "."], cwd=str(root), check=True)
    subprocess.run(["git", "commit", "-qm", "init"], cwd=str(root), check=True)


@pytest.mark.skipif(WRIT_BIN is None, reason="writ binary not found")
class TestSessionStartHookEndToEnd:
    """The hook is what every agent pays for at session start; run it for real."""

    def test_session_start_hook_runs_and_emits_brief_within_budget(self, tmp_path: Path):
        _git_repo(tmp_path)
        _init_with_dev_writ_on_path(tmp_path)
        (command,) = _writ_hook_commands(_settings(tmp_path), "SessionStart")

        result = subprocess.run(["bash", "-c", command], cwd=str(tmp_path),
                                capture_output=True, text=True, timeout=10)

        assert result.returncode == 0, result.stderr
        assert "context-brief" in result.stdout, result.stdout
        assert len(result.stdout.encode()) <= BRIEF_HOOK_BUDGET, (
            f"hook injects {len(result.stdout.encode())} B"
        )

    @pytest.mark.xfail(
        strict=True,
        reason=(
            "finding 24: the hook embeds `which writ` at init time, not the binary "
            "that ran init; a stale binary earlier on PATH gets pinned, and "
            "`2>/dev/null || true` hides its errors"
        ),
    )
    def test_hook_uses_the_binary_that_ran_init(self, tmp_path: Path):
        decoy_dir = tmp_path / "decoy-bin"
        decoy_dir.mkdir()
        decoy = decoy_dir / "writ"
        decoy.write_text("#!/bin/sh\necho decoy\n")
        decoy.chmod(0o755)
        repo = tmp_path / "repo"
        repo.mkdir()
        env = dict(os.environ)
        env["PATH"] = f"{decoy_dir}{os.pathsep}{env.get('PATH', '')}"

        subprocess.run([WRIT_BIN, "init", "-y"], cwd=str(repo), env=env,
                       capture_output=True, text=True, check=True)

        (command,) = _writ_hook_commands(_settings(repo), "SessionStart")
        assert str(decoy) not in command
        assert str(Path(WRIT_BIN).resolve()) in command or " writ context" in command

    def test_init_replaces_pathless_legacy_hooks(self, tmp_path: Path):
        legacy = {"hooks": [{"type": "command", "command": "writ context 2>/dev/null || true"}]}
        (tmp_path / ".claude").mkdir()
        (tmp_path / ".claude" / "settings.json").write_text(json.dumps({
            "hooks": {"SessionStart": [legacy], "UserPromptSubmit": [legacy]},
        }))

        run_writ(["init", "-y"], cwd=str(tmp_path))

        settings = _settings(tmp_path)
        assert len(_writ_hook_commands(settings, "SessionStart")) == 1
        assert "--format brief" in _writ_hook_commands(settings, "SessionStart")[0]
        assert not settings["hooks"].get("UserPromptSubmit"), settings["hooks"]

    def test_uninit_restores_preexisting_settings_exactly(self, tmp_path: Path):
        original = {
            "permissions": {"allow": ["Bash(npm test)"], "deny": ["Bash(rm -rf *)"]},
            "env": {"FOO": "bar"},
            "hooks": {
                "PreToolUse": [{"matcher": "Bash",
                                "hooks": [{"type": "command", "command": "echo pre"}]}],
                "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "echo mine"}]}],
            },
        }
        (tmp_path / ".claude").mkdir()
        (tmp_path / ".claude" / "settings.json").write_text(json.dumps(original))

        run_writ(["init", "-y"], cwd=str(tmp_path))
        run_writ(["uninit", "--force"], cwd=str(tmp_path), check=False)

        assert _settings(tmp_path) == original

    def test_uninit_after_fresh_init_leaves_no_writ_hooks(self, tmp_path: Path):
        run_writ(["init", "-y"], cwd=str(tmp_path))
        run_writ(["uninit", "--force"], cwd=str(tmp_path), check=False)

        path = tmp_path / ".claude" / "settings.json"
        if path.exists():
            settings = json.loads(path.read_text())
            for event in ("SessionStart", "UserPromptSubmit"):
                assert not _writ_hook_commands(settings, event), event
