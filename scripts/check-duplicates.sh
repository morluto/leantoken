#!/usr/bin/env bash
set -euo pipefail

echo "==> Checking for duplicate source files..."

files=$(find . -name '*.rs' \
  -not -path './target/*' \
  -not -path './.git/*' \
  -type f)

# Check for exact duplicate content via SHA-256. This job runs on Ubuntu,
# where `sha256sum` is provided by coreutils.
duplicate_hashes=$(echo "$files" | xargs sha256sum 2>/dev/null | sort | awk '
{
  hash = $1
  file = $2
  if (hash in seen) {
    duplicates[hash] = duplicates[hash] "\n  " file
  } else {
    seen[hash] = file
  }
}
END {
  for (h in duplicates) {
    print "Duplicate files (identical content):"
    print "  " seen[h] duplicates[h]
  }
}')

if [ -n "$duplicate_hashes" ]; then
  echo "::error::Exact duplicate files detected!"
  echo "$duplicate_hashes"
  exit 1
fi

echo "No exact duplicate files found."

# Check for code-level duplication: find files with high Jaccard similarity
# on trimmed, non-blank, non-comment lines
SIMILARITY_THRESHOLD=85
tempdir=$(mktemp -d)
trap 'rm -rf "$tempdir"' EXIT

echo "$files" | while IFS= read -r f; do
  base=$(basename "$f")
  grep -v '^\s*$' "$f" | grep -v '^\s*//' | sed 's/^\s*//;s/\s*$//' | sort -u > "$tempdir/$base.norm"
done 2>/dev/null || true

norm_files=$(find "$tempdir" -name '*.norm' | sort)
while IFS= read -r f1; do
  while IFS= read -r f2; do
    [ "$f1" = "$f2" ] && continue
    [ "$f1" \> "$f2" ] && continue
    total=$(sort -u "$f1" "$f2" | wc -l | tr -d ' ')
    common=$(comm -12 "$f1" "$f2" | wc -l | tr -d ' ')
    if [ "$total" -gt 10 ] && [ "$common" -gt 0 ]; then
      pct=$(( common * 100 / total ))
      if [ "$pct" -ge "$SIMILARITY_THRESHOLD" ]; then
        echo "::warning::$(basename "$f1" .norm).rs and $(basename "$f2" .norm).rs share ${pct}% similar lines -- consider refactoring shared logic."
      fi
    fi
  done < <(echo "$norm_files")
done < <(echo "$norm_files")
