---
name: optimize-accuracy-first
description: Diagnose, design, implement, and validate system optimizations while treating correctness, retrieval quality, bounded behavior, and avoided downstream work as primary outcomes. Use for performance, latency, CPU, memory, storage, indexing, search, retrieval, caching, batching, concurrency, tokenizer, database, or scalability work; for evaluating a proposed “faster” implementation; or for deciding whether an optimization is worthwhile on representative workloads.
---

# Optimize Accuracy First

Optimize the work required to produce a correct, useful result. Do not optimize
one local timer while making callers search again, consume worse evidence, or
lose a correctness guarantee.

This skill is self-contained. Use repository source, version control, the
project's normal build and test commands, checked-in benchmarks, and ordinary OS
measurements. Do not require an optional MCP server, hosted research service, or
external workflow product. If an optional facility is unavailable, continue
with the repository-owned workflow.

Read [references/measurement-matrix.md](references/measurement-matrix.md) before
designing or running an experiment.

## Define the actual outcome

Write one falsifiable optimization claim before changing code:

> For workload **W**, change **C** should reduce **owner O** and improve
> **metric M**, while preserving **invariants I**.

Name the caller-visible outcome. “Reduce SQL statements” is not enough if the
same rows still dominate. “Use less CPU” is not enough if retrieval misses cause
another request. Prefer claims such as:

- reduce verified regex chunks without changing matches or bounded failures;
- improve relevant-source coverage within the same exact token budget;
- remove repeated content scans while preserving excerpts, ordering, and scores;
- reduce cold publication writes without weakening atomic generations;
- reduce deep-read work without changing live-file staleness detection.

Classify the request before acting:

- For **diagnose**, locate and explain the owner; do not implement.
- For **research or compare**, produce a decision with evidence; do not mutate.
- For **change or optimize**, implement the narrowest supported candidate and
  validate it.

## Establish non-negotiable invariants

Record the applicable invariants before measuring. Include more than happy-path
result equality:

- result membership, coordinates, ordering, scores, and occurrence counts;
- exact token accounting, truncation, continuation, and saturation semantics;
- case, Unicode, regex, parser, and tokenizer semantics;
- error categories, limits, cancellation, and `LimitExceeded` behavior;
- deterministic output and stable tie-breaking;
- live-content freshness, expected-hash behavior, and snapshot consistency;
- atomic publication, generation invalidation, and bounded memory;
- protocol shape and downstream caller expectations.

Treat a changed invariant as a product change, not a performance optimization.
Stop and surface it unless the user explicitly authorizes that tradeoff.

## Measure total work

Keep three scorecards. Never collapse them into one unexplained number.

1. **Accuracy and usefulness**
   - exact result parity where semantics should not change;
   - relevant-file and relevant-range coverage for ranked retrieval;
   - precision, dead-end source, ordering, and exact budget compliance;
   - complete, deterministic, bounded responses;
   - downstream retrieval calls, rereads, retries, and resends.

2. **Time and compute**
   - end-to-end wall time plus p50 and p95 distributions;
   - user CPU, system CPU, average utilization, and wait time where available;
   - phase durations and candidate/work counts;
   - allocation or copied-byte evidence when it identifies an owner.

3. **Resource and operational cost**
   - peak RSS, retained memory, swaps, and concurrency multiplier;
   - database, WAL, index, and cache bytes;
   - process write bytes and write amplification;
   - cold versus warm behavior, startup/indexing cost, and invalidation cost.

Accuracy can be the best performance optimization. A candidate that retrieves
the right evidence in one request may dominate a locally faster candidate that
causes another tool call. Conversely, do not claim avoided downstream work
without measuring it or a direct retrieval-quality proxy.

## Locate the owner before designing

Inspect the complete request path from input to observable output. Add
diagnostic phase and candidate counters at stable boundaries when ownership is
unclear. Keep these counters deterministic; do not add timing assertions to
tests.

Prefer counters such as:

- input rows, planned candidates, loaded candidates, verified candidates;
- raw versus unique hydration keys and batch counts;
- scans, tokenizations, parses, allocations, and bytes processed;
- cacheable primitive keys, repeat distance, and byte-weighted reuse;
- per-index rebuild rows/bytes and transaction/checkpoint work.

Use phase timing to find an owner, then use counts to explain it. Do not infer
an optimization from statement count, thread count, or a profiler sample alone.

## Form candidate explanations

Generate at least two plausible explanations when the design is uncertain.
Favor the smallest layer that owns the work:

1. eliminate duplicated work within one request;
2. route exact operations to exact indexed queries;
3. prefilter candidates with a sound necessary condition, then verify fully;
4. batch only when duplicated round trips or query setup are material;
5. reuse generation-scoped primitives only after real arrival traces show value;
6. change representation only with oracle-level parity;
7. add concurrency only after serialized work is the measured owner;
8. replace architecture only when the current ownership boundary cannot meet
   the measured requirement.

Use the database for bounded indexed filtering and joins, the application for
domain policy, and the existing libraries for tokenization, pooling, parsing,
and statement caching unless evidence shows that boundary is the problem.

Prototype risky candidates in a diagnostic-only path first. Measure the whole
path, including planning overhead, candidate expansion, hydration, verification,
publication, and invalidation.

## Build a representative experiment

Use the repository's existing benchmark harness before creating a new one.
Match the corpus, request shape, cache state, build profile, and concurrency to
the claim.

Always:

- run performance work in release mode;
- bind results to the exact source tree, configuration, corpus revision,
  tokenizer, and host;
- compare base and candidate on the same host;
- alternate A/B and B/A order when cache or thermal order can matter;
- retain raw reports and negative results;
- use deterministic behavioral assertions for correctness;
- treat elapsed time as measured evidence, not a unit-test threshold.

Use a tiny synthetic corpus to debug mechanics, never as scalability proof.
Use a large real repository to expose candidate growth, indexing, memory, and
write cost. Use frozen labeled tasks when changing retrieval or ranking.

## Prove correctness before interpreting speed

Build a differential oracle whenever the candidate has a behavior-preserving
fallback:

1. Run the baseline/full path and optimized path on the same input.
2. Compare the complete observable result, including order and metadata.
3. Cover positive, negative, boundary, Unicode/case, truncated, stale, and
   bounded-failure shapes.
4. Force both planned and fallback branches.
5. Confirm the regression proof would fail on the base for the intended reason
   when fixing a defect.

For a faster tokenizer, parser, ranker, or representation, compare against the
canonical implementation over a representative corpus. Aggregate equality is
insufficient; compare per-file or per-request outputs so offsetting errors
cannot hide.

## Interpret the A/B

Apply this decision order:

1. Reject unapproved correctness, accuracy, or boundedness regressions,
   regardless of speed.
2. Prefer a measured accuracy improvement when its resource cost remains within
   stated bounds and it reduces likely downstream work.
3. For behavior parity, require a reproducible, material improvement in total
   work—not only one micro-phase.
4. Mark inconsistent, order-sensitive, or underpowered results inconclusive.
5. Keep the current design when the proposed complexity is larger than the
   measured benefit.

Report effect sizes, raw counts, sample policy, and uncertainty. Do not transfer
speedup ratios from another repository, corpus, machine, or storage engine.

## Avoid recurring traps

Do not:

- weaken Unicode, case, regex, limit, or token semantics for a fast path;
- accept a faster tokenizer without canonical corpus parity;
- optimize a cheap phase because it executes many statements;
- add a cache from identical-request replay alone;
- cache complete responses whose inputs contain budgets, hashes, generations,
  consistency modes, or constraints;
- perform dynamic selectivity probes without charging their lookup and expanded
  candidate cost;
- remove index metadata without measuring query-time retokenization;
- add parser or worker threads before measuring overlap and serialized owners;
- split an atomic generation to reduce WAL or commit size;
- use p50 alone, debug timings, one ordered run, or timing assertions;
- tune prompts, labels, thresholds, or corpora after seeing the candidate;
- present retrieval fixtures as end-to-end task-success evidence.

## Close the work

For an implemented candidate:

1. Keep independent concerns in separate changes.
2. Remove diagnostic scaffolding that is not intentionally retained.
3. Preserve phase/candidate counters that provide stable regression evidence.
4. Run focused behavioral and differential tests.
5. Run the repository's normal full validation on the final tree.
6. Record the representative A/B only when the exact final tree was measured.

Lead the handoff with the decision: adopt, reject, or gather more evidence.
State the accuracy result first, then downstream work, wall/CPU, memory, storage,
and limitations. A well-measured rejection is a successful optimization result.
