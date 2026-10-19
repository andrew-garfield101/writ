#!/usr/bin/env bash
# Claims (0.4.4 note, findings 188 and 176, ruling 189):
#   "When two agents' commands were open as a file changed and exactly one of those
#    commands names the file, writ seals it under that agent and records that it decided
#    from the command: `attributed_by: command_text` on the seal, and the receipt,
#    handoff and `writ why` say 'attributed from the command'."
#   "When both commands name the file, or neither does, writ asks exactly as in 0.4.3."
#
# Two main agents stand in through writ agent ids (alpha, bravo) with their own
# sessions, Bash Pre/Post hooks fed as Claude Code does. Mirrors Bri's f188_* and
# rc_189 in crates/writ-cli/tests/.
#
# Usage: command_text.sh [WRIT_BIN] [SCRATCH_DIR]
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/_lib.sh"
ROOT="$(cd "$HERE/../.." && pwd)"
WRIT="$(rc_bin "${1:-$ROOT/target/release/writ}")"
FIX="$ROOT/crates/writ-cli/tests/fixtures/claude_hooks"
unset WRIT_AGENT_ID
rc_setup .command_text command-text "${2:-}"
echo "binary: $WRIT ($("$WRIT" --version))"

HERE="$HERE" WRIT="$WRIT" FIX="$FIX" D="$D" python3 - <<'PY'
import json, os, re, sys
sys.path.insert(0, os.environ["HERE"])
from _hooks import *

# The 0.4.3 ambiguity line, unchanged in 0.4.4 (template_size and auto_seal check its use).
AMBIG = re.compile(r"^shared\.txt changed while 2 agents ran commands \((alpha, bravo|bravo, alpha)\); "
                   r"writ could not tell whose change it is; if it is yours: writ seal --paths shared\.txt --auto$")

def overlapped(root, ca, cb, tag):
    """alpha's command ca runs (and writes) while bravo's cb is open; cb never runs."""
    a = fire(root, "PreToolUse", bash_stdin(root, "sess-a", "PreToolUse", ca, f"ta{tag}"), "Bash", "sess-a", "alpha"); tick()
    b = fire(root, "PreToolUse", bash_stdin(root, "sess-b", "PreToolUse", cb, f"tb{tag}"), "Bash", "sess-b", "bravo"); tick()
    sh_run(root, ca); tick()
    a += fire(root, "PostToolUse", bash_stdin(root, "sess-a", "PostToolUse", ca, f"ta{tag}"), "Bash", "sess-a", "alpha"); tick()
    b += fire(root, "PostToolUse", bash_stdin(root, "sess-b", "PostToolUse", cb, f"tb{tag}"), "Bash", "sess-b", "bravo")
    return a, b

def line_with(text, needle):
    return next((l for l in text.splitlines() if needle in l), "")

# --- one command names the path ------------------------------------------------------
p = project("one")
since = (seals(p) or [{}])[0].get("id", "")
add_spec(p, "sa", "alpha"); add_spec(p, "sb", "bravo")
before = len(seals(p))
a, b = overlapped(p, "printf 'alpha\\n' >> shared.txt", "ls >/dev/null", "1")
top = newest(p)
chk("1 alpha's command names shared.txt, bravo's `ls` was open: hooks exit 0 and ask nobody",
    zero(a) and zero(b) and not told(a) and not told(b), f"told={told(a) + told(b)}")
chk("2 sealed once, under alpha's spec sa, with attributed_by command_text in `writ log --format json`",
    len(seals(p)) == before + 1 and agent_of(top) == "alpha" and top.get("spec_id") == "sa"
    and paths_of(top) == ["shared.txt"] and top.get("attributed_by") == "command_text",
    f"seals {before}->{len(seals(p))} {agent_of(top)}/{top.get('spec_id')} attributed_by={top.get('attributed_by')!r}")
recs = hook_records(p)
chk("3 hooks.log has the command text record and no ambiguous record for shared.txt",
    any(r.get("path") == "shared.txt" and r.get("attributed_by") == "command_text" for r in recs)
    and not any(r.get("outcome") == "ambiguous" for r in recs),
    json.dumps([r for r in recs if r.get("path") == "shared.txt"])[:400])
# bravo then seals its own line by hand, so the receipt has a shared file line
(p / "shared.txt").write_text((p / "shared.txt").read_text() + "bravo\n")
hs = writ(p, "seal", "-s", "bravo's line", "--spec", "sb", "--paths", "shared.txt", "--agent", "bravo", agent="bravo")
rec = writ(p, "handoff", "--since", since, "--format", "receipt").stdout
l = line_with(rec, "shared.txt")
chk("4 the receipt's shared.txt line reads `sa by alpha, attributed from the command`, once",
    hs.returncode == 0 and "sa by alpha, attributed from the command" in l and l.count("attributed from the command") == 1,
    f"hand seal rc={hs.returncode} {hs.stderr.strip()[:160]!r} line={l!r}")
why = writ(p, "why", "shared.txt").stdout
chk("5 `writ why shared.txt` says attributed from the command on alpha's seal only",
    sum("attributed from the command" in x for x in why.splitlines()) == 1, why[:500])

# --- both commands name the path: the 0.4.3 ask, unchanged ----------------------------
p = project("both")
add_spec(p, "sa", "alpha"); add_spec(p, "sb", "bravo")
before = len(seals(p))
a, b = overlapped(p, "printf 'alpha\\n' >> shared.txt", "grep -c line shared.txt >/dev/null", "2")
ta, tb = told(a), told(b)
chk("6 both commands name shared.txt: nothing sealed, nothing recorded as command_text",
    len(seals(p)) == before and "command_text" not in (p / ".writ/hooks.log").read_text(), f"seals {before}->{len(seals(p))}")
chk("7 each agent gets the 0.4.3 ambiguity line, word for word",
    len(ta) == 1 and len(tb) == 1 and bool(AMBIG.match(ta[0].split(" --agent")[0])) and bool(AMBIG.match(tb[0].split(" --agent")[0])),
    f"alpha={ta} bravo={tb}")

# --- neither names it: still asked (no reader rule) -----------------------------------
p = project("neither")
add_spec(p, "sa", "alpha"); add_spec(p, "sb", "bravo")
(HOME / "gen.sh").write_text("printf 'alpha\\n' >> shared.txt\n")
before = len(seals(p))
a, b = overlapped(p, f"sh {HOME}/gen.sh", "pytest -q", "3")
chk("8 neither command names shared.txt (a script wrote it, the other is a reader): asked, not sealed",
    len(seals(p)) == before and len(told(a)) == 1 and len(told(b)) == 1, f"seals {before}->{len(seals(p))} told={told(a) + told(b)}")
done(8)
PY
rc=$?
echo "command_text: exit $rc"
exit $rc
