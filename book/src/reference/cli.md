# CLI Reference

Complete reference for all writ commands.

## Core Commands

### `writ init`

Guided setup: initializes writ, scans environment, detects git, imports baseline, configures agent frameworks, sets output format preferences.

```bash
writ init [OPTIONS]

Options:
  --yes / -y                   Accept all defaults, no prompts (CI-safe)
  --bare                       Create only .writ/ directory, no framework integration
  --no-git                     Skip git integration even if git repo detected
  --no-claude                  Skip Claude Code integration
  --no-codex                   Skip Codex / OpenAI integration
  --no-generic                 Skip generic agent instructions
  --frameworks <LIST>          Comma-separated: claude,codex,generic
  --format <FORMAT>            Set output format: toon, json, json-compact
  --name <PROJECT>             Set project name (default: directory name)
  --profile <PROFILE>          Deployment profile (storage budgets, retention)
  --spec <SPEC>                Create a spec during init
  --title <TITLE>              Title for the spec (used with --spec)
  --description <DESCRIPTION>  Description for the spec (used with --spec)
```

> **Note:** `writ install` and `writ uninstall` are deprecated aliases for `writ init` and `writ uninit` respectively. They print a deprecation notice and call the new command.

### `writ uninit`

Remove writ from the project. Deletes `.writ/` directory and framework hooks.

```bash
writ uninit [OPTIONS]

Options:
  --force             Skip confirmation prompt
  --keep-writignore   Keep the .writignore file
```

### `writ task`

Create a task for an agent. One command that creates a spec, a workspace, and prints a launch command.

```bash
writ task <TITLE> [OPTIONS]

Options:
  --id <ID>              Override the auto-derived spec/workspace ID
  --format <FORMAT>      Output: human (default), json
```

The title is slugified into a spec ID automatically (e.g. "backend API work" becomes `backend-api-work`). Use `--id` to override.

Output:

```
task created: backend-api-work
  workspace: workspaces/backend-api-work/

  Launch an agent in this workspace:
    cd workspaces/backend-api-work/

  Suggested prompt: "Backend API work."
  Or provide your own prompt for the agent.
```

The first `writ task` invocation adds `workspaces/` to `.gitignore` automatically. Running `writ task` from inside a workspace shows a warning — tasks should be created from the main project directory.

### `writ task list`

Show all active tasks and their status.

```bash
writ task list [OPTIONS]

Options:
  --format <FORMAT>      Output: human (default), json
```

### `writ plan`

Batch spec creation from a list of tasks.

```bash
writ plan [TASKS]... [OPTIONS]

Options:
  -f, --file <PATH>        Read tasks from a file (one per line)
```

Reads from inline arguments, a file, or stdin. Titles are slugified into spec IDs automatically (e.g. "Implement OAuth2 auth" becomes `implement-oauth2-auth`).

Output:

```
3 specs created:
  implement-oauth2-auth     "Implement OAuth2 auth"
  add-stripe-payments       "Add Stripe payments"
  build-admin-dashboard     "Build admin dashboard"

Next: launch your agents. They discover specs via `writ context`.
Run `writ watch` to enable automatic convergence.
```

### `writ seal`

Create a structured checkpoint from current changes. When `--spec` is provided, the seal captures only the files that changed since this agent's last seal for this spec (spec-scoped sealing). Without `--spec`, the seal captures all changes in the working directory.

```bash
writ seal -s <SUMMARY> [OPTIONS]

Options:
  -s, --summary <SUMMARY>       Summary of what changed and why (required)
  --agent <AGENT>                Agent identifier [default: human]
  --spec <SPEC>                  Linked spec ID (enables spec-scoped change detection)
  --status <STATUS>              Task status: in-progress, complete, blocked [default: in-progress]
  --paths <PATHS>                Seal only these paths (comma-separated)
  --tests-passed <N>             Number of tests that passed
  --tests-failed <N>             Number of tests that failed
  --linted                       Whether the code was linted
  --allow-empty                  Allow sealing with no file changes
  --expected-head <SEAL_ID>      Optimistic conflict detection
  --enforce-scope                Reject out of scope file changes
```

When `--spec` is used with an unclaimed spec, the spec is auto-claimed by this agent on first seal.

### `writ context`

Structured project state for LLM consumption.

```bash
writ context [OPTIONS]

Options:
  --spec <SPEC>              Scope to a specific spec
  --for-agent <AGENT>        Scope entire context to an agent's world
  --seal-limit <N>           Maximum recent seals to include [default: 10]
  --status <STATUS>          Filter seals by status
  --agent <AGENT>            Filter seals by agent ID
  --format <FORMAT>          Output: json, toon, human, brief
```

When called from inside a workspace created by `writ task`, the output includes a `task` field at the top with the assigned task ID, title, and status.

When unclaimed specs exist (created via `writ plan` or `writ spec add` but not yet claimed by an agent), the output includes an `unclaimed_specs` section listing available tasks.

### `writ watch`

Live seal event monitoring. Shows agent activity in real time without needing to poll `writ status`.

```bash
writ watch [OPTIONS]

Options:
  --interval <SECONDS>       Polling interval [default: 5]
  --daemon                   Run as background process
  --stop                     Stop running daemon
  --status                   Show daemon status and recent activity
```

By default, runs in the foreground showing real time output. Press `q` to quit. Seal events display as they happen, with overlapping files flagged for visibility.

Daemon mode (`--daemon`) runs the watcher as a background process with output logged to `.writ/watch.log` and PID stored in `.writ/watch.pid`.

Note: convergence runs automatically at `writ spec done` and as a backstop at `writ finish`. Watch is a monitoring tool, not a convergence trigger.

Configuration via `.writ/config.toml`:

```toml
[watch]
interval = 5              # polling interval in seconds
auto_converge = true      # auto-converge on overlap detection
max_retries = 3           # convergence retry limit before escalating
log_file = ".writ/watch.log"
```

### `writ status`

High level project overview. Agent activity, spec progress, commit readiness. Complements `writ state` (low level working directory state) with a fleet aware, progress oriented view.

```bash
writ status [OPTIONS]

Options:
  --completed              Show all completed specs in detail
  --active                 Show all in-progress specs in detail
  --agent <AGENT>          Filter to a single agent's work
  --spec <SPEC>            Detail view of one spec
  --watch                  Live-updating view (refreshes every 5s)
  --interval <SECONDS>     Refresh interval for --watch [default: 5]
  --format <FORMAT>        Output: json, toon, json-compact
```

The default view adapts automatically to project scale — expanding details for small projects, collapsing to summaries for large fleets. Use filter flags to drill down.

When tasks exist (specs created via `writ task`), they appear under a "Tasks" header showing task name, agent, seal count, and status.

### `writ log`

Show seal history.

```bash
writ log [OPTIONS]

Options:
  --all              Include seals from diverged branches
  --spec <SPEC>      Filter by spec ID
  --limit <N>        Maximum entries
  --format <FORMAT>  Output: json, human
```

### `writ show`

Inspect a specific seal.

```bash
writ show <SEAL_ID> [OPTIONS]

Options:
  --diff             Show file changes
  --format <FORMAT>  Output: json, human
```

### `writ diff`

Show content level diff of working directory changes. Enhanced with spec and agent aware filtering.

```bash
writ diff [OPTIONS]

Options:
  --completed              Diff across completed specs (default)
  --spec <SPEC>            Diff for a single spec
  --agent <AGENT>          Diff for a single agent's work
  --all                    Include in-progress specs
  --file <PATH>            Diff for a single file
  --stat                   Summary only (files and line counts)
  --full                   Full unified diff output
  --format <FORMAT>        Output format for machine consumption
```

### `writ state`

Show working directory state (new, modified, deleted files).

```bash
writ state [OPTIONS]

Options:
  --format <FORMAT>  Output: json, human
```

### `writ restore`

Restore working directory to a seal's state.

```bash
writ restore <SEAL_ID>
```

## Round Trip Commands

### `writ summary`

Generate session summary for git commits and PRs.

```bash
writ summary --format <FORMAT>

Formats:
  commit   One-line commit message with provenance
  pr       Full PR description with spec/agent breakdown
  human    Detailed session overview
  json     Machine-readable output
```

### `writ finish`

Promote completed specs to git commits. Auto-converges outstanding changes before committing (both same-directory overlaps and workspace changes). Interactive spec selection, commit strategy options, and auto generated messages from seal history.

```bash
writ finish [OPTIONS]

Options:
  --yes, -y                Accept defaults, no prompts (matches existing behavior)
  --full                   Per-spec commits with interactive review
  --strategy <STRATEGY>    Commit strategy: single (default), per-spec, grouped
  --message, -m <MSG>      Commit message (skips message prompt)
  --specs <LIST>           Comma-separated spec IDs to include
  --all                    Include all completed specs (no selection prompt)
  --dry-run                Preview what would be committed without doing it
  --cleanup                Auto-clean workspace directories after commit (no prompt)
  --no-cleanup             Keep workspace directories after commit (no prompt)

  # Auto mode (workflow.commit_mode = "auto")
  --auto                   Commit immediately without approval
  --verify                 Run verification command before committing
  --no-verify              Skip verification command
```

When workspaces exist with changes, `writ finish` automatically:
1. Detects all non-main workspaces
2. Runs convergence (prints merge report)
3. If convergence has unresolved escalations, errors out before committing
4. If convergence is clean, proceeds to git commit
5. After commit, prompts for workspace directory cleanup (default: yes)

## Convergence Commands

### `writ converge`

Two-spec convergence.

```bash
writ converge <LEFT_SPEC> <RIGHT_SPEC> [OPTIONS]

Options:
  --apply            Apply the merge result
  --format <FORMAT>  Output: json, human
```

### `writ converge-all`

Merge all diverged branches.

```bash
writ converge-all [OPTIONS]

Options:
  --apply              Apply merge results
  --dry-run            Preview without applying
  --strategy <STRAT>   escalate, three-way-merge, most-recent, orchestrator
  --format <FORMAT>    Output: json, human
```

### `writ converge-workspaces`

Merge across named workspaces.

```bash
writ converge-workspaces <WORKSPACE>... [OPTIONS]

Options:
  --apply              Apply merge results
  --dry-run            Preview without applying
  --strategy <STRAT>   escalate, three-way-merge, most-recent, orchestrator
  --format <FORMAT>    Output: json, human
```

Non-overlapping changes merge cleanly. Overlapping changes go through the convergence engine.

## Spec Management

### `writ spec add`

Register a new spec.

```bash
writ spec add "<DESCRIPTION>" [OPTIONS]

Options:
  --id <ID>              Custom spec ID (default: auto-generated hash)
  --title <TITLE>        Custom title (default: derived from description)
```

### `writ spec status`

Show specs, optionally filtered by lifecycle state.

```bash
writ spec status [OPTIONS]

Options:
  --state <STATE>   Filter: active, stale, completed, cancelled, archived
```

### `writ spec claim`

Claim an unclaimed spec for the current agent. Prevents other agents from picking up the same task.

```bash
writ spec claim <ID> [OPTIONS]

Options:
  --agent <AGENT>    Agent identifier (auto-detected if not provided)
```

If the spec is already claimed by another agent, returns an error with the claiming agent's ID.

Specs are also auto-claimed on first seal: when `writ seal --spec X` runs and spec X has no owner, it is automatically claimed by the sealing agent.

### `writ spec done`

Mark a spec as completed. Creates a final seal, transitions the spec from active to completed, and prints hints for the user.

```bash
writ spec done <ID> [OPTIONS]

Options:
  -s, --summary <MSG>    Completion summary (used in commit message by writ finish)
```

If the agent has exactly one active spec, the ID can be omitted: `writ spec done`.

### `writ spec complete`

Mark a spec as completed (same as `writ spec done` without the final seal creation).

```bash
writ spec complete <ID>
```

### `writ spec cancel`

Cancel a spec.

```bash
writ spec cancel <ID>
```

### `writ spec assign`

Assign a spec to a workspace. Assigned specs are visible only in their workspace and the main workspace.

```bash
writ spec assign <SPEC-ID> --workspace <NAME>
```

### `writ spec unassign`

Remove a spec's workspace assignment. The spec becomes globally visible in all workspaces.

```bash
writ spec unassign <SPEC-ID>
```

### `writ spec reopen`

Reopen a completed spec for continued work. Sets the spec back to active. Seal history is preserved.

```bash
writ spec reopen <ID>
```

## Workspace Commands

### `writ workspace create`

Create a new isolated workspace with its own directory and files.

```bash
writ workspace create <NAME> [OPTIONS]

Options:
  --path <DIR>          Directory for the workspace (default: workspaces/<name>/)
  --specs <FILTER>      Assign matching specs (glob or comma-separated IDs)
  --from <WORKSPACE>    Create from another workspace's state instead of main
```

Creates a parallel directory with a full copy of the project files, its own index and HEAD, and a `.writ-workspace` pointer file back to the main project. All writ commands work from the workspace directory automatically.

### `writ workspace list`

List all workspaces with paths and spec counts.

```bash
writ workspace list

# Output:
#   main             .                  0 specs    base workspace
#   auth-team        ../ws-auth         3 specs
#   payments-team    ../ws-payments     4 specs
```

### `writ workspace status`

Show detailed status for a workspace, including spec progress and seal counts.

```bash
writ workspace status [NAME]
```

### `writ workspace delete`

Delete a workspace. Removes workspace state and parallel directory. Does NOT delete seals, specs, or objects from the shared store.

```bash
writ workspace delete <NAME> [OPTIONS]

Options:
  --force         Skip confirmation prompt
  --keep-files    Preserve the parallel directory on disk
```

Cannot delete the main workspace.

## Security Commands

### `writ verify`

Verify seal chain integrity and signatures.

```bash
writ verify [OPTIONS]              # Full chain verification (default)
writ verify --seal <ID> [OPTIONS] # Single seal verification

Options:
  --format <FORMAT>  Output: json, human
```

### `writ security events`

View the security event audit log.

```bash
writ security events [OPTIONS]

Options:
  --severity <LEVEL>      Filter: info, warning, critical
  --event-type <TYPE>     Filter by event type
```

## Agent Management

### `writ agent register`

Register an agent identity.

```bash
writ agent register --id <ID> [OPTIONS]

Options:
  --role <ROLE>            Agent role
  --trust-level <LEVEL>    full, standard, restricted, untrusted
```

## Git Bridge

### `writ bridge import`

Import git working tree as a baseline seal.

```bash
writ bridge import [OPTIONS]

Options:
  --git-ref <REF>   Git reference to import
```

### `writ bridge export`

Export seals as git commits.

```bash
writ bridge export [OPTIONS]

Options:
  --branch <BRANCH>   Target git branch
  --pr-body           Include PR-style metadata
```

## Garbage Collection

### `writ gc status`

Storage breakdown and stale spec warnings.

```bash
writ gc status
```

### `writ gc run`

Generate and execute a cleanup plan.

```bash
writ gc run [OPTIONS]

Options:
  --dry-run   Preview without executing
  --yes       Skip confirmation prompt
```

### `writ gc storage`

Detailed storage usage by category.

```bash
writ gc storage
```

### `writ gc log`

GC audit history.

```bash
writ gc log [OPTIONS]

Options:
  --limit <N>   Maximum entries
```

## Maintenance Commands

### `writ doctor`

Run health checks on a `.writ/` repository. Read only by default.

```bash
writ doctor [OPTIONS]

Options:
  --json               Machine-readable JSON output (DoctorReport)
  --fix                Reserved for future use (auto-repair common issues)
```

Runs 8 checks:

| Check | What it verifies |
|-------|-----------------|
| `version_file` | `.writ/version.toml` exists and parses |
| `schema_version` | Schema version matches current binary |
| `directories` | Required dirs: objects, seals, specs, heads, keys, agents |
| `index` | `.writ/index.json` exists and deserializes |
| `config` | `.writ/config.toml` parses (if present) |
| `master_key` | `keys/.master` exists |
| `specs` | All spec JSON files deserialize |
| `seals` | First 50 seal JSON files deserialize |

Example output:

```
  ✓ version_file — version.toml exists and parses
  ✓ schema_version — schema version 1 is current
  ✓ directories — all expected directories present
  ✓ index — index.json exists and deserializes
  ✓ config — config.toml exists and parses
  ✓ master_key — master key present
  ✓ specs — 3 spec file(s) OK
  ✓ seals — 12 seal file(s) sampled, all OK

  8 passed, 0 failed, 0 warnings
```

Exit code 0 when all checks pass, 1 if any check fails.

See [Version Compatibility](version-compat.md) for details on schema versioning, auto-migration, and troubleshooting.

## Remote Sync

### `writ push`

Push local state to a remote.

```bash
writ push [REMOTE]
```

### `writ pull`

Pull remote state into local.

```bash
writ pull [REMOTE]
```

### `writ remote`

Manage remotes.

```bash
writ remote init <PATH>        # Initialize a remote repository
writ remote add <NAME> <PATH>  # Add a named remote
writ remote status [REMOTE]    # Check remote state
```

## MCP Server

### `writ mcp-serve`

Start the MCP server. Communicates over stdio using the standard MCP protocol. Normally started automatically by the MCP client (Claude Code or Claude Desktop) using the config in `.mcp.json`.

```bash
writ mcp-serve
```

The server exposes 21 tools matching the full writ CLI. Each tool calls the CLI via subprocess — same behavior, same output, same enforcement. See the [MCP server guide](../guides/mcp-server.md) for the full tool list and setup instructions.

### `writ mcp-install`

Generate MCP configuration for agent frameworks.

```bash
writ mcp-install [OPTIONS]

Options:
  --desktop     Write config for Claude Desktop instead of project .mcp.json
  --global      Write config to global Claude Code settings
```

**Default (no flags):** Writes `.mcp.json` to project root. Commit this to git for zero setup team adoption.

**`--desktop`:** Writes to Claude Desktop's config file (`~/Library/Application Support/Claude/claude_desktop_config.json` on macOS).

> **Note:** `writ init` automatically generates `.mcp.json` when Claude Code is detected. Use `writ mcp-install` for manual setup or when adding MCP to an existing project.
