# Architecture

Writ is one Rust core with three ways in: the CLI, the MCP server and the Python package. All three call the same code, so a seal made from Python is the same seal the CLI would make.

## Crates

```
writ/
├── crates/
│   ├── writ-core/    # objects, index, seals, specs, diff, context, convergence, bridge,
│   │                 # auto seal hooks, receipts, doctor, gc, keys and signing
│   ├── writ-cli/     # the `writ` command (clap)
│   ├── writ-mcp/     # MCP server over stdio (rmcp); writ tools for MCP clients
│   └── writ-py/      # Python bindings (PyO3), published to PyPI as writ-vcs
```

## The `.writ` Directory

Everything writ knows lives in `.writ` at the project root. It is never committed: `writ init` adds it to `.gitignore`.

| Path | What it holds |
|------|---------------|
| `objects/` | File contents, content addressed by SHA-256, compressed with zstd |
| `seals/` | One JSON file per seal: changes, agent, spec, summary, hashes, signature |
| `specs/` | One file per spec: title, status, claim, lifecycle state, closing summary |
| `keys/` | Ed25519 keys: `<identity>.pub`, `<identity>.enc` (encrypted), the master key `.master` |
| `agents/` | Registered agents: trust level, scope, status |
| `security/events.jsonl` | The append only security event log |
| `hooks.log` | One record per auto seal hook decision: what was sealed, refused or asked |
| `receipts/` | The receipt each `writ finish` committed, so `writ handoff --commit` can rebuild it |
| `config.toml` | Project configuration (see [Configuration](configuration.md)) |
| `writ.lock` | The repository lock |

## Storage

Store writes are atomic: each file is written to a temporary file, flushed to disk with fsync, then renamed into place, so a reader sees the old file or the new one and never a partial write. Objects are checked against their hash when read. Concurrent writers take an advisory lock on `writ.lock` (`flock` on Unix), which the operating system releases if a process dies, so a crash never leaves a stale lock.

## Integrity

Each seal carries a BLAKE3 content hash of its own data and a chain hash that combines it with the previous seal's chain hash, so changing any seal breaks every chain hash after it. From 0.4.5 each seal is also signed with an Ed25519 key belonging to the identity that made it, created on that identity's first seal. `writ verify` checks all of it. The [Security Model](../concepts/security-model.md) says what a signature proves and what it does not.

## How Agents Reach Writ

`writ init` detects the frameworks in the project and installs what each needs:

| Framework | Detection | What gets installed |
|-----------|-----------|---------------------|
| Claude Code | `CLAUDE.md` or `.claude/` exists | A writ block in `CLAUDE.md`, slash commands in `.claude/commands/`, the MCP server in `.mcp.json`, and the hooks in `.claude/settings.json` |
| Claude Desktop | `writ mcp-install --desktop` | The MCP server in Claude Desktop's settings |
| Codex | `AGENTS.md` or `.codex/` exists | A writ block in `AGENTS.md` |
| Any agent | Always | `.writignore`, a baseline seal of the current files, and the `writ` CLI |

Under Claude Code the hooks do the sealing. A command's file changes are attributed to the agent that ran it and sealed under that agent's spec as the tool call finishes; when writ cannot tell whose change a file is, it asks with one line rather than guess. The hooks table in [Configuration](configuration.md) lists each hook and what it does.

## Git Underneath

Writ does not replace git. `writ init` imports the current git state as the baseline, agents seal into `.writ`, and `writ finish` converges the finished specs and writes one ordinary git commit whose body is the receipt. From there it is plain git: push, open a pull request, review.

## Building from Source

```bash
# Rust core and CLI
cargo build --release
cargo test -p writ-core -p writ-cli

# Python bindings
cd crates/writ-py
python3 -m venv .venv && source .venv/bin/activate
pip install maturin pytest
maturin develop
pytest tests/
```

The binary is at `target/release/writ`. [What We Measure](numbers.md) lists the release checks and run results for each release, each with its evidence file.
