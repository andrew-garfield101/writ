# Troubleshooting

## Something Broke During Agent Work

Writ's restore system is your safety net. Every seal is an immutable snapshot.

```bash
# Find the last known good seal
writ log --all

# Inspect it to confirm
writ show <SEAL_ID> --diff

# Restore to that state
writ restore <SEAL_ID>

# Seal the restored state
writ seal -s "restored to pre-breakage state" --agent human
```

Restoring doesn't delete history. All previous seals remain in the log.

## Convergence Produced Unexpected Results

Check the convergence report for details:

```bash
writ converge-all --dry-run --strategy escalate
```

Common causes:
- **Low confidence scores:** The engine wasn't sure about the merge. Review the escalated files manually.
- **Structural misparse:** The language analyzer didn't correctly identify a code construct. File a bug report.
- **Overlapping function edits:** Two agents modified the same function body. This is a real conflict. Review and resolve.

## Seal Chain Verification Fails

```bash
writ verify --chain
```

If verification fails, it reports the exact seal where the chain broke. Common causes:
- A seal file was manually edited (don't do this)
- Disk corruption
- An interrupted write operation

Recovery: the seals before the break point are still valid. You can restore to the last valid seal.

## Context Shows Stale Specs

`writ context` automatically detects specs with no recent activity. If a spec is stale:

```bash
# If the spec is done, complete it
writ spec complete <ID>

# If the spec is abandoned, cancel it
writ spec cancel <ID>
```

## Storage Growing Too Large

Check storage usage:

```bash
writ gc status
writ gc storage
```

Run garbage collection:

```bash
writ gc run --dry-run    # Preview what would be cleaned
writ gc run              # Execute with confirmation
```

GC never deletes seals. It only cleans expired working state, archived specs past retention, and old security events.

## Git and Writ Out of Sync

If your git repo has moved ahead of writ's baseline:

```bash
writ bridge import
```

This re-imports the current git state as a new baseline seal.

## Getting Help

- **Bug reports:** [github.com/andrew-garfield101/writ/issues](https://github.com/andrew-garfield101/writ/issues)
- **Source code:** [github.com/andrew-garfield101/writ](https://github.com/andrew-garfield101/writ)
