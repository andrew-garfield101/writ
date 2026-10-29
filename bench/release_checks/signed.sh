#!/usr/bin/env bash
# Claims (0.4.5 note, finding 199; finding 209):
#   "Seals are signed by default. The first time writ seals for an identity it makes an
#    Ed25519 key for it, stored where `writ agent register` stores keys, under .writ,
#    which git never sees."
#   "`writ verify` checks every signature against its identity's key and reports older
#    unsigned seals by count, not as failures. A signature that was altered fails."
#   "The receipt's survival line says N seals, N signed."
#   "A key writ made proves the seals came from one continuing identity, not who that is."
#
# Identity comes from the framework only (CLAUDECODE, CLAUDE_CODE_SESSION_ID), as for a
# fresh Claude Code session; a hand seal uses an identity from `writ agent register`.
# Mirrors Bri's f199_* in crates/writ-cli/tests/signing_states.rs.
#
# Usage: signed.sh [WRIT_BIN] [SCRATCH_DIR]
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/_lib.sh"
ROOT="$(cd "$HERE/../.." && pwd)"
WRIT="$(rc_bin "${1:-$ROOT/target/release/writ}")"
FIX="$ROOT/crates/writ-cli/tests/fixtures/claude_hooks"
unset WRIT_AGENT_ID
rc_setup .signed signed "${2:-}"
echo "binary: $WRIT ($("$WRIT" --version))"

HERE="$HERE" WRIT="$WRIT" FIX="$FIX" D="$D" python3 - <<'PY'
import json, os, sys
sys.path.insert(0, os.environ["HERE"])
from _hooks import *

S = "55555555-5555-4555-8555-555555555555"

def hook_edit(root, rel, text):
    with open(root / rel, "a") as fh:
        fh.write(text)
    v = fixture("post_tool_use_edit_main.json", root, S)
    v["tool_input"]["file_path"] = str(root / rel)
    v["tool_response"]["filePath"] = str(root / rel)
    return fire(root, "PostToolUse", v, "Edit", S)

def vjson(root, *args):
    r = writ(root, "verify", *args, "--format", "json")
    try:
        return r.returncode, json.loads(r.stdout)
    except ValueError:
        return r.returncode, {"raw": (r.stdout + r.stderr)[:300]}

p = project("signed")
before = newest(p).get("id", "")
older = [s for s in seals(p) if not s.get("signature")]

# --- first hook seal of a fresh identity ------------------------------------------------
res = hook_edit(p, "a.txt", "first\n")
s1 = newest(p); who = agent_of(s1) or ""
chk("1 a fresh Claude Code session's first hook seal carries a signature",
    zero(res) and s1.get("id") != before and bool(s1.get("signature")), f"{who} sig={bool(s1.get('signature'))} {show(res)}")
keys = p / ".writ/keys"
chk("2 its key is where `writ agent register` stores keys: .writ/keys/<id>.pub and .enc",
    bool(who) and (keys / f"{who}.pub").is_file() and (keys / f"{who}.enc").is_file(),
    f"{sorted(x.name for x in keys.iterdir()) if keys.is_dir() else 'no .writ/keys'}")
rc, v = vjson(p, "--seal", s1.get("id", "x")[:12])
chk("3 `writ verify --seal` checks it against that key: signature_valid true, exit 0",
    rc == 0 and v.get("signature_valid") is True and v.get("valid") is True, json.dumps(v)[:300])
hook_edit(p, "b.txt", "second\n")
s2 = newest(p)
chk("4 the same identity's second hook seal is signed with the same key and verifies",
    agent_of(s2) == who and bool(s2.get("signature")) and vjson(p, "--seal", s2["id"][:12])[1].get("signature_valid") is True,
    f"{agent_of(s2)} sig={bool(s2.get('signature'))}")

# --- verify counts older unsigned seals, not as failures --------------------------------
all_s = seals(p)
n_signed = sum(1 for s in all_s if s.get("signature"))
n_unsigned = len(all_s) - n_signed
rc, v = vjson(p)
chk(f"5 `writ verify` (chain): valid, exit 0, seals_signed {n_signed}, seals_unsigned {n_unsigned} "
    f"({len(older)} seal(s) from before the hook, unsigned)",
    rc == 0 and v.get("valid") is True and v.get("seals_signed") == n_signed and v.get("seals_unsigned") == n_unsigned
    and len(older) >= 1 and not v.get("failures"), json.dumps(v)[:400])
r = writ(p, "verify")
line = next((l for l in r.stdout.splitlines() if "unsigned" in l), "")
chk("6 the text form names both counts and does not read as a failure",
    r.returncode == 0 and f"{n_signed} signed" in line and f"{n_unsigned} unsigned" in line
    and "fail" not in r.stdout.lower(), r.stdout[:400])

# --- the receipt ------------------------------------------------------------------------
r = writ(p, "handoff", "--since", before, "--format", "receipt")
surv = next((l for l in r.stdout.splitlines() if "survival" in l.lower()), "")
chk("7 the receipt's survival line says `2 seals, 2 signed` for the two hook seals",
    "2 seals, 2 signed" in surv, surv or r.stdout[:400])

# --- a hand seal by a registered identity ----------------------------------------------
reg = writ(p, "agent", "register", "alpha")
add_spec(p, "hand", "alpha")
(p / "shared.txt").write_text("alpha by hand\n")
r = writ(p, "seal", "-s", "hand", "--agent", "alpha", "--spec", "hand", "--paths", "shared.txt", agent="alpha")
s3 = newest(p)
chk("8 a hand seal by a registered identity is signed and verifies against its key",
    r.returncode == 0 and agent_of(s3) == "alpha" and bool(s3.get("signature"))
    and vjson(p, "--seal", s3["id"][:12])[1].get("signature_valid") is True, f"reg rc={reg.returncode} {(reg.stdout + reg.stderr)[:150]!r} sig={bool(s3.get('signature'))}")

# --- git never sees a key ---------------------------------------------------------------
st = run(p, ["git", "status", "--porcelain", "--untracked-files=all"]).stdout
ls = run(p, ["git", "ls-files", ".writ"]).stdout
ign = run(p, ["git", "check-ignore", "-q", f".writ/keys/{who}.enc"]).returncode
chk("9 git never sees a key: no .writ path in status or the index, the key file is ignored",
    ".writ/" not in st and not ls.strip() and ign == 0, f"status={st[:200]!r} ls={ls[:200]!r} ignored_rc={ign}")

# --- an altered signature fails --------------------------------------------------------
# On a build that signs nothing the tamper target is the registered hand seal (check 8),
# which 0.4.4 already signed: the control then shows whether verify checks at all (209).
target = s1 if s1.get("signature") else s3
f = p / ".writ/seals" / f"{target.get('id')}.json"
raw = json.loads(f.read_text()) if f.is_file() else {}
sig = raw.get("signature") or "0"
raw["signature"] = ("1" if sig[0] == "0" else "0") + sig[1:]
f.write_text(json.dumps(raw)) if f.is_file() else None
s1 = target
rc1, one = vjson(p, "--seal", s1["id"][:12])
chk("10 one hex digit of a signature flipped: `verify --seal` says signature_valid false, valid false, exit nonzero",
    rc1 != 0 and one.get("signature_valid") is False and one.get("valid") is False, f"rc={rc1} {json.dumps(one)[:300]}")
rc2, v = vjson(p)
chk("11 and `writ verify` (chain) fails, naming that seal",
    rc2 != 0 and v.get("valid") is False and s1["id"][:12] in json.dumps(v.get("failures")), f"rc={rc2} {json.dumps(v)[:300]}")

# --- what the key proves (the sentence the docs make) -----------------------------------
r = writ(p, "agent", "list")
chk("12 `writ agent list` says the first seal key proves continuity of one identity, not a person",
    who in r.stdout and "continu" in r.stdout.lower(), r.stdout[-400:])
done(12)
PY
rc=$?
echo "signed: exit $rc"
exit $rc
