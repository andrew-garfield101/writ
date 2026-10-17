# What We Measure

Every number writ publishes is on this page, with the script or evidence file that produced it, the date, and the binary it ran on. A number without a file is not on this page and not in a release note. Misses stay on the page: the history is the point, not the latest result.

Two kinds of source:

- **Scripts** live in this repository and anyone can run them: `bench/release_checks/`, `bench/finish_audit.py`, `bench/writ-scorecard.sh`.
- **Evidence files** are the raw records of live runs: each agent's full transcript, writ's own records, doctor output before and after, and the finish audit. Paths below are relative to the evidence collection, one folder per run.

A live run is two Claude Code agents, launched with `claude -p`, working at the same time in one directory on a small project, on features that touch the same files. Nobody tells them about writ beyond what `writ init` generates. At the end the harness runs `writ finish` and audits the commit.

## Zero touch safe finish

The question: at the end of a run, does `writ doctor` say the repository is safe to finish, with no human having run anything? Scored per agent; "2/2" means both agents left the repository in that state.

| Date | Release tree | Run | Result | Evidence |
|---|---|---|---|---|
| 2026-10-06 | 0.4.0 | live run 1 | 0/2 | `0.4.0-live-run1/summary.json` |
| 2026-10-06 | 0.4.0 | live run 3 | 0/2 | `0.4.0-live-run3/summary.json` |
| 2026-10-06 | 0.4.0 | live run 4 | 0/2 | `0.4.0-live-run4/summary.json` |
| 2026-10-11 | 0.4.1 | live run 1 | 0/2 strict, 2/2 lenient | `0.4.1-live-run1/summary.json`, `0.4.1-live-run1/rescore.json` |
| 2026-10-11 | 0.4.2 | run A, a claim left by an exited session | 2/2 | `0.4.2-live-runA/summary.json`, `report.md` |
| 2026-10-11 | 0.4.2 | run B, a spec committed under a working agent | 2/2 | `0.4.2-live-runB/summary.json`, `report.md` |
| 2026-10-12 | 0.4.2 | run B again, final build | 2/2 | `0.4.2-live-runB2/summary.json`, `report.md` |
| 2026-10-11 | 0.4.1 release | stranger kit on a real repository (677 commits) | 2/2 | `0.4.2-stranger-kit-e2e/summary.json` |
| 2026-10-14 | 0.4.3 development build `86a5d9c7d67d` | two agents, each splitting its task across two subagents; no writ instructions, sealing automatic; the receipt named the wrong author for four files (finding 169, fixed in `07209051b2d9`) | 2/2 | `0.4.3-fleet-live-c409/summary.json`, `report.md` |
| 2026-10-16 | 0.4.3 release candidate `5da679fd4fab` (Amis `3b14d8a540c3`) | exit run A: two agents, no writ instructions, sealing automatic; a hand seal over the other agent's sealed lines was refused and the agent merged them back from the seal | 2/2 | `0.4.3-live-runA-freeze4/summary.json`, `report.md` |
| 2026-10-17 | 0.4.3 release candidate `5da679fd4fab` | exit run B: two agents and a claim left by an exited session, released by one agent with `--stale`; a hand seal over the other agent's sealed line was refused and the agent merged it | 2/2 | `0.4.3-live-runB-freeze4-rerun/summary.json`, `report.md` (earlier attempts on this build ended on the agents' session limit and are kept as voids) |

What the misses were. On 0.4.0 the fix for the injected stale claim needed `--force`, which an agent should not run on its own. On 0.4.1 both agents read the fix two or three times and left it alone because the claim was not theirs; "lenient" counts a finish that was safe once the human ran the printed fix. 0.4.2 changed the finding to say any agent may run it.

Run B2 has a `void-429/` folder: an earlier attempt that stopped when the agents hit their session limit. It is kept and not scored.

The 0.4.2 runs used dev builds of the 0.4.2 tree, before the version bump, so their `summary.json` says `writ 0.4.1`; each `report.md` names the binary's hash and the seal it was built from.

## Sealed lines lost

The question: did the commit `writ finish` made keep every line any agent sealed? `bench/finish_audit.py` checks every added line of every seal against the finish commit. A line is either in the commit, replaced by a later seal, or lost.

| Date | What was finished | Added lines checked | Replaced by a later seal | Lost | Evidence |
|---|---|---:|---:|---:|---|
| 2026-10-06 | 0.4.0 live run 1 | 914 | 0 | 0 | `0.4.0-live-run1/audit.txt` |
| 2026-10-06 | 0.4.0 live run 3 | 725 | 0 | 0 | `0.4.0-live-run3/audit.txt` |
| 2026-10-06 | 0.4.0 live run 4 | 697 | 0 | 0 | `0.4.0-live-run4/audit.txt` |
| 2026-10-11 | 0.4.1 live run 1 | 1,046 | 4 | 0 | `0.4.1-live-run1/audit.txt` |
| 2026-10-11 | 0.4.2 run A | 989 | 0 | 0 | `0.4.2-live-runA/audit.txt` |
| 2026-10-11 | 0.4.2 run B | 1,062 | 3 | 0 | `0.4.2-live-runB/audit.txt` |
| 2026-10-12 | 0.4.2 run B again | 729 | 0 | 0 | `0.4.2-live-runB2/audit.txt` |
| 2026-10-11 | stranger kit, real repository | 100 | 0 | 0 | `0.4.2-stranger-kit-e2e/audit.txt` |
| 2026-10-12 | writ itself, commit `68ee77f` (60 files, four agents) | 10,228 | 26 | 0 | `audit_68ee77f.txt` |
| 2026-10-14 | 0.4.3 run with subagents (13 files, 7 specs); the receipt named the wrong author for four files (finding 169, fixed in `07209051b2d9`) | 2,694 | 65 | 0 | `0.4.3-fleet-live-c409/audit.txt` |
| 2026-10-16 | 0.4.3 release candidate, exit run A (13 files, 3 specs; finish's survival line: 1,079 sealed lines, none lost) | 1,896 | 4 | 0 | `0.4.3-live-runA-freeze4/audit.txt` |
| 2026-10-17 | 0.4.3 release candidate, exit run B (17 files, 2 specs; finish's survival line: 1,152 sealed lines, none lost) | 1,741 | 3 | 0 | `0.4.3-live-runB-freeze4-rerun/audit.txt` |

## Survival check time

The survival check (`writ doctor --survival`) is what decides whether writ will say "safe to finish". Measured on the writ repository itself by `bench/release_checks/doctor_timing.sh`, which copies the repository and its `.writ` to a scratch directory and times doctor there, five runs each.

| Date | Binary | Commit | Fast tier median | Survival, full run | Survival, cached |
|---|---|---|---:|---|---:|
| 2026-10-14 | writ 0.4.2 (Homebrew) | `6f0d234` | 150 ms | 4.0 to 4.8 s | 150 ms |
| 2026-10-16 | 0.4.3 release candidate `5da679fd4fab` | `6f0d234`, store of 297 seals; load average 3.3 rising to 4.6 on 10 cores | 60 ms | 5.3 to 5.6 s | 58 ms |

A full run is the whole store checked from scratch; it happens once after the store changes, and later runs reuse the result. Each run's samples and conditions are saved in `0.4.3-doctor-timing/` from 2026-10-16 on. The full run grows with the store: about 5.5 s at 297 seals. The 0.4.2 release note said 3.4 to 4.5 seconds; those timings were recorded in development seals, not saved to a file, so this page shows the script's measurement instead.

## Hurdles

The question: how often does writ get in an agent's way? A hurdle is a forced seal, an allowed removal, a refused seal, or a refused finish, counted from writ's own records only (seal fields and refusal events), over the seals of the specs in one sprint. Produced by `bench/writ-scorecard.sh --hurdles-records`, with `WRIT_HURDLE_SPECS` naming the sprint's specs.

| Release | Specs | Hurdles | Seals | Per 10 seals | Counted |
|---|---|---:|---:|---:|---|
| 0.4.1 | `stale-release`, `survival-tier`, `ci-and-hooks`, `harness-0-4-1` | 20 | 41 | 4.9 | 2026-10-14 |
| 0.4.2 | `act-on-it`, `merge-descendant`, `session-end-and-ci`, `harness-0-4-2` | 12 | 38 | 3.2 | 2026-10-14 |

0.4.2 over 0.4.1: 0.65 times the rate. The 0.4.2 release note said 0.67 times (20 over 39, 12 over 35) because the seal totals were counted before each release's own release seals; the hurdle counts are the same. From 0.4.3 the count is saved to a file when the release is cut.

## Unsealed window

The time from an agent's edit to the seal that records it, read from each run's hook log. writ takes the edit time from the file's modification time, so the figure is exact; on a file system whose timestamps are too coarse to order the edit, the record says so and the figure is an upper bound ("at most").

| Date | Run | Edit path | Edit to seal, median / worst | Over | Evidence |
|---|---|---|---|---|---|
| 2026-10-14 | 0.4.3 run with subagents (build `86a5d9c7d67d`); the receipt named the wrong author for four files (finding 169, fixed in `07209051b2d9`) | shell commands (Bash); the run used no edit tool | 366 ms / 1.2 s | 14 seals of 9 files, none with a coarse timestamp | `0.4.3-fleet-live-c409/summary.json` (`window_mtime`), from `hooks.log` |
| 2026-10-16 | 0.4.3 release candidate, exit run A | shell commands (Bash) and Write | 258 ms / 260 ms | 3 seals, none with a coarse timestamp | `0.4.3-live-runA-freeze4/summary.json` (`window_mtime`) |
| 2026-10-17 | 0.4.3 release candidate, exit run B | shell commands (Bash) | 439 ms / 439 ms | 2 seals, none with a coarse timestamp | `0.4.3-live-runB-freeze4-rerun/summary.json` (`window_mtime`) |

The edit tools (Edit, Write, NotebookEdit) have no live run figure yet; their fleet figures are below.

## Ambiguous changes

When two agents run shell commands at the same time and a file changes while both are running, writ cannot always tell whose change it is. It then seals the file for nobody and asks each of those agents in one line to claim it, because sealing a change under the wrong agent is worse than not sealing it.

| Run | Identities in the run\* | Agents asked | Hook sealed | Asked | Evidence |
|---|---|---|---|---|---|
| c409 | 6 | 4 | 14 | 17 | `0.4.3-fleet-live-c409/summary.json` (`ambiguity`), from `hooks.log` |
| A | 2 | 2 | 3 | 28 | `0.4.3-live-runA-freeze4/summary.json` (`ambiguity`), release candidate `5da679fd4fab` |
| B | 3 | 2 | 2 | 54 | `0.4.3-live-runB-freeze4-rerun/summary.json` (`ambiguity`), release candidate `5da679fd4fab` |

\* Identities in the run: c409's six are two mains and four subagents; run B's third identity is the injected stale session, which never edited.

With two agents running test suites in the same directory, most writes land while both agents have a command open, so writ asks rather than guesses. In these runs the agents sealed when asked and nothing was lost.

For 0.4.4, the ask count is the number to drive down.

Most of the ambiguity comes from commands that run for a long time: a test run such as pytest keeps an agent's command open for minutes, and every file another agent changes in that time overlaps it.

Credit moved: files sealed by an agent that the hooks had not named as the maker or a candidate. On the run above, 6: the four files of finding 169 and two sealed by a main agent whose candidates were its own subagent and the other main agent (`0.4.3-fleet-live-c409/report.md`). The exit runs on the build with the fix report this count beside the ambiguity row.

## Hook cost

| What | Size | Result | Evidence |
|---|---|---|---|
| Snapshot taken before each shell command (Bash), per call | 50 identities | not measured yet | |

## Fleet numbers

Many agents editing in one directory at the same time, driven through the installed edit hook by `bench/fleet/sim.py`, the way Claude Code calls it. Measured on 2026-10-14 on a 0.4.3 development build (binary sha256 `c63f55be96f4`), against 0.4.2 with the same script. The 0.4.3 release build is a later binary; its numbers replace these when its run files are saved. Reproduce with `bench/release_checks/fleet_numbers.sh`, which reruns each published shape and compares.

What these numbers cover. They are from the edit tool path (Edit, Write, NotebookEdit), where the hook is told which file the agent edited. Edits made by shell commands (the Bash hook pair) are not measured here, and "no edit sealed under the wrong agent" from these runs does not carry over to them.

At one agent the seal follows the edit by 38 ms. At 50 agents in one directory it lands when the turn ends: median 5.0 to 5.3 s, worst 11 s, every edit sealed by the end. At 50 agents the slowest hook took about 1 s to return to the agent. Edit to seal is an upper bound: the time from the edit to the first seal that holds it.

| Agents x edits | Repository | Hook return p50 / max | Edit to seal p50 / max | Edits sealed by the end | Sealed under another agent | Evidence |
|---|---|---|---|---|---|---|
| 1 x 50, one after another | clone of the writ repository, 279 files | 90 ms / 152 ms | 38 ms / 77 ms | 50 of 50 | 0 | `0.4.3-fleet-sim/1x50-this-repo.json` |
| 50 x 20, all at once (load average 3.8 on 10 cores) | generated, 1,105 files | 295 ms / 937 ms | 5.3 s / 11.0 s | 1,000 of 1,000 | 0 | `0.4.3-fleet-sim/50x20-run1-with-load.json` |
| 50 x 20, all at once, again (load average 3.5) | generated, 1,105 files | 291 ms / 1,081 ms | 5.0 s / 11.0 s | 1,000 of 1,000 | 0 | `0.4.3-fleet-sim/50x20-run2-with-load.json` |
| 0.4.2, 50 x 20, a seal command per edit | generated, 1,105 files | 7.0 s / 21.7 s | not measured | 9 files left unsealed | 0 | `0.4.3-fleet-sim/before-0.4.2-naive-50x20-run1.json` |
| 0.4.2, 50 x 20, again | generated, 1,105 files | 7.1 s / 20.1 s | not measured | 17 files left unsealed | 0 | `0.4.3-fleet-sim/before-0.4.2-naive-50x20-run2.json` |

0.4.2 has no auto seal; its rows use a hook that runs `writ seal --paths` after each edit, the closest a user could get. At 50 agents in 0.4.3 most edits are sealed when each agent's turn ends rather than on the edit itself (in the second run: 19 seals on the edit, 11 put off because another agent held the store lock, 42 at the end of the turn), which is why the window there is the turn, not the hook.

These are not a pass or fail against the 0.4.3 aim of edit to seal under 1 s: that aim was set for the two-agent live runs above, not for 50 agents.

The floor. If a seal is put off because another agent held the store lock, and the session is killed before its turn ends, that file stays unsealed until `writ finish` reports it as left out. Nothing seals it sooner.

### Seal cost

| What | Binary | p50 / max | Evidence |
|---|---|---|---|
| One `writ seal --paths` of one file on a clone of the writ repository, 15 runs | writ 0.4.2 | 165 ms / 188 ms | `0.4.3-fleet-sim/bare-seal-this-repo.json` |
| Same | 0.4.3 development build `c63f55be96f4` | 123 ms / 146 ms | same file |
| 50 identities x 20 seals at once, generated repository of 1,056 files, `writ seal` run directly | 0.4.3 development build before the lock change (`3947b702d852`) | 7.9 s / 18.2 s; 480 seals failed on the lock timeout, 521 succeeded | `0.4.3-fleet-haris/fleet_50x20_before_3947b702d852.json` |
| Same | after (`adbd8d70c276`) | 837 ms / 7.2 s; 0 failed | `0.4.3-fleet-haris/fleet_50x20_after_adbd8d70c276.json` |

## Release checks

Every claim in a release note has a script in `bench/release_checks/` that tests it on a throwaway project. To reproduce a release's count, build that release and run them all:

```bash
cargo build --release -p writ-cli
for s in bench/release_checks/*.sh; do "$s" || echo "FAILED: $s"; done
```

Each script prints one PASS or FAIL line per check and names, in its header, the release note sentence it backs. The index with each script's check count is `bench/release_checks/README.md`.
