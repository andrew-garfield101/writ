#!/usr/bin/env bash
# Release check (0.4.1, finding 97): once every spec is committed by
# `writ finish`, their seal chains are not diverged branches: the brief context
# reads risk low with no converge action and no next action at all (nothing is
# committable), and doctor is clean. Before finish, the same store still asks
# to converge (the check is not blind). On 0.4.0 the committed store kept
# "2 diverged branch(es)", risk medium, next converge at high priority.
#
# Usage: bench/release_checks/committed_chains.sh [WRIT_BIN] [SCRATCH_DIR]
# Exit 0 when every check passes, 1 on any FAIL, 2 on bad arguments.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
. "$ROOT/bench/release_checks/_lib.sh"
W="$(rc_bin "${1:-$ROOT/target/release/writ}")"
echo "binary: $W ($("$W" --version))"
rc_setup .committed_chains_scratch writ-committed-chains "${2:-}"
unset WRIT_AGENT_ID CLAUDECODE
rc_git_project "$D/p"
"$W" init -y > "$D/init.log" 2>&1
for i in 1 2 3; do "$W" spec add --id s$i --title "spec $i" --agent a$i --claim > /dev/null; done
# Interleaved seals on three spec chains fork them (seal isolation).
for r in 1 2 3; do
  for i in 1 2 3; do
    echo "v$i-$r" >> f$i.txt
    "$W" seal -s "s$i round $r" --agent a$i --spec s$i --paths f$i.txt > /dev/null 2>&1
  done
done
"$W" context --format brief > "$D/brief.before" 2>&1
for i in 1 2 3; do "$W" spec done s$i --agent a$i --no-seal > /dev/null 2>&1; done
"$W" finish -y > "$D/finish.log" 2>&1
"$W" context --format brief > "$D/brief.after" 2>&1
"$W" doctor --format json > "$D/doctor.json" 2>/dev/null || true
chk "before finish the forked chains ask to converge" "grep -q 'action: converge' $D/brief.before"
chk "finish committed all three specs" "[ \$(git log --oneline | wc -l) -eq 2 ] && git show --stat HEAD | grep -q f3.txt"
chk "after finish: risk low, score 0" "grep -A2 '^risk:' $D/brief.after | grep -q 'level: low' && grep -A2 '^risk:' $D/brief.after | grep -q 'score: 0'"
chk "after finish: no converge and no next action" "! grep -q 'converge' $D/brief.after && ! grep -q '^next:' $D/brief.after"
chk "after finish: doctor fast tier clean" "rc_json $D/doctor.json 'not r[\"findings\"] and r[\"clean\"]'"
exit $fail
