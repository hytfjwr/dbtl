#!/usr/bin/env bash
# Decide whether a failed workflow run should be re-run once — and only
# once — because its jobs never received a hosted runner.
#
# A runner-acquisition failure leaves a cancelled job with no runner name
# and zero steps, which is the same API shape a queued job shows after a
# concurrency-group or human cancel. The only reliable discriminator is
# the annotation GitHub attaches to the check run:
#
#   The job was not acquired by Runner of type hosted even after multiple
#   attempts
#
# The re-run happens only when all of these hold:
#
#   * the run's conclusion is `failure` — a cancelled run belongs to a
#     human or the concurrency group, not to us;
#   * the run is still on its first attempt, so we never loop;
#   * the run's head commit is still the tip of its head branch — when a
#     newer push exists, re-running this run would cancel the newer run in
#     the same concurrency group;
#   * every job that did not succeed — failed, cancelled, timed out —
#     outside the workflow's aggregate check (the `CI result`-style job
#     whose outcome only echoes its `needs`) carries the annotation above.
#     Scope-skipped and needs-skipped jobs pass as `skipped`; when nothing
#     failed outside the aggregate there is no acquisition failure to
#     repair and the run is left alone.
#
# Test failures, cancellations that reached a real runner, and failures
# the annotation cannot identify all fall through to a human.
#
# Live mode (the retry workflow) reads everything through the GitHub API —
# GH_REPO names the repository, the job token authenticates:
#
#   retry-runner-failures.sh --run-id <id>
#
# Offline mode replays the same decision over recorded API payloads, one
# file per response inside a fixture directory:
#
#   <dir>/run.json                  the run object (GET /actions/runs/{id})
#   <dir>/jobs.json                 GET /actions/runs/{id}/jobs
#   <dir>/annotations-<job>.json    GET /check-runs/{job}/annotations
#   <dir>/branch-head               current tip sha of the run's head
#                                   branch; missing or empty means the
#                                   lookup could not resolve it
#
#   retry-runner-failures.sh --fixtures <dir>
#
# stdout carries the verdict as key=value lines (`rerun`, `reason`, plus
# the `failed`/`acquired` name lists) so a workflow step can forward it
# into $GITHUB_OUTPUT. `--summary <file>` appends a Markdown report of the
# decision.
set -euo pipefail

# Substring of the check-run annotation GitHub leaves when no hosted
# runner picks the job up. Matched as a fragment so cosmetic wording
# changes around it do not silently disable the retry.
ACQUIRED_NEEDLE='was not acquired by Runner'

RUN_ID=""
FIXTURES=""
SUMMARY=""
while [ $# -gt 0 ]; do
    case "$1" in
        --run-id)
            RUN_ID="${2:?--run-id needs a value}"
            shift 2
            ;;
        --fixtures)
            FIXTURES="${2:?--fixtures needs a directory}"
            shift 2
            ;;
        --summary)
            SUMMARY="${2:?--summary needs a file}"
            shift 2
            ;;
        *)
            echo "unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

if [ -n "$RUN_ID" ] && [ -n "$FIXTURES" ]; then
    echo "pass either --run-id or --fixtures, not both" >&2
    exit 2
fi
if [ -z "$RUN_ID" ] && [ -z "$FIXTURES" ]; then
    echo "pass either --run-id or --fixtures" >&2
    exit 2
fi
if [ -n "$RUN_ID" ]; then
    : "${GH_REPO:?GH_REPO must name the repository as owner/name}"
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# --- fetch (live) or locate (offline) the API payloads -------------------

if [ -n "$FIXTURES" ]; then
    RUN_JSON="$FIXTURES/run.json"
    JOBS_JSON="$FIXTURES/jobs.json"
    ANN_DIR="$FIXTURES"
    [ -f "$RUN_JSON" ] || {
        echo "::error::$RUN_JSON is missing" >&2
        exit 2
    }
    [ -f "$JOBS_JSON" ] || {
        echo "::error::$JOBS_JSON is missing" >&2
        exit 2
    }
else
    RUN_JSON="$TMP/run.json"
    JOBS_JSON="$TMP/jobs.json"
    gh api "repos/$GH_REPO/actions/runs/$RUN_ID" >"$RUN_JSON"
    gh api "repos/$GH_REPO/actions/runs/$RUN_ID/jobs?per_page=100" >"$JOBS_JSON"
fi

jq -e . "$RUN_JSON" >/dev/null
jq -e . "$JOBS_JSON" >/dev/null

CONCLUSION=$(jq -r '.conclusion // ""' "$RUN_JSON")
ATTEMPT=$(jq -r '.run_attempt // 0' "$RUN_JSON")
HEAD_SHA=$(jq -r '.head_sha // ""' "$RUN_JSON")
HEAD_BRANCH=$(jq -r '.head_branch // ""' "$RUN_JSON")
HEAD_REPO=$(jq -r '.head_repository.full_name // ""' "$RUN_JSON")
WF_NAME=$(jq -r '.name // ""' "$RUN_JSON")
EVENT=$(jq -r '.event // ""' "$RUN_JSON")
RUN_URL=$(jq -r '.html_url // ""' "$RUN_JSON")
[ -z "$RUN_ID" ] && RUN_ID=$(jq -r '.id // ""' "$RUN_JSON")
[ -z "$HEAD_REPO" ] && HEAD_REPO="${GH_REPO:-}"

FAILED_NAMES=()
ACQUIRED_NAMES=()
OTHER_NAMES=()

join() {
    local out=""
    local n
    for n in "$@"; do
        [ -n "$out" ] && out="$out, $n" || out="$n"
    done
    printf '%s' "$out"
}

write_summary() {
    {
        echo "### Runner-failure retry"
        echo
        echo "- Run: [${WF_NAME} run ${RUN_ID}](${RUN_URL}) — \`${EVENT}\` on \`${HEAD_BRANCH}\`"
        echo "- Verdict: **${VERDICT}** — ${REASON}"
        echo "- Failed or cancelled jobs outside the aggregate: ${FAILED_TEXT:-none}"
        echo "- Jobs carrying the runner-acquisition annotation: ${ACQUIRED_TEXT:-none}"
        echo
    } >>"$SUMMARY"
}

verdict() {
    VERDICT="$1"
    REASON="$2"
    RERUN="$3"
    printf 'rerun=%s\n' "$RERUN"
    printf 'reason=%s\n' "$REASON"
    printf 'failed=%s\n' "$(join "${FAILED_NAMES[@]+"${FAILED_NAMES[@]}"}")"
    printf 'acquired=%s\n' "$(join "${ACQUIRED_NAMES[@]+"${ACQUIRED_NAMES[@]}"}")"
    FAILED_TEXT=$(join "${FAILED_NAMES[@]+"${FAILED_NAMES[@]}"}")
    ACQUIRED_TEXT=$(join "${ACQUIRED_NAMES[@]+"${ACQUIRED_NAMES[@]}"}")
    [ -n "$SUMMARY" ] && write_summary
    exit 0
}

veto() {
    verdict "no re-run" "$1" false
}

# --- gate 1+2: the run itself ---------------------------------------------

[ "$CONCLUSION" = "failure" ] ||
    veto "the run's conclusion is ${CONCLUSION:-unknown}, not failure"

[ "$ATTEMPT" = "1" ] ||
    veto "the run is already on attempt $ATTEMPT — only the first attempt is retried"

# --- gate 3: the run is still the tip of its branch -----------------------
# Re-running a superseded run would cancel the newer run sharing its
# concurrency group. An unresolvable head — a deleted branch, a private
# fork, an API error — means we cannot prove freshness, so we stay out.

[ -n "$HEAD_BRANCH" ] ||
    veto "the run reports no head branch to compare against"

# Pull-request runs from a fork can spell the head branch as
# `owner:branch`; git refs never contain a colon, so stripping the owner
# prefix is safe. The repository comes from head_repository, so the tip is
# always resolved on the repository the branch actually lives in.
BRANCH_NAME="${HEAD_BRANCH##*:}"

if [ -n "$FIXTURES" ]; then
    CURRENT_HEAD=""
    [ -f "$FIXTURES/branch-head" ] &&
        CURRENT_HEAD=$(tr -d '[:space:]' <"$FIXTURES/branch-head")
else
    CURRENT_HEAD=$(gh api "repos/$HEAD_REPO/branches/$BRANCH_NAME" --jq .commit.sha 2>/dev/null || true)
fi

[ -n "$CURRENT_HEAD" ] ||
    veto "the tip of ${HEAD_REPO}/${HEAD_BRANCH} could not be resolved — the branch may be gone"

[ "$CURRENT_HEAD" = "$HEAD_SHA" ] ||
    veto "the branch tip moved to ${CURRENT_HEAD:0:8}; a newer run supersedes this one"

# --- gate 4: what actually failed -----------------------------------------
# The aggregate (<workflow> result) only mirrors its needs, so it is not
# evidence either way. Everything else that did not succeed must carry the
# acquisition annotation.

TOTAL_JOBS=$(jq '.total_count // 0' "$JOBS_JSON")
LISTED_JOBS=$(jq '.jobs // [] | length' "$JOBS_JSON")
[ "$TOTAL_JOBS" -le "$LISTED_JOBS" ] ||
    veto "the run reports $TOTAL_JOBS jobs but only $LISTED_JOBS were listed"

AGGREGATE="$WF_NAME result"

while IFS=$'\t' read -r jid jname jconcl; do
    [ -z "$jid" ] && continue
    FAILED_NAMES+=("$jname")
    if [ -n "$FIXTURES" ]; then
        ann="$ANN_DIR/annotations-$jid.json"
        [ -f "$ann" ] || {
            echo "::error::$ann is missing for candidate job $jname" >&2
            exit 2
        }
        messages=$(jq -r '.[].message // empty' "$ann")
    else
        messages=$(gh api --paginate "repos/$GH_REPO/check-runs/$jid/annotations" --jq '.[].message')
    fi
    if grep -qF "$ACQUIRED_NEEDLE" <<<"$messages"; then
        ACQUIRED_NAMES+=("$jname")
    else
        OTHER_NAMES+=("$jname ($jconcl)")
    fi
done < <(
    jq -r --arg agg "$AGGREGATE" '
        .jobs // [] | .[]
        | select(.name != $agg)
        | select(.conclusion != "success" and .conclusion != "skipped" and .conclusion != "neutral")
        | [.id, .name, .conclusion] | @tsv' "$JOBS_JSON"
)

[ "${#FAILED_NAMES[@]}" -gt 0 ] ||
    veto "nothing failed outside the aggregate check — there is no lost runner to retry"

[ "${#OTHER_NAMES[@]}" -eq 0 ] ||
    veto "$(join "${OTHER_NAMES[@]}") ended without the runner-acquisition annotation"

verdict "re-run" "${#ACQUIRED_NAMES[@]} job(s) lost their hosted runner before starting: $(join "${ACQUIRED_NAMES[@]}")" true
