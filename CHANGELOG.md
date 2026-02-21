# Changelog

All notable changes to writ-vcs will be documented in this file.

Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] — 2026-02-21

Initial public release. AI-native version control for agentic systems.

### Added

**Core**
- Content-addressable object store (SHA-256) with atomic writes and hash verification on retrieve
- Seals — structured checkpoints with agent identity, spec linkage, verification metadata, and status lifecycle
- Specs — structured requirements with status, dependencies, file scope, and acceptance criteria
- Index tracking with content-level diff engine
- Advisory file locking (`flock(2)`) for safe concurrent multi-agent sealing
- Path traversal protection, input sanitization, and hash validation

**Context**
- `writ context` — one-call structured state for agents: specs, seals, working state, agent activity, file contention, integration risk, diverged branches, scope violations, session status
- Spec-scoped context filtering
- Integration risk scoring (low/medium/high, 0-100) from divergence, contention, and scope violations
- File contention map — files touched by 2+ agents, sorted by risk
- Agent activity tracking with per-agent file ownership across all branches
- Ghost work detection — warns when a seal has 0 file changes

**Convergence**
- Three-way merge engine with LCS-based edit operations
- `converge-all` — merge all diverged branches in sequence
- `MostRecent` strategy — resolves conflicts by preferring the most recently sealed version
- `MostComplete` strategy — resolves conflicts by preferring the version with more content
- Post-convergence quality reports with per-file decisions, consistency checks, and quality scoring
- Lost-content warnings when conflict resolution discards data
- Structured JSON conflict reports (no `<<<<` markers)
- Pre/post convergence integration risk delta reporting

**Install and Workflow**
- `writ install` — one-command setup: init, `.writignore`, git detection, bridge import, framework hooks
- `writ install --spec` — optional spec creation during install for zero-friction setup
- Idempotent install with re-import on git HEAD change
- Git dirty state detection and reporting
- Agent framework auto-detection and hook installation for Claude Code (`CLAUDE.md`, slash commands) and Codex (`AGENTS.md`)
- Framework hooks include restore/rollback guidance, round-trip commands, and convergence strategies

**Summary and Round-Trip**
- `writ summary --format commit` — one-line commit message with full provenance
- `writ summary --format pr` — full PR description with spec/agent breakdown
- `writ summary --format human` — detailed session overview
- `writ summary --format json` — machine-readable output
- Auto-generated `.writ/summary.json` and `.writ/summary.txt` when all specs complete
- Post-convergence summary preserves all specs and agents via merged-heads archiving

**Restore**
- `writ restore SEAL_ID` — rewind working directory to any seal's state
- Immutable history — restoring doesn't delete previous seals
- Content-addressable store ensures all file versions remain retrievable

**Git Bridge**
- `writ bridge import` — import git working tree as baseline seal
- `writ bridge export` — export seals as git commits with metadata trailers
- Bridge import attribution fix — first agent after import only gets its own changes

**Remote Sync**
- `writ push` / `writ pull` for distributed workflows

**Python SDK** (`pip install writ-vcs`)
- `writ.Repository` — open, init, install, seal, context, summary, converge, restore, log, state, diff
- `writ.sdk.Agent` — context manager for agent workflows with auto-sealing
- `writ.sdk.Phase` — multi-step workflow phases
- `writ.sdk.Pipeline` — orchestrated multi-phase workflows with auto-summary

**CLI**
- Full command set: install, seal, context, log, summary, finish, converge, converge-all, spec, state, diff, show, restore, bridge, push, pull
- `writ finish` — one-command round-trip: generates summary, runs `git add .`, commits with writ-generated message. Supports `--full` (PR-style body) and `--dry-run` (preview)
- `--format` support (json, human) on most commands
- Convergence strategy selection with validation

**CI/CD**
- GitHub Actions: Rust + Python test matrix on Ubuntu and macOS
- Release workflow: multi-platform wheel builds (Linux x86/arm64, macOS x86/arm64, Windows x86), PyPI trusted publishing, CLI binary releases

**Testing**
- 537 tests (306 Rust + 231 Python) covering core, CLI, bindings, convergence, install, and casual user workflows
