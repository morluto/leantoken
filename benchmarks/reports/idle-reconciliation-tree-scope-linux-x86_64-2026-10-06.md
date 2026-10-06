# Large-tree reconciliation scope on Linux x86-64

The pinned native release-profile build spends most of its measured warm reconciliation time
walking admitted empty directories. Moving the same directory population under
the existing generated `target` exclusion removes that repeated discovery work,
even though native recursive-watch admission still reaches its raw directory cap.
This is a diagnostic characterization for #603, not a product optimization.

The [machine-readable report](idle-reconciliation-tree-scope-linux-x86_64-2026-10-06.json)
retains raw process observations, all reconciliation wall phases, discovery counts,
admission diagnostics, memory snapshots and checked shutdown metadata.

## Pinned environment and workload

- Native source: [7c05cd05b93c6e1dddbb8a6c9c712f856a8f3516](https://github.com/morluto/leantoken/commit/7c05cd05b93c6e1dddbb8a6c9c712f856a8f3516),
  retained by `measurement/idle-tree-source-7c05cd05`. This is an unmerged
  experimental revision built from a clean tree with `cargo build --locked --release`,
  without `cfg(test)`; it is not a published product release. Fetch the retained
  branch before checking out the commit to reproduce the source. Relative to main
  `622340e396eaca37778cf2107972c0e9ad01bab4`, its only changes are in
  `src/services/context/facets.rs` and `tests/services/context_regressions.rs`.
  Outline, tokenizer, watcher and indexing code is identical. Binary SHA-256:
  `3af5cd2a537498078e095b8815ee25a6b73c14a03a6948c5558335ff6211fd91`.
- Disposable Linux container: one CPU quota, 3 GiB memory, no network, one
  index worker, one native MCP process at a time, isolated HOME/cache.
  The host MCP installation/configuration was unchanged.
- Each case has one 35-byte Rust file, initially
  `pub fn original() -> usize { 111 }\n`, and 50,001 empty child directories.
  The admitted case has 50,002 physical directories including its repository root;
  the generated case has 50,003 including the additional `target` parent.
- Both reach `admission_directory_limit`, with an **incomplete** observed prefix
  of 50,002 entries / 50,001 directories. These counts are not tree populations.
- After generation one and watcher initialization, observe at least 31 seconds
  without queries or changes. Record actual elapsed time and the completed poll.
- Run five outline calls per arm in working-tree / indexed / indexed / working-tree
  order. Arguments are `paths: ["file.rs"]`, `max_results: 10`, `max_tokens: 1000`
  and explicit `consistency: "reconcile_working_tree"` or `"indexed_generation"`.
  Preserve both consistency contracts; compare structural payloads separately
  from mode-specific metadata and verify the exactness flag and declared accounting component sum.
- Change the file to `pub fn modified() -> usize { 222 }\n`, preserve its length,
  restore its original mtime in nanoseconds, and wait at most 40 seconds for
  automatic generation two. Verify the new outline, then close stdin.

## Observations

| Observation | Admitted empty directories | Generated `target` control |
| --- | ---: | ---: |
| Idle wall seconds | 31.004 | 31.003 |
| Idle process CPU seconds | 1.93 | 0 measured ticks |
| Idle share of one core | 6.225% | Below 10 ms CPU resolution |
| Idle reconciliation wall ms | 1,933.462 | 0.737 |
| Discovery wall ms | 1,932.916 | 0.325 |
| Hash/plan wall ms | 0.257 | 0.160 |
| Walked entries per reconciliation | 50,003 | 2 |
| Admitted source files / bytes | 1 / 35 | 1 / 35 |
| Idle Rss / PSS / private KiB | 45,436 / 44,462 / 43,492 | 45,572 / 44,622 / 43,676 |
| Working-tree five-call CPU seconds, first/last arm | 9.09 / 9.22 | 0.03 / 0.02 |
| Working-tree latency median ms, first/last arm | 1,834.47 / 1,871.01 | 11.10 / 12.21 |
| Indexed five-call CPU seconds, first/last arm | 0.02 / 0.02 | 0.01 / 0.03 |
| Indexed latency median ms, first/last arm | 10.21 / 9.27 | 11.44 / 11.20 |
| Automatic generation-two observation seconds | 9.970 | 29.070 |
| EOF exit code | 0 | 0 |

All 40 unchanged outline calls pass structural payload parity, the exact-accounting
flag and the declared component-sum checks. A separate complete-wire BPE oracle
was not run. Their generation remains one. Both automatic freshness probes pass;
both sessions exit cleanly with no pending or unanswered request IDs.

Discovery accounts for nearly all measured no-op reconciliation **wall time** in
the admitted case. Total process CPU also tracks that work. Phase CPU was not
measured separately. Both warm idle windows report zero physical `read_bytes`;
`rchar` rises by 12,323 in each, but neither counter represents all directory
metadata or `getdents` work.

## Implications and limits

Repeated working-tree queries can produce sustained CPU on a large admitted tree,
beyond the bounded background poll's duty. Indexed-generation queries use the
latest published snapshot and avoid a query-triggered reconciliation; they do
not promise a fresh working-tree scan. This comparison preserves that distinction
and does not change defaults, polling intervals, timestamp trust or watch bounds.

The generated control shows that a raw admission fallback does not itself imply
expensive ongoing discovery. Operators can keep irrelevant generated trees outside
the effective indexing scope, using existing exclusions. Before choosing a code
optimization, measure the admitted tree and query schedule rather than inferring
cost from physical directory counts or an unchanged generation alone.

Each case has one idle window and a tiny admitted source corpus. This is not a
source-heavy hashing study, an allocation-owner attribution, a multi-session
memory reduction, or a long-duration leak test. The differently timed freshness
observations must not be treated as a before/after latency improvement. The older
host's approximately 103% CPU snapshot still lacks captured client traffic; this
experiment does not establish its cause. Remaining #603 optimization work stays
open. The pending language-role review at this pinned source does not participate
in the outline, watcher or indexing operations measured here.
