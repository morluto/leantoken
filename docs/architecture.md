# Architecture and reliability

LeanToken serves one repository per process. Its retrieval kernel is built
around one immutable, atomically published repository generation.

```text
explicit refresh
      |
      v
bounded repository discovery and reads
      |
      v
complete derived generation
      |
      v
atomic SQLite publication
      |
      +----> search
      +----> outline       one pinned generation per response
      +----> read
      +----> context (orchestration over the three primitives)
                    |
                 CLI / MCP
```

Repository files are authoritative only while `refresh` builds the next
generation. After publication, every retrieval operation—including `read`—uses
indexed content from a pinned SQLite read transaction. Edits, checkouts, and
deletions are invisible until the next explicit refresh. A cancelled or failed
refresh leaves the previous generation intact. No watcher, request-time
reconciliation, or live-file fallback participates in correctness.

## Ownership boundaries

- One `Services` instance and one MCP process own one normalized repository.
  A host that needs another repository starts another process. There is no
  repository registry inside the server.
- The indexer owns bounded discovery, source reads, chunking, tree-sitter
  extraction, and construction of a complete candidate generation.
- Storage owns schema migration, read transactions, staging, generation
  validation, and atomic publication. Published generations are disposable
  projections: an incompatible computation fingerprint or integrity failure
  causes a managed cache to be rebuilt, not individually healed.
- Retrieval owns normalized requests, deterministic ranking, exact token
  accounting, and response assembly against one `IndexSnapshot`.
- `context` is orchestration over search, outline, and read. It is not a second
  storage or consistency system.
- CLI and MCP are projections. The MCP catalog contains only `refresh`,
  `search`, `outline`, `read`, and `context`.
- rmcp 3.1.2 owns MCP initialization, protocol negotiation, request
  cancellation, dispatch, result envelopes, and stdio lifecycle. LeanToken
  retains only product validation, bounded active-call admission, and safe
  error projection.

## Generation state machine

```text
Empty --refresh succeeds--> Published(n)
  |                            |
  | query: IndexNotReady       | query/page/restart: read n
  |                            |
  +--failed/cancelled refresh--+
                               |
                     refresh succeeds
                               v
                          Published(n+1)
```

Building does not mutate the visible generation. Publication verifies the
baseline generation and computation fingerprint in the same SQLite writer
transaction that advances `repository_generation`. A response opens one
deferred read transaction and reports the generation pinned by that transaction.

The computation fingerprint covers `INDEX_CONTENT_VERSION`, the package
implementation version, discovery and preparation bounds, index scope,
chunking, and tokenizer semantics. Parser, resolver, schema, or projection
semantic changes must bump `INDEX_CONTENT_VERSION`. Managed cache names include
that version, making an incompatible projection disposable as a unit.

## Cursors

Search, outline, and read use the same sealed cursor codec. Its authenticated
envelope contains:

```text
{ version, repository, generation, normalized-request digest, typed position }
```

The codec has a process-random authentication capability, a 2 KiB wire bound,
and constant-time tag verification. A cursor cannot be moved between
repositories, requests, generations, or server lifetimes. Typed position data
is operation-specific; the wire format and validation policy are not.

## Imports

The generic index records parser-observed import text and coordinates. It does
not claim compiler-grade resolved edges. `resolved_path` and candidate paths
remain absent unless a future maintained compiler or LSP adapter supplies them
behind an explicitly language-specific contract. This avoids presenting a
heuristic graph as high-confidence semantic evidence.

## Stateless retrieval

Ordinary retrieval does not persist receipts, query proofs, read-delta bases,
token-savings sessions, failure observations, or conversational LRU state.
Clients may send a known content hash to `read`; an exact match produces a
source-free not-modified response. Removed receipt and delta request fields are
rejected instead of silently changing meaning.

Migration 12 drops historical receipt, query-receipt, delta, savings, failure,
and path-projection tables. Current retrieval neither reads nor writes mutable
auxiliary state; only the published generation participates in correctness.

## Bounds and resource ownership

`ProcessBudget` is the process-owned admission capability for blocking work.
Refresh and retrieval acquire it before filesystem, CPU, or SQLite-heavy work;
operation-specific scan, result, token, parser, and response bounds subdivide
that capacity. A full refresh is also serialized by the database operation
lease, so concurrent processes cannot publish conflicting baselines.

Important configured bounds include maximum walk entries, indexed files,
aggregate source bytes, discovery depth, source bytes per file, preparation
batch files/bytes, index workers, results, source tokens, context lines, and
serialized response tokens. Regex and structural paths have additional fixed
candidate and scan ceilings. New scans, fan-out, storage, or concurrency must
document both its local bound and how it composes under `ProcessBudget`.

SQLite uses WAL, foreign keys, bounded busy handling, a bounded connection
pool, and request-scoped read transactions. Text chunks are canonical source
for indexed reads; reconstruction validates contiguous chunk coordinates and
the exact indexed byte size. Hash mismatch or malformed chunk layout is index
corruption, never permission to reopen a live file.

## Testing ownership

Tests target state transitions and invariants:

- model tests vary dirty-query prefixes and assert that only the last published
  state is observable;
- one state-machine integration covers empty/query, refresh, pagination,
  cancellation, another refresh, stale cursors, and restart;
- cursor unit tests cover request, repository, generation, lifetime, and
  tamper binding;
- focused SQLite tests own staging/publication and query-plan behavior;
- one rmcp composition test owns initialization and the five-tool catalog.

Feature-specific tests should prove observable retrieval or stable storage
contracts. Do not add another startup scheduler suite, protocol mega-test, or
benchmark binary to compensate for unclear semantic ownership.

## Research basis

The architecture follows the current rmcp 3.1.2 server contract and its 3.x
stateless/result-shape changes. External pattern research also informed the
boundary: rust-analyzer separates mutable host updates from immutable analysis
snapshots, while ripgrep lets one invocation own one traversal and its resource
limits. These patterns are design evidence; LeanToken's source, types, and
tests remain authoritative for this implementation.
