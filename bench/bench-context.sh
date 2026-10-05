#!/usr/bin/env bash
# writ context cost benchmark + budget gate (sprint "Context on a Budget", B.1).
#
# Usage: bench/bench-context.sh [WRIT_BIN] [extra run_bench.py args...]
#   WRIT_BIN defaults to target/release/writ.
#   Results go to bench/context/results/latest.{json,md} unless --out is given.
#   Exits 1 on budget regression; pass --no-fail for baseline capture.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WRIT_BIN="${1:-$ROOT/target/release/writ}"
shift || true

ARGS=("$@")
if [[ " ${ARGS[*]-} " != *" --out "* ]]; then
    ARGS+=(--out "$ROOT/bench/context/results/latest.json")
fi

exec python3 "$ROOT/bench/context/run_bench.py" --writ "$WRIT_BIN" "${ARGS[@]}"
