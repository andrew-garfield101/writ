# Shared helpers for bench/release_checks scripts (sourced, not run).
# rc_setup <marker> <prefix> [SCRATCH_DIR]: validate and enter the scratch dir.
rc_setup() {
    local mark="$1" prefix="$2" d="${3:-}"
    if [ -n "$d" ]; then
        if [ -d "$d" ] && [ -n "$(ls -A "$d")" ] && [ ! -f "$d/$mark" ]; then
            echo "refusing to reuse non-empty $d (no $mark marker); pass an empty or new dir" >&2
            exit 2
        fi
        rm -rf "$d"; mkdir -p "$d"
    else
        local tmp="${TMPDIR:-/tmp}"
        d="$(mktemp -d "${tmp%/}/$prefix.XXXXXX")"
    fi
    cd "$d"; touch "$mark"; D="$(pwd -P)"
    echo "scratch: $D"
}
# rc_bin <path-or-name>: absolute, executable writ or exit 2.
rc_bin() {
    local w="$1"
    case "$w" in /*) ;; */*) w="$PWD/$w" ;; *) w="$(command -v "$w" || echo "$w")" ;; esac
    [ -x "$w" ] || { echo "writ binary not found or not executable: $w" >&2; exit 2; }
    echo "$w"
}
# rc_git_project <dir>: a git repo with one commit, entered.
rc_git_project() {
    mkdir -p "$1"; cd "$1"
    git init -q -b main
    printf 'print("hi")\n' > app.py
    git add -A && git -c user.name=t -c user.email=t@localhost commit -qm base
}
fail=0
chk() { if eval "$2"; then echo "PASS $1"; else echo "FAIL $1"; fail=1; fi; }
# jq-free JSON assertion: rc_json <file> <python expression over r>
rc_json() { python3 -c "import json,sys; r=json.load(open(sys.argv[1])); sys.exit(0 if ($2) else 1)" "$1"; }
