# ARB workflow-evidence A/B v1

This report evaluates the opt-in `workflow_evidence` context contract on the
two-task frozen ARB trace2code smoke set. It is a diagnostic contract test, not
an ARB leaderboard result or evidence for making workflow evidence implicit.

## Reproducibility

- Manifest BLAKE3:
  `888fd766be72d8831946cf0038cf39374b5480564088f8d2cf7aa8d553bb6a7f`
- Harness revision:
  `1b07f0535d23f2e7627a0e7e7809ac0641fa2f38`
- Harness worktree dirty: `true` because the generated reports and unrelated
  pre-existing untracked files were present; all source owned by the
  implementation was committed before both runs
- Tokenizer: `cl100k_base`, exact counts
- Tasks: one Rust task from `clap-rs/clap` and one Python task from
  `pallets/click`, both at the manifest's exact base revisions

Both arms use the same revision, manifest, repositories, token budgets, and
benchmark harness. The baseline passes empty evidence. The candidate derives
typed evidence deterministically from the public ARB query object:

- the UTF-8-safe last 8 KiB of the observed failure trace;
- up to eight backtick identifiers or trace tokens in occurrence order;
- up to eight repository-relative paths observed in the trace;
- up to eight failed test names, test definitions, or the observed command.

Extraction does not inspect `root_cause_files`, root-cause spans, related tests,
hard negatives, or any other label. The raw reports are
[`arb-workflow-evidence-baseline-v1-2026-07-27.json`](arb-workflow-evidence-baseline-v1-2026-07-27.json),
[`arb-workflow-evidence-candidate-v1-2026-07-27.json`](arb-workflow-evidence-candidate-v1-2026-07-27.json),
and
[`arb-workflow-evidence-ablation-v1-2026-07-27.json`](arb-workflow-evidence-ablation-v1-2026-07-27.json).

## Results

| Metric | Baseline | Evidence | Delta |
| --- | ---: | ---: | ---: |
| Selected gold files | 0/3 | 1/3 | +1 |
| Generated gold files | 1/3 | 2/3 | +1 |
| Selected line anchors | 0/3 | 0/3 | 0 |
| Source tokens | 1,151 | 1,255 | +104 |
| Complete first-response JSON tokens | 2,367 | 2,534 | +167 |
| Dead-end fragments | 16 | 13 | -3 |
| Dead-end source tokens | 1,151 | 1,054 | -97 |
| Two-turn context JSON tokens | 7,563 | 7,359 | -204 |

On the Clap task, workflow evidence generated and selected
`clap_builder/src/builder/arg.rs`, improving selected file recall from 0/2 to
1/2. Returned source rose from 797 to 909 tokens while dead-end source fell
from 797 to 708 tokens.

On the Click task, `src/click/core.py` remained present in the generated
candidate set but was not selected. Selected recall therefore stayed 0/1.
This remains a selection or structural-localization failure for later
experiments rather than evidence for allocating more generic query fan-out.

## Contract and bounds

The production contract is explicit and opt-in. It accepts failure traces,
symbols, repository-relative paths, and test intents, with at most eight values
per class, 8 KiB per value, and 32 KiB combined. Typed lanes share the existing
12-query ceiling and existing per-query storage caps. Test intent contributes a
bounded path prior rather than another executable search. Empty evidence
delegates to the pre-existing planner byte-for-byte at the query-plan boundary.

## Decision

Adopt the typed workflow-evidence contract as an opt-in retrieval input. The
smoke A/B demonstrates that directly observed execution evidence can repair a
candidate-generation miss and reduce dead-end evidence without increasing
two-turn protocol cost on these tasks.

Do not make workflow evidence implicit and do not promote a global ranking
change from this run. Two tasks cannot establish repository, language, or task
family generality, and neither arm recovered a line anchor. Carry the Click
failure into the planned history and AST structural-search experiments and use
larger frozen samples only when an experiment needs stronger promotion
evidence.
