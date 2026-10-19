#!/usr/bin/env bash
# Claims (0.4.3 note, auto seal):
#   "`writ init -y` installs a Claude Code hook that seals each file an agent edits,
#    under that agent's own spec, as soon as the edit lands; `writ uninit` removes it."
#   "A hook never fails the agent's tool call: on any error it exits 0 and writes the
#    error to .writ/hooks.log."
#   "If an auto seal is refused (the edit dropped another agent's sealed lines), the
#    agent is told in one line with the fix, in its next context."
#   "Auto seal shrinks the unsealed window to the hook's latency" (latency printed).
#
# Drives the hooks exactly as Claude Code does, with no `claude` binary: reads the
# writ hook commands from .claude/settings.json, runs each with `sh -c` in the
# project root, stdin = the live hook JSON captured in sprint 6
# (crates/writ-cli/tests/fixtures/claude_hooks/), with paths and session rewritten.
# Identity comes from the framework (CLAUDECODE=1, CLAUDE_CODE_SESSION_ID), never
# WRIT_AGENT_ID. Mirrors Bri's crates/writ-cli/tests/auto_seal_states.rs.
#
# Usage: auto_seal.sh [WRIT_BIN] [SCRATCH_DIR]
#   LATENCY_EDITS=20 by default. LATENCY_BUDGET=0 prints latency without enforcing the
#   release budget (CI on shared runners with a debug build); the release run keeps 1.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/_lib.sh"
ROOT="$(cd "$HERE/../.." && pwd)"
WRIT="$(rc_bin "${1:-$ROOT/target/release/writ}")"
FIX="$ROOT/crates/writ-cli/tests/fixtures/claude_hooks"
unset WRIT_AGENT_ID
rc_setup .auto_seal auto-seal "${2:-}"
echo "binary: $WRIT ($("$WRIT" --version))"
[ -d "$FIX" ] || { echo "fixtures missing: $FIX" >&2; exit 2; }

WRIT="$WRIT" FIX="$FIX" D="$D" LATENCY_EDITS="${LATENCY_EDITS:-20}" LATENCY_BUDGET="${LATENCY_BUDGET:-1}" python3 - <<'PY'
import json, os, re, shutil, statistics, subprocess, sys, time
from pathlib import Path

WRIT, FIX, D = os.environ["WRIT"], Path(os.environ["FIX"]), Path(os.environ["D"])
N = int(os.environ["LATENCY_EDITS"])
BIN = D / "bin"; BIN.mkdir()
(BIN / "writ").symlink_to(WRIT)
HOME = D / "home"; HOME.mkdir()
fail = 0

def chk(name, ok, detail=""):
    global fail
    print(("PASS " if ok else "FAIL ") + name + ("" if ok or not detail else f"  [{detail}]"))
    if not ok:
        fail = 1

def env(session=None, agent=None):
    e = {"PATH": f"{BIN}:/usr/bin:/bin:/usr/sbin:/sbin", "HOME": str(HOME),
         "TMPDIR": os.environ.get("TMPDIR", "/tmp"), "GIT_CONFIG_NOSYSTEM": "1",
         "WRIT_EXCLUDES_FILE": str(HOME / "none"),
         "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@localhost",
         "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@localhost"}
    if agent:
        e["WRIT_AGENT_ID"] = agent
    else:
        e["CLAUDECODE"] = "1"
        if session:
            e["CLAUDE_CODE_SESSION_ID"] = session
    return e

def run(root, args, session=None, agent="human", stdin=None):
    return subprocess.run(args, cwd=root, env=env(session, agent), input=stdin,
                          capture_output=True, text=True)

def writ(root, *args, session=None, agent="human"):
    return run(root, ["writ", *args], session, agent)

def project(name):
    root = D / name; root.mkdir()
    for a in (["git", "init", "-q", "-b", "main"],):
        run(root, a)
    (root / "a.txt").write_text("".join(f"line {i}\n" for i in range(1, 21)))
    (root / "b.txt").write_text("".join(f"line {i}\n" for i in range(1, 21)))
    (root / "shared.txt").write_text("".join(f"line {i}\n" for i in range(1, 41)))
    (root / "nb.ipynb").write_text('{"cells": []}\n')
    (root / ".gitignore").write_text("ignored.txt\n")
    run(root, ["git", "add", "-A"]); run(root, ["git", "commit", "-qm", "base"])
    r = writ(root, "init", "-y")
    assert r.returncode == 0, r.stdout + r.stderr
    run(root, ["git", "add", "-A"]); run(root, ["git", "commit", "-qm", "init", "--no-verify"])
    return root

def settings(root):
    try:
        return json.loads((root / ".claude/settings.json").read_text())
    except (OSError, ValueError):
        return {}

def covers(matcher, tool):
    m = (matcher or "").strip()
    if m in ("", "*", ".*"):
        return True
    try:
        return re.fullmatch(m, tool) is not None
    except re.error:
        return tool in m.strip("^$()").split("|")

def commands(root, event, tool=None):
    out = []
    for e in settings(root).get("hooks", {}).get(event, []) or []:
        if tool and not covers(e.get("matcher"), tool):
            continue
        out += [h["command"] for h in e.get("hooks", []) if "writ" in h.get("command", "")]
    return out

def fixture(name, root, session, rel=None, tool=None):
    v = json.loads((FIX / name).read_text())
    v["session_id"] = session; v["cwd"] = str(root)
    v["transcript_path"] = f"/tmp/fixture/{session}.jsonl"
    if rel:
        ti = v.get("tool_input", {})
        key = "notebook_path" if "notebook_path" in ti else "file_path"
        ti[key] = str(root / rel)
        if isinstance(v.get("tool_response"), dict) and "filePath" in v["tool_response"]:
            v["tool_response"]["filePath"] = str(root / rel)
    return v

def fire(root, event, stdin, tool=None, session=None, raw=None, agent=None):
    res = []
    for c in commands(root, event, tool):
        t = time.perf_counter()
        e = env(session, agent)
        if agent and session:
            e["CLAUDE_CODE_SESSION_ID"] = session
        r = subprocess.run(["sh", "-c", c], cwd=root, env=e, capture_output=True,
                           text=True, input=raw if raw is not None else json.dumps(stdin))
        res.append((r, (time.perf_counter() - t) * 1000))
    return res

def seals(root):
    r = writ(root, "log", "--all", "--format", "json", "--limit", "1000")
    try:
        return json.loads(r.stdout)
    except ValueError:
        return []

def paths_of(seal):
    return sorted(c.get("path") for c in seal.get("changes", []))

def specs(root):
    try:
        return json.loads(writ(root, "spec", "status", "--format", "json").stdout)
    except ValueError:
        return []

def hooks_log(root):
    p = root / ".writ/hooks.log"
    return p.read_text() if p.exists() else ""

def zero_and_silent(res):
    return bool(res) and all(r.returncode == 0 and not r.stdout.strip() for r, _ in res)

def show(res):
    return "; ".join(f"rc={r.returncode} out={r.stdout.strip()[:120]!r} err={r.stderr.strip()[:120]!r}" for r, _ in res) or "no writ hook ran"

# --- install and remove -------------------------------------------------------
p = project("install")
for n, tool in zip("abc", ("Edit", "Write", "NotebookEdit")):
    chk(f"1{n} init -y: a writ PostToolUse hook covers {tool}", bool(commands(p, "PostToolUse", tool)),
        json.dumps(settings(p).get("hooks", {}).get("PostToolUse")))
chk("2 init -y: writ hooks on Stop and SessionEnd", bool(commands(p, "Stop")) and bool(commands(p, "SessionEnd")))
# a user's own hook beside writ's must survive uninit
s = settings(p)
s.setdefault("hooks", {}).setdefault("PostToolUse", []).append(
    {"matcher": "Edit", "hooks": [{"type": "command", "command": "echo user-hook"}]})
(p / ".claude/settings.json").write_text(json.dumps(s, indent=2))
r = writ(p, "uninit", "-y")
left = [c for ev in settings(p).get("hooks", {}).values() for e in ev or [] for c in
        [h.get("command", "") for h in e.get("hooks", [])] if "writ" in c]
users = [h.get("command") for e in settings(p).get("hooks", {}).get("PostToolUse", []) or [] for h in e.get("hooks", [])]
chk("3 uninit removes every writ hook and keeps the user's own", r.returncode == 0 and not left and "echo user-hook" in users,
    f"rc={r.returncode} left={left} users={users}")

# --- a scripted edit ------------------------------------------------------------
p = project("edit")
S1 = "11111111-1111-4111-8111-111111111111"
r = writ(p, "spec", "add", "hand made", "--id", "hand", "--claim", session=S1, agent=None)
(p / "a.txt").write_text((p / "a.txt").read_text().replace("line 3\n", "line 3 edited\n"))
before = len(seals(p))
res = fire(p, "PostToolUse", fixture("post_tool_use_edit_main.json", p, S1, "a.txt"), "Edit", S1)
new = seals(p)
top = new[0] if new else {}
chk("4 Edit: hook exits 0 and prints nothing", zero_and_silent(res), show(res))
chk("5 Edit: exactly one seal, of exactly a.txt, summary 'auto: Edit'",
    len(new) == before + 1 and paths_of(top) == ["a.txt"] and str(top.get("summary", "")).startswith("auto: Edit"),
    f"seals {before}->{len(new)} paths={paths_of(top)} summary={top.get('summary')!r}")
chk("6 Edit: sealed under the spec this session claimed by hand", top.get("spec_id") == "hand",
    f"spec_id={top.get('spec_id')!r} agent={top.get('agent')}")

# Write and NotebookEdit each seal their own path
(p / "b.txt").write_text("written\n")
fire(p, "PostToolUse", fixture("post_tool_use_write_main.json", p, S1, "b.txt"), "Write", S1)
w = seals(p)[0]
(p / "nb.ipynb").write_text('{"cells": [1]}\n')
fire(p, "PostToolUse", fixture("post_tool_use_notebookedit_main.json", p, S1, "nb.ipynb"), "NotebookEdit", S1)
n = seals(p)[0]
chk("7 Write seals b.txt; NotebookEdit seals nb.ipynb (notebook_path)",
    paths_of(w) == ["b.txt"] and paths_of(n) == ["nb.ipynb"], f"write={paths_of(w)} notebook={paths_of(n)}")

# --- no spec: first edit creates and claims an auto spec ---------------------------
p = project("nospec")
S2 = "22222222-2222-4222-8222-222222222222"
(p / "a.txt").write_text("changed\n")
res = fire(p, "PostToolUse", fixture("post_tool_use_edit_main.json", p, S2, "a.txt"), "Edit", S2)
autos = [s for s in specs(p) if str(s.get("id", "")).startswith("auto-")]
top = (seals(p) or [{}])[0]
chk("8 no claimed spec: first edit creates one auto- spec and seals a.txt under it",
    zero_and_silent(res) and len(autos) == 1 and top.get("spec_id") == autos[0].get("id") and paths_of(top) == ["a.txt"],
    f"autos={[s.get('id') for s in autos]} seal spec={top.get('spec_id')} paths={paths_of(top)} {show(res)}")

# --- another agent's pending path is never sealed ---------------------------------
S3 = "33333333-3333-4333-8333-333333333333"
(p / "b.txt").write_text("other session's pending work\n")       # session S3 edits b.txt, no hook yet
(p / "a.txt").write_text("changed again\n")
fire(p, "PostToolUse", fixture("post_tool_use_edit_main.json", p, S2, "a.txt"), "Edit", S2)
top = seals(p)[0]
all_paths = {x for s in seals(p) if (s.get("agent") or {}).get("id") != "writ-bridge" for x in paths_of(s)}  # not the git baseline import
chk("9 S2's edit seals a.txt only; S3's pending b.txt is in no seal",
    paths_of(top) == ["a.txt"] and "b.txt" not in all_paths, f"top={paths_of(top)} all={sorted(all_paths)}")

# --- never fails the tool -----------------------------------------------------------
before = len(seals(p)); log0 = len(hooks_log(p))
bad = [("not json", "this is not json"), ("empty stdin", ""),
       ("no file_path", json.dumps({"session_id": S2, "hook_event_name": "PostToolUse", "tool_name": "Edit", "tool_input": {}})),
       ("missing file", json.dumps(fixture("post_tool_use_edit_main.json", p, S2, "gone.txt"))),
       ("outside repo", json.dumps(fixture("post_tool_use_edit_main.json", p, S2, "../outside.txt")))]
results = {w: fire(p, "PostToolUse", None, "Edit", S2, raw=s) for w, s in bad}
chk("10 bad stdin (5 kinds): every hook exits 0, prints nothing, seals nothing",
    all(zero_and_silent(r) for r in results.values()) and len(seals(p)) == before,
    "; ".join(f"{w}: {show(r)}" for w, r in results.items() if not zero_and_silent(r)))
chk("11 bad stdin is written to .writ/hooks.log", len(hooks_log(p)) > log0)
sd = p / ".writ/seals"; moved = p / ".writ/seals.moved"
sd.rename(moved); sd.write_text("not a directory")
(p / "a.txt").write_text("edit on a broken store\n")
log1 = len(hooks_log(p))
res = fire(p, "PostToolUse", fixture("post_tool_use_edit_main.json", p, S2, "a.txt"), "Edit", S2)
sd.unlink(); moved.rename(sd)
chk("12 broken store: exit 0, nothing to the agent, error in hooks.log",
    zero_and_silent(res) and len(hooks_log(p)) > log1, show(res))

# --- refused auto seal reaches the agent ----------------------------------------------
p = project("refused")
SA, SB = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
lines = [f"line {i}\n" for i in range(1, 41)]
b = lines.copy(); b[29] = "line 30 by B\n"
(p / "shared.txt").write_text("".join(b))
fire(p, "PostToolUse", fixture("post_tool_use_edit_main.json", p, SB, "shared.txt"), "Edit", SB)
b_seal = next((x for x in seals(p) if paths_of(x) == ["shared.txt"]), {})   # B's own auto seal, if any
b_id = (b_seal.get("agent") or {}).get("id") if b_seal else None
a = lines.copy(); a[1] = "line 2 by A\n"                        # A writes from a stale read: B's line 30 is gone
(p / "shared.txt").write_text("".join(a))
before = len(seals(p))
res = fire(p, "PostToolUse", fixture("post_tool_use_write_main.json", p, SA, "shared.txt"), "Write", SA)
told = []
for r, _ in res:
    if r.returncode == 0 and r.stdout.strip():
        try:
            told.append(json.loads(r.stdout)["hookSpecificOutput"]["additionalContext"])
        except (ValueError, KeyError, TypeError):
            told.append("<stdout not hook JSON> " + r.stdout.strip())
    elif r.returncode == 2:
        told.append(r.stderr.strip())
msg = told[0].strip() if told else ""
chk("13 refused auto seal: no seal, exit 0 (or 2, which Claude Code treats as feedback, the tool already ran)",
    bool(res) and len(seals(p)) == before and all(r.returncode in (0, 2) for r, _ in res), show(res))
chk("14 refused auto seal: one line to the agent naming the path, the other agent and a fix",
    len(told) == 1 and len(msg.splitlines()) == 1 and "shared.txt" in msg and bool(b_id) and b_id in msg and "fix:" in msg,
    f"told={told} other={b_id}")
chk("15 refused auto seal: the fix is not --force and not a destructive restore",
    bool(msg) and "--force" not in msg and "restore" not in msg, msg)

# --- 168 / 169 / 170: the ambiguity line, its fix from any state, and the auto title ----------
# Bash Pre/Post hooks fed by hand as Claude Code would (tool_name Bash, tool_use_id),
# mirroring Bri's f168_*/f169_*/f170_* in auto_seal_states.rs: alpha's read-only command
# is open while bravo writes, so writ cannot tell whose change it is (ruling 160).
def bash_stdin(root, session, event, cmd, tid):
    v = {"session_id": session, "transcript_path": f"/tmp/fixture/{session}.jsonl", "cwd": str(root),
         "hook_event_name": event, "permission_mode": "bypassPermissions",
         "tool_name": "Bash", "tool_input": {"command": cmd, "description": "run"}, "tool_use_id": tid}
    if event == "PostToolUse":
        v["tool_response"] = {"stdout": "", "stderr": "", "interrupted": False}
    return v

def bash(root, agent, session, event, cmd, tid):
    return fire(root, event, bash_stdin(root, session, event, cmd, tid), "Bash", session, agent=agent)

def sh_run(root, cmd):
    subprocess.run(["sh", "-c", cmd], cwd=root, env=env(), capture_output=True)

def tick():
    time.sleep(0.03)

def bash_call(root, agent, session, cmd, tid):
    r = bash(root, agent, session, "PreToolUse", cmd, tid); sh_run(root, cmd)
    return r + bash(root, agent, session, "PostToolUse", cmd, tid)

def told(res):
    out = []
    for r, _ in res:
        if r.returncode == 0 and r.stdout.strip():
            try:
                out.append(json.loads(r.stdout)["hookSpecificOutput"]["additionalContext"].strip())
            except (ValueError, KeyError, TypeError):
                out.append("<not hook JSON> " + r.stdout.strip())
        elif r.returncode == 2:
            out.append(r.stderr.strip())
    return out

def ambiguity(root, paths, tag):
    ca = "cat a.txt >/dev/null"
    # bravo writes through a script whose command text names no path, so 0.4.4's
    # command text rule (finding 188) cannot decide and the 0.4.3 fallback is what
    # these checks exercise, as Bri's Rust tests do since sprint 7.
    gen = HOME / f"gen_{tag}.sh"
    gen.write_text("".join(f"printf 'bravo {tag}\\n' >> {f}\n" for f in paths))
    cb = f"sh {gen}"
    a = bash(root, "alpha", "sess-a", "PreToolUse", ca, f"toolu_a_{tag}"); tick()
    b = bash(root, "bravo", "sess-b", "PreToolUse", cb, f"toolu_b_{tag}"); tick()
    sh_run(root, cb); tick()
    b += bash(root, "bravo", "sess-b", "PostToolUse", cb, f"toolu_b_{tag}"); tick()
    a += bash(root, "alpha", "sess-a", "PostToolUse", ca, f"toolu_a_{tag}")
    return a, b

def fix_of(line):
    tail = line.split("if it is yours: ")[1] if "if it is yours: " in line else line.split("fix: ", 1)[-1]
    return tail.split(";")[0].strip().strip("`")

def paste(root, agent, session, fix):
    e = env(session, agent); e["CLAUDE_CODE_SESSION_ID"] = session
    return subprocess.run(["sh", "-c", fix], cwd=root, env=e, capture_output=True, text=True)

def spec_show(root, sid):
    if not sid:
        return {}
    try:
        return json.loads(writ(root, "spec", "show", sid, "--format", "json").stdout)
    except ValueError:
        return {}

def add_spec(root, sid, agent):
    writ(root, "spec", "add", "--id", sid, "--title", f"spec {sid}", agent=agent)
    writ(root, "spec", "claim", sid, "--agent", agent, agent=agent)

# 168, closed spec: alpha's own auto spec is closed, then it pastes the ambiguity fix
p = project("f168-closed")
bash_call(p, "alpha", "sess-a", "printf 'A\\n' >> a.txt", "toolu_c")
auto = (seals(p) or [{}])[0].get("spec_id") or ""
writ(p, "spec", "done", "-s", "alpha's own words", "--agent", "alpha", agent="alpha")
add_spec(p, "sb", "bravo")
a, _ = ambiguity(p, ["shared.txt"], "1")
lines = told(a)
fix = fix_of(lines[0]) if len(lines) == 1 else ""
r1 = paste(p, "alpha", "sess-a", fix) if fix.startswith("writ ") else None
top = (seals(p) or [{}])[0]
fu = spec_show(p, top.get("spec_id", "")) if top else {}
r2 = paste(p, "alpha", "sess-a", fix) if fix.startswith("writ ") else None
closed = spec_show(p, auto)
chk("18 168: after alpha closed its auto spec, the pasted ambiguity fix exits 0 and seals shared.txt under alpha in <auto>-followup (continues <auto>), the closed spec keeps alpha's words, a second paste exits 0",
    bool(auto.startswith("auto-")) and r1 is not None and r1.returncode == 0 and (top.get("agent") or {}).get("id") == "alpha"
    and paths_of(top) == ["shared.txt"] and top.get("spec_id") == f"{auto}-followup" and fu.get("continues") == auto
    and "alpha's own words" in json.dumps(closed) and r2 is not None and r2.returncode == 0,
    f"auto={auto} lines={lines} fix={fix!r} rc={getattr(r1,'returncode',None)} {getattr(r1,'stderr','')[:200]!r} top={top.get('spec_id')}/{paths_of(top)} continues={fu.get('continues')}")

# 168, no spec ever
p = project("f168-nospec")
add_spec(p, "sb", "bravo")
a, _ = ambiguity(p, ["shared.txt"], "5")
lines = told(a); fix = fix_of(lines[0]) if len(lines) == 1 else ""
r = paste(p, "alpha", "sess-a", fix) if fix.startswith("writ ") else None
top = (seals(p) or [{}])[0]
chk("19 168: alpha with no spec ever pastes the fix: exit 0, an auto- spec claimed by alpha, shared.txt sealed",
    r is not None and r.returncode == 0 and (top.get("agent") or {}).get("id") == "alpha" and paths_of(top) == ["shared.txt"]
    and str(top.get("spec_id", "")).startswith("auto-") and spec_show(p, top.get("spec_id", "")).get("claimed_by") == "alpha",
    f"lines={lines} rc={getattr(r,'returncode',None)} {getattr(r,'stderr','')[:200]!r} top={top.get('spec_id')}")

# 168, several paths: one line, one comma-separated fix that works
p = project("f168-several")
add_spec(p, "sa", "alpha"); add_spec(p, "sb", "bravo")
a, _ = ambiguity(p, ["shared.txt", "b.txt"], "2")
lines = told(a); line = lines[0] if len(lines) == 1 else ""
fix = fix_of(line) if line else ""
listed = set(fix.split("--paths ")[1].split()[0].split(",")) if "--paths " in fix else set()
r = paste(p, "alpha", "sess-a", fix) if fix.startswith("writ ") else None
chk("20 168: two ambiguous paths give one line, one `writ seal`, --paths a,b, and pasting it seals both",
    len(lines) == 1 and len(line.splitlines()) == 1 and line.count("writ seal") == 1 and listed == {"shared.txt", "b.txt"}
    and r is not None and r.returncode == 0 and paths_of((seals(p) or [{}])[0]) == ["b.txt", "shared.txt"],
    f"lines={lines} rc={getattr(r,'returncode',None)}")

# 169: left_out names the candidates, not anyone_may_fix; a third identity is refused
p = project("f169")
add_spec(p, "sa", "alpha"); add_spec(p, "sb", "bravo")
a, _ = ambiguity(p, ["shared.txt"], "3")
lines = told(a); fix = fix_of(lines[0]) if len(lines) == 1 else ""
writ(p, "spec", "done", "sa", "--agent", "alpha", "--no-seal", agent="alpha")
writ(p, "spec", "done", "sb", "--agent", "bravo", "--no-seal", agent="bravo")
try:
    d = json.loads(writ(p, "doctor", "--format", "json", agent="charlie").stdout)
except ValueError:
    d = {}
f = next((x for x in d.get("findings", []) if x.get("check") == "left_out" and "shared.txt" in json.dumps(x)), {})
msg = f.get("message", "")
chk("21 169: the ambiguous path's left_out names alpha and bravo, says it changed while, anyone_may_fix false, needs_human false",
    bool(f) and f.get("anyone_may_fix") is False and f.get("needs_human") is False and "alpha" in msg and "bravo" in msg and "changed while" in msg,
    json.dumps(f)[:400])
before = len(seals(p))
r = paste(p, "charlie", "sess-c", fix) if fix.startswith("writ ") else None
writ(p, "spec", "add", "--id", "fu", "--title", "follow up", "--claim", agent="charlie")
r2 = writ(p, "seal", "-s", "per doctor", "--agent", "charlie", "--spec", "fu", "--paths", "shared.txt", agent="charlie")
t = (r.stdout + r.stderr) if r else ""
chk("22 169: a third identity pasting the fix, or sealing it under a follow-up spec, is refused naming alpha and bravo; nothing sealed",
    r is not None and r.returncode != 0 and "alpha" in t and "bravo" in t and r2.returncode != 0 and len(seals(p)) == before,
    f"rc={getattr(r,'returncode',None)} {t[:200]!r} followup rc={r2.returncode}")

# 170: the auto title is the first sentence, cut at a word under 60 chars with an ellipsis
for n, (tag, prompt, sentence) in enumerate([
        ("long", "You are working on a Flask API project in this directory and the work is large. Split it.",
         "You are working on a Flask API project in this directory and the work is large."),
        ("short", "Fix the parser. Then add tests for it.", "Fix the parser.")]):
    p = project(f"f170-{tag}")
    up = {"session_id": "sess-a", "cwd": str(p), "hook_event_name": "UserPromptSubmit",
          "prompt": prompt, "transcript_path": "/tmp/fixture/sess-a.jsonl"}
    fire(p, "UserPromptSubmit", up, None, "sess-a", agent="alpha")
    bash_call(p, "alpha", "sess-a", "printf 'A\\n' >> a.txt", "toolu_t")
    title = spec_show(p, (seals(p) or [{}])[0].get("spec_id", "")).get("title", "")
    if len(sentence) < 60:
        ok = title.rstrip(".") == sentence.rstrip(".") and "…" not in title
    else:
        kept = title.rstrip("…").rstrip()
        ok = len(title) < 60 and title.endswith("…") and sentence.startswith(kept) and sentence[len(kept):len(kept) + 1] in (" ", ",", ".")
    chk(f"{23 + n} 170 ({tag}): auto spec title is the prompt's first sentence, under 60 chars, cut at a word with …", ok, repr(title))

# --- latency ---------------------------------------------------------------------------
p = project("latency")
SL = "44444444-4444-4444-8444-444444444444"
ms = []
for i in range(N):
    (p / "a.txt").write_text(f"edit {i}\n")
    res = fire(p, "PostToolUse", fixture("post_tool_use_edit_main.json", p, SL, "a.txt"), "Edit", SL)
    ms += [t for _, t in res]
sealed = sum(1 for s in seals(p) if paths_of(s) == ["a.txt"])
if ms:
    p50, mx = statistics.median(ms), max(ms)
    print(f"latency: {len(ms)} hook runs, p50 {p50:.0f} ms, max {mx:.0f} ms, min {min(ms):.0f} ms; {sealed} seals of a.txt")
    chk(f"16 every one of {N} edits sealed", sealed == N, f"{sealed}/{N}")
    if os.environ["LATENCY_BUDGET"] == "1":
        chk("17 hook latency p50 < 100 ms and max < 500 ms (release budget)", p50 < 100 and mx < 500, f"p50 {p50:.0f} max {mx:.0f}")
    else:
        print("SKIP 17 latency budget (LATENCY_BUDGET=0; printed above, not enforced)")
else:
    chk("16 every edit sealed", False, "no writ hook ran"); chk("17 latency", False, "no writ hook ran")
sys.exit(fail)
PY
