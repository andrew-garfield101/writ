<!-- BEGIN WRIT CONFIGURATION — managed by writ init -->
## Writ
This project uses writ. In Claude Code, writ seals the edits it can attribute and asks you with one line about the rest; in other tools, seal with `writ seal -s "<summary>" --paths <files>`. Leave git commits and `writ finish` to the user.
`writ context` shows project state and other agents' work. When a task is done, `writ spec done -s "<what you did>"` records your own summary (optional).
If writ cannot tell whose change a file is, it asks you with one line; run that command.
When `writ context` or `writ doctor` shows a finding whose fix is not marked for a human, run that fix, including for findings about other agents' specs, because writ has already checked it is safe.
<!-- END WRIT CONFIGURATION -->
