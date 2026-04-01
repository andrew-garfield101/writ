# Workspaces

Workspaces provide physical file isolation for agents. Most multi-agent work does not need workspaces. Agents work in the same directory and [spec-scoped sealing](multi-agent-workflow.md) keeps their changes separate. Workspaces exist for Level 2 scenarios where agents make competing rewrites to the same file and need separate copies to work from.

When you run `writ task`, writ creates a workspace automatically, a full copy of your project where an agent works without stepping on anyone else's files. When the work is done, `writ finish` converges everything back together and commits to git. You never interact with workspaces directly. You think in tasks. Writ handles the rest.

> **When to use workspaces:** Two agents need to rewrite the same function in fundamentally different ways. One agent takes the PKCE approach, another takes the implicit flow approach. They need separate copies of the file. For everything else (different files, additive changes to the same file), use the [same-directory workflow](multi-agent-workflow.md) instead.

## The Flow

```bash
# One time setup
writ init

# Create tasks (one command each)
writ task "backend API work"
writ task "payment integration"
writ task "dashboard UI"

# Each command prints a launch path:
#   task created: backend-api-work
#   workspace: workspaces/backend-api-work/
#
#   Launch an agent in this workspace:
#     cd workspaces/backend-api-work/

# Launch agents in the printed workspace paths
# (however you normally launch agents — Claude Code, Codex, scripts, etc.)

# Agents work. They use 3 commands: context, seal, spec done.
# They don't know they're in a workspace. They just work.

# When ready, converge and commit
writ finish
git push
```

Three phases. Create tasks. Agents work. Finish.

## Same Steps as Git

The biggest misconception writ needs to fight: "it's more stuff to learn." It's not. It's the same number of steps as git, mapped to agent native concepts.

```
Git (human workflow):                Writ (agent workflow):
---------------------                ----------------------
git init                             writ init
git checkout -b feature              writ task "description"
  write code                           agents work
  git add . && git commit              writ seal (agents do this)
git merge feature                    writ converge-all
writ finish                          writ finish
git push                             git push
```

Same rhythm. Same mental model. The concepts map directly:

| Git Concept | Writ Concept | What Improves |
|------------|-------------|---------------|
| Branch | Task | Structured metadata: title, status, agent assignment, file scope |
| Commit | Seal | Agent identity, spec linkage, workspace tagging, immutable chain |
| Merge | Converge | Genesis tree based, confidence scoring, auto resolves independent changes |
| `git status` | `writ context` | One call, structured output, token optimized, agent ready |
| `git log` | `writ log` | Filterable by spec, agent, workspace |

Writ doesn't add complexity. It replaces git's human oriented steps with agent native equivalents that carry more information and require less manual intervention. The step count is identical. The capability per step is higher.

## The Human's 3 Commands

```
writ init                  Once per project
writ task "..."            Once per task (creates everything agents need)
writ finish                Once when work is done (converges + commits to git)
```

Everything between `writ task` and `writ finish` is the agent's world. Check in with `writ status` whenever you want, but you don't have to.

## The Agent's 3 Commands

```
writ context               Understand the project and assigned task
writ seal -s "..."         Checkpoint work
writ spec done <id>        Mark task complete
```

The agent never creates specs, never touches workspaces, never runs git. It discovers its task via `writ context`, works, checkpoints, and reports done.

When an agent runs `writ context` from a workspace directory, the first thing it sees is its assigned task:

```
task: backend-api-work
  title: "Backend API work"
  status: active

recent_seals: (none yet)
```

No discovery gap. No ID mismatch. The task is right there.

## Task Discovery

In earlier testing, agents sometimes created new specs instead of using pre-assigned ones. `writ task` eliminates this entirely: the spec already exists when the agent starts. The agent doesn't need to create anything.

If an agent tries `writ spec add` for a spec that already exists in its workspace:

```
error: spec 'backend-api-work' already exists
  hint: this spec is assigned to your workspace — use it with:
    writ seal -s "your summary" --spec backend-api-work
```

## Checking Progress

```bash
$ writ status

-- Tasks -----------------------------------------------
  backend-api-work        agent-1    3 seals    working
  payment-integration     agent-2    complete   (5 seals)
  dashboard-ui            agent-3    1 seal     working

  1 complete / 2 in progress
  Run `writ finish` when ready.
```

Tasks are labeled as "Tasks" in status output because you created them with `writ task`. Full transparency into what every agent is doing without branch archaeology or parsing commit messages.

## Finishing

`writ finish` handles convergence automatically. When workspaces exist with completed work:

1. Writ detects all workspaces with changes
2. Runs convergence: files changed in only one workspace merge cleanly, overlapping changes go through the convergence engine
3. If convergence is clean, proceeds to git commit
4. If there are escalations that need human resolution, writ stops and shows you what needs attention
5. After a successful commit, prompts to clean up workspace directories

```
$ writ finish

  Converging 3 workspaces...
  3 tasks committed.

  Clean up workspaces? [Y/n]: y
    removed workspaces/backend-api-work/
    removed workspaces/payment-integration/
    removed workspaces/dashboard-ui/
```

Use `--cleanup` to auto-clean without the prompt, or `--no-cleanup` to keep workspace directories around for inspection.

## Two Paths

A user trying writ for the first time has two natural paths:

### Path 1: Task driven (multi agent)

```bash
writ init
writ task "build payment system"
writ task "redesign dashboard"
# launch agents in printed workspace paths
# agents work...
writ finish
git push
```

The user defines tasks, writ sets up everything, agents work in isolation, convergence brings it together.

### Path 2: Direct (single agent or manual work)

```bash
writ init
# launch agent (or work manually) in the project directory
# agent uses: context, seal, spec add, spec done
writ finish
git push
```

No tasks, no workspaces. The agent works directly in the project. This is the simplest path, identical to pre-workspace writ. Works perfectly for single agent use or manual human work.

Both paths end the same way: `writ finish` then `git push`. The difference is whether you need isolation (Path 1) or not (Path 2). You don't have to decide upfront. Start with Path 2 and switch to Path 1 by running `writ task` whenever you need a second agent.

## Under the Hood

`writ task` is syntactic sugar. One command that assembles three things:

1. **Spec**: a structured task definition (ID derived from title, or `--id` to override)
2. **Workspace**: a full project copy at `workspaces/<id>/` with its own index and HEAD
3. **Assignment**: the spec is scoped to the workspace so `writ context` surfaces it as the agent's task

All workspaces share the same `.writ/` directory for objects, seals, and specs. Creating a workspace copies files but doesn't duplicate the content addressable storage.

```
my-project/
  .writ/                        <- writ internals (hidden, gitignored)
  workspaces/                   <- agent working directories (gitignored)
    backend-api-work/           <- full project copy, agent works here
    payment-integration/        <- full project copy, agent works here
    dashboard-ui/               <- full project copy, agent works here
  src/
  ...
```

The first `writ task` invocation adds `workspaces/` to `.gitignore` automatically. Running `writ task` from inside a workspace shows a warning. Tasks should be created from the main project directory.

## Advanced Usage

The power user commands from workspaces v1 still exist for advanced use cases and orchestrator scripts:

```bash
# Create a workspace manually (without a task)
writ workspace create custom-workspace --path ../my-workspace

# Assign additional specs to a workspace
writ spec assign another-spec --workspace custom-workspace

# Remove a workspace assignment
writ spec unassign another-spec

# List workspaces
writ workspace list

# Delete a workspace (preserves seals, specs, and objects)
writ workspace delete custom-workspace
```

Most users will never need these. `writ task` and `writ finish` handle the full lifecycle.

## When to Use Workspaces

| Scenario | Recommendation |
|----------|---------------|
| Single agent | No workspace needed. Work directly in the project. |
| Multiple agents, different files | No workspace needed. Same directory, spec-scoped sealing. See [Multi Agent Workflow](multi-agent-workflow.md). |
| Multiple agents, additive changes to same file | No workspace needed. Convergence runs at `spec done` and `writ finish`. |
| Multiple agents, competing rewrites of same code | `writ task` per approach. Physical isolation prevents in-progress breakage. |
| CI pipeline with parallel jobs | `writ task` per job. Each gets isolation with shared history. |

Most multi-agent work is Level 0 (different files) or Level 1 (additive changes). Workspaces are the Level 2 tool for the rare case where agents rewrite the same code in incompatible ways.
