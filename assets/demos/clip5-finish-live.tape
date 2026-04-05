# Clip 5: Finish (LIVE) — Real writ finish on a project with completed agent work.
#
# PREREQUISITES:
#   - Agents have completed work in ~/git/expense-tracker
#   - Specs are done, seals exist
#
# Run with: cd /Users/agarfield/git/writ && vhs assets/demos/clip5-finish-live.tape

Output assets/demos/clip5-finish-live.gif
Set Theme "Catppuccin Mocha"
Set Width 1000
Set Height 550
Set FontSize 15
Set Padding 20
Set TypingSpeed 40ms
Set Shell "zsh"

Type "cd ~/git/expense-tracker"
Enter
Sleep 500ms

Type "writ finish"
Enter

# Wait for convergence output + prompt to fully render
# Being generous — we can trim dead time later
Sleep 15s

# Type y at the Proceed? prompt
Type "y"
Enter

# Wait for commit confirmation
Sleep 8s

# Show clean git history
Type "git log --oneline"
Enter
Sleep 5s
