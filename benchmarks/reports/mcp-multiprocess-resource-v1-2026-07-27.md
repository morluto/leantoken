# Stdio MCP multi-process resource profile

Decision: retain the stdio MCP architecture. On this host, the four-process
run stayed below every predeclared threshold, retained exactly one index
leader/watcher, published one cold generation, and recovered from leader
termination with one successor watcher. There is no measured basis in this
experiment to introduce a shared daemon and its authentication, repository
identity, lifecycle, stale-socket, version-skew, crash-recovery, and Windows
support surface.

## Method

The release-mode example generated 200 Rust files with 40 functions each in a
fresh temporary repository for every process count. It launched 1, 2, and 4
stdio MCP processes against the same cache, waited for generation 1, issued one
concurrent cold query plus 10 concurrent warm rounds per process, and sampled
Linux `/proc`, `/proc/locks`, and SQLite while the processes remained live.
For the 2- and 4-process runs it then killed the observed lock owner, changed a
fixture, and required a surviving process to own the lock, create the sole
watcher, and publish generation 2.

The exact release binary, fixture, thresholds, full per-process samples, and
observation limits are recorded in the
[raw artifact](mcp-multiprocess-resource-v1-2026-07-27.json).

## Results

| Processes | Aggregate RSS / peak MiB | Threads / FDs | Estimated read connections | Startup p50 / p95 ms | Cold p50 / p95 ms | Warm p50 / p95 ms | WAL delta bytes | Observed / expected accounting updates | Takeover ms |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 47.27 / 47.27 | 17 / 27 | 8 | 423.70 / 423.70 | 6.01 / 6.01 | 5.16 / 6.61 | 45,320 | 11 / 11 | n/a |
| 2 | 84.83 / 84.85 | 31 / 50 | 16 | 535.97 / 539.02 | 5.13 / 6.55 | 5.43 / 6.23 | 45,320 | 11 / 22 | 299.02 |
| 4 | 165.72 / 165.72 | 59 / 96 | 32 | 627.19 / 711.60 | 4.61 / 4.70 | 4.33 / 5.95 | 45,320 | 11 / 44 | 122.96 |

Every run had one leadership-lock owner, one inotify watcher owned by that
leader, one cold generation publication, and eight established read
connections per process. The constant WAL delta and reduced observed telemetry
updates at 2 and 4 processes reflect best-effort zero-timeout accounting writes
losing SQLite writer races; functional queries all succeeded and generation
publication remained single-owner.

## Threshold decision

| Signal | Observed | Threshold | Result |
| --- | ---: | ---: | --- |
| Incremental RSS per follower | 39.48 MiB | at most 128 MiB | pass |
| Four-process / one-process startup p95 | 1.68× | at most 3× | pass |
| Four-process / one-process warm p95 | 0.90× | at most 3× | pass |
| Normalized WAL bytes per query ratio | 0.25× | at most 3× | pass |
| Established read connections per process | 8 | at most 8 | pass |
| Slowest leader takeover | 299.02 ms | at most 5,000 ms | pass |

The measured cost is process-local memory and read-pool capacity, approximately
39.5 MiB of incremental RSS and eight read connections per additional process.
Neither startup/index contention nor write amplification crossed its stop
condition. A shared daemon should be reconsidered only with representative
evidence that these costs exceed the recorded thresholds.

## Limits

This is a single-host Linux `/proc` observation, not a cross-platform or
population latency claim. Main-database file descriptors minus the writer are a
documented estimate of established read connections. SQLite does not expose
cross-process statement counts, so response-accounting deltas and generation
numbers are the observed write proxies. One orchestrator receives readiness and
query responses in process order, which can add bounded client-side delay to
later processes. Cold p95 has only one sample per process; warm p95 has 10
samples per process. Compare regressions only on the same host and release
build, and keep contrary reruns rather than retuning the frozen thresholds.
