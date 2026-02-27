"""Structured reporting for test runs.

Produces JSON/YAML results with per-check pass/fail data,
phase timing, and a human-readable terminal summary.
"""

import json
import time
from dataclasses import dataclass, field, asdict
from pathlib import Path
from typing import Any


@dataclass
class CheckResult:
    """Result of a single validation check."""

    name: str
    category: str  # convergence, security, metadata, quality
    passed: bool
    details: str = ""
    skipped: bool = False


@dataclass
class PhaseResult:
    """Result of a single execution phase."""

    name: str
    started_at: float = 0.0
    finished_at: float = 0.0
    success: bool = True
    error: str = ""

    @property
    def duration_seconds(self) -> float:
        return round(self.finished_at - self.started_at, 2)


@dataclass
class AgentResult:
    """Result of an agent's execution."""

    agent_id: str
    spec_id: str
    success: bool = True
    seal_id: str = ""
    files_changed: list[str] = field(default_factory=list)
    error: str = ""
    duration_seconds: float = 0.0


@dataclass
class TestRunReport:
    """Complete test run report."""

    tr_id: int
    mode: str  # "scripted" or "live"
    charter_title: str = ""
    started_at: float = 0.0
    finished_at: float = 0.0
    phases: list[PhaseResult] = field(default_factory=list)
    agents: list[AgentResult] = field(default_factory=list)
    convergence_report: dict[str, Any] = field(default_factory=dict)
    checks: list[CheckResult] = field(default_factory=list)
    issues_found: list[dict[str, Any]] = field(default_factory=list)

    @property
    def duration_seconds(self) -> float:
        return round(self.finished_at - self.started_at, 2)

    @property
    def summary(self) -> dict[str, int]:
        total = len(self.checks)
        passed = sum(1 for c in self.checks if c.passed and not c.skipped)
        failed = sum(1 for c in self.checks if not c.passed and not c.skipped)
        skipped = sum(1 for c in self.checks if c.skipped)
        return {"total": total, "passed": passed, "failed": failed, "skipped": skipped}

    def to_dict(self) -> dict[str, Any]:
        """Convert to serializable dict."""
        d = {
            "tr_id": self.tr_id,
            "mode": self.mode,
            "charter_title": self.charter_title,
            "duration_seconds": self.duration_seconds,
            "summary": self.summary,
            "phases": [
                {
                    "name": p.name,
                    "duration_seconds": p.duration_seconds,
                    "success": p.success,
                    "error": p.error,
                }
                for p in self.phases
            ],
            "agents": [
                {
                    "agent_id": a.agent_id,
                    "spec_id": a.spec_id,
                    "success": a.success,
                    "seal_id": a.seal_id,
                    "files_changed": a.files_changed,
                    "duration_seconds": a.duration_seconds,
                    "error": a.error,
                }
                for a in self.agents
            ],
            "checks": [
                {
                    "name": c.name,
                    "category": c.category,
                    "passed": c.passed,
                    "skipped": c.skipped,
                    "details": c.details,
                }
                for c in self.checks
            ],
            "issues_found": self.issues_found,
        }
        return d

    def to_json(self) -> str:
        return json.dumps(self.to_dict(), indent=2)

    def to_yaml(self) -> str:
        """YAML output using simple formatting (no pyyaml dependency required)."""
        try:
            import yaml
            return yaml.dump(self.to_dict(), default_flow_style=False, sort_keys=False)
        except ImportError:
            return self.to_json()

    def save(self, path: Path) -> None:
        """Save results to a YAML file."""
        path.write_text(self.to_yaml())

    def print_summary(self) -> None:
        """Print a human-readable summary to stdout."""
        s = self.summary
        status = "PASSED" if s["failed"] == 0 else "FAILED"

        print()
        print(f"  {'=' * 60}")
        print(f"  TR{self.tr_id} — {self.charter_title}")
        print(f"  Mode: {self.mode} | Duration: {self.duration_seconds}s")
        print(f"  {'=' * 60}")
        print()

        # Phase timing
        print("  Phases:")
        for phase in self.phases:
            icon = "ok" if phase.success else "FAIL"
            print(f"    [{icon}] {phase.name} ({phase.duration_seconds}s)")
        print()

        # Agent results
        if self.agents:
            print("  Agents:")
            for agent in self.agents:
                icon = "ok" if agent.success else "FAIL"
                print(
                    f"    [{icon}] {agent.agent_id} -> {agent.spec_id} "
                    f"({agent.duration_seconds}s, {len(agent.files_changed)} files)"
                )
            print()

        # Check results by category
        categories = {}
        for check in self.checks:
            categories.setdefault(check.category, []).append(check)

        print("  Validation Checks:")
        for category, checks in categories.items():
            passed = sum(1 for c in checks if c.passed and not c.skipped)
            total = sum(1 for c in checks if not c.skipped)
            print(f"    {category}: {passed}/{total}")
            for check in checks:
                if check.skipped:
                    icon = "SKIP"
                elif check.passed:
                    icon = "PASS"
                else:
                    icon = "FAIL"
                line = f"      [{icon}] {check.name}"
                if not check.passed and check.details:
                    line += f" — {check.details}"
                print(line)
        print()

        # Issues
        if self.issues_found:
            print(f"  Issues Found: {len(self.issues_found)}")
            for issue in self.issues_found:
                print(f"    - [{issue.get('severity', '?')}] {issue.get('description', '?')}")
            print()

        # Final verdict
        print(f"  Result: {status} ({s['passed']} passed, {s['failed']} failed, {s['skipped']} skipped)")
        print(f"  {'=' * 60}")
        print()
