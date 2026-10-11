#!/usr/bin/env bash
# Release check (0.4.1, finding 91): `writ spec release <id> --stale` releases a
# claim only when its holder is provably gone (same host, pid exited), refuses a
# live holder and a claim from another host while naming the evidence and the
# human's --force, never records the release as forced, and doctor's stale_claim
# fix is `--stale`; no doctor fix_command contains --force.
#
# Usage: bench/release_checks/stale_release.sh [WRIT_BIN] [SCRATCH_DIR]
# Exit 0 when every check passes, 1 on any FAIL, 2 on bad arguments.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
. "$ROOT/bench/release_checks/_lib.sh"
W="$(rc_bin "${1:-$ROOT/target/release/writ}")"
echo "binary: $W ($("$W" --version))"
rc_setup .stale_release_scratch writ-stale-release "${2:-}"
unset WRIT_AGENT_ID CLAUDECODE
# A session is a process named claude (sh under that name) that claims and exits.
mkdir bin; ln -s "$W" bin/writ; ln -s /bin/sh bin/claude
export PATH="$D/bin:/usr/bin:/bin:/usr/sbin:/sbin"
rc_git_project "$D/p"
writ init -y > init.log 2>&1
for s in ghost live far; do writ spec add --id $s --title $s --agent human > /dev/null; done
claude -c "writ spec claim ghost --agent ghost-session && :" > /dev/null
claude -c "writ spec claim live --agent live-session && sleep 60" > /dev/null 2>&1 &
live_pid=$!; trap 'kill $live_pid 2>/dev/null || true' EXIT
for _ in $(seq 50); do grep -q live-session .writ/specs/live.json 2>/dev/null && break; sleep 0.1; done
writ spec claim far --agent far-session > /dev/null
python3 - <<'PY'
import json
p = ".writ/specs/far.json"; d = json.load(open(p)); d["claimed_host"] = "other-host.invalid"
json.dump(d, open(p, "w"), indent=2)
PY
writ doctor --format json > doctor.json 2>/dev/null || true
chk "doctor's stale_claim fix for the exited session is 'writ spec release ghost --stale'" \
  "rc_json doctor.json 'any(f[\"check\"]==\"stale_claim\" and f[\"fix_command\"]==\"writ spec release ghost --stale\" for f in r[\"findings\"])'"
chk "no doctor fix_command contains --force" \
  "rc_json doctor.json 'not any(\"--force\" in (f[\"fix_command\"] or \"\") for f in r[\"findings\"])'"
rc=0; writ spec release live --stale --agent other > live.out 2>&1 || rc=$?
chk "--stale refuses a live holder (says it is alive) and names the human's --force" \
  "[ $rc -ne 0 ] && grep -q 'holder is alive' live.out && grep -q 'writ spec release live --force' live.out && grep -q live-session .writ/specs/live.json"
rc=0; writ spec release far --stale --agent other > far.out 2>&1 || rc=$?
chk "--stale refuses a claim from another host and names the host" \
  "[ $rc -ne 0 ] && grep -q \"other-host.invalid\" far.out && grep -q far-session .writ/specs/far.json"
rc=0; writ spec release ghost --stale --agent other > ghost.out 2>&1 || rc=$?
chk "--stale releases the exited session's claim, not recorded as forced" \
  "[ $rc -eq 0 ] && rc_json .writ/specs/ghost.json 'r.get(\"claimed_by\") is None and not r.get(\"release_forced\")'"
exit $fail
