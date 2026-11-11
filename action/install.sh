#!/usr/bin/env bash
# Install dbtl from the release artifacts of the repository this action came
# from: pick the artifact matching the runner, download it together with
# checksums.txt, verify its sha256, unpack it, and put it on PATH.
# `version: local` skips all of this and uses the dbtl already on PATH.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
. "$SCRIPT_DIR/lib.sh"

version="$(dbtl_resolve_version "${DBTL_VERSION:-}" "${DBTL_ACTION_REF:-}")"

if [ "$version" = "local" ]; then
	if ! command -v dbtl >/dev/null 2>&1; then
		dbtl_die "version 'local' uses the dbtl already on PATH, but no dbtl was found"
	fi
	echo "Using dbtl from PATH: $(command -v dbtl) ($(dbtl --version 2>/dev/null || echo 'version unreadable'))"
	exit 0
fi

read -r target ext <<< "$(dbtl_select_artifact "${RUNNER_OS:?RUNNER_OS is not set}" "${RUNNER_ARCH:?RUNNER_ARCH is not set}")"
if [ -z "$target" ]; then
	exit 1 # dbtl_select_artifact already reported the unsupported runner
fi

repo="${DBTL_ACTION_REPOSITORY:?the action repository is unknown}"
artifact="dbtl-${version}-${target}.${ext}"
base_url="https://github.com/${repo}/releases/download/${version}"
work="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/dbtl-install.XXXXXX")"

sums="$work/checksums.txt"
if ! curl -fsSL --retry 3 -o "$sums" "${base_url}/checksums.txt"; then
	dbtl_die "could not download checksums.txt from release ${version} — the release may not exist or ships no checksums (${base_url})"
fi

# Require the artifact's checksum line up front: "release X has no artifact
# for Y" is a clearer failure than a bare digest mismatch.
dbtl_require_sha256 "$sums" "$artifact" "$version" "$target" >/dev/null

file="$work/$artifact"
if ! curl -fsSL --retry 3 -o "$file" "${base_url}/${artifact}"; then
	dbtl_die "could not download ${artifact} from release ${version}"
fi

dbtl_verify_sha256 "$sums" "$artifact" "$file"
echo "Verified sha256 of ${artifact}"

dest="$work/dist"
mkdir -p "$dest"
if [ "$ext" = "zip" ]; then
	if command -v unzip >/dev/null 2>&1; then
		unzip -q "$file" -d "$dest"
	elif command -v 7z >/dev/null 2>&1; then
		7z x -o"$dest" "$file" >/dev/null
	elif command -v tar >/dev/null 2>&1; then
		tar -xf "$file" -C "$dest"
	else
		dbtl_die "none of unzip, 7z, or tar is available to extract ${artifact}"
	fi
else
	tar -xzf "$file" -C "$dest"
fi

bin_dir="$dest/dbtl-${version}-${target}"
bin="$bin_dir/dbtl"
if [ -f "$bin_dir/dbtl.exe" ]; then
	bin="$bin_dir/dbtl.exe"
fi
if [ ! -f "$bin" ]; then
	dbtl_die "extracted ${artifact} but found no dbtl binary inside ${bin_dir}"
fi
chmod +x "$bin" 2>/dev/null || true

reported="$("$bin" --version | awk '{print $NF}')"
if [ "$reported" != "${version#v}" ]; then
	dbtl_die "installed dbtl reports version '${reported}', expected '${version#v}'"
fi

if [ -n "${GITHUB_PATH:-}" ]; then
	if command -v cygpath >/dev/null 2>&1; then
		cygpath -w "$bin_dir" >>"$GITHUB_PATH"
	else
		echo "$bin_dir" >>"$GITHUB_PATH"
	fi
fi
echo "dbtl ${version} ready at ${bin}"
