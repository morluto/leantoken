# ARB Git-history lane experiment v1

This report evaluates a bounded, benchmark-only Git-history path lane on the
two-task ARB trace2code smoke set. The lane is not production ranking.

## Reproducibility

- Manifest BLAKE3:
  `888fd766be72d8831946cf0038cf39374b5480564088f8d2cf7aa8d553bb6a7f`
- Harness revision:
  `0cd7f1d357fc6c39085960dc9a74a80695c15ae1`
- Harness worktree dirty: `true` because generated reports and unrelated
  pre-existing untracked files were present; implementation source was
  committed before both final runs
- Baseline: typed workflow evidence
- Candidate: typed workflow evidence plus the Git-history lane
- Tokenizer: `cl100k_base`, exact counts

For each task, the lane freezes at most 256 ancestors, merges the eight
caller-observed workflow symbols into one POSIX extended regex, and submits the
complete commit set to one `git log --no-walk` pickaxe process. It would retain
at most four current files by matching-commit count and recency. The fixed
maximum is two Git subprocesses per task, independent of symbol count.

The machine-readable reports are
[`arb-history-lane-baseline-v1-2026-07-27.json`](arb-history-lane-baseline-v1-2026-07-27.json),
[`arb-history-lane-candidate-v1-2026-07-27.json`](arb-history-lane-candidate-v1-2026-07-27.json),
and
[`arb-history-lane-ablation-v1-2026-07-27.json`](arb-history-lane-ablation-v1-2026-07-27.json).

## Result

Both target repositories were intentionally prepared as blobless partial
clones. Their commit and tree objects were sufficient to freeze 256 ancestors,
but pickaxe required historical blobs that were not local. The final runner
sets `GIT_NO_LAZY_FETCH=1`; both tasks therefore reported:

```text
history_objects_unavailable_without_lazy_fetch
```

No candidate paths were emitted and context was not modified. All retrieval
metrics were consequently identical:

| Metric | Workflow evidence | History candidate | Delta |
| --- | ---: | ---: | ---: |
| Selected gold files | 1/3 | 1/3 | 0 |
| Generated gold files | 2/3 | 2/3 | 0 |
| Source tokens | 1,255 | 1,255 | 0 |
| First-response JSON tokens | 2,534 | 2,534 | 0 |
| Dead-end source tokens | 1,054 | 1,054 | 0 |
| Two-turn context JSON tokens | 7,359 | 7,359 | 0 |

A complete tiny Git fixture separately verifies the available path: two
observed symbols use exactly two subprocesses, inspect two commits, and rank the
symbol-owning current file first.

## Decision

Keep the bounded lane as experimental infrastructure, but do not promote or
enable it in production. The current small ARB checkouts cannot evaluate its
retrieval value without downloading historical blobs, and downloading those
objects would violate this experiment's resource constraint.

Missing history is an unavailable measurement, not a zero-match result. A
future evaluation may reuse the lane on an already-complete local checkout or a
separately approved small fixture corpus. It must retain lazy-fetch suppression,
the two-process bound, and a paired workflow-evidence baseline.
