#!/usr/bin/env bash
set -euo pipefail

# Bump version across all 4 locations in the writ project.
#
# Usage:
#   ./scripts/bump-version.sh 0.2.0
#
# Locations updated:
#   1. Cargo.toml (workspace)
#   2. crates/writ-py/pyproject.toml
#   3. crates/writ-py/python/writ/__init__.py
#   4. packaging/homebrew/writ.rb

if [ $# -ne 1 ]; then
    echo "Usage: $0 <version>"
    echo "Example: $0 0.2.0"
    exit 1
fi

VERSION="$1"

# Validate version format (semver without v prefix)
if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$ ]]; then
    echo "Error: version must be semver format (e.g., 0.2.0 or 0.2.0-rc1)"
    exit 1
fi

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

echo "Bumping version to $VERSION"

# 1. Cargo.toml (workspace root — only the [workspace.package] version line)
sed -i '' "s/^version = \"[^\"]*\"/version = \"$VERSION\"/" "$REPO_ROOT/Cargo.toml"
echo "  Updated Cargo.toml"

# 2. pyproject.toml
sed -i '' "s/^version = \"[^\"]*\"/version = \"$VERSION\"/" "$REPO_ROOT/crates/writ-py/pyproject.toml"
echo "  Updated pyproject.toml"

# 3. __init__.py
sed -i '' "s/__version__ = \"[^\"]*\"/__version__ = \"$VERSION\"/" "$REPO_ROOT/crates/writ-py/python/writ/__init__.py"
echo "  Updated __init__.py"

# 4. Homebrew formula
sed -i '' "s/version \"[^\"]*\"/version \"$VERSION\"/" "$REPO_ROOT/packaging/homebrew/writ.rb"
echo "  Updated writ.rb"

echo ""
echo "Version bumped to $VERSION in all 4 locations."
echo ""
echo "Next steps:"
echo "  git add -u"
echo "  git commit -m \"chore: bump version to $VERSION\""
echo "  git tag v$VERSION"
echo "  git push origin main --tags"
