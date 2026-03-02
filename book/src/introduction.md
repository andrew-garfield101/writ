# Writ

**AI native version control for agentic systems.**

Writ is a version control system designed for multi agent workflows. It sits alongside git, not instead of it. Multiple AI agents work on the same codebase in parallel, each sealing (checkpointing) their work independently. When their work diverges, writ's convergence engine merges it back together using structural awareness, not just line based diffing.

> **Note:** Writ is in early alpha. The core is stable and thoroughly tested (1,350+ Rust tests, 400+ Python tests), but the API may change before 1.0. We welcome feedback and contributions.

## The Problem

The ecosystem's current answer to multi agent work is "give each agent a git worktree." That solves **isolation**, keeping agents from stepping on each other. But it doesn't solve **convergence**, bringing their work back together intelligently.

When five agents work concurrently and three of them touch the same file, git merge sees conflicting lines and gives up. Writ's convergence engine understands code structure. It knows the difference between an import, a function definition, and a statement. Two agents both adding imports to the same file isn't a conflict. It's two additive changes that compose naturally.

**Worktrees solve isolation. Writ solves convergence.**

## What Writ Provides

| Capability | What It Does |
|-----------|-------------|
| **Seals** | Structured checkpoints with agent identity, spec linkage, test results, and status lifecycle. Every seal is cryptographically chained to its predecessor. |
| **Specs** | Requirements with status, dependencies, file scope, and acceptance criteria. Agents know what they're working on and what files they own. |
| **Context** | One call returns everything an agent needs: specs, seals, working state, agent activity, file contention, integration risk. Designed for LLM context windows. |
| **Convergence** | A six phase pipeline that merges meaning, not lines. Language aware analysis for Python, Rust, Go, TypeScript, and JavaScript. Confidence scored, fully auditable. |
| **Security** | BLAKE3 hash chains, Ed25519 signatures, agent trust levels, scope enforcement, content traceability, and append only audit logging. |
| **Git Bridge** | Bidirectional sync between writ and git. `writ install` imports your git state. `writ finish` commits back with full provenance. |

## How It Works

```
 Human world                Agent world                  Human world
+-----------+  writ install  +------------------+  writ finish   +--------------+
| git repo  | ------------->| agents work in   | ------------> | git commit   |
| (branch)  |               | writ: specs,     |               | with full    |
|           |               | seals, context   |               | provenance   |
+-----------+               +------------------+               +--------------+
```

1. A human checks out a branch and runs `writ install`
2. Agents work in writ: creating seals, checking context, converging changes
3. When done, `writ finish` generates a git commit with a detailed provenance message

## Quick Example

```bash
# Set up writ in any project
writ install

# Agents seal checkpoints as they work
writ seal -s "added auth module" --agent implementer --spec auth
writ seal -s "tests passing" --agent tester --spec auth --tests-passed 42

# One call gives agents everything they need
writ context --format json

# When done, commit back to git
writ finish
```

## Next Steps

- **[Installation](getting-started/installation.md)** to get writ on your machine
- **[Quickstart](getting-started/quickstart.md)** for a five minute walkthrough
- **[Concepts](concepts/seals-vs-commits.md)** to understand the data model
- **[Convergence](concepts/convergence.md)** to see the core value proposition in depth
