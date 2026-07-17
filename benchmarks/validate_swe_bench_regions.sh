#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  printf 'usage: %s TASKS_JSONL SEALED_LABELS_JSONL RECEIPT_JSON\n' "$0" >&2
  exit 2
fi

tasks=$1
labels=$2
receipt=$3
artifact_blake3=${ARTIFACT_BLAKE3:-target/debug/examples/artifact_blake3}
if [[ -e $receipt ]]; then
  printf 'refusing to overwrite %s\n' "$receipt" >&2
  exit 1
fi
if [[ ! -x $artifact_blake3 ]]; then
  printf 'artifact hasher is not executable: %s\n' "$artifact_blake3" >&2
  exit 1
fi
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

bindings_valid=$(jq -n --slurpfile tasks "$tasks" --slurpfile labels "$labels" '
  ($tasks | length) == ($labels | length)
  and ([$tasks[].task_id] | length) == ([$tasks[].task_id] | unique | length)
  and ([$labels[].task_id] | length) == ([$labels[].task_id] | unique | length)
  and all($tasks[];
    . as $task
    | [$labels[]
        | select(
            .task_id == $task.task_id
            and .source_record_blake3 == $task.source_record_blake3
          )]
    | length == 1
  )
')
if [[ $bindings_valid != true ]]; then
  printf 'task and sealed-label bindings are invalid\n' >&2
  exit 1
fi

jq -nrc --slurpfile tasks "$tasks" --slurpfile labels "$labels" '
  [
    $tasks[] as $task
    | ($labels[] | select(.task_id == $task.task_id)) as $sealed
    | ($sealed.core_regions + $sealed.optional_regions)[]
    | {
        repository: ($task.repository.url
          | sub("^https://github.com/"; "")
          | sub("\\.git$"; "")),
        revision: $task.repository.revision,
        path: .path,
        end_line: .end_line
      }
  ]
  | group_by([.repository, .revision, .path])
  | map({
      repository: .[0].repository,
      revision: .[0].revision,
      path: .[0].path,
      max_end_line: (map(.end_line) | max),
      regions: length
  })
  | sort_by(.repository, .revision, .path)
  | .[]
' > "$work/files.jsonl"

: > "$work/content-manifest.jsonl"
files=0
regions=0
retrieved=0
retrieved_bytes=0
missing=0
out_of_bounds=0

while IFS= read -r entry; do
  repository=$(jq -r '.repository' <<<"$entry")
  revision=$(jq -r '.revision' <<<"$entry")
  path=$(jq -r '.path' <<<"$entry")
  max_end_line=$(jq -r '.max_end_line' <<<"$entry")
  entry_regions=$(jq -r '.regions' <<<"$entry")
  encoded_path=$(jq -rn --arg value "$path" '$value | @uri')
  encoded_path=${encoded_path//%2F//}
  url="https://raw.githubusercontent.com/$repository/$revision/$encoded_path"
  files=$((files + 1))
  regions=$((regions + entry_regions))

  if ! curl --fail --location --silent --retry 3 --retry-all-errors \
    --output "$work/content" "$url" 2> "$work/curl-error"; then
    missing=$((missing + 1))
    continue
  fi

  retrieved=$((retrieved + 1))
  bytes=$(wc -c < "$work/content")
  retrieved_bytes=$((retrieved_bytes + bytes))
  lines=$(awk 'END { print NR }' "$work/content")
  if (( max_end_line > lines )); then
    out_of_bounds=$((out_of_bounds + 1))
  fi
  content_hash=$("$artifact_blake3" "$work/content" | cut -d' ' -f1)
  jq -cn \
    --arg repository "$repository" \
    --arg revision "$revision" \
    --arg path "$path" \
    --arg content_blake3 "$content_hash" \
    --argjson lines "$lines" \
    --argjson max_end_line "$max_end_line" \
    '{
      repository: $repository,
      revision: $revision,
      path: $path,
      content_blake3: $content_blake3,
      lines: $lines,
      max_end_line: $max_end_line
    }' >> "$work/content-manifest.jsonl"
done < "$work/files.jsonl"

manifest_hash=$("$artifact_blake3" "$work/content-manifest.jsonl" | cut -d' ' -f1)
tasks_hash=$("$artifact_blake3" "$tasks" | cut -d' ' -f1)
labels_hash=$("$artifact_blake3" "$labels" | cut -d' ' -f1)
verifier_hash=$("$artifact_blake3" "$0" | cut -d' ' -f1)
task_count=$(jq -s 'length' "$tasks")
repository_revisions=$(jq -s '[.[] | [.repository.url, .repository.revision]] | unique | length' "$tasks")
passed=false
if (( retrieved == files && missing == 0 && out_of_bounds == 0 )); then
  passed=true
fi

jq -n \
  --argjson tasks "$task_count" \
  --argjson repository_revisions "$repository_revisions" \
  --argjson unique_files "$files" \
  --argjson regions "$regions" \
  --argjson retrieved_files "$retrieved" \
  --argjson retrieved_bytes "$retrieved_bytes" \
  --argjson missing_files "$missing" \
  --argjson out_of_bounds_files "$out_of_bounds" \
  --arg verifier_blake3 "$verifier_hash" \
  --arg tasks_blake3 "$tasks_hash" \
  --arg sealed_labels_blake3 "$labels_hash" \
  --arg content_manifest_blake3 "$manifest_hash" \
  --argjson validation_passed "$passed" \
  '{
    schema_version: 1,
    method: "fetch exact GitHub repository/revision/path content and validate maximum labeled end line",
    verifier_blake3: $verifier_blake3,
    tasks_blake3: $tasks_blake3,
    sealed_labels_blake3: $sealed_labels_blake3,
    tasks: $tasks,
    repository_revisions: $repository_revisions,
    unique_files: $unique_files,
    regions: $regions,
    retrieved_files: $retrieved_files,
    retrieved_bytes: $retrieved_bytes,
    missing_files: $missing_files,
    out_of_bounds_files: $out_of_bounds_files,
    content_manifest_blake3: $content_manifest_blake3,
    validation_passed: $validation_passed
  }' > "$receipt"

if [[ $passed != true ]]; then
  exit 1
fi
