# Lifecycle and Storage

A long running repository with many agents accumulates specs nobody touches anymore, objects nothing references, and security events past their use. Writ tracks all three and cleans them only on a plan you can read first.

## Spec Lifecycle

Every spec has a lifecycle state, separate from its work status:

| State | Meaning |
|-------|---------|
| `active` | In use. The default. |
| `stale` | No seal within the stale timeout (2 hours on the development profile). |
| `completed` | All work done; kept until its retention period ends. |
| `cancelled` | Cancelled by hand (`writ spec cancel`) or by expiry. |
| `archived` | Past retention; metadata only. |

Nothing runs on a timer: a spec is marked stale when writ next evaluates it, and the next `writ gc run` cancels a spec that has stayed stale past the expiry timeout (24 hours on the development profile) and archives a completed spec past its retention. Filter by state with `writ spec status --state stale`. A stale claim is released with `writ spec release <id> --stale`, which works only when the holder is provably gone (see the [CLI reference](../reference/cli.md#writ-spec-release)).

## Storage

```bash
writ gc status     # storage used against the budget, and specs by lifecycle state
writ gc storage    # the breakdown by category
writ gc audit      # what is active, what is already committed to git, what can be pruned
writ gc log        # every cleanup writ has run
```

Writ warns when usage passes 80 percent of the profile's budget. A seal is never refused because of storage.

## Cleanup

```bash
writ gc run --dry-run          # show the plan, change nothing
writ gc run                    # show the plan, ask, then run it
writ gc run --committed        # also prune work already committed to git
```

A plan is made of these actions: move a spec to its next lifecycle state, remove a spec that reached a terminal state past its grace period, remove security events past their retention, and prune objects no seal references. A plain `gc run` never deletes a seal, never removes an active spec, and never removes a critical security event.

`--committed` goes further: it prunes the objects and seals of specs already committed to git and older than `--keep-days` (default 7), because that work is in git history. Seal metadata is archived before the seal files go. After `writ finish`, writ runs this committed pruning by itself when more than 500 unreferenced objects have built up (`auto_gc`, `auto_gc_object_threshold` and `auto_gc_keep_days` under `[gc]`).

If objects a seal refers to are already missing from the store, `gc run` stops and lists them rather than compute what is live from a broken store; `writ repair` regenerates them from the working tree or git history, and `--force` proceeds anyway.

Every run is recorded and shown by `writ gc log`.

## Deployment Profiles

`writ init --profile <name>` sets the budget and retention periods:

| Profile | Storage budget | Stale after | Completed specs kept | Info / warning / critical events kept |
|---------|---------------|-------------|----------------------|---------------------------------------|
| `raspberry-pi` | 500 MB | 1 hour | 1 day | 7 days / 30 days / 2 years |
| `development` (default) | 5 GB | 2 hours | 7 days | 30 days / 180 days / 2 years |
| `production` | 100 GB | 4 hours | 30 days | 30 days / 1 year / 2 years |
| `enterprise` | unlimited | 8 hours | 90 days | 90 days / 2 years / 2 years |

Every value can be changed under `[gc]` in `.writ/config.toml`.

## Next Steps

- **[Configuration](../reference/configuration.md)** for the `[gc]` keys
- **[CLI Reference](../reference/cli.md#garbage-collection)** for every `gc` option
- **[Workflow Modes](../guides/workflow-modes.md)** for manual and automatic finishing
