#!/usr/bin/env bash
# Aggregate the jobs a workflow's merge gate depends on. Reads the `needs`
# object — the workflow passes ${{ toJSON(needs) }} as NEEDS_JSON — and
# fails when any needed job ended `failure` or `cancelled`. `success` and
# `skipped` pass: a change-scoped skip is not a failure. A missing or
# unrecognized result fails closed.
set -euo pipefail

: "${NEEDS_JSON:?NEEDS_JSON must be set to the toJSON(needs) payload}"

rows=$(jq -r 'to_entries[] | [.key, (.value.result // "")] | @tsv' <<<"$NEEDS_JSON")
if [ -z "$rows" ]; then
    echo "::error::the needs payload held no jobs"
    exit 1
fi

status=0
while IFS=$'\t' read -r job result; do
    case "$result" in
        success | skipped) printf '  %-24s %s\n' "$job" "$result" ;;
        *)
            printf '  %-24s %s\n' "$job" "${result:-<none>}"
            status=1
            ;;
    esac
done <<<"$rows"

if [ "$status" -ne 0 ]; then
    echo "::error::a required job failed or was cancelled" >&2
    exit 1
fi
