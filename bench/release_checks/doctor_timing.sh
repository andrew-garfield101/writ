#!/usr/bin/env bash
# Claim (numbers page, every release note's Numbers section): "`writ doctor` fast
# tier on the writ repository: <N> ms median. The survival tier on the same
# repository: <a> to <b> seconds for a full run."
#
# Measures on a copy of the repository (working tree minus target/ and venvs, with
# .writ and .git), never on the repository itself: a full survival run needs the
# survival cache gone, and nothing here writes to the real .writ. Prints every
# sample, the median, min and max, the binary and its version, and the commit.
#
# Checks (budgets, not the claim's exact range; the range is what it prints):
#   1. fast tier median under 1000 ms
#   2. survival full run max under 5000 ms (sprint 6 budget)
#   3. survival incremental (cache warm) median under 1000 ms
#   4. the copy's doctor result equals the repo's (same findings), so the copy is faithful
#
# Usage: doctor_timing.sh [WRIT_BIN] [SCRATCH_DIR] [REPO_DIR] ; RUNS=5 by default.
#   TIMING_JSON=<path> also writes every sample with its conditions (binary sha256, commit,
#   newest seal, store size in seals, load average 1/5/15, cpu count, date) to that file;
#   default: TEAM_WRIT_DOCS/evidence/0.4.3-doctor-timing/doctor_timing_<sha12>_<UTC>.json
#   beside this repo when that folder's parent exists. TIMING_JSON= (empty) writes nothing.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/_lib.sh"
ROOT="$(cd "$HERE/../.." && pwd)"
WRIT="$(rc_bin "${1:-$ROOT/target/release/writ}")"
REPO="$(cd "${3:-$ROOT}" && pwd)"
RUNS="${RUNS:-5}"
unset WRIT_AGENT_ID
rc_setup .doctor_timing doctor-timing "${2:-}"

echo "binary: $WRIT ($("$WRIT" --version))"
echo "repo: $REPO at $(git -C "$REPO" rev-parse --short HEAD), runs: $RUNS"
rsync -a --exclude '/target' --exclude '/.venv*' --exclude '/book/book' "$REPO/" "$D/repo/"
cd "$D/repo"
# A copy taken while another writ process is writing can miss an object; retry once.
if ! "$WRIT" doctor --format json 2>/dev/null | python3 -c 'import json,sys; r=json.load(sys.stdin); sys.exit(0 if not any(f.get("check")=="store_integrity" for f in r.get("findings",[])) else 1)'; then
    echo "copy inconsistent (taken mid-write); copying again"
    rsync -a --delete --exclude '/target' --exclude '/.venv*' --exclude '/book/book' "$REPO/" "$D/repo/"
fi

ms() { python3 -c 'import json,sys; print(json.load(sys.stdin)["elapsed_ms"])'; }
stats() { python3 -c '
import statistics,sys
v=[int(x) for x in sys.argv[2:]]
print(f"{sys.argv[1]}: samples {v} median {statistics.median(v):.0f} min {min(v)} max {max(v)}")' "$@"; }
med() { python3 -c 'import statistics,sys; print(int(statistics.median([int(x) for x in sys.argv[1:]])))' "$@"; }
mx() { python3 -c 'import sys; print(max(int(x) for x in sys.argv[1:]))' "$@"; }

fast=(); full=(); incr=()
for _ in $(seq "$RUNS"); do fast+=("$("$WRIT" doctor --format json 2>/dev/null | ms)"); done
for _ in $(seq "$RUNS"); do
    rm -f .writ/survival_cache.json
    full+=("$("$WRIT" doctor --survival --format json 2>/dev/null | ms)")
done
for _ in $(seq "$RUNS"); do incr+=("$("$WRIT" doctor --survival --format json 2>/dev/null | ms)"); done
stats "fast tier ms" "${fast[@]}"
stats "survival full run ms (cache removed each run, total doctor time)" "${full[@]}"
stats "survival incremental ms (cache warm)" "${incr[@]}"
"$WRIT" doctor --survival 2>/dev/null | tail -1

sha="$(shasum -a 256 "$WRIT" | cut -c1-64)"
EVD="$(cd "$ROOT/.." && pwd)/TEAM_WRIT_DOCS/evidence"
if [ -z "${TIMING_JSON+x}" ] && [ -d "$EVD" ]; then
    mkdir -p "$EVD/0.4.3-doctor-timing"
    TIMING_JSON="$EVD/0.4.3-doctor-timing/doctor_timing_${sha:0:12}_$(date -u +%Y%m%dT%H%M%SZ).json"
fi
if [ -n "${TIMING_JSON:-}" ]; then
    SEALS="$(ls .writ/seals 2>/dev/null | wc -l | tr -d ' ')"
    FJ="$(IFS=,; echo "${fast[*]}")"; UJ="$(IFS=,; echo "${full[*]}")"; IJ="$(IFS=,; echo "${incr[*]}")"
    NEWEST="$("$WRIT" log --all --format json --limit 1 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["id"][:12])' 2>/dev/null)"
    python3 - "$TIMING_JSON" <<PY
import json, os, sys, time
d = {"what": "writ doctor timing on an rsync copy of a repository (bench/release_checks/doctor_timing.sh)",
     "date": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
     "binary": {"path": "$WRIT", "version": "$("$WRIT" --version)", "sha256": "$sha"},
     "repo": {"path": "$REPO", "commit": "$(git -C "$REPO" rev-parse --short HEAD)", "newest_seal": "$NEWEST", "seals": int("$SEALS" or 0)},
     "load_avg": list(os.getloadavg()), "cpus": os.cpu_count(), "runs": $RUNS,
     "fast_ms": [$FJ], "survival_full_ms": [$UJ], "survival_incremental_ms": [$IJ]}
json.dump(d, open(sys.argv[1], "w"), indent=2)
print("saved:", sys.argv[1])
PY
fi

kinds() { "$WRIT" doctor --format json 2>/dev/null | python3 -c 'import json,sys; print(sorted((f.get("check"), f.get("severity")) for f in json.load(sys.stdin).get("findings",[])))'; }
copy_kinds="$(kinds)"; repo_kinds="$(cd "$REPO" && kinds)"
echo "findings: copy $copy_kinds, repo $repo_kinds"

chk "1 fast median < 1000 ms ($(med "${fast[@]}"))" '[ "$(med "${fast[@]}")" -lt 1000 ]'
chk "2 survival full max < 5000 ms ($(mx "${full[@]}"))" '[ "$(mx "${full[@]}")" -lt 5000 ]'
chk "3 survival incremental median < 1000 ms ($(med "${incr[@]}"))" '[ "$(med "${incr[@]}")" -lt 1000 ]'
chk "4 copy doctor findings equal the repo's" '[ "$copy_kinds" = "$repo_kinds" ]'
exit $fail
