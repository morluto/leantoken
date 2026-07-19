# LeanToken index and retrieval design plan

Goal: [goal.md](goal.md)

## Phase 1: Reproducible baseline and design contracts

Status: complete

### Implementation

- [x] Inventory existing hot-path bounds, snapshot invariants, cursor contracts, import behavior, and ranking semantics.
- [x] Record the repository/agent concurrency contract: per-repository cache and failure isolation; same-repository shared generations, single watcher leadership, and request-local read snapshots.
- [x] Bind service-managed database metadata to the canonical repository root and reject explicit database reuse by a different root.
- [x] Add focused measurement support for connection setup, reconciliation scaling, dedup distributions, and tree pages without changing production responses; bounded batch methods make hydration statement count independent of hit count.
- [x] Record baseline query plans and release measurements on the smallest representative corpora.

### Verification

- [x] Existing focused storage, services, ranking, indexer, and files tests pass unchanged.
- [x] Concurrent isolation tests cover two independent repositories, two services for one repository, and a mismatched explicit database/root pair.
- [x] Baseline reports identify corpus, build profile, sample count, and relevant caps.

### Exit criteria

- [x] Each later design decision has a reproducible failing or scaling signal and a behavior-preservation test surface.
- [x] Repository ownership and agent-sharing semantics are enforced before performance changes can introduce shared capacity.

## Phase 2: Batched retrieval read model

Status: complete

### Implementation

- [x] Accumulate bounded structural/lexical hit sets before content hydration.
- [x] Add repository-specific batched SQLite queries for enclosing symbols and overlapping chunks using JSON table-valued input.
- [x] Reuse prepared statements within and across pooled request sessions.
- [x] Migrate context and search assembly without changing ranking signals, excerpts, ordering, hashes, or token accounting.

### Verification

- [x] Unit/integration tests preserve observable results across symbols, references, lexical hits, duplicate ranges, missing chunks, and cancellation.
- [x] `EXPLAIN QUERY PLAN` uses the intended file/range indexes.
- [x] Batch structure bounds statements by query/channel rather than candidate count; release context p50/p95 is recorded below.

### Exit criteria

- [x] Per-hit SQLite hydration fan-out is removed and retrieval behavior remains equivalent.

## Phase 3: Explicit changes and reverse import projection

Status: complete

### Implementation

- [x] Add explicit created/modified/deleted/visibility change classification.
- [x] Separate import candidate enumeration from candidate existence and preserve conservative ambiguity rules.
- [x] Add normalized `import_candidates` storage with a reverse candidate-path index and transactional maintenance.
- [x] Query and reconcile only importers affected by membership changes; ordinary modifications avoid repository-wide import scans.

### Verification

- [x] Tests cover new target resolution, target deletion, renamed paths, directories, ignore controls, cancellation, and stale plans while existing ambiguity unit coverage remains intact.
- [x] Query-plan checks prove candidate-path reverse lookup uses its index.
- [x] Release profile records targeted create/delete/rename work independent of unrelated import rows.

### Exit criteria

- [x] Targeted reconciliation work follows the explicit change set and affected dependency projection.

## Phase 4: Deterministic candidate aggregation and overlap reduction

Status: complete

### Implementation

- [x] Retain existing exact candidate identity/provenance coalescing before final selection.
- [x] Partition retained overlap candidates by path with `HashMap<Path, Vec<Index>>` while preserving first-match and score-recomputation semantics.
- [x] Keep the partitioned implementation; focused evidence does not justify an interval-tree dependency.

### Verification

- [x] Existing tests cover exact priority, first retained match, non-transitive overlaps, same content at distinct paths, stable ordering, and merged provenance.
- [x] Release benchmark reports one-file and many-file candidate distributions.

### Exit criteria

- [x] Cross-file quadratic comparisons are eliminated with deterministic behavior unchanged.

## Phase 5: Path pagination projection decision

Status: complete

### Implementation

- [x] Measure tree first/deep pages and retain existing bounded behavior tests for glob and fuzzy pages.
- [x] Add an ordinary SQLite path hierarchy projection maintained transactionally with file changes.
- [x] Use indexed path keyset pagination for projected paths and retain bounded scans for glob/fuzzy semantics.
- [x] Record the implemented projection decision below.

### Verification

- [x] Tests cover nested roots, depth, directory deduplication, cursor generation, deletion, rename, and ignore changes; existing path tests retain Unicode-safe string handling.
- [x] Query-plan and release evidence support the projection.

### Exit criteria

- [x] Tree pagination has an evidence-backed implementation decision; fuzzy/glob behavior remains explicit and bounded.

## Phase 6: Connection reuse decision

Status: complete

### Implementation

- [x] Isolate connection-open, transaction, and query execution costs after batching.
- [x] Select established synchronous `r2d2_sqlite` 0.35.0, compatible with rusqlite 0.40.1, Rust 1.95, read-only flags, initialization, and request-scoped transactions.
- [x] Scope the adopted pool per service/repository cache; no cross-repository backpressure or failure coupling is introduced.
- [x] Keep pooling inside the existing `spawn_blocking` boundary without a second executor.
- [x] Record the material measured improvement below.

### Verification

- [x] Snapshot isolation, contention retries, rollback-on-return cleanup, and cross-process WAL behavior pass; cancellation checks remain at service boundaries.
- [x] Concurrent work in one repository cannot consume another repository's pool capacity or change its generation/results.
- [x] Release evidence demonstrates the implemented choice materially reduces session setup.

### Exit criteria

- [x] Connection lifecycle is evidence-backed and no custom pool exists.

## Phase 7: Full validation and handoff

Status: complete

### Implementation

- [x] Update architecture/measurement documentation and record residual risks or intentionally retained bounded scans.
- [x] Inspect the full diff for API/schema changes, migration safety, unrelated churn, and benchmark integrity with the AI-code audit procedure.

### Verification

- [x] `cargo fmt --all -- --check`
- [x] `cargo check --all-targets --all-features`
- [x] `cargo clippy --all-targets --all-features -- -D warnings`
- [x] `cargo test --all-targets --all-features`
- [x] `cargo doc --no-deps`
- [x] Required focused release profiles and representative retrieval behavior checks pass.
- [x] `git diff --check` and final status inspection pass.

### Exit criteria

- [x] Goal completion proof in `goal.md` is satisfied and no required work remains.

## Recorded evidence

- DeepWiki review of CodeDB (`justrach/codedb`) confirmed the applicable boundary: project-scoped stores/watchers and independent project state, with agent coordination layered separately. LeanToken adopts per-repository cache/failure isolation and same-repository shared generations, but not CodeDB's edit locks or global project router.
- Focused debug verification passed for 142 library tests, storage migrations/snapshots/query plans, targeted importer reconciliation, structural search/context behavior, tree pagination, repository ownership, and same-repository sharing.
- `EXPLAIN QUERY PLAN` uses `import_candidates_path_idx`, `chunks_file_line_idx`, and `sqlite_autoindex_path_entries_1` for the new reverse-import, batch-range, and tree-keyset paths.
- macOS arm64 release connection profile, synthetic 301-file corpus, 1,000 samples: unpooled open + snapshot + generation p50 480.542 us / p95 1,975.875 us; pooled checkout + snapshot + generation p50 7.125 us / p95 19.959 us; pinned generation query p50 3.166 us / p95 4.291 us. Artifact: `target/index-performance-design-pooled-profile.json`.
- macOS arm64 release hot-path profile, synthetic 500-file corpus, five samples: context p50 162.117 ms / p95 174.927 ms; tree first page p50 0.834 ms / p95 1.603 ms; tree deep page p50 0.454 ms / p95 0.679 ms; 2,000-candidate dedup p50 17.911 ms for one path and 5.503 ms across many paths. Artifact: `target/index-performance-hot-paths.json`.
- Timing evidence is host-local and was collected from a dirty implementation worktree; it supports local design decisions, not a cross-platform latency claim.
