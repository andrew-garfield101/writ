# writ

**The first AI native version control system for agentic development.**

[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue)](LICENSE)
[![CI](https://github.com/andrew-garfield101/writ/actions/workflows/ci.yml/badge.svg)](https://github.com/andrew-garfield101/writ/actions/workflows/ci.yml)
[![PyPI](https://img.shields.io/pypi/v/writ-vcs)](https://pypi.org/project/writ-vcs/)
[![Docs](https://img.shields.io/badge/docs-andrew--garfield101.github.io%2Fwrit-blue)](https://andrew-garfield101.github.io/writ/)

Writ is a version control system designed from the ground up for LLMs and agentic systems.

Instead of bolting conventions into a VCS built for humans, writ provides elegant AI native version control.

Structured context in one call. Three way merge that auto resolves overlapping agent work. Cryptographic integrity across any environment. And a clean round trip back to git when the work is done.

Writ works alongside git, not instead of it. One `writ init` and agents get everything: native MCP tools, slash commands, workflow instructions, and a structured CLI. No plugins, no configuration, no separate install.

**Context in one call.** Building situational awareness with current tools means multiple calls, parsing unstructured output, synthesizing project state from fragments. That's tokens and compute spent on infrastructure, not on the agent's actual task. `writ context` delivers everything — specs, seals, working state, file contention, integration risk — all in one structured response. That structured data costs more tokens than raw git output. The tradeoff: one call with ready to consume coordination data versus five separate calls that agents must parse, correlate, and reason about on their own. TOON format and spec scoped filtering keep context overhead lean, and writ's token ratio improves as projects scale.

**Automatic convergence.** When multiple agents touch the same files, conventional merging sees conflicts. Writ sees overlapping work and merges it. Each agent's changes are tracked independently through spec scoped seals. When convergence runs, a three way merge engine uses the genesis state (a snapshot of the codebase when the spec was created) as the common ancestor, producing the correct combined result. Additive changes merge automatically. Real conflicts escalate with structured context and confidence scores for human or orchestrator review. No `<<<<` markers. No guesswork.

**Cryptographic integrity.** BLAKE3 hash chains and Ed25519 signatures on every seal. Agent identity with trust levels and scope enforcement. Tamper with any checkpoint and the chain breaks.

**Environment agnostic.** Zero trust setups where every agent action is verified. Fully autonomous systems like [OpenClaw](https://github.com/openclaw) where agents operate without oversight. Mixed workflows with humans in the loop. VMs, containers, CI runners, bare metal. Writ provides version control for whatever environment agents work in.

**Built for any scale.** Writ's efficiency compounds as you add agents. TOON format and scoped context keep overhead lean — scoped context grows only 1.6x from 1 to 8 agents while providing full multi agent awareness that git cannot offer at any token cost. Convergence handles what git can't. Deployment profiles scale from a 500MB Raspberry Pi to unlimited enterprise. The more agents in the system, the more writ matters. Lifecycle management and garbage collection keep repositories clean without ever compromising the immutable seal history.

> *"One `writ context` call and I know who did what, which specs are complete, where work overlaps, and what files are contested. That is genuinely valuable and unlike anything available in git alone."*
>
> — AAIS_8, orchestrator agent reviewing a 14-agent, 40-seal project

## Writ's Building Blocks

Six first class primitives:

| Primitive | What It Is |
|-----------|-----------|
| **Spec** | A task unit with a hash ID, lifecycle states, agent claiming, and a genesis snapshot. Tracks what's being worked on, who's doing it, and how far along. Closest analog is an issue that lives inside the VCS itself — not a branch. |
| **Seal** | An immutable snapshot of file state. Parent chained, cryptographically signed, spec scoped. Every checkpoint is permanent and restorable. |
| **Context** | Computed project state for agents. Specs, seals, file contention, integration risk, all assembled on demand in one structured call. The intelligence layer that turns raw data into coordination. |
| **Convergence** | Three way merge using sealed histories and genesis trees as base. Auto resolves non-conflicting changes. Escalates real conflicts with confidence scores. |
| **Object Store** | Content addressable storage (SHA-256). Every file version stored once, deduplicated automatically. Same model as git's object store. |
| **Finish** | The round trip to git. Converges outstanding work, materializes to disk, commits. One command replaces branch management and merge resolution. |

## The Commands

Two for the human:

```
writ init       Set up the project (once)
                Agents work... (user does nothing)
writ finish     Commit everything to git
```

Three for the agent:

```
writ spec add "task description"    Create a task (auto-generated hash ID)
writ seal -s "what I did"           Checkpoint work (auto-scoped to claimed spec)
writ spec done                      Mark task complete (auto-scoped)
```

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

Writ detects your environment and configures everything automatically. Agent frameworks get workflow instructions added to their configuration files — CLAUDE.md for Claude Code, AGENTS.md for Codex. Claude Code also gets 21 MCP tools, 20 slash commands, and a SessionStart hook that injects context at the beginning of every agent session. For any other agent framework, the CLI is available in PATH and the Python SDK works out of the box. See the [quickstart guide](https://andrew-garfield101.github.io/writ/getting-started/quickstart.html) for a full walkthrough.

The full round trip:

```bash
# Human sets up the project
writ init

# Launch agents with their tasks
claude -p "Add user authentication to the API"
claude -p "Add payment processing with Stripe"
claude -p "Write tests for auth and payments"

# Each agent automatically:
#   1. Discovers writ (via CLAUDE.md, MCP tools, SessionStart hook)
#   2. Creates a spec: writ spec add "user authentication"
#   3. Works and checkpoints: writ seal -s "auth endpoint added"
#   4. Marks complete: writ spec done

# Human checks progress anytime
writ status

# When ready, promote to git
writ finish
git push
```

Agents discover writ without being told. The user's prompts are about the task, not the tool. "Add user authentication" — not "Add user authentication and please use writ." Init generates everything agents need to adopt writ automatically.

```
 Human world                    Agent world                       Human world
┌──────────┐  writ init     ┌─────────────────┐  writ finish   ┌──────────────┐
│ git repo │ ──────────────→│ agents work:    │ ─────────────→ │ git commit   │
│          │                │ spec add, seal, │  writ status   │ with full    │
│          │                │ spec done       │◀────────────── │ provenance   │
└──────────┘                └─────────────────┘                └──────────────┘
```

## Why Writ

Most multi agent tooling gives each agent a git worktree and merges via PRs. That handles **isolation** — keeping agents from stepping on each other's files. It doesn't handle **convergence** — bringing their work back together when they inevitably touch the same code.

Git worktrees weren't designed for agents. They solve the wrong problem at the wrong layer. The agent still has to shell out to a CLI, parse unstructured text output, reconstruct project state from multiple commands, and hope that merge conflicts get caught before they corrupt the codebase. Every one of those steps burns tokens and compute on work that isn't the agent's actual task.

Orchestration frameworks like OpenClaw, CrewAI, and LangGraph coordinate what agents *do*. But they don't provide version control for what agents *produce*. When a sub agent in an automated pipeline modifies a file that another sub agent depends on, there's no structured record of what changed, who changed it, or how to safely merge the results. The orchestrator coordinates tasks. Writ controls the artifacts.

Writ puts agent native metadata inside the VCS:

| Git | Writ | What Changes |
|-----|------|-------------|
| (nothing) | **Spec** | Task tracking with lifecycle, agent claiming, genesis snapshots — no git equivalent |
| Commit | **Seal** | Immutable checkpoint with agent identity, spec linkage, crypto signatures |
| Multiple `git` commands | `writ context` | One call returns everything an agent needs — structured data, not text to parse |
| `git merge` | Convergence | Three way merge on seal trees. Auto resolves non-conflicting changes |
| `git checkout <ref>` | `writ restore` | Instant rollback to any seal — every seal is an immutable snapshot |
| (nothing) | **Integration risk** | Automatic overlap scoring across agents and specs |
| (nothing) | **File contention** | Which files are touched by which agents, sorted by risk |
| (nothing) | `writ finish` | Converge + commit in one command |
| `git verify-commit` | `writ verify` | BLAKE3 + Ed25519, full chain validation |
| (nothing) | `writ status` | Fleet dashboard — agents, specs, progress, commit readiness |

### Use Cases

- **Multi agent software development.** Multiple coding agents working concurrently on overlapping codebases. The core use case writ was designed for
- **Single agent workflows.** Even one agent benefits from structured checkpoints, instant rollback, and the git round trip — `writ init` → work → `writ finish`
- **Autonomous pipelines.** Sub agents in orchestration frameworks producing artifacts that need version control, provenance, and safe convergence
- **Knowledge work.** Documentation, configuration, data processing. Any iterative task where agents modify shared files and need structured history
- **Human AI collaboration.** Mixed workflows where humans and agents contribute to the same project with full transparency into who did what

## Context

Situational awareness is the most expensive recurring cost in an agentic workflow. With conventional tools, that means multiple calls — `git log`, `git diff`, `git status`, reading files — each returning unstructured text that needs parsing and synthesis. Capable agents spend a significant portion of their compute budget on tooling overhead instead of their actual task. At fleet scale, that overhead compounds: every agent, every session, every context read.

Writ consolidates all of that into a single call:

```bash
# One call. Full situational awareness.
writ context --format toon
```

One structured response. Everything an agent needs to start working immediately:

- **Spec details** — title, status, claimed agent, seal count, last activity
- **Unclaimed specs** — tasks available for this agent to pick up
- **Recent seals** — who did what, when, with which files
- **Working state** — new, modified, and deleted files in the current scope
- **Agent activity** — which agents own which files, their latest work
- **File contention** — "hot files" touched by 2+ agents, sorted by risk
- **Integration risk** — level (low/medium/high), score (0-100), contributing factors
- **Stale specs** — specs with no recent activity that may be abandoned
- **Session status** — whether all specs are complete

Or programmatically:

```python
ctx = repo.context(format="toon")
```

With git, an agent makes 4-5 tool calls and synthesizes its own situational model from unstructured text. With writ, one call returns structured data ready for immediate consumption. That's a fundamental shift in how agents bootstrap into a task.

### Output Formats

Every time an agent requests context, there's token use — repeated key names, structural punctuation, formatting overhead. Tokens spent on syntax instead of reasoning. TOON minimizes that:

```
seals[5]{id,summary,agent,timestamp,spec}:
  seal-0041,Implement phase 3 pattern matching,agent-1,2026-03-04T10:00:00Z,S-041
  seal-0042,Add import deduplication,agent-2,2026-03-04T10:15:00Z,S-039
  seal-0043,Scale test scenarios,agent-3,2026-03-04T10:30:00Z,S-045
```

Field names declared once. Rows streamed as values. No braces, no repeated keys.

One `writ context` call replaces five git commands — `status`, `log`, `diff`, `branch`, and author tracking — delivering specs, seals, ownership, convergence state, and integration risk in a single structured payload.

That structured data costs more tokens than raw git output. A medium project (4 specs, 20 files): writ TOON returns 3,372 tokens versus 369 for five git commands. That's the cost of coordination data — agent attribution, integration risk, file contention, spec lifecycle — information git does not track and cannot provide at any token cost. The tradeoff: git returns unstructured text that agents must parse, correlate, and reason about across five separate calls. Writ returns structured data ready for immediate consumption.

Three things keep that overhead in check:

**TOON format** saves 12% over JSON on every call. Field names declared once, rows streamed as values, no redundant structure.

**Spec scoped context** (`writ context --spec <id>`) is the big lever. Each agent sees only their slice of the project — 65% fewer tokens than full context at fleet scale. Scoped context grows only 1.6x from 1 to 8 agents. Full context grows 3x over the same range.

**Writ scales better with project size.** Git output grows 6.3x from small to XL projects. TOON grows only 3.4x. The bigger the project and the more agents in it, the better writ's efficiency relative to git.

At fleet scale (8 agents, 5 context calls each), scoped context saves 145,000 tokens over a session compared to full context. See the [output formats guide](https://andrew-garfield101.github.io/writ/concepts/output-formats.html) for the full benchmark methodology and numbers.*

Context output is also adaptive. Solo agent with no divergence? Integration risk and convergence sections don't appear. No scope violations? That section is omitted. The output scales with complexity, not with a fixed schema. A single agent gets a lean response. A 50 agent fleet gets the full picture. Every token in the response carries information.

---

\* *Benchmarks measured on macOS with Claude Code using tiktoken cl100k_base encoding. Build: writ 0.1.0-alpha.32. Results from a single test session. See the [output formats guide](https://andrew-garfield101.github.io/writ/concepts/output-formats.html) for methodology.*

## Multi-Agent Workflow

Multiple agents, same project directory, zero ceremony. Each agent creates its own spec, seals its own changes, and marks its own task complete. Convergence merges overlapping work automatically when specs complete and when `writ finish` runs.

There are three paths to assigning work, scaling from hands on to fully automated:

### Path A: Manual Launch (1-5 agents)

Open terminal tabs, launch agents, prompt them with tasks. Agents self organize.

```bash
writ init

# Terminal 1:
claude -p "Add user authentication to the API"

# Terminal 2:
claude -p "Add payment processing with Stripe"

# Terminal 3:
claude -p "Write tests for auth and payments"

# When agents finish:
writ finish
git push
```

Each agent receives its task from the user's prompt and creates its own spec via `writ spec add`. No pre-definition needed. This is where most users start.

### Path B: Batch Planning (5-50 agents)

Pre-define tasks and let agents claim them. Good for structured work where you know the task list upfront.

```bash
writ init

# Define tasks
writ plan "Implement OAuth2 login" \
         "Add Stripe payment processing" \
         "Build admin dashboard" \
         "Write API documentation" \
         "Add rate limiting middleware"

# Launch agents — they discover tasks via writ context
for i in $(seq 1 5); do
  claude -p "You are working on this project. Check writ context for your task." &
done

# When agents finish:
writ finish
git push
```

`writ plan` pre-creates specs. Agents see unclaimed specs in `writ context` and claim one that matches their understanding. If an agent doesn't find a match, it creates its own spec.

### Path C: Programmatic SDK (50-500+ agents)

Full programmatic control via the Python SDK. Build orchestration scripts that create specs, launch agents, monitor progress, and finish.

```python
import writ
import subprocess

repo = writ.Repository.open(".")
tasks = [
    "Implement user authentication",
    "Add payment processing",
    "Build admin dashboard",
    # ... 50 more tasks
]
repo.plan(tasks)

# Launch agents
for task in tasks:
    subprocess.Popen(["claude", "-p", task])

# Monitor progress
while True:
    ctx = repo.context()
    specs = ctx.get("specs", [])
    done = [s for s in specs if s["status"] == "complete"]
    print(f"{len(done)}/{len(tasks)} complete")
    if len(done) == len(tasks):
        break
    time.sleep(30)

# Finish
repo.finish()
```

### All Three Paths Converge

Regardless of how tasks are assigned, the rest is identical:
- Agents seal against their claimed spec
- Convergence merges overlapping work at `spec done` and `finish`
- `writ finish` materializes to disk and commits to git
- `git push` ships it

```
Path A (manual)  ──┐
Path B (batch)   ──┼──▶  Agents work  ──▶  writ finish  ──▶  git push
Path C (SDK)     ──┘
```

The human checks in with a single command:

```bash
$ writ status

  Active    2 agents    2 specs in progress
  Done      1 agent     1 spec completed (not committed)

  b7e2a4f1  payments           pay-dev     5 seals    Complete
  a3f8b2c9  auth-migration     auth-dev    3 seals    working
  d4c1e8a6  test-suite         test-bot    1 seal     working

  1 spec complete · run `writ finish` when ready
```

Full transparency. No branch archaeology, no parsing commit messages. Every agent's work is tracked, attributed, and queryable from the moment it happens.

## Convergence

Five agents. Same file. Git sees five conflicts. Your options: resolve them manually, pick a winner, or assign files to single owners — which defeats the purpose of having multiple agents in the first place.

This is what git worktrees, branch isolation, and PR automation don't solve. Worktrees give agents isolation. Nothing in conventional tooling gives them convergence. Isolation keeps agents from stepping on each other. Convergence brings their work back together. Git handles the first. Writ handles the second.

### How It Works

Every spec in writ has a **genesis tree**: a snapshot of the file index at the moment the spec was created. When convergence runs, it uses this genesis tree as the common ancestor for three way merge:

- **Base**: the genesis tree (what files looked like when the spec started)
- **Left**: spec A's sealed version of the file
- **Right**: spec B's sealed version of the file

Non-conflicting changes (different lines, additive edits, independent sections) merge automatically. When both specs modify the same region in incompatible ways, the **Escalate** strategy auto-resolves by selecting the more complete version. If that's ambiguous, the conflict escalates with structured context for human or orchestrator review.

A three layer **pool filter** ensures only relevant specs participate:
1. **Epoch boundary** — only specs from the current session (since last `writ finish`)
2. **Commit state** — only uncommitted specs
3. **Genesis tree** — structural filtering against the common ancestor

Merge ordering is optimized automatically: specs that touch disjoint files merge first, minimizing conflict complexity for the overlapping cases that follow.

Results are stored in the object store as **shadow state** — not written to disk. Convergence can run while agents are still working without disturbing anyone's files. Disk materialization only happens at `writ finish`.

### When Convergence Runs

- **At `writ spec done`** — checks for overlapping work with other completed specs
- **At `writ finish`** — final backstop that catches anything remaining before git commit

Every resolution is confidence scored. High confidence (≥ 0.85) auto-resolves. Low confidence escalates with structured context so orchestrator agents can resolve conflicts programmatically, or surface them to a human with all the data they need.

```bash
# Manual convergence with strategy selection
writ converge-all --apply --strategy escalate
```

### Integration Risk

Before starting work or after convergence, check the risk level:

```bash
writ context --format human
# INTEGRATION RISK: HIGH (score: 65)
#   - 7 diverged specs (>3)
#   - file touched by 11 agents (>=5)
#   - 6 scope violations (>5)
```

See the [convergence deep dive](https://andrew-garfield101.github.io/writ/concepts/convergence.html) for the full pipeline architecture.

## Workspaces

Most multi agent work happens in the same directory with spec scoped sealing and convergence handling overlaps automatically. Workspaces exist for the rare case where agents need physical file isolation — competing rewrites of the same code, where each agent's changes would break the other's in-progress work.

```bash
# Level 2: agents rewrite the same code in fundamentally different ways
writ task "rewrite auth: PKCE approach"     # creates isolated workspace
writ task "rewrite auth: implicit approach"  # creates isolated workspace
# launch agents in workspace directories — each has their own copy
writ finish                                  # auto-converges + commits
```

`writ task` creates a spec, a workspace directory with a full project copy, and prints a launch command. Agents in workspaces use the same three commands: `context`, `seal`, `spec done`. `writ finish` auto-converges workspaces before committing to git.

Same number of steps as the git workflow everyone already knows:

| Git | Writ | What Improves |
|-----|------|---------------|
| `git checkout -b feature` | Same directory (or `writ task` for Level 2) | No branches needed for most multi agent work |
| `git add && git commit` | `writ seal` | Agent identity, spec linkage, immutable chain |
| `git merge` | `writ finish` (auto-converges) | Structure aware, auto resolves independent changes |

When to use workspaces vs same directory: if agents touch different files or add to the same file, same directory works. If agents rewrite the same function body in different ways, use `writ task`. See the [workspaces guide](https://andrew-garfield101.github.io/writ/guides/workspaces.html) for the full architecture and advanced commands.

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

Agents can do the same thing programmatically. If an agent detects something went wrong — tests failing, scope violations piling up — it can walk the seal history and self correct:

```python
seals = repo.log(limit=10)
for s in seals:
    if s["verification"].get("tests_passed", 0) > 0:
        repo.restore(s["id"])
        repo.seal(summary="Rolled back — tests were failing")
        break
```

Restoring doesn't delete history. The old seals still exist in the log. `writ log --all` always shows the complete record. And since writ works alongside git, you always have the git safety net underneath.

## Security

Writ is built for environments where multiple autonomous agents have write access to the same codebase. That demands security guarantees that traditional VCS was never designed for — whether you're running a zero trust setup where every agent action is verified, a fully autonomous system where agents operate without human oversight, or anything in between.

**Cryptographic integrity.** Every seal is chained to its predecessor via BLAKE3 hashes — tamper with any checkpoint and the entire chain breaks. Ed25519 digital signatures authenticate who created each seal. `writ verify` validates the full history in one command.

**Agent identity.** Every agent is a registered entity with a unique keypair, trust level, role, and scope constraints. Trust levels (full, standard, restricted, untrusted) directly affect convergence behavior — untrusted or newly introduced agents receive lower confidence caps, limiting their influence on automated merge decisions. Agents can be suspended or revoked without deleting their history, and all seals created after a compromise timestamp are automatically flagged for review.

**Scope enforcement.** Specs declare which files they own. When an agent seals changes to files outside its spec's scope, writ flags the violation — in context output, in the audit log, and optionally as a hard rejection. No more agents silently modifying files they shouldn't touch.

**Audit trail.** An append only security event log records scope violations, signature failures, agent revocations, and convergence anomalies. Events are severity classified (info, warning, critical) with configurable retention.

```bash
writ verify                                # validate full seal chain integrity
writ security events --severity warning    # review security audit log
```

See the [security model](https://andrew-garfield101.github.io/writ/concepts/security-model.html) for the full trust framework, scope enforcement rules, and audit system.

## Lifecycle and Storage

AI models update. Tooling shifts. What agents produce today may need to be rolled back tomorrow. A VCS for agentic development needs more than immutable history — it needs active lifecycle management that keeps repositories healthy as projects scale and models evolve.

**Spec lifecycle.** Specs progress through a managed lifecycle: active, stale, completed, cancelled, archived. Stale detection is automatic — `writ context` warns when specs go inactive so nothing falls through the cracks.

**Storage aware.** Writ tracks storage usage across categories (seals, working state, security events, keys) and alerts when usage approaches configured budgets. Seals are never refused — immutable history is sacred.

**Safe cleanup.** `writ gc` generates a plan, shows what it will do, and asks before executing. GC only cleans expired working state, archived specs past retention, and old security events. Every cleanup action is recorded in an audit trail.

**Deployment profiles.** Preconfigured storage budgets and retention periods for different environments — from a 500MB Raspberry Pi to unlimited enterprise.

**Workflow modes.** Two modes that scale from solo developer to enterprise fleet. `user` mode (default): you run `writ finish` manually. `auto` mode: fully autonomous commit pipeline with configurable safety rails — test verification, max specs per commit, branch targeting. Configure globally or per project. See the [configuration reference](https://andrew-garfield101.github.io/writ/reference/configuration.html) for details.

```bash
writ gc status                             # storage breakdown + stale spec warnings
writ gc run --dry-run                      # preview cleanup plan without executing
```

## Python SDK

```python
import writ

repo = writ.Repository.open(".")
ctx = repo.context()                                    # project state
repo.plan(["Task A", "Task B"])                         # batch spec creation
result = repo.seal(summary="work done", agent_id="worker-3")  # checkpoint (auto-scoped)
repo.spec_done(spec_id="a3f7b2c1")                     # mark complete
repo.finish()                                           # converge + commit
```

Full programmatic access to every writ operation. Built via PyO3 bindings to the Rust core — same code path as the CLI, not a separate implementation. See the [Python SDK reference](https://andrew-garfield101.github.io/writ/reference/python-sdk.html) for the full API.

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

Each tool is a thin wrapper around the CLI. Same behavior, same output, same enforcement. Auto-scoping works through MCP — agents seal and call spec done without passing explicit IDs when they have one claimed spec. `writ_context` defaults to TOON format. The MCP server is the CLI, just reachable through the protocol agents already speak.

See the [MCP server guide](https://andrew-garfield101.github.io/writ/guides/mcp-server.html) for the full setup walkthrough and tool reference.

## CLI Reference

```
writ init                             # guided setup (git detect + bridge import + framework integration)
writ init --yes                       # non-interactive setup (CI safe, accept all defaults)
writ init --profile production        # setup with a deployment profile
writ uninit [--force]                 # clean removal of writ from the project
writ plan "task1" "task2" "task3"     # batch spec creation (or -f tasks.txt, or stdin)
writ task "description"              # create task: spec + workspace (Level 2 isolation)
writ task list                       # show all active tasks and status
writ seal -s "..."                   # checkpoint (auto-scoped to claimed spec)
writ seal -s "..." --spec ID          # checkpoint (explicit spec)
writ context [--spec ID] [--format]   # structured context (json, toon, human, brief)
writ status [--watch] [--completed]   # fleet overview: agents, specs, progress
writ watch [--interval N]            # live seal event monitoring
writ log [--all] [--spec ID]          # seal history (--all includes diverged branches)
writ summary --format commit          # one-line commit message with full provenance
writ summary --format pr              # full PR description with spec/agent breakdown
writ finish [--strategy per-spec]      # converge + commit to git
writ finish --yes                     # accept defaults, no prompts
writ finish --dry-run                 # preview without committing
writ finish --cleanup                 # auto-clean workspace directories after commit
writ converge LEFT RIGHT [--apply]    # two-spec convergence
writ converge-all --apply --strategy  # merge all diverged specs
writ converge-workspaces a b         # merge across named workspaces
writ verify                           # verify full seal chain integrity
writ verify --seal SEAL_ID            # verify a specific seal
writ security events [--severity]     # security audit log with filtering
writ spec add "description"           # create a spec (auto-generated hash ID)
writ spec claim ID                    # claim an unclaimed spec (auto-claims on first seal)
writ spec status [--state active]     # show specs, filtered by lifecycle state
writ spec done [ID] [-s "..."]        # mark spec complete (ID optional with auto-scoping)
writ spec cancel ID                   # cancel a spec
writ spec reopen ID                   # reopen completed spec
writ gc status                        # storage breakdown + stale spec warnings
writ gc run [--dry-run] [--yes]       # generate and execute cleanup plan
writ gc storage                       # detailed storage usage by category
writ state                            # working directory changes
writ diff [--spec ID] [--stat]        # spec aware diff with filtering
writ show SEAL_ID [--diff]            # inspect a seal
writ restore SEAL_ID                  # restore to a seal's state
writ bridge import                    # import git state as baseline
writ push / pull                      # sync with remotes
writ workspace create <name>          # create isolated parallel workspace
writ workspace list                   # list all workspaces
writ workspace status [name]          # workspace details and progress
writ workspace delete <name>          # remove workspace, preserve history
writ spec assign <id> --workspace <name>  # scope spec to workspace
writ spec unassign <id>               # make spec globally visible
writ mcp-serve                        # start MCP server
writ mcp-install [--desktop]          # generate MCP config
writ doctor [--json]                  # repo health check (8 diagnostic checks)
```

## Architecture

```
writ/
├── crates/
│   ├── writ-core/    # Rust: objects, index, seals, specs, diff, context, convergence, bridge
│   ├── writ-cli/     # CLI (clap): init, seal, context, converge, summary, restore, mcp-serve, ...
│   ├── writ-mcp/     # MCP server (rmcp): 21 tools, CLI passthrough, stdio transport
│   └── writ-py/      # Python bindings (PyO3): full CLI as Python API
```

**Storage:** Content addressable object store (SHA-256). Atomic writes (temp + fsync + rename). Hash verification on retrieve. Advisory file locking for concurrency.

**Integrity:** BLAKE3 hash chains link every seal to its predecessor. Ed25519 digital signatures authenticate authorship. `writ verify` validates the entire history in one command.

**Test coverage:** 2,096+ Rust tests, 834+ Python tests, 41 YAML scenario tests across unit, integration, and end to end layers.

### Framework Support

| Framework | Detection | What Gets Installed |
|-----------|-----------|-------------------|
| **Claude Code** | `CLAUDE.md` or `.claude/` exists | Writ workflow in `CLAUDE.md`, 20 slash commands in `.claude/commands/`, 21 MCP tools via `.mcp.json`, SessionStart hook |
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

- **Spec aware resolution.** Convergence that uses writ's first class spec metadata — file scope, acceptance criteria, semantic intent — to make merge decisions that no other VCS has the context for. Deterministic convergence ships in v0.1.0. Spec aware resolution planned for a future release
- **LLM assisted convergence.** Direct LLM API integration for conflict resolution when deterministic patterns can't resolve. Composition only — the LLM can select, reorder, and combine from existing code, never generate novel content. Planned for a future release

See [CHANGELOG.md](CHANGELOG.md) for shipped features and version history.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, test commands, and contribution guidelines.

## License

AGPL-3.0-only. See [LICENSE](LICENSE) for details.

For commercial licensing inquiries, contact the project owner.

---

writ-vcs™ © 2026 Andrew Garfield
