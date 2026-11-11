#!/usr/bin/env bash
# The dry-run stand-in for the SARIF upload step: prints the call the real
# `uses: github/codeql-action/upload-sarif` step would make, without
# touching the code-scanning API.
set -euo pipefail

sarif="${DBTL_SARIF_FILE:-}"
if [ -z "$sarif" ] || [ ! -f "$sarif" ]; then
	echo "::warning::no SARIF file was produced — nothing to upload" >&2
	exit 0
fi

cat <<EOF
[dry-run] would upload SARIF file '${sarif}' via github/codeql-action/upload-sarif with category 'dbtl'
[dry-run]   equivalent call: POST /repos/${GITHUB_REPOSITORY:-<repo>}/code-scanning/sarifs with the gzipped SARIF contents
EOF
