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
  is the general form for any fixture project. Projects with a
  `packages.yml` may only declare `local:` packages. Both commands are
  deterministic: rerunning the same project and dbt version is
  byte-identical — see `tests/fixtures/README.md`. Its options:

  | Option | Effect |
  | ------ | ------ |
  | `--project <DIR>` | The fixture project directory. |
  | `--out <FILE>` | Where to write the manifest. Default: `tests/fixtures/<name>_manifest.json` for a `<name>_project` directory. |
  | `--profile <NAME>` | The profile name for the throwaway `profiles.yml`. Default: the `profile:` key of `dbt_project.yml`. |
  | `--dbt-version <X.Y.Z>` | Pin a dbt-core release. Default: `uvx` resolves the latest. |
  | `--vars <YAML\|JSON>` | Passed to `dbt compile --vars` as given, so the manifest records the values `dbtl --vars` produces from source. |
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

When jobs fail only because no hosted runner ever picked them up, a
companion workflow re-runs the failed jobs once on its own — a real
failure, a cancellation, or a second attempt is left for a human.

A `changes` job first classifies the pull request's diff into scopes, and
each job runs only when its scope changed:

| Scope | What counts | What runs |
| ----- | ----------- | --------- |
| code | Anything outside the docs and site buckets: Rust sources, `Cargo.*`, `tests/`, `crates/`, `xtask/`, `benches/`, `action/`, `demo/`, `.github/` (except a change to `docs-site.yml` alone), and config files. | Everything: `Lint`, `Build & Test` on Linux, Windows, and macOS, `MSRV`, `Hygiene`, `Docs reference`, `Coverage (report)`, `TUI E2E`, and `Build and dogfood` (the release build, then the Action self-test and Dogfood E2E on that binary). |
| docs | Repository prose: top-level `*.md`, one level of `.github/*.md`, `docs/**`, and `schema/**`. Site content docs also set `site`. | `Markdown tests` (the suites that read repository Markdown), the spell check, and `Docs reference` when `docs/` or `schema/` changed. |
| site | `site/**` and `docs-site.yml`. | The spell check and the security jobs. The site build lives in the docs-site workflow. |

The security jobs always run. When classification cannot produce a diff it
reports every scope changed, so an ambiguous pull request runs everything.

If you add a job that must gate merges, add it to the aggregate's `needs` —
`tests/ci_scripts.rs` fails when a job sits outside every aggregate's
`needs` without being declared report-only.

`security.yml` runs the small static checks as one `Security checks` job: it
installs the tools, runs the shell-script lint, `pinact`, and `cargo deny` as
one `parallel:` group of `run:` steps, then runs the jactionlint action
(workflows and composite actions, with the profile pinned in
`.github/jactionlint.yaml`). Keep `uses:` steps out of `parallel:` groups and
`background: true` steps — `tests/ci_scripts.rs` fails on them, because the
runner races them. zizmor and the advisory scan stay separate jobs.

Shell scripts are linted by the shell-script step of that job, pinned to
shellcheck v0.11.0. The step also checks every one-line `run:` value in the
composite action file, so a multi-line `run:` block must move into a script.
`bash .github/scripts/run-shellcheck.sh` reproduces it locally (needs
`shellcheck` on PATH). To lint the workflows locally, install jactionlint
(`go install github.com/jdx/jactionlint/v2/cmd/jactionlint@v2.6.0`) and run
`jactionlint` from the repository root.


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
- Sources are formatted and linted: run `pnpm run format` to fix formatting,
  `pnpm run format:check` and `pnpm run lint` to check, and `pnpm run check`
  for the type check. All of them run in CI.
- Browser tests (Chromium) cover the docs drawer, horizontal overflow and the
  search dialog. To run them locally, install the browser once with
  `pnpm exec playwright install chromium`, then `pnpm run build` and
  `pnpm run test:e2e`. They run in CI on pull requests only.
- The site is published only when a release is cut —
  [RELEASING.md](RELEASING.md) describes that flow. A pull request builds
  the site to check it, and nothing is deployed.
- In examples, write the release version as `@DBTL_TAG@` (for example `v1.2.3`) or
  `@DBTL_VERSION@` (`1.2.3`). The build replaces them with the version in
  `Cargo.toml`, and an unknown `@DBTL_...@` token fails the build.

To preview the site locally:

1. Run `cd site && pnpm install`.
2. Run `pnpm run dev` to serve the site with live reload.
3. To check search as well, run `pnpm run build && pnpm run preview`.

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
