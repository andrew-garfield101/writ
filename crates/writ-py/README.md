# writ-vcs

**AI-native version control for agentic development.**

Structured checkpoints, spec-driven development, and multi-agent coordination for agentic AI workflows.

## Install

```bash
pip install writ-vcs
```

## Quick start

```bash
# Human sets up the project (once)
writ init

# Agents work. Each agent uses three commands:
writ spec add "task description"    # create a task
writ seal -s "what I did"           # checkpoint work
writ spec done                      # mark task complete

# Human commits everything to git (once)
writ finish
git push
```

That's the full workflow. `writ init` detects your environment and sets up everything automatically. Agents discover writ via MCP tools, slash commands, or CLI. `writ finish` converges overlapping work and commits to git.

## Python SDK

```python
import writ

repo = writ.Repository.open(".")
ctx = repo.context()                                    # full project state
repo.seal(summary="work done", agent_id="worker-3")    # checkpoint (auto-scoped)
repo.finish()                                           # converge + commit
```

## CLI

```bash
writ init                                          # guided project setup
writ context                                       # full project state (one call)
writ context --format toon                         # token-optimized output
writ spec add "JWT-based auth with token refresh"  # create a spec
writ seal -s "added auth" --spec auth              # structured checkpoint
writ spec done                                     # mark spec complete
writ status                                        # fleet overview
writ finish                                        # converge + commit to git
writ log                                           # seal history
writ verify                                        # cryptographic chain validation
```

## Features

- **Structured context in one call.** `writ context` returns specs, seals, working state, file contention, integration risk. One call, one structured response.
- **Convergence engine.** Merges overlapping agent work using genesis trees as base. Auto resolves non-conflicting changes. Escalates real conflicts with confidence scores.
- **Cryptographic integrity.** BLAKE3 hash chains and Ed25519 signatures on every seal. `writ verify` validates the full history.
- **Spec-driven workflow.** Specs are structured requirements that agents work against. Task tracking with lifecycle, agent claiming, and genesis snapshots.
- **Git round trip.** `writ finish` converges outstanding work and commits to git with full provenance.
- **MCP server.** Native MCP server built in Rust. 21 tools available through the protocol agents already speak.
- **TOON format.** Token-optimized output that saves 12% over JSON per call. Spec-scoped context saves 65% at fleet scale.

## Links

- [Documentation](https://andrew-garfield101.github.io/writ/)
- [Repository](https://github.com/andrew-garfield101/writ)
- [Issues](https://github.com/andrew-garfield101/writ/issues)
