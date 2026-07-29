# Dependency-heavy two-worker cold-index follow-up

Date: 2026-07-29

Decision: keep the production MCP background-indexing default at one worker.
Two workers fail the preregistered p50, p95, and CPU gates. The downstream MCP
contention and multi-process promotion stage was therefore not run.

## Evidence identity

- Corpus: `https://github.com/tile-ai/TileLang.git` at
  `eb31994ad782108d8754b19603b428eca9c1e19d`, with recursive submodules
  initialized.
- LeanToken: clean release build at
  `19599583bf01a963cebfc51b87a1a457a7baaeef`, version `0.1.18`.
- Host: Linux 6.1.0-50-cloud-amd64, x86-64, eight available processors,
  rustc 1.95.0.
- Raw evidence:
  [`dependency-heavy-cold-index-two-worker-follow-up-linux-x86_64-2026-07-29.json`](dependency-heavy-cold-index-two-worker-follow-up-linux-x86_64-2026-07-29.json),
  136,994 bytes, SHA-256
  `8bebb719f60daafe7513bbe1026435f7e5fb4584760f3d1dff256ef2d07cd5de`.
- Generated at 2026-07-29 07:13:13 UTC. The report binds the release
  executable with BLAKE3
  `c08df4751ab96fdd5db760f37d838b9d1c47210de3cf0f42f906444896784568`.

The ignore-visible snapshot contained 34,322 files and 556,046,941 source
bytes. Every completed index had 33,928 admitted files, 484,403,939 indexed
source bytes, 126,011 chunks, 1,392,610 symbols, 632,904 references, and 87,889
imports. Every arm matched logical-index digest
`c111761be1c51dfbb9a6f6af21323aba4ceeb0d6abde95fdc6e71e9ebf40fc21`
and retrieval digest
`98e6f6b4956a452bc6f33f80015472f952836d05e4e1c2771215512c814e2b63`.

## Frozen policy

The schema-v2 follow-up ran eight fresh subprocesses and SQLite caches in
alternating ABBA/BAAB order:

```text
1,2,2,1,2,1,1,2
```

Each arm required four samples. Preparation first had to own at least 35% of
baseline leaf-phase time. Two workers then had to improve both p50 and p95 wall
time by at least 20%, while increasing mean CPU and maximum sampled RSS by no
more than 25%, mean process writes by no more than 5%, and final storage by no
more than 5%. Logical/retrieval parity, every cancellation phase, timeout,
restart, and atomic generation publication were hard gates.

The profiler did not evict operating-system page-cache state. Counterbalancing
limits order bias but does not make this a cold-disk measurement.

## Run samples

| Sequence | Workers | Wall | Preparation | Process CPU | Peak RSS | Process writes |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 0 | 1 | 450.10 s | 239.32 s | 437.84 s | 125,849,600 B | 8,393,785,344 B |
| 1 | 2 | 359.28 s | 182.36 s | 483.04 s | 142,082,048 B | 8,184,610,816 B |
| 2 | 2 | 379.16 s | 189.93 s | 537.63 s | 147,001,344 B | 8,273,985,536 B |
| 3 | 1 | 400.38 s | 224.27 s | 386.96 s | 119,332,864 B | 8,498,012,160 B |
| 4 | 2 | 398.38 s | 195.95 s | 562.67 s | 149,585,920 B | 8,375,042,048 B |
| 5 | 1 | 434.03 s | 224.48 s | 414.25 s | 119,549,952 B | 8,565,395,456 B |
| 6 | 1 | 409.04 s | 234.77 s | 395.70 s | 124,084,224 B | 8,461,787,136 B |
| 7 | 2 | 374.01 s | 187.18 s | 529.33 s | 151,080,960 B | 8,306,704,384 B |

Preparation remained the dominant owner at 54.69% of baseline leaf-phase wall
time, so testing parser concurrency was still correctly targeted. The expanded
sample distribution did not reproduce the original two-sample magnitude:

| Metric | 1 worker | 2 workers | Candidate delta | Gate |
| --- | ---: | ---: | ---: | --- |
| Wall p50 | 409.04 s | 374.01 s | -8.56% | Fail: require at least -20% |
| Wall p95 | 450.10 s | 398.38 s | -11.49% | Fail: require at least -20% |
| Mean CPU | 408.69 s | 528.17 s | +29.24% | Fail: allow at most +25% |
| Peak RSS | 125,849,600 B | 151,080,960 B | +20.05% | Pass |
| Mean writes | 8,479,745,024 B | 8,285,085,696 B | -2.30% | Pass |
| Final storage | 2,425,778,176 B | 2,425,778,176 B | 0.00% | Pass |

The p50 and p95 reductions are material observations on this host, but neither
meets the frozen adoption threshold and the CPU increase independently exceeds
its cap. Faster preparation cannot eliminate the serial SQLite publication
tail.

## Cancellation and restart

All seven target phases were observed. Cancellation before commit left
generation zero and the subsequent rebuild published generation one with exact
logical/retrieval parity. Cancellation observed at commit/checkpoint completed
the already-committed generation and returned
`completed_after_cancellation`; reopening retained generation one and the
follow-up reconciliation completed in 3.00 seconds. This is the intended
post-commit behavior and does not reproduce the old false `Cancelled` outcome.

| Target phase | Cancel-to-return | Result | Generation after attempt | Restart | Restart parity |
| --- | ---: | --- | ---: | ---: | --- |
| Preparation | 0.09 s | Cancelled | 0 | 331.09 s / generation 1 | Match |
| Relational write | 0.01 s | Cancelled | 0 | 381.44 s / generation 1 | Match |
| Chunk word FTS | 7.95 s | Cancelled | 0 | 388.86 s / generation 1 | Match |
| Chunk trigram FTS | 80.54 s | Cancelled | 0 | 375.16 s / generation 1 | Match |
| Symbol FTS | 29.85 s | Cancelled | 0 | 387.53 s / generation 1 | Match |
| Reference FTS | 4.95 s | Cancelled | 0 | 360.82 s / generation 1 | Match |
| Commit/checkpoint | 25.17 s | Completed after cancellation | 1 | 3.00 s / generation 1 | Match |

No attempt exceeded the 7,200-second index timeout or 600-second cancellation
grace.

## Decision

Keep one worker as the default for MCP background indexing. Do not add a
generation-one two-worker policy and do not run the downstream MCP contention
promotion stage: the generation-one candidate already fails three hard gates,
so later evidence cannot make it eligible.

This is retained negative evidence, not a claim that two workers are always
slower. Users who explicitly select a different `--max-index-workers` value
retain that existing control. Four workers remain rejected by the earlier
screening report.

## Limitations

- This is one pinned dependency-heavy corpus on one Linux host.
- Four samples per arm are enough for the preregistered follow-up, not a broad
  workload or cross-platform performance claim.
- Fresh processes and SQLite paths isolate process state, but corpus pages were
  not evicted from the operating-system cache.
- RSS and SQLite main/WAL/SHM values are 25 ms samples and may miss shorter
  peaks. Per-phase CPU is sampled attribution; exact diagnostics remain the
  phase wall-time source of truth.
- The snapshot copies ignore-visible regular files and does not vendor the
  external corpus or preserve Git metadata.
