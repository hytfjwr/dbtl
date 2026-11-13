#!/usr/bin/env bash
# Classify a pull request's changed paths into change scopes and print the
# scopes as `name=true|false` lines on stdout — the `changes` job appends
# them to $GITHUB_OUTPUT and downstream jobs skip themselves on the scopes
# they do not consume.
#
#   changed-paths.sh pull_request          diff the merge commit's parents
#   changed-paths.sh <event>               push/schedule/dispatch — no
#                                          classification, everything runs
#   changed-paths.sh --paths <file>        classify a newline-separated list
#                                          (the test seam — no git involved)
#
# Scopes:
#   code     every file outside the docs/site buckets — Rust sources,
#            Cargo.*, tests/, crates/, xtask/, benches/, action/, demo/,
#            .github/ (except a docs-site.yml-only change), config, and
#            anything unrecognized. The safe default: unsure means code.
#   docs     prose a reader sees: top-level *.md, exactly one level of
#            .github/*.md, docs/**, schema/**. site content docs set BOTH
#            docs and site (tests read repository Markdown alongside the
#            site pages). *.md under tests/, crates/, xtask/, action/,
#            benches/ or any other directory is a build/test input — code.
#   site     site/** and .github/workflows/docs-site.yml
#   docsref  docs/** and schema/** — the surface `Docs reference`
#            regenerates, so only edits there can drift it
#
# A diff that cannot be produced (an unreachable base or an empty list)
# reports every scope true: when classification is unsure, run everything.
set -euo pipefail

usage() {
    echo "usage: changed-paths.sh <event> | changed-paths.sh --paths <file>" >&2
    exit 2
}

emit() {
    local kv
    for kv in "code=$1" "docs=$2" "site=$3" "docsref=$4"; do
        case "$kv" in
            *=true | *=false) ;;
            *)
                echo "::error::changed-paths produced a bad value: $kv" >&2
                exit 1
                ;;
        esac
    done
    printf 'code=%s\ndocs=%s\nsite=%s\ndocsref=%s\n' "$1" "$2" "$3" "$4"
}

all_true() {
    emit true true true true
}

mode=''
paths_file=''
case "${1:-}" in
    --paths)
        [ $# -eq 2 ] || usage
        paths_file=$2
        [ -f "$paths_file" ] || {
            echo "::error::--paths file not found: $paths_file" >&2
            exit 2
        }
        mode='file'
        ;;
    pull_request)
        [ $# -eq 1 ] || usage
        mode='pr'
        ;;
    '' | -h | --help)
        usage
        ;;
    *)
        [ $# -eq 1 ] || usage
        mode='all'
        ;;
esac

case "$mode" in
    all)
        all_true
        exit 0
        ;;
esac

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT

if [ "$mode" = file ]; then
    cat "$paths_file" >"$tmp"
elif ! git diff --name-only -z HEAD^1 HEAD 2>/dev/null | tr '\0' '\n' >"$tmp"; then
    echo "::warning::cannot diff the merge ref's parents — treating every scope as changed" >&2
    all_true
    exit 0
fi

if [ ! -s "$tmp" ]; then
    echo "::warning::the changed-path list is empty — treating every scope as changed" >&2
    all_true
    exit 0
fi

code=false
docs=false
site=false
docsref=false
while IFS= read -r path || [ -n "$path" ]; do
    [ -z "$path" ] && continue
    case "$path" in
        site/*)
            site=true
            case "$path" in
                site/src/content/docs/*.md) docs=true ;;
            esac
            ;;
        .github/workflows/docs-site.yml)
            site=true
            ;;
        docs/* | schema/*)
            docs=true
            docsref=true
            ;;
        .github/*.md)
            # One level only — .github/ISSUE_TEMPLATE/x.md is code.
            case "${path#.github/}" in
                */*) code=true ;;
                *) docs=true ;;
            esac
            ;;
        *.md)
            # Top-level *.md is prose; deeper *.md is a build/test input.
            case "$path" in
                */*) code=true ;;
                *) docs=true ;;
            esac
            ;;
        *)
            code=true
            ;;
    esac
done <"$tmp"

emit "$code" "$docs" "$site" "$docsref"
