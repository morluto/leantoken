# OOM release gate

Date: 2026-07-20

Host:

- Linux 6.1.0-50-cloud-amd64 x86_64
- rustc 1.97.0 (2d8144b78 2026-07-07)
- cargo 1.97.0 (c980f4866 2026-06-30)
- release profile

## Incident and same-corpus evidence

Issue #86 recorded a macOS process at 10.3 GB while indexing `$HOME`. The
related Linux incident recorded three OOM kills at 13.6-13.9 GB anonymous RSS
against a 103 GB home directory containing 1,374,445 files. Re-running that
unsafe workload is not part of this gate: the root guard must reject it before
opening the cache or starting discovery.

The comparable measurement uses the same host, sampler, synthetic Rust corpus,
and command arguments as the streaming-publication report:

```bash
indexing_profile \
  --files 7500 \
  --file-bytes 8192 \
  --iterations 1 \
  --read-samples 1 \
  --hot-set 1
```

A shell sampler read `/proc/<pid>/status` every 50 ms and retained the largest
`VmRSS` value.

| Revision | Admitted files | Source bytes | Initial index | Sampled peak RSS |
| --- | ---: | ---: | ---: | ---: |
| `e38693e` collect-all baseline | 7,501 | 61,447,529 | 31.78 s | 143,728 KiB |
| `70565cf` final hardening stack | 7,501 | 61,447,529 | 23.56 s | 80,696 KiB |

The final stack reduced sampled peak RSS by 43.86% and initial-index wall time
by 25.88%. Its schema-v6 diagnostics reported 30 preparation batches, a
256-file high-water, and a 2,097,408-byte source high-water per batch. The
worktree was clean and the report identified revision
`70565cfc8ec22948cc04d2c9eb3c4b1e18fe858a`.

Immediately after the initial commit, logical file sizes were:

| Artifact | Logical bytes |
| --- | ---: |
| SQLite database | 132,452,352 |
| WAL | 133,310,872 |
| SHM | 262,144 |
| Total | 266,025,368 |

Logical size is not allocated disk usage. The WAL value is captured before a
checkpoint so the gate exposes transient publication growth rather than hiding
it behind process teardown.

## Scenario matrix

| Scenario | Automated evidence | Result |
| --- | --- | --- |
| HOME, HOME ancestor, or filesystem root | `config::tests::unsafe_root_policy_rejects_home_and_its_ancestors`, `config::tests::unsafe_root_policy_rejects_a_filesystem_root_without_home_context`, `binary::mcp_rejects_home_root_after_initialize_without_opening_storage` | Initialize completes; no SQLite cache is opened; MCP becomes safely unavailable. |
| Broad override with a low hard limit | `repository::discovery_*_limit_accepts_boundary_and_rejects_*`, `indexer::full_reconcile_limit_error_preserves_the_committed_generation`, `binary::mcp_index_limit_failure_is_terminal_and_does_not_retry` | The first exceeded bound fails loudly, publishes no partial generation, and is not retried after the tree shrinks. |
| Large legitimate repository | Release profile above and the pinned Tokio / near-file-limit profiles in the streaming report | One atomic generation completes with bounded preparation high-water. |
| Initial watcher burst or overflow | `watcher::tests::initial_burst_collapses_to_one_quiet_full_reconciliation`, `watcher::tests::retained_path_state_overflow_becomes_one_sticky_reconciliation`, `watcher::tests::consecutive_full_reconciliations_observe_capped_cooldown` | Burst state is sticky and bounded; follow-up work observes quiet-period and cooldown rules. |
| Leader killed during reconciliation | `binary::mcp_follower_rebuilds_after_leader_is_killed_during_reconciliation`, `storage::tests::streamed_cancellation_rolls_back_every_insert_and_generation`, `storage::tests::later_streamed_storage_failure_rolls_back_earlier_files`, `storage::tests::streamed_panic_rolls_back_and_leaves_storage_reusable` | An abrupt process kill leaves generation 1 intact; the follower publishes generation 2 from that committed baseline. Batch errors and panics roll back earlier inserts. |
| Multiple MCP processes, one root | `binary::concurrent_mcp_startup_initializes_once_and_followers_read` plus the Linux receipt below | One lifetime leader lock holder publishes; all processes read the same committed generation. |
| Multiple roots | `config::default_cache_identity_is_independent_per_repository`, `services::independent_repositories_index_concurrently_without_result_leakage` | Cache identities, locks, generations, and results remain repository-scoped. |

The same-root Linux receipt pre-indexed 1,500 files, initialized three release
MCP processes, waited three seconds, and sampled their steady resident sets.
They used 8,168 KiB, 13,068 KiB, and 12,256 KiB. `fuser` reported exactly one
holder for `index.sqlite.leader.lock`, PID 1076442, whose RSS was 13,068 KiB.
This is a steady-state snapshot, not a startup peak or long-duration soak.

## Fix matrix

| Failure boundary | Change |
| --- | --- |
| #87 hidden/generated discovery | #112 shared ignore and generated-tree policy |
| #88 collect-all publication | #113 bounded preparation with one atomic transaction |
| #89 watcher rescan loop | #115 sticky reconciliation, quiet period, and capped backoff |
| #90 public path leakage | #111 allowlisted MCP error mapping |
| #91 abandoned managed caches | #117 leased list/prune lifecycle |
| #92 unsafe launch and moving package | #105 broad-root refusal and #116 exact launcher pin/refresh |
| #93 missing work bounds | #110 incremental walk/file/byte/depth and batch limits |

## Limits of the conclusion

- Absolute RSS and storage sizes are single-host Linux results. macOS and
  Windows CI exercise correctness, not comparable process-memory sampling.
- The synthetic corpus controls file count and size but not the language,
  parser-output, or directory-shape distribution of every monorepo.
- The 50 ms RSS sampler can miss a shorter peak and is not allocator-level
  accounting.
- Discovery still retains bounded path metadata proportional to admitted
  files. Hard file, byte, walk-entry, and depth limits cap that growth.
- Watcher scheduler tests inject burst and overflow state deterministically;
  they are not a long-running stress test of every operating-system notify
  backend.
- Managed-cache pruning is explicit. There is intentionally no startup auto-GC,
  and a missing repository root alone is not enough to delete an offline cache.
