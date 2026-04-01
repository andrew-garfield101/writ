# writ-vcs

**The first AI native version control system for agentic development.** *(writ for short)*

Git was built for humans passing patches between each other. It works. But the world it was designed for (one developer, one branch, manual merges) isn't the world agents work in. When you scale from one agent to five, and from five to fifty, you don't need a better interface to git. You need a version control system that understands what agents actually do.

Writ is that system. Git is the storage layer. Writ is the intelligence layer.

## What Agents Actually Need

Agents don't read `git log` the way humans do. They parse it, tokenize it, reconstruct state from unstructured text across multiple tool calls. Every call costs tokens. Every response needs interpretation. The more agents you have, the more that cost compounds.

Writ replaces that workflow with structured, single call access to everything an agent needs to start working:

- **Context in one call.** `writ context` returns specs, seals, working state, file contention, integration risk, and divergence status as structured JSON. Not text to parse. Not five tool calls stitched together. One call, one response, the format agents work best with.

- **Seals instead of commits.** A seal carries agent identity, spec linkage, test results, status lifecycle, and cryptographic integrity. Every seal is chained to its predecessor via BLAKE3 hashes. The metadata that git forces you to encode in commit messages and branch names is a first class part of the data model.

- **Specs instead of branches.** A spec is a task unit with a hash ID, lifecycle states, agent claiming, and a genesis snapshot, closer to an issue inside the VCS than a branch. When an agent seals work against a spec, that link is permanent. Context groups work by spec. Convergence merges spec by spec. Scope enforcement keeps agents in their lane.

- **Convergence instead of merge.** Writ's convergence engine merges agent work using sealed histories and genesis trees as the common ancestor. Non-conflicting changes compose automatically. Real conflicts escalate with structured context and confidence scores. Giving an agent better git access doesn't make `git merge` smarter. Convergence is the problem, and it requires a purpose built solution.

## Where Writ Fits

Writ sits alongside git, not instead of it. A human checks out a branch, runs `writ init`, and agents work in writ: sealing checkpoints, checking context, converging changes. When they're done, `writ finish` commits everything back to git with full provenance. Git handles what it's good at: storage, distribution, collaboration history. Writ handles what it was never designed for: agent native workflows at scale.

```
 Human world                    Agent world                       Human world
┌──────────┐  writ init     ┌─────────────────┐  writ finish   ┌──────────────┐
│ git repo │ ──────────────→│ agents work in  │ ─────────────→ │ git commit   │
│ (branch) │                │ writ: specs,    │                │ with full    │
│          │                │ seals, context  │                │ provenance   │
└──────────┘                └─────────────────┘                └──────────────┘
```

## When You Need It

If you have one agent doing straightforward work on isolated files, git is probably fine. Writ's value is proportional to complexity.

The inflection point comes when:

- Multiple agents work concurrently on overlapping codebases
- You need structured awareness of who is doing what and which files are contested
- Merging agent work requires understanding code structure, not just line positions
- Context cost matters: agents spend real tokens reconstructing state that writ delivers in one call
- You need cryptographic proof of what happened, who did it, and that nothing was tampered with

The single agent with git world is today. The fleet of fifty agents world is next quarter. Writ is built for that transition.

## Quick Example

```bash
# Set up writ in any project
writ init

# Agents seal checkpoints as they work
writ seal -s "added auth module" --spec auth
writ seal -s "tests passing" --spec auth --tests-passed 42

# One call gives agents everything they need
writ context --format json

# When done, commit back to git
writ finish
```

> **Note:** Writ is in early alpha. The core is stable and thoroughly tested (2,096+ Rust tests, 834+ Python tests), but the API may change before 1.0. We welcome feedback and contributions.

## Next Steps

- **[Installation](getting-started/installation.md)** to get writ on your machine
- **[Quickstart](getting-started/quickstart.md)** for a five minute walkthrough
- **[Seals vs Commits](concepts/seals-vs-commits.md)** to understand the data model
- **[Convergence](concepts/convergence.md)** for the deep dive on how writ merges agent work
