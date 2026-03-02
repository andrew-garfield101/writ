# CLI Reference

Complete reference for all writ commands.

## Core Commands

### `writ install`

One-command setup: initializes writ, detects git, imports baseline, installs framework hooks.

```bash
writ install [OPTIONS]

Options:
  --format <FORMAT>            Output format: "human" (default) or "json"
  --spec <SPEC>                Create a spec during install
  --title <TITLE>              Title for the spec (used with --spec)
  --description <DESCRIPTION>  Description for the spec (used with --spec)
```

### `writ uninstall`

Remove writ from the project. Deletes `.writ/` directory and framework hooks.

```bash
writ uninstall [OPTIONS]

Options:
  --force             Skip confirmation prompt
  --keep-writignore   Keep the .writignore file
```

### `writ seal`

Create a structured checkpoint from current changes.

```bash
writ seal -s <SUMMARY> [OPTIONS]

Options:
  -s, --summary <SUMMARY>       Summary of what changed and why (required)
  --agent <AGENT>                Agent identifier [default: human]
  --spec <SPEC>                  Linked spec ID
  --status <STATUS>              Task status: in-progress, complete, blocked [default: in-progress]
  --paths <PATHS>                Seal only these paths (comma-separated)
  --tests-passed <N>             Number of tests that passed
  --tests-failed <N>             Number of tests that failed
  --linted                       Whether the code was linted
  --allow-empty                  Allow sealing with no file changes
  --expected-head <SEAL_ID>      Optimistic conflict detection
  --enforce-scope                Reject out of scope file changes
```

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
  --format <FORMAT>          Output: json (default), human, brief
```

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

Show content level diff of working directory changes.

```bash
writ diff
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

One-command round trip: summary, git add, git commit.

```bash
writ finish [OPTIONS]

Options:
  --full       Include PR-style body in commit message
  --dry-run    Preview without committing
```

## Convergence Commands

### `writ converge`

Two-spec convergence (three-way merge).

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
  --strategy <STRAT>   escalate, three-way-merge, most-recent, most-complete, orchestrator
  --format <FORMAT>    Output: json, human
```

## Spec Management

### `writ spec add`

Register a new spec.

```bash
writ spec add --id <ID> --title <TITLE> [OPTIONS]

Options:
  --description <DESC>   Spec description
```

### `writ spec status`

Show specs, optionally filtered by lifecycle state.

```bash
writ spec status [OPTIONS]

Options:
  --state <STATE>   Filter: active, stale, completed, cancelled, archived
```

### `writ spec complete`

Mark a spec as completed.

```bash
writ spec complete <ID>
```

### `writ spec cancel`

Cancel a spec.

```bash
writ spec cancel <ID>
```

## Security Commands

### `writ verify`

Verify seal chain integrity and signatures.

```bash
writ verify --chain [OPTIONS]     # Full chain verification
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
