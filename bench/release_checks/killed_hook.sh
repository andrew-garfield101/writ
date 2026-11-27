#!/usr/bin/env bash
# Claim (0.4.6 note, Added; finding 213):
#   "`writ doctor` reports a hook that was stopped before it finished. ... A run
#    since the last finish with a start and no end, whose process is gone, is now
#    a yellow finding: "hook killed before it finished: <tool> for <identity> at
#    <time>". Its fix is the seal that never happened,
#    `writ seal --paths <paths> --auto --agent <identity>`, for that identity only."
#
# A project set up by `writ init -y` with one seal gets, dated after that setup
# (records before the last finish or import are not reported), a hand written
# .writ/hooks.log in the record format the hook writes. Shape 1 (killed): a
# PostToolUse firing of a dead pid logs `start` and `attributed` for a.txt and
# never `end`; the same pid's next firing (a Stop) starts and ends. Shape 2
# (running): the last firing of this script's own live pid has a start and no
# end, so the hook may still be running and doctor must say nothing. Records are
# compact JSON, as the hook writes them. 0.4.5 has no
# hook_killed check: checks 1 to 5 fail there.
#
# Usage: killed_hook.sh [WRIT_BIN] [SCRATCH_DIR]
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/_lib.sh"
ROOT="$(cd "$HERE/../.." && pwd)"
WRIT="$(rc_bin "${1:-$ROOT/target/release/writ}")"
unset WRIT_AGENT_ID CLAUDECODE CLAUDE_CODE_SESSION_ID WRIT_FORMAT
rc_setup .killed_hook killed-hook "${2:-}"
echo "binary: $WRIT ($("$WRIT" --version))"

WRIT="$WRIT" D="$D" python3 - <<'PY'
import datetime as dt, json, os, subprocess, sys, time
from pathlib import Path

WRIT, D = os.environ["WRIT"], Path(os.environ["D"])
DEAD = 4_000_001          # above every platform's pid_max: no process holds it
fails = 0

def chk(name, ok, detail=""):
    global fails
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"  [{detail}]"))
    fails += 0 if ok else 1

def run(root, *args):
    return subprocess.run(args, cwd=root, capture_output=True, text=True,
                          stdin=subprocess.DEVNULL)

def project(name):
    root = D / name
    root.mkdir()
    run(root, "git", "init", "-q", "-b", "main")
    (root / "app.py").write_text('print("hi")\n')
    run(root, "git", "add", "-A")
    run(root, "git", "-c", "user.name=t", "-c", "user.email=t@localhost", "commit", "-qm", "base")
    run(root, WRIT, "init", "-y")
    (root / "a.txt").write_text("a\n")
    run(root, WRIT, "seal", "-s", "one", "--paths", "a.txt")
    time.sleep(1.5)   # the records below are dated after setup (the import counts as a finish)
    return root

def ts(ago):
    t = dt.datetime.now(dt.timezone.utc) - dt.timedelta(seconds=ago)
    return t.isoformat(timespec="milliseconds").replace("+00:00", "Z")

def compact(d):
    # the hook writes compact JSON, and doctor matches `"outcome":"start"` as written
    return json.dumps(d, separators=(",", ":"))

def start(pid, event, tool, at):
    return compact({"event": event, "outcome": "start", "session": "s-1", "agent_id": "a-1",
                       "agent_type": "", "tool": tool, "tool_use_id": "t", "ts": at, "ms": 0, "pid": pid})

def rec(pid, event, outcome, at, **extra):
    return compact({"event": event, "outcome": outcome, **extra, "ts": at, "ms": 5, "pid": pid})

def shape(root, pid):
    lines = [
        start(pid, "PostToolUse", "Bash", ts(1.0)),
        rec(pid, "PostToolUse", "attributed", ts(0.9), path="a.txt", agent="web-builder"),
        start(pid, "Stop", "", ts(0.5)),
        rec(pid, "Stop", "end", ts(0.4)),
    ]
    log = root / ".writ" / "hooks.log"
    log.write_text("\n".join(lines) + "\n")
    return log

def doctor(root):
    p = run(root, WRIT, "doctor", "--format", "json")
    try:
        return p.returncode, json.loads(p.stdout)
    except json.JSONDecodeError:
        return p.returncode, {"findings": [], "error": p.stdout[:300] + p.stderr[:300]}

def killed(v):
    return [f for f in v.get("findings", []) if f.get("check") == "hook_killed"]

# Shape 1: a dead pid's firing with a start and no end.
r1 = project("killed")
log = shape(r1, DEAD)
before = log.read_bytes()
rc, v = doctor(r1)
k = killed(v)
chk("1 one hook_killed finding, yellow, for a dead pid's start with no end",
    len(k) == 1 and k[0].get("severity") == "yellow", json.dumps(v)[:400])
msg = k[0].get("message", "") if k else ""
chk("2 message says 'hook killed before it finished' and names the tool and the identity",
    all(w in msg for w in ("hook killed before it finished", "Bash", "web-builder")), msg)
fix = k[0].get("fix_command") if k else None
chk("3 fix is the missing seal for that identity: writ seal --paths a.txt --auto --agent web-builder",
    fix == "writ seal --paths a.txt --auto --agent web-builder", repr(fix))
chk("4 the fix is for that identity only (anyone_may_fix false, needs_human false)",
    bool(k) and k[0].get("anyone_may_fix") is False and k[0].get("needs_human") is False,
    json.dumps(k[0] if k else {})[:300])
chk("5 doctor exits 1 on the finding", rc == 1, f"rc {rc}")
chk("6 doctor leaves hooks.log as it was", log.read_bytes() == before)

# Shape 2: a live pid's (this process's) last firing has a start and no end: still running.
r2 = project("running")
me = os.getpid()
(r2 / ".writ" / "hooks.log").write_text("\n".join([
    start(DEAD, "Stop", "", ts(1.0)),
    rec(DEAD, "Stop", "end", ts(0.9)),
    start(me, "PostToolUse", "Bash", ts(0.5)),
    rec(me, "PostToolUse", "attributed", ts(0.4), path="a.txt", agent="web-builder"),
]) + "\n")
rc2, v2 = doctor(r2)
chk("7 a live pid's start with no end is not reported", not killed(v2),
    json.dumps(killed(v2))[:300])

sys.exit(1 if fails else 0)
PY
