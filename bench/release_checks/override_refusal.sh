#!/usr/bin/env bash
# Release check (0.4.1, finding 102): a seal that would drop lines this spec
# sealed is refused once with an explanation; a confirmed removal is recorded
# and not second guessed. Backs the Changed line ending "writ refuses once and
# explains; if you confirm the removal after that, writ records your
# confirmation and does not second guess it."
#   named: a seals "line 2 A"; b seals its own version without it; a keeps
#     working on top of b's version. a's seal is refused, names b, offers
#     "keep your lines" and "accept b's version" with the exact
#     --allow-removals command, never --force. The override then succeeds and
#     the seal carries override_context {path, other_agents, lines, added_by}.
#   nobody: no other agent; a's line is gone from disk. The refusal says the
#     lines are no longer on disk and no other agent worked on the file, offers "remove them on purpose";
#     the override's context has no other agent.
#
# Usage: bench/release_checks/override_refusal.sh [WRIT_BIN] [SCRATCH_DIR]
# Exit 0 when every check passes, 1 on any FAIL, 2 on bad arguments.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
. "$ROOT/bench/release_checks/_lib.sh"
W="$(rc_bin "${1:-$ROOT/target/release/writ}")"
echo "binary: $W ($("$W" --version))"
rc_setup .override_refusal_scratch writ-override-refusal "${2:-}"
w() { env -i HOME="$HOME" PATH=/usr/bin:/bin:/usr/sbin:/sbin "$W" "$@"; }
g() { git -c user.name=t -c user.email=t@localhost "$@"; }
mark() {  # mark "<n>:<suffix>" ...: 40 numbered lines, the given ones suffixed
  python3 -c "
import sys
m = dict((int(a.split(':')[0]), a.split(':')[1]) for a in sys.argv[1:])
open('shared.txt', 'w').write(''.join(f'line {i}' + (' ' + m[i] if i in m else '') + '\n' for i in range(40)))" "$@"
}
newest() { w log --all --format json --limit 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["id"])'; }
setup() {  # setup <dir> <with_b>: committed shared.txt; sa (agent-a) seals "line 2 A"
  rc_git_project "$1"
  w init -y > /dev/null 2>&1; git add -A; g commit -qm init
  # The base version is sealed and committed through writ, as in a real project.
  w spec add --id setup --title setup --agent human --claim > /dev/null
  mark; w seal -s base --agent human --spec setup --paths shared.txt > /dev/null 2>&1
  w spec done setup --agent human --no-seal > /dev/null 2>&1
  w finish -y --no-check > /dev/null 2>&1
  w spec add --id sa --title a --agent agent-a --claim > /dev/null
  [ "$2" = yes ] && w spec add --id sb --title b --agent agent-b --claim > /dev/null
  mark 2:A; w seal -s "a adds line 2 A" --agent agent-a --spec sa --paths shared.txt > /dev/null 2>&1
  ADDED="$(newest)"
}
ctx_ok() {  # ctx_ok <seal> <python expr over the override_context list c>
  w show "$1" --format json | python3 -c "import json,sys; c=json.load(sys.stdin).get('override_context') or []; sys.exit(0 if ($2) else 1)"
}

# named
setup "$D/named" yes
mark 30:B; w seal -s "b's version" --agent agent-b --spec sb --paths shared.txt > /dev/null 2>&1
mark 10:A2 30:B
rc=0; w seal -s "a on top of b" --agent agent-a --spec sa --paths shared.txt > "$D/named.refusal" 2>&1 || rc=$?
chk "named: a's seal on top of b's version is refused, names the lost line" \
  "[ $rc -ne 0 ] && grep -q 'line 2 A' $D/named.refusal"
chk "named: refusal names agent-b and offers keep your lines / accept agent-b's version" \
  "grep -q 'keep your lines' $D/named.refusal && grep -q \"accept agent-b's version of shared.txt\" $D/named.refusal"
chk "named: refusal gives the --allow-removals command and never --force" \
  "grep -q -- '--allow-removals shared.txt' $D/named.refusal && ! grep -q -- '--force' $D/named.refusal"
rc=0; w seal -s "a accepts b" --agent agent-a --spec sa --paths shared.txt --allow-removals shared.txt > "$D/named.override" 2>&1 || rc=$?
OV="$(newest)"
chk "named: the confirmed override seals (not second guessed)" "[ $rc -eq 0 ]"
chk "named: override_context has path, other_agents [agent-b], lines ['line 2 A'], added_by" \
  "ctx_ok $OV 'any(x[\"path\"]==\"shared.txt\" and \"agent-b\" in x[\"other_agents\"] and any(\"line 2 A\" in l for l in x[\"lines\"]) and any(\"$ADDED\".startswith(a) or a.startswith(\"$ADDED\"[:12]) for a in x[\"added_by\"]) for x in c)'"
chk "named: no fast-tier finding after the confirmed override" \
  "w doctor --format json > $D/named.doctor 2>/dev/null; rc_json $D/named.doctor 'r[\"clean\"]'"

# nobody
setup "$D/nobody" no
mark 30:B
rc=0; w seal -s "a's line gone" --agent agent-a --spec sa --paths shared.txt > "$D/nobody.refusal" 2>&1 || rc=$?
chk "nobody: refused, says the lines are no longer on disk and no other agent worked on the file" \
  "[ $rc -ne 0 ] && grep -q 'are no longer in shared.txt on disk' $D/nobody.refusal && grep -q 'No other agent has worked on shared.txt' $D/nobody.refusal && ! grep -q 'accept ' $D/nobody.refusal"
chk "nobody: offers 'remove them on purpose' with the --allow-removals command" \
  "grep -q 'remove them on purpose' $D/nobody.refusal && grep -q -- '--allow-removals shared.txt' $D/nobody.refusal"
rc=0; w seal -s "a removes on purpose" --agent agent-a --spec sa --paths shared.txt --allow-removals shared.txt > /dev/null 2>&1 || rc=$?
OV="$(newest)"
chk "nobody: override seals; context has path and line, no other agent" \
  "[ $rc -eq 0 ] && ctx_ok $OV 'any(x[\"path\"]==\"shared.txt\" and not x[\"other_agents\"] and any(\"line 2 A\" in l for l in x[\"lines\"]) for x in c)'"
# git base: the same "nobody" case on a file whose base version came from a git
# commit, not a writ seal (the usual case: files that predate writ init).
rc_git_project "$D/gitbase"
w init -y > /dev/null 2>&1; git add -A; g commit -qm init
mark; git add -A; g commit -qm shared
w spec add --id sa --title a --agent agent-a --claim > /dev/null
mark 2:A; w seal -s "a adds line 2 A" --agent agent-a --spec sa --paths shared.txt > /dev/null 2>&1
mark 30:B
rc=0; w seal -s "a's line gone" --agent agent-a --spec sa --paths shared.txt > "$D/gitbase.refusal" 2>&1 || rc=$?
chk "git base: the same removal is refused when shared.txt's base came from git, not a seal" \
  "[ $rc -ne 0 ] && grep -q 'line 2 A' $D/gitbase.refusal"
exit $fail
