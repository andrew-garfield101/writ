# Security Model

Writ is built for environments where multiple autonomous agents have write access to the same codebase. That demands security guarantees that traditional VCS was never designed for.

Whether you're running a zero trust setup where every agent action is verified, a fully autonomous system where agents operate without human oversight, or a mixed environment with humans in the loop. The security model is the same. Trust nothing by default. Verify everything cryptographically. Log every security relevant action.

## Cryptographic Seal Chains

Every seal is linked to its predecessor through a chain of BLAKE3 hashes. Tamper with any seal and the entire chain breaks.

### How It Works

Each seal contains three cryptographic fields:

| Field | What It Hashes | What It Proves |
|-------|---------------|----------------|
| `content_hash` | The seal's own data (files, summary, metadata) | This seal hasn't been modified |
| `parent_seal_hash` | The previous seal's `chain_hash` | The previous seal is the real predecessor |
| `chain_hash` | Combination of content hash and parent seal hash | The entire history up to this point is intact |

### Verification

```bash
# Verify the full chain from genesis to HEAD
writ verify

# Verify a specific seal
writ verify --seal a7c2e8f4b31a
```

Both commands support `--format json` for programmatic consumption and `--format human` for readable output.

If any seal has been tampered with, verification reports exactly where the chain broke and what was expected versus what was found. From 0.4.5 it also checks every signature against the signing identity's key (below) and reports the counts: `seals_signed` and `seals_unsigned` in JSON, and a line such as "12 signed (signature checked against the agent's key), 3 unsigned" in text. Unsigned seals are counted, never failures; a signature that does not match fails verification and names the seal.

### Ed25519 Signatures

From 0.4.5, seals are signed by default. The first time writ seals for an identity, whether the auto seal hook does it or the agent seals by hand, writ makes an Ed25519 key for that identity and stores it where `writ agent register` stores keys: the public key in `.writ/keys/<identity>.pub`, the private key encrypted in `.writ/keys/<identity>.enc`. Every later seal by that identity is signed with the same key, and `writ verify` checks each signature against the key of the identity that made the seal. `writ agent register` adopts a key writ already made for that name.

**What a key writ made proves.** It proves continuity: every seal signed with it came from the same identity in this store, and none of those seals was altered after it was signed. It does not prove who that identity is. A key the hook made for `reviewer` says the seals came from one continuing `reviewer`, not which person or model ran it. The private keys are encrypted with a master key kept beside them in `.writ/keys/.master` (files readable only by you, mode 0600), so a key is exactly as private as the `.writ` directory: anyone who can write to `.writ` can sign as any identity in it. `.writ` is never committed, so git and anyone reading the git history never see a key.

Seals made before 0.4.5, seals imported from git by the bridge, and any seal written when writ could not make a key carry no signature. `writ verify` counts them as unsigned. Before 0.4.5, `writ verify` did not check signatures at all, even when a seal carried one.

Writ also makes a dedicated convergence key on `writ init`, so seals the convergence engine creates during a merge are signed by the engine, not by an agent.

## Agent Identity

Every identity that seals gets a key on its first seal (above). Registering an agent adds what a first seal key does not: a trust level, scope constraints, and the ability to suspend or revoke it.

### Registration

```bash
writ agent register backend-dev --trust-level standard --scope "src/api/**"
```

### Trust Levels

| Level | Description | Impact |
|-------|-------------|--------|
| **Full** | Project owner or lead. Unrestricted access. | Full confidence scoring in convergence. |
| **Standard** | Normal working agent. The default. | Standard confidence scoring. |
| **Restricted** | Limited scope. Constrained to specific files. | Reduced confidence caps. Changes more likely to be reviewed. |
| **Untrusted** | New or unverified. | Lowest confidence caps. Changes almost always escalated. |

Trust levels directly affect convergence. When two agents' changes conflict, the engine factors in their trust levels when scoring confidence. An untrusted agent's changes receive lower confidence, making auto resolution less likely and human review more likely. This is how writ scales security across fleets of agents with varying levels of trust: from a known, verified lead agent to a newly introduced model that hasn't earned trust yet.

### Suspension and Revocation

Agents can be:
- **Suspended:** Temporarily blocked from creating seals. All existing seals remain.
- **Revoked:** Permanently deactivated. History preserved, but the agent cannot create new seals.

Both actions are recorded as security events in the audit log. In a fleet of autonomous agents, this is how you respond to a compromised agent immediately. Suspend it, review the damage, and all seals created after the compromise timestamp are automatically flagged.

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

No more agents silently modifying files they shouldn't touch.

## Content Traceability (In Development)

Content traceability is part of the LLM assisted convergence pipeline (Phase 5), currently feature flagged for a future release. When enabled, it enforces a strict rule: **every line in merged output must trace back to an input** (base, left, or right).

This will prevent:
- **Hallucinated content** from leaking into merge results when an LLM is involved in resolution
- **Convergence bugs** that might inject novel content
- **Silent data corruption** during complex multi way merges

## Security Event Monitoring

Writ maintains an append only security event log that records:

| Event Type | When It Fires |
|-----------|--------------|
| `scope_violation` | Agent seals changes outside its declared scope |
| `chain_hash_failure` | Seal chain verification detects tampering |
| `authentication_failure` | Signature verification fails |
| `seal_refused` | A seal is refused, by hand or by the auto seal hook (from 0.4.5 the hook's refusals are recorded too), with the identity, the files and the other agent |
| `finish_refused` | `writ finish` refuses, for example because sealed lines would be lost |
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
| **Signatures** | Continuity. Every signed seal verifies against the key of the identity that made it; a key writ made proves one continuing identity, not a person. |
| **Trust levels** | Convergence quality. Lower trust agents get more scrutiny. |
| **Scope enforcement** | File boundaries. Agents stay in their lane. |
| **Content traceability** | Merge integrity. No hallucinated or injected content survives. *(In development, part of LLM convergence pipeline)* |
| **Event monitoring** | Auditability. Every security relevant action is logged. |

## Next Steps

- **[Seals vs Commits](seals-vs-commits.md)** for the data model behind seal chains
- **[Convergence](convergence.md)** for how trust levels affect merge decisions
- **[Troubleshooting](../troubleshooting.md)** for common verification failures and how to resolve them
