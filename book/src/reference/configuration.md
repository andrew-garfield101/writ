# Configuration

Writ uses a two tier configuration system that mirrors git: a global config for your defaults and a project config for per repository overrides. Any setting in the global config can be overridden at the project level.

## Configuration Hierarchy

```
~/.writ/config              Global defaults (like ~/.gitconfig)
.writ/config.toml           Project-level overrides
CLI flags                   Per-invocation override (highest priority)
Environment variables       WRIT_FORMAT, WRIT_COMMIT_MODE, etc.
```

Resolution order (highest priority first):
1. CLI flag (`--format toon`)
2. Environment variable (`WRIT_FORMAT=toon`)
3. Project config (`.writ/config.toml`)
4. Global config (`~/.writ/config`)
5. Built-in default

## Global Config

Created automatically on first `writ init`. Located at `~/.writ/config`.

```toml
[user]
name = "Andrew"                           # your name for seal attribution

[output]
format = "toon"                           # default output format: json | toon | json-compact

[init]
frameworks = ["claude", "codex", "generic"]  # default frameworks to configure

[workflow]
commit_mode = "user"                      # user | propose | auto
commit_strategy = "single"                # single | per-spec | grouped
```

## Project Config

Created by `writ init` in each project. Located at `.writ/config.toml`.

```toml
[project]
name = "my-project"
profile = "development"                   # deployment profile

[output]
format = "toon"                           # override global format for this project

[workflow]
commit_mode = "user"                      # user | propose | auto
commit_strategy = "single"               # single | per-spec | grouped
stale_timeout = 3600                      # seconds before a spec is flagged stale (0 to disable)

[workflow.auto]
verify_command = "cargo test --quiet"     # must exit 0 for auto-commit to proceed
max_specs_per_commit = 10                 # prevent mega-commits
branch = "writ/auto"                      # target branch for auto commits (not main)
notify = "log"                            # log | stdout | none

[gc]
profile = "development"                   # storage profile
max_storage_bytes = 5368709120            # 5 GB
retention_days = 30
compression_level = 3
```

## Output Formats

Three output formats for `writ context` and other structured commands:

| Format | Flag | Description |
|--------|------|-------------|
| `json` | `--format json` | Standard JSON. Maximum compatibility. Default if no config is set. |
| `toon` | `--format toon` | Token Oriented Object Notation. Field names declared once, rows streamed as values. Recommended for LLM agents. |
| `json-compact` | `--format json-compact` | Minified JSON. No whitespace. Good for CI pipelines. |

Set your default format globally:

```bash
# During first writ init, you'll be prompted:
# Default output format for agent context:
#   (1) toon          Token Oriented Object Notation (optimized for LLM agents)
#   (2) json          Standard JSON (maximum compatibility)
#   (3) json-compact  Minified JSON
```

Or set it directly in config:

```toml
# ~/.writ/config (global)
[output]
format = "toon"

# .writ/config.toml (project override)
[output]
format = "json"
```

Override per command:

```bash
writ context --format toon
writ status --format json
writ diff --format json-compact
```

## Workflow Modes

Three modes that scale from solo developer to enterprise fleet. Set during `writ init` or in config.

### `user` (Default)

The user runs `writ finish` manually. Maximum control, maximum safety.

```toml
[workflow]
commit_mode = "user"
```

Flow: agents complete specs → user runs `writ status` → user runs `writ finish` → git commit created.

### `propose`

An orchestrator proposes commits. The user reviews and accepts.

```toml
[workflow]
commit_mode = "propose"
```

Flow: agents complete specs → orchestrator runs `writ finish --propose` → user reviews with `writ finish --review` → user accepts with `writ finish --accept`.

Good for medium teams (10-50 agents) with supervised autonomous work.

### `auto`

Fully autonomous. Orchestrator commits directly. Requires high trust.

```toml
[workflow]
commit_mode = "auto"

[workflow.auto]
verify_command = "cargo test --quiet"    # must exit 0 to commit
max_specs_per_commit = 10                # prevent mega-commits
branch = "writ/auto"                     # commit to a branch, not main
notify = "log"                           # log | stdout | none
```

Flow: agents complete specs → orchestrator runs `writ finish --auto` → verification runs → git commit created on target branch.

Safety rails: test verification blocks bad commits. Branch targeting keeps auto commits off main. Max specs per commit prevents runaway batches.

## Deployment Profiles

Set during initialization with `writ init --profile <name>`. Controls storage budgets, retention periods, and compression levels.

| Profile | Storage Budget | Retention | Compression | Use Case |
|---------|---------------|-----------|-------------|----------|
| `raspberry-pi` | 500 MB | 7 days | Level 1 | Constrained environments |
| `development` | 5 GB | 30 days | Level 3 | Local development |
| `production` | 100 GB | 90 days | Level 3 | Production systems |
| `enterprise` | Unlimited | 365 days | Level 6 | Enterprise deployments |

## Files

| File | Location | Purpose |
|------|----------|---------|
| `~/.writ/config` | Home directory | Global defaults (format, frameworks, workflow mode) |
| `.writ/` | Project root | Writ repository data (seals, objects, heads, keys, config) |
| `.writ/config.toml` | Inside .writ | Project-level configuration overrides |
| `.writ/gc-config.json` | Inside .writ | Garbage collection settings (profile, budgets, retention) |
| `.writ/keys/` | Inside .writ | Cryptographic keys (Ed25519 convergence keypair, AES-GCM encrypted) |
| `.writignore` | Project root | Files to exclude from tracking (gitignore-compatible syntax) |

## .writignore

Same syntax as `.gitignore`. Created automatically by `writ init` with sensible defaults:

```
.git/
node_modules/
__pycache__/
*.pyc
.env
target/
dist/
build/
```

Add project specific exclusions as needed. Writ respects nested `.writignore` files the same way git respects nested `.gitignore` files.

## Environment Variables

| Variable | Effect | Example |
|----------|--------|---------|
| `WRIT_FORMAT` | Override output format | `WRIT_FORMAT=toon writ context` |
| `WRIT_COMMIT_MODE` | Override workflow mode | `WRIT_COMMIT_MODE=auto writ finish` |
| `WRIT_PROFILE` | Override deployment profile | `WRIT_PROFILE=production writ init` |
