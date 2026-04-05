# Clip 5: Finish — Convergence, one clean commit.
#
# Simulated output based on real test run data.
# The writ finish output and git log are displayed via hidden printf commands.
#
# Run with: vhs assets/demos/clip5-finish.tape

Output assets/demos/clip5-finish.gif
Set Theme "Catppuccin Mocha"
Set Width 1000
Set Height 550
Set FontSize 15
Set Padding 20
Set TypingSpeed 40ms
Set Shell "zsh"

# Show the command
Type "writ finish"
Enter

Hide

Sleep 1500ms
Type "printf '\n  ✓ Seal-tree convergence: 6 files merged across 4 specs.\n'"
Enter
Sleep 2s

Type "printf '\nCompleted specs ready to commit:\n'"
Enter
Sleep 500ms

Type "printf '  ✓ 08340b40 — Created expense data models and categories\n'"
Enter
Sleep 400ms

Type "printf '  ✓ 0f8285cd — JSON file storage with save and load\n'"
Enter
Sleep 400ms

Type "printf '  ✓ 4fcfc7a0 — CLI with add, list, and summary commands\n'"
Enter
Sleep 400ms

Type "printf '  ✓ 2db4f207 — Pytest test suite for models and storage\n'"
Enter
Sleep 1500ms

Type "printf '\nCommit message:\n  writ: 4 features complete — expense tracker (4 specs)\n'"
Enter
Sleep 1500ms

Type "printf '\nProceed? [Y/n] '"
Enter
Sleep 1500ms

Show

# Human confirms
Type "y"
Enter

Hide
Sleep 800ms

Type "printf '\n  ✓ Committed 23d863e3\n  ✓ 4 spec(s) marked as committed.\n\n  → Run git push when ready.\n'"
Enter
Sleep 2500ms

Show

# Show clean git history
Sleep 500ms
Type "git log --oneline"
Enter

Hide
Sleep 300ms
Type "printf '23d863e writ: 4 features complete — expense tracker (4 specs)\nf1a2b3c init\n'"
Enter

Show
Sleep 3s
