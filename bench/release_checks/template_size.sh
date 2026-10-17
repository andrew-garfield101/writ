#!/usr/bin/env bash
# Claim (0.4.3 note, template): "With sealing automatic, the instructions writ adds
# for agents shrink to N lines with no mandatory command: the CLAUDE.md block, the
# AGENTS.md block and the session start message each say only that `writ context`
# exists and that `writ spec done -s` closes a task."
#
# Counts the non-empty lines inside the managed block of CLAUDE.md and AGENTS.md and
# the lines the SessionStart hook prints before the context brief, after a fresh
# `writ init -y` (bound: at most 5 non-empty lines, ruling 171). Prints each count and the text, so the note's number comes from here.
# Mirrors Bri's tp_* assertions in crates/writ-cli/tests/auto_seal_states.rs.
#
# Usage: template_size.sh [WRIT_BIN] [SCRATCH_DIR]
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/_lib.sh"
ROOT="$(cd "$HERE/../.." && pwd)"
WRIT="$(rc_bin "${1:-$ROOT/target/release/writ}")"
unset WRIT_AGENT_ID
rc_setup .template_size template-size "${2:-}"
echo "binary: $WRIT ($("$WRIT" --version))"
mkdir -p "$D/bin"; ln -s "$WRIT" "$D/bin/writ"; export RC_BIN_DIR="$D/bin"
rc_git_project "$D/p"
printf '# agents\n' > AGENTS.md
PATH="$D/bin:$PATH" "$WRIT" init -y > init.log 2>&1 || { cat init.log; exit 1; }

WRIT="$WRIT" python3 - <<'PY'
import json, os, subprocess, sys

BEGIN = "<!-- BEGIN WRIT CONFIGURATION"
END = "<!-- END WRIT CONFIGURATION -->"
MANDATORY = ["MANDATORY", "MUST", "EVERY TASK", "NOT complete", "BEFORE", "FIRST ACTION"]
fail = 0

def chk(name, ok, detail=""):
    global fail
    print(("PASS " if ok else "FAIL ") + name + ("" if ok or not detail else f"  [{detail}]"))
    fail |= not ok

def block(path):
    try:
        t = open(path).read()
    except OSError:
        return None
    if BEGIN not in t or END not in t:
        return None
    body = t[t.index(BEGIN):t.index(END)].splitlines()[1:]
    return [l.strip() for l in body if l.strip()]

def session_start_echo():
    s = json.load(open(".claude/settings.json"))
    cmds = [h["command"] for e in s.get("hooks", {}).get("SessionStart", []) or []
            for h in e.get("hooks", []) if "writ" in h.get("command", "")]
    if not cmds:
        return None
    stdin = json.dumps({"session_id": "55555555-5555-4555-8555-555555555555", "cwd": os.getcwd(),
                        "hook_event_name": "SessionStart", "source": "startup",
                        "transcript_path": "/tmp/fixture.jsonl"})
    env = {k: v for k, v in os.environ.items() if k != "WRIT_AGENT_ID"}
    env["CLAUDECODE"] = "1"
    env["PATH"] = os.environ["RC_BIN_DIR"] + ":/usr/bin:/bin:/usr/sbin:/sbin"   # the writ under test only
    r = subprocess.run(["sh", "-c", cmds[0]], input=stdin, capture_output=True, text=True, env=env)
    out = []
    for l in r.stdout.splitlines():
        if l.startswith("# writ context"):
            break
        if l.strip():
            out.append(l.strip())
    return out

surfaces = {"CLAUDE.md block": block("CLAUDE.md"), "AGENTS.md block": block("AGENTS.md"),
            "SessionStart echo": session_start_echo()}
for name, lines in surfaces.items():
    if lines is None:
        chk(f"{name}: present", False, "not found"); continue
    text = "\n".join(lines)
    print(f"--- {name}: {len(lines)} lines\n{text}\n---")
    words = [w for w in MANDATORY if w in text]
    chk(f"{name}: at most 5 non-empty lines ({len(lines)})", len(lines) <= 5)
    chk(f"{name}: no mandatory wording", not words, ",".join(words))
    seal_lines = [l for l in lines if "writ seal -s" in l]   # not "writ seals the edits" (ruling 175)
    chk(f"{name}: one `writ seal -s`, on the 'in other tools' line (decision 167); no identity instruction",
        text.count("writ seal -s") == 1 and bool(seal_lines) and "in other tools" in seal_lines[0] and "WRIT_AGENT_ID" not in text,
        "; ".join(seal_lines) or "no writ seal line")
    att = [l for l in lines if "seals the edits it can attribute" in l]
    chk(f"{name}: says writ 'seals the edits it can attribute' (ruling 175), scoped to Claude Code, and no longer 'sealed automatically'",
        bool(att) and all("Claude Code" in l for l in att) and "sealed automatically" not in text,
        "; ".join(att) or "no 'seals the edits it can attribute' line")
    chk(f"{name}: names writ context and writ spec done -s",
        "writ context" in text and "writ spec done -s" in text)
a, c = surfaces["AGENTS.md block"], surfaces["CLAUDE.md block"]
strip = lambda v: [l for l in (v or []) if not l.startswith("#")]
chk("AGENTS.md and CLAUDE.md blocks match beyond the heading", a is not None and strip(a) == strip(c))
sys.exit(fail)
PY
