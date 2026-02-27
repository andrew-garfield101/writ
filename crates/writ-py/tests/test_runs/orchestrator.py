"""Core test run orchestrator.

Drives a 5-phase test run:
  Phase 1: Setup     — scaffold workspace, writ init, specs
  Phase 2: Agents    — run each agent, restore-to-baseline between
  Phase 3: Converge  — converge_all with strategy from charter
  Phase 4: Validate  — run all checks, collect pass/fail
  Phase 5: Report    — write results, print summary
"""

import os
import tempfile
import time
from pathlib import Path

import yaml
import writ

from .report import TestRunReport, PhaseResult, AgentResult, CheckResult
from .agents import ScriptedAgentRunner, LiveAgentRunner
from .checks import run_checks
from .scaffolds import get_scaffold


class TestRunOrchestrator:
    """Execute a test run from a charter YAML."""

    def __init__(
        self,
        tr_dir: str,
        mode: str = "scripted",
        verbose: bool = False,
        trust_live_agents: bool = False,
    ):
        self.tr_dir = Path(tr_dir)
        self.mode = mode
        self.verbose = verbose

        charter_path = self.tr_dir / "charter.yaml"
        if not charter_path.exists():
            raise FileNotFoundError(f"Charter not found: {charter_path}")

        with open(charter_path) as f:
            self.charter = yaml.safe_load(f)

        self.workspace: Path | None = None
        self.repo = None
        self.convergence_report: dict = {}
        self.report = TestRunReport(
            tr_id=self.charter["tr"],
            mode=mode,
            charter_title=self.charter.get("title", f"TR{self.charter['tr']}"),
        )

        # Select agent runner
        if mode == "scripted":
            self.agent_runner = ScriptedAgentRunner(self.tr_dir)
        elif mode == "live":
            self.agent_runner = LiveAgentRunner(
                self.tr_dir, trust_agents=trust_live_agents
            )
        else:
            raise ValueError(f"Unknown mode: {mode}. Use 'scripted' or 'live'.")

    def run(self) -> TestRunReport:
        """Execute all phases and return the report."""
        self.report.started_at = time.time()

        with tempfile.TemporaryDirectory() as tmp:
            self.workspace = Path(tmp)

            self._run_phase("Setup", self._setup_workspace)
            self._run_phase("Agents", self._run_agents)
            self._run_phase("Convergence", self._run_convergence)
            self._run_phase("Validation", self._run_checks)

        self.report.finished_at = time.time()

        # Phase 5: Report
        self._generate_report()
        return self.report

    def _run_phase(self, name: str, fn) -> None:
        """Execute a phase with timing and error handling."""
        phase = PhaseResult(name=name, started_at=time.time())
        self._log(f"\n  PHASE: {name}")
        self._log(f"  {'=' * 50}")

        try:
            fn()
            phase.success = True
        except Exception as e:
            phase.success = False
            phase.error = str(e)
            self._log(f"  ERROR: {e}")

        phase.finished_at = time.time()
        self.report.phases.append(phase)
        self._log(f"  Phase {name}: {'ok' if phase.success else 'FAILED'} ({phase.duration_seconds}s)")

    def _setup_workspace(self) -> None:
        """Phase 1: Scaffold workspace, init writ, create specs."""
        domain = self.charter.get("domain", "taskapp")
        scaffold_fn = get_scaffold(domain)
        scaffold_fn(self.workspace)
        self._log(f"  Scaffolded '{domain}' in {self.workspace}")

        # Initialize writ
        self.repo = writ.Repository.init(str(self.workspace))
        self._log("  Initialized writ repository")

        # Create specs from charter
        for agent_def in self.charter.get("agents", []):
            spec_id = agent_def.get("spec", agent_def["name"])
            title = agent_def.get("title", spec_id)
            self.repo.add_spec(id=spec_id, title=title)

            file_scope = agent_def.get("file_scope")
            if file_scope:
                self.repo.update_spec(spec_id, file_scope=file_scope)

            self._log(f"  Created spec: {spec_id}")

        # Baseline seal
        self.repo.seal(
            summary="baseline: scaffold",
            agent_id="setup",
            agent_type="agent",
            status="in-progress",
        )
        self._log("  Baseline seal created")

    def _run_agents(self) -> None:
        """Phase 2: Run each agent, restore-to-baseline between."""
        agents = self.charter.get("agents", [])
        baseline_files = self._snapshot_workspace()

        for i, agent_def in enumerate(agents):
            agent_id = agent_def["name"]
            spec_id = agent_def.get("spec", agent_id)

            # Restore baseline before each agent (except the first)
            if i > 0:
                self._restore_baseline(baseline_files)
                self._log(f"  Restored baseline for {agent_id}")

            self._log(f"  Running agent: {agent_id} (spec: {spec_id})")
            start = time.time()

            try:
                result = self.agent_runner.run(
                    agent_def=agent_def,
                    workspace=self.workspace,
                    repo=self.repo,
                )
                result.duration_seconds = round(time.time() - start, 2)
                self.report.agents.append(result)
                self._log(
                    f"  Agent {agent_id}: {'ok' if result.success else 'FAILED'} "
                    f"({result.duration_seconds}s, {len(result.files_changed)} files)"
                )
            except Exception as e:
                result = AgentResult(
                    agent_id=agent_id,
                    spec_id=spec_id,
                    success=False,
                    error=str(e),
                    duration_seconds=round(time.time() - start, 2),
                )
                self.report.agents.append(result)
                self._log(f"  Agent {agent_id}: FAILED — {e}")

    def _run_convergence(self) -> None:
        """Phase 3: Run converge_all."""
        strategy = self.charter.get("convergence", {}).get("strategy", "escalate")
        self._log(f"  Running converge_all(strategy={strategy}, apply=True)")

        self.convergence_report = self.repo.converge_all(
            strategy=strategy,
            apply=True,
        )
        self.report.convergence_report = self.convergence_report

        is_clean = self.convergence_report.get("is_clean", False)
        total_conflicts = self.convergence_report.get("total_conflicts", 0)
        self._log(f"  Convergence: is_clean={is_clean}, total_conflicts={total_conflicts}")

    def _run_checks(self) -> None:
        """Phase 4: Run all validation checks."""
        check_defs = self.charter.get("checks", {})
        results = run_checks(
            check_defs=check_defs,
            convergence_report=self.convergence_report,
            repo=self.repo,
            workspace=self.workspace,
        )
        self.report.checks = results

        # Record issues from failed checks
        for check in results:
            if not check.passed and not check.skipped:
                self.report.issues_found.append({
                    "description": f"Check failed: {check.name}",
                    "category": check.category,
                    "details": check.details,
                    "severity": "major" if check.category in ("security", "convergence") else "minor",
                })

    def _generate_report(self) -> None:
        """Phase 5: Write results and print summary."""
        # Save results.yaml
        results_path = self.tr_dir / "results.yaml"
        self.report.save(results_path)
        self._log(f"\n  Results saved to: {results_path}")

        # Print summary
        self.report.print_summary()

    def _snapshot_workspace(self) -> dict[str, bytes]:
        """Capture all file contents for baseline restore."""
        snapshot = {}
        for root, _dirs, files in os.walk(self.workspace):
            # Skip .writ directory
            rel_root = os.path.relpath(root, self.workspace)
            if rel_root.startswith(".writ") or rel_root.startswith(".git"):
                continue
            for fname in files:
                full_path = os.path.join(root, fname)
                rel_path = os.path.relpath(full_path, self.workspace)
                try:
                    with open(full_path, "rb") as f:
                        snapshot[rel_path] = f.read()
                except (IOError, PermissionError):
                    pass
        return snapshot

    def _restore_baseline(self, baseline: dict[str, bytes]) -> None:
        """Restore workspace to baseline state."""
        # Remove non-baseline files (except .writ/ and .git/)
        for root, _dirs, files in os.walk(self.workspace):
            rel_root = os.path.relpath(root, self.workspace)
            if rel_root.startswith(".writ") or rel_root.startswith(".git"):
                continue
            for fname in files:
                full_path = os.path.join(root, fname)
                rel_path = os.path.relpath(full_path, self.workspace)
                if rel_path not in baseline:
                    os.remove(full_path)

        # Restore baseline files
        for rel_path, content in baseline.items():
            full_path = self.workspace / rel_path
            full_path.parent.mkdir(parents=True, exist_ok=True)
            with open(full_path, "wb") as f:
                f.write(content)

    def _log(self, msg: str) -> None:
        """Print message if verbose, otherwise just key phases."""
        if self.verbose or msg.strip().startswith(("PHASE", "Result", "=")):
            print(msg)
