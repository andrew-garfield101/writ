#!/usr/bin/env bash
# Claims (0.4.5 note, finding 202, ruling 202):
#   "A writ command a named subagent types in its shell runs as that subagent: writ finds
#    the open hook window whose command text contains the invocation. `writ spec done -s`
#    typed by `reviewer` closes reviewer's spec with its words."
#   "When no open window matches, or more than one does, the command runs as the parent,
#    as before, and says which identity it ran as" (on stderr, and only when another
#    identity in the session has a window or is a live named subagent: Amis, ee535f9e8824).
#
# The subagent's shell has the parent's environment (CLAUDECODE=1, the session id, no
# agent id): that is the 0.4.4 failure, so every command here runs with exactly that.
# The Bash PreToolUse is fed as Claude Code sends it for a subagent (agent_id,
# agent_type), the command runs, then PostToolUse.
#
# Usage: subagent_hand_command.sh [WRIT_BIN] [SCRATCH_DIR]
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/_lib.sh"
ROOT="$(cd "$HERE/../.." && pwd)"
WRIT="$(rc_bin "${1:-$ROOT/target/release/writ}")"
FIX="$ROOT/crates/writ-cli/tests/fixtures/claude_hooks"
unset WRIT_AGENT_ID
rc_setup .subagent_hand subagent-hand "${2:-}"
echo "binary: $WRIT ($("$WRIT" --version))"

HERE="$HERE" WRIT="$WRIT" FIX="$FIX" D="$D" python3 - <<'PY'
import json, os, subprocess, sys
sys.path.insert(0, os.environ["HERE"])
from _hooks import *

S = "77777777-7777-4777-8777-777777777777"
X, Y = "a1b2c3d4e5f6a7b8c", "a9e8d7c6b5a4f3e2d"

def named(root, *names):
    d = root / ".claude/agents"; d.mkdir(parents=True, exist_ok=True)
    for n in names:
        (d / f"{n}.md").write_text(f"---\nname: {n}\ndescription: test agent\n---\nYou are {n}.\n")
    run(root, ["git", "add", "-A"]); run(root, ["git", "commit", "-qm", "agent files", "--no-verify"])

def sub_edit(root, sub, rel, text):
    (root / rel).write_text(text)
    v = fixture("post_tool_use_edit_subagent.json", root, S)
    v["agent_id"], v["agent_type"] = sub
    v["tool_input"]["file_path"] = str(root / rel)
    v["tool_response"]["filePath"] = str(root / rel)
    return fire(root, "PostToolUse", v, "Edit", S)

def parent_edit(root, rel, text):
    (root / rel).write_text(text)
    v = fixture("post_tool_use_edit_main.json", root, S)
    v["tool_input"]["file_path"] = str(root / rel)
    if isinstance(v.get("tool_response"), dict):
        v["tool_response"]["filePath"] = str(root / rel)
    return fire(root, "PostToolUse", v, "Edit", S)

def open_window(root, cmd, tid, sub=None):
    extra = {"agent_id": sub[0], "agent_type": sub[1]} if sub else None
    return fire(root, "PreToolUse", bash_stdin(root, S, "PreToolUse", cmd, tid, extra), "Bash", S)

def close_window(root, cmd, tid, sub=None):
    extra = {"agent_id": sub[0], "agent_type": sub[1]} if sub else None
    return fire(root, "PostToolUse", bash_stdin(root, S, "PostToolUse", cmd, tid, extra), "Bash", S)

def typed(root, cmd):
    """Run cmd in a shell with the parent's environment, as a subagent's Bash does."""
    return subprocess.run(["sh", "-c", cmd], cwd=root, env=env(S), capture_output=True, text=True)

def spec(root, sid):
    r = writ(root, "spec", "show", sid, "--format", "json")
    try:
        return json.loads(r.stdout)
    except ValueError:
        return {}

def done_with(root, sid, words):
    s = spec(root, sid)
    return str(s.get("status", "")).lower() in ("done", "complete", "completed") and words in json.dumps(s)

def specs_of(root, who):
    try:
        st = json.loads(writ(root, "spec", "status", "--format", "json").stdout)
    except ValueError:
        return []
    return [s["id"] for s in st if who in json.dumps(s.get("agent") or s.get("claimed_by") or "")]

# --- one match: the subagent's own words land on its spec -------------------------------
p = project("one")
named(p, "reviewer")
add_spec(p, "review", "reviewer")
sub_edit(p, (X, "reviewer"), "b.txt", "reviewed\n")
parent_edit(p, "a.txt", "parent work\n")
parent = agent_of(newest(p))
CMD = 'writ spec done -s "reviewer checked b.txt"'
open_window(p, CMD, "t1", (X, "reviewer"))
r = typed(p, CMD)
close_window(p, CMD, "t1", (X, "reviewer"))
out = r.stdout + r.stderr
chk("1 `writ spec done -s` typed in reviewer's shell (parent environment) exits 0",
    r.returncode == 0, out[-300:])
chk("2 it closed reviewer's spec `review` with reviewer's words", done_with(p, "review", "reviewer checked b.txt"),
    json.dumps(spec(p, "review"))[:300])
chk("3 it says which identity it ran as, `reviewer`", "reviewer" in out and "ran as" in out.lower(), out[-300:])
chk("4 the parent's own work is untouched: its spec is still open",
    bool(parent) and any(not done_with(p, sid, "reviewer checked") for sid in specs_of(p, parent)), f"parent={parent}")
rec = writ(p, "handoff", "--format", "receipt").stdout
chk("5 the receipt carries reviewer's words on its own spec line (`- review (reviewer...`)",
    any(l.startswith("- review ") and "reviewer checked b.txt" in l for l in rec.splitlines()), rec[-400:])

# --- no window: the parent, and it says so when another identity is live ---------------
q = project("none")
named(q, "reviewer")
parent_edit(q, "a.txt", "parent work\n")
parent = agent_of(newest(q))
sub_edit(q, (X, "reviewer"), "b.txt", "live reviewer\n")          # reviewer is live, no open window
r = typed(q, 'writ spec done -s "parent closes"')
chk("6 a live `reviewer` with no open window running it: the command runs as the parent and says so on stderr",
    bool(parent) and f"ran as {parent} (no other identity" in r.stderr, r.stderr[-300:])
q2 = project("alone")
parent_edit(q2, "a.txt", "parent work\n")
r = typed(q2, 'writ spec done -s "parent alone"')
chk("6b nobody else in the session: the command runs as the parent and prints no identity line",
    r.returncode == 0 and "ran as" not in (r.stdout + r.stderr), (r.stdout + r.stderr)[-300:])

# --- two windows match: the parent, never a guess --------------------------------------
w = project("two")
named(w, "reviewer", "tester")
add_spec(w, "review", "reviewer"); add_spec(w, "test", "tester")
sub_edit(w, (X, "reviewer"), "b.txt", "r\n"); sub_edit(w, (Y, "tester"), "shared.txt", "t\n")
parent_edit(w, "a.txt", "parent work\n")
parent = agent_of(newest(w))
CMD2 = 'writ spec done -s "same words"'
open_window(w, CMD2, "t2", (X, "reviewer")); open_window(w, CMD2, "t3", (Y, "tester"))
r = typed(w, CMD2)
out = r.stdout + r.stderr
chk("7 two open windows carry the same command: neither subagent's spec is closed",
    not done_with(w, "review", "same words") and not done_with(w, "test", "same words"),
    f"{json.dumps(spec(w, 'review'))[:150]} {json.dumps(spec(w, 'test'))[:150]}")
chk("8 and it says it ran as the parent", bool(parent) and parent in out and "ran as" in out.lower(), out[-300:])
done(9)
PY
rc=$?
echo "subagent_hand_command: exit $rc"
exit $rc
