#!/usr/bin/env bash
set -euo pipefail

echo "==> Validating AGENTS.md commands..."

# Extract code blocks from AGENTS.md and verify they reference valid commands
AGENTS_FILE="AGENTS.md"

if [ ! -f "$AGENTS_FILE" ]; then
  echo "::error::AGENTS.md not found at repository root"
  exit 1
fi

# Verify AGENTS.md is not empty
if [ "$(wc -c < "$AGENTS_FILE")" -lt 100 ]; then
  echo "::error::AGENTS.md is too short (< 100 chars)"
  exit 1
fi

# Extract cargo commands from code blocks and validate they parse
failures=0
while IFS= read -r cmd; do
  # Remove comments (# ...) from the command
  bare_cmd=$(echo "$cmd" | sed 's/#.*//' | xargs)
  if [ -z "$bare_cmd" ]; then
    continue
  fi

  # Validate cargo subcommand exists
  subcmd=$(echo "$bare_cmd" | awk '{print $2}')
  if ! cargo help "$subcmd" &>/dev/null; then
    echo "::error::Unknown cargo subcommand in AGENTS.md: '$subcmd' (from: $bare_cmd)"
    failures=$((failures + 1))
  fi

  # Cargo aliases are not ordinary subcommands. `cargo "$subcmd" --help`
  # executes the alias, which is unsafe for test aliases and can turn a
  # documentation check into a full test run. `cargo help` above verifies that
  # the alias is registered; skip the ordinary-subcommand flag check here.
  if grep -Eq "^[[:space:]]*$subcmd[[:space:]]*=" .cargo/config.toml 2>/dev/null; then
    continue
  fi

  # Validate flags parse (dry-run via cargo help)
  if ! cargo "$subcmd" --help &>/dev/null; then
    echo "::error::Command in AGENTS.md may have invalid flags: $bare_cmd"
    failures=$((failures + 1))
  fi
done < <(grep -E '^cargo ' "$AGENTS_FILE" | sort -u)

# Check that referenced files and directories exist
for ref in src/ tests/ benchmarks/ docs/ scripts/ .github/; do
  if [ ! -e "$ref" ]; then
    echo "::error::AGENTS.md references '$ref' which does not exist"
    failures=$((failures + 1))
  fi
done

if [ "$failures" -gt 0 ]; then
  echo "::error::AGENTS.md validation found ${failures} issue(s)"
  exit 1
else
  echo "AGENTS.md validation passed."
fi
