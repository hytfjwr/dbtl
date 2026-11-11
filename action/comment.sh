#!/usr/bin/env bash
# Post the markdown check report on the pull request as a sticky comment:
# the first existing comment starting with `<!-- dbtl:report -->` is
# updated, a new one is posted otherwise. Every API failure degrades to a
# `::warning::` — a read-only token (forked pull requests) or a missing
# report must never flip the check's verdict.
set -euo pipefail

marker='<!-- dbtl:report -->'
report="${DBTL_REPORT:-}"

if [ -z "$report" ] || [ ! -f "$report" ]; then
	echo "::warning::no markdown report was produced — skipping the pull request comment" >&2
	exit 0
fi

# GitHub caps comment bodies at 65536 characters; an oversized report is
# truncated and pointed at the step summary, which carries the full text.
body="$report"
if [ "$(wc -c <"$report")" -gt 65000 ]; then
	body="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/dbtl-report-truncated.md"
	head -c 65000 "$report" >"$body"
	printf '\n\n_Report truncated — see the job summary for the full dbtl check report._\n' >>"$body"
fi

repo="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is not set}"
pr="${DBTL_PR_NUMBER:?DBTL_PR_NUMBER is not set}"

if [ "${DBTL_DRY_RUN:-}" = "true" ]; then
	cat <<EOF
[dry-run] would list comments via GET /repos/${repo}/issues/${pr}/comments (paginated) and pick the first whose body starts with "${marker}"
[dry-run] if found: PATCH /repos/${repo}/issues/comments/<id> with -F body=@${body}
[dry-run] else: POST /repos/${repo}/issues/${pr}/comments with -F body=@${body}
EOF
	exit 0
fi

existing="$(gh api --paginate "repos/${repo}/issues/${pr}/comments" \
	--jq ".[] | select(.body | startswith(\"${marker}\")) | .id" 2>/dev/null | head -n1 || true)"

if [ -n "$existing" ]; then
	if gh api -X PATCH "repos/${repo}/issues/comments/${existing}" -F "body=@${body}" >/dev/null; then
		echo "Updated the dbtl report comment ${existing}"
		exit 0
	fi
	echo "::warning::could not update comment ${existing} — posting a new one" >&2
fi

if gh api -X POST "repos/${repo}/issues/${pr}/comments" -F "body=@${body}" >/dev/null; then
	echo "Posted the dbtl report comment"
else
	echo "::warning::could not post the dbtl report comment (the token is likely read-only — e.g. a forked pull request)" >&2
fi
