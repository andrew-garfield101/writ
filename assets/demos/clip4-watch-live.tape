# Clip 4: Watch (LIVE) — Real writ watch capturing real agent seals.
#
# HOW THIS WORKS:
#   1. Run this tape: vhs assets/demos/clip4-watch-live.tape
#   2. VHS starts recording and runs `writ watch` in a real shell
#   3. In SEPARATE iTerm tabs, launch agents against the same project
#   4. As agents seal, writ watch picks them up and VHS captures it
#   5. VHS waits for the full duration then renders the GIF
#
# PREREQUISITES:
#   - A writ-initialized project at /tmp/expense-tracker (from Clip 1)
#   - Agents ready to launch in separate terminals
#
# TIMING:
#   5 minutes of recording time. Adjust if agents need more/less.
#   We can always trim the GIF with ffmpeg after.
#
# Run with: cd /Users/agarfield/git/writ && vhs assets/demos/clip4-watch-live.tape

Output assets/demos/clip4-watch-live.gif
Set Theme "Catppuccin Mocha"
Set Width 1000
Set Height 450
Set FontSize 15
Set Padding 20
Set Shell "zsh"

Type "cd ~/git/expense-tracker"
Enter
Sleep 500ms

Type "writ watch"
Enter

# Wait 5 minutes for agents to do their work
# VHS records everything that appears in the terminal
Sleep 300s
