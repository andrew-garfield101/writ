#!/usr/bin/env bash
# Claims (0.4.5 note, finding 206):
#   "writ no longer asks you about files it wrote itself: the file `writ finish`
#    writes, and the files `writ restore` and `writ converge` write, are not
#    attributed to whichever agents had commands open at the time."
#
# Two agents (alpha, bravo) seal versions of shared.txt on their own specs (bravo's supersedes) (the
# divergence is built with the test fixture key `[seal] foreign_line_check = false`, as in
# receipt.sh; every finish, restore and converge runs with the key removed). Both agents
# then have a Bash command open (PreToolUse sent, PostToolUse not yet) while the writ
# command writes shared.txt; the PostToolUse hooks follow. 0.4.4 asked both agents
# whose change shared.txt was (finding 206, from this repo's 0.4.4 finish).
#
# Usage: own_writes.sh [WRIT_BIN] [SCRATCH_DIR]
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/_lib.sh"
ROOT="$(cd "$HERE/../.." && pwd)"
WRIT="$(rc_bin "${1:-$ROOT/target/release/writ}")"
FIX="$ROOT/crates/writ-cli/tests/fixtures/claude_hooks"
unset WRIT_AGENT_ID
rc_setup .own_writes own-writes "${2:-}"
echo "binary: $WRIT ($("$WRIT" --version))"

HERE="$HERE" WRIT="$WRIT" FIX="$FIX" D="$D" python3 - <<'PY'
import json, os, re, sys
sys.path.insert(0, os.environ["HERE"])
from _hooks import *

def numbered(n, edits):
    return "".join(f"line {i} {edits[i]}\n" if i in edits else f"line {i}\n" for i in range(n))

def fx_key(root, on):
    p = root / ".writ/config.toml"
    s = re.sub(r"\n?\[seal\]\nforeign_line_check = false\n", "\n", p.read_text())
    if on:
        s = s.rstrip("\n") + "\n\n[seal]\nforeign_line_check = false\n"
    p.write_text(s)

def diverged(name):
    """sa (alpha) and sb (bravo) sealed different lines of shared.txt from the same base."""
    root = project(name)
    run(root, ["git", "config", "user.name", "fixture"]); run(root, ["git", "config", "user.email", "fixture@example.invalid"])
    (root / "shared.txt").write_text(numbered(40, {}))
    run(root, ["git", "add", "-A"]); run(root, ["git", "commit", "-qm", "base40", "--no-verify"])
    fx_key(root, True)
    add_spec(root, "sa", "alpha"); add_spec(root, "sb", "bravo")
    (root / "shared.txt").write_text(numbered(40, {2: "A"}))
    a = writ(root, "seal", "-s", "alpha work", "--agent", "alpha", "--spec", "sa", "--paths", "shared.txt", agent="alpha")
    (root / "shared.txt").write_text(numbered(40, {30: "B"}))
    b = writ(root, "seal", "-s", "bravo work", "--agent", "bravo", "--spec", "sb", "--paths", "shared.txt", agent="bravo")
    fx_key(root, False)
    for sid, who in (("sa", "alpha"), ("sb", "bravo")):
        writ(root, "spec", "done", sid, "-s", f"{who} done", "--agent", who, agent=who)
    return root, a.returncode == 0 and b.returncode == 0

def around(root, tag, writ_args):
    """Open a Bash window for alpha and bravo, run the writ command, close both windows."""
    sess = {"alpha": "sess-a", "bravo": "sess-b"}
    for who, s in sess.items():
        fire(root, "PreToolUse", bash_stdin(root, s, "PreToolUse", "sleep 1", f"{tag}{who}"), "Bash", s, who)
    tick()
    n_log = len(hook_records(root))
    r = writ(root, *writ_args)
    tick()
    res = []
    for who, s in sess.items():
        res += fire(root, "PostToolUse", bash_stdin(root, s, "PostToolUse", "sleep 1", f"{tag}{who}"), "Bash", s, who)
    recs = [x for x in hook_records(root)[n_log:] if x.get("path") == "shared.txt"]
    return r, res, recs

def quiet(res, recs):
    t = [x for x in told(res) if "shared.txt" in x]
    bad = [x for x in recs if x.get("outcome") in ("ambiguous", "sealed", "refused")]
    return not t and not bad, f"told={t[:1]} records={[x.get('outcome') for x in recs]}"

# --- finish writes the merged file ------------------------------------------------------
p, ok = diverged("finish")
chk("1 fixture: alpha and bravo sealed different lines of shared.txt on their own specs", ok)
before = (p / "shared.txt").stat().st_mtime_ns
r, res, recs = around(p, "f", ["finish", "-y"])
txt = (p / "shared.txt").read_text()
head = run(p, ["git", "show", "HEAD:shared.txt"]).stdout
chk("2 `writ finish` committed shared.txt and rewrote it in the working tree (mtime changed, content equals HEAD)",
    r.returncode == 0 and txt == head and (p / "shared.txt").stat().st_mtime_ns != before and "line 30 B" in head, (r.stdout + r.stderr)[-300:])
q_ok, why = quiet(res, recs)
chk("3 neither agent is asked about shared.txt, and the hook records no ambiguous or sealed write of it", q_ok, why)
chk("4 nothing left to seal after the finish: no auto seal of shared.txt by either agent",
    not any(paths_of(s) == ["shared.txt"] and agent_of(s) in ("alpha", "bravo") and "auto" in (s.get("summary") or "")
            for s in seals(p)), str([(agent_of(s), s.get("summary")) for s in seals(p)[:3]]))

# --- restore writes an earlier version ------------------------------------------------------
target = next((s["id"] for s in seals(p) if s.get("summary") == "alpha work"), None)
r, res, recs = around(p, "r", ["restore", target or "x", "--force"] if target else ["restore", "x"])
q_ok, why = quiet(res, recs)
chk("5 `writ restore` rewrites shared.txt with both agents' commands open: nobody is asked",
    r.returncode == 0 and "line 30 B" not in (p / "shared.txt").read_text() and q_ok, f"rc={r.returncode} {why} {(r.stderr)[-200:]}")

# --- converge writes the merged file -------------------------------------------------------
c, ok = diverged("converge")
before = (c / "shared.txt").stat().st_mtime_ns
r, res, recs = around(c, "c", ["converge", "sa", "sb", "--apply"])
txt = (c / "shared.txt").read_text()
q_ok, why = quiet(res, recs)
chk("6 `writ converge sa sb --apply` writes the merge with both commands open: nobody is asked",
    r.returncode == 0 and (c / "shared.txt").stat().st_mtime_ns != before and q_ok, f"rc={r.returncode} {why} {(r.stdout + r.stderr)[-200:]}")

# --- a real agent write in the same window is still sealed ---------------------------------
for who, s in (("alpha", "sess-a"),):
    fire(c, "PreToolUse", bash_stdin(c, s, "PreToolUse", "printf x >> a.txt", "k1"), "Bash", s, who)
    sh_run(c, "printf x >> a.txt"); tick()
    fire(c, "PostToolUse", bash_stdin(c, s, "PostToolUse", "printf x >> a.txt", "k1"), "Bash", s, who)
chk("7 an agent's own write after it is still sealed under that agent (the marker is gone when the command exits)",
    agent_of(newest(c)) == "alpha" and paths_of(newest(c)) == ["a.txt"], f"{agent_of(newest(c))}/{paths_of(newest(c))}")
done(7)
PY
rc=$?
echo "own_writes: exit $rc"
exit $rc
