# Releasing `dbtl`

Releases are almost fully automated. The two-click flow is:

1. Run the **Open release PR** workflow (`.github/workflows/bump.yml`) —
   Actions → "Open release PR" → pick `patch`/`minor`/`major`, or type an
   explicit version. It bumps `Cargo.toml` + `Cargo.lock` and cuts the
   `CHANGELOG.md` release section on a `release/vX.Y.Z` branch and opens a
   PR.
2. Merge that PR. The merge triggers **Release**
   (`.github/workflows/release.yml`), which tags `vX.Y.Z` at the merge
   commit, builds every target, publishes the GitHub release, and updates
   the Homebrew tap.

## What the release pipeline produces

`release.yml` resolves the tag from `Cargo.toml` (so the shipped binary's
`--version` can never disagree with the tag), then calls the reusable build
workflow (`.github/workflows/release-build.yml`) for every target:

| Target | Artifact |
| ------ | -------- |
| `aarch64-apple-darwin`, `x86_64-apple-darwin` | `dbtl-<tag>-<target>.tar.gz` |
| `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` | `dbtl-<tag>-<target>.tar.gz` |
| `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` | `dbtl-<tag>-<target>.tar.gz` (fully static — the job fails if the ELF requests a dynamic loader) |
| `x86_64-pc-windows-msvc` | `dbtl-<tag>-<target>.zip` |

Every archive contains the binary, `LICENSE`, `README.md`, the generated
shell completions (`completions/`), and the man page (`man/dbtl.1`). The
package step adds `checksums.txt` (SHA-256 over every archive and the SBOM)
and a CycloneDX SBOM (`dbtl-<tag>.cdx.json`, normal deps only — the
binary's shipped dependency closure). A dedicated job attests build
provenance and the SBOM (`gh attestation verify` works against every
asset), then the GitHub release is created with generated notes and all
assets attached.

Once the release exists, the pipeline calls the **Demo** workflow
(`.github/workflows/demo.yml`) to render `demo/demo.tape` into `demo.gif`
at the tag and attach it to the release. The GIF is a convenience asset:
it is not covered by `checksums.txt` or the attestations, and a failure to
render or attach it never blocks the release or the tap update. To attach
or refresh it by hand — for a release that went out without one, or after
a failed attach — run the **Demo** workflow manually with `release-tag`
set to the release tag; it renders the GIF at the ref you run it on and
uploads it over any existing `demo.gif`.

The pipeline also starts the **Docs site** workflow
(`.github/workflows/docs-site.yml`) on `main`, which builds the site from
the release tag and publishes it to GitHub Pages. This is the only way the
site is published; pull requests just build it. A failure here never blocks
the release or the tap update. To publish again by hand, run the **Docs
site** workflow on `main` with `ref` set to a release tag (empty means the
latest release).

## The tag-push and resume paths

- Pushing a `v*` tag directly also runs the pipeline — an escape hatch that
  fails fast when the tag doesn't match `Cargo.toml`'s version.
- Re-running the workflow (`workflow_dispatch`) is safe mid-release: an
  existing tag without a published release resumes; a tag whose release is
  already published refuses with instructions to bump.

## Dry runs

**Release dry run** (`.github/workflows/release-dry-run.yml`) calls the same
reusable build under a dummy tag on PRs touching the release workflows or
`Cargo.toml`/`Cargo.lock`, and renders the demo GIF through the same
workflow call a real release makes — it builds, packages, and renders but
never tags, publishes, attests, attaches, or touches the tap.

## Secrets

The pipeline uses one fine-grained PAT, `RELEASE_GITHUB_TOKEN`, with
contents + pull-requests read/write on this repository and contents
read/write on `hytfjwr/homebrew-tap`:

- `bump.yml` needs it because PRs created by `GITHUB_TOKEN` never trigger
  CI — the release PR must run required checks to be mergeable.
- The Homebrew step pushes `Formula/dbtl.rb` to `hytfjwr/homebrew-tap`. The
  formula covers the four macOS/GNU-Linux targets (binary + completions +
  man page). Without the token the step logs a notice and skips — the
  release itself still publishes.

## Pre-release checklist

- [ ] `CHANGELOG.md` has entries under `## [Unreleased]` — the bump
      workflow cuts the release section itself (`cargo xtask
      changelog-release`, which moves `Unreleased` into a dated version
      section and repoints the footer compare links) and fails before
      pushing anything when `Unreleased` is empty. Set the `allow_empty`
      input only for a release with no user-facing changes — it cuts a
      `- No notable changes.` section.
- [ ] CI is green on `main`, including `Docs reference` and `Dogfood`.
- [ ] Run the bump workflow, review the release PR (version, lockfile, and
      the cut changelog section), merge it.
- [ ] Confirm the `vX.Y.Z` release published all archives, `checksums.txt`,
      the SBOM, `demo.gif`, and attestations; spot-check
      `gh attestation verify dbtl-vX.Y.Z-*.tar.gz --repo hytfjwr/dbtl`.
- [ ] Confirm the Homebrew formula updated (or the notice explaining why
      not).
