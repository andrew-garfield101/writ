"""CLI helpers for E2E tests — self-contained, no external dependencies.

All writ CLI wrappers and validation helpers needed by the E2E test suite.
"""

import json
import subprocess
from pathlib import Path
from typing import Callable, Dict, List, Optional, Union


# ---------------------------------------------------------------------------
# Core CLI wrappers
# ---------------------------------------------------------------------------

def writ_cmd(
    writ_bin: str,
    cwd: Path,
    *args: str,
    check: bool = True,
    capture_json: bool = False,
) -> Union[subprocess.CompletedProcess, dict]:
    """Run a writ CLI command and return the result.

    Args:
        writ_bin: Path to writ binary.
        cwd: Working directory.
        *args: Command arguments (e.g., "seal", "-s", "msg").
        check: Raise on non-zero exit code.
        capture_json: Parse stdout as JSON and return dict.

    Returns:
        CompletedProcess or parsed JSON dict.
    """
    result = subprocess.run(
        [writ_bin, *args],
        cwd=cwd,
        capture_output=True,
        text=True,
    )
    if check and result.returncode != 0:
        raise subprocess.CalledProcessError(
            result.returncode,
            [writ_bin, *args],
            result.stdout,
            result.stderr,
        )
    if capture_json:
        try:
            return json.loads(result.stdout)
        except json.JSONDecodeError:
            raise ValueError(
                f"Expected JSON from `writ {' '.join(args)}`, "
                f"got: {result.stdout[:200]}"
            )
    return result


def writ_context(writ_bin: str, cwd: Path, **kwargs) -> dict:
    """Get writ context as parsed dict."""
    return writ_cmd(
        writ_bin, cwd, "context", "--format", "json",
        capture_json=True, **kwargs,
    )


def writ_log(writ_bin: str, cwd: Path, **kwargs) -> List:
    """Get writ log as parsed list."""
    return writ_cmd(
        writ_bin, cwd, "log", "--format", "json",
        capture_json=True, **kwargs,
    )


def writ_spec_list(writ_bin: str, cwd: Path, **kwargs) -> List:
    """Get spec list as parsed list."""
    return writ_cmd(
        writ_bin, cwd, "spec", "status", "--format", "json",
        capture_json=True, **kwargs,
    )


def git_log(cwd: Path, limit: int = 10) -> List[Dict]:
    """Get git log as list of dicts with hash and message."""
    result = subprocess.run(
        ["git", "log", f"--max-count={limit}",
         "--format=%H|||%s|||%b---END---"],
        cwd=cwd,
        capture_output=True,
        text=True,
    )
    commits = []
    for entry in result.stdout.strip().split("---END---"):
        entry = entry.strip()
        if not entry:
            continue
        parts = entry.split("|||")
        if len(parts) >= 2:
            commits.append({
                "hash": parts[0].strip(),
                "subject": parts[1].strip(),
                "body": parts[2].strip() if len(parts) > 2 else "",
            })
    return commits


def git_diff_stat(cwd: Path, ref: str = "HEAD~1") -> str:
    """Get git diff --stat output."""
    result = subprocess.run(
        ["git", "diff", "--stat", ref],
        cwd=cwd,
        capture_output=True,
        text=True,
    )
    return result.stdout


# ---------------------------------------------------------------------------
# File utilities
# ---------------------------------------------------------------------------

def file_contains(path: Path, text: str) -> bool:
    """Check if a file contains given text."""
    if not path.exists():
        return False
    return text in path.read_text()


def file_between_markers(
    path: Path,
    begin: str = "<!-- BEGIN WRIT CONFIGURATION",
    end_marker: str = "<!-- END WRIT CONFIGURATION -->",
) -> Optional[str]:
    """Extract content between writ markers in a file."""
    if not path.exists():
        return None
    content = path.read_text()
    start_idx = content.find(begin)
    end_idx = content.find(end_marker)
    if start_idx == -1 or end_idx == -1:
        return None
    return content[start_idx:end_idx + len(end_marker)]


def count_marker_occurrences(
    path: Path,
    marker: str = "<!-- BEGIN WRIT CONFIGURATION",
) -> int:
    """Count how many times a marker appears in a file."""
    if not path.exists():
        return 0
    return path.read_text().count(marker)


# ---------------------------------------------------------------------------
# E2E-specific helpers
# ---------------------------------------------------------------------------

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


# ---------------------------------------------------------------------------
# Agent adoption scorecard
# ---------------------------------------------------------------------------

class AgentScorecard:
    """Score an agent's writ adoption from post-run state.

    Scoring:
        - Ran writ context at start:     20 pts (inferred from seal existence)
        - Created or claimed a spec:     20 pts
        - Created at least 1 seal:       20 pts
        - Used writ spec done:           15 pts
        - Did NOT run git commit:        10 pts
        - Completed assigned task:       15 pts
    """

    def __init__(self, agent_id: str):
        self.agent_id = agent_id
        self.scores: Dict[str, int] = {}
        self.notes: Dict[str, str] = {}
        self.max_scores = {
            "context_used": 20,
            "spec_created": 20,
            "seal_created": 20,
            "spec_done": 15,
            "no_git_commit": 10,
            "task_completed": 15,
        }

    def evaluate(
        self,
        writ_bin: str,
        cwd: Path,
        task_validation_fn: Optional[Callable] = None,
        git_commits_before: int = 0,
    ) -> dict:
        """Run all scoring checks against current writ state.

        Args:
            writ_bin: Path to writ binary.
            cwd: Project directory.
            task_validation_fn: Optional callable(cwd) -> bool for task check.
            git_commits_before: Number of git commits before agent ran.

        Returns:
            Dict with total score, breakdown, and pass/fail.
        """
        log = writ_log(writ_bin, cwd)
        specs = writ_spec_list(writ_bin, cwd)

        # Check seals by this agent
        agent_seals = []
        for s in log:
            agent_field = s.get("agent", {})
            if isinstance(agent_field, dict):
                if agent_field.get("id") == self.agent_id:
                    agent_seals.append(s)
            elif agent_field == self.agent_id:
                agent_seals.append(s)
            elif s.get("agent_id") == self.agent_id:
                agent_seals.append(s)

        # Check specs by this agent
        agent_specs = [
            s for s in specs
            if s.get("created_by") == self.agent_id
        ]

        # Context used — inferred from agent having seals
        if agent_seals:
            self.scores["context_used"] = 20
            self.notes["context_used"] = f"Agent has {len(agent_seals)} seal(s)"
        else:
            self.scores["context_used"] = 0
            self.notes["context_used"] = "No seals found"

        # Spec created
        if agent_specs:
            self.scores["spec_created"] = 20
            self.notes["spec_created"] = f"Created {len(agent_specs)} spec(s)"
        else:
            specs_used = set()
            for s in agent_seals:
                sid = s.get("spec_id") or s.get("spec")
                if sid:
                    specs_used.add(sid)
            if specs_used:
                self.scores["spec_created"] = 20
                self.notes["spec_created"] = f"Used spec(s): {specs_used}"
            else:
                self.scores["spec_created"] = 0
                self.notes["spec_created"] = "No specs created or used"

        # Seal created
        if agent_seals:
            self.scores["seal_created"] = 20
            self.notes["seal_created"] = f"{len(agent_seals)} seal(s)"
        else:
            self.scores["seal_created"] = 0
            self.notes["seal_created"] = "No seals"

        # Spec done
        completed_by_agent = [
            s for s in agent_seals
            if s.get("status") == "complete"
        ]
        completed_specs = [
            sp for sp in specs
            if sp.get("status") in ("complete", "completed")
            and sp.get("created_by") == self.agent_id
        ]
        if completed_by_agent or completed_specs:
            self.scores["spec_done"] = 15
            self.notes["spec_done"] = "Spec marked complete"
        else:
            self.scores["spec_done"] = 0
            self.notes["spec_done"] = "No spec marked complete"

        # No git commit
        current_commits = git_log(cwd)
        if len(current_commits) <= git_commits_before:
            self.scores["no_git_commit"] = 10
            self.notes["no_git_commit"] = "No unauthorized git commits"
        else:
            self.scores["no_git_commit"] = 0
            self.notes["no_git_commit"] = (
                f"Agent made {len(current_commits) - git_commits_before} "
                "git commit(s)"
            )

        # Task completed
        if task_validation_fn:
            try:
                if task_validation_fn(cwd):
                    self.scores["task_completed"] = 15
                    self.notes["task_completed"] = "Task validation passed"
                else:
                    self.scores["task_completed"] = 0
                    self.notes["task_completed"] = "Task validation failed"
            except Exception as e:
                self.scores["task_completed"] = 0
                self.notes["task_completed"] = f"Validation error: {e}"
        else:
            self.scores["task_completed"] = 15
            self.notes["task_completed"] = "No validation fn — assumed pass"

        total = sum(self.scores.values())
        max_total = sum(self.max_scores.values())

        return {
            "agent_id": self.agent_id,
            "total": total,
            "max": max_total,
            "percentage": round(total / max_total * 100),
            "pass": total >= 70,
            "target": total >= 90,
            "breakdown": self.scores,
            "notes": self.notes,
        }
