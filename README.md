<h1 align="center">dbtl</h1>

<p align="center">
A terminal UI for dbt model lineage — no compile, no browser.
</p>

<p align="center">
<a href="https://github.com/hytfjwr/dbtl/actions/workflows/ci.yml"><img src="https://github.com/hytfjwr/dbtl/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
<a href="https://github.com/hytfjwr/dbtl/actions/workflows/security.yml"><img src="https://github.com/hytfjwr/dbtl/actions/workflows/security.yml/badge.svg" alt="Security"></a>
<a href="https://github.com/hytfjwr/dbtl/releases"><img src="https://img.shields.io/github/v/release/hytfjwr/dbtl" alt="GitHub release"></a>
<a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
<img src="https://img.shields.io/badge/rust-1.92+-orange.svg" alt="Rust 1.92+">
</p>

`dbtl` renders your dbt project's dependency graph as ASCII art in a two-pane terminal UI. Pick a model on the left, see its lineage on the right, and answer "what does this model depend on, and what does it affect?" without leaving the terminal.

```
╭Seed────────────────────╮   ╭View───────────────────╮   ╭View────────────────────╮
│ source_datetime_policy │──▶│ stg_payment__shoppers │──▶│ int_shoppers__combined │
╰────────────────tests:4─╯  │╰───────────────tests:5─╯   ╰────────────────────────╯
                            │
╭Source────╮                │
│ shoppers │────────────────╯
╰──────────╯
```

#### Preview
<img width="3835" height="2072" alt="Screenshot 2026-06-12 at 11 23 00" src="https://github.com/user-attachments/assets/b5cded19-6019-4e1a-a5e7-18764f48bb81" />


Key features:

- **No `dbt compile` required.** Builds lineage straight from `dbt_project.yml` + model `.sql` (`ref()`/`source()`) + `schema.yml`. Uses `target/manifest.json` automatically when present.
- **Single static binary, works over SSH.** Everything happens inside the terminal — no `dbt docs serve`, no browser.
- **Navigate the graph, not just the list.** Move a cursor between lineage nodes, re-root on any model, filter by direction and depth, and walk your re-root history.
- **Built for test-coverage and impact chores.** Coverage/blast-radius/layer lenses (the layer lens also labels each column's layer along the pane border), a stats dashboard, untested-model cycling, and one-key export to Mermaid / Graphviz / Markdown impact reports.
- **Trace a path, see a path.** Move the lineage cursor and the root↔cursor path lights up — nodes AND the connectors between them — while everything off-path dims. A compact mode (`v`) collapses boxes to one-row nodes for big graphs.
- **Diff two states of the project, then ship the review.** `--diff <baseline>` compares against another manifest or checkout: added models glow green and modified ones amber right in the lineage, a status chip counts `+added ~modified -removed`, and `D` opens a reviewer-shaped **PR impact report** — the changed nodes (with reasons), the aggregate downstream blast radius and affected marts, the affected exposures, a ready-to-run `dbt build --select …` command, and risk flags (untested changes, changed hubs, newly-introduced layer violations). Press `e` to export it as Markdown for the PR description, `y` to copy the build command.
- **Exposures answer "who cares".** dbt exposures (dashboards, notebooks, …) render as terminator nodes on the right edge of the lineage — in both manifest and source mode — the impact chip gains an `exp:N` count, and the `i` impact report lists each affected exposure with its kind and owner.

## Quick Start

Run it at the root of any dbt project:

```console
$ dbtl
```

That's it. If `target/manifest.json` exists it is used; otherwise the project source is parsed directly.

Source mode infers lineage from the raw SQL (no Jinja rendering), so a compiled
manifest is always more accurate — if `dbt` is on your `PATH`, press `P` inside
the app to run `dbt parse` and switch to the generated manifest in place.

## Install

**Homebrew (macOS / Linux):**

```console
$ brew install hytfjwr/tap/dbtl
```

**Prebuilt binaries:** grab a tarball for your platform from the
[releases page](https://github.com/hytfjwr/dbtl/releases) (`checksums.txt`
included).

**From source (requires Rust 1.92+):**

```console
$ git clone https://github.com/hytfjwr/dbtl.git
$ cd dbtl
$ cargo install --path .
```

## Usage

```console
# Auto-detect: reads ./target/manifest.json if present, otherwise parses ./ as source
$ dbtl

# Point at a project directory
$ dbtl --project /path/to/dbt-project

# Force a mode
$ dbtl --manifest /path/to/target/manifest.json
$ dbtl --source /path/to/dbt-project

# Select a model at startup and auto-reload on source changes
$ dbtl --select stg_orders --watch

# Pick a color theme (or set DBTL_THEME)
$ dbtl --theme ayu-mirage
$ dbtl --list-themes

# Diff against a baseline: "what does my branch change?"
$ dbtl --diff /path/to/main-checkout            # another worktree (or its manifest.json)
$ dbtl --diff target/manifest.prod.json         # a saved production manifest

# Generate static Markdown docs (no TUI)
$ dbtl docs --out ./dbt-docs                    # auto-detect the data source, write into ./dbt-docs
$ dbtl docs --manifest target/manifest.json --out docs/lineage
$ dbtl docs --source /path/to/project --out ./dbt-docs --quiet   # no compile needed; CI-quiet
```


### Markdown docs (`dbtl docs`)

`dbtl docs --out <DIR>` is a non-interactive subcommand (it never starts the TUI,
so it runs fine in CI with no TTY). It writes a Markdown
documentation tree you can commit straight to a repo and read on GitHub/GitLab:

- **One page per node** (model / source / seed / snapshot / exposure): description,
  materialization, schema, tags, a column definition table (in dbt definition order),
  tests, direct + transitive upstream/downstream (as links), and a per-node Mermaid
  lineage diagram.
- **An index `README.md`**: project summary, test-coverage % and orphan-model list,
  a table of every node (linked), and a whole-project Mermaid diagram (split by layer
  for large projects).

The data source is resolved exactly like the TUI (`--manifest` / `--source` / `--project`
/ auto-detect). Output is **deterministic** — regenerating from the same input is
byte-for-byte identical, so a CI job can `dbtl docs … && git diff --exit-code` to
assert the docs are up to date. Existing files are overwritten, never deleted.

The diff keys nodes by `unique_id` (a rename reads as removed + added — which is
what it is to every downstream `ref()`), and compares the surfaces a dbt change
actually moves: materialization, direct dependencies, columns, tests, and model
SQL. Two caveats: comparing raw SQL can't see a change inside a shared macro,
and a compiled-manifest baseline also contains installed packages' models, which
a source-parsed current side doesn't — for the cleanest signal, diff manifest
against manifest.


### Keybindings

Press `?` inside the app for the full list. Highlights:

| Key | Action |
|------|------|
| `j` `k` `h` `l` | Move in the list / move the lineage cursor |
| `Tab` | Switch focus between list ⇄ lineage |
| `Enter` | Structure view (columns / tests / description); on a lineage node, re-root to it |
| `/` | Fuzzy search (list filter / lineage jump) |
| `u` `d` `[` `]` `0` | Filter the lineage view (direction / depth / reset) |
| `b` `f` | Back / forward in re-root history |
| `t` | Cycle lineage lens (test coverage → blast-radius heat → layer → layer violation → diff) |
| `v` | Toggle compact lineage (1-row nodes — fits ~2x more graph on screen) |
| `w` | Toggle whole-graph overview (the entire DAG in the lineage pane; Compact, minimap on) |
| `s` / `S` | SQL preview (syntax-highlighted) / project stats dashboard |
| `o` | Open the model's SQL in `$EDITOR` |
| `m` `x` `c` | Copy lineage as Mermaid / Graphviz DOT / ASCII art |
| `i` / `e` | Copy a Markdown impact report / write the diagram to a file |
| `!` | Copy the view as a runnable `dbt build --select` command |
| `Space` `'` | Bookmark / cycle bookmarks |
| `T` `*` | Filter the list to untested / bookmarked models |
| `P` | Run `dbt parse` and switch to the compiled manifest |
| `D` | PR impact report vs the `--diff` baseline (then `e` exports it as Markdown, `y` copies the suggested `dbt build` command) |
| `Ctrl-p` | Command palette |
| `Ctrl-t` | Cycle the color theme |
| `q` | Quit |

Mouse is supported too: click a lineage node to re-root, wheel to scroll. The lineage pane pans on both axes — a horizontal wheel notch (trackpad swipe / tilt wheel) or `Shift`+wheel scrolls it sideways.

### Color themes

Built-in presets: `default` (xterm-256, works everywhere), `ayu-dark`, `ayu-mirage`, `ayu-light`, `gruvbox-dark` (truecolor). Pick one with `--theme <name>` (or the `DBTL_THEME` env var), cycle live with `Ctrl-t`, and list everything with `--list-themes`.

You can also define your own theme as a YAML file under `~/.config/dbtl/themes/` (or `$XDG_CONFIG_HOME/dbtl/themes/`) — the file name becomes the theme name:

```yaml
# ~/.config/dbtl/themes/my-ayu.yml
base: ayu-mirage        # optional preset to start from (default: "default")
colors:                 # override any role; the rest comes from the base
  accent: "#ffcc66"     # "#rrggbb" truecolor…
  class_table: 114      # …or an xterm-256 index
```

Run `dbtl --theme my-ayu`. Unknown roles and bad colours fail with the valid alternatives listed; a palette that breaks the lens-visibility contract (e.g. a lens tint equal to a node colour) starts anyway but prints a warning.

### If borders look doubled or misaligned

Unicode box-drawing characters are East Asian Ambiguous width; some terminal configurations draw them 2 cells wide. `dbtl` probes the terminal at startup and falls back to pure ASCII (`+ - | >`) automatically, and you can force a mode:

```console
$ dbtl --ascii     # pure ASCII rendering
$ dbtl --unicode   # skip the probe, keep Unicode box drawing
```

## License

[MIT](LICENSE)
