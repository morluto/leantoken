#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 --execute ARM CLEAN_REPOSITORY OUTPUT_DIRECTORY" >&2
  echo "arms: full-native, thin-native, thin-leantoken-dual, thin-leantoken-structured, thin-leantoken-structured-owner" >&2
  exit 2
}

canonical_directory() {
  (cd -- "$1" && pwd -P)
}

canonical_file() {
  local directory
  local filename
  directory=$(canonical_directory "$(dirname -- "$1")")
  filename=$(basename -- "$1")
  printf '%s/%s\n' "$directory" "$filename"
}

if [[ $# -ne 4 || $1 != "--execute" ]]; then
  usage
fi

arm=$2
repository=$3
output_directory=$4
script_directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
manifest="$script_directory/multi_agent_context_pilot.json"
codex_binary=${CODEX_PILOT_CODEX_BINARY:-codex}
leantoken_binary=${CODEX_PILOT_LEANTOKEN_BINARY:-"$script_directory/../target/release/leantoken"}
receipt_binary=${CODEX_PILOT_RECEIPT_BINARY:-"$script_directory/../target/release/examples/codex_multi_agent_receipt"}
codex_state_directory=${CODEX_PILOT_STATE_DIRECTORY:-"${CODEX_HOME:-$HOME/.codex}"}
sessions_root="$codex_state_directory/sessions"

command -v jq >/dev/null
command -v "$codex_binary" >/dev/null
repository=$(canonical_directory "$repository")
leantoken_binary=$(canonical_file "$leantoken_binary")
receipt_binary=$(canonical_file "$receipt_binary")
test -x "$leantoken_binary"
test -x "$receipt_binary"
test -d "$sessions_root"

expected_revision=$(jq -r '.repository_revision' "$manifest")
actual_revision=$(git -C "$repository" rev-parse HEAD)
if [[ $actual_revision != "$expected_revision" ]]; then
  echo "repository revision mismatch: expected $expected_revision, got $actual_revision" >&2
  exit 1
fi
if [[ -n $(git -C "$repository" status --porcelain) ]]; then
  echo "pilot repository must be clean" >&2
  exit 1
fi
if [[ $("$codex_binary" --version) != "codex-cli 0.144.1" ]]; then
  echo "pilot is frozen to codex-cli 0.144.1" >&2
  exit 1
fi

case "$arm" in
  full-native)
    fork_turns=all
    retrieval_contract="The child must use only native repository search and read commands; it must not use web search or MCP."
    result_mode=
    ;;
  thin-native)
    fork_turns=none
    retrieval_contract="The child must use only native repository search and read commands; it must not use web search or MCP."
    result_mode=
    ;;
  thin-leantoken-dual)
    fork_turns=none
    retrieval_contract="The child must use the leantoken MCP server as its only repository discovery and source-reading mechanism; it must not run native shell search/read commands or use web search. It must call leantoken_context first, use at most 6 LeanToken retrieval calls, and keep each context request at or below 1800 source tokens."
    result_mode=dual
    ;;
  thin-leantoken-structured | thin-leantoken-structured-owner)
    fork_turns=none
    retrieval_contract="The child must use the leantoken MCP server as its only repository discovery and source-reading mechanism; it must not run native shell search/read commands or use web search. It must call leantoken_context first, use at most 6 LeanToken retrieval calls, and keep each context request at or below 1800 source tokens."
    result_mode=structured
    ;;
  *) usage ;;
esac

task_prompt=$(jq -r '.task_prompt' "$manifest")
prompt="Run one controlled read-only repository investigation. Do not solve the repository task yourself. Spawn exactly one child with task name known_hashes, fork_turns set to $fork_turns, and no further descendants. $retrieval_contract Child task: $task_prompt Return one compact JSON object with an evidence array; each item must contain path, symbol, and role. Wait for the child, then return the child JSON unchanged with no extra commentary. The stdin block is fixed parent-history calibration material; do not summarize it."

mkdir -p "$output_directory/private"
stdout_path="$output_directory/private/$arm.stdout.jsonl"
stderr_path="$output_directory/private/$arm.stderr.log"
receipt_path="$output_directory/$arm.receipt.json"
svg_path="$output_directory/$arm.tokens.svg"
for path in "$stdout_path" "$stderr_path" "$receipt_path" "$svg_path"; do
  if [[ -e $path ]]; then
    echo "refusing existing artifact: $path" >&2
    exit 1
  fi
done

codex_args=(
  exec --json --ignore-user-config --ignore-rules --strict-config --enable multi_agent
  -c 'agents.max_threads=2'
  -c 'agents.max_depth=1'
  -c 'model_reasoning_effort="low"'
  -c 'approval_policy="never"'
  -c 'web_search="disabled"'
  -c 'include_apps_instructions=false'
  -s read-only
  -C "$repository"
)
if [[ -n $result_mode ]]; then
  command_value=$(jq -Rn --arg value "$leantoken_binary" '$value')
  args_value=$(jq -cn --arg root "$repository" --arg mode "$result_mode" \
    '["mcp","--root",$root,"--tokenizer","o200k_base","--result-mode",$mode]')
  codex_args+=(
    -c "mcp_servers.leantoken.command=$command_value"
    -c "mcp_servers.leantoken.args=$args_value"
    -c 'mcp_servers.leantoken.required=true'
    -c 'mcp_servers.leantoken.default_tools_approval_mode="approve"'
    -c 'mcp_servers.leantoken.startup_timeout_sec=300'
    -c 'mcp_servers.leantoken.tool_timeout_sec=120'
  )
fi

sed -n '1,420p' "$repository/docs/measurement.md" |
  "$codex_binary" "${codex_args[@]}" "$prompt" >"$stdout_path" 2>"$stderr_path"

root_thread_id=$(jq -r 'select(.type == "thread.started") | .thread_id' "$stdout_path" | head -n 1)
if [[ -z $root_thread_id ]]; then
  echo "Codex JSONL did not report a root thread ID" >&2
  exit 1
fi
root_rollout=$(find "$sessions_root" -type f -name "*-$root_thread_id.jsonl" -print -quit)
if [[ -z $root_rollout ]]; then
  echo "root rollout not found below $sessions_root" >&2
  exit 1
fi

"$receipt_binary" \
  --root-rollout "$root_rollout" \
  --sessions-root "$sessions_root" \
  --experiment-id multi-agent-context-pilot \
  --arm "$arm" \
  --expected-children 1 \
  --gold-manifest "$manifest" \
  --output "$receipt_path" \
  --svg "$svg_path"

jq '{arm, task_evaluation, provider_usage, child: (.thread_receipts[1] | {provider_request_count, provider_usage, tool_calls})}' "$receipt_path"
