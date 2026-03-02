# Examples

Sample projects demonstrating writ workflows. Each example is self-contained
with step-by-step instructions.

## Solo Workflow

**[`solo-workflow/`](solo-workflow/)** — Using writ as a single developer alongside git.

Covers: install, seal, context, restore, verify, finish. The simplest path
to understanding what writ does and why it's useful.

## Multi-Agent Convergence

**[`multi-agent/`](multi-agent/)** — Multiple agents working in parallel with
automatic convergence.

Covers: specs, parallel seals, divergence detection, convergence, chain
verification. Includes a runnable `demo.py` script that demonstrates the
full workflow in ~3 seconds.

```bash
cd multi-agent
python demo.py
```

This is writ's core differentiator — intelligent merging of parallel agent
work that would cause merge conflicts in git.
