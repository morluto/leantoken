# Dependency-heavy cold-index decision

Date: 2026-07-28

Decision: keep the production default unchanged. Two preparation workers pass
the preregistered matrix thresholds and are eligible for a narrower follow-up
experiment. Four workers are rejected because their CPU and peak-RSS increases
exceed the frozen limits.

## Evidence identity

- Corpus: `https://github.com/tile-ai/TileLang.git` at
  `eb31994ad782108d8754b19603b428eca9c1e19d`, with recursive submodules
  initialized.
- LeanToken: clean release build at
  `cb6b6dbffb4b31fd2ecf74dc61d936242cb69fa4`, version `0.1.17`.
- Host: Linux 6.1.0-50-cloud-amd64, x86-64, eight available processors,
  rustc 1.95.0.
- Raw evidence:
  [`dependency-heavy-cold-index-tilelang-linux-x86_64-2026-07-28.json`](dependency-heavy-cold-index-tilelang-linux-x86_64-2026-07-28.json),
  115,048 bytes, SHA-256
  `732bff9d9b9ae38105db938b5111b1506a9d0e6cfe44552a64d3a4b791ba4bc3`.
- Generated at 2026-07-28 16:16:00 UTC. The report binds the release
  executable with BLAKE3
  `c8a567ec2364074c789948c86272a6febdf8ac66ad2f965184bb358d195aa031`.

The exact matrix used worker order `1,2,4,4,2,1`, a 25 ms resource sample
interval, a 7,200 second per-index timeout, and parity queries `TODO`, `class`,
`matmul`, `kernel`, `tvm`, and `TileLang`. Every arm and cancellation probe ran
in a fresh subprocess against a fresh SQLite path.

## Corpus and parity

The snapshot contained 34,322 regular files and 556,046,941 bytes at a maximum
directory depth of 14. LeanToken admitted 33,928 files and 484,403,939 source
bytes, producing:

| Object | Count |
| --- | ---: |
| Chunks | 126,011 |
| Symbols | 1,392,610 |
| References | 632,904 |
| Imports | 87,889 |

All six arms produced the same logical-table digest
`2b10e58011bfa8f571e0bc659c1dcc94f8fac2c763377f208e5190e71a1102b6`
and retrieval digest
`98e6f6b4956a452bc6f33f80015472f952836d05e4e1c2771215512c814e2b63`.
Response parity is complete.

## Worker matrix

The policy was frozen before the run: preparation must own at least 35% of
leaf-phase time; a candidate must improve median wall time by at least 20%
while increasing mean CPU and maximum sampled RSS by no more than 25%, and
mean process writes and final footprint by no more than 5%.

| Workers | Samples | Wall p50 | Wall p95 | Mean CPU | Peak RSS | Mean writes | Final storage |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 2 | 448.65 s | 448.77 s | 427.90 s | 123,265,024 B | 8,416,301,056 B | 2,425,778,176 B |
| 2 | 2 | 337.06 s | 416.05 s | 524.07 s | 138,682,368 B | 8,410,896,384 B | 2,425,778,176 B |
| 4 | 2 | 297.23 s | 315.78 s | 591.71 s | 185,716,736 B | 8,154,937,344 B | 2,425,778,176 B |

Relative to one worker:

| Workers | Wall reduction | CPU increase | Peak-RSS increase | Write increase | Footprint increase | Gate |
| ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 2 | 24.87% | 22.47% | 12.51% | -0.06% | 0.00% | Pass |
| 4 | 33.75% | 38.28% | 50.66% | -3.11% | 0.00% | Fail |

The two-worker samples were 337.06 seconds and 416.05 seconds. With only two
samples, nearest-rank p50 is the lower observation and p95 is the higher one.
That spread is why this result admits a follow-up experiment rather than a
production-default change.

## Work owners

Preparation owned 55.86% of the baseline leaf-phase wall time, satisfying the
prerequisite for testing parallel preparation. Across the two one-worker
samples, preparation took 242.18 and 257.43 seconds. The mean accumulated
worker time was 249.69 seconds:

| Preparation component | Mean accumulated worker time |
| --- | ---: |
| Parsing | 99.25 s |
| Source token counting | 75.88 s |
| Chunk token counting | 70.16 s |
| File reads | 2.46 s |
| Text preparation | 1.60 s |
| Hashing and projection | 0.26 s |

HTML and C++ were the dominant language owners. On the one-worker arms they
averaged 108.63 seconds and 88.65 seconds of accumulated preparation work,
respectively; Python averaged 23.09 seconds and unknown/non-source files
18.66 seconds.

Publication is also substantial but does not scale with preparation workers.
The one-worker arms averaged 87.34 seconds rebuilding the chunk-trigram FTS
index, 33.82 seconds rebuilding symbol FTS, 30.12 seconds writing relational
rows, and 21.70 seconds checkpointing. Every arm reached a sampled
main/WAL/SHM high-water of 4,861,151,408 bytes before settling at
2,425,778,176 bytes. Process writes ranged from 8.11 to 8.44 GB. These figures
make FTS write amplification and peak WAL footprint separate optimization
candidates; they do not justify weakening atomic publication.

## Cancellation probes

Every requested phase was observed, no probe timed out or exceeded its grace,
and rebuilding the same database path matched the baseline digests.

| Requested phase | Return latency after request | Result | Generation after attempt | Restart wall | Restart parity |
| --- | ---: | --- | ---: | ---: | --- |
| Preparation | 0.12 s | Cancelled | 0 | 272.37 s | Match |
| Relational write | 0.02 s | Cancelled | 0 | 282.47 s | Match |
| Chunk word FTS | 146.42 s | Cancelled | 1 | 2.89 s | Match |
| Chunk trigram FTS | 148.46 s | Cancelled | 1 | 2.78 s | Match |
| Symbol FTS | 62.49 s | Cancelled | 1 | 2.87 s | Match |
| Reference FTS | 37.27 s | Cancelled | 1 | 6.00 s | Match |
| Commit and checkpoint | 26.77 s | Cancelled | 1 | 2.77 s | Match |

Cancellation during preparation or relational insertion leaves generation
zero and restarts as a full build. Cancellation requested from an FTS phase
onward returns `cancelled` only after a complete generation-one index has been
published. Atomicity and retrieval correctness held, but reporting cancellation
after publication is an observable contract problem. It should be fixed
separately by defining the commit point precisely, adding bounded cancellation
checks between publication phases, and never reporting cancellation after a
successful commit.

## Decision and next experiment

No production configuration changes in this pull request.

The smallest performance follow-up is a generation-one-only two-worker
prototype, retaining one worker for warm reconciliation. It should repeat at
least six samples in mirrored order and include normal MCP contention and
multi-process workloads. It advances only if full response and index digests
remain identical, p50 and p95 both improve materially, and the existing CPU,
RSS, write, footprint, timeout, and cancellation gates still pass.

Before adopting that prototype, fix and regression-test the post-publication
cancellation result described above. A later first-party-versus-dependency
scope experiment may determine whether dependency indexing should remain
default behavior, but this corpus does not answer that product question.

## Limitations

- This is one pinned dependency-heavy corpus on one Linux host.
- Fresh processes and SQLite paths isolate process initialization and database
  state, but the run did not evict corpus data from the operating-system page
  cache. Mirrored ordering counterbalances order effects; this is not a
  cold-disk measurement.
- RSS and SQLite main/WAL/SHM values are 25 ms samples and may miss shorter
  peaks. Per-phase CPU is sampled attribution; exact diagnostic wall time is
  the phase-owner evidence.
- Two samples per worker count are enough to execute the preregistered screen,
  not to establish a stable production latency distribution.
- The snapshot copies ignore-visible regular files and does not vendor the
  external corpus or its Git metadata.
