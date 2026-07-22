#!/usr/bin/env bash
set -euo pipefail

echo "==> Checking for duplicate source files..."

records=$(mktemp)
trap 'rm -f "$records"' EXIT

while IFS= read -r -d '' file; do
  hash=$(sha256sum "$file" | awk '{print $1}')
  printf '%s\t%s\n' "$hash" "$file" >> "$records"
done < <(find . -name '*.rs' \
  -not -path './target/*' \
  -not -path './.git/*' \
  -type f \
  -print0)

# Check for exact duplicate content via SHA-256. This job runs on Ubuntu,
# where `sha256sum` is provided by coreutils.
duplicate_hashes=$(sort "$records" | awk -F '\t' '
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
