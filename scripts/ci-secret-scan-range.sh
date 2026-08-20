#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: ci-secret-scan-range.sh <event> <base> <head>" >&2
  exit 2
fi

event=$1
base=$2
head=$3
zero_revision=0000000000000000000000000000000000000000

is_object_id() {
  [[ $1 =~ ^([0-9a-fA-F]{40}|[0-9a-fA-F]{64})$ ]]
}

if ! is_object_id "$head"; then
  echo "secret-scan head must be one full Git object ID" >&2
  exit 2
fi

case "$event" in
  pull_request | merge_group | push)
    if [[ -z $base || $base == "$zero_revision" ]]; then
      # A newly created branch has no provider base. Scanning the immutable
      # head walks every commit reachable from that source revision.
      printf '%s\n' "$head"
    elif is_object_id "$base"; then
      printf '%s..%s\n' "$base" "$head"
    else
      echo "secret-scan base must be empty or one full Git object ID" >&2
      exit 2
    fi
    ;;
  schedule | workflow_dispatch)
    printf '%s\n' --all
    ;;
  *)
    echo "unsupported secret-scan event: $event" >&2
    exit 2
    ;;
esac
