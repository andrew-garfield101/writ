#!/usr/bin/env bash
# Claims (0.4.5 note, findings 190 and 208):
#   "An auto spec's title comes from the first sentence of the first prompt that is not
#    a message from another agent or a system notice; with no such prompt it is
#    `auto work by <identity>`."
#   "When a spec is closed with `writ spec done -s`, the finish commit's subject is the
#    first sentence of that summary."
#
# Prompts are fed through the UserPromptSubmit hook exactly as Claude Code sends them;
# the wrapper shapes are the ones seen on this repo: <agent-message> (it titled the 0.4.4
# bump's auto spec, finding 208), <cross-session-message>, [SYSTEM NOTIFICATION and
# <task-notification> (finding 133).
#
# Usage: titles.sh [WRIT_BIN] [SCRATCH_DIR]
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/_lib.sh"
ROOT="$(cd "$HERE/../.." && pwd)"
WRIT="$(rc_bin "${1:-$ROOT/target/release/writ}")"
FIX="$ROOT/crates/writ-cli/tests/fixtures/claude_hooks"
unset WRIT_AGENT_ID
rc_setup .titles titles "${2:-}"
echo "binary: $WRIT ($("$WRIT" --version))"

HERE="$HERE" WRIT="$WRIT" FIX="$FIX" D="$D" python3 - <<'PY'
import json, os, sys
sys.path.insert(0, os.environ["HERE"])
from _hooks import *

AGENT_MSG = '<agent-message from="a5eb00ac0f3ede640">\nBri: freeze 6 is green, run the gate now.\n</agent-message>'
SYSTEM = [
    ("a system notification", "[SYSTEM NOTIFICATION - NOT USER INPUT]\nThis is an automated background-task event, NOT a message from the user.\n<task-notification>\n<task-id>bx</task-id>\n<status>completed</status>\n</task-notification>"),
    ("a cross session message", '<cross-session-message from="writ-69">\nAubs: README draft is up.\n</cross-session-message>'),
    ("a task notification", None),  # the captured fixture
]

def prompt(root, session, text):
    if text is None:
        v = fixture("user_prompt_submit_task_notification.json", root, session)
    else:
        v = fixture("user_prompt_submit.json", root, session)
        v["prompt"] = text
    return fire(root, "UserPromptSubmit", v, None, session)

def edit(root, session, rel, text):
    with open(root / rel, "a") as fh:
        fh.write(text)
    v = fixture("post_tool_use_edit_main.json", root, session)
    v["tool_input"]["file_path"] = str(root / rel)
    v["tool_response"]["filePath"] = str(root / rel)
    return fire(root, "PostToolUse", v, "Edit", session)

def auto_spec(root):
    s = newest(root)
    sid = s.get("spec_id")
    r = writ(root, "spec", "show", sid or "x", "--format", "json")
    try:
        return sid, json.loads(r.stdout).get("title", "")
    except ValueError:
        return sid, ""

# --- 190: agent messages and system notices never title the spec ------------------------
p = project("titles")
S1 = "11111111-1111-4111-8111-111111111111"
prompt(p, S1, "Add the parser module. Then wire the tests.")
prompt(p, S1, AGENT_MSG)
for _, t in SYSTEM:
    prompt(p, S1, t)
edit(p, S1, "a.txt", "parser\n")
sid, title = auto_spec(p)
chk("1 a real prompt, then an agent message and three system notices: the title is the real prompt's first sentence",
    title == "Add the parser module.", f"{sid}: {title!r}")

for i, (what, t) in enumerate([("an agent message", AGENT_MSG)] + SYSTEM, start=2):
    s = f"2222222{i}-2222-4222-8222-222222222222"
    prompt(p, s, t)
    edit(p, s, "b.txt", f"{i}\n")
    sid, title = auto_spec(p)
    who = agent_of(newest(p)) or "?"
    chk(f"{i} a session whose only prompt is {what}: the title is `auto work by {who}`, nothing from the message",
        title == f"auto work by {who}", f"{sid}: {title!r}")

# --- 208 / 190: the finish subject uses the closing summary -----------------------------
q = project("subject")
run(q, ["git", "config", "user.name", "fixture"]); run(q, ["git", "config", "user.email", "fixture@example.invalid"])
S3 = "33333333-3333-4333-8333-333333333333"
prompt(q, S3, AGENT_MSG)
edit(q, S3, "a.txt", "moved\n")
sid, _ = auto_spec(q)
d = writ(q, "spec", "done", sid or "x", "-s", "Moved the parser to its own module. Tests follow in the next spec.",
         "--agent", agent_of(newest(q)) or "x", agent=agent_of(newest(q)) or "x")
f = writ(q, "finish", "-y")
subj = run(q, ["git", "log", "-1", "--format=%s"]).stdout.strip()
chk("6 an auto spec closed with `spec done -s`: the finish subject is the summary's first sentence",
    f.returncode == 0 and "Moved the parser to its own module" in subj and "Tests follow" not in subj
    and "agent-message" not in subj, f"done rc={d.returncode} finish rc={f.returncode} subject={subj!r}")
done(6)
PY
rc=$?
echo "titles: exit $rc"
exit $rc
