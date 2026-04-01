# Convergence

Giving an agent better git access doesn't make `git merge` smarter. When five agents work concurrently and three of them touch the same file, the merge problem doesn't go away because you gave each agent its own worktree. The work still has to come back together.

Git's merge is line based. It compares text and gives up when two sides change the same region. It doesn't know the difference between an import and a function body. It can't tell that two agents adding different functions to the same file is perfectly safe, not a conflict. The tool was built for humans resolving one merge at a time, not for fleets of agents producing changes that need to compose automatically.

Writ's convergence engine merges agent work using sealed histories and genesis trees. Instead of comparing raw text on disk, it reads each agent's sealed snapshots and uses the genesis state (a snapshot of the codebase at spec creation) as the common ancestor. Non-conflicting changes merge automatically. Real conflicts escalate with structured context and confidence scores. The core principle: **compose, don't choose.** Multi agent work is fundamentally additive. Agents build complementary features. The engine preserves all contributions wherever possible and escalates clearly when it can't.

## How It Works

### Pool Filter

Before any merging begins, a three layer filter determines which specs participate:

1. **Epoch boundary** — only specs from the current session (since the last `writ finish`)
2. **Commit state** — only uncommitted specs
3. **Genesis tree** — structural filtering against the common ancestor

This prevents completed, already committed, or irrelevant specs from interfering with the merge.

### Genesis Tree Merge

Every spec has a **genesis tree**: a snapshot of the file index at the moment the spec was created. Writ's convergence engine uses this as the common ancestor:

- **Base**: the genesis tree (what files looked like when the spec started)
- **Left**: spec A's sealed version of the file
- **Right**: spec B's sealed version of the file

Non-conflicting changes (different lines, additive edits, independent sections) merge automatically. When both specs modify the same region in incompatible ways, the **Escalate** strategy auto-resolves by selecting the more complete version. If that's ambiguous, the conflict escalates with structured context for human or orchestrator review.

### Shadow Materialization

Merge results are stored in the object store as **shadow state** — not written to disk. This means convergence can run while agents are still working without disturbing anyone's files. Disk materialization only happens at `writ finish`.

### Verification

The `HardenedVerifier` runs integrity checks on every merged output:

- **Duplicate definitions:** No function or class name appears twice
- **Balanced delimiters:** Brackets, braces, and parentheses are balanced
- **Content loss detection:** Warns if significant content from either side was lost
- **Conflict marker scan:** Ensures no `<<<<<<<` markers leaked into output

If verification fails, the merge is rejected. Writ never silently applies a broken merge.

## When Convergence Runs

- **At `writ spec done`** — checks for overlapping work with other completed specs
- **At `writ finish`** — final backstop that catches anything remaining before git commit
- **Manually** — `writ converge-all --apply` for explicit control

## Confidence Thresholds

| Range | Action |
|-------|--------|
| >= 0.85 | **Auto resolve.** The merge is highly confident. Apply without human review. |
| 0.60 to 0.84 | **Suggest.** Present as a recommendation for review. |
| < 0.60 | **Escalate.** The conflict is too ambiguous for automation. Escalate to a human or orchestrator agent. |

## Running Convergence

```bash
# Merge all diverged specs, escalate what can't be auto resolved
writ converge-all --apply --strategy escalate

# Preview what would happen without applying
writ converge-all --dry-run

# Two spec convergence
writ converge backend frontend --apply
```

```python
# Python SDK
report = repo.converge_all(strategy="escalate", apply=True)
print(f"Merged {len(report['merge_order'])} specs")
print(f"Auto-merged: {report['total_auto_merged']}")
```

## Strategies

| Strategy | Behavior |
|----------|----------|
| `escalate` | Auto resolve high confidence merges, escalate the rest. Recommended for most workflows. |
| `three-way-merge` | Standard three way merge. Leaves conflict markers where resolution fails. |
| `most-recent` | Prefers the most recently sealed version on conflict. |
| `orchestrator` | Reports all conflicts as structured JSON for an orchestrator agent to resolve programmatically. |

The `orchestrator` strategy is designed for automated pipelines. Instead of `<<<<<<<` markers that need text parsing, conflicts come back as structured JSON that orchestrator agents can resolve programmatically. This is the convergence equivalent of what `writ context` does for project state: structured data, not text to parse.

## Merge Ordering

When multiple specs have diverged, writ optimizes the merge order:

1. Specs that touch **disjoint files** merge first (zero conflict risk)
2. Specs with **minimal overlap** merge next
3. Specs with **high overlap** merge last, benefiting from the cleaner base established by earlier merges

This greedy overlap minimizing approach reduces total conflict complexity. For a fleet of agents working across many specs, ordering matters — it's the difference between cascading conflicts and clean sequential merges.

## Integration Risk

Before starting work or after convergence, check the risk level:

```bash
writ context --format human
```

```
INTEGRATION RISK: HIGH (score: 65)
  7 diverged specs (>3)
  file touched by 11 agents (>=5)
  6 scope violations (>5)
```

Integration risk is scored 0 to 100 based on:
- Number of diverged specs
- File contention (files touched by multiple agents)
- Scope violations
- Number of active agents

When risk is high, converge before starting new work. Context surfaces this automatically so agents don't need to discover it themselves.

## Example: Additive Changes

Two agents both modify `app.py`. Agent A adds:

```python
from auth import validate_token
```

Agent B adds:

```python
from payments import process_charge
```

Git sees conflicting changes to the import block and produces conflict markers. Writ's convergence engine recognizes both as non-conflicting additive changes and composes them:

```python
from auth import validate_token
from payments import process_charge
```

Auto resolved. No human intervention needed.

## Example: Real Conflict

Agent A changes `calculate_tax()` to use a flat rate. Agent B changes it to use a progressive rate. These are incompatible implementations of the same function.

Writ classifies this as a real conflict with no automatic resolution. It escalates with full context:

```json
{
  "file": "billing.py",
  "conflict_type": "BothModified",
  "region": "calculate_tax function body",
  "left_agent": "billing-dev",
  "right_agent": "tax-specialist",
  "confidence": 0.30,
  "recommendation": "Manual review required"
}
```

Structured data, not text to parse. An orchestrator agent or human can review and decide.

## In Development

Two additional convergence phases are implemented and feature flagged for future releases:

**Spec aware resolution (Phase 4).** Uses writ's first class spec metadata — file scope, acceptance criteria, design notes — to resolve ambiguous conflicts. Does this file belong to spec A or spec B? Which spec has this file in its declared scope? Which agent has the higher trust level? Spec context gives convergence something no other VCS can offer.

**LLM assisted resolution (Phase 5).** Sends unresolved conflicts to an LLM for resolution with full context. The LLM can select, reorder, and combine from the inputs — composition only, never novel code generation. Includes content traceability: every line in merged output must trace back to an input, preventing hallucinated or injected content from reaching the working tree.

Both phases use the same structured metadata that `writ context` surfaces. When enabled, they slot into the pipeline between the genesis tree merge and verification.

## Next Steps

- **[Security Model](security-model.md)** for how trust levels affect convergence confidence
- **[Convergence Resolution Guide](../guides/convergence-resolution.md)** for handling escalations
- **[Multi Agent Workflow](../guides/multi-agent-workflow.md)** for production patterns
