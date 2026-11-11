# Recorded demo of the dbtl TUI against the bundled fixture manifest.
# Run from the repository root after `cargo build --release`:
#
#   vhs demo/demo.tape
#
# Requires VHS (https://github.com/charmbracelet/vhs). The GIF lands at
# demo/demo.gif — add it to .gitignore if you don't commit it.

Output demo/demo.gif

Set Shell bash
Set FontSize 15
Set Width 1500
Set Height 850
Set Theme "Catppuccin Mocha"
Set TypingSpeed 60ms
Set PlaybackSpeed 1.0

Type "target/release/dbtl --manifest tests/fixtures/manifest.json"
Sleep 400ms
Enter
Sleep 2500ms

# Browse the model list.
Down
Sleep 400ms
Down
Sleep 400ms
Down
Sleep 1200ms

# Cycle the tint lenses (coverage → blast radius → lint …).
Type "t"
Sleep 1200ms
Type "t"
Sleep 1200ms

# The cursor node's structure view.
Enter
Sleep 1800ms
Escape
Sleep 600ms

# Whole-graph overview, then back to the lineage.
Type "w"
Sleep 1800ms
Type "w"
Sleep 800ms

# The notification center.
Ctrl+n
Sleep 1500ms
Escape
Sleep 600ms

# The stats dashboard.
Type "S"
Sleep 1800ms
Escape
Sleep 600ms

# A fresh tab on the cursor's model.
Type "O"
Sleep 1500ms

# Quit.
Type "q"
Sleep 800ms
