# Claude Code hook stdin fixtures

Real hook stdin captured from Claude Code, scrubbed: scratch repo -> `/repo`,
home -> `~`, session and prompt ids zeroed. Edit/Write/NotebookEdit, Stop,
SessionStart/End, Subagent* and UserPromptSubmit: sprint 6 experiment
(`TEAM_WRIT_DOCS/evidence/0.4.3-hook-experiment/`, Claude Code 2.1.290).

## Bash and PostToolUseFailure (sprint 7, finding 186)

Captured 2026-10-18, Claude Code 2.1.290 (hook env `CLAUDE_CODE_EXECPATH`
`.../versions/2.1.290`), `claude -p --model haiku --permission-mode
bypassPermissions`, settings registering PreToolUse, PostToolUse and
PostToolUseFailure on matcher `Bash`. One session, three Bash calls:
`cat missing_file`, `printf 'x\n' > wrote.txt && false`, `echo ok`.
Raw (scrubbed) log: `TEAM_WRIT_DOCS/evidence/0.4.4-hook-failure/hooklog.jsonl`.

| file | call |
|---|---|
| `pre_tool_use_bash.json` | PreToolUse of `cat missing_file` |
| `post_tool_use_failure_bash.json` | PostToolUseFailure of `cat missing_file` (exit 1) |
| `post_tool_use_failure_bash_after_write.json` | PostToolUseFailure of `printf ... > wrote.txt && false`; `wrote.txt` was written |
| `post_tool_use_bash.json` | PostToolUse of `echo ok` (exit 0) |

PostToolUseFailure carries: `session_id`, `transcript_path`, `cwd`,
`prompt_id`, `permission_mode`, `effort`, `hook_event_name`
("PostToolUseFailure"), `tool_name`, `tool_input` (`command`,
`description`), `tool_use_id` (same as the call's PreToolUse),
`error` (string, "Exit code 1" followed by stderr on a new line when there is
any), `is_interrupt` (bool), `duration_ms`.

It does NOT carry `tool_response`, and there is no numeric exit code field:
the exit code is only inside the `error` text. PostToolUse did NOT fire for
either failed call (6 firings: Pre+Failure, Pre+Failure, Pre+Post). Writes
made before the failure are on disk (`wrote.txt` existed after the run).
