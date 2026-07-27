# MCP compact projection ablation

Date: 2026-07-27

Experiment: `compact-projections-v1`

Source revision: `da2da4d16b63cb9abd6efaf8b92fe5d65a9d05da`

Frozen manifest:
[`../compact_projection_tasks.json`](../compact_projection_tasks.json)

Machine-readable result:
[`mcp-response-ablation-compact-projections-v1-2026-07-27.json`](mcp-response-ablation-compact-projections-v1-2026-07-27.json)

Run the checked release experiment with:

```bash
cargo run --release --example compact_projection_benchmark -- \
  --manifest benchmarks/compact_projection_tasks.json \
  --repository-root . \
  --source-revision "$(git rev-parse HEAD)" \
  --output target/compact-projection-report.json
```

The runner verifies the canonical-LF fixture-tree BLAKE3, exact
`cl100k_base` counting, the frozen workload manifest, path/symbol/hit
membership parity, verification coordinates, and every acceptance gate before
writing a result.

## Scope

The fixture indexes `fixtures/sample_repo`, adds 64 deterministic Rust callers
of one target symbol, and exercises broad `files`, `outline`, and `search`
requests. Each baseline and projection runs through the public service path
against the same repository generation. Complete response counts cover the
serialized service DTO, including metadata and continuation state.

The retry proxy is zero when the projection preserves the labeled
path/symbol/hit concepts and enough path, line-range, and hash evidence to
verify or expand the result. It does not substitute for a model-executed task.

## Result

| Projection | Full response | Compact response | Delta | Membership/concept parity | Verifiable | Retry proxy delta |
| --- | ---: | ---: | ---: | --- | --- | ---: |
| `files: paths` | 404 | 175 | -229 | pass | pass | 0 |
| `outline: signatures` | 3,536 | 2,770 | -766 | pass | pass | 0 |
| `search: grouped` | 7,011 | 278 | -6,733 | pass | pass | 0 |
| aggregate | 10,951 | 3,223 | -7,728 | pass | pass | 0 |

The aggregate response is 70.6% smaller on this frozen workload. Every
individual projection has a negative complete-response token delta; no
projection depends on an aggregate win to hide a regression.

## Decision

Adopt all three representations as explicit MCP opt-ins while keeping `full`
as the machine-readable schema default. The service layer owns projection,
exact response-budget finalization, cursor semantics, and telemetry. Adapters
only select the requested service response.

`files: paths` retains ordered paths and continuation while omitting
per-entry metadata. `outline: signatures` retains ordered signatures, line
ranges, per-file signature-set hashes, parse coverage, freshness, and
projection-bound cursors while omitting imports and byte offsets.
`search: grouped` retains one verifiable excerpt per selected symbol/file
group, summarized reference counts and ranges, coverage, freshness, and the
normal ranked-page continuation while omitting repeated scores, reasons, and
excerpts.

These measurements are a regression boundary, not a population estimate.
They do not measure provider billing, host UI framing, or end-to-end model task
success. Defaults remain unchanged because the evidence supports opt-in
representations, not replacing information-rich responses for every caller.
