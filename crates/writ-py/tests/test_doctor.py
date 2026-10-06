"""Contract tests for Repository.doctor() (sprint 3, doctor-core).

The report shape, the stable check ids, the honesty condition (headline
names the tier and the survival state, never "safe to finish"), and that a
finding's fix_command is pasteable.
"""

import os
import shutil
import subprocess
from pathlib import Path

import pytest
import writ

CLEAN = "fast checks clean; survival check not available until 0.4.1"
CHECK_IDS = {
    "store_integrity",
    "stale_claim",
    "committed_spec_seal",
    "unsealed_at_risk",
    "version_skew",
    "left_out",
}
FINDING_KEYS = {"check", "severity", "message", "fix_command", "needs_human", "paths"}


def _seal(repo: writ.Repository, summary: str) -> dict:
    return repo.seal(
        summary=summary, agent_id="amis-test", agent_type="agent", status="in-progress"
    )


def _writ_bin() -> str:
    """A built writ CLI: target/release, then target/debug, then PATH.

    Skips the calling test when none exists, which is the case on the
    Python CI jobs (they only run maturin develop). Finding 94.
    """
    root = Path(__file__).resolve().parents[3]
    for profile in ("release", "debug"):
        dev = root / "target" / profile / "writ"
        if dev.exists():
            return str(dev)
    on_path = shutil.which("writ")
    if on_path:
        return on_path
    pytest.skip("writ CLI not built (target/release or target/debug) and not on PATH")


class TestDoctorReportShape:
    def test_clean_repo_headline_exact(self, tmp_repo):
        repo, _ = tmp_repo
        report = repo.doctor()
        assert report["headline"] == CLEAN
        assert report["tier"] == "fast"
        assert report["survival_last_green"] is None
        assert report["clean"] is True
        assert report["findings"] == []

    def test_checks_run_are_known_ids(self, tmp_repo):
        repo, _ = tmp_repo
        report = repo.doctor()
        assert report["checks_run"], "at least one check ran"
        assert set(report["checks_run"]) <= CHECK_IDS

    def test_never_says_safe_to_finish(self, tmp_repo):
        repo, path = tmp_repo
        assert "safe to finish" not in str(repo.doctor()).lower()
        (path / ".writ" / "workspaces" / "main" / "index.json").write_text("{")
        assert "safe to finish" not in str(repo.doctor()).lower()


class TestStoreIntegrity:
    def test_missing_blob_finding_shape_and_fix(self, tmp_repo):
        repo, path = tmp_repo
        (path / "kept.txt").write_text("kept\n")
        seal = _seal(repo, "one")
        blob = next(c["new_hash"] for c in seal["changes"] if c["path"] == "kept.txt")
        os.remove(path / ".writ" / "objects" / blob[:2] / blob[2:])

        report = repo.doctor()
        assert report["clean"] is False
        assert report["red"] >= 1
        finding = report["findings"][0]
        assert set(finding) == FINDING_KEYS
        assert finding["check"] == "store_integrity"
        assert finding["severity"] == "red"
        assert finding["fix_command"] == "writ repair"
        assert finding["paths"] == ["kept.txt"]
        assert "survival check not available until 0.4.1" in report["headline"]

    def test_fix_command_clears_the_finding(self, tmp_repo):
        repo, path = tmp_repo
        (path / "kept.txt").write_text("kept\n")
        seal = _seal(repo, "one")
        blob = next(c["new_hash"] for c in seal["changes"] if c["path"] == "kept.txt")
        os.remove(path / ".writ" / "objects" / blob[:2] / blob[2:])
        fix = repo.doctor()["findings"][0]["fix_command"]

        subprocess.run(fix.replace("writ", _writ_bin(), 1).split(), cwd=path, check=True,
                       capture_output=True)

        assert writ.Repository.open(str(path)).doctor()["clean"] is True
