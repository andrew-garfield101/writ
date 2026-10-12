#!/usr/bin/env bash
# Release check (0.4.2, finding 114): when a claim's holder has exited, doctor's
# stale_claim finding says so and says any agent may run the fix, carries
# `anyone_may_fix: true` (doctor JSON, human output, context JSON and default
# context), and the fix pasted by an uninvolved agent clears it; a second paste
# by a third agent exits 0 and says the claim is already released. Negatives:
# no finding about a live claim's spec or files carries the flag, for any
# viewer including the holder; a finding marked for a human never carries it.
# The managed CLAUDE.md block tells agents to act on such findings.
#
# Usage: bench/release_checks/stale_anyone_may_fix.sh [WRIT_BIN] [SCRATCH_DIR]
# Exit 0 when every check passes, 1 on any FAIL, 2 on bad arguments.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
. "$ROOT/bench/release_checks/_lib.sh"
W="$(rc_bin "${1:-$ROOT/target/release/writ}")"
echo "binary: $W ($("$W" --version))"
rc_setup .stale_anyone_scratch writ-stale-anyone "${2:-}"
unset WRIT_AGENT_ID CLAUDECODE
mkdir bin; ln -s "$W" bin/writ; ln -s /bin/sh bin/claude
export PATH="$D/bin:/usr/bin:/bin:/usr/sbin:/sbin"
rc_git_project "$D/p"
writ init -y > init.log 2>&1
as() { local who="$1"; shift; WRIT_AGENT_ID="$who" writ "$@"; }

chk "the managed CLAUDE.md block says to act on any finding not marked for a human, other agents' specs included" \
  "grep -q 'not marked for a human' CLAUDE.md && grep -q \"other agents' specs\" CLAUDE.md"

# The run 4 shape: a third session claims ghost and exits (same host, dead pid).
as human spec add --id ghost --title ghost > /dev/null
claude -c "writ spec claim ghost --agent ghost-session && :" > /dev/null
as other doctor --format json > d1.json 2>/dev/null || true
F='[f for f in r["findings"] if f["check"]=="stale_claim"]'
chk "stale_claim says the holder has exited and any agent may run the fix" \
  "rc_json d1.json 'any(\"has exited\" in f[\"message\"] and \"any agent may run\" in f[\"message\"] for f in $F)'"
chk "doctor JSON: anyone_may_fix true, needs_human false, fix is --stale" \
  "rc_json d1.json 'any(f[\"anyone_may_fix\"] is True and f[\"needs_human\"] is False and f[\"fix_command\"]==\"writ spec release ghost --stale\" for f in $F)'"
as other doctor > d1.txt 2>&1 || true
chk "human doctor output carries the message" "grep -q 'has exited' d1.txt && grep -q 'any agent may run' d1.txt"
as other context --format json --for-agent other > c1.json 2>/dev/null
chk "context JSON doctor block: anyone_may_fix true" \
  "rc_json c1.json 'any(f[\"check\"]==\"stale_claim\" and f[\"anyone_may_fix\"] is True for f in r[\"doctor\"][\"findings\"])'"
as other context --for-agent other > c1.txt 2>&1
chk "default context says any agent may run the fix" "grep -q 'any agent may run' c1.txt"
FIX="$(python3 -c 'import json; r=json.load(open("d1.json")); print(next(f["fix_command"] for f in r["findings"] if f["check"]=="stale_claim"))')"
rc=0; WRIT_AGENT_ID=other sh -c "$FIX" > fix1.out 2>&1 || rc=$?
as other doctor --format json > d2.json 2>/dev/null || true
chk "an uninvolved agent pastes the fix: exit 0, the stale_claim is gone, not forced" \
  "[ $rc -eq 0 ] && rc_json d2.json 'not any(f[\"check\"]==\"stale_claim\" for f in r[\"findings\"])' && rc_json .writ/specs/ghost.json 'r.get(\"claimed_by\") is None and not r.get(\"release_forced\")'"
rc=0; WRIT_AGENT_ID=third sh -c "$FIX" > fix2.out 2>&1 || rc=$?
chk "a third agent pastes it again: exit 0 and says already" \
  "[ $rc -eq 0 ] && grep -qi already fix2.out"

# Negative 1: a's live claim with an old unsealed edit while b is active.
as a spec add --id sa --title sa > /dev/null; as a spec claim sa --agent a > /dev/null
as b spec add --id sb --title sb > /dev/null; as b spec claim sb --agent b > /dev/null
printf 'a1\n' > a.txt; as a seal -s work --agent a --spec sa --paths a.txt > /dev/null 2>&1
printf 'a1\na2\n' > a.txt; touch -t "$(date -v-45M +%Y%m%d%H%M.%S)" a.txt
printf 'b1\n' > b.txt; as b seal -s work --agent b --spec sb --paths b.txt > /dev/null 2>&1
ok=1
for viewer in other human b a; do
  as "$viewer" doctor --format json > "live.$viewer.json" 2>/dev/null || true
  rc_json "live.$viewer.json" 'not any(f["anyone_may_fix"] and ("sa" in (f["fix_command"] or "").split() or "a.txt" in f.get("paths", []) or "a.txt" in (f["fix_command"] or "")) for f in r["findings"])' || ok=0
  rc_json "live.$viewer.json" 'any(f["check"]=="unsealed_at_risk" for f in r["findings"])' || ok=0
done
chk "unsealed_at_risk on a's live claim is reported and never anyone_may_fix, for other, human, b and a" "[ $ok -eq 1 ]"

# Negative 2: a claim writ cannot prove abandoned (another host, idle past the
# limit) is marked for a human and never anyone_may_fix.
as human spec add --id far --title far > /dev/null; as far-session spec claim far --agent far-session > /dev/null
python3 - <<'PY'
import json, datetime
p = ".writ/specs/far.json"; d = json.load(open(p)); d["claimed_host"] = "other-host.invalid"
old = (datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(days=2)).isoformat().replace("+00:00", "Z")
d["last_activity"] = old; d["updated_at"] = old
json.dump(d, open(p, "w"), indent=2)
PY
as other doctor --format json > far.json 2>/dev/null || true
chk "an unprovable stale claim is needs_human and never anyone_may_fix" \
  "rc_json far.json 'any(f[\"check\"]==\"stale_claim\" and \"far\" in f[\"message\"] and f[\"needs_human\"] is True and f[\"anyone_may_fix\"] is False for f in r[\"findings\"])'"
chk "every finding carries anyone_may_fix; when true, a fix and needs_human false" \
  "( for j in d1.json far.json live.*.json; do rc_json \$j 'all(isinstance(f.get(\"anyone_may_fix\"), bool) and (not f[\"anyone_may_fix\"] or (f[\"fix_command\"] and not f[\"needs_human\"])) for f in r[\"findings\"])' || exit 1; done )"
exit $fail
