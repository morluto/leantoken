#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 --plan REPOSITORIES_ROOT" >&2
  echo "       $0 --execute REPOSITORIES_ROOT OUTPUT_DIRECTORY" >&2
  echo "       $0 --resume REPOSITORIES_ROOT OUTPUT_DIRECTORY" >&2
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

sha256_file() {
  sha256sum "$1" | cut -d ' ' -f 1
}

if [[ $# -eq 2 && $1 == "--plan" ]]; then
  mode=plan
  repositories_root=$2
  output_directory=
elif [[ $# -eq 3 && ( $1 == "--execute" || $1 == "--resume" ) ]]; then
  mode=${1#--}
  repositories_root=$2
  output_directory=$3
else
  usage
fi

script_directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repository_root=$(canonical_directory "$script_directory/..")
manifest=${CODEX_SUITE_MANIFEST:-"$script_directory/multi_agent_context_suite.json"}
manifest=$(canonical_file "$manifest")
codex_binary=${CODEX_SUITE_CODEX_BINARY:-codex}
leantoken_binary=${CODEX_SUITE_LEANTOKEN_BINARY:-"$repository_root/target/release/leantoken"}
receipt_binary=${CODEX_SUITE_RECEIPT_BINARY:-"$repository_root/target/release/examples/codex_multi_agent_receipt"}
suite_binary=${CODEX_SUITE_AGGREGATE_BINARY:-"$repository_root/target/release/examples/codex_multi_agent_suite"}
codex_state_directory=${CODEX_SUITE_STATE_DIRECTORY:-"${CODEX_HOME:-$HOME/.codex}"}
sessions_root="$codex_state_directory/sessions"

command -v jq >/dev/null
command -v sha256sum >/dev/null
command -v "$codex_binary" >/dev/null
repositories_root=$(canonical_directory "$repositories_root")
leantoken_binary=$(canonical_file "$leantoken_binary")
receipt_binary=$(canonical_file "$receipt_binary")
suite_binary=$(canonical_file "$suite_binary")
test -x "$leantoken_binary"
test -x "$receipt_binary"
test -x "$suite_binary"
test -d "$sessions_root"
jq -e '.schema_version == 1 and (.tasks | length) >= 3 and .controls.repetitions >= 5' "$manifest" >/dev/null

expected_host_version=$(jq -r '.controls.host_version' "$manifest")
if [[ $("$codex_binary" --version) != "codex-cli $expected_host_version" ]]; then
  echo "suite is frozen to codex-cli $expected_host_version" >&2
  exit 1
fi
expected_host_hash=$(jq -r '.controls.host_binary_sha256' "$manifest")
actual_host_hash=$(sha256_file "$(command -v "$codex_binary")")
if [[ $actual_host_hash != "$expected_host_hash" ]]; then
  echo "Codex binary hash mismatch" >&2
  exit 1
fi
expected_runtime_hash=$(jq -r '.controls.candidate_runtime.binary_sha256' "$manifest")
actual_runtime_hash=$(sha256_file "$leantoken_binary")
if [[ $actual_runtime_hash != "$expected_runtime_hash" ]]; then
  echo "LeanToken candidate binary hash mismatch" >&2
  exit 1
fi

parent_history_path="$repository_root/$(jq -r '.controls.parent_history.path' "$manifest")"
parent_history_end_line=$(jq -r '.controls.parent_history.end_line' "$manifest")
expected_parent_hash=$(jq -r '.controls.parent_history.sha256' "$manifest")
actual_parent_hash=$(sed -n "1,${parent_history_end_line}p" "$parent_history_path" | sha256sum | cut -d ' ' -f 1)
if [[ $actual_parent_hash != "$expected_parent_hash" ]]; then
  echo "parent-history calibration hash mismatch" >&2
  exit 1
fi

while IFS=$'\t' read -r corpus revision; do
  corpus_root=$(canonical_directory "$repositories_root/$corpus")
  actual_revision=$(git -C "$corpus_root" rev-parse HEAD)
  if [[ $actual_revision != "$revision" ]]; then
    echo "$corpus revision mismatch: expected $revision, got $actual_revision" >&2
    exit 1
  fi
  if [[ -n $(git -C "$corpus_root" status --porcelain) ]]; then
    echo "$corpus repository must be clean" >&2
    exit 1
  fi
done < <(jq -r '.tasks[] | [.corpus, .repository_revision] | @tsv' "$manifest")

schedule_file=$(mktemp -p /tmp leantoken-multi-agent-schedule.XXXXXX)
trap 'rm -f -- "$schedule_file"' EXIT
seed=$(jq -r '.controls.randomization.seed' "$manifest")
repetitions=$(jq -r '.controls.repetitions' "$manifest")
for repetition in $(seq 1 "$repetitions"); do
  while IFS=$'\t' read -r task_id corpus; do
    while IFS= read -r arm; do
      schedule_key=$(printf '%s' "$seed|$repetition|$task_id|$arm" | sha256sum | cut -d ' ' -f 1)
      printf '%s\t%s\t%s\t%s\t%s\n' "$schedule_key" "$repetition" "$task_id" "$corpus" "$arm" >>"$schedule_file"
    done < <(jq -r '.arms[].name' "$manifest")
  done < <(jq -r '.tasks[] | [.id, .corpus] | @tsv' "$manifest")
done
sort -o "$schedule_file" "$schedule_file"

if [[ $mode == plan ]]; then
  printf 'order\trepetition\ttask\tcorpus\tarm\n'
  cut -f 2- "$schedule_file" | nl -w 3 -s $'\t'
  exit 0
fi

if [[ $mode == execute ]]; then
  if [[ -e $output_directory ]]; then
    echo "refusing existing output directory: $output_directory" >&2
    exit 1
  fi
  mkdir -p "$output_directory/private" "$output_directory/gold" "$output_directory/runs"
  output_directory=$(canonical_directory "$output_directory")
  cp "$manifest" "$output_directory/manifest.json"
else
  output_directory=$(canonical_directory "$output_directory")
  if ! cmp -s "$manifest" "$output_directory/manifest.json"; then
    echo "resume manifest does not match the frozen output manifest" >&2
    exit 1
  fi
fi

experiment_id=$(jq -r '.experiment_id' "$manifest")
while IFS= read -r task_id; do
  jq --arg task_id "$task_id" '
    . as $suite
    | .tasks[]
    | select(.id == $task_id)
    | {
        schema_version: 1,
        experiment_id: $suite.experiment_id,
        task_id: .id,
        repository_revision: .repository_revision,
        evaluation_mode: "path",
        expected_evidence: .expected_evidence
      }
  ' "$manifest" >"$output_directory/gold/$task_id.json"
done < <(jq -r '.tasks[].id' "$manifest")

while IFS= read -r corpus; do
  echo "warming LeanToken index: $corpus"
  "$leantoken_binary" --root "$repositories_root/$corpus" --tokenizer o200k_base --json index >/dev/null
done < <(jq -r '.tasks[].corpus' "$manifest" | sort -u)

expected_runs=$(wc -l <"$schedule_file")
schedule_index=0
while IFS=$'\t' read -r schedule_key repetition task_id corpus arm; do
  schedule_index=$((schedule_index + 1))
  task_slug=${task_id//-/_}
  repository="$repositories_root/$corpus"
  task_prompt=$(jq -r --arg task_id "$task_id" '.tasks[] | select(.id == $task_id) | .prompt' "$manifest")
  run_name=$(printf '%03d-%s-%s-r%s' "$schedule_index" "$task_id" "$arm" "$repetition")
  run_directory="$output_directory/runs/$run_name"
  private_directory="$output_directory/private/$run_name"
  mkdir -p "$run_directory" "$private_directory"
  stdout_path="$private_directory/stdout.jsonl"
  stderr_path="$private_directory/stderr.log"
  receipt_path="$run_directory/receipt.json"
  svg_path="$run_directory/tokens.svg"
  run_metadata_path="$run_directory/run.json"

  if [[ -e $run_metadata_path ]]; then
    jq -e \
      --argjson schedule_index "$schedule_index" \
      --argjson repetition "$repetition" \
      --arg task_id "$task_id" \
      --arg arm "$arm" \
      '.schedule_index == $schedule_index and .repetition == $repetition and .task_id == $task_id and .arm == $arm' \
      "$run_metadata_path" >/dev/null
    test -s "$receipt_path"
    echo "[$schedule_index/$expected_runs] already complete: task=$task_id arm=$arm repetition=$repetition"
    continue
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
    thin-leantoken-structured-owner)
      fork_turns=none
      retrieval_contract="The child must use the leantoken MCP server as its only repository discovery and source-reading mechanism; it must not run native shell search/read commands or use web search. It must call leantoken_context first, use at most 8 LeanToken retrieval calls, and keep each context request at or below 1600 source tokens. Text and regex search hits may include an enclosing_symbol; prefer that owner or an exact returned line range when reading."
      result_mode=structured
      ;;
    thin-leantoken-context-bundle)
      fork_turns=none
      retrieval_contract="The child must use the leantoken MCP server as its only repository discovery mechanism; it must not run native shell search/read commands or use web search. Make exactly one leantoken_context call first with at most 1600 source tokens. If and only if that bundle lacks a directly required implementation or regression-test file, make at most one leantoken_search call with at most 1200 source tokens. Do not call leantoken_read, leantoken_outline, or leantoken_files. Infer the minimal file set from the returned fragments and hits without extra verification."
      result_mode=structured
      ;;
    *)
      echo "unknown arm: $arm" >&2
      exit 1
      ;;
  esac

  prompt="Run one controlled read-only repository investigation. Do not solve the repository task yourself. Spawn exactly one child with task name $task_slug, fork_turns set to $fork_turns, and no further descendants. $retrieval_contract Child task: $task_prompt Identify the minimal complete set of implementation and regression-test files that directly own this change. Return one compact JSON object with an evidence array; every item must contain path, symbol, and role. Do not report speculative or merely adjacent files. Wait for the child, then return the child JSON unchanged with no extra commentary. The stdin block is fixed parent-history calibration material; do not summarize it."

  echo "[$schedule_index/$expected_runs] task=$task_id arm=$arm repetition=$repetition"
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

  if [[ ! -e $stdout_path ]]; then
    sed -n "1,${parent_history_end_line}p" "$parent_history_path" |
      "$codex_binary" "${codex_args[@]}" "$prompt" >"$stdout_path" 2>"$stderr_path"
  else
    echo "recovering receipt from existing Codex output: $run_name"
  fi

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
    --experiment-id "$experiment_id" \
    --arm "$arm" \
    --expected-children 1 \
    --gold-manifest "$output_directory/gold/$task_id.json" \
    --output "$receipt_path" \
    --svg "$svg_path"

  jq -n \
    --argjson schedule_index "$schedule_index" \
    --arg schedule_key "$schedule_key" \
    --argjson repetition "$repetition" \
    --arg task_id "$task_id" \
    --arg corpus "$corpus" \
    --arg arm "$arm" \
    --arg receipt "runs/$run_name/receipt.json" \
    '{schema_version:1, schedule_index:$schedule_index, schedule_key:$schedule_key, repetition:$repetition, task_id:$task_id, corpus:$corpus, arm:$arm, receipt:$receipt}' \
    >"$run_metadata_path"

  if [[ ${CODEX_SUITE_STOP_AFTER:-0} -gt 0 && $schedule_index -ge ${CODEX_SUITE_STOP_AFTER} ]]; then
    echo "stopped after frozen schedule index $schedule_index"
    exit 0
  fi
done <"$schedule_file"

"$suite_binary" \
  --manifest "$output_directory/manifest.json" \
  --runs-root "$output_directory" \
  --output "$output_directory/aggregate.json"
echo "completed $expected_runs randomized runs"
