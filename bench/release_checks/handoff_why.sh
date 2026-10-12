#!/usr/bin/env bash
# Release check (0.4.2, U.3 handoff and why, finding 122) on Bri's
# handoff_states.rs fixture: three specs (sa alpha, sb bravo, sc charlie), two
# shared files, one override after being told (alpha takes bravo's
# shared.txt), all done and finished.
#   handoff: names exactly the finish audit's specs and multi-spec files
#     (bench/finish_audit.py on the finish commit), each spec with title,
#     seals and a committed state; resolutions accepted_after_told (with the
#     finish wording) and kept; every agent; no unsealed work; markdown and
#     brief (<= 2 KB) name the same; next_action equals doctor's before and
#     after the finish and is never converge; no write to objects, seals or
#     specs by handoff or why.
#   why: shared.txt history sa, sb, sa with what each added and removed, the
#     override (bravo, the line, after being told), the merge naming the
#     finish commit and both specs; two.txt sb, sc, no override, merge kept.
#   122: mixed override worded "accepted bravo's version of shared.txt for 1
#     line(s) ... removed 1 own line(s)" in finish and handoff; an own rewrite
#     is rewrote_own and never "bravo accepted charlie's version".
#
# Usage: bench/release_checks/handoff_why.sh [WRIT_BIN] [SCRATCH_DIR]
# Exit 0 when every check passes, 1 on any FAIL, 2 on bad arguments.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
. "$ROOT/bench/release_checks/_lib.sh"
W="$(rc_bin "${1:-$ROOT/target/release/writ}")"
echo "binary: $W ($("$W" --version))"
rc_setup .handoff_why_scratch writ-handoff-why "${2:-}"
mkdir home
w() { local who="$1"; shift; env -i HOME="$D/home" PATH=/usr/bin:/bin:/usr/sbin:/sbin \
      WRIT_AGENT_ID="$who" WRIT_EXCLUDES_FILE="$D/home/none" GIT_CONFIG_NOSYSTEM=1 \
      GIT_AUTHOR_NAME=fixture GIT_AUTHOR_EMAIL=fixture@example.invalid \
      GIT_COMMITTER_NAME=fixture GIT_COMMITTER_EMAIL=fixture@example.invalid "$W" "$@" < /dev/null; }
numbered() { python3 -c 'import sys; n=int(sys.argv[1]); e=dict(a.split("=",1) for a in sys.argv[2:]); print("".join(f"line {i} {e[str(i)]}\n" if str(i) in e else f"line {i}\n" for i in range(n)), end="")' "$@"; }
snap() { find .writ/objects .writ/seals .writ/specs -type f -print0 2>/dev/null | sort -z | xargs -0 shasum | shasum; }
TOLD="alpha accepted bravo's version of shared.txt after being told"

# fixture <dir> <none|mixed|own>: Bri's fixture up to spec done (no finish).
fixture() {
    local mode="$2" a b
    rc_git_project "$1"
    git config user.name fixture; git config user.email fixture@example.invalid
    printf '# fixture\n' > README.md; printf 'target/\n' > .gitignore
    numbered 40 > shared.txt; numbered 20 > two.txt
    git add -A; git -c user.name=t -c user.email=t@localhost commit -qm fixture
    w human init -y > /dev/null 2>&1; git add -A; git -c user.name=t -c user.email=t@localhost commit -qm "writ init" --no-verify
    SINCE="$(w human log --all --format json --limit 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["id"])')"
    for s in sa:alpha sb:bravo sc:charlie; do
        w "${s#*:}" spec add --id "${s%%:*}" --title "spec ${s%%:*}" > /dev/null
        w "${s#*:}" spec claim "${s%%:*}" --agent "${s#*:}" > /dev/null
    done
    if [ "$mode" = mixed ]; then a="2=A 20=A"; b="20=A 30=B"; else a="2=A"; b="30=B"; fi
    numbered 40 $a > shared.txt; w alpha seal -s work --agent alpha --spec sa --paths shared.txt > /dev/null 2>&1
    numbered 40 $b > shared.txt; w bravo seal -s work --agent bravo --spec sb --paths shared.txt > /dev/null 2>&1
    numbered 20 5=B > two.txt; w bravo seal -s work --agent bravo --spec sb --paths two.txt > /dev/null 2>&1
    numbered 20 5=B 15=C > two.txt; printf 'c\n' > c.txt
    w charlie seal -s work --agent charlie --spec sc --paths two.txt,c.txt > /dev/null 2>&1
    numbered 40 10=A2 30=B > shared.txt
    w alpha seal -s work --agent alpha --spec sa --paths shared.txt > "$D/$mode.told" 2>&1 && return 1
    w alpha seal -s work --agent alpha --spec sa --paths shared.txt --allow-removals shared.txt > /dev/null 2>&1
    if [ "$mode" = own ]; then
        numbered 20 7=B2 15=C > two.txt
        w bravo seal -s work --agent bravo --spec sb --paths two.txt > /dev/null 2>&1 && return 1
        w bravo seal -s work --agent bravo --spec sb --paths two.txt --allow-removals two.txt > /dev/null 2>&1
    fi
    for s in sa:alpha sb:bravo sc:charlie; do w "${s#*:}" spec done "${s%%:*}" --agent "${s#*:}" --no-seal > /dev/null 2>&1; done
}
# next_equal <label>: handoff's next_action equals doctor's and is not converge.
next_equal() {
    w human handoff --since "$SINCE" --format json > "$D/$1.h.json" 2>/dev/null || true
    w human doctor --format json > "$D/$1.d.json" 2>/dev/null || true
    python3 - "$D/$1.h.json" "$D/$1.d.json" <<'PY'
import json, sys
h = json.load(open(sys.argv[1])); d = json.load(open(sys.argv[2]))
hn, dn = h.get("next_action"), d.get("next_action")
sys.exit(0 if isinstance(hn, str) and hn == dn and "converge" not in hn.lower() else 1)
PY
}

# --- main fixture -----------------------------------------------------------
fixture "$D/main" none || { echo "FAIL fixture: the told-refusal did not happen"; exit 1; }
chk "fixture: alpha's overwrite was refused naming bravo before the override" "grep -q bravo $D/none.told"
chk "next_action: handoff equals doctor with specs waiting for finish, never converge" "next_equal waiting"
w human finish -y > "$D/main.finish" 2>&1 || true
chk "fixture: finish words the override as '$TOLD'" "grep -qF \"$TOLD\" $D/main.finish"
WRIT_AUDIT_REPO="$D/main" python3 "$ROOT/bench/finish_audit.py" "$(git rev-parse HEAD)" > "$D/audit.txt" 2>&1 || true
python3 - "$D/audit.txt" > "$D/audit.json" <<'PY' || echo '{}' > "$D/audit.json"
import json, re, sys
t = open(sys.argv[1]).read()
lst = lambda key: sorted(x.strip().strip("'") for x in re.search(re.escape(key) + r".*?\[(.*?)\]", t).group(1).split(",") if x.strip())
print(json.dumps({"specs": lst("specs="), "multi": lst("sealed by >1 spec:"), "lost": int(re.search(r"LOST: (\d+)", t).group(1))}))
PY
chk "finish audit: specs sa, sb, sc; shared.txt and two.txt sealed by more than one spec; 0 lost" \
  "rc_json $D/audit.json 'r=={\"specs\":[\"sa\",\"sb\",\"sc\"],\"multi\":[\"shared.txt\",\"two.txt\"],\"lost\":0}'"
before="$(snap)"
w human handoff --since "$SINCE" --format json > "$D/h.json" 2>/dev/null || true
w human handoff --since "$SINCE" > "$D/h.md" 2>&1 || true
w human handoff --since "$SINCE" --format brief > "$D/h.brief" 2>/dev/null || true
w human why shared.txt --format json > "$D/why.shared.json" 2>/dev/null || true
w human why shared.txt > "$D/why.shared.md" 2>&1 || true
w human why two.txt --format json > "$D/why.two.json" 2>/dev/null || true
chk "handoff and why (json, markdown, brief) write nothing to objects, seals or specs" "[ \"$before\" = \"\$(snap)\" ]"
python3 - "$D/h.json" "$D/audit.json" > "$D/h.check" <<'PY' || true
import json, sys
h = json.load(open(sys.argv[1])); a = json.load(open(sys.argv[2]))
ok = lambda name, cond: print(("PASS " if cond else "FAIL ") + name)
specs = h.get("specs", [])
ok("handoff json names exactly the audit's specs", sorted(s["id"] for s in specs) == a["specs"])
ok("each spec has a title, at least one seal and a committed state",
   all(s.get("title") and (s.get("seals") or 0) >= 1 and "commit" in (s.get("state") or "") for s in specs))
merges = {m["path"]: m for m in h.get("merges", [])}
ok("handoff json names exactly the audit's merges", sorted(merges) == a["multi"])
sh, tw = merges.get("shared.txt", {}), merges.get("two.txt", {})
ok("shared.txt resolved accepted_after_told, worded as finish words it",
   sh.get("resolution") == "accepted_after_told" and "alpha accepted bravo's version of shared.txt after being told" in (sh.get("detail") or ""))
ok("two.txt resolved kept; both merges name sb", tw.get("resolution") == "kept" and all("sb" in json.dumps(m.get("specs")) for m in (sh, tw)))
ag = json.dumps(h.get("agents"))
ok("every agent named, no unsealed work, doctor headline present",
   all(x in ag for x in ("alpha", "bravo", "charlie")) and h.get("unsealed") == [] and isinstance(h.get("doctor", {}).get("headline"), str))
PY
while read -r verdict name; do chk "$name" "[ $verdict = PASS ]"; done < "$D/h.check"
chk "markdown handoff names the specs, files, agents and the override wording" \
  "( for x in sa sb sc shared.txt two.txt alpha bravo charlie; do grep -q \"\$x\" $D/h.md || exit 1; done ) && grep -qF \"$TOLD\" $D/h.md"
chk "brief handoff is at most 2 KB and names sa, sb, sc" \
  "[ \$(wc -c < $D/h.brief) -le 2048 ] && grep -q sa $D/h.brief && grep -q sb $D/h.brief && grep -q sc $D/h.brief"
chk "next_action: handoff equals doctor after the finish, never converge" "next_equal clean"
head7="$(git rev-parse HEAD | cut -c1-7)"
python3 - "$D/why.shared.json" "$D/why.two.json" "$head7" > "$D/why.check" <<'PY' || true
import json, sys
s = json.load(open(sys.argv[1])); t = json.load(open(sys.argv[2])); head = sys.argv[3]
ok = lambda name, cond: print(("PASS " if cond else "FAIL ") + name)
has = lambda e, k, line: any(line in x for x in (e.get(k) or []))
h = [e for e in s.get("history", []) if isinstance(e.get("spec"), str)]
ok("why shared.txt: history sa, sb, sa", [e["spec"] for e in h] == ["sa", "sb", "sa"])
ok("why shared.txt: what each added and removed (2 A; 30 B; -2 A +10 A2), agents alpha and bravo",
   len(h) == 3 and has(h[0], "added", "line 2 A") and h[0].get("agent") == "alpha" and has(h[1], "added", "line 30 B")
   and h[1].get("agent") == "bravo" and has(h[2], "removed", "line 2 A") and has(h[2], "added", "line 10 A2"))
ov = json.dumps(h[2].get("override")) if len(h) == 3 else ""
ok("why shared.txt: only the last seal has an override, naming bravo, the line and after being told",
   len(h) == 3 and all(not e.get("override") for e in h[:2]) and all(x in ov for x in ("bravo", "line 2 A", "after being told")))
m = json.dumps(s.get("merge"))
ok("why shared.txt: the merge names the finish commit and both specs", head in m and "sa" in m and "sb" in m)
th = [e for e in t.get("history", []) if isinstance(e.get("spec"), str)]
ok("why two.txt: history sb, sc, no override, merge kept",
   [e["spec"] for e in th] == ["sb", "sc"] and not any(e.get("override") for e in t.get("history", [])) and "kept" in json.dumps(t.get("merge")))
PY
while read -r verdict name; do chk "$name" "[ $verdict = PASS ]"; done < "$D/why.check"
chk "markdown why shared.txt names the specs, lines, override and commit" \
  "( for x in sa sb 'line 2 A' 'line 30 B' 'after being told' $head7; do grep -qF \"\$x\" $D/why.shared.md || exit 1; done )"

# --- 122 mixed ---------------------------------------------------------------
fixture "$D/mixed" mixed || { echo "FAIL fixture (mixed): no told-refusal"; fail=1; }
w human finish -y > "$D/mixed.finish" 2>&1 || true
w human handoff --since "$SINCE" --format json > "$D/mixed.h.json" 2>/dev/null || true
chk "122 mixed: finish says alpha accepted bravo's version of shared.txt for 1 line and removed 1 own line" \
  "grep -qF \"alpha accepted bravo's version of shared.txt for 1 line\" $D/mixed.finish && grep -qF 'removed 1 own line' $D/mixed.finish"
chk "122 mixed: handoff's shared.txt detail says both, with counts" \
  "rc_json $D/mixed.h.json 'any(m[\"path\"]==\"shared.txt\" and \"for 1 line\" in m[\"detail\"] and \"removed 1 own line\" in m[\"detail\"] for m in r[\"merges\"])'"

# --- 122 own rewrite -----------------------------------------------------------
fixture "$D/own" own || { echo "FAIL fixture (own): no told-refusal"; fail=1; }
w human finish -y > "$D/own.finish" 2>&1 || true
w human handoff --since "$SINCE" --format json > "$D/own.h.json" 2>/dev/null || true
w human handoff --since "$SINCE" > "$D/own.h.md" 2>&1 || true
w human why two.txt > "$D/own.why.md" 2>&1 || true
chk "122 own rewrite: two.txt is rewrote_own, shared.txt still accepted_after_told" \
  "rc_json $D/own.h.json '{m[\"path\"]: m[\"resolution\"] for m in r[\"merges\"]}.get(\"two.txt\")==\"rewrote_own\" and {m[\"path\"]: m[\"resolution\"] for m in r[\"merges\"]}.get(\"shared.txt\")==\"accepted_after_told\"'"
chk "122 own rewrite: no surface says bravo accepted charlie's version (finish, handoff, why)" \
  "! grep -qF \"bravo accepted charlie's version\" $D/own.finish $D/own.h.md $D/own.why.md $D/own.h.json"
exit $fail
