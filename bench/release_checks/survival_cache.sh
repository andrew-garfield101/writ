#!/usr/bin/env bash
# Claim (rule, every release since 0.4.1): "'safe to finish' is never false", and the
# survival tier's answer does not depend on whether its cache was warm. Finding 136
# (sprint 6): on a copy of the writ repository, brew 0.4.2 printed "safe to finish" from
# an incremental cache (1 spec audited) while a full run of the same store printed 4
# override notes and no "safe to finish" (58 specs audited).
#
# For each store: run `writ doctor --survival` warm (the cache as it stands, after the
# last seal), then remove .writ/survival_cache.json and run it cold; the headline,
# safe-to-finish, the finding list and next_action must be equal.
#   A. Bri's f136 fixtures: an open captured override keeps its note warm and cold
#      after an incremental run; after a plain finish commits it, neither tier shows a
#      captured-override note (Haris's root cause: notes advanced last_green and
#      committed specs fell out of the audit; the override search had no time order).
#   B. a copy of a real repository (default: this one; tree, .writ and .git, never
#      its own .writ), cache as copied vs cold.
#
# B reads a live tree: run it at the release close, when no agent is mid-edit.
# Usage: survival_cache.sh [WRIT_BIN] [SCRATCH_DIR] [REPO_DIR]   (REPO_DIR=- skips B)
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/_lib.sh"
ROOT="$(cd "$HERE/../.." && pwd)"
W="$(rc_bin "${1:-$ROOT/target/release/writ}")"
REPO="${3:-$ROOT}"
unset WRIT_AGENT_ID
rc_setup .survival_cache survival-cache "${2:-}"
echo "binary: $W ($("$W" --version))"
mkdir home
w() { local who="$1"; shift; env -i HOME="$D/home" PATH=/usr/bin:/bin:/usr/sbin:/sbin \
      WRIT_AGENT_ID="$who" WRIT_EXCLUDES_FILE="$D/home/none" GIT_CONFIG_NOSYSTEM=1 \
      GIT_AUTHOR_NAME=fixture GIT_AUTHOR_EMAIL=fixture@example.invalid \
      GIT_COMMITTER_NAME=fixture GIT_COMMITTER_EMAIL=fixture@example.invalid "$W" "$@" < /dev/null; }
numbered() { python3 -c 'import sys; n=int(sys.argv[1]); e=dict(a.split("=",1) for a in sys.argv[2:]); print("".join(f"line {i} {e[str(i)]}\n" if str(i) in e else f"line {i}\n" for i in range(n)), end="")' "$@"; }

# warm_cold <label>: doctor --survival warm, then cold; compare. Runs in the cwd.
warm_cold() {
    w human doctor --survival --format json > "$D/$1.warm.json" 2>"$D/$1.warm.err"
    rm -f .writ/survival_cache.json
    w human doctor --survival --format json > "$D/$1.cold.json" 2>"$D/$1.cold.err"
    python3 - "$D/$1.warm.json" "$D/$1.cold.json" <<'PY'
import json, sys
def view(p):
    try:
        r = json.load(open(p))
    except ValueError:
        print(f"  no JSON from doctor ({p}): {open(p[:-5] + '.err').read().strip()[:300]!r}")
        sys.exit(1)
    f = sorted((x.get("check"), x.get("severity"), (x.get("paths") or [None])[0]) for x in r.get("findings", []))
    return {"headline": r.get("headline"), "safe": "safe to finish" in (r.get("headline") or ""),
            "findings": f, "next": r.get("next_action")}
a, b = view(sys.argv[1]), view(sys.argv[2])
print(f"  warm: {a['headline']!r}, {len(a['findings'])} finding(s)")
print(f"  cold: {b['headline']!r}, {len(b['findings'])} finding(s)")
sys.exit(0 if a == b else 1)
PY
}

# --- A. fixtures (Bri's f136 group, doctor_states.rs captured_override) -----------------
NOTE="override captured another spec's version"
has_note() { grep -qF "$NOTE" "$D/$1.json"; }
# captured <dir>: sb seals line 30; sa seals over it, then puts sb's version back with
# spec done --allow-removals (an override that captures sb's content); sb done.
captured() {
    rc_git_project "$1"
    git config user.name fixture; git config user.email fixture@example.invalid
    numbered 40 > shared.txt; git add -A; git commit -qm fixture
    w human init -y > /dev/null 2>&1; git add -A; git commit -qm "writ init" --no-verify
    for s in sa:alpha sb:bravo; do
        w "${s#*:}" spec add --id "${s%%:*}" --title "spec ${s%%:*}" > /dev/null
        w "${s#*:}" spec claim "${s%%:*}" --agent "${s#*:}" > /dev/null
    done
    numbered 40 30=B > shared.txt;     w bravo seal -s work --agent bravo --spec sb --paths shared.txt > /dev/null 2>&1
    numbered 40 2=A 30=B > shared.txt; w alpha seal -s work --agent alpha --spec sa --paths shared.txt > /dev/null 2>&1
    numbered 40 30=B > shared.txt
    w alpha spec done sa --agent alpha -s done --paths shared.txt --allow-removals shared.txt > /dev/null 2>&1
    w bravo spec done sb --agent bravo --no-seal > /dev/null 2>&1
    w human doctor --survival --format json > "$D/$(basename "$1").first.json" 2>/dev/null
}
later_seal() {   # an unrelated later seal, so the next warm run is incremental
    w carol spec add --id later --title later --claim > /dev/null 2>&1
    printf 'carol\n' > later.txt; w carol seal -s later --agent carol --spec later --paths later.txt > /dev/null 2>&1
}

captured "$D/open"
chk "A1 fixture: an open captured override shows the note and no safe to finish" \
    'has_note open.first && ! grep -qF "safe to finish" "$D/open.first.json"'
later_seal
chk "A2 open override after an incremental run: warm and cold agree" "warm_cold open"
chk "A3 open override: the note is still there warm and cold" 'has_note open.warm && has_note open.cold'

captured "$D/plain"
H0="$(git rev-parse HEAD)"; w human finish -y > "$D/plain.finish" 2>&1
if [ "$(git rev-parse HEAD)" != "$H0" ]; then
    later_seal
    chk "A4 override committed by a plain finish: warm and cold agree" "warm_cold plain"
    chk "A5 after a plain finish no captured-override note remains on the committed specs" \
        '! has_note plain.warm && ! has_note plain.cold && ! grep -qF -- "--accept-removals" "$D/plain.cold.json"'
else
    echo "SKIP A4-A5: finish refused on the open note (no plain-finish path for this state)"
fi

# --- B. a copy of a real repository -------------------------------------------------------
if [ "$REPO" != "-" ]; then
    REPO="$(cd "$REPO" && pwd)"
    echo "repo: $REPO at $(git -C "$REPO" rev-parse --short HEAD)"
    rsync -a --exclude '/target' --exclude '/.venv*' --exclude '/book/book' "$REPO/" "$D/copy/"
    cd "$D/copy"
    chk "B $(basename "$REPO") copy: warm (cache as copied) and cold survival agree" "warm_cold copy"
fi
exit $fail
