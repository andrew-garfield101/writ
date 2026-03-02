# Specs and Agents

Orchestration frameworks coordinate what agents *do*. Writ controls what agents *produce*. Two concepts make this possible: **specs** (structured requirements) and **agents** (registered identities with trust levels).

## Specs

A spec is a structured requirement that agents work against. In git, the relationship between code and requirements is implicit — a branch named `feature/auth` is convention. Nothing enforces it. Nothing links a commit to a ticket. Nothing tells an agent "these are the files you should be touching."

In writ, that relationship is explicit and machine readable.

### Creating a Spec

```bash
writ spec add --id auth --title "Authentication System" \
  --description "JWT-based auth with token refresh and role-based access"
```

Or during install:

```bash
writ install --spec auth --title "Authentication System"
```

### What a Spec Contains

| Field | Purpose |
|-------|---------|
| `id` | Unique identifier, used in `--spec` flags |
| `title` | Human readable name |
| `description` | What needs to be built |
| `status` | Current state (`in-progress`, `complete`, `blocked`) |
| `depends_on` | Other spec IDs this depends on |
| `file_scope` | Which files this spec is expected to touch |

### Lifecycle

Specs move through a managed lifecycle:

```
active -> stale -> completed -> archived
                -> cancelled -> archived
```

- **Active:** Work is happening. Seals reference this spec.
- **Stale:** No activity for a configurable period. `writ context` warns about stale specs so nothing falls through the cracks.
- **Completed:** All work is done. Marked with `writ spec complete <id>`.
- **Cancelled:** Abandoned. Marked with `writ spec cancel <id>`.
- **Archived:** Eligible for garbage collection after retention period.

### Why Specs Matter

When a sub agent in an automated pipeline modifies a file that another sub agent depends on, git has no structured record of what changed, who changed it, or which requirement it serves. Writ does:

```bash
writ seal -s "added token refresh" --agent dev-1 --spec auth
```

This seal is now permanently linked to the `auth` spec. Context output groups work by spec. Convergence merges spec by spec. Scope enforcement restricts agents to their spec's declared files. This is the structured provenance that makes multi agent workflows manageable at scale.

## Agents

An agent is a registered identity in writ. It can represent an AI model, a human developer, or an automated system.

### Registering an Agent

```bash
writ agent register --id backend-dev --role implementer --trust-level standard
```

### Trust Levels

| Level | Description | Convergence Impact |
|-------|-------------|-------------------|
| `full` | Unrestricted. Typically the project owner or lead agent. | Full confidence scoring |
| `standard` | Normal working agent. The default for most agents. | Standard confidence scoring |
| `restricted` | Limited scope. For agents that should only touch specific files. | Reduced confidence caps |
| `untrusted` | New or unverified agents. | Lowest confidence caps, more likely to escalate |

Trust levels directly affect convergence. When two agents' changes conflict, the higher trust agent's changes receive a confidence boost. An untrusted agent's changes are more likely to be escalated for human review rather than auto resolved. This matters most in zero trust environments where every agent action needs verification, and in fully autonomous pipelines where human review isn't available for every merge.

### Agent Identity in Practice

Every seal records which agent created it:

```bash
writ seal -s "implemented login" --agent backend-dev --spec auth
```

`writ context` shows per agent activity: which files each agent has modified, their latest work, and how many seals they've created. For a fleet of agents working concurrently, this is the difference between "which of my 12 agents touched this file" being a five minute archaeology project versus a single structured lookup.

### Scope Enforcement

Agents can be constrained to specific files or directories:

```bash
# Register an agent with scope constraints
writ agent register --id frontend-dev --role implementer \
  --trust-level standard --scope "src/components/*,src/styles/*"
```

When scope enforcement is enabled, sealing changes to files outside the agent's scope triggers a warning (or a rejection, if configured to be strict):

```bash
writ seal -s "updated database config" --agent frontend-dev --enforce-scope
# Warning: frontend-dev modified src/db/config.py, which is outside scope
```

Scope violations appear in `writ context` and in the security event log.

### Suspension and Revocation

Agents can be suspended (temporarily blocked from sealing) or revoked (permanently deactivated) without deleting their history. Every seal they created remains in the log. In autonomous environments, this is how you respond to a compromised or misbehaving agent without losing the audit trail.

## How Specs and Agents Work Together

The combination of specs and agents gives writ something git fundamentally lacks: **structured provenance**.

```bash
writ context --format human
```

Shows:

```
SPECS:
  auth (in-progress) — Authentication System
    agents: backend-dev (12 seals), test-bot (3 seals)
    files: src/auth.py, src/middleware.py, tests/test_auth.py

  payments (in-progress) — Payment Integration
    agents: payments-dev (8 seals)
    files: src/payments.py, src/stripe.py

INTEGRATION RISK: LOW (score: 15)
  No file contention between specs
```

Every piece of work is linked to a requirement and attributed to an identity. Convergence uses this metadata to make better merge decisions. Security auditing uses it to track who changed what and whether they were authorized. And context delivers all of it in one call — structured data an agent can act on immediately.

## Next Steps

- **[Convergence](convergence.md)** to see how specs and agents inform merge decisions
- **[Security Model](security-model.md)** for trust levels, scope enforcement, and audit trails
