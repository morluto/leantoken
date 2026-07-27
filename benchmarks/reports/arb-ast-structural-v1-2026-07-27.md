# ARB AST structural lane v1

Date: 2026-07-27

## Decision

Keep the bounded benchmark infrastructure, but do not promote the AST
structural lane into production retrieval or force its paths into selected
context.

The lane found one labeled owner path in each of the two smoke tasks without
using labels: `clap_builder/src/builder/arg.rs` and `src/click/core.py`.
However, soft focus did not change selected file recall, source tokens, or
dead-end source tokens. It only added diagnostic JSON. Owner discovery is
therefore promising evidence for a later selection contract, not a retrieval
quality win by itself.

## Frozen inputs

- Dataset: two-task local ARB v2 trace2code smoke adapter output.
- Manifest BLAKE3:
  `888fd766be72d8831946cf0038cf39374b5480564088f8d2cf7aa8d553bb6a7f`.
- Harness revision:
  `bc1e0747f49c511c0abf0139c88bce6c02e3fe02`.
- No corpus, repository, or history download was performed for this
  experiment.
- Baseline: `--workflow-evidence`.
- Candidate: `--workflow-evidence --ast-structural-lane`.

Both reports were generated from the same revision and already-local pinned
Clap and Click checkouts. The dirty-worktree bit is expected because the
archive files and unrelated user-owned untracked files existed while the
reports were written.

## Contract and bounds

The lane:

1. reads only bounded workflow failure traces and declared task languages;
2. tolerantly parses at most 16 KiB with the existing tree-sitter parser;
3. examines at most two languages and keeps at most eight call-reference terms;
4. issues one definition-only indexed search per term, with at most 16 results
   and 1,024 response tokens per search;
5. ranks paths by distinct matched definition terms, hit count, normalized
   score, and lexical path;
6. supplies at most two soft focus paths, with no forced fragment quota.

Gold files, spans, and root-cause labels are used only after retrieval to score
the report. The experiment performs no additional repository scan, parser pass
during indexing, storage write, Git subprocess, or production ranking change.

## Paired result

| Metric | Baseline | Candidate | Delta |
| --- | ---: | ---: | ---: |
| Selected labeled files | 1 / 3 | 1 / 3 | 0 |
| Generated labeled files | 2 / 3 | 2 / 3 | 0 |
| Source tokens | 1,255 | 1,255 | 0 |
| Complete response JSON tokens | 2,534 | 2,698 | +164 |
| Dead-end source tokens | 1,054 | 1,054 | 0 |
| Two-turn JSON tokens | 7,359 | 7,729 | +370 |

The candidate extracted two Rust terms and six Python terms, made eight total
bounded structural searches, and placed a labeled owner in each task's path
list. Click's `src/click/core.py` remained generated but not selected.

## Rejected forced-focus probe

During development, a forced minimum of one fragment for each of two AST paths
raised selected labeled files from 1 / 3 to 2 / 3. It also raised source tokens
from 1,255 to 3,921 and dead-end source tokens from 1,054 to 1,409. That
configuration was rejected: the recall gain depended on consuming substantially
more context and irrelevant evidence.

The next useful experiment should test a bounded owner-evidence reservation or
orientation capsule that can admit one small structural owner excerpt without
boosting every candidate from that file. Reusing the current focus-minimum
contract is not justified by this smoke result.

