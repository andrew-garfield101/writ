# writ

**The first AI native version control system for agentic development.**

[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue)](LICENSE)
[![CI](https://github.com/andrew-garfield101/writ/actions/workflows/ci.yml/badge.svg)](https://github.com/andrew-garfield101/writ/actions/workflows/ci.yml)
[![PyPI](https://img.shields.io/pypi/v/writ-vcs)](https://pypi.org/project/writ-vcs/)
[![Docs](https://img.shields.io/badge/docs-andrew--garfield101.github.io%2Fwrit-blue)](https://andrew-garfield101.github.io/writ/)


Agents are writing production code, managing infrastructure, processing documents at scale. They are remarkably capable. The version control they're using? Still git. Still designed for humans typing at keyboards. Agents spend tokens on tooling overhead — parsing unstructured text, reconstructing project state from multiple calls, navigating merge conflicts that conventional tools weren't built to handle. The tooling doesn't match what agents can do.

Writ changes that equation. Structured context in one call. Semantic convergence that merges meaning, not lines. Cryptographic integrity across any environment. And a clean round trip back to git when the work is done.

Writ works alongside git, not instead of it.

**One call context.** Building situational awareness with current tools means multiple calls, parsing unstructured output, synthesizing project state from fragments. That's tokens and compute spent on infrastructure, not on the agent's actual task. `writ context` delivers everything — specs, seals, working state, file contention, integration risk — in one structured response. With TOON output format, even the structural overhead disappears. Agents spend tokens on reasoning, not parsing.

**Semantic convergence.** When multiple agents touch the same files, conventional merging sees conflicts. Writ sees structure. Language aware analyzers decompose code into imports, definitions, and statements — composing additive changes automatically and escalating real conflicts with full context and confidence scores. No `<<<<` markers. No guesswork.

**Cryptographic integrity.** BLAKE3 hash chains and Ed25519 signatures on every seal. Agent identity with trust levels and scope enforcement. Content traceability ensures no line in merged output appears from thin air. Tamper with any checkpoint and the chain breaks.

**Environment agnostic.** Zero trust setups where every agent action is verified. Fully autonomous systems like [OpenClaw](https://github.com/openclaw) where agents operate without oversight. Mixed workflows with humans in the loop. VMs, containers, CI runners, bare metal. Writ provides version control for whatever environment agents work in.

**Built for any scale.** An indie developer running 15 agents. An enterprise org running 500 across multiple orchestrators. Writ's efficiency compounds — context costs drop, convergence handles what git can't, and deployment profiles scale from a 500MB Raspberry Pi to unlimited enterprise. The more agents in the system, the more writ matters.

> *"One `writ context` call and I know who did what, which specs are complete, where branches diverged, and what files are contested. That is genuinely valuable and unlike anything available in git alone."*
>
> — AAIS_8, orchestrator agent reviewing a 14-agent, 40-seal project

## Table of Contents

- [Install](#install)
- [Why Writ](#why-writ)
- [Context](#context)
- [Multi-Agent Workflow](#multi-agent-workflow)
- [Convergence](#convergence)
- [Going Back](#going-back)
- [Security](#security)
- [Lifecycle and Storage](#lifecycle-and-storage)
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

One command sets up everything:

```bash
writ init
```

That's it. Writ detects your environment and configures sensible defaults automatically. If you're in a git repo, it reads the branch and HEAD. If Claude Code is present (`CLAUDE.md` or `.claude/`), it creates `/writ-seal` and `/writ-context` slash commands. If Codex is detected (`AGENTS.md` or `.codex/`), it adds writ workflow instructions. For any other agent framework, the CLI is available in PATH and the Python SDK works out of the box. See the [quickstart guide](https://andrew-garfield101.github.io/writ/getting-started/quickstart.html) for a full walkthrough.

The full round trip looks like this:

```bash
# Human checks out a branch, sets up writ
writ init

# Agents work — sealing checkpoints as they go
writ seal -s "added auth module" --agent implementer --spec auth
writ seal -s "tests passing" --agent tester --spec auth --tests-passed 42

# Agent marks its task complete
writ spec done auth

# One call gives agents everything they need
writ context --format toon              # token-optimized for LLM agents

# Human checks in on progress from anywhere
writ status

# When ready, promote completed work to git
writ finish

# Or generate commit messages and PR descriptions from the full session history
git commit -m "$(writ summary --format commit)"
gh pr create --body "$(writ summary --format pr)"
```

Every command in this workflow is available to agents by default after `writ init`. No additional configuration. No agent-specific setup scripts. Agents seal checkpoints and mark tasks complete. The user checks in with `writ status` and promotes completed work to git with `writ finish`. Three commands for the user. The agents handle the rest.

```
 Human world                    Agent world                       Human world
┌──────────┐  writ init     ┌─────────────────┐  writ finish   ┌──────────────┐
│ git repo │ ──────────────→│ agents work:    │ ─────────────→ │ git commit   │
│ (branch) │                │ seal, spec done │  writ status   │ with full    │
│          │                │ context, log    │◀──────────────│ provenance   │
└──────────┘                └─────────────────┘                └──────────────┘
```

## Why Writ

Most multi-agent tooling gives each agent a git worktree and merges via PRs. That handles **isolation** — keeping agents from stepping on each other's files. It doesn't handle **convergence** — bringing their work back together when they inevitably touch the same code.

Git worktrees weren't designed for agents. They solve the wrong problem at the wrong layer. The agent still has to shell out to a CLI, parse unstructured text output, reconstruct project state from multiple commands, and hope that merge conflicts get caught before they corrupt the codebase. Every one of those steps burns tokens and compute on work that isn't the agent's actual task.

Orchestration frameworks like OpenClaw, CrewAI, and LangGraph coordinate what agents *do*. But they don't provide version control for what agents *produce*. When a sub-agent in an automated pipeline modifies a file that another sub-agent depends on, there's no structured record of what changed, who changed it, or how to safely merge the results. The orchestrator coordinates tasks. Writ controls the artifacts.

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
| `git verify-commit` | `writ verify --chain` | BLAKE3 hash chains + Ed25519 signatures — tamper-evident by default |
| (nothing) | `writ status` | Fleet overview — agent activity, spec progress, commit readiness |
| (nothing) | `writ spec done` | Agent marks task complete with final seal — user finishes to git |

### Use Cases

- **Multi-agent software development.** Multiple coding agents working concurrently on overlapping codebases — the core use case writ was designed for
- **Single-agent workflows.** Even one agent benefits from structured checkpoints, context(), and the git round-trip — `writ init` → work → `writ finish`
- **Autonomous pipelines.** Sub-agents in orchestration frameworks (OpenClaw, CrewAI, custom systems) producing artifacts that need version control, provenance, and safe convergence
- **Knowledge work.** Documentation, configuration, data processing — any iterative task where agents modify shared files and need structured history
- **Human-AI collaboration.** Mixed workflows where humans and agents contribute to the same project, with full transparency into who did what

## Context

Situational awareness is the most expensive recurring cost in an agentic workflow. With conventional tools, that means multiple calls — `git log`, `git diff`, `git status`, reading files — each returning unstructured text that needs parsing and synthesis. Capable agents spend a significant portion of their compute budget on tooling overhead instead of their actual task.

Writ consolidates all of that into a single call:

```bash
# One call. Full situational awareness.
writ context --format toon --spec auth-migration
```

One structured response. Everything an agent needs to start working immediately:

- **Spec details** — title, description, status, dependencies, file scope, acceptance criteria
- **Recent seals** — who did what, when, with which files and verification results
- **Working state** — new/modified/deleted files filtered to your spec's scope
- **Agent activity** — which agents own which files, their latest work
- **File contention** — "hot files" touched by 2+ agents, sorted by risk
- **Integration risk** — level (low/medium/high), score (0-100), contributing factors
- **Diverged branches** — specs with unmerged work, with convergence recommendations
- **Scope violations** — seals that touched files outside their spec's declared scope
- **Session status** — whether all specs are complete, with inline summary

Or programmatically:

```python
ctx = repo.context(spec="auth-migration", format="toon")
```

With git, an agent makes 4-5 tool calls and synthesizes its own situational model from unstructured text. With writ, one call returns structured data ready for immediate consumption. That's a fundamental shift in how agents bootstrap into a task.

### Output Formats

Every time an agent requests context, there's a token tax — repeated key names, structural punctuation, formatting overhead. Tokens spent on syntax instead of reasoning. TOON eliminates that tax:

```
seals[5]{id,summary,agent,timestamp,spec}:
  seal-0041,Implement phase 3 pattern matching,cc,2026-03-04T10:00:00Z,S-041
  seal-0042,Add import deduplication,amis,2026-03-04T10:15:00Z,S-039
  seal-0043,Scale test scenarios,bri,2026-03-04T10:30:00Z,S-045
```

Field names declared once. Rows streamed as values. No braces, no repeated keys.

One `writ context` call replaces five git commands and delivers [25% more information per token](https://andrew-garfield101.github.io/writ/concepts/output-formats.html). TOON format reduces output size by 20-33% versus JSON. These savings compound at fleet scale — fewer calls, less parsing, more context window for reasoning. See the [output formats guide](https://andrew-garfield101.github.io/writ/concepts/output-formats.html) for the full benchmark methodology and numbers.

Context output is also adaptive. Solo agent with no divergence? Integration risk and convergence sections don't appear. No scope violations? That section is omitted. The output scales with complexity, not with a fixed schema — a single agent gets a lean response, a 50 agent fleet gets the full picture. Every token in the response carries information.

## Multi-Agent Workflow

Three agents, different specs, working concurrently. Sealing is serialized via advisory file locks, so agents queue safely:

```bash
# Agent A: auth migration (sealing work in progress)
writ seal -s "token refresh endpoint" --agent auth-dev --spec auth-migration

# Agent B: payments (marking task complete)
writ seal -s "stripe integration" --agent pay-dev --spec payments
writ spec done payments

# Agent C: testing (cross-cutting)
writ seal -s "42 tests passing" --agent test-bot --spec test-suite --tests-passed 42
```

The human checks in:

```bash
$ writ status

  Active    2 agents    2 specs in progress
  Done      1 agent     1 spec completed (not committed)

  S-002  payments             pay-dev     5 seals    Complete
  S-001  auth-migration       auth-dev    3 seals    working
  S-003  test-suite           test-bot    1 seal     working

  1 spec complete · run `writ finish` when ready
```

Full transparency. No branch archaeology. No parsing commit messages to figure out which agent did what.

## Convergence

Five agents. Same file. Git sees five conflicts. Your options: resolve them manually, pick a winner and hope, or assign files to single owners — which defeats the purpose of having multiple agents in the first place.

This is the problem no amount of git configuration, worktree isolation, or PR automation solves. Worktrees give agents isolation. Nothing in conventional tooling gives them convergence. Isolation keeps agents from stepping on each other. Convergence brings their work back together. Git handles the first. Writ handles the second.

Writ's convergence engine understands code structurally. It knows the difference between an import, a function definition, and a statement. When two agents both add imports to the same file, writ doesn't see a conflict — it sees two additive changes and composes them. When two agents add functions with different names, writ composes them. When two agents modify the same function body differently, writ knows that's a real semantic conflict and escalates it with full context for human or orchestrator review. No `<<<<` markers. No guesswork. Structured data all the way through.

Five deterministic resolution patterns handle the common cases:

| Pattern | What It Resolves |
|---------|-----------------|
| **Import Accumulation** | Both sides add imports — union them, deduplicate across languages |
| **Non-Overlapping Definitions** | Both sides add functions with different names — compose them |
| **EOF Append** | Both sides append to end of file — concatenate |
| **Additive Composition** | Both sides preserved base and added content — compose |
| **Superset Containment** | One side contains everything the other has — use the superset |

Every resolution is confidence scored. High confidence (≥ 0.85) auto-resolves. Low confidence escalates with structured context so orchestrator agents can resolve conflicts programmatically — or surface them to a human with all the data they need to decide.

The pipeline behind this is layered and auditable. Spec aware resolution uses writ's first class spec and seal metadata — file scope, acceptance criteria, design notes — to make informed merge decisions that no other VCS has the context to make. Post-merge verification catches structural damage automatically — duplicate definitions, unbalanced delimiters, content loss, leftover conflict markers — before bad merges ever reach the working tree. Content traceability ensures every line in merged output traces back to an input. Novel content — from bugs, hallucinations, or compromised agents — is detected and rejected.

Merge ordering is optimized automatically: specs that touch disjoint files merge first, minimizing conflict complexity for the overlapping cases that follow. At scale, this is the difference between a working codebase and a merge conflict graveyard. See the [convergence deep dive](https://andrew-garfield101.github.io/writ/concepts/convergence.html) for the full six phase pipeline.

```bash
# Merge all diverged branches — auto-resolve what's confident, escalate the rest
writ converge-all --apply --strategy most-complete
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

AI models are evolving rapidly. Agents that worked perfectly last week can produce unexpected results after a model update. A convergence might produce garbage. An agent might go off the rails. In fully autonomous environments, these failures can cascade before anyone notices.

Writ is designed for this reality. Every seal is an immutable snapshot of the entire working directory, and you can restore to any of them:

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

Writ is built for environments where multiple autonomous agents have write access to the same codebase. That demands security guarantees that traditional VCS was never designed for — whether you're running a zero-trust setup where every agent action is verified, a fully autonomous system where agents operate without human oversight, or anything in between.

**Cryptographic integrity.** Every seal is chained to its predecessor via BLAKE3 hashes — tamper with any checkpoint and the entire chain breaks. Ed25519 digital signatures authenticate who created each seal. `writ verify --chain` validates the full history in one command.

**Agent identity.** Every agent is a registered entity with a unique keypair, trust level, role, and scope constraints. Trust levels (full, standard, restricted, untrusted) directly affect convergence behavior — untrusted or newly introduced agents receive lower confidence caps, limiting their influence on automated merge decisions. Agents can be suspended or revoked without deleting their history, and all seals created after a compromise timestamp are automatically flagged for review.

**Scope enforcement.** Specs declare which files they own. When an agent seals changes to files outside its spec's scope, writ flags the violation — in context output, in the audit log, and optionally as a hard rejection. No more agents silently modifying files they shouldn't touch.

**Content traceability.** The no-silent-addition rule: every line in merged output must trace back to an input (base, left, or right). Novel content — whether from a convergence bug, a model hallucination, or a compromised agent — is automatically detected and rejected before it reaches the working tree.

**Audit trail.** An append-only security event log records scope violations, signature failures, agent revocations, and convergence anomalies. Events are severity-classified (info, warning, critical) with configurable retention.

```bash
writ verify --chain                        # validate full seal chain integrity
writ security events --severity warning    # review security audit log
```

See the [security model](https://andrew-garfield101.github.io/writ/concepts/security-model.html) for the full trust framework, scope enforcement rules, and audit system.

## Lifecycle and Storage

AI models update. Tooling shifts. What agents produce today may need to be rolled back tomorrow. A VCS for agentic development needs more than immutable history — it needs active lifecycle management that keeps repositories healthy as projects scale and models evolve.

**Spec lifecycle.** Specs progress through a managed lifecycle: active, stale, completed, cancelled, archived. Stale detection is automatic — `writ context` warns when specs go inactive so nothing falls through the cracks.

**Storage-aware.** Writ tracks storage usage across categories (seals, working state, security events, keys) and alerts when usage approaches configured budgets. Seals are never refused — immutable history is sacred.

**Safe cleanup.** `writ gc` generates a plan, shows what it will do, and asks before executing. GC only cleans expired working state, archived specs past retention, and old security events. Every cleanup action is recorded in an audit trail.

**Deployment profiles.** Pre-configured storage budgets and retention periods for different environments — from a 500MB Raspberry Pi to unlimited enterprise.

**Workflow modes.** Three modes that scale from solo developer to enterprise fleet. `user` mode (default): you run `writ finish` manually. `propose` mode: an orchestrator groups and proposes commits, you review and accept. `auto` mode: fully autonomous commit pipeline with configurable safety rails — test verification, max specs per commit, branch targeting. Configure globally or per project. See the [configuration reference](https://andrew-garfield101.github.io/writ/reference/configuration.html) for details.

```bash
writ gc status                             # storage breakdown + stale spec warnings
writ gc run --dry-run                      # preview cleanup plan without executing
```

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
writ init                             # guided setup (git detect + bridge import + framework integration)
writ init --yes                       # non-interactive setup (CI-safe, accept all defaults)
writ init --profile production        # setup with a deployment profile (storage budgets, retention)
writ uninstall [--force]              # clean removal of writ from the project
writ seal -s "..." --agent ID         # create a structured checkpoint
writ context [--spec ID] [--format]   # structured context dump (json, toon, human, brief)
writ status [--watch] [--completed]   # fleet overview: agents, specs, progress
writ log [--all] [--spec ID]          # seal history (--all includes diverged branches)
writ summary --format commit          # one-line commit message with full provenance
writ summary --format pr              # full PR description with spec/agent breakdown
writ finish [--strategy per-spec]      # promote completed specs → git commit(s)
writ finish --yes                     # accept defaults, no prompts (same as current behavior)
writ finish --dry-run                 # preview without committing
writ converge LEFT RIGHT [--apply]    # two-spec convergence
writ converge-all --apply --strategy  # merge all diverged branches (escalate, manual, orchestrator)
writ verify --chain                   # verify cryptographic integrity of the full seal chain
writ verify --seal SEAL_ID            # verify a specific seal's hash and signature
writ security events [--severity]     # security audit log with filtering
writ spec add --id ID --title "..."   # register a spec
writ spec status [--state active]     # show specs, optionally filtered by lifecycle state
writ spec done ID [-s "..."]          # mark spec complete (creates final seal)
writ spec cancel ID                   # cancel a spec (lifecycle transition)
writ reopen --spec ID                 # reopen completed spec for continued work
writ gc status                        # storage breakdown + stale spec warnings
writ gc run [--dry-run] [--yes]       # generate and execute cleanup plan
writ gc storage                       # detailed storage usage by category
writ state                            # working directory changes
writ diff [--spec ID] [--stat]        # spec-aware diff with filtering
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
│   ├── writ-cli/     # CLI (clap): init, seal, context, converge, summary, restore, ...
│   └── writ-py/      # Python bindings (PyO3) + Agent SDK (Pipeline, Agent, Phase)
```

**Storage:** Content-addressable object store (SHA-256). Atomic writes (temp + fsync + rename). Hash verification on retrieve. Advisory file locking for concurrency.

**Integrity:** BLAKE3 hash chains link every seal to its predecessor. Ed25519 digital signatures authenticate authorship. `writ verify --chain` validates the entire history in one command.

**Test coverage:** 1,350+ Rust tests, 400+ Python tests, 41 YAML scenario tests across unit, integration, and end-to-end layers.

### Framework Support

| Framework | Detection | What Gets Installed |
|-----------|-----------|-------------------|
| **Claude Code** | `CLAUDE.md` or `.claude/` exists | Writ workflow in `CLAUDE.md`, `/writ-seal` and `/writ-context` slash commands |
| **Codex** | `AGENTS.md` or `.codex/` exists | Writ workflow section in `AGENTS.md` |
| **Any agent** | Always | `.writignore`, baseline seal, writ CLI available in PATH |

We're continuously expanding framework integrations for the most widely used agent tools and models, while maintaining flexible configuration for custom-built agentic systems.

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

- **LLM assisted convergence.** Direct LLM API integration for conflict resolution — when deterministic patterns can't resolve a conflict, writ queries an LLM to compose a resolution from the existing code. Composition only — the LLM can select, reorder, and combine from the inputs, never generate novel code. Feature flagged with full audit trail
- **Spec aware resolution.** Convergence Phase 4 uses writ's first class spec metadata — file scope, acceptance criteria, semantic intent — to resolve ambiguous conflicts that no other VCS has the context to handle
- **MCP server.** Model Context Protocol integration for IDE native writ access

See [CHANGELOG.md](CHANGELOG.md) for shipped features and version history.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, test commands, and contribution guidelines.

## License

AGPL-3.0-only. See [LICENSE](LICENSE) for details.

For commercial licensing inquiries, contact the project owner.

---

writ-vcs™ © 2026 Andrew Garfield
