#!/usr/bin/env python3
"""`writ context` cost benchmark and budget gate (B.1).

Builds the deterministic fixture with the writ binary under test, runs
``writ context`` across formats and scopes, and records output bytes and
wall time. Optionally measures a second, real repo (e.g. the writ repo).
Writes a JSON result plus a markdown table, then enforces the sprint budgets.

Exit codes: 0 = within budget (or --no-fail), 1 = budget exceeded,
2 = setup error (missing binary, fixture build failed).

Units: KB = 1,024 bytes (context budgets); binary MB = 1,000,000 bytes, which
matches how the sprint doc reports 11.4 MB for the 11,362,176-byte 0.2.0 binary.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import fixture  # noqa: E402  (sibling module, no package install needed)

KB = 1024
MB = 1_000_000

# name -> extra argv for `writ context`
CASES: list[tuple[str, list[str]]] = [
    ("default", []),
    ("json", ["--format", "json"]),
    ("json-compact", ["--format", "json-compact"]),
    ("toon", ["--format", "toon"]),
    ("human", ["--format", "human"]),
    ("brief", ["--format", "brief"]),
    ("spec", ["--spec", "s-alpha"]),
    ("for-agent", ["--for-agent", "agent-a"]),
    ("budget-8192", ["--budget", "8192"]),
    ("max-files-50", ["--max-files", "50"]),
    # Caps off: isolates ignore-rule effects (A.1) from list caps (A.2).
    ("uncapped", ["--max-files", "0"]),
]

# Budgets: case -> max bytes. Latency applies to every supported case.
BYTE_BUDGETS = {"default": 32 * KB, "brief": 2 * KB, "budget-8192": 8 * KB}
LATENCY_BUDGET_MS = 300
BINARY_BUDGET_BYTES = 13 * MB

# Path prefixes that must never appear in context.
FIXTURE_LEAK_PREFIXES = [name + "/" for name, _, _ in fixture.IGNORED_TREES]
# Gitignored dirs in the writ repo itself (informational for --repo runs).
REPO_LEAK_PREFIXES = [".venv311/", ".venv/", "target/", "docs/", "testing/", "scripts/",
                      "node_modules/"]


def count_leaks(text: str, prefixes: list[str]) -> dict[str, int]:
    """Count paths that START with each prefix (not substring hits like a/docs/)."""
    return {
        p: len(re.findall(r'(?:^|[\s"\',\[:])' + re.escape(p), text, re.MULTILINE))
        for p in prefixes
    }


def dir_size(path: Path) -> int:
    total = 0
    for root, _dirs, files in os.walk(path):
        for name in files:
            try:
                total += (Path(root) / name).lstat().st_size
            except OSError:
                pass
    return total


def run_case(
    writ_bin: str, repo: Path, argv: list[str], runs: int, leak_prefixes: list[str]
) -> dict:
    """Run one context invocation `runs` times (plus one warm-up)."""
    cmd = [writ_bin, "context", *argv]
    times_ms: list[float] = []
    proc = None
    for i in range(runs + 1):
        start = time.perf_counter()
        proc = subprocess.run(cmd, cwd=repo, capture_output=True)
        elapsed = (time.perf_counter() - start) * 1000
        if proc.returncode != 0:
            return {
                "argv": argv,
                "supported": False,
                "exit_code": proc.returncode,
                "error": proc.stderr.decode(errors="replace").strip().splitlines()[:1],
            }
        if i > 0:  # discard warm-up
            times_ms.append(elapsed)
    out = proc.stdout
    text = out.decode(errors="replace")
    return {
        "argv": argv,
        "supported": True,
        "bytes": len(out),
        "approx_tokens": len(out) // 4,
        "ms_median": round(statistics.median(times_ms), 1),
        "ms_min": round(min(times_ms), 1),
        "ms_max": round(max(times_ms), 1),
        "leaked_paths": count_leaks(text, leak_prefixes),
    }


def measure_repo(writ_bin: str, repo: Path, runs: int, cases, leak_prefixes) -> dict:
    return {
        "path": str(repo),
        "writ_dir_bytes": dir_size(repo / ".writ"),
        "cases": {
            name: run_case(writ_bin, repo, argv, runs, leak_prefixes) for name, argv in cases
        },
    }


def check_budgets(result: dict) -> list[str]:
    """Return a list of human-readable budget violations."""
    failures = []
    binary = result["binary"]["bytes"]
    if binary > BINARY_BUDGET_BYTES:
        failures.append(f"binary {binary:,} B > {BINARY_BUDGET_BYTES:,} B")
    cases = result["fixture"]["cases"]
    for name, limit in BYTE_BUDGETS.items():
        case = cases[name]
        if not case["supported"]:
            failures.append(f"{name}: unsupported by this binary (budget {limit:,} B)")
        elif case["bytes"] > limit:
            failures.append(f"{name}: {case['bytes']:,} B > {limit:,} B")
    for name, case in cases.items():
        if case["supported"] and case["ms_median"] > LATENCY_BUDGET_MS:
            failures.append(f"{name}: {case['ms_median']} ms > {LATENCY_BUDGET_MS} ms")
        leaked = sum(case.get("leaked_paths", {}).values()) if case["supported"] else 0
        if leaked:
            failures.append(f"{name}: {leaked} ignored paths leaked into context")
    return failures


def fmt_case_row(name: str, case: dict) -> str:
    limit = BYTE_BUDGETS.get(name)
    if not case["supported"]:
        err = (case.get("error") or [""])[0]
        return f"| {name} | n/a | n/a | n/a | {'≤ ' + str(limit) if limit else ''} | unsupported: `{err[:60]}` |"
    leaked = sum(case["leaked_paths"].values())
    verdict = []
    if limit and case["bytes"] > limit:
        verdict.append("OVER BYTES")
    if case["ms_median"] > LATENCY_BUDGET_MS:
        verdict.append("OVER LATENCY")
    if leaked:
        verdict.append(f"{leaked} leaked")
    return (
        f"| {name} | {case['bytes']:,} | {case['approx_tokens']:,} | {case['ms_median']} "
        f"| {'≤ ' + format(limit, ',') if limit else ''} | {', '.join(verdict) or 'ok'} |"
    )


def to_markdown(result: dict, failures: list[str]) -> str:
    lines = [
        f"# writ context benchmark: {result['binary']['version']}",
        "",
        *([f"**{result['label']}**", ""] if result.get("label") else []),
        f"- Captured: {result['captured_at']} on {result['host']}",
        f"- Binary: `{result['binary']['path']}` ({result['binary']['bytes']:,} B, "
        f"budget ≤ {BINARY_BUDGET_BYTES:,} B)",
        f"- Fixture: {fixture.TRACKED_FILES:,} tracked, {fixture.TRACKED_FILES:,} unsealed "
        f"modified, {sum(c for _, c, _ in fixture.IGNORED_TREES):,} ignored files, "
        f"3 specs, 6 seals, 2 agents (seed {fixture.SEED}); build {result['fixture']['build']['build_seconds']} s",
        f"- Fixture `.writ`: {result['fixture']['writ_dir_bytes']:,} B; "
        f"median of {result['runs']} runs after 1 warm-up",
        "",
        "## Fixture",
        "",
        "| case | bytes | ~tokens | ms (median) | budget B | verdict |",
        "|---|---:|---:|---:|---:|---|",
    ]
    lines += [fmt_case_row(n, c) for n, c in result["fixture"]["cases"].items()]
    leaks = result["fixture"]["cases"]["default"]
    if leaks["supported"]:
        lines += ["", "Leaked ignored-path occurrences (default format): "
                  + ", ".join(f"`{k}` {v}" for k, v in leaks["leaked_paths"].items())]
    for repo in result.get("repos", []):
        lines += ["", f"## Repo `{repo['path']}`", "",
                  f"`.writ`: {repo['writ_dir_bytes']:,} B", "",
                  "| case | bytes | ~tokens | ms (median) | budget B | verdict |",
                  "|---|---:|---:|---:|---:|---|"]
        lines += [fmt_case_row(n, c) for n, c in repo["cases"].items()]
    lines += ["", "## Budget gate (fixture only; repo rows are informational)", ""]
    lines += [f"- FAIL {f}" for f in failures] or ["- PASS: all budgets met"]
    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(
        description="writ context cost benchmark (B.1)",
        epilog=(
            f"Budgets: default <= {BYTE_BUDGETS['default']:,} B, brief <= {BYTE_BUDGETS['brief']:,} B, "
            f"--budget 8192 <= {BYTE_BUDGETS['budget-8192']:,} B, every case median "
            f"<= {LATENCY_BUDGET_MS} ms, binary <= {BINARY_BUDGET_BYTES:,} B, zero leaked "
            "ignored paths. Units: KB = 1,024 bytes for context output; binary MB = "
            "1,000,000 bytes (the sprint doc's 11.4 MB = 11,362,176 B). Exit 0 = pass "
            "(or --no-fail), 1 = budget exceeded, 2 = setup error."
        ),
    )
    p.add_argument("--writ", required=True, help="writ binary under test")
    p.add_argument("--out", type=Path, required=True, help="result JSON path")
    p.add_argument("--md", type=Path, help="markdown table path (default: --out with .md)")
    p.add_argument("--fixture-dir", type=Path, help="where to build the fixture (default: temp)")
    p.add_argument("--keep-fixture", action="store_true", help="do not delete the fixture")
    p.add_argument("--repo", type=Path, action="append", default=[],
                   help="also measure this existing writ repo (repeatable; read-only)")
    p.add_argument("--runs", type=int, default=7, help="timed runs per case (median reported)")
    p.add_argument("--no-fail", action="store_true", help="report budget failures, exit 0")
    p.add_argument("--label", default="", help="free-text label recorded in JSON and markdown")
    args = p.parse_args(argv)

    # Absolute: the fixture and repo runs use their own cwd.
    writ_bin = os.path.abspath(shutil.which(args.writ) or args.writ)
    if not Path(writ_bin).is_file():
        print(f"bench: writ binary not found: {args.writ}", file=sys.stderr)
        return 2
    version = subprocess.run([writ_bin, "--version"], capture_output=True, text=True).stdout.strip()

    tmp = None
    fixture_dir = args.fixture_dir
    if fixture_dir is None:
        tmp = tempfile.mkdtemp(prefix="writ-bench-")
        fixture_dir = Path(tmp) / "fixture"
    try:
        build = fixture.build(fixture_dir.resolve(), writ_bin, force=True)
        result = {
            "schema": 1,
            "label": args.label,
            "captured_at": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
            "host": f"{platform.system()} {platform.machine()}",
            "runs": args.runs,
            "binary": {
                "path": writ_bin,
                "resolved": str(Path(writ_bin).resolve()),
                "version": version,
                "bytes": Path(writ_bin).resolve().stat().st_size,
            },
            "budgets": {
                "bytes": BYTE_BUDGETS,
                "latency_ms": LATENCY_BUDGET_MS,
                "binary_bytes": BINARY_BUDGET_BYTES,
            },
            "fixture": {"build": build, **measure_repo(
                writ_bin, fixture_dir, args.runs, CASES, FIXTURE_LEAK_PREFIXES
            )},
            "repos": [],
        }
        repo_cases = [c for c in CASES if c[0] not in ("spec", "for-agent")]
        for repo in args.repo:
            result["repos"].append(measure_repo(
                writ_bin, repo.resolve(), args.runs, repo_cases, REPO_LEAK_PREFIXES
            ))
    except fixture.FixtureError as exc:
        print(f"bench: fixture build failed: {exc}", file=sys.stderr)
        return 2
    finally:
        if tmp and not args.keep_fixture:
            shutil.rmtree(tmp, ignore_errors=True)

    failures = check_budgets(result)
    result["budget_failures"] = failures
    result["passed"] = not failures
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(result, indent=2) + "\n")
    md_path = args.md or args.out.with_suffix(".md")
    md_path.write_text(to_markdown(result, failures))
    print(md_path.read_text())

    if failures and not args.no_fail:
        print(f"bench: {len(failures)} budget failure(s)", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
