"""E2E test fixtures, CLI options, and JSON reporter.

Reuses helpers from testing/roundtrip/ — no code duplication.
"""

import json
import os
import shutil
import subprocess
import sys
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import List, Optional

import pytest

# ---------------------------------------------------------------------------
# Path setup: import from testing/roundtrip/
# ---------------------------------------------------------------------------

_E2E_DIR = Path(__file__).resolve().parent
_REPO_ROOT = _E2E_DIR.parent.parent.parent.parent  # crates/writ-py/tests/e2e/ → repo root

# Add e2e/ to sys.path so `from helpers.cli import ...` works in test files.
# testing/roundtrip/ is added AFTER so it doesn't shadow our helpers package.
if str(_E2E_DIR) not in sys.path:
    sys.path.insert(0, str(_E2E_DIR))
if str(_REPO_ROOT / "testing" / "roundtrip") not in sys.path:
    sys.path.append(str(_REPO_ROOT / "testing" / "roundtrip"))


# ---------------------------------------------------------------------------
# Binary discovery
# ---------------------------------------------------------------------------

def _find_writ_binary() -> str:
    """Locate writ CLI binary: target/release → target/debug → PATH."""
    candidates = [
        _REPO_ROOT / "target" / "release" / "writ",
        _REPO_ROOT / "target" / "debug" / "writ",
    ]
    for c in candidates:
        if c.exists() and os.access(c, os.X_OK):
            return str(c)
    result = shutil.which("writ")
    if result:
        return result
    pytest.skip("writ binary not found (run cargo build first)")


def _find_claude_binary() -> Optional[str]:
    """Locate claude CLI binary on PATH."""
    return shutil.which("claude")


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture(scope="session")
def writ_bin() -> str:
    """Path to the writ CLI binary."""
    return _find_writ_binary()


@pytest.fixture(scope="session")
def repo_root() -> Path:
    """Repository root directory."""
    return _REPO_ROOT


@pytest.fixture(scope="session")
def claude_bin() -> Optional[str]:
    """Path to claude CLI binary, or None."""
    return _find_claude_binary()


@pytest.fixture
def tmp_git_repo(tmp_path: Path) -> Path:
    """Fresh git-initialized temp directory with initial commit."""
    subprocess.run(["git", "init"], cwd=tmp_path,
                   capture_output=True, check=True)
    subprocess.run(["git", "config", "user.email", "e2e@test.com"],
                   cwd=tmp_path, capture_output=True)
    subprocess.run(["git", "config", "user.name", "E2E Test"],
                   cwd=tmp_path, capture_output=True)
    (tmp_path / "README.md").write_text("# E2E Test Project\n")
    subprocess.run(["git", "add", "."], cwd=tmp_path,
                   capture_output=True, check=True)
    subprocess.run(["git", "commit", "-m", "initial"],
                   cwd=tmp_path, capture_output=True, check=True)
    return tmp_path


@pytest.fixture
def writ_project(tmp_git_repo: Path, writ_bin: str) -> Path:
    """Git repo with writ init --yes completed."""
    result = subprocess.run(
        [writ_bin, "init", "--yes"],
        cwd=tmp_git_repo, capture_output=True, text=True,
    )
    assert result.returncode == 0, f"writ init failed: {result.stderr}"
    return tmp_git_repo


@pytest.fixture
def writ_project_with_spec(writ_project: Path, writ_bin: str) -> Path:
    """Writ project with a spec and some source files."""
    (writ_project / "src").mkdir(exist_ok=True)
    (writ_project / "src" / "app.py").write_text("# main app\n")
    (writ_project / "src" / "models.py").write_text("# data models\n")
    subprocess.run(
        [writ_bin, "spec", "add", "--id", "feat-1", "--title", "Feature 1"],
        cwd=writ_project, capture_output=True, check=True,
    )
    return writ_project


@pytest.fixture
def portfolio_project(writ_project: Path) -> Path:
    """Writ project with portfolio scaffold."""
    from scaffolds import scaffold_portfolio
    scaffold_portfolio(writ_project)
    return writ_project


# ---------------------------------------------------------------------------
# CLI options and markers
# ---------------------------------------------------------------------------

def pytest_addoption(parser):
    parser.addoption(
        "--live", action="store_true", default=False,
        help="Run live agent tests (requires claude CLI)",
    )
    parser.addoption(
        "--visual", action="store_true", default=False,
        help="Enable verbose output for tmux visual mode",
    )
    parser.addoption(
        "--results-dir", default=None,
        help="Directory for JSON results output",
    )


def pytest_configure(config):
    config.addinivalue_line("markers", "live: requires live claude agent (use --live)")
    config.addinivalue_line("markers", "slow: tests that take >30 seconds")


def pytest_collection_modifyitems(config, items):
    if not config.getoption("--live"):
        skip_live = pytest.mark.skip(reason="need --live flag to run")
        for item in items:
            if "live" in item.keywords:
                item.add_marker(skip_live)


# ---------------------------------------------------------------------------
# JSON Reporter
# ---------------------------------------------------------------------------

@dataclass
class E2ETestResult:
    name: str
    section: str
    outcome: str
    duration: float = 0.0
    message: str = ""


@dataclass
class E2EReport:
    run_id: str
    started_at: str
    finished_at: str = ""
    total: int = 0
    passed: int = 0
    failed: int = 0
    skipped: int = 0
    duration_seconds: float = 0.0
    sections: dict = field(default_factory=dict)
    results: List = field(default_factory=list)


_reporter_results: List[E2ETestResult] = []
_reporter_start: float = 0.0


def _section_from_nodeid(nodeid: str) -> str:
    """Extract section tag from test file name."""
    filename = nodeid.split("::")[0].split("/")[-1]
    for tag in ("s1", "s2", "s5", "s6"):
        if tag in filename:
            return tag.upper()
    if "live" in filename:
        return "LIVE"
    return "OTHER"


def pytest_sessionstart(session):
    global _reporter_start
    _reporter_start = time.time()


def pytest_runtest_logreport(report):
    if report.when != "call":
        return
    result = E2ETestResult(
        name=report.nodeid,
        section=_section_from_nodeid(report.nodeid),
        outcome=report.outcome,
        duration=round(report.duration, 3),
        message=str(report.longrepr)[:500] if report.failed else "",
    )
    _reporter_results.append(result)


def pytest_sessionfinish(session, exitstatus):
    results_dir_opt = session.config.getoption("--results-dir", default=None)
    results_dir = Path(results_dir_opt) if results_dir_opt else (
        Path(__file__).parent / "results"
    )
    results_dir.mkdir(exist_ok=True)

    # Build section summaries
    sections = {}
    for r in _reporter_results:
        if r.section not in sections:
            sections[r.section] = {"passed": 0, "failed": 0, "skipped": 0}
        if r.outcome == "passed":
            sections[r.section]["passed"] += 1
        elif r.outcome == "failed":
            sections[r.section]["failed"] += 1
        else:
            sections[r.section]["skipped"] += 1

    run_id = time.strftime("%Y%m%dT%H%M%S")
    report = E2EReport(
        run_id=run_id,
        started_at=time.strftime(
            "%Y-%m-%dT%H:%M:%S", time.localtime(_reporter_start),
        ),
        finished_at=time.strftime("%Y-%m-%dT%H:%M:%S"),
        total=len(_reporter_results),
        passed=sum(1 for r in _reporter_results if r.outcome == "passed"),
        failed=sum(1 for r in _reporter_results if r.outcome == "failed"),
        skipped=sum(1 for r in _reporter_results if r.outcome == "skipped"),
        duration_seconds=round(time.time() - _reporter_start, 2),
        sections=sections,
        results=[asdict(r) for r in _reporter_results],
    )

    output = asdict(report)
    (results_dir / "latest.json").write_text(json.dumps(output, indent=2))
    (results_dir / f"e2e_{run_id}.json").write_text(json.dumps(output, indent=2))
