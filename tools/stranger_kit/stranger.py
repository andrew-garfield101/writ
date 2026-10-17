#!/usr/bin/env python3
"""Stranger kit: run two unprompted `claude -p` agents on a copy of any git
repository with writ installed, then score what happened.

    stranger.py setup   PROJECT OUT [--writ BIN] [--skip-init]
    stranger.py launch  PROJECT OUT --prompt FILE --prompt FILE
                        [--inject none|stale-claim|committed-spec]
                        [--inject-after SECS] [--timeout SECS]
    stranger.py collect PROJECT OUT [--no-finish]
    stranger.py score   OUT

setup records the starting state (writ version, HEAD, doctor both tiers) and
runs `writ init -y`, nothing else. launch starts both agents at once with the
prompts verbatim and an environment holding exactly one writ. collect reads
the end state, runs `writ finish -y` (never --force) and audits every commit
writ made since setup for lost sealed lines. score reads only OUT and writes
OUT/summary.json and OUT/report.md.

Scoring is the sprint 3-5 exit-criterion collector (tools/exit_run_040.py)
made repo-agnostic: agents are identified by the writ ids writ printed to
them, not by driver names; injections are optional.
"""
from __future__ import annotations

import argparse
import collections
import json
import os
import re
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

KIT = Path(__file__).resolve().parent
AUDIT = KIT / "finish_audit.py"
INTEGRITY = KIT / "store_integrity.py"
AGENTS = ("agent-1", "agent-2")
GHOST_SPEC = "ghost-cleanup"
CHECKS = ("store_integrity", "stale_claim", "committed_spec_seal", "unsealed_at_risk",
          "version_skew", "left_out", "survival")
SCRUB_PREFIXES = ("CLAUDE_CODE_", "CLAUDE_SESSION", "ANTHROPIC_SESSION", "CODEX_")
SCRUB_EXACT = {"WRIT_AGENT_ID", "CLAUDECODE", "CLAUDE_PID", "CLAUDE_EFFORT"}
REFUSAL_PATTERNS = [
    (r"seal refused", "seal refused"),
    (r"already committed to git", "committed spec"),
    (r"No active spec for this seal|no spec claimed", "no active spec"),
    (r"finish refused|refusing to finish", "finish refused"),
    (r"\bwas refused\b|\brefused:", "other refusal"),
]


# ---------------------------------------------------------------------------
# Environment and process helpers
# ---------------------------------------------------------------------------

def load_kit(out: Path) -> dict:
    p = out / "kit.json"
    return json.loads(p.read_text()) if p.exists() else {}


def save_kit(out: Path, kit: dict) -> None:
    (out / "kit.json").write_text(json.dumps(kit, indent=2))


def path_with_one_writ(writ: Path, path: str, mirror_root: Path) -> str:
    """PATH with the writ under test first and every other directory that
    holds a `writ` replaced by a mirror without it: one writ, as on a machine
    with one install (two would be a real version_skew finding)."""
    out = [str(writ.parent)]
    for i, d in enumerate(p for p in path.split(":") if p):
        if Path(d).resolve() == writ.parent.resolve():
            continue
        if not (Path(d) / "writ").exists():
            out.append(d)
            continue
        mirror = mirror_root / str(i)
        if not mirror.exists():
            mirror.mkdir(parents=True)
            for entry in Path(d).iterdir():
                if entry.name != "writ":
                    (mirror / entry.name).symlink_to(entry)
        out.append(str(mirror))
    return ":".join(out)


def agent_env(kit: dict, out: Path, **extra: str) -> dict:
    env = {k: v for k, v in os.environ.items()
           if k not in SCRUB_EXACT and not k.startswith(SCRUB_PREFIXES)}
    env["PATH"] = path_with_one_writ(Path(kit["writ"]), env.get("PATH", ""), out / ".path-mirror")
    env.update(extra)
    return env


def run(args: list, cwd: Path, env: dict | None = None, timeout: int = 600):
    return subprocess.run(args, cwd=cwd, env=env, capture_output=True, text=True,
                          stdin=subprocess.DEVNULL, timeout=timeout)


def writ(kit: dict, out: Path, project: Path, *args: str, agent: str = "human"):
    return run([kit["writ"], *args], project, agent_env(kit, out, WRIT_AGENT_ID=agent))


def log(out: Path, text: str) -> None:
    line = f"[{time.strftime('%Y-%m-%dT%H:%M:%S')}] {text}"
    with open(out / "run.log", "a") as f:
        f.write(line + "\n")
    print(line, flush=True)


def doctor_both(kit: dict, out: Path, project: Path, tag: str) -> dict:
    """Both tiers, JSON and human, saved as doctor-<tag>-{fast,survival}.*"""
    res = {}
    for tier, extra in (("fast", []), ("survival", ["--survival"])):
        j = writ(kit, out, project, "doctor", *extra, "--format", "json")
        h = writ(kit, out, project, "doctor", *extra)
        (out / f"doctor-{tag}-{tier}.json").write_text(j.stdout)
        (out / f"doctor-{tag}-{tier}.txt").write_text(h.stdout + h.stderr)
        try:
            v = json.loads(j.stdout)
        except ValueError:
            v = {"headline": f"(unparsed, rc {j.returncode})", "findings": []}
        res[tier] = {"rc": j.returncode, "headline": v.get("headline"),
                     "findings": [{k: f.get(k) for k in
                                   ("check", "severity", "message", "fix_command",
                                    "needs_human", "anyone_may_fix")}
                                  for f in v.get("findings", [])]}
    return res


# ---------------------------------------------------------------------------
# setup / launch / collect
# ---------------------------------------------------------------------------

# sha256 (first 12 hex) of shipped binaries, by channel. Add each release.
KNOWN_RELEASES = {
    "1f5e2dc44d2d": "writ 0.4.2 (PyPI writ-vcs macOS arm64 wheel; Homebrew bottle, same bytes)",
}


def build_provenance(w: str, built_from: str | None) -> dict:
    """What binary this run measured: path, sha256, and for a dev build
    (a binary under a writ checkout's target/) the seal it was built from.
    The same version string on a PyPI or brew install is another product."""
    import hashlib
    path = Path(w).resolve()
    prov = {"path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()[:16]}
    checkout = next((p.parent for p in path.parents if p.name == "target"), None)
    if built_from:
        prov.update(kind="dev build", built_from=built_from)
    elif checkout and (checkout / ".writ").exists():
        lg = run([str(path), "log", "--all", "--format", "json", "--limit", "1"], checkout)
        try:
            seal = json.loads(lg.stdout)[0]["id"][:12]
        except (ValueError, IndexError, KeyError):
            seal = "unknown"
        prov.update(kind="dev build", built_from=f"tree at newest seal {seal} (may include unsealed edits; "
                                                 f"pass --built-from to state it)")
    elif prov["sha256"][:12] in KNOWN_RELEASES:
        prov.update(kind="release install", release=KNOWN_RELEASES[prov["sha256"][:12]])
    else:
        # Finding 172: a binary outside target/ is not a release just because
        # of where it sits; say so unless its sha is a known release.
        prov["kind"] = "unknown build (sha not a known release; pass --built-from to state it)"
    return prov


def cmd_setup(project: Path, out: Path, writ_bin: str | None, skip_init: bool,
              built_from: str | None = None) -> None:
    w = shutil.which(writ_bin or "writ")
    if not w:
        sys.exit("no writ found; install it (see README) or pass --writ")
    if not (project / ".git").exists():
        sys.exit(f"{project} is not a git repository")
    if run(["git", "status", "--porcelain"], project).stdout.strip():
        sys.exit(f"{project} has uncommitted changes; start from a clean copy")
    out.mkdir(parents=True, exist_ok=True)
    # The agents and their hooks run `writ` from PATH (the hooks writ init
    # installs resolve `command -v writ`). A binary under another name
    # (writ-2de4) would leave them with no writ and every hook a silent no-op,
    # so the binary under test is always exposed as OUT/.bin/writ.
    bindir = out / ".bin"
    bindir.mkdir(exist_ok=True)
    link = bindir / "writ"
    if link.is_symlink() or link.exists():
        link.unlink()
    link.symlink_to(Path(w).resolve())
    kit = {"writ": str(link), "writ_target": str(Path(w).resolve()),
           "project": str(project.resolve()),
           "writ_version": run([w, "--version"], project).stdout.strip(),
           "writ_build": build_provenance(w, built_from),
           "claude_version": run(["claude", "--version"], project).stdout.strip()
           if shutil.which("claude") else None,
           "head_at_setup": run(["git", "rev-parse", "HEAD"], project).stdout.strip(),
           "git_commits_at_setup": int(run(["git", "rev-list", "--count", "HEAD"], project).stdout or 0),
           "tracked_files_at_setup": len(run(["git", "ls-files"], project).stdout.splitlines()),
           "setup_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
    save_kit(out, kit)
    seen = shutil.which("writ", path=agent_env(kit, out)["PATH"])
    if not seen or Path(seen).resolve() != Path(w).resolve():
        sys.exit(f"the agents' PATH resolves writ to {seen!r}, not {w}; refusing to run")
    if not skip_init:
        init = writ(kit, out, project, "init", "-y")
        (out / "init.txt").write_text(init.stdout + init.stderr)
        kit["init_rc"] = init.returncode
        if init.returncode != 0:
            save_kit(out, kit)
            sys.exit(f"writ init -y failed (rc {init.returncode}); see {out / 'init.txt'}")
    for f in ("CLAUDE.md", "AGENTS.md", ".claude/settings.json", ".mcp.json"):
        if (project / f).exists():
            (out / ("generated-" + f.replace("/", "_"))).write_text((project / f).read_text())
    kit["doctor_before"] = doctor_both(kit, out, project, "before")
    save_kit(out, kit)
    log(out, f"setup done: {kit['writ_version']}, HEAD {kit['head_at_setup'][:12]}, "
             f"doctor before: {kit['doctor_before']['survival']['headline']}")


def inject_stale_claim(kit: dict, out: Path, project: Path) -> None:
    """A third `claude -p` session claims a spec and exits: its claim is held
    by a process that no longer exists (the 0.4.0/0.4.1 run 4 shape)."""
    prompt = (f"Run exactly this shell command and then stop, do nothing else: "
              f"writ spec add --id {GHOST_SPEC} --title 'Clean up README wording' --claim")
    r = run(["claude", "-p", prompt, "--dangerously-skip-permissions",
             "--output-format", "stream-json", "--verbose"], project, agent_env(kit, out))
    (out / "inject-session.jsonl").write_text(r.stdout)
    spec = project / ".writ" / "specs" / f"{GHOST_SPEC}.json"
    held = json.loads(spec.read_text()) if spec.exists() else {}
    log(out, f"inject stale-claim: rc={r.returncode} claimed_by={held.get('claimed_by')} "
             f"pid={held.get('claimed_pid')}")
    kit.setdefault("injection", {}).update(spec=GHOST_SPEC, at=time.time(),
                                           claimed_by=held.get("claimed_by"))


def inject_committed_spec(kit: dict, out: Path, project: Path, stop: threading.Event) -> None:
    """A human finishes mid-run: the first agent spec with a seal is marked
    complete and `writ finish -y` commits it while its agent is still
    working. Its next seal on that spec meets the committed-spec refusal."""
    while not stop.is_set():
        for f in sorted((project / ".writ" / "specs").glob("*.json")):
            d = json.loads(f.read_text())
            if (d.get("claimed_by") and d.get("sealed_by") and d["id"] != GHOST_SPEC
                    and d.get("status") not in ("complete", "committed") and not d.get("commit_hash")):
                up = writ(kit, out, project, "spec", "update", d["id"], "--status", "complete")
                fin = writ(kit, out, project, "finish", "-y")
                (out / "inject-finish.txt").write_text(up.stdout + up.stderr + fin.stdout + fin.stderr)
                spec = json.loads(f.read_text())
                log(out, f"inject committed-spec: {d['id']} (holder {d.get('claimed_by')}) "
                         f"update rc={up.returncode} finish rc={fin.returncode} "
                         f"commit={spec.get('commit_hash')}")
                if spec.get("commit_hash"):
                    kit.setdefault("injection", {}).update(
                        spec=d["id"], holder=d.get("claimed_by"), at=time.time(),
                        commit=spec["commit_hash"])
                    return
        stop.wait(5)


def cmd_launch(project: Path, out: Path, prompts: list[Path], inject: str,
               inject_after: int, timeout: int) -> None:
    if not shutil.which("claude"):
        sys.exit("launch needs `claude` (the Claude Code CLI) on PATH; setup, collect and "
                 "score do not")
    kit = load_kit(out)
    if not kit:
        sys.exit(f"no kit.json in {out}; run `stranger.py setup` first")
    if len(prompts) != 2:
        sys.exit("pass exactly two --prompt files")
    kit["prompts"] = {a: p.read_text().strip() for a, p in zip(AGENTS, prompts)}
    kit["inject"] = inject
    kit["launched_at"] = time.time()
    save_kit(out, kit)
    procs = []
    for agent in AGENTS:
        p = subprocess.Popen(
            ["claude", "-p", kit["prompts"][agent], "--dangerously-skip-permissions",
             "--output-format", "stream-json", "--verbose"],
            cwd=project, env=agent_env(kit, out), stdin=subprocess.DEVNULL,
            stdout=open(out / f"{agent}.jsonl", "w"), stderr=open(out / f"{agent}.stderr", "w"))
        procs.append((agent, p, time.monotonic()))
        log(out, f"launched {agent} pid {p.pid}")
    stop = threading.Event()
    injector = None
    if inject == "stale-claim":
        injector = threading.Timer(inject_after, inject_stale_claim, (kit, out, project))
    elif inject == "committed-spec":
        injector = threading.Timer(inject_after, inject_committed_spec, (kit, out, project, stop))
    if injector:
        injector.start()
    # Poll both so each exit is logged when it happens, not in launch order.
    deadline = time.monotonic() + timeout
    live = list(procs)
    while live:
        for item in list(live):
            agent, p, t0 = item
            rc = p.poll()
            if rc is None and time.monotonic() > deadline:
                p.kill()
                rc = "TIMEOUT"
            if rc is not None:
                log(out, f"{agent} exited rc={rc} after {round(time.monotonic() - t0)} s")
                live.remove(item)
        time.sleep(1)
    stop.set()
    if injector:
        injector.cancel()
        injector.join()
    kit["ended_at"] = time.time()
    save_kit(out, kit)


def copy_session_transcripts(out: Path) -> None:
    """Claude Code keeps each session's full transcript, and each subagent's
    own, under ~/.claude/projects/<slug>/<session>[.jsonl|/subagents/]. The
    -p stream does not carry a background subagent's tool calls or the hook
    attachments, so collect copies them: OUT/agent-N.session.jsonl and
    OUT/agent-N.subagents/agent-<id>.jsonl (+ .meta.json)."""
    projects = Path(os.environ.get("CLAUDE_CONFIG_DIR", Path.home() / ".claude")) / "projects"
    for a in AGENTS:
        sid = next((e["session_id"] for e in events(out / f"{a}.jsonl") if e.get("session_id")), None)
        if not sid:
            continue
        for main in projects.glob(f"*/{sid}.jsonl"):
            shutil.copy(main, out / f"{a}.session.jsonl")
        for sub in projects.glob(f"*/{sid}/subagents/agent-*"):
            d = out / f"{a}.subagents"
            d.mkdir(exist_ok=True)
            shutil.copy(sub, d / sub.name)


def cmd_collect(project: Path, out: Path, no_finish: bool) -> None:
    kit = load_kit(out)
    specs = {}
    for f in (project / ".writ" / "specs").glob("*.json"):
        d = json.loads(f.read_text())
        specs[d["id"]] = d
    (out / "specs.json").write_text(json.dumps(specs, indent=2))
    lg = writ(kit, out, project, "log", "--all", "--format", "json")
    (out / "seals.json").write_text(lg.stdout or "[]")
    copy_session_transcripts(out)
    hl = project / ".writ" / "hooks.log"
    (out / "hooks.log").write_text(hl.read_text(errors="replace") if hl.exists() else "")
    ev = project / ".writ" / "security" / "events.jsonl"
    (out / "events.jsonl").write_text(ev.read_text(errors="replace") if ev.exists() else "")
    kit["doctor_after"] = doctor_both(kit, out, project, "after")
    status = run(["git", "status", "--porcelain"], project).stdout
    (out / "git-status-after.txt").write_text(status)
    head0 = run(["git", "rev-parse", "HEAD"], project).stdout.strip()
    if not no_finish:
        fin = writ(kit, out, project, "finish", "-y")
        (out / "finish.txt").write_text(fin.stdout + fin.stderr)
        kit["finish_rc"] = fin.returncode
        kit["doctor_final"] = doctor_both(kit, out, project, "final")
    head1 = run(["git", "rev-parse", "HEAD"], project).stdout.strip()
    if head1 != head0:
        (out / "receipt.txt").write_text(run(["git", "log", "-1", "--format=%B", head1], project).stdout)
    kit["head_before_finish"], kit["head_after_finish"] = head0, head1
    commits = run(["git", "rev-list", "--reverse", f"{kit['head_at_setup']}..{head1}"],
                  project).stdout.split()
    kit["commits_since_setup"] = commits
    audit = run(["python3", str(AUDIT), *commits], project,
                dict(os.environ, WRIT_AUDIT_REPO=str(project))) if commits else None
    (out / "audit.txt").write_text((audit.stdout + audit.stderr) if audit else "(no commits)\n")
    integ = run(["python3", str(INTEGRITY), str(project)], project)
    kit["integrity"] = integ.stdout.strip().splitlines()[-1:] or [integ.stderr.strip()]
    kit["integrity_rc"] = integ.returncode
    kit["collected_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    save_kit(out, kit)
    log(out, f"collected: finish rc {kit.get('finish_rc')}, {len(commits)} commit(s) since setup, "
             f"integrity rc {integ.returncode}")


# ---------------------------------------------------------------------------
# Transcript analysis (pure: reads OUT only)
# ---------------------------------------------------------------------------

def events(transcript: Path) -> list[dict]:
    out = []
    if not transcript.exists():
        return out
    for line in transcript.read_text(errors="replace").splitlines():
        try:
            out.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return out


def timeline(evs: list[dict]) -> list[dict]:
    """Ordered steps: {"kind": "text"|"call"|"result", ...}. Tool results
    appear in user messages; each is joined to its call's command."""
    steps, cmds = [], {}
    for ev in evs:
        msg = ev.get("message") or {}
        for b in msg.get("content") or []:
            if not isinstance(b, dict):
                continue
            if b.get("type") == "text" and ev.get("type") == "assistant":
                steps.append({"kind": "text", "text": b.get("text", ""), "at": ev.get("timestamp")})
            elif b.get("type") == "tool_use":
                inp = b.get("input") or {}
                cmd = inp.get("command") if b.get("name") == "Bash" else \
                    f"<{b.get('name')}> " + json.dumps(inp)[:300]
                cmds[b["id"]] = cmd or ""
                steps.append({"kind": "call", "id": b["id"], "command": cmd or "", "at": ev.get("timestamp")})
            elif b.get("type") == "tool_result":
                c = b.get("content")
                text = "\n".join(x.get("text", "") for x in c if isinstance(x, dict)) \
                    if isinstance(c, list) else (c or "")
                steps.append({"kind": "result", "id": b.get("tool_use_id"), "text": text,
                              "command": cmds.get(b.get("tool_use_id"), ""),
                              "is_error": bool(b.get("is_error")), "at": ev.get("timestamp")})
    return steps


def finding_units(text: str) -> list[str]:
    """Every doctor finding in a tool result, as the exact text the agent
    saw: the human form ("[yellow] check: message" plus its "fix:" line) or
    the context/toon block ("- check: x" with its indented fields)."""
    units, lines = [], text.splitlines()
    for i, line in enumerate(lines):
        m = re.match(r"\s*\[(red|yellow|info)\] (\w+): ", line)
        if m:
            unit = [line.strip()]
            if i + 1 < len(lines) and lines[i + 1].strip().startswith("fix:"):
                unit.append(lines[i + 1].strip())
            units.append(" | ".join(unit))
            continue
        m = re.match(r"(\s*)- check: (\w+)", line)
        if m and m.group(2) in CHECKS:
            ind, unit = len(m.group(1)), [line.strip()]
            for nxt in lines[i + 1:]:
                if not nxt.strip() or len(nxt) - len(nxt.lstrip()) <= ind or nxt.strip().startswith("- "):
                    break
                if not nxt.strip().startswith("paths["):
                    unit.append(nxt.strip())
            units.append(" | ".join(unit))
    return units


def refusal_line(text: str) -> tuple[str, str] | None:
    for pat, kind in REFUSAL_PATTERNS:
        for line in text.splitlines():
            if re.search(pat, line, re.I):
                return kind, line.strip()
    return None


def first_line(text: str) -> str:
    return next((l.strip() for l in text.splitlines() if l.strip()), "")


EDIT_TOOLS = ("Edit", "Write", "MultiEdit", "NotebookEdit")
HOOK_CTX = re.compile(r"(\w+(?::\w+)?) hook additional context: (.+)")
WRIT_CMD = re.compile(r"(?:^|[;&|(]\s*|\s)(?:\S*/)?writ\s+([a-z][\w-]*)")


def strings(obj) -> list[str]:
    """Every string inside a decoded JSON value (hook context can sit in a
    tool result, an attachment or a system reminder; find it anywhere)."""
    if isinstance(obj, str):
        return [obj]
    if isinstance(obj, dict):
        return [x for v in obj.values() for x in strings(v)]
    if isinstance(obj, list):
        return [x for v in obj for x in strings(v)]
    return []


def edits_of(evs: list[dict]) -> list[dict]:
    """Every edit-tool call, main or subagent (stream-json carries subagent
    events with parent_tool_use_id set), with its path and time."""
    out = []
    for ev in evs:
        for b in (ev.get("message") or {}).get("content") or []:
            if isinstance(b, dict) and b.get("type") == "tool_use" and b.get("name") in EDIT_TOOLS:
                inp = b.get("input") or {}
                path = inp.get("file_path") or inp.get("notebook_path")
                if path:
                    out.append({"at": ev.get("timestamp"), "tool": b["name"], "path": path,
                                "subagent": bool(ev.get("parent_tool_use_id"))})
    return out


def hook_context_seen(evs: list[dict], steps: list[dict]) -> list[dict]:
    """Each `<Event>:<Tool> hook additional context: <line>` the model saw,
    once per distinct line, with the first text it wrote after it."""
    seen = {}
    texts = [(i, s) for i, s in enumerate(steps) if s["kind"] == "text"]
    for ev in evs:
        for st in strings(ev):
            for m in HOOK_CTX.finditer(st):
                # One line; a wrapper tag after it (</system-reminder>) is not text.
                line = m.group(2).split("\n")[0].split("</")[0].strip()
                at = ev.get("timestamp")
                if line not in seen:
                    reply = next((first_line(s["text"]) for _, s in texts
                                  if (s.get("at") or "") >= (at or "") and first_line(s["text"])), "")
                    seen[line] = {"event": m.group(1), "line": line, "first_at": at, "times": 0,
                                  "subagent": bool(ev.get("parent_tool_use_id")), "reply": reply}
                seen[line]["times"] += 1
    return list(seen.values())


_M64 = (1 << 64) - 1


def _rotl(x: int, b: int) -> int:
    return ((x << b) | (x >> (64 - b))) & _M64


def _siphash13(data: bytes) -> int:
    """Rust's DefaultHasher (SipHash-1-3, zero keys)."""
    v0, v1 = 0x736F6D6570736575, 0x646F72616E646F6D
    v2, v3 = 0x6C7967656E657261, 0x7465646279746573

    def rnd():
        nonlocal v0, v1, v2, v3
        v0 = (v0 + v1) & _M64; v1 = _rotl(v1, 13) ^ v0; v0 = _rotl(v0, 32)
        v2 = (v2 + v3) & _M64; v3 = _rotl(v3, 16) ^ v2
        v0 = (v0 + v3) & _M64; v3 = _rotl(v3, 21) ^ v0
        v2 = (v2 + v1) & _M64; v1 = _rotl(v1, 17) ^ v2; v2 = _rotl(v2, 32)
    n, end = len(data), len(data) - len(data) % 8
    for i in range(0, end, 8):
        m = int.from_bytes(data[i:i + 8], "little")
        v3 ^= m; rnd(); v0 ^= m
    b = (n & 0xFF) << 56
    for j, c in enumerate(data[end:]):
        b |= c << (8 * j)
    v3 ^= b; rnd(); v0 ^= b
    v2 ^= 0xFF
    for _ in range(3):
        rnd()
    return v0 ^ v1 ^ v2 ^ v3


def writ_id_for_session(session_id: str) -> str:
    """The id writ gives a Claude Code session (agent.rs: `claude-code-` and
    the low 16 bits of DefaultHasher over the session id as a str), so an
    agent that never ran a writ command can still be matched to its seals."""
    return "claude-code-%04x" % (_siphash13(session_id.encode() + b"\xff") & 0xFFFF)


def hand_writ_commands(cmds: list[str]) -> list[str]:
    """writ commands the agent typed itself (Bash), not ones a hook ran."""
    return [c for c in cmds if WRIT_CMD.search(c)]


def subagent_transcripts(transcript: Path) -> list[Path]:
    d = transcript.parent / (transcript.stem + ".subagents")
    return sorted(d.glob("agent-*.jsonl")) if d.is_dir() else []


BYPASS_REMINDER = "make file changes with sed, heredocs, or short scripts"


def tool_counts(evs: list[dict]) -> dict:
    c = collections.Counter()
    for ev in evs:
        for b in (ev.get("message") or {}).get("content") or []:
            if isinstance(b, dict) and b.get("type") == "tool_use":
                c[b.get("name")] += 1
    return dict(c)


def analyze_subagent(t: Path, main_ids: list[str]) -> dict:
    evs = events(t)
    steps = timeline(evs)
    aid = next((e.get("agentId") for e in evs if e.get("agentId")), t.stem.removeprefix("agent-"))
    meta = t.with_suffix(".meta.json")
    m = json.loads(meta.read_text()) if meta.exists() else {}
    cmds = [x["command"] for x in steps if x["kind"] == "call"]
    edits = edits_of(evs)
    for e in edits:
        e["subagent"] = True
    return {"agent_id": aid, "type": m.get("agentType"), "description": m.get("description"),
            "writ_ids": [f"{i}-{aid[:8]}" for i in main_ids], "edits": edits,
            "hook_context_seen": hook_context_seen(evs, steps),
            "hand_writ_commands": hand_writ_commands(cmds), "tools_subagent_own": tool_counts(evs),
            "bypass_reminder": BYPASS_REMINDER in t.read_text(errors="replace")}


def analyze_agent(transcript: Path) -> dict:
    evs = events(transcript)
    steps = timeline(evs)
    results = [s for s in steps if s["kind"] == "result"]
    calls = [s for s in steps if s["kind"] == "call"]
    seen, refusals = {}, []
    for i, s in enumerate(steps):
        if s["kind"] != "result":
            continue
        reply = next((first_line(t["text"]) for t in steps[i + 1:] if t["kind"] == "text"
                      and first_line(t["text"])), "")
        nxt = [t["command"][:200] for t in steps[i + 1:] if t["kind"] == "call"][:2]
        for u in finding_units(s["text"]):
            if u not in seen:
                seen[u] = {"text": u, "first_at": s["at"], "seen_in": s["command"][:200],
                           "times": 0, "reply": reply, "next_commands": nxt}
            seen[u]["times"] += 1
        r = refusal_line(s["text"]) if "writ" in s["command"] else None
        if r:
            refusals.append({"at": s["at"], "kind": r[0], "line": r[1],
                             "command": s["command"][:200], "reply": reply, "next_commands": nxt})
    result = next((e for e in reversed(evs) if e.get("type") == "result"), {})
    final = result.get("result") or ""
    # What the agent told its user about each finding: final-report lines
    # naming the check (or its words) or an id quoted in the finding.
    for f in seen.values():
        check = re.search(r"(?:check: |\] )(\w+)", f["text"])
        tokens = {t for t in re.findall(r"'([^']{3,})'", f["text"])}
        if check:
            tokens |= {check.group(1), check.group(1).replace("_", " ")}
        f["final_mentions"] = [l.strip() for l in final.splitlines()
                               if any(t.lower() in l.lower() for t in tokens)][:4]
    writ_ids = sorted({m for s in results
                       for g in re.findall(r"claimed by: (\S+)|for agent '([^']+)'", s["text"])
                       for m in g if m})
    cmds = [c["command"] for c in calls]
    keys = ("doctor", "stale", "claim", "refus", "committed", "follow-up", "finding",
            "another agent", "other agent", "writ")
    hand = hand_writ_commands(cmds)
    sessions = sorted({e["session_id"] for e in evs if e.get("session_id")})
    main_ids = [writ_id_for_session(x) for x in sessions]
    subs = [analyze_subagent(t, main_ids) for t in subagent_transcripts(transcript)]
    # The -p stream also carries subagent events (parent_tool_use_id set);
    # with the subagents' own transcripts collected, count those there only.
    own = [e for e in evs if not e.get("parent_tool_use_id")] if subs else evs
    # The on-disk session transcript carries hook attachments the stream lacks.
    disk = transcript.parent / (transcript.stem + ".session.jsonl")
    ctx = hook_context_seen(evs + (events(disk) if disk.exists() else []), steps)
    return {
        "writ_ids": writ_ids,
        "session_writ_ids": [writ_id_for_session(x) for x in sessions],
        "edits": edits_of(own) + [e for sa in subs for e in sa["edits"]],
        "hook_context_seen": ctx,
        "subagents": subs,
        # Scope is in the key name (Aubs: a total read from the wrong scope
        # was published once). tools_total = own + subagents', each call once.
        "tools_total_main_and_subagents": dict(sum((collections.Counter(x) for x in
                           [tool_counts(own)] + [sa["tools_subagent_own"] for sa in subs]), collections.Counter())),
        "tools_main_own": tool_counts(own),
        "tools_stream_incl_subagents": tool_counts(evs),
        "bypass_reminder": (disk.exists() and BYPASS_REMINDER in disk.read_text(errors="replace"))
        or any(sa["bypass_reminder"] for sa in subs),
        "hand_writ_commands": hand,
        "hand_writ_by_subcommand": dict(collections.Counter(
            " ".join(WRIT_CMD.search(c).group(1).split()[:1]) for c in hand)),
        "spec_done_with_summary": [c for c in hand if re.search(r"\bwrit spec done\b.*(-s|--summary)\s", c)],
        "writ_calls": sum("writ" in c for c in cmds),
        "doctor_calls": sum("writ doctor" in c for c in cmds),
        "context_calls": sum("writ context" in c for c in cmds),
        "seal_calls": sum(bool(re.search(r"\bwrit seal\b", c)) for c in cmds),
        "findings_seen": list(seen.values()),
        "refusals": refusals,
        "forced": [c for c in cmds if re.search(r"\bwrit (seal|spec done|spec release|finish)\b.*--force", c)],
        "stale_releases": [c for c in cmds if re.search(r"\bwrit spec release\b.*--stale", c)],
        "allow_removals": [c for c in cmds if "--allow-removals" in c],
        "git_fallbacks": [c for c in cmds if re.search(r"\bgit (commit|add|push|stash|checkout|reset)\b", c)],
        "followup_pasted": [c for c in cmds if "--claim && writ seal" in c],
        "final_report_lines": [l.strip() for l in final.splitlines()
                               if l.strip() and any(k in l.lower() for k in keys)],
        "result": result.get("subtype"), "turns": result.get("num_turns"),
        "cost_usd": result.get("total_cost_usd"),
        "duration_s": round((result.get("duration_ms") or 0) / 1000),
        "limit_hit": "hit your session limit" in transcript.read_text(errors="replace")
        if transcript.exists() else False,
    }


def records_hurdles(seals: list, events_text: str, specs: list[str]) -> dict:
    """Records only (bench/writ-scorecard.sh --hurdles-records): seal fields
    forced/allow_removals and seal_refused/finish_refused events by spec."""
    c = collections.Counter()
    for s in seals:
        if s.get("spec_id") in specs:
            c["seals"] += 1
            c["forced"] += bool(s.get("forced"))
            c["allow_removals"] += bool(s.get("allow_removals"))
    for line in events_text.splitlines():
        try:
            e = json.loads(line)
        except ValueError:
            continue
        if e.get("event_type") in ("seal_refused", "finish_refused") and \
                set(e.get("specs") or []) & set(specs):
            c[e["event_type"]] += 1
    h = c["forced"] + c["allow_removals"] + c["seal_refused"] + c["finish_refused"]
    return {**{k: c[k] for k in ("seals", "forced", "allow_removals", "seal_refused",
                                 "finish_refused")},
            "hurdles": h, "per_10_seals": round(10 * h / c["seals"], 1) if c["seals"] else None}


def _ts(s: str | None) -> float | None:
    if not s:
        return None
    try:
        from datetime import datetime
        return datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()
    except ValueError:
        return None


def pct(xs: list[float], q: float) -> float | None:
    if not xs:
        return None
    xs = sorted(xs)
    return round(xs[min(len(xs) - 1, int(q * (len(xs) - 1) + 0.5))], 3)


def unsealed_window(edits: list[dict], seals: list, project: str | None) -> dict:
    """Edit-to-seal: for each edit, seconds until the first seal at or after
    it whose changes include the path (any spec: a seal of that content is
    what makes it recoverable). Edits no seal followed are counted apart."""
    root = (project or "").rstrip("/") + "/"
    by_path = collections.defaultdict(list)
    for x in seals:
        t = _ts(x.get("timestamp"))
        for c in x.get("changes") or []:
            if t is not None:
                by_path[c.get("path")].append(t)
    for v in by_path.values():
        v.sort()
    windows, never = [], []
    for e in edits:
        rel = e["path"][len(root):] if root != "/" and e["path"].startswith(root) else e["path"]
        t = _ts(e.get("at"))
        nxt = next((x for x in by_path.get(rel, []) if t is not None and x >= t - 0.001), None)
        if nxt is None:
            never.append(rel)
        else:
            windows.append(nxt - t)
    return {"edits": len(edits), "sealed": len(windows), "never_sealed": len(never),
            "never_sealed_paths": sorted(set(never)),
            "p50_s": pct(windows, 0.5), "max_s": round(max(windows), 3) if windows else None}


def _is_obj(line: str) -> bool:
    try:
        return isinstance(json.loads(line), dict)
    except ValueError:
        return False


def hooks_log_summary(text: str) -> dict:
    """`.writ/hooks.log`: JSON lines from 0.4.3 ({event, tool, path, agent,
    spec, outcome, edit_to_seal_ms, ms, ...}); any other line is counted as
    text, with errors and refusals found by word."""
    lines = [l for l in text.splitlines() if l.strip()]
    recs = []
    for l in lines:
        try:
            r = json.loads(l)
            if isinstance(r, dict):
                recs.append(r)
        except ValueError:
            pass
    # Parsed records count errors and refusals by their outcome field only (a
    # path such as tests/test_errors.py is not an error); unparseable lines by word.
    bad = [l for l in lines if not _is_obj(l)]
    out = {"lines": len(lines), "parsed": len(recs), "unparseable": len(lines) - len(recs),
           "errors": sum(str(r.get("outcome", "")).startswith(("error", "fail")) for r in recs)
           + sum(bool(re.search(r"\berror\b|\bfailed\b", l, re.I)) for l in bad),
           "refused": sum(str(r.get("outcome", "")).startswith("refus") for r in recs)
           + sum("refus" in l.lower() for l in bad)}
    if recs:
        out["by_event"] = dict(collections.Counter(r.get("event") for r in recs))
        out["by_outcome"] = dict(collections.Counter(r.get("outcome") for r in recs))
        out["agents"] = sorted({r.get("agent") for r in recs if r.get("agent")})
        hm = [float(r["ms"]) for r in recs if isinstance(r.get("ms"), (int, float))]
        es = [float(r["edit_to_seal_ms"]) for r in recs
              if isinstance(r.get("edit_to_seal_ms"), (int, float))]
        out["hook_ms"] = {"p50": pct(hm, 0.5), "max": max(hm) if hm else None}
        out["edit_to_seal_ms"] = {"p50": pct(es, 0.5), "max": max(es) if es else None}
    return out


def seal_sources(seals: list) -> dict:
    """Seals by identity and by what made them, from the summary prefix:
    "auto: Bash", "auto: Edit"/"auto: Write"/..., "auto: retry", "auto: Stop"
    and the like, else "hand". writ-bridge baselines are left out."""
    out: dict = {}
    for x in seals:
        who = (x.get("agent") or {}).get("id") or "?"
        if who == "writ-bridge":
            continue
        summ = x.get("summary") or ""
        src = " ".join(summ.split()[:2]).rstrip(":") if summ.startswith("auto:") else "hand"
        out.setdefault(who, collections.Counter())[src] += 1
    return {k: dict(v) for k, v in sorted(out.items())}


def _records(out: Path) -> list[dict]:
    p = out / "hooks.log"
    recs = []
    for l in (p.read_text(errors="replace").splitlines() if p.exists() else []):
        try:
            r = json.loads(l)
        except ValueError:
            continue
        if isinstance(r, dict):
            recs.append(r)
    return recs


def hook_window(recs: list[dict]) -> dict:
    """The headline window (Aubs): over hooks.log records whose outcome is
    "sealed", edit_to_seal_ms is mtime-based, exact unless coarse_mtime is
    set (then "at most"). Counted per record and per distinct file."""
    sealed = [r for r in recs if r.get("outcome") == "sealed"
              and isinstance(r.get("edit_to_seal_ms"), (int, float))]
    exact = [float(r["edit_to_seal_ms"]) for r in sealed if not r.get("coarse_mtime")]
    coarse = [float(r["edit_to_seal_ms"]) for r in sealed if r.get("coarse_mtime")]
    return {"sealed_records": len(sealed), "files": len({r.get("path") for r in sealed}),
            "exact_ms": {"p50": pct(exact, 0.5), "max": max(exact) if exact else None, "n": len(exact)},
            "at_most_ms_coarse": {"p50": pct(coarse, 0.5), "max": max(coarse) if coarse else None,
                                  "n": len(coarse)}}


def worst_window(recs: list[dict]) -> dict | None:
    """The worst sealed record, its hook kind and its cause (Aubs, ruling
    175): a drain ("retry") or Stop sweep means the edit waited in the queue;
    the hook records on that path since its previous seal say why (lock
    timeout, ambiguity); a Bash Post means the command ran that long."""
    sealed = [r for r in recs if r.get("outcome") == "sealed"
              and isinstance(r.get("edit_to_seal_ms"), (int, float))]
    if not sealed:
        return None
    w = max(sealed, key=lambda r: r["edit_to_seal_ms"])
    path, ts = w.get("path"), w.get("ts", "")
    prev_seal = max((r.get("ts", "") for r in sealed if r.get("path") == path and r.get("ts", "") < ts),
                    default="")
    between = [f"{r.get('outcome')} by {r.get('agent')} ({r.get('event')} {r.get('tool') or ''})".strip()
               for r in recs if r.get("ts", "") > prev_seal and r.get("ts", "") < ts
               and path in ((r.get("paths") or []) + [r.get("path")])]
    kind = w.get("tool") or w.get("event")
    if kind in ("retry", "Stop", "SubagentStop"):
        cause = "queued, then " + ("drained by the identity's next hook" if kind == "retry" else "swept at " + kind)
    elif kind == "Bash":
        cause = "Bash call: the write came early in a long command and the seal waited for its Post"
    else:
        cause = "edit hook: lock wait or seal cost"
    if between:
        cause += "; on this path before it: " + ", ".join(between)
    return {"ms": w["edit_to_seal_ms"], "path": path, "agent": w.get("agent"), "hook": kind,
            "seal": w.get("seal"), "cause": cause}


def ambiguity_rate(recs: list[dict]) -> dict:
    amb = [r for r in recs if r.get("outcome") == "ambiguous"]
    files = {p for r in amb for p in (r.get("paths") or [r.get("path")]) if p}
    sealed = [r for r in recs if r.get("outcome") == "sealed"]
    return {"ambiguous_records": len(amb), "ambiguous_files": len(files),
            "sealed_records": len(sealed),
            "identities": len({r.get("agent") for r in recs if r.get("agent")})}


def credit_moved(recs: list[dict], seals: list) -> list[str]:
    """Finding 169: a path the hook attributed (or found ambiguous between
    candidates) sealed by an identity outside that set."""
    who: dict = {}
    for r in recs:
        if r.get("outcome") in ("attributed", "sealed") and r.get("agent"):
            for p in (r.get("paths") or [r.get("path")]):
                if p:
                    who.setdefault(p, set()).add(r["agent"])
        if r.get("outcome") == "ambiguous":
            for p in (r.get("paths") or [r.get("path")]):
                if p:
                    who.setdefault(p, set()).update(r.get("agents") or [])
    out = []
    for x in seals:
        a = (x.get("agent") or {}).get("id")
        if not a or a == "writ-bridge":
            continue
        for c in x.get("changes") or []:
            p = c.get("path")
            if p in who and a not in who[p]:
                kids = sorted(c for c in who[p] if c.startswith(a + "-"))
                tag = f"; the sealer is the parent of candidate {', '.join(kids)}" if kids else ""
                out.append(f"{p} sealed by {a} (seal {x['id'][:12]}, spec {x.get('spec_id')}); "
                           f"hooks attributed it to {sorted(who[p])}{tag}")
    return out


def lost_lines(audit_text: str) -> int:
    return sum(int(m) for m in re.findall(r"LOST: (\d+)", audit_text))


def score(out: Path) -> dict:
    kit = load_kit(out)
    specs = json.loads((out / "specs.json").read_text()) if (out / "specs.json").exists() else {}
    seals = json.loads((out / "seals.json").read_text() or "[]") if (out / "seals.json").exists() else []
    events_text = (out / "events.jsonl").read_text() if (out / "events.jsonl").exists() else ""
    audit_text = (out / "audit.txt").read_text() if (out / "audit.txt").exists() else ""
    lost = lost_lines(audit_text)
    inj = kit.get("injection") or {}
    after = (kit.get("doctor_after") or {})
    fast_after = (after.get("fast") or {}).get("findings", [])
    surv_after = (after.get("survival") or {}).get("findings", [])
    agents = {}
    for a in AGENTS:
        info = analyze_agent(out / f"{a}.jsonl")
        ids = set(info["writ_ids"]) | set(info.get("session_writ_ids") or [])
        # Its subagents seal as "<its id>-<first 8 of agent_id>" (0.4.3).
        subs = {(x.get("agent") or {}).get("id") for x in seals}
        info["subagent_ids"] = sorted(i for i in subs if i and any(
            i.startswith(m + "-") for m in ids))
        ids |= set(info["subagent_ids"])
        info["writ_ids"] = sorted(set(info["writ_ids"]) | set(info.get("session_writ_ids") or []))
        # Its specs: claimed or created under one of its writ ids, or sealed
        # by one (a follow-up spec made after a committed-spec refusal
        # counts). The stale-claim ghost spec is never an agent's.
        sealed_under = {x.get("spec_id") for x in seals if (x.get("agent") or {}).get("id") in ids}
        own = sorted(sp for sp, v in specs.items() if sp != GHOST_SPEC and (
            v.get("claimed_by") in ids or v.get("created_by") in ids or sp in sealed_under))
        info["specs"] = own
        info["hurdles_records"] = records_hurdles(seals, events_text, own)
        own_stale = [f for f in fast_after if f["check"] == "stale_claim"
                     and any(f"'{s}'" in (f.get("message") or "") for s in own)]
        blocking = [f for f in fast_after + surv_after
                    if f not in own_stale and f["severity"] in ("red", "yellow")]
        done = any(specs[s].get("status") in ("complete", "committed") or specs[s].get("commit_hash")
                   for s in own)
        base_ok = (info["result"] == "success" and not info["forced"] and not info["git_fallbacks"]
                   and kit.get("finish_rc") == 0 and lost == 0 and done)
        info["own_stale_claims"] = [f["message"] for f in own_stale]
        info["window"] = unsealed_window(info["edits"], seals, kit.get("project"))
        info["zero_touch_strict"] = bool(base_ok and not blocking)
        info["zero_touch_lenient"] = bool(base_ok)
        agents[a] = info
    # Recovery from the injection, by whom and how.
    recovery = {}
    if kit.get("inject") == "stale-claim":
        ghost = specs.get(GHOST_SPEC, {})
        recovery = {"ghost_released": ghost.get("claimed_by") is None,
                    "released_by": [a for a, i in agents.items()
                                    if any(GHOST_SPEC in c for c in i["stale_releases"])],
                    "forced_release_by": [a for a, i in agents.items()
                                          if any(GHOST_SPEC in c for c in i["forced"])]}
    elif kit.get("inject") == "committed-spec":
        holder = inj.get("holder")
        who = [a for a, i in agents.items() if holder in i["writ_ids"]]
        later = [s for s in seals if s.get("agent", {}).get("id") == holder
                 and s.get("spec_id") != inj.get("spec")
                 and s.get("timestamp", "") > time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(inj.get("at", 0)))]
        recovery = {"committed_spec": inj.get("spec"), "agent": who,
                    "refusals_seen": [r for a in who for r in agents[a]["refusals"]
                                      if r["kind"] == "committed spec"],
                    "followup_pasted": [c for a in who for c in agents[a]["followup_pasted"]],
                    "sealed_after_on_other_spec": sorted({s.get("spec_id") for s in later})}
    summary = {
        "writ_version": kit.get("writ_version"), "writ_build": kit.get("writ_build"),
        "claude_version": kit.get("claude_version"),
        "repo": {"commits_at_setup": kit.get("git_commits_at_setup"),
                 "tracked_files_at_setup": kit.get("tracked_files_at_setup")},
        "inject": kit.get("inject", "none"), "injection": inj, "recovery": recovery,
        "doctor": {t: {tier: (kit.get(f"doctor_{t}") or {}).get(tier, {}).get("headline")
                       for tier in ("fast", "survival")} for t in ("before", "after", "final")},
        "doctor_after_findings": [f"{f['check']}/{f['severity']}: {f['message']}"
                                  for f in fast_after + surv_after],
        "finish_rc": kit.get("finish_rc"), "commits_since_setup": len(kit.get("commits_since_setup", [])),
        "lost_lines": lost, "integrity": kit.get("integrity"), "integrity_rc": kit.get("integrity_rc"),
        "agents": agents,
        "zero_touch_strict": f"{sum(i['zero_touch_strict'] for i in agents.values())}/{len(AGENTS)}",
        "zero_touch_lenient": f"{sum(i['zero_touch_lenient'] for i in agents.values())}/{len(AGENTS)}",
        "run_hurdles": {k: sum(len(i[k]) for i in agents.values())
                        for k in ("forced", "refusals", "git_fallbacks", "allow_removals")},
        "records_hurdles_all_agent_specs": records_hurdles(
            seals, events_text, sorted({s for i in agents.values() for s in i["specs"]})),
        "void": [a for a, i in agents.items() if i["limit_hit"] or i["result"] is None],
        "unsealed_window": unsealed_window([e for i in agents.values() for e in i["edits"]],
                                           seals, kit.get("project")),
        "hooks_log": hooks_log_summary((out / "hooks.log").read_text(errors="replace")
                                       if (out / "hooks.log").exists() else ""),
        "hand_writ_commands": {a: len(i["hand_writ_commands"]) for a, i in agents.items()},
        "seals_by_identity_and_source": seal_sources(seals),
        "window_mtime": hook_window(_records(out)),
        "window_worst": worst_window(_records(out)),
        "ambiguity": ambiguity_rate(_records(out)),
        "credit_moved": credit_moved(_records(out), seals),
        "receipt": (out / "receipt.txt").read_text() if (out / "receipt.txt").exists() else None,
        "tools_total": dict(sum((collections.Counter(i["tools_total_main_and_subagents"])
                                 for i in agents.values()), collections.Counter())),
    }
    # A writ that installs edit hooks (0.4.3+) logs every firing; agents that
    # edited with no line in hooks.log means the hooks never ran (wrong PATH,
    # broken install): the run measured nothing about auto seal. VOID.
    gen = out / "generated-.claude_settings.json"
    hooked = gen.exists() and "PostToolUse" in gen.read_text()
    n_edits = summary["unsealed_window"]["edits"]
    if hooked and n_edits and not summary["hooks_log"]["lines"]:
        summary["void"].append(f"hooks never fired ({n_edits} edits, empty hooks.log)")
    (out / "summary.json").write_text(json.dumps(summary, indent=2))
    (out / "report.md").write_text(report(summary, kit))
    return summary


def report(s: dict, kit: dict) -> str:
    b = s.get("writ_build") or {}
    L = [f"# Stranger run: {s['writ_version']}", "",
         f"Binary: `{b.get('path', '?')}` (sha256 {b.get('sha256', '?')}), {b.get('kind', 'unknown')}"
         + (f", built from {b['built_from']}" if b.get("built_from") else "") + ".", ""]
    L.append(f"Repo: {s['repo']['commits_at_setup']} commits, {s['repo']['tracked_files_at_setup']} "
             f"tracked files at setup. Injection: {s['inject']}. Claude: {s['claude_version']}.")
    if s["void"]:
        L.append(f"**VOID: {', '.join(s['void'])} (session limit, no result, or hooks never fired).**")
    w, amb = s["window_mtime"], s["ambiguity"]
    L += ["", "## Numbers", "",
          f"- **Window (headline): the seal followed the write by {w['exact_ms']['p50']} ms median, "
          f"{w['exact_ms']['max']} ms worst, over {w['sealed_records']} sealed records on {w['files']} "
          f"distinct files** (file mtime to seal; exact"
          + (f"; {w['at_most_ms_coarse']['n']} coarse-mtime records, at most p50 "
             f"{w['at_most_ms_coarse']['p50']} max {w['at_most_ms_coarse']['max']} ms" if w['at_most_ms_coarse']['n'] else "")
          + ")",
          (f"- Worst window record: {s['window_worst']['ms']} ms, {s['window_worst']['path']} by "
           f"{s['window_worst']['agent']}, hook {s['window_worst']['hook']}, seal {s['window_worst']['seal']}: "
           f"{s['window_worst']['cause']}" if s.get("window_worst") else "- Worst window record: none"),
          f"- Ambiguity (ruling 160): {amb['ambiguous_records']} ambiguous records over "
          f"{amb['ambiguous_files']} files against {amb['sealed_records']} sealed records, "
          f"{amb['identities']} identities",
          f"- Credit moved (169): {len(s['credit_moved'])}"
          + ("".join(f"\n  - {c}" for c in s['credit_moved'])),
          f"- Zero-touch safe finish: strict {s['zero_touch_strict']}, lenient {s['zero_touch_lenient']}",
          f"- Finish rc {s['finish_rc']}, {s['commits_since_setup']} commit(s), lost sealed lines {s['lost_lines']}",
          f"- Store integrity: {'; '.join(s['integrity'] or [])} (rc {s['integrity_rc']})",
          f"- Run hurdles: {s['run_hurdles']}",
          f"- Records hurdles (agent specs): {s['records_hurdles_all_agent_specs']}",
          f"- Unsealed window, edit-tool edits, at most (from the tool call's start to the first seal "
          f"holding the path): p50 {s['unsealed_window']['p50_s']} s, max {s['unsealed_window']['max_s']} s; "
          f"{s['unsealed_window']['sealed']}/{s['unsealed_window']['edits']} edits sealed, "
          f"{s['unsealed_window']['never_sealed']} never ({', '.join(s['unsealed_window']['never_sealed_paths']) or 'none'})",
          f"- Hook log: {s['hooks_log']['lines']} lines, {s['hooks_log'].get('parsed', 0)} parsed, "
          f"**{s['hooks_log'].get('unparseable', 0)} unparseable** (counted, not dropped); "
          f"{ {k: v for k, v in s['hooks_log'].items() if k not in ('lines', 'parsed', 'unparseable')} }"
          + (f"; Bash-edit window from hooks.log is at most (Pre snapshot to seal written)"
             if s['hooks_log'].get('by_event', {}).get('PreToolUse') else ""),
          f"- writ commands run by hand (counted, not scored): {s['hand_writ_commands']}",
          f"- Tool calls, all agents and subagents, each call once (the total to quote): {s['tools_total']}",
          "- Seals by identity and source (subagents seal as <main id>-<agent8>): "
          + "; ".join(f"{k} {v}" for k, v in s["seals_by_identity_and_source"].items()), ""]
    if s.get("receipt"):
        L += ["## Finish receipt (commit body)", "", "```", s["receipt"].rstrip(), "```", ""]
    L += [
          "## Doctor", "", "| when | fast | survival |", "|---|---|---|"]
    for t, v in s["doctor"].items():
        L.append(f"| {t} | {v['fast']} | {v['survival']} |")
    if s["doctor_after_findings"]:
        L += ["", "Findings when both agents had exited:", ""]
        L += [f"- `{f}`" for f in s["doctor_after_findings"]]
    if s["recovery"]:
        L += ["", "## Injection recovery", "", f"```\n{json.dumps(s['recovery'], indent=1)[:1200]}\n```"]
    for a, i in s["agents"].items():
        L += ["", f"## {a} ({', '.join(i['writ_ids']) or 'no writ id'})", "",
              f"Prompt: {kit.get('prompts', {}).get(a, '')}", "",
              f"{i['result']}, {i['turns']} turns, ${i['cost_usd']}, {i['duration_s']} s. "
              f"Specs {i['specs']}. writ calls {i['writ_calls']} (context {i['context_calls']}, "
              f"seal {i['seal_calls']}, doctor {i['doctor_calls']}). Zero-touch strict "
              f"{i['zero_touch_strict']}, lenient {i['zero_touch_lenient']}. Forced {len(i['forced'])}, "
              f"--stale {len(i['stale_releases'])}, git fallbacks {len(i['git_fallbacks'])}, "
              f"own stale claims {len(i['own_stale_claims'])}.", ""]
        for f in i["findings_seen"]:
            L += [f"- Saw ({f['times']}x, first via `{f['seen_in'][:80]}`): `{f['text']}`",
                  f"  - Wrote back: \"{f['reply']}\"",
                  f"  - Then ran: {', '.join('`' + c[:100] + '`' for c in f['next_commands']) or 'nothing'}"]
            L += [f"  - Told its user: \"{m}\"" for m in f.get("final_mentions", [])]
        for h in i["hook_context_seen"]:
            L += [f"- Hook told it ({h['times']}x, {h['event']}"
                  + (", in a subagent" if h["subagent"] else "") + f"): `{h['line']}`",
                  f"  - Wrote back: \"{h['reply']}\""]
        L.append(f"- Tool calls, this main plus its {len(i.get('subagents', []))} subagent(s), each call once: "
                 f"{i.get('tools_total_main_and_subagents')}"
                 + ("; Claude Code told it to change files through Bash (bypass permissions mode)"
                    if i.get("bypass_reminder") else ""))
        if i["hand_writ_commands"]:
            L.append(f"- Ran by hand: {i['hand_writ_by_subcommand']}; spec done with its own "
                     f"summary: {len(i['spec_done_with_summary'])}")
        for r in i["refusals"]:
            L += [f"- Refused ({r['kind']}) on `{r['command'][:100]}`: `{r['line']}`",
                  f"  - Wrote back: \"{r['reply']}\""]
        if i["final_report_lines"]:
            L += ["", "Final report, lines about writ:", ""]
            L += [f"> {l}" for l in i["final_report_lines"]]
    return "\n".join(L) + "\n"


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("setup"); s.add_argument("project", type=Path); s.add_argument("out", type=Path)
    s.add_argument("--writ"); s.add_argument("--skip-init", action="store_true")
    s.add_argument("--built-from", help="for a dev build: the seal or commit it was built from")
    l = sub.add_parser("launch"); l.add_argument("project", type=Path); l.add_argument("out", type=Path)
    l.add_argument("--prompt", action="append", type=Path, required=True)
    l.add_argument("--inject", choices=("none", "stale-claim", "committed-spec"), default="none")
    l.add_argument("--inject-after", type=int, default=120)
    l.add_argument("--timeout", type=int, default=1800)
    c = sub.add_parser("collect"); c.add_argument("project", type=Path); c.add_argument("out", type=Path)
    c.add_argument("--no-finish", action="store_true")
    r = sub.add_parser("score"); r.add_argument("out", type=Path)
    a = ap.parse_args()
    if a.cmd == "setup":
        cmd_setup(a.project, a.out, a.writ, a.skip_init, a.built_from)
    elif a.cmd == "launch":
        cmd_launch(a.project, a.out, a.prompt, a.inject, a.inject_after, a.timeout)
    elif a.cmd == "collect":
        cmd_collect(a.project, a.out, a.no_finish)
    else:
        s = score(a.out)
        print(f"zero-touch strict {s['zero_touch_strict']}, lenient {s['zero_touch_lenient']}, "
              f"lost {s['lost_lines']}; wrote {a.out / 'summary.json'} and {a.out / 'report.md'}")


if __name__ == "__main__":
    main()
