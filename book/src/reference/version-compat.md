# Version Compatibility

Writ tracks the on-disk format of `.writ/` repositories separately from the binary version. This page explains how schema versioning, auto-migration, and health checks work.

## Schema Version vs Binary Version

The `.writ/version.toml` file tracks two distinct things:

- **Schema version** (`schema_version`): An integer that describes the on-disk layout of `.writ/`. Increments when the directory structure or file formats change. Migrations upgrade old schemas to current.
- **Binary version** (`created_by`, `last_opened_by`): The writ binary version (from Cargo.toml / PyPI). Can move independently of schema version.

```toml
# .writ/version.toml
schema_version = 1
created_by = "0.1.0"
last_opened_by = "0.1.0"
created_at = "2026-03-06T10:00:00Z"
last_opened_at = "2026-03-06T14:30:00Z"
```

The schema version is what matters for compatibility. Two different binary versions with the same schema version are fully compatible.

## How Auto-Migration Works

When you run any writ command that opens a repository, writ checks the schema version:

1. **Load** `.writ/version.toml` (if it exists)
2. **Missing file?** Treat the repo as schema version 0 (pre-versioning legacy repo)
3. **Schema is current?** Continue normally, update `last_opened_by`
4. **Schema is behind?** Run migrations automatically (v0 to v1, v1 to v2, etc.)
5. **Schema is ahead?** Return an error: the repo was created by a newer version of writ

### Migration guarantees

- **Sequential**: Migrations run one step at a time (v0 to v1, then v1 to v2). No steps are skipped.
- **Idempotent**: Running a migration twice produces the same result. Safe to retry after interruption.
- **Backup**: `.writ/version.toml` is backed up to `.writ/version.toml.bak` before any migration starts.
- **Atomic per-step**: After each step succeeds, the version file is updated. A failure mid-chain leaves the repo at the last successful step.
- **Append-only**: Migration functions are never removed. Future versions accumulate steps.

### v0 to v1 migration

The first migration handles all repositories created before schema versioning existed:

- Creates `.writ/version.toml` with schema_version = 1
- Ensures all expected directories exist: `objects/`, `seals/`, `specs/`, `heads/`, `keys/`, `agents/`, `proposals/`, `security/`, `security/events/`
- Creates the `HEAD` file if missing
- If a legacy `settings.json` exists without a `config.toml`, creates a default `config.toml`

This migration runs silently. You may see a brief message in stderr:

```
writ: migrating .writ schema v0 → v1
```

## Checking Repo Version

To see a repository's current version and health status:

```bash
writ doctor
```

Example output for a healthy repo:

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

For machine-readable output:

```bash
writ doctor --json
```

Returns a structured `DoctorReport` with all check results, statuses, and messages.

## Health Checks

`writ doctor` runs 8 checks:

| Check | What it verifies | Fail means |
|-------|-----------------|------------|
| `version_file` | `.writ/version.toml` exists and parses | Corrupt version file (Warning if missing) |
| `schema_version` | Schema version matches current binary | Newer writ created this repo, or migration needed |
| `directories` | Required dirs: objects, seals, specs, heads, keys, agents | Structural damage to `.writ/` |
| `index` | `.writ/index.json` exists and deserializes | Index corruption, rebuild needed |
| `config` | `.writ/config.toml` parses (if present) | Invalid TOML in config |
| `master_key` | `keys/.master` exists | Missing cryptographic key |
| `specs` | All spec JSON files deserialize | Corrupt spec data |
| `seals` | First 50 seal JSON files deserialize | Corrupt seal data |

Exit code is 0 when all checks pass, 1 if any check fails.

## Troubleshooting

### "this repo was created by a newer version of writ"

Your writ binary is older than the one that last touched this repo. The on-disk schema has features your binary doesn't understand.

**Fix**: Update writ to the latest version.

```bash
pip install --upgrade writ-vcs    # if installed via pip
brew upgrade writ                  # if installed via Homebrew
cargo install writ-cli             # if installed from source
```

### Missing `.writ/version.toml`

This means the repo was created before schema versioning existed (pre-beta). Any writ command will auto-migrate it to the current schema. You can also run `writ doctor` to confirm the repo is healthy after migration.

### `writ doctor` reports failures

Doctor is read-only by default. It tells you what's wrong but doesn't fix anything. Common issues:

- **Missing directories**: A partial init or interrupted migration. Re-running `writ init --yes` in the project will recreate missing structure.
- **Corrupt index.json**: The file tracking index is damaged. A fresh `writ bridge import` from git can rebuild it.
- **Missing master key**: The Ed25519 keypair was lost. Seals can still be created (they'll lack signatures) but verification will fail for new seals.

The `--fix` flag is reserved for a future release that will automate common repairs.
