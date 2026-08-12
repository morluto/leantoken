# Benchmarks and evaluation

The `leantoken-benchmarks` package contains opt-in experiment executables. It is
not part of ordinary retrieval and is excluded from default workspace members.
Most contributors need only the product tests.

```bash
cargo test-product
cargo test-contract       # token-economy contract
cargo test-extras         # benchmark executable contracts
```

Use `cargo run --release -p leantoken-benchmarks --bin NAME -- --help` for a
version-matched command reference. The binary list in
`crates/benchmarks/Cargo.toml` is canonical; this guide does not duplicate all
of their flags.

## Evidence policy

Run comparisons from release builds against pinned repositories or frozen
fixtures. Keep baseline and candidate manifests byte-identical. Record exact
commands, revisions, input digests, tokenizer, platform, sample count, and raw
results. Exploratory output belongs under `target/`.

`benchmarks/reports/*.json` contains machine-readable fixtures and reviewed
evidence used by analyzers or tests. Do not edit a frozen report to describe a
new run; create a versioned successor. Dated Markdown summaries were removed
because they duplicated the JSON and became stale product documentation. Their
original interpretation remains in Git history.

The governing interpretation rules are in
[`docs/measurement.md`](../docs/measurement.md).

## Retrieval promotion gate

Changes to candidate generation, ranking, context allocation, or default
signals require baseline and candidate reports from the same frozen manifest
and a machine-readable receipt from `benchmark_ablation`.

```bash
cargo run --release -p leantoken-benchmarks --bin benchmark_ablation -- \
  --baseline target/baseline.json \
  --candidate target/candidate.json \
  --promotion-track cost \
  --baseline-task-success-rate 0.80 \
  --candidate-task-success-rate 0.80 \
  --baseline-two-turn-provider-input-tokens 120000 \
  --candidate-two-turn-provider-input-tokens 110000 \
  --baseline-follow-up-native-reads 20 \
  --candidate-follow-up-native-reads 18 \
  --baseline-tool-calls 80 \
  --candidate-tool-calls 74 \
  --output target/promotion-receipt.json
```

Use the `quality` track only when paired agent evaluation shows a task-success
gain. Use `cost` when success and recall are preserved. The gate fails closed
on global or task-family recall regression, increased dead-end or repeated
evidence, more follow-up native reads, resource-envelope violations, or failure
to meet the selected track's success and cost threshold. It writes the JSON
scorecard even on failure so CI can retain diagnostic evidence.

Task success, complete provider input, native reads, and tool calls must come
from the same paired evaluation. The retrieval harness does not invent these
values. Promotion thresholds are repository policy in code, not per-run knobs.

## Common owners

- `representative_benchmark` and `benchmark_ablation` own frozen retrieval
  comparison and promotion.
- `indexing_profile`, `hot_path_bounds`, and `real_repository_profile` own
  indexing, boundedness, storage, and resource evidence.
- `mcp_wire_*`, `host_wire_compatibility`, and
  `mcp_multiprocess_profile` own protocol and process-cost diagnostics.
- `typescript_parse_diagnostic` and `resolved_reference_oracle` own parser and
  heuristic-resolution evaluations; they do not establish compiler semantics.
- `model_ab*`, trajectory tools, and SWE-bench helpers own opt-in model
  evaluations and frozen adapters.
- `leantoken-lab` owns offline reading and normalization of committed evidence.

Specialized binaries validate their own manifests and bounds. Read their
`--help`, source, and schema-bearing fixtures before running them. Avoid copying
one-off commands into evergreen documentation.
