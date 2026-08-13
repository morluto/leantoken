# OpenClaw storage and retrieval profile

This diagnostic measured LeanToken against a clean checkout of OpenClaw at
`42515c4f07ea3b02e191d30cf97865d4e6229ef0`. The source tree was never modified;
each index lived outside the repository. LeanToken was built in release mode
with Rust 1.95.0 and bundled SQLite 3.53.3.

The run is decision evidence for the measured host, not a portable performance
claim. Cold indexing has one sample per schema and the baseline ran first, so
small indexing differences are not causal evidence. Steady-state context was
also repeated in reverse B/A order because its schema difference was large.
Timings are observations, never test assertions.

## Cold publication owners

The baseline saw 28,560 files (345,389,690 discovered source bytes), indexed
28,186, skipped 374, and published a 1,781,661,696-byte database in 276.36
seconds.

| Phase | Wall time | Linux process write bytes |
| --- | ---: | ---: |
| Parallel file preparation | 100.66 s | not attributed |
| Relational insertion | 30.70 s | 1,092,120,576 |
| Chunk word FTS rebuild | 10.03 s | 76,718,080 |
| Chunk trigram FTS rebuild | 91.18 s | 1,270,714,368 |
| Symbol trigram FTS rebuild | 7.11 s | 21,200,896 |
| Reference trigram FTS rebuild | 25.07 s | 189,181,952 |
| Commit, auto-checkpoint disabled | 3.58 s | 1,725,722,624 |
| Explicit `TRUNCATE` checkpoint | 5.78 s | 1,781,661,696 |

Preparation and insertion happen inside the publication interval, so these rows
must not be summed. The storage phases establish the useful owner order:
preparation and chunk-trigram construction dominate; reference FTS and
relational insertion are secondary; commit and checkpoint do not explain the
261–353 second cold-index range.

SQLite `dbstat` attributed 719,593,472 bytes to chunk trigram FTS, 74,878,976
to chunk word FTS, 95,715,328 to reference FTS, and 18,731,008 to symbol FTS.
GNU `time -v` measured 473.21 user seconds, 41.72 system seconds, 178% average
CPU, 125,108 KiB maximum RSS, and zero swaps for the complete baseline
index-and-query run. Cold indexing is CPU- and write-intensive but not
memory-intensive on this corpus.

A successful SQLite `TRUNCATE` checkpoint reports zero log/checkpoint frame
counts after truncation. The useful checkpoint evidence here is elapsed time,
write bytes, and the zero-byte residual WAL—not the returned frame counters.

### Preparation subphases

A second cold run instrumented each file only in the profiled path. The run was
slower overall than the first sample, so its absolute wall time is not compared
as a regression. Summed worker durations identify the composition of the work;
they overlap across Rayon workers and must not be added to wall time.

| Worker subphase | Summed worker time | Share of 641.54 s |
| --- | ---: | ---: |
| Tree-sitter parse and syntax extraction | 205.72 s | 32.1% |
| Whole-file exact token count | 190.38 s | 29.7% |
| Per-chunk exact token counts | 179.03 s | 27.9% |
| Bounded file reads | 63.07 s | 9.8% |
| UTF-8 classification and chunk construction | 2.19 s | 0.3% |
| Hashing and record projection | 0.49 s | 0.1% |

The 641.54 seconds of summed worker work completed in 170.64 seconds of
preparation wall time, a 3.76× overlap. Preparation already uses the configured
parallelism effectively. Additional parser threads are therefore not the first
move; exact token counting is the largest combined owner at 57.6% of worker
time.

An A/B against the no-allocation counter in the `tiktoken` Rust crate reduced a
full cl100k corpus pass from 36.23–42.87 seconds to 5.89–5.92 seconds with about
70 MB process RSS. It is not compatible: 3,550 of 28,186 files differed,
totaling 17,658 tokens with a maximum per-file error of 1,483. Python
`tiktoken` 0.13.0, used as the canonical oracle over raw file bytes, matched
LeanToken's current `tiktoken-rs` counts on every indexed file. The apparent
alternative is rejected despite its 6.1–7.3× speedup.

The transferable opportunity is narrower: `tiktoken-rs` currently implements
`count_ordinary()` as `encode_ordinary(text).len()`, allocating the complete
token vector. A canonical-compatible no-allocation counter would remove a
measured owner without changing token budgets. This is better pursued upstream
with corpus parity tests than by swapping to an incompatible tokenizer.

## `columnsize=0` A/B

The variant recreated all four empty external-content FTS tables with
`columnsize=0`; production schema and migrations were not changed.

| Metric | Baseline | `columnsize=0` | Delta |
| --- | ---: | ---: | ---: |
| Database bytes | 1,781,661,696 | 1,749,532,672 | −1.80% |
| Cold index wall | 276.36 s | 265.06 s | −4.09% |
| Chunk trigram rebuild | 91.18 s | 87.71 s | −3.81% |
| Reference FTS rebuild | 25.07 s | 17.97 s | −28.31% |
| Complete-process max RSS | 125,108 KiB | 128,360 KiB | +2.60% |
| Complete-process wall | 288.14 s | 298.02 s | +3.43% |

The one ordered cold sample cannot distinguish the 4.09% index change from
cache/order variance. The storage saving is exact and small because
`columnsize=0` removes only each FTS table's `%_docsize` shadow table.

The query regression is decisive and repeated:

| Warm context shape | Baseline p50 | `columnsize=0` p50 | Change |
| --- | ---: | ---: | ---: |
| Realistic task, initial A/B | 1,066.44 ms | 2,682.07 ms | +151.5% |
| Constraint-heavy, initial A/B | 828.94 ms | 2,571.76 ms | +210.3% |
| Known-hash replay, initial A/B | 855.92 ms | 2,577.93 ms | +201.2% |
| Realistic task, reverse B/A | 935.01 ms | 2,533.77 ms | +171.0% |

The diagnostic phase owner is lexical FTS: realistic lexical search measured
459.92 ms versus 2,195.45 ms in the initial A/B and 408.47 ms versus 2,118.49
ms in reverse order. Candidate counts and selected work were unchanged.
LeanToken selects and orders lexical hits with `bm25()`. SQLite documents that
`columnsize=0` makes `xColumnSize()` load and retokenize content on demand, and
BM25 uses that API. The result is a 1.8% disk saving purchased with roughly
2.5–3.1× context latency and about 3× steady-state CPU time. Reject this schema
change.

`detail=none` is not a substitute: SQLite trigram FTS forbids MATCH tokens
longer than three Unicode characters with `detail=none` or `detail=column`.
LeanToken relies on longer phrase MATCH expressions for sound substring
candidate planning and lexical search.

## Regex selectivity experiment

The diagnostic compared the current full mandatory-literal MATCH expression
with a Zoekt-style pair chosen from the two lowest document-frequency mandatory
trigrams. Frequencies came from a temporary FTS5 `fts5vocab(row)` table. Counts
are capped at LeanToken's 10,001-row preflight boundary.

| Shape | Full candidates / query p50 | Rare pair candidates / query p50 | Frequency lookup |
| --- | ---: | ---: | ---: |
| Negative | 0 / 2.75 ms | 0 / 0.12 ms | 37.83 ms |
| Sparse positive | 171 / 10.10 ms | 751 / 0.37 ms | 29.55 ms |
| Common positive | ≥10,001 / 7.83 ms | ≥10,001 / 1.72 ms | 8.03 ms |
| Compound positive | 488 / 30.30 ms | 6,030 / 1.99 ms | 45.50 ms |

The shorter pair makes MATCH itself cheaper, but dynamic SQLite vocabulary
lookups make every measured plan slower before candidate loading or regex
verification. It also expands positive candidate sets by 4.4× and 12.4×.
Zoekt can afford this technique because its search index exposes compact ngram
posting metadata directly. Do not add runtime `fts5vocab` selectivity probes.
A future generation-built frequency side table would need an end-to-end A/B,
including candidate body loads, verification, storage, invalidation, and
publication cost.

## Lexical rank-order experiment

SQLite documents `ORDER BY rank` as potentially faster than
`ORDER BY bm25(table)`, especially with `LIMIT`. The diagnostic compared the
current query, `rank` with the existing deterministic tie-breakers, rank-only
ordering, and a materialized rank-first top-128 followed by bounded hydration.

| Query | Current BM25 p50 | Rank-first hydration p50 | Result set/order/score |
| --- | ---: | ---: | --- |
| `gateway` | 223.99 ms | 153.43 ms | exact |
| `authentication` | 53.92 ms | 68.62 ms | exact |
| `configuration` | 60.13 ms | 91.95 ms | exact |
| `startup` | 55.61 ms | 35.68 ms | exact |

The rank-first form was 31–36% faster for two shapes and 27–53% slower for the
other two. All four happened to preserve the top-128 set, order, and exact score,
but this small matrix is not a recall proof and there is no consistent
performance win. Do not change production ranking. Any future attempt must
preserve hit order, context coverage on the frozen real-repository tasks, and
downstream retrieval calls before CPU or wall time are considered.

## Context batching and reuse

The realistic task generated 280 candidates. Its 186 adaptive excerpts, 200
enclosing-symbol locations, and 24 stored excerpts were all unique within the
request. Lexical search (roughly 0.37–0.46 seconds in the measured baseline)
was much larger than enclosing lookup (6–8 ms), stored excerpt hydration
(under 1 ms), or lexical verification (2–3 ms). Request-wide hydration batching
could reduce statement executions but does not own enough wall time to justify
restructuring the request.

The controlled identical-request replay is intentionally not production
evidence. It produced 2,224 primitive calls over 12 context evaluations and 428
unique generation-scoped keys. There were no immediate repeats; typical repeat
distance was 422 primitive calls, aligned with later repetitions of the same
request shape. The frozen progressive trajectory remains the more realistic
counterweight: 4 exact range rereads among 141 retrieval calls, without
normalized primitive keys. No cross-request LRU is justified. Any future trace
must preserve request order and pinned generation while reporting exact repeat
distance and byte-weighted hit potential.

## Decisions

- Keep stored column sizes and the current atomic generation publication.
- Keep the current full mandatory-literal regex FTS expression.
- Keep request-local fused lexical facts and 32-name exact-symbol batches.
- Keep the canonical-compatible tokenizer and exact budget accounting.
- Keep the current deterministic BM25 query; the rank-first prototype is not a
  general win.
- Do not pursue request-wide context hydration batching until the lexical query
  owner is addressed.
- Do not add a cross-request cache without production-like primitive arrivals.
- Pursue a canonical-compatible no-allocation token counter upstream, guarded
  by all-encoding corpus parity.
- Treat retrieval coverage and avoided follow-up tool calls as the primary
  optimization score. Resource reductions are acceptance constraints, not a
  substitute for accuracy.
