# Changelog

All notable changes to writ will be documented in this file.

Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.0] — Unreleased

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
