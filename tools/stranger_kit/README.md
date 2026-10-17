# Stranger kit

This kit runs two Claude Code agents at once on a copy of one of your own repositories, with writ installed. It then scores what happened and writes a one-page report you can send back.

It covers running and scoring only. It does not explain how to use writ, and it gives the agents no instructions about writ. They see only what `writ init` installs and what writ prints to them. That is the test.

## What you need

- A git repository of your own with real history, committed and clean.
- `claude` (Claude Code CLI), logged in.
- `python3` 3.10 or later, `git`, and `zstd` (`brew install zstd`). The audit uses `zstd` to read compressed objects.
- writ 0.4.2 or later from one of these. Use exactly one; two installs on PATH is itself a finding.
  - `pipx install writ-vcs` (or `pip install writ-vcs` in a venv)
  - `brew install andrew-garfield101/writ/writ`

  Check with `writ --version`.

The agents run with `--dangerously-skip-permissions`. Use a copy.

## Tests

`python3 -m pytest tools/stranger_kit -q` from the writ repo. Set `STRANGER_BINS` to a colon-separated list of writ binaries to run the no-agent end-to-end check on each (for example the PyPI install and a dev build).

## Run

```bash
KIT=/path/to/writ/tools/stranger_kit   # this directory, from a clone of the writ repo

# 1. A throwaway copy with full history. Never your working checkout.
git clone --no-local /path/to/your/repo /tmp/stranger/repo
OUT=/tmp/stranger/out

# 2. Record the starting state and run `writ init -y` (nothing else).
python3 $KIT/stranger.py setup /tmp/stranger/repo $OUT
#    Already ran `writ init` yourself? Add --skip-init.

# 3. Two prompts, one per file, written the way you would brief an agent on
#    this repository. Do not mention writ. Two tasks that touch some of the
#    same files make the run worth scoring.
$EDITOR /tmp/stranger/prompt-1.txt /tmp/stranger/prompt-2.txt

# 4. Launch both agents together and wait for both to exit.
python3 $KIT/stranger.py launch /tmp/stranger/repo $OUT \
    --prompt /tmp/stranger/prompt-1.txt --prompt /tmp/stranger/prompt-2.txt

# 5. Read the end state, run `writ finish -y`, audit the commits.
python3 $KIT/stranger.py collect /tmp/stranger/repo $OUT

# 6. Score.
python3 $KIT/stranger.py score $OUT
```

Send back `$OUT/report.md` and `$OUT/summary.json`. The transcripts (`$OUT/agent-*.jsonl`) can contain your code, so share them only if you want to.

### Optional injection

Pass one of these to `launch`. Both start after `--inject-after` seconds (default 120).

- `--inject stale-claim`: a third `claude -p` session claims a spec and exits, so its claim belongs to a process that no longer exists.
- `--inject committed-spec`: acts as a human who finishes mid-run. The first agent spec with a seal is marked complete and committed by `writ finish -y` while its agent is still working.

Use the default `none` for your first run.

## What the steps do and do not touch

- `setup` refuses a dirty tree. It records the writ and claude versions, HEAD, `writ doctor` (both tiers), and the files `writ init` generated, then runs `writ init -y`. It changes nothing that `writ init` installs.
- `launch` passes your prompts verbatim. The agents get your environment minus Claude session variables, plus a PATH holding only the writ that `setup` found.
- `collect` copies `.writ/hooks.log` (every writ hook firing), runs `writ doctor` on both tiers, then `writ finish -y` (never `--force`), then `writ doctor` again. It runs `finish_audit.py` over every commit made since setup, plus a store integrity check. `--no-finish` skips the finish.
- `score` reads `$OUT` only.

## What the report says

- **Zero-touch safe finish, per agent.** This requires all of the following: the agent succeeded; it never forced; it never fell back to git commands; its spec is complete or committed; finish exited 0; 0 sealed lines were lost.
  - Strict: no doctor finding was left when both agents exited, apart from a stale claim on the agent's own spec.
  - Lenient: the strict rule without the doctor condition.
- **Unsealed window.** For every edit an agent or its subagents made (Edit, Write, NotebookEdit), the seconds from the edit to the first seal that holds that path, p50 and max, and the edits no seal followed. With auto seal (0.4.3) this is the hook's latency plus the tool's run time.
- **Hook log.** Lines in `.writ/hooks.log`, how many name an error and how many a refused seal.
- **writ commands run by hand, per agent.** Counted and reported, not scored: an agent that closes its task with `writ spec done -s` and its own summary has not missed anything.
- **Lost sealed lines.** Lines an agent sealed that are missing from what finish committed and that no later seal removed.
- **Hurdles.**
  - Per run: what the agents ran, meaning forced commands, refusals, git fallbacks, and `--allow-removals`.
  - Records-based: what writ recorded for the agents' specs, meaning forced and allow_removals seals and seal_refused and finish_refused events, per 10 seals.
- **For each agent:**
  - Every doctor finding and refusal it saw, as the exact text it saw, and every line a writ hook handed back to it (`... hook additional context: ...`).
  - The line it wrote next.
  - The commands it ran next.
  - What it told you about the finding in its final report.
- **Doctor before, after and final, both tiers.** After an injection, the report also shows who recovered and how.
