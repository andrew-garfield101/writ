# Workflow Modes

Writ's workflow scales from a solo developer pressing enter through prompts to an enterprise fleet of 500 agents committing autonomously. The scaling happens in the automation layer between agents and git, not in the user's interface. One agent or 500, the user checks in with `writ status`, promotes work with `writ finish`, and pushes with `git push`.

Three modes control how completed work becomes git commits.

## User Mode (Default)

The user runs `writ finish` manually. Maximum control.

```
agents complete work → user runs writ status → user runs writ finish → git commit
```

```toml
# .writ/config.toml
[workflow]
commit_mode = "user"
```

This is the default because it's the safest starting point. The user reviews everything. Good for solo developers, small teams, and learning writ.

### The Three Command Interface

From the user's perspective, the entire workflow is three commands:

```bash
writ status                  # What's happening?
writ finish                  # Promote completed work to git
git push                     # Standard git from here
```

`writ status` shows a fleet overview: how many agents are active, which specs are complete, what's ready to commit. It adapts automatically to scale — expanding details for small projects, collapsing to summaries for large ones.

`writ finish` is interactive. It shows all completed specs, lets you select which to include, and offers commit strategies:

- **Single commit** (default): All specs as one git commit. Clean and simple.
- **Per spec commits**: Each spec becomes its own git commit. Good when git bisect and per feature rollback matter.
- **Grouped commits**: Writ auto detects logical groupings by file overlap. Good for large scale where coherence matters more than granularity.

### Live Monitoring

```bash
writ status --watch
```

Refreshes every 5 seconds. Shows agents completing specs in real time. Keyboard shortcuts let you jump to finish or diff without leaving the view.

## Propose Mode

An orchestrator proposes commits. The user reviews and accepts.

```
agents complete work → orchestrator proposes → user reviews → user accepts → git commit
```

```toml
# .writ/config.toml
[workflow]
commit_mode = "propose"
```

Good for medium teams (10-50 agents) with supervised autonomous work. The orchestrator handles grouping and timing. The user has final say.

### How Proposals Work

The orchestrator creates proposals programmatically:

```bash
writ finish --propose --specs S-041,S-042,S-039 --message "Sprint B: convergence and storage"
```

This creates a proposal in writ's state, not a git commit. The user sees it next time they check in:

```bash
writ status
# Commit Proposals:
#   #1  Sprint B: convergence and storage    3 specs  8 files    11:30 AM

writ finish --review 1      # review the proposal in detail
writ finish --accept 1      # accept and commit
writ finish --reject 1      # reject
writ finish --accept-all    # accept all pending proposals
```

Proposals persist until accepted, rejected, or superseded. Multiple proposals can coexist.

## Auto Mode

Fully autonomous. Orchestrator commits directly. No human in the loop.

```
agents complete work → orchestrator runs writ finish --auto → git commit
```

```toml
# .writ/config.toml
[workflow]
commit_mode = "auto"

[workflow.auto]
verify_command = "cargo test --quiet"    # must exit 0 to commit
max_specs_per_commit = 10                # prevent mega-commits
branch = "writ/auto"                     # commit to a branch, not main
notify = "log"                           # log | stdout | none
```

### Safety Rails

Auto mode is powerful and dangerous. The configuration includes guardrails:

**Test verification**: The `verify_command` must exit 0 before a commit proceeds. If tests fail, the commit is blocked and the spec goes into a blocked state.

**Branch targeting**: Auto commits go to a designated branch (`writ/auto`), not directly to main. The human merges when ready. This is the strongest safety rail — agents commit freely, but the human controls what reaches main.

**Max specs per commit**: Prevents runaway mega-commits. If more than N specs are completed, they're committed in batches.

### When to Use Auto

- CI pipelines where completed work should commit immediately
- Overnight batch runs with trusted agents
- Environments with strong test suites that catch regressions

## The Agent's Perspective

Agents follow the same workflow regardless of mode:

```bash
writ context                                 # understand project state
writ spec add --id auth --title "Auth"       # create or claim a task
# ... do work ...
writ seal -s "added auth endpoint"           # checkpoint
# ... more work ...
writ spec done auth -s "JWT auth complete"   # mark task complete
```

Agents seal checkpoints and mark tasks complete. They do not run `writ finish` or `git commit`. The workflow mode determines what happens after `spec done` — and that's not the agent's concern.

## Choosing a Mode

| Situation | Mode | Why |
|-----------|------|-----|
| Learning writ | `user` | Full control, see everything |
| Solo developer | `user` | Simple, direct |
| Small team (2-5 agents) | `user` | Easy to track manually |
| Medium team (10-50 agents) | `propose` | Orchestrator handles grouping |
| CI pipeline | `auto` | Needs to commit without waiting |
| Overnight batch | `auto` | No human present |
| Production with oversight | `propose` | Balance autonomy and control |

## Configuration

Set the mode during `writ init` or directly in config. See [Configuration](../reference/configuration.md) for the full reference.

```bash
# During init:
# Default workflow mode:
#   (1) user      You run `writ finish` manually (recommended)
#   (2) propose   Orchestrator proposes, you approve
#   (3) auto      Fully autonomous (CI/pipelines)
```

Global config applies to all projects. Override per project in `.writ/config.toml`.
