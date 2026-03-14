# writ

**The first AI native version control system for agentic development.**

[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue)](LICENSE)
[![CI](https://github.com/andrew-garfield101/writ/actions/workflows/ci.yml/badge.svg)](https://github.com/andrew-garfield101/writ/actions/workflows/ci.yml)
[![PyPI](https://img.shields.io/pypi/v/writ-vcs)](https://pypi.org/project/writ-vcs/)
[![Docs](https://img.shields.io/badge/docs-andrew--garfield101.github.io%2Fwrit-blue)](https://andrew-garfield101.github.io/writ/)


Writ is a version control system designed from the ground up for LLMs and agentic systems. 

Instead of bolting conventions into a VCS built for humans, writ provides elegant AI native version control. 

Structured context in one call. Semantic convergence that merges meaning, not lines. Cryptographic integrity across any environment. And a clean round trip back to git when the work is done.

Writ works alongside git, not instead of it. One `writ init` and agents get everything: native MCP tools, slash commands, workflow instructions, and a structured CLI. No plugins, no configuration, no separate install.

**Context in one call.** Building situational awareness with current tools means multiple calls, parsing unstructured output, synthesizing project state from fragments. That's tokens and compute spent on infrastructure, not on the agent's actual task. `writ context` delivers everything. specs, seals, working state, file contention, integration risk all in one structured response. And with token optimized format options (TOON) even the structural overhead disappears. Agents spend tokens on reasoning, not parsing.

**Semantic convergence.** When multiple agents touch the same files, conventional merging sees conflicts. Writ sees structure and merges meaning. Language aware analyzers decompose code into imports, definitions, and statements, composing additive changes automatically and escalating real conflicts with full context and confidence scores. No `<<<<` markers. No guesswork.

**Cryptographic integrity.** BLAKE3 hash chains and Ed25519 signatures on every seal. Agent identity with trust levels and scope enforcement. Content traceability ensures no line in merged output appears from thin air. Tamper with any checkpoint and the chain breaks.

**Environment agnostic.** Zero trust setups where every agent action is verified. Fully autonomous systems like [OpenClaw](https://github.com/openclaw) where agents operate without oversight. Mixed workflows with humans in the loop. VMs, containers, CI runners, bare metal. Writ provides version control for whatever environment agents work in.

**Built for any scale.** Writ's efficiency compounds at scale. Context costs drop, convergence handles what git can't, and deployment profiles scale from a 500MB Raspberry Pi to unlimited enterprise. The more agents in the system, the more writ matters. Lifecycle management and garbage collection keep repositories clean without ever compromising the immutable seal history.

> *"One `writ context` call and I know who did what, which specs are complete, where branches diverged, and what files are contested. That is genuinely valuable and unlike anything available in git alone."*
>
> — AAIS_8, orchestrator agent reviewing a 14-agent, 40-seal project

## Table of Contents

- [Install](#install)
- [Why Writ](#why-writ)
- [Context](#context)
- [Multi-Agent Workflow](#multi-agent-workflow)
- [Convergence](#convergence)
- [Workspaces](#workspaces)
- [Going Back](#going-back)
- [Security](#security)
- [Lifecycle and Storage](#lifecycle-and-storage)
- [Python SDK](#python-sdk)
- [MCP Server](#mcp-server)
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

That's it. Writ detects your environment and configures sensible defaults automatically. If you're in a git repo, it reads the branch and HEAD.
   Detected agent frameworks get writ workflow instructions added to their configuration files,  CLAUDE.md for Claude Code, AGENTS.md for
  Codex. Claude Code also gets /writ-seal and /writ-context slash commands. For any other agent framework, the CLI is available in PATH and the
  Python SDK works out of the box. See the quickstart guide for a full walkthrough.

The full round trip looks like this:

```bash
  # Human sets up the project
  writ init

  # Define tasks (optional — agents can also create their own specs)
  writ plan "Add auth module" "Payment integration" "Dashboard UI"

  # Start the convergence daemon (auto-merges overlapping work)
  writ watch

  # Launch agents however you normally do — Claude Code, Codex, scripts, etc.
  # Agents work in the same directory. No workspaces needed.

  # Agents discover specs, claim them, and work
  writ context --format toon    # agent sees unclaimed specs
  writ spec claim auth          # agent claims a task
  writ seal -s "auth endpoint" --spec auth    # checkpoint (only this spec's changes)
  writ spec done auth           # agent marks task complete

  # Human checks in on progress from anywhere
  writ status

  # When ready, promote completed work to git
  writ finish     # converges + commits

  # Standard git from here
  git push
```

Every command in this workflow is available to agents by default after `writ init`. No additional configuration. No agent specific setup scripts. Agents discover specs via `writ context`, seal checkpoints with `--spec`, and mark tasks complete. The user starts `writ watch` for auto-convergence, checks in with `writ status`, and promotes completed work to git with `writ finish`. A full getting started guide with example workflows available https://andrew-garfield101.github.io/writ/getting-started/quickstart.html

```
 Human world                    Agent world                       Human world
┌──────────┐  writ init     ┌─────────────────┐  writ finish   ┌──────────────┐
│ git repo │ ──────────────→│ agents work:    │ ─────────────→ │ git commit   │
│ (branch) │                │ seal, spec done │  writ status   │ with full    │
│          │                │ context, log    │◀────────────── │ provenance   │
└──────────┘                └─────────────────┘                └──────────────┘
```

## Why Writ

Most multi agent tooling gives each agent a git worktree and merges via PRs. That handles **isolation** keeping agents from stepping on each other's files. It doesn't handle **convergence** bringing their work back together when they inevitably touch the same code.

Git worktrees weren't designed for agents. They solve the wrong problem at the wrong layer. The agent still has to shell out to a CLI, parse unstructured text output, reconstruct project state from multiple commands, and hope that merge conflicts get caught before they corrupt the codebase. Every one of those steps burns tokens and compute on work that isn't the agent's actual task.

Orchestration frameworks like OpenClaw, CrewAI, and LangGraph coordinate what agents *do*. But they don't provide version control for what agents *produce*. When a sub agent in an automated pipeline modifies a file that another sub agent depends on, there's no structured record of what changed, who changed it, or how to safely merge the results. The orchestrator coordinates tasks. Writ controls the artifacts.

Writ puts agent native metadata inside the VCS:

| Git | Writ | What Changes |
|-----|------|-------------|
| Branch | **Spec** | Structured requirement with status, dependencies, file scope, acceptance criteria |
| Commit | **Seal** | Checkpoint with agent identity, spec linkage, verification, status lifecycle |
| Multiple `git` commands | `writ context` | One call returns everything an agent needs — not text to parse |
| `git merge` | `writ converge-all` | Multi-branch convergence with strategy selection and structured conflict reports |
| `git checkout <ref>` | `writ restore` | Restore working directory to any seal — every seal is an immutable snapshot |
| (nothing) | **Integration risk** | Automatic risk scoring from divergence, contention, and scope violations |
| (nothing) | **File contention** | Which files are touched by which agents, sorted by risk |
| (nothing) | **Session summary** | Auto-generated commit messages and PR descriptions from seal history |
| `git verify-commit` | `writ verify --chain` | BLAKE3 hash chains + Ed25519 signatures — tamper-evident by default |
| (nothing) | `writ watch` | Real time convergence daemon — auto-merges overlapping work as agents seal |
| (nothing) | `writ plan` | Batch task definition — agents discover and claim specs via context |
| (nothing) | `writ status` | Fleet overview — agent activity, spec progress, commit readiness |
| (nothing) | `writ spec done` | Agent marks task complete with final seal — user finishes to git |

### Use Cases

- **Multi agent software development.** Multiple coding agents working concurrently on overlapping codebases. The core use case writ was designed for
- **Single-agent workflows.** Even one agent benefits from structured checkpoints, context(), and the git round-trip — `writ init` → work → `writ finish`
- **Autonomous pipelines.** Sub agents in orchestration frameworks (OpenClaw, CrewAI, custom systems) producing artifacts that need version control, provenance, and safe convergence
- **Knowledge work.** Documentation, configuration, data processing. Any iterative task where agents modify shared files and need structured history
- **Human-AI collaboration.** Mixed workflows where humans and agents contribute to the same project, with full transparency into who did what

## Context

Situational awareness is the most expensive recurring cost in an agentic workflow. With conventional tools, that means multiple calls — `git log`, `git diff`, `git status`, reading files, each returning unstructured text that needs parsing and synthesis. Capable agents spend a significant portion of their compute budget on tooling overhead instead of their actual task. At fleet scale, that overhead compounds, every agent, every session, every context read.

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

Every time an agent requests context, there's token use, repeated key names, structural punctuation, formatting overhead. Tokens spent on syntax instead of reasoning. TOON minimizes that token use. 

```
seals[5]{id,summary,agent,timestamp,spec}:
  seal-0041,Implement phase 3 pattern matching,agent-1,2026-03-04T10:00:00Z,S-041
  seal-0042,Add import deduplication,agent-2,2026-03-04T10:15:00Z,S-039
  seal-0043,Scale test scenarios,agent-3,2026-03-04T10:30:00Z,S-045
```

Field names declared once. Rows streamed as values. No braces, no repeated keys.

One `writ context` call replaces five git commands and delivers [25% more information per token](https://andrew-garfield101.github.io/writ/concepts/output-formats.html). TOON format reduces output size by 20-33% versus JSON. These savings compound at fleet scale, fewer calls, less parsing, more context window for reasoning. See the [output formats guide](https://andrew-garfield101.github.io/writ/concepts/output-formats.html) for the full benchmark methodology and numbers.

Context output is also adaptive. Solo agent with no divergence? Integration risk and convergence sections don't appear. No scope violations? That section is omitted. The output scales with complexity, not with a fixed schema. A single agent gets a lean response, a 50 agent fleet gets the full picture. Every token in the response carries information.

## Multi-Agent Workflow
Multiple agents, same project directory, zero ceremony. Spec-scoped sealing captures only each agent's changes. `writ watch` auto-converges overlapping work in real time:

```bash
# Setup: define tasks and start the convergence daemon
writ plan "Auth migration" "Payments" "Test suite"
writ watch

# Agents work in the same directory — each seals only their own changes
# Agent A: writ seal -s "token refresh endpoint" --spec auth-migration
# Agent B: writ seal -s "stripe integration" --spec payments
# Agent C: writ seal -s "42 tests passing" --spec test-suite --tests-passed 42

# writ watch detects overlapping changes and auto-converges in real time
# Agents never pause, never wait, never merge
```

The human checks in with a single command:

```bash
$ writ status

  Active    2 agents    2 specs in progress
  Done      1 agent     1 spec completed (not committed)

  S-002  payments             pay-dev     5 seals    Complete
  S-001  auth-migration       auth-dev    3 seals    working
  S-003  test-suite           test-bot    1 seal     working

  Auto-Convergence: 12 merges, 0 conflicts

  1 spec complete · run `writ finish` when ready
```

Full transparency. No branch archaeology, no parsing commit messages. Every agent's work is tracked, attributed, and queryable from the moment it happens.

## Convergence

Five agents. Same file. Git sees five conflicts. Your options: resolve them manually, pick a winner, or assign files to single owners, which defeats the purpose of having multiple agents in the first place.

This is what git configuration, worktree isolation, and PR automation don't solve. Worktrees give agents isolation. Nothing in conventional tooling gives them convergence. Isolation keeps agents from stepping on each other. Convergence brings their work back together. Git handles the first. Writ handles the second.

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

The pipeline behind this is layered and auditable. Spec aware resolution uses writ's first class spec and seal metadata, file scope, acceptance criteria, design notes, to make informed merge decisions that no other VCS has the context to make. Post-merge verification catches structural damage automatically. Duplicate definitions, unbalanced delimiters, content loss, leftover conflict markers, before bad merges ever reach the working tree. Content traceability ensures every line in merged output traces back to an input. Novel content, from bugs, hallucinations, or compromised agents is detected and rejected.

Merge ordering is optimized automatically: specs that touch disjoint files merge first, minimizing conflict complexity for the overlapping cases that follow. At scale, this is the difference between a working codebase and hours of manual conflict resolution. See the [convergence deep dive](https://andrew-garfield101.github.io/writ/concepts/convergence.html) for the full six phase pipeline.

```bash
# Merge all diverged branches — auto-resolve what's confident, escalate the rest
writ converge-all --apply --strategy escalate
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

## Workspaces

Most multi-agent work happens in the same directory with spec-scoped sealing and `writ watch` handling convergence automatically. Workspaces exist for the rare case where agents need physical file isolation — competing rewrites of the same code, where each agent's changes would break the other's in-progress work.

```bash
# Level 2: agents rewrite the same code in fundamentally different ways
writ task "rewrite auth: PKCE approach"     # creates isolated workspace
writ task "rewrite auth: implicit approach"  # creates isolated workspace
# launch agents in workspace directories — each has their own copy
writ finish                                  # auto-converges + commits
```

`writ task` creates a spec, a workspace directory with a full project copy, and prints a launch command. Agents in workspaces use the same 3 commands: `context`, `seal`, `spec done`. `writ finish` auto-converges workspaces before committing to git.

Same number of steps as the git workflow everyone already knows:

| Git | Writ | What Improves |
|-----|------|---------------|
| `git checkout -b feature` | Same directory (or `writ task` for Level 2) | No branches needed for most multi-agent work |
| `git add && git commit` | `writ seal --spec` | Agent identity, spec linkage, immutable chain |
| `git merge` | `writ finish` (auto-converges) | Structure aware, auto resolves independent changes |

When to use workspaces vs same-directory: if agents touch different files or add to the same file, same-directory works. If agents rewrite the same function body in different ways, use `writ task`. See the [workspaces guide](https://andrew-garfield101.github.io/writ/guides/workspaces.html) for the full architecture and advanced commands.

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

Agents can do the same thing programmatically. If an agent detects that something went wrong like tests failing, scope violations piling up, it can walk the seal history and self-correct:

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

**Workflow modes.** Two modes that scale from solo developer to enterprise fleet. `user` mode (default): you run `writ finish` manually. `auto` mode: fully autonomous commit pipeline with configurable safety rails — test verification, max specs per commit, branch targeting. Configure globally or per project. See the [configuration reference](https://andrew-garfield101.github.io/writ/reference/configuration.html) for details.

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

Higher-level abstractions for agent workflows (planned, not yet available):

```python
from writ.sdk import Agent, Phase, Pipeline

with Agent("implementer", spec_id="auth") as agent:
    ctx = agent.context
    # ... do work ...
    agent.seal("implemented token refresh", tests_passed=12)
```

## MCP Server

Writ ships a native MCP (Model Context Protocol) server built in Rust. It's part of the `writ` binary — no separate install, no Python runtime, no plugins. When an agent connects via MCP, every writ command is available as a native tool in the agent's palette.

```bash
# Automatic: writ init generates .mcp.json for Claude Code
writ init

# Manual: generate MCP config for Claude Code
writ mcp-install

# Manual: generate MCP config for Claude Desktop
writ mcp-install --desktop
```

When `.mcp.json` is committed to your repository, every developer who clones the project gets MCP tools automatically. Zero setup for the team.

21 tools are available through MCP, matching the full CLI:

| Category | Tools |
|----------|-------|
| **Core Workflow** | `writ_context`, `writ_seal`, `writ_spec_add`, `writ_spec_done` |
| **Status and Review** | `writ_status`, `writ_diff`, `writ_log`, `writ_show` |
| **Spec Management** | `writ_spec_status`, `writ_spec_show`, `writ_spec_reopen` |
| **Round Trip** | `writ_finish`, `writ_summary` |
| **Recovery and Convergence** | `writ_restore`, `writ_converge` |
| **Workspaces** | `writ_workspace_create`, `writ_workspace_list`, `writ_workspace_status`, `writ_workspace_delete` |
| **Diagnostics** | `writ_verify`, `writ_doctor` |

Each tool is a thin wrapper around the CLI. Same behavior, same output, same enforcement. `writ_seal` requires a spec (C.13 enforcement at the schema level). `writ_context` defaults to TOON format and writes the context token (C.14). The MCP server is the CLI, just reachable through the protocol agents already speak.

See the [MCP server guide](https://andrew-garfield101.github.io/writ/guides/mcp-server.html) for the full setup walkthrough and tool reference.

## CLI Reference

```
writ init                             # guided setup (git detect + bridge import + framework integration)
writ init --yes                       # non-interactive setup (CI-safe, accept all defaults)
writ init --profile production        # setup with a deployment profile (storage budgets, retention)
writ uninit [--force]                 # clean removal of writ from the project
writ plan "task1" "task2" "task3"     # batch spec creation (or -f tasks.txt, or stdin)
writ watch [--interval N]            # convergence daemon: auto-merges overlapping work
writ watch --daemon                  # run as background process
writ task "description"              # create task: spec + workspace (Level 2 isolation)
writ task list                       # show all active tasks and status
writ seal -s "..." --spec ID          # create a structured checkpoint (spec-scoped)
writ context [--spec ID] [--format]   # structured context dump (json, toon, human, brief)
writ status [--watch] [--completed]   # fleet overview: agents, specs, progress
writ log [--all] [--spec ID]          # seal history (--all includes diverged branches)
writ summary --format commit          # one-line commit message with full provenance
writ summary --format pr              # full PR description with spec/agent breakdown
writ finish [--strategy per-spec]      # auto-converge workspaces + promote specs → git commit(s)
writ finish --yes                     # accept defaults, no prompts (same as current behavior)
writ finish --dry-run                 # preview without committing
writ finish --cleanup                 # auto-clean workspace directories after commit
writ converge LEFT RIGHT [--apply]    # two-spec convergence
writ converge-all --apply --strategy  # merge all diverged branches
writ converge-workspaces a b         # merge across named workspaces
writ verify --chain                   # verify cryptographic integrity of the full seal chain
writ verify --seal SEAL_ID            # verify a specific seal's hash and signature
writ security events [--severity]     # security audit log with filtering
writ spec add --id ID --title "..."   # register a spec
writ spec claim ID                    # claim an unclaimed spec (auto-claims on first seal)
writ spec status [--state active]     # show specs, optionally filtered by lifecycle state
writ spec done ID [-s "..."]          # mark spec complete (creates final seal)
writ spec cancel ID                   # cancel a spec (lifecycle transition)
writ spec reopen ID                   # reopen completed spec for continued work
writ gc status                        # storage breakdown + stale spec warnings
writ gc run [--dry-run] [--yes]       # generate and execute cleanup plan
writ gc storage                       # detailed storage usage by category
writ state                            # working directory changes
writ diff [--spec ID] [--stat]        # spec-aware diff with filtering
writ show SEAL_ID [--diff]            # inspect a seal
writ restore SEAL_ID                  # restore to a seal's state
writ bridge import                    # import git state as baseline
writ push / pull                      # sync with remotes
writ workspace create <name> [--path] [--specs]  # create isolated parallel workspace
writ workspace list                              # list all workspaces with paths and specs
writ workspace status [name]                     # workspace details and progress
writ workspace delete <name> [--keep-files]      # remove workspace, preserve history
writ spec assign <id> --workspace <name>         # scope a spec to a workspace
writ spec unassign <id>                          # make a spec globally visible again
writ mcp-serve                        # start MCP server (used by .mcp.json)
writ mcp-install [--desktop]          # generate MCP config for Claude Code or Claude Desktop
writ doctor [--json]                  # repo health check (8 diagnostic checks)
```

## Architecture

```
writ/
├── crates/
│   ├── writ-core/    # Rust: objects, index, seals, specs, diff, context, convergence, bridge
│   ├── writ-cli/     # CLI (clap): init, seal, context, converge, summary, restore, mcp-serve, ...
│   ├── writ-mcp/     # MCP server (rmcp): 17 tools, CLI passthrough, stdio transport
│   └── writ-py/      # Python bindings (PyO3) + Agent SDK (Pipeline, Agent, Phase)
```

**Storage:** Content-addressable object store (SHA-256). Atomic writes (temp + fsync + rename). Hash verification on retrieve. Advisory file locking for concurrency.

**Integrity:** BLAKE3 hash chains link every seal to its predecessor. Ed25519 digital signatures authenticate authorship. `writ verify --chain` validates the entire history in one command.

**Test coverage:** 1,800+ Rust tests, 400+ Python tests, 41 YAML scenario tests across unit, integration, and end-to-end layers.

### Framework Support

| Framework | Detection | What Gets Installed |
|-----------|-----------|-------------------|
| **Claude Code** | `CLAUDE.md` or `.claude/` exists | Writ workflow in `CLAUDE.md`, 17 slash commands in `.claude/commands/`, `.mcp.json` for native MCP tools |
| **Claude Desktop** | `writ mcp-install --desktop` | MCP server config in Claude Desktop settings |
| **Codex** | `AGENTS.md` or `.codex/` exists | Writ workflow section in `AGENTS.md` |
| **Any agent** | Always | `.writignore`, baseline seal, writ CLI available in PATH |

The MCP server and slash commands ship with writ. No separate install, no Python runtime, no plugins. `writ init` detects your environment and sets up everything automatically. See the [MCP server guide](https://andrew-garfield101.github.io/writ/guides/mcp-server.html) for setup details and the full tool list.

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

- **LLM assisted convergence (in preview).** Direct LLM API integration for conflict resolution when deterministic patterns can't resolve a conflict, writ queries an LLM to compose a resolution from the existing code. Composition only, the LLM can select, reorder, and combine from the inputs, never generate novel code. Implemented and feature flagged with full audit trail
- **Spec aware resolution (in preview).** Convergence Phase 4 uses writ's first class spec metadata — file scope, acceptance criteria, semantic intent — to resolve ambiguous conflicts that no other VCS has the context to handle. Implemented and feature flagged

See [CHANGELOG.md](CHANGELOG.md) for shipped features and version history.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, test commands, and contribution guidelines.

## License

AGPL-3.0-only. See [LICENSE](LICENSE) for details.

For commercial licensing inquiries, contact the project owner.

---

writ-vcs™ © 2026 Andrew Garfield
