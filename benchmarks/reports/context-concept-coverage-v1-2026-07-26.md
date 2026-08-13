# Context concept coverage v1

## Decision

Adopt the benchmark and its label boundary. Do not claim that current context
retrieval has solved task-level coverage.

The frozen regression thresholds passed, but selected evidence covered only
6/12 concepts. The result is development evidence from a consumed prospective
validation set, not a promotion gate or generalization result.

## Identity

- Harness revision: `3d96f14a5fd7798dd1aca8603ec4a28e4e5d4a69`
- Harness worktree dirty: `false`
- Source manifest BLAKE3:
  `5991d8a643a873ef61d5a4122f52abd7f589a5403d13ab609ebc6b9428e73d9a`
- Concept-label BLAKE3:
  `f496deaadd5098b1faf9f7519cb9e92cdcf14ebf09d73f5beebd675b889c9763`
- Full report BLAKE3:
  `371ed1a2ac598693fb8884729ff18801d4cf5ec34ac8c54cc19e3093af3d14d8`
- Tokenizer: exact `cl100k_base`
- Source budget: 1,200 tokens per task

## Accuracy

| Metric | Result |
|---|---:|
| Candidate relevant-file recall | 11/11 (100%) |
| Candidate concept recall | 9/12 (75%) |
| Selected relevant-file recall | 7/11 (63.6%) |
| Selected concept recall | 6/12 (50%) |
| Concept selection retention | 6/9 (66.7%) |
| Labeled line-anchor recall | 19/38 (50%) |
| Labeled-file precision | 7/27 (25.9%) |

The difference between 100% candidate file recall and 75% candidate concept
recall is the central result: seeing some candidate from every labeled file
does not prove that candidate generation reached every required behavior in
those files.

Generation missed Flask's `run-server-name-ipv6` and
`run-ipv6-regression` concepts and Tokio's
`blocking-pool-shutdown-regression`. Selection then lost Gin's
`non-bmp-regression` plus Express's `empty-extension-resolution` and
`trailing-dot-regression`, despite candidates reaching those anchors.

## Cost

- Selected source: 4,073 tokens
- Complete first-response JSON: 6,999 tokens
- Complete two-turn JSON, including known-hash request and response: 14,756
  tokens
- Dead-end source: 1,703 tokens
- Exact known-hash resends: 0
- Estimated repeated-range source: 549 tokens

The four warm context medians ranged from 90.7 ms to 256.6 ms on this host.
Cold indexing ranged from 535 ms for Express to 4.78 s for Tokio. These timings
describe this machine only and are not acceptance thresholds.

## Reproduction

```bash
cargo run --release --example representative_benchmark -- \
  --manifest benchmarks/validation.json \
  --concept-labels benchmarks/context_concept_coverage.json \
  --require-concept-thresholds \
  --repos-root target/validation-repos \
  --output target/context-concept-coverage.json
```

The full JSON report retains per-concept matched anchors, candidate diagnostics,
selected evidence, exact token accounting, pinned revisions, and host metadata.

