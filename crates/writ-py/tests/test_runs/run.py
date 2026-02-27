"""CLI entry point for Layer 5 test runs.

Usage:
    python -m test_runs.run tr22 --mode scripted      # fast, deterministic
    python -m test_runs.run tr22 --mode live           # real Claude agents (sandboxed)
    python -m test_runs.run tr22 --mode scripted -v    # verbose output

Run from the tests/ directory:
    cd crates/writ-py/tests
    python -m test_runs.run tr22 --mode scripted
"""

import argparse
import sys
from pathlib import Path

from .orchestrator import TestRunOrchestrator


def main():
    parser = argparse.ArgumentParser(
        description="Layer 5: Live Agent Test Run Framework",
        usage="python -m test_runs.run <tr_id> [options]",
    )
    parser.add_argument(
        "tr_id",
        help="Test run identifier (e.g., tr22). Must match a directory under test_runs/.",
    )
    parser.add_argument(
        "--mode",
        choices=["scripted", "live"],
        default="scripted",
        help="Agent execution mode (default: scripted)",
    )
    parser.add_argument(
        "-v", "--verbose",
        action="store_true",
        help="Verbose output — show all phase details",
    )
    parser.add_argument(
        "--trust-live-agents",
        action="store_true",
        help=(
            "DANGEROUS: Skip all permission prompts for live agents. "
            "Only use in disposable/isolated environments. Without this flag, "
            "live agents run with normal Claude Code permissions — you'll see "
            "and approve each action."
        ),
    )

    args = parser.parse_args()

    # Resolve TR directory
    base_dir = Path(__file__).parent
    tr_dir = base_dir / args.tr_id
    if not tr_dir.exists():
        print(f"Error: TR directory not found: {tr_dir}", file=sys.stderr)
        print(f"Available TRs: {[d.name for d in base_dir.iterdir() if d.is_dir() and d.name.startswith('tr')]}")
        sys.exit(1)

    charter_path = tr_dir / "charter.yaml"
    if not charter_path.exists():
        print(f"Error: No charter.yaml in {tr_dir}", file=sys.stderr)
        sys.exit(1)

    # Safety warning for live mode
    if args.mode == "live":
        print(f"\n  Layer 5 Test Run: {args.tr_id} (mode: LIVE)")
        print(f"  {'=' * 50}")
        if args.trust_live_agents:
            print("  WARNING: --trust-live-agents is ON")
            print("  Agents will run with NO permission prompts.")
            print("  They are scoped to a temp workspace via --directory.")
        else:
            print("  Agents run with normal Claude Code permissions.")
            print("  You will see permission prompts in the terminal —")
            print("  approve or deny each action as it comes.")
            print("  Agents are scoped to a temp workspace via --directory.")
        print(f"  {'=' * 50}\n")
    else:
        print(f"\n  Layer 5 Test Run: {args.tr_id} (mode: {args.mode})")
        print(f"  {'=' * 50}\n")

    orchestrator = TestRunOrchestrator(
        tr_dir=str(tr_dir),
        mode=args.mode,
        verbose=args.verbose,
        trust_live_agents=args.trust_live_agents,
    )

    try:
        report = orchestrator.run()
        summary = report.summary
        sys.exit(0 if summary["failed"] == 0 else 1)
    except Exception as e:
        print(f"\n  FATAL ERROR: {e}", file=sys.stderr)
        import traceback
        traceback.print_exc()
        sys.exit(2)


if __name__ == "__main__":
    main()
