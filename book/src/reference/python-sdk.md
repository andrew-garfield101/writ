# Python SDK Reference

Every method on `writ.Repository` calls the same Rust code as the CLI command of the same name, through PyO3 bindings; there is no separate implementation. This page covers the methods most programs use. `help(writ.Repository)` lists every method with its arguments.

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

### A Whole Run

```python
import writ

repo = writ.Repository.open(".")
specs = repo.plan(["Add the parser", "Write the parser tests"])   # one spec per task
repo.spec_claim(specs[0]["spec_id"], "worker-3")                  # the agent takes one
ctx = repo.context(for_agent="worker-3")                          # project state for this agent
repo.seal(summary="parser added", agent_id="worker-3")            # checkpoint on its claimed spec
repo.spec_done(summary="Parser added with tests.", agent_id="worker-3")
print(repo.handoff_text())                                        # what happened since the last finish
repo.finish()                                                     # converge and commit to git
```

### Sealing

```python
result = repo.seal(
    summary="added auth endpoint",
    agent_id="dev-1",
    agent_type="agent",
    spec_id="auth",
    status="in-progress",
    paths=["src/auth.py"],     # optional: seal only these paths
    tests_passed=12,
    tests_failed=0,
)
```

From 0.4.5 the seal is signed with a key writ makes for `dev-1` on its first seal (see [Security Model](../concepts/security-model.md#ed25519-signatures)).

### Closing a Spec

```python
repo.spec_done(spec_id="auth", summary="Token refresh with a 15 minute expiry.", agent_id="dev-1")
```

The summary is the agent's own words in the commit receipt.

### Context

```python
ctx = repo.context()                          # full context
ctx = repo.context(spec="auth")               # scoped to a spec
brief = repo.context(format="brief")          # the compact form agents read at session start
```

### History and Going Back

```python
seals = repo.log(limit=10)
repo.restore(seals[3]["id"])                  # working directory back to that seal
repo.seal(summary="rolled back to the last good seal", agent_id="dev-1")
```

Restoring never deletes history; every earlier seal stays in the log.

### What Happened, and Why

```python
report = repo.handoff(since="last-finish")    # dict: agents, specs, shared files, refusals, next action
text = repo.handoff_text(format="markdown")   # or "brief"; the receipt forms are in the CLI
lineage = repo.why("src/auth.py")             # the specs that sealed this file, in order
health = repo.doctor()                        # findings and the command that fixes each
```

### Convergence

```python
report = repo.converge_all(strategy="escalate", apply=True)
```

### Verification

```python
result = repo.verify_chain()    # keys: valid, verified, signed, unsigned, failures, ...
result = repo.verify_seal("a7c2e8f4b31a")
```

From 0.4.5 `verify_chain()` checks every signature against the key of the identity that made the seal and counts `signed` and `unsigned` seals; unsigned seals are not failures. `verify_chain(use_convergence_key=True)` checks every seal against the convergence engine's key instead, so on a 0.4.5 store it reports agent seals, signed with their own keys, as failures; use it only for seals the engine made.

See the [Getting Started guide](../getting-started/quickstart.md) for usage examples.
