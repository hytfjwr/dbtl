# Releasing

Releases run entirely from the GitHub UI, in two clicks:

1. **Actions → "Open release PR" → Run workflow.** Pick the bump level
   (patch/minor/major) or type an explicit version. This creates a
   `release/vX.Y.Z` branch with the `Cargo.toml`/`Cargo.lock` bump and opens
   a PR.
2. **Merge the PR** once CI is green. The merge triggers the Release
   workflow, which creates the `vX.Y.Z` tag, builds the four target
   binaries, publishes the GitHub release with checksums, and updates
   `hytfjwr/homebrew-tap`.

## One-time setup: `RELEASE_GITHUB_TOKEN`

Both workflows need a repository secret `RELEASE_GITHUB_TOKEN` — a
fine-grained PAT with:

- **`hytfjwr/dbtl`**: Contents (read/write) + Pull requests (read/write).
  Required because PRs created with the default `GITHUB_TOKEN` never trigger
  CI, so the required status checks would never pass.
- **`hytfjwr/homebrew-tap`**: Contents (read/write), for the formula update.

Without it, "Open release PR" fails immediately and the Homebrew job is
skipped with a notice (update `Formula/dbtl.rb` by hand from
`checksums.txt`).

## Escape hatches

- **Actions → "Release" → Run workflow** releases whatever version `main`'s
  `Cargo.toml` currently has. This is also the retry path: if a previous run
  created the tag but died before publishing, re-running resumes instead of
  refusing.
- **Pushing a `v*` tag** (including publishing a release from the GitHub UI,
  which pushes the tag) triggers the same pipeline. It fails fast if the tag
  does not match `Cargo.toml`'s version, and attaches assets to an
  already-existing release instead of failing.
