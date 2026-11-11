# Shared helpers for the composite action's shell steps. This file is
# sourced, never executed directly — each function takes plain arguments and
# prints its result on stdout (errors on stderr, non-zero exit), so the
# whole surface is exercisable without a runner or a network.
# shellcheck shell=bash

# Emit a workflow-command error and exit the CALLING script (scripts run
# under `set -e`, so `|| exit 1` sites convert this into a step failure).
dbtl_die() {
	printf '::error::%s\n' "$*" >&2
	exit 1
}

# Normalize a version-ish value to `vX.Y.Z`. Accepts `X.Y.Z` or `vX.Y.Z`;
# anything else is rejected — the result lands in a release-download URL.
dbtl_normalize_version() {
	local v="${1:-}"
	v="${v#v}"
	if [[ "$v" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
		printf 'v%s\n' "$v"
		return 0
	fi
	printf '::error::invalid version %s — expected X.Y.Z or vX.Y.Z\n' "'${1:-}'" >&2
	return 1
}

# Resolve which dbtl to install. An explicit `version` input wins; `local`
# means "use the dbtl already on PATH" (no install at all). With an empty
# input the ref the action was invoked at supplies the version — a `vX.Y.Z`
# tag names it exactly, while a SHA or branch pin carries none, so the
# caller is told to set `version` explicitly.
dbtl_resolve_version() {
	local input="${1:-}" ref="${2:-}"
	if [ -n "$input" ]; then
		if [ "$input" = "local" ]; then
			printf 'local\n'
			return 0
		fi
		dbtl_normalize_version "$input"
		return
	fi
	if [ -n "$ref" ] && [[ "$ref" =~ ^v?[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
		dbtl_normalize_version "$ref"
		return
	fi
	printf '::error::cannot determine the dbtl version to install: the action was invoked at %s, not a vX.Y.Z tag — set the version input explicitly (e.g. version: v0.6.0)\n' "${ref:-<none>}" >&2
	return 1
}

# Map RUNNER_OS/RUNNER_ARCH to a release artifact: prints "<target> <ext>"
# where <ext> is `tar.gz` or `zip` (Windows). Releases ship gnu Linux
# builds, so Linux maps to *-unknown-linux-gnu; anything outside the five
# shipped targets is an explicit error, not a guess.
dbtl_select_artifact() {
	local os="${1:-}" arch="${2:-}"
	case "${os}/${arch}" in
	Linux/X64) printf '%s %s\n' "x86_64-unknown-linux-gnu" "tar.gz" ;;
	Linux/ARM64) printf '%s %s\n' "aarch64-unknown-linux-gnu" "tar.gz" ;;
	macOS/ARM64) printf '%s %s\n' "aarch64-apple-darwin" "tar.gz" ;;
	macOS/X64) printf '%s %s\n' "x86_64-apple-darwin" "tar.gz" ;;
	Windows/X64) printf '%s %s\n' "x86_64-pc-windows-msvc" "zip" ;;
	*)
		printf '::error::unsupported runner %s/%s — releases ship Linux, macOS, and Windows x86_64/ARM64 artifacts\n' "${os}" "${arch}" >&2
		return 1
		;;
	esac
}

# sha256 of a file on stdout — `sha256sum` where it exists, `shasum -a 256`
# otherwise. No verifier means a hard error: verification is never skipped.
dbtl_sha256() {
	local file="${1:-}"
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$file" | awk '{print $1}'
	elif command -v shasum >/dev/null 2>&1; then
		shasum -a 256 "$file" | awk '{print $1}'
	else
		printf '::error::cannot verify the download: neither sha256sum nor shasum is on PATH\n' >&2
		return 1
	fi
}

# The expected sha256 of <name> from a checksums.txt file (sha256sum format:
# field 1 is the digest, field 2 the name — optionally `*`-prefixed for
# binary mode). Prints the digest, or nothing when the name is unlisted.
dbtl_expected_sha256() {
	local sums="${1:?checksums file required}" name="${2:?artifact name required}"
	awk -v name="$name" '$2 == name || $2 == "*" name { print $1; exit }' "$sums"
}

# Like dbtl_expected_sha256 but unlisting is an error named for the user:
# "release <tag> has no artifact for <target>" — the answer a runner on an
# unsupported target (or a release that predates that target) needs.
dbtl_require_sha256() {
	local sums="${1:?checksums file required}" name="${2:?artifact name required}" tag="${3:?tag required}" target="${4:?target required}"
	local expected
	expected="$(dbtl_expected_sha256 "$sums" "$name")"
	if [ -z "$expected" ]; then
		printf '::error::release %s has no artifact for %s (no %s line in checksums.txt)\n' "$tag" "$target" "'${name}'" >&2
		return 1
	fi
	printf '%s\n' "$expected"
}

# Verify <file> against the <name> line in <sums>. A missing line and a
# digest mismatch are both errors — there is no unverified install path.
dbtl_verify_sha256() {
	local sums="${1:?checksums file required}" name="${2:?artifact name required}" file="${3:?file required}"
	local expected actual
	expected="$(dbtl_expected_sha256 "$sums" "$name")"
	if [ -z "$expected" ]; then
		printf '::error::%s is not listed in %s\n' "$name" "$sums" >&2
		return 1
	fi
	actual="$(dbtl_sha256 "$file")" || return 1
	if [ "$actual" != "$expected" ]; then
		printf '::error::checksum mismatch for %s: expected %s, got %s\n' "$name" "$expected" "$actual" >&2
		return 1
	fi
	return 0
}

# Decide which baseline `dbtl check --diff git:<base>` compares against.
# Prints the rev, or nothing when the run has no baseline. An explicit
# `base-ref` input wins and may never look like a flag (`-…` is rejected);
# otherwise the event supplies it — a pull request's base sha, or a push's
# `before` commit. An all-zero `before` (a branch's first push) and
# non-PR/push events get no baseline.
dbtl_resolve_base() {
	local input="${1:-}" event="${2:-}" pr_base="${3:-}" before="${4:-}"
	if [ -n "$input" ]; then
		case "$input" in
		-*)
			printf '::error::base-ref %s is not a valid git revision\n' "'$input'" >&2
			return 1
			;;
		esac
		printf '%s\n' "$input"
		return 0
	fi
	case "$event" in
	pull_request | pull_request_target)
		printf '%s\n' "$pr_base"
		;;
	push)
		case "$before" in
		*[!0]*) printf '%s\n' "$before" ;;
		*) : ;; # empty or all zeros — no baseline
		esac
		;;
	esac
	return 0
}

# Split the free-form `args` input on whitespace, one argument per line.
# Quoting is deliberately NOT interpreted (the input's description says so);
# `read -a` splits on IFS without touching quotes or glob characters, so `*`
# can never turn into a file list — and a whitespace-only input produces no
# argument at all.
dbtl_split_args() {
	local -a words
	read -ra words <<<"${1:-}"
	if ((${#words[@]})); then
		printf '%s\n' "${words[@]}"
	fi
}
