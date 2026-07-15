#!/usr/bin/env bash
set -euo pipefail

MAX_LINES=1000
EXCLUDE_DIRS="target|.git"

echo "==> Checking for files exceeding ${MAX_LINES} lines..."

offenders=$(find . -name '*.rs' \
  -not -path './target/*' \
  -not -path './.git/*' \
  -exec wc -l {} \; \
  | sort -rn \
  | awk -v max="$MAX_LINES" '$1 > max { print NR ". " $2 ": " $1 " lines" }')

if [ -n "$offenders" ]; then
  echo "$offenders"
  echo "::warning::Files exceed ${MAX_LINES}-line threshold. Consider splitting them into smaller modules."
else
  echo "All files within ${MAX_LINES}-line limit."
fi
