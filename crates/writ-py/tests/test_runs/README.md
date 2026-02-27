# Layer 5: Live Agent Test Runs

Formalized test run framework for multi-agent convergence testing with real or scripted agents.

## Quick Start

```bash
cd crates/writ-py/tests
source ../../../.venv/bin/activate

# Fast mode — scripted agents, ~1 second
python -m test_runs.run tr22 --mode scripted

# Verbose — see all phase details
python -m test_runs.run tr22 --mode scripted -v

# Live mode — real Claude Code agents, ~20-30 min
# You'll see and approve each permission prompt in the terminal.
# Agents are scoped to a temp workspace via --directory.
python -m test_runs.run tr22 --mode live

# Live mode with no permission prompts (ONLY in disposable environments)
python -m test_runs.run tr22 --mode live --trust-live-agents
```

### Live mode security

By default, live agents run with **normal Claude Code permissions** — you see every action
and approve or deny it. Agents are scoped to a temporary workspace directory via `--directory`
so they can't wander into your home directory, photos, etc.

The `--trust-live-agents` flag bypasses all permission prompts. **Only use this in
disposable/isolated environments** (VMs, containers, CI runners).

## How It Works

Each test run (TR) follows 5 phases:

```
Phase 1: Setup      → scaffold workspace, writ init, register specs
Phase 2: Agents     → run each agent, restore-to-baseline between
Phase 3: Converge   → converge_all with strategy from charter
Phase 4: Validate   → run all checks, collect pass/fail
Phase 5: Report     → write results.yaml, print summary
```

## Two Agent Modes

| Mode | Speed | Deterministic | Use Case |
|------|-------|---------------|----------|
| `scripted` | ~1s | Yes | Framework dev, CI, regression |
| `live` | ~20-30min | No | Per-sprint exploratory testing |

**Scripted** agents are Python functions that make predefined file changes and seal. Same approach as Layer 4 YAML scenarios but with richer behavior.

**Live** agents spawn real Claude Code sessions via `claude -p` with full permissions. Non-deterministic — each run produces different code. This is where new bugs are discovered.

## Creating a New TR

1. Create a directory: `test_runs/tr23/`
2. Write a charter: `tr23/charter.yaml` (see tr22 for template)
3. For scripted mode: write `tr23/agents_scripted.py`
4. For live mode: write prompts in `tr23/prompts/{agent-name}.md`
5. Run: `python -m test_runs.run tr23 --mode scripted`

## Charter Format

```yaml
tr: 23
title: "My Test Run"
domain: "Task management app"    # scaffold to use
agents:
  - name: agent-a
    spec: feature-a
    title: "Feature A"
    file_scope: ["src/*.py"]     # optional scope constraint
convergence:
  strategy: escalate
checks:
  convergence:
    - type: not_degraded
    - type: definitions_preserved
      file: models.py
      definitions: [User, Task]
  security:
    - type: chain_valid
  metadata:
    - type: post_convergence_clean
  quality:
    - type: python_syntax
      file: app.py
```

## Available Checks

### Convergence
- `not_degraded` — convergence quality not degraded
- `is_clean` — no unresolved conflicts
- `no_escalations` / `has_escalations` — escalation presence
- `definitions_preserved` — named classes/functions present in file
- `file_contains` / `file_not_contains` — substring match
- `file_exists` — file on disk

### Security
- `chain_valid` — seal chain verification passes
- `chain_no_failures` — zero failures in chain
- `seals_have_hashes` — all seals have content_hash + chain_hash

### Metadata
- `post_convergence_clean` — diverged_branches=0, convergence_recommended=false
- `spec_exists` — spec registered in repo
- `context_has_field` — context output contains named field

### Quality
- `python_syntax` — Python file compiles
- `python_import_order` — stdlib before third-party before local
- `no_duplicate_imports` — no exact duplicate import lines
- `bracket_balance` — parens/brackets/braces balanced

## Output

Results are saved to `tr{N}/results.yaml` with:
- Phase timing
- Per-agent results (seal IDs, files changed, duration)
- Per-check pass/fail with details
- Summary counts
- Issues found (auto-populated from failed checks)

## Regression Capture

When a TR finds a bug, capture it as a Layer 4 YAML scenario:

```
tr22/
├── charter.yaml
├── results.yaml           # auto-generated
└── regression/
    └── tr22_bug_name.yaml  # manually created Layer 4 scenario
```

Copy the regression scenario to `tests/scenarios/convergence/` so it runs on every commit.
