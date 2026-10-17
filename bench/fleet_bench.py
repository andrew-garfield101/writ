#!/usr/bin/env python3
"""Fleet bench: N identities, N specs, K seals each on an F-file repo.

    fleet_bench.py --writ BIN [--agents 50] [--seals-per-agent 20]
                   [--files 1000] [--commits 20] [--sequential]
                   [--json OUT] [--keep]

Builds a scratch git repo with F files and a history of --commits commits,
runs `writ init -y` with BIN, gives each identity (WRIT_AGENT_ID=fleet-NN)
its own spec, then has every identity seal one of its own files K times
with `writ seal --paths <file>` (all identities at once unless
--sequential). Every seal is the plain CLI seal an agent or a hook runs;
no hook is involved, so the number is writ's own seal path under
contention. After the seals it times `writ context`, `writ doctor` (fast
tier), `writ doctor --survival` cold (cache removed: full) and warm after
one more seal (incremental), and `writ handoff`, then checks the store
with bench/store_integrity.py.

Prints every number with the binary (version, sha256 prefix) that
produced it, and writes the same as JSON with --json.
"""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
INTEGRITY = ROOT / "bench" / "store_integrity.py"


def provenance(writ: str) -> dict:
    p = Path(writ).resolve()
    ver = subprocess.run([str(p), "--version"], capture_output=True, text=True).stdout.strip()
    return {"path": str(p), "version": ver,
            "sha256": hashlib.sha256(p.read_bytes()).hexdigest()}


def env_for(writ: str, home: Path, agent: str | None = None) -> dict:
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("WRIT_", "CLAUDE_CODE_")) and k != "CLAUDECODE"}
    bindir = home / "bin"
    bindir.mkdir(parents=True, exist_ok=True)
    link = bindir / "writ"
    if not link.exists():
        link.symlink_to(Path(writ).resolve())
    env["PATH"] = f"{bindir}:/usr/bin:/bin:/usr/sbin:/sbin"
    env["HOME"] = str(home)
    env.update(GIT_AUTHOR_NAME="fleet", GIT_AUTHOR_EMAIL="fleet@example.invalid",
               GIT_COMMITTER_NAME="fleet", GIT_COMMITTER_EMAIL="fleet@example.invalid",
               GIT_CONFIG_NOSYSTEM="1")
    if agent:
        env["WRIT_AGENT_ID"] = agent
    return env


def run(args: list, cwd: Path, env: dict, check: bool = True) -> subprocess.CompletedProcess:
    r = subprocess.run(args, cwd=cwd, env=env, capture_output=True, text=True,
                       stdin=subprocess.DEVNULL)
    if check and r.returncode != 0:
        sys.exit(f"{' '.join(map(str, args))} failed ({r.returncode}):\n{r.stdout}{r.stderr}")
    return r


def timed(args: list, cwd: Path, env: dict) -> tuple[float, subprocess.CompletedProcess]:
    t = time.perf_counter()
    r = run(args, cwd, env, check=False)
    return (time.perf_counter() - t) * 1000.0, r


def file_for(agent: int, j: int, files: int, agents: int) -> str:
    """Each identity owns the files i with i % agents == agent."""
    owned = list(range(agent, files, agents)) or [agent]
    return f"src/m{owned[j % len(owned)]:04d}.txt"


def build_repo(repo: Path, files: int, commits: int, env: dict) -> int:
    run(["git", "init", "-q", "-b", "main"], repo, env)
    (repo / "README.md").write_text("# fleet bench\n")
    (repo / ".gitignore").write_text("target/\n")
    src = repo / "src"
    src.mkdir()
    for i in range(files):
        (src / f"m{i:04d}.txt").write_text("".join(f"line {k} of file {i}\n" for k in range(30)))
    run(["git", "add", "-A"], repo, env)
    run(["git", "commit", "-q", "-m", "initial"], repo, env)
    made = 1
    for c in range(1, commits):
        for i in range(c, files, max(1, files // 20)):
            with open(src / f"m{i:04d}.txt", "a") as f:
                f.write(f"history {c}\n")
        run(["git", "commit", "-q", "-am", f"history {c}"], repo, env)
        made += 1
    return made


def pct(xs: list[float], q: float) -> float:
    if not xs:
        return 0.0
    s = sorted(xs)
    return round(s[min(len(s) - 1, int(round(q * (len(s) - 1))))], 1)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--writ", required=True)
    ap.add_argument("--agents", type=int, default=50)
    ap.add_argument("--seals-per-agent", type=int, default=20)
    ap.add_argument("--files", type=int, default=1000)
    ap.add_argument("--commits", type=int, default=20)
    ap.add_argument("--sequential", action="store_true")
    ap.add_argument("--json", type=Path)
    ap.add_argument("--keep", action="store_true")
    a = ap.parse_args()

    prov = provenance(a.writ)
    tag = f"[{prov['version']} {prov['sha256'][:12]} {prov['path']}]"
    work = Path(tempfile.mkdtemp(prefix="writ-fleet-"))
    repo, home = work / "repo", work / "home"
    repo.mkdir()
    home.mkdir()
    env = env_for(a.writ, home)
    commits = build_repo(repo, a.files, a.commits, env)
    t, r = timed(["writ", "init", "-y"], repo, env)
    if r.returncode != 0:
        sys.exit(f"writ init failed:\n{r.stdout}{r.stderr}")
    init_ms = round(t, 1)
    run(["git", "add", "-A"], repo, env)
    run(["git", "commit", "-q", "--no-verify", "-m", "writ init"], repo, env)
    commits += 1

    agents = [f"fleet-{i:02d}" for i in range(a.agents)]
    for i, ag in enumerate(agents):
        e = env_for(a.writ, home, ag)
        run(["writ", "spec", "add", "--id", f"spec-{i:02d}", "--title", f"fleet work {i}"], repo, e)
        run(["writ", "spec", "claim", f"spec-{i:02d}", "--agent", ag], repo, e)

    seal_ms: list[float] = []
    failures: list[str] = []
    lock = threading.Lock()

    def worker(i: int) -> None:
        ag = agents[i]
        e = env_for(a.writ, home, ag)
        for j in range(a.seals_per_agent):
            rel = file_for(i, j, a.files, a.agents)
            with open(repo / rel, "a") as f:
                f.write(f"{ag} edit {j}\n")
            ms, r = timed(["writ", "seal", "-s", f"{ag} edit {j}", "--agent", ag,
                           "--spec", f"spec-{i:02d}", "--paths", rel], repo, e)
            with lock:
                seal_ms.append(ms)
                if r.returncode != 0:
                    failures.append(f"{ag} {rel}: {(r.stderr or r.stdout).strip()[:200]}")

    started = time.perf_counter()
    if a.sequential:
        for i in range(a.agents):
            worker(i)
    else:
        threads = [threading.Thread(target=worker, args=(i,)) for i in range(a.agents)]
        for th in threads:
            th.start()
        for th in threads:
            th.join()
    seal_wall_s = round(time.perf_counter() - started, 2)

    obs = env_for(a.writ, home, "fleet-observer")
    context_ms, _ = timed(["writ", "context", "--format", "brief"], repo, obs)
    doctor_ms, _ = timed(["writ", "doctor"], repo, obs)
    (repo / ".writ" / "survival_cache.json").unlink(missing_ok=True)
    survival_full_ms, rs = timed(["writ", "doctor", "--survival"], repo, obs)
    e0 = env_for(a.writ, home, agents[0])
    rel = file_for(0, 0, a.files, a.agents)
    with open(repo / rel, "a") as f:
        f.write("one more\n")
    run(["writ", "seal", "-s", "one more", "--agent", agents[0], "--spec", "spec-00",
         "--paths", rel], repo, e0, check=False)
    survival_inc_ms, ri = timed(["writ", "doctor", "--survival"], repo, obs)
    handoff_ms, _ = timed(["writ", "handoff"], repo, obs)

    seals = specs = 0
    log = run(["writ", "log", "--all", "--format", "json", "--limit", "100000"], repo, obs,
              check=False)
    try:
        entries = json.loads(log.stdout)
        mine = [s for s in entries if str(s.get("agent", {}).get("id", s.get("agent", ""))).startswith("fleet-")
                or str(s.get("agent")).startswith("fleet-")]
        seals = len(mine)
        specs = len({s.get("spec_id") for s in mine if s.get("spec_id")})
    except (json.JSONDecodeError, AttributeError):
        pass
    integ = subprocess.run([sys.executable, str(INTEGRITY), str(repo)],
                           capture_output=True, text=True)
    tracked = len(run(["git", "ls-files"], repo, env).stdout.split())

    out = {
        "binary": prov,
        "date": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
        "repo": {"files": tracked, "git_commits": commits},
        "agents": a.agents,
        "specs": specs,
        "seals_per_agent": a.seals_per_agent,
        "seals": seals,
        "concurrent": not a.sequential,
        "seal_failures": len(failures),
        "seal_failure_samples": failures[:5],
        "seal_wall_s": seal_wall_s,
        "seal_ms": {"p50": pct(seal_ms, 0.5), "p90": pct(seal_ms, 0.9),
                    "max": round(max(seal_ms), 1) if seal_ms else 0.0,
                    "mean": round(statistics.mean(seal_ms), 1) if seal_ms else 0.0},
        "init_ms": init_ms,
        "context_ms": round(context_ms, 1),
        "doctor_fast_ms": round(doctor_ms, 1),
        "survival_full_ms": round(survival_full_ms, 1),
        "survival_incremental_ms": round(survival_inc_ms, 1),
        "survival_headline": (rs.stdout.splitlines() or [""])[0],
        "handoff_ms": round(handoff_ms, 1),
        "store_ok": integ.returncode == 0,
        "integrity": (integ.stdout.strip().splitlines() or [""])[-1],
        "scratch": str(work) if a.keep else None,
    }
    print(f"{tag} repo {tracked} files, {commits} git commits; {a.agents} identities x "
          f"{a.seals_per_agent} seals ({'sequential' if a.sequential else 'concurrent'})")
    print(f"{tag} seal_ms p50 {out['seal_ms']['p50']} p90 {out['seal_ms']['p90']} "
          f"max {out['seal_ms']['max']}; seals {seals}, specs {specs}, failures "
          f"{len(failures)}, wall {seal_wall_s} s")
    print(f"{tag} context_ms {out['context_ms']} doctor_fast_ms {out['doctor_fast_ms']} "
          f"survival_full_ms {out['survival_full_ms']} survival_incremental_ms "
          f"{out['survival_incremental_ms']} handoff_ms {out['handoff_ms']}")
    print(f"{tag} store_ok {out['store_ok']}: {out['integrity']}")
    for f in failures[:5]:
        print(f"{tag} seal failure: {f}")
    if a.json:
        a.json.write_text(json.dumps(out, indent=2) + "\n")
    if not a.keep:
        shutil.rmtree(work, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
