"""Tests for framework file generation during writ init (T.8, T.9, T.10).

Tests the contracts for:
- CLAUDE.md creation and marker-based append (T.8)
- CLAUDE.md marker-based update on reinit (T.9)
- Idempotent .gitignore modification (T.10)
- AGENTS.md creation and marker-based append (parallel to T.8/T.9)

These tests validate the string manipulation and file I/O contracts
defined in writ-init-spec.md. They use pure Python helpers to test
the expected behavior independent of the Rust implementation.

Once Izzy's implementations land (I.12-I.17), these will be wired to
test the actual CLI/SDK calls. For now they test the contract.
"""

import os
import textwrap
from pathlib import Path
from typing import Optional

import pytest

# ---------------------------------------------------------------------------
# Marker constants (from writ-init-spec.md)
# ---------------------------------------------------------------------------

BEGIN_MARKER = "<!-- BEGIN WRIT CONFIGURATION — managed by writ init -->"
END_MARKER = "<!-- END WRIT CONFIGURATION -->"

WRIT_SECTION_KEYWORDS = [
    "Writ Version Control",
    "writ context",
    "writ seal",
    "writ spec",
    "writ log",
]

GITIGNORE_ENTRY = ".writ/"
GITIGNORE_COMMENT = "# Writ version control state"


# ---------------------------------------------------------------------------
# Helper functions — simulate the expected behavior from the spec
# ---------------------------------------------------------------------------

def append_writ_section(existing_content: Optional[str], writ_section: str) -> str:
    """Append or replace writ section in a markdown file.

    Simulates the expected behavior from writ-init-spec.md:
    - If file doesn't exist (None), create with just the writ section
    - If file exists but has no markers, append after separator
    - If file exists with markers, replace content between markers
    """
    if existing_content is None:
        return f"{BEGIN_MARKER}\n{writ_section}\n{END_MARKER}\n"

    if BEGIN_MARKER in existing_content and END_MARKER in existing_content:
        # Replace existing writ section
        before = existing_content.split(BEGIN_MARKER)[0]
        after = existing_content.split(END_MARKER)[1]
        return f"{before}{BEGIN_MARKER}\n{writ_section}\n{END_MARKER}{after}"

    # Append with separator
    separator = "\n---\n\n"
    if existing_content.endswith("\n"):
        return f"{existing_content}{separator}{BEGIN_MARKER}\n{writ_section}\n{END_MARKER}\n"
    return f"{existing_content}\n{separator}{BEGIN_MARKER}\n{writ_section}\n{END_MARKER}\n"


def remove_writ_section(content: str) -> str:
    """Remove writ section between markers, preserving surrounding content.

    Used by writ uninit.
    """
    if BEGIN_MARKER not in content or END_MARKER not in content:
        return content

    before = content.split(BEGIN_MARKER)[0]
    after = content.split(END_MARKER)[1]

    # Clean up the separator before the markers
    if before.rstrip().endswith("---"):
        before = before.rstrip()[:-3].rstrip() + "\n"

    return before + after.lstrip("\n")


def update_gitignore(existing_content: Optional[str]) -> str:
    """Add .writ/ entry to .gitignore if not already present.

    Simulates idempotent .gitignore modification from spec.
    """
    if existing_content is None:
        return f"{GITIGNORE_COMMENT}\n{GITIGNORE_ENTRY}\n"

    # Check if already present
    lines = existing_content.split("\n")
    for line in lines:
        stripped = line.strip()
        if stripped == GITIGNORE_ENTRY or stripped == GITIGNORE_ENTRY.rstrip("/"):
            return existing_content  # Already present, no change

    # Append
    if existing_content.endswith("\n"):
        return f"{existing_content}\n{GITIGNORE_COMMENT}\n{GITIGNORE_ENTRY}\n"
    return f"{existing_content}\n\n{GITIGNORE_COMMENT}\n{GITIGNORE_ENTRY}\n"


# ---------------------------------------------------------------------------
# Sample writ section content (from spec)
# ---------------------------------------------------------------------------

SAMPLE_WRIT_SECTION = textwrap.dedent("""\
    ## Writ Version Control

    This project uses writ for version control alongside git.
    All agents must use writ commands for checkpointing and context retrieval.

    ### Required Workflow
    1. At session start, run `writ context` to get structured project state
    2. Before starting work, create or claim a spec: `writ spec create "<task description>"`
    3. Checkpoint work regularly: `writ seal -s "<summary>" --spec <spec-id>`
    4. When task is complete: `writ finish --spec <spec-id>`

    ### Context Retrieval
    - `writ context` returns project state in token-optimized TOON format by default
    - TOON uses ~40% fewer tokens than JSON with identical information
    - For standard JSON output: `writ context --format json`

    ### Available Commands
    - `writ context` — structured project state (files, specs, recent activity)
    - `writ seal -s "<summary>"` — create a checkpoint of current work
    - `writ spec create "<description>"` — create a new task spec
    - `writ spec list` — view active specs
    - `writ log` — recent seal history

    ### Slash Commands
    - `/writ-seal` — interactive seal creation
    - `/writ-context` — get project context""")

SAMPLE_AGENTS_SECTION = textwrap.dedent("""\
    ## Version Control — Writ

    This project uses writ (AI-native version control) for checkpointing and coordination.

    ### Workflow
    1. Run `writ context` at the start of every task to understand project state
    2. Checkpoint with `writ seal -s "<summary>"` after meaningful progress
    3. Create specs for tasks: `writ spec create "<description>"`
    4. Complete tasks with `writ finish --spec <spec-id>`

    ### Key Commands
    - `writ context` — get structured project state
    - `writ seal -s "<summary>"` — checkpoint work
    - `writ spec create / list / finish` — task management
    - `writ log` — recent history""")


# ═══════════════════════════════════════════════════════════════════════════
# T.8: CLAUDE.md append preserves existing content
# ═══════════════════════════════════════════════════════════════════════════

class TestClaudeMdCreation:
    """T.8: Tests for CLAUDE.md creation and append behavior."""

    def test_create_claudemd_new(self):
        """Creating CLAUDE.md from scratch wraps section in markers."""
        result = append_writ_section(None, SAMPLE_WRIT_SECTION)
        assert BEGIN_MARKER in result
        assert END_MARKER in result
        assert "Writ Version Control" in result

    def test_create_claudemd_markers_wrap_content(self):
        """Markers appear before and after the writ section."""
        result = append_writ_section(None, SAMPLE_WRIT_SECTION)
        lines = result.split("\n")
        begin_idx = next(i for i, l in enumerate(lines) if BEGIN_MARKER in l)
        end_idx = next(i for i, l in enumerate(lines) if END_MARKER in l)
        assert begin_idx < end_idx
        # Writ content is between markers
        middle = "\n".join(lines[begin_idx + 1:end_idx])
        assert "writ context" in middle

    def test_append_to_existing_claudemd(self):
        """Existing content is preserved when appending writ section."""
        existing = textwrap.dedent("""\
            # My Project

            This is my existing CLAUDE.md content.

            ## Important Rules
            - Always test your code
            - Use type hints
            """)
        result = append_writ_section(existing, SAMPLE_WRIT_SECTION)

        # Existing content preserved
        assert "My Project" in result
        assert "Important Rules" in result
        assert "Always test your code" in result

        # Writ section added
        assert BEGIN_MARKER in result
        assert "Writ Version Control" in result

    def test_append_adds_separator(self):
        """Separator (---) appears between existing content and writ section."""
        existing = "# My Project\n\nSome content.\n"
        result = append_writ_section(existing, SAMPLE_WRIT_SECTION)
        assert "---" in result
        # Separator is between existing content and markers
        separator_idx = result.index("---")
        marker_idx = result.index(BEGIN_MARKER)
        assert separator_idx < marker_idx

    def test_existing_content_byte_identical(self):
        """Content before the separator is byte-identical to original."""
        existing = "# My Project\n\nLine 1\nLine 2\nLine 3\n"
        result = append_writ_section(existing, SAMPLE_WRIT_SECTION)
        # Everything before the separator should start with the original content
        assert result.startswith(existing)

    def test_writ_section_contains_required_commands(self):
        """Writ section includes all required command references."""
        result = append_writ_section(None, SAMPLE_WRIT_SECTION)
        for keyword in WRIT_SECTION_KEYWORDS:
            assert keyword in result, f"Missing required keyword: {keyword}"

    def test_markers_are_html_comments(self):
        """Markers are HTML comments (invisible in rendered markdown)."""
        assert BEGIN_MARKER.startswith("<!--")
        assert BEGIN_MARKER.endswith("-->")
        assert END_MARKER.startswith("<!--")
        assert END_MARKER.endswith("-->")


# ═══════════════════════════════════════════════════════════════════════════
# T.9: CLAUDE.md marker-based update on reinit
# ═══════════════════════════════════════════════════════════════════════════

class TestClaudeMdReinit:
    """T.9: Tests for marker-based CLAUDE.md update on reinit."""

    def test_reinit_replaces_marker_section(self):
        """Running init twice replaces the writ section, not duplicates it."""
        # First init
        first = append_writ_section(None, "OLD CONTENT v1")
        assert "OLD CONTENT v1" in first

        # Second init (reinit) with new content
        second = append_writ_section(first, "NEW CONTENT v2")
        assert "NEW CONTENT v2" in second
        assert "OLD CONTENT v1" not in second

    def test_reinit_preserves_user_content_before(self):
        """User content before markers is preserved on reinit."""
        existing = "# My Rules\n\nDo important things.\n"
        first = append_writ_section(existing, "WRIT V1")
        second = append_writ_section(first, "WRIT V2")

        assert "My Rules" in second
        assert "Do important things" in second
        assert "WRIT V2" in second
        assert "WRIT V1" not in second

    def test_reinit_preserves_user_content_after(self):
        """User content after markers is preserved on reinit."""
        # Simulate user adding content after the writ section
        first = append_writ_section("# Header\n", "WRIT V1")
        with_user_addition = first + "\n## My Custom Section\n\nUser added this.\n"

        second = append_writ_section(with_user_addition, "WRIT V2")
        assert "My Custom Section" in second
        assert "User added this" in second
        assert "WRIT V2" in second
        assert "WRIT V1" not in second

    def test_reinit_no_duplicate_sections(self):
        """Multiple reinits don't create duplicate writ sections."""
        content = append_writ_section(None, "V1")
        content = append_writ_section(content, "V2")
        content = append_writ_section(content, "V3")

        # Should have exactly one begin and one end marker
        assert content.count(BEGIN_MARKER) == 1
        assert content.count(END_MARKER) == 1
        assert "V3" in content
        assert "V1" not in content
        assert "V2" not in content

    def test_reinit_with_modified_content_between_markers(self):
        """If user edited text between markers, reinit still replaces cleanly."""
        first = append_writ_section(None, "ORIGINAL")
        # User modifies content between markers
        modified = first.replace("ORIGINAL", "USER MODIFIED THIS")
        assert "USER MODIFIED THIS" in modified

        # Reinit should replace everything between markers
        second = append_writ_section(modified, "FRESH CONTENT")
        assert "FRESH CONTENT" in second
        assert "USER MODIFIED THIS" not in second


# ═══════════════════════════════════════════════════════════════════════════
# T.10: Idempotent .gitignore modification
# ═══════════════════════════════════════════════════════════════════════════

class TestGitignoreIdempotent:
    """T.10: Tests for idempotent .gitignore modification."""

    def test_gitignore_creates_new(self):
        """No .gitignore → creates one with .writ/ entry and comment."""
        result = update_gitignore(None)
        assert GITIGNORE_ENTRY in result
        assert GITIGNORE_COMMENT in result

    def test_gitignore_appends_to_existing(self):
        """Existing .gitignore → appends .writ/ entry at end."""
        existing = "node_modules/\n*.pyc\n__pycache__/\n"
        result = update_gitignore(existing)

        # Existing entries preserved
        assert "node_modules/" in result
        assert "*.pyc" in result
        assert "__pycache__/" in result

        # Writ entry added
        assert GITIGNORE_ENTRY in result

    def test_gitignore_idempotent_no_change(self):
        """Already has .writ/ → no modification."""
        existing = "node_modules/\n.writ/\n*.pyc\n"
        result = update_gitignore(existing)
        assert result == existing  # Byte-identical, no changes

    def test_gitignore_idempotent_with_comment(self):
        """Already has .writ/ with comment → no modification."""
        existing = "node_modules/\n\n# Writ version control state\n.writ/\n"
        result = update_gitignore(existing)
        assert result == existing

    def test_gitignore_preserves_existing_content(self):
        """All existing entries remain after append."""
        existing = textwrap.dedent("""\
            # Python
            __pycache__/
            *.pyc
            .venv/

            # IDE
            .idea/
            .vscode/

            # Build
            dist/
            build/
            """)
        result = update_gitignore(existing)

        for line in existing.strip().split("\n"):
            if line.strip():
                assert line in result, f"Missing line: {line}"

    def test_gitignore_handles_no_trailing_newline(self):
        """Works when existing file doesn't end with newline."""
        existing = "node_modules/\n*.pyc"  # No trailing newline
        result = update_gitignore(existing)
        assert GITIGNORE_ENTRY in result
        assert "node_modules/" in result

    def test_gitignore_comment_before_entry(self):
        """Comment appears before the .writ/ entry."""
        result = update_gitignore(None)
        comment_idx = result.index(GITIGNORE_COMMENT)
        entry_idx = result.index(GITIGNORE_ENTRY)
        assert comment_idx < entry_idx

    def test_gitignore_detects_without_trailing_slash(self):
        """Detects .writ (without trailing slash) as already present."""
        existing = "node_modules/\n.writ\n"
        result = update_gitignore(existing)
        # Should not add duplicate — .writ covers .writ/
        assert result == existing

    def test_gitignore_no_duplicate_on_repeated_calls(self):
        """Calling update_gitignore multiple times doesn't duplicate entries."""
        result = update_gitignore(None)
        result = update_gitignore(result)
        result = update_gitignore(result)
        assert result.count(GITIGNORE_ENTRY) == 1


# ═══════════════════════════════════════════════════════════════════════════
# AGENTS.md tests (parallel to CLAUDE.md)
# ═══════════════════════════════════════════════════════════════════════════

class TestAgentsMdCreation:
    """Tests for AGENTS.md creation and marker-based management."""

    def test_create_agentsmd_new(self):
        """Creates new AGENTS.md with writ section wrapped in markers."""
        result = append_writ_section(None, SAMPLE_AGENTS_SECTION)
        assert BEGIN_MARKER in result
        assert END_MARKER in result
        assert "Version Control — Writ" in result

    def test_append_to_existing_agentsmd(self):
        """Existing AGENTS.md content preserved when appending."""
        existing = textwrap.dedent("""\
            # Agent Configuration

            ## Code Style
            Follow PEP 8 guidelines.
            """)
        result = append_writ_section(existing, SAMPLE_AGENTS_SECTION)
        assert "Agent Configuration" in result
        assert "Code Style" in result
        assert "Version Control — Writ" in result

    def test_agentsmd_markers_present(self):
        """Marker comments wrap the writ section."""
        result = append_writ_section(None, SAMPLE_AGENTS_SECTION)
        assert BEGIN_MARKER in result
        assert END_MARKER in result

    def test_agentsmd_reinit_replaces(self):
        """Reinit replaces writ section in AGENTS.md, preserving user content."""
        existing = "# Config\n\nUser rules.\n"
        first = append_writ_section(existing, "AGENTS V1")
        second = append_writ_section(first, "AGENTS V2")

        assert "User rules" in second
        assert "AGENTS V2" in second
        assert "AGENTS V1" not in second

    def test_agentsmd_required_content(self):
        """AGENTS.md writ section contains required workflow text."""
        result = append_writ_section(None, SAMPLE_AGENTS_SECTION)
        assert "writ context" in result
        assert "writ seal" in result


# ═══════════════════════════════════════════════════════════════════════════
# Uninstall removal tests (T.13 partial — marker removal logic)
# ═══════════════════════════════════════════════════════════════════════════

class TestMarkerRemoval:
    """Tests for surgical removal of writ sections (used by uninstall)."""

    def test_remove_from_new_file(self):
        """Remove writ section from a file that was only writ content."""
        content = append_writ_section(None, SAMPLE_WRIT_SECTION)
        result = remove_writ_section(content)
        assert BEGIN_MARKER not in result
        assert END_MARKER not in result
        assert "Writ Version Control" not in result

    def test_remove_preserves_user_content(self):
        """Removing writ section preserves content before and after."""
        existing = "# My Project\n\nImportant stuff.\n"
        with_writ = append_writ_section(existing, SAMPLE_WRIT_SECTION)
        with_user_after = with_writ + "\n## My Footer\n\nFooter content.\n"

        result = remove_writ_section(with_user_after)
        assert "My Project" in result
        assert "Important stuff" in result
        assert "My Footer" in result
        assert "Footer content" in result
        assert BEGIN_MARKER not in result
        assert "Writ Version Control" not in result

    def test_remove_cleans_separator(self):
        """The --- separator before the writ section is also removed."""
        existing = "# Header\n\nContent.\n"
        with_writ = append_writ_section(existing, "WRIT STUFF")
        result = remove_writ_section(with_writ)
        # Should not have a dangling separator
        assert result.strip().endswith("Content.")

    def test_remove_no_markers_returns_unchanged(self):
        """If no markers found, file is returned unchanged."""
        content = "# Just a normal file\n\nNo writ here.\n"
        result = remove_writ_section(content)
        assert result == content


# ═══════════════════════════════════════════════════════════════════════════
# File I/O integration tests (using tmp_path)
# ═══════════════════════════════════════════════════════════════════════════

class TestFileIOIntegration:
    """Tests that exercise actual file read/write with the helper functions."""

    def test_claudemd_full_lifecycle(self, tmp_path: Path):
        """Full lifecycle: create → reinit → uninstall."""
        claudemd = tmp_path / "CLAUDE.md"

        # Create
        content = append_writ_section(None, SAMPLE_WRIT_SECTION)
        claudemd.write_text(content)
        assert claudemd.exists()
        assert BEGIN_MARKER in claudemd.read_text()

        # Reinit (replace section)
        existing = claudemd.read_text()
        updated = append_writ_section(existing, "UPDATED WRIT SECTION")
        claudemd.write_text(updated)
        assert "UPDATED WRIT SECTION" in claudemd.read_text()
        assert claudemd.read_text().count(BEGIN_MARKER) == 1

        # Uninstall (remove section)
        cleaned = remove_writ_section(claudemd.read_text())
        claudemd.write_text(cleaned)
        assert BEGIN_MARKER not in claudemd.read_text()

    def test_gitignore_full_lifecycle(self, tmp_path: Path):
        """Full lifecycle: create → idempotent check → verify content."""
        gitignore = tmp_path / ".gitignore"

        # Create from scratch
        content = update_gitignore(None)
        gitignore.write_text(content)
        assert GITIGNORE_ENTRY in gitignore.read_text()

        # Idempotent — no change
        existing = gitignore.read_text()
        result = update_gitignore(existing)
        assert result == existing

    def test_append_to_real_claudemd(self, tmp_path: Path):
        """Append to a realistic CLAUDE.md file."""
        claudemd = tmp_path / "CLAUDE.md"
        existing_content = textwrap.dedent("""\
            # Project Instructions

            ## Code Style
            - Use Black for formatting
            - Type hints required on all public functions

            ## Testing
            - All PRs must have tests
            - Use pytest, no unittest

            ## Architecture
            - Keep modules under 500 lines
            - Prefer composition over inheritance
            """)
        claudemd.write_text(existing_content)

        # Append writ section
        content = claudemd.read_text()
        updated = append_writ_section(content, SAMPLE_WRIT_SECTION)
        claudemd.write_text(updated)

        final = claudemd.read_text()

        # All original content intact
        assert "Project Instructions" in final
        assert "Black for formatting" in final
        assert "All PRs must have tests" in final
        assert "composition over inheritance" in final

        # Writ section present
        assert BEGIN_MARKER in final
        assert "Writ Version Control" in final
        assert "writ context" in final

    def test_append_to_real_gitignore(self, tmp_path: Path):
        """Append to a realistic .gitignore file."""
        gitignore = tmp_path / ".gitignore"
        existing_content = textwrap.dedent("""\
            # Python
            __pycache__/
            *.pyc
            *.pyo
            .venv/
            dist/
            *.egg-info/

            # IDE
            .idea/
            .vscode/
            *.swp

            # OS
            .DS_Store
            Thumbs.db
            """)
        gitignore.write_text(existing_content)

        content = gitignore.read_text()
        updated = update_gitignore(content)
        gitignore.write_text(updated)

        final = gitignore.read_text()
        assert GITIGNORE_ENTRY in final
        assert "__pycache__/" in final
        assert ".DS_Store" in final
        assert final.count(GITIGNORE_ENTRY) == 1
