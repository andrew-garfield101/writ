#!/usr/bin/env bash
# Release check (0.4.2, finding 124, injection B's spec done form): the agent
# sealed, a finish committed its spec, and the agent then runs its auto-scoped
# `writ spec done -s "<summary>"`. It exits 0, says the spec is already
# committed and names it, never says "no spec claimed" or tells the agent to
# create a spec; the summary is kept on the spec (spec show) and handoff shows
# it; doctor is clean after.
#
# Usage: bench/release_checks/spec_done_committed.sh [WRIT_BIN] [SCRATCH_DIR]
# Exit 0 when every check passes, 1 on any FAIL, 2 on bad arguments.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
. "$ROOT/bench/release_checks/_lib.sh"
W="$(rc_bin "${1:-$ROOT/target/release/writ}")"
echo "binary: $W ($("$W" --version))"
rc_setup .spec_done_committed_scratch writ-spec-done-committed "${2:-}"
unset WRIT_AGENT_ID CLAUDECODE
mkdir bin; ln -s "$W" bin/writ
export PATH="$D/bin:/usr/bin:/bin:/usr/sbin:/sbin"
as() { local who="$1"; shift; WRIT_AGENT_ID="$who" writ "$@"; }
rc_git_project "$D/p"
git config user.name fixture; git config user.email fixture@example.invalid
writ init -y > /dev/null 2>&1; git add -A; git commit -qm init --no-verify
as a spec add --id auth --title "Auth module" > /dev/null
as a spec claim auth --agent a > /dev/null
printf 'def login():\n    pass\n' > auth.py
as a seal -s work --agent a --spec auth --paths auth.py > /dev/null 2>&1
as human spec update auth --status complete > /dev/null 2>&1
as human finish -y --no-check > "$D/finish.out" 2>&1
commit="$(git rev-parse --short=7 HEAD)"
rc=0; as a spec done -s "added login" > "$D/done.out" 2>&1 || rc=$?
chk "spec done after the commit exits 0 and says auth is already committed" \
  "[ $rc -eq 0 ] && grep -q auth $D/done.out && grep -qi 'already committed' $D/done.out"
chk "it names the commit" "grep -q $commit $D/done.out"
chk "it never says no spec claimed or to create a spec" \
  "! grep -qi 'no spec claimed' $D/done.out && ! grep -qi 'create one' $D/done.out"
as a spec show auth --format json > "$D/show.json" 2>/dev/null || true
chk "the summary is kept on the committed spec (spec show)" "grep -q 'added login' $D/show.json"
as a handoff --format json > "$D/handoff.json" 2>/dev/null || true
chk "handoff shows the post-commit summary" "grep -q 'added login' $D/handoff.json"
rc=0; as a doctor --format json > "$D/doctor.json" 2>/dev/null || rc=$?
chk "doctor clean after" "[ $rc -eq 0 ] && rc_json $D/doctor.json 'r[\"clean\"]'"
exit $fail
