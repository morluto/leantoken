#!/usr/bin/env bash
set -euo pipefail

echo "==> Scanning for TODO, FIXME, HACK markers..."

markers=$(rg -n --no-heading \
  -g '*.rs' \
  -g '!target/' \
  -e 'TODO' -e 'FIXME' -e 'HACK' -e 'XXX' \
  2>/dev/null || true)

if [ -z "$markers" ]; then
  echo "No unresolved TODO/FIXME/HACK/XXX markers found."
else
  count=$(echo "$markers" | wc -l | tr -d ' ')
  echo "$markers"
  echo "::warning::Found ${count} technical debt marker(s). Link to tracking issues where possible (e.g., TODO(#123))."
fi
