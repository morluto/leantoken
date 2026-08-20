#!/usr/bin/env bash
set -euo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/ci-secret-scan-range.sh"
base=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
head=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb

assert_range() {
  local expected=$1
  shift
  local actual
  actual="$(bash "$script" "$@")"
  if [[ $actual != "$expected" ]]; then
    echo "expected secret-scan range '$expected', got '$actual'" >&2
    exit 1
  fi
}

assert_failure() {
  if bash "$script" "$@" >/dev/null 2>&1; then
    echo "invalid secret-scan context unexpectedly succeeded: $*" >&2
    exit 1
  fi
}

assert_range "$base..$head" pull_request "$base" "$head"
assert_range "$base..$head" merge_group "$base" "$head"
assert_range "$base..$head" push "$base" "$head"
assert_range "$head" push 0000000000000000000000000000000000000000 "$head"
assert_range --all schedule "" "$head"
assert_range --all workflow_dispatch "" "$head"

assert_failure pull_request malformed "$head"
assert_failure merge_group "$base" 'bad;revision'
assert_failure unsupported "$base" "$head"
