# Accuracy-first measurement matrix

Use the smallest rows that cover the proposed owner, then add a representative
real-repository run before making a scalability or release claim.

## Experiment record

Bind every saved result to:

- baseline and candidate commit/tree;
- dirty-tree status;
- build profile and compiler/toolchain;
- operating system, architecture, CPU, and available memory;
- corpus identity, revision, visible file count, and source bytes;
- database schema/configuration and tokenizer identity;
- cold/warm state, execution order, iterations, and concurrency;
- exact commands and raw artifact paths.

State the hypothesis and adoption rule before inspecting the candidate result.
Do not edit thresholds after the run.

## Search and regex

Cover:

- negative query;
- sparse-positive query;
- common-positive query;
- compound query with multiple mandatory literals;
- case-sensitive planned path;
- case-insensitive fallback;
- patterns with no sound mandatory literal;
- Unicode and sub-three-character literals;
- invalid pattern;
- zero, one, boundary, and over-limit result counts.

Correctness gates:

- optimized and full-scan membership, coordinates, counts, excerpts, and order;
- identical fallback selection for unsupported plans;
- identical limit preflight and `LimitExceeded` behavior;
- no false negatives from candidate planning;
- compatible case and Unicode semantics.

Diagnostic counters:

- mandatory literals/terms selected;
- FTS candidate IDs;
- full-scan chunks loaded;
- candidate chunks hydrated;
- chunks regex-verified;
- returned occurrences;
- plan/fallback reason.

Measure query planning, FTS lookup, hydration, verification, complete response
latency, CPU, and peak RSS. A cheaper MATCH expression can still lose if it
requires frequency probes or expands positive candidates.

## Context and ranked retrieval

Cover:

- realistic maintenance task;
- constraint-heavy task;
- exact-symbol and broad behavioral tasks;
- small, ordinary, and near-maximum token budgets;
- focus paths/symbols, must-include constraints, exclusions, and diff scope;
- initial request and progressive request with known hashes/generation;
- zero-result, partial-coverage, complete, and truncated responses.

Accuracy gates:

- labeled-file recall and relevant line/range coverage;
- candidate recall before selection;
- relevant-source precision and dead-end source;
- deterministic order and scores;
- exact source-token budget;
- complete serialized response size;
- known-hash resends and two-turn cost;
- downstream repository retrieval calls or a declared proxy.

Diagnostic phases and counters:

- task parsing/facets;
- lexical, symbol, reference, path, import, and diff candidates;
- raw and unique adaptive excerpt requests;
- raw and unique enclosing-symbol requests;
- storage batches and rows hydrated;
- lexical scan and occurrence work;
- fusion, deduplication, allocation, and serialization.

If hydration keys are already unique, batching reduces statement setup but not
row or content work. Require phase evidence before restructuring the request.

LeanToken repository-owned retrieval commands and frozen data are documented in
`benchmarks/README.md` and `docs/measurement.md`. Preserve manifest hashes,
pinned revisions, budgets, and labels when comparing candidates.

## Live reads

Cover:

- shallow and deep line ranges;
- small and near-file-limit inputs;
- complete and token-truncated reads;
- continuation from an exact cursor/byte position;
- expected-hash match and mismatch;
- indexed/live hash match and stale live content;
- UTF-8 boundaries, final line without newline, and out-of-range requests.

Correctness gates:

- exact returned bytes and line coordinates;
- identical hashes and stale/not-modified status;
- identical token counts, truncation, and continuation;
- no reliance on indexed byte anchors until live content identity is proven.

Measure bytes read, scans from byte zero, seeks, tokenizations, wall/CPU, peak
RSS, and allocation/copy volume. Compare page-cache-warm and cold-enough states
separately; do not infer a content cache from warm-read microseconds alone.

For the repository's near-limit synthetic diagnostic:

```bash
cargo run --release --example deep_live_read -- --iterations 100
```

## Indexing and storage

Cover:

- cold initial index;
- unchanged full reconciliation;
- targeted modification;
- create, delete, rename, and ignore visibility change;
- watcher-delivered change;
- near-limit files and large real repository;
- active readers during publication when transaction behavior changes.

Attribute:

- discovery and hashing/plan construction;
- preparation wall and high-water batch size;
- summed worker read, text preparation, hash, parse, tokenization, and
  projection time;
- relational insertion;
- each FTS rebuild independently;
- commit and checkpoint;
- process write bytes;
- database, WAL, SHM, FTS table, and shadow-table bytes;
- user/system CPU, utilization, peak RSS, and swaps.

Worker durations overlap; never add them to wall time. Statement or row counts
are explanations, not latency by themselves. Preserve atomic publication,
generation checks, rollback, and snapshot consistency.

Use the repository-owned profiler in release mode and wrap it with
`/usr/bin/time -v` or the platform equivalent when making CPU or memory claims:

```bash
cargo run --release --example indexing_profile -- \
  --files 5000 \
  --file-bytes 8192 \
  --iterations 20 \
  --read-samples 5000 \
  --output target/indexing_profile_report.json
```

For a real corpus, pass a clean pinned checkout with `--repository`; never write
measurements into the supplied source tree.

## Reuse and caching

Collect ordered, production-like primitive arrivals before designing a cache.
Normalize keys only by semantics already proven equivalent.

Record:

- pinned generation and consistency mode;
- primitive type and normalized key;
- result byte weight, not only entry count;
- exact repeat count and repeat-distance distribution;
- immediate versus later-request reuse;
- invalidation events and generation changes;
- maximum resident bytes under expected concurrency.

Separate controlled identical-request replay from progressive real workflows.
The first proves cacheability; only the second estimates value.

Prefer request-local fused facts before cross-request state. If a cache is
justified, keep it byte-bounded, primitive-level, generation-scoped, and filled
only from complete snapshots. Test invalidation ordering around mutation and
index replacement. Do not cache complete context responses.

## Base/candidate performance

Use the checked-in paired runner when its schema covers the owner. It builds
clean base/head trees, alternates A/B and B/A, checks observable parity, and
retains provenance:

```bash
benchmarks/run_paired_performance.sh \
  BASE_REVISION \
  target/paired-performance \
  10
```

Use more repetitions for a release or design claim as documented in
`benchmarks/README.md`. Do not compare different hosts or incompatible report
schemas. Treat material but statistically uncertain results as inconclusive,
not wins.

## Decision report

Report in this order:

1. **Decision:** adopt, reject, or gather more evidence.
2. **Accuracy:** parity or measured quality change, including bounded failures.
3. **Downstream work:** calls, rereads, resends, source tokens, complete payload.
4. **Performance:** end-to-end and owner phases with sample policy.
5. **Resources:** CPU, RSS, swaps, storage, and writes.
6. **Complexity:** new state, invalidation, concurrency, and maintenance cost.
7. **Limits:** corpus, host, cache state, sample size, and untested cases.

Keep rejected candidates and negative findings in the record when they prevent
future repetition of the same experiment.
