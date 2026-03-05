# Error Codes

*Full error reference coming soon.*

## Common Errors

### "No writ repository found"

You're not inside a writ project. Run `writ init` to set one up, or navigate to a directory that has a `.writ/` folder.

### "Seal chain verification failed"

A seal in the history has been tampered with or corrupted. Run `writ verify --chain` for details on which seal failed and why.

### "Push diverged"

The remote has seals that your local repository doesn't. Pull first with `writ pull`, then try pushing again.

### "File outside agent scope"

An agent tried to seal changes to files outside its declared scope. Either update the agent's scope constraints or use a different agent.

### "Invalid lifecycle transition"

A spec can't move from its current state to the requested state. For example, you can't complete a spec that's been cancelled. Check the spec's current state with `writ spec status`.
