# Multi Agent Workflow

Multiple agents work in the same project directory. Each agent seals its own changes with `--spec`, and writ keeps them separate. No workspaces, no branches, no path copying. Agents work. Writ handles the rest.

## The Flow

```bash
# One time setup
writ init

# Define tasks (optional — agents can also create their own specs)
writ plan "Implement OAuth2 auth" "Add Stripe payments" "Build admin dashboard"

# Launch agents however you normally do
# Agents discover specs via writ context, claim one, work, seal, done.
# Convergence runs automatically at spec done and at finish.

# When ready
writ finish
git push
```

Three writ commands for the human: `init`, `plan`, `finish`. Plan is optional. Everything between init and finish is the agent's world.

## Same Steps as Git

The biggest misconception writ needs to fight: "it's more stuff to learn." It's not. It's the same number of steps as git, mapped to agent native concepts.

```
Git (human workflow):                Writ (agent workflow):
---------------------                ----------------------
git init                             writ init
git checkout -b feature              (agents just start working)
  write code                           agents work
  git add . && git commit              writ seal (agents do this)
git checkout main && git merge       writ finish (converges + commits)
git push                             git push
```

Same rhythm. Same mental model. The concepts map directly:

| Git Concept | Writ Concept | What Improves |
|------------|-------------|---------------|
| (nothing) | Spec | Task tracking with lifecycle, agent claiming, genesis snapshots. No git equivalent |
| Commit | Seal | Agent identity, spec linkage, immutable chain |
| Merge | Converge | Writ's convergence engine merges seal trees. Auto resolves independent changes |
| Worktree | Workspace | Shared object store, scoped context, native convergence (Level 2 only) |
| `git status` | `writ context` | One call, structured output, token optimized |
| `git log` | `writ log` | Filterable by spec, agent, workspace |
| (nothing) | `writ plan` | Batch task definition |

## The Human's Commands

```
writ init                  Once per project
writ plan -f tasks.txt     Once if pre-defining tasks (optional)
writ status                Whenever you want to check in
writ watch                 Live seal monitoring (optional)
writ finish                Once when work is done
git push                   Standard git
```

Most sessions: init, finish, push. Three commands total. Two of those are git.

## The Agent's Commands

```
writ context               Understand the project and find available specs
writ spec claim <id>       Claim a spec (or auto-claim on first seal)
writ seal -s "..." --spec  Checkpoint work (captures only this spec's changes)
writ spec done <id>        Mark task complete
```

The agent never creates workspaces, never runs convergence, never touches git. It discovers its task via context, claims it, works, seals, and reports done. Everything else is writ's job.

## Spec-Scoped Sealing

This is the foundation that makes same-directory multi-agent work possible.

When an agent runs `writ seal -s "auth endpoint" --spec auth-feature`, writ captures only the files that changed since this agent's last seal for this spec. Other agents' changes are invisible to the seal, not because they're in a different directory, but because writ knows they belong to a different spec.

```bash
# 4 agents, same directory, zero ceremony
cd my-project
writ init

# Terminal 1: Agent works on auth (touches src/auth/)
# Terminal 2: Agent works on payments (touches src/payments/)
# Terminal 3: Agent works on UI (touches src/components/)
# Terminal 4: Agent works on docs (touches docs/)

# Each agent seals only its own changes
# Agent 1: writ seal -s "auth endpoint" --spec auth → only src/auth/ files
# Agent 2: writ seal -s "stripe setup" --spec payments → only src/payments/ files
# No cross-contamination. No "no changes to seal" errors.
```

Sealing without `--spec` still captures the entire working directory for backward compatibility. Single-agent and human workflows are unchanged.

## Three Isolation Levels

Not every multi-agent scenario needs the same kind of isolation. Writ provides three levels, each building on the last.

### Level 0: No Isolation Needed (Most Common)

Agents touch different files. Agent A works on `src/auth/`, Agent B works on `src/payments/`. They never modify the same file. Spec-scoped sealing keeps each agent's changes in its own seals. No convergence needed because there's nothing to converge.

```
Agent A seals: src/auth/login.py, src/auth/tokens.py
Agent B seals: src/payments/stripe.py, src/payments/webhooks.py
→ No overlap. Each seal contains only its own files.
→ writ finish combines everything into one git commit.
```

This is the majority of multi-agent work. Good task decomposition naturally separates file concerns. Writ handles it with zero overhead.

### Level 1: Additive Overlap (Common)

Agents add to the same file in different sections: different functions, different config blocks, different test cases. The changes are independent and can be merged. Writ's convergence engine handles this automatically at `writ spec done` and as a backstop at `writ finish`.

```
Agent A seals: added loginWithGoogle() to auth.ts
Agent B seals: added loginWithGitHub() to auth.ts

→ Agent A calls spec done → convergence checks for overlaps
→ convergence engine: both are independent function additions
→ auth.ts now has both functions
→ merged automatically, no human intervention
```

Two agents both adding to a shared config file, both adding functions to a utility module, both adding routes to an API file. The convergence engine handles it because non-conflicting additions compose automatically.

### Level 2: Competing Rewrites (Rare)

Agents rewrite the same code in fundamentally different ways. Agent A rewrites the entire auth module using PKCE flow. Agent B rewrites it using implicit flow. They need separate copies of the file to work from because each agent's changes would break the other's in-progress work.

THIS is when you need workspaces:

```bash
writ task "rewrite auth: PKCE approach"     # creates isolated workspace
writ task "rewrite auth: implicit approach"  # creates isolated workspace
# launch agents in workspace directories
# each has their own copy of auth.ts
# convergence (or human decision) reconciles at the end
```

Level 2 is rare. Most multi-agent work is Levels 0 and 1. But when it happens, workspaces are there. The `writ task` flow and all workspace plumbing remain available as the advanced tool for genuine physical isolation. See the [Workspaces guide](workspaces.md) for the full setup.

### The Hierarchy

```
Level 0: Different files          → spec-scoped sealing (automatic)
Level 1: Same file, additions     → convergence engine (auto at spec done / finish)
Level 2: Same file, rewrites      → workspaces (explicit via writ task)
```

Users start at Level 0. Most stay there. If they hit Level 1, convergence handles it automatically when specs complete. If they genuinely need Level 2, they opt in with `writ task`. Each level is discovered when needed, not configured upfront.

## Writ Watch

`writ watch` is a live monitoring tool that shows seal events as they happen. It gives you a real time view of agent activity without needing to poll `writ status`.

### Starting the Watcher

```bash
writ watch
```

Runs in the foreground, showing real time output:

```
$ writ watch

  writ watch active — monitoring for new seals...

  [10:15:03] seal a3f8b2c9 (agent-1, auth-feature): 3 files
  [10:15:47] seal b7e2a4f1 (agent-2, payments): 2 files
  [10:16:12] seal d4c1e8a6 (agent-3, auth-feature): 1 file — overlaps with agent-1
  [10:22:05] seal e9f3c7d2 (agent-4, dashboard): 4 files

  Press q to quit.
```

Run this in a dedicated terminal tab for passive visibility into agent activity. Overlapping files are flagged so you can track where convergence will be needed.

### How Convergence Works

Convergence runs automatically at two points:

1. **`writ spec done`**: when an agent marks its task complete, convergence checks for overlapping work with other completed specs
2. **`writ finish`**: final backstop that catches anything remaining before git commit

You do not need to run convergence manually in the normal workflow. Agents work, call spec done, and convergence handles the rest. `writ converge-all --apply` is available for explicit manual control when needed.

### Configuration

```toml
# .writ/config.toml
[watch]
interval = 5              # polling interval in seconds (default: 5)
```

CLI overrides:

```bash
writ watch --interval 10          # custom polling interval
writ watch --daemon               # run as background process
writ watch --stop                 # stop running daemon
writ watch --status               # show daemon status
```

## Writ Plan

`writ plan` is batch spec creation for medium-to-large scale task setup. It reads a list of tasks and creates specs for all of them in one shot.

```bash
# From inline arguments
writ plan "Implement OAuth2 auth" "Add Stripe payments" "Build admin dashboard"

# From a file (one task per line)
writ plan -f tasks.txt

# From stdin (pipe from any tool)
cat tasks.txt | writ plan
```

Output:

```
$ writ plan "Implement OAuth2 auth" "Add Stripe payments" "Build admin dashboard"

  3 specs created:
    implement-oauth2-auth     "Implement OAuth2 auth"
    add-stripe-payments       "Add Stripe payments"
    build-admin-dashboard     "Build admin dashboard"

  Next: launch your agents. They discover specs via `writ context`.
```

Titles are slugified into spec IDs automatically. Agents discover unclaimed specs via `writ context` and claim them.

## Three Paths to Scale

### Path A: Agent Self-Organization (1-5 agents)

The agent creates its own spec from its prompt. Zero ceremony. Human just launches agents.

```bash
writ init
# launch agents with prompts
# agents: context → spec add → work → seal → spec done
writ finish
```

### Path B: Batch Planning (5-50 agents)

User defines a task list. Agents discover and claim.

```bash
writ init
writ plan -f tasks.txt
# launch agents
# agents: context → claim spec → work → seal → spec done
writ finish
```

### Path C: Programmatic SDK (50+ agents)

Orchestrator creates specs and launches agents via code.

```python
import writ

repo = writ.Repository.open(".")
for task in tasks:
    repo.add_spec(id=task.id, title=task.title)
    orchestrator.launch_agent(task)
```

All three paths converge: specs exist → agents work (same directory, spec scoped sealing) → convergence runs at spec done and finish → git push. One set of internals. Three entry points for different scales.

## Spec Claiming

When an agent runs `writ context` and sees unclaimed specs, it picks one up:

```
$ writ context

  unclaimed_specs:
    implement-oauth2-auth     "Implement OAuth2 auth"
    add-stripe-payments       "Add Stripe payments"
    build-admin-dashboard     "Build admin dashboard"
```

The agent claims a spec explicitly:

```bash
writ spec claim implement-oauth2-auth
# "Claimed spec implement-oauth2-auth for agent claude-1"
```

Or implicitly via first seal:

```bash
writ seal -s "started auth work" --spec implement-oauth2-auth
# Spec auto-claimed by this agent on first seal
```

Once claimed, the spec does not appear as unclaimed for other agents. This prevents duplicate work.

## Checking Progress

```bash
$ writ status

  Active    3 agents    2 specs in progress
  Done      1 agent     1 spec completed (not committed)

  a3f8b2c9  implement-oauth2-auth     agent-1    3 seals    working
  b7e2a4f1  add-stripe-payments       agent-2    complete   (5 seals)
  d4c1e8a6  build-admin-dashboard     agent-3    1 seal     working

  1 spec complete · run `writ finish` when ready
```

Full transparency into what every agent is doing without branch archaeology or parsing commit messages.

## Finishing

`writ finish` promotes completed work to git. Convergence runs as a final backstop, catching any remaining overlaps before committing.

```bash
$ writ finish

  3 specs ready to commit.

  Commit strategy: single (all specs in one commit)

  [main abc1234] feat: OAuth2 auth, Stripe payments, admin dashboard

$ git push
```

Convergence also runs at `writ spec done`, so most overlaps are already resolved by finish time.

## Troubleshooting

### "No changes to seal"

This means nothing changed since your last seal for this spec. Check:
- Did you modify any files since your last seal?
- Are you using `--spec` correctly? Without `--spec`, writ captures the full directory.

### Convergence conflict flagged

Run `writ status` to see which files have conflicts. The conflict means two agents rewrote the same code in incompatible ways. The human resolves it by choosing one version or manually merging.

### Agent not seeing other agents' work

Run `writ context` to refresh. Convergence runs at `writ spec done` and `writ finish`. To merge outstanding overlaps manually, run `writ converge-all --apply`.

## Next Steps

- **[Workspaces](workspaces.md)** for Level 2 physical isolation when competing rewrites require separate directories
- **[Convergence](../concepts/convergence.md)** for the deep dive on how writ merges agent work
- **[Workflow Modes](workflow-modes.md)** for commit automation options
- **[CLI Reference](../reference/cli.md)** for the full command reference
