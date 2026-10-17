#!/usr/bin/env bash
# Release check (0.4.1, finding 91): `writ spec release <id> --stale` releases a
# claim only when its holder is provably gone (same host, pid exited), refuses a
# live holder and a claim from another host while naming the evidence and the
# human's --force, never records the release as forced, and doctor's stale_claim
# fix is `--stale`; no doctor fix_command contains --force.
# 0.4.2 (session end): `writ init -y` installs a Claude Code SessionEnd hook that
# records the host's session end; when the pid cannot be judged (here: no
# process table) that record is the proof and `--stale` releases, while a claim
# without one is still refused, a live holder that ran the hook is still alive,
# `/clear` records nothing, and doctor reads the records without writing.
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
for s in ghost live far ended cleared; do writ spec add --id $s --title $s --agent human > /dev/null; done
# The generated SessionEnd hook, run the way Claude Code runs it (a shell under
# the session process, the hook's JSON on stdin).
END_HOOK="$(python3 -c 'import json; print(json.load(open(".claude/settings.json")).get("hooks",{}).get("SessionEnd",[{}])[0].get("hooks",[{}])[0].get("command",""))')"
export END_HOOK
# 0.4.2 ran `writ session-end`; 0.4.3 runs one dispatcher, `writ hook`, that reads the
# event from stdin's hook_event_name (which Claude Code always sends). Either is writ's.
chk "writ init -y installed a SessionEnd hook that runs writ (session-end, or the hook dispatcher)" \
  "case \"\$END_HOOK\" in *'\"\$W\" session-end'*|*'\"\$W\" hook'*) true ;; *) false ;; esac"
[ -n "$END_HOOK" ] || END_HOOK=:
claude -c "writ spec claim ghost --agent ghost-session && :" > /dev/null
# The live holder also runs the end hook (a host reporting an end while its
# process lives on): the pid proof must still decide.
claude -c "writ spec claim live --agent live-session && echo '{\"hook_event_name\":\"SessionEnd\",\"reason\":\"other\"}' | sh -c \"\$END_HOOK\" && sleep 60" > /dev/null 2>&1 &
live_pid=$!; trap 'kill $live_pid 2>/dev/null || true' EXIT
for _ in $(seq 50); do ls .writ/agents/claudecode/ended/*.json > /dev/null 2>&1 && break; sleep 0.1; done
writ spec claim far --agent far-session > /dev/null
python3 - <<'PY'
import json
p = ".writ/specs/far.json"; d = json.load(open(p)); d["claimed_host"] = "other-host.invalid"
json.dump(d, open(p, "w"), indent=2)
PY
claude -c "writ spec claim ended --agent ended-session && echo '{\"hook_event_name\":\"SessionEnd\",\"session_id\":\"t-ended\",\"reason\":\"prompt_input_exit\"}' | sh -c \"\$END_HOOK\"; :" > /dev/null
claude -c "writ spec claim cleared --agent cleared-session && echo '{\"hook_event_name\":\"SessionEnd\",\"reason\":\"clear\"}' | sh -c \"\$END_HOOK\"; :" > /dev/null
claude -c "echo '{\"hook_event_name\":\"SessionEnd\",\"reason\":\"resume\"}' | sh -c \"\$END_HOOK\"; :" > /dev/null
ls .writ/agents/claudecode/ended/ > ended.ls 2>/dev/null || true
chk "the SessionEnd hook wrote one record per ended session (ended, live), none for /clear or /resume" \
  "[ \$(wc -l < ended.ls) -eq 2 ]"
chk "the ended session's record names its claim and the host's reason" \
  "python3 -c 'import json,glob,sys; rs=[json.load(open(f)) for f in glob.glob(\".writ/agents/claudecode/ended/*.json\")]; sys.exit(0 if any(r[\"claims\"]==[{\"spec\":\"ended\",\"agent\":\"ended-session\"}] and r[\"reason\"]==\"prompt_input_exit\" and r[\"host_session_id\"]==\"t-ended\" for r in rs) else 1)'"
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

rc=0; writ spec release live --stale --agent other > live2.out 2>&1 || rc=$?
chk "a session end record never overrides a live pid: --stale still refuses the live holder" \
  "[ $rc -ne 0 ] && grep -q 'holder is alive' live2.out && grep -q live-session .writ/specs/live.json"

# Second proof: make the pid unjudgeable (no process table), so only a session
# end record can prove a holder gone.
mkdir noproc; printf '#!/bin/sh\nexit 1\n' > noproc/ps; chmod +x noproc/ps
claude -c "writ spec claim ghost --agent ghost2-session && :" > /dev/null
ls -l .writ/agents/claudecode/ended/ > before_doctor.ls 2>&1 || true
( export PATH="$D/p/noproc:$PATH"; writ doctor --format json > doctor_noproc.json 2>/dev/null || true )
ls -l .writ/agents/claudecode/ended/ > after_doctor.ls 2>&1 || true
chk "doctor reads session end records and never writes one" \
  "cmp -s before_doctor.ls after_doctor.ls && [ -z \"\$(find .writ/agents -newer before_doctor.ls)\" ]"
chk "without a process table doctor's fix for the ended session is 'writ spec release ended --stale'" \
  "rc_json doctor_noproc.json 'any(f[\"check\"]==\"stale_claim\" and f[\"fix_command\"]==\"writ spec release ended --stale\" for f in r[\"findings\"])'"
rc=0; ( export PATH="$D/p/noproc:$PATH"; writ spec release ghost --stale --agent other ) > ghost2.out 2>&1 || rc=$?
chk "without a process table and without a session end record --stale refuses (names the process table)" \
  "[ $rc -ne 0 ] && grep -q 'process table' ghost2.out && grep -q ghost2-session .writ/specs/ghost.json"
rc=0; ( export PATH="$D/p/noproc:$PATH"; writ spec release cleared --stale --agent other ) > cleared.out 2>&1 || rc=$?
chk "a /clear session end is not proof: --stale refuses the cleared session's claim" \
  "[ $rc -ne 0 ] && grep -q cleared-session .writ/specs/cleared.json"
rc=0; ( export PATH="$D/p/noproc:$PATH"; writ spec release ended --stale --agent other ) > ended.out 2>&1 || rc=$?
chk "with the session end record --stale releases the ended session's claim, not forced; the audit event names the hook" \
  "[ $rc -eq 0 ] && grep 'claim_stale_released' .writ/security/events.jsonl | grep -q 'SessionEnd hook' && rc_json .writ/specs/ended.json 'r.get(\"claimed_by\") is None and not r.get(\"release_forced\")'"
exit $fail
