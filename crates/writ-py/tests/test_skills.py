"""Tests for PY.3: Skills generation/removal Python bindings."""

import os

import writ


class TestGenerateSkills:
    """Tests for writ.generate_skills()."""

    def test_generate_creates_skill_dirs(self, tmp_path):
        """generate_skills creates .claude/skills/ with writ skill directories."""
        result = writ.generate_skills(str(tmp_path))

        assert isinstance(result, dict)
        assert "created" in result
        assert "updated" in result
        assert "skipped" in result
        assert result["created"] > 0

        # Verify skill directories were created.
        skills_dir = tmp_path / ".claude" / "skills"
        assert skills_dir.exists()

        # At least some writ-prefixed dirs should exist.
        writ_dirs = [
            d for d in os.listdir(skills_dir)
            if d.startswith("writ-") and (skills_dir / d).is_dir()
        ]
        assert len(writ_dirs) > 0
        assert result["created"] == len(writ_dirs)

    def test_generate_is_idempotent(self, tmp_path):
        """Running generate_skills twice produces skipped count on second run."""
        first = writ.generate_skills(str(tmp_path))
        assert first["created"] > 0

        second = writ.generate_skills(str(tmp_path))
        assert second["created"] == 0
        assert second["skipped"] == first["created"]


class TestRemoveSkills:
    """Tests for writ.remove_skills()."""

    def test_remove_cleans_up_skills(self, tmp_path):
        """remove_skills removes writ skill directories."""
        writ.generate_skills(str(tmp_path))

        skills_dir = tmp_path / ".claude" / "skills"
        writ_dirs_before = [
            d for d in os.listdir(skills_dir)
            if d.startswith("writ-")
        ]
        assert len(writ_dirs_before) > 0

        removed = writ.remove_skills(str(tmp_path))
        assert isinstance(removed, list)
        assert len(removed) == len(writ_dirs_before)

        # All writ dirs should be gone.
        if skills_dir.exists():
            remaining_writ = [
                d for d in os.listdir(skills_dir)
                if d.startswith("writ-")
            ]
            assert len(remaining_writ) == 0

    def test_remove_preserves_non_writ_skills(self, tmp_path):
        """remove_skills does not touch non-writ skill directories."""
        writ.generate_skills(str(tmp_path))

        # Create a custom (non-writ) skill directory.
        skills_dir = tmp_path / ".claude" / "skills"
        custom_dir = skills_dir / "my-custom-skill"
        custom_dir.mkdir(parents=True, exist_ok=True)
        (custom_dir / "SKILL.md").write_text("# My custom skill\n")

        removed = writ.remove_skills(str(tmp_path))
        assert len(removed) > 0

        # Custom skill should still be there.
        assert custom_dir.exists()
        assert (custom_dir / "SKILL.md").read_text() == "# My custom skill\n"
