#!/usr/bin/env python3
"""Seal cost on a long own history of one large file (finding 214, the 213 shape).

Builds a fresh project per N in a scratch directory and times one seal:

1. a git project with `big.rs` of LINES distinct lines, committed and sealed
   by spec `setup` (agent `human`), finished to git;
2. spec `own` (agent-a) seals the file N times; seal k rewrites EDITS lines
   in place and appends one line, all text new to the file;
3. spec `other` (agent-b) seals the file K times after that; each seal
   removes REMOVED lines of the original text and appends one line of its
   own, so each records informed removals;
4. agent-a removes DROP of the lines its first seal rewrote from the current file
   (which carries agent-b's lines), adds one new line at the top (the
   refactor shape: new unsealed text) and seals: the own line check finds a
   loss and asks every agent-b seal whether it saw the own seal that added
   each line it removed (`cross_spec_removals` -> `remover_saw`).

The timed seal is the last one, with the foreign line check on (the
production setting; setup seals run with `[seal] foreign_line_check = false`,
the test fixture setting). It is refused: the decision is recorded (exit
code and a sha256 of the refusal text with seal ids removed) so a before and
after binary can be compared on the same input.

Usage:
    seal_long_history.py --writ BIN --scratch DIR [--n 5,10,26,50] [--lines 38000]
                         [--k 3] [--removed 20] [--edits 20] [--drop 10]
                         [--timeout 600] [--out FILE] [--label NAME]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

FIXTURE_KEY = "\n[seal]\nforeign_line_check = false\n"


class Project:
    """One scratch project driven through the writ CLI."""

    def __init__(self, writ: Path, root: Path):
        self.writ = writ
        self.root = root
        self.env = {"HOME": os.environ["HOME"], "PATH": "/usr/bin:/bin:/usr/sbin:/sbin"}

    def run(self, *args: str, check: bool = True, timeout: float | None = None):
        p = subprocess.run([str(self.writ), *args], cwd=self.root, env=self.env,
                           capture_output=True, text=True, timeout=timeout)
        if check and p.returncode != 0:
            raise SystemExit(f"writ {' '.join(args)}: rc {p.returncode}\n{p.stdout}\n{p.stderr}")
        return p

    def git(self, *args: str) -> None:
        subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@localhost", *args],
                       cwd=self.root, check=True, capture_output=True)

    def fixture_key(self, on: bool) -> None:
        cfg = self.root / ".writ" / "config.toml"
        s = cfg.read_text().replace(FIXTURE_KEY, "")
        cfg.write_text(s.rstrip("\n") + "\n" + FIXTURE_KEY if on else s)

    def write(self, lines: list[str]) -> None:
        (self.root / "big.rs").write_text("".join(l + "\n" for l in lines))


def build(writ: Path, root: Path, a: argparse.Namespace, n: int) -> tuple[Project, list[str]]:
    """Steps 1 to 3 and the edit of step 4; returns the project, ready to seal."""
    if root.exists():
        shutil.rmtree(root)
    root.mkdir(parents=True)
    p = Project(writ, root)
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=root, check=True)
    lines = [f"    let value_{i} = compute({i}, \"base\");" for i in range(a.lines)]
    p.write(lines)
    p.git("add", "-A")
    p.git("commit", "-qm", "base")
    p.run("init", "-y")
    p.fixture_key(True)
    p.git("add", "-A")
    p.git("commit", "-qm", "init")
    p.run("spec", "add", "--id", "setup", "--title", "setup", "--agent", "human", "--claim")
    p.run("seal", "-s", "base", "--agent", "human", "--spec", "setup", "--paths", "big.rs")
    p.run("spec", "done", "setup", "--agent", "human", "--no-seal")
    p.run("finish", "-y", "--no-check")

    p.run("spec", "add", "--id", "own", "--title", "own", "--agent", "agent-a", "--claim")
    p.run("spec", "add", "--id", "other", "--title", "other", "--agent", "agent-b", "--claim")
    own_appended: list[str] = []
    stride = max(1, a.lines // (n * a.edits + 1))
    for k in range(n):
        for j in range(a.edits):
            i = (k * a.edits + j) * stride % a.lines
            lines[i] = f"    let value_{i} = compute({i}, \"own {k}\");"
        add = f"    own_step_{k}();"
        lines.append(add)
        own_appended.append(add)
        p.write(lines)
        p.run("seal", "-s", f"own {k}", "--agent", "agent-a", "--spec", "own", "--paths", "big.rs")
    removed = 0
    for k in range(a.k):
        # Remove original (base) lines from the back half, never an own line.
        victims = [i for i, l in enumerate(lines) if "\"base\"" in l][-(a.removed):]
        for i in reversed(victims):
            del lines[i]
            removed += 1
        lines.append(f"    other_step_{k}();")
        p.write(lines)
        p.run("seal", "-s", f"other {k}", "--agent", "agent-b", "--spec", "other",
              "--paths", "big.rs")
    # The first DROP lines own seal 0 rewrote: pure deletions of own lines.
    drop = {l for l in lines if l.endswith('"own 0");')}
    drop = set(sorted(drop)[: a.drop])
    lines = [l for l in lines if l not in drop]
    # New, unsealed text elsewhere in the file: the refactor shape. Without
    # it the removal is "editing its own work" and the check stops early.
    lines.insert(0, "    refactor_new();")
    p.write(lines)
    p.fixture_key(False)
    return p, own_appended


def decision(p: subprocess.CompletedProcess) -> str:
    """sha256 of the seal's output with ids and timings removed."""
    text = re.sub(r"\b[0-9a-f]{12,64}\b", "<id>", p.stdout + p.stderr)
    text = re.sub(r"\d+(\.\d+)? ?m?s\b", "<t>", text)
    return hashlib.sha256(text.encode()).hexdigest()[:16]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--writ", required=True, type=Path)
    ap.add_argument("--scratch", required=True, type=Path)
    ap.add_argument("--n", default="5,10,26,50")
    ap.add_argument("--lines", type=int, default=38000)
    ap.add_argument("--k", type=int, default=3, help="agent-b seals")
    ap.add_argument("--removed", type=int, default=20, help="lines each agent-b seal removes")
    ap.add_argument("--edits", type=int, default=20, help="lines each own seal rewrites")
    ap.add_argument("--drop", type=int, default=10, help="own lines the timed seal drops")
    ap.add_argument("--timeout", type=float, default=600)
    ap.add_argument("--out", type=Path)
    ap.add_argument("--label", default="run")
    ap.add_argument("--keep", action="store_true", help="keep each project")
    a = ap.parse_args()
    writ = a.writ.resolve()
    sha = hashlib.sha256(writ.read_bytes()).hexdigest()

    results = []
    for n in [int(x) for x in a.n.split(",")]:
        root = a.scratch.resolve() / f"n{n}"
        t0 = time.perf_counter()
        p, _ = build(writ, root, a, n)
        setup_s = time.perf_counter() - t0
        load = round(os.getloadavg()[0], 2)
        t0 = time.perf_counter()
        try:
            r = p.run("seal", "-s", "drop own lines", "--agent", "agent-a", "--spec", "own",
                      "--paths", "big.rs", check=False, timeout=a.timeout)
            ms, rc, dec, tail = (time.perf_counter() - t0) * 1000, r.returncode, decision(r), \
                (r.stdout + r.stderr).strip().splitlines()[:3]
        except subprocess.TimeoutExpired:
            ms, rc, dec, tail = a.timeout * 1000, None, "timeout", []
        row = {"n": n, "seal_ms": round(ms), "rc": rc, "refused": rc not in (0, None),
               "decision": dec, "first_lines": tail, "load_avg_1m": load,
               "setup_s": round(setup_s, 1)}
        results.append(row)
        print(json.dumps(row), flush=True)
        if not a.keep:
            shutil.rmtree(root, ignore_errors=True)

    rec = {"what": "timed seal of big.rs after N own seals (bench/seal_long_history.py)",
           "params": {k: getattr(a, k) for k in ("lines", "k", "removed", "edits", "drop")},
           "binary": {"path": str(writ), "sha256": sha},
           "date": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
           "rows": results}
    if a.out:
        doc = json.loads(a.out.read_text()) if a.out.exists() else {}
        doc[a.label] = rec
        a.out.parent.mkdir(parents=True, exist_ok=True)
        a.out.write_text(json.dumps(doc, indent=1) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
