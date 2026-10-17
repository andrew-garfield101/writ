#!/usr/bin/env bash
# Claim (0.4.3 note, the receipt): "`writ finish` writes into the commit body the same
# facts `writ handoff` reports for the same range, from one code path: one line per spec
# with the agent's own closing words (or 'no summary from the agent'), one line per
# shared file with how it was resolved, refusals, and the survival line. `--receipt
# <path>` writes the same as markdown for a pull request."
#
# Fixture: Bri's handoff_states shape (receipt_states.rs): sa alpha, sb bravo, sc charlie;
# shared.txt overridden by alpha after being told; two.txt kept; closing words on sa and
# sb, none on sc. Checks against Bri's contract: body == `handoff --since <base> --format
# receipt` byte for byte; --receipt file == `--format pr`; under 40 lines; spec lines
# first; closing words verbatim; resolutions; the refusal; the survival line equals the
# finish's and finish_audit.py says 0 lost; deterministic.
#
# Optional real-repo mode (this repo's own finish at the 0.4.3 close): set RECEIPT_REPO
# and RECEIPT_SINCE (the seal before the finished range); check R1 compares HEAD's body
# with that repo's `handoff --since $RECEIPT_SINCE --format receipt`. Read only.
#
# Usage: receipt.sh [WRIT_BIN] [SCRATCH_DIR]
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
. "$ROOT/bench/release_checks/_lib.sh"
W="$(rc_bin "${1:-$ROOT/target/release/writ}")"
echo "binary: $W ($("$W" --version))"
rc_setup .receipt_scratch writ-receipt "${2:-}"
mkdir home
# Ruling 183 (0.4.3): a hand seal that drops another spec's sealed lines is refused at seal
# time. This script's fixture builds that divergence on purpose to test what finish, doctor
# and handoff do with it, so the fixture seals with `[seal] foreign_line_check = false` (the
# test-fixture setting) and every diagnostic runs with the key removed again: fx_key on|off.
fx_key() { [ -f .writ/config.toml ] || return 0
  python3 - "$1" <<'FXPY'
import re, sys
p = ".writ/config.toml"; s = open(p).read()
s = re.sub(r"\n?\[seal\]\nforeign_line_check = false\n", "\n", s)
if sys.argv[1] == "on":
    s = s.rstrip("\n") + "\n\n[seal]\nforeign_line_check = false\n"
open(p, "w").write(s)
FXPY
}
w_raw() { local who="$1"; shift; env -i HOME="$D/home" PATH=/usr/bin:/bin:/usr/sbin:/sbin \
      WRIT_AGENT_ID="$who" WRIT_EXCLUDES_FILE="$D/home/none" GIT_CONFIG_NOSYSTEM=1 \
      GIT_AUTHOR_NAME=fixture GIT_AUTHOR_EMAIL=fixture@example.invalid \
      GIT_COMMITTER_NAME=fixture GIT_COMMITTER_EMAIL=fixture@example.invalid "$W" "$@" < /dev/null; }
w() { local a; for a in "$@"; do case "$a" in doctor|finish|handoff|why|context) fx_key off; w_raw "$@"; local rc=$?; fx_key on; return $rc ;; seal|spec|log|show|init|status|diff) break ;; esac; done; w_raw "$@"; }
numbered() { python3 -c 'import sys; n=int(sys.argv[1]); e=dict(a.split("=",1) for a in sys.argv[2:]); print("".join(f"line {i} {e[str(i)]}\n" if str(i) in e else f"line {i}\n" for i in range(n)), end="")' "$@"; }
WA="alpha words: moved the parser"
WB="bravo words: two.txt and the tail"

rc_git_project "$D/p"
git config user.name fixture; git config user.email fixture@example.invalid
printf '# fixture\n' > README.md; printf 'target/\n' > .gitignore
numbered 40 > shared.txt; numbered 20 > two.txt
git add -A; git -c user.name=t -c user.email=t@localhost commit -qm fixture
w human init -y > /dev/null 2>&1; fx_key on; git add -A; git -c user.name=t -c user.email=t@localhost commit -qm "writ init" --no-verify
SINCE="$(w human log --all --format json --limit 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["id"])')"
for s in sa:alpha sb:bravo sc:charlie; do
    w "${s#*:}" spec add --id "${s%%:*}" --title "spec ${s%%:*}" > /dev/null
    w "${s#*:}" spec claim "${s%%:*}" --agent "${s#*:}" > /dev/null
done
numbered 40 2=A > shared.txt;  w alpha seal -s work --agent alpha --spec sa --paths shared.txt > /dev/null 2>&1
numbered 40 30=B > shared.txt; w bravo seal -s work --agent bravo --spec sb --paths shared.txt > /dev/null 2>&1
numbered 20 5=B > two.txt;     w bravo seal -s work --agent bravo --spec sb --paths two.txt > /dev/null 2>&1
numbered 20 5=B 15=C > two.txt; printf 'c\n' > c.txt
w charlie seal -s work --agent charlie --spec sc --paths two.txt,c.txt > /dev/null 2>&1
numbered 40 10=A2 30=B > shared.txt
w alpha seal -s work --agent alpha --spec sa --paths shared.txt > "$D/told" 2>&1
TOLD_RC=$?
w alpha seal -s work --agent alpha --spec sa --paths shared.txt --allow-removals shared.txt > /dev/null 2>&1
w alpha spec done sa --agent alpha -s "$WA" --no-seal > /dev/null 2>&1
w bravo spec done sb --agent bravo -s "$WB" --no-seal > /dev/null 2>&1
w charlie spec done sc --agent charlie --no-seal > /dev/null 2>&1
chk "0 fixture: alpha's overwrite was refused naming bravo" '[ "$TOLD_RC" -ne 0 ] && grep -q bravo "$D/told"'

HEAD0="$(git rev-parse HEAD)"
w human finish -y --receipt "$D/receipt.md" > "$D/finish.out" 2>&1; FRC=$?
git log -1 --format=%b > "$D/body.txt"
w human handoff --since "$SINCE" --format receipt > "$D/h1.txt" 2>"$D/h1.err"; HRC=$?
w human handoff --since "$SINCE" --format receipt > "$D/h2.txt" 2>/dev/null
w human handoff --since "$SINCE" --format pr > "$D/pr.md" 2>"$D/pr.err"
WRIT_AUDIT_REPO="$D/p" python3 "$ROOT/bench/finish_audit.py" "$(git rev-parse HEAD)" > "$D/audit.txt" 2>&1 || true

chk "1 finish -y --receipt exits 0 and makes one commit" '[ "$FRC" -eq 0 ] && [ "$(git rev-list --count "$HEAD0"..HEAD)" -eq 1 ]'
chk "2 commit body equals handoff --format receipt for the same range, byte for byte" \
    '[ "$HRC" -eq 0 ] && [ -s "$D/body.txt" ] && python3 -c "import sys; a,b=(open(f).read().strip() for f in sys.argv[1:]); sys.exit(0 if a==b else 1)" "$D/body.txt" "$D/h1.txt"'
chk "3 --receipt file equals handoff --format pr" \
    '[ -s "$D/receipt.md" ] && python3 -c "import sys; a,b=(open(f).read().strip() for f in sys.argv[1:]); sys.exit(0 if a==b else 1)" "$D/receipt.md" "$D/pr.md"'
NL="$(grep -c . "$D/body.txt")"
chk "4 receipt present and under 40 lines ($NL)" '[ "$NL" -gt 0 ] && [ "$NL" -lt 40 ]'
chk "5 the spec lines come before the shared file lines" \
    'python3 -c "
import sys; L=open(sys.argv[1]).read().splitlines()
s=[i for i,l in enumerate(L) if any(l.lstrip(\"- \").startswith(x+\" \") for x in (\"sa\",\"sb\",\"sc\"))]
f=[i for i,l in enumerate(L) if l.lstrip(\"- \").startswith(\"shared.txt\")]
sys.exit(0 if s and f and max(s) < min(f) else 1)" "$D/body.txt"'
chk "6 closing words verbatim for sa and sb; sc reads 'no summary from the agent'" \
    'grep -qF "$WA" "$D/body.txt" && grep -qF "$WB" "$D/body.txt" && grep -E "(^|[^a-z])sc([^a-z]|$)" "$D/body.txt" | grep -qF "no summary from the agent"'
chk "7 shared.txt resolved 'accepted after being told' naming alpha; two.txt kept" \
    'grep -F shared.txt "$D/body.txt" | grep -F "accepted after being told" | grep -qF alpha && grep -F two.txt "$D/body.txt" | grep -qF kept'
chk "8 the refusal during the range is listed (shared.txt)" 'grep -iE "refus" "$D/body.txt" | grep -qF shared.txt'
SURV="$(grep -oE '[0-9,]+ sealed line\(s\) of [0-9]+ spec\(s\) checked, none lost' "$D/finish.out" | head -1)"
chk "9 survival line equals the finish's ('$SURV') and finish_audit says 0 lost" \
    '[ -n "$SURV" ] && grep -qF "$SURV" "$D/body.txt" && grep -q "LOST: 0" "$D/audit.txt"'
chk "10 deterministic: two handoff receipts are identical" '[ -s "$D/h1.txt" ] && cmp -s "$D/h1.txt" "$D/h2.txt"'
chk "11 records only: names alpha, bravo and charlie and no agent outside the records" \
    'grep -q alpha "$D/body.txt" && grep -q bravo "$D/body.txt" && grep -q charlie "$D/body.txt" && ! grep -qiwE "djo|amis|haris|bri|claude|claude-code" "$D/body.txt"'

if [ -n "${RECEIPT_REPO:-}" ] && [ -n "${RECEIPT_SINCE:-}" ]; then
    ( cd "$RECEIPT_REPO" && git log -1 --format=%b > "$D/real.body" \
      && env -u WRIT_AGENT_ID "$W" handoff --since "$RECEIPT_SINCE" --format receipt > "$D/real.h" 2>/dev/null )
    chk "R1 $RECEIPT_REPO HEAD body equals handoff --since $RECEIPT_SINCE --format receipt" \
        'python3 -c "import sys; a,b=(open(f).read().strip() for f in sys.argv[1:]); sys.exit(0 if a and a==b else 1)" "$D/real.body" "$D/real.h"'
fi
echo "--- receipt (commit body)"; cat "$D/body.txt"
exit $fail
