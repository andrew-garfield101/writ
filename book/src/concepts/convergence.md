# Convergence

Giving an agent better git access doesn't make `git merge` smarter. When five agents work concurrently and three of them touch the same file, the merge problem doesn't go away because you gave each agent its own worktree. The work still has to come back together.

Git's merge is line based. It compares text and gives up when two sides change the same region. It doesn't know the difference between an import and a function body. It can't tell that two agents adding different functions to the same file is perfectly safe, not a conflict. The tool was built for humans resolving one merge at a time, not for fleets of agents producing changes that need to compose automatically.

Writ's convergence engine understands code **structure**. It decomposes files into semantic units — imports, function definitions, class bodies, statements — and resolves conflicts at that level. The core principle: **compose, don't choose.** Multi agent work is fundamentally additive. Agents build complementary features. The engine preserves all contributions wherever possible and escalates clearly when it can't.

## The Pipeline

Convergence runs through six phases for each conflicted file:

### Phase 1: Structural Diff

The file is decomposed into structural units using language aware analysis. Writ has dedicated analyzers for:

- Python (functions, classes, imports, decorators)
- Rust (functions, impl blocks, use statements, modules)
- TypeScript (functions, classes, interfaces, imports)
- JavaScript (functions, classes, imports, exports)
- Go (functions, types, imports, methods)
- Generic fallback (line based, for everything else)

Each unit is tagged with its type (import, definition, statement) and its location in the file.

### Phase 2: Classification

Conflict regions are categorized:

| Type | Meaning |
|------|---------|
| `BothModified` | Both sides changed the same structural unit |
| `DeleteVsModify` | One side deleted a unit the other modified |
| `BothAdded` | Both sides added new content in the same region |
| `OneAdded` | Only one side added content (trivial merge) |
| `Identical` | Both sides made the same change (trivial merge) |

### Phase 3: Pattern Resolution

Five deterministic patterns attempt to resolve conflicts. Each pattern has a base confidence score. The highest confidence match wins.

| Pattern | Confidence | What It Resolves |
|---------|-----------|-----------------|
| **Import Accumulation** | 0.95 | Both sides add or modify imports. Union them. |
| **Non Overlapping Definitions** | 0.92 | Both sides add functions or classes with different names. Compose them. |
| **EOF Append** | 0.92 | Both sides append to the end of the file. Concatenate. |
| **Additive Composition** | 0.88 | Both sides preserved the base and added content. Compose. |
| **Superset Containment** | 0.82 | One side contains everything the other has. Use the superset. |

Confidence scores adjust dynamically based on merge complexity. Larger merges receive proportionally more cautious scores.

### Phase 4: Spec Aware Resolution (Feature Flagged)

This is where writ's first class spec metadata gives convergence something no other VCS can offer. Specs carry file scope, acceptance criteria, and design notes. When a conflict is ambiguous at the structural level, spec context can resolve it — does this file belong to spec A or spec B? Which spec has this file in its declared scope? Which agent has the higher trust level for this file?

Currently feature flagged off. When enabled, it uses the same structured metadata that `writ context` surfaces.

### Phase 5: LLM Assisted Resolution (Feature Flagged)

Sends unresolved conflicts to an LLM for resolution with full context. The LLM can select, reorder, and combine from the inputs — composition only, never novel code generation. Currently feature flagged off.

### Phase 6: Verification

The `HardenedVerifier` runs integrity checks on every merged output:

- **Duplicate definitions:** No function or class name appears twice
- **Balanced delimiters:** Brackets, braces, and parentheses are balanced
- **Content loss detection:** Warns if significant content from either side was lost
- **Conflict marker scan:** Ensures no `<<<<<<<` markers leaked into output
- **Content traceability:** Every line in the output must trace back to an input (base, left, or right). Novel content injected by bugs or hallucinations is detected and rejected.

If verification fails, the merge is rejected. Writ never silently applies a broken merge.

## Confidence Thresholds

| Range | Action |
|-------|--------|
| >= 0.85 | **Auto resolve.** The pattern is highly confident. Merge without human review. |
| 0.60 to 0.84 | **Suggest.** Pass to Phase 4/5 or present as a recommendation. |
| < 0.60 | **Escalate.** The conflict is too ambiguous for automation. Escalate to a human or orchestrator agent. |

## Running Convergence

```bash
# Merge all diverged branches, escalate what can't be auto resolved
writ converge-all --apply --strategy escalate

# Preview what would happen without applying
writ converge-all --dry-run

# Two spec convergence
writ converge backend frontend --apply
```

```python
# Python SDK
report = repo.converge_all(strategy="escalate", apply=True)
print(f"Merged {len(report['merge_order'])} branches")
print(f"Auto-merged: {report['total_auto_merged']}")
```

## Strategies

| Strategy | Behavior |
|----------|----------|
| `escalate` | Auto resolve high confidence conflicts, escalate the rest. Recommended for most workflows. |
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
  7 diverged branches (>3)
  file touched by 11 agents (>=5)
  6 scope violations (>5)
```

Integration risk is scored 0 to 100 based on:
- Number of diverged branches
- File contention (files touched by multiple agents)
- Scope violations
- Number of active agents

When risk is high, converge before starting new work. Context surfaces this automatically so agents don't need to discover it themselves.

## Example: Import Accumulation

Two agents both modify `app.py`. Agent A adds:

```python
from auth import validate_token
```

Agent B adds:

```python
from payments import process_charge
```

Git sees conflicting changes to the import block and produces conflict markers. Writ's `ImportAccumulation` pattern recognizes both as additive import changes and unions them:

```python
from auth import validate_token
from payments import process_charge
```

Confidence: 0.95. Auto resolved. No human intervention needed.

## Example: Real Conflict

Agent A changes `calculate_tax()` to use a flat rate. Agent B changes it to use a progressive rate. These are incompatible implementations of the same function.

Writ classifies this as `BothModified` with no matching pattern. It escalates with full context:

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

## Next Steps

- **[Security Model](security-model.md)** for how trust levels affect convergence confidence
- **[Convergence Resolution Guide](../guides/convergence-resolution.md)** for handling escalations
- **[Multi Agent Workflow](../guides/multi-agent-workflow.md)** for production patterns
