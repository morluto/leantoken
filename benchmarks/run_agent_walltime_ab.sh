#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
repos_arg="${1:-${repo_root}/target/validation-repos}"
output_arg="${2:-${repo_root}/target/agent-walltime-ab}"

for command in cargo git python3 rg rustc sed; do
  if ! command -v "${command}" >/dev/null 2>&1; then
    echo "error: required command '${command}' was not found" >&2
    exit 2
  fi
done

repos_root="$(cd "${repos_arg}" && pwd -P)"
mkdir -p "$(dirname "${output_arg}")"
output_parent="$(cd "$(dirname "${output_arg}")" && pwd -P)"
output_dir="${output_parent}/$(basename "${output_arg}")"
if [[ -e "${output_dir}" ]]; then
  echo "error: output path already exists: ${output_dir}" >&2
  exit 2
fi
mkdir "${output_dir}"

source_sha="$(git -C "${repo_root}" rev-parse --verify HEAD)"
work_root="$(mktemp -d "${TMPDIR:-/tmp}/leantoken-agent-walltime-ab.XXXXXX")"
worktree="${work_root}/source"
build_target="${work_root}/target"

cleanup() {
  git -C "${repo_root}" worktree remove --force "${worktree}" >/dev/null 2>&1 || true
  rm -rf "${work_root}"
}
trap cleanup EXIT

git -C "${repo_root}" worktree add --detach "${worktree}" "${source_sha}"
(
  cd "${worktree}"
  CARGO_INCREMENTAL=0 CARGO_TARGET_DIR="${build_target}" \
    cargo build --release --bin leantoken
)

python3 "${worktree}/scripts/agent_walltime_ab.py" \
  --manifest "${worktree}/benchmarks/agent_walltime_ab.json" \
  --validation-manifest "${worktree}/benchmarks/validation.json" \
  --repos-root "${repos_root}" \
  --source-root "${worktree}" \
  --leantoken "${build_target}/release/leantoken" \
  --output "${output_dir}/report.json" \
  --markdown-output "${output_dir}/report.md"

cat "${output_dir}/report.md"
