# Output Formats

For most version control systems, output format is an afterthought. Humans read the terminal, so the output is text. But writ's primary consumer isn't a human reading a terminal — it's an LLM agent parsing structured data into its context window. Every unnecessary token in that output is a token the agent can't spend on reasoning.

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

`"path"`, `"hash"`, `"modified"`, `"agent"`, `"spec"` — repeated for every single row. For 20 files with 5 keys, that's 100 redundant key tokens before you even count the quotes, braces, colons, and commas.

The same data in TOON:

```
files[20]{path,hash,modified,agent,spec}:
  src/main.rs,a3f2b1c...,2026-03-04T10:00:00Z,cc,S-041
  src/lib.rs,b4e3c2d...,2026-03-04T09:45:00Z,amis,S-039
  src/convergence.rs,c5f4d3e...,2026-03-04T09:30:00Z,cc,S-041
```

Field names declared once in the header. Row count declared explicitly. No braces, no repeated keys, no quotes unless a value contains a delimiter. The LLM receives identical information in significantly fewer tokens.

## Benchmarks

Real measurements from writ's benchmark suite (F.14), comparing payload size across all three formats on representative project data:

| Data Type | JSON | JSON Compact | TOON | TOON vs JSON |
|-----------|------|-------------|------|-------------|
| Seal log (20 seals) | 20,650 B | 14,869 B | 13,733 B | **33% smaller** |
| Full context (5 specs, 10 seals, 40 files) | 8,182 B | 6,252 B | 6,507 B | **20% smaller** |
| Spec list (15 specs) | 5,912 B | 4,831 B | 5,297 B | **10% smaller** |

Seal logs show the most dramatic savings because they're highly tabular — each seal repeats the same fields (id, summary, agent, timestamp, spec). TOON's header once format eliminates all that redundancy. Full context is a mix of nested and tabular data, so savings are more modest but still substantial.

Token savings are typically higher than byte savings. JSON structural characters — `{`, `}`, `[`, `]`, `"`, `:` — each consume individual tokens in most tokenizers. TOON eliminates these entirely, so the token reduction exceeds the byte reduction.

These savings compound. Five agents making 10 context calls per session means hundreds of redundant key tokens per call eliminated. At fleet scale (50+ agents), that translates directly to measurable cost reduction and reduced context window pressure — agents can work on larger projects before hitting limits.

## Choosing a Format

**Use TOON when** the consumer is an LLM agent. This is the primary use case writ was designed for. TOON delivers the same structured data in fewer tokens, leaving more room for reasoning. This is the recommended format for agentic workflows.

**Use JSON when** you need maximum compatibility. JSON is the universal interchange format — every language, every tool, every pipeline can parse it. Use JSON for debugging, for piping writ output to other tools, or when you're unsure what will consume the output.

**Use JSON Compact when** you want JSON compatibility with reduced whitespace. This is a middle ground — parseable by any JSON library, smaller than pretty JSON, but not as token efficient as TOON.

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
| `writ log` | Seal history. Highly tabular — ideal for TOON. |
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

The `format="dict"` default is what most Python code wants — a native Python dict with no serialization cost. Use `format="toon"` when building prompts to send to an LLM, so the context string goes directly into the prompt without re-serialization:

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
  src/main.rs,a3f2b1c,2026-03-04T10:00:00Z,cc,S-041
  src/lib.rs,b4e3c2d,2026-03-04T09:45:00Z,amis,S-039
  src/convergence.rs,c5f4d3e,2026-03-04T09:30:00Z,cc,S-041

seals[2]{id,summary,agent,timestamp,spec}:
  seal-0041,Implement phase 3 pattern matching,cc,2026-03-04T10:00:00Z,S-041
  seal-0039,Language analyzer improvements,amis,2026-03-04T09:45:00Z,S-039

specs[2]{id,description,status,agent}:
  S-041,Convergence phase 3,active,cc
  S-039,Language analyzers,complete,amis
```

The single line comment header at the top costs a few tokens but gives the LLM metadata about what it's reading — project name, format, timestamp. Negligible cost, meaningful orientation.

## Next Steps

- **[Configuration](../reference/configuration.md)** for the full config file reference
- **[CLI Reference](../reference/cli.md)** for all commands that support `--format`
- **[Python SDK](../reference/python-sdk.md)** for programmatic format selection
