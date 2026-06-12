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
```

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
| `t` | Cycle lineage lens (test coverage → blast-radius heat → layer → layer violation) |
| `v` | Toggle compact lineage (1-row nodes — fits ~2x more graph on screen) |
| `s` / `S` | SQL preview (syntax-highlighted) / project stats dashboard |
| `o` | Open the model's SQL in `$EDITOR` |
| `m` `x` `c` | Copy lineage as Mermaid / Graphviz DOT / ASCII art |
| `i` / `e` | Copy a Markdown impact report / write the diagram to a file |
| `Space` `'` | Bookmark / cycle bookmarks |
| `T` `*` | Filter the list to untested / bookmarked models |
| `P` | Run `dbt parse` and switch to the compiled manifest |
| `Ctrl-p` | Command palette |
| `q` | Quit |

Mouse is supported too: click a lineage node to re-root, wheel to scroll.

### If borders look doubled or misaligned

Unicode box-drawing characters are East Asian Ambiguous width; some terminal configurations draw them 2 cells wide. `dbtl` probes the terminal at startup and falls back to pure ASCII (`+ - | >`) automatically, and you can force a mode:

```console
$ dbtl --ascii     # pure ASCII rendering
$ dbtl --unicode   # skip the probe, keep Unicode box drawing
```

## License

[MIT](LICENSE)
