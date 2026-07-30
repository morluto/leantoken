#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
manifest="${repo_root}/benchmarks/paired_performance.json"
base_ref="${1:-HEAD^}"
output_arg="${2:-${repo_root}/target/paired-performance}"

for command in benchstat cargo git go jq python3 rustc; do
  if ! command -v "${command}" >/dev/null 2>&1; then
    echo "error: required command '${command}' was not found" >&2
    exit 2
  fi
done

pairs="${3:-$(jq -r '.default_pairs' "${manifest}")}"
if ! [[ "${pairs}" =~ ^[0-9]+$ ]] || (( pairs < 10 )); then
  echo "error: pairs must be an integer of at least 10" >&2
  exit 2
fi

if [[ -n "$(git -C "${repo_root}" status --porcelain --untracked-files=all)" ]]; then
  echo "error: paired performance runs require a clean committed HEAD" >&2
  exit 2
fi

base_sha="$(git -C "${repo_root}" rev-parse --verify "${base_ref}^{commit}")"
head_sha="$(git -C "${repo_root}" rev-parse --verify HEAD)"
if [[ "${base_sha}" == "${head_sha}" ]]; then
  echo "error: base and head resolve to the same commit" >&2
  exit 2
fi

export RUSTUP_TOOLCHAIN=1.95
rustc_version="$(rustc --version)"
rustc_prefix="$(jq -r '.rustc_version_prefix' "${manifest}")"
if [[ "${rustc_version}" != "${rustc_prefix}"* ]]; then
  echo "error: rustc '${rustc_version}' does not match '${rustc_prefix}'" >&2
  exit 2
fi

expected_benchstat_version="$(jq -r '.benchstat_version' "${manifest}")"
actual_benchstat_version="$(
  go version -m "$(command -v benchstat)" |
    awk '$1 == "mod" && $2 == "golang.org/x/perf" { print $3 }'
)"
if [[ "${actual_benchstat_version}" != "${expected_benchstat_version}" ]]; then
  echo "error: benchstat '${actual_benchstat_version}' does not match '${expected_benchstat_version}'" >&2
  exit 2
fi

host_os="$(rustc --print cfg | sed -n 's/^target_os="\(.*\)"$/\1/p')"
host_arch="$(rustc --print cfg | sed -n 's/^target_arch="\(.*\)"$/\1/p')"
if [[ -z "${host_os}" || -z "${host_arch}" ]]; then
  echo "error: could not determine rustc host OS and architecture" >&2
  exit 2
fi

mkdir -p "$(dirname "${output_arg}")"
output_parent="$(cd "$(dirname "${output_arg}")" && pwd -P)"
output_dir="${output_parent}/$(basename "${output_arg}")"
if [[ -e "${output_dir}" ]]; then
  echo "error: output path already exists: ${output_dir}" >&2
  exit 2
fi
mkdir "${output_dir}"
cp "${manifest}" "${output_dir}/manifest.json"

work_root="$(mktemp -d "${TMPDIR:-/tmp}/leantoken-paired-performance.XXXXXX")"
base_worktree="${work_root}/base"
head_worktree="${work_root}/head"
build_target="${work_root}/target"
base_bin="${work_root}/base-bin"
head_bin="${work_root}/head-bin"
hot_path_root="${work_root}/hot-path-corpus"

cleanup() {
  git -C "${repo_root}" worktree remove --force "${base_worktree}" >/dev/null 2>&1 || true
  git -C "${repo_root}" worktree remove --force "${head_worktree}" >/dev/null 2>&1 || true
  rm -rf "${work_root}"
}
trap cleanup EXIT

git -C "${repo_root}" worktree add --detach "${base_worktree}" "${base_sha}"
git -C "${repo_root}" worktree add --detach "${head_worktree}" "${head_sha}"

for side in base head; do
  if [[ "${side}" == "base" ]]; then
    worktree="${base_worktree}"
    bin_dir="${base_bin}"
  else
    worktree="${head_worktree}"
    bin_dir="${head_bin}"
    (
      cd "${worktree}"
      CARGO_TARGET_DIR="${build_target}" cargo clean --release -p leantoken
    )
  fi
  (
    cd "${worktree}"
    CARGO_INCREMENTAL=0 CARGO_TARGET_DIR="${build_target}" \
      cargo build --release --package leantoken-benchmarks --bin hot_path_bounds --bin indexing_profile
  )
  mkdir "${bin_dir}"
  cp "${build_target}/release/examples/hot_path_bounds" "${bin_dir}/"
  cp "${build_target}/release/examples/indexing_profile" "${bin_dir}/"
done

hot_args=()
while IFS= read -r argument; do
  hot_args+=("${argument}")
done < <(jq -r '.runner.hot_path_args[]' "${manifest}")

index_args=()
while IFS= read -r argument; do
  index_args+=("${argument}")
done < <(jq -r '.runner.indexing_profile_args[]' "${manifest}")

run_sample() {
  local side="$1"
  local pair="$2"
  local order="$3"
  local sequence="$4"
  local worktree bin_dir source_sha source_tree sample

  if [[ "${side}" == "base" ]]; then
    worktree="${base_worktree}"
    bin_dir="${base_bin}"
    source_sha="${base_sha}"
  else
    worktree="${head_worktree}"
    bin_dir="${head_bin}"
    source_sha="${head_sha}"
  fi
  source_tree="$(git -C "${worktree}" rev-parse "HEAD^{tree}")"
  sample="${output_dir}/samples/${side}-$(printf '%02d' "${pair}")"
  mkdir -p "${sample}"

  (
    cd "${worktree}"
    "${bin_dir}/hot_path_bounds" "${hot_args[@]}" \
      --repository-root "${hot_path_root}" \
      >"${sample}/hot-path.json" 2>"${sample}/hot-path.stderr.log"
    rm -rf "${hot_path_root}"
    "${bin_dir}/indexing_profile" "${index_args[@]}" \
      --output "${sample}/indexing-profile.json" \
      >"${sample}/indexing-profile.stdout.json" \
      2>"${sample}/indexing-profile.stderr.log"
  )

  if [[ -n "$(git -C "${worktree}" status --porcelain --untracked-files=all)" ]]; then
    echo "error: ${side} worktree became dirty during pair ${pair}" >&2
    exit 2
  fi

  jq -n \
    --arg side "${side}" \
    --argjson pair "${pair}" \
    --arg order "${order}" \
    --argjson sequence "${sequence}" \
    --arg source_sha "${source_sha}" \
    --arg source_tree_sha "${source_tree}" \
    --arg rustc_version "${rustc_version}" \
    --arg benchstat_version "${actual_benchstat_version}" \
    --arg host_os "${host_os}" \
    --arg host_arch "${host_arch}" \
    '{
      schema_version: 1,
      side: $side,
      pair: $pair,
      order: $order,
      sequence: $sequence,
      source_sha: $source_sha,
      source_tree_sha: $source_tree_sha,
      source_dirty: false,
      rustc_version: $rustc_version,
      benchstat_version: $benchstat_version,
      host_os: $host_os,
      host_arch: $host_arch
    }' >"${sample}/provenance.json"
}

for ((pair = 1; pair <= pairs; pair++)); do
  if (( pair % 2 == 1 )); then
    run_sample base "${pair}" AB 1
    run_sample head "${pair}" AB 2
  else
    run_sample head "${pair}" BA 1
    run_sample base "${pair}" BA 2
  fi
done

python3 "${head_worktree}/scripts/paired_performance.py" collect \
  --manifest "${head_worktree}/benchmarks/paired_performance.json" \
  --samples "${output_dir}/samples" \
  --pairs "${pairs}" \
  --base-out "${output_dir}/base.txt" \
  --head-out "${output_dir}/head.txt" \
  --parity-out "${output_dir}/parity.json"

benchstat "base=${output_dir}/base.txt" "head=${output_dir}/head.txt" \
  >"${output_dir}/benchstat.txt" 2>"${output_dir}/benchstat.stderr.log"
benchstat -format csv "base=${output_dir}/base.txt" "head=${output_dir}/head.txt" \
  >"${output_dir}/benchstat.csv" 2>>"${output_dir}/benchstat.stderr.log"

python3 "${head_worktree}/scripts/paired_performance.py" gate \
  --manifest "${head_worktree}/benchmarks/paired_performance.json" \
  --benchstat-csv "${output_dir}/benchstat.csv" \
  --markdown-out "${output_dir}/report.md" \
  --json-out "${output_dir}/report.json"

cat "${output_dir}/report.md"
