# Releasing `dbtl`

Releases are almost fully automated. The two-click flow is:

1. Run the **Open release PR** workflow (`.github/workflows/bump.yml`) —
   Actions → "Open release PR" → pick `patch`/`minor`/`major`, or type an
   explicit version. It bumps `Cargo.toml` + `Cargo.lock` on a
   `release/vX.Y.Z` branch and opens a PR.
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

## The tag-push and resume paths

- Pushing a `v*` tag directly also runs the pipeline — an escape hatch that
  fails fast when the tag doesn't match `Cargo.toml`'s version.
- Re-running the workflow (`workflow_dispatch`) is safe mid-release: an
  existing tag without a published release resumes; a tag whose release is
  already published refuses with instructions to bump.

## Dry runs

**Release dry run** (`.github/workflows/release-dry-run.yml`) calls the same
reusable build under a dummy tag on PRs touching the release workflows or
`Cargo.toml`/`Cargo.lock` — it builds and packages every target but never
tags, publishes, attests, or touches the tap.

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

- [ ] `CHANGELOG.md` has an entry for the release — rename `Unreleased` to
      the new version with the date (keep an empty `Unreleased` on top).
- [ ] CI is green on `main`, including `Docs reference` and `Dogfood`.
- [ ] Run the bump workflow, review the release PR (version and lockfile
      only), merge it.
- [ ] Confirm the `vX.Y.Z` release published all archives, `checksums.txt`,
      the SBOM, and attestations; spot-check
      `gh attestation verify dbtl-vX.Y.Z-*.tar.gz --repo hytfjwr/dbtl`.
- [ ] Confirm the Homebrew formula updated (or the notice explaining why
      not).
