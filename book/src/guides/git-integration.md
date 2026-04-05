# Git Integration

Writ works alongside git, not instead of it. Git handles remote collaboration, code review, CI, and everything teams already depend on. Writ adds the intelligence layer for agents: structured checkpoints, fleet coordination, and semantic convergence. The two systems connect through a clean round trip.

## The Round Trip

```
git repo → writ init → agents work in writ → writ finish → git commit → git push
```

### Starting: `writ init`

When you run `writ init` in a git repository, writ:

1. Detects the git branch and HEAD commit
2. Imports the current working tree as a baseline seal (`bridge import`)
3. Configures agent frameworks (CLAUDE.md, AGENTS.md, etc.)
4. Sets up format preferences and workflow mode

The baseline seal becomes the "base" for all convergence operations. Every agent's work is measured against this common starting point.

### During Work: Agents Seal, Not Commit

Agents use `writ seal` to checkpoint their work and `writ spec done` to mark tasks complete. They do not run `git add`, `git commit`, or `git push`. The seal chain captures richer metadata than git commits (agent identity, spec linkage, test results, verification status) and keeps the git history clean.

### Finishing: `writ finish`

`writ finish` bridges writ's seal chain back to git:

1. Shows all completed specs
2. Lets you select which to include
3. Offers commit strategies (single, per spec, grouped)
4. Auto generates commit messages from seal history
5. Runs `git add` and `git commit`

```bash
writ finish                          # interactive: select specs, choose strategy
writ finish --yes                    # accept defaults, one commit, no prompts
writ finish --strategy per-spec      # one git commit per completed spec
writ finish --dry-run                # preview without committing
```

After `writ finish`, you're back in standard git. Push, create PRs, run CI. All normal.

### Manual Alternative

If you prefer driving git yourself:

```bash
git commit -m "$(writ summary --format commit)"
gh pr create --body "$(writ summary --format pr)"
```

`writ summary` generates commit messages and PR descriptions from the full seal history. `writ finish` uses this internally, but the standalone commands work for custom workflows.

## Bridge Commands

### Import: Git to Writ

```bash
writ bridge import                   # import current working tree
writ bridge import --git-ref v1.0    # import a specific git ref
```

Creates a baseline seal from the git state. This is done automatically by `writ init`, but you can re-import if you need to update the baseline (e.g., after pulling new changes from remote).

### Export: Writ to Git

```bash
writ bridge export                   # export seals as git commits
writ bridge export --branch feature  # target a specific branch
writ bridge export --pr-body         # include PR-style metadata in commit messages
```

For workflows where you want each seal represented as a git commit (rather than using `writ finish` to create a summary commit).

## Workflow Modes and Git

The [workflow mode](workflow-modes.md) determines how `writ finish` creates git commits:

- **User mode**: You run `writ finish` manually. Full control over what gets committed and when.
- **Propose mode**: An orchestrator proposes commits. You review and accept. Git commits are created on acceptance.
- **Auto mode**: Commits happen automatically, targeted to a specific branch (`writ/auto`). You merge to main when ready.

Auto mode's branch targeting is the key safety rail. Agents commit freely to `writ/auto`, but the human controls what reaches main through standard git merge or PR workflows.

## Best Practices

- Run `writ init` once per project, at the start of each session
- Let agents work entirely in writ (`seal`, `spec done`, `context`)
- Use `writ status` to monitor progress
- Run `writ finish` when you're ready to commit to git
- Push with standard git after finishing
- If you pull new changes from remote, consider re-importing: `writ bridge import`
