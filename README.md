# writ

**AI-native version control for agentic systems.**

> *"Every minute an agent spends figuring out 'what happened before me' is wasted compute. Writ minimizes that."*
>
> *"One `writ context` call and I know who did what, which specs are complete, where branches diverged, and what files are contested. That is genuinely valuable and unlike anything available in git alone."*
>
> — AAIS_8, orchestrator agent reviewing a 14-agent, 40-seal project

Writ is version control designed for LLMs and multi-agent development. Its primitives are **specs** (not branches), **seals** (not commits), and **convergence** (not merging). It works alongside git, not instead of it.

## The round-trip

Git is the human layer. Writ is the agent layer. The bridge connects them.

```
 Human world                Agent world                  Human world
┌──────────┐  writ install  ┌─────────────────┐  writ summary   ┌──────────────┐
│ git repo │ ──────────────→│ agents work in  │ ──────────────→ │ git commit   │
│ (branch) │                │ writ: specs,    │                 │ with full    │
│          │                │ seals, context  │                 │ provenance   │
└──────────┘                └─────────────────┘                 └──────────────┘
```

```bash
# Human checks out a branch and sets up writ
git checkout -b feature/auth
writ install

# Agents work entirely in writ — sealing checkpoints as they go
writ seal -s "added auth module" --agent implementer --spec auth
writ seal -s "tests passing" --agent tester --spec auth --tests-passed 42 --status complete

# When agents finish, writ generates the commit message
writ summary --format commit | git commit -aF -

# Or a full PR description
writ summary --format pr
```

Each seal carries structured metadata: agent identity, spec linkage, task status, verification results, file changes, and tree hashes. Full provenance survives the round-trip.

## `context()`

The most expensive thing in an agent's workflow is building situational awareness. With git, that means multiple commands, each returning text that needs parsing. With writ:

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

## Quick start

```bash
# Install the CLI
cargo build --release
cp target/release/writ ~/.local/bin/

# In any git repo (or standalone)
writ install

# Agents can immediately work
writ context --format json
writ seal -s "first checkpoint" --agent my-agent --spec my-feature
```

### Python

```bash
cd crates/writ-py
python3 -m venv .venv && source .venv/bin/activate
pip install maturin && maturin develop
```

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

# Check seal hints for scope violations or ghost work
for hint in seal.get("hints", []):
    print(hint)
```

### Agent SDK

Higher-level abstractions for common agent workflows:

```python
from writ.sdk import Agent, Phase, Pipeline

with Agent("implementer", spec_id="auth") as agent:
    ctx = agent.context
    # ... do work ...
    if agent.should_seal():
        agent.seal("implemented token refresh", tests_passed=12)
```

The `Pipeline` class runs multi-phase workflows (spec-writer, implementer, tester, reviewer, integrator) and automatically generates a session summary on completion.

## Multi-agent workflow

Three agents, different specs, working concurrently. Sealing is serialized via advisory file locks, so agents queue safely.

```python
# Agent A: auth migration
repo.seal(summary="token refresh", agent_id="auth-dev", spec_id="auth-migration")

# Agent B: payments (concurrent, different spec)
repo.seal(summary="stripe integration", agent_id="pay-dev", spec_id="payments", status="complete")

# Agent C: testing (concurrent, cross-cutting)
repo.seal(summary="42 tests passing", agent_id="test-bot", spec_id="test-suite", tests_passed=42)
```

### Convergence

When specs overlap, convergence handles it. Two modes:

```bash
# Merge two specific specs
writ converge auth-migration payments --apply

# Merge ALL diverged branches at once (recommended after parallel work)
writ converge-all --apply --strategy most-recent
```

The `most-recent` strategy resolves conflicts by preferring the version from the most recently sealed branch — with warnings about discarded content. The default `three-way-merge` strategy leaves conflicts unresolved for manual/programmatic resolution.

```python
# Programmatic convergence
report = repo.converge_all(strategy="most-recent", apply=True)
print(f"Merged {len(report['merge_order'])} branches")
print(f"Auto-merged: {report['total_auto_merged']}, Resolved: {report['total_resolutions']}")
for warning in report.get("warnings", []):
    print(f"  WARNING: {warning}")
```

Conflicts are structured JSON — not `<<<<` markers — so orchestrator agents can resolve them programmatically.

### Integration risk

Before starting work or after convergence, check the risk level:

```bash
writ context --format human
# INTEGRATION RISK: HIGH (score: 65)
#   - 7 diverged branches (>3)
#   - file touched by 11 agents (>=5)
#   - 6 scope violations (>5)
```

```python
ctx = repo.context()
risk = ctx.get("integration_risk")
if risk and risk["level"] == "high":
    repo.converge_all(strategy="most-recent", apply=True)
```

### The human checks in

```bash
$ writ summary --format human

  WRIT SESSION SUMMARY
  12 spec(s) complete — 40 seal(s) by 14 agent(s)

  Specs:
    ✓ auth-migration          [complete] 5 seal(s) by auth-dev, tester
    ✓ payments                [complete] 3 seal(s) by pay-dev, reviewer
    ...

  Commit message (use `writ summary --format commit`):
    writ: 12 features complete — 40 seal(s) by 14 agent(s) (83 files)

  For full PR description: writ summary --format pr
```

## Why not just git?

Git's data model is built around **branches** (a pointer to a commit). Commits carry no structured metadata about which task they serve, which agent made them, or whether tests passed. You can bolt conventions on top, but conventions are things some agents follow and others don't.

Writ puts agent-native metadata inside the VCS, not layered on top:

| Git | Writ | What changes |
|-----|------|-------------|
| Branch | **Spec** | Structured requirement with status, dependencies, file scope, acceptance criteria |
| Commit | **Seal** | Checkpoint with agent identity, spec linkage, verification, status lifecycle |
| `git status` | `writ context` | One call returns everything an agent needs — not text to parse |
| `git merge` | `writ converge-all` | Multi-branch convergence with strategy selection and structured conflict reports |
| (nothing) | **Integration risk** | Automatic risk scoring from divergence, contention, and scope violations |
| (nothing) | **File contention** | Which files are touched by which agents, sorted by risk |
| (nothing) | **Session summary** | Auto-generated commit messages and PR descriptions from seal history |

## Architecture

```
writ/
├── crates/
│   ├── writ-core/    # Rust: objects, index, seals, specs, diff, context, convergence, bridge
│   ├── writ-cli/     # CLI (clap): install, seal, context, converge, converge-all, summary, ...
│   └── writ-py/      # Python bindings (PyO3) + Agent SDK (Pipeline, Agent, Phase)
```

**Storage:** Content-addressable object store (SHA-256). Atomic writes (temp + fsync + rename) on all state files. Hash verification on retrieve. Advisory file locking for concurrency.

**Test coverage:** 288 Rust + 210 Python = 498 total tests.

## CLI reference

```
writ install                          # one-command setup (init + git detect + bridge import + hooks)
writ context [--spec ID] [--format]   # structured context dump (json, human, brief)
writ seal -s "..." --agent ID         # create a checkpoint
writ log [--all] [--spec ID]          # seal history (--all includes diverged branches)
writ summary --format commit          # one-line commit message
writ summary --format pr              # full PR description
writ converge LEFT RIGHT [--apply]    # two-spec convergence
writ converge-all --apply --strategy  # merge all diverged branches
writ spec add --id ID --title "..."   # register a spec
writ spec status                      # show all specs and their status
writ state                            # working directory changes
writ diff                             # content-level diff
writ show SEAL_ID [--diff]            # inspect a seal
writ restore SEAL_ID                  # restore to a seal's state
writ bridge import                    # import git state as baseline
writ bridge export --pr-body          # export seals as git commits
writ push / pull                      # sync with remotes
```

## Building from source

```bash
cargo build
cargo test -p writ-core -p writ-cli

cd crates/writ-py
python3 -m venv .venv && source .venv/bin/activate
pip install maturin pytest
maturin develop
pytest tests/
```

## Roadmap

### Shipped

- **Round-trip workflow.** `writ install` for one-command setup. `writ summary --format commit` and `--format pr` for git integration.
- **Convergence engine.** Three-way merge with `converge-all`, `MostRecent` strategy, lost-content warnings, and structured conflict reports.
- **Integration risk.** Automatic risk scoring (low/medium/high) from divergence, file contention, and scope violations.
- **File contention map.** Files touched by 2+ agents surfaced in context, sorted by agent count.
- **Agent activity tracking.** Per-agent file ownership, seal counts, latest work — across all branches including diverged ones.
- **Scope enforcement.** File scope declarations on specs with violation detection at seal time and in context.
- **Ghost work detection.** Warns when a seal has 0 file changes but a non-empty summary (another agent likely captured overlapping work).
- **Session lifecycle.** `session_complete` flag and auto-generated `.writ/summary.json` / `.writ/summary.txt` when all specs finish.
- **Spec-scoped context.** Working state, pending changes, and seal history filtered to spec-relevant files.
- **Diverged branch detection.** Identifies "ghost agent" branches with per-branch recommendations.
- **Bridge import attribution fix.** Post-import index refresh ensures the first agent after bridge import only gets attributed its own changes.
- **Verification metadata.** `tests_passed`, `tests_failed`, `linted` on seals.
- **Concurrency safety.** Advisory file locking via `flock(2)`, atomic writes on all state files.
- **Git bridge.** Import/export with metadata trailers preserving full provenance.
- **Agent framework hooks.** Auto-detection and configuration for Claude Code (CLAUDE.md) and Codex (AGENTS.md).
- **Agent SDK.** `Agent`, `Phase`, `Pipeline` abstractions with auto-summary on completion.
- **Security hardening.** Path traversal protection, hash validation, input sanitization.
- **Remote sync.** `writ push` / `writ pull` for distributed workflows.

### Ahead

- **PyPI / Homebrew distribution.** Packaged install via `pip install writ` and `brew install writ`.
- **Post-convergence validation.** Consistency checks after merge (e.g., nav item parity across pages).
- **Shared file annotations.** Cross-cutting concern declarations for files every agent touches.
- **MCP server.** Model Context Protocol integration for IDE-native writ access.
- **Scale hardening.** Performance validation at hundreds of specs and thousands of seals.

## License

AGPL-3.0-only. See [LICENSE](LICENSE) for details.
