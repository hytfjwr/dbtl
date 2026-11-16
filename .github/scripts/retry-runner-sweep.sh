#!/usr/bin/env bash
# Sweep recently-failed CI/Dogfood/Security runs whose actor is
# github-actions[bot] and feed each to the same runner-loss decision the
# workflow_run path uses — then issue `gh run rerun --failed` for the
# runs it accepts.
#
# A run triggered by GITHUB_TOKEN — the release pull request's checks are
# the case that motivated this — never produces a workflow_run event, so
# the event-driven job can never see it. This sweep is the only path that
# reaches those runs. Human-authored runs are filtered out: they already
# take the event path, and evaluating one failure through two paths could
# issue the retry twice.
#
# The decision is idempotent — an accepted run either advances to a new
# attempt (an earlier attempt then holds jobs, and the gate vetoes) or
# lands back in approval gating (the newest attempt lists no jobs) — so
# a wide window and repeated sweeps can never issue a second retry.
#
# Live mode reads the GitHub API — GH_REPO names the repository, the job
# token authenticates:
#
#   retry-runner-sweep.sh --decision <script> --since <ISO8601>
#                         [--max N] [--summary FILE] [--dry-run]
#
#   retry-runner-sweep.sh --decision <script> --run-id <id>
#                         [--summary FILE] [--dry-run]
#
# `--run-id` pins the evaluation to one run and skips the listing — and
# the actor filter — entirely: a human picks that target deliberately.
# `--dry-run` evaluates and reports but never issues a re-run; it is the
# manual-dispatch default so an inspection can never touch a real run.
#
# Offline mode replays recorded API payloads:
#
#   <dir>/runs.json              GET /actions/runs (the listing)
#   <dir>/<run-id>/...           the decision script's fixture layout,
#                                one directory per listed run
#
#   retry-runner-sweep.sh --decision <script> --since <ISO8601> --fixtures <dir>
#
# With `--run-id` the fixture directory IS the run's fixture directory.
# stdout reports `evaluated=<id>` per run inspected, `rerun=<id>` per
# run the decision accepted, and `issued=<id>` per re-run dispatched (in
# offline mode: would be dispatched). `--summary <file>` appends a
# Markdown bullet list with one line per evaluated run.
set -euo pipefail

BOT_ACTOR='github-actions[bot]'
# The same set the workflow_run trigger watches — a run of any other
# workflow is never the retry's business.
WATCHED_NAMES='["CI","Dogfood","Security"]'

DECISION=""
SUMMARY=""
FIXTURES=""
RUN_ID=""
SINCE=""
MAX=20
DRY_RUN=0
while [ $# -gt 0 ]; do
    case "$1" in
        --decision)
            DECISION="${2:?--decision needs a script path}"
            shift 2
            ;;
        --summary)
            SUMMARY="${2:?--summary needs a file}"
            shift 2
            ;;
        --fixtures)
            FIXTURES="${2:?--fixtures needs a directory}"
            shift 2
            ;;
        --run-id)
            RUN_ID="${2:?--run-id needs a value}"
            shift 2
            ;;
        --since)
            SINCE="${2:?--since needs an ISO-8601 timestamp}"
            shift 2
            ;;
        --max)
            MAX="${2:?--max needs a count}"
            shift 2
            ;;
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        *)
            echo "unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

[ -n "$DECISION" ] || {
    echo "--decision is required" >&2
    exit 2
}
[ -f "$DECISION" ] || {
    echo "decision script not found: $DECISION" >&2
    exit 2
}
if [ -z "$RUN_ID" ] && [ -z "$SINCE" ]; then
    echo "--since is required when sweeping the window" >&2
    exit 2
fi
case "$MAX" in
    '' | *[!0-9]*)
        echo "--max needs a number, got: $MAX" >&2
        exit 2
        ;;
esac
if [ -z "$FIXTURES" ]; then
    : "${GH_REPO:?GH_REPO must name the repository as owner/name}"
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# --- candidates ------------------------------------------------------------
# `id<TAB>workflow<TAB>url` lines for every run to evaluate, newest first.
# `--run-id` skips the listing: the single run is fetched (or read from
# the fixture directory) only to name it in the summary.

candidates() {
    local list_json="$TMP/runs.json"
    if [ -n "$RUN_ID" ]; then
        if [ -n "$FIXTURES" ]; then
            list_json="$FIXTURES/run.json"
        else
            gh api "repos/$GH_REPO/actions/runs/$RUN_ID" >"$list_json"
        fi
        [ -f "$list_json" ] || {
            echo "::error::$list_json is missing" >&2
            exit 2
        }
        jq -r '[.id, (.name // "?"), (.html_url // "")] | @tsv' "$list_json"
        return
    fi
    if [ -n "$FIXTURES" ]; then
        list_json="$FIXTURES/runs.json"
    else
        gh api "repos/$GH_REPO/actions/runs" \
            -f status=failure \
            -f "actor=$BOT_ACTOR" \
            -f "created=>$SINCE" \
            -f per_page=100 >"$list_json"
    fi
    [ -f "$list_json" ] || {
        echo "::error::$list_json is missing" >&2
        exit 2
    }
    # The listing params narrow server-side; the same filters repeat here
    # so a replayed payload — or an API surprise — cannot widen the set.
    jq -r --arg since "$SINCE" --arg bot "$BOT_ACTOR" --argjson watched "$WATCHED_NAMES" '
        .workflow_runs // []
        | map(select(
            .conclusion == "failure"
            and .actor.login == $bot
            and (.name | IN($watched[]))
            and (.created_at // "") >= $since))
        | sort_by(.created_at)
        | reverse
        | unique_by(.id)
        | .[] | [.id, .name, (.html_url // "")] | @tsv' "$list_json"
}

# --- evaluate each candidate through the shared decision --------------------
# The listing lands in a file first: inside a process substitution a
# failed fetch would kill only that subshell, and the sweep would report
# a silent "nothing found" instead of failing.

candidates >"$TMP/candidates.tsv"

COUNT=0
declare -a SUM_LINES
while IFS=$'\t' read -r rid rname rurl; do
    [ -n "$rid" ] || continue
    [ "$COUNT" -lt "$MAX" ] || break
    COUNT=$((COUNT + 1))

    if [ -n "$FIXTURES" ]; then
        dir="$FIXTURES"
        [ -n "$RUN_ID" ] || dir="$FIXTURES/$rid"
        # A missing or malformed fixture is an authoring error — abort
        # loudly instead of recording a wrong verdict.
        out=$(bash "$DECISION" --fixtures "$dir")
    else
        # A live API hiccup inside one decision must not kill the sweep —
        # that run is recorded and the rest still evaluate.
        if ! out=$(bash "$DECISION" --run-id "$rid"); then
            echo "evaluated=$rid"
            echo "failed=$rid"
            SUM_LINES+=("- [${rname} run ${rid}](${rurl}) — **decision error**, left for a human")
            continue
        fi
    fi

    rerun=$(sed -n 's/^rerun=//p' <<<"$out")
    reason=$(sed -n 's/^reason=//p' <<<"$out")
    echo "evaluated=$rid"
    if [ "$rerun" = "true" ]; then
        echo "rerun=$rid"
        if [ "$DRY_RUN" = 0 ]; then
            if [ -n "$FIXTURES" ]; then
                echo "issued=$rid"
            elif gh run rerun "$rid" --failed >&2; then
                echo "issued=$rid"
            else
                # The dispatch failed; nothing advanced, so the next
                # sweep evaluates this run again and records it here.
                SUM_LINES+=("- [${rname} run ${rid}](${rurl}) — **re-run could not be dispatched** — ${reason}")
                continue
            fi
        fi
        SUM_LINES+=("- [${rname} run ${rid}](${rurl}) — **re-run** — ${reason}")
    else
        SUM_LINES+=("- [${rname} run ${rid}](${rurl}) — no re-run — ${reason}")
    fi
done <"$TMP/candidates.tsv"

echo "swept=$COUNT"

[ -n "$SUMMARY" ] || exit 0
{
    echo "### Runner-failure sweep"
    echo
    if [ -n "$RUN_ID" ]; then
        scope="run \`${RUN_ID}\`"
    else
        scope="failed CI/Dogfood/Security runs by \`${BOT_ACTOR}\` created at or after \`${SINCE}\`"
    fi
    [ "$DRY_RUN" = 1 ] && scope="$scope — **dry run**: nothing was re-run"
    echo "- Scope: $scope"
    echo "- Evaluated: $COUNT"
    printf '%s\n' "${SUM_LINES[@]+"${SUM_LINES[@]}"}"
    echo
} >>"$SUMMARY"
