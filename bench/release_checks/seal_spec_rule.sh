#!/usr/bin/env bash
# Release check (0.4.1, findings 98, 99, 100): the spec rule for seals.
#   98: an explicit --agent with no spec is refused like a detected agent.
#   99: a human seal with no spec still succeeds, and its warning names
#       `writ spec add "..." --claim`.
#   100: with [security] claim_enforcement = "strict", a human seal with no
#       spec is refused.
# Every writ call runs under `env -i` so no agent variable from the caller's
# shell (CLAUDECODE, CLAUDE_CODE_SESSION_ID, ...) turns a human seal into an
# agent seal (finding 95).
#
# Usage: bench/release_checks/seal_spec_rule.sh [WRIT_BIN] [SCRATCH_DIR]
# Exit 0 when every check passes, 1 on any FAIL, 2 on bad arguments.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
. "$ROOT/bench/release_checks/_lib.sh"
W="$(rc_bin "${1:-$ROOT/target/release/writ}")"
echo "binary: $W ($("$W" --version))"
rc_setup .seal_spec_rule_scratch writ-seal-spec-rule "${2:-}"
w() { env -i HOME="$HOME" PATH=/usr/bin:/bin:/usr/sbin:/sbin "$W" "$@"; }
project() {  # project <dir>: a git repo with writ initialized, entered
  rc_git_project "$1"
  w init -y > /dev/null 2>&1
}
sealed_count() { w log --all --format json 2>/dev/null | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))' 2>/dev/null || echo 0; }

# 98: explicit --agent, no spec
project "$D/p98"; n0=$(sealed_count)
echo one > f.txt
rc=0; w seal -s "agent flag only" --agent my-agent --paths f.txt > "$D/98.out" 2>&1 || rc=$?
chk "98: --agent with no spec is refused and nothing is sealed" \
  "[ $rc -ne 0 ] && grep -q 'No active spec' $D/98.out && [ \$(sealed_count) -eq $n0 ]"
chk "98: the refusal names writ spec add ... --claim" "grep -q 'writ spec add \".*\" --claim' $D/98.out"

# 99: human, no spec, default (warn)
project "$D/p99"; n0=$(sealed_count)
echo one > f.txt
rc=0; w seal -s "human no spec" --paths f.txt > "$D/99.out" 2>&1 || rc=$?
chk "99: a human seal with no spec succeeds" "[ $rc -eq 0 ] && [ \$(sealed_count) -gt $n0 ]"
chk "99: its warning names writ spec add ... --claim" \
  "grep -i 'warning' $D/99.out | grep -q 'writ spec add \".*\" --claim'"

# 100: human, no spec, claim_enforcement = "strict"
project "$D/p100"
python3 - <<'PY'
import pathlib, re
p = pathlib.Path(".writ/config.toml"); s = p.read_text()
s = re.sub(r"(?m)^\[security\]$", '[security]\nclaim_enforcement = "strict"', s, count=1)
assert 'claim_enforcement = "strict"' in s, "no [security] table"
p.write_text(s)
PY
n0=$(sealed_count)
echo one > f.txt
rc=0; w seal -s "human strict" --paths f.txt > "$D/100.out" 2>&1 || rc=$?
chk "100: strict claim_enforcement refuses a human seal with no spec, nothing sealed" \
  "[ $rc -ne 0 ] && [ \$(sealed_count) -eq $n0 ]"
exit $fail
