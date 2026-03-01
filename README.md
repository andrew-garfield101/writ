# writ

**AI-native version control for agentic systems.**

[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue)](LICENSE)
[![CI](https://github.com/andrew-garfield101/writ/actions/workflows/ci.yml/badge.svg)](https://github.com/andrew-garfield101/writ/actions/workflows/ci.yml)

> [!WARNING]
> Writ is in early alpha. The core is stable and thoroughly tested (1,350+ Rust tests, 400+ Python tests, 41 YAML scenario tests), but the API may change before 1.0. Use it, break it, file issues.

---

Writ is a version control system designed from the ground up for LLMs and multi-agent development fleets. Its core primitives are **specs** (not branches), **seals** (not commits), and **convergence** (not merging). It works alongside git, not instead of it.

**One-call context.** A single `writ context` returns everything an agent needs — specs, seals, working state, file contention, integration risk — in structured JSON. No parsing `git log` output. No 4-5 tool calls to build situational awareness.

**Semantic convergence.** When multiple agents touch the same files, writ merges *meaning*, not lines. Language-aware analyzers decompose code structurally, compose additive changes automatically, and escalate real conflicts with full context. The core principle: compose, don't choose.

**Cryptographic integrity.** BLAKE3 hash chains and Ed25519 signatures on every seal. Agent identity with trust levels and scope enforcement. Content traceability ensures no line in merged output appears from thin air. Tamper with any checkpoint and the chain breaks.

**Built for any scale.** Deployment profiles from a 500MB Raspberry Pi to unlimited enterprise. Lifecycle management and garbage collection keep repositories clean without ever compromising the immutable seal history.

> *"One `writ context` call and I know who did what, which specs are complete, where branches diverged, and what files are contested. That is genuinely valuable and unlike anything available in git alone."*
>
> — AAIS_8, orchestrator agent reviewing a 14-agent, 40-seal project

## Table of Contents

- [Install](#install)
- [60-Second Workflow](#60-second-workflow)
- [Why Not Just Git?](#why-not-just-git)
- [Context](#context)
- [Multi-Agent Workflow](#multi-agent-workflow)
- [Convergence](#convergence)
- [Going Back](#going-back)
- [Security](#security)
- [Lifecycle and Storage](#lifecycle-and-storage)
- [`writ install`](#writ-install)
- [Python SDK](#python-sdk)
- [CLI Reference](#cli-reference)
- [Architecture](#architecture)
- [Building from Source](#building-from-source)
- [Roadmap](#roadmap)
- [Contributing](#contributing)
- [License](#license)

## Install

```bash
pip install writ-vcs
```

Or build from source:

```bash
cargo install --path crates/writ-cli
```

## 60-Second Workflow

```bash
# Set up writ in any project (works with or without git)
writ install

# Agents seal checkpoints as they work
writ seal -s "added auth module" --agent implementer --spec auth
writ seal -s "tests passing" --agent tester --spec auth --tests-passed 42 --status complete

# One call gives agents everything they need: specs, seals, state, risk
writ context --format json

# When done, commit everything in one shot
writ finish

# Or manually: generate the commit message from the full session history
git commit -m "$(writ summary --format commit)"

# Or a detailed PR description
gh pr create --body "$(writ summary --format pr)"
```

That's it. Human checks out a branch, agents work in writ, human gets a commit with full provenance.

```
 Human world                Agent world                  Human world
┌──────────┐  writ install  ┌─────────────────┐  writ finish   ┌──────────────┐
│ git repo │ ──────────────→│ agents work in  │ ─────────────→ │ git commit   │
│ (branch) │                │ writ: specs,    │                │ with full    │
│          │                │ seals, context  │                │ provenance   │
└──────────┘                └─────────────────┘                └──────────────┘
```

## Why Not Just Git?

Git's data model was built for humans. Commits carry no structured metadata about which task they serve, which agent made them, or whether tests passed. You can bolt conventions on top, but conventions are things some agents follow and others don't.

Writ puts agent-native metadata inside the VCS:

| Git | Writ | What Changes |
|-----|------|-------------|
| Branch | **Spec** | Structured requirement with status, dependencies, file scope, acceptance criteria |
| Commit | **Seal** | Checkpoint with agent identity, spec linkage, verification, status lifecycle |
| `git status` | `writ context` | One call returns everything an agent needs — not text to parse |
| `git merge` | `writ converge-all` | Multi-branch convergence with strategy selection and structured conflict reports |
| `git checkout <ref>` | `writ restore` | Restore working directory to any seal — every seal is an immutable snapshot |
| (nothing) | **Integration risk** | Automatic risk scoring from divergence, contention, and scope violations |
| (nothing) | **File contention** | Which files are touched by which agents, sorted by risk |
| (nothing) | **Session summary** | Auto-generated commit messages and PR descriptions from seal history |
| `git verify-commit` | `writ verify --chain` | BLAKE3 hash chains + Ed25519 signatures on every seal — tamper-evident by default |

## Context

The most expensive thing in an agent's workflow is building situational awareness. With git, that means multiple tool calls — `git log`, `git diff`, reading files — each returning text that needs parsing. With writ:

```python
ctx = repo.context(spec="auth-migration")
```

One call. One structured dict. Everything an agent needs:

- **Spec details** — title, description, status, dependencies, file scope, acceptance criteria
- **Recent seals** — who did what, when, with which files and verification results
- **Working state** — new/modified/deleted files filtered to your spec's scope
- **Agent activity** — which agents own which files, their latest work
- **File contention** — "hot files" touched by 2+ agents, sorted by risk
- **Integration risk** — level (low/medium/high), score (0-100), contributing factors
- **Diverged branches** — specs with unmerged work, with convergence recommendations
- **Scope violations** — seals that touched files outside their spec's declared scope
- **Session status** — whether all specs are complete, with inline summary

## Multi-Agent Workflow

Three agents, different specs, working concurrently. Sealing is serialized via advisory file locks, so agents queue safely.

```python
# Agent A: auth migration
repo.seal(summary="token refresh", agent_id="auth-dev", spec_id="auth-migration")

# Agent B: payments (concurrent, different spec)
repo.seal(summary="stripe integration", agent_id="pay-dev", spec_id="payments", status="complete")

# Agent C: testing (concurrent, cross-cutting)
repo.seal(summary="42 tests passing", agent_id="test-bot", spec_id="test-suite", tests_passed=42)
```

The human checks in:

```bash
$ writ spec status
  > auth-migration       InProgress  (3 seal(s))
  v payment-refactor     Complete    (5 seal(s))
    db-optimization      Pending     (0 seal(s))
```

Full transparency. No branch archaeology. No parsing commit messages to figure out which agent did what.

## Convergence

The most complex problem in multi-agent development isn't writing code — it's merging it. When five agents work concurrently on overlapping files, traditional line-based merging falls apart. Writ merges *meaning*, not lines.

The convergence engine understands code structurally — it knows the difference between an import, a function definition, and a statement. When two agents both add imports to the same file, writ doesn't see a "conflict" — it sees two additive changes and composes them. When two agents modify the same function body differently, writ knows that's a real conflict and escalates it with full context.

Five deterministic resolution patterns handle the common cases:

| Pattern | What It Resolves |
|---------|-----------------|
| **Import Accumulation** | Both sides add imports — union them, deduplicate across languages |
| **Non-Overlapping Definitions** | Both sides add functions with different names — compose them |
| **EOF Append** | Both sides append to end of file — concatenate |
| **Additive Composition** | Both sides preserved base and added content — compose |
| **Superset Containment** | One side contains everything the other has — use the superset |

Every resolution is confidence-scored. High confidence (≥ 0.85) auto-resolves. Low confidence escalates with structured context for human or orchestrator review. Conflicts are structured JSON — not `<<<<` markers — so orchestrator agents can resolve them programmatically.

The resolution pipeline is layered and auditable. Spec-aware resolution uses writ's first-class spec and seal metadata — file scope, acceptance criteria, design notes — to make informed decisions that no other VCS can make. Post-merge verification catches structural damage automatically — duplicate definitions, unbalanced delimiters, content loss, leftover conflict markers — before bad merges reach the working tree. Content traceability ensures every line in merged output traces back to an input.

Merge ordering is optimized automatically: specs that touch disjoint files merge first, minimizing conflict complexity for the overlapping cases that follow.

```bash
# Merge ALL diverged branches at once — escalate what can't be auto-resolved
writ converge-all --apply --strategy escalate
```

```python
report = repo.converge_all(strategy="escalate", apply=True)
print(f"Merged {len(report['merge_order'])} branches")
print(f"Auto-merged: {report['total_auto_merged']}, Resolved: {report['total_resolutions']}")

if report.get("quality_report"):
    qr = report["quality_report"]
    print(f"Quality score: {qr['quality_score']}/100 — {qr['summary']}")
```

### Integration Risk

Before starting work or after convergence, check the risk level:

```bash
writ context --format human
# INTEGRATION RISK: HIGH (score: 65)
#   - 7 diverged branches (>3)
#   - file touched by 11 agents (>=5)
#   - 6 scope violations (>5)
```

## Going Back

Something broke. An agent went off the rails. A convergence produced garbage. You need to undo.

Every seal is an immutable snapshot of the entire working directory, and you can restore to any of them:

```bash
# See the full history — find the seal before things went wrong
writ log --all

# Inspect a specific seal to confirm it's the right one
writ show a3f8b2 --diff

# Restore the working directory to that seal's state
writ restore a3f8b2
```

Agents can do the same thing programmatically. If an agent detects that something went wrong — tests failing, scope violations piling up — it can walk the seal history and self-correct:

```python
seals = repo.log(limit=10)
for s in seals:
    if s["verification"].get("tests_passed", 0) > 0:
        repo.restore(s["id"])
        repo.seal(summary="Rolled back — tests were failing", agent_id="fixer-bot")
        break
```

Restoring doesn't delete history. The old seals still exist in the log. `writ log --all` always shows the complete record. And since writ works alongside git, you always have the git safety net underneath.

## Security

Writ is built for environments where multiple autonomous agents have write access to the same codebase. That demands security guarantees that traditional VCS was never designed for.

**Cryptographic integrity.** Every seal is chained to its predecessor via BLAKE3 hashes — tamper with any checkpoint and the entire chain breaks. Ed25519 digital signatures authenticate who created each seal. `writ verify --chain` validates the full history in one command.

**Agent identity.** Every agent is a registered entity with a trust level, role, and scope constraints. Untrusted or newly introduced agents receive lower convergence confidence caps, limiting their influence on automated merge decisions. Agents can be suspended or revoked without deleting their history.

**Scope enforcement.** Specs declare which files they own. When an agent seals changes to files outside its spec's scope, writ flags the violation — in context output, in the audit log, and optionally as a hard rejection. No more agents silently modifying files they shouldn't touch.

**Content traceability.** The no-silent-addition rule: every line in merged output must trace back to an input (base, left, or right). Novel content — whether from a convergence bug or an LLM hallucination — is automatically detected and flagged before it reaches the working tree.

**Audit trail.** An append-only security event log records scope violations, signature failures, agent revocations, and convergence anomalies. Events are severity-classified (info, warning, critical) with configurable retention.

```bash
writ verify --chain                        # validate full seal chain integrity
writ security events --severity warning    # review security audit log
```

## Lifecycle and Storage

As projects grow — more agents, more specs, more seals — storage and state need active management. Writ includes a built-in garbage collection system that keeps repositories clean without ever compromising the immutable seal history.

**Spec lifecycle.** Specs progress through a managed lifecycle: active, stale, completed, cancelled, archived. Stale detection is automatic — `writ context` warns when specs go inactive so nothing falls through the cracks.

**Storage-aware.** Writ tracks storage usage across categories (seals, working state, security events, keys) and alerts when usage approaches configured budgets. Seals are never refused.

**Safe cleanup.** `writ gc` generates a plan, shows what it will do, and asks before executing. Seals are immutable and never deleted — GC only cleans expired working state, archived specs past retention, and old security events. Every cleanup action is recorded in an audit trail.

**Deployment profiles.** Pre-configured storage budgets and retention periods for different environments — from a 500MB Raspberry Pi to unlimited enterprise.

```bash
writ gc status                             # storage breakdown + stale spec warnings
writ gc run --dry-run                      # preview cleanup plan without executing
```

## `writ install`

One command. No config files, no setup wizards, no 12-step onboarding.

```bash
writ install
```

What it does (all idempotent — safe to run multiple times):

1. **Init** — creates `.writ/` directory if it doesn't exist
2. **`.writignore`** — creates a sensible default (`.git/`, `node_modules/`, etc.)
3. **Git detection** — finds git repo, reads branch name and HEAD commit
4. **Bridge import** — imports the git working tree as a baseline seal
5. **Framework hooks** — detects Claude Code, Codex, and installs writ workflow instructions
6. **File tracking** — reports how many files are now tracked

```
initialized writ repository in .writ/
created .writignore
git: main @ a3f8b2c1
imported git baseline: 47 file(s), seal d81a5736e16d
detected ClaudeCode (CLAUDE.md)
  + .claude/commands/writ-seal.md
  + .claude/commands/writ-context.md
  ~ CLAUDE.md
tracked: 47 file(s)
```

### Framework Support

| Framework | Detection | What Gets Installed |
|-----------|-----------|-------------------|
| **Claude Code** | `CLAUDE.md` or `.claude/` exists | Writ workflow in `CLAUDE.md`, `/writ-seal` and `/writ-context` slash commands |
| **Codex** | `AGENTS.md` or `.codex/` exists | Writ workflow section in `AGENTS.md` |
| **Any agent** | Always | `.writignore`, baseline seal, writ CLI available in PATH |

## Python SDK

```python
import writ

repo = writ.Repository.open(".")
ctx = repo.context(spec="auth-migration")
seal = repo.seal(
    summary="token refresh endpoint",
    agent_id="worker-3",
    spec_id="auth-migration",
    tests_passed=12,
)
```

Higher-level abstractions for agent workflows:

```python
from writ.sdk import Agent, Phase, Pipeline

with Agent("implementer", spec_id="auth") as agent:
    ctx = agent.context
    # ... do work ...
    agent.seal("implemented token refresh", tests_passed=12)
```

## CLI Reference

```
writ install                          # one-command setup (init + git detect + bridge import + hooks)
writ install --profile production     # setup with a deployment profile (storage budgets, retention)
writ uninstall [--force]              # clean removal of writ from the project
writ seal -s "..." --agent ID         # create a structured checkpoint
writ context [--spec ID] [--format]   # structured context dump (json, human, brief)
writ log [--all] [--spec ID]          # seal history (--all includes diverged branches)
writ summary --format commit          # one-line commit message with full provenance
writ summary --format pr              # full PR description with spec/agent breakdown
writ finish                           # one-command: summary → git add → git commit
writ finish --full                    # same, but with PR-style commit body
writ finish --dry-run                 # preview without committing
writ converge LEFT RIGHT [--apply]    # two-spec convergence
writ converge-all --apply --strategy  # merge all diverged branches (escalate, manual, orchestrator)
writ verify --chain                   # verify cryptographic integrity of the full seal chain
writ verify --seal SEAL_ID            # verify a specific seal's hash and signature
writ security events [--severity]     # security audit log with filtering
writ spec add --id ID --title "..."   # register a spec
writ spec status [--state active]     # show specs, optionally filtered by lifecycle state
writ spec cancel ID                   # cancel a spec (lifecycle transition)
writ gc status                        # storage breakdown + stale spec warnings
writ gc run [--dry-run] [--yes]       # generate and execute cleanup plan
writ gc storage                       # detailed storage usage by category
writ state                            # working directory changes
writ diff                             # content-level diff
writ show SEAL_ID [--diff]            # inspect a seal
writ restore SEAL_ID                  # restore to a seal's state
writ bridge import                    # import git state as baseline
writ push / pull                      # sync with remotes
```

## Architecture

```
writ/
├── crates/
│   ├── writ-core/    # Rust: objects, index, seals, specs, diff, context, convergence, bridge
│   ├── writ-cli/     # CLI (clap): install, seal, context, converge, summary, restore, ...
│   └── writ-py/      # Python bindings (PyO3) + Agent SDK (Pipeline, Agent, Phase)
```

**Storage:** Content-addressable object store (SHA-256). Atomic writes (temp + fsync + rename). Hash verification on retrieve. Advisory file locking for concurrency.

**Integrity:** BLAKE3 hash chains link every seal to its predecessor. Ed25519 digital signatures authenticate authorship. `writ verify --chain` validates the entire history in one command.

**Test coverage:** 1,350+ Rust tests, 400+ Python tests, 41 YAML scenario tests across unit, integration, and end-to-end layers.

## Building from Source

```bash
# Rust core + CLI
cargo build --release
cargo test -p writ-core -p writ-cli

# Python bindings
cd crates/writ-py
python3 -m venv .venv && source .venv/bin/activate
pip install maturin pytest
maturin develop
pytest tests/
```

## Roadmap

### Shipped

- **Round-trip workflow.** `writ install` → agents work → `writ finish` → git commit with full provenance
- **Convergence engine v2.** Six-phase pipeline with language-aware analyzers for Python, Rust, Go, TypeScript, and JavaScript. Compose-not-choose philosophy. Hardened post-merge verification. Optimized N-agent merge ordering. Full audit trail with per-resolution confidence scoring
- **Cryptographic seal integrity.** BLAKE3 hash chains + Ed25519 signatures. Dedicated convergence engine keypair. `writ verify --chain` and `writ verify --seal`
- **Agent identity and trust.** Registration with trust levels, roles, and scope constraints. Suspension and revocation without losing history
- **Content traceability.** No-silent-addition rule — novel content from bugs or hallucinations detected and rejected
- **Security event monitoring.** Append-only audit log with severity filtering
- **Integration risk and file contention.** Automatic risk scoring. Hot file detection sorted by agent count
- **Spec-scoped context.** Working state, seals, and contention filtered to spec-relevant files
- **Diverged branch detection.** Per-branch convergence recommendations
- **Garbage collection.** Spec lifecycle management, storage tracking, deployment profiles, safe cleanup with audit trails. Seals are never deleted
- **Git bridge.** Import/export with metadata trailers preserving full provenance
- **Agent framework hooks.** Auto-detection and configuration for Claude Code and Codex
- **Agent SDK.** `Agent`, `Phase`, `Pipeline` abstractions with auto-summary on completion
- **Remote sync.** `writ push` / `writ pull` for distributed workflows
- **CI/CD.** GitHub Actions for automated testing and PyPI publishing

### Ahead

- **Homebrew distribution.** `brew install writ` via tap
- **MCP server.** Model Context Protocol integration for IDE-native writ access
- **Storage compression.** zstd compression on stored objects for reduced disk usage

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, test commands, and contribution guidelines.

## License

AGPL-3.0-only. See [LICENSE](LICENSE) for details.

For commercial licensing inquiries, contact the project maintainer.

---

writ-vcs™ © 2026 Andrew Garfield
