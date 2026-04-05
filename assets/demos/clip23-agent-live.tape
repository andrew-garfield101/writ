# Clips 2+3: The Prompt + Agent Discovers Writ (LIVE)
#
# This is ONE recording session that gets split into two GIFs:
#   - Clip 2: prompt typed + enter (first ~10s)
#   - Clip 3: agent output trimmed to 4 beats (spec add, write, seal, done)
#
# HOW THIS WORKS:
#   1. VHS launches claude --dangerously-skip-permissions
#   2. Waits for CLI to load
#   3. Types the expense tracker prompt
#   4. Waits for agent to finish (up to 5 min)
#   5. Lux trims the recording into Clips 2 and 3
#
# PREREQUISITES:
#   - Fresh writ-initialized project at ~/git/expense-tracker-demo
#   - claude CLI installed
#
# Run with: cd /Users/agarfield/git/writ && vhs assets/demos/clip23-agent-live.tape

Output assets/demos/clip23-agent-live.gif
Set Theme "Catppuccin Mocha"
Set Width 1000
Set Height 600
Set FontSize 15
Set Padding 20
Set TypingSpeed 50ms
Set Shell "zsh"

Type "cd ~/git/expense-tracker-demo"
Enter
Sleep 500ms

Type "claude --dangerously-skip-permissions"
Enter

# Wait for Claude Code CLI to fully load
Sleep 8s

# Type the prompt — this is the Clip 2 money shot
Type "Build me a simple expense tracker. I want to log expenses with a category, amount, and date, then get a summary grouped by category."
Sleep 1s
Enter

# Agent works — wait up to 5 minutes for it to finish
# VHS captures all output including writ spec add, seal, spec done
Sleep 300s
