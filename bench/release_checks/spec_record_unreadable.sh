#!/usr/bin/env bash
# Claim (0.4.6 note, headline and Fixed; finding 218):
#   "A seal never lands when its spec record cannot be read." / "The seal now
#    reads the record before anything is written and refuses if it cannot:
#    "seal refused: the record of spec '<id>' could not be read (...); nothing
#    was written, because a seal never lands without its spec record.""
#
# A project set up by `writ init -y`; agent alpha claims spec s; s's record
# (.writ/specs/s.json) is truncated to invalid JSON before the seal. Through the
# CLI the record is read while the seal resolves its spec, so the refusal there
# is "spec not found: 's'" (an explicit --spec) or "No active spec" (alpha's
# claim no longer readable), not the core's 218 message; the Python API
# (`Repository.seal(spec_id='s')`) and a record that goes bad after the CLI
# resolved it print "seal refused: the record of spec 's' could not be read
# (...)" (Rust f218_* tests; probe in evidence/0.4.6-release-checks-freeze1).
# What this script holds the binary to is the behavior: every CLI seal refuses
# and writes nothing, or (--auto, the hook's command) lands on a spec whose
# record reads and lists it. Never a seal without its record. Control: with
# the record put back, the same --spec seal lands.
#
# Usage: spec_record_unreadable.sh [WRIT_BIN] [SCRATCH_DIR]
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/_lib.sh"
ROOT="$(cd "$HERE/../.." && pwd)"
WRIT="$(rc_bin "${1:-$ROOT/target/release/writ}")"
unset WRIT_AGENT_ID CLAUDECODE CLAUDE_CODE_SESSION_ID WRIT_FORMAT
rc_setup .spec_record_unreadable spec-record-unreadable "${2:-}"
echo "binary: $WRIT ($("$WRIT" --version))"

WRIT="$WRIT" D="$D" python3 - <<'PY'
import hashlib, json, os, subprocess, sys
from pathlib import Path

WRIT, D = os.environ["WRIT"], Path(os.environ["D"])
fails = 0

def chk(name, ok, detail=""):
    global fails
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"  [{detail}]"))
    fails += 0 if ok else 1

def run(root, *args, agent=None):
    env = dict(os.environ)
    if agent:
        env["WRIT_AGENT_ID"] = agent
    return subprocess.run(args, cwd=root, capture_output=True, text=True,
                          stdin=subprocess.DEVNULL, env=env)

def store_digest(writ):
    """Hash of every file under .writ that a seal writes (objects, seals, HEAD, refs, index)."""
    h = hashlib.sha256()
    for p in sorted(writ.rglob("*")):
        rel = p.relative_to(writ).as_posix()
        if not p.is_file() or rel.startswith(("security", "hooks.log", "doctor_cache", "agents/", "sessions/", "keys/", "events")):
            continue
        h.update(rel.encode() + b"\0" + p.read_bytes())
    return h.hexdigest()

def seal_count(root):
    p = run(root, WRIT, "log", "--all", "--format", "json")
    try:
        return len(json.loads(p.stdout))
    except json.JSONDecodeError:
        return None

root = D / "proj"
root.mkdir()
run(root, "git", "init", "-q", "-b", "main")
(root / "app.py").write_text('print("hi")\n')
run(root, "git", "add", "-A")
run(root, "git", "-c", "user.name=t", "-c", "user.email=t@localhost", "commit", "-qm", "base")
run(root, WRIT, "init", "-y")
run(root, WRIT, "spec", "add", "--id", "s", "--title", "s")
c = run(root, WRIT, "spec", "claim", "s", "--agent", "alpha", agent="alpha")
record = root / ".writ" / "specs" / "s.json"
good = record.read_bytes() if record.exists() else b""
chk("1 setup: spec s claimed by alpha, record at .writ/specs/s.json",
    c.returncode == 0 and good, c.stdout + c.stderr)

record.write_text('{"id": "s", "tit')
bad = record.read_bytes()
(root / "a.txt").write_text("a\n")
writ_dir = root / ".writ"
before = store_digest(writ_dir)
n_before = seal_count(root)
seal = ["seal", "-s", "x", "--agent", "alpha", "--spec", "s", "--paths", "a.txt"]
p = run(root, WRIT, *seal, agent="alpha")
out = p.stdout + p.stderr
chk("2 a --spec seal on the unreadable record is refused, naming spec 's'",
    p.returncode != 0 and "'s'" in out, f"rc {p.returncode}: {out[:300]}")
p = run(root, WRIT, "seal", "-s", "x", "--agent", "alpha", "--paths", "a.txt", agent="alpha")
chk("3 alpha's seal with no --spec (its claim unreadable) is refused",
    p.returncode != 0, f"rc {p.returncode}: {(p.stdout + p.stderr)[:300]}")
chk("4 no seal was added to the log", seal_count(root) == n_before, f"{n_before} -> {seal_count(root)}")
chk("5 HEAD, objects, seals and index unchanged byte for byte", store_digest(writ_dir) == before)
chk("6 the unreadable record is left as it was", record.read_bytes() == bad)

p = run(root, WRIT, "seal", "--paths", "a.txt", "--auto", "--agent", "alpha", agent="alpha")
log = run(root, WRIT, "log", "--all", "--format", "json")
try:
    newest = json.loads(log.stdout)[0]
except (json.JSONDecodeError, IndexError):
    newest = {}
sid = newest.get("spec_id") or ""
rec_ok = False
rp = root / ".writ" / "specs" / f"{sid}.json"
if sid and sid != "s" and rp.exists():
    try:
        rec_ok = newest.get("id", "?") in rp.read_text() and bool(json.loads(rp.read_text()))
    except json.JSONDecodeError:
        rec_ok = False
chk("7 the hook's --auto seal lands on another spec whose record reads and lists it",
    p.returncode == 0 and rec_ok, f"rc {p.returncode} spec {sid!r}: {(p.stdout + p.stderr)[:200]}")

record.write_bytes(good)
n_mid = seal_count(root)
(root / "a.txt").write_text("a\nmore\n")
p2 = run(root, WRIT, *seal, agent="alpha")
chk("8 control: with the record put back, the same --spec seal lands",
    p2.returncode == 0 and seal_count(root) == (n_mid or 0) + 1, (p2.stdout + p2.stderr)[:300])

sys.exit(1 if fails else 0)
PY
