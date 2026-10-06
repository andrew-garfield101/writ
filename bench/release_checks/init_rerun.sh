#!/usr/bin/env bash
# Release check: re-running `writ init -y` in an existing project refreshes the
# generated files (managed CLAUDE.md and AGENTS.md blocks, SessionStart hook,
# settings instructions, skills, slash commands, AGENT_INSTRUCTIONS.md), keeps
# the user's own text and settings, and leaves the store and specs alone.
# Backs the 0.3.0 release note claim; caught findings 71 and 72.
#
# Usage: bench/release_checks/init_rerun.sh [WRIT_BIN] [SCRATCH_DIR]
#   WRIT_BIN     writ binary under test (default: <repo>/target/release/writ)
#   SCRATCH_DIR  throwaway project dir (default: a fresh mktemp dir). An existing
#                dir is reused only if empty or left by a previous run of this
#                script, so a typo cannot wipe real work.
# Exit 0 when every check passes, 1 on any FAIL, 2 on bad arguments.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MARK=".init_rerun_scratch"

W="${1:-$ROOT/target/release/writ}"
case "$W" in /*) ;; */*) W="$PWD/$W" ;; *) W="$(command -v "$W" || echo "$W")" ;; esac
[ -x "$W" ] || { echo "writ binary not found or not executable: $W" >&2; exit 2; }
echo "binary: $W ($("$W" --version))"

if [ $# -ge 2 ]; then
    D="$2"
    if [ -d "$D" ] && [ -n "$(ls -A "$D")" ] && [ ! -f "$D/$MARK" ]; then
        echo "refusing to reuse non-empty $D (no $MARK marker); pass an empty or new dir" >&2
        exit 2
    fi
    rm -rf "$D"; mkdir -p "$D"
else
    tmp="${TMPDIR:-/tmp}"
    D="$(mktemp -d "${tmp%/}/writ-init-rerun.XXXXXX")"
fi
cd "$D"; touch "$MARK"
echo "scratch: $D"
unset WRIT_AGENT_ID
git init -q -b main
printf 'print("hi")\n' > app.py
git add -A && git -c user.name=t -c user.email=t@localhost commit -qm base
"$W" init -y > init1.log 2>&1
"$W" spec add --id s1 --title "spec one" --agent t1 --claim > /dev/null
echo 'x = 1' >> app.py; "$W" seal -s "first" --agent t1 --spec s1 --paths app.py > /dev/null
echo 'y = 2' >> app.py; "$W" seal -s "second" --agent t1 --spec s1 --paths app.py > /dev/null
echo "--- generated files after first init"
find .claude CLAUDE.md AGENTS.md .writ/AGENT_INSTRUCTIONS.md -type f 2>/dev/null | sed 's|/[^/]*$||' | sort | uniq -c

snap() {  # store, spec and index fingerprints
  { ls .writ/seals | sort; } > "$1.seals"
  (cd .writ/specs && for f in $(ls | sort); do echo "$f $(shasum -a 256 "$f" | cut -c1-16)"; done) > "$1.specs"
  find .writ/workspaces -name index.json -type f -exec shasum -a 256 {} \; | sort > "$1.index"
  shasum -a 256 .writ/bridge.json >> "$1.index"
  # finding 34: last_opened_at changes on every open; compare the rest
  grep -v '^last_opened_at' .writ/version.toml | shasum -a 256 | sed 's|-$|version.toml minus last_opened_at|' >> "$1.index"
  "$W" spec show s1 --format json > "$1.s1.json"
  "$W" log --format json > "$1.log.json" 2>/dev/null || "$W" log > "$1.log.json"
}
# Finding 82: seed user config the rerun must keep. A custom [doctor] section
# (a table writ does not write by default) and one non-default framework flag.
# codex = false is an opt-out: the rerun must leave AGENTS.md alone (CC, sprint 3).
python3 - <<'PY2'
import pathlib, re
p = pathlib.Path(".writ/config.toml")
s = p.read_text()
assert re.search(r"(?m)^codex = true$", s), "expected codex = true in [frameworks]"
s = re.sub(r"(?m)^codex = true$", "codex = false", s, count=1)
s += "\n[doctor]\nstale_claim_minutes = 77\nallow_missing = [\"deadbeefcafe0123\"]\n"
p.write_text(s)
PY2
cfg_val() {  # cfg_val <file> <table> <key>: the raw value of key in [table]
  awk -v t="[$2]" -v k="$3" '$0 == t { f = 1; next } /^\[/ { f = 0 } f && $1 == k { sub(/^[^=]*=[ \t]*/, ""); print; exit }' "$1"
}
cp .writ/config.toml config.before.toml
snap before

# Tamper with every generated file and add user content outside the markers.
python3 - <<'PY'
import pathlib, json
B = "<!-- BEGIN WRIT CONFIGURATION — managed by writ init -->"
E = "<!-- END WRIT CONFIGURATION -->"
for name in ("CLAUDE.md", "AGENTS.md"):
    p = pathlib.Path(name)
    if not p.exists():
        continue
    s = p.read_text()
    assert B in s and E in s, f"markers missing in {name}"
    i, j = s.index(B) + len(B), s.index(E)
    p.write_text("# My notes\nUSER-ABOVE-MARKERS\n\n" + s[:i] + "\nSTALE-MANAGED-TEXT\n" + s[j:] + "\nUSER-BELOW-MARKERS\n")
for sk in pathlib.Path(".claude/skills").rglob("SKILL.md"):
    sk.write_text("STALE-SKILL\n")
for c in pathlib.Path(".claude/commands").glob("*.md"):
    c.write_text("STALE-CMD\n")
ai = pathlib.Path(".writ/AGENT_INSTRUCTIONS.md")
if ai.exists():
    ai.write_text("STALE-AGENT-INSTRUCTIONS\n")
st = pathlib.Path(".claude/settings.json")
d = json.loads(st.read_text())
d["userKey"] = "USER-SETTING"
d["permissions"]["allow"].append("Bash(ls *)")
# Stale writ entries as an older or hand-edited writ would leave them: writ's
# markers kept (writ for version control / ## Writ VCS Active), wording changed.
d["instructions"] = ["MANDATORY: This project uses writ for version control. STALE-INSTRUCTIONS", "USER-INSTRUCTION"]
d["hooks"]["SessionStart"][0]["hooks"][0]["command"] = "echo '## Writ VCS Active' && echo STALE-HOOK"
d["hooks"]["SessionStart"].append({"hooks": [{"type": "command", "command": "echo USER-HOOK"}]})
st.write_text(json.dumps(d, indent=2))
PY
cp .claude/settings.json settings.tampered.json 2>/dev/null || true
cp AGENTS.md AGENTS.tampered.md  # codex = false is seeded above: the rerun must not touch it
"$W" init -y > init2.log 2>&1; echo "init2 rc=$?"
snap after

fail=0
chk() { if eval "$2"; then echo "PASS $1"; else echo "FAIL $1"; fail=1; fi; }
chk "seal list unchanged ($(wc -l < before.seals | tr -d ' ') seals)" "cmp -s before.seals after.seals"
chk "spec files unchanged" "cmp -s before.specs after.specs"
chk "index unchanged" "cmp -s before.index after.index"
chk "spec s1 show identical (claim, status, seals)" "cmp -s before.s1.json after.s1.json"
chk "log identical (no new baseline seal)" "cmp -s before.log.json after.log.json"
chk "managed block refreshed (stale text gone)" "! grep -q STALE-MANAGED-TEXT CLAUDE.md"
chk "managed block present once" "[ \$(grep -c 'BEGIN WRIT CONFIGURATION' CLAUDE.md) -eq 1 ]"
chk "text above markers preserved" "grep -q USER-ABOVE-MARKERS CLAUDE.md"
chk "text below markers preserved" "grep -q USER-BELOW-MARKERS CLAUDE.md"
chk "skills refreshed" "! grep -rq STALE-SKILL .claude/skills"
chk "slash commands refreshed" "! grep -rq STALE-CMD .claude/commands"
chk "SessionStart hook refreshed" "! grep -q STALE-HOOK .claude/settings.json"
chk "settings instructions refreshed" "! grep -q STALE-INSTRUCTIONS .claude/settings.json"
chk "AGENT_INSTRUCTIONS.md refreshed" "[ ! -f .writ/AGENT_INSTRUCTIONS.md ] || ! grep -q STALE-AGENT .writ/AGENT_INSTRUCTIONS.md"
chk "AGENTS.md byte for byte unchanged (codex = false opt-out honoured)" "[ -f AGENTS.tampered.md ] && cmp -s AGENTS.tampered.md AGENTS.md"
chk "AGENTS.md user text preserved" "[ ! -f AGENTS.md ] || grep -q USER-BELOW-MARKERS AGENTS.md"
chk "user permission preserved" "grep -q 'Bash(ls \\*)' .claude/settings.json"
chk "user settings key preserved" "grep -q USER-SETTING .claude/settings.json"
chk "SessionStart hook present in settings" "grep -q SessionStart .claude/settings.json"
chk "EXTRA user instruction preserved" "grep -q USER-INSTRUCTION .claude/settings.json"
chk "EXTRA user hook preserved" "grep -q USER-HOOK .claude/settings.json"
chk "EXTRA exactly one writ hook" "[ \$(grep -c 'Writ VCS Active' .claude/settings.json) -eq 1 ]"
chk "config [doctor] section preserved (finding 82)" "[ \"\$(cfg_val .writ/config.toml doctor stale_claim_minutes)\" = 77 ] && grep -q deadbeefcafe0123 .writ/config.toml"
chk "config [frameworks] codex = false preserved (finding 82)" "[ \"\$(cfg_val .writ/config.toml frameworks codex)\" = false ]"
chk "config [git] baseline_ref unchanged (finding 82)" "[ -n \"\$(cfg_val config.before.toml git baseline_ref)\" ] && [ \"\$(cfg_val config.before.toml git baseline_ref)\" = \"\$(cfg_val .writ/config.toml git baseline_ref)\" ]"
chk "config [project] initialized unchanged (finding 82)" "[ -n \"\$(cfg_val config.before.toml project initialized)\" ] && [ \"\$(cfg_val config.before.toml project initialized)\" = \"\$(cfg_val .writ/config.toml project initialized)\" ]"
echo "--- config.toml diff (before rerun vs after)"; diff config.before.toml .writ/config.toml || true
echo "--- init2 output"; cat init2.log
exit $fail
