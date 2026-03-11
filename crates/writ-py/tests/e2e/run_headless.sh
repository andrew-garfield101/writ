#!/usr/bin/env bash
# Run E2E tests in headless mode (CI).
#
# Usage:
#   ./e2e/run_headless.sh              # Skip slow + live tests
#   ./e2e/run_headless.sh --all        # Include slow tests
#   ./e2e/run_headless.sh --live       # Include live agent tests
#
# Exit code: 0 on success, 1 on failure.
# Results: e2e/results/latest.json

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"

cd "$REPO_ROOT"

# Activate venv if available
source .venv/bin/activate 2>/dev/null || true

# Default: skip live and slow
MARKER="-m not live and not slow"
EXTRA_ARGS=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --all) MARKER="-m not live"; shift ;;
        --live) MARKER=""; shift ;;
        *) EXTRA_ARGS="$EXTRA_ARGS $1"; shift ;;
    esac
done

pytest crates/writ-py/tests/e2e/tests/ \
    -v \
    --tb=short \
    $MARKER \
    --results-dir "$SCRIPT_DIR/results" \
    $EXTRA_ARGS
