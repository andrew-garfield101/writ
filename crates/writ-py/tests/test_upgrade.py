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
    """repo.doctor() returns the 0.4 fast-tier report (doctor-core)."""

    def test_doctor_returns_report_dict(self, tmp_repo):
        """Doctor returns a dict with the fast-tier report keys."""
        repo, path = tmp_repo
        report = repo.doctor()

        assert isinstance(report, dict)
        for key in ("headline", "tier", "survival_last_green", "checks_run",
                    "findings", "clean", "red", "yellow", "elapsed_ms"):
            assert key in report, key

    def test_doctor_counts_match_findings(self, tmp_repo):
        """red + yellow = number of findings."""
        repo, path = tmp_repo
        report = repo.doctor()

        assert report["red"] + report["yellow"] == len(report["findings"])

    def test_doctor_fresh_repo_clean(self, tmp_repo):
        """Fresh repo has no findings."""
        repo, path = tmp_repo
        report = repo.doctor()

        assert report["clean"], report["findings"]
        assert report["headline"] == (
            "fast checks clean; survival check not available until 0.4.1"
        )

    def test_doctor_detects_missing_directory(self, tmp_repo):
        """A removed objects directory is a red store_integrity finding."""
        repo, path = tmp_repo
        shutil.rmtree(str(path / ".writ" / "objects"))

        report = repo.doctor()
        hits = [f for f in report["findings"] if ".writ/objects" in f["paths"]]
        assert hits and hits[0]["check"] == "store_integrity"
        assert hits[0]["severity"] == "red"
        assert hits[0]["fix_command"] == "writ repair"

    def test_doctor_detects_corrupt_index(self, tmp_repo):
        """A corrupt index.json is a red store_integrity finding."""
        repo, path = tmp_repo
        ws_index = path / ".writ" / "workspaces" / "main" / "index.json"
        ws_index.write_text("not valid json")

        report = repo.doctor()
        hits = [f for f in report["findings"] if "index.json" in f["message"]]
        assert hits and hits[0]["severity"] == "red"


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
        """Schema version is 3 (current, 0.3.0 spec fields)."""
        repo, path = tmp_repo
        info = repo.version_info()

        assert info["schema_version"] == 3

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

        # Re-open — should auto-migrate from v0 → current
        repo2 = writ.Repository.open(str(tmp_path))

        # After migration, version should be current
        info = repo2.version_info()
        assert info["schema_version"] == 3

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
