#!/usr/bin/env python3
"""Run `writ verify --all-chains --format json` and classify the result.

`writ verify` exits 1 on any failure (chain, missing objects, unreadable
trees), so callers must not treat a non-zero exit as "could not run". This
wrapper captures the exit code and the JSON and returns one verdict:

  PASS      all chains valid, nothing missing, nothing unreadable.
  EXPECTED  the only failures are missing objects whose hashes all match an
            --allow-missing prefix, and every allowance is used (finding 28:
            the writ repo's three permanently lost junk blobs).
  FAIL      anything else: a chain failure, an unreadable tree, a missing
            object not on the allow list, or an allowance that no longer
            matches (stale list).

Exit: 0 = PASS or EXPECTED, 1 = FAIL, 2 = verify could not run or emitted
no JSON.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path


def classify(report: dict, allow: tuple[str, ...] = ()) -> dict:
    """Classify a parsed `writ verify --all-chains --format json` report.

    Args:
        report: The parsed verify JSON.
        allow: Hash prefixes (>= 12 hex) of objects known to be lost.

    Returns:
        Dict with `verdict` and the evidence behind it.
    """
    problems: list[str] = []
    chains = [report.get("head_chain") or {}] + list(report.get("spec_chains") or [])
    for chain in chains:
        if chain and not chain.get("valid", True):
            name = chain.get("spec_id", "head")
            problems.append(f"chain {name}: {len(chain.get('failures') or [])} failure(s)")
    unreadable = report.get("unreadable_trees") or []
    if unreadable:
        problems.append(f"{len(unreadable)} unreadable tree(s)")
    missing = [m["hash"] for m in report.get("missing_objects") or []]
    excused = [h for h in missing if any(h.startswith(a) for a in allow)]
    unexcused = [h for h in missing if h not in excused]
    unused = [a for a in allow if not any(h.startswith(a) for h in excused)]
    if unexcused:
        problems.append(f"{len(unexcused)} missing object(s) not allow-listed: "
                        + ", ".join(h[:12] for h in unexcused))
    if unused:
        problems.append(f"allowance(s) no longer missing (update the list): {', '.join(unused)}")
    if problems:
        verdict = "FAIL"
    elif excused:
        verdict = "EXPECTED"
    else:
        verdict = "PASS"
    if verdict == "PASS" and report.get("all_valid") is False:
        verdict, problems = "FAIL", ["all_valid false with no failure this check recognizes"]
    return {
        "verdict": verdict,
        "all_valid": report.get("all_valid"),
        "missing_objects": len(missing),
        "excused": sorted(h[:12] for h in excused),
        "problems": problems,
    }


def run(repo: Path, writ: str, allow: tuple[str, ...]) -> dict:
    """Run verify in `repo`, capture its exit code, and classify the JSON."""
    proc = subprocess.run([writ, "verify", "--all-chains", "--format", "json"],
                          cwd=repo, capture_output=True, text=True, stdin=subprocess.DEVNULL)
    try:
        report = json.loads(proc.stdout)
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"verify rc {proc.returncode}, no JSON: "
                           f"{(proc.stderr or proc.stdout).strip()[:300]}") from exc
    result = classify(report, allow)
    result["verify_rc"] = proc.returncode
    return result


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("repo", type=Path, help="project root containing .writ/")
    ap.add_argument("--writ", default="writ", help="writ binary (default: writ on PATH)")
    ap.add_argument("--allow-missing", action="append", default=[], metavar="PREFIX",
                    help="hash prefix (>= 12 hex) of a known-lost object to excuse")
    args = ap.parse_args(argv)
    if any(len(a) < 12 for a in args.allow_missing):
        print("verify_check: --allow-missing prefixes must be at least 12 characters",
              file=sys.stderr)
        return 2
    try:
        result = run(args.repo, args.writ, tuple(args.allow_missing))
    except (OSError, RuntimeError) as exc:
        print(f"verify_check: {exc}", file=sys.stderr)
        return 2
    print(json.dumps(result, indent=2))
    return 1 if result["verdict"] == "FAIL" else 0


if __name__ == "__main__":
    sys.exit(main())
