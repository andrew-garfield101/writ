# Quickstart

Get from zero to a working writ workflow in five minutes.

## Prerequisites

- A project directory (with or without git)
- Writ installed ([see Installation](installation.md))

## 1. Install Writ in Your Project

Navigate to your project and run:

```bash
cd my-project
writ install
```

Output:

```
initialized writ repository in .writ/
created .writignore
git: main @ a3f8b2c1
imported git baseline: 47 file(s), seal d81a5736e16d
tracked: 47 file(s)
```

This creates a `.writ/` directory alongside your existing `.git/` directory. If you have a git repo, writ imports the current state as a baseline seal. Your git workflow is untouched.

## 2. Create a Seal

A **seal** is writ's version of a commit, but with structured metadata. After making some changes to your project:

```bash
writ seal -s "added authentication endpoint" --agent dev-1 --spec auth
```

Output:

```
sealed 3 file(s)
seal: a7c2e8f4b31a
agent: dev-1
spec: auth
status: in-progress
```

Key differences from `git commit`:
- `--agent` identifies who created this checkpoint
- `--spec` links it to a requirement
- Status tracking is built in (`in-progress` by default, `complete` when done)

## 3. Check Context

`writ context` returns everything an agent needs to understand the current project state in one call:

```bash
writ context --format human
```

This shows:
- Active specs and their status
- Recent seals with who did what
- Working directory changes
- File contention (files touched by multiple agents)
- Integration risk level
- Recommendations for next steps

For agents consuming this programmatically:

```bash
writ context --format json
```

Returns a structured JSON object designed to fit efficiently into an LLM's context window.

## 4. Add a Spec

Specs are structured requirements that agents work against:

```bash
writ spec add --id auth --title "Authentication System" --description "JWT-based auth with token refresh"
```

Now when agents seal work with `--spec auth`, that work is linked to this requirement. Context output will scope to the spec's files and show progress.

## 5. Seal Completion

When an agent finishes its work on a spec:

```bash
writ seal -s "auth system complete, all tests passing" \
  --agent dev-1 \
  --spec auth \
  --status complete \
  --tests-passed 42
```

## 6. Round Trip Back to Git

When the writ session is done, commit everything back to git:

```bash
# One-command round trip
writ finish

# Or manually with a generated commit message
git commit -m "$(writ summary --format commit)"

# Or create a PR with a detailed description
gh pr create --body "$(writ summary --format pr)"
```

`writ finish` generates a commit message from the full session history, including which agents worked on which specs, what was completed, and what was tested.

## What Just Happened

In five minutes you:

1. **Installed** writ alongside an existing git repo
2. **Sealed** a checkpoint with structured metadata (agent, spec, status)
3. **Checked context** to see the project state in one call
4. **Defined a spec** that agents can work against
5. **Completed** work with test results attached
6. **Committed** back to git with auto generated provenance

## Python SDK

Everything above also works through the Python SDK:

```python
import writ

repo = writ.Repository.open(".")
ctx = repo.context(spec="auth")
repo.seal(
    summary="added auth endpoint",
    agent_id="dev-1",
    spec_id="auth",
    tests_passed=12,
)
```

## Next Steps

- **[Your First Convergence](your-first-convergence.md)** to see what happens when multiple agents work in parallel
- **[Seals vs Commits](../concepts/seals-vs-commits.md)** to understand the data model
- **[Convergence](../concepts/convergence.md)** for the deep dive on semantic merging
