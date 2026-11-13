#!/usr/bin/env bash
# Prunes old generations of the Actions caches a workflow's writers leave
# behind, so the repository stays inside its Actions cache budget.
#
#   prune-caches.sh <key-prefix>...
#
#       Lists refs/heads/main cache entries through the GitHub API, keeps
#       the newest entry per series (by created_at) under each prefix, and
#       deletes the rest.
#
#   prune-caches.sh --entries <file> <key-prefix>...
#
#       Test seam: reads the listing from a TSV file — id, key, created_at,
#       ref — and prints the would-be deletions as `id\tkey` lines instead
#       of calling the API.
#
#   KEEP_OTHER=<n> (default 1) — entries outside the rust-cache key shape
#       share one group per matched prefix and keep the newest n.
#
# Series rule: a rust-cache key is `v0-rust-<series>-<env-hash>-<lock-hash>`
# — stripping the two trailing hashes yields the series, so generations
# written under a changed environment still count as one series and the
# stale-env entries retire with them. Within a series only the newest
# entry survives, which is always the generation main's latest push just
# wrote. Keys outside the v0-rust-* shape (perf-bin-* binary bundles are
# content-keyed rather than generational) form one group per matched
# prefix. Keys matching no given prefix — actionlint-*, pinact-*, and any
# series another workflow owns — are never touched.
set -euo pipefail

keep_other=${KEEP_OTHER:-1}
entries_file=''
prefixes=()
while [ $# -gt 0 ]; do
    case "$1" in
        --entries)
            entries_file=${2:?"--entries needs a TSV file path"}
            shift 2
            ;;
        --)
            shift
            break
            ;;
        -*)
            echo "::error::unknown flag: $1" >&2
            exit 2
            ;;
        *)
            prefixes+=("$1")
            shift
            ;;
    esac
done
while [ $# -gt 0 ]; do
    prefixes+=("$1")
    shift
done
if [ "${#prefixes[@]}" -eq 0 ]; then
    echo "::error::no key prefixes given — nothing is safe to delete" >&2
    exit 2
fi

listing=$(mktemp)
work=$(mktemp)
sorted=$(mktemp)
kept=$(mktemp)
victims=$(mktemp)
trap 'rm -f "$listing" "$work" "$sorted" "$kept" "$victims"' EXIT

if [ -n "$entries_file" ]; then
    [ -f "$entries_file" ] || {
        echo "::error::--entries file not found: $entries_file" >&2
        exit 2
    }
    cat "$entries_file" >"$listing"
else
    [ -n "${GITHUB_REPOSITORY:-}" ] || {
        echo "::error::GITHUB_REPOSITORY is not set" >&2
        exit 1
    }
    gh api --paginate "repos/${GITHUB_REPOSITORY}/actions/caches?per_page=100" \
        --jq '.actions_caches[] | [.id, .key, .created_at, .ref] | @tsv' >"$listing"
fi

# Fold each entry into its series; only refs/heads/main entries that match
# a prefix participate.
while IFS=$'\t' read -r id key created ref; do
    [ -n "$id" ] || continue
    [ "$ref" = "refs/heads/main" ] || continue
    matched=''
    for p in "${prefixes[@]}"; do
        case "$key" in
            "$p"*)
                matched=$p
                break
                ;;
        esac
    done
    [ -n "$matched" ] || continue
    case "$key" in
        v0-rust-*)
            # Strip the trailing env-hash and lock-hash: a series spans
            # every environment its writers have used.
            series="rust:${key%-*-*}"
            ;;
        *)
            series="other:$matched"
            ;;
    esac
    printf '%s\t%s\t%s\t%s\n' "$series" "$created" "$id" "$key" >>"$work"
done <"$listing"

# created_at is ISO-8601 UTC — a lexical sort is chronological.
sort -t $'\t' -k1,1 -k2,2r -k3,3 "$work" >"$sorted"

# Sorted rows keep one series' entries contiguous, so a scalar counter is
# enough — this must run under the older bash macOS runners ship.
prev_series=''
n=0
while IFS=$'\t' read -r series created id key; do
    if [ "$series" != "$prev_series" ]; then
        prev_series=$series
        n=0
    fi
    lim=1
    case "$series" in
        other:*) lim=$keep_other ;;
    esac
    if [ "$n" -ge "$lim" ]; then
        printf '%s\t%s\t%s\n' "$id" "$key" "$series" >>"$victims"
    else
        printf '%s\t%s\n' "$id" "$key" >>"$kept"
    fi
    n=$((n + 1))
done <"$sorted"

if [ -n "$entries_file" ]; then
    cut -f1,2 "$victims"
    exit 0
fi

while IFS=$'\t' read -r id key; do
    echo "keep: $key (id $id)"
done <"$kept"
if [ ! -s "$victims" ]; then
    echo "nothing to prune"
    exit 0
fi

failed=0
while IFS=$'\t' read -r id key _series; do
    echo "deleting: $key (id $id)"
    if ! gh api -X DELETE "repos/${GITHUB_REPOSITORY}/actions/caches/${id}"; then
        echo "::warning::failed to delete cache id $id ($key)" >&2
        failed=1
    fi
done <"$victims"
exit "$failed"
