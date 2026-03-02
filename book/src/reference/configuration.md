# Configuration

*Full configuration reference coming soon.*

## Files

| File | Location | Purpose |
|------|----------|---------|
| `.writ/` | Project root | Writ repository data (seals, objects, heads, keys) |
| `.writignore` | Project root | Files to exclude from tracking (gitignore-compatible syntax) |
| `.writ/gc-config.json` | Inside .writ | Garbage collection settings |
| `.writ/config.json` | Inside .writ | Remote configuration |
| `.writ/keys/` | Inside .writ | Cryptographic keys (convergence keypair) |

## Deployment Profiles

Set during initialization with `writ init --profile <name>`:

| Profile | Storage Budget | Retention | Use Case |
|---------|---------------|-----------|----------|
| `raspberry-pi` | 500 MB | 7 days | Constrained environments |
| `development` | 5 GB | 30 days | Local development |
| `production` | 100 GB | 90 days | Production systems |
| `enterprise` | Unlimited | 365 days | Enterprise deployments |

## .writignore

Same syntax as `.gitignore`. Created automatically by `writ install` with sensible defaults:

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
