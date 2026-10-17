#!/usr/bin/env bash
# Claim (numbers page and 0.4.3 note, fleet): "With N agents on N specs, K seals each, on
# a repository of F files: seal p50 and max, context, doctor fast, survival full and
# incremental, handoff", each produced by bench/fleet_bench.py, whose JSON is the evidence.
#
# Reproducibility, not a benchmark of its own: given the published JSON (FLEET_JSON), this
# reruns bench/fleet_bench.py with the same agents, seals and files on the binary under
# test and checks the counts are identical and each timing is within FLEET_TOLERANCE times
# the published one (default 2.0; timings move with the machine). Without FLEET_JSON it
# runs a small size (FLEET_SIZE="5 4 100": agents, seals per agent, files) and checks the
# script's output carries every number and the binary it came from (Bri's contract in
# bench/fleet/test_fleet_contract.py).
#
# Sim mode (the published 0.4.3 fleet numbers): FLEET_JSON may be a directory or a
# bench/fleet/sim.py JSON (one with hook_ms). Default: TEAM_WRIT_DOCS/evidence/0.4.3-fleet-sim
# beside this repo, when present. For each published sim run of an auto seal build
# (before-*, bare-seal-* skipped) it reruns sim.py at the same shape on WRIT_BIN and checks:
# every edit sealed by the end, cross 0, hook exit 0, nothing printed to the agent, store
# ok, and, when the binary's sha256 equals the published run's, hook_ms p50 and window_ms
# p50 within FLEET_TOLERANCE (max printed, not enforced). On another build the timings are
# printed side by side and not compared: a faster build is not a failed reproduction.
#
# Usage: fleet_numbers.sh [WRIT_BIN] [SCRATCH_DIR]
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/_lib.sh"
ROOT="$(cd "$HERE/../.." && pwd)"
W="$(rc_bin "${1:-$ROOT/target/release/writ}")"
BENCH="$ROOT/bench/fleet_bench.py"
SIM="$ROOT/bench/fleet/sim.py"
DEFAULT_SIM="$(cd "$ROOT/.." && pwd)/TEAM_WRIT_DOCS/evidence/0.4.3-fleet-sim"
PUB="${FLEET_JSON-}"; [ -z "${FLEET_JSON+x}" ] && [ -d "$DEFAULT_SIM" ] && PUB="$DEFAULT_SIM"
[ -n "$PUB" ] && PUB="$(cd "$(dirname "$PUB")" && pwd)/$(basename "$PUB")"
SIM_FILES=()
if [ -n "$PUB" ]; then
    if [ -d "$PUB" ]; then cands=("$PUB"/*.json); else cands=("$PUB"); fi
    for f in "${cands[@]}"; do
        case "$(basename "$f")" in before-*|bare-seal-*) continue ;; esac
        python3 -c 'import json,sys; sys.exit(0 if "hook_ms" in json.load(open(sys.argv[1])) else 1)' "$f" 2>/dev/null && SIM_FILES+=("$f")
    done
fi
unset WRIT_AGENT_ID
rc_setup .fleet_numbers fleet-numbers "${2:-}"
echo "binary: $W ($("$W" --version))"
if [ ${#SIM_FILES[@]} -gt 0 ]; then
    TOL="${FLEET_TOLERANCE:-2.0}"; n=0
    MYSHA="$(shasum -a 256 "$W" | cut -c1-12)"
    cat > "$D/cmp.py" <<'PY'
import json, sys
a, b = (json.load(open(p)) for p in sys.argv[1:3]); t = float(sys.argv[3]); ok = True
for k in ("hook_ms", "window_ms"):
    x, y = a[k]["p50"], b[k]["p50"]
    print(f"  {k}: published p50 {x} max {a[k]['max']}, rerun p50 {y} max {b[k]['max']}")
    ok = ok and x / t <= y <= x * t
sys.exit(0 if ok else 1)
PY
    for f in "${SIM_FILES[@]}"; do
        n=$((n+1)); name="$(basename "$f" .json)"
        args=$(python3 - "$f" "$ROOT" <<'PY'
import json, sys
r = json.load(open(sys.argv[1])); a = ["--agents", str(r["agents"]), "--edits", str(r["edits_per_agent"])]
if r["repo"].get("cloned_from"): a += ["--clone", sys.argv[2]]
else: a += ["--files", "1000"]
if not r.get("concurrent", True): a.append("--sequential")
print(" ".join(a))
PY
)
        echo "published: $f (sim.py $args)"
        python3 "$SIM" --writ "$W" $args --json "$D/$name.json" > "$D/$name.out" 2>&1; RC=$?
        tail -3 "$D/$name.out"
        chk "S$n.1 $name: sim.py exits 0 on this binary" '[ "$RC" -eq 0 ] && [ -s "$D/$name.json" ]'
        chk "S$n.2 $name: every edit sealed by the end, cross 0, hook exit 0, silent, store ok" 'rc_json "$D/$name.json" "r[\"edits_sealed_by_end\"] == r[\"edits\"] and r[\"edits_never_sealed\"] == 0 and r[\"cross\"] == 0 and r[\"hook_exit_nonzero\"] == 0 and r[\"hook_stdout_nonempty\"] == 0 and r[\"store_ok\"] is True"'
        PUBSHA="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["binary"]["sha256"][:12])' "$f")"
        if [ "$PUBSHA" = "$MYSHA" ]; then
            chk "S$n.3 $name: hook p50 and window p50 within ${TOL}x of published (same binary $MYSHA)" 'python3 "$D/cmp.py" "$f" "$D/$name.json" "$TOL"'
        else
            python3 "$D/cmp.py" "$f" "$D/$name.json" "$TOL" > /dev/null; python3 "$D/cmp.py" "$f" "$D/$name.json" 1e9
            echo "SKIP S$n.3 $name: published on $PUBSHA, tested $MYSHA; timings printed, not compared across builds"
        fi
    done
    exit $fail
fi
chk "1 bench/fleet_bench.py exists" '[ -f "$BENCH" ]'
[ -f "$BENCH" ] || exit $fail

if [ -n "$PUB" ]; then
    read -r N K F < <(python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); print(r["agents"], r["seals"]//r["agents"], r["repo"]["files"])' "$PUB")
    echo "published: $PUB ($N agents, $K seals each, $F files)"
else
    read -r N K F <<< "${FLEET_SIZE:-5 4 100}"
fi
python3 "$BENCH" --writ "$W" --agents "$N" --seals-per-agent "$K" --files "$F" --json "$D/run.json" > "$D/stdout.txt" 2>"$D/stderr.txt"
RC=$?
chk "2 fleet_bench.py exits 0 ($N x $K on $F files)" '[ "$RC" -eq 0 ]'
[ "$RC" -eq 0 ] || { tail -20 "$D/stderr.txt"; exit 1; }
cat "$D/stdout.txt"
chk "3 JSON has every number, the binary (path, version, sha256) and the date" 'rc_json "$D/run.json" "
all(isinstance(r.get(k), (int, float)) and r[k] > 0 for k in (\"context_ms\",\"doctor_fast_ms\",\"survival_full_ms\",\"survival_incremental_ms\",\"handoff_ms\"))
and r[\"seal_ms\"][\"p50\"] > 0 and r[\"seal_ms\"][\"max\"] >= r[\"seal_ms\"][\"p50\"]
and all(r[\"binary\"].get(k) for k in (\"path\",\"version\",\"sha256\")) and r.get(\"date\")"'
chk "4 counts: agents == specs == $N, seals == $((N*K)), store_ok" \
    'rc_json "$D/run.json" "r[\"agents\"] == r[\"specs\"] == $N and r[\"seals\"] == $((N*K)) and r[\"store_ok\"] is True"'
chk "5 stdout names the binary and prints each number" \
    'grep -qF "$("$W" --version | awk "{print \$2}")" "$D/stdout.txt" && grep -qiE "seal.*p50" "$D/stdout.txt" && grep -qi survival "$D/stdout.txt"'
if [ -n "$PUB" ]; then
    TOL="${FLEET_TOLERANCE:-2.0}"
    chk "6 rerun matches the published counts and repo size" 'python3 - "$PUB" "$D/run.json" <<PY
import json, sys
a, b = (json.load(open(p)) for p in sys.argv[1:])
sys.exit(0 if all(a[k] == b[k] for k in ("agents", "specs", "seals")) and a["repo"]["files"] == b["repo"]["files"] else 1)
PY'
    chk "7 every timing within ${TOL}x of the published one" 'python3 - "$PUB" "$D/run.json" "$TOL" <<PY
import json, sys
a, b = (json.load(open(p)) for p in sys.argv[1:3]); t = float(sys.argv[3]); bad = []
pairs = [("seal p50", a["seal_ms"]["p50"], b["seal_ms"]["p50"]), ("seal max", a["seal_ms"]["max"], b["seal_ms"]["max"])]
pairs += [(k, a[k], b[k]) for k in ("context_ms","doctor_fast_ms","survival_full_ms","survival_incremental_ms","handoff_ms")]
for name, x, y in pairs:
    print(f"  {name}: published {x}, rerun {y}")
    if not (x / t <= y <= x * t):
        bad.append(name)
sys.exit(1 if bad else 0)
PY'
fi
exit $fail
