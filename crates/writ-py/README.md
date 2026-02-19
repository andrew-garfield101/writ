# writ — AI-native version control

Structured checkpoints, spec-driven development, and multi-agent coordination for agentic AI workflows.

## Install

```bash
pip install writ-vcs
```

## Quick start

```python
import writ

# One-command setup (detects git, imports baseline, creates .writignore)
result = writ.Repository.install(".")

# Or manual init
repo = writ.Repository.init(".")

# Create a seal (structured checkpoint)
repo.seal(
    summary="implemented auth module",
    agent_id="implementer",
    agent_type="agent",
    spec_id="auth",
    status="in-progress",
)

# Get structured context (optimized for LLM consumption)
ctx = repo.context()

# Spec-driven workflow
repo.add_spec("auth", "Authentication", "JWT-based auth system")
repo.update_spec("auth", status="complete")
```

## Agent SDK

Higher-level abstractions for common agent patterns:

```python
from writ.sdk import Agent, Phase, Pipeline

with Agent("implementer", spec_id="auth") as agent:
    ctx = agent.context  # project state loaded automatically
    # ... do work ...
    agent.seal("implemented token refresh")
```

## CLI

```bash
writ install                                    # one-command project setup
writ seal -s "added auth" --agent impl --spec auth  # structured checkpoint
writ context                                    # LLM-optimized project state
writ log                                        # seal history
writ bridge export --branch writ/output         # export to git
```

## Links

- [Repository](https://github.com/agarfield/writ)
- [Full documentation](https://github.com/agarfield/writ#readme)
