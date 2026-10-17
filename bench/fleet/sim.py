#!/usr/bin/env python3
"""Fleet simulator: N identities editing one repo at once through writ's
installed PostToolUse hook, exactly as Claude Code would call it.

    sim.py --writ BIN [--agents 50] [--edits 20] [--files 1000]
           [--clone REPO] [--shared 0] [--sequential] [--json OUT]
           [--budget-p50-ms 100 --budget-max-ms 500]

Builds a scratch repo (F files with git history, or a `git clone --no-local`
of --clone), runs `writ init -y` with BIN first on PATH, then drives each
identity in its own thread (WRIT_AGENT_ID=sim-NN, session sess-NN): for each
edit it changes one of its own files and pipes the documented PostToolUse
JSON to every writ hook command registered for Edit in
.claude/settings.json. `--shared K` makes every identity also edit K files
that all identities share (the overwrite shape). Measures every hook call's
wall time and exit code, then reads the store back:

  hook_ms {p50, p90, max}, hook_exit_nonzero, hook_stdout_nonempty,
  seals (identity seals), specs (distinct specs they sealed into),
  auto_specs, cross (seals carrying a path the identity never edited),
  unsealed_edits (an edit whose content no seal holds), hooks_log_lines,
  doctor_fast_ms, survival_full_ms, store integrity, binary provenance.

With --budget-* the exit code is 1 when p50 or max is over budget (the
release gate's latency check: one identity, --sequential, --clone of this
repo). On a binary with no PostToolUse hook (0.4.2) it exits 3 and says so:
there is nothing to measure.

Every number is printed with the binary it came from.
"""
from __future__ import annotations

import argparse
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

ROOT = Path(__file__).resolve().parents[2]
INTEGRITY = ROOT / "bench" / "store_integrity.py"
EDIT_TOOLS = ("Edit", "Write", "MultiEdit", "NotebookEdit")


def provenance(writ: str) -> dict:
    p = Path(writ).resolve()
    ver = subprocess.run([str(p), "--version"], capture_output=True, text=True).stdout.strip()
    return {"path": str(p), "version": ver,
            "sha256": hashlib.sha256(p.read_bytes()).hexdigest()}


def base_env(writ: str, home: Path) -> dict:
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("WRIT_", "CLAUDE_CODE_")) and k not in ("CLAUDECODE",)}
    bindir = home / "bin"
    bindir.mkdir(parents=True, exist_ok=True)
    link = bindir / "writ"
    if not link.exists():
        link.symlink_to(Path(writ).resolve())
    env["PATH"] = f"{bindir}:/usr/bin:/bin:/usr/sbin:/sbin"
    env.update(GIT_AUTHOR_NAME="sim", GIT_AUTHOR_EMAIL="sim@example.invalid",
               GIT_COMMITTER_NAME="sim", GIT_COMMITTER_EMAIL="sim@example.invalid",
               GIT_CONFIG_NOSYSTEM="1")
    return env


def sh(args: list, cwd: Path, env: dict, check: bool = True, **kw) -> subprocess.CompletedProcess:
    r = subprocess.run(args, cwd=cwd, env=env, capture_output=True, text=True, **kw)
    if check and r.returncode != 0:
        sys.exit(f"{' '.join(map(str, args))} failed ({r.returncode}):\n{r.stdout}{r.stderr}")
    return r


def build_repo(repo: Path, files: int, agents: int, shared: int, env: dict) -> None:
    repo.mkdir(parents=True)
    sh(["git", "init", "-q", "-b", "main"], repo, env)
    for i in range(files):
        f = repo / f"src/m{i // 100:02d}/f{i:04d}.txt"
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text("".join(f"file {i} line {j}\n" for j in range(30)))
        if i and i % 250 == 0:  # some history, not one commit
            sh(["git", "add", "-A"], repo, env)
            sh(["git", "commit", "-q", "-m", f"files to {i}"], repo, env)
    for a in range(agents):
        (repo / "agents").mkdir(exist_ok=True)
        (repo / f"agents/a{a:02d}.txt").write_text("".join(f"agent {a} line {j}\n" for j in range(40)))
    for s in range(shared):
        (repo / f"shared{s}.txt").write_text("".join(f"shared line {j}\n" for j in range(200)))
    sh(["git", "add", "-A"], repo, env)
    sh(["git", "commit", "-q", "-m", "fleet fixture"], repo, env)


def hook_commands(repo: Path, tool: str = "Edit") -> list[str]:
    s = repo / ".claude" / "settings.json"
    if not s.exists():
        return []
    entries = (json.loads(s.read_text()).get("hooks") or {}).get("PostToolUse") or []
    out = []
    for e in entries:
        m = (e.get("matcher") or "*").strip().strip("^$()")
        if m in ("*", ".*", "") or tool in [t.strip() for t in m.split("|")]:
            out += [h["command"] for h in e.get("hooks", []) if "writ" in h.get("command", "")]
    return out


class Fleet:
    def __init__(self, repo: Path, env: dict, cmds: list[str]):
        self.repo, self.env, self.cmds = repo, env, cmds
        self.lock = threading.Lock()
        self.calls: list[dict] = []
        self.edited: dict[str, set] = {}
        self.final_content: dict[str, str] = {}
        self.edits: list[dict] = []

    def edit(self, agent: str, session: str, rel: str, k: int) -> None:
        path = self.repo / rel
        lines = path.read_text().splitlines(keepends=True)
        idx = (k * 7) % len(lines)
        lines[idx] = f"{agent} edit {k} {lines[idx].strip()}\n"
        path.write_text("".join(lines))
        with self.lock:
            self.edits.append({"agent": agent, "path": rel, "t": time.time()})
        stdin = json.dumps({
            "session_id": session, "transcript_path": f"/tmp/sim/{session}.jsonl",
            "cwd": str(self.repo), "permission_mode": "bypassPermissions",
            "hook_event_name": "PostToolUse", "tool_name": "Edit",
            "tool_input": {"file_path": str(path), "old_string": "x", "new_string": "y",
                           "replace_all": False},
            "tool_response": {"filePath": str(path), "success": True},
            "tool_use_id": f"toolu_{session}_{k}", "duration_ms": 3})
        env = dict(self.env, WRIT_AGENT_ID=agent, CLAUDECODE="1",
                   CLAUDE_CODE_SESSION_ID=session, CLAUDE_PROJECT_DIR=str(self.repo))
        for c in self.cmds:
            t = time.perf_counter()
            r = subprocess.run(["sh", "-c", c], cwd=self.repo, env=env, input=stdin,
                               capture_output=True, text=True)
            ms = (time.perf_counter() - t) * 1000
            with self.lock:
                self.calls.append({"agent": agent, "path": rel, "ms": ms, "rc": r.returncode,
                                   "stdout": r.stdout.strip()[:300]})
        with self.lock:
            self.edited.setdefault(agent, set()).add(rel)
            self.final_content[rel] = path.read_text()


def fake_hook(work: Path) -> str:
    """Stand-in PostToolUse hook for builds without auto seal (harness
    self-test and a naive-hook baseline; never reported as writ's number)."""
    script = work / "fake_hook.py"
    script.write_text(
        "import json, os, subprocess, sys\n"
        "d = json.load(sys.stdin)\n"
        "p = d['tool_input'].get('file_path') or d['tool_input'].get('notebook_path')\n"
        "rel = os.path.relpath(p, d['cwd'])\n"
        "sid = 'auto-' + d['session_id']\n"
        "a = os.environ['WRIT_AGENT_ID']\n"
        "if not os.path.exists(os.path.join(d['cwd'], '.writ', 'specs', sid + '.json')):\n"
        "    subprocess.run(['writ', 'spec', 'add', '--id', sid, '--title', 'auto work by ' + a],"
        " capture_output=True)\n"
        "    subprocess.run(['writ', 'spec', 'claim', sid, '--agent', a], capture_output=True)\n"
        "r = subprocess.run(['writ', 'seal', '-s', 'auto: Edit ' + rel, '--agent', a, '--spec', sid,"
        " '--paths', rel], capture_output=True, text=True)\n"
        "if r.returncode:\n"
        "    open(os.path.join(d['cwd'], '.writ', 'hooks.log'), 'a').write(rel + ': ' + r.stderr.strip()[:200] + '\\n')\n")
    return f"{sys.executable} {script} || true"


def stop_commands(repo: Path) -> list[str]:
    s = repo / ".claude" / "settings.json"
    if not s.exists():
        return []
    entries = (json.loads(s.read_text()).get("hooks") or {}).get("Stop") or []
    return [h["command"] for e in entries for h in e.get("hooks", []) if "writ" in h.get("command", "")]


def iso_ts(s: str) -> float:
    from datetime import datetime
    return datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()


def outcomes(log: Path) -> dict:
    """hooks.log JSONL outcome counts by event (unparseable lines counted)."""
    out: dict = {}
    if not log.exists():
        return out
    for line in log.read_text(errors="replace").splitlines():
        try:
            r = json.loads(line)
        except json.JSONDecodeError:
            r = None
        # Finding 164: an interleaved line can parse as a string or number.
        key = f"{r.get('event')}:{r.get('outcome')}" if isinstance(r, dict) else "unparseable"
        out[key] = out.get(key, 0) + 1
    return out


def pct(xs: list[float], q: float) -> float:
    xs = sorted(xs)
    return round(xs[min(len(xs) - 1, int(q * (len(xs) - 1) + 0.5))], 1)


def timed(args: list, cwd: Path, env: dict) -> tuple[float, subprocess.CompletedProcess]:
    t = time.perf_counter()
    r = subprocess.run(args, cwd=cwd, env=env, capture_output=True, text=True)
    return round((time.perf_counter() - t) * 1000, 1), r


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--writ", required=True)
    ap.add_argument("--agents", type=int, default=50)
    ap.add_argument("--edits", type=int, default=20)
    ap.add_argument("--files", type=int, default=1000)
    ap.add_argument("--shared", type=int, default=0)
    ap.add_argument("--clone", type=Path)
    ap.add_argument("--sequential", action="store_true")
    ap.add_argument("--json", type=Path)
    ap.add_argument("--keep", action="store_true", help="keep the scratch repo")
    ap.add_argument("--fake-hook", action="store_true",
                    help="harness self-test on a build without auto seal: a shell hook that "
                         "claims auto-<session> and runs `writ seal --paths <file>`")
    ap.add_argument("--budget-p50-ms", type=float)
    ap.add_argument("--budget-max-ms", type=float)
    a = ap.parse_args()

    writ = shutil.which(a.writ) or a.writ
    load_start = os.getloadavg()
    prov = provenance(writ)
    work = Path(tempfile.mkdtemp(prefix="writ-fleet-"))
    env = base_env(writ, work / "home")
    env["HOME"] = str(work / "home")
    repo = work / "repo"
    if a.clone:
        sh(["git", "clone", "-q", "--no-local", str(a.clone), str(repo)], work, env)
        (repo / "agents").mkdir(exist_ok=True)
        for i in range(a.agents):
            (repo / f"agents/a{i:02d}.txt").write_text("".join(f"agent {i} line {j}\n" for j in range(40)))
        sh(["git", "add", "-A"], repo, env)
        sh(["git", "commit", "-q", "-m", "fleet agent files"], repo, env)
    else:
        build_repo(repo, a.files, a.agents, a.shared, env)
    init_ms, init = timed(["writ", "init", "-y"], repo, dict(env, WRIT_AGENT_ID="human"))
    if init.returncode != 0:
        sys.exit(f"writ init -y failed:\n{init.stdout}{init.stderr}")
    sh(["git", "add", "-A"], repo, env)
    sh(["git", "commit", "-q", "--no-verify", "-m", "writ init"], repo, env, check=False)
    cmds = hook_commands(repo)
    if not cmds and a.fake_hook:
        cmds = [fake_hook(work)]
    if not cmds:
        print(f"{prov['version']} ({prov['sha256'][:12]}): writ init installs no PostToolUse "
              f"hook for Edit; nothing to measure")
        return 3
    tracked = len(sh(["git", "ls-files"], repo, env).stdout.splitlines())
    commits = int(sh(["git", "rev-list", "--count", "HEAD"], repo, env).stdout.strip())

    fleet = Fleet(repo, env, cmds)
    names = [f"sim-{i:02d}" for i in range(a.agents)]

    def worker(i: int) -> None:
        for k in range(a.edits):
            own = f"agents/a{i:02d}.txt"
            rel = own if not a.shared or k % 4 else f"shared{k % a.shared}.txt"
            fleet.edit(names[i], f"sess-{i:02d}", rel, k)

    t0 = time.perf_counter()
    if a.sequential:
        for i in range(a.agents):
            worker(i)
    else:
        threads = [threading.Thread(target=worker, args=(i,)) for i in range(a.agents)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
    wall_s = round(time.perf_counter() - t0, 1)
    # Each session's turn ends: its Stop hook sweeps what it missed (149).
    stops = stop_commands(repo)
    stop_ms = []
    for i, name in enumerate(names):
        stdin = json.dumps({"session_id": f"sess-{i:02d}", "transcript_path": f"/tmp/sim/sess-{i:02d}.jsonl",
                            "cwd": str(repo), "hook_event_name": "Stop", "stop_hook_active": False,
                            "last_assistant_message": "done", "background_tasks": []})
        senv = dict(env, WRIT_AGENT_ID=name, CLAUDECODE="1", CLAUDE_CODE_SESSION_ID=f"sess-{i:02d}",
                    CLAUDE_PROJECT_DIR=str(repo))
        for c in stops:
            t = time.perf_counter()
            r = subprocess.run(["sh", "-c", c], cwd=repo, env=senv, input=stdin, capture_output=True, text=True)
            stop_ms.append((time.perf_counter() - t) * 1000)
            if r.returncode != 0:
                fleet.calls.append({"agent": name, "path": "<Stop>", "ms": 0, "rc": r.returncode,
                                    "stdout": r.stdout.strip()[:300]})

    ms = [c["ms"] for c in fleet.calls]
    log_ms, lg = timed(["writ", "log", "--all", "--format", "json", "--limit", "100000"],
                       repo, dict(env, WRIT_AGENT_ID="human"))
    seals = json.loads(lg.stdout or "[]")
    mine = [s for s in seals if (s.get("agent") or {}).get("id") in names]
    cross = [s["id"][:12] for s in mine
             for c in s.get("changes") or []
             if c["path"] not in fleet.edited.get(s["agent"]["id"], set())]
    specs = sorted({s.get("spec_id") for s in mine if s.get("spec_id")})
    # Edit-to-seal window: first identity seal at or after the edit holding the path.
    seal_times: dict[str, list[float]] = {}
    for s in mine:
        for c in s.get("changes") or []:
            seal_times.setdefault(c["path"], []).append(iso_ts(s["timestamp"]))
    for v in seal_times.values():
        v.sort()
    windows, never = [], []
    for e in fleet.edits:
        nxt = next((x for x in seal_times.get(e["path"], []) if x >= e["t"] - 0.002), None)
        (never.append(e["path"]) if nxt is None else windows.append((nxt - e["t"]) * 1000))
    # Edits left unsealed at the end: edited paths still pending.
    ctx = subprocess.run(["writ", "context", "--format", "json"], cwd=repo,
                         env=dict(env, WRIT_AGENT_ID="human"), capture_output=True, text=True)
    try:
        pend = json.loads(ctx.stdout).get("pending_changes") or json.loads(ctx.stdout).get("pending") or {}
        pend_paths = {c.get("path") for c in (pend.get("changes") or pend.get("files") or [])
                      if isinstance(c, dict)}
    except (json.JSONDecodeError, AttributeError):
        pend_paths = set()
    edited_all = set().union(*fleet.edited.values()) if fleet.edited else set()
    unsealed = sorted(edited_all & pend_paths)
    hooks_log = repo / ".writ" / "hooks.log"
    henv = dict(env, WRIT_AGENT_ID="human")
    doctor_ms, _ = timed(["writ", "doctor", "--format", "json"], repo, henv)
    cache = repo / ".writ" / "survival_cache.json"
    if cache.exists():
        cache.unlink()
    survival_ms, surv = timed(["writ", "doctor", "--survival", "--format", "json"], repo, henv)
    context_ms, _ = timed(["writ", "context", "--format", "brief"], repo, henv)
    handoff_ms, _ = timed(["writ", "handoff", "--format", "json"], repo, henv)
    # Ruling 181: `spec done` runs survival for the identity's own specs; its
    # time at this size is a fleet number. One identity's auto spec, closed.
    first = names[0] if names else None
    sd_spec = next((x.get("spec_id") for x in mine if (x.get("agent") or {}).get("id") == first), None)
    spec_done_ms, sd = (timed(["writ", "spec", "done", sd_spec, "-s", "sim done", "--agent", first,
                               "--no-seal"], repo, dict(env, WRIT_AGENT_ID=first))
                        if sd_spec else (None, None))
    integ = subprocess.run([sys.executable, str(INTEGRITY), str(repo)], capture_output=True, text=True)
    try:
        headline = json.loads(surv.stdout).get("headline")
    except json.JSONDecodeError:
        headline = (surv.stdout + surv.stderr).strip()[:200]

    res = {
        "binary": prov, "date": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "load_avg": {"start": [round(x, 2) for x in load_start],
                     "end": [round(x, 2) for x in os.getloadavg()], "cpus": os.cpu_count()},
        "repo": {"files": tracked, "git_commits": commits, "cloned_from": str(a.clone) if a.clone else None},
        "agents": a.agents, "edits_per_agent": a.edits, "shared_files": a.shared,
        "concurrent": not a.sequential, "wall_s": wall_s, "init_ms": init_ms,
        "hook_commands": len(cmds), "hook_calls": len(ms),
        "hook_ms": {"p50": pct(ms, 0.5), "p90": pct(ms, 0.9), "max": round(max(ms), 1)},
        "hook_exit_nonzero": sum(c["rc"] != 0 for c in fleet.calls),
        "hook_stdout_nonempty": sum(bool(c["stdout"]) for c in fleet.calls),
        "hook_stdout_samples": sorted({c["stdout"] for c in fleet.calls if c["stdout"]})[:5],
        "window_ms": {"p50": pct(windows, 0.5) if windows else None,
                      "p90": pct(windows, 0.9) if windows else None,
                      "max": round(max(windows), 1) if windows else None},
        "edits": len(fleet.edits), "edits_sealed_by_end": len(windows),
        "edits_never_sealed": len(never), "stop_hooks": len(stops),
        "stop_ms_max": round(max(stop_ms), 1) if stop_ms else None,
        "seals": len(mine), "specs": len(specs),
        "auto_specs": sum(1 for s in specs if s.startswith("auto-")),
        "cross": len(cross), "cross_seals": cross[:10],
        "unsealed_edited_paths": len(unsealed), "unsealed_sample": unsealed[:10],
        "hooks_log_lines": len(hooks_log.read_text().splitlines()) if hooks_log.exists() else 0,
        "hooks_log_outcomes": outcomes(hooks_log),
        "log_ms": log_ms, "doctor_fast_ms": doctor_ms, "survival_full_ms": survival_ms,
        "survival_headline": headline, "context_ms": context_ms, "handoff_ms": handoff_ms,
        "spec_done_ms": spec_done_ms, "spec_done_rc": sd.returncode if sd else None,
        "integrity": (integ.stdout.strip().splitlines() or [integ.stderr.strip()])[-1],
        "integrity_rc": integ.returncode, "store_ok": integ.returncode == 0,
        "scratch": str(work) if a.keep else None,
    }
    over = []
    if a.budget_p50_ms is not None and res["hook_ms"]["p50"] > a.budget_p50_ms:
        over.append(f"p50 {res['hook_ms']['p50']} ms > {a.budget_p50_ms}")
    if a.budget_max_ms is not None and res["hook_ms"]["max"] > a.budget_max_ms:
        over.append(f"max {res['hook_ms']['max']} ms > {a.budget_max_ms}")
    res["over_budget"] = over

    tag = f"[{prov['version']} {prov['sha256'][:12]} {prov['path']}]"
    print(f"{tag} load average 1/5/15 min at start {res['load_avg']['start']}, end {res['load_avg']['end']}"
          f" ({os.cpu_count()} cpus)")
    print(f"{tag} repo {tracked} files, {commits} commits; {a.agents} identities x {a.edits} edits"
          f" ({'sequential' if a.sequential else 'concurrent'}), wall {wall_s} s")
    print(f"{tag} hook_ms p50 {res['hook_ms']['p50']} p90 {res['hook_ms']['p90']} max {res['hook_ms']['max']}"
          f" over {len(ms)} calls; exit nonzero {res['hook_exit_nonzero']}; stdout nonempty {res['hook_stdout_nonempty']}")
    print(f"{tag} window_ms (edit to seal) p50 {res['window_ms']['p50']} p90 {res['window_ms']['p90']}"
          f" max {res['window_ms']['max']}; edits sealed by end {len(windows)}/{len(fleet.edits)}"
          f" (Stop hooks {len(stops)})")
    print(f"{tag} seals {res['seals']} specs {res['specs']} (auto {res['auto_specs']}) cross {res['cross']}"
          f" unsealed edited paths {res['unsealed_edited_paths']} hooks.log lines {res['hooks_log_lines']}")
    print(f"{tag} hooks.log outcomes {res['hooks_log_outcomes']}")
    print(f"{tag} doctor_fast_ms {doctor_ms} survival_full_ms {survival_ms} context_ms {context_ms}"
          f" handoff_ms {handoff_ms} log_ms {log_ms} spec_done_ms {spec_done_ms}; survival: {headline}")
    print(f"{tag} integrity rc {integ.returncode}: {res['integrity']}")
    if over:
        print(f"{tag} OVER BUDGET: {'; '.join(over)}")
    if a.json:
        a.json.write_text(json.dumps(res, indent=2))
    if not a.keep:
        shutil.rmtree(work, ignore_errors=True)
    return 1 if over else 0


if __name__ == "__main__":
    sys.exit(main())
