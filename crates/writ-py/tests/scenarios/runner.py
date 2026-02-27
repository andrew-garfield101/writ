"""Headless, deterministic scenario execution engine for writ.

Executes YAML-defined convergence scenarios:
  init → baseline → specs → agent changes → seal → converge → assert

Usage:
    runner = ScenarioRunner(scenario_path)
    runner.run()  # raises AssertionError on failure
"""

import os
import tempfile
import time
from pathlib import Path

import yaml
import writ

from assertions import convergence as conv_assert
from assertions import security as sec_assert
from assertions import metadata as meta_assert
from assertions import performance as perf_assert


REQUIRED_FIELDS = {"scenario", "setup", "agents", "convergence", "assertions"}
REQUIRED_AGENT_FIELDS = {"id", "changes"}
VALID_ACTIONS = {"write", "append", "delete"}


class ScenarioRunner:
    """Execute a YAML scenario file against a real writ repository."""

    def __init__(self, scenario_path: str):
        with open(scenario_path) as f:
            self.scenario = yaml.safe_load(f)
        self.scenario_path = scenario_path
        self.tmp_dir: str | None = None
        self.repo = None
        self.report: dict | None = None
        self.timings: dict = {}
        self._validate_schema()

    def _validate_schema(self):
        """Validate required fields in scenario YAML."""
        name = Path(self.scenario_path).stem
        missing = REQUIRED_FIELDS - set(self.scenario.keys())
        if missing:
            raise ValueError(
                f"Scenario '{name}' missing required fields: {missing}"
            )

        for i, agent in enumerate(self.scenario.get("agents", [])):
            agent_missing = REQUIRED_AGENT_FIELDS - set(agent.keys())
            if agent_missing:
                raise ValueError(
                    f"Scenario '{name}' agent #{i} missing fields: {agent_missing}"
                )
            for j, change in enumerate(agent.get("changes", [])):
                action = change.get("action", "write")
                if action not in VALID_ACTIONS:
                    raise ValueError(
                        f"Scenario '{name}' agent '{agent['id']}' change #{j}: "
                        f"unknown action '{action}' (valid: {VALID_ACTIONS})"
                    )
                if action in ("write", "append") and "content" not in change:
                    raise ValueError(
                        f"Scenario '{name}' agent '{agent['id']}' change #{j}: "
                        f"action '{action}' requires 'content' field"
                    )

    def run(self) -> dict:
        """Execute the full scenario and return the convergence report."""
        sec_assert.clear_cache()
        with tempfile.TemporaryDirectory() as tmp:
            self.tmp_dir = tmp
            self._create_baseline()
            self._init_repo()
            self._create_specs()
            self._execute_agents()
            self._run_convergence()
            self._check_assertions()
            return self.report

    def _create_baseline(self):
        """Write baseline files to temp directory."""
        setup = self.scenario.get("setup", {})
        for file_def in setup.get("baseline", []):
            path = os.path.join(self.tmp_dir, file_def["path"])
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as f:
                f.write(file_def["content"])

    def _init_repo(self):
        """Initialize writ repository and create baseline seal."""
        self.repo = writ.Repository.init(self.tmp_dir)

        # Seal baseline if there are baseline files
        setup = self.scenario.get("setup", {})
        if setup.get("baseline"):
            self.repo.seal(
                summary="baseline",
                agent_id="setup",
                agent_type="agent",
                status="in-progress",
            )

    def _create_specs(self):
        """Create specs with file scope declarations."""
        setup = self.scenario.get("setup", {})
        for spec_def in setup.get("specs", []):
            spec_id = spec_def["id"]
            title = spec_def.get("title", spec_id)
            description = spec_def.get("description", "")
            self.repo.add_spec(
                id=spec_id,
                title=title,
                description=description,
            )
            # Apply file_scope if specified
            file_scope = spec_def.get("file_scope")
            if file_scope:
                self.repo.update_spec(spec_id, file_scope=file_scope)

    def _execute_agents(self):
        """Apply each agent's predefined changes and seal.

        For proper divergence simulation, baseline files are restored
        before each agent's changes (so each agent starts from the
        same baseline, simulating parallel work).
        """
        t0 = time.monotonic()
        setup = self.scenario.get("setup", {})
        baseline_files = {
            f["path"]: f["content"] for f in setup.get("baseline", [])
        }

        for agent_def in self.scenario.get("agents", []):
            # Restore baseline files to simulate parallel work
            for rel_path, content in baseline_files.items():
                full_path = os.path.join(self.tmp_dir, rel_path)
                with open(full_path, "w") as f:
                    f.write(content)

            # Apply this agent's changes
            for change in agent_def.get("changes", []):
                path = os.path.join(self.tmp_dir, change["path"])
                action = change.get("action", "write")

                if action == "write":
                    os.makedirs(os.path.dirname(path), exist_ok=True)
                    with open(path, "w") as f:
                        f.write(change["content"])
                elif action == "append":
                    with open(path, "a") as f:
                        f.write(change["content"])
                elif action == "delete":
                    if os.path.exists(path):
                        os.remove(path)

            # Seal the agent's work
            spec_id = agent_def.get("spec")
            seal_result = self.repo.seal(
                summary=f"{agent_def['id']} work on {spec_id or 'unscoped'}",
                agent_id=agent_def["id"],
                agent_type="agent",
                spec_id=spec_id,
                status="in-progress",
                allow_empty=True,
            )
        self.timings["agents_seconds"] = round(time.monotonic() - t0, 4)

    def _run_convergence(self):
        """Run converge-all with configured strategy."""
        config = self.scenario.get("convergence", {})
        strategy = config.get("strategy", "escalate")
        apply = config.get("apply", True)
        t0 = time.monotonic()
        self.report = self.repo.converge_all(
            strategy=strategy,
            apply=apply,
        )
        self.timings["convergence_seconds"] = round(time.monotonic() - t0, 4)
        self.timings["total_seconds"] = round(
            self.timings.get("agents_seconds", 0)
            + self.timings["convergence_seconds"],
            4,
        )

    def _check_assertions(self):
        """Validate all assertions against the convergence result."""
        assertions = self.scenario.get("assertions", {})

        for assertion in assertions.get("convergence", []):
            conv_assert.check(
                assertion, self.report, self.repo, self.tmp_dir
            )

        for assertion in assertions.get("verification", []):
            conv_assert.check_verification(
                assertion, self.report, self.repo, self.tmp_dir
            )

        for assertion in assertions.get("security", []):
            sec_assert.check(assertion, self.report, self.repo)

        for assertion in assertions.get("metadata", []):
            meta_assert.check(assertion, self.repo)

        for assertion in assertions.get("performance", []):
            perf_assert.check(assertion, self.timings)
