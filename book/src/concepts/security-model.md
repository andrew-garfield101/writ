# Security Model

Writ is built for environments where multiple autonomous agents have write access to the same codebase. That demands security guarantees beyond what traditional VCS provides.

## Cryptographic Seal Chains

Every seal is linked to its predecessor through a chain of BLAKE3 hashes. Tamper with any seal and the entire chain breaks.

### How It Works

Each seal contains three cryptographic fields:

| Field | What It Hashes | What It Proves |
|-------|---------------|----------------|
| `content_hash` | The seal's own data (files, summary, metadata) | This seal hasn't been modified |
| `parent_seal_hash` | The previous seal's content | The previous seal is the real predecessor |
| `chain_hash` | Combination of content hash and parent seal hash | The entire history up to this point is intact |

### Verification

```bash
# Verify the full chain from genesis to HEAD
writ verify --chain

# Verify a specific seal
writ verify --seal a7c2e8f4b31a
```

Both commands support `--format json` for programmatic consumption and `--format human` for readable output.

If any seal has been tampered with, verification reports exactly where the chain broke and what was expected versus what was found.

### Ed25519 Signatures

Seals can be signed with Ed25519 digital signatures, authenticating who created them. Writ generates a dedicated convergence keypair on `writ init`, stored in `.writ/keys/` with AES-GCM encryption and restricted file permissions (0600).

This means convergence seals (created by the engine during merge) are cryptographically distinguishable from agent seals (created by agents during work). You can always verify whether a merge result came from the convergence engine or was manually crafted.

## Agent Identity

Every agent in writ is a registered entity, not just a string ID.

### Registration

```bash
writ agent register --id backend-dev --role implementer --trust-level standard
```

### Trust Levels

| Level | Description | Impact |
|-------|-------------|--------|
| **Full** | Project owner or lead. Unrestricted access. | Full confidence scoring in convergence. |
| **Standard** | Normal working agent. The default. | Standard confidence scoring. |
| **Restricted** | Limited scope. Constrained to specific files. | Reduced confidence caps. Changes more likely to be reviewed. |
| **Untrusted** | New or unverified. | Lowest confidence caps. Changes almost always escalated. |

Trust levels directly affect convergence. When two agents' changes conflict, the engine factors in their trust levels when scoring confidence. An untrusted agent's changes receive lower confidence, making auto resolution less likely and human review more likely.

### Suspension and Revocation

Agents can be:
- **Suspended:** Temporarily blocked from creating seals. All existing seals remain.
- **Revoked:** Permanently deactivated. History preserved, but the agent cannot create new seals.

Both actions are recorded as security events in the audit log.

## Scope Enforcement

Specs declare which files they own. Agents can be constrained to specific files or directories.

### How It Works

1. A spec declares its file scope: `src/auth/*`, `tests/test_auth.py`
2. An agent is registered with scope constraints
3. When the agent seals changes to files outside its scope, writ responds based on configuration:
   - **Warning mode** (default): The seal succeeds but a scope violation is logged
   - **Enforce mode**: The seal is rejected

### Configuration

Scope enforcement is configurable:

```bash
# Warning mode (default): seal succeeds, violation logged
writ seal -s "updated config" --agent frontend-dev

# Enforce mode: seal rejected if out of scope
writ seal -s "updated config" --agent frontend-dev --enforce-scope
```

### Visibility

Scope violations appear in three places:
1. `writ context` output (under scope violations)
2. Security event log (`writ security events`)
3. Integration risk scoring (violations increase the risk score)

## Content Traceability

The convergence engine enforces a strict rule: **every line in merged output must trace back to an input** (base, left, or right).

This prevents:
- **Hallucinated content** from leaking into merge results if an LLM is involved in resolution
- **Convergence bugs** that might inject novel content
- **Silent data corruption** during complex multi way merges

If the verifier detects content in the merged output that doesn't originate from any input, the merge is rejected. This is the "no silent additions" rule.

## Security Event Monitoring

Writ maintains an append only security event log that records:

| Event Type | When It Fires |
|-----------|--------------|
| `scope_violation` | Agent seals changes outside its declared scope |
| `chain_hash_failure` | Seal chain verification detects tampering |
| `authentication_failure` | Signature verification fails |
| `agent_revoked` | An agent is revoked or suspended |
| `convergence_low_confidence` | Convergence produces results below the confidence threshold |
| `unrecognized_agent` | A seal references an unknown agent ID |

### Viewing Events

```bash
# All events
writ security events

# Filter by severity
writ security events --severity warning

# Filter by event type
writ security events --event-type scope_violation
```

Events are severity classified (info, warning, critical) with configurable retention. GC can clean old events past their retention period, but the event log is append only during normal operation.

## Summary

| Layer | What It Protects |
|-------|-----------------|
| **Hash chains** | History integrity. No seal can be modified after creation. |
| **Signatures** | Authorship. Every seal can be verified back to its creator. |
| **Trust levels** | Convergence quality. Lower trust agents get more scrutiny. |
| **Scope enforcement** | File boundaries. Agents stay in their lane. |
| **Content traceability** | Merge integrity. No hallucinated or injected content survives. |
| **Event monitoring** | Auditability. Every security relevant action is logged. |

## Next Steps

- **[Seals vs Commits](seals-vs-commits.md)** for the data model behind seal chains
- **[Convergence](convergence.md)** for how trust levels affect merge decisions
- **[Troubleshooting](../troubleshooting.md)** for common verification failures and how to resolve them
