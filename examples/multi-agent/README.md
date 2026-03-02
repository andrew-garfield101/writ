# Multi-Agent Convergence: Parallel Work Without Merge Conflicts

This example demonstrates writ's core differentiator: **convergence**.
Multiple agents work on the same codebase in parallel, and writ
intelligently merges their work — resolving import conflicts, composing
function additions, and flagging true incompatibilities.

## Prerequisites

- `writ` CLI installed (`pip install writ-vcs` or `cargo install --path crates/writ-cli`)
- Python 3.8+ (for the demo script)
- `pip install writ-vcs` (for Python bindings)

## Quick Start

Run the included demo script to see convergence in action:

```bash
python demo.py
```

This script simulates two agents working in parallel on the same project,
then converges their work automatically.

## What the Demo Does

### 1. Sets up a baseline project

A simple Python web app with `app.py` and `models.py`.

### 2. Agent A: Backend developer

Adds authentication routes and a `Session` model — seals their work
to the `backend` spec.

### 3. Agent B: API developer

Adds data validation helpers and a `Schema` model to the same files —
seals their work to the `api` spec.

### 4. Convergence

Both agents modified `models.py` (adding different classes) and `app.py`
(adding different routes). With git, this would be a merge conflict.
With writ, convergence:

- **Composes the models**: Both `Session` and `Schema` appear in the final file
- **Merges the routes**: Both sets of routes are preserved
- **Deduplicates imports**: Shared imports appear once
- **Verifies traceability**: Proves the merged output contains only content from the inputs

### 5. Verification

The seal chain is verified to prove cryptographic integrity across
the entire history, including the convergence seal.

## Step-by-Step (Manual)

If you want to walk through it yourself instead of running the script:

```bash
mkdir demo-project && cd demo-project
writ install
```

### Create specs for each agent

```bash
writ spec add --id backend --title "Backend Auth"
writ spec add --id api --title "API Layer"
```

### Agent A works and seals

```bash
# (make changes to app.py and models.py)
writ seal -s "added auth routes and Session model" --agent agent-a --spec backend
```

### Agent B works and seals

```bash
# (make different changes to the same files)
writ seal -s "added validation and Schema model" --agent agent-b --spec api
```

### Check for divergence

```bash
writ context
```

You'll see `convergence_recommended: true` and an `integration_risk` score.

### Converge

```bash
# Preview what will be merged
writ converge-all --dry-run

# Apply the merge
writ converge-all --apply --strategy most-complete
```

### Verify the result

```bash
writ verify --chain
writ context
```

The context now shows `convergence_recommended: false` — all branches
are merged and the project is in a clean state.

## Why This Matters

With git worktrees, parallel agent work requires a human to resolve merge
conflicts manually. With writ:

- **Definition-aware merging** understands functions, classes, and imports
- **Traceability verification** proves no hallucinated content was added
- **Cryptographic chain** provides full provenance of who changed what
- **Automatic composition** handles the common case (additive changes) without human intervention
- **Escalation** surfaces true conflicts that need human judgment
