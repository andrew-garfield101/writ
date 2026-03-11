# Workspaces

Workspaces are isolated parallel environments for agent teams. Each workspace gets its own directory, its own files, and its own scoped context — while sharing the same object store, seal chain, and specs with every other workspace. When work is done, workspaces converge back together through writ's convergence engine.

Think of them as git worktrees, built for agents: cheap to create, isolated by default, and able to merge structurally instead of line by line.

## Why Workspaces

Git worktrees solve isolation. Each agent gets its own copy of the files, works independently, and doesn't step on anyone else's work. But when the work is done, you're back to `git merge` — line level conflicts, manual resolution, no understanding of what the changes actually mean.

Writ workspaces solve isolation AND convergence. Each agent team works in its own directory with its own scoped context. When teams finish, `writ converge-workspaces` merges their work structurally — composing imports, composing function definitions, escalating real conflicts with full context. The same convergence engine that handles multi-spec merges handles multi-workspace merges.

The other advantage is scoped context. In a project with 30 specs across 3 teams, an agent in the auth workspace only sees auth-related specs, auth-team seals, and auth-relevant files. That's fewer tokens for context, less noise, and a sharper picture of the work that matters.

## Creating a Workspace

```bash
# Create a workspace with auto-generated path (.writ/ws/auth-team/)
writ workspace create auth-team

# Create a workspace at a specific directory
writ workspace create auth-team --path ../ws-auth

# Create a workspace and assign specs to it
writ workspace create auth-team --specs "auth-*"

# Create a workspace from another workspace's state
writ workspace create auth-v2 --from auth-team
```

Each workspace gets:
- Its own directory with a full copy of the project files
- Its own index, HEAD, and spec heads inside `.writ/workspaces/<name>/`
- A `.writ-workspace` pointer file that links back to the main project's `.writ/` directory

All workspaces share the same object store, seals, and specs. Creating a workspace is cheap — it copies files but doesn't duplicate the content-addressable storage.

## Architecture

```
my-project/                              ← main workspace
├── .writ/
│   ├── workspaces/
│   │   ├── main/
│   │   │   ├── HEAD
│   │   │   ├── index.json
│   │   │   └── heads/
│   │   ├── auth-team/
│   │   │   ├── HEAD
│   │   │   ├── index.json
│   │   │   ├── heads/
│   │   │   └── config.toml             ← ancestor seal, workspace path
│   │   └── payments-team/
│   │       ├── HEAD
│   │       ├── index.json
│   │       ├── heads/
│   │       └── config.toml
│   ├── objects/                          ← SHARED across all workspaces
│   ├── seals/                            ← SHARED (tagged by workspace)
│   └── specs/                            ← SHARED (assignable to workspaces)
├── src/                                  ← main workspace files
└── ...

../ws-auth/                              ← auth-team workspace directory
├── .writ-workspace                      ← pointer back to main .writ/
├── src/                                  ← auth-team's version of files
└── ...
```

The main project directory IS the "main" workspace. Other workspaces are sibling directories (or anywhere you choose). All share the same `.writ/` directory for objects, seals, and specs. Each has its own index, HEAD, and file state.

## Spec Assignment

Specs can be assigned to workspaces to scope visibility:

```bash
# Assign specs to a workspace
writ spec assign auth-0 --workspace auth-team
writ spec assign auth-1 --workspace auth-team

# Or assign during workspace creation
writ workspace create auth-team --specs auth-0,auth-1,auth-2

# Glob patterns work too
writ workspace create payments-team --specs "pay-*"

# Unassign to make a spec global again
writ spec unassign auth-0
```

A spec assigned to a workspace is visible in that workspace and the main workspace. A spec with no workspace assignment (the default) is globally visible in all workspaces.

## Scoped Context

This is the killer feature. When `writ context` runs inside a workspace, it returns only workspace-relevant data:

- **Specs:** Only specs assigned to this workspace, plus global specs
- **Seals:** Only seals created in this workspace
- **Files:** The workspace's own file state
- **Agent activity:** Only agents who sealed in this workspace
- **Dependencies:** Read-only summary of specs from other workspaces that your specs depend on

```bash
# Full context in a 30-spec project
cd my-project
writ context           # 30 specs, all seals, all files

# Scoped context in the auth workspace
cd ../ws-auth
writ context           # 3 specs, auth-team seals, auth files only
```

The scoped context is smaller — fewer specs, fewer seals, fewer files. That means fewer tokens for the agent's context window, less noise, and a sharper picture of the work that matters. At fleet scale, the savings are significant: each agent reads context multiple times per session, and scoped context can be dramatically smaller than full project context.

Dependencies from other workspaces appear as a read-only summary so agents know about upstream work without drowning in unrelated detail:

```
dependencies[1]{id,title,status,workspace}:
  shared-config,App configuration,complete,main
```

## The Workspace Workflow

A typical multi-team workflow:

```bash
# 1. Set up the project and create specs
writ init
writ spec add --id auth-0 --title "JWT authentication"
writ spec add --id auth-1 --title "OAuth integration"
writ spec add --id pay-1 --title "Stripe payments"
writ spec add --id pay-2 --title "Invoice generation"

# 2. Create workspaces with spec assignments
writ workspace create auth-team --path ../ws-auth --specs "auth-*"
writ workspace create payments-team --path ../ws-payments --specs "pay-*"

# 3. Agents work in their respective workspaces
# Auth agents:
cd ../ws-auth
writ context                    # scoped to auth specs only
writ seal -s "JWT endpoint" --agent auth-dev --spec auth-0
writ spec done auth-0

# Payment agents:
cd ../ws-payments
writ context                    # scoped to payment specs only
writ seal -s "Stripe integration" --agent pay-dev --spec pay-1
writ spec done pay-1

# 4. Human checks progress from the main project
cd my-project
writ status                     # shows all workspaces, all specs

# 5. Converge when ready
writ converge-workspaces auth-team payments-team

# 6. Finish to git
writ finish
git push
```

## Convergence Across Workspaces

When workspaces converge, the process mirrors writ's existing convergence pipeline:

1. Each workspace's current file state is compared against the common ancestor (the state when workspaces were created)
2. Files changed in only one workspace are taken as-is (clean merge)
3. Files changed in multiple workspaces go through the convergence engine — structural analysis, pattern matching, confidence scoring
4. The merged result is written to the main workspace
5. A convergence seal records which workspaces were merged

```bash
# Preview convergence without applying
writ converge-workspaces auth-team payments-team --dry-run

# Converge with strategy selection
writ converge-workspaces auth-team payments-team --strategy escalate

# Partial convergence (not all workspaces need to be ready)
writ converge-workspaces auth-team --strategy escalate
```

The same convergence strategies apply: `escalate` (recommended, auto resolves high confidence conflicts), `three-way-merge` (leaves conflicts unresolved), `most-recent` (prefers newest changes), `orchestrator` (structured JSON for programmatic resolution).

After convergence, `writ finish` generates commit messages that include a workspace breakdown:

```
Converge auth, payments, and UI teams

Workspaces:
- auth-team: 3 specs (auth-0, auth-1, auth-2)
- payments-team: 4 specs (pay-1, pay-2, pay-3, pay-4)

7 specs completed · 20 files merged · 0 conflicts
```

## Managing Workspaces

```bash
# List all workspaces
writ workspace list
#   main             .                  0 specs    base workspace
#   auth-team        ../ws-auth         3 specs    2 complete, 1 in-progress
#   payments-team    ../ws-payments     4 specs    4 complete ← ready to converge

# Delete a workspace (preserves all seals and specs in the shared store)
writ workspace delete auth-team

# Delete but keep the files on disk
writ workspace delete auth-team --keep-files
```

Deleting a workspace removes the workspace state and its parallel directory. It does NOT delete seals, specs, or objects from the shared store. History is preserved. You cannot delete the main workspace.

## All Commands Work in Workspaces

After the workspace is created, every writ command works from the workspace directory. No special flags needed — writ detects the workspace from the `.writ-workspace` pointer file:

```bash
cd ../ws-auth

writ seal -s "added auth module" --agent dev --spec auth-0    # seals to auth-team workspace
writ context                                                   # scoped to auth-team
writ status                                                    # reads from auth-team
writ log                                                       # auth-team seal history
writ diff                                                      # auth-team file changes
writ spec done auth-0                                          # marks spec complete
writ restore a3f8b2                                            # restores in auth-team only
```

## MCP and Slash Commands

Workspace operations are available through MCP tools and slash commands:

| MCP Tool / Slash Command | What It Does |
|--------------------------|-------------|
| `writ_workspace_create` / `/writ-workspace-create` | Create a new workspace |
| `writ_workspace_list` / `/writ-workspace-list` | List all workspaces |
| `writ_workspace_status` / `/writ-workspace-status` | Workspace details |

The `writ_context` and `writ_log` MCP tools accept an optional `workspace` parameter for cross-workspace visibility when needed.

## When to Use Workspaces

| Scenario | Recommendation |
|----------|---------------|
| Single agent, single task | No workspace needed. Main is sufficient. |
| Multiple agents, same files | Workspaces. Each team gets isolation + convergence. |
| Multiple agents, disjoint files | Specs alone may suffice. Workspaces add scoped context. |
| Large project, many specs | Workspaces reduce context size. Agents see only what matters. |
| CI pipeline with parallel jobs | Workspaces give each job isolation with shared history. |

Workspaces are opt-in. Single-workspace projects (the default) work exactly as before. Workspaces add value when you have multiple teams, many specs, or need the scoped context to keep agent token costs down.
