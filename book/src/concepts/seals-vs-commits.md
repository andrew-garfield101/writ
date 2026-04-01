# Seals vs Commits

Git commits were designed for humans. An email address, a timestamp, a diff, and a message. Everything else (which task it serves, whether tests passed, which requirement it belongs to) is convention layered on top through commit messages and branch names.

Writ's fundamental unit of history is the **seal**. A seal carries the same structured metadata that agents need to work effectively, built into the data model rather than bolted on through naming conventions.

## What a Git Commit Looks Like

```
commit a3f8b2c1
Author: dev@example.com
Date:   Mon Feb 24 14:30:00 2026

    fix: update auth token handling
```

A commit knows: who (an email), when (a timestamp), and what (a diff). That's it. An agent parsing this has to tokenize unstructured text, hope the commit message follows conventions, and make multiple tool calls to reconstruct the state that writ delivers in one structured response.

## What a Writ Seal Looks Like

```json
{
  "id": "a7c2e8f4b31a",
  "timestamp": "2026-02-24T14:30:00Z",
  "agent_id": "auth-dev",
  "agent_type": "agent",
  "spec_id": "auth-migration",
  "summary": "token refresh endpoint with 15-min expiry",
  "status": "in-progress",
  "tests_passed": 12,
  "tests_failed": 0,
  "linted": true,
  "files_changed": 3,
  "content_hash": "blake3:...",
  "parent_seal_hash": "blake3:...",
  "chain_hash": "blake3:..."
}
```

A seal knows everything a commit knows, plus:

- **Who** made it (agent identity with trust level and role, not just an email)
- **What task** it serves (linked spec with status and file scope)
- **What status** the task is in (in progress, complete, blocked)
- **Whether it was tested** (pass/fail counts, lint status)
- **Cryptographic integrity** (BLAKE3 hash chain linking to the previous seal)

This is the intelligence layer. Git stores diffs. Writ stores structured knowledge about what happened, who did it, and why.

## Key Differences

| | Git Commit | Writ Seal |
|-|-----------|-----------|
| **Identity** | Email address | Agent ID with trust level and role |
| **Task linkage** | Branch name (convention) | First class spec reference |
| **Status** | None | `in-progress`, `complete`, `blocked` |
| **Test results** | None (CI runs after the fact) | `tests_passed`, `tests_failed`, `linted` |
| **Integrity** | SHA-1 hash | BLAKE3 content hash + chain hash + Ed25519 signature |
| **Scope awareness** | None | Can enforce file scope constraints per agent |
| **Machine readable** | Needs parsing | Structured JSON from the start |

## Immutability

Like git commits, seals are **immutable**. Once created, a seal's content and metadata never change. The hash chain guarantees this: tamper with any seal and every subsequent chain hash breaks.

Unlike git, writ never rewrites history. There's no `rebase`, no `amend`, no `force push`. If something goes wrong, you `writ restore` to a previous seal and seal the restored state. The old seals stay in the log. Every seal is an immutable snapshot. This is what makes safe rollback possible in fully autonomous environments where agents operate without human oversight.

## Creating a Seal

```bash
# Minimal
writ seal -s "added login endpoint"

# With full metadata
writ seal -s "auth system complete" \
  --agent auth-dev \
  --spec auth-migration \
  --status complete \
  --tests-passed 42 \
  --tests-failed 0 \
  --linted
```

```python
# Python SDK
repo.seal(
    summary="auth system complete",
    agent_id="auth-dev",
    spec_id="auth-migration",
    status="complete",
    tests_passed=42,
)
```

## Viewing Seal History

```bash
# Recent seals
writ log

# All seals, including from diverged branches
writ log --all

# Seals for a specific spec
writ log --spec auth-migration

# Inspect a specific seal
writ show a7c2e8f4b31a --diff
```

## Hash Chains

Every seal contains three hashes:

1. **Content hash:** BLAKE3 hash of the seal's own data (files, metadata). Proves the seal hasn't been modified.
2. **Parent seal hash:** BLAKE3 hash of the previous seal. Links seals into a chain.
3. **Chain hash:** BLAKE3 hash combining the content hash and parent seal hash. Provides tamper evidence across the entire history.

Verify the chain at any time:

```bash
writ verify
```

If any seal has been tampered with, verification fails and reports exactly where the chain broke.

## Next Steps

- **[Specs and Agents](specs-and-agents.md)** to understand how work is organized
- **[Security Model](security-model.md)** for the full cryptographic integrity story
