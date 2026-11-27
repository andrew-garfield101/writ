#!/usr/bin/env bash
# Claim (0.4.6 note, Changed; finding 221):
#   "The survival line says how much it checked. The line `writ finish` prints
#    and the receipt records now reads, for example, `Survival: 1 of 7 added
#    line(s) of 1 spec(s) checked, none lost; 5 were sealed earlier under spec-a,
#    1 blank`."
#
# Spec A (agent amis) seals six lines of lib.rs, backs them out with
# --allow-removals, closes and is finished. Spec B restores the same lines and
# adds one new line, closes and is finished. The commit adds 7 lines: 1 new
# (checked), 5 sealed earlier under spec-a, 1 blank. Both the finish output and
# the commit's receipt must carry that line; a commit whose added lines were all
# sealed under it must read "N of N" with no "earlier" clause. 0.4.5 printed
# "N sealed line(s) ... checked" with neither count: checks 2 to 4 fail there.
#
# Usage: survival_line.sh [WRIT_BIN] [SCRATCH_DIR]
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/_lib.sh"
ROOT="$(cd "$HERE/../.." && pwd)"
WRIT="$(rc_bin "${1:-$ROOT/target/release/writ}")"
unset WRIT_AGENT_ID CLAUDECODE CLAUDE_CODE_SESSION_ID WRIT_FORMAT
rc_setup .survival_line survival-line "${2:-}"
echo "binary: $WRIT ($("$WRIT" --version))"

WRIT="$WRIT" D="$D" python3 - <<'PY'
import os, subprocess, sys
from pathlib import Path

WRIT, D = os.environ["WRIT"], Path(os.environ["D"])
fails = 0
LINES = ["fn alpha() -> u32 { 1 }", "fn bravo() -> u32 { 2 }", "",
         "fn charlie() -> u32 { 3 }", "fn delta() -> u32 { 4 }", "fn echo() -> u32 { 5 }"]

def chk(name, ok, detail=""):
    global fails
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"  [{detail}]"))
    fails += 0 if ok else 1

def git(root, *args):
    return subprocess.run(["git", *args], cwd=root, capture_output=True, text=True,
                          stdin=subprocess.DEVNULL).stdout

def w(root, agent, *args):
    env = dict(os.environ, WRIT_AGENT_ID=agent)
    p = subprocess.run([WRIT, *args], cwd=root, capture_output=True, text=True,
                       stdin=subprocess.DEVNULL, env=env)
    return p.returncode, p.stdout + p.stderr

def text(extra=()):
    return "// base\n" + "".join(l + "\n" for l in [*LINES, *extra])

def project(name):
    root = D / name
    root.mkdir()
    git(root, "init", "-q", "-b", "main")
    git(root, "config", "user.name", "writ-check")
    git(root, "config", "user.email", "writ-check@localhost")
    (root / ".gitignore").write_text(".writ/\n")
    (root / ".writignore").write_text(".writ\n.git\n")
    (root / "README.md").write_text("base\n")
    git(root, "add", "-A")
    git(root, "commit", "-q", "-m", "base")
    w(root, "human", "init", "-y", "--bare")
    return root

def seal(root, spec, *extra):
    return w(root, "amis", "seal", "-s", "work", "--agent", "amis", "--spec", spec, "--paths", "lib.rs", *extra)

def survival_lines(out, body):
    printed = next((l for l in out.splitlines() if "urvival:" in l and "checked" in l), "")
    receipt = next((l for l in body.splitlines() if l.startswith("Survival:")), "")
    return printed, receipt

# Shape 1: lines sealed under spec-a, backed out and finished; restored under spec-b plus one new line.
r = project("restore")
(r / "lib.rs").write_text("// base\n")
git(r, "add", "lib.rs"); git(r, "commit", "-q", "-m", "lib")
steps = []
steps.append(w(r, "amis", "spec", "add", "--id", "spec-a", "--title", "spec-a", "--claim"))
(r / "lib.rs").write_text(text())
steps.append(seal(r, "spec-a"))
(r / "lib.rs").write_text("// base\n// a only\n")
steps.append(seal(r, "spec-a", "--allow-removals", "lib.rs"))
steps.append(w(r, "amis", "spec", "done", "spec-a", "-s", "a", "--agent", "amis"))
steps.append(w(r, "human", "finish", "-y"))
steps.append(w(r, "amis", "spec", "add", "--id", "spec-b", "--title", "spec-b", "--claim"))
(r / "lib.rs").write_text(text(["fn foxtrot() -> u32 { 6 }"]))
steps.append(seal(r, "spec-b", "--allow-removals", "lib.rs"))
steps.append(w(r, "amis", "spec", "done", "spec-b", "-s", "b", "--agent", "amis"))
rc, out = w(r, "human", "finish", "-y")
steps.append((rc, out))
bad = [s[1][:200] for s in steps if s[0] != 0]
chk("1 setup: two specs sealed, closed and finished, each finish exit 0", not bad, " | ".join(bad))

want = "1 of 7 added line(s) of 1 spec(s) checked, none lost; 5 were sealed earlier under spec-a, 1 blank"
printed, receipt = survival_lines(out, git(r, "log", "-1", "--format=%b"))
chk("2 finish prints 'N of M added line(s)' with the 'sealed earlier under spec-a' clause",
    want in printed, printed or out[-400:])
chk("3 the commit's receipt records the same line", receipt.startswith("Survival: " + want), receipt)

# Shape 2: every added line sealed under the finished spec: N of N, no "earlier" clause.
r2 = project("all")
w(r2, "amis", "spec", "add", "--id", "spec-c", "--title", "spec-c", "--claim")
(r2 / "lib.rs").write_text("fn one() {}\nfn two() {}\n")
seal(r2, "spec-c")
w(r2, "amis", "spec", "done", "spec-c", "-s", "c", "--agent", "amis")
w(r2, "human", "finish", "-y")
_, receipt2 = survival_lines("", git(r2, "log", "-1", "--format=%b"))
chk("4 all added lines sealed under the spec: 'Survival: 2 of 2 added line(s) ... none lost', no 'earlier'",
    receipt2.startswith("Survival: 2 of 2 added line(s) of 1 spec(s) checked, none lost")
    and "earlier" not in receipt2, receipt2)

sys.exit(1 if fails else 0)
PY
