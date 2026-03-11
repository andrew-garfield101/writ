"""CLI helpers — re-exports from testing/roundtrip plus E2E additions."""

import importlib.util
import subprocess
from pathlib import Path

# Load roundtrip helpers by file path to avoid name conflict with this package.
_REPO_ROOT = Path(__file__).resolve().parent.parent.parent.parent.parent.parent  # helpers/ → e2e/ → tests/ → writ-py/ → crates/ → repo root
_helpers_path = _REPO_ROOT / "testing" / "roundtrip" / "helpers.py"

_spec = importlib.util.spec_from_file_location("roundtrip_helpers", str(_helpers_path))
_roundtrip = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_roundtrip)

# Re-export everything from roundtrip helpers
AgentScorecard = _roundtrip.AgentScorecard
count_marker_occurrences = _roundtrip.count_marker_occurrences
file_between_markers = _roundtrip.file_between_markers
file_contains = _roundtrip.file_contains
git_diff_stat = _roundtrip.git_diff_stat
git_log = _roundtrip.git_log
writ_cmd = _roundtrip.writ_cmd
writ_context = _roundtrip.writ_context
writ_log = _roundtrip.writ_log
writ_spec_list = _roundtrip.writ_spec_list


# E2E-specific additions

def writ_status(
    writ_bin: str,
    cwd: Path,
    **kwargs,
) -> subprocess.CompletedProcess:
    """Get writ status (fleet overview)."""
    return writ_cmd(writ_bin, cwd, "status", check=False, **kwargs)


def writ_verify_chain(writ_bin: str, cwd: Path) -> dict:
    """Run writ verify --chain and return parsed JSON."""
    return writ_cmd(
        writ_bin, cwd, "verify", "--chain", "--format", "json",
        capture_json=True,
    )


def writ_finish_dry(
    writ_bin: str,
    cwd: Path,
) -> subprocess.CompletedProcess:
    """Run writ finish --yes --dry-run."""
    return writ_cmd(writ_bin, cwd, "finish", "--yes", "--dry-run", check=False)
