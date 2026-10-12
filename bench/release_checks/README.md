# Release checks

**Rule:** every claim in a release note gets a check script here before the tag.
If the note says writ does something, a script in this directory does it on a
throwaway project and exits non zero when the claim is false. A claim without a
script is cut from the note or the tag waits.

Why: the 0.3.0 note told users to re-run `writ init -y` after upgrading. Testing
that sentence as written (13 of 19 checks on the build of the day) found two
bugs the test suite missed (findings 71 and 72), both fixed before the tag.

## Conventions

- One script per claim, named after the claim: `init_rerun.sh`.
- Arguments: `[WRIT_BIN] [SCRATCH_DIR]`. Defaults: `target/release/writ` from
  this repo and a fresh `mktemp -d`. Run from any directory.
- Never touch this repo's `.writ` or the caller's identity: work in the scratch
  dir, `unset WRIT_AGENT_ID`.
- Print one `PASS <check>` or `FAIL <check>` line per check; exit 0 only when
  every check passed, 1 on any failure, 2 on bad arguments.
- The header comment names the release note sentence the script backs.

## Running

```bash
cargo build --release -p writ-cli
for s in bench/release_checks/*.sh; do "$s" || echo "FAILED: $s"; done
```

## Index

| Script | Claim | Checks |
|---|---|---|
| `init_rerun.sh` | Re-running `writ init -y` refreshes generated files, keeps user text and settings, and leaves the store, specs and `.writ/config.toml` alone (0.3.0; config checks and the codex opt-out check added for finding 82, green from 297f927ac566; check 27: the SessionStart hook runs PATH's writ with the init writ as fallback and a note when they differ, finding 89, 0.4.1; checks 28-29: rerun adds exactly one SessionEnd hook beside the user's own, and it runs PATH's `writ session-end` silently with exit 0, 0.4.2; brew 0.4.1 fails exactly 28-29) | 29 |
| `hook_after_uninstall.sh` | After the writ that ran `writ init` is uninstalled (doctor's multiple-writs fix), the SessionStart hook still prints the brief from the writ left on PATH, says which one it ran, and doctor names the dead fallback as version_skew with `writ init -y` (finding 89, 0.4.1; brew 0.4.0 fails 3 of 5) | 5 |
| `upgrade_hook_skew.sh` | A project initialized by an older writ gets one `version_skew` finding on `.claude/settings.json` with `writ init -y` from the new build, and `writ init -y` clears it (0.4.1 Upgrade section); checks 3-4: today's start hook without the SessionEnd hook is `version_skew` on `.claude/settings.json`, `writ init -y` adds it and doctor is clean (0.4.2 Upgrade section) | 4 |
| `stale_release.sh` | `writ spec release <id> --stale` releases only a provably gone holder (same host, pid exited), refuses a live holder and another host's claim naming the evidence and the human's `--force`, is not recorded as forced; doctor's stale_claim fix is `--stale`, no fix contains `--force` (finding 91, 0.4.1; brew 0.4.0 fails 5 of 5). 0.4.2 session end: init installs the SessionEnd hook; it records one session end per ended session (none for `/clear` or `/resume`) naming the session's claims; a record never overrides a live pid; with the pid unjudgeable (no process table) a record is the proof and `--stale` releases, without one it refuses, and doctor never writes a record (brew 0.4.1 fails exactly the 5 positive session-end checks) | 14 |
| `committed_chains.sh` | After `writ finish` commits every spec, forked seal chains are not diverged branches: brief reads risk low, score 0, no converge and no next action, doctor clean; before finish it still asks to converge (finding 97, 0.4.1; brew 0.4.0 fails 2 of 5) | 5 |
| `survival_tier.sh` | "safe to finish" only when fast clean and survival green on the current state; green path commits; a stale whole-file rewrite by another agent never drops a sealed line silently (0.4.1 survival tier; stale rewrite case FAILS on the sprint 4 tree, reported to CC) | 7 |
| `seal_spec_rule.sh` | Seal spec rule: `--agent` with no spec is refused (98), a human no-spec seal warns with `writ spec add "..." --claim` (99), `claim_enforcement = "strict"` refuses a no-spec seal (100); all writ calls under `env -i` (0.4.1; brew 0.4.0 fails 4 of 5) | 5 |
| `override_refusal.sh` | Finding 102: a seal dropping this spec's sealed lines is refused once, names the other agent with "keep your lines" / "accept <agent>'s version" (or, with nobody, says the lines are no longer on disk and offers "remove them on purpose"), gives the `--allow-removals` command, never `--force`; the confirmed override seals and records `override_context` {path, other_agents, lines, added_by}. Check 10: the same on a file whose base came from git (FAILS on 36b1ba149cdc, reported) | 10 |
| `survival_readonly.sh` | Finding 110: `writ doctor --survival` writes nothing to the store: object count, `stale_rewrite_notices.json` and every other store file (incl. `convergence_v3_pending.json`) unchanged, only doctor caches, security events and `last_opened_at` may move; a finish afterwards still commits both specs (4/5 on 8cc9f095c3c4; 5/5 after Amis 0b987e4a4376 and Haris 987df6664f3e) | 5 |
| `survival_notes.sh` | Finding 111: a sealed file git and writ both ignore is one `[info]` survival note naming both exits and `writ seal --paths <path>`, never a finding, safe to finish kept; deleting it and sealing by path clears it. Finding 112: finish shows the check mark only when survival is safe (0.4.1; brew 0.4.0 fails 5 of 6); 0.4.2: 113 the note on the first `doctor --survival` before any finish, finish names the file with both exits; 116 after the deletion seal, finish and a later `doctor --survival` are silent on it; 123 the note shows while the spec is still open (brew 0.4.1 passes 7 of 11) | 11 |
| `stale_anyone_may_fix.sh` | Finding 114: an exited holder's stale_claim says it has exited and any agent may run the fix, `anyone_may_fix: true` in doctor JSON, human output, context JSON and default context; an uninvolved agent's paste clears it, a third agent's paste exits 0 and says already; never on a live claim's finding (viewers other, human, b, a) nor on a needs_human one; the CLAUDE.md block says to act on such findings (0.4.2; brew 0.4.1 passes 1 of 11) | 11 |
| `merge_descendant.sh` | Finding 115 on the real sprint 4 blobs (`crates/writ-cli/tests/fixtures/f115`): an informed descendant is survival green and safe to finish, finish commits it byte for byte with no conflict, escalation or refusal, survival green after; the stale rewrite stays red, never safe, finish refuses with HEAD unchanged; both fixture seals accepted (0.4.2; brew 0.4.1 passes 4 of 8, the stale and fixture checks) | 8 |
| `injection_b.sh` | Injection B (finding 121): after a finish commits the agent's spec, a seal in both forms (`--spec auth`, auto-scoped) is refused naming the spec as committed and prints exactly one command, `writ spec add "<title> (follow-up)" --id <id> --claim && writ seal -s "<summary>" --spec <id> --paths <paths>`, no reopen, no --force; pasted twice both exit 0 (the second says already), one seal on the follow-up spec claimed by the agent, no second spec, file unchanged, doctor clean (0.4.2; brew 0.4.1 passes 4 of 12) | 12 |
| `handoff_why.sh` | U.3 and finding 122 on Bri's handoff_states.rs fixture (three specs, two shared files, one override after being told): handoff names exactly the finish audit's specs and merges (bench/finish_audit.py), resolutions accepted_after_told with the finish wording and kept, all agents, no unsealed work, markdown and brief (<= 2 KB); next_action equals doctor's before and after finish, never converge; handoff and why write nothing to objects, seals or specs; why gives history sa, sb, sa with added/removed lines, the override and the merge commit, and sb, sc kept for two.txt; 122 mixed wording with counts, own rewrite is rewrote_own and never "bravo accepted charlie's version" (0.4.2; brew 0.4.1 passes the 4 fixture checks of 13 reported, and its finish says "bravo accepted charlie's version") | 24 |
| `spec_done_committed.sh` | Finding 124: `writ spec done -s` after a finish committed the agent's spec exits 0, says already committed naming the spec and the commit, never "no spec claimed"; the summary is on `spec show` and in handoff; doctor clean (0.4.2; brew 0.4.1 passes 1 of 6) | 6 |
