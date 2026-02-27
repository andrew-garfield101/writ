"""Scale scenario generator for Layer 4 YAML tests.

Generates deterministic YAML scenarios with configurable agent counts,
file counts, and overlap ratios. Agents make predefined file changes
(add uniquely-named functions) — no LLM calls, just convergence engine
stress testing.

Usage:
    # From tests/ directory:
    python -m scenarios.generate_scale --agents 50 --files 20 --overlap 0.3
    python -m scenarios.generate_scale --agents 100 --files 10 --overlap 0.0 --output scenarios/scale/

    # Quick presets:
    python -m scenarios.generate_scale --preset medium    # 20 agents, 10 files, 0.2 overlap
    python -m scenarios.generate_scale --preset large     # 50 agents, 20 files, 0.3 overlap
    python -m scenarios.generate_scale --preset stress    # 100 agents, 10 files, 0.5 overlap
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import yaml


PRESETS = {
    "medium": {"agents": 20, "files": 10, "overlap": 0.2},
    "large": {"agents": 50, "files": 20, "overlap": 0.3},
    "stress": {"agents": 100, "files": 10, "overlap": 0.5},
}


def generate_scenario(
    num_agents: int,
    num_files: int,
    overlap: float,
    scenario_name: str | None = None,
) -> dict:
    """Generate a scale scenario definition.

    Args:
        num_agents: Number of agents to simulate.
        num_files: Number of Python files in the baseline.
        overlap: Fraction of files that multiple agents touch (0.0 = disjoint, 1.0 = all shared).
        scenario_name: Optional scenario name. Auto-generated if not provided.
    """
    if scenario_name is None:
        overlap_pct = int(overlap * 100)
        scenario_name = f"scale_{num_agents}a_{num_files}f_{overlap_pct}pct_overlap"

    # Calculate file assignments
    # Each agent gets a "primary" file plus some shared files based on overlap
    num_shared = max(1, int(num_files * overlap))
    num_private = num_files - num_shared

    # Generate baseline files
    baseline = []
    for i in range(num_files):
        baseline.append({
            "path": f"modules/module_{i}.py",
            "content": f"# Module {i}\n\ndef baseline_{i}():\n    return {i}\n",
        })

    # Generate specs
    specs = [{"id": f"spec-{i}"} for i in range(num_agents)]

    # Generate agents with file assignments
    agents = []
    for agent_idx in range(num_agents):
        changes = []

        # Primary file: each agent gets one (round-robin across private files)
        if num_private > 0:
            primary_file_idx = agent_idx % num_private
        else:
            primary_file_idx = agent_idx % num_files

        # Add a unique function to their primary file
        file_idx = primary_file_idx
        existing_content = f"# Module {file_idx}\n\ndef baseline_{file_idx}():\n    return {file_idx}\n"
        new_content = (
            existing_content
            + f"\n\ndef agent_{agent_idx}_feature():\n"
            f"    \"\"\"Added by agent-{agent_idx}.\"\"\"\n"
            f"    return \"agent-{agent_idx}-primary\"\n"
        )
        changes.append({
            "path": f"modules/module_{file_idx}.py",
            "action": "write",
            "content": new_content,
        })

        # Shared files: add to a shared module based on overlap
        if overlap > 0 and num_shared > 0:
            shared_file_idx = num_private + (agent_idx % num_shared)
            if shared_file_idx < num_files and shared_file_idx != file_idx:
                shared_existing = (
                    f"# Module {shared_file_idx}\n\n"
                    f"def baseline_{shared_file_idx}():\n"
                    f"    return {shared_file_idx}\n"
                )
                shared_new = (
                    shared_existing
                    + f"\n\ndef agent_{agent_idx}_shared_feature():\n"
                    f"    \"\"\"Shared file contribution by agent-{agent_idx}.\"\"\"\n"
                    f"    return \"agent-{agent_idx}-shared\"\n"
                )
                changes.append({
                    "path": f"modules/module_{shared_file_idx}.py",
                    "action": "write",
                    "content": shared_new,
                })

        agents.append({
            "id": f"agent-{agent_idx}",
            "spec": f"spec-{agent_idx}",
            "changes": changes,
        })

    # Generate assertions
    convergence_assertions = [
        {"type": "not_degraded"},
    ]

    # Verify a sample of agents' work survived (check every 5th agent, plus first and last)
    sample_indices = set([0, num_agents - 1])
    sample_indices.update(range(0, num_agents, max(1, num_agents // 10)))

    for idx in sorted(sample_indices):
        if num_private > 0:
            file_idx = idx % num_private
        else:
            file_idx = idx % num_files
        convergence_assertions.append({
            "type": "file_contains",
            "file": f"modules/module_{file_idx}.py",
            "content": f"agent_{idx}_feature",
        })

    # Determine if we expect escalations based on overlap
    if overlap == 0.0:
        convergence_assertions.insert(1, {"type": "no_escalations"})

    security_assertions = [
        {"type": "chain_valid"},
        {"type": "chain_no_failures"},
    ]

    scenario = {
        "scenario": scenario_name,
        "version": 1,
        "description": (
            f"Scale test: {num_agents} agents, {num_files} files, "
            f"{int(overlap * 100)}% overlap. "
            f"Generated by generate_scale.py. "
            f"Tests that convergence handles {num_agents}-agent sequential merging "
            f"without dropping any agent's work."
        ),
        "tags": ["scale", "generated", f"{num_agents}-agent"],
        "setup": {
            "specs": specs,
            "baseline": baseline,
        },
        "agents": agents,
        "convergence": {"strategy": "escalate"},
        "assertions": {
            "convergence": convergence_assertions,
            "security": security_assertions,
        },
    }

    return scenario


def write_scenario(scenario: dict, output_dir: Path) -> Path:
    """Write a scenario to a YAML file."""
    output_dir.mkdir(parents=True, exist_ok=True)
    filename = f"{scenario['scenario']}.yaml"
    output_path = output_dir / filename

    with open(output_path, "w") as f:
        yaml.dump(scenario, f, default_flow_style=False, sort_keys=False)

    return output_path


def main():
    parser = argparse.ArgumentParser(
        description="Generate scale YAML scenarios for convergence stress testing",
    )
    parser.add_argument("--agents", type=int, help="Number of agents")
    parser.add_argument("--files", type=int, default=10, help="Number of files (default: 10)")
    parser.add_argument("--overlap", type=float, default=0.0, help="File overlap ratio 0.0-1.0 (default: 0.0)")
    parser.add_argument("--preset", choices=list(PRESETS.keys()), help="Use a preset configuration")
    parser.add_argument("--output", type=str, help="Output directory (default: scenarios/scale/)")
    parser.add_argument("--name", type=str, help="Custom scenario name")
    parser.add_argument("--dry-run", action="store_true", help="Print scenario without writing")

    args = parser.parse_args()

    # Apply preset or manual args
    if args.preset:
        config = PRESETS[args.preset]
        num_agents = config["agents"]
        num_files = config["files"]
        overlap = config["overlap"]
    elif args.agents:
        num_agents = args.agents
        num_files = args.files
        overlap = args.overlap
    else:
        parser.error("Either --agents or --preset is required")
        return

    # Validate
    if num_agents < 2:
        parser.error("Need at least 2 agents")
    if num_files < 1:
        parser.error("Need at least 1 file")
    if not 0.0 <= overlap <= 1.0:
        parser.error("Overlap must be between 0.0 and 1.0")

    # Generate
    scenario = generate_scenario(
        num_agents=num_agents,
        num_files=num_files,
        overlap=overlap,
        scenario_name=args.name,
    )

    if args.dry_run:
        print(yaml.dump(scenario, default_flow_style=False, sort_keys=False))
        return

    # Write
    output_dir = Path(args.output) if args.output else Path(__file__).parent / "scale"
    path = write_scenario(scenario, output_dir)
    print(f"Generated: {path}")
    print(f"  Agents: {num_agents}, Files: {num_files}, Overlap: {int(overlap * 100)}%")
    print(f"  Assertions: {len(scenario['assertions']['convergence'])} convergence + {len(scenario['assertions']['security'])} security")


if __name__ == "__main__":
    main()
