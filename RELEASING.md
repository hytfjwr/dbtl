# Releasing `dbtl`

Releases are almost fully automated. The flow is:

1. Run the **Open release PR** workflow (`.github/workflows/bump.yml`) —
   Actions → "Open release PR" → pick `patch`/`minor`/`major`, or type an
   explicit version. It bumps `Cargo.toml` + `Cargo.lock` and cuts the
   `CHANGELOG.md` release section on a `release/vX.Y.Z` branch, then opens
   a PR authored by github-actions[bot] and requests your review.
2. Approve the release PR's workflow runs. Runs on a bot-authored PR start
   in the approval-gated state, so open the PR's Checks tab and approve the
   waiting workflows. The required checks then run and report as usual.
3. Merge that PR. The merge triggers **Release**
   (`.github/workflows/release.yml`), which tags `vX.Y.Z` at the merge
   commit, builds every target, publishes the GitHub release, and updates
   the Homebrew tap.

## What the release pipeline produces

`release.yml` resolves the tag from `Cargo.toml` (so the shipped binary's
`--version` can never disagree with the tag), then calls the reusable build
workflow (`.github/workflows/release-build.yml`) once per target. The per-OS
archives it produces are the ones listed on the
[installation page](site/src/content/docs/getting-started/installation.md);
the Homebrew tap's formula downloads those same GNU-Linux archives. The musl
builds are fully static — the job fails if the ELF requests a dynamic
loader.

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

## Secrets and one-time setup

The pipeline needs no personal access token. `bump.yml` and `release.yml`
run on the job's `GITHUB_TOKEN`. The only secret is
`HOMEBREW_TAP_DEPLOY_KEY`: the private half of an SSH deploy key that lets
the Homebrew job clone `hytfjwr/homebrew-tap` and push `Formula/dbtl.rb`.
The formula covers the four macOS/GNU-Linux targets (binary + completions
+ man page). Without the secret the job logs a notice and skips — the
release itself still publishes.

One-time setup for this flow:

1. Turn on **Allow GitHub Actions to create and approve pull requests**
   (Settings → Actions → General). With it off, `gh pr create` fails in the
   bump workflow. With it on, workflows holding `pull-requests: write` can
   also approve pull requests — this repository requires no approvals, so
   nothing changes in practice.
2. Let github-actions[bot] create `release/v*` branches. If the `release/v*`
   branch rules restrict creation to admins, the bot's push is refused —
   relax the creation rule or give the bot a bypass.
3. Register the deploy key. Generate an unencrypted SSH key pair, add the
   public half on `hytfjwr/homebrew-tap` as a deploy key with write
   access, and store the private half as this repository's
   `HOMEBREW_TAP_DEPLOY_KEY` secret.

A `RELEASE_GITHUB_TOKEN` secret, if still present from an earlier setup,
is no longer read by any workflow and can be deleted once a release has
gone through on this flow.

## When something goes wrong

- **The release PR's checks never start.** Approve the waiting workflow
  runs from the PR's Checks tab.
- **The approved runs do not count as required checks.** An admin can
  merge the PR with bypass.
- **The merge happened but Release did not start.** Run the **Release**
  workflow manually on `main`. It reads the version from `Cargo.toml` and
  resumes the same pipeline.
- **The bump workflow failed while creating the PR.** The branch was
  already pushed, and the `release/v*` rules forbid deleting or
  force-pushing it. Ask an admin to delete `release/vX.Y.Z`, then re-run
  the workflow.
- **Only the Homebrew tap update failed.** Nothing else depends on that
  job — fix the cause and re-run just the failed job.
- **The github.com host-key check failed.** GitHub rotated its ed25519
  host key. Verify the new fingerprint against GitHub's published SSH key
  fingerprints, then update the `GITHUB_HOST_KEY` pin in `release.yml`.

## Pre-release checklist

- [ ] `CHANGELOG.md` has entries under `## [Unreleased]` — the bump
      workflow cuts the release section itself (`cargo xtask
      changelog-release`, which moves `Unreleased` into a dated version
      section and repoints the footer compare links) and fails before
      pushing anything when `Unreleased` is empty. Set the `allow_empty`
      input only for a release with no user-facing changes — it cuts a
      `- No notable changes.` section.
- [ ] CI is green on `main`, including `Docs reference` and `Dogfood`.
- [ ] Run the bump workflow, approve the release PR's waiting workflow
      runs, review the PR (version, lockfile, and the cut changelog
      section), merge it.
- [ ] Confirm the `vX.Y.Z` release published all archives, `checksums.txt`,
      the SBOM, `demo.gif`, and attestations; spot-check
      `gh attestation verify dbtl-vX.Y.Z-*.tar.gz --repo hytfjwr/dbtl`.
- [ ] Confirm the Homebrew formula updated (or the notice explaining why
      not).
