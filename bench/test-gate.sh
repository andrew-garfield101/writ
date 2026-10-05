#!/usr/bin/env bash
# Full-suite test gate (scorecard, permanent from sprint 1).
#
# Passes only when both languages are green with explicit expected failures:
#   Rust:   0 failed; every #[ignore] carries a reason (#[ignore = "..."]).
#   Python: 0 failed, 0 errors, 0 XPASS (xfails are strict=True); skips allowed.
# Writes bench/context/results/test-gate.json for bench/writ-scorecard.sh.
#
# Usage: bench/test-gate.sh            (needs maturin develop done in the venv)
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CARGO="${CARGO:-$(command -v cargo || echo /Users/agarfield/.cargo/bin/cargo)}"
VENV="${VENV:-$ROOT/.venv311}"
OUT="$ROOT/bench/context/results/test-gate.json"
mkdir -p "$(dirname "$OUT")"
cd "$ROOT"

# Logs kept next to the JSON so a red gate can be diagnosed after the fact.
rust_log="$(dirname "$OUT")/test-gate-rust.log"
py_log="$(dirname "$OUT")/test-gate-python.log"

# --no-fail-fast: without it cargo stops at the first failing test binary and
# the counts silently undercount every binary after it.
"$CARGO" test -p writ-core -p writ-cli --no-fail-fast >"$rust_log" 2>&1
rust_rc=$?
bare_ignores="$(grep -rnE '#\[ignore\][[:space:]]*$' crates --include='*.rs' | grep -v '^\s*//' || true)"

# Store verify on this repo with the binary the Rust run just built. verify
# exits 1 on any failure; capture it. This repo's three finding 28 blobs are
# the expected outcome, anything else fails the gate.
WRIT_BIN="${WRIT:-$ROOT/target/debug/writ}"
verify_rc=0
verify_json="$(python3 "$ROOT/bench/verify_check.py" "$ROOT" --writ "$WRIT_BIN" \
    --allow-missing ae01d21c123e --allow-missing 50ff4a66d3af --allow-missing 80dbd0cf0eac 2>&1)" \
    || verify_rc=$?

# shellcheck disable=SC1091
source "$VENV/bin/activate"
python -m pytest crates/writ-py/tests bench/test_verify_check.py -q -p no:cacheprovider -rxXf >"$py_log" 2>&1
py_rc=$?

RUST_LOG="$rust_log" PY_LOG="$py_log" RUST_RC=$rust_rc PY_RC=$py_rc \
VERIFY_JSON="$verify_json" VERIFY_RC=$verify_rc \
BARE="$bare_ignores" OUT="$OUT" python3 - <<'PY'
import json, os, re, datetime as dt

rust = open(os.environ["RUST_LOG"]).read()
py = open(os.environ["PY_LOG"]).read()
r = {"passed": 0, "failed": 0, "ignored": 0}
for m in re.finditer(r"test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored", rust):
    r["passed"] += int(m[1]); r["failed"] += int(m[2]); r["ignored"] += int(m[3])
tail = py.strip().splitlines()[-1] if py.strip() else ""
p = {k: int(v) for v, k in re.findall(
    r"(\d+) (passed|failed|skipped|xfailed|xpassed|errors?|deselected)", tail)}
p["errors"] = p.pop("error", 0) + p.pop("errors", 0)
bare = [b for b in os.environ["BARE"].splitlines() if b.strip()]
# Every ignored Rust test must be on this list; each names its finding.
EXPECTED_IGNORED = {
    "ignore::tests::repo_level::test_rejected_scope_violation_seal_leaves_object_count_unchanged",  # 17
    "test_large_file_block_insert_reports_only_inserted_lines",  # 23
    "test_insert_that_crosses_line_limit_is_still_minimal",  # 23
}
ignored_tests = set(re.findall(r"^test (\S+) \.\.\. ignored", rust, re.M))
unexpected_ignores = sorted(ignored_tests - EXPECTED_IGNORED)
stale_ignores = sorted(EXPECTED_IGNORED - ignored_tests)
problems = []
if int(os.environ["RUST_RC"]) != 0 or r["failed"]:
    problems.append(f"rust: {r['failed']} failed (rc {os.environ['RUST_RC']})")
if bare:
    problems.append(f"rust: {len(bare)} #[ignore] without a reason")
if unexpected_ignores:
    problems.append(f"rust: ignored tests not on the expected list: {unexpected_ignores}")
if stale_ignores:
    problems.append(f"rust: expected-ignored tests no longer ignored (fixed? update list): {stale_ignores}")
for key in ("failed", "errors", "xpassed"):
    if p.get(key):
        problems.append(f"python: {p[key]} {key}")
if int(os.environ["PY_RC"]) not in (0,) and not problems:
    problems.append(f"python: pytest rc {os.environ['PY_RC']}")
try:
    verify = json.loads(os.environ["VERIFY_JSON"])
    if verify["verdict"] == "FAIL":
        problems.extend(f"verify: {v}" for v in verify["problems"])
except json.JSONDecodeError:
    verify = {"verdict": "ERROR", "detail": os.environ["VERIFY_JSON"].strip()[:300]}
    problems.append(f"verify: could not run (rc {os.environ['VERIFY_RC']}): {verify['detail']}")
result_failures = re.findall(r"^test (\S+) \.\.\. FAILED", rust, re.M)
result = {
    "captured_at": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
    "rust": r, "python": p, "verify": verify, "bare_ignores": bare,
    "rust_failures": result_failures,
    "rust_ignored": sorted(ignored_tests),
    "python_xfails": re.findall(r"^XFAIL (\S+)", py, re.M),
    "python_failures": re.findall(r"^(?:FAILED|ERROR) (\S+)", py, re.M),
    "passed": not problems, "problems": problems,
}
json.dump(result, open(os.environ["OUT"], "w"), indent=2)
print(json.dumps(result, indent=2))
PY
python3 -c "import json,sys; sys.exit(0 if json.load(open('$OUT'))['passed'] else 1)"
