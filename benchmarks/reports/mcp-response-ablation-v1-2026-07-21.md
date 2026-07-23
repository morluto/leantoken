# MCP response ablation

Date: 2026-07-21

Experiment: `mcp-response-ablation-v1`

Frozen manifest: [`../mcp_response_ablation.json`](../mcp_response_ablation.json)

Machine-readable result:
[`mcp-response-ablation-v1-2026-07-21.json`](mcp-response-ablation-v1-2026-07-21.json)

Run the checked experiment with:

```bash
cargo run --release --example mcp_response_ablation -- \
  --manifest benchmarks/mcp_response_ablation.json \
  --repository-root . \
  --output target/mcp-response-ablation.json
```

The example verifies the canonical-LF fixture-tree BLAKE3, exact
`cl100k_base` counting, the bound host-compatibility matrix, every candidate in
the frozen manifest, and the acceptance gates. Its unit test regenerates the
report and compares it with the checked JSON value.

## Scope

The fixture indexes `fixtures/sample_repo` and retrieves one fixed 500-source-
token context task. The complete wire count includes initialize,
`notifications/initialized`, `tools/list`, the call request, and its result.
The pre-change dual result is reconstructed from the runtime's internal task
fingerprint so the baseline and candidate use the same selected source and
retrieval trajectory.

The first response returns 195 source tokens in five fragments. A follow-up
passes all five aligned receipt hashes. It exactly resends zero source tokens,
reports one known-hash omission, and returns 14 source tokens in overlapping
but non-identical ranges. Serialization-only candidates must add zero exact
resends and zero overlapping tokens relative to that baseline.

## Result

| Candidate | Complete wire delta | Decision |
| --- | ---: | --- |
| structured-only result | -588 | host-scoped opt-in for Codex CLI 0.144.1 only |
| text-only result | -552 | rejected: no real-host model-consumption proof |
| omit internal task fingerprint | -39 | accepted |
| aligned receipt hash table | -212 | retained existing compaction |
| compact fragment metadata | -336 | retained existing compaction |
| omit empty/default fields | -26 | retained existing compaction |
| short reason codes | -10 | rejected: no model-behavior evidence |
| omit omission details | -60 | rejected: loses resend diagnostics |
| omit default freshness metadata | -22 | rejected: loses explicit correctness state |
| positional outline tuples | -101 | rejected: breaks typed range identity |
| tree path strings | -129 | rejected: loses kind/language/size metadata |
| remove tool-description examples | -91 | rejected: call-quality effect unmeasured |

Omitting the task fingerprint reduces response JSON from 549 to 531 tokens,
the dual result from 1,162 to 1,123 tokens, and the complete modeled handoff
from 3,979 to 3,940 tokens. The request already carries the task, no follow-up
request accepts the fingerprint, and aligned fragment hashes retain the
identity used by known-content deduplication. The in-memory response keeps the
fingerprint for evaluation; only serialization omits it.

The reviewed context snapshot retains fragment paths, line ranges, source,
selection reasons, aligned fragment hashes, bounded omission details,
warnings, repository generation, freshness, emitted-source count, and exact-
token status. It omits the internal task fingerprint, repeated fragment hash,
score, per-fragment token count, default source representation, and null
cursor. The six-tool MCP catalog is 2,381 tokens after adding the read-only
`leantoken_savings` entry. Output schemas remain unpublished.

## Decision

Keep `dual` as the global result mode. Codex CLI 0.144.1 has one structured-
only model-consumption proof, but other requested hosts and text-only behavior
remain unknown. The structured delta is therefore an explicit host/version
opt-in, not a new default.

All provider-native input values remain null. The exact local wire reductions
do not establish a provider billing reduction because no provider-visible
request frame permits representation-level attribution. Absolute deltas also
depend on selected evidence, path lengths, and response size; this small fixed
fixture is a regression boundary, not a population estimate.
