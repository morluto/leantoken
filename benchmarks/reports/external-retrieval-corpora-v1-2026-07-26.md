# External Retrieval Corpora v1

This report records the first clean run of the pinned Semble and Sverklo
adapters. It evaluates the benchmark surface, not a production ranking change.

## Reproducibility

- Harness revision:
  `92460ae95af8635ad31035e165d4482213b37c46`
- Harness worktree dirty: `false`
- Tokenizer: `cl100k_base`, exact counts
- Semble manifest BLAKE3:
  `6bf423afa52e77de78e3bc9aa18500a0fb8f136f295af3170241510197fc10a4`
- Sverklo manifest BLAKE3:
  `7bce24ee3f31d32b5bc9afa8fc98033d7a38bf931e9b71ee0e4c85390be7a32e`

The Semble adapter converted 20 Requests tasks and skipped none. The Sverklo
adapter converted 25 P1/P2/P4 tasks and explicitly skipped five unsupported P5
name-only tasks. Both generated manifests passed release preflight against the
pinned target repository revisions before evaluation.

## Results

| Corpus / stratum | Tasks | File recall | Candidate file recall | Line recall |
| --- | ---: | ---: | ---: | ---: |
| Semble Requests, all | 20 | 17/20 (85.0%) | 20/20 (100%) | 5/5 (100%) |
| Semble architecture | 7 | 7/7 (100%) | 7/7 (100%) | n/a |
| Semble semantic | 8 | 5/8 (62.5%) | 8/8 (100%) | n/a |
| Semble symbol | 5 | 5/5 (100%) | 5/5 (100%) | 5/5 (100%) |
| Sverklo, all | 25 | 25/52 (48.1%) | 38/52 (73.1%) | 7/43 (16.3%) |
| Sverklo P1 definitions | 10 | 10/10 (100%) | 10/10 (100%) | 5/10 (50.0%) |
| Sverklo P2 references | 10 | 12/15 (80.0%) | 13/15 (86.7%) | 2/33 (6.1%) |
| Sverklo P4 dependencies | 5 | 3/27 (11.1%) | 15/27 (55.6%) | n/a |

Semble returned 22,091 source tokens versus a 105,950-token full-file oracle,
a 79.1% source reduction. Its complete response JSON used 40,994 tokens versus
165,572 for the scripted discovery-plus-oracle envelope, a 75.2% reduction.

Sverklo returned 30,002 source tokens versus a 191,034-token full-file oracle,
an 84.3% source reduction. Its complete response JSON used 49,842 tokens versus
290,602 for the scripted envelope, an 82.8% reduction. These savings are
diagnostic because the oracle reads every labeled file and the P4 labels can
contain large dependency sets.

On this Linux host, Requests indexed 120 files and 340 chunks in 1.70 seconds;
its task-level warm-context median was 89.9 ms and p95 was 147.8 ms. Sverklo
indexed 307 files and 922 chunks in 2.47 seconds; its task-level warm-context
median was 35.6 ms and p95 was 226.6 ms. Timing is host and cache dependent.

## Interpretation

The benchmark separates useful failure owners:

- All three missed Semble semantic files existed in the generated candidate
  set, locating that gap after candidate generation.
- Sverklo P1 file discovery is complete, but exact line coverage is only 50%.
- Sverklo P2 loses two relevant files before selection, one more during
  selection, and most exact reference anchors.
- Sverklo P4 exposes a capability gap: current context retrieval is not a
  transitive dependency-file enumerator. Twelve labeled files never enter the
  candidate set and another twelve are not selected.

The first full run also exposed a benchmark defect: repeated context responses
were byte-compared even though `meta.receipt_id` is intentionally unique. The
harness now removes only that field for determinism checks and retains exact
comparison for ranking, fragments, coverage, omissions, and all other metadata.
A regression test verifies that another metadata change still fails the
comparison.

## Adoption Decision

Adopt the external-corpus lock, adapters, schema-v4 provenance, and benchmark
runner support. They are reproducible and expose distinct candidate-generation,
selection, line-localization, and dependency-graph gaps.

Do not change production ranking from this run. Use these manifests as
diagnostic evidence for separately gated experiments, beginning with the local
semantic lane. Any candidate must preserve exact and symbol behavior, report
results by task family, and pass a separately frozen promotion gate.

The result is not blind evidence. Semble prompt and label generation share a
model provenance, Sverklo uses category-specific line tolerances in its source
methodology, P5 is excluded, and neither corpus measures model task success.
