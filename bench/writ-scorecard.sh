#!/usr/bin/env bash
# writ scorecard v0: analytics + gc audit + latest context bench as one markdown report.
#
# Usage: bench/writ-scorecard.sh [--hurdles] [REPO_DIR] [WRIT_BIN]
#   REPO_DIR defaults to the repo containing this script; WRIT_BIN defaults to `writ`.
#   --hurdles prints only the hurdle ratio section (fast; no analytics or bench).
#   Bench section uses bench/context/results/latest.md, else the newest baseline-*.md.
set -euo pipefail

HURDLES_ONLY=0
if [[ "${1:-}" == "--hurdles" ]]; then HURDLES_ONLY=1; shift; fi
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO="${1:-$ROOT}"
WRIT="${2:-writ}"
BENCH_DIR="$ROOT/bench/context"

# --- Hurdle ratio (sprint 3, doctor-harness) --------------------------------
# Forced seals, survival allowances, refusals, hand recoveries and stale-rewrite
# notices per sprint and agent. Sources, in order of trust:
#   1. structured fields on seal records (`forced`, `allow_removals`) and
#      refusal events in .writ/security/events.jsonl, when writ records them
#      (0.3.0 records neither: a refused seal writes nothing, a forced seal
#      looks like any other);
#   2. the seal log (`writ log --all --format json`): summaries that say the
#      seal itself used the flag, "(..., --force)" or "--allow-removals:";
#      specs titled "... (follow-up)" (doctor's stuck-file fix) as recoveries;
#   3. .writ/stale_rewrite_notices.json (open notices) and saved finish output
#      (WRIT_FINISH_LOGS, a glob) for notices and finish refusals;
#   4. a self-reported ledger (WRIT_HURDLE_LEDGER, JSONL: sprint, agent, spec,
#      kind in forced|allow_removals|refusal|hand_recovery|notice, seal, note),
#      for what the store cannot show. Deduplicated by (kind, seal).
# Sprints are assigned by the seal's spec created_at against WRIT_SPRINTS.
hurdles_section() {
    local log_json
    log_json="$(cd "$REPO" && "$WRIT" log --all --format json 2>/dev/null)" || { echo "scorecard: writ log failed" >&2; return 2; }
    LOG_JSON="$log_json" REPO_DIR="$REPO" python3 - <<'PY'
import collections, glob, json, os, re
from pathlib import Path

repo = Path(os.environ["REPO_DIR"]); writ = repo / ".writ"
seals = json.loads(os.environ["LOG_JSON"])
sprints = sorted(
    (start, name) for name, start in (
        kv.split("=", 1) for kv in os.environ.get(
            "WRIT_SPRINTS",
            "sprint-1=2026-10-04T00:00:00Z,sprint-2=2026-10-05T03:00:00Z,sprint-3=2026-10-05T21:00:00Z",
        ).split(",") if kv))

def sprint_of(ts):
    name = "pre"
    for start, n in sprints:
        if ts.replace("+00:00", "Z") >= start:
            name = n
    return name

specs = {}
for f in glob.glob(str(writ / "specs" / "*.json")):
    try:
        d = json.load(open(f)); specs[d["id"]] = d
    except (OSError, ValueError, KeyError):
        continue

# A summary declares the seal's own flag as "(..., --force)", "(--force: why)"
# or "--force: why" at a sentence start. Quoted text ('...', `...`, "...") is
# dropped first so a summary that describes the convention is not counted.
FORCED = re.compile(r"(?:\(|, )--force(?:\)|:)|(?:^|[.;] )--force:|\(forced\)")
ALLOWED = re.compile(r"(?:\(|, )--allow-removals(?:\)|:)|(?:^|[.;] )--allow-removals:")
QUOTED = re.compile(r"'[^']*'|`[^`]*`|\"[^\"]*\"")
events = []          # (sprint, agent, kind, seal, source)
seal_count = collections.Counter()
seal_index = {}
for s in seals:
    spec = specs.get(s.get("spec_id") or "", {})
    sp = sprint_of(spec.get("created_at") or s["timestamp"])
    agent = s["agent"]["id"]; sid = s["id"][:12]
    seal_count[(sp, agent)] += 1
    seal_index[sid] = (sp, agent)
    summ = QUOTED.sub("", s.get("summary") or "")
    if s.get("forced") or s.get("force") or FORCED.search(summ):
        events.append((sp, agent, "forced", sid, "record" if s.get("forced") or s.get("force") else "log"))
    if s.get("allow_removals") or ALLOWED.search(summ):
        events.append((sp, agent, "allow_removals", sid, "record" if s.get("allow_removals") else "log"))
for sid_full, spec in specs.items():
    if (spec.get("title") or "").rstrip().endswith("(follow-up)"):
        sp = sprint_of(spec.get("created_at") or "")
        events.append((sp, spec.get("claimed_by") or spec.get("created_by") or "?", "recovery_follow_up", sid_full, "spec"))

ev_path = writ / "security" / "events.jsonl"
if ev_path.exists():
    for line in ev_path.read_text().splitlines():
        try:
            e = json.loads(line)
        except ValueError:
            continue
        if "refus" in (e.get("event_type") or ""):
            events.append((sprint_of(e.get("timestamp", "")), e.get("agent_id", "?"), "refusal", None, "events"))

notices_path = writ / "stale_rewrite_notices.json"
open_notices = 0
if notices_path.exists():
    try:
        n = json.load(open(notices_path))
        open_notices = len(n if isinstance(n, list) else n.get("notices", []))
    except ValueError:
        pass
for f in sorted(glob.glob(os.environ.get("WRIT_FINISH_LOGS", ""))) if os.environ.get("WRIT_FINISH_LOGS") else []:
    text = Path(f).read_text(errors="replace")
    ts = re.search(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d", text)
    sp = sprint_of(ts[0] + "Z") if ts else "?"
    events += [(sp, "finish", "notice", None, f"finish:{Path(f).name}")
               for _ in re.findall(r"(?im)^.*stale.rewrite.*$", text)]
    events += [(sp, "finish", "refusal", None, f"finish:{Path(f).name}")
               for _ in re.findall(r"(?im)^.*\bfinish refused\b.*$", text)]

ledger = os.environ.get("WRIT_HURDLE_LEDGER")
seen = {(k, s) for _, _, k, s, _ in events if s}
ledger_rows = 0
if ledger and Path(ledger).exists():
    for line in Path(ledger).read_text().splitlines():
        if not line.strip():
            continue
        e = json.loads(line); ledger_rows += 1
        key = (e["kind"], (e.get("seal") or "")[:12] or None)
        if key[1] and key in seen:
            continue
        seen.add(key)
        events.append((e["sprint"], e["agent"], e["kind"], key[1], "ledger"))

KINDS = [("forced", "forced"), ("allow_removals", "allowed"), ("refusal", "refusals"),
         ("hand_recovery", "hand rec."), ("recovery_follow_up", "follow-up rec."), ("notice", "notices")]
by = collections.Counter((sp, ag, k) for sp, ag, k, _, _ in events)
src = collections.Counter(s.split(":")[0] for *_, s in events)
print("## Hurdle ratio")
print()
print("Forced seals, survival allowances, refusals, hand recoveries, stale notices per sprint "
      "(spec created_at). Target: sprint 3 halves sprint 2 on the same agent (baseline: bri 6/3/1/0).")
print(f"Sources: {dict(src) or 'none'}; ledger rows {ledger_rows} "
      f"({ledger or 'unset: WRIT_HURDLE_LEDGER'}); open stale notices in store: {open_notices}.")
print("writ 0.3.0 records neither forced seals nor refusals; log counts are summary markers only.")
print()
print("| sprint | agent | seals | " + " | ".join(h for _, h in KINDS) + " | total | per 10 seals |")
print("|---|---|---:|" + "---:|" * (len(KINDS) + 2))
rows = sorted({(sp, ag) for sp, ag, _ in by} | {k for k in seal_count})
totals = collections.Counter()
for sp, ag in rows:
    vals = [by[(sp, ag, k)] for k, _ in KINDS]
    t = sum(vals); n = seal_count[(sp, ag)]
    totals[sp] += t
    if t == 0:
        continue
    rate = f"{10 * t / n:.1f}" if n else "n/a"
    print(f"| {sp} | {ag} | {n} | " + " | ".join(map(str, vals)) + f" | {t} | {rate} |")
print()
for sp in sorted({sp for sp, _ in seal_count} | set(totals)):
    n = sum(v for (s, _), v in seal_count.items() if s == sp)
    print(f"- {sp}: {totals[sp]} hurdles over {n} seals")
bri2 = sum(by[("sprint-2", "bri", k)] for k, _ in KINDS)
bri3 = sum(by[("sprint-3", "bri", k)] for k, _ in KINDS)
n3 = sum(v for (s, a), v in seal_count.items() if s == "sprint-3" and a == "bri")
ratio = f"{bri3 / bri2:.2f}x; target <= 0.50x" if bri2 and n3 else "no sprint-3 seals yet"
print(f"- bri sprint-3 vs sprint-2: {bri3} vs {bri2} ({ratio})")
print()
PY
}

if [[ $HURDLES_ONLY -eq 1 ]]; then
    hurdles_section
    exit $?
fi

analytics="$(cd "$REPO" && "$WRIT" analytics --format json)" || { echo "scorecard: writ analytics failed" >&2; exit 2; }
gc_audit="$(cd "$REPO" && "$WRIT" gc audit --format json)" || { echo "scorecard: writ gc audit failed" >&2; exit 2; }

bench_md="$BENCH_DIR/results/latest.md"
if [[ ! -f "$bench_md" ]]; then
    bench_md="$(ls -1 "$BENCH_DIR"/baseline-*.md 2>/dev/null | sort | tail -n 1 || true)"
fi

echo "# writ scorecard"
echo
echo "- Repo: \`$REPO\`"
echo "- Binary: \`$(command -v "$WRIT" || echo "$WRIT")\` ($("$WRIT" --version))"
echo "- Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo

ANALYTICS="$analytics" GC_AUDIT="$gc_audit" python3 - <<'PY'
import json, os

a = json.loads(os.environ["ANALYTICS"])
g = json.loads(os.environ["GC_AUDIT"])

print("## Agents")
print()
print("| agent | seals | specs | files/seal | warnings | conv. triggered | conv. ok |")
print("|---|---:|---:|---:|---:|---:|---:|")
for ag in a.get("agents", []):
    print(f"| {ag['agent_id']} | {ag['seal_count']} | {ag['spec_count']} "
          f"| {ag['avg_files_per_seal']:.1f} | {ag['warning_count']} "
          f"| {ag['convergence_triggered']} | {ag['convergence_succeeded']} |")
print()
print(f"Totals: {a.get('total_seals', 0)} seals, {a.get('total_specs', 0)} specs, "
      f"{a.get('total_agents', 0)} agents.")
print()

def mb(n): return f"{n / 1_000_000:.1f} MB"
store = g["total_other_bytes"] + g["total_seal_bytes"]
orph = g["orphaned_bytes"]
print("## Storage (gc audit)")
print()
print("| metric | value |")
print("|---|---:|")
print(f"| store (objects + seals) | {mb(store)} |")
print(f"| orphaned | {g['orphaned_objects']:,} objects, {mb(orph)} "
      f"({100 * orph / store if store else 0:.0f}%) |")
print(f"| committed prunable | {g['committed_prunable_objects']:,} objects, "
      f"{mb(g['committed_prunable_bytes'])} |")
print(f"| seals active / committed | {g['active_seals']} / {g['committed_seals']} |")
print(f"| specs active / committed | {g['active_specs']} / {g['committed_specs']} |")
print()
PY

hurdles_section
echo "## Store integrity"
echo
echo "Referenced-but-missing objects (every seal tree and spec genesis tree walked, every change hash, every workspace index). Must be 0."
echo
# Finding 28: three junk blobs (two .venv311 dist-info files and an
# import-time .so) were pruned from the writ repo and are permanently lost;
# nothing depends on them. Excused for THIS repo only; zero everywhere else.
allow_args=()
if [[ "$(cd "$REPO" && pwd -P)" == "$(cd "$ROOT" && pwd -P)" ]]; then
    allow_args=(--allow-missing ae01d21c123e --allow-missing 50ff4a66d3af --allow-missing 80dbd0cf0eac)
fi
if integrity="$(python3 "$ROOT/bench/store_integrity.py" "$REPO" --json "${allow_args[@]}" 2>&1)"; then
    status="PASS"
else
    rc=$?; status="$([[ $rc -eq 1 ]] && echo FAIL || echo ERROR)"
fi
INTEGRITY="$integrity" STATUS="$status" python3 - <<'PY'
import json, os
status = os.environ["STATUS"]
try:
    r = json.loads(os.environ["INTEGRITY"])
    print(f"**{status}**: {r['missing_objects']} missing of {r['referenced_objects']} referenced "
          f"({r['seals']} seals, {r.get('genesis_trees', 0)} genesis trees, {r['index_references']} index entries).")
    if r["missing_sample"]:
        print(f"Sample: {', '.join(r['missing_sample'])}")
    if r.get("allowed_missing"):
        print(f"Excused (finding 28, permanently lost, this repo only): {', '.join(r['allowed_missing'])}")
    if r.get("unused_allowances"):
        print(f"Unused allowances (remove them): {', '.join(r['unused_allowances'])}")
except json.JSONDecodeError:
    print(f"**{status}**: {os.environ['INTEGRITY'].strip()}")
PY
echo

echo "## Verify"
echo
echo "\`writ verify --all-chains\` exits 1 on any failure; the exit code is captured, not fatal. EXPECTED means the only failures are the allow-listed missing objects above."
echo
verify_rc=0
verify="$(python3 "$ROOT/bench/verify_check.py" "$REPO" --writ "$WRIT" "${allow_args[@]}" 2>&1)" || verify_rc=$?
VERIFY="$verify" VERIFY_RC="$verify_rc" python3 - <<'PY'
import json, os
try:
    v = json.loads(os.environ["VERIFY"])
    print(f"**{v['verdict']}** (verify exit {v['verify_rc']}): {v['missing_objects']} missing, "
          f"{len(v['excused'])} excused.")
    for prob in v["problems"]:
        print(f"- FAIL {prob}")
except json.JSONDecodeError:
    print(f"**ERROR** (rc {os.environ['VERIFY_RC']}): {os.environ['VERIFY'].strip()}")
PY
echo

echo "## Context cost"
echo
if [[ -n "$bench_md" && -f "$bench_md" ]]; then
    echo "Source: \`${bench_md#$ROOT/}\`"
    echo
    # Demote the bench report's headings one level so they nest under this section.
    sed -e 's/^## /#### /' -e 's/^# /### /' "$bench_md"
else
    echo "No bench results. Run \`bench/bench-context.sh <writ-bin>\`."
fi
echo

echo "## Test gate"
echo
gate="$BENCH_DIR/results/test-gate.json"
if [[ -f "$gate" ]]; then
    GATE="$gate" python3 - <<'PY'
import json, os
g = json.load(open(os.environ["GATE"]))
r, p = g["rust"], g["python"]
print(f"Gate: **{'PASS' if g['passed'] else 'FAIL'}** ({g['captured_at']}). "
      "Rule: 0 failures in both languages, Rust ignores carry a reason, Python xfails strict (0 XPASS).")
print()
print("| suite | passed | failed | expected-fail | skipped |")
print("|---|---:|---:|---:|---:|")
print(f"| Rust | {r['passed']} | {r['failed']} | {r['ignored']} (ignored) | 0 |")
print(f"| Python | {p.get('passed', 0)} | {p.get('failed', 0) + p.get('errors', 0)} "
      f"| {p.get('xfailed', 0)} (xfail) | {p.get('skipped', 0)} |")
for prob in g["problems"]:
    print(f"- FAIL {prob}")
PY
else
    echo "No gate results. Run \`bench/test-gate.sh\`."
fi
echo

cat <<'MD'
## Known issues (sprint 1 dogfood findings)

1. `.gitignore` is imported into `.writignore` once at init; later rules never reach writ.
2. `.writignore` cannot express path globs such as `.venv*/`.
3. `pending_changes` / `working_state` / `file_scope` are uncapped and not scoped by `--spec` or `--for-agent`.
4. `--format brief` carries no specs, seals, or risk.
5. `writ init` hooks inject full context on every prompt (`UserPromptSubmit`).
6. With zero pending changes, `file_scope` is about 90% of context.
7. Init replaces the managed CLAUDE.md block and drops operational guidance.
8. `writ spec add` auto-claims for the creator, and claims cannot be released.
9. Two identity resolvers disagree; subagents collide on the hub's id unless `--agent` is explicit.
10. `writ status` shows agent unknown for a claimed spec while `writ context` shows the claimer; `writ spec show --format json` does not emit JSON.
11. `writ context` reports `writ_version: "0.1.0"` from the 0.2.0 binary.
12. `writ gc audit` on the writ repo reported about 91% of the object store as orphaned right after reinit. **Corrected:** those objects were live (see 28); the audit's orphan count was wrong, not the store.
13. A seal without `--paths` captures other agents' pending files; scope enforcement warns or rejects, never filters. `writ spec done` has no `--paths` and sweeps the tree.
    Canonical repro (writ repo, 2026-10-04): seal 22d02ab3aabb (amis, ctx-ignore, `spec done`, 23:16:31) captured `bench/bench-context.sh` and five files under `bench/context/`;
    seal b2230a6a12a3 (bri, bench-context, `--paths` with all seven files) then captured only `bench/writ-scorecard.sh`, with no hint or `file_scope_warning`.
14. `writ seal` has no `--format json`, so `hints` and `file_scope_warning` are not machine-readable.
15. `--for-agent` does not scope pending changes to the agent's spec files.
16. Nested `.gitignore` files (e.g. `sub/.gitignore`) are not honored; only the repo root `.gitignore` is read. Known gap, sprint 2. Pinned by `test_nested_gitignore_file_behavior_documented`.
17. A seal rejected by strict scope enforcement leaves its blobs and tree as orphans (26 objects for 25 files). Other rejections tested write nothing.
18. `writ seal --spec X` succeeds when X is claimed by another agent; only `writ spec claim` rejects.
19. ~~0.2.0 `writ init` stores gitignored files the baseline seal never references.~~ **Retracted:** those blobs are referenced by seal trees and the workspace index; the orphan scan does not look there (28).
28. `writ gc run` prunes live data. `find_orphaned_objects` misses two live roots: it never walks seal trees (carried-forward blobs) and never reads workspace indexes (bridge-imported untracked files, `.writignore`). Pruned 1,297 objects on the writ repo; `writ context` then fails with object not found while `writ verify --all-chains` stays valid. Metric: Store integrity above.
20. Seals from `writ workspace create` dirs (`.writ/ws/<name>/`) capture nothing; 16 tests passed vacuously by sweeping init files until 880af38, 2 more by re-sealing main's copy. 18 strict xfails until sprint 2 `workspace-seal` (retire in favor of `writ task`).
MD
