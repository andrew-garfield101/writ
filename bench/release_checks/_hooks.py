"""Shared helpers for the release checks that drive writ's Claude Code hooks by hand.

Imported by hook_failure.sh, named_subagent.sh and command_text.sh (sprint 7). Each
hook command is read from the project's .claude/settings.json and run with `sh -c`
in the project root, stdin = a hook JSON, exactly as Claude Code runs it. Identity
comes from the framework (CLAUDECODE=1, CLAUDE_CODE_SESSION_ID) unless an explicit
writ agent id is passed, which stands in for two separate main agents.
"""
from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

WRIT = os.environ["WRIT"]
FIX = Path(os.environ["FIX"])
D = Path(os.environ["D"])
BIN = D / "bin"
HOME = D / "home"
BIN.mkdir(exist_ok=True)
HOME.mkdir(exist_ok=True)
if not (BIN / "writ").exists():
    (BIN / "writ").symlink_to(WRIT)
_fail = 0


def chk(name: str, ok: bool, detail: str = "") -> None:
    """Print one PASS or FAIL line; remember a failure for done()."""
    global _fail
    print(("PASS " if ok else "FAIL ") + name + ("" if ok or not detail else f"  [{detail}]"))
    if not ok:
        _fail = 1


def done(total: int) -> None:
    """Exit 0 only when every check passed."""
    sys.exit(_fail)


def env(session: str | None = None, agent: str | None = None) -> dict[str, str]:
    e = {"PATH": f"{BIN}:/usr/bin:/bin:/usr/sbin:/sbin", "HOME": str(HOME),
         "TMPDIR": os.environ.get("TMPDIR", "/tmp"), "GIT_CONFIG_NOSYSTEM": "1",
         "WRIT_EXCLUDES_FILE": str(HOME / "none"),
         "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@localhost",
         "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@localhost"}
    if agent:
        e["WRIT_AGENT_ID"] = agent
    else:
        e["CLAUDECODE"] = "1"
    if session:
        e["CLAUDE_CODE_SESSION_ID"] = session
    return e


def run(root: Path, args: list[str], session=None, agent="human", stdin=None):
    return subprocess.run(args, cwd=root, env=env(session, agent), input=stdin,
                          capture_output=True, text=True)


def writ(root: Path, *args: str, session=None, agent="human"):
    return run(root, ["writ", *args], session, agent)


def project(name: str) -> Path:
    """A git repo with a few files, `writ init -y` run and committed."""
    root = D / name
    root.mkdir()
    run(root, ["git", "init", "-q", "-b", "main"])
    for f, n in (("a.txt", 20), ("b.txt", 20), ("shared.txt", 40)):
        (root / f).write_text("".join(f"line {i}\n" for i in range(1, n + 1)))
    run(root, ["git", "add", "-A"])
    run(root, ["git", "commit", "-qm", "base"])
    r = writ(root, "init", "-y")
    assert r.returncode == 0, r.stdout + r.stderr
    run(root, ["git", "add", "-A"])
    run(root, ["git", "commit", "-qm", "init", "--no-verify"])
    return root


def settings(root: Path) -> dict:
    try:
        return json.loads((root / ".claude/settings.json").read_text())
    except (OSError, ValueError):
        return {}


def covers(matcher: str | None, tool: str) -> bool:
    m = (matcher or "").strip()
    if m in ("", "*", ".*"):
        return True
    try:
        return re.fullmatch(m, tool) is not None
    except re.error:
        return tool in m.strip("^$()").split("|")


def commands(root: Path, event: str, tool: str | None = None) -> list[str]:
    out = []
    for e in settings(root).get("hooks", {}).get(event, []) or []:
        if tool and not covers(e.get("matcher"), tool):
            continue
        out += [h["command"] for h in e.get("hooks", []) if "writ" in h.get("command", "")]
    return out


def fire(root: Path, event: str, stdin: dict, tool=None, session=None, agent=None):
    """Run every writ hook registered for event (and tool) with stdin."""
    res = []
    for c in commands(root, event, tool):
        r = subprocess.run(["sh", "-c", c], cwd=root, env=env(session, agent),
                           capture_output=True, text=True, input=json.dumps(stdin))
        res.append(r)
    return res


def fixture(name: str, root: Path, session: str) -> dict:
    v = json.loads((FIX / name).read_text())
    v["session_id"] = session
    v["cwd"] = str(root)
    v["transcript_path"] = f"/tmp/fixture/{session}.jsonl"
    return v


def bash_stdin(root: Path, session: str, event: str, cmd: str, tid: str, extra=None) -> dict:
    v = {"session_id": session, "transcript_path": f"/tmp/fixture/{session}.jsonl",
         "cwd": str(root), "hook_event_name": event, "permission_mode": "bypassPermissions",
         "tool_name": "Bash", "tool_input": {"command": cmd, "description": "run"},
         "tool_use_id": tid}
    if event == "PostToolUse":
        v["tool_response"] = {"stdout": "", "stderr": "", "interrupted": False}
    v.update(extra or {})
    return v


def sh_run(root: Path, cmd: str) -> int:
    return subprocess.run(["sh", "-c", cmd], cwd=root, env=env(), capture_output=True).returncode


def tick() -> None:
    time.sleep(0.03)


def seals(root: Path) -> list[dict]:
    r = writ(root, "log", "--all", "--format", "json", "--limit", "1000")
    try:
        return json.loads(r.stdout)
    except ValueError:
        return []


def newest(root: Path) -> dict:
    return (seals(root) or [{}])[0]


def paths_of(seal: dict) -> list[str]:
    return sorted(c.get("path") for c in seal.get("changes", []))


def agent_of(seal: dict) -> str | None:
    return (seal.get("agent") or {}).get("id")


def add_spec(root: Path, sid: str, agent: str) -> None:
    writ(root, "spec", "add", "--id", sid, "--title", f"spec {sid}", agent=agent)
    writ(root, "spec", "claim", sid, "--agent", agent, agent=agent)


def hook_records(root: Path) -> list[dict]:
    p = root / ".writ/hooks.log"
    out = []
    for line in (p.read_text().splitlines() if p.exists() else []):
        try:
            out.append(json.loads(line))
        except ValueError:
            pass
    return out


def told(res) -> list[str]:
    """The lines the hook handed the agent (additionalContext or exit 2 stderr)."""
    out = []
    for r in res:
        if r.returncode == 0 and r.stdout.strip():
            try:
                out.append(json.loads(r.stdout)["hookSpecificOutput"]["additionalContext"].strip())
            except (ValueError, KeyError, TypeError):
                out.append("<not hook JSON> " + r.stdout.strip())
        elif r.returncode == 2:
            out.append(r.stderr.strip())
    return out


def zero(res) -> bool:
    return bool(res) and all(r.returncode == 0 for r in res)


def show(res) -> str:
    return "; ".join(f"rc={r.returncode} out={r.stdout.strip()[:120]!r} err={r.stderr.strip()[:120]!r}"
                     for r in res) or "no writ hook ran"
