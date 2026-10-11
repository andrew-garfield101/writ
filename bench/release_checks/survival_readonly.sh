#!/usr/bin/env bash
# Release check (0.4.1, finding 110): `writ doctor --survival` writes nothing
# to the store. On a store where two specs sealed different parts of one file
# (a merge `writ finish` would make), a doctor --survival run leaves the object
# count and .writ/stale_rewrite_notices.json byte for byte as they were, and
# every other store file too (convergence_v3_pending.json included); only
# doctor's caches, security events and version.toml's last_opened_at may move.
# The run must still have done its work: the merge is audited (survival green
# or a named finding), and a finish afterwards still commits both specs' lines.
#
# Usage: bench/release_checks/survival_readonly.sh [WRIT_BIN] [SCRATCH_DIR]
# Exit 0 when every check passes, 1 on any FAIL, 2 on bad arguments.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
. "$ROOT/bench/release_checks/_lib.sh"
W="$(rc_bin "${1:-$ROOT/target/release/writ}")"
echo "binary: $W ($("$W" --version))"
rc_setup .survival_readonly_scratch writ-survival-readonly "${2:-}"
w() { env -i HOME="$HOME" PATH=/usr/bin:/bin:/usr/sbin:/sbin "$W" "$@"; }
g() { git -c user.name=t -c user.email=t@localhost "$@"; }
mark() {
  python3 -c "
import sys
m = dict((int(a.split(':')[0]), a.split(':')[1]) for a in sys.argv[1:])
open('shared.txt', 'w').write(''.join(f'line {i}' + (' ' + m[i] if i in m else '') + '\n' for i in range(40)))" "$@"
}
rc_git_project "$D/p"
w init -y > /dev/null 2>&1; git add -A; g commit -qm init
mark; git add -A; g commit -qm shared
w spec add --id sa --title a --agent agent-a --claim > /dev/null
w spec add --id sb --title b --agent agent-b --claim > /dev/null
mark 5:A; w seal -s "a edits line 5" --agent agent-a --spec sa --paths shared.txt > /dev/null 2>&1
mark 5:A 30:B; w seal -s "b edits line 30" --agent agent-b --spec sb --paths shared.txt > /dev/null 2>&1
w spec done sa --agent agent-a --no-seal > /dev/null 2>&1
w spec done sb --agent agent-b --no-seal > /dev/null 2>&1
objects() { find .writ/objects -type f | wc -l | tr -d ' '; }
notices() { if [ -f .writ/stale_rewrite_notices.json ]; then shasum -a 256 .writ/stale_rewrite_notices.json | cut -c1-64; else echo absent; fi; }
store() {  # every store file except doctor's own caches, security events and version.toml's open stamp (finding 34)
  find .writ -type f ! -name survival_cache.json ! -name doctor_cache.json ! -name version.toml ! -path '*/security/*' -exec shasum -a 256 {} + | sort -k2
  grep -v '^last_opened_at' .writ/version.toml | shasum -a 256
}
o1=$(objects); n1=$(notices); store > "$D/store.before"
w doctor --survival --format json > "$D/survival.json" 2>/dev/null || true
o2=$(objects); n2=$(notices); store > "$D/store.after"
echo "objects $o1 -> $o2; notices $n1 -> $n2"
chk "doctor --survival ran and audited the specs" "rc_json $D/survival.json 'r.get(\"tier\") in (\"survival\", \"full\") or r.get(\"survival_last_green\") is not None or any(f[\"check\"]==\"survival\" for f in r[\"findings\"])'"
chk "object count unchanged by doctor --survival ($o1)" "[ $o1 -eq $o2 ]"
chk "stale_rewrite_notices.json byte for byte unchanged" "[ '$n1' = '$n2' ]"
chk "no other store file changed, convergence_v3_pending.json included (caches, security/, last_opened_at excluded)" "cmp -s $D/store.before $D/store.after"
w finish -y > "$D/finish.log" 2>&1 || true
chk "finish afterwards still commits both specs' lines" \
  "git show HEAD:shared.txt | grep -q 'line 5 A' && git show HEAD:shared.txt | grep -q 'line 30 B'"
exit $fail
