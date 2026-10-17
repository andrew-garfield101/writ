#!/usr/bin/env bash
# Claim (0.4.3 note): "The stranger kit (tools/stranger_kit/) runs on the released writ:
# `pip install writ-vcs`, then setup, launch, collect and score on a copy of your repo."
#
# Installs the shipped wheel from PyPI into a scratch venv (WRIT_SPEC, default
# writ-vcs==0.4.2; set it to the version being released once it is on PyPI) and runs
# the kit's setup, collect and score with that venv's `writ` and nothing else on PATH
# for writ. `launch` needs a live `claude` and is not run here: two scripted agents
# (two writ identities, one spec and one seal each, spec done -s) stand in for it, and
# a stub `claude` answers setup's `claude --version`. What is checked is that the kit's
# own steps work against the shipped binary, not how agents behave.
#
# Usage: kit_shipped.sh [unused] [SCRATCH_DIR] [KIT_DIR]
#   KIT_DIR defaults to tools/stranger_kit in this repo. WRIT_SPEC=writ-vcs==X.Y.Z.
#   WRIT_BIN=<path> skips PyPI and runs the kit on that binary instead (dev build);
#   WRIT_BUILT_FROM=<seal> is passed to the kit's --built-from (the kit otherwise labels any
#   binary outside a checkout's target/ a release install, finding 165).
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/_lib.sh"
ROOT="$(cd "$HERE/../.." && pwd)"
KIT="$(cd "${3:-$ROOT/tools/stranger_kit}" 2>/dev/null && pwd)" || { echo "kit not found: ${3:-$ROOT/tools/stranger_kit}" >&2; exit 2; }
[ -f "$KIT/stranger.py" ] || { echo "no stranger.py in $KIT" >&2; exit 2; }
SPEC="${WRIT_SPEC:-writ-vcs==0.4.2}"
[ -n "${WRIT_BIN:-}" ] && WRIT_BIN="$(rc_bin "$WRIT_BIN")"
ZSTD="$(command -v zstd || true)"
[ -n "$ZSTD" ] || { echo "zstd not found; the kit's audit needs it (brew install zstd)" >&2; exit 2; }
unset WRIT_AGENT_ID
rc_setup .kit_shipped kit-shipped "${2:-}"
echo "kit: $KIT"

mkdir -p stub
printf '#!/bin/sh\necho "stub claude (release check, no live agents)"\n' > stub/claude; chmod +x stub/claude
ln -s "$ZSTD" stub/zstd
if [ -n "${WRIT_BIN:-}" ]; then
    W="$(rc_bin "$WRIT_BIN")"; WANT="$("$W" --version)"; KIND="dev build"
    echo "binary: $W ($WANT), given by WRIT_BIN"
else
    python3 -m venv venv > /dev/null || { echo "venv failed" >&2; exit 2; }
    venv/bin/pip install --quiet --disable-pip-version-check --no-cache-dir "$SPEC" > pip.log 2>&1
    PIP_RC=$?
    W="$D/venv/bin/writ"; WANT="writ ${SPEC##*==}"; KIND="release install"
    chk "1 pip install $SPEC from PyPI puts a writ CLI in the venv" '[ "$PIP_RC" -eq 0 ] && [ -x "$W" ]'
    [ -x "$W" ] || { cat pip.log; exit 1; }
    echo "binary: $W ($("$W" --version))"
    chk "2 the venv writ is $WANT" '[ "$("$W" --version)" = "$WANT" ]'
fi
# Only the kit's writ and the stub claude: no other writ reachable.
KPATH="$D/stub:/usr/bin:/bin:/usr/sbin:/sbin"
k() { env -i HOME="$D" PATH="$KPATH" TMPDIR="${TMPDIR:-/tmp}" GIT_CONFIG_NOSYSTEM=1 python3 "$KIT/stranger.py" "$@"; }
a() { local who="$1"; shift; env -i HOME="$D" PATH="$KPATH" WRIT_AGENT_ID="$who" GIT_CONFIG_NOSYSTEM=1 "$W" "$@" < /dev/null; }

rc_git_project "$D/repo"
git config user.name kit-check; git config user.email kit-check@localhost
for i in 2 3; do printf 'v%s\n' "$i" >> app.py; git add -A; git -c user.name=t -c user.email=t@localhost commit -qm "c$i"; done
printf 'def a():\n    return 1\n' > a.py; printf 'def b():\n    return 1\n' > b.py
git add -A; git -c user.name=t -c user.email=t@localhost commit -qm "c4"
OUT="$D/out"

BF=(); [ "$KIND" = "dev build" ] && BF=(--built-from "${WRIT_BUILT_FROM:-unstated (WRIT_BIN given to kit_shipped.sh)}")
k setup "$D/repo" "$OUT" --writ "$W" ${BF[@]+"${BF[@]}"} > "$D/setup.log" 2>&1; SRC=$?
chk "3 kit setup exits 0 with the shipped writ (init -y, doctor before)" '[ "$SRC" -eq 0 ]'
[ "$SRC" -eq 0 ] || { cat "$D/setup.log"; exit 1; }
chk "4 kit.json records $WANT, provenance '$KIND'" \
    'rc_json "$OUT/kit.json" "r[\"writ_version\"] == \"$WANT\" and r[\"writ_build\"][\"kind\"] == \"$KIND\""'

# Two scripted agents in place of launch.
a kit-a spec add "add feature a" --id fa --claim > /dev/null 2>&1
a kit-b spec add "add feature b" --id fb --claim > /dev/null 2>&1
printf 'def a():\n    return 2\n' > a.py; a kit-a seal -s "feature a" --spec fa --paths a.py > /dev/null 2>&1
printf 'def b():\n    return 2\n' > b.py; a kit-b seal -s "feature b" --spec fb --paths b.py > /dev/null 2>&1
a kit-a spec done fa -s "a done" > /dev/null 2>&1; a kit-b spec done fb -s "b done" > /dev/null 2>&1
: > "$OUT/agent-1.jsonl"; : > "$OUT/agent-2.jsonl"

k collect "$D/repo" "$OUT" > "$D/collect.log" 2>&1; CRC=$?
chk "5 kit collect exits 0: writ finish -y rc 0, commits audited" \
    '[ "$CRC" -eq 0 ] && rc_json "$OUT/kit.json" "r.get(\"finish_rc\") == 0 and len(r.get(\"commits_since_setup\", [])) >= 1"'
chk "6 the audit finds the two seals' lines and 0 lost" 'grep -q "LOST: 0" "$OUT/audit.txt" && ! grep -q "added lines checked: 0;" "$OUT/audit.txt"'
chk "7 store integrity after the run is clean (rc 0)" 'rc_json "$OUT/kit.json" "r.get(\"integrity_rc\") == 0"'
k score "$OUT" > "$D/score.log" 2>&1; SCRC=$?
chk "8 kit score exits 0 and writes report.md and summary.json" '[ "$SCRC" -eq 0 ] && [ -s "$OUT/report.md" ] && [ -s "$OUT/summary.json" ]'
chk "9 summary.json: lost_lines 0, writ_version $WANT" \
    'rc_json "$OUT/summary.json" "r.get(\"lost_lines\") == 0 and r.get(\"writ_version\") == \"$WANT\""'
head -8 "$OUT/report.md" 2>/dev/null
exit $fail
