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
| 2026-10-19 | 0.4.4 release candidate `6a9d7c2aad2d` | exit run A: two agents, no writ instructions; three seals refused for dropping the other agent's sealed lines, each merged | 2/2 | `0.4.4-live-runA-freeze6/summary.json`, `report.md` |
| 2026-10-19 | 0.4.4 release candidate `6a9d7c2aad2d` | exit run B: two agents and a claim left by an exited session, released by an agent without `--force` | 2/2 | `0.4.4-live-runB-freeze6/summary.json`, `report.md` |
| 2026-10-19 | 0.4.4 release candidate `6a9d7c2aad2d` | two agents splitting their tasks across named subagents (`store-builder`, `web-builder`); the receipt names them, and `web-builder-ab037dfe` for the second live instance | 2/2 | `0.4.4-fleet-live-freeze6/summary.json`, `receipt.txt` |
| 2026-10-19 | 0.4.5 release candidate `a42897769f1c` | exit run A: two agents, no writ instructions; five seals refused by the hook for dropping the other agent's sealed lines, and one `spec done` refused because 72 of the agent's own sealed lines were no longer on disk; that agent moved them into a new file and recorded their removal from `app.py` with `--allow-removals` | 2/2 | `0.4.5-live-runA-freeze7/summary.json`, `report.md` |
| 2026-10-19 | 0.4.5 release candidate `a42897769f1c` | exit run B: two agents and a claim left by an exited session, released by an agent without `--force` | 2/2 | `0.4.5-live-runB-freeze7/summary.json`, `report.md` |
| 2026-10-19 | 0.4.5 release candidate `a42897769f1c` | two agents splitting their tasks across named subagents; the receipt names `store-builder`, `store-builder-a27e3c8b`, `web-builder` and `web-builder-a5aaa142`, one name per live instance | 2/2 | `0.4.5-fleet-live-freeze7/summary.json`, `receipt.txt` (the first attempt that day ended after about a minute, when both agents hit their Claude Code session limit; kept as a void in `VOID.txt` and not scored) |

What the misses were. On 0.4.0 the fix for the injected stale claim needed `--force`, which an agent should not run on its own. On 0.4.1 both agents read the fix two or three times and left it alone because the claim was not theirs; "lenient" counts a finish that was safe once the human ran the printed fix. 0.4.2 changed the finding to say any agent may run it.

Run B2 has a `void-429/` folder: an earlier attempt that stopped when the agents hit their session limit. It is kept and not scored.

The 0.4.2 runs used dev builds of the 0.4.2 tree, before the version bump, so their `summary.json` says `writ 0.4.1`; each `report.md` names the binary's hash and the seal it was built from.

The 0.4.5 runs used the release candidate before the version bump, so their `summary.json` says `writ 0.4.4`; `writ_build` in the same file names the binary's hash.

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
| 2026-10-19 | 0.4.4 release candidate, exit run A (2 specs; finish's survival line: 1,045 sealed lines, none lost) | 1,625 | 0 | 0 | `0.4.4-live-runA-freeze6/audit.txt` |
| 2026-10-19 | 0.4.4 release candidate, exit run B (2 specs; finish's survival line: 1,119 sealed lines, none lost) | 1,870 | 27 | 0 | `0.4.4-live-runB-freeze6/audit.txt` |
| 2026-10-19 | 0.4.4 release candidate, run with named subagents (5 specs; finish's survival line: 1,485 sealed lines, none lost) | 2,275 | 3 | 0 | `0.4.4-fleet-live-freeze6/audit.txt` |
| 2026-10-19 | 0.4.5 release candidate, exit run A (3 specs; the receipt's survival line: 1,471 sealed lines, none lost; 12 seals, 12 signed) | 2,620 | 138 | 0 | `0.4.5-live-runA-freeze7/audit.txt`, `report.md` |
| 2026-10-19 | 0.4.5 release candidate, exit run B (2 specs; the receipt's survival line: 1,183 sealed lines, none lost; 9 seals, 9 signed) | 1,913 | 4 | 0 | `0.4.5-live-runB-freeze7/audit.txt`, `report.md` |
| 2026-10-19 | 0.4.5 release candidate, run with named subagents (6 specs; the receipt's survival line: 1,530 sealed lines, none lost; 15 seals, 15 signed) | 2,507 | 5 | 0 | `0.4.5-fleet-live-freeze7/audit.txt`, `report.md` |

## Survival check time

The survival check (`writ doctor --survival`) is what decides whether writ will say "safe to finish". Measured on the writ repository itself by `bench/release_checks/doctor_timing.sh`, which copies the repository and its `.writ` to a scratch directory and times doctor there, five runs each.

| Date | Binary | Commit | Fast tier median | Survival, full run | Survival, cached |
|---|---|---|---:|---|---:|
| 2026-10-14 | writ 0.4.2 (Homebrew) | `6f0d234` | 150 ms | 4.0 to 4.8 s | 150 ms |
| 2026-10-16 | 0.4.3 release candidate `5da679fd4fab` | `6f0d234`, store of 297 seals; load average 3.3 rising to 4.6 on 10 cores | 60 ms | 5.3 to 5.6 s | 58 ms |
| 2026-10-19 | 0.4.5 release candidate `a42897769f1c` | `17c0c13`, store of 436 seals; load average 8.2 over the last minute, 6.6 over fifteen, on 10 cores | 68 ms | 6.8 to 8.5 s, median 7.2 s | 70 ms |

A full run is the whole store checked from scratch; it happens once after the store changes, and later runs reuse the result. Each run's samples and conditions are saved in `0.4.3-doctor-timing/` from 2026-10-16 on. The full run grows with the store: about 5.5 s at 297 seals. The 0.4.2 release note said 3.4 to 4.5 seconds; those timings were recorded in development seals, not saved to a file, so this page shows the script's measurement instead.

The 0.4.5 full run misses the 5 s budget, as 0.4.2, 0.4.3 and 0.4.4 did, and by more: a median of 7.2 s at 436 seals (`0.4.5-release-checks-freeze7/doctor_timing_a42897769f1c_20261019T153038Z.json`), against 5.6 s at 332 seals on the 0.4.4 release candidate (`0.4.3-doctor-timing/doctor_timing_6a9d7c2aad2d_20261019T011820Z.json`). The budget stays, and the number is stated as measured. Signing is not the cause: on one copy of the repository, five full runs each, the 0.4.4 release build (`17c0c13`) took a median of 6,356 ms and the 0.4.5 tree 6,347 ms (`0.4.5-signing-cost/f212_survival_context_amis_tree.json`).

## Hurdles

The question: how often does writ get in an agent's way? A hurdle is a forced seal, an allowed removal, a refused seal, or a refused finish, counted from writ's own records only (seal fields and refusal events), over the seals of the specs in one sprint. Produced by `bench/writ-scorecard.sh --hurdles-records`, with `WRIT_HURDLE_SPECS` naming the sprint's specs.

| Release | Specs | Hurdles | Seals | Per 10 seals | Counted |
|---|---|---:|---:|---:|---|
| 0.4.1 | `stale-release`, `survival-tier`, `ci-and-hooks`, `harness-0-4-1` | 20 | 41 | 4.9 | 2026-10-14 |
| 0.4.2 | `act-on-it`, `merge-descendant`, `session-end-and-ci`, `harness-0-4-2` | 12 | 38 | 3.2 | 2026-10-14 |
| 0.4.3 | `auto-seal`, `harness-0-4-3`, `receipt-and-scale`, `release-0-4-3`, `close-0-4-3-agents`, `fix-0-4-3-build` | 28 | 88 | 3.2 | 2026-10-18 |
| 0.4.5 | `signed-0-4-5`, `harness-0-4-5`, `release-0-4-5` | 14 | 96 | 1.5 | 2026-10-29 |

0.4.2 over 0.4.1: 0.65 times the rate. The 0.4.2 release note said 0.67 times (20 over 39, 12 over 35) because the seal totals were counted before each release's own release seals; the hurdle counts are the same. 0.4.3 over 0.4.2: the same rate. 24 of its 28 hurdles are allowed removals in the test harness and auto seal specs, where fixtures were rewritten on purpose; 16 refusal events in the sprint name no spec of the set and are not counted. From 0.4.3 the count is saved to a file when the release is cut: `0.4.3-hurdles/hurdles_records_5d30ff7_20261018T071256Z.txt`.

0.4.5 over 0.4.3: 0.46 times the rate. Its 14 hurdles are seven refused seals and seven allowed removals. In every one of the seven refusals the lines removed were the same agent's own earlier lines (the refusal event says no other agent had worked on the file since), and each was then sealed with `--allow-removals`. The 20 refusal events that name no spec of the set are the same 20 counted when 0.4.4 was cut (`0.4.4-hurdles/hurdles_records_freeze6_20261019T014811Z.txt`). Saved: `0.4.5-hurdles/hurdles_records_freeze7_20261029T004152Z.txt`, counted on the writ repository before the 0.4.5 finish.

## Unsealed window

The time from an agent's edit to the seal that records it, read from each run's hook log. writ takes the edit time from the file's modification time, so the figure is exact; on a file system whose timestamps are too coarse to order the edit, the record says so and the figure is an upper bound ("at most").

| Date | Run | Edit path | Edit to seal, median / worst | Over | Evidence |
|---|---|---|---|---|---|
| 2026-10-14 | 0.4.3 run with subagents (build `86a5d9c7d67d`); the receipt named the wrong author for four files (finding 169, fixed in `07209051b2d9`) | shell commands (Bash); the run used no edit tool | 366 ms / 1.2 s | 14 seals of 9 files, none with a coarse timestamp | `0.4.3-fleet-live-c409/summary.json` (`window_mtime`), from `hooks.log` |
| 2026-10-16 | 0.4.3 release candidate, exit run A | shell commands (Bash) and Write | 258 ms / 260 ms | 3 seals, none with a coarse timestamp | `0.4.3-live-runA-freeze4/summary.json` (`window_mtime`) |
| 2026-10-17 | 0.4.3 release candidate, exit run B | shell commands (Bash) | 439 ms / 439 ms | 2 seals, none with a coarse timestamp | `0.4.3-live-runB-freeze4-rerun/summary.json` (`window_mtime`) |
| 2026-10-18 | 0.4.4 development build `8538885aee46`, exit run A | shell commands (Bash) | 560 ms / 3,055 ms | 28 seals, none with a coarse timestamp | `0.4.4-live-runA-freeze5/summary.json` (`window_mtime`) |
| 2026-10-19 | 0.4.4 release candidate `6a9d7c2aad2d`, exit run A | shell commands (Bash) | 416 ms / 2,993 ms | 25 seals, none with a coarse timestamp | `0.4.4-live-runA-freeze6/summary.json` (`window_mtime`) |
| 2026-10-19 | 0.4.4 release candidate `6a9d7c2aad2d`, exit run B | shell commands (Bash) | 272 ms / 428 ms | 23 seals, none with a coarse timestamp | `0.4.4-live-runB-freeze6/summary.json` (`window_mtime`) |
| 2026-10-19 | 0.4.4 release candidate `6a9d7c2aad2d`, run with named subagents | shell commands (Bash) 12, Write 2 | 300 ms / 345 ms | 14 seals, none with a coarse timestamp | `0.4.4-fleet-live-freeze6/summary.json` (`window_mtime`) |
| 2026-10-19 | 0.4.5 release candidate `a42897769f1c`, exit run A | shell commands (Bash) | 353 ms / 713 ms | 25 seals, none with a coarse timestamp | `0.4.5-live-runA-freeze7/summary.json` (`window_mtime`) |
| 2026-10-19 | 0.4.5 release candidate `a42897769f1c`, exit run B | shell commands (Bash) | 277 ms / 465 ms | 21 seals, none with a coarse timestamp | `0.4.5-live-runB-freeze7/summary.json` (`window_mtime`) |
| 2026-10-19 | 0.4.5 release candidate `a42897769f1c`, run with named subagents | shell commands (Bash) 19, Write 2, Edit 2 | 247 ms / 397 ms | 23 seals, none with a coarse timestamp | `0.4.5-fleet-live-freeze7/summary.json` (`window_mtime`) |

The 0.4.4 runs seal many more writes than 0.4.3's, including writes early in long commands such as a test run, whose seal waits for the command to return; that is where the worst cases above come from. The edit tools have one live figure so far (two Write seals in the run with named subagents); their fleet figures are below.

## Ambiguous changes

When two agents run shell commands at the same time and a file changes while both are running, writ cannot always tell whose change it is. It then seals the file for nobody and asks each of those agents in one line to claim it, because sealing a change under the wrong agent is worse than not sealing it.

| Run | Identities in the run\* | Agents asked | Hook sealed | Not attributed (records) | Ask lines shown | Evidence |
|---|---|---|---|---|---|---|
| c409 | 6 | 4 | 14 | 17 | 4 | `0.4.3-fleet-live-c409/summary.json` (`ambiguity`), from `hooks.log` |
| A | 2 | 2 | 3 | 28 | 5 | `0.4.3-live-runA-freeze4/summary.json` (`ambiguity`), release candidate `5da679fd4fab` |
| B | 3 | 2 | 2 | 54 | 10 | `0.4.3-live-runB-freeze4-rerun/summary.json` (`ambiguity`), release candidate `5da679fd4fab` |
| A on 0.4.4 | 2 | 0 | 28 | 0 | 0 | `0.4.4-live-runA-freeze5/summary.json` (`ambiguity`) and `ask_counts.json`, 2026-10-18, development build `8538885aee46` with the PostToolUseFailure hook and command text attribution; PostToolUseFailure fired 7 times, 32 windows opened and 32 closed |
| A on 0.4.4 | 2 | 0 | 25 | 0 | 0 | `0.4.4-live-runA-freeze6/summary.json` (`ambiguity`) and `ask_counts.json`, 2026-10-19, release candidate `6a9d7c2aad2d`; 31 windows, all closed |
| B on 0.4.4 | 3 | 0 | 23 | 0 | 0 | `0.4.4-live-runB-freeze6/summary.json` (`ambiguity`) and `ask_counts.json`, release candidate `6a9d7c2aad2d`; 33 windows, all closed |
| named subagents on 0.4.4 | 6 | 0 | 14 | 0 | 0 | `0.4.4-fleet-live-freeze6/summary.json` (`ambiguity`) and `ask_counts.json`, release candidate `6a9d7c2aad2d`; 58 windows, all closed; one seal attributed from the command text |
| A on 0.4.5 | 2 | 0 | 26 | 0 | 0 | `0.4.5-live-runA-freeze7/summary.json` (`ambiguity`) and `ask_counts.json`, 2026-10-19, 0.4.5 release candidate `a42897769f1c` |
| B on 0.4.5 | 3 | 0 | 21 | 0 | 0 | `0.4.5-live-runB-freeze7/summary.json` (`ambiguity`) and `ask_counts.json`, 0.4.5 release candidate `a42897769f1c` |
| named subagents on 0.4.5 | 6 | 0 | 23 | 0 | 0 | `0.4.5-fleet-live-freeze7/summary.json` (`ambiguity`) and `ask_counts.json`, 0.4.5 release candidate `a42897769f1c`; 49 windows, none left open (`README.md`) |

\* Identities in the run: c409's six are two mains and four subagents; run B's third identity is the injected stale session, which never edited.

"Not attributed" counts hook log records, including the end of turn sweep, which prints nothing; "ask lines shown" counts the lines an agent actually received (finding 187, counted 2026-10-18 from the hook logs and transcripts, `0.4.4-ask-count/results.md`). In these runs the agents sealed when asked and nothing was lost.

Where the records come from (finding 186, measured 2026-10-18 on these three runs): Claude Code sends the PostToolUse event only after a shell command succeeds; a failed command gets a separate PostToolUseFailure event, which 0.4.3's hooks do not register. So a command that exits non zero, such as `cat` on a missing file, leaves that agent's window open until its turn ends, and every file the other agent changes in that time has two candidates. All 99 records across the three runs have such a window among their candidates. 0.4.4 registers the failure event and closes stale windows; on the same three runs that leaves 6 records of 99 ambiguous, with 0 wrong attributions (`0.4.4-ask-count/results.md`, the "rules 0+1+2" column; the change ships when the two agent runs on the 0.4.4 build confirm it).

Credit moved: files sealed by an agent that the hooks had not named as the maker or a candidate. On the run above, 6: the four files of finding 169 and two sealed by a main agent whose candidates were its own subagent and the other main agent (`0.4.3-fleet-live-c409/report.md`). On the three 0.4.4 release candidate runs, 0 in each, and 0 wrong attributions, checked by hand against each identity's own transcript (`README.md` and `summary.json` `credit_moved` in `0.4.4-live-runA-freeze6/`, `0.4.4-live-runB-freeze6/` and `0.4.4-fleet-live-freeze6/`).
 On the three 0.4.5 release candidate runs, 0 in each, and 0 wrong attributions (`summary.json` `credit_moved` and `README.md` in `0.4.5-live-runA-freeze7/`, `0.4.5-live-runB-freeze7/` and `0.4.5-fleet-live-freeze7/`).

## Signed seals and recorded refusals

From 0.4.5 writ signs every seal with a key it makes for the identity that sealed, and the hook records each seal it refuses so the receipt can list it. The question for each run: was every seal in the receipt signed, did every refusal in the hook log reach the receipt, and did every hook run finish? A hook run writes a start record and an end record; a start with no end is a hook stopped before it finished, for example by Claude Code's 10 second timeout.

| Date | Run | Seals signed | Hook refusals in the receipt, of those in the hook log | Hooks stopped before finishing | Evidence |
|---|---|---|---|---|---|
| 2026-10-19 | 0.4.5 release candidate `a42897769f1c`, exit run A | 12 of 12; `writ verify --all-chains` exit 0, 4 chains, 0 failures | 5 of 5 | 0 (92 started, 92 ended) | `0.4.5-live-runA-freeze7/report.md`, `README.md`, `summary.json` (`hooks_log`) |
| 2026-10-19 | 0.4.5 release candidate `a42897769f1c`, exit run B | 9 of 9; `writ verify --all-chains` exit 0, 3 chains, 0 failures | 2 of 2 | 0 (103 started, 103 ended) | `0.4.5-live-runB-freeze7/report.md`, `README.md`, `summary.json` (`hooks_log`) |
| 2026-10-19 | 0.4.5 release candidate `a42897769f1c`, run with named subagents | 15 of 15; `writ verify --all-chains` exit 0, 7 chains, 0 failures | 1 of 1 | 0 (120 started, 120 ended) | `0.4.5-fleet-live-freeze7/report.md`, `README.md`, `summary.json` (`hooks_log`) |

Seals signed counts the seals in the receipt's range. Each repository holds one more seal, unsigned: the import of the existing git history at setup, made before any agent ran.

A writ command a named subagent types in its own shell now runs as that subagent. In the run with named subagents, the one subagent that typed `writ spec done -s` itself (`store-builder`) closed its own spec, and the receipt quotes it: "Add User.password_hash, PBKDF2 password module, in-memory UserStore with auth, and tests; merged concurrent Task/TaskStore additions in models.py" (`0.4.5-fleet-live-freeze7/receipt.txt`). The other three named subagents typed no `spec done`, and their receipt lines read "no summary from the agent".

## Hook cost

| What | Size | Result | Evidence |
|---|---|---|---|
| Snapshot taken before each shell command (Bash), per call | 50 identities | not measured yet | |
| Edit hook return with signing on, one agent, 50 edits one after another, 0.4.5 release candidate `a42897769f1c`, 2026-10-19 | clone of the writ repository, 330 files | p50 18.5 ms, p90 19.8 ms, max 43.6 ms; the 0.4.4 release candidate on the same shape (320 files): 17.5 ms, 19.2 ms, 42.5 ms | `0.4.5-gate-freeze7/test-gate-hook-latency.json`, `0.4.4-gate-freeze6/test-gate-hook-latency.json` |

## Fleet numbers

Many agents editing in one directory at the same time, driven through the installed edit hook by `bench/fleet/sim.py`, the way Claude Code calls it. Measured on 2026-10-15 on the 0.4.3 release candidate (binary sha256 `5da679fd4fab`, the tree of the 0.4.3 tag before its Windows build fix), and on 2026-10-14 on an earlier development build (`c63f55be96f4`, before the store lock stopped polling), against 0.4.2 with the same script. Reproduce with `bench/release_checks/fleet_numbers.sh`, which reruns each published shape and compares.

What these numbers cover. They are from the edit tool path (Edit, Write, NotebookEdit), where the hook is told which file the agent edited. Edits made by shell commands (the Bash hook pair) are not measured here, and "no edit sealed under the wrong agent" from these runs does not carry over to them.

On the release candidate, at one agent the seal follows the edit by 13 ms. At 50 agents in one directory: median 985 ms, nine in ten under 3.0 s, worst 6.5 s, every edit sealed by the end; the slowest hook took 682 ms to return to the agent. The development build before the lock change read 38 ms at one agent and a median of 5.0 to 5.3 s at 50, its rows kept below. Edit to seal is an upper bound: the time from the edit to the first seal that holds it.

| Agents x edits | Repository | Hook return p50 / max | Edit to seal p50 / max | Edits sealed by the end | Sealed under another agent | Evidence |
|---|---|---|---|---|---|---|
| 1 x 50, one after another, release candidate (load average 10 on 10 cores) | clone of the writ repository, 279 files | 21 ms / 49 ms | 13 ms / 42 ms | 50 of 50 | 0 | `0.4.3-fleet-sim/1x50-this-repo-freeze4.json` |
| 50 x 20, all at once, release candidate (load average 7.3 rising to 10.2) | generated, 1,105 files | 383 ms / 682 ms | 985 ms / 6.5 s | 1,000 of 1,000 | 0 | `0.4.3-fleet-sim/50x20-freeze4.json` |
| 1 x 50, one after another, development build | clone of the writ repository, 279 files | 90 ms / 152 ms | 38 ms / 77 ms | 50 of 50 | 0 | `0.4.3-fleet-sim/1x50-this-repo.json` |
| 50 x 20, all at once (load average 3.8 on 10 cores) | generated, 1,105 files | 295 ms / 937 ms | 5.3 s / 11.0 s | 1,000 of 1,000 | 0 | `0.4.3-fleet-sim/50x20-run1-with-load.json` |
| 50 x 20, all at once, again (load average 3.5) | generated, 1,105 files | 291 ms / 1,081 ms | 5.0 s / 11.0 s | 1,000 of 1,000 | 0 | `0.4.3-fleet-sim/50x20-run2-with-load.json` |
| 0.4.2, 50 x 20, a seal command per edit | generated, 1,105 files | 7.0 s / 21.7 s | not measured | 9 files left unsealed | 0 | `0.4.3-fleet-sim/before-0.4.2-naive-50x20-run1.json` |
| 0.4.2, 50 x 20, again | generated, 1,105 files | 7.1 s / 20.1 s | not measured | 17 files left unsealed | 0 | `0.4.3-fleet-sim/before-0.4.2-naive-50x20-run2.json` |
| 50 x 5, all at once, 0.4.5 release candidate `a42897769f1c` with signing on, 2026-10-19 (load average 7.2 rising to 9.2 on 10 cores) | generated, 1,105 files | 303 ms / 540 ms | 414 ms / 2.0 s | 250 of 250 | 0 | `0.4.5-gate-freeze7/test-gate-fleet.json` |

0.4.2 has no auto seal; its rows use a hook that runs `writ seal --paths` after each edit, the closest a user could get. At 50 agents in 0.4.3 most edits are sealed when each agent's turn ends rather than on the edit itself (release candidate: 273 hook calls sealed on the edit, 206 put off because another agent held the store lock, 26 seals at the end of the turn; development build, second run: 19, 11 and 42), which is why the window there is the turn, not the hook.

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

| Release | Build | Checks passed | Scripts | Misses | Evidence |
|---|---|---:|---:|---|---|
| 0.4.5 | release candidate `a42897769f1c`, 2026-10-19 | 357 of 359 | 32 | the survival full run over its 5 s budget (`doctor_timing.sh` check 2, above); the CHANGELOG heading 0.4.5 against the version before the bump, 0.4.4 (`tag_version.sh` check 6), which passes once the version is bumped | `0.4.5-release-checks-freeze7/SUMMARY.txt` |

The test gate (`bench/test-gate.sh`) runs the Rust and Python suites, the formatter and the Windows build check on the release build.

| Release | Build | Rust passed / failed / ignored | Python passed / failed / expected failures | Formatter, Windows check | Evidence |
|---|---|---|---|---|---|
| 0.4.5 | release candidate `a42897769f1c`, 2026-10-19 | 2,866 / 0 / 9 | 995 / 0 / 20 (10 skipped) | both exit 0 | `0.4.5-gate-freeze7/test-gate.json`; its one problem is the same CHANGELOG heading check, cleared by the version bump |
