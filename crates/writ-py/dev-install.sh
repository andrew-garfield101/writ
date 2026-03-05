#!/usr/bin/env bash
# dev-install.sh — Build and install writ (Python + CLI) into the active venv.
#
# Usage:
#   cd crates/writ-py
#   ./dev-install.sh
#
# maturin develop does NOT process .data/scripts/, so this script handles
# building the CLI binary and copying it into the venv manually.
set -euo pipefail

if [ -z "${VIRTUAL_ENV:-}" ]; then
    echo "Error: no virtual environment active. Activate a venv first." >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Build CLI binary
echo "Building writ CLI..."
cargo build --release -p writ-cli

# Build Python extension
echo "Building Python extension..."
cd "$SCRIPT_DIR"
maturin develop --release

# Copy CLI into venv bin directory
VENV_BIN="$(python -c 'import sys; print(sys.prefix)')/bin"
cp "$REPO_ROOT/target/release/writ" "$VENV_BIN/writ"
chmod +x "$VENV_BIN/writ"
echo "Installed writ CLI to $VENV_BIN/writ"

# Verify
echo ""
echo "Verification:"
which writ && writ --version
python -c "import writ; print(f'Python API: writ {writ.__version__}')"
