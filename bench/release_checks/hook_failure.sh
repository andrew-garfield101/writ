#!/usr/bin/env bash
# Claims (0.4.4 note, finding 186):
#   "`writ init -y` registers the PostToolUseFailure hook beside PostToolUse, for the
#    edit tools and Bash; `writ uninit` removes it."
#   "A shell command that fails still closes its window, and a file it wrote before
#    failing is sealed under its agent."
#   "An agent's window left open by a host that sent no event closes at its next call."
#   "A project set up by 0.4.3 reads version_skew until `writ init -y`."
#
# Drives the hooks as Claude Code does (commands from .claude/settings.json, `sh -c`,
# stdin JSON), with the PostToolUseFailure stdin captured live in sprint 7
# (crates/writ-cli/tests/fixtures/claude_hooks/post_tool_use_failure_bash*.json).
# Mirrors Bri's f186_* in crates/writ-cli/tests/auto_seal_states.rs.
#
# Usage: hook_failure.sh [WRIT_BIN] [SCRATCH_DIR]
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/_lib.sh"
ROOT="$(cd "$HERE/../.." && pwd)"
WRIT="$(rc_bin "${1:-$ROOT/target/release/writ}")"
FIX="$ROOT/crates/writ-cli/tests/fixtures/claude_hooks"
unset WRIT_AGENT_ID
rc_setup .hook_failure hook-failure "${2:-}"
echo "binary: $WRIT ($("$WRIT" --version))"
[ -f "$FIX/post_tool_use_failure_bash.json" ] || { echo "fixture missing: $FIX/post_tool_use_failure_bash.json" >&2; exit 2; }

HERE="$HERE" WRIT="$WRIT" FIX="$FIX" D="$D" python3 - <<'PY'
import json, os, sys
sys.path.insert(0, os.environ["HERE"])
from _hooks import *

TOOLS = ("Edit", "Write", "NotebookEdit", "Bash")

# --- registration ------------------------------------------------------------------
p = project("install")
for n, tool in zip("abcd", TOOLS):
    chk(f"1{n} init -y: a writ PostToolUseFailure hook covers {tool}",
        bool(commands(p, "PostToolUseFailure", tool)),
        json.dumps(settings(p).get("hooks", {}).get("PostToolUseFailure")))
chk("2 PostToolUseFailure runs the same writ command as PostToolUse, once per tool",
    all(commands(p, "PostToolUseFailure", t) == commands(p, "PostToolUse", t) and
        len(commands(p, "PostToolUseFailure", t)) == 1 for t in TOOLS),
    json.dumps(settings(p).get("hooks", {})))

# --- the captured failure stdin seals what the command wrote before it failed -----
p = project("wrote")
add_spec(p, "sa", "alpha")
f = fixture("post_tool_use_failure_bash_after_write.json", p, "sess-a")
cmd, tid = f["tool_input"]["command"], f["tool_use_id"]
pre = fixture("pre_tool_use_bash.json", p, "sess-a")
pre["tool_input"]["command"], pre["tool_use_id"] = cmd, tid
before = len(seals(p))
res = fire(p, "PreToolUse", pre, "Bash", "sess-a", "alpha")
rc = sh_run(p, cmd); tick()
res += fire(p, "PostToolUseFailure", f, "Bash", "sess-a", "alpha")
top = newest(p)
chk("3 the captured failure (`printf 'x\\n' > wrote.txt && false`, exit 1): hooks exit 0 and hand the agent nothing",
    rc != 0 and zero(res) and not told(res), f"cmd rc={rc} {show(res)}")
chk("4 wrote.txt, written before the failure, is sealed once, under alpha's spec sa",
    len(seals(p)) == before + 1 and paths_of(top) == ["wrote.txt"] and agent_of(top) == "alpha"
    and top.get("spec_id") == "sa", f"seals {before}->{len(seals(p))} top={agent_of(top)}/{top.get('spec_id')}/{paths_of(top)}")
recs = [r for r in hook_records(p) if r.get("event") == "PostToolUseFailure"]
chk("5 hooks.log records the firing under event PostToolUseFailure", bool(recs),
    (p / ".writ/hooks.log").read_text()[-600:] if (p / ".writ/hooks.log").exists() else "no hooks.log")
stop = fire(p, "Stop", {"session_id": "sess-a", "cwd": str(p), "hook_event_name": "Stop",
                        "stop_hook_active": False}, None, "sess-a", "alpha")
chk("6 the window is closed: alpha's Stop seals nothing more", zero(stop) and len(seals(p)) == before + 1,
    f"seals={len(seals(p))} {show(stop)}")

# --- a failed read no longer makes its agent a candidate for the other's writes ----
p = project("read")
add_spec(p, "sa", "alpha"); add_spec(p, "sb", "bravo")
f = fixture("post_tool_use_failure_bash.json", p, "sess-a")
ca, ta = f["tool_input"]["command"], f["tool_use_id"]
pre = fixture("pre_tool_use_bash.json", p, "sess-a")
pre["tool_input"]["command"], pre["tool_use_id"] = ca, ta
a = fire(p, "PreToolUse", pre, "Bash", "sess-a", "alpha")
sh_run(p, ca); tick()
a += fire(p, "PostToolUseFailure", f, "Bash", "sess-a", "alpha"); tick()
before = len(seals(p))
cb = "printf 'bravo\\n' >> shared.txt"
b = fire(p, "PreToolUse", bash_stdin(p, "sess-b", "PreToolUse", cb, "tb"), "Bash", "sess-b", "bravo")
sh_run(p, cb); tick()
b += fire(p, "PostToolUse", bash_stdin(p, "sess-b", "PostToolUse", cb, "tb"), "Bash", "sess-b", "bravo")
top = newest(p)
amb = [r for r in hook_records(p) if r.get("outcome") == "ambiguous"]
chk("7 after alpha's `cat missing_file` failed, bravo's write to shared.txt is sealed under bravo, nobody asked, no ambiguous record",
    zero(a) and zero(b) and not told(a) and not told(b) and len(seals(p)) == before + 1
    and agent_of(top) == "bravo" and paths_of(top) == ["shared.txt"] and not amb,
    f"top={agent_of(top)}/{paths_of(top)} told={told(a) + told(b)} amb={amb}")

# --- a host that sends nothing for the failed call: closed at the next PreToolUse --
p = project("stale")
add_spec(p, "sa", "alpha"); add_spec(p, "sb", "bravo")
a1 = "cat NO_SUCH_FILE.md 2>/dev/null"
fire(p, "PreToolUse", bash_stdin(p, "sess-a", "PreToolUse", a1, "ta1"), "Bash", "sess-a", "alpha")
sh_run(p, a1); tick()
a2 = "ls >/dev/null"
fire(p, "PreToolUse", bash_stdin(p, "sess-a", "PreToolUse", a2, "ta2"), "Bash", "sess-a", "alpha")
sh_run(p, a2)
fire(p, "PostToolUse", bash_stdin(p, "sess-a", "PostToolUse", a2, "ta2"), "Bash", "sess-a", "alpha"); tick()
before = len(seals(p))
b = fire(p, "PreToolUse", bash_stdin(p, "sess-b", "PreToolUse", cb, "tb"), "Bash", "sess-b", "bravo")
sh_run(p, cb); tick()
b += fire(p, "PostToolUse", bash_stdin(p, "sess-b", "PostToolUse", cb, "tb"), "Bash", "sess-b", "bravo")
top = newest(p)
late = [r for r in hook_records(p) if r.get("event") == "window_closed_late" or "window_closed_late" in json.dumps(r)]
chk("8 no event for alpha's failed call: its next PreToolUse closes the window, bravo's write is sealed under bravo, nobody asked",
    not told(b) and len(seals(p)) == before + 1 and agent_of(top) == "bravo" and paths_of(top) == ["shared.txt"],
    f"top={agent_of(top)}/{paths_of(top)} told={told(b)}")
chk("9 hooks.log records the late close", bool(late), "no window_closed_late record")

# --- 0.4.3 settings read version_skew; init -y clears it; uninit removes it --------
p = project("skew")
s = settings(p)
s.get("hooks", {}).pop("PostToolUseFailure", None)
(p / ".claude/settings.json").write_text(json.dumps(s, indent=2))
def skew(root):
    try:
        d = json.loads(writ(root, "doctor", "--format", "json").stdout)
    except ValueError:
        return ["<doctor json unreadable>"]
    return [x for x in d.get("findings", []) if x.get("check") == "version_skew"]
sk = skew(p)
chk("10 settings without PostToolUseFailure (a 0.4.3 project): doctor reports version_skew on .claude/settings.json naming `writ init -y`",
    bool(sk) and "init -y" in json.dumps(sk) and any(".claude/settings.json" in (x.get("paths") or []) for x in sk), json.dumps(sk)[:300])
writ(p, "init", "-y")
chk("11 `writ init -y` adds it back and the version_skew is gone",
    bool(commands(p, "PostToolUseFailure", "Bash")) and not skew(p), json.dumps(skew(p))[:300])
s = settings(p)
s["hooks"].setdefault("PostToolUseFailure", []).append(
    {"matcher": "Bash", "hooks": [{"type": "command", "command": "echo user-failure-hook"}]})
(p / ".claude/settings.json").write_text(json.dumps(s, indent=2))
r = writ(p, "uninit", "-y")
mine = commands(p, "PostToolUseFailure")
users = [h.get("command") for e in settings(p).get("hooks", {}).get("PostToolUseFailure", []) or [] for h in e.get("hooks", [])]
chk("12 uninit removes writ's PostToolUseFailure hook and keeps the user's own",
    r.returncode == 0 and not mine and "echo user-failure-hook" in users, f"rc={r.returncode} writ={mine} users={users}")
done(15)
PY
rc=$?
echo "hook_failure: exit $rc"
exit $rc
