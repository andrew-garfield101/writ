# Clip 1: Init — Human sets up a project. One command. Done.
#
# Run with: cd /Users/agarfield/git/writ && vhs assets/demos/clip1-init.tape

Output assets/demos/clip1-init.gif
Set Theme "Catppuccin Mocha"
Set Width 1000
Set Height 550
Set FontSize 15
Set Padding 20
Set TypingSpeed 30ms
Set Shell "zsh"

Require writ
Require git

# Quick project setup (fast, not the point)
Type "cd /tmp && mkdir expense-tracker && cd expense-tracker"
Enter
Sleep 500ms

Type "git init -q && echo '# Expense Tracker' > README.md"
Enter
Sleep 500ms

Type "git add . && git commit -qm 'init'"
Enter
Sleep 800ms

# THE command. Normal speed. This is what matters.
Sleep 500ms
Type "writ init --yes"
Sleep 400ms
Enter

# Let the full output render
Sleep 6s

# Hold on the final output
Sleep 3s
