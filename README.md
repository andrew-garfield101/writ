# writ

**AI native version control for agentic systems.**

[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue)](LICENSE)
[![CI](https://github.com/andrew-garfield101/writ/actions/workflows/ci.yml/badge.svg)](https://github.com/andrew-garfield101/writ/actions/workflows/ci.yml)

> [!WARNING]
> Writ is in early alpha. The core is stable and thoroughly tested (1,350+ Rust tests, 400+ Python tests, 41 YAML scenario tests), but the API may change before 1.0. Use it, break it, file issues.

---

Structured checkpoints, spec driven development, and multi agent coordination. Writ works alongside git, not instead of it.

Worktrees solve isolation. **Writ solves convergence.**

## Table of Contents

- [Install](#install)
- [60 Second Demo](#60-second-demo)
- [Why Writ](#why-writ)
- [Context](#context)
- [Convergence](#convergence)
- [Security](#security)
- [Python SDK](#python-sdk)
- [CLI Reference](#cli-reference)
- [Building from Source](#building-from-source)
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

## 60 Second Demo

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
```

That's it. Human checks out a branch, agents work in writ, human gets a commit with full provenance.

```
 Human world                Agent world                  Human world
+-----------+  writ install  +------------------+  writ finish   +--------------+
| git repo  | ------------->| agents work in   | ------------> | git commit   |
| (branch)  |               | writ: specs,     |               | with full    |
|           |               | seals, context   |               | provenance   |
+-----------+               +------------------+               +--------------+
```

## Why Writ

Most multi agent tooling gives each agent a git worktree and merges via PRs. That handles **isolation**, keeping agents from stepping on each other. It doesn't handle **convergence**, bringing their work back together when they touch the same files.

Writ is built for convergence:

- **Structured checkpoints.** Every seal carries agent identity, spec linkage, test results, and task status. No conventions to hope agents follow.
- **One call context.** `writ context` returns everything an agent needs (specs, seals, working state, file contention, integration risk) in one structured JSON response. No parsing `git log` output.
- **Semantic merging.** The convergence engine understands code structure. Two agents adding imports to the same file isn't a conflict, it's two additive changes that compose naturally.
- **Cryptographic integrity.** BLAKE3 hash chains and Ed25519 signatures on every seal. Tamper with any checkpoint and the chain breaks.

## Context

The most expensive thing in an agent's workflow is building situational awareness. With git, that means multiple tool calls returning text that needs parsing. With writ:

```python
ctx = repo.context(spec="auth-migration")
```

One call. One structured dict. Everything an agent needs:

- **Spec details** with status, dependencies, file scope
- **Recent seals** with who did what, when, and which files changed
- **Working state** filtered to your spec's scope
- **Agent activity** with per agent file ownership
- **File contention** showing "hot files" touched by 2+ agents
- **Integration risk** scored 0 to 100 with contributing factors
- **Diverged branches** with convergence recommendations

## Convergence

When multiple agents work concurrently on overlapping files, traditional line based merging breaks. Writ merges **meaning**, not lines.

The convergence engine decomposes files into structural units (imports, function definitions, class bodies) and resolves conflicts at that level. Language aware analyzers for Python, Rust, Go, TypeScript, and JavaScript, with graceful fallback for everything else.

Five deterministic resolution patterns handle common cases:

| Pattern | What It Resolves |
|---------|-----------------|
| **Import Accumulation** | Both sides add imports. Union them. |
| **Non overlapping Definitions** | Both sides add functions with different names. Compose them. |
| **EOF Append** | Both sides append to end of file. Concatenate. |
| **Additive Composition** | Both sides preserved base and added content. Compose. |
| **Superset Containment** | One side contains everything the other has. Use the superset. |

Every resolution is confidence scored. High confidence (>= 0.85) auto resolves. Low confidence escalates with structured context for human or orchestrator review. Post merge verification catches structural damage (duplicate definitions, unbalanced delimiters, content loss) before bad merges reach the working tree.

```bash
writ converge-all --apply --strategy escalate
```

## Security

Built for environments where multiple autonomous agents have write access to the same codebase.

- **Cryptographic seal chains.** BLAKE3 content hashes link every seal to its predecessor. Ed25519 signatures authenticate authorship. `writ verify --chain` validates the full history.
- **Agent identity.** Registered agents with trust levels (full, standard, restricted, untrusted). Trust affects convergence confidence scoring.
- **Scope enforcement.** Agents can be constrained to specific files. Out of scope changes trigger warnings or rejections.
- **Content traceability.** Every line in merged output must trace back to an input. Novel content from bugs or hallucinations is detected and rejected.
- **Audit trail.** Append only security event log for scope violations, chain failures, agent revocations, and convergence anomalies.

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

Higher level abstractions:

```python
from writ.sdk import Agent, Phase, Pipeline

with Agent("implementer", spec_id="auth") as agent:
    ctx = agent.context
    # ... do work ...
    agent.seal("implemented token refresh", tests_passed=12)
```

## CLI Reference

```
writ install                          # one command setup
writ seal -s "..." --agent ID         # structured checkpoint
writ context [--spec ID] [--format]   # project state (json, human, brief)
writ log [--all] [--spec ID]          # seal history
writ summary --format commit|pr       # git commit messages, PR descriptions
writ finish [--full] [--dry-run]      # one command round trip to git
writ converge-all --apply --strategy  # merge all diverged branches
writ verify --chain                   # cryptographic chain verification
writ security events [--severity]     # security audit log
writ spec add --id ID --title "..."   # register a spec
writ gc status                        # storage and lifecycle overview
writ restore SEAL_ID                  # restore to any seal
writ state                            # working directory changes
writ diff                             # content level diff
writ show SEAL_ID [--diff]            # inspect a seal
```

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

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, test commands, and contribution guidelines.

## License

AGPL-3.0-only. See [LICENSE](LICENSE) for details.

For commercial licensing inquiries, contact the project maintainer.

---

writ-vcs &copy; 2026 Andrew Garfield
