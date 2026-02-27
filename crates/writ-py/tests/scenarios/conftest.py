"""Pytest integration for YAML scenario auto-discovery.

Drop a .yaml file into any subdirectory under scenarios/ and it will
be automatically collected and executed as a pytest test item.

Usage:
    pytest tests/scenarios/ -v
    pytest tests/scenarios/convergence/ -v  # run just convergence scenarios
"""

import sys
from pathlib import Path

import pytest

# Ensure the scenarios directory is on the path for assertion imports
sys.path.insert(0, str(Path(__file__).parent))

from runner import ScenarioRunner


_SCENARIOS_DIR = Path(__file__).parent


def pytest_collect_file(parent, file_path):
    """Auto-discover .yaml scenario files under the scenarios/ directory."""
    if file_path.suffix == ".yaml" and _SCENARIOS_DIR in file_path.parents:
        return ScenarioFile.from_parent(parent, path=file_path)
    return None


class ScenarioFile(pytest.File):
    """A YAML scenario file that yields one test item."""

    def collect(self):
        yield ScenarioItem.from_parent(
            self,
            name=self.path.stem,
            callobj=self.path,
        )


class ScenarioItem(pytest.Item):
    """A single YAML scenario test."""

    def __init__(self, name, parent, callobj):
        super().__init__(name, parent)
        self.scenario_path = callobj

    def runtest(self):
        runner = ScenarioRunner(str(self.scenario_path))
        runner.run()

    def repr_failure(self, excinfo):
        """Provide clear failure output for scenario tests."""
        return (
            f"Scenario {self.name} failed:\n"
            f"  File: {self.scenario_path}\n"
            f"  {excinfo.getrepr(style='short')}"
        )

    def reportinfo(self):
        return self.path, 0, f"scenario: {self.name}"
