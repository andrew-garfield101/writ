# Your First Convergence

This walkthrough demonstrates writ's core value: merging work from multiple agents intelligently.

## The Scenario

Two agents work in parallel on different specs. Both touch some of the same files. When they're done, their work needs to be merged.

## Setup

Start with a project that has writ installed:

```bash
mkdir demo && cd demo
git init && echo "# Demo" > README.md && git add . && git commit -m "init"
writ install
```

Create two specs:

```bash
writ spec add --id backend --title "Backend API"
writ spec add --id frontend --title "Frontend Components"
```

## Agent A: Backend Work

Agent A adds a utility module and updates the main application:

```python
# utils.py (new file)
def validate_token(token: str) -> bool:
    return len(token) > 0

def hash_password(password: str) -> str:
    import hashlib
    return hashlib.sha256(password.encode()).hexdigest()
```

```bash
writ seal -s "added auth utilities" --agent backend-dev --spec backend
```

## Agent B: Frontend Work

Meanwhile, Agent B adds its own utility functions and also updates the main application:

```python
# utils.py (new file, different functions)
def format_date(timestamp: float) -> str:
    from datetime import datetime
    return datetime.fromtimestamp(timestamp).isoformat()

def sanitize_input(text: str) -> str:
    return text.strip().replace("<", "&lt;").replace(">", "&gt;")
```

```bash
writ seal -s "added display utilities" --agent frontend-dev --spec frontend
```

## The Divergence

Both agents created a `utils.py` with different functions. In git, this would be a merge conflict. In writ:

```bash
writ context --format human
```

Context will show:
- Two diverged branches (backend, frontend)
- `convergence_recommended: true`
- Integration risk assessment

## Converge

```bash
writ converge-all --apply --strategy escalate
```

Writ's convergence engine analyzes the conflict structurally:

1. **Phase 1 (Structural Diff):** Decomposes both versions into structural units (imports, function definitions)
2. **Phase 2 (Classification):** Both sides added non overlapping function definitions
3. **Phase 3 (Pattern Resolution):** The `NonOverlappingDefinitions` pattern fires at 0.92 confidence, both sides' functions are composed into one file

The merged `utils.py` contains all four functions from both agents:

```python
def validate_token(token: str) -> bool:
    return len(token) > 0

def hash_password(password: str) -> str:
    import hashlib
    return hashlib.sha256(password.encode()).hexdigest()

def format_date(timestamp: float) -> str:
    from datetime import datetime
    return datetime.fromtimestamp(timestamp).isoformat()

def sanitize_input(text: str) -> str:
    return text.strip().replace("<", "&lt;").replace(">", "&gt;")
```

No manual intervention. No conflict markers. Both agents' contributions preserved.

## When Conflicts Are Real

If both agents had modified the *same* function body differently, that's a genuine conflict. Writ escalates it with full context instead of producing `<<<<<<<` markers:

```json
{
  "escalations": [{
    "file": "utils.py",
    "conflict_type": "BothModified",
    "region": "validate_token function body",
    "left_version": "...",
    "right_version": "...",
    "confidence": 0.35,
    "recommendation": "Manual review required"
  }]
}
```

Structured data, not text to parse. An orchestrator agent can resolve this programmatically.

## What Made This Work

The key insight: multi-agent work is fundamentally **additive**. Agents build complementary features, not competing implementations. Writ's convergence engine is built around this principle.

It knows that:
- Two agents adding different imports = **compose them**
- Two agents adding different functions = **compose them**
- Two agents appending to the end of a file = **concatenate**
- Two agents changing the same line differently = **real conflict, escalate**

## Next Steps

- **[Convergence](../concepts/convergence.md)** for the full pipeline breakdown
- **[Multi Agent Workflow](../guides/multi-agent-workflow.md)** for production patterns
- **[Convergence Resolution](../guides/convergence-resolution.md)** for handling escalations
