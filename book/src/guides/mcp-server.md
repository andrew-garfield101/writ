# MCP Server

Writ ships a native MCP (Model Context Protocol) server built in Rust. It's part of the `writ` binary — no separate install, no Python runtime, no plugins. When an agent connects via MCP, every writ command is available as a native tool in the agent's palette.

## How It Works

The MCP server is a thin bridge between the MCP protocol and the writ CLI. Each tool function calls the `writ` CLI via subprocess — same behavior, same output, same enforcement as running commands directly. The server communicates over stdio using the standard MCP protocol.

```
┌─────────────────┐         stdio            ┌──────────────────┐
│                  │ ◄─────────────────────► │                  │
│   Claude Code    │    MCP Protocol          │   writ mcp-serve │
│   (or Claude     │    (JSON messages)       │                  │
│    Desktop)      │                          │   Rust binary    │
│                  │    Tool calls:            │   (part of writ) │
│   Agent sees     │    writ_context()        │                  │
│   writ tools     │    writ_seal()           │   Calls writ CLI │
│   natively       │    writ_spec_add()       │   via Command    │
│                  │                          │                  │
└─────────────────┘                          └──────────────────┘
```

## Setup

### Automatic (Claude Code)

`writ init` detects Claude Code and generates `.mcp.json` in your project root:

```bash
writ init
# ✓ Generated .mcp.json (MCP server for Claude Code)
```

The generated `.mcp.json`:

```json
{
  "mcpServers": {
    "writ": {
      "command": "writ",
      "args": ["mcp-serve"]
    }
  }
}
```

Commit this file to git. Every developer who clones the project gets MCP tools automatically — zero setup for the team.

### Manual (Claude Code)

If you skipped init or want to add MCP to an existing project:

```bash
writ mcp-install
```

This writes `.mcp.json` to the project root. Equivalent to what `writ init` generates.

### Claude Desktop

```bash
writ mcp-install --desktop
```

This writes the MCP server config to Claude Desktop's configuration file (`~/Library/Application Support/Claude/claude_desktop_config.json` on macOS).

### Starting the Server

You don't normally start the server manually — the MCP client (Claude Code or Claude Desktop) starts it automatically using the config. But for debugging or testing:

```bash
writ mcp-serve
```

The server starts on stdio and waits for MCP protocol messages.

## Tools

21 tools are available, organized by category. Each tool maps directly to a CLI command.

### Core Workflow

| Tool | Parameters | What It Does |
|------|-----------|-------------|
| `writ_context` | spec (optional), format (optional, default: toon) | Full project state. Run this FIRST at the start of every task. |
| `writ_seal` | summary (required), spec (optional), agent (optional, default: claude-code), paths (optional) | Checkpoint current work. Auto-scoped when agent has one claimed spec. |
| `writ_spec_add` | description (required), id (optional), title (optional) | Create a new task/spec. Hash ID auto-generated from description. |
| `writ_spec_done` | id (optional), summary (optional) | Mark a spec complete. Creates a final seal. |

### Status and Review

| Tool | Parameters | What It Does |
|------|-----------|-------------|
| `writ_status` | completed (optional), active (optional), agent (optional), spec (optional) | Fleet overview — agents, specs, progress. |
| `writ_diff` | spec (optional), agent (optional), stat (optional) | View file changes, optionally scoped to a spec or agent. |
| `writ_log` | spec (optional), agent (optional), all (optional), limit (optional) | Seal history. `all` includes diverged branches. |
| `writ_show` | seal_id (required), diff (optional) | Inspect a specific seal in detail. |

### Spec Management

| Tool | Parameters | What It Does |
|------|-----------|-------------|
| `writ_spec_status` | state (optional), format (optional) | List specs, optionally filtered by lifecycle state. |
| `writ_spec_show` | id (required) | Detailed view of a single spec. |
| `writ_spec_reopen` | id (required) | Reopen a completed spec for continued work. |

### Round Trip

| Tool | Parameters | What It Does |
|------|-----------|-------------|
| `writ_finish` | specs (optional), message (optional), strategy (optional), dry_run (optional) | Promote completed specs to git commits. |
| `writ_summary` | format (optional, default: commit) | Generate commit messages or PR descriptions from seal history. |

### Recovery and Convergence

| Tool | Parameters | What It Does |
|------|-----------|-------------|
| `writ_restore` | seal_id (required) | Restore working directory to any seal's state. |
| `writ_converge` | strategy (optional), dry_run (optional) | Merge diverged branches. Strategies: escalate, three-way-merge, most-recent, orchestrator. |

### Diagnostics

| Tool | Parameters | What It Does |
|------|-----------|-------------|
| `writ_verify` | chain (optional), all_chains (optional), seal (optional) | Verify cryptographic integrity of seals and chains. |
| `writ_doctor` | fix (optional) | Run 8 diagnostic checks on repo health. |

## Smart Defaults

The MCP server applies sensible defaults that reduce the number of parameters agents need to pass:

- **Agent identity**: Defaults to `claude-code`. No need to pass `--agent` on every seal.
- **Output format**: Defaults to `toon` for `writ_context`. Agents get token optimized output without asking.
- **Auto-scoping**: When an agent has exactly one claimed spec, `spec` is optional on `writ_seal` and `writ_spec_done`. The MCP server resolves the claimed spec automatically.
- **Context token**: `writ_context` via MCP writes `.writ/.context_token` (via the CLI passthrough), satisfying the freshness check automatically.

## The Five Layer Adoption Stack

The MCP server is one layer in writ's full agent adoption architecture:

| Layer | What | How |
|-------|------|-----|
| 1. Instructions | Writ workflow in `CLAUDE.md` / `AGENTS.md` | Generated by `writ init` |
| 2. Slash Commands | 20 commands in `.claude/commands/` | Generated by `writ init` |
| 3. MCP Tools | 21 native tools via MCP protocol | `.mcp.json` generated by `writ init` |
| 4. Hooks | SessionStart hook runs `writ context` automatically | Configured by `writ init` |
| 5. Enforcement | Seal requires spec (C.13), context freshness check (C.14) | Built into the CLI |

Each layer reinforces the others. Instructions tell agents what to do. Slash commands and MCP tools make it easy. Hooks provide automatic context. Enforcement catches anything that slips through. An agent that ignores the instructions still sees writ tools in its palette, still gets context injected at session start, and still gets blocked if it tries to seal without a spec.

## Distribution

The MCP server ships with every writ installation method:

```bash
pip install writ-vcs        # includes writ mcp-serve
cargo install writ-vcs      # includes writ mcp-serve
brew install writ            # includes writ mcp-serve
```

Single binary. No runtime dependencies. `writ mcp-serve` just works.
