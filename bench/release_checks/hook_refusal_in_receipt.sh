#!/usr/bin/env bash
# Claims (0.4.5 note, findings 203 and 204):
#   "A refusal the hook makes (an edit that drops another agent's sealed lines) is recorded
#    like a hand seal's: a seal_refused event with the identity, the path and the other
#    agent, and the receipt's Refusals in range lists it, so an agent whose only seal was
#    refused still appears in the receipt."
#   "When two instances of one subagent type are live, the refusal names the instance,
#    `<type>-<first 8 of its id>`, not the type."
#
# Part A: two main agents (writ agent ids alpha and bravo standing in for two sessions);
# bravo writes shared.txt from a stale read that drops alpha's sealed line. Part B: two
# live `reviewer` instances (.claude/agents/reviewer.md), the second does the same.
# Mirrors Bri's f203_* / f204_* assertions.
#
# Usage: hook_refusal_in_receipt.sh [WRIT_BIN] [SCRATCH_DIR]
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/_lib.sh"
ROOT="$(cd "$HERE/../.." && pwd)"
WRIT="$(rc_bin "${1:-$ROOT/target/release/writ}")"
FIX="$ROOT/crates/writ-cli/tests/fixtures/claude_hooks"
unset WRIT_AGENT_ID
rc_setup .hook_refusal hook-refusal "${2:-}"
echo "binary: $WRIT ($("$WRIT" --version))"

HERE="$HERE" WRIT="$WRIT" FIX="$FIX" D="$D" python3 - <<'PY'
import json, os, sys
sys.path.insert(0, os.environ["HERE"])
from _hooks import *

LINES = [f"line {i}\n" for i in range(1, 41)]

def write_hook(root, rel, text, session, agent=None, sub=None):
    (root / rel).write_text(text)
    name = "post_tool_use_edit_subagent.json" if sub else "post_tool_use_write_main.json"
    v = fixture(name, root, session)
    v["tool_input"]["file_path"] = str(root / rel)
    if isinstance(v.get("tool_response"), dict):
        v["tool_response"]["filePath"] = str(root / rel)
    if sub:
        v["agent_id"], v["agent_type"] = sub
    tool = "Edit" if sub else "Write"
    return fire(root, "PostToolUse", v, tool, session, agent)

def events(root):
    p = root / ".writ/security/events.jsonl"
    out = []
    for l in (p.read_text().splitlines() if p.exists() else []):
        try:
            out.append(json.loads(l))
        except ValueError:
            pass
    return out

def refused_events(root, who):
    return [e for e in events(root) if e.get("event_type") == "seal_refused" and e.get("agent_id") == who]

def receipt(root, since, fmt="receipt"):
    return writ(root, "handoff", "--since", since, "--format", fmt).stdout

def body_refusals(text):
    """The `- ` lines under a Refusals in range heading."""
    out, on = [], False
    for l in text.splitlines():
        if "Refusals in range" in l:
            on = True; continue
        if on and l.startswith("- "):
            out.append(l)
        elif on and l.strip() and not l.startswith("- "):
            on = False
    return out

def close_all(root):
    """Close every open spec as its claimant, so finish commits."""
    try:
        st = json.loads(writ(root, "spec", "status", "--format", "json").stdout)
    except ValueError:
        st = []
    for x in st:
        who = x.get("claimed_by")
        if who and x.get("status") not in ("complete", "committed"):
            writ(root, "spec", "done", x["id"], "-s", f"{who} done", "--agent", who, agent=who)

# --- A: two main agents -----------------------------------------------------------------
p = project("main")
run(p, ["git", "config", "user.name", "fixture"]); run(p, ["git", "config", "user.email", "fixture@example.invalid"])
since = newest(p)["id"]
a = LINES.copy(); a[9] = "line 10 by alpha\n"
write_hook(p, "shared.txt", "".join(a), "sess-a", "alpha")
chk("1 fixture: alpha's hook seal of shared.txt", agent_of(newest(p)) == "alpha", agent_of(newest(p)))
n0 = len(seals(p))
b = LINES.copy(); b[29] = "line 30 by bravo\n"          # bravo's stale read: alpha's line 10 is gone
res = write_hook(p, "shared.txt", "".join(b), "sess-b", "bravo")
msg = " ".join(told(res))
chk("2 bravo's write is refused by the hook: nothing sealed, one line naming shared.txt and agent alpha",
    len(seals(p)) == n0 and all(r.returncode in (0, 2) for r in res) and "shared.txt" in msg and "alpha" in msg,
    f"{msg[:200]} {show(res)}")
ev = refused_events(p, "bravo")
chk("3 security/events.jsonl has a seal_refused event for bravo naming shared.txt and alpha",
    len(ev) == 1 and "shared.txt" in ev[0].get("files", []) and "alpha" in json.dumps(ev[0]),
    json.dumps(ev)[:300] or "no seal_refused event for bravo")
rec = receipt(p, since)
line = next((l for l in rec.splitlines() if "refused" in l and "bravo" in l), "")
chk("4 the receipt's Refusals in range lists it: `seal refused ... for bravo`, files shared.txt",
    "Refusals in range" in rec and "shared.txt" in line, line or rec[-400:])
chk("5 bravo has no seal in range and still appears in the receipt (finding 203)",
    not any(agent_of(s) == "bravo" for s in seals(p)) and "bravo" in rec, rec[-300:])
pr = receipt(p, since, "pr")
chk("6 the PR form lists it too", "Refusals in range" in pr and any("bravo" in l and "shared.txt" in l for l in pr.splitlines()),
    pr[-300:])
# finish: put alpha's sealed version back (bravo's refused write is on disk), close the open spec
(p / "shared.txt").write_text("".join(a))
close_all(p)
fin = writ(p, "finish", "-y")
body = run(p, ["git", "log", "-1", "--format=%B"]).stdout
r = writ(p, "handoff", "--commit", "HEAD", "--format", "json")
try:
    j = json.loads(r.stdout)        # a list: one receipt per finish commit
    refs = [x for rc in (j if isinstance(j, list) else [j]) for x in rc.get("refusals", []) if x.get("agent") == "bravo"]
except (ValueError, AttributeError):
    refs = []
chk("7 after `writ finish`, the commit body and `handoff --commit HEAD --format json` both carry bravo's refusal",
    fin.returncode == 0 and any("bravo" in l and "shared.txt" in l for l in body_refusals(body)) and len(refs) >= 1,
    f"finish rc={fin.returncode} body={body[-250:]!r} json={r.stdout[:200]!r}")

# --- B: two live instances of one named type (204, Bri's f204 shape) ----------------------
S = "66666666-6666-4666-8666-666666666666"
X, Y = "a1b2c3d4e5f6a7b8c", "a9e8d7c6b5a4f3e2d"
SUF = f"reviewer-{Y[:8]}"

def named_project(name):
    q = project(name)
    d = q / ".claude/agents"; d.mkdir(parents=True, exist_ok=True)
    (d / "reviewer.md").write_text("---\nname: reviewer\ndescription: test agent\n---\nYou are reviewer.\n")
    run(q, ["git", "add", "-A"]); run(q, ["git", "commit", "-qm", "agent file", "--no-verify"])
    run(q, ["git", "config", "user.name", "fixture"]); run(q, ["git", "config", "user.email", "fixture@example.invalid"])
    return q

# (a) the plain instance seals first; the suffixed one's stale write is refused
q = named_project("named")
since = newest(q)["id"]
write_hook(q, "b.txt", "first instance\n", S, sub=(X, "reviewer"))
write_hook(q, "a.txt", "second instance\n", S, sub=(Y, "reviewer"))
a = LINES.copy(); a[29] = "line 30 by reviewer one\n"
write_hook(q, "shared.txt", "".join(a), S, sub=(X, "reviewer"))
chk("8 fixture: the first instance seals as `reviewer`, the second as `reviewer-a9e8d7c6`",
    any(agent_of(x) == SUF for x in seals(q)) and agent_of(newest(q)) == "reviewer", agent_of(newest(q)))
b = LINES.copy(); b[9] = "line 10 by reviewer two\n"
write_hook(q, "shared.txt", "".join(b), S, sub=(Y, "reviewer"))
ev = refused_events(q, SUF)
e = ev[0] if len(ev) == 1 else {}
other = [k for k, v in e.items() if k not in ("details", "agent_id") and "reviewer" in json.dumps(v)
         and SUF not in json.dumps(v)]
chk("9 its seal_refused event: agent_id `reviewer-a9e8d7c6`, the other agent `reviewer` in a field of its own, "
    "files shared.txt, lines recorded (finding 204)",
    bool(e) and bool(other) and "shared.txt" in e.get("files", []) and e.get("lines") not in (None, [], ""),
    json.dumps(ev)[:400] or "no seal_refused event for the suffixed instance")
rec = receipt(q, since)
chk("10 the receipt lists it: a line under Refusals in range with `reviewer-a9e8d7c6` and shared.txt",
    any(SUF in l and "shared.txt" in l for l in body_refusals(rec)), rec[-400:])
# shared.txt carries the refused write on disk; put the sealed version back so finish can commit
(q / "shared.txt").write_text("".join(a))
close_all(q)
fin = writ(q, "finish", "-y")
body = run(q, ["git", "log", "-1", "--format=%B"]).stdout
chk("11 after `writ finish`, the commit body's Refusals in range names `reviewer-a9e8d7c6` and shared.txt",
    fin.returncode == 0 and any(SUF in l and "shared.txt" in l for l in body_refusals(body)),
    f"finish rc={fin.returncode} {(fin.stdout + fin.stderr)[-200:]!r} body={body[-300:]!r}")

# (b) reverse order: the suffixed instance sealed, the plain one is refused and told its name
w = named_project("reverse")
write_hook(w, "b.txt", "first instance\n", S, sub=(X, "reviewer"))
a = LINES.copy(); a[29] = "line 30 by reviewer two\n"
write_hook(w, "shared.txt", "".join(a), S, sub=(Y, "reviewer"))
chk("12 fixture: shared.txt sealed by `reviewer-a9e8d7c6`", agent_of(newest(w)) == SUF, agent_of(newest(w)))
b = LINES.copy(); b[9] = "line 10 by reviewer one\n"
res = write_hook(w, "shared.txt", "".join(b), S, sub=(X, "reviewer"))
msg = " ".join(told(res))
chk("13 the line told to the plain instance names the other as `reviewer-a9e8d7c6`, not the type",
    f"agent {SUF}" in msg, msg[:300] or show(res))
done(13)
PY
rc=$?
echo "hook_refusal_in_receipt: exit $rc"
exit $rc
