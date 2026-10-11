#!/usr/bin/env bash
# Release check (0.4.1, finding 89): after the writ that ran `writ init` is
# uninstalled (for example by pasting doctor's "more than one writ on PATH"
# fix), the SessionStart hook still prints the brief context, from the writ
# left on PATH. On 0.4.0 the hook pinned the init writ's path and went silent.
# Backs the 0.4.1 note sentence "Removing an older writ no longer silences the
# session start hook".
#
# Usage: bench/release_checks/hook_after_uninstall.sh [WRIT_BIN] [SCRATCH_DIR]
#   WRIT_BIN     writ binary under test (default: <repo>/target/release/writ)
#   SCRATCH_DIR  throwaway dir (default: a fresh mktemp dir); reused only if
#                empty or left by a previous run of this script.
# Exit 0 when every check passes, 1 on any FAIL, 2 on bad arguments.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MARK=".hook_after_uninstall_scratch"

W="${1:-$ROOT/target/release/writ}"
case "$W" in /*) ;; */*) W="$PWD/$W" ;; *) W="$(command -v "$W" || echo "$W")" ;; esac
[ -x "$W" ] || { echo "writ binary not found or not executable: $W" >&2; exit 2; }
echo "binary: $W ($("$W" --version))"

if [ $# -ge 2 ]; then
    D="$2"
    if [ -d "$D" ] && [ -n "$(ls -A "$D")" ] && [ ! -f "$D/$MARK" ]; then
        echo "refusing to reuse non-empty $D (no $MARK marker); pass an empty or new dir" >&2
        exit 2
    fi
    rm -rf "$D"; mkdir -p "$D"
else
    tmp="${TMPDIR:-/tmp}"
    D="$(mktemp -d "${tmp%/}/writ-hook-uninstall.XXXXXX")"
fi
cd "$D"; touch "$MARK"; D="$(pwd -P)"
echo "scratch: $D"
unset WRIT_AGENT_ID

# Two installs of the same build: "old" runs init, "kept" is what stays.
mkdir -p old/bin kept/bin
cp "$W" old/bin/writ; cp "$W" kept/bin/writ
SYS=/usr/bin:/bin:/usr/sbin:/sbin

mkdir proj; cd proj
git init -q -b main
printf 'print("hi")\n' > app.py
git add -A && git -c user.name=t -c user.email=t@localhost commit -qm base
PATH="$D/old/bin:$SYS" writ init -y > init.log 2>&1
hook="$(python3 -c 'import json; d = json.load(open(".claude/settings.json")); print(next(h["command"] for e in d["hooks"]["SessionStart"] for h in e["hooks"] if "Writ VCS Active" in h["command"]))')"

fail=0
chk() { if eval "$2"; then echo "PASS $1"; else echo "FAIL $1"; fail=1; fi; }

PATH="$D/old/bin:$D/kept/bin:$SYS" sh -c "$hook" > before.out 2>&1 || true
chk "hook prints the brief before the uninstall" "grep -q 'writ context-brief' before.out"

rm -rf "$D/old"   # what `pip uninstall` / `brew uninstall` of the init writ does
PATH="$D/kept/bin:$SYS" sh -c "$hook" > after.out 2>&1 || true
chk "hook still prints the brief after the init writ is uninstalled" "grep -q 'writ context-brief' after.out"
chk "hook says which writ it ran instead" "grep -q '^Note: this session runs $D/kept/bin/writ' after.out"
# doctor exits 1 when it has findings; capture first (pipefail is on).
PATH="$D/kept/bin:$SYS" writ doctor --format json > doctor.json 2>/dev/null || true
chk "doctor reports the dead fallback as version_skew with writ init -y" \
  "python3 -c 'import json,sys; r=json.load(open(\"doctor.json\")); sys.exit(0 if any(f[\"check\"]==\"version_skew\" and f[\"fix_command\"]==\"writ init -y\" and \".claude/settings.json\" in f[\"paths\"] for f in r[\"findings\"]) else 1)'"
PATH="$D/kept/bin:$SYS" writ init -y > init2.log 2>&1
PATH="$D/kept/bin:$SYS" sh -c "$hook" > /dev/null 2>&1 || true
hook2="$(python3 -c 'import json; d = json.load(open(".claude/settings.json")); print(next(h["command"] for e in d["hooks"]["SessionStart"] for h in e["hooks"] if "Writ VCS Active" in h["command"]))')"
PATH="$D/kept/bin:$SYS" sh -c "$hook2" > after-init.out 2>&1 || true
chk "after writ init -y the note is gone and the brief prints" "grep -q 'writ context-brief' after-init.out && ! grep -q '^Note:' after-init.out"
exit $fail
