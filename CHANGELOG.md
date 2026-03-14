# Changelog

All notable changes to writ will be documented in this file.

Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
