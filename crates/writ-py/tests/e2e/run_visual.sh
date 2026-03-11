#!/usr/bin/env bash
# Launch E2E tests in a tmux session for visual observation.
#
# Usage:
#   ./e2e/run_visual.sh                  # Run all scripted tests
#   ./e2e/run_visual.sh --live           # Include live agent tests
#   ./e2e/run_visual.sh --section s1     # Run only golden path tests
#
# Attach: tmux attach -t writ-e2e

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"
SESSION="writ-e2e"

# Parse args
PYTEST_ARGS=""
SECTION=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --live) PYTEST_ARGS="$PYTEST_ARGS --live"; shift ;;
        --section) SECTION="$2"; shift 2 ;;
        *) PYTEST_ARGS="$PYTEST_ARGS $1"; shift ;;
    esac
done

# Build test path
TEST_PATH="$SCRIPT_DIR/tests/"
if [[ -n "$SECTION" ]]; then
    TEST_PATH="$SCRIPT_DIR/tests/test_${SECTION}*.py"
fi

# Kill existing session
tmux kill-session -t "$SESSION" 2>/dev/null || true

# Create session — main pane runs tests
tmux new-session -d -s "$SESSION" -x 200 -y 50

# Pane 0: Run the tests
tmux send-keys -t "$SESSION" \
    "cd $REPO_ROOT && source .venv/bin/activate 2>/dev/null; echo '=== WRIT E2E TEST SUITE ===' && echo '' && pytest $TEST_PATH -v --tb=short $PYTEST_ARGS 2>&1 | tee $SCRIPT_DIR/results/visual_output.log; echo '' && echo '=== DONE ===' && echo 'Results: cat crates/writ-py/tests/e2e/results/latest.json | python3 -m json.tool'" Enter

# Split bottom pane
tmux split-window -v -t "$SESSION" -p 30

# Pane 1: Show results when available
tmux send-keys -t "$SESSION" \
    "cd $REPO_ROOT && echo 'Waiting for results...' && while [ ! -f crates/writ-py/tests/e2e/results/latest.json ]; do sleep 2; done && echo '=== RESULTS ===' && python3 -m json.tool crates/writ-py/tests/e2e/results/latest.json 2>/dev/null | head -30" Enter

# Layout
tmux select-layout -t "$SESSION" main-horizontal 2>/dev/null || true

echo ""
echo "  tmux session created: $SESSION"
echo "  Attach with:  tmux attach -t $SESSION"
echo ""
