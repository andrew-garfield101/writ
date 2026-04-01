# Your First Convergence

This walkthrough demonstrates the core problem writ solves: merging work from multiple agents who touch the same files. Not with line based diffing, but with sealed histories and genesis trees.

## The Scenario

Two agents work in parallel on different specs. Both touch some of the same files. When they're done, their work needs to be merged. In git, this produces conflict markers. In writ, it composes naturally.

## Setup

Start with a project that has writ initialized:

```bash
mkdir demo && cd demo
git init && echo "# Demo" > README.md && git add . && git commit -m "init"
writ init
```

Create two specs:

```bash
writ spec add "Backend API"
writ spec add "Frontend Components"
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
writ seal -s "added auth utilities" --spec backend
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
writ seal -s "added display utilities" --spec frontend
```

## The Divergence

Both agents created a `utils.py` with different functions. In git, this is a conflict. Two sides changed the same file — give up, produce markers, make a human figure it out.

```bash
writ context --format human
```

Context shows:
- Two diverged branches (backend, frontend)
- `convergence_recommended: true`
- Integration risk assessment

## Converge

```bash
writ converge-all --apply --strategy escalate
```

Writ's convergence engine analyzes the conflict:

1. **Pool filter** selects both specs for convergence (both have sealed changes to the same file)
2. **Genesis tree merge** uses each spec's genesis snapshot as the common ancestor — since both added new content to the same file, the merge identifies non overlapping additions
3. **Confidence scoring** rates the merge at high confidence — both sides added independent functions with no overlap, so they compose cleanly

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

No manual intervention. No conflict markers. Both agents' contributions preserved. The engine composed additive changes instead of forcing a choice.

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

Structured data, not text to parse. An orchestrator agent can resolve this programmatically — no regex parsing of conflict markers required.

## Why This Matters

This is a two agent demo. The principle scales. When you have ten agents working across five specs, all touching overlapping files, the merge problem doesn't scale linearly — it explodes combinatorially. Writ's convergence engine handles this by:

1. **Optimizing merge order** — disjoint specs merge first, reducing conflict surface for later merges
2. **Composing additive changes** — the common case in multi agent work where agents build complementary features
3. **Escalating real conflicts** — with structured context that agents or humans can act on

Giving each agent a git worktree solves isolation. This is what solves convergence.

## Next Steps

- **[Convergence](../concepts/convergence.md)** for the deep dive on how writ merges agent work
- **[Multi Agent Workflow](../guides/multi-agent-workflow.md)** for production patterns
- **[Convergence Resolution](../guides/convergence-resolution.md)** for handling escalations
