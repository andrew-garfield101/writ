# writ

**AI-native version control for agentic systems.**

Writ is a version control system designed from the ground up for LLMs and multi-agent development fleets. Its core primitives are **specs** (not branches), **seals** (not commits), and **convergence** (not merging).

## Why not git?

Git was designed for humans typing commands in a terminal. Every interaction is string-in, string-out. When an agent uses git, it shells out to a CLI, parses unstructured text, and reconstructs meaning from output designed for human eyeballs. That's not just slow — it's an entire category of bugs.

Libraries like GitPython and pygit2 help, but the fundamental problem isn't the interface — it's the data model. Git's organizational primitive is the **branch** (a pointer to a commit). It carries no structured metadata about *what task* a commit serves, *which agent* made it, whether *tests passed*, or *what files are in scope*. You can bolt conventions on top, but conventions are things some agents follow and others don't.

Writ's bet is that agent-native metadata belongs **inside** the VCS, not layered on top. Specs, agent identity, verification status, and structured context are first-class — not commit message conventions.

## Core concepts

| Git | Writ | What changes |
|-----|------|-------------|
| Branch | **Spec** | A structured requirement document with status, dependencies, file scope, and acceptance criteria. The unit of work. |
| Commit | **Seal** | A structured checkpoint carrying agent identity, spec linkage, verification metadata, and a content-addressable tree hash. |
| `git status` | `writ state` | Returns structured data (dict/JSON), not text to parse. |
| `git log` | `writ context` | One call returns spec details, recent history, working state, pending changes, and file scope — sized for a context window. |

## The killer feature: `context()`

The most expensive thing in an agent's workflow is building situational awareness. With git, that's 4-5 separate commands, each returning text that needs parsing and synthesis. With writ:

```python
ctx = repo.context(spec="auth-migration")
```

One call. One structured response. Spec details, recent seal history, working state, pending changes, file scope — all in a single JSON-serializable dict. That's not an incremental improvement over git. It's a category difference in how agents bootstrap into a task.

## Quick start

### CLI

```bash
# Initialize
writ init

# Check state (structured output)
writ state --format json

# Create a seal
writ seal --summary "implemented token refresh" --agent worker-1 --spec auth-migration

# Selective seal (only specific paths)
writ seal --summary "auth changes only" --paths src/auth,src/middleware --spec auth-migration

# View seal history
writ log

# Inspect a specific seal
writ show abc123 --diff

# Get structured context for an agent
writ context --spec auth-migration --format json

# Manage specs
writ spec add --id auth-migration --title "Migrate to OAuth2"
writ spec update auth-migration --status in-progress
writ spec status

# Restore to a previous state
writ restore abc123 --force
```

### Python (via PyO3 bindings)

```bash
cd crates/writ-py
python3 -m venv .venv && source .venv/bin/activate
pip install maturin && maturin develop
```

```python
import writ

# Every return value is a dict. json.dumps() always works.
repo = writ.Repository.init("/path/to/project")

# State
state = repo.state()
# {"changes": [{"path": "auth.py", "status": "new", ...}], "tracked_count": 0}

# Seal with full agent identity and spec linkage
seal = repo.seal(
    summary="implemented token refresh endpoint",
    agent_id="claude-worker-3",
    agent_type="agent",
    spec_id="auth-migration",
    status="in-progress",
    paths=["src/auth", "tests/test_auth.py"],
)

# One-call context for task bootstrapping
ctx = repo.context(spec="auth-migration")

# Diff, inspect, restore
diff = repo.diff()
seal_info = repo.get_seal("abc123")
result = repo.restore("abc123")

# Spec management
repo.add_spec(id="auth-migration", title="Migrate to OAuth2")
repo.update_spec("auth-migration", status="in-progress")
specs = repo.list_specs()
```

## Multi-agent scenario

Three agents, each assigned a spec. They work independently, creating seals as they go:

```python
# Agent A — works on auth
ctx = repo.context(spec="auth-migration")  # scoped context, only auth history
# ... does work ...
repo.seal(summary="token refresh endpoint", agent_id="agent-auth",
          spec_id="auth-migration", status="in-progress", paths=["src/auth"])

# Agent B — works on payments
ctx = repo.context(spec="payment-refactor")  # separate scoped context
# ... does work ...
repo.seal(summary="stripe integration", agent_id="agent-payments",
          spec_id="payment-refactor", status="complete", paths=["src/payments"])
```

The human developer checks in:

```bash
$ writ spec status
  > auth-migration       InProgress  (3 seal(s))
  v payment-refactor     Complete    (5 seal(s))
    db-optimization      Pending     (0 seal(s))

$ writ show abc123 --diff
seal abc123def456...
  agent:     agent-auth (Agent)
  spec:      auth-migration
  status:    InProgress
  summary:   token refresh endpoint
  changes:   2 file(s)
    + src/auth/refresh.py
    ~ tests/test_auth.py

--- a/src/auth/refresh.py
+++ b/src/auth/refresh.py
@@ -1,0 +1,42 @@
+async def refresh_token(token: str) -> TokenResponse:
...
```

Full transparency. No branch archaeology. No parsing commit messages to figure out which agent did what.

## Architecture

```
writ/
├── crates/
│   ├── writ-core/    # Core library (Rust) — objects, index, seals, specs, diff, context
│   ├── writ-cli/     # CLI binary (clap)
│   └── writ-py/      # Python bindings (PyO3 + maturin)
```

**Storage model:** Content-addressable object store (SHA-256, 2-char prefix directories). Seals and specs stored as JSON in `.writ/seals/` and `.writ/specs/`. Index at `.writ/index.json`. HEAD pointer at `.writ/HEAD`.

**Test coverage:** 81 Rust tests + 35 Python tests = 116 total tests.

## Building from source

```bash
# Prerequisites: Rust toolchain, Python 3.8+
cargo build          # builds writ-core and writ-cli
cargo test --workspace --exclude writ-py  # runs Rust tests

# Python bindings
cd crates/writ-py
python3 -m venv .venv && source .venv/bin/activate
pip install maturin pytest
maturin develop
pytest tests/
```

## Roadmap

The core VCS primitives are implemented. What's ahead:

- **Convergence** — writ's answer to merging. When multiple agents modify overlapping files, their seals need to reconcile. This is the hardest unsolved problem.
- **Remote/shared state** — `writ push` / `writ pull` for agents running across machines or processes. Currently locking is local (`flock`-based); distributed locking for multi-machine scenarios is future work.
- **Agent SDK** — Higher-level Python toolkit for common agent workflows (task pickup, context loading, seal-on-completion patterns).

## License

AGPL-3.0-only. See [LICENSE](LICENSE) for details.
