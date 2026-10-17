"""Fleet numbers contract (sprint 6, 0.4.3; Haris's `receipt-and-scale` item 2
and Bri's bench/fleet harness). Written before the code; strict xfail until
each lands (the gate fails on XPASS, so a landed item must drop its marker).

Contract for ``bench/fleet_bench.py`` (Haris):
    python3 bench/fleet_bench.py --writ BIN --agents N --seals-per-agent K \
        --files F --json OUT
exits 0, prints every number on stdout with the binary it came from, and
writes JSON with at least:
    binary: {path, version, sha256}
    repo: {files, git_commits}
    agents, specs, seals            (agents == specs == N, seals == N*K)
    seal_ms: {p50, max}
    context_ms, doctor_fast_ms, survival_full_ms, survival_incremental_ms,
    handoff_ms                      (numbers, > 0)
    store_ok: true                  (integrity after the run)
    date                            (ISO 8601)
The full-size numbers (50 x 20 on 1,000 files) are a separate run whose
JSON is the evidence; this test runs a small size so it fits the gate.

Contract for ``bench/fleet/sim.py`` (Bri, 50 in-process identities):
    python3 bench/fleet/sim.py --writ BIN --agents N --edits K --json OUT
drives N identities through the installed PostToolUse hook (stdin JSON,
WRIT_AGENT_ID per identity), concurrently, on a scratch repo, then reports
hook_ms {p50, max} (hook return time), window_ms {p50, max} (edit to seal),
edits_never_sealed == 0 after each session's Stop, seals <= N*K (deferred
paths drain into later seals, ruling 149), specs == N (one auto spec each), cross
(seals carrying another identity's path) == 0, hook_exit_nonzero == 0,
hooks_log_lines, binary {path, version, sha256}.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
FLEET_BENCH = ROOT / "bench" / "fleet_bench.py"
SIM = ROOT / "bench" / "fleet" / "sim.py"


def _writ() -> str:
    """The binary under test: WRIT_BIN, else the release build, else PATH."""
    env = os.environ.get("WRIT_BIN")
    if env:
        return env
    rel = ROOT / "target" / "release" / "writ"
    if rel.exists():
        return str(rel)
    found = shutil.which("writ")
    if not found:
        pytest.skip("no writ binary")
    return found


def _run(script: Path, args: list[str], out: Path) -> dict:
    proc = subprocess.run(
        [sys.executable, str(script), *args, "--json", str(out)],
        capture_output=True,
        text=True,
        timeout=600,
        cwd=ROOT,
    )
    assert proc.returncode == 0, proc.stdout + proc.stderr
    data = json.loads(out.read_text())
    data["_stdout"] = proc.stdout
    return data


def _assert_binary(data: dict, writ: str) -> None:
    b = data["binary"]
    assert Path(b["path"]).resolve() == Path(writ).resolve()
    assert b["version"].startswith("writ ") or b["version"][0].isdigit()
    assert len(b["sha256"]) == 64
    # "prints every number with its binary": stdout names version and sha.
    assert b["sha256"][:12] in data["_stdout"]


@pytest.mark.xfail(
    not FLEET_BENCH.exists(), reason="pending bench/fleet_bench.py (Haris)", strict=True
)
def test_fleet_bench_prints_every_number_with_its_binary(tmp_path: Path) -> None:
    writ = _writ()
    data = _run(
        FLEET_BENCH,
        ["--writ", writ, "--agents", "3", "--seals-per-agent", "2", "--files", "50"],
        tmp_path / "fleet.json",
    )
    _assert_binary(data, writ)
    assert data["agents"] == 3 and data["specs"] == 3
    assert data["seals"] >= 6
    assert data["repo"]["files"] >= 50 and data["repo"]["git_commits"] >= 1
    for key in ("context_ms", "doctor_fast_ms", "survival_full_ms",
                "survival_incremental_ms", "handoff_ms"):
        assert isinstance(data[key], (int, float)) and data[key] > 0, key
        assert str(round(data[key])) in data["_stdout"] or key in data["_stdout"], key
    assert 0 < data["seal_ms"]["p50"] <= data["seal_ms"]["max"]
    assert data["store_ok"] is True
    assert "T" in data["date"]


def _auto_seal_landed(writ: str) -> bool:
    """True once `writ init -y` installs a PostToolUse hook."""
    import tempfile

    with tempfile.TemporaryDirectory() as d:
        subprocess.run(["git", "init", "-q", d], check=True)
        Path(d, "a.txt").write_text("a\n")
        subprocess.run(["git", "-C", d, "add", "-A"], check=True)
        subprocess.run(
            ["git", "-C", d, "-c", "user.name=f", "-c", "user.email=f@example.invalid",
             "commit", "-qm", "i"],
            check=True,
        )
        subprocess.run([writ, "init", "-y"], cwd=d, capture_output=True)
        s = Path(d, ".claude", "settings.json")
        return s.exists() and "PostToolUse" in s.read_text()


def test_fleet_sim_no_cross_seals_and_every_hook_exit_zero(tmp_path: Path) -> None:
    writ = _writ()
    if not _auto_seal_landed(writ):
        pytest.skip(f"{writ} installs no PostToolUse hook (a release before 0.4.3)")
    data = _run(SIM, ["--writ", writ, "--agents", "5", "--edits", "3"], tmp_path / "sim.json")
    _assert_binary(data, writ)
    assert data["specs"] == 5
    assert 0 < data["seals"] <= 15
    assert data["cross"] == 0
    assert data["hook_exit_nonzero"] == 0
    assert 0 < data["hook_ms"]["p50"] <= data["hook_ms"]["max"]
    # Ruling 149: every edit sealed by the end (a later hook or Stop).
    assert data["edits"] == 15 and data["edits_never_sealed"] == 0
    assert data["window_ms"]["p50"] is not None
