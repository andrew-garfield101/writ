"""UPG.12: Python binding tests for upgrade & migration.

Tests the doctor() and version_info() Python bindings:
- Fresh repo passes doctor checks
- Doctor report structure is correct
- Version info accessible and well-formed
- Legacy repo (no version.toml) opens successfully
- Doctor detects injected failures

Bindings tested: doctor, version_info
"""

import os
import shutil
from pathlib import Path

import pytest
import writ


class TestDoctorBinding:
    """repo.doctor() returns structured health check report."""

    def test_doctor_returns_dict(self, tmp_repo):
        """Doctor returns a dict with required keys."""
        repo, path = tmp_repo
        report = repo.doctor()

        assert isinstance(report, dict)
        assert "checks" in report
        assert "passed" in report
        assert "failed" in report
        assert "warnings" in report
        assert "is_healthy" in report

    def test_doctor_checks_are_list_of_dicts(self, tmp_repo):
        """Each check has name, status, message."""
        repo, path = tmp_repo
        report = repo.doctor()

        assert isinstance(report["checks"], list)
        assert len(report["checks"]) == 8

        for check in report["checks"]:
            assert "name" in check
            assert "status" in check
            assert "message" in check
            assert check["status"] in ("pass", "fail", "warning")

    def test_doctor_counts_sum_correctly(self, tmp_repo):
        """Passed + failed + warnings = total checks."""
        repo, path = tmp_repo
        report = repo.doctor()

        total = report["passed"] + report["failed"] + report["warnings"]
        assert total == len(report["checks"])

    def test_doctor_fresh_repo_healthy(self, tmp_repo):
        """Fresh repo passes all doctor checks."""
        repo, path = tmp_repo
        report = repo.doctor()

        assert report["is_healthy"], (
            f"fresh repo should be healthy, failures: "
            f"{[c for c in report['checks'] if c['status'] == 'fail']}"
        )
        assert report["failed"] == 0

    def test_doctor_detects_missing_directory(self, tmp_repo):
        """Doctor reports failure when a required directory is removed."""
        repo, path = tmp_repo
        writ_dir = path / ".writ"

        # Remove objects directory
        shutil.rmtree(str(writ_dir / "objects"))

        report = repo.doctor()
        dir_check = next(c for c in report["checks"] if c["name"] == "directories")
        assert dir_check["status"] == "fail"
        assert "objects" in dir_check["message"]

    def test_doctor_detects_corrupt_index(self, tmp_repo):
        """Doctor reports failure when index.json is corrupted."""
        repo, path = tmp_repo
        writ_dir = path / ".writ"

        (writ_dir / "index.json").write_text("not valid json")

        report = repo.doctor()
        idx_check = next(c for c in report["checks"] if c["name"] == "index")
        assert idx_check["status"] == "fail"

    def test_doctor_check_names(self, tmp_repo):
        """Doctor runs all 8 expected checks by name."""
        repo, path = tmp_repo
        report = repo.doctor()

        names = {c["name"] for c in report["checks"]}
        expected = {
            "version_file",
            "schema_version",
            "directories",
            "index",
            "config",
            "master_key",
            "specs",
            "seals",
        }
        assert names == expected


class TestVersionInfoBinding:
    """repo.version_info() returns version metadata."""

    def test_version_info_returns_dict(self, tmp_repo):
        """Version info returns a dict with required keys."""
        repo, path = tmp_repo
        info = repo.version_info()

        assert isinstance(info, dict)
        assert "schema_version" in info
        assert "created_by" in info
        assert "last_opened_by" in info

    def test_version_info_schema_is_current(self, tmp_repo):
        """Schema version is 1 (current)."""
        repo, path = tmp_repo
        info = repo.version_info()

        assert info["schema_version"] == 1

    def test_version_info_has_binary_version(self, tmp_repo):
        """created_by and last_opened_by contain version strings."""
        repo, path = tmp_repo
        info = repo.version_info()

        assert isinstance(info["created_by"], str)
        assert len(info["created_by"]) > 0
        assert isinstance(info["last_opened_by"], str)
        assert len(info["last_opened_by"]) > 0

    def test_version_info_has_timestamps(self, tmp_repo):
        """Version info includes created_at and last_opened_at."""
        repo, path = tmp_repo
        info = repo.version_info()

        assert info.get("created_at") is not None
        assert info.get("last_opened_at") is not None


class TestLegacyRepoCompat:
    """Opening repos without version.toml works via auto-migration."""

    def test_legacy_repo_opens_successfully(self, tmp_path):
        """A repo with version.toml removed still opens (auto-migrates)."""
        # Create a normal repo
        repo = writ.Repository.init(str(tmp_path))
        del repo

        # Remove version.toml to simulate a legacy repo
        version_path = tmp_path / ".writ" / "version.toml"
        if version_path.exists():
            os.remove(str(version_path))

        # Re-open — should auto-migrate from v0 → v1
        repo2 = writ.Repository.open(str(tmp_path))

        # After migration, version should be current
        info = repo2.version_info()
        assert info["schema_version"] == 1

    def test_legacy_repo_creates_missing_dirs(self, tmp_path):
        """Auto-migration creates directories that were added post-launch."""
        repo = writ.Repository.init(str(tmp_path))
        del repo

        writ_dir = tmp_path / ".writ"

        # Remove version.toml and proposals/ to simulate old repo
        version_path = writ_dir / "version.toml"
        if version_path.exists():
            os.remove(str(version_path))
        proposals_dir = writ_dir / "proposals"
        if proposals_dir.exists():
            shutil.rmtree(str(proposals_dir))

        # Re-open triggers migration
        repo2 = writ.Repository.open(str(tmp_path))
        del repo2

        # proposals/ should be recreated
        assert (writ_dir / "proposals").is_dir()
