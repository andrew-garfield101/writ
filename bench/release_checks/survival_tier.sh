#!/usr/bin/env bash
# Release check (0.4.1, survival tier): "safe to finish" appears only when the
# fast checks are clean and `writ doctor --survival` is green on the current
# state, and `writ finish` never commits a tree that drops another spec's
# sealed line without saying so.
#   1. fresh repo: headline says "survival not run", never "safe to finish"
#   2. two specs on separate files: survival green as of the newest seal, and
#      only then "safe to finish"; finish commits both
#   3. stale rewrite: spec a seals "line 2 A" and closes; agent b seals a
#      whole-file rewrite made from a copy taken before a's edit, which drops
#      "line 2 A". Either the seal is refused, or survival is not green and
#      finish refuses naming the line; in no case does HEAD lose the line
#      while doctor says "safe to finish".
#
# Usage: bench/release_checks/survival_tier.sh [WRIT_BIN] [SCRATCH_DIR]
# Exit 0 when every check passes, 1 on any FAIL, 2 on bad arguments.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
. "$ROOT/bench/release_checks/_lib.sh"
W="$(rc_bin "${1:-$ROOT/target/release/writ}")"
echo "binary: $W ($("$W" --version))"
rc_setup .survival_tier_scratch writ-survival-tier "${2:-}"
unset WRIT_AGENT_ID CLAUDECODE
g() { git -c user.name=t -c user.email=t@localhost "$@"; }
newest() { "$W" log --all --format json --limit 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["id"][:12])'; }
project() {  # project <dir>: git repo, writ init committed, 40-line shared.txt committed
  rc_git_project "$1"
  "$W" init -y > /dev/null 2>&1; git add -A; g commit -qm init
  python3 -c 'open("shared.txt","w").write("".join(f"line {i}\n" for i in range(40)))'
  git add -A; g commit -qm shared
}
mark() {  # mark <line> <suffix>: shared.txt with exactly that one line marked
  python3 -c "import sys; n=int(sys.argv[1]); open('shared.txt','w').write(''.join(f'line {i}' + (' ' + sys.argv[2] if i == n else '') + '\n' for i in range(40)))" "$1" "$2"
}

# 1. fresh
project "$D/fresh"
"$W" doctor > "$D/fresh.txt" 2>&1 || true
chk "fresh repo: survival not run, no 'safe to finish'" \
  "grep -q 'survival not run' $D/fresh.txt && ! grep -qi 'safe to finish' $D/fresh.txt"

# 2. green
project "$D/green"
"$W" spec add --id sa --title a --agent a --claim > /dev/null; "$W" spec add --id sb --title b --agent b --claim > /dev/null
echo a > a.txt; "$W" seal -s a --agent a --spec sa --paths a.txt > /dev/null 2>&1
echo b > b.txt; "$W" seal -s b --agent b --spec sb --paths b.txt > /dev/null 2>&1
n=$(newest)
for s in a b; do "$W" spec done s$s --agent $s --no-seal > /dev/null 2>&1; done
"$W" doctor > "$D/green.before" 2>&1 || true
"$W" doctor --survival --format json > "$D/green.json" 2>/dev/null || true
"$W" doctor > "$D/green.after" 2>&1 || true
chk "green: no 'safe to finish' before the survival tier has run" "! grep -qi 'safe to finish' $D/green.before"
chk "green: survival clean, survival_last_green is the newest seal ($n)" \
  "rc_json $D/green.json 'r[\"clean\"] and (r.get(\"survival_last_green\") or \"\").startswith(\"$n\")'"
chk "green: headline 'fast checks clean; survival green as of $n; safe to finish'" \
  "grep -q 'fast checks clean; survival green as of $n; safe to finish' $D/green.after"
"$W" finish -y > "$D/green.finish" 2>&1 || true
chk "green: finish commits both specs" "git show --stat HEAD | grep -q a.txt && git show --stat HEAD | grep -q b.txt"

# 3. stale rewrite
project "$D/stale"
"$W" spec add --id sa --title a --agent a --claim > /dev/null; "$W" spec add --id sb --title b --agent b --claim > /dev/null
mark 2 A; "$W" seal -s "a adds line 2 A" --agent a --spec sa --paths shared.txt > /dev/null 2>&1
"$W" spec done sa --agent a --no-seal > /dev/null 2>&1
mark 30 B   # b's copy predates a's edit
rc=0; "$W" seal -s "b rewrites shared.txt" --agent b --spec sb --paths shared.txt > "$D/stale.seal" 2>&1 || rc=$?
"$W" spec done sb --agent b --no-seal > /dev/null 2>&1 || true
"$W" doctor --survival > "$D/stale.survival" 2>&1 || true
"$W" finish -y > "$D/stale.finish" 2>&1 || true
head_has=$(git show HEAD:shared.txt | grep -c 'line 2 A' || true)
echo "stale: b seal rc=$rc; survival: $(head -1 $D/stale.survival); HEAD has line 2 A: $head_has"
chk "stale rewrite: no 'safe to finish' unless HEAD keeps 'line 2 A'" \
  "[ $head_has -ge 1 ] || ! grep -qi 'safe to finish' $D/stale.survival $D/stale.finish"
chk "stale rewrite: 'line 2 A' is never silently dropped (seal refused, or finish refuses naming it, or HEAD keeps it)" \
  "[ $rc -ne 0 ] || [ $head_has -ge 1 ] || grep -q 'line 2 A' $D/stale.finish"
exit $fail
