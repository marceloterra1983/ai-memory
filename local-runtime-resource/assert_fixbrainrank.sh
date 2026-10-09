#!/usr/bin/env bash
# Small assertion: the fixbrainrank guard test passes — exactly one test.
#
# Runs the same functional test the rebuild resource uses, through the lib
# harness (src/lib.rs `mod integration`), and refuses zero or multiple tests.
# Assumes a cargo toolchain is already on PATH (e.g. inside the build image).
#
# Usage:
#   assert_fixbrainrank.sh [SRC=/path/to/checkout] [-- CARGO_TEST_FLAGS...]
set -euo pipefail

src="${1:-$(pwd)}"
shift || true
cd "$src"

FILTER='integration::retrieval_entity_project_name::entity_stream_does_not_vote_for_the_scoped_project_name'

log="$(mktemp)"
trap 'rm -f "$log"' EXIT

if ! cargo test --locked -p ai-memory-store --lib "$FILTER" -- --exact "$@" >"$log" 2>&1; then
    echo "assert_fixbrainrank: functional test failed" >&2
    tail -30 "$log" >&2
    exit 1
fi

grep -E "^test |test result" "$log"
grep -Fxq "test $FILTER ... ok" "$log" \
    && grep -Eq '^test result: ok\. 1 passed; 0 failed;' "$log" \
    || { echo "assert_fixbrainrank: expected exactly 1 passing test" >&2; exit 1; }

echo "assert_fixbrainrank: ok (1 passed)"
