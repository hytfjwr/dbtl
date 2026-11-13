#!/usr/bin/env bash
# Install the pinned shellcheck into a directory: download the pinned
# linux x86_64 release tarball, verify it against the pinned SHA-256, and
# extract the binary. The pin lives in exactly one place — the workflow's
# SHELLCHECK_VERSION / SHELLCHECK_SHA256 environment — so the lint job and
# the actionlint job can never drift onto different versions. A digest
# mismatch aborts the install: there is no unverified path.
#
#   install-shellcheck.sh <dest-dir>
#
# Places the binary at <dest-dir>/shellcheck.
set -euo pipefail

: "${SHELLCHECK_VERSION:?SHELLCHECK_VERSION must name the pinned tag (vX.Y.Z)}"
: "${SHELLCHECK_SHA256:?SHELLCHECK_SHA256 must pin the tarball digest}"

dest="${1:?install directory required}"
work="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/shellcheck-install.XXXXXX")"
trap 'rm -rf "$work"' EXIT

tarball="$work/shellcheck-${SHELLCHECK_VERSION}.linux.x86_64.tar.xz"
curl -fsSL --retry 3 -o "$tarball" \
	"https://github.com/koalaman/shellcheck/releases/download/${SHELLCHECK_VERSION}/shellcheck-${SHELLCHECK_VERSION}.linux.x86_64.tar.xz"

# sha256sum on the ubuntu runners, shasum elsewhere (macOS) — either way a
# mismatch is fatal.
if command -v sha256sum >/dev/null 2>&1; then
	printf '%s  %s\n' "$SHELLCHECK_SHA256" "$tarball" | sha256sum -c -
else
	printf '%s  %s\n' "$SHELLCHECK_SHA256" "$tarball" | shasum -a 256 -c -
fi

mkdir -p "$dest"
tar -xJf "$tarball" -C "$dest" --strip-components=1 "shellcheck-${SHELLCHECK_VERSION}/shellcheck"

reported="$("$dest/shellcheck" --version | awk '/^version:/ {print $2}')"
if [ "v${reported}" != "$SHELLCHECK_VERSION" ]; then
	printf '::error::installed shellcheck reports version %s, expected %s\n' "${reported:-<none>}" "$SHELLCHECK_VERSION" >&2
	exit 1
fi
echo "shellcheck ${SHELLCHECK_VERSION} ready at ${dest}/shellcheck"
