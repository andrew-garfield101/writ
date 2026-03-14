# Quickstart

Get from zero to a working writ workflow in five minutes.

## Prerequisites

- A project directory (with or without git)
- Writ installed ([see Installation](installation.md))

## 1. Initialize Writ in Your Project

Navigate to your project and run:

```bash
cd my-project
writ init
```

On first run, writ prompts for your name, preferred output format, and default agent frameworks. These go into a global config (`~/.writ/config`) that applies to all projects. Then it sets up the current project:

```
initialized writ repository in .writ/
created .writignore
git: main @ a3f8b2c1
imported git baseline: 47 file(s), seal d81a5736e16d
tracked: 47 file(s)

output format: toon (token-optimized for LLM agents)
workflow mode: user (you run `writ finish` to commit)
```

Writ detects your environment automatically. If you're in a git repo, it reads the branch and HEAD. If Claude Code is present, it installs slash commands and writ workflow instructions in `CLAUDE.md`. If Codex is detected, it adds workflow instructions to `AGENTS.md`. For any other framework, the CLI and Python SDK work out of the box. Override any setting per project with `.writ/config.toml` or per command with flags.

## 2. Create a Seal

A **seal** is writ's structured checkpoint. After making some changes to your project:

```bash
writ seal -s "added authentication endpoint" --agent dev-1 --spec auth
```

Output:

```
sealed 3 file(s)
seal: a7c2e8f4b31a
agent: dev-1
spec: auth
status: in-progress
```

Every seal records who created it, which spec it serves, and what status the work is in. This is metadata that git forces into commit messages and branch naming conventions. In writ, it's structured data.

## 3. Check Context

This is the single most important command in writ. One call returns everything an agent needs to understand the current project state:

```bash
writ context --format human
```

This shows:
- Active specs and their status
- Recent seals with who did what
- Working directory changes
- File contention (files touched by multiple agents)
- Integration risk level
- Diverged branches with convergence recommendations

For agents consuming this programmatically, use TOON for maximum token efficiency:

```bash
writ context --format toon
```

TOON delivers the same structured data as JSON in up to 33% fewer bytes — field names declared once, rows streamed as values, no braces or repeated keys. Compare this to the alternative: `git log`, `git diff`, `git status`, read a few files, parse all the text, synthesize a mental model. With writ, that entire workflow collapses into one call. See [Output Formats](../concepts/output-formats.md) for benchmark numbers and format details.

## 4. Add a Spec

Specs are structured requirements that agents work against. Think of them as feature tickets that live inside the VCS:

```bash
writ spec add --id auth --title "Authentication System" \
  --description "JWT-based auth with token refresh"
```

Now when agents seal work with `--spec auth`, that work is permanently linked to this requirement. Context output scopes to the spec's files and shows progress. Convergence merges spec by spec. Scope enforcement can restrict agents to their spec's declared files.

## 5. Mark the Spec Complete

When an agent finishes its work on a spec:

```bash
writ spec done auth -s "JWT auth with token refresh, all tests passing"
```

This creates a final seal, transitions the spec to `completed`, and prints a hint:

```
spec auth marked as completed
final seal: b4e3c2d8a91f
agent: dev-1 | seals: 3 | files: 4 changed

run `writ status` to see all completed specs.
run `writ finish` to commit completed work to git.
```

The agent is done. It can terminate or pick up another spec.

## 6. Check Status and Finish

The user checks in on progress from anywhere:

```bash
writ status
```

```
  Active    2 agents    1 spec in progress
  Done      1 agent     1 spec completed (not committed)

  S-auth  Authentication System    dev-1    3 seals    Complete

  1 spec complete · 4 files changed · run `writ finish` when ready
```

When ready, promote completed work to git:

```bash
writ finish
```

`writ finish` shows all completed specs, lets you select which to include, and offers commit strategy options (single commit, per spec, or grouped). It generates a commit message from the full session history — which agents worked on which specs, what was completed, what was tested. Full provenance, automatically.

```bash
# Or manually with a generated commit message
git commit -m "$(writ summary --format commit)"

# Or create a PR with a detailed description
gh pr create --body "$(writ summary --format pr)"
```

## What Just Happened

In five minutes you:

1. **Initialized** writ with automatic environment detection and format configuration
2. **Sealed** a checkpoint with structured metadata (agent, spec, status)
3. **Checked context** in token optimized TOON format — one call, full project state
4. **Defined a spec** that agents can work against
5. **Completed** the spec with `writ spec done` — final seal, clean lifecycle transition
6. **Checked status** to see fleet progress at a glance
7. **Committed** back to git with `writ finish` — auto generated provenance, strategy selection

Git stayed in place the whole time. Writ added the intelligence layer on top.

## Python SDK

Everything above also works through the Python SDK:

```python
import writ

repo = writ.Repository.open(".")
ctx = repo.context(spec="auth")
repo.seal(
    summary="added auth endpoint",
    agent_id="dev-1",
    spec_id="auth",
    tests_passed=12,
)
```

## Multiple Agents

Everything above works with multiple agents in the same project directory. Each agent seals with `--spec`, and writ keeps their changes separate. For multi-agent work:

```bash
# Define tasks in batch
writ plan "Implement auth" "Add payments" "Build dashboard"

# Start the convergence daemon (auto-merges overlapping work)
writ watch

# Launch agents — they discover specs via writ context, claim one, and work
```

No workspaces, no branches, no path management. Agents work in the same directory. `writ watch` auto-converges overlapping changes in real time. See the [Multi Agent Workflow](../guides/multi-agent-workflow.md) guide for the full walkthrough.

## Next Steps

- **[Multi Agent Workflow](../guides/multi-agent-workflow.md)** for running multiple agents in the same directory
- **[Your First Convergence](your-first-convergence.md)** to see what happens when agents' work overlaps
- **[Output Formats](../concepts/output-formats.md)** for TOON benchmarks and format configuration
- **[Seals vs Commits](../concepts/seals-vs-commits.md)** to understand the data model
- **[Configuration](../reference/configuration.md)** for workflow modes, format config, and deployment profiles
- **[Convergence](../concepts/convergence.md)** for the deep dive on semantic merging
