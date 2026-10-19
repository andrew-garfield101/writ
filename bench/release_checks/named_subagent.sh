#!/usr/bin/env bash
# Claims (0.4.4 note, finding 191, rulings 192 and 195):
#   "A Claude Code subagent whose type is a file under .claude/agents/ seals under that
#    type's name, and under the spec that name claimed."
#   "A second instance of the same type, while the first is still live, gets
#    `<type>-<first 8 of its id>`."
#   "Built in types (general-purpose, Explore and the others) keep the 0.4.3 identity,
#    `<parent>-<first 8 of the subagent id>`."
#   "The receipt names the agent."
#
# Stdin is the 0.4.3 subagent capture (post_tool_use_edit_subagent.json: agent_id,
# agent_type) with agent_type rewritten; identity from CLAUDECODE and the session only.
# Mirrors Bri's f191_* in crates/writ-cli/tests/auto_seal_states.rs.
#
# Usage: named_subagent.sh [WRIT_BIN] [SCRATCH_DIR]
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/_lib.sh"
ROOT="$(cd "$HERE/../.." && pwd)"
WRIT="$(rc_bin "${1:-$ROOT/target/release/writ}")"
FIX="$ROOT/crates/writ-cli/tests/fixtures/claude_hooks"
unset WRIT_AGENT_ID
rc_setup .named_subagent named-subagent "${2:-}"
echo "binary: $WRIT ($("$WRIT" --version))"
[ -f "$FIX/post_tool_use_edit_subagent.json" ] || { echo "fixture missing in $FIX" >&2; exit 2; }

HERE="$HERE" WRIT="$WRIT" FIX="$FIX" D="$D" python3 - <<'PY'
import json, os, sys
sys.path.insert(0, os.environ["HERE"])
from _hooks import *

S = "44444444-4444-4444-8444-444444444444"
X, Y, Z = "a1b2c3d4e5f6a7b8c", "a9e8d7c6b5a4f3e2d", "a0a1a2a3a4a5a6a7a"

def agent_file(root, name):
    d = root / ".claude/agents"; d.mkdir(parents=True, exist_ok=True)
    (d / f"{name}.md").write_text(f"---\nname: {name}\ndescription: test agent\n---\nYou are {name}.\n")

def sub_edit(root, sub_id, ty, rel, text):
    (root / rel).write_text(text)
    v = fixture("post_tool_use_edit_subagent.json", root, S)
    v["agent_id"], v["agent_type"] = sub_id, ty
    v["tool_input"]["file_path"] = str(root / rel)
    v["tool_response"]["filePath"] = str(root / rel)
    return fire(root, "PostToolUse", v, "Edit", S)

def sub_stop(root, sub_id, ty):
    return fire(root, "SubagentStop", {"session_id": S, "cwd": str(root), "hook_event_name": "SubagentStop",
                                       "agent_id": sub_id, "agent_type": ty, "stop_hook_active": False}, None, S)

def parent_id(root):
    (root / "a.txt").write_text("parent edit\n")
    v = fixture("post_tool_use_edit_main.json", root, S)
    v["tool_input"]["file_path"] = str(root / "a.txt")
    if isinstance(v.get("tool_response"), dict):
        v["tool_response"]["filePath"] = str(root / "a.txt")
    fire(root, "PostToolUse", v, "Edit", S)
    return agent_of(newest(root))

def autos(root):
    try:
        return [s for s in json.loads(writ(root, "spec", "status", "--format", "json").stdout)
                if str(s.get("id", "")).startswith("auto-")]
    except ValueError:
        return []

# --- a named type seals under its name, edit tool and Bash ---------------------------
p = project("named")
agent_file(p, "reviewer")
res = sub_edit(p, X, "reviewer", "b.txt", "reviewer was here\n")
top = newest(p)
chk("1 an Edit by a `reviewer` subagent (.claude/agents/reviewer.md) seals b.txt under identity `reviewer`",
    zero(res) and not told(res) and agent_of(top) == "reviewer" and paths_of(top) == ["b.txt"],
    f"{agent_of(top)}/{paths_of(top)} {show(res)}")
cmd = "printf 'reviewer\\n' >> c.txt"
extra = {"agent_id": X, "agent_type": "reviewer"}
res = fire(p, "PreToolUse", bash_stdin(p, S, "PreToolUse", cmd, "tr1", extra), "Bash", S)
sh_run(p, cmd); tick()
res += fire(p, "PostToolUse", bash_stdin(p, S, "PostToolUse", cmd, "tr1", extra), "Bash", S)
top = newest(p)
chk("2 its Bash write seals c.txt under `reviewer` too (the case in finding 191)",
    zero(res) and agent_of(top) == "reviewer" and paths_of(top) == ["c.txt"], f"{agent_of(top)}/{paths_of(top)} {show(res)}")
r = writ(p, "handoff", "--format", "receipt")
chk("3 the receipt names the agent `reviewer`, not a hash", "reviewer" in r.stdout, r.stdout[:400])

# --- its claimed spec is honoured -------------------------------------------------------
p = project("spec")
agent_file(p, "reviewer")
add_spec(p, "review", "reviewer")
n0 = len(autos(p))
sub_edit(p, X, "reviewer", "b.txt", "reviewed\n")
top = newest(p)
chk("4 `reviewer` claimed spec `review` by hand: the hook seals under `review` and opens no auto spec",
    agent_of(top) == "reviewer" and top.get("spec_id") == "review" and len(autos(p)) == n0,
    f"{agent_of(top)}/{top.get('spec_id')} autos {n0}->{len(autos(p))}")

# --- two live instances, then stopped ----------------------------------------------------
p = project("two")
agent_file(p, "reviewer")
sub_edit(p, X, "reviewer", "b.txt", "x\n"); sx = newest(p)
sub_edit(p, Y, "reviewer", "shared.txt", "y\n"); sy = newest(p)
chk("5 two live `reviewer` instances: the first keeps `reviewer`, the second is `reviewer-a9e8d7c6`",
    agent_of(sx) == "reviewer" and agent_of(sy) == f"reviewer-{Y[:8]}", f"{agent_of(sx)}, {agent_of(sy)}")
st = sub_stop(p, X, "reviewer") + sub_stop(p, Y, "reviewer")
sub_edit(p, Z, "reviewer", "a.txt", "lone\n")
chk("6 after both SubagentStop, a new lone instance is plain `reviewer` again",
    zero(st) and agent_of(newest(p)) == "reviewer", f"{agent_of(newest(p))} {show(st)}")

# --- built in and unknown types keep the parent derived form ----------------------------
p = project("builtin")
agent_file(p, "reviewer")
par = parent_id(p)
got = {}
for i, (sid, ty) in enumerate(((X, "general-purpose"), (Y, "Explore"), (Z, "no-such-agent-file"))):
    sub_edit(p, sid, ty, f"f{i}.txt", f"{ty}\n")
    got[ty] = (agent_of(newest(p)), f"{par}-{sid[:8]}")
for n, ty in zip("abc", got):
    chk(f"7{n} agent_type `{ty}` keeps the parent derived identity <parent>-<agent8>",
        bool(par) and got[ty][0] == got[ty][1], f"got {got[ty][0]}, want {got[ty][1]}")
done(9)
PY
rc=$?
echo "named_subagent: exit $rc"
exit $rc
