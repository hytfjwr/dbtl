# Recorded demo of the dbtl TUI, driven against the bundled fixture manifest.
#
# demo/demo.gif is gitignored — CI builds it: the Demo workflow renders this
# tape and uploads the GIF as the `demo-gif` run artifact on pull requests,
# and a follow-up workflow attaches it to each release as `demo.gif`.
#
# Manual render, from the repository root after `cargo build --release`:
#
#   vhs demo/demo.tape
#
# Requires VHS (https://github.com/charmbracelet/vhs).

Output demo/demo.gif

Set Shell bash
Set FontSize 16
Set Width 1360
Set Height 800
Set Theme "Catppuccin Mocha"
Set TypingSpeed 60ms
Set PlaybackSpeed 1.0
Set Framerate 24

# Every recording starts from the same screen: an empty state dir means no
# saved tab strip, so dbtl lands on the home tab; an empty config dir keeps
# the default theme and preferences.
Env XDG_STATE_HOME "/tmp/dbtl-demo-xdg/state"
Env XDG_CONFIG_HOME "/tmp/dbtl-demo-xdg/config"

# Launch off-screen so the first frame is the TUI, not a shell prompt. The
# rm clears any state a previous local render left behind.
Hide
Type "rm -rf /tmp/dbtl-demo-xdg"
Enter
Sleep 300ms
Type "target/release/dbtl --manifest tests/fixtures/manifest.json --unicode"
Enter
Sleep 2600ms
Show

# Home: project health — coverage, lint findings, the attention list.
Sleep 2200ms

# j steps the home cursor over the attention items.
Type "j"
Sleep 450ms
Type "j"
Sleep 900ms

# Enter opens the item as a model tab (list + lineage).
Enter
Sleep 2200ms

# Tab moves focus to the lineage pane; h/l step the node cursor across
# columns, j/k step it within a column.
Tab
Sleep 700ms
Type "h"
Sleep 900ms
Type "h"
Sleep 900ms
Type "j"
Sleep 900ms
Type "l"
Sleep 800ms

# Enter re-roots the lineage to the cursor's node (crumb trail on the left).
Enter
Sleep 1800ms

# t cycles the colour lenses: coverage, degree heat, layer, layer
# violations, lint.
Type "t"
Sleep 700ms
Type "t"
Sleep 700ms
Type "t"
Sleep 700ms
Type "t"
Sleep 700ms
Type "t"
Sleep 1400ms

# Enter on the root node opens its structure view; Esc closes it.
Enter
Sleep 1800ms
Escape
Sleep 600ms

# w toggles the whole-graph overview (minimap on the right).
Type "w"
Sleep 2000ms
Type "w"
Sleep 800ms

# S opens the stats dashboard; Esc closes it.
Type "S"
Sleep 2000ms
Escape
Sleep 500ms

# q quits.
Type "q"
Sleep 400ms
