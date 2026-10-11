#!/usr/bin/env bash
# Release check (0.4.1, findings 111 and 112).
#   111: a sealed file that git and writ both ignore (so no finish can commit
#     it) is an [info] note under `writ doctor --survival` that names both
#     exits (remove the ignore rule and seal, or leave it as history) and the
#     `writ seal --paths <path>` way to retire it. It is never a finding and
#     never takes away "safe to finish". Deleting the file and running
#     `writ seal --paths <path>` records the deletion and the note goes away.
#   112: `writ finish` prints a check mark beside the doctor and survival
#     lines only when survival is safe; when survival is red it does not.
#
# Usage: bench/release_checks/survival_notes.sh [WRIT_BIN] [SCRATCH_DIR]
# Exit 0 when every check passes, 1 on any FAIL, 2 on bad arguments.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
. "$ROOT/bench/release_checks/_lib.sh"
W="$(rc_bin "${1:-$ROOT/target/release/writ}")"
echo "binary: $W ($("$W" --version))"
rc_setup .survival_notes_scratch writ-survival-notes "${2:-}"
w() { env -i HOME="$HOME" PATH=/usr/bin:/bin:/usr/sbin:/sbin "$W" "$@"; }
g() { git -c user.name=t -c user.email=t@localhost "$@"; }
P=out/results/.gitignore

# 111
rc_git_project "$D/p111"
w init -y > /dev/null 2>&1; git add -A; g commit -qm init
w spec add --id sa --title a --agent agent-a --claim > /dev/null
mkdir -p out/results; printf '*\n!.gitignore\n' > "$P"
w seal -s "results dir" --agent agent-a --spec sa --paths "$P" > /dev/null 2>&1
echo "out/results/" >> .gitignore; git add .gitignore; g commit -qm "ignore results"
w spec done sa --agent agent-a --no-seal > /dev/null 2>&1
w finish -y > "$D/111.finish" 2>&1 || true   # git refuses the ignored file; it stays sealed only
w doctor --survival > "$D/111.survival" 2>&1 || true
w doctor --survival --format json > "$D/111.json" 2>/dev/null || true
chk "111: the sealed, ignored file is one [info] survival note naming the path" \
  "[ \$(grep -c '\[info\] survival: $P was sealed' $D/111.survival) -eq 1 ]"
chk "111: the note names both exits and the seal-the-deletion command" \
  "grep -q 'remove the ignore rule' $D/111.survival && grep -q 'stays as history' $D/111.survival && grep -q 'writ seal --paths $P' $D/111.survival"
chk "111: it is not a finding and the headline still says safe to finish" \
  "rc_json $D/111.json 'not any(f[\"check\"]==\"survival\" for f in r[\"findings\"])' && head -1 $D/111.survival | grep -q 'survival green as of .*; safe to finish'"
w spec add --id sb --title b --agent agent-b --claim > /dev/null
rm "$P"
rc=0; w seal -s "retire results/.gitignore" --agent agent-b --spec sb --paths "$P" > "$D/111.del" 2>&1 || rc=$?
w doctor --survival > "$D/111.after" 2>&1 || true
chk "111: writ seal --paths records the deletion and the note goes away" \
  "[ $rc -eq 0 ] && grep -q '^sealed' $D/111.del && ! grep -q '\[info\]' $D/111.after"

# 112: red survival (stale whole-file rewrite on a git base, as survival_tier.sh case 3)
rc_git_project "$D/p112"
w init -y > /dev/null 2>&1; git add -A; g commit -qm init
python3 -c 'open("shared.txt","w").write("".join(f"line {i}\n" for i in range(40)))'; git add -A; g commit -qm shared
w spec add --id sa --title a --agent agent-a --claim > /dev/null; w spec add --id sb --title b --agent agent-b --claim > /dev/null
sed -i.bak 's/^line 2$/line 2 A/' shared.txt; rm -f shared.txt.bak
w seal -s a --agent agent-a --spec sa --paths shared.txt > /dev/null 2>&1; w spec done sa --agent agent-a --no-seal > /dev/null 2>&1
python3 -c 'open("shared.txt","w").write("".join(f"line {i}" + (" B" if i == 30 else "") + "\n" for i in range(40)))'
w seal -s b --agent agent-b --spec sb --paths shared.txt > /dev/null 2>&1 || true
w spec done sb --agent agent-b --no-seal > /dev/null 2>&1 || true
rc=0; w finish -y > "$D/112.finish" 2>&1 || rc=$?
chk "112: on red survival finish refuses and prints no check mark on the doctor or survival line" \
  "[ $rc -ne 0 ] && grep -q 'would lose 1 sealed line' $D/112.finish && ! grep -E '✓ (doctor|survival)' $D/112.finish"
chk "112: on green survival finish prints the check mark (111 project above)" "grep -qE '✓ (doctor|survival)' $D/111.finish"
exit $fail
