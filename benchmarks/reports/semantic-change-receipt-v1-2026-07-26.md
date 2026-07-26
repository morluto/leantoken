# Semantic change receipt

Date: 2026-07-26

Experiment: `semantic-change-receipt-v1`

Harness revision:
`116f3e2a60813ea07b69195286b8e8a445b80988`

The release-mode run reported `harness_tracked_worktree_dirty: false`:

```bash
cargo run --release --example semantic_change_receipt_benchmark -- \
  --iterations 21 \
  --output benchmarks/reports/semantic-change-receipt-v1.json
```

## Scope

The deterministic two-commit fixture covers:

- public signature and private body-only modifications;
- a unique-body public rename;
- symbol additions and removals within one file;
- whole-file symbol additions and removals;
- added, removed, and modified JSON configuration key paths;
- unchanged symbols and configuration keys that must not appear;
- likely owner tests found for source and missing for configuration; and
- sentinel configuration values that must not appear in the receipt.

Unit tests separately reject ambiguous rename fingerprints and declarations
without bodies. Integration tests cover working-tree range gaps, non-review
workflow omission, complete response token accounting, and history
`diff_symbol` classification.

## Result

| Metric | Result |
| --- | ---: |
| Expected/returned truth items | 10 / 10 |
| True positives | 10 |
| False positives / false negatives | 0 / 0 |
| Precision / recall | 1.000 / 1.000 |
| Response tokens without/with receipt | 704 / 1,174 |
| Receipt overhead | 470 tokens |
| Receipt overhead relative to compact control | 66.76% |
| Latency p50 / p95 / max (21 runs) | 69.4 / 79.9 / 80.2 ms |

All adoption gates passed. Neither `base-secret-value` nor
`head-secret-value` appeared in the serialized semantic receipt. The raw
machine-readable result is
[`semantic-change-receipt-v1.json`](semantic-change-receipt-v1.json).

## Decision

Adopt the receipt for materialized immutable-range `review` context and matched
`history(diff_symbol)` operations. It is deterministic, bounded, and useful
review metadata without adding a tool or model dependency.

Do not enable it for plan-only or ordinary implementation context. The 470-token
receipt is a 66.76% increase over this deliberately compact fixture response; it
is diagnostic cost, not a direct token-saving claim. Rename classification
intentionally under-classifies unless the body fingerprint is unique, JSON
coverage is restricted to recognized configuration filenames, and owner-test
matching remains a filename heuristic. The fixture validates protocol
correctness, not downstream model task success, repository-wide recall, or
change risk. No risk score should be inferred from these fields.
