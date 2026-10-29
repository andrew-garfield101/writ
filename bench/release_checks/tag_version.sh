#!/usr/bin/env bash
# Retro rule 1 (sprint 7, finding 208): the tag carries the version it names.
# The first v0.4.4 tag pointed at a commit whose Cargo.toml still said 0.4.3,
# because the bump's files went under another spec and finish committed only
# Cargo.lock. This check reads every version site AT THE TAGGED COMMIT, not in
# the working tree, so a bump left out of the commit is caught before the push.
#
# Usage:
#   tag_version.sh <tag> [<rev>]   read the files at <rev> (default: <tag>
#                                  itself); <tag> is vX.Y.Z and names the
#                                  version expected. Run with <rev> = HEAD
#                                  before creating the tag, and with the tag
#                                  alone after creating it, before pushing it.
#   tag_version.sh --tree          working tree: the sites agree with each
#                                  other and with the CHANGELOG's top heading
#                                  (bench/test-gate.sh under GATE_RELEASE=1).
# Sites: Cargo.toml [workspace.package] version, crates/writ-py/pyproject.toml,
# crates/writ-py/python/writ/__init__.py, packaging/homebrew/writ.rb, the
# workspace crates in Cargo.lock, and the first `## [X.Y.Z] — YYYY-MM-DD`
# heading in CHANGELOG.md, whose section must carry no <!-- --> or <placeholder>.
# Exit 0 all PASS, 1 any FAIL, 2 bad arguments or unknown rev.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

mode=""; tag=""; rev=""
case "${1:-}" in
    --tree) mode=tree ;;
    v[0-9]*) mode=rev; tag="$1"; rev="${2:-$1}" ;;
    *) echo "usage: $0 <vX.Y.Z> [<rev>] | --tree" >&2; exit 2 ;;
esac

if [ "$mode" = rev ]; then
    git rev-parse -q --verify "$rev^{commit}" >/dev/null || { echo "unknown rev: $rev" >&2; exit 2; }
    commit="$(git rev-parse --short=12 "$rev^{commit}")"
    echo "checking $tag against $rev ($commit)"
    show() { git show "$commit:$1" 2>/dev/null; }
else
    echo "checking the working tree"
    show() { cat "$1" 2>/dev/null; }
fi

v_cargo="$(show Cargo.toml | sed -n 's/^version = "\([^"]*\)".*/\1/p' | head -1)"
v_py="$(show crates/writ-py/pyproject.toml | sed -n 's/^version = "\([^"]*\)".*/\1/p' | head -1)"
v_init="$(show crates/writ-py/python/writ/__init__.py | sed -n 's/^__version__ = "\([^"]*\)".*/\1/p' | head -1)"
v_brew="$(show packaging/homebrew/writ.rb | sed -n 's/^ *version "\([^"]*\)".*/\1/p' | head -1)"
# Workspace crates in Cargo.lock: the [[package]] blocks with no `source` line
# (registry and git packages always carry one). One distinct version expected.
v_lock="$(show Cargo.lock | awk '
    /^\[\[package\]\]/ { if (name != "" && src == 0) print ver; name = ""; ver = ""; src = 0; next }
    /^name = /    { name = $3 }
    /^version = / { ver = $3; gsub(/"/, "", ver) }
    /^source = /  { src = 1 }
    END { if (name != "" && src == 0) print ver }' | sort -u | paste -sd, -)"
n_lock="$(show Cargo.lock | awk '/^\[\[package\]\]/{if(n!=""&&!s)c++;n="";s=0;next} /^name = /{n=$3} /^source = /{s=1} END{if(n!=""&&!s)c++; print c+0}')"
cl_head="$(show CHANGELOG.md | grep -m1 '^## \[')"
v_cl="$(printf '%s' "$cl_head" | sed -n 's/^## \[\([^]]*\)\].*/\1/p')"

printf '  %-28s %s\n' Cargo.toml "${v_cargo:-<none>}" pyproject.toml "${v_py:-<none>}" \
    __init__.py "${v_init:-<none>}" writ.rb "${v_brew:-<none>}" "Cargo.lock workspace (${n_lock})" "${v_lock:-<none>}" \
    "CHANGELOG top" "${cl_head:-<none>}"

fail=0; n=0; ok=0
chk() {
    n=$((n + 1))
    if [ "$2" = "$3" ] && [ -n "$2" ]; then echo "PASS $1 ($2)"; ok=$((ok + 1)); else echo "FAIL $1: '$2' vs '$3'"; fail=1; fi
}
want="${tag#v}"
src="the tag"
[ "$mode" = tree ] && { want="$v_cargo"; src="Cargo.toml"; }
[ "$mode" = rev ] && chk "1 Cargo.toml equals the tag" "$v_cargo" "$want"
chk "2 pyproject.toml equals $src" "$v_py" "$want"
chk "3 __init__.py equals $src" "$v_init" "$want"
chk "4 writ.rb equals $src" "$v_brew" "$want"
chk "5 Cargo.lock workspace crates equal $src" "$v_lock" "$want"
chk "6 CHANGELOG top heading equals $src" "$v_cl" "$want"
# The heading carries a real date, not a placeholder (the release style is `## [X.Y.Z] — YYYY-MM-DD`).
n=$((n + 1))
if printf '%s' "$cl_head" | grep -Eq '^## \[[^]]+\] — [0-9]{4}-[0-9]{2}-[0-9]{2}$'; then
    echo "PASS 7 CHANGELOG heading dated"; ok=$((ok + 1))
else
    echo "FAIL 7 CHANGELOG heading not '## [X.Y.Z] — YYYY-MM-DD': '$cl_head'"; fail=1
fi
# Nothing staged ships: the top section carries no HTML comment and no <placeholder>.
n=$((n + 1))
cl_top="$(show CHANGELOG.md | awk '/^## \[/{c++} c==1')"
# Code spans are stripped first: `--since <seal>` is usage, not a placeholder.
staged="$(printf '%s\n' "$cl_top" | sed 's/`[^`]*`//g' | grep -nE '<!--|-->|<(date|seal|tbd|TBD|todo|TODO)[^>]*>' | head -3)"
if [ -z "$staged" ]; then
    echo "PASS 8 CHANGELOG top section has no staged comment or placeholder"; ok=$((ok + 1))
else
    echo "FAIL 8 CHANGELOG top section has staged text: $(printf '%s' "$staged" | tr '\n' ' ')"; fail=1
fi
echo "$ok/$n"
exit $fail
