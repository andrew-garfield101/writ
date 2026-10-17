#!/usr/bin/env bash
# Live fleet run (sprint 6, 0.4.3): two `claude -p` mains on one Flask
# project, each told to split its task across two subagents through the
# Agent tool. The prompts say nothing about writ; the agents see only what
# `writ init -y` installs (zero-instruction template). Driven by the stranger
# kit (setup, launch, collect, score), which records every hook firing
# (.writ/hooks.log), hook lines handed back to each agent, hand-run writ
# commands and the edit-to-seal window, main and subagent.
#
# Usage: bench/fleet/live_fleet.sh WRIT_BIN OUT_DIR [BUILT_FROM] [-- launch args]
#   e.g. bench/fleet/live_fleet.sh target/release/writ /tmp/fleet-live 3d331e94c824
# Exit-criterion runs (0.4.3) use the sprint 5 two-deliverable prompts:
#   PROMPT1=bench/fleet/prompts/exit-1.txt PROMPT2=bench/fleet/prompts/exit-2.txt \
#     bench/fleet/live_fleet.sh BIN OUT SEAL                         # run A
#   ... bench/fleet/live_fleet.sh BIN OUT SEAL -- --inject stale-claim   # run B
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WRIT_BIN="$1"; OUT="$2"; BUILT="${3:-}"
shift 3 2>/dev/null || shift $#
[[ "${1:-}" == "--" ]] && shift
KIT="$ROOT/tools/stranger_kit/stranger.py"
SCAFFOLDS="${SCAFFOLDS:-$ROOT/../TEAM_WRIT_DOCS/testing/roundtrip}"
PROJ="$OUT/proj"
mkdir -p "$OUT"
[[ -e "$PROJ" ]] && { echo "$PROJ exists; use a fresh OUT_DIR" >&2; exit 2; }
python3 - "$PROJ" "$SCAFFOLDS" <<'PY'
import subprocess, sys
from pathlib import Path
p = Path(sys.argv[1]); p.mkdir(parents=True)
sys.path.insert(0, sys.argv[2])
from scaffolds import get_scaffold
run = lambda *a: subprocess.run(a, cwd=p, check=True, capture_output=True)
run("git", "init", "-q", "-b", "main")
run("git", "config", "user.email", "fixture@example.invalid"); run("git", "config", "user.name", "fixture")
get_scaffold("python-api")(p)
run("git", "add", "-A"); run("git", "commit", "-qm", "initial scaffold")
PY
setup=(python3 "$KIT" setup "$PROJ" "$OUT" --writ "$WRIT_BIN")
[[ -n "$BUILT" ]] && setup+=(--built-from "$BUILT")
"${setup[@]}"
[[ "${LIVE_FLEET_SETUP_ONLY:-0}" == "1" ]] && { echo "setup only: $OUT"; exit 0; }
python3 "$KIT" launch "$PROJ" "$OUT" \
    --prompt "${PROMPT1:-$ROOT/bench/fleet/prompts/fleet-1.txt}" \
    --prompt "${PROMPT2:-$ROOT/bench/fleet/prompts/fleet-2.txt}" "$@"
python3 "$KIT" collect "$PROJ" "$OUT"
python3 "$KIT" score "$OUT"
