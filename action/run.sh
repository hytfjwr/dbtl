#!/usr/bin/env bash
# Run `dbtl check` for the action: resolve the diff baseline, fetch its
# commit when the shallow checkout lacks it, invoke dbtl, and publish the
# exit-code/report/sarif outputs. The script itself always exits 0 — dbtl's
# verdict travels through the `exit-code` output and is re-raised by the
# action's last step, after the comment and SARIF steps had their chance.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
. "$SCRIPT_DIR/lib.sh"

project_dir="${DBTL_PROJECT_DIR:-.}"
if [ ! -d "$project_dir" ]; then
	dbtl_die "project-dir '${project_dir}' does not exist"
fi

base="$(dbtl_resolve_base "${DBTL_BASE_REF:-}" "${DBTL_EVENT_NAME:-}" "${DBTL_PR_BASE_SHA:-}" "${DBTL_PUSH_BEFORE:-}")"

# A 40-hex base names a commit the shallow checkout may not carry — fetch
# exactly that object. Anything else (branch names, tags) is left for dbtl's
# own `git:<rev>` resolution and its error hints.
if [ -n "$base" ] && [[ "$base" =~ ^[0-9a-fA-F]{40}$ ]]; then
	if ! git -C "$project_dir" rev-parse --verify --quiet "${base}^{commit}" >/dev/null 2>&1; then
		echo "Base commit ${base} is not in the checkout — fetching it from origin."
		git -C "$project_dir" fetch --no-tags --depth=1 origin "$base" ||
			dbtl_die "could not fetch base commit ${base} (the checkout is too shallow — set fetch-depth: 0, or pass a reachable base-ref)"
	fi
fi

report="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/dbtl-report.md"
sarif_file="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/dbtl.sarif"
rm -f "$report" "$sarif_file"

cmd=(dbtl check --project "$project_dir" --format github
	--report "markdown:${report}" --path-prefix "$project_dir")
if [ -n "$base" ]; then
	cmd+=(--diff "git:${base}")
fi
if [ "${DBTL_LINT:-}" = "true" ]; then
	cmd+=(--lint)
fi
if [ -n "${DBTL_FAIL_ON:-}" ]; then
	cmd+=(--fail-on "${DBTL_FAIL_ON}")
fi
if [ "${DBTL_STEP_SUMMARY:-}" = "true" ]; then
	cmd+=(--step-summary)
fi
if [ "${DBTL_SARIF:-}" = "true" ]; then
	cmd+=(--report "sarif:${sarif_file}")
fi
while IFS= read -r arg; do
	cmd+=("$arg")
done < <(dbtl_split_args "${DBTL_ARGS:-}")

printf '+ %s\n' "${cmd[*]}"
set +e
"${cmd[@]}"
rc=$?
set -e

{
	printf 'exit-code=%s\n' "$rc"
	if [ -f "$report" ]; then printf 'report=%s\n' "$report"; else printf 'report=\n'; fi
	if [ -f "$sarif_file" ]; then printf 'sarif=%s\n' "$sarif_file"; else printf 'sarif=\n'; fi
} >>"${GITHUB_OUTPUT:-/dev/null}"
printf 'dbtl check exited %s\n' "$rc"
