# ARB orientation capsule v1

Date: 2026-07-27

## Decision

Keep the bounded orientation-capsule contract as benchmark infrastructure and
a candidate handoff shape. Do not add it to production context responses until
a trajectory experiment shows that agents follow the route and avoid more
downstream retrieval tokens than the capsule costs.

On the two-task smoke set, one 46-token Clap capsule and one 33-token Click
capsule each named a labeled owner path. Selected source, dead-end source, and
two-turn context cost were unchanged because capsules remain separate routing
artifacts rather than being counted as retrieved source.

## Frozen inputs

- Dataset: two-task local ARB v2 trace2code smoke adapter output.
- Manifest BLAKE3:
  `888fd766be72d8831946cf0038cf39374b5480564088f8d2cf7aa8d553bb6a7f`.
- Harness revision:
  `e2bd35083fde002d1f2cb3014a768b5fef7c639b`.
- Baseline: `--workflow-evidence --ast-structural-lane`.
- Candidate:
  `--workflow-evidence --ast-structural-lane --orientation-capsule`.
- No corpus, repository, model, embedding, SCIP, or history download was
  performed.

Both arms used the same revision and already-local pinned Clap and Click
checkouts. The dirty-worktree bit includes the report files and unrelated
user-owned untracked files present while the artifacts were generated.

## Contract and bounds

The capsule reuses AST lane results and therefore adds no parser pass, indexed
search, repository scan, Git subprocess, or storage write. Per task it retains:

- at most one owner path;
- at most four matched trace terms;
- at most four indexed definition names;
- at most 128 exact serialized tokens.

If the artifact exceeds the limit, definitions and then terms are dropped
deterministically. A route that still cannot fit is reported unavailable. Gold
labels are applied only after construction to measure path relevance.

Capsules do not enter `ContextResponse`, selected fragments, source-token
accounting, known-hash receipts, or the context token budget. This prevents a
path-only route from being misreported as decisive source evidence.

## Paired result

| Metric | Baseline | Candidate | Delta |
| --- | ---: | ---: | ---: |
| Selected labeled files | 1 / 3 | 1 / 3 | 0 |
| Source tokens | 1,255 | 1,255 | 0 |
| Dead-end source tokens | 1,054 | 1,054 | 0 |
| Complete response JSON tokens | 2,698 | 2,698 | 0 |
| Two-turn context JSON tokens | 7,729 | 7,729 | 0 |
| Capsule paths | 0 | 2 | +2 |
| Relevant capsule paths | 0 | 2 | +2 |
| Capsule path relevance | n/a | 100% | n/a |
| Capsule payload tokens | 0 | 79 | +79 |

The capsule token count is reported separately from context response tokens.
It is a real prospective handoff cost, not free savings.

## Next gate

Attach the capsule to the existing trajectory/context-utilization harness and
freeze a small paired agent run. Promotion requires observable follow-up on the
named owner plus lower total retrieval cost or fewer dead-end calls. Path
relevance alone is insufficient, and this two-task diagnostic is not a
generalization claim.

