# MCP retrieval concurrency profile, 2026-07-26

## Decision

Keep the initial execution default at eight running closures, with eight queued
operations and 16 independently admitted MCP tool calls. Do not add execution
lanes, reconciliation coalescing, a dedicated blocking runtime, an adaptive
limiter, or a shared daemon from this run.

The large-repository result identifies the existing execution bound as useful
backpressure rather than an arbitrary SQLite restriction. At mixed concurrency
8, complete-request p50/p95 was 264/1,388 ms. At concurrency 16 it was
374/2,718 ms, while queue p95 rose from 162 microseconds to 294 ms. SQLite reader
checkout p95 remained 32 microseconds or less in mixed traffic and active
snapshots peaked at eight. Raising only the pool or worker count would
therefore move the bound into more simultaneous CPU and request memory without
evidence of a connection checkout bottleneck.

At concurrency 32, the active bound rejected 32 of 64 mixed calls immediately.
The accepted calls completed without a queue timeout and the complete-request
p95 was 2,184 ms. Under concurrent targeted indexing, 32 calls were rejected
and 32 completed without a queue timeout; at concurrency 16, three admitted
calls reached the 500 ms queue timeout. This is the intended bounded-overload
behavior; fail-fast responses make the p50 at concurrency 32 look artificially
low, so p95 and outcome counts must be read together.

WAL size stayed at its 16 MiB journal limit throughout every large-repository
scenario. Passive checkpoints returned a busy status of zero. Peak sampled RSS rose
from 148 MiB at mixed concurrency 1 to 251 MiB at mixed concurrency 32 and
251 MiB with concurrent indexing. The process used at most eight running
blocking closures and eight active SQLite snapshots. The busiest scenario used
10 distinct Tokio blocking threads to execute closures over its lifetime.

Steady mixed traffic and cancellation storms had zero normalized response,
order, generation, or token-accounting differences at every concurrency.
Concurrent indexing had zero order and generation differences. Nineteen
responses differed from the idle baseline only where the observable freshness
state changed from `current` to `reconciling`; the serialized token-accounting
fields changed with that response metadata as expected.

## Method

- Candidate tree: working tree based on
  `7d26d3606f83d2af9930b1b24132b91aae11bfae`
- Rust: `rustc 1.95.0 (59807616e 2026-04-14)`
- Host: Linux 6.8, four-core DO-Premium-AMD, 15 GiB RAM
- Small repository: generated 64-file Rust fixture, 963 KiB SQLite index
- Large repository: OpenClaw at
  `42515c4f07ea3b02e191d30cf97865d4e6229ef0`, 28,186 indexed files,
  1.66 GiB SQLite index
- Load levels: 1, 2, 4, 8, 16, 32
- Scenarios: mixed requests, cancellation storms, and targeted indexing
  concurrent with retrieval
- Executor defaults: eight running, 16 active, 500 ms queue timeout

The full 61 KiB machine-readable result was written to
`target/concurrency-profile-2026-07-26.json`. It is intentionally ignored:
latency, CPU, RSS, and allocator high-water behavior are same-host evidence,
not portable product constants.

## Phase 5 gates

The run does not isolate different database, filesystem, or Git saturation
owners, so lane splitting would be speculative. This original matrix submitted
only one targeted reconciliation at a time. A subsequent deterministic wave
admission proof demonstrated that concurrent `reconcile_working_tree` callers
otherwise submit redundant scans serialized by the repository operation lock;
the Services-owned coordinator now coalesces callers before scan start and
assigns later callers to one pending freshness wave. Queue growth follows
actual retrieval load rather than unrelated Tokio blocking work, so a dedicated
runtime has no measured owner. The sample count and workload classification are
insufficient for an adaptive controller. Finally, this is a single-process
experiment and provides no evidence that process multiplication owns resource
growth, so a shared daemon remains out of scope.
