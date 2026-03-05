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

**Documentation Site**
- mdbook based documentation site (`book/` directory)
- Introduction, installation, quickstart, and first convergence walkthrough
- Concepts: seals vs commits, specs and agents, convergence pipeline, security model
- Full CLI reference with all commands and options
- Python SDK reference
- Troubleshooting guide
- CONTRIBUTING.md with development setup and contribution guidelines

### Changed

- Linear diff fallback for large files (10k+ lines) uses O(n) algorithm instead of O(n^2) LCS
- `repo.rs` refactored: convergence engine extracted into `convergence/` module (~10K LOC)
- Convergence conflict reports use structured JSON instead of `<<<<<<<` markers
- Test directories restructured for Layer 1-5 framework

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
