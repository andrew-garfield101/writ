# Changelog

All notable changes to writ will be documented in this file.

Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Found and fixed before release

- In the second exit run (freeze 3), one agent's hand seal replaced the other agent's sealed tests in tests/test_models.py (seal e8970c1acb67 over 824769147a89) with no refusal and no line to anyone, because hand seals had no check against other specs' sealed lines. Both agents then closed their specs on "fast checks clean" while the survival tier was red. Finish refused correctly and no line was lost, but the agents were never told. Found during release testing and never shipped: hand seals and `spec done` now refuse in that state (above). (finding 180)

- In the exit run on the first 0.4.3 build, the auto seal hook recorded a false sentence. One agent's Write was sealed. Twenty seconds later another agent's Bash command overwrote the file with a version that dropped 38 of those sealed lines; whose write it was could not be told (ruling 160). On the first agent's next tool call, the hook's retry of queued paths sealed the overwrite under the first agent's spec, using the own-rewrite allowance (finding 144). Seal 5d266561d99c therefore says "claude-code-de5d removed 38 own line(s) from app.py on purpose after being told". The agent was never told, and the lines were not its removal. No line was lost: the other agent merged by hand from the first agent's seal. Fixed: a retry, Stop or SessionEnd seals a queued path only when the file's latest write falls in no other agent's command; an ambiguous path is never retried; a retry never uses the allowance. When the hook does use it for an agent's own fresh edit, the record says "rewrote N own line(s) in its own edit, recorded by writ's auto seal", never "after being told". (finding 173)
- An auto spec that never got a seal is removed at SessionEnd, so no receipt lists "0 seals". (finding 174)

### Added

- Auto seal through Claude Code hooks. `writ init` installs one hook command, `writ hook`, on PreToolUse (Bash), PostToolUse (Edit, Write, MultiEdit, NotebookEdit, Bash), UserPromptSubmit, Stop, SubagentStop and SessionEnd. After each edit it seals exactly the edited path under the agent's own claimed spec. An agent with no spec gets one on its first edit: `auto-<key>`, titled from its first prompt line. The hook always exits 0, never seals another agent's pending path, and writes every firing to `.writ/hooks.log`. Auto seal shrinks the window in which an edit is unsealed to the hook's latency; it does not make an overwrite between an edit and its seal impossible. On a clone of writ's own repository (release build, one agent), the hook returns in p50 97 ms, max 156 ms, and the edit-to-seal window is p50 89 ms, max 150 ms.
- A subagent spawned by the Agent tool gets its own identity, `<parent id>-<first 8 of agent_id>`, and its own auto spec, which names the parent. This needs no `WRIT_AGENT_ID` and no template line. A writ command the subagent runs by hand in Bash still resolves to the parent's identity, because Claude Code gives subagents the parent's environment. (finding 131)
- An edit that drops lines another open spec sealed is not auto sealed. The agent is told once, in its next context, as one line naming the path, the other agent and the fix: `writ seal --auto --paths <path>` keeps its version, or it can put the lines back and its next edit seals itself. (findings 141, 145)
- When the repository lock is busy, the hook waits at most 200 ms (`[hooks] lock_wait_ms` in `.writ/config.toml`). It then leaves the path pending, and the agent's next hook seals it, as does Stop or SessionEnd. Stop seals a touched path whose content changed after its auto seal. SessionEnd closes the session's auto specs: it records the count and span of their seals and writes no summary. A spec the agent closed with `writ spec done -s` is left alone. (ruling 149, finding 151)
- Edits made through Bash are auto sealed too. Under bypass permissions, Claude Code steers agents to `sed`, heredocs and scripts, so the edit tools' hook alone never fired (finding 153). The ambiguity line names the agents whose commands were running. A PreToolUse hook on Bash records the tree's stat signature; the PostToolUse hook seals the paths that call changed, under the agent's own spec, with the summary "auto: Bash <paths>". A path belongs to the agent whose Bash call was running when its mtime says it was written. When two agents' calls were both running, nobody's seal takes the path: each agent is told in one line, with `writ seal --paths <path> --auto` to use if the change is its own. With no usable mtime (`cp -p`, a checkout), the call owns the path only if no other agent's call overlapped it. The unsealed window over a Bash edit is reported as an upper bound: at most from the call's start to its seal. Stop seals what a missing Post left. (rulings 155, 160)
- A pasted `writ seal --paths <p> --auto` clears the state in every state it can be printed in. With no spec at all it opens the agent's auto spec. After the agent closed its auto spec it opens `<id>-followup`, which records the spec it `continues`; the closed spec keeps the agent's words. One ambiguity line covers all of a call's ambiguous paths, comma-separated (`--paths a,b`). A subagent's line carries `--agent <parent>-<agent8>`, because its shell resolves to its parent. A hand-typed fix from the parent's shell seals as the one subagent candidate; with several, it is refused and each `--agent` form is named. (finding 168)
- An ambiguous path is sealed only by one of the agents whose commands could have written it. Another agent's seal of it, hand or `--auto`, is refused with one line naming them. `writ doctor`'s left_out finding for it names them, is not for any agent and not for a human, and its fix is `writ seal --paths <p> --auto` for one of them. (finding 169)
- An auto spec's title is the first sentence of the agent's prompt; if that is long, it is cut at a word boundary under 60 characters with "…". (finding 170)
- `writ spec done` claims the one unclaimed spec the agent created, as its first seal already does. (ruling 157)
- `.writ/hooks.log` writes each record with a single append, so concurrent hooks never interleave a line. (finding 154)
- `writ seal --auto`: writ writes the summary (`auto: seal <paths>`) and prints no advisories.
- `writ log` collapses consecutive auto seals of one spec into one line, with the count and span; `--format json` keeps every seal.
- The receipt. Each commit `writ finish` makes carries a body built only from writ's records: one line per spec (agents, seal count, and the agent's own closing words, or "no summary from the agent" with the seal count and span), each file two committed specs sealed and how it resolved, overrides in the seal's own wording, seal and finish refusals in the range, and the survival line last. The body is under 40 lines. `writ finish --receipt <path>` writes the full version as markdown for a PR description. `writ handoff --format pr` prints the same thing, `--format receipt` prints the commit body form, and `--commit <rev>` rebuilds either one for an earlier finish commit. The range and survival numbers of each finish are kept in `.writ/receipts/<commit>.json`.

### Changed

- A seal is cheaper, and so is waiting for one. On a 1,000-file repository, a one-path seal takes p50 53 to 58 ms, down from 167 to 220 ms. With 50 agents sealing 20 times each at once, every seal now lands: p50 837 ms, 0 lock timeouts, 19.6 s total. In 0.4.2 that run had p50 7.9 s, 480 of 1,000 seals failed on the lock, and it took 158 s. `bench/fleet_bench.py` prints these numbers. The changes: the per-seal storage check no longer decompresses every object, the stuck-file check compares git object ids instead of hashing HEAD blobs, a named-path seal no longer walks the tree, opening a repository no longer takes the lock, and lock waiters block in the kernel instead of polling. (finding 158)
- On macOS, store writes call `fsync(2)` instead of `F_FULLFSYNC`. The ordering is the same: data is on disk before each rename, as `sync_data` gives on Linux. The drive's write cache is no longer forced, so a power cut can lose the last seconds of seals. Git takes the same exposure for its loose objects, which it does not fsync by default. Set `[store] full_fsync = true` in `.writ/config.toml` to get the old behaviour back. (finding 158)
- The generated CLAUDE.md and AGENTS.md block, the SessionStart echo and the settings instruction are now five lines with no mandatory command: in Claude Code, writ seals the edits it can attribute and asks about the rest; `writ context` shows state, `writ spec done -s` records the agent's own summary (optional), and agents act on doctor findings not marked for a human. `.writ/AGENT_INSTRUCTIONS.md`, for hosts without hooks, keeps the hand workflow. A fifth line says that when writ cannot tell whose change a file is, it asks with one line and the agent runs that command. The first line reads "In Claude Code, writ seals the edits it can attribute and asks you with one line about the rest; in other tools, seal with `writ seal -s "<summary>" --paths <files>`". In the exit run, writ asked about more writes than it sealed, so "sealed automatically" was not true. (ruling 175) The sealing sentence names the tool it holds in, because AGENTS.md is read by Codex and other tools that have no writ hook yet. (finding 146)
- Every agent-facing seal (a hand seal, the hook, Python, MCP through the CLI) refuses to record the removal of lines another open spec sealed. The refusal is one line naming the file, the other agent, its spec and seal, and the number of lines, with two ways out: merge their lines back and seal again, or `writ seal --paths <p> --auto --allow-removals <p>` to record the removal on purpose ("accepted <agent>'s version ... after being told"). The hook and a hand seal print the same line for the same state. Removing a spec's own lines behaves as before. (finding 145 closed, ruling 181)
- `writ spec done` checks survival of the closing agent's own specs against the files on disk, fresh, never from a cached green. It prints the check's time, and refuses while any of their sealed lines are missing, naming the file and, when another spec's seal removed the lines, that agent and seal. If the check cannot finish within 5 s it refuses with "survival not run". It never offers `--force`. (ruling 181)
- Both checks are on for every agent-facing surface. Writ's own test fixtures turn them off with `[seal] foreign_line_check = false`, a setting `writ init` never writes. While it is set, `writ doctor` and `writ context` show a yellow `seal_check_disabled` finding for a person, each seal records that it was made with the check off, and the receipt says how many were. With the key set, a finish can commit a closed spec while an open spec's sealed line is missing from disk and survival reads green: open specs are not audited at finish, and spec done's check is off too. That is what the yellow is for. (ruling 183)

### Fixed

- `writ doctor --survival` could say "safe to finish" from its cache while a full run did not. A run with notes moved the incremental bound forward, so later runs stopped auditing the committed specs that carried those notes. Since 0.4.1, the warm "safe to finish" could therefore be false. Now a run with notes does not move the bound, specs with notes or info lines are audited again on every run, and a change to the root ignore files triggers a full run. (finding 136)
- The "override captured another spec's version" note no longer names a seal made after the override as its source. Order comes from the store (parent links and recorded content), and timestamps decide only when the store records no order. A captured override on a spec that a finish has committed is history, whether or not `--accept-removals` was passed. After notes, the headline says "a person decides". (finding 136)

## [0.4.2] — 2026-10-12

### Fixed

- The seal-tree merge in `writ finish` takes a seal built on another spec's version without a conflict when its content carries that spec's work: it keeps a distinctive line of an earlier seal of that spec and lacks every line a later seal of that spec replaced (those lines being that spec's own, not base text). Before, a descendant that deleted a block the other spec had relabelled conflicted and the survival check refused (sprint 4, `test_adoption_gates.py`). A rewrite whose content lacks the other spec's lines still conflicts and finish still refuses, whatever base the seal recorded. (finding 115)
- A sealed file that git and writ both ignore (and HEAD does not hold) gets its info note on the first `writ doctor --survival`, not only after a finish. `writ finish` no longer lists it under "Sealed but NOT staged"; it prints the same note as doctor (the seal stays as history; delete the file and `writ seal --paths <path>` to retire it). (finding 113)
- The info note for a sealed file that git and writ both ignore also shows while its spec is still open, with the same text. (finding 123)
- A seal that recorded the deletion of such a path retires the note: `writ doctor --survival` and `writ finish` are silent on it afterwards, before and after a finish, and the deletion seal is never cited as sealed-but-ignored. (finding 116)
- `writ doctor --survival` full run on writ's own store: 5.7-6.4 s down to 3.4-4.2 s (each path's seal history is read and diffed once, not once per spec); incremental and cached runs unchanged.

- Every doctor finding carries `anyone_may_fix`: true when the printed fix is one any agent may run, including an agent whose spec the finding is not about, because writ has checked it is safe. It is false when the fix needs a human, when it changes something outside the repository (a writ install, accepting lost objects), or when it touches another agent's live claim (one writ cannot prove dead). A stale claim whose holder is proven gone now says the holder has exited and that any agent may run the fix. Pending edits on a live claim (`unsealed_at_risk`, and `left_out` files a live claim sealed or scoped, finding 118) are the holder's: the holder sees its seal, another agent sees who should seal them and no command. `writ doctor`, `writ context` and the doctor notice after seal and spec done print who may run each fix; the brief line counts the fixes any agent may run. Every generated instruction surface gains one step: when context or doctor shows a finding whose fix is not marked for a human, run that fix, including for findings about other agents' specs. A second run of such a fix exits 0 and says the state is already clear: `spec release --stale` on a released claim, `seal --paths` on files with no unsealed change. (findings 114, 118)

- A seal refused because a finish committed its spec prints exactly one command, with the refused seal's summary and paths filled in: `writ spec add "<title> (follow-up)" --id <spec>-followup --claim && writ seal -s "<summary>" --spec <spec>-followup --paths <paths>`. No choice is offered and nothing suggests reopening. The same refusal now covers a seal without `--spec` whose agent's newest spec was committed (before, it said "No active spec" and never named the committed spec). Pasting the command twice is safe: the second run says the state is already clear. The follow-up spec continues the committed one by its id. (findings 65, 121)
- Doctor's fix for pending work on a live claim is the holder's own seal onto its spec, not a new follow up spec; it is runnable for the holder and named for the holder or a human to everyone else. Doctor's own follow up spec, used only for files nobody holds, is `doctor-followup-<date>`, titled "follow up to writ doctor findings". (findings 119, 120)

- An override's recorded sentence now says whose call it was. "accepted <other>'s version" only when the other agent's seals removed the lines or their sealed lines replaced them; when the sealer replaced its own lines with its own content it says "removed N own line(s) on purpose after being told" and names other agents only as having touched the file; when both, it says both with counts ("accepted <other>'s version of <file> for N line(s) and removed M own line(s) on purpose after being told"). `writ finish`, `writ seal`, `writ handoff` and `writ why` use this one wording. Records written before 0.4.2 are described without naming an author. (finding 122)
- `writ spec done` after a finish committed the agent's spec exits 0, names the spec and the commit ("already committed ... nothing to close"), and keeps the summary on the spec as a post-commit note that `writ handoff` shows; it no longer says "no spec claimed". (finding 124)

### Added

- `writ handoff [--since <seal|time|last-finish>] [--format markdown|brief|json] [--survival]`: the agents that worked, one line per spec (title, seals, state, the committed spec it continues), every file more than one spec sealed and how it resolved (`kept`, `superseded`, `accepted_after_told`, `mixed`, `rewrote_own`, with the recorded override sentence), open stale rewrite notices, unsealed work and whose it is, the doctor result, and the one next action. Read only. The next action is doctor's: `writ doctor` gains `next_action` (JSON) and a `next:` line, computed once and shown by both, and it never says converge. Python `Repository.handoff()` / `handoff_text()`, MCP `writ_handoff`.
- A Claude Code SessionEnd hook, installed by `writ init -y` beside the SessionStart hook (same PATH-first writ resolution, silent, never fails the session's exit). It runs the hidden `writ session-end`, which records the host's session end under `.writ/agents/claudecode/ended/` with the claims the session held; `/clear` and `/resume` record nothing. `writ spec release --stale` and doctor's stale_claim check take a matching record as proof the holder is gone when the pid cannot be judged (no process table, another host); a live pid always wins, and doctor only reads records. A project with the start hook but no end hook reports `version_skew` until `writ init -y`.
- `writ why <file> [--format markdown|json]`: every seal of the file in order with its spec and agent, the lines each added and removed, any override in its recorded words, and the merge (finish commit and specs) that produced the current content. Read only. Python `Repository.why()`, MCP `writ_why`.

## [0.4.1] — 2026-10-12

### Fixed

- Committed specs' seal chains no longer count as diverged branches and no longer feed `integration_risk` (contention, scope violations, divergence); `converge-all` no longer offers them. A repository whose work is all committed reads risk low with no converge action. `next` defers to doctor: under a clean doctor, converge and review_risk are never shown at high priority (a blocking dependency still is). (finding 97)
- `writ spec release <id> --stale` releases another agent's claim only with proof its holder is gone: same host and the recorded pid has exited, or now belongs to a process with a different start time. A live holder, another host, or a claim without a recorded pid, start time and host is refused with the missing evidence named; `--stale` never acts as `--force`. Doctor's `stale_claim` fix is `--stale`; a claim writ cannot judge has no fix command and names the human's `--force`. No doctor fix command contains `--force`. A human `--force` release is recorded on the spec (`release_forced: true`) and as a `claim_force_released` event. Python `spec_release(..., stale=True)` and the MCP `writ_spec_release` `stale` parameter match. (finding 91)
- `writ seal` with no spec: an explicit `--agent` is refused exactly like an agent detected from the environment (finding 98); under `[security] claim_enforcement = "strict"` a human's seal is refused too and nothing is recorded (finding 100); the human warning and every refusal name the full fix, `writ spec add "brief description of your task" --claim` (finding 99).
- A seal refused for removing its spec's own sealed lines now says whether another agent worked on the file since your last seal of it (naming them) or nobody did, shows the seals that added the lines, and offers both choices: put them back, or accept the other version / remove them on purpose with the exact `--allow-removals` command. A seal that uses `--allow-removals` records `override_context` (path, other agents, lines, adding seals) and `writ seal` prints it. Another spec's removal excuses your line only if that spec's version saw the seal that added it (finding 105, seal-time half). `writ finish --accept-removals <seal>` accepts a seal's captured removals for the survival tier. (findings 102, 105)
- The seal-time own-line check also protects lines on files whose base came from a plain git commit (files that predate `writ init` or arrived outside writ): a spec's first seal of such a file is measured against the spec's genesis snapshot or the git commit it started from, not an empty file. Before, the check passed silently on those files. (finding 106)
- `writ doctor --survival` (on demand and on the watch interval) computes the merge `writ finish` would make in memory: it stores no object and writes neither `stale_rewrite_notices.json` nor `convergence_v3_pending.json`. Finish still records them when it runs. (finding 110)
- The survival audit no longer reports long-closed specs' work as lost. It reads removals from every spec's seal chain (it read only the main chain, so most later edits looked like losses), checks only distinctive lines (not blank, not text the file already had), counts a later seal of a since-committed spec as superseding a line, skips specs not yet committed when checking against git HEAD, and reports a sealed file that git ignores as such. Each superseded line names the seal, the spec and the reason. On writ's own store: 263 lost against HEAD before, 0 after. (finding 77)
- `writ doctor --survival` runs the survival tier: every line a spec sealed must still be there, or a later seal must have replaced it. Specs not yet committed are checked against the working tree, committed ones against HEAD; the check covers open specs and those sealed or committed since the last green result, and is cached until a seal, a spec record or an audited file changes. A lost line is a red `survival` finding naming the line, the path and the seal that added it; a sealed file git ignores is a yellow one naming the ignore rule. writ refuses once and explains; if you confirm the removal after that (`--allow-removals`), writ records your confirmation and does not second guess it. The one exception it reports, as a yellow note that never blocks: the confirmed seal recorded another spec's sealed version of the file in place of your lines; `writ finish --accept-removals <seal>` confirms that too. If the other agent never sealed its version, writ has no record of it, and your confirmed deletion stands. `writ finish` runs it on the tree it will commit and refuses before staging anything, listing each lost line and its seal (`finish_refused`, reason `survival_check`), unless `--force`. `writ watch` reruns it when the store changes. The headline reads `fast checks clean; survival green as of <seal>`, and says `safe to finish` only when both tiers are green on the current state; `survival_last_green` is populated.

## [0.4.0] — 2026-10-06

### Added

- `writ doctor` runs the fast tier: six checks with stable ids (`store_integrity`, `stale_claim`, `committed_spec_seal`, `unsealed_at_risk`, `version_skew`, `left_out`). Each finding names what is wrong, the paths involved, and the exact command that clears it when pasted. `--format json` for scripts; exit 0 when clean, 1 on any finding. The first line always names the tier that ran and the survival state; on a clean repository it reads `fast checks clean; survival check not available until 0.4.1`.
- `writ finish` runs doctor first and refuses on a red finding unless `--force`. `writ watch` runs doctor every interval and reports when the findings change. `writ context --format brief` carries one `doctor:` line.
- Every finish refusal (doctor, build check, survival check, unresolved convergence, `--strict`) is recorded as a `finish_refused` event in `.writ/security/events.jsonl` with the reason, specs and files.
- `writ repair` also repairs the `.writ` layout: missing directories, `version.toml`, the main HEAD and index (rebuilt from the newest seal), an unparseable `config.toml` (moved aside), and unparseable seal or spec records (moved to `.writ/quarantine/`, never deleted). For objects no source can regenerate it prints `writ doctor --allow-missing <prefix>...`, which records the loss as accepted under `[doctor] allow_missing`.
- `[doctor]` settings in `.writ/config.toml`: `stale_claim_minutes` (default 120), `unsealed_minutes` (default 30), `allow_missing`.
- Claims record the claiming session's process and host, so doctor can tell a claim whose holder has exited.
- Python: `Repository.doctor()` returns the fast-tier report (it returned the 0.3 layout checks).

### Fixed

- `writ finish` no longer lists files identical to git HEAD as left out.

## [0.3.0] — 2026-10-05

Two agents can now share one directory: each seal takes only its own spec's files, closing a spec never sweeps another agent's work, and `writ finish` commits only what was sealed.

### Fixed

- Re-running `writ init` on an existing project is a refresh: it rewrites writ's own CLAUDE.md block, skills, SessionStart hook and settings instruction from the current template (keeping your other hooks, instructions, permissions and keys) and never seals. A moved git HEAD is reported with `writ bridge import` instead of being re-imported. An unparseable `.claude/settings.json` is reported and left unchanged instead of being replaced.
- The generated skills teach the same `writ seal -s "..." --paths <files>` and `writ spec add "..." --scope "..."` forms as the CLAUDE.md block.
- `--scope "app.py,models.py,tests/*"` is three entries, as agents write it; it was stored as one literal pattern that matched nothing (the repeated flag still works, and specs stored the old way match per item).
- The SessionStart hook, the CLAUDE.md and AGENTS.md blocks, `.writ/AGENT_INSTRUCTIONS.md`, the settings instruction and the generated skills now render the workflow from one source, so the hook no longer contradicts the block on claims or `--scope`.
- `writ spec done` without `-s` uses the spec title as the summary, so finish output and commit messages never show "(no summary)"; the templates show `writ spec done -s "<what you did>"`.
- **Closing a spec could lose that spec's own edits to a shared file, and record another agent's edit as its own.** `writ spec done` sealed every file on disk, so if another spec's version of a shared file was there at that moment, the closing seal recorded it and the spec's earlier edit vanished from its history; convergence and `writ finish` then committed the file without it and reported no conflict. The loss happened when the spec closed, not during the merge. It affected workspaces, where specs start from a common base, and shared directories when an agent rewrote a file from a stale copy; specs in one directory whose later seals already held the earlier edits were not affected. We found it by rebuilding each spec's sealed content and checking that every added line survived into the commit; writ now runs that check itself (see Survival checks under Added). `writ spec done` now seals only the spec's own files.
- In a shared directory, convergence could add back lines that a later spec had deliberately removed, including code another spec moved. Convergence now recognizes when one version was written on top of another and merges only genuinely concurrent versions.
- The escalate strategy no longer settles a conflict by keeping the longer version; escalated files report no merged hash.
- `writ finish --auto` and `writ finish --accept` honor `--strategy`; they previously ignored it and committed as `single`.
- Diff alignment above 10,000 lines is correct again.
- Convergence no longer writes unreferenced record objects into the store, and pending merge results are protected from garbage collection until they are written out.
- `writ init --bare` now adds `.writ/` to `.gitignore`, so the store is never committed by `git add`.

### Changed

- Five behavior changes for single agent users:
  - (a) A stale claim held by another agent id forces `--paths` on `writ seal`. Release it with `writ spec release <id>` (`--force` when you are not the holder).
  - (b) `writ spec done` without pending files of its own makes no seal; it closes the spec and says so.
  - (c) `writ finish` no longer commits unsealed edits by default. It stages the sealed content of completed specs and lists what it leaves out; `--include-unsealed` restores the old behavior.
  - (d) `writ seal` no longer silently indexes untouched pending files; files it does not capture stay pending for their owner.
  - (e) Python and MCP `seal()` default to status `in-progress`; a seal no longer completes its spec unless asked.
- `writ seal` and `writ spec done` refuse a spec that is already committed to git (also in the Python and MCP bindings). Finish never staged such a seal and the spec could not be reopened, so the file stayed one version behind; the error names the commit and gives the `writ spec add "..." --claim` command for a new spec. Archiving a committed spec is unchanged.
- Nested `.gitignore` files and the global git excludes file (`core.excludesFile`) are honored like git.
- `writ init` never rewrites a committed `.mcp.json` and `writ uninit` never deletes one. For an untracked `.mcp.json`, init adds writ's server and uninit removes only that entry; other servers are kept. The writ repository ships `.mcp.json`, so Claude Code picks up writ's MCP tools on clone.
- The repository schema is now version 3. Writ 0.3.0 upgrades a repo on first open, after which writ 0.2.x refuses it with "please update writ": 0.2.x cannot read spec records written by 0.3.0. Upgrade every machine and CI job that touches the repo together. From 0.3.0 on, an older writ opening a newer repository stops before reading any record with "this repository uses writ schema vN; upgrade to X or newer", and spec, index and bridge records ignore fields they do not know instead of failing (seal records stay strict).
- `writ finish` runs `cargo check` on the staged tree before every commit in a Cargo project and refuses to commit a tree that does not compile; `--no-check` or `[workflow] finish_check = false` turns it off.
- `--strategy per-spec` needs specs that compile independently; when every spec touches the same core files, nothing is committed and the default `single` strategy is the right choice.
- `writ spec add` no longer claims the spec unless `--claim` is passed.
- `writ finish` never cancels or archives specs; `--archive-unclaimed` does.
- Seals respect claims: sealing to a spec another agent holds warns with the holder named, and is refused under `[security] claim_enforcement = "strict"`. The existing `[security] scope_enforcement` key is now read.
- Existing projects get the new generated instructions, hook and skills only when `writ init` is run again in the project. It refreshes the managed CLAUDE.md block, the hook and the skills, and leaves the store and specs untouched.
- Writ protects sealed work, not unsealed work. Until an agent's first seal, another agent can still overwrite its edits on disk. Seal early.

### Added

- Survival checks at seal time: a seal or `writ spec done` that would remove a line the same spec added earlier is refused, naming the file, the lines and the command to proceed; `--allow-removals <path>` lifts it for one file.
- Survival checks at merge time: `writ finish` and `writ converge-all` check that every spec's sealed additions survive the merge or are covered by a reported conflict; a loss is escalated and nothing is staged. When a later version was written from a stale copy, lines it never saw are kept and a stale rewrite notice names them in the finish output, in the agent's next `writ context` under `stale_rewrite_notices`, and in `.writ/stale_rewrite_notices.json`.
- `writ spec release <id> [--force]` releases a claim; also in Python and MCP.
- `writ spec add --scope <glob>` declares a spec's files up front; a seal without `--paths` takes only files its spec owns.
- `writ repair` regenerates referenced but missing objects from the working tree or git history, verifying every hash before writing; `--dry-run` reports what a run would recover. Available from Python as `Repository.repair(dry_run)`.
- Faster and smaller: the working tree is scanned once per `writ context` call with a size, mtime and inode cache (275 ms to 91 ms on the writ repository), and a linear space diff replaces the quadratic table for diff and three way merge (a 34,000 line file converges in 0.06 s and 34 MB instead of about 10 s and 9 GB; the raspberry_pi profile no longer runs out of memory on large files).
- Status truth: the `writ status` agent column shows the claim holder, every per spec count comes from that spec's own seals, and `writ spec show --format json` emits JSON.
- One agent identity resolver for the CLI, MCP and Python: `--agent`, then `WRIT_AGENT_ID`, then `default_agent`, then the framework session, then `human`. Each Claude Code session gets its own stable id.

---

## [0.2.1] — 2026-10-05

**On 0.2.0, do not run `writ gc run` or `writ finish` until you upgrade.**

### Fixed

- The garbage collector, including the automatic run inside `writ finish`, treated every object carried forward unchanged as garbage. `writ gc run` could delete file contents writ still needs (every file from a git baseline import), and the automatic run inside `writ finish` could remove committed seals and their file contents seven days after a spec was committed. `writ verify` now reports referenced but missing objects, and `gc run` refuses to proceed when any are missing (`--force` overrides).

---

## [0.2.0] — 2026-10-04

### Added

**Convergence v2 (Six Phase Pipeline)**
- Structural diff engine: decomposes files into semantic units (imports, definitions, statements)
- Language aware analyzers for Python, Rust, Go, TypeScript, and JavaScript with generic fallback
- Classification phase: `BothModified`, `DeleteVsModify`, `BothAdded`, `OneAdded`, `Identical`
- Five deterministic resolution patterns: ImportAccumulation (0.95), NonOverlappingDefinitions (0.92), EofAppend (0.92), AdditiveComposition (0.88), SupersetContainment (0.82)
- Dynamic confidence scoring: larger merges receive proportionally more cautious scores
- Phase 4 (spec aware resolution) and Phase 5 (LLM assisted resolution) implemented and feature flagged off
- HardenedVerifier (Phase 6): duplicate definition detection, balanced delimiter checking, content loss detection, conflict marker scanning
- Content traceability: every line in merged output must trace back to an input; novel content is rejected
- Optimized N-agent merge ordering: greedy overlap minimizing algorithm merges disjoint specs first
- Trust adjusted confidence: agent trust levels affect merge confidence scoring
- PropTest integration: three invariant tests running 512 cases each

**Security (Sprints A, B, C)**
- Cryptographic seal integrity: BLAKE3 content hashes, parent seal hashes, chain hashes on every seal
- Ed25519 digital signatures for seal authentication
- `writ verify --chain` validates full seal chain from genesis to HEAD
- `writ verify --seal <id>` verifies individual seal content hash, chain linkage, and signature
- Convergence keypair generated on `writ init`, stored in `.writ/keys/` with AES-GCM encryption and 0600 file permissions
- Agent identity system: `RegisteredAgent` with trust levels (full, standard, restricted, untrusted)
- Agent management: register, suspend, revoke agents via CLI (`writ agent register`)
- Scope enforcement: configurable per agent file scope constraints with warning and enforce modes
- Security event monitoring: append only audit log with severity classification (info, warning, critical)
- Event filtering by severity and event type via `writ security events`
- Events tracked: scope violations, chain hash failures, authentication failures, agent revocations, convergence low confidence, unrecognized agents

**Garbage Collection and Lifecycle Management**
- Spec lifecycle state machine: active, stale, completed, cancelled, archived
- `writ spec complete <id>` and `writ spec cancel <id>` lifecycle transitions
- Stale spec detection: `writ context` warns when specs have no recent activity
- GC plan generation with three layer safety rules (seals never deleted)
- GC execution with tombstones and audit records for every cleanup action
- Four deployment profiles: raspberry-pi (500MB), development (5GB), production (100GB), enterprise (unlimited)
- `writ gc status`, `writ gc run`, `writ gc run --dry-run`, `writ gc storage`, `writ gc log`
- Storage pressure monitoring: warns via security events when usage approaches configured budgets
- Python bindings: `gc_status()`, `gc_dry_run()`, `gc()`, `cancel_spec()`, `complete_spec()`, `storage_report()`

**Storage and Compression**
- zstd compression on stored objects with magic byte format (0x00=raw, 0x01=zstd, 0x02=dict-future)
- Streaming decompression bomb protection with configurable size limits and security event emission
- Per profile compression levels (RPi=1, dev=3, prod=3, enterprise=6)
- Backward compatible: old repositories work without migration
- Compression statistics tracking via `CompressionStats` struct
- Object pruning with reachability analysis and flagged seal safety interlock
- Opportunistic recompression of legacy uncompressed objects

**Test Framework**
- Layer 1: Extracted shared `test_utils.rs`, 33 dedicated diff3 tests, 3 proptest invariant tests. Total: 1,350+ Rust tests.
- Layer 2: `ScenarioBuilder` fluent API for integration tests. 9 convergence scenarios.
- Layer 3: Python contract tests. 29 convergence, 33 security, 51 GC tests. Total: 400+ Python tests.
- Layer 4: YAML scenario runner with timing support. 41 scenarios: 23 convergence, 7 negative, 7 scale, 2 security, 2 e2e. Scale scenarios up to 100 agents.
- Layer 5: Live agent test run framework. TR22: 3 agents, 20 checks, scripted and live modes.

**Upgrade and Migration**
- Schema versioning: `.writ/version.toml` tracks on-disk format version separately from binary version
- Auto-migration on open: repos created before versioning (schema v0) are silently migrated to v1
- Migration runner: sequential, idempotent, with backup before each migration step
- `writ doctor`: read only health check command with 8 checks (version file, schema, directories, index, config, master key, specs, seals)
- `writ doctor --json` for machine readable output
- Clear error when opening a repo created by a newer version of writ
- v0 to v1 migration: creates version.toml, ensures all expected directories, creates HEAD if missing, migrates legacy settings.json to config.toml

**Packaging**
- `pip install writ-vcs` now bundles the `writ` CLI binary via PEP 427 `.data/scripts/`
- Single package install puts both Python API and CLI on PATH

**Documentation Site**
- mdbook based documentation site (`book/` directory)
- Introduction, installation, quickstart, and first convergence walkthrough
- Concepts: seals vs commits, specs and agents, convergence pipeline, security model
- Full CLI reference with all commands and options
- Python SDK reference
- Troubleshooting guide
- CONTRIBUTING.md with development setup and contribution guidelines

**MCP Server (Rust Native)**
- `writ mcp-serve`: native MCP server built in Rust via `rmcp` crate. Ships as part of the `writ` binary — no Python runtime, no separate install
- 21 MCP tools matching the full CLI: context, seal, spec management, status, diff, log, finish, converge, restore, verify, doctor, workspace management
- CLI passthrough architecture: each MCP tool calls the `writ` CLI via subprocess. Same behavior, same output, same enforcement
- `writ mcp-install`: generates `.mcp.json` for Claude Code project integration. Commit to git for zero setup team adoption
- `writ mcp-install --desktop`: generates config for Claude Desktop
- `.mcp.json` auto-generated during `writ init` when Claude Code is detected
- Schema level enforcement: `writ_seal` requires `spec` parameter (C.13), `writ_context` defaults to TOON and writes context token (C.14)

**Slash Commands (Claude Code)**
- 20 slash commands generated by `writ init` in `.claude/commands/`
- Core workflow: `/writ-context`, `/writ-seal`, `/writ-spec-add`, `/writ-spec-done`
- Status and review: `/writ-status`, `/writ-diff`, `/writ-log`, `/writ-show`
- Spec management: `/writ-spec-status`, `/writ-spec-show`, `/writ-spec-reopen`
- Round trip: `/writ-finish`, `/writ-summary`
- Recovery and convergence: `/writ-restore`, `/writ-converge`
- Diagnostics: `/writ-verify`, `/writ-doctor`
- Each command is a thin wrapper around the CLI with accurate flag documentation
- `writ uninit` removes only `writ-*.md` files from `.claude/commands/`, preserving non-writ commands

**Workspaces**
- `writ workspace create <name>`: isolated parallel environments for agent teams. Each workspace gets its own directory, index, HEAD, and file state while sharing the same object store, seal chain, and specs
- `writ workspace list`: overview of all workspaces with paths, spec counts, and completion status
- `writ workspace status [name]`: detailed workspace view with spec progress and seal counts
- `writ workspace delete <name>`: removes workspace state and parallel directory. Seals, specs, and objects preserved in shared store
- `writ spec assign <id> --workspace <name>`: scope spec visibility to a workspace (visible in that workspace and main)
- `writ spec unassign <id>`: remove workspace assignment, making spec globally visible
- Scoped context: `writ context` inside a workspace returns only workspace relevant specs, seals, files, and agent activity
- Cross workspace convergence: `writ converge-workspaces a b` merges workspace file states through the existing convergence engine
- `.writ-workspace` pointer file in parallel directories links back to main project's `.writ/` directory
- All writ commands work from workspace directories automatically — no special flags needed
- 4 new MCP tools: `writ_workspace_create`, `writ_workspace_list`, `writ_workspace_status`, `writ_workspace_delete`
- 3 new slash commands: `/writ-workspace-create`, `/writ-workspace-list`, `/writ-workspace-status`

**Spec-Scoped Sealing**
- `writ seal --spec X` captures only the files that changed since this agent's last seal for spec X. Multiple agents work in the same directory without cross-contamination.
- Per-agent, per-spec baselines: each (agent, spec) pair maintains its own baseline state
- Genesis index: `writ spec add` snapshots the current file index for use as the first seal baseline
- Auto-claiming: first `writ seal --spec X` auto-claims unclaimed spec X for this agent
- Without `--spec`, seal captures the full working directory (backward compatible)

**Writ Watch (Convergence Daemon)**
- `writ watch`: long-running process that monitors for new seals and auto-converges overlapping changes in real time
- Detects overlapping seals from different specs touching the same file, runs convergence automatically
- Terminal mode (default): real-time output showing seal detection, convergence, and conflicts
- Daemon mode (`--daemon`): background process with PID file and log output
- Configuration via `.writ/config.toml` `[watch]` section: interval, auto_converge, max_retries
- Conflict recording: unresolvable overlaps stored in `.writ/conflicts/` and surfaced via `writ status`

**Writ Plan (Batch Task Definition)**
- `writ plan "task1" "task2"`: batch spec creation from inline arguments
- `writ plan -f tasks.txt`: one task per line from file
- Stdin support: `cat tasks.txt | writ plan`
- Titles auto-slugified to spec IDs (e.g. "Implement OAuth2 auth" becomes `implement-oauth2-auth`)

**Spec Claiming**
- `writ spec claim <id>`: explicitly claim an unclaimed spec for the current agent
- Unclaimed specs visible in `writ context` output for agent discovery
- First-claim-wins: second attempt returns actionable error with claiming agent's ID
- Auto-claim on first `writ seal --spec X` if spec is unclaimed

**Agent Adoption Enforcement**
- C.13: `writ seal` without `--spec` in agent context (env var detected) returns exit 1 with actionable error. Human context warns but allows.
- C.14: `writ context` writes timestamp to `.writ/.context_token`. `writ seal` checks freshness (4h window) and warns agents if stale or missing. Warning only, never blocks.

**CLI Naming**
- `writ uninit` replaces `writ uninstall` for removing writ from a project. `writ uninstall` remains as a hidden deprecated alias that prints a notice and delegates to `uninit`.
- `--keep-writignore` flag preserves `.writignore` during uninit
- `--format json` support for machine readable uninit output

### Changed

- Linear diff fallback for large files (10k+ lines) uses O(n) algorithm instead of O(n^2) LCS
- `repo.rs` refactored: convergence engine extracted into `convergence/` module (~10K LOC)
- Convergence conflict reports use structured JSON instead of `<<<<<<<` markers
- Test directories restructured for Layer 1-5 framework

### Optimized

- Context output reduced by 26% (2,283 to 1,685 tokens). Empty spec fields, default enum values, and redundant seal paths are now omitted. Writ delivers 2.5x more information than equivalent git commands at 25% better token efficiency per capability.
- Adaptive context output: empty sections (integration risk, scope violations, diverged branches) are omitted entirely when they carry no information. Output scales with complexity, not a fixed schema.
- Token benchmarks added to CI (F.14b, F.14c): tiktoken cl100k_base verification of all format efficiency claims. Anthropic API script for ground truth Claude token counts.

---

## [0.1.0] — 2026-02-21

Initial public release. AI native version control for agentic systems.

### Added

**Core**
- Content addressable object store (SHA-256) with atomic writes and hash verification on retrieve
- Seals: structured checkpoints with agent identity, spec linkage, verification metadata, and status lifecycle
- Specs: structured requirements with status, dependencies, file scope, and acceptance criteria
- Index tracking with content level diff engine
- Advisory file locking (`flock(2)`) for safe concurrent multi-agent sealing
- Path traversal protection, input sanitization, and hash validation

**Context**
- `writ context`: structured state for agents with specs, seals, working state, agent activity, file contention, integration risk, diverged branches, scope violations, session status
- Spec scoped context filtering
- Integration risk scoring (low/medium/high, 0 to 100)
- File contention map: files touched by 2+ agents, sorted by risk
- Agent activity tracking with per agent file ownership
- Ghost work detection: warns when a seal has 0 file changes

**Convergence**
- Three way merge engine with LCS based edit operations
- `converge-all`: merge all diverged branches in sequence
- `MostRecent` and `MostComplete` strategies
- Post-convergence quality reports with per file decisions and quality scoring
- Structured JSON conflict reports

**Init and Workflow**
- `writ init`: guided interactive setup with environment scanning, git detection, agent framework integration, and output format selection
- `writ init --yes`: non-interactive mode for CI and scripting
- `writ init --spec`: optional spec creation during init
- Agent framework auto detection and configuration for Claude Code, Codex, and generic agents
- `writ install` retained as deprecated alias (prints notice, calls `writ init`)

**Summary and Round Trip**
- `writ summary --format commit|pr|human|json`
- `writ finish`: one command round trip (summary, git add, git commit)

**Restore**
- `writ restore SEAL_ID`: restore working directory to any seal's state
- Immutable history preserved through all operations

**Git Bridge**
- `writ bridge import`: import git working tree as baseline seal
- `writ bridge export`: export seals as git commits with metadata trailers

**Remote Sync**
- `writ push` / `writ pull` for distributed workflows

**Python SDK** (`pip install writ-vcs`)
- `writ.Repository`: open, init, install, seal, context, summary, converge, restore, log, state, diff
- `writ.sdk.Agent`, `writ.sdk.Phase`, `writ.sdk.Pipeline` for orchestrated workflows

**CLI**
- Full command set: install, seal, context, log, summary, finish, converge, converge-all, spec, state, diff, show, restore, bridge, push, pull
- `--format` support (json, human) on most commands

**CI/CD**
- GitHub Actions: Rust + Python test matrix on Ubuntu and macOS
- Release workflow: multi platform wheel builds, PyPI trusted publishing, CLI binary releases

**Testing**
- 537 tests (306 Rust + 231 Python) covering core, CLI, bindings, convergence, install, and workflows
