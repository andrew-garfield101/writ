# Python SDK Reference

*Full API reference coming soon.*

## Installation

```bash
pip install writ-vcs
```

## Quick Reference

### Repository

```python
import writ

# Open an existing writ repository
repo = writ.Repository.open(".")

# Initialize a new repository
repo = writ.Repository.init("/path/to/project")
```

### Sealing

```python
result = repo.seal(
    summary="added auth endpoint",
    agent_id="dev-1",
    agent_type="agent",
    spec_id="auth",
    status="in-progress",
    tests_passed=12,
    tests_failed=0,
)
```

### Context

```python
# Full context
ctx = repo.context()

# Scoped to a spec
ctx = repo.context(spec="auth")
```

### Convergence

```python
report = repo.converge_all(strategy="escalate", apply=True)
```

### Verification

```python
result = repo.verify_chain()
result = repo.verify_seal("a7c2e8f4b31a")
```

### High-Level SDK

```python
from writ.sdk import Agent, Phase, Pipeline

with Agent("implementer", spec_id="auth") as agent:
    ctx = agent.context
    # ... do work ...
    agent.seal("implemented token refresh", tests_passed=12)
```

See the [Getting Started guide](../getting-started/quickstart.md) for usage examples.
