# Exact-read delta protocol

Date: 2026-07-26

Experiment: `read-receipt-delta-v1`

Harness revision:
`8fa8c80e4627eb61c43e047550b43b4ce57c4a70`

The release-mode run used a detached worktree at the harness revision and
reported `harness_worktree_dirty: false`:

```bash
cargo run --release --example read_delta_benchmark -- \
  --output target/read_delta_benchmark_report.json
```

## Scope

The protocol benchmark performs four deterministic repeated-read cases:

- a one-line edit to the complete current `src/services/read.rs`;
- an uneconomic one-line small-file diff;
- a symbol that moves and changes across an indexed generation; and
- a changed target whose prior read did not opt into base capture.

Each case compares the opt-in response with a delta-disabled full-content
control. A selected delta must contain both sides of the edit and cost strictly
fewer source tokens and complete serialized response tokens. Fallbacks must
return complete current content and the expected reason.

Separate owner tests cover unchanged hashes, truncated reads, moved targets,
changed overlapping receipt evidence, and exact receipt suppression. Registry
unit tests enforce the 30-minute TTL, 128-entry limit, 512 KiB per-entry limit,
8 MiB retained-content limit, byte accounting, and refresh behavior.

## Result

| Case | Decision | Source tokens full/returned | Complete JSON full/returned | Delta |
| --- | --- | ---: | ---: | ---: |
| real source line edit | delta | 10,970 / 115 | 12,889 / 434 | -12,455 |
| small uneconomic edit | `delta_not_smaller` fallback | 4 / 4 | 185 / 309 | +124 |
| moved symbol | `target_changed` fallback | 9 / 9 | 194 / 319 | +125 |
| missing base | `base_unavailable` fallback | 8 / 8 | 192 / 308 | +116 |

Across the four cases, the opt-in responses avoided 10,855 of 10,991 full
source tokens (98.76%) and reduced complete serialized responses from 13,460
to 1,370 tokens (89.82%). The useful large-edit case accounts for the savings.
Safe fallback diagnostics add 116 to 125 complete-response tokens over a
delta-disabled full read.

## Decision

Adopt the bounded exact-read delta protocol as an explicit opt-in. It provides
a large, verified reduction for repeated exact reads of a large target while
preserving full-content fallback when reconstruction or economy is uncertain.
Do not enable it automatically for every read: callers that do not expect a
follow-up edit pay metadata overhead, and uneconomic or unavailable bases are
more expensive than a normal full read.

This experiment establishes protocol correctness and deterministic token
economy, not model task success. Ranked context, continuation pages, process
restart persistence, and provider-visible billing remain out of scope. Any
default agent-policy change requires a repeated edit-fix-test model evaluation.
