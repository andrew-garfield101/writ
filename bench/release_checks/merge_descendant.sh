#!/usr/bin/env bash
# Release check (0.4.2, finding 115) on the real sprint 4 blobs (Bri's fixture,
# crates/writ-cli/tests/fixtures/f115): git base, two harness seals by bri, then
# amis seals a version that already holds the harness lines (an informed
# descendant). Survival is green and says safe to finish; finish commits amis's
# version byte for byte with no conflict, escalation or refusal; survival stays
# green after. Negative: the stale rewrite (no harness lines) is red, never
# safe to finish, and finish refuses with HEAD unchanged.
#
# Usage: bench/release_checks/merge_descendant.sh [WRIT_BIN] [SCRATCH_DIR]
# Exit 0 when every check passes, 1 on any FAIL, 2 on bad arguments.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
. "$ROOT/bench/release_checks/_lib.sh"
FX="$ROOT/crates/writ-cli/tests/fixtures/f115"
W="$(rc_bin "${1:-$ROOT/target/release/writ}")"
echo "binary: $W ($("$W" --version))"
rc_setup .merge_descendant_scratch writ-merge-descendant "${2:-}"
# Ruling 183 (0.4.3): a hand seal that drops another spec's sealed lines is refused at seal
# time. This script's fixture builds that divergence on purpose to test what finish, doctor
# and handoff do with it, so the fixture seals with `[seal] foreign_line_check = false` (the
# test-fixture setting) and every diagnostic runs with the key removed again: fx_key on|off.
fx_key() { [ -f .writ/config.toml ] || return 0
  python3 - "$1" <<'FXPY'
import re, sys
p = ".writ/config.toml"; s = open(p).read()
s = re.sub(r"\n?\[seal\]\nforeign_line_check = false\n", "\n", s)
if sys.argv[1] == "on":
    s = s.rstrip("\n") + "\n\n[seal]\nforeign_line_check = false\n"
open(p, "w").write(s)
FXPY
}
w_raw() { env -i HOME="$HOME" PATH=/usr/bin:/bin:/usr/sbin:/sbin "$W" "$@"; }
w() { local a; for a in "$@"; do case "$a" in doctor|finish|handoff|why|context) fx_key off; w_raw "$@"; local rc=$?; fx_key on; return $rc ;; seal|spec|log|show|init|status|diff) break ;; esac; done; w_raw "$@"; }
g() { git -c user.name=t -c user.email=t@localhost "$@"; }

# fixture <dir> <second version>: the sprint 4 shape, both specs done.
fixture() {
    rc_git_project "$1"
    w init -y > /dev/null 2>&1; fx_key on; git add -A; g commit -qm init
    cp "$FX/base.py.txt" t.py; git add t.py; g commit -qm base
    w spec add --id harness --title harness --agent bri --claim > /dev/null
    w spec add --id stale-release --title stale-release --agent amis --claim > /dev/null
    cp "$FX/harness1.py.txt" t.py; w seal -s h1 --agent bri --spec harness --paths t.py > /dev/null 2>&1
    cp "$FX/harness2.py.txt" t.py; w seal -s h2 --agent bri --spec harness --paths t.py > /dev/null 2>&1
    seal_rc=0; cp "$2" t.py; w seal -s second --agent amis --spec stale-release --paths t.py > "$D/$(basename "$1").seal" 2>&1 || seal_rc=$?
    w spec done harness --agent bri --no-seal > /dev/null 2>&1
    w spec done stale-release --agent amis --no-seal > /dev/null 2>&1
}

fixture "$D/informed" "$FX/amis.py.txt"
chk "informed descendant: amis's seal is accepted (fixture shape holds)" "[ $seal_rc -eq 0 ]"
rc=0; w doctor --survival > "$D/i.survival" 2>&1 || rc=$?
chk "informed descendant: survival green and safe to finish" \
  "[ $rc -eq 0 ] && head -1 $D/i.survival | grep -q 'safe to finish\$'"
head0="$(git rev-parse HEAD)"; rc=0; w finish -y > "$D/i.finish" 2>&1 || rc=$?
chk "informed descendant: finish commits amis's version byte for byte" \
  "[ $rc -eq 0 ] && [ \"\$(git rev-parse HEAD)\" != $head0 ] && git show HEAD:t.py | cmp -s - $FX/amis.py.txt"
chk "informed descendant: finish reports no conflict, escalation or refusal" \
  "! grep -qiE 'conflict|escalat|refus' $D/i.finish"
rc=0; w doctor --survival > "$D/i.after" 2>&1 || rc=$?
chk "informed descendant: survival green after the finish" "[ $rc -eq 0 ]"

fixture "$D/stale" "$FX/stale.py.txt"
chk "stale rewrite: the seal is accepted, so the finish-time guard is what is tested" "[ $seal_rc -eq 0 ]"
rc=0; w doctor --survival > "$D/s.survival" 2>&1 || rc=$?
chk "stale rewrite: survival red, never safe to finish" \
  "[ $rc -ne 0 ] && ! grep -q 'safe to finish' $D/s.survival"
head0="$(git rev-parse HEAD)"; rc=0; w finish -y > "$D/s.finish" 2>&1 || rc=$?
chk "stale rewrite: finish refuses and HEAD is unchanged" \
  "[ $rc -ne 0 ] && [ \"\$(git rev-parse HEAD)\" = $head0 ]"
exit $fail
