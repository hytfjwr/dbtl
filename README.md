<p align="center">
  <img src=".github/assets/banner.png" alt="dbtl — a terminal UI for dbt model lineage. No compile, no browser." width="100%">
</p>

<p align="center">
  <a href="https://github.com/hytfjwr/dbtl/actions/workflows/ci.yml"><img src="https://github.com/hytfjwr/dbtl/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/hytfjwr/dbtl/actions/workflows/security.yml"><img src="https://github.com/hytfjwr/dbtl/actions/workflows/security.yml/badge.svg" alt="Security"></a>
  <a href="https://github.com/hytfjwr/dbtl/releases"><img src="https://img.shields.io/github/v/release/hytfjwr/dbtl" alt="GitHub release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/rust-1.92+-orange.svg" alt="Rust 1.92+">
</p>

<p align="center">
  <a href="#install">Install</a> ·
  <a href="#quick-start">Quick start</a> ·
  <a href="#features">Features</a> ·
  <a href="#usage">Usage</a> ·
  <a href="#keybindings">Keybindings</a>
</p>

---

`dbtl` draws your dbt project's dependency graph in a two-pane terminal UI. Pick a model on the left and its lineage appears on the right, so you can answer *"what does this model depend on, and what does it affect?"* without running `dbt compile` or opening a browser.

```
╭Seed────────────────────╮   ╭View───────────────────╮   ╭View────────────────────╮
│ source_datetime_policy │──▶│ stg_payment__shoppers │──▶│ int_shoppers__combined │
╰────────────────tests:4─╯  │╰───────────────tests:5─╯   ╰────────────────────────╯
                            │
╭Source────╮                │
│ shoppers │────────────────╯
╰──────────╯
```

<img width="3835" height="2072" alt="dbtl running in a terminal: the model list on the left, the selected model's lineage on the right" src="https://github.com/user-attachments/assets/b5cded19-6019-4e1a-a5e7-18764f48bb81" />

## Install

```console
$ brew install hytfjwr/tap/dbtl
```

Homebrew works on macOS and Linux. Other options:

- **Prebuilt binaries.** Download a tarball for your platform from the [releases page](https://github.com/hytfjwr/dbtl/releases). `checksums.txt` is included.
- **From source** (Rust 1.92+):

  ```console
  $ git clone https://github.com/hytfjwr/dbtl.git
  $ cd dbtl
  $ cargo install --path .
  ```

## Quick start

Run it at the root of any dbt project:

```console
$ dbtl
```

If `target/manifest.json` exists, `dbtl` uses it. Otherwise it parses the project source directly.

Source mode infers lineage from the raw SQL without rendering Jinja, so a compiled manifest is always more accurate. If `dbt` is on your `PATH`, press `P` inside the app to run `dbt parse` and switch to the generated manifest in place.

## Features

<table>
<tr>
<td width="50%" valign="top">

### No compile, no browser
`dbtl` builds lineage from `dbt_project.yml`, model `.sql` files (`ref()` / `source()`) and `schema.yml`. It is a single static binary and runs entirely in the terminal, including over SSH. There is no `dbt docs serve` step.

</td>
<td width="50%" valign="top">

### Navigate the graph itself
Move a cursor between lineage nodes and re-root on any model. You can filter by direction and depth, and go back and forward through your re-root history. A mouse click on a node re-roots too.

</td>
</tr>
<tr>
<td valign="top">

### Trace a path
The path from the root to the cursor lights up, including the connectors between nodes, and everything off the path dims. Compact mode (`v`) shrinks boxes to one-row nodes for big graphs. Overview mode (`w`) shows the whole DAG.

</td>
<td valign="top">

### Coverage and impact lenses
Lenses tint the lineage by test coverage, downstream blast radius, layer, or layer violation. There is also a stats dashboard, a key to jump between untested models, and one-key export to Mermaid, Graphviz, or a Markdown impact report.

</td>
</tr>
<tr>
<td valign="top">

### Review a branch with `--diff`
Compare against another manifest or checkout. Added models show green and modified ones amber. `D` opens a PR impact report that lists changed nodes with reasons, downstream blast radius, affected marts and exposures, a ready-to-run `dbt build --select …`, and risk flags. Press `e` to export it as Markdown.

</td>
<td valign="top">

### Exposures show who is affected
dbt exposures such as dashboards and notebooks appear as end nodes at the right edge of the lineage, in both manifest and source mode. The impact report lists each affected exposure with its type and owner.

</td>
</tr>
</table>

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
$ dbtl docs --out ./dbt-docs --clean            # also delete stale generated pages
$ dbtl docs --out ./dbt-docs --check            # write nothing; exit 1 if docs are stale
```

### How `--diff` compares

The diff identifies nodes by `unique_id`, so a rename shows up as one node removed and another added. That is also how every downstream `ref()` sees it. It compares the things a dbt change actually moves: materialization, direct dependencies, columns, tests, and model SQL.

There are two caveats. Comparing raw SQL cannot see a change inside a shared macro. A compiled-manifest baseline also contains the models of installed packages, which a source-parsed current side does not. For the cleanest result, compare a manifest against a manifest.

### Markdown docs (`dbtl docs`)

`dbtl docs --out <DIR>` never starts the TUI, so it runs in CI without a TTY. It writes a Markdown documentation tree that you can commit to a repo and read on GitHub or GitLab:

- **One page per node** (model, source, seed, snapshot, exposure). Each page has the description, materialization, schema, tags, a column table in dbt definition order, tests, direct and transitive upstream/downstream links, and a Mermaid lineage diagram.
- **An index `README.md`** with a project summary, test coverage %, a list of orphan models, a linked table of every node, and a whole-project Mermaid diagram. Large projects get the diagram split by layer.

The data source is chosen the same way as in the TUI (`--manifest` / `--source` / `--project` / auto-detect). The output is deterministic: regenerating from the same input produces byte-identical files.

Every generated file starts with a `<!-- Generated by dbtl. Do not edit. -->` marker. Two housekeeping flags rely on it:

- **`--clean`** writes the docs, then deletes marked files under `<DIR>` that this run did not regenerate, such as the pages of removed or renamed models. Only marked files are ever removed, so a hand-written `.md` in the same directory is safe.
- **`--check`** writes nothing. It compares the generated set against `<DIR>`, prints one line per difference (a missing, changed, or stale marked file), and exits 1 if there are any. Unlike running `git diff --exit-code` after regenerating, it also catches stale pages, and it needs no checkout step in CI: `dbtl docs --out docs/ --check`.

## Keybindings

Press `?` inside the app for the full list. The most useful ones:

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

The mouse works too: click a lineage node to re-root, and use the wheel to scroll. The lineage pane pans in both directions. A horizontal wheel notch (trackpad swipe or tilt wheel) or `Shift`+wheel scrolls it sideways.

## Color themes

Built-in presets: `default` (xterm-256, works everywhere), `ayu-dark`, `ayu-mirage`, `ayu-light`, and `gruvbox-dark` (truecolor). Choose one with `--theme <name>` or the `DBTL_THEME` environment variable, switch live with `Ctrl-t`, and list them all with `--list-themes`.

To define your own theme, put a YAML file under `~/.config/dbtl/themes/` (or `$XDG_CONFIG_HOME/dbtl/themes/`). The file name becomes the theme name:

```yaml
# ~/.config/dbtl/themes/my-ayu.yml
base: ayu-mirage        # optional preset to start from (default: "default")
colors:                 # override any role; the rest comes from the base
  accent: "#ffcc66"     # "#rrggbb" truecolor…
  class_table: 114      # …or an xterm-256 index
```

Then run `dbtl --theme my-ayu`. An unknown role or an invalid color stops startup with an error that lists the valid values. If a palette makes a lens invisible (for example, a lens tint equal to a node color), `dbtl` still starts but prints a warning.

## Troubleshooting

<details>
<summary><b>Borders look doubled or misaligned</b></summary>

<br>

Unicode box-drawing characters have East Asian Ambiguous width, and some terminal configurations draw them 2 cells wide. `dbtl` probes the terminal at startup and falls back to pure ASCII (`+ - | >`) automatically. You can also force a mode:

```console
$ dbtl --ascii     # pure ASCII rendering
$ dbtl --unicode   # skip the probe, keep Unicode box drawing
```

</details>

## License

[MIT](LICENSE)
