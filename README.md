# writ

**AI-native version control for agentic systems.**

> *"Git tracks files. Writ tracks intent, authorship, and verification on top of that."*
>
> *"Every minute an agent spends figuring out 'what happened before me' is wasted compute. Writ minimizes that."*
>
> — AAIS_2, after completing a full spec-driven development cycle using writ

Writ is version control designed for LLMs and multi-agent development. Its primitives are **specs** (not branches), **seals** (not commits), and **convergence** (not merging). It works alongside git, not instead of it.

## The round-trip

Git is the human layer. Writ is the agent layer. The bridge connects them.

```
 Human world                Agent world                  Human world
┌──────────┐  writ install  ┌─────────────────┐  bridge export  ┌──────────────┐
│ git repo │ ──────────────→│ agents work in  │ ──────────────→ │ git branch   │
│ (HEAD)   │                │ writ: specs,    │                 │ with commits │
│          │                │ seals, context  │                 │ + trailers   │
└──────────┘                └─────────────────┘                 └──────────────┘
                                                                       │
                                                                  git merge / PR
```

```bash
# Human commits code in git
git commit -m "initial project"

# One command sets up writ and imports the git baseline
writ install

# Agents work entirely in writ
writ seal --summary "added auth module" --agent implementer --spec auth
writ seal --summary "tests passing" --agent tester --tests-passed 42

# Export back to git when done
writ bridge export --branch writ/output --pr-body

# Human creates a PR from the branch
git log writ/output   # sees agent seals as commits with metadata trailers
```

Each exported git commit carries structured trailers: `Writ-Seal-Id`, `Writ-Spec`, `Writ-Status`, `Writ-Tests-Passed`. Full provenance survives the round-trip.

## `context()`

The most expensive thing in an agent's workflow is building situational awareness. With git, that takes 4 to 5 separate commands, each returning text that needs parsing. With writ:

```python
ctx = repo.context(spec="auth-migration")
```

One call. One structured dict. Spec details, dependency status, recent seal history, working state, pending changes, file scope, verification results, and a nudge if there are unsealed changes. That's not an incremental improvement. It's a category difference in how agents bootstrap into a task.

## Quick start

```bash
# In any git repo (or standalone)
writ install

# Agents can immediately work
writ context --format json
writ seal --summary "first checkpoint" --agent my-agent
```

### Python

```bash
cd crates/writ-py
python3 -m venv .venv && source .venv/bin/activate
pip install maturin && maturin develop
```

```python
import writ

writ.Repository.install(".")
repo = writ.Repository.open(".")
ctx = repo.context(spec="auth-migration")
repo.seal(
    summary="token refresh endpoint",
    agent_id="worker-3",
    spec_id="auth-migration",
    tests_passed=12,
)
```

### Agent SDK

Higher-level abstractions for common agent workflows:

```python
from writ_sdk import Agent, Phase

with Agent("implementer", spec_id="auth") as agent:
    ctx = agent.context
    # ... do work ...
    if agent.should_seal():
        agent.seal("implemented token refresh", tests_passed=12)
```

## Multi-agent workflow

Three agents, different specs, working concurrently. Sealing is serialized via advisory file locks, so agents queue safely.

```python
# Agent A: auth migration
with Agent("agent-auth", spec_id="auth-migration") as a:
    ctx = a.context                          # scoped to auth seals only
    # ... implements auth ...
    a.seal("token refresh endpoint")

# Agent B: payments (concurrent, different spec)
with Agent("agent-payments", spec_id="payment-refactor") as b:
    ctx = b.context                          # scoped to payments only
    # ... implements payments ...
    b.seal("stripe integration", tests_passed=8, status="complete")
```

When specs overlap, convergence handles it:

```python
report = repo.converge("auth-migration", "payment-refactor")
if report["is_clean"]:
    repo.apply_convergence(report, [])
else:
    # Conflicts are JSON dicts, not <<<< markers
    # An orchestrator agent can resolve them programmatically
    for conflict in report["conflicts"]:
        resolution = resolve(conflict)
        resolutions.append(resolution)
    repo.apply_convergence(report, resolutions)
```

The human checks in:

```bash
$ writ spec status
  > auth-migration       InProgress  (3 seals)
  v payment-refactor     Complete    (5 seals)
    db-optimization      Pending     (0 seals)
```

## Why not just git?

Git's data model is built around **branches** (a pointer to a commit). Commits carry no structured metadata about which task they serve, which agent made them, or whether tests passed. You can bolt conventions on top, but conventions are things some agents follow and others don't.

Writ puts agent-native metadata inside the VCS, not layered on top:

| Git | Writ | What changes |
|-----|------|-------------|
| Branch | **Spec** | Structured requirement with status, dependencies, file scope, acceptance criteria |
| Commit | **Seal** | Checkpoint with agent identity, spec linkage, verification metadata, tree hash |
| `git status` | `writ state` | Returns structured data (dict/JSON), not text to parse |
| `git log` | `writ context` | One call returns everything an agent needs to start working |

## Architecture

```
writ/
├── crates/
│   ├── writ-core/    # Rust: objects, index, seals, specs, diff, context, convergence, bridge
│   ├── writ-cli/     # CLI (clap)
│   └── writ-py/      # Python bindings (PyO3) + Agent SDK
```

**Storage:** Content-addressable object store (SHA-256). Atomic writes (temp + fsync + rename) on all state files. Hash verification on retrieve. Advisory file locking for concurrency.

**Test coverage:** 168 Rust + 93 Python = 261 total.

## Building from source

```bash
cargo build
cargo test --workspace --exclude writ-py

cd crates/writ-py
python3 -m venv .venv && source .venv/bin/activate
pip install maturin pytest
maturin develop
pytest tests/
```

## Roadmap

### Shipped

- **Round-trip.** `writ install` for one-command setup. `bridge export --pr-body` for structured PR descriptions.
- **Convergence.** Spec-based three-way merge with JSON conflict regions.
- **Verification.** `tests_passed`, `tests_failed`, `linted` on seals.
- **Concurrency.** Advisory file locking via `flock(2)`.
- **Git bridge.** Import/export with metadata trailers preserving provenance.
- **Context filtering.** Status and agent filters, seal nudging, changed paths.
- **Rich context.** Acceptance criteria, design notes, tech stack, dependency status, and spec progress in `context()`.
- **Agent SDK.** Agent, Phase, Pipeline abstractions for common workflows.
- **Security hardening.** Path traversal protection, hash validation, input sanitization, serde hardening.
- **Atomic storage.** Crash-safe writes on all state files.
- **Remote/shared state.** `writ push` / `writ pull` for agents across machines.

### Ahead

- **Spec-scoped HEAD.** Per-spec tip pointers for true parallel agent isolation.
- **Merge-on-seal.** Automatic conflict detection when HEAD moves during work.
- **Agent framework integrations.** Hooks for Codex, Claude Code, and other orchestrators.
- **Scale testing.** Performance validation with hundreds of specs and thousands of seals.

## License

AGPL-3.0-only. See [LICENSE](LICENSE) for details.
