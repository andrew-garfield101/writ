#!/usr/bin/env bash
# Release check (0.4.2, injection B, finding 121): an agent's spec is committed
# by a finish mid-run and the agent seals again, in both seal forms (explicit
# `--spec auth`, and auto-scoped with no `--spec`). The refusal names the spec
# as committed and prints exactly one command:
#   writ spec add "<title> (follow-up)" --id <id> --claim && writ seal -s "<summary>" --spec <id> --paths <paths>
# with no reopen and no --force. Pasted verbatim it exits 0 and seals the work
# on the follow-up spec, claimed by the agent; pasted again it exits 0, says
# already, and adds no seal and no spec; the file is unchanged; doctor clean.
#
# Usage: bench/release_checks/injection_b.sh [WRIT_BIN] [SCRATCH_DIR]
# Exit 0 when every check passes, 1 on any FAIL, 2 on bad arguments.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
. "$ROOT/bench/release_checks/_lib.sh"
W="$(rc_bin "${1:-$ROOT/target/release/writ}")"
echo "binary: $W ($("$W" --version))"
rc_setup .injection_b_scratch writ-injection-b "${2:-}"
unset WRIT_AGENT_ID CLAUDECODE
mkdir bin; ln -s "$W" bin/writ
export PATH="$D/bin:/usr/bin:/bin:/usr/sbin:/sbin"
as() { local who="$1"; shift; WRIT_AGENT_ID="$who" writ "$@"; }

run_form() {
    local form="$1"; shift
    rc_git_project "$D/$form"
    writ init -y > /dev/null 2>&1; git add -A; git -c user.name=t -c user.email=t@localhost commit -qm init
    as a spec add --id auth --title "Auth module" > /dev/null
    as a spec claim auth --agent a > /dev/null
    printf 'def login():\n    pass\n' > auth.py
    as a seal -s work --agent a --spec auth --paths auth.py > /dev/null 2>&1
    if [ "$form" = explicit ]; then
        as a spec done auth --agent a --no-seal > /dev/null 2>&1
    else
        as human spec update auth --status complete > /dev/null 2>&1
    fi
    as human finish -y --no-check > "$D/$form.finish" 2>&1
    printf 'def login():\n    return True\n' > auth.py
    local rc=0
    as a seal -s "login returns" "$@" --paths auth.py > "$D/$form.refusal" 2>&1 || rc=$?
    chk "$form: the seal is refused and names 'auth' as committed" \
      "[ $rc -ne 0 ] && grep -q \"'auth'\" $D/$form.refusal && grep -q committed $D/$form.refusal"
    python3 - "$D/$form.refusal" > "$D/$form.cmd" <<'PY' || true
import re, sys
text = open(sys.argv[1]).read()
cmds = [l.strip() for l in text.splitlines() if l.strip().startswith("writ ")]
if len(cmds) != 1:
    sys.exit(1)
m = re.fullmatch(r'writ spec add "Auth module \(follow[- ]up\)" --id (\S+) --claim && writ seal -s "login returns" --spec (\S+) --paths auth\.py', cmds[0])
if not m or m.group(1) != m.group(2) or not m.group(1).startswith("auth"):
    sys.exit(1)
print(cmds[0]); print(m.group(1))
PY
    local cmd id
    cmd="$(sed -n 1p "$D/$form.cmd")"; id="$(sed -n 2p "$D/$form.cmd")"
    chk "$form: exactly one printed command, the follow-up spec add && seal with the agent's summary and paths" "[ -n \"$id\" ]"
    chk "$form: no reopen, no --force" "! grep -qi reopen $D/$form.refusal && ! grep -q -- --force $D/$form.refusal"
    rc=0; WRIT_AGENT_ID=a sh -c "$cmd" > "$D/$form.paste1" 2>&1 || rc=$?
    local rc2=0; WRIT_AGENT_ID=a sh -c "$cmd" > "$D/$form.paste2" 2>&1 || rc2=$?
    chk "$form: pasted twice, both exit 0; the second says already" \
      "[ -n \"$id\" ] && [ $rc -eq 0 ] && [ $rc2 -eq 0 ] && grep -qi already $D/$form.paste2"
    as a spec show "${id:-none}" --format json > "$D/$form.spec.json" 2>/dev/null || true
    as a log --all --format json --limit 50 > "$D/$form.log.json" 2>/dev/null || true
    chk "$form: one seal on the follow-up spec, claimed by a, no second spec, file unchanged" \
      "rc_json $D/$form.spec.json 'r[\"claimed_by\"]==\"a\"' && rc_json $D/$form.log.json 'sum(1 for s in r if s.get(\"spec_id\")==\"$id\")==1' && ! as a spec status --format json | grep -q '\"$id-2\"' && printf 'def login():\n    return True\n' | cmp -s - auth.py"
    rc=0; as a doctor --format json > "$D/$form.doctor.json" 2>/dev/null || rc=$?
    chk "$form: doctor clean after the follow-up" "[ $rc -eq 0 ] && rc_json $D/$form.doctor.json 'r[\"clean\"]'"
}

run_form explicit --agent a --spec auth
run_form auto
exit $fail
