#!/usr/bin/env bash
# Full-suite test gate (scorecard, permanent from sprint 1).
#
# Passes only when both languages are green with explicit expected failures:
#   Rust:   0 failed; every #[ignore] carries a reason (#[ignore = "..."]).
#   Python: 0 failed, 0 errors, 0 XPASS (xfails are strict=True); skips allowed.
# Writes bench/context/results/test-gate.json for bench/writ-scorecard.sh.
#
# Usage: bench/test-gate.sh
#   Builds target/release/writ and the Python extension from this tree first:
#   the e2e tests prefer target/release/writ and pytest imports whatever writ
#   is installed in the venv, so either could otherwise be stale. Set
#   GATE_SKIP_BUILD=1 only when both were just built from this tree.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CARGO="${CARGO:-$(command -v cargo || echo /Users/agarfield/.cargo/bin/cargo)}"
VENV="${VENV:-$ROOT/.venv311}"
OUT="$ROOT/bench/context/results/test-gate.json"
mkdir -p "$(dirname "$OUT")"
cd "$ROOT"

# Identity is scrubbed for the whole gate: the MCP unit tests and the Python
# resolver tests assume no WRIT_AGENT_ID (S.3), and an agent running the gate
# exports its own. Tests that need an identity set it per command.
unset WRIT_AGENT_ID

# Logs kept next to the JSON so a red gate can be diagnosed after the fact.
rust_log="$(dirname "$OUT")/test-gate-rust.log"
py_log="$(dirname "$OUT")/test-gate-python.log"

# shellcheck disable=SC1091
source "$VENV/bin/activate"

build_log="$(dirname "$OUT")/test-gate-build.log"
build_rc=0
if [[ "${GATE_SKIP_BUILD:-0}" != "1" ]]; then
    {
        "$CARGO" build --release -p writ-cli &&
            (cd "$ROOT/crates/writ-py" && PATH="$(dirname "$CARGO"):$PATH" maturin develop --release)
    } >"$build_log" 2>&1 || build_rc=$?
else
    echo "GATE_SKIP_BUILD=1: using existing release binary and Python extension" >"$build_log"
fi

# --no-fail-fast: without it cargo stops at the first failing test binary and
# the counts silently undercount every binary after it.
"$CARGO" test -p writ-core -p writ-cli -p writ-mcp --no-fail-fast >"$rust_log" 2>&1
rust_rc=$?
# Finding 27 (CC decision 2026-10-05): tests asserting wall-clock bounds are
# #[ignore]d out of the default suite and run here alone, one thread, after
# the parallel suite has finished, so load from other test binaries cannot
# push them over their limits. A failure here is still a gate failure.
timing_log="$(dirname "$OUT")/test-gate-timing.log"
TIMING_TESTS=(
    repo::scale_tests::test_scale_100_specs
    repo::scale_tests::test_scale_500_seals_linear_chain
    repo::scale_tests::test_scale_context_with_many_seals
    repo::scale_tests::test_scale_parallel_specs
    repo::scale_tests::test_scale_many_files_in_single_seal
    repo::chain_tests::test_chain_100_seals_performance
    repo::context_edge_case_tests::test_context_500_seals_all_scopes
)
timing_load="$(uptime | sed -E 's/.*load averages?: *//')"
"$CARGO" test -p writ-core --lib --no-fail-fast -- --ignored --exact --test-threads=1 \
    "${TIMING_TESTS[@]}" >"$timing_log" 2>&1
timing_rc=$?
bare_ignores="$(grep -rnE '#\[ignore\][[:space:]]*$' crates --include='*.rs' | grep -v '^\s*//' || true)"

# Store verify on this repo with the binary the Rust run just built. verify
# exits 1 on any failure; capture it. This repo's three finding 28 blobs are
# the expected outcome, anything else fails the gate.
WRIT_BIN="${WRIT:-$ROOT/target/debug/writ}"
verify_rc=0
verify_json="$(python3 "$ROOT/bench/verify_check.py" "$ROOT" --writ "$WRIT_BIN" \
    --allow-missing ae01d21c123e --allow-missing 50ff4a66d3af --allow-missing 80dbd0cf0eac 2>&1)" \
    || verify_rc=$?

# writ doctor on this repo's own store (sprint 3, 0.4.0 exit criterion 4, 5).
# Release binary, exit 0 required with GATE_RELEASE=1 (no red otherwise) (this repo's finding 28 blobs are excused by
# [doctor] allow_missing in .writ/config.toml), wall time median of 5 runs
# against a 300 ms budget. Honesty: the shipped binary and the Python package
# never contain "safe to finish" (the release build carries no test code, so
# the tests asserting its absence do not count). The brief must stay under
# 2 KB and carry a `doctor:` line.
DOCTOR_BIN="$ROOT/target/release/writ"
doctor_json="$(dirname "$OUT")/test-gate-doctor.json"
doctor_rc=0
"$DOCTOR_BIN" doctor --format json >"$doctor_json" 2>&1 || doctor_rc=$?
doctor_ms="$(python3 - "$DOCTOR_BIN" "$ROOT" <<'TIMING'
import statistics, subprocess, sys, time
runs = []
for _ in range(5):
    t = time.perf_counter()
    subprocess.run([sys.argv[1], "doctor", "--format", "json"], cwd=sys.argv[2],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    runs.append((time.perf_counter() - t) * 1000)
print(f"{statistics.median(runs):.0f}")
TIMING
)"
safe_hits="$(LC_ALL=C grep -l -i "safe to finish" "$DOCTOR_BIN" 2>/dev/null || true)
$(grep -rli "safe to finish" crates/writ-py/python 2>/dev/null || true)"
brief_out="$(dirname "$OUT")/test-gate-brief.txt"
"$DOCTOR_BIN" context --format brief >"$brief_out" 2>/dev/null || true

python -m pytest crates/writ-py/tests bench/test_verify_check.py -q -p no:cacheprovider -rxXf >"$py_log" 2>&1
py_rc=$?

BUILD_RC=$build_rc BUILD_LOG="$build_log" \
TIMING_LOG="$timing_log" TIMING_RC=$timing_rc TIMING_LOAD="$timing_load" TIMING_TESTS="${TIMING_TESTS[*]}" \
RUST_LOG="$rust_log" PY_LOG="$py_log" RUST_RC=$rust_rc PY_RC=$py_rc \
VERIFY_JSON="$verify_json" VERIFY_RC=$verify_rc \
DOCTOR_JSON="$doctor_json" DOCTOR_RC=$doctor_rc DOCTOR_MS="$doctor_ms" SAFE_HITS="$safe_hits" BRIEF_OUT="$brief_out" \
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
timing_log = open(os.environ["TIMING_LOG"]).read()
timing_expected = os.environ["TIMING_TESTS"].split()
timing = {
    "rc": int(os.environ["TIMING_RC"]),
    "load_at_start": os.environ["TIMING_LOAD"],
    "passed": re.findall(r"^test (\S+) \.\.\. ok", timing_log, re.M),
    "failed": re.findall(r"^test (\S+) \.\.\. FAILED", timing_log, re.M),
}
timing["not_run"] = sorted(set(timing_expected) - set(timing["passed"]) - set(timing["failed"]))
m = re.search(r"finished in ([\d.]+)s", timing_log)
timing["seconds"] = float(m[1]) if m else None
# Every ignored Rust test must be on this list; each names its finding.
EXPECTED_IGNORED = {
    "ignore::tests::repo_level::test_rejected_scope_violation_seal_leaves_object_count_unchanged",  # 17
}
# Gate checks waiting on an implementation: reported, not failed, and a
# problem once they pass (remove the entry then), like EXPECTED_IGNORED.
PENDING_GATE_CHECKS = {

}
# The timing group is ignored in the default run by design (run above).
EXPECTED_IGNORED |= set(timing_expected)
ignored_tests = set(re.findall(r"^test (\S+) \.\.\. ignored", rust, re.M))
unexpected_ignores = sorted(ignored_tests - EXPECTED_IGNORED)
stale_ignores = sorted(EXPECTED_IGNORED - ignored_tests)
problems = []
if int(os.environ["BUILD_RC"]) != 0:
    problems.append(f"build: release binary or Python extension failed (rc {os.environ['BUILD_RC']}), see {os.environ['BUILD_LOG']}")
if int(os.environ["RUST_RC"]) != 0 or r["failed"]:
    problems.append(f"rust: {r['failed']} failed (rc {os.environ['RUST_RC']})")
if timing["rc"] != 0 or timing["failed"]:
    problems.append(f"timing: {len(timing['failed'])} failed (rc {timing['rc']}, load {timing['load_at_start']}): {timing['failed']}")
if timing["not_run"] and timing["rc"] == 0:
    problems.append(f"timing: expected tests did not run (renamed?): {timing['not_run']}")
if bare:
    problems.append(f"rust: {len(bare)} #[ignore] without a reason")
if unexpected_ignores:
    problems.append(f"rust: ignored tests not on the expected list: {unexpected_ignores}")
if stale_ignores and r["passed"] + r["failed"] == 0:
    stale_ignores = []  # nothing compiled/ran; the rc problem above says why
    problems.append("rust: no test results parsed (build failed?), see test-gate-rust.log")
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
# Doctor on this repo (exit criterion 4, 5).
DOCTOR_BUDGET_MS = 300
BRIEF_BUDGET_BYTES = 2048
doctor = {"rc": int(os.environ["DOCTOR_RC"]), "budget_ms": DOCTOR_BUDGET_MS}
try:
    d = json.load(open(os.environ["DOCTOR_JSON"]))
    doctor.update(headline=d.get("headline"), tier=d.get("tier"), clean=d.get("clean"),
                  elapsed_ms=d.get("elapsed_ms"),
                  findings=[f"{f['check']}/{f['severity']}: {f['message']}" for f in d.get("findings", [])])
except (ValueError, OSError):
    doctor["error"] = open(os.environ["DOCTOR_JSON"]).read().strip()[:300]
doctor["wall_ms_median5"] = int(os.environ["DOCTOR_MS"]) if os.environ["DOCTOR_MS"].isdigit() else None
# Release (GATE_RELEASE=1): exit 0 required. Mid-sprint the shared tree
# carries other agents' in-flight work, so yellow is reported, red fails.
doctor["release_mode"] = os.environ.get("GATE_RELEASE") == "1"
reds = [f for f in doctor.get("findings", []) if "/red:" in f]
if doctor.get("error") or (doctor["rc"] != 0 and (doctor["release_mode"] or reds)):
    problems.append(f"doctor: exit {doctor['rc']} on this repo: {doctor.get('findings') or doctor.get('error')}")
if doctor["wall_ms_median5"] is None or doctor["wall_ms_median5"] > DOCTOR_BUDGET_MS:
    problems.append(f"doctor: {doctor['wall_ms_median5']} ms median wall time, budget {DOCTOR_BUDGET_MS} ms")
if doctor.get("tier") != "fast" or "survival check not available until 0.4.1" not in (doctor.get("headline") or ""):
    problems.append(f"doctor: honesty headline missing tier or survival note: {doctor.get('headline')!r}")
safe_hits = [h for h in os.environ["SAFE_HITS"].splitlines() if h.strip()]
if safe_hits:
    problems.append(f"honesty: 'safe to finish' present in shipped artifacts: {safe_hits}")
brief = open(os.environ["BRIEF_OUT"], "rb").read()
brief_line = next((l for l in brief.decode("utf-8", "replace").splitlines()
                   if l.strip().startswith("doctor:")), None)
doctor["brief_bytes"] = len(brief)
doctor["brief_line"] = brief_line
if len(brief) >= BRIEF_BUDGET_BYTES:
    problems.append(f"brief: {len(brief)} bytes, budget {BRIEF_BUDGET_BYTES}")
pending = []
if brief_line is None:
    if "brief_doctor_line" in PENDING_GATE_CHECKS:
        pending.append("brief_doctor_line")
    else:
        problems.append("brief: no doctor: line in context --format brief")
elif "brief_doctor_line" in PENDING_GATE_CHECKS:
    problems.append("gate: brief_doctor_line passes now, remove it from PENDING_GATE_CHECKS")
doctor["pending"] = pending
result_failures = re.findall(r"^test (\S+) \.\.\. FAILED", rust, re.M)
# Finding 27 watch list: tests seen failing once under full-suite load and
# passing alone. Still gate failures; labelled so a recurrence is recognised.
# The test_scale_* group asserts wall-clock bounds (repo.rs ~21672-21860) and a
# different one failed per run under load during S.9 (Haris, 2026-10-05).
FLAKY_WATCH = {
    "test_sdk::test_agent_context_manager",  # 27, python
    # The Rust wall-clock group moved to the serial timing step (TIMING_TESTS).
}
result = {
    "captured_at": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
    "build_rc": int(os.environ["BUILD_RC"]),
    "rust": r, "python": p, "timing": timing, "verify": verify, "doctor": doctor, "bare_ignores": bare,
    "rust_failures": result_failures,
    "flaky_watch_hits": sorted(
        t for t in FLAKY_WATCH
        if t in result_failures or re.search(rf"^(?:FAILED|ERROR) \S*{re.escape(t.split('::')[-1])}\b", py, re.M)
    ),
    "rust_ignored": sorted(ignored_tests),
    "python_xfails": re.findall(r"^XFAIL (\S+)", py, re.M),
    "python_failures": re.findall(r"^(?:FAILED|ERROR) (\S+)", py, re.M),
    "passed": not problems, "problems": problems,
}
json.dump(result, open(os.environ["OUT"], "w"), indent=2)
print(json.dumps(result, indent=2))
PY
python3 -c "import json,sys; sys.exit(0 if json.load(open('$OUT'))['passed'] else 1)"
