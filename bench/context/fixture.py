#!/usr/bin/env python3
"""Deterministic messy-repo fixture for the `writ context` cost benchmark (B.1, bench/context).

Builds a git + writ repo that reproduces the conditions behind the
"Context on a Budget" sprint:

* 1,000 tracked source files, all modified and left unsealed.
* 5,000 generated files in ignored dirs:
    - ``.venv/``         1,500  (exact name, in .gitignore at init)
    - ``node_modules/``  1,500  (exact name, in .gitignore at init)
    - ``.venv311/``      1,000  (only matched by the ``.venv*/`` glob)
    - ``build/``         1,000  (added to .gitignore AFTER ``writ init``)
* 3 specs and 6 seals across 2 agents.

The fixture is built with the writ binary under test, so before/after runs
each get a store in their own on-disk format. File contents are seeded, so
byte counts are stable across runs; seal ids and timestamps are not.

Standard library only (CI runs this without a venv).
"""

from __future__ import annotations

import argparse
import random
import shutil
import subprocess
import sys
import time
from pathlib import Path

SEED = 20261004
TRACKED_FILES = 1_000
MODULES = 20  # src/mod_00 .. src/mod_19, 50 files each

# Ignored trees: (dir, file_count, in_gitignore_at_init)
IGNORED_TREES: list[tuple[str, int, bool]] = [
    (".venv", 1_500, True),
    ("node_modules", 1_500, True),
    (".venv311", 1_000, True),  # covered only by the `.venv*/` glob
    ("build", 1_000, False),  # appended to .gitignore after init
]

GITIGNORE_AT_INIT = ".venv/\nnode_modules/\n.venv*/\n__pycache__/\n"
GITIGNORE_AFTER_INIT = "build/\n"

SPECS = [
    ("s-alpha", "Alpha: parser rework"),
    ("s-beta", "Beta: cache layer"),
    ("s-gamma", "Gamma: CLI polish"),
]
AGENTS = ["agent-a", "agent-b"]
# (agent, spec, module index) for each of the 6 seals
SEAL_PLAN = [
    ("agent-a", "s-alpha", 0),
    ("agent-a", "s-alpha", 1),
    ("agent-a", "s-beta", 2),
    ("agent-b", "s-beta", 3),
    ("agent-b", "s-gamma", 4),
    ("agent-b", "s-gamma", 5),
]
FILES_PER_SEAL = 20

WORDS = (
    "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi "
    "omicron pi rho sigma tau upsilon phi chi psi omega"
).split()

# Fixed identity for the throwaway fixture repo only (never the user's config).
GIT_IDENTITY = ["-c", "user.name=bench", "-c", "user.email=bench@localhost"]


class FixtureError(RuntimeError):
    """Raised when a git or writ command fails while building the fixture."""


def run(cmd: list[str], cwd: Path) -> str:
    """Run a command, raising FixtureError with stderr on failure."""
    proc = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)
    if proc.returncode != 0:
        raise FixtureError(
            f"command failed ({proc.returncode}): {' '.join(cmd)}\n{proc.stderr.strip()}"
        )
    return proc.stdout


def body(rng: random.Random, lines: int) -> str:
    """Generate deterministic pseudo-source text."""
    out = []
    for i in range(lines):
        words = " ".join(rng.choice(WORDS) for _ in range(rng.randint(3, 10)))
        out.append(f"# {i:03d} {words}")
    return "\n".join(out) + "\n"


def tracked_path(index: int) -> str:
    return f"src/mod_{index % MODULES:02d}/file_{index:04d}.py"


def write_tracked(root: Path, rng: random.Random) -> None:
    for i in range(TRACKED_FILES):
        path = root / tracked_path(i)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body(rng, rng.randint(10, 40)))


def write_ignored_tree(root: Path, name: str, count: int, rng: random.Random) -> None:
    for i in range(count):
        path = root / name / f"pkg_{i % 50:02d}" / f"sub_{i % 7}" / f"gen_{i:05d}.py"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body(rng, 5))


def append_line(root: Path, rel: str, text: str) -> None:
    with open(root / rel, "a", encoding="utf-8") as fh:
        fh.write(text + "\n")


def build(dest: Path, writ_bin: str, force: bool = False) -> dict:
    """Build the fixture at ``dest`` using ``writ_bin``. Returns build stats."""
    if dest.exists():
        if not force:
            raise FixtureError(f"{dest} exists; pass --force to rebuild")
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    rng = random.Random(SEED)
    started = time.perf_counter()

    # 1. Tracked tree + ignored trees, committed to git.
    run(["git", "init", "-q", "-b", "main"], dest)
    (dest / ".gitignore").write_text(GITIGNORE_AT_INIT)
    write_tracked(dest, rng)
    for name, count, _ in IGNORED_TREES:
        write_ignored_tree(dest, name, count, rng)
    run(["git", "add", "-A"], dest)
    run(["git", *GIT_IDENTITY, "commit", "-q", "-m", "fixture baseline"], dest)

    # 2. writ init (bare: no framework files, no hooks touching the user).
    run([writ_bin, "init", "-y", "--bare"], dest)

    # 3. Ignore rule added after init: only a live .gitignore read can see it.
    append_line(dest, ".gitignore", GITIGNORE_AFTER_INIT.strip())
    run(["git", "add", ".gitignore"], dest)
    run(["git", *GIT_IDENTITY, "commit", "-q", "-m", "ignore build/"], dest)

    # 4. Specs and seals.
    for spec_id, title in SPECS:
        run([writ_bin, "spec", "add", "--id", spec_id, "--title", title], dest)
    for n, (agent, spec_id, module) in enumerate(SEAL_PLAN):
        touched = [tracked_path(module + MODULES * k) for k in range(FILES_PER_SEAL)]
        for rel in touched:
            append_line(dest, rel, f"# seal {n} by {agent}")
        run(
            [writ_bin, "seal", "-s", f"seal {n}: {spec_id} work", "--agent", agent,
             "--spec", spec_id, "--paths", ",".join(touched)],
            dest,
        )

    # 5. 1,000 unsealed modifications: every tracked file.
    for i in range(TRACKED_FILES):
        append_line(dest, tracked_path(i), "# unsealed edit")

    return {
        "path": str(dest),
        "seed": SEED,
        "tracked_files": TRACKED_FILES,
        "unsealed_modified": TRACKED_FILES,
        "ignored_files": {name: count for name, count, _ in IGNORED_TREES},
        "gitignore_added_after_init": GITIGNORE_AFTER_INIT.split(),
        "specs": [s for s, _ in SPECS],
        "agents": AGENTS,
        "seals": len(SEAL_PLAN),
        "build_seconds": round(time.perf_counter() - started, 2),
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("dest", type=Path, help="directory to create the fixture in")
    parser.add_argument("--writ", default="writ", help="writ binary used to build it")
    parser.add_argument("--force", action="store_true", help="delete dest if it exists")
    args = parser.parse_args(argv)
    try:
        stats = build(args.dest.resolve(), args.writ, force=args.force)
    except FixtureError as exc:
        print(f"fixture: {exc}", file=sys.stderr)
        return 2
    print(f"fixture built at {stats['path']} in {stats['build_seconds']}s")
    return 0


if __name__ == "__main__":
    sys.exit(main())
