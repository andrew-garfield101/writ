<!-- BEGIN WRIT CONFIGURATION — managed by writ init -->
## Version Control — Writ (MANDATORY)

This project uses writ for version control. You MUST follow this workflow.

### Required Workflow
1. BEFORE starting any work, run `writ context` to check project state
2. If no spec is assigned to you, create one: `writ spec add "brief description of your task" --scope "<files you will change, comma-separated>"`
3. AFTER each meaningful unit of work: `writ seal -s "<summary>" --paths <changed files, comma-separated>` (auto-scoped to your spec)
4. When the task is complete, BEFORE reporting results: `writ spec done -s "<what you did>"`

Your task is NOT complete until you have sealed your work.
`--scope` declares the files your task owns; without it, pass `--paths` with the files you changed.
If a seal prints `left out:` or `NOT SEALED:`, run the command it prints if those files are yours.

### Commands
- `writ context` — structured project state. Run this FIRST.
- `writ seal -s "<summary>" --paths <changed files, comma-separated>` — checkpoint work (auto-scoped to your spec)
- `writ spec add "brief description of your task" --scope "<files you will change, comma-separated>"` — create a spec (ID auto-generated)
- `writ spec done -s "<what you did>"` — mark task complete (auto-scoped)
- `writ status` — project overview
- `writ log` — seal history

Subagents inherit the parent's identity: set `WRIT_AGENT_ID=<your-name>` on every writ command (or pass `--agent <your-name>`).
Do NOT run `git commit` or `writ finish` — the user manages the git round-trip.
`writ restore <seal-id>` overwrites working directory files — use only when reverting to a known-good state.
<!-- END WRIT CONFIGURATION -->
