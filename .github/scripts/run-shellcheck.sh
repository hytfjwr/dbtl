#!/usr/bin/env bash
# Lint the repository's shell with shellcheck.
#
#   run-shellcheck.sh                       lint every target (CI entry)
#   run-shellcheck.sh --print-targets [dir] print the tracked targets, one
#                                           per line (repo at dir; default
#                                           cwd — a test seam)
#   run-shellcheck.sh --extract-runs <yml>  print "LINE<TAB>command" for
#                                           each single-line run: value in
#                                           the composite action file —
#                                           fails on a multi-line form
#
# Targets are enumerated, never listed by hand: every git-tracked *.sh plus
# any tracked file whose shebang names a shell, so a new script is linted
# without touching this file. Every tracked composite action file (the
# root action.yml plus any .github/actions/<name>/action.y{a,}ml) has its
# run: values linted too — a one-liner is extracted and checked as its own
# snippet, while a multi-line block must move into a tracked script: a YAML
# block scalar cannot be reconstructed faithfully in shell, and linting a
# fragment would hide the problem the check exists to find.
set -euo pipefail

say_err() { printf '::error::%s\n' "$*" >&2; }
usage() {
	say_err "usage: run-shellcheck.sh [--print-targets [dir] | --extract-runs <file>]"
	exit 2
}

# shebang_is_shell <file>: true when the first line is a #! line whose
# interpreter — or env target — is a shell shellcheck understands.
shebang_is_shell() {
	local first rest
	IFS= read -r first <"$1" 2>/dev/null || return 1
	case "$first" in
	'#!'*) ;;
	*) return 1 ;;
	esac
	rest="${first#'#!'}"
	local -a toks
	read -ra toks <<<"$rest"
	[ "${#toks[@]}" -gt 0 ] || return 1
	local base="${toks[0]##*/}"
	if [ "$base" = "env" ]; then
		base=''
		local t
		for t in "${toks[@]:1}"; do
			case "$t" in
			-* | *=*) ;; # env options and VAR=val prefixes
			*)
				base="${t##*/}"
				break
				;;
			esac
		done
	fi
	case "$base" in
	sh | bash | dash | ash | ksh | ksh93 | mksh | pdksh | zsh | osh | yash) return 0 ;;
	esac
	return 1
}

# print_targets <dir>: every tracked shell file under the git repo at dir —
# the *.sh pathspec plus a shebang scan of everything else.
print_targets() {
	local dir="${1:?dir}"
	local f
	git -C "$dir" ls-files -z -- '*.sh'
	git -C "$dir" ls-files -z | while IFS= read -r -d '' f; do
		case "$f" in
		*.sh) continue ;;
		esac
		if [ -f "$dir/$f" ] && shebang_is_shell "$dir/$f"; then
			printf '%s\0' "$f"
		fi
	done
}

# grep_matches <pat> <file>: print grep -nE matches; exit 1 means "no
# matches" (prints nothing) while a real read error aborts — silently
# skipping run: linting on an unreadable file is a fail-open.
grep_matches() {
	local out rc
	set +e
	out="$(grep -nE "$1" "$2" 2>/dev/null)"
	rc=$?
	set -e
	if [ "$rc" -eq 0 ]; then
		printf '%s\n' "$out"
	elif [ "$rc" -gt 1 ]; then
		say_err "cannot read $2 (grep exit $rc)"
		exit 2
	fi
}

# print_run_snippets <file>: "LINE<TAB>command" per single-line run: —
# a multi-line form (block scalar, empty value, or a value ending in a
# continuation operator) is an error, not a lint target. The match is a
# line-level heuristic: it does not parse YAML, so a `run :` spelling or
# a `run:`-shaped line inside another key's block scalar is not handled.
print_run_snippets() {
	local file="${1:?file}"
	[ -f "$file" ] || {
		say_err "no such file: $file"
		exit 2
	}
	local bad
	bad="$(grep_matches '^[[:space:]]*(-[[:space:]]+)?run:[[:space:]]*([|>]|$)' "$file")"
	if [ -z "$bad" ]; then
		bad="$(grep_matches '^[[:space:]]*(-[[:space:]]+)?run:.*(&&|\|\||\||\\)[[:space:]]*$' "$file")"
	fi
	if [ -n "$bad" ]; then
		say_err "$file holds a multi-line run: block — move it into a tracked script (a *.sh under action/ is linted automatically):"
		printf '  %s\n' "$bad" >&2
		exit 2
	fi
	local lines line n text
	lines="$(grep_matches '^[[:space:]]*(-[[:space:]]+)?run:[[:space:]]+[^[:space:]]' "$file")"
	[ -n "$lines" ] || return 0
	while IFS= read -r line; do
		n="${line%%:*}"
		text="${line#*:}"
		text="${text#*run:}"
		text="${text#"${text%%[![:space:]]*}"}"
		printf '%s\t%s\n' "$n" "$text"
	done <<<"$lines"
}

mode='all'
arg=''
case "${1:-}" in
'')
	mode='all'
	;;
--print-targets)
	mode='print-targets'
	arg="${2:-}"
	[ $# -le 2 ] || usage
	;;
--extract-runs)
	mode='extract-runs'
	arg="${2:-}"
	[ $# -eq 2 ] || usage
	;;
*)
	usage
	;;
esac

case "$mode" in
extract-runs)
	print_run_snippets "$arg"
	exit 0
	;;
print-targets)
	root="$(git -C "${arg:-.}" rev-parse --show-toplevel)"
	print_targets "$root" | tr '\0' '\n'
	exit 0
	;;
esac

root="$(git rev-parse --show-toplevel)"
cd "$root"

if ! command -v shellcheck >/dev/null 2>&1; then
	say_err "shellcheck is not on PATH"
	exit 2
fi

work="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/run-shellcheck.XXXXXX")"
trap 'rm -rf "$work"' EXIT

# Enumeration results land in files first: a failure inside a process
# substitution (an unlintable multi-line run:, an unreadable file) exits
# only that subshell — the direct call keeps the exit in this shell.
print_targets "$root" >"$work/targets"
# Every tracked composite action file — the root one plus any under
# .github/actions/<name>/ — so a new action is linted without a list edit.
git -C "$root" ls-files -z -- 'action.yml' 'action.yaml' '**/action.yml' '**/action.yaml' >"$work/actions"

files=()
while IFS= read -r -d '' f; do
	files+=("$f")
done <"$work/targets"

# Extracted run: snippets are linted as bash text — a .shellcheckrc next to
# the temp files supplies the dialect (shellcheck reads it from each file's
# own directory, not the cwd). The file name carries the action file and
# its line number so a finding points back at its source.
snips=()
printf 'shell=bash\n' >"$work/.shellcheckrc"
while IFS= read -r -d '' afile; do
	[ -f "$root/$afile" ] || continue
	print_run_snippets "$root/$afile" >"$work/runs"
	slug="$(printf '%s' "$afile" | tr '/.' '__')"
	while IFS=$'\t' read -r n text; do
		[ -n "$n" ] || continue
		snip="$work/${slug}-run-${n}.sh"
		printf '%s\n' "$text" >"$snip"
		snips+=("$snip")
	done <"$work/runs"
done <"$work/actions"

total=$((${#files[@]} + ${#snips[@]}))
echo "shellcheck: linting $total target(s)"
if [ "${#files[@]}" -gt 0 ]; then
	shellcheck "${files[@]}"
fi
if [ "${#snips[@]}" -gt 0 ]; then
	shellcheck "${snips[@]}"
fi
echo "shellcheck: $total target(s) clean"
