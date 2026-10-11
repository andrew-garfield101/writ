#!/usr/bin/env bash
# Release check (0.4.1 Upgrade section): a project initialized by an older writ
# (default: the writ first on PATH, e.g. brew 0.4.0) gets one version_skew
# finding naming .claude/settings.json with `writ init -y` from the new build,
# and `writ init -y` clears it. Backs "Doctor reports hooks written by 0.4.0 or
# earlier as version_skew until you do."
#
# Usage: bench/release_checks/upgrade_hook_skew.sh [WRIT_BIN] [OLD_WRIT] [SCRATCH_DIR]
#   WRIT_BIN     new writ under test (default: <repo>/target/release/writ)
#   OLD_WRIT     the older writ that runs the first init (default: writ on PATH)
#   SCRATCH_DIR  throwaway dir (default: fresh mktemp dir); reused only if empty
#                or left by a previous run of this script.
# Exit 0 when every check passes, 1 on any FAIL, 2 on bad arguments.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MARK=".upgrade_hook_skew_scratch"
W="${1:-$ROOT/target/release/writ}"
OLD="${2:-$(command -v writ || true)}"
for b in "$W" "$OLD"; do [ -x "$b" ] || { echo "writ binary not found or not executable: $b" >&2; exit 2; }; done
echo "new: $W ($("$W" --version)); old: $OLD ($("$OLD" --version))"
if [ $# -ge 3 ]; then
    D="$3"
    if [ -d "$D" ] && [ -n "$(ls -A "$D")" ] && [ ! -f "$D/$MARK" ]; then
        echo "refusing to reuse non-empty $D (no $MARK marker)" >&2; exit 2
    fi
    rm -rf "$D"; mkdir -p "$D"
else
    tmp="${TMPDIR:-/tmp}"; D="$(mktemp -d "${tmp%/}/writ-upgrade-skew.XXXXXX")"
fi
cd "$D"; touch "$MARK"; echo "scratch: $D"
unset WRIT_AGENT_ID
git init -q -b main
printf 'print("hi")\n' > app.py
git add -A && git -c user.name=t -c user.email=t@localhost commit -qm base
"$OLD" init -y > init-old.log 2>&1
"$W" doctor --format json > before.json 2>/dev/null || true
"$W" init -y > init-new.log 2>&1
"$W" doctor --format json > after.json 2>/dev/null || true
fail=0
chk() { if eval "$2"; then echo "PASS $1"; else echo "FAIL $1"; fail=1; fi; }
chk "old hook reported as version_skew on .claude/settings.json, fix writ init -y" \
  "python3 -c 'import json,sys; r=json.load(open(\"before.json\")); sys.exit(0 if any(f[\"check\"]==\"version_skew\" and f[\"fix_command\"]==\"writ init -y\" and \".claude/settings.json\" in f[\"paths\"] for f in r[\"findings\"]) else 1)'"
chk "writ init -y clears it (doctor clean)" \
  "python3 -c 'import json,sys; r=json.load(open(\"after.json\")); sys.exit(0 if r[\"clean\"] and not r[\"findings\"] else 1)'"
exit $fail
