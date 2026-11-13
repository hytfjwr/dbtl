<p align="center">
  <img src=".github/assets/banner.png" alt="dbtl — a terminal UI for dbt model lineage. No compile, no browser." width="100%">
</p>

<p align="center">
  <a href="https://github.com/hytfjwr/dbtl/actions/workflows/ci.yml"><img src="https://github.com/hytfjwr/dbtl/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/hytfjwr/dbtl/actions/workflows/security.yml"><img src="https://github.com/hytfjwr/dbtl/actions/workflows/security.yml/badge.svg" alt="Security"></a>
  <a href="https://github.com/hytfjwr/dbtl/releases"><img src="https://img.shields.io/github/v/release/hytfjwr/dbtl" alt="GitHub release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/rust-1.88+-orange.svg" alt="Rust 1.88+">
</p>

<p align="center">
  <a href="https://hytfjwr.github.io/dbtl/">Documentation</a> ·
  <a href="#install">Install</a> ·
  <a href="#quick-start">Quick start</a> ·
  <a href="https://hytfjwr.github.io/dbtl/playground/">Playground</a> ·
  <a href="CHANGELOG.md">Changelog</a>
</p>

---

`dbtl` shows a dbt project's model lineage in the terminal. Pick a model in the list and the graph of what it reads from and what depends on it appears beside it. It reads the project files directly or an existing `target/manifest.json`, so there is no compile step and no browser.

```
╭Seed────────────────────╮   ╭View───────────────────╮   ╭View────────────────────╮
│ source_datetime_policy │──▶│ stg_payment__shoppers │──▶│ int_shoppers__combined │
╰────────────────tests:4─╯  │╰───────────────tests:5─╯   ╰────────────────────────╯
                            │
╭Source────╮                │
│ shoppers │────────────────╯
╰──────────╯
```

Each box is a node. The tag in its top border is the resource type or materialization (`Seed`, `Source`, `View`, `Table`, and so on), and `tests:N` in the bottom border counts its tests.

<img src="https://github.com/hytfjwr/dbtl/releases/latest/download/demo.gif" alt="dbtl demo: browsing a model's lineage in a terminal, cycling colour lenses, and opening the stats dashboard" width="100%" />

## Install

```console
$ brew install hytfjwr/tap/dbtl
```

Prebuilt binaries for macOS, Linux, and Windows are on the [releases page](https://github.com/hytfjwr/dbtl/releases). To build from source (Rust 1.88 or later), run `cargo install --path .` in a clone of the repository. See [Installation](https://hytfjwr.github.io/dbtl/getting-started/installation/) for details.

## Quick start

Run it at the root of a dbt project:

```console
$ cd path/to/dbt_project
$ dbtl
```

If `target/manifest.json` exists, `dbtl` uses it. Otherwise it parses the project source directly. Continue with [Quick start](https://hytfjwr.github.io/dbtl/getting-started/quick-start/).

## What it does

In CI, without a terminal:

- [Gates and impact reports](https://hytfjwr.github.io/dbtl/cli/check-and-impact/): `dbtl check` fails a pull request on rules you set, such as coverage minimums or breaking column changes, and `dbtl impact` prints the blast radius — the models downstream of a change.
- [Lint](https://hytfjwr.github.io/dbtl/cli/lint/): `dbtl lint` runs a static best-practice rule set over the project graph, with no warehouse connection and no `dbt run`.
- [Docs generator](https://hytfjwr.github.io/dbtl/cli/docs/): `dbtl docs` writes a deterministic Markdown tree with a page per node, so a docs-drift gate can compare regenerated output.
- [Selectors](https://hytfjwr.github.io/dbtl/cli/selectors/): `dbtl ls` prints the nodes a dbt selector spec matches, and `dbtl columns` prints column-level lineage.
- CI recipes: a [GitHub Action](https://hytfjwr.github.io/dbtl/ci/github-action/) and a [pre-commit hook](https://hytfjwr.github.io/dbtl/ci/pre-commit/).

In the terminal:

- [Lineage without a compile step](https://hytfjwr.github.io/dbtl/getting-started/overview/): the graph comes from `dbt_project.yml`, model SQL, and `schema.yml`, or from `target/manifest.json`.
- [Graph navigation](https://hytfjwr.github.io/dbtl/tui/lineage/): move a cursor between nodes, re-root (recenter) the graph on any model, filter by direction and depth, and light up the path between two nodes.
- [Lenses](https://hytfjwr.github.io/dbtl/tui/lenses/): tint the lineage by one property at a time — test coverage, downstream impact, layer, lint findings, or `--diff` changes.
- [Search](https://hytfjwr.github.io/dbtl/tui/search/): filter the list by name, full text, or a dbt selector.
- [Diff review](https://hytfjwr.github.io/dbtl/tui/diff/): compare against another manifest, checkout, or `git:<rev>` and open the same impact report CI prints.
- [Run results and catalog](https://hytfjwr.github.io/dbtl/tui/run-results-and-catalog/): last-run status, runtime, and warehouse column types and row counts.
- [Exposures](https://hytfjwr.github.io/dbtl/tui/exposures/): dashboards and other exposures as end nodes in the lineage.
- [Interface](https://hytfjwr.github.io/dbtl/tui/interface/): tabs, a home page, an inspector pane, and mouse support.
- [Themes](https://hytfjwr.github.io/dbtl/tui/themes/): built-in palettes and user-defined color themes.

## Keybindings

Press `?` in the app for every key in the current mode, or read the [keybindings reference](https://hytfjwr.github.io/dbtl/reference/keybindings/).

<details>
<summary>The keys you use most</summary>

| Key | Action |
|---|---|
| `j` `k` `h` `l` | Move in the list or the lineage (arrow keys work too) |
| `g` `G` | Jump to the first or last model |
| `PgUp` `PgDn` | Page the list (`Ctrl-u` `Ctrl-d` also work) |
| `Tab` | Cycle focus: list, lineage, inspector |
| `Enter` | Open the structure view, or re-root on a lineage model |
| `/` | Search the list. A `!` prefix takes a dbt selector, a `?` prefix searches full text |
| `t` | Cycle the lineage lens |
| `p` | Anchor or clear a two-node path |
| `u` `d` `[` `]` `0` | Filter the lineage by direction and depth, or reset the view |
| `b` `f` | Go back or forward in the re-root history |
| `z` | Send the lineage cursor home and re-center |
| `v` `w` `M` | Toggle compact nodes, the whole-graph overview, the minimap |
| `O` `X` | Open the cursor's node in a new tab, close the active tab |
| `{` `}` `~` | Previous tab, next tab, the home tab |
| `Space` `'` `"` | Bookmark the model, cycle bookmarks forward or backward |
| `T` `*` `F` | Filter the list to untested, bookmarked, or lint-finding models |
| `s` `S` | SQL preview, project stats |
| `D` | PR impact report against the `--diff` baseline |
| `K` | Column lineage for the traced column |
| `a` | Node actions menu |
| `y` | Yank menu: `yy` unique_id, `ym` Mermaid, `yc` ASCII art, `y!` dbt selector |
| `o` | Open the model's file in `$VISUAL` (falling back to `$EDITOR`) |
| `P` | Run `dbt parse` and switch to the manifest |
| `r` | Reload the data source |
| `Ctrl-p` | Command palette |
| `Ctrl-n` | Notification center |
| `Ctrl-t` | Cycle the color theme |
| `?` | Help for the current mode |
| `q` | Quit (`Ctrl-c` from anywhere) |

</details>

## Documentation

The [documentation site](https://hytfjwr.github.io/dbtl/) covers the TUI page by page, the [CLI](https://hytfjwr.github.io/dbtl/cli/overview/), [CI setups](https://hytfjwr.github.io/dbtl/ci/github-action/), [configuration](https://hytfjwr.github.io/dbtl/configuration/project-config/), and generated [references](https://hytfjwr.github.io/dbtl/reference/cli/). The [playground](https://hytfjwr.github.io/dbtl/playground/) runs it in a browser on bundled sample projects.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[MIT](LICENSE)
