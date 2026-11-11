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
selectors, column lineage), `dbtl-tui` (the interactive app), `dbtl-docs`
(the `dbtl docs` generator), and `xtask` (dev automation). The root
`src/lib.rs` is a thin facade re-exporting the member crates.

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
- `cargo xtask gen-docs` — regenerate `docs/reference/` and `schema/` from
  the CLI/config definitions. These files are generated; edit the
  generators, not the output. `gen-docs --check` runs in CI and fails on
  drift.

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

CI runs on every PR: fmt/clippy (`Lint`), the test suite on Linux, Windows,
and macOS (`Build & Test`), an MSRV check (`MSRV`), spell check,
`cargo machete`, and `cargo doc` (`Hygiene`), generated-docs freshness
(`Docs reference`), the Action self-test, the dogfood end-to-end job, and a
real-PTY TUI suite on Unix (`TUI E2E`).

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

## Reporting bugs

Open an issue with the bug-report template — it asks for the `dbtl`
version, platform, invocation, and what you expected versus what happened.
The exit code table in the README helps tell a bad invocation (exit 2/3)
from a real gate failure (exit 1).

## License

By contributing you agree your contributions are licensed under the
project's [MIT license](LICENSE).
