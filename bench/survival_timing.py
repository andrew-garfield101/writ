#!/usr/bin/env python3
"""Full survival tier timing on a copy of a repository (finding 214).

Runs `writ doctor --survival --format json` RUNS times on REPO with the
survival cache removed before each run (a full run, not incremental), and
`writ context --format json` RUNS times, then writes the shape of
`evidence/0.4.5-signing-cost/f212_survival_context_amis_tree.json`:

    {"<label>": {"survival_full_elapsed_ms": [...], "survival_full_wall_ms": [...],
                 "survival_check_ms": [...], "context_ms": [...],
                 "survival_full_elapsed_ms_p50": n, "survival_full_wall_ms_p50": n,
                 "survival_check_ms_p50": n, "context_ms_p50": n},
     "load_avg_1m": [...], "repo": "...", "binary": {...}, ...}

`elapsed_ms` is doctor's own total, `wall_ms` the process wall time,
`survival_check_ms` doctor's `check_ms.survival`. Load average (1 min) is
read before every survival run. With --out naming an existing file, the
label is added to it (before and after in one file, as f212 did).

Never point REPO at a live repository: the run writes the survival cache.
Make the copy with `--copy-from`, which rsyncs a source repository into
REPO excluding target/, venvs and book/book (the doctor_timing.sh rule).

Usage:
    survival_timing.py --writ BIN --repo COPY [--copy-from SRC] [--runs 5]
                       [--label NAME] [--out FILE] [--no-context]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import statistics
import subprocess
import sys
import time
from pathlib import Path


def sha256(path: Path) -> str:
    """Hex sha256 of a file."""
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def run_json(cmd: list[str], cwd: Path) -> tuple[dict, float]:
    """Run a writ command that prints JSON; return (parsed, wall ms)."""
    t0 = time.perf_counter()
    p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)
    wall = (time.perf_counter() - t0) * 1000
    try:
        out = json.loads(p.stdout)
    except json.JSONDecodeError as e:
        raise SystemExit(
            f"{' '.join(cmd)}: rc {p.returncode}, not JSON ({e}); stderr: {p.stderr[-500:]}"
        ) from e
    return out, wall


def p50(values: list[float]) -> float:
    return statistics.median(values) if values else 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--writ", required=True, type=Path)
    ap.add_argument("--repo", required=True, type=Path, help="a COPY of the repository")
    ap.add_argument("--copy-from", type=Path, help="rsync this repository into --repo first")
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--label", default="run")
    ap.add_argument("--out", type=Path)
    ap.add_argument("--no-context", action="store_true")
    a = ap.parse_args()

    writ = a.writ.resolve()
    repo = a.repo.resolve()
    if a.copy_from:
        src = a.copy_from.resolve()
        if src == repo:
            raise SystemExit("--copy-from and --repo are the same directory")
        repo.mkdir(parents=True, exist_ok=True)
        subprocess.run(
            ["rsync", "-a", "--delete", "--exclude", "/target", "--exclude", "/.venv*",
             "--exclude", "/book/book", f"{src}/", f"{repo}/"],
            check=True,
        )
    if not (repo / ".writ").is_dir():
        raise SystemExit(f"{repo} has no .writ")
    env = {k: v for k, v in os.environ.items() if k != "WRIT_AGENT_ID"}

    elapsed, wall, check, ctx, load = [], [], [], [], []
    headline = None
    for _ in range(a.runs):
        (repo / ".writ" / "survival_cache.json").unlink(missing_ok=True)
        load.append(round(os.getloadavg()[0], 2))
        d, w = run_json([str(writ), "doctor", "--survival", "--format", "json"], repo)
        if "survival" not in d.get("checks_run", []):
            raise SystemExit(f"survival did not run: {d.get('headline')}")
        elapsed.append(int(d["elapsed_ms"]))
        wall.append(int(round(w)))
        check.append(int(d.get("check_ms", {}).get("survival", 0)))
        headline = d.get("survival") or d.get("headline")
    if not a.no_context:
        for _ in range(a.runs):
            _, w = run_json([str(writ), "context", "--format", "json"], repo)
            ctx.append(round(w, 1))

    rec = {
        "survival_full_elapsed_ms": elapsed,
        "survival_full_wall_ms": wall,
        "survival_check_ms": check,
        "context_ms": ctx,
        "survival_full_elapsed_ms_p50": p50(elapsed),
        "survival_full_wall_ms_p50": p50(wall),
        "survival_check_ms_p50": p50(check),
        "context_ms_p50": p50(ctx),
        "load_avg_1m": load,
        "survival_result": headline,
        "binary": {"path": str(writ), "sha256": sha256(writ),
                   "version": subprocess.run([str(writ), "--version"], capture_output=True,
                                             text=True).stdout.strip()},
        "date": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }
    doc: dict = {}
    if a.out and a.out.exists():
        doc = json.loads(a.out.read_text())
    doc[a.label] = rec
    doc.setdefault("load_avg_1m", []).extend(load)
    doc["repo"] = f"rsync copy at {repo}" + (f" of {a.copy_from}" if a.copy_from else "")
    doc["seals_in_store"] = len(list((repo / ".writ" / "seals").iterdir()))
    doc["machine"] = {"platform": platform.platform(), "machine": platform.machine(),
                      "cpus": os.cpu_count()}
    doc["what"] = ("full survival tier (survival cache removed each run) and context, "
                   "bench/survival_timing.py")
    text = json.dumps(doc, indent=1)
    if a.out:
        a.out.parent.mkdir(parents=True, exist_ok=True)
        a.out.write_text(text + "\n")
    print(f"{a.label}: survival full elapsed {elapsed} p50 {p50(elapsed)} ms; "
          f"wall p50 {p50(wall)} ms; survival check p50 {p50(check)} ms; "
          f"context p50 {p50(ctx)} ms; load {load}; {headline}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
