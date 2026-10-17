#!/usr/bin/env bash
# Claims (0.4.3 note, Fixed; findings 145 and 180, decision 166):
#   "A seal you run by hand is now refused the same way the hook's is when it would record
#    the removal of lines another agent's spec sealed. writ names the other agent and the
#    lines; merge their lines back, or seal with --allow-removals to take their version."
#   "writ spec done refuses while the survival check is red."
#
# Fixture: bravo seals line 30 of shared.txt under sb; alpha writes the file from a stale
# read (line 30 gone) and seals by hand under sa.
#   1 alpha's hand seal exits non-zero, seals nothing, names bravo and the file
#   2 the refusal offers --allow-removals (and never --force)
#   3 with --allow-removals the seal lands and the record says "accepted bravo's version"
#   4 alpha replacing its own sealed line by hand is unchanged: not a foreign removal, no
#     mention of bravo, sealed as alpha's own (0.4.2 and freeze 3 seal it with no refusal)
#   5 merge path: alpha restores bravo's line and its plain hand seal is accepted
#   6 spec done over a red survival tier is refused naming the lost lines; nothing closed
#   7 after the lines come back, spec done succeeds
#  15 spec done prints its survival time on refusal and on success (finding 181; budget 5 s)
# Ruling 183: the check is on in every agent-facing surface and can be turned off only by
# `[seal] foreign_line_check = false` in .writ/config.toml (test fixtures):
#   8 init -y never writes the key
#   9 with the key false, doctor JSON has a yellow naming the disabled check, needs_human,
#     fix removes the key; doctor text shows it
#  10 with the key false, context carries it
#  11 with the key false, the foreign hand seal goes through, and finish loses nothing
#  12 the refusal output never prints the config key
#  13 with the key false, the receipt says "N seal(s) made with the foreign-line check disabled in config"
#  14 without the key, the receipt has no such line
#
# Usage: hand_seal_foreign.sh [WRIT_BIN] [SCRATCH_DIR]
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/_lib.sh"
ROOT="$(cd "$HERE/../.." && pwd)"
W="$(rc_bin "${1:-$ROOT/target/release/writ}")"
unset WRIT_AGENT_ID
rc_setup .hand_seal_foreign hand-seal-foreign "${2:-}"
echo "binary: $W ($("$W" --version))"
mkdir home
w() { local who="$1"; shift; env -i HOME="$D/home" PATH=/usr/bin:/bin:/usr/sbin:/sbin \
      WRIT_AGENT_ID="$who" WRIT_EXCLUDES_FILE="$D/home/none" GIT_CONFIG_NOSYSTEM=1 "$W" "$@" < /dev/null; }
numbered() { python3 -c 'import sys; n=int(sys.argv[1]); e=dict(a.split("=",1) for a in sys.argv[2:]); print("".join(f"line {i} {e[str(i)]}\n" if str(i) in e else f"line {i}\n" for i in range(n)), end="")' "$@"; }
nseals() { w human log --all --format json --limit 1000 | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))'; }
newest() { w human log --all --format json --limit 1 | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)[0]))'; }

project() {
    rc_git_project "$1"
    git config user.name fixture; git config user.email fixture@example.invalid
    numbered 40 > shared.txt; git add -A; git commit -qm fixture
    w human init -y > /dev/null 2>&1; git add -A; git commit -qm "writ init" --no-verify
    for s in sa:alpha sb:bravo; do
        w "${s#*:}" spec add --id "${s%%:*}" --title "spec ${s%%:*}" > /dev/null 2>&1
        w "${s#*:}" spec claim "${s%%:*}" --agent "${s#*:}" > /dev/null 2>&1
    done
    numbered 40 30=B > shared.txt; w bravo seal -s bravo --agent bravo --spec sb --paths shared.txt > /dev/null 2>&1
}

# --- foreign removal, refused -----------------------------------------------------------
project "$D/p1"
numbered 40 2=A > shared.txt                      # stale read: bravo's line 30 is gone
N0="$(nseals)"
w alpha seal -s alpha --agent alpha --spec sa --paths shared.txt > "$D/refused.txt" 2>&1; RC=$?
chk "1 hand seal dropping bravo's sealed line: refused, nothing sealed, names bravo and shared.txt" \
    '[ "$RC" -ne 0 ] && [ "$(nseals)" = "$N0" ] && grep -q bravo "$D/refused.txt" && grep -q shared.txt "$D/refused.txt"'
chk "2 the refusal offers --allow-removals and never --force" \
    'grep -q -- "--allow-removals" "$D/refused.txt" && ! grep -q -- "--force" "$D/refused.txt"'
w alpha seal -s alpha --agent alpha --spec sa --paths shared.txt --allow-removals shared.txt > "$D/allowed.txt" 2>&1; RC=$?
chk "3 with --allow-removals the seal lands; the record says alpha accepted bravo's version" \
    '[ "$RC" -eq 0 ] && grep -q "accepted bravo'"'"'s version" "$D/allowed.txt"'
chk "12 the refusal never prints the config key (no how-to for turning the check off)" 'grep -q -- --allow-removals "$D/refused.txt" && ! grep -q foreign_line_check "$D/refused.txt"'
sed -n 1,12p "$D/refused.txt"

# --- own-line removal: unchanged -----------------------------------------------------------
project "$D/p2"
numbered 40 2=A 30=B > shared.txt; w alpha seal -s a1 --agent alpha --spec sa --paths shared.txt > /dev/null 2>&1
numbered 40 30=B > shared.txt
w alpha seal -s a2 --agent alpha --spec sa --paths shared.txt > "$D/own.txt" 2>&1; R1=$?
[ "$R1" -ne 0 ] && w alpha seal -s a2 --agent alpha --spec sa --paths shared.txt --allow-removals shared.txt > /dev/null 2>&1
chk "4 alpha replacing its own sealed line: not treated as a foreign removal (no mention of bravo), and it ends sealed as alpha's own" \
    '! grep -q bravo "$D/own.txt" && newest | grep -q "\"summary\": \"a2\"" && ! newest | grep -q "accepted bravo"'

# --- merge path --------------------------------------------------------------------------
project "$D/p3"
numbered 40 2=A 30=B > shared.txt
w alpha seal -s merged --agent alpha --spec sa --paths shared.txt > "$D/merged.txt" 2>&1; RC=$?
chk "5 alpha merges bravo's line back: plain hand seal accepted" '[ "$RC" -eq 0 ]'
w alpha spec done sa --agent alpha -s a --no-seal > /dev/null 2>&1; w bravo spec done sb --agent bravo -s b --no-seal > /dev/null 2>&1
w human finish -y > "$D/p3.finish" 2>&1
chk "14 without the key: finish commits and the receipt has no disabled-check line" \
    '[ "$(git log -1 --format=%s)" != "writ init" ] && ! git log -1 --format=%B | grep -q "foreign-line check disabled"'

# --- spec done over red ------------------------------------------------------------------
# bravo's sealed line vanishes from disk with no seal (an overwrite nobody sealed): red.
project "$D/p4"
numbered 40 > shared.txt
w human doctor --survival > "$D/red.txt" 2>&1
w bravo spec done sb --agent bravo -s "bravo done" --no-seal > "$D/done.txt" 2>&1; RC=$?
STATUS="$(w human spec show sb --format json 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status"))')"
chk "6 spec done while survival is red: refused, names the lost line, spec still open" \
    'grep -q "survival red" "$D/red.txt" && [ "$RC" -ne 0 ] && grep -q shared.txt "$D/done.txt" && [ "$STATUS" != complete ]'
numbered 40 30=B > shared.txt
w bravo spec done sb --agent bravo -s "bravo done" --no-seal > "$D/done2.txt" 2>&1; RC=$?
chk "7 once the line is back, spec done succeeds" '[ "$RC" -eq 0 ]'
chk "15 spec done prints its survival check time, refusing and succeeding (181)" \
    'grep -qE "survival check [0-9]+ ms" "$D/done.txt" && grep -qE "survival: .*\([0-9]+ ms\)" "$D/done2.txt"'
# --- ruling 183: the opt-out key -------------------------------------------------------
project "$D/p5"
chk "8 init -y never writes foreign_line_check" '! grep -q foreign_line_check .writ/config.toml'
printf '\n[seal]\nforeign_line_check = false\n' >> .writ/config.toml
w human doctor --format json > "$D/off.json" 2>/dev/null
w human doctor > "$D/off.txt" 2>&1
chk "9 key false: doctor yellow names the disabled check, needs_human, fix removes the key; text shows it" \
    'python3 - "$D/off.json" <<PY && grep -qi "foreign-line check" "$D/off.txt"
import json, sys
r = json.load(open(sys.argv[1]))
f = [x for x in r.get("findings", []) if "foreign-line check" in (x.get("message") or "").lower()]
ok = bool(f) and f[0].get("severity") == "yellow" and f[0].get("needs_human") is True and "finish check still applies" in f[0]["message"].lower() and "foreign_line_check" in json.dumps(f[0])
sys.exit(0 if ok else 1)
PY'
w human context --format brief > "$D/off.ctx" 2>&1
chk "10 key false: context carries the disabled-check line" 'grep -qi "foreign-line check" "$D/off.ctx"'
numbered 40 2=A > shared.txt
w alpha seal -s alpha --agent alpha --spec sa --paths shared.txt > "$D/off.seal" 2>&1; SRC=$?
w alpha spec done sa --agent alpha -s a --no-seal > /dev/null 2>&1
w bravo spec done sb --agent bravo -s b --no-seal > /dev/null 2>&1
H0="$(git rev-parse HEAD)"
w human finish -y > "$D/off.finish" 2>&1; FRC=$?
chk "11 key false: the foreign hand seal goes through, and finish loses nothing (refuses, or commits with bravo's line kept)" \
    '[ "$SRC" -eq 0 ] && { { [ "$FRC" -ne 0 ] && [ "$(git rev-parse HEAD)" = "$H0" ]; } || { [ "$FRC" -eq 0 ] && git show HEAD:shared.txt | grep -q "line 30 B"; }; }'
RECEIPT_RE="[0-9]+ seal\(s\) made with the foreign-line check disabled in config"
chk "13 key false: the finish commit's receipt says how many seals were made with the check disabled" \
    '[ "$FRC" -ne 0 ] || git log -1 --format=%B | grep -qE "$RECEIPT_RE"'
exit $fail
