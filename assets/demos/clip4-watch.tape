# Clip 4: Watch — Seals flowing from 4 agents in real-time.
#
# This clip simulates writ watch output using a helper script.
# The printf commands are hidden; only their output appears on screen.
# Output lines are based on real test run data.
#
# Run with: vhs assets/demos/clip4-watch.tape

Output assets/demos/clip4-watch.gif
Set Theme "Catppuccin Mocha"
Set Width 1000
Set Height 450
Set FontSize 15
Set Padding 20
Set TypingSpeed 40ms
Set Shell "zsh"

# Show the command being typed
Type "writ watch"
Enter

# Everything below is hidden input, only output appears
Hide

Sleep 1500ms
Type "printf '\n  writ watch active — monitoring for new seals (5s interval)...\n\n'"
Enter
Sleep 2500ms

# Agent 1 seals work
Type "printf '  [12:30:15 PM] seal a1b2c3d4 (claude-code-f58a, 08340b40): 2 files\n'"
Enter
Sleep 3s

# Agent 2 seals work
Type "printf '  [12:30:22 PM] seal e5f6g7h8 (claude-code-318e, 0f8285cd): 2 files\n'"
Enter
Sleep 2500ms

# Agent 3 seals work
Type "printf '  [12:30:28 PM] seal i9j0k1l2 (claude-code-4076, 4fcfc7a0): 3 files\n'"
Enter
Sleep 3s

# Agent 4 seals work
Type "printf '  [12:30:33 PM] seal m3n4o5p6 (claude-code-0bf9, 2db4f207): 2 files\n'"
Enter
Sleep 3s

# Agents finishing — spec_done auto-seals (0 files)
Type "printf '  [12:30:38 PM] seal q7r8s9t0 (claude-code-f58a, 08340b40): 0 files\n'"
Enter
Sleep 2s

Type "printf '  [12:30:43 PM] seal u1v2w3x4 (claude-code-318e, 0f8285cd): 0 files\n'"
Enter
Sleep 2s

Type "printf '  [12:30:48 PM] seal y5z6a7b8 (claude-code-4076, 4fcfc7a0): 0 files\n'"
Enter
Sleep 2s

Type "printf '  [12:30:55 PM] seal c9d0e1f2 (claude-code-0bf9, 2db4f207): 0 files\n'"
Enter

Show

# Hold final state
Sleep 3s
