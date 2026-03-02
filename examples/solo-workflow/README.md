# Solo Workflow: Using Writ as a Developer

This example walks through using writ as a single developer working alongside git.
You'll see how writ gives you structured checkpoints, rollback, and a full audit trail
that goes beyond what `git commit` provides.

## Prerequisites

- `writ` CLI installed (`pip install writ-vcs` or `cargo install --path crates/writ-cli`)
- A terminal

## Setup

```bash
# Create a fresh project
mkdir my-project && cd my-project
git init

# Install writ alongside git
writ install
```

You now have both `.git/` and `.writ/` in your project. They coexist — writ doesn't
replace git, it layers on top.

## Step 1: Write some code and seal it

Create a simple Python module:

```bash
cat > calculator.py << 'EOF'
"""A simple calculator module."""


def add(a: float, b: float) -> float:
    return a + b


def subtract(a: float, b: float) -> float:
    return a - b
EOF
```

Now seal your work. A seal is like a commit, but lighter — designed for frequent
checkpointing during development:

```bash
writ seal -s "calculator: add and subtract" --agent dev
```

## Step 2: Check your context

`writ context` shows the full project state — what's been done, what's changed,
what files are in play:

```bash
writ context
```

This is what makes writ valuable for AI agents: one call gives them everything
they need to understand the project state.

## Step 3: Add more functionality and seal again

```bash
cat >> calculator.py << 'EOF'


def multiply(a: float, b: float) -> float:
    return a * b


def divide(a: float, b: float) -> float:
    if b == 0:
        raise ValueError("Cannot divide by zero")
    return a / b
EOF

writ seal -s "calculator: added multiply and divide" --agent dev
```

## Step 4: View your seal history

```bash
writ log
```

You'll see both seals with timestamps, agent IDs, and summaries. Each seal
captures the exact state of your working directory at that moment.

## Step 5: Make a mistake and roll back

Let's say you accidentally break something:

```bash
echo "BROKEN CODE" > calculator.py

# Check the damage
writ state
```

No problem — restore to your last good seal:

```bash
# See your seals
writ log

# Restore to the previous seal (copy the seal ID from the log)
writ restore <SEAL_ID>

# Verify it's back
cat calculator.py
```

Your code is restored. The broken state is gone, but the seal history is preserved —
nothing is ever deleted.

## Step 6: Finish and commit to git

When you're done, `writ finish` generates a commit message from your full
session history and commits to git:

```bash
writ finish
```

Or do it manually with a generated summary:

```bash
git add -A
git commit -m "$(writ summary --format commit)"
```

## Step 7: Verify your seal chain

Every seal includes a cryptographic hash chain. You can verify the integrity
of your entire history:

```bash
writ verify --chain
```

This proves no seals were tampered with — every checkpoint is cryptographically
linked to the one before it.

## Key Takeaways

- **Seals are lightweight** — seal early and often, unlike commits
- **Context is powerful** — one call gives the full project state
- **Restore is safe** — roll back without losing history
- **Chain integrity** — cryptographic proof that nothing was tampered with
- **Git integration** — `writ finish` bridges back to git seamlessly
