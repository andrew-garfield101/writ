# Output Formats

For most version control systems, output format is an afterthought. Humans read the terminal, so the output is text. But writ's primary consumer isn't a human reading a terminal. It's an LLM agent parsing structured data into its context window. Every unnecessary token in that output is a token the agent can't spend on reasoning.

This is the token tax. JSON's repeated key names, braces, quotes, and commas are structural overhead that carries no information for an agent that already knows the schema. For a single context call, the cost is small. For a fleet of agents making dozens of context calls per session, it compounds into real compute savings.

Writ's format system addresses this directly. Every command that produces structured output supports multiple formats, and the choice is configurable at the global, project, or per command level.

## Available Formats

| Format | Flag | Description |
|--------|------|-------------|
| **JSON** | `json` | Standard JSON with indentation. Maximum compatibility. Human readable. |
| **JSON Compact** | `json-compact` | Minified JSON with no whitespace. Smaller than pretty JSON, still parseable by any JSON library. |
| **TOON** | `toon` | Token Oriented Object Notation. Field names declared once, rows streamed as values. Designed for LLM consumption. |

## The Token Tax

Consider a typical context response for a project with 20 tracked files. In JSON, every row repeats every key name:

```json
{
  "files": [
    {"path": "src/main.rs", "hash": "a3f2b1c...", "modified": "2026-03-04T10:00:00Z", "agent": "cc", "spec": "S-041"},
    {"path": "src/lib.rs", "hash": "b4e3c2d...", "modified": "2026-03-04T09:45:00Z", "agent": "amis", "spec": "S-039"},
    {"path": "src/convergence.rs", "hash": "c5f4d3e...", "modified": "2026-03-04T09:30:00Z", "agent": "cc", "spec": "S-041"}
  ]
}
```

`"path"`, `"hash"`, `"modified"`, `"agent"`, `"spec"`. Repeated for every single row. For 20 files with 5 keys, that's 100 redundant key tokens before you even count the quotes, braces, colons, and commas.

The same data in TOON:

```
files[20]{path,hash,modified,agent,spec}:
  src/main.rs,a3f2b1c...,2026-03-04T10:00:00Z,agent-1,S-041
  src/lib.rs,b4e3c2d...,2026-03-04T09:45:00Z,agent-2,S-039
  src/convergence.rs,c5f4d3e...,2026-03-04T09:30:00Z,agent-1,S-041
```

Field names declared once in the header. Row count declared explicitly. No braces, no repeated keys, no quotes unless a value contains a delimiter. The LLM receives identical information in significantly fewer tokens.

## Benchmarks

All numbers come from writ's benchmark suite, which runs in CI using tiktoken cl100k_base (the BPE tokenizer used by GPT-4 and close enough to Claude's tokenizer for benchmarking). The benchmark generates a realistic `ContextOutput` struct with 5 specs, 10 recent seals, and 40 tracked files, the same struct that `repo.context()` returns in production, and formats it through each formatter. Results are verified against Claude's actual tokenizer via the Anthropic API.

### Git vs Writ: cost per capability

The core question: what does it cost an agent to understand project state?

Without writ, an agent runs multiple git commands (`git status`, `git log`, `git diff --stat`, `git branch -a`), each returning unstructured text that needs parsing and synthesis. With writ, one call returns structured data ready for immediate consumption.

| Source | Tokens | Capabilities | Tokens per Capability |
|--------|--------|-------------|----------------------|
| git (5 commands) | 895 | 4 | 224 |
| writ context (TOON) | 1,685 | 10 | 168 |

**Git capabilities (4):** working state, commit history, diff stat, branches.

**Writ capabilities (10):** working state, seal history, diff, specs, agent activity, integration risk, convergence status, file contention, chain integrity, session state.

Writ's total token count is higher because it delivers 2.5x more information. But per capability, writ is 25% more efficient. It does it in a single call with structured output, versus five separate commands returning text that the agent has to parse, correlate, and synthesize.

### TOON vs JSON: format efficiency

Within writ, TOON reduces the structural overhead of the output itself. Real byte savings on representative project data:

| Data Type | Savings (TOON vs JSON) |
|-----------|----------------------|
| Seal log (20 seals) | **~33%** |
| Full context | **~20%** |
| Spec list | **~10%** |

Actual token savings from BPE tokenization are more modest than byte savings. BPE tokenizers are smart about merging structural characters (`{"` becomes one token, `": "` becomes one token). The measured token reductions are approximately 29% on seal logs, 15% on full context, and 10% on spec lists. Still meaningful, still compounding at scale, but we report the real numbers.

### Fleet scaling

At scale, the efficiency compounds. A 10 agent fleet reading context 5 times each makes 50 writ calls vs 250 git commands. The token cost is 84,250 (writ) vs 44,750 (git). But writ delivers structured, pre-parsed output with full multi-agent awareness. Git output requires each agent to parse, correlate, and synthesize five separate command outputs, adding interpretation overhead that doesn't show up in raw token counts.

The tool call reduction alone is significant. 50 calls vs 250 calls means fewer round trips, less orchestration complexity, and less context window consumed by intermediate parsing.

### Adaptive output

Writ context is adaptive. Empty sections are omitted entirely. No diverged branches means the field doesn't appear, no scope violations means no violations section, low integration risk from a solo agent means no risk block. This isn't a format trick. It's a design principle. Solo agents get a lean context. Fleet deployments get the full picture. The output scales with complexity, not with a fixed schema.

## Verifying benchmarks

Writ's token efficiency claims are tested in CI. You can reproduce them yourself.

### Rust benchmarks (tiktoken cl100k_base)

Requires the writ source and Rust toolchain:

```bash
git clone https://github.com/andrew-garfield101/writ.git
cd writ
cargo test -p writ-core benchmark -- --nocapture
```

This runs 10 benchmark tests covering context, seal logs, spec lists, fleet scaling, and per-section token breakdowns.

### Anthropic API benchmarks (ground truth Claude counts)

Requires Python and an Anthropic API key:

```bash
pip install anthropic
export ANTHROPIC_API_KEY=sk-ant-...
python scripts/anthropic-token-bench.py
```

Add `--live` to benchmark against your own writ repo:

```bash
python scripts/anthropic-token-bench.py --live
```

These use Claude's actual tokenizer via the API, giving exact token counts rather than estimates.

## Choosing a Format

**Use TOON when** the consumer is an LLM agent. This is the primary use case writ was designed for. TOON delivers the same structured data in fewer tokens, leaving more room for reasoning. This is the recommended format for agentic workflows.

**Use JSON when** you need maximum compatibility. JSON is the universal interchange format: every language, every tool, every pipeline can parse it. Use JSON for debugging, for piping writ output to other tools, or when you're unsure what will consume the output.

**Use JSON Compact when** you want JSON compatibility with reduced whitespace. This is a middle ground: parseable by any JSON library, smaller than pretty JSON, but not as token efficient as TOON.

## Configuration

Format preference follows a resolution chain. Higher priority overrides lower:

```
CLI flag          --format toon           (highest priority)
Environment var   WRIT_FORMAT=toon
Project config    .writ/config.toml       [output] format = "toon"
Global config     ~/.writ/config          [output] format = "toon"
Default           json                    (lowest priority)
```

### Setting Format Globally

During `writ init`, you choose a default format that applies to all projects:

```bash
writ config --global output.format toon
```

Or set it during first run setup, where TOON is presented as the recommended option for agent workflows.

### Setting Format Per Project

Override the global default for a specific project:

```bash
writ config output.format json
```

This writes to `.writ/config.toml` and applies only to the current project.

### Setting Format Per Command

Override everything with a flag:

```bash
writ context --format toon
writ context --format json
writ context --format json-compact
writ context -f toon                    # short flag
```

### Environment Variable

For CI and scripting where you want all writ output in a specific format:

```bash
export WRIT_FORMAT=toon
writ context                            # uses TOON without --format flag
```

## Commands That Support Formats

| Command | Notes |
|---------|-------|
| `writ context` | Primary use case for TOON. Full project state. |
| `writ log` | Seal history. Highly tabular, ideal for TOON. |
| `writ spec status` | Active specs and their state. |
| `writ status` | Fleet overview: agents, specs, progress. |
| `writ show` | Single seal detail. |

## Python SDK

The Python SDK supports format selection through the `format` parameter:

```python
import writ

repo = writ.Repository.open(".")

# Default: returns Python dict (parsed internally, no format overhead)
ctx = repo.context()

# TOON string — for embedding directly in LLM prompts
ctx_toon = repo.context(format="toon")

# JSON string — for debugging or piping to tools
ctx_json = repo.context(format="json")
```

The `format="dict"` default is what most Python code wants: a native Python dict with no serialization cost. Use `format="toon"` when building prompts to send to an LLM, so the context string goes directly into the prompt without re-serialization:

```python
import writ

context_str = writ.context(format="toon")

prompt = f"""Here is the current project state:

{context_str}

Your task: implement the storage compression feature per spec S-042.
"""
# context_str is already TOON — minimal tokens, maximum information
```

## TOON Format Reference

TOON (Token Oriented Object Notation) uses a tabular header format. Field names are declared once, then each row is a comma separated list of values in the same order.

### Syntax

```
section_name[row_count]{field1,field2,field3}:
  value1,value2,value3
  value4,value5,value6
```

### Rules

- **Unquoted strings**: Values without commas or newlines are unquoted (`src/main.rs`)
- **Quoted strings**: Values containing commas or newlines are quoted (`"commit message, with comma"`)
- **Empty values**: Represented as empty fields between commas (trailing comma for last field)
- **Row count**: Always declared in the header (`[20]`). Helps agents validate completeness.
- **Unicode**: Passed through as is. TOON is UTF-8.
- **Nested data**: Falls back to indented object notation for non tabular structures (rare in writ's output).

### Full Context Example

```
# writ context | project: my-app | format: toon | timestamp: 2026-03-04T12:00:00Z

files[3]{path,hash,modified,agent,spec}:
  src/main.rs,a3f2b1c,2026-03-04T10:00:00Z,agent-1,S-041
  src/lib.rs,b4e3c2d,2026-03-04T09:45:00Z,agent-2,S-039
  src/convergence.rs,c5f4d3e,2026-03-04T09:30:00Z,agent-1,S-041

seals[2]{id,summary,agent,timestamp,spec}:
  seal-0041,Implement phase 3 pattern matching,agent-1,2026-03-04T10:00:00Z,S-041
  seal-0039,Auth middleware and token validation,agent-2,2026-03-04T09:45:00Z,S-039

specs[2]{id,description,status,agent}:
  S-041,Convergence phase 3,active,agent-1
  S-039,Auth middleware,complete,agent-2
```

The single line comment header at the top costs a few tokens but gives the LLM metadata about what it's reading: project name, format, timestamp. Negligible cost, meaningful orientation.

## Next Steps

- **[Configuration](../reference/configuration.md)** for the full config file reference
- **[CLI Reference](../reference/cli.md)** for all commands that support `--format`
- **[Python SDK](../reference/python-sdk.md)** for programmatic format selection
