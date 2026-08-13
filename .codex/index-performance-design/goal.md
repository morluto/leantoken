# LeanToken index and retrieval design goal

Companion plan: [plan.md](plan.md)

## Outcome

Remove the confirmed retrieval and indexing design debt while preserving LeanToken's public behavior, deterministic ranking, exact token budgets, request-scoped SQLite snapshot consistency, and cross-platform support.

The completed design must:

- replace per-hit excerpt/enclosing-symbol query fan-out with a repository-specific batched SQLite read model;
- model import resolution candidates as an indexed reverse dependency projection and classify reconciliation changes explicitly;
- partition overlap deduplication by path without changing LeanToken's overlap or merge semantics;
- use SQLite-backed path projections and keyset pagination where measurements show the existing tree scan is material;
- reuse prepared statements and measure connection setup separately, adopting an established compatible connection pool only when release evidence justifies it;
- retain SQLite FTS5 and avoid a second search engine, custom connection pool, generic data-loader framework, or speculative interval-tree subsystem;
- isolate caches, locks, watcher leadership, reconciliation capacity, and failures by canonical repository while allowing agents on the same repository to share one index and independent read snapshots.

## Baseline

- Context and search hydrate structural hits through repeated `find_enclosing_symbol` and `get_chunks_overlapping` calls.
- Every targeted reconciliation materializes all indexed files and scans stored imports because unresolved candidate paths are not persisted.
- Overlap deduplication compares each candidate with the entire retained vector.
- Tree, glob, and fuzzy path pages scan the indexed file table; directories are reconstructed per request.
- Every response opens a new read-only SQLite connection and pins the correct request-scoped WAL snapshot.
- Managed cache paths and all coordination locks derive from the canonical database path, so default configurations are per-repository and same-repository agents intentionally share state.
- Cache metadata does not currently bind an explicit database path to its canonical repository root; pointing two repositories at one explicit database can therefore mix ownership and let the last reconciler replace the other repository's contents.
- Existing archived release evidence covers an 865-file Tokio tree, but does not isolate SQL statement count, connection setup, candidate hydration, or multi-page tree scaling.

## Constraints

- Application behavior remains in `Services`; CLI and MCP adapters remain thin.
- Preserve response schemas, result ordering, score semantics, omission behavior, cursor generation checks, import ambiguity rules, and exact token accounting.
- Preserve the `BEGIN DEFERRED` request snapshot invariant and stale-generation reconciliation protection.
- Different canonical repositories must have independent database, lock, watcher-leader, connection-capacity, and failure domains by default. Agents using the same canonical repository must share one database generation and one watcher leader while retaining request-local read sessions.
- Persist and validate repository ownership for service-managed database access so an explicit database path cannot silently alternate between different roots.
- Use SQLite joins, indexes, transactions, FTS5, row-value keyset pagination, rusqlite statement caching, and supported table-valued inputs.
- Custom code is limited to LeanToken domain policy: import candidate enumeration, signal fusion, ranking, overlap semantics, token allocation, and explicit change classification.
- Benchmarks and repository-scale profilers run in release mode.
- Do not weaken tests, caps, benchmark corpora, prompts, labels, token budgets, or correctness checks to obtain a passing result.
- No publication, push, release, destructive workspace operation, or external state change is authorized.

## Non-goals

- Replacing SQLite FTS5 with Tantivy or another search engine.
- Building a generic ORM, cache, data-loader framework, virtual table, interval tree, or custom connection pool.
- Expanding import resolution beyond the currently supported conservative language/path semantics unless required to preserve behavior.
- Changing MCP schemas or retrieval quality policy except where required to preserve existing semantics after batching.
- Adding a global multi-repository daemon, cross-repository ranking, per-call project routing, or agent edit/advisory locks. Those solve workspace coordination concerns outside LeanToken's read-oriented repository service.

## Primary verifier

Run the repository's full supported checks successfully:

```bash
cargo fmt --all -- --check
cargo check --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo doc --no-deps
```

Then run focused release-mode profiles that report before/after evidence for:

- context/search SQL statement count and warm p50/p95;
- existing-file targeted reconciliation versus repository file/import count;
- candidate overlap deduplication with one-file and many-file distributions;
- tree first-page and deep-page work on a large synthetic path corpus;
- read-session connection setup versus query execution.
- concurrent same-repository agents versus concurrent independent repositories, including contention and failure isolation.

## Supporting verification

- Behavioral tests for empty inputs, duplicate/overlapping candidates, ordering, multi-channel provenance, import creation/deletion/ambiguity, ignore changes, cancellation, stale generations, snapshot isolation, cursor stability, Unicode paths, depth limits, and token budgets.
- Isolation tests proving two default-config repositories cannot share cache/lock identities or results, same-repository service instances safely share generations, and an explicit database/root ownership mismatch is rejected before reconciliation.
- `EXPLAIN QUERY PLAN` evidence that new batch, reverse-import, and keyset queries use their intended indexes and do not introduce accidental full scans on hot paths.
- Existing representative retrieval benchmark comparison when ranking candidate generation or hydration order changes.
- Git diff inspection proving no unrelated changes or generated artifacts are included.

## Iteration loop

1. Measure and record the current smallest representative baseline.
2. Change one owner boundary or projection at a time.
3. Run the focused behavioral tests and query-plan checks.
4. Run the smallest relevant release measurement.
5. Record evidence in `plan.md`, then continue or revise the route.
6. Run the full verifier only after all focused phases pass.

## Approval gates

Ask separately before pushing, opening a pull request, publishing, releasing, deleting material files, changing public schemas, or changing frozen benchmark inputs. None of those actions is required by this goal.

## Blocker standard

Difficulty, slow compilation, or an initially negative benchmark is not a blocker. A blocker requires a repeated external condition that prevents meaningful local progress, such as an unavailable required platform/runtime capability or a necessary product choice between incompatible observable behaviors.

## Completion proof

- Every phase in `plan.md` is complete with commands and observed evidence recorded.
- The primary verifier passes without weakened checks.
- Required focused release profiles demonstrate bounded or improved work, or explicitly reject an evidence-gated architecture such as pooling/path projection with recorded measurements.
- Public behavior, snapshot isolation, import ambiguity, ranking determinism, and token budgets are covered by passing tests.
- `git diff --check` passes and the final diff contains only scoped source, tests, benchmarks, documentation, migrations, and durable goal artifacts.
