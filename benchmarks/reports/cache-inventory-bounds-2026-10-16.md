# Cache inventory resource diagnostic, 2026-10-16

This records the resource envelope for issue #540. The adoption requirement is
correct, typed refusal at independent inventory limits, with destructive prune
refusing incomplete initial inventory. There is no latency or memory improvement
threshold and no baseline speedup claim.

The measured native binary was built in the debug profile with Rust 1.95.0
(`59807616e`), on Linux x86_64 with 16 logical CPUs. Its SHA-256 is recorded in
the [raw observations](cache-inventory-bounds-2026-10-16.jsonl). The source was
`0ebe4478b9cd9edde54a9533b8a1585ded991844` plus the busy-timeout/cancellation
follow-up. SHA-256 of `git diff HEAD -- src/ Cargo.toml Cargo.lock` at measurement
was `b4d23b704207e36701cdf29c248e770bf96a12d0de6dff697250824349573736`.
Test-only refactoring after measurement does not change the measured binary.

Reproduce on Linux with GNU time and Python 3.11+:

```sh
python3 scripts/measure_cache_inventory.py /path/to/leantoken --build-profile debug
```

The script creates 64 independent caches, each indexing one 31-byte Rust source
with the estimate tokenizer. HOME, data/config/cache roots and temporary paths
are disposable. Populations grow in ascending order; each measurement is one
sequential, warm-cache `--json cache list --limit 1` process. GNU time measures
peak process RSS. Linux inotify watches each database and counts file opens,
checks event overflow, and verifies one observed open per database; this is a
file-open observation rather than a SQLite connection hook. Python measures
whole subprocess wall time, including process launch and GNU time overhead.

| Caches | Database file opens | Peak RSS (KiB) | Wall time (seconds) |
| --- | --- | --- | --- |
| 1 | 1 | 26,756 | 0.0178 |
| 16 | 16 | 26,812 | 0.0427 |
| 64 | 64 | 27,304 | 0.1204 |

Adding unmanaged root entries to reach 4,097 produces `incomplete_inventory`,
`complete: false`, dimension `root_entries`, observed 4,097 and limit 4,096.
The surrounding resource guard observed a 0.05 GiB peak process-group RSS and
at least 49.15 GiB host memory available during the complete fixture run.

These are small synthetic debug-build observations, not release latency,
maximum-population RSS, cold-storage measurements or a whole-process hard heap
guarantee. Page size still does not reduce work below the admitted population.
Independent counter/byte/SQLite/elapsed limits, cooperative cancellation and
failure before destructive selection are established by behavioral regressions,
including injected small quotas, a locked database and cancellation after an
actual removal. Filesystem scheduling and SQLite internals remain outside a
hard wall-clock/heap guarantee.
