# Architecture

LeanToken is a local code-intelligence service. Its core contract is one
immutable repository generation per retrieval.

```text
explicit refresh or refresh signal
              |
              v
bounded repository acquisition
              |
              v
complete derived generation
              |
              v
atomic SQLite publication
              |
              v
files / search / outline / read
              |
              v
thin CLI and MCP adapters
```

Repository files are authoritative while the next generation is built. After
publication, indexed retrieval reads the published bytes and projections. A
request pins one SQLite read transaction, represented internally by
`RepositoryGeneration`, for its entire lifetime.

## Ownership

- `Services` owns validation, retrieval behavior, budgets, and response models.
- The indexer owns bounded discovery, source preparation, parsing, and complete
  generation construction.
- Storage owns migrations, transactions, generation publication, FTS5, and
  `GenerationReadTransaction`.
- CLI and MCP translate requests and responses; they do not implement ranking
  or indexing policy.
- `leantoken-git` owns bounded Git subprocess behavior.
- `leantoken-lab` owns offline artifact analysis. It is not in the retrieval
  correctness path.
- The benchmark package owns opt-in evaluation executables.

One configured process serves one repository. Hosts that need several
repositories start several processes. There is no in-process repository
registry or global scheduler.

## Repository generations

Refresh discovers and prepares source outside the publication transaction.
Prepared records are staged in a disposable SQLite database. Publication then
checks its baseline, replaces all affected projections, rebuilds FTS, advances
the generation, and commits atomically. Cancellation before commit rolls the
publication back. Once commit succeeds, the new generation is authoritative.

A retrieval opens one deferred read transaction and verifies that the index is
initialized and structurally valid. Search, outline, path admission, and read
therefore observe the same generation. Integrity failure invalidates the whole
disposable generation; startup does not independently repair path, FTS,
fingerprint, import, or accounting projections.

The computation-semantics fingerprint covers the schema and derivation rules
that affect retrieval. A mismatch requires a rebuild. SQLite generations are
derived state, never the only copy of repository source.

## Freshness and live content

`refresh` is the explicit correctness boundary. A watcher or periodic adapter
may request refresh after a filesystem event, but correctness does not depend
on event completeness or timing. The compatibility
`reconcile_working_tree` request option is implemented as refresh-before-query;
new integrations should prefer an explicit refresh followed by normal
generation retrieval.

Canonical `read` returns bytes stored in the pinned generation. It cannot
combine live bytes with indexed symbol coordinates. `Services::read_worktree`
is an explicitly live library operation for callers that need dirty content;
its response identifies the weaker worktree boundary and uses caller-selected,
immutable read artifacts for deltas.

## Storage domains

LeanToken separates three kinds of state:

1. The repository-generation database contains source and all projections used
   by retrieval. It is disposable and atomically published.
2. The artifact database contains immutable, content-addressed evidence,
   exhaustive-query proofs, and worktree-read bases. An artifact ID commits to
   its kind, repository, generation, and canonical payload. Reads never mutate
   TTL or access order.
3. The instrumentation database contains best-effort aggregate counters.
   Failure to write instrumentation cannot fail or delay retrieval.

The generation database uses bundled SQLite with FTS5, WAL, foreign keys,
versioned migrations, bounded busy waits, and a bounded read-connection pool.
The pool has one reader beyond the CPU-bound retrieval cohort so reconciliation
can read its publication baseline without starving a complete cohort of pinned
generation reads.
An explicit database path is bound to its canonical repository and normalized
index scope. A database claimed by another root or scope is rejected.

## Cursors and artifacts

Every retrieval cursor uses one sealed envelope containing its format version,
repository identity, generation or content identity, normalized-request
digest, and typed position. Operations own only their position payload. Decode
rejects malformed, cross-repository, stale-generation, or request-mismatched
cursors before query execution.

Evidence reuse is caller-carried. Known content hashes can suppress bytes the
client already holds. Persisted evidence and exact-query proofs are immutable
artifacts; extending evidence creates a successor rather than mutating a
server-side session. Artifacts are bounded by count and logical bytes, and
capacity exhaustion fails explicitly instead of silently evicting referenced
state.

## Resource ownership

One process-owned budget bounds indexing workers, repository reads, SQLite
connections, subprocess output, CPU admission, and response materialization.
Operation-specific limits are subdivisions of that owner. No adapter may start
an unbounded scan or worker pool behind the service layer.

Long-running operations accept cancellation. Publication checks cancellation
between bounded phases and immediately before commit. Retrieval admission
happens before scarce connections and response-sized allocations are acquired.

## Retrieval hot-path bounds

These are safety limits, not performance claims. Requests that cannot complete
within a correctness-relevant scan boundary fail with a typed limit instead of
returning a false exhaustive result.

| Path | Bound |
| --- | --- |
| Context query terms | 12 (`MAX_CONTEXT_QUERIES`) |
| Context hits per term/source | 20 symbols/refs, 30 FTS |
| Regex matching chunks | `min(max_results × 20, 2000)` |
| Trigram candidate chunks | 10000 |
| Lightweight rows inspected for path-scoped trigram planning | 100000 |
| Full-scan fallback files | 10000 |
| Full-scan fallback chunks per file | 256 |
| File scan page size | 1000 for find (path projection) and globset fallback; tree/glob SQL-page `max_results + 1` projected paths |
| Focus patterns | 32 |
| Focused files inspected per pattern | First 4 eligible indexed paths |
| File-local focus inspection | 256 chunks and 128 symbols |
| Required-evidence contracts | 32 contracts, 16 literals each, 64 KiB total query text |
| Regex cancellation interval | At most 64 verified chunks |
| Search occurrences | 100,000 fail-closed scan cap; 100 returned coordinates per page |
| Batched history targets | 64 requested, 32 returned per page |
| Batched history source | 32 paths and 8 MiB per revision endpoint |
| Read response fitting | At most 18 materializations in one generation |
| Context excerpts | At most 256 exact tokenizer tokens per fragment; omit a required line that cannot fit |

Repository discovery defaults to 500,000 walked entries, 150,000 admitted
files, 2 GiB aggregate source, depth 64, and 2 MiB per file. Preparation batches
hold at most 256 files or 64 MiB of source. Configuration may lower these
limits. Broad roots are rejected unless explicitly authorized.

## Context owner selection

Implementation-shaped tasks classify recognized-language implementation files
as production across root, library, application, package, and workspace layouts.
Tests, documentation, examples, repository tooling, generated schemas,
snapshots, fixtures, agent skills, benchmark reports, and unscoped root prose
are auxiliary evidence.

Greedy selection first reserves a production fragment where a specific exact
atom corroborates the same normalized primary-change identity. Qualified
adjacent surfaces, short acronyms such as CLI and MCP, and prose hyphen compounds
do not override a matching owner. A deterministic supporting-file fallback is
considered only when no production owner fits. Within either class, facet breadth
chooses an owner only among comparable evidence that preserves the baseline
representation and all primary facets; the highest-utility qualifying excerpt
from that path is emitted.

Natural phrase queries prefer the primary clause and use trailing clauses only
when a query slot remains. After explicit required and focused evidence,
selection may reserve one relevant test path and one non-auxiliary preservation
fragment, then at most one auxiliary, two failure-trace, two test, and two
preservation fragments. The reservations use at most thirteen linear passes over
the already bounded candidate pool; they add no repository scan or candidate
fan-out and never relax source-token or fragment bounds.

## Language intelligence

Tree-sitter adapters extract definitions, syntactic references, and import
candidates for supported languages. These records are structural heuristics,
not compiler binding claims. Import or reference edges that cannot be justified
by the maintained adapter remain unresolved or are labeled heuristic. A wrong
high-confidence graph is worse than an absent edge.

C and C++ adapters include direct identifier and named field/pointer-field call
references from the same bounded parser pass, including C source in ambiguous
`.h` files. Declarations, comments, strings, and non-call field access are not
treated as calls; malformed recovered top-level expressions are excluded unless
they occur inside executable scope.

## Determinism and failure behavior

Ranking and pagination use stable tie-breakers. Token accounting uses the
configured tokenizer and separates source tokens from complete response tokens.
No response may cross its declared source or serialized-response ceiling.

Generation zero returns `index_not_ready`, never an empty success. Stale
cursors, changed baselines, invalid artifacts, scope mismatches, and exhausted
bounds are typed failures. An existing committed generation remains readable
while a replacement is prepared. Instrumentation and watcher failures do not
invalidate that generation.

The key behavioral evidence is state-machine coverage over
refresh/query/cancel/page/restart interleavings plus focused real SQLite, path
safety, watcher recovery, process isolation, CLI, and MCP lifecycle tests.
Historical benchmark anecdotes do not replace these contracts.
