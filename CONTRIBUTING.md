# Contributing

Thanks for helping improve `dbtl`. This document covers the development
workflow: how to build, test, and open a pull request that passes CI.

## Getting set up

You need a stable Rust toolchain (1.88+, the `rust-version` in
`Cargo.toml`). Clone and build:

```console
$ git clone https://github.com/hytfjwr/dbtl.git
$ cd dbtl
$ cargo build
```

Try the app against the bundled fixtures:

```console
$ cargo run --release -- --manifest tests/fixtures/manifest.json
$ cargo run --release -- --source tests/fixtures/sample_project
```

## Workspace layout

A Cargo workspace: the `dbtl` binary and root library plus member crates
`dbtl-core` (the dbt domain — manifest/source loaders, `Dag`, diff,
selectors, column lineage, lineage layout), `dbtl-tui` (the interactive
app), `dbtl-docs` (the `dbtl docs` generator), `dbtl-report` (the
`check`/`lint` report model, renderers, and lint-gate evaluation — no
filesystem, terminal, or clap dependencies), `dbtl-wasm` (the in-memory
engine the docs site compiles to WebAssembly — a pure `engine` API plus
thin `wasm-bindgen` JSON bindings), and `xtask` (dev
automation). The root `src/lib.rs` is a thin facade re-exporting the
member crates.

## Everyday commands

```console
$ cargo test --workspace                  # the full suite — bare `cargo test` skips member crates
$ cargo nextest run --workspace --profile ci   # the CI runner path
$ cargo test --workspace <name>           # a single test by substring
$ cargo test --test <file>                # one integration file (cli_contract, lint_cli, docs_cli, …)
$ cargo clippy --workspace --all-targets -- -D warnings
$ cargo fmt --all
$ cargo xtask --help                      # maintenance tasks — see below
```

Useful `xtask` tasks:

- `cargo xtask synth --models 1000 --out /tmp/synth` — write a deterministic
  synthetic dbt project (source + `target/manifest.json`) for manual runs.
- `cargo xtask regen-sample-manifest` — regenerate
  `tests/fixtures/sample_manifest.json` with real `dbt` (needs `uv`).
- `cargo xtask gen-fixture-manifest --project tests/fixtures/logistics_project`
  — regenerate `tests/fixtures/logistics_manifest.json` the same way. This
  is the general form: `--project` picks any fixture project dir,
  `--out`/`--profile` override the defaults, and `--dbt-version X.Y.Z` pins
  a release (otherwise `uvx` resolves the latest). Projects with a
  `packages.yml` may only declare `local:` packages. Both commands are
  deterministic: rerunning the same project and dbt version is
  byte-identical — see `tests/fixtures/README.md`.
- `cargo xtask gen-docs` — regenerate `docs/reference/` and `schema/` from
  the CLI/config definitions. These files are generated; edit the
  generators, not the output. `gen-docs --check` runs in CI and fails on
  drift.
- `cargo xtask changelog-release --version X.Y.Z --date YYYY-MM-DD` — move
  the `Unreleased` entries into a dated `X.Y.Z` section and repoint the
  footer compare links (`--file` rewrites a copy; `--allow-empty` cuts a
  `- No notable changes.` section). The release workflow runs this on the
  release PR, so there is no need to run it by hand.

## Pull requests

- Branch from `main`, use an English
  [Conventional Commit](https://www.conventionalcommits.org/) message
  (`feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `ci:`, `chore:` …), and
  keep a change focused.
- **Before pushing**, run `cargo fmt --all` and
  `cargo clippy --workspace --all-targets -- -D warnings` and fix
  everything they report — CI enforces both as errors.
- Add or update tests for behavior changes; run `cargo test --workspace`
  (or `cargo nextest run --workspace --profile ci`) locally.
- If you changed flags, config keys, keybindings, or lint rules, run
  `cargo xtask gen-docs` and commit the regenerated reference docs.
- User-facing changes belong in `CHANGELOG.md` under `## Unreleased`.
- Docs changes: keep prose lines wrapped reasonably, and make sure every
  relative link resolves — `tests/doc_links.rs` checks links and anchors.

CI runs on every PR, gated by three required aggregate checks — `CI
result`, `Security result`, and `Dogfood result`. Each aggregate fails when
a job it needs fails or is cancelled; a skipped job passes.

A `changes` job first classifies the pull request's diff into scopes, and
each job runs only when its scope changed:

- **code** — anything outside the docs/site buckets (Rust sources,
  `Cargo.*`, `tests/`, `crates/`, `xtask/`, `benches/`, `action/`, `demo/`,
  `.github/` except a `docs-site.yml`-only change, config files). Runs
  everything: `Lint`, `Build & Test` on Linux/Windows/macOS, `MSRV`,
  `Hygiene`, `Docs reference`, `Coverage (report)`, `TUI E2E`, the Action
  self-test, and Dogfood E2E.
- **docs** — repository prose: top-level `*.md`, one level of `.github/*.md`,
  `docs/**`, `schema/**` (site content docs also set `site`). Runs
  `Markdown tests` (the suites that read repository Markdown), the spell
  check, and `Docs reference` when `docs/` or `schema/` changed.
- **site** — `site/**` and `docs-site.yml`. Runs the spell check and the
  security jobs; the site build lives in the docs-site workflow.

The security jobs always run. When classification cannot produce a diff it
reports every scope changed, so an ambiguous pull request runs everything.

If you add a job that must gate merges, add it to the aggregate's `needs` —
`tests/ci_scripts.rs` fails when a job sits outside every aggregate's
`needs` without being declared report-only.

## Testing notes

- Integration tests live in `tests/` (unit tests in each member crate's
  `src/`). Shared helpers are in `tests/common/`, fixtures in
  `tests/fixtures/`.
- The PTY end-to-end suite (`tests/tui_pty/`) is Unix-only and every test is
  `#[ignore]`d — it never runs under a plain `cargo test`; CI invokes it
  explicitly. Run it locally with
  `cargo test --test tui_pty --locked -- --ignored`.
- `perf_*` scaling-budget tests are also `#[ignore]`d; the nightly job is
  their only runner:
  `cargo test --release --workspace --locked -- --ignored perf_`.
- Determinism is a contract: `dbtl docs` output, reports, and generated
  files must be byte-identical across runs. Tests that produce output files
  write to temporary directories.

## Updating the demo

The recorded demo is scripted in `demo/demo.tape` (a VHS tape). Changing the
tape on a pull request renders `demo.gif` as a run artifact, so you can review
the new recording before merging. Each release attaches the rendered GIF to
the release as `demo.gif`, which is what the README displays.

## Docs site

The documentation site lives in `site/`.

- Write content as Markdown in `site/src/content/docs/`.
- `site/src/nav.json` sets the sidebar order.
- The reference pages are read from `docs/reference/`, which
  `cargo xtask gen-docs` generates.
- To preview locally, run `cd site && pnpm install && pnpm run dev`. To check
  search as well, run `pnpm run build && pnpm run preview`.
- Sources are formatted and linted: run `pnpm run format` to fix formatting,
  `pnpm run format:check` and `pnpm run lint` to check, and `pnpm run check`
  for the type check. All of them run in CI.
- Browser tests (Chromium) cover the docs drawer, horizontal overflow and the
  search dialog. To run them locally, install the browser once with
  `pnpm exec playwright install chromium`, then `pnpm run build` and
  `pnpm run test:e2e`. They run in CI on pull requests only.

- The site is published only when a release is cut. A pull request builds it
  to check it, and nothing is deployed.
- In examples, write the release version as `@DBTL_TAG@` (for example `v1.2.3`) or
  `@DBTL_VERSION@` (`1.2.3`). The build replaces them with the version in
  `Cargo.toml`, and an unknown `@DBTL_...@` token fails the build.

### The WebAssembly engine

`pnpm run dev`, `pnpm run build`, and `pnpm run check` all start by building
the engine (`site/scripts/build-engine.mjs`): `dbtl-wasm` compiled to
`wasm32-unknown-unknown`, post-processed by `wasm-bindgen`, and written to
`site/public/engine/` (gitignored, like the staged sample data in
`site/public/samples/`). You need:

```console
$ rustup target add wasm32-unknown-unknown
$ cargo install wasm-bindgen-cli --version <version> --locked
```

where `<version>` is exactly the `wasm-bindgen` version pinned in
`crates/wasm/Cargo.toml` — the CLI post-processes the `.wasm`, so a
mismatched version fails the build. To build or check the site without the
Rust toolchain, set `DBTL_SKIP_ENGINE=1`: every script then skips the
engine step, checks still pass, and anything that loads the engine gets a
clear "not built" error — enough for site work that never touches the
playground. `pnpm run check:engine` loads the built wasm from Node and
compares its lint output against the native engine for both bundled
samples; it runs inside `pnpm run build`.

`wasm-bindgen` is excluded from automated Cargo updates: the crate pin in
`crates/wasm/Cargo.toml`, the CLI version CI installs in
`.github/workflows/docs-site.yml`, and that workflow's archive SHA-256 must
move together — a bump is one manual change to all three.

## Reporting bugs

Open an issue with the bug-report template — it asks for the `dbtl`
version, platform, invocation, and what you expected versus what happened.
The [exit codes page](https://hytfjwr.github.io/dbtl/configuration/exit-codes/) helps tell
a bad invocation (exit 2/3) from a real gate failure (exit 1).

## License

By contributing you agree your contributions are licensed under the
project's [MIT license](LICENSE).
